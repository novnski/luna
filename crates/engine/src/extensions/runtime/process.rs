//! Platform process control for extension workers.
//!
//! At spawn, in the child before `exec`: a new session (its own process group
//! and no controlling terminal), `RLIMIT_CORE` 0, `RLIMIT_CPU` and
//! `RLIMIT_NOFILE`, and close-on-exec on every inherited descriptor above
//! stdio. The environment is an allow-list. Afterwards: the group leader's
//! exit is observed with `waitid(WNOWAIT)`, so the group is signalled while
//! the leader is still unreaped and its group id cannot be reused; the group's
//! processes and resident memory are sampled (macOS and Linux).
//!
//! None of this is an OS sandbox: a native worker keeps the account's file,
//! network and process access, and a child that starts its own session
//! leaves the sampled group.

use std::ffi::OsString;
use std::io;
use std::path::Path;
use std::time::Duration;

/// Environment variables a worker inherits. Nothing that locates LUNA's data,
/// owner IPC or credentials is on it.
pub(super) const ENVIRONMENT_ALLOW_LIST: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "LC_MESSAGES",
    "TMPDIR",
    "TZ",
];

/// Whether this platform samples group memory and process counts.
pub(super) const GROUP_SAMPLING: bool = cfg!(any(target_os = "macos", target_os = "linux"));

/// The hard CPU limit sits this far above the soft one, so a worker that
/// ignores `SIGXCPU` is still killed.
#[cfg(unix)]
const CPU_HARD_MARGIN: u64 = 2;
/// Highest descriptor marked close-on-exec in the child.
#[cfg(unix)]
const DESCRIPTOR_SCAN_LIMIT: u64 = 65_536;
/// Descriptors scanned above the highest one open before the fork, for any
/// opened concurrently by another thread.
#[cfg(unix)]
const DESCRIPTOR_SCAN_MARGIN: libc::c_int = 64;

/// One past the highest descriptor to mark close-on-exec in the child.
/// Scanning only what is open keeps the window between fork and exec short:
/// during it the child shares every open file description, including the
/// engine's `flock`ed instance lock.
#[cfg(unix)]
fn descriptor_scan_end(soft_limit: libc::rlim_t) -> libc::c_int {
    let ceiling = soft_limit.clamp(256, DESCRIPTOR_SCAN_LIMIT as libc::rlim_t) as libc::c_int;
    let highest = ["/dev/fd", "/proc/self/fd"]
        .iter()
        .find_map(|dir| std::fs::read_dir(dir).ok())
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|entry| entry.file_name().to_str()?.parse::<libc::c_int>().ok())
                .max()
                .unwrap_or(2)
        });
    match highest {
        Some(highest) => highest
            .saturating_add(1 + DESCRIPTOR_SCAN_MARGIN)
            .min(ceiling),
        None => ceiling,
    }
}

/// The allow-listed subset of this process's environment.
pub(super) fn worker_environment() -> Vec<(OsString, OsString)> {
    ENVIRONMENT_ALLOW_LIST
        .iter()
        .filter_map(|key| std::env::var_os(key).map(|value| (OsString::from(key), value)))
        .collect()
}

pub(super) struct SpawnLimits {
    pub cpu_seconds: u64,
    pub open_files: u64,
}

pub(super) struct Spawned {
    pub child: tokio::process::Child,
    pub pid: u32,
}

