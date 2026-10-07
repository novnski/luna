//! Executable resolution (OC-00, PD-15): `OPENCODE_EXECUTABLE`, then PATH,
//! the login shell's PATH and the known install locations, in that order.
//!
//! LUNA speaks the OpenCode 2 wire only, so a candidate qualifies only when
//! its `--version` reports 2.x. A Homebrew 1.18 that comes earlier on PATH is
//! skipped and the search continues (it no longer shadows
//! `~/.opencode/bin/opencode`). When only 1.x exists, resolution fails with a
//! clear "OpenCode 2 is required" error instead of booting the old wire.
//! Version probes are cached per binary (path, mtime and size), so an
//! upgraded binary is re-probed at the next resolution.

use std::ffi::OsString;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use super::*;

/// The generation LUNA speaks.
pub(super) const REQUIRED_MAJOR: u64 = 2;

const INSTALL_COMMAND: &str = "curl -fsSL https://opencode.ai/install | bash";

/// `opencode --version` answers in well under a second (0.14 s for 2.0.22,
/// 0.4 s for 1.18.34 on this machine); anything slower is a wedged wrapper.
const VERSION_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// A probe that produced no version is retried after this long (a transient
/// failure must not hide a binary for the whole process lifetime).
const FAILED_PROBE_TTL: Duration = Duration::from_secs(60);

pub(super) fn opencode_install_paths() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    // `crate::executable::home_dir` checks USERPROFILE too, so the Unix-style
    // install locations resolve on Windows (`~/.local/bin/opencode.exe`, the
    // bun layout) instead of silently vanishing with an unset HOME.
    if let Some(home) = crate::executable::home_dir() {
        dirs.push(home.join(".opencode").join("bin").join("opencode"));
        dirs.push(home.join(".local").join("bin").join("opencode"));
        dirs.push(home.join(".bun").join("bin").join("opencode"));
        dirs.push(home.join(".npm-global").join("bin").join("opencode"));
    }
    dirs.push(PathBuf::from("/opt/homebrew/bin/opencode"));
    dirs.push(PathBuf::from("/usr/local/bin/opencode"));
    dirs
}

pub(super) const INSTALL_HINT: &str = "opencode 2 (searched PATH, the login shell's PATH, \
     ~/.opencode/bin, ~/.local/bin, ~/.bun/bin, ~/.npm-global/bin, /opt/homebrew/bin, \
     /usr/local/bin, and fnm/nvm/volta/pnpm/bun install dirs; Windows also checks USERPROFILE, \
     %APPDATA%\\npm, and explicit NVM_SYMLINK/VOLTA_HOME/PNPM_HOME; install with \
     `curl -fsSL https://opencode.ai/install | bash`, then `opencode auth login`; set \
     OPENCODE_EXECUTABLE to override)";

/// Where a resolved binary came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpencodeSource {
    /// `OPENCODE_EXECUTABLE`.
    Override,
    /// A path the harness was constructed with (`with_executable`).
    Explicit,
    /// PATH, the login shell's PATH, or a known install location.
    Search,
}

/// One resolved OpenCode binary: what every discovery call, run, fork and
/// compaction of this process launches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpencodeResolution {
    pub path: PathBuf,
    /// The semver `--version` reported; `None` when the probe gave nothing
    /// parseable (only accepted for an explicit override, whose server is
    /// still checked against `/api/info` at boot).
    pub version: Option<String>,
    pub source: OpencodeSource,
}

/// The user's OpenCode 2 binary, or `None` (not installed, only 1.x, or an
/// invalid `OPENCODE_EXECUTABLE`). [`OpencodeHarness`] reports the reason.
pub fn resolve_opencode_executable() -> Option<PathBuf> {
    resolve_opencode().ok().map(|resolution| resolution.path)
}

/// The resolution this process used last (path, version, source), for
/// diagnostics and evidence manifests.
pub fn last_opencode_resolution() -> Option<OpencodeResolution> {
    last_resolution().lock().ok().and_then(|last| last.clone())
}

