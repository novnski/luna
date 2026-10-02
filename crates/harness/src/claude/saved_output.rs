//! Host-only hydration of Claude's truncated Bash receipts. Model content is
//! unchanged; only a bounded, verified file in the launch's own session is read.
use super::wire::Frame;
use crate::{AgentEvent, CancellationToken};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::{Component, Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

const MAX_FILE_BYTES: usize = 1024 * 1024;
const MAX_SERIALIZED_BYTES: usize = 2 * 1024 * 1024;

pub(super) struct SavedOutputReader {
    home: Option<PathBuf>,
    launch_cwd: PathBuf,
    resume: Option<String>,
    owner: Option<(String, String)>,
    scope_changed: bool,
    read_active: Arc<AtomicBool>,
}
struct ReadGuard(Arc<AtomicBool>);
impl Drop for ReadGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}
impl SavedOutputReader {
    pub fn new(home: Option<PathBuf>, cwd: &str, resume: Option<&str>) -> Self {
        let launch_cwd = if cwd.is_empty() {
            std::env::current_dir().unwrap_or_default()
        } else {
            PathBuf::from(cwd)
        };
        let home = home.map(|p| {
            if p.is_absolute() {
                p
            } else {
                launch_cwd.join(p)
            }
        });
        Self {
            home,
            launch_cwd,
            resume: resume.map(str::to_owned),
            owner: None,
            scope_changed: false,
            read_active: Arc::new(AtomicBool::new(false)),
        }
    }
    pub fn observe(&mut self, frame: &Frame) {
        if let Frame::System(init) = frame
            && init.subtype == "init"
            && init.parent_tool_use_id.is_none()
        {
            let valid = !init.session_id.is_empty()
                && init.session_id.len() <= 128
                && init
                    .session_id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                && Path::new(&init.cwd).is_absolute()
                && self.resume.as_ref().is_none_or(|id| id == &init.session_id);
            if !valid {
                self.scope_changed = true;
                return;
            }
            if let Some((id, cwd)) = &self.owner {
                if id != &init.session_id || cwd != &init.cwd {
                    self.scope_changed = true;
                }
            } else {
                self.owner = Some((init.session_id.clone(), init.cwd.clone()));
            }
        }
    }
    pub fn admission(&self, frame: &Frame) -> Result<(), &'static str> {
        let Frame::User(user) = frame else {
            return Err("not a native result frame");
        };
        if user.parent_tool_use_id.is_some() {
            return Err("child session ownership not verified");
        }
        if self.scope_changed {
            return Err("session ownership changed");
        }
        let Some((id, _)) = &self.owner else {
            return Err("session ownership unavailable");
        };
        if user.session_id != *id {
            return Err("result session ownership differs");
        }
        Ok(())
    }
    pub async fn enrich(
        &self,
        event: &mut AgentEvent,
        admission: Result<(), &'static str>,
        stop: &CancellationToken,
    ) {
        let event = match event {
            AgentEvent::Subagent { event, .. } => event.as_mut(),
            other => other,
        };
        let AgentEvent::ToolResult {
            output: Some(output),
            ..
        } = event
        else {
            return;
        };
        if !output.starts_with("{\"lunaToolOutput\":") {
            return;
        }
        let Ok(mut value) = serde_json::from_str::<Value>(output) else {
            return;
        };
        let payload = &mut value["lunaToolOutput"];
        if payload["tool"] != "Bash"
            || payload["provider"] != "claudeCode"
            || payload.get("nonExecution").is_some()
        {
            return;
        }
        let Some(path) = payload
            .pointer("/result/persistedOutputPath")
            .and_then(Value::as_str)
            .map(PathBuf::from)
        else {
            return;
        };
        let Some(size) = payload
            .pointer("/result/persistedOutputSize")
            .and_then(Value::as_u64)
        else {
            return;
        };
        let Some(prefix) = payload
            .pointer("/result/stdout")
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            return;
        };
        if size <= prefix.len() as u64 {
            return;
        }
        let result = match admission {
            _ if stop.is_cancelled() => Err("read interrupted"),
            Err(reason) => Err(reason),
            Ok(()) => match (&self.home, &self.owner) {
                (Some(home), Some((sid, cwd))) => {
                    let home = home.clone();
                    let sid = sid.clone();
                    let cwd = cwd.clone();
                    let launch = self.launch_cwd.clone();
                    // A timed-out blocking read may still be in the kernel.
                    // Keep at most one such worker per native session.
                    if self.read_active.swap(true, Ordering::AcqRel) {
                        payload["savedOutput"] = json!({"state":"unavailable","reason":"saved output read still pending"});
                        if let Ok(encoded) = serde_json::to_string(&value) {
                            *output = encoded;
                        }
                        return;
                    }
                    let active = self.read_active.clone();
                    let job = tokio::task::spawn_blocking(move || {
                        let _guard = ReadGuard(active);
                        read_verified(&home, &launch, &cwd, &sid, &path, size, &prefix)
                    });
                    tokio::select! {
                        _ = stop.cancelled() => Err("read interrupted"),
                        result = tokio::time::timeout(Duration::from_secs(1),job) => match result {
                            Ok(Ok(result)) => result, _ => Err("bounded read unavailable"),
                        }
                    }
                }
                _ => Err("provider home unavailable"),
            },
        };
        payload["savedOutput"] = match result {
            Ok(text) => {
                json!({"state":"loaded","source":"claude-saved-bash-output","byteLength":text.len(),"sha256":format!("{:x}",Sha256::digest(text.as_bytes())),"text":text})
            }
            Err(reason) => json!({"state":"unavailable","reason":reason}),
        };
        if payload["savedOutput"]["state"] == "loaded" {
            let candidate = serde_json::to_string(&value).unwrap_or_default();
            // The RPC carries this envelope as another JSON string. Account
            // for escaping there too, rather than bounding just raw UTF8.
            if serde_json::to_vec(&candidate).map_or(true, |v| v.len() > MAX_SERIALIZED_BYTES) {
                value["lunaToolOutput"]["savedOutput"] =
                    json!({"state":"unavailable","reason":"serialized saved output exceeds limit"});
            }
        }
        if let Ok(encoded) = serde_json::to_string(&value) {
            *output = encoded;
        }
    }
}