#[derive(Debug, Clone, Copy, Default)]
pub(super) struct GroupUsage {
    pub processes: u32,
    pub resident_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Signal {
    Terminate,
    Kill,
}

/// `(soft, hard)` for one resource of this process.
#[cfg(unix)]
macro_rules! get_limit {
    ($resource:expr) => {{
        let mut limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: `limit` is a valid out-pointer.
        if unsafe { libc::getrlimit($resource, &mut limit) } == 0 {
            Some((limit.rlim_cur, limit.rlim_max))
        } else {
            None
        }
    }};
}

#[cfg(unix)]
macro_rules! set_limit {
    ($resource:expr, $soft:expr, $hard:expr) => {{
        let limit = libc::rlimit {
            rlim_cur: $soft,
            rlim_max: $hard,
        };
        // SAFETY: async-signal-safe; `limit` is a valid pointer.
        if unsafe { libc::setrlimit($resource, &limit) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }};
}

/// Runs in the forked child before `exec`; only async-signal-safe calls.
#[cfg(unix)]
fn child_setup(
    cpu_soft: libc::rlim_t,
    cpu_hard: libc::rlim_t,
    files: libc::rlim_t,
    scan_to: libc::c_int,
) -> io::Result<()> {
    // SAFETY: plain syscall in the child.
    if unsafe { libc::setsid() } < 0 {
        return Err(io::Error::last_os_error());
    }
    set_limit!(libc::RLIMIT_CORE, 0, 0);
    set_limit!(libc::RLIMIT_CPU, cpu_soft, cpu_hard);
    set_limit!(libc::RLIMIT_NOFILE, files, files);
    for fd in 3..scan_to {
        // SAFETY: `fcntl` on a descriptor number that may or may not be open.
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFD);
            if flags >= 0 && flags & libc::FD_CLOEXEC == 0 {
                libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
            }
        }
    }
    Ok(())
}

/// Spawn `program` as a supervised worker with piped stdio.
#[cfg(unix)]
pub(super) fn spawn(
    program: &Path,
    args: &[OsString],
    cwd: &Path,
    environment: &[(OsString, OsString)],
    limits: &SpawnLimits,
) -> io::Result<Spawned> {
    use std::process::Stdio;

    // Never ask for more than this process's own hard limits.
    let requested_cpu = limits.cpu_seconds as libc::rlim_t;
    let cpu_ceiling: libc::rlim_t =
        get_limit!(libc::RLIMIT_CPU).map_or(libc::RLIM_INFINITY, |(_, hard)| hard);
    let cpu_soft = requested_cpu.min(cpu_ceiling);
    let cpu_hard = requested_cpu
        .saturating_add(CPU_HARD_MARGIN as libc::rlim_t)
        .min(cpu_ceiling);
    let (files_soft_now, files_hard_now): (libc::rlim_t, libc::rlim_t) =
        get_limit!(libc::RLIMIT_NOFILE).unwrap_or((1024, 1024));
    let files = (limits.open_files as libc::rlim_t).min(files_hard_now);
    let scan_to = descriptor_scan_end(files_soft_now);

    let mut command = tokio::process::Command::new(program);
    command
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .envs(
            environment
                .iter()
                .map(|(key, value)| (key.as_os_str(), value.as_os_str())),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(false);
    // SAFETY: `child_setup` only makes async-signal-safe calls.
    unsafe {
        command.pre_exec(move || child_setup(cpu_soft, cpu_hard, files, scan_to));
    }
    let child = command.spawn()?;
    let pid = child
        .id()
        .ok_or_else(|| io::Error::other("the worker exited before it could be supervised"))?;
    Ok(Spawned { child, pid })
}

#[cfg(not(unix))]
pub(super) fn spawn(
    _: &Path,
    _: &[OsString],
    _: &Path,
    _: &[(OsString, OsString)],
    _: &SpawnLimits,
) -> io::Result<Spawned> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "extension workers are not supported on this platform yet",
    ))
}

/// Resolve once the group leader `pid` has exited, without reaping it: the
/// leader stays a zombie, so its process group id cannot be reused until the
/// supervisor reaps it.
#[cfg(unix)]
pub(super) fn watch_leader(pid: u32) -> io::Result<tokio::sync::oneshot::Receiver<()>> {
    let (done, receiver) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("luna-extension-wait".into())
        .spawn(move || {
            loop {
                // SAFETY: `info` is a valid out-pointer; `WNOWAIT` leaves the
                // child waitable for the supervisor.
                let result = unsafe {
                    let mut info: libc::siginfo_t = std::mem::zeroed();
                    libc::waitid(
                        libc::P_PID,
                        pid as libc::id_t,
                        &mut info,
                        libc::WEXITED | libc::WNOWAIT,
                    )
                };
                if result == 0 || io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
                    break;
                }
            }
            let _ = done.send(());
        })?;
    Ok(receiver)
}

#[cfg(not(unix))]
pub(super) fn watch_leader(_: u32) -> io::Result<tokio::sync::oneshot::Receiver<()>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "extension workers are not supported on this platform yet",
    ))
}