pub(super) fn resolve_opencode() -> Result<OpencodeResolution, HarnessError> {
    // Like Codex and Claude: an explicit override is validated, never
    // silently replaced by a PATH search.
    if let Some(path) = std::env::var_os("OPENCODE_EXECUTABLE").filter(|path| !path.is_empty()) {
        let path = crate::executable::validate_native_override(&PathBuf::from(path))?;
        return check_explicit(path, OpencodeSource::Override);
    }
    let mut rejected: Vec<(PathBuf, String)> = Vec::new();
    let mut unknown: Vec<PathBuf> = Vec::new();
    let found =
        crate::executable::find_on_paths_matching("opencode", opencode_install_paths(), |path| {
            match probe_version(path) {
                Some(version) if version_major(&version) == Some(REQUIRED_MAJOR) => true,
                Some(version) => {
                    if !rejected.iter().any(|(seen, _)| seen == path) {
                        rejected.push((path.to_path_buf(), version));
                    }
                    false
                }
                None => {
                    if !unknown.iter().any(|seen| seen == path) {
                        unknown.push(path.to_path_buf());
                    }
                    false
                }
            }
        });
    match found {
        Some(path) => {
            let version = probe_version(&path);
            Ok(record(OpencodeResolution {
                path,
                version,
                source: OpencodeSource::Search,
            }))
        }
        None => Err(no_v2_error(&rejected, &unknown)),
    }
}

/// An explicitly chosen binary: rejected only when it provably is not 2.x.
pub(super) fn check_explicit(
    path: PathBuf,
    source: OpencodeSource,
) -> Result<OpencodeResolution, HarnessError> {
    let version = probe_version(&path);
    if let Some(version) = &version
        && version_major(version) != Some(REQUIRED_MAJOR)
    {
        return Err(v2_required(version, &path));
    }
    Ok(record(OpencodeResolution {
        path,
        version,
        source,
    }))
}

/// Any OpenCode binary at all, whatever its generation. `installed()` keeps
/// the provider visible on a 1.x-only machine so its actions fail with the
/// actionable "OpenCode 2 is required" error instead of the provider
/// silently vanishing.
pub(super) fn opencode_present() -> bool {
    if let Some(path) = std::env::var_os("OPENCODE_EXECUTABLE").filter(|path| !path.is_empty()) {
        return crate::executable::validate_native_override(&PathBuf::from(path)).is_ok();
    }
    crate::executable::find_on_paths("opencode", opencode_install_paths()).is_some()
}

fn v2_required(version: &str, path: &Path) -> HarnessError {
    HarnessError::NotInstalled(format!(
        "OpenCode 2 is required (found {version} at {}); install: {INSTALL_COMMAND}",
        path.display()
    ))
}

fn no_v2_error(rejected: &[(PathBuf, String)], unknown: &[PathBuf]) -> HarnessError {
    if let Some((path, version)) = rejected.first() {
        return v2_required(version, path);
    }
    if let Some(path) = unknown.first() {
        return HarnessError::NotInstalled(format!(
            "OpenCode 2 is required, but `{} --version` reported no version; install: \
             {INSTALL_COMMAND}, or set OPENCODE_EXECUTABLE",
            path.display()
        ));
    }
    HarnessError::NotInstalled(INSTALL_HINT.into())
}

fn last_resolution() -> &'static Mutex<Option<OpencodeResolution>> {
    static LAST: OnceLock<Mutex<Option<OpencodeResolution>>> = OnceLock::new();
    LAST.get_or_init(|| Mutex::new(None))
}

/// Remember (and log once per change) what this process resolved.
fn record(resolution: OpencodeResolution) -> OpencodeResolution {
    if let Ok(mut last) = last_resolution().lock()
        && last.as_ref() != Some(&resolution)
    {
        tracing::info!(
            target: "luna_harness::opencode",
            path = %resolution.path.display(),
            version = resolution.version.as_deref().unwrap_or("unknown"),
            source = ?resolution.source,
            "resolved the OpenCode executable"
        );
        *last = Some(resolution.clone());
    }
    resolution
}