fn read_verified(
    home: &Path,
    launch: &Path,
    cwd: &str,
    sid: &str,
    target: &Path,
    size: u64,
    prefix: &str,
) -> Result<String, &'static str> {
    use std::io::Read;
    if size > MAX_FILE_BYTES as u64 {
        return Err("saved output exceeds byte limit");
    }
    if !target.is_absolute() || prefix.is_empty() {
        return Err("unverified output path or prefix");
    }
    let launch =
        std::fs::canonicalize(launch).map_err(|_| "launch working directory unavailable")?;
    let observed =
        std::fs::canonicalize(cwd).map_err(|_| "observed working directory unavailable")?;
    if launch != observed {
        return Err("launch working directory differs");
    }
    let canonical_home = std::fs::canonicalize(home).map_err(|_| "provider home unavailable")?;
    let relative = target
        .strip_prefix(home)
        .or_else(|_| target.strip_prefix(&canonical_home))
        .map_err(|_| "output outside provider home")?;
    let parts = relative
        .components()
        .map(|part| match part {
            Component::Normal(p) => p.to_str().ok_or("invalid output path"),
            _ => Err("invalid output path"),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let project: String = cwd
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    if parts.len() != 5
        || parts[0] != "projects"
        || parts[1] != project
        || parts[2] != sid
        || parts[3] != "tool-results"
        || !parts[4].ends_with(".txt")
        || parts[4].starts_with('.')
        || parts[4].len() > 255
        || !parts[4]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
    {
        return Err("output outside owned session directory");
    }
    let mut file =
        open_scoped(&canonical_home, &parts).map_err(|_| "saved output unavailable or linked")?;
    let before = file
        .metadata()
        .map_err(|_| "saved output metadata unavailable")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if before.nlink() != 1 {
            return Err("saved output is multiply linked");
        }
    }
    if !before.is_file() || before.len() != size {
        return Err("saved output byte count differs");
    }
    let mut bytes = Vec::new();
    file.by_ref()
        .take(MAX_FILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "saved output unreadable")?;
    let after = file
        .metadata()
        .map_err(|_| "saved output metadata unavailable")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if after.nlink() != 1 {
            return Err("saved output link ownership changed");
        }
    }
    if bytes.len() as u64 != size
        || before.len() != after.len()
        || before.modified().ok() != after.modified().ok()
    {
        return Err("saved output changed during read");
    }
    let text = String::from_utf8(bytes).map_err(|_| "saved output is not UTF8")?;
    if !text.starts_with(prefix) {
        return Err("saved output prefix differs");
    }
    Ok(text)
}

#[cfg(unix)]
fn open_scoped(home: &Path, parts: &[&str]) -> std::io::Result<std::fs::File> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::fs::OpenOptionsExt;
    let mut current = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(home)?;
    for (i, part) in parts.iter().enumerate() {
        let name = CString::new(*part)?;
        let final_file = i + 1 == parts.len();
        let flags = libc::O_RDONLY
            | libc::O_NOFOLLOW
            | libc::O_CLOEXEC
            | if final_file {
                libc::O_NONBLOCK
            } else {
                libc::O_DIRECTORY
            };
        // Descriptor-relative traversal pins every directory and refuses all
        // child symlinks; no check-then-open race can escape the owned root.
        let fd = unsafe { libc::openat(current.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        current = unsafe { std::fs::File::from_raw_fd(fd) };
    }
    Ok(current)
}
#[cfg(not(unix))]
fn open_scoped(_home: &Path, _parts: &[&str]) -> std::io::Result<std::fs::File> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "safe saved output reader unavailable",
    ))
}