/// Signal every process in the group. Callers only do this while the leader
/// is unreaped.
#[cfg(unix)]
pub(super) fn signal_group(pgid: u32, signal: Signal) {
    let signal = match signal {
        Signal::Terminate => libc::SIGTERM,
        Signal::Kill => libc::SIGKILL,
    };
    // SAFETY: plain syscall; a negative pid addresses the process group.
    unsafe {
        libc::kill(-(pgid as libc::pid_t), signal);
    }
}

#[cfg(not(unix))]
pub(super) fn signal_group(_: u32, _: Signal) {}

/// Whether any process of the group still exists.
#[cfg(unix)]
pub(super) fn group_exists(pgid: u32) -> bool {
    // SAFETY: signal 0 only checks for existence.
    let result = unsafe { libc::kill(-(pgid as libc::pid_t), 0) };
    result == 0 || io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

#[cfg(not(unix))]
pub(super) fn group_exists(_: u32) -> bool {
    false
}

/// Wait until the group has no process left, up to `timeout`.
pub(super) async fn wait_group_gone(pgid: u32, timeout: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if !group_exists(pgid) {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The group's processes and their total resident memory.
#[cfg(target_os = "macos")]
pub(super) fn group_usage(pgid: u32) -> Option<GroupUsage> {
    const SLOTS: usize = 4096;
    let mut pids: Vec<libc::c_int> = vec![0; SLOTS];
    let bytes = (SLOTS * std::mem::size_of::<libc::c_int>()) as libc::c_int;
    // SAFETY: `pids` holds `bytes` bytes. Unused slots stay zero, so the
    // members are read from the buffer, whatever unit the result counts in.
    let result =
        unsafe { libc::proc_listpgrppids(pgid as libc::pid_t, pids.as_mut_ptr().cast(), bytes) };
    if result < 0 {
        return None;
    }
    let mut usage = GroupUsage::default();
    for pid in pids.into_iter().filter(|pid| *pid > 0) {
        usage.processes += 1;
        let size = std::mem::size_of::<libc::proc_taskinfo>() as libc::c_int;
        // SAFETY: `info` is a valid buffer of `size` bytes.
        let (written, info) = unsafe {
            let mut info: libc::proc_taskinfo = std::mem::zeroed();
            let written = libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTASKINFO,
                0,
                (&mut info as *mut libc::proc_taskinfo).cast(),
                size,
            );
            (written, info)
        };
        if written == size {
            usage.resident_bytes = usage.resident_bytes.saturating_add(info.pti_resident_size);
        }
    }
    Some(usage)
}

#[cfg(target_os = "linux")]
pub(super) fn group_usage(pgid: u32) -> Option<GroupUsage> {
    // SAFETY: plain sysconf query.
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) }.max(4096) as u64;
    let mut usage = GroupUsage::default();
    for entry in std::fs::read_dir("/proc").ok()?.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|name| name.parse::<u32>().ok()) else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        // After the parenthesised command: state (field 3), process group
        // (field 5) and resident pages (field 24).
        let Some((_, rest)) = stat.rsplit_once(')') else {
            continue;
        };
        let fields: Vec<&str> = rest.split_whitespace().collect();
        if fields.get(2).and_then(|field| field.parse::<u32>().ok()) != Some(pgid) {
            continue;
        }
        usage.processes += 1;
        let pages: u64 = fields
            .get(21)
            .and_then(|field| field.parse().ok())
            .unwrap_or(0);
        usage.resident_bytes = usage.resident_bytes.saturating_add(pages * page);
    }
    Some(usage)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(super) fn group_usage(_: u32) -> Option<GroupUsage> {
    None
}

/// Exit status as `(code, signal)`.
pub(super) fn exit_parts(status: &std::process::ExitStatus) -> (Option<i32>, Option<i32>) {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;
        (status.code(), status.signal())
    }
    #[cfg(not(unix))]
    {
        (status.code(), None)
    }
}

/// The CPU-limit signal, where the platform has one.
pub(super) fn cpu_limit_signal() -> Option<i32> {
    #[cfg(unix)]
    {
        Some(libc::SIGXCPU)
    }
    #[cfg(not(unix))]
    {
        None
    }
}