/// The leading `major.minor.patch` of the first version-looking token
/// (`opencode v2.0.22` on 2.x, a bare `1.18.34` on 1.x).
pub(super) fn parse_version(text: &str) -> Option<String> {
    text.split_whitespace().find_map(|token| {
        let token = token.trim_start_matches('v');
        let end = token
            .find(|ch: char| !(ch.is_ascii_digit() || ch == '.'))
            .unwrap_or(token.len());
        let candidate = token[..end].trim_end_matches('.');
        let parts: Vec<&str> = candidate.split('.').collect();
        (parts.len() >= 3 && parts.iter().all(|part| !part.is_empty()))
            .then(|| candidate.to_owned())
    })
}

pub(super) fn version_major(version: &str) -> Option<u64> {
    version.split('.').next()?.parse().ok()
}

struct ProbeEntry {
    modified: Option<SystemTime>,
    len: u64,
    version: Option<String>,
    probed_at: Instant,
}

fn probe_cache() -> &'static Mutex<HashMap<PathBuf, ProbeEntry>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, ProbeEntry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// `--version` of one binary, cached per resolved path and file stamp.
fn probe_version(path: &Path) -> Option<String> {
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let metadata = std::fs::metadata(&canonical).ok();
    let modified = metadata.as_ref().and_then(|meta| meta.modified().ok());
    let len = metadata.as_ref().map(|meta| meta.len()).unwrap_or(0);
    if let Ok(cache) = probe_cache().lock()
        && let Some(entry) = cache.get(&canonical)
        && entry.modified == modified
        && entry.len == len
        && (entry.version.is_some() || entry.probed_at.elapsed() < FAILED_PROBE_TTL)
    {
        return entry.version.clone();
    }
    let version = run_version_probe(path);
    if let Ok(mut cache) = probe_cache().lock() {
        cache.insert(
            canonical,
            ProbeEntry {
                modified,
                len,
                version: version.clone(),
                probed_at: Instant::now(),
            },
        );
    }
    version
}

/// Run `<path> --version` synchronously with a hard timeout. Resolution is
/// synchronous (`installed()`, the update checker), so this cannot use the
/// async process layer.
fn run_version_probe(path: &Path) -> Option<String> {
    let mut command = std::process::Command::new(path);
    command
        .arg("--version")
        .env("OPENCODE_DISABLE_AUTOUPDATE", "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    if let Some(search_path) = probe_search_path(path) {
        command.env("PATH", search_path);
    }
    let mut child = command.spawn().ok()?;
    let stdout = child.stdout.take()?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut text = String::new();
        let _ = std::io::Read::read_to_string(&mut std::io::Read::take(stdout, 4096), &mut text);
        let _ = tx.send(text);
    });
    let output = rx.recv_timeout(VERSION_PROBE_TIMEOUT).ok();
    let reap_deadline = Instant::now() + Duration::from_secs(1);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if output.is_some() && Instant::now() < reap_deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
        }
    }
    parse_version(&output?)
}

/// The probe's PATH: the binary's own directory first (npm shims find their
/// `node` there), then this process's PATH and the login shell's.
fn probe_search_path(path: &Path) -> Option<OsString> {
    let mut paths: Vec<PathBuf> = Vec::new();
    if let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        paths.push(dir.to_path_buf());
    }
    if let Some(current) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&current));
    }
    if let Some(shell) = crate::shell_env::login_shell_path() {
        paths.extend(std::env::split_paths(shell));
    }
    let mut seen = std::collections::HashSet::new();
    paths.retain(|p| !p.as_os_str().is_empty() && seen.insert(p.clone()));
    std::env::join_paths(paths).ok()
}
