//! Signed GitHub Releases update checking and installation.
//!
//! Install kinds and their update paths:
//! - **Managed** (`~/.luna/app/<ver>` + `current` symlink — a future installer):
//!   download the headless tarball into a new versioned dir, flip
//!   the symlink, restart the service. Same flow the installer script performs,
//!   natively.
//! - **MacApp** (running out of an app bundle): download the app tarball, swap the
//!   bundle directory, relaunch. Driven by the UI.
//! - **Unmanaged** (source builds, hand-copied binaries): report only.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, bail};
use futures::StreamExt as _;
use ring::signature::{ED25519, UnparsedPublicKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt as _;
use tokio::sync::watch;

#[cfg(windows)]
pub mod windows;

/// The version compiled into this binary (the workspace version).
pub const fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Background check cadence.
const CHECK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(6 * 60 * 60);
/// Retry sooner after a failed check (offline boot, transient edge error).
const CHECK_RETRY: std::time::Duration = std::time::Duration::from_secs(30 * 60);
/// First check waits out engine boot (room joins, doc re-sync).
const CHECK_INITIAL_DELAY: std::time::Duration = std::time::Duration::from_secs(20);
/// While an auto-apply is deferred behind active sessions, re-probe idleness
/// this often.
const IDLE_RECHECK: std::time::Duration = std::time::Duration::from_secs(5 * 60);
const RELEASE_BASE: &str = "https://github.com/novnski/luna/releases/latest/download";
const RELEASE_ASSET_BASE: &str = "https://github.com/novnski/luna/releases/download";
const MAX_MANIFEST_BYTES: usize = 1024 * 1024;
const MAX_ARTIFACT_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_UNPACKED_BYTES: u64 = 2 * 1024 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Release metadata
// ---------------------------------------------------------------------------

/// Signed `manifest.json` from the latest public GitHub Release.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub version: String,
    pub files: BTreeMap<String, FileMeta>,
    #[serde(skip)]
    verified: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FileMeta {
    pub sha256: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
}

/// Artifact-name platform pair matching the packaging scripts. Unsupported
/// updater targets retain their real OS name rather than impersonating Linux.
pub fn platform_key() -> (&'static str, &'static str) {
    let os = std::env::consts::OS;
    let arch = match (os, std::env::consts::ARCH) {
        ("macos", "aarch64") => "arm64",
        (_, arch) => arch,
    };
    (os, arch)
}

fn managed_updates_supported(os: &str) -> bool {
    matches!(os, "linux" | "macos")
}

fn require_managed_update_platform() -> anyhow::Result<()> {
    if !managed_updates_supported(std::env::consts::OS) {
        bail!(
            "managed updates are not supported on {}",
            std::env::consts::OS
        );
    }
    Ok(())
}

fn require_mac_app_update_platform() -> anyhow::Result<()> {
    if std::env::consts::OS != "macos" {
        bail!(
            "macOS app updates are not supported on {}",
            std::env::consts::OS
        );
    }
    Ok(())
}

/// `luna-<ver>-<os>-<arch>.tar.gz` — the headless/CLI tarball.
pub fn headless_artifact(version: &str) -> String {
    let (os, arch) = platform_key();
    format!("luna-{version}-{os}-{arch}.tar.gz")
}

/// `luna-<ver>-macos-<arch>-app.tar.gz` — the macOS app update payload.
pub fn mac_app_artifact(version: &str) -> String {
    let (_, arch) = platform_key();
    format!("luna-{version}-macos-{arch}-app.tar.gz")
}

fn version_parts(version: &str) -> Option<[u64; 3]> {
    let mut parts = version.split('.');
    let parse = |part: &str| {
        (!part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
            .then(|| part.parse().ok())
            .flatten()
    };
    let parsed = [
        parse(parts.next()?)?,
        parse(parts.next()?)?,
        parse(parts.next()?)?,
    ];
    parts.next().is_none().then_some(parsed)
}

/// Strictly-newer three-part release version comparison.
pub fn version_newer(latest: &str, current: &str) -> bool {
    match (version_parts(latest), version_parts(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

/// Fetch and authenticate the newest release. There is no unsigned fallback.
pub async fn fetch_latest(_edge_url: &str) -> anyhow::Result<Manifest> {
    let base = release_base()?;
    let client = http_client()?;
    let manifest_url = format!("{base}/manifest.json");
    let response = client
        .get(&manifest_url)
        .send()
        .await
        .context("fetching update manifest")?
        .error_for_status()
        .context("fetching update manifest")?;
    let bytes = read_bounded(response, MAX_MANIFEST_BYTES)
        .await
        .context("reading update manifest")?;
    let response = client
        .get(format!("{base}/manifest.sig"))
        .send()
        .await
        .context("fetching update manifest signature")?
        .error_for_status()
        .context("fetching update manifest signature")?;
    let signature = read_bounded(response, 64)
        .await
        .context("reading update manifest signature")?;
    anyhow::ensure!(signature.len() == 64, "update signature must be 64 bytes");
    let key = update_public_key()?;
    UnparsedPublicKey::new(&ED25519, key)
        .verify(&bytes, &signature)
        .map_err(|_| anyhow::anyhow!("update manifest signature is invalid"))?;
    let mut manifest: Manifest =
        serde_json::from_slice(&bytes).context("parsing signed manifest")?;
    validate_manifest(&manifest)?;
    manifest.verified = true;
    Ok(manifest)
}

async fn read_bounded(response: reqwest::Response, max: usize) -> anyhow::Result<Vec<u8>> {
    anyhow::ensure!(
        response
            .content_length()
            .is_none_or(|size| size <= max as u64),
        "update response is too large"
    );
    let mut output = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("reading update response")?;
        anyhow::ensure!(
            output.len().saturating_add(chunk.len()) <= max,
            "update response is too large"
        );
        output.extend_from_slice(&chunk);
    }
    Ok(output)
}

fn http_client() -> anyhow::Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(concat!("luna/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(std::time::Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 10 {
                return attempt.error("too many update redirects");
            }
            if attempt.previous().iter().any(|url| url.scheme() == "https")
                && attempt.url().scheme() != "https"
            {
                return attempt.error("update redirect would downgrade HTTPS");
            }
            attempt.follow()
        }))
        .build()
        .context("building http client")
}

fn validate_release_override(value: &str) -> anyhow::Result<String> {
    let url = reqwest::Url::parse(value.trim()).context("invalid update feed URL")?;
    anyhow::ensure!(
        (url.scheme() == "https"
            || cfg!(debug_assertions)
                && url.scheme() == "http"
                && matches!(url.host_str(), Some("127.0.0.1" | "localhost")))
            && url.host_str().is_some(),
        "update feed must use HTTPS (or debug loopback HTTP)"
    );
    anyhow::ensure!(
        url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "update feed must be a base URL without credentials, query, or fragment"
    );
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

fn release_base() -> anyhow::Result<String> {
    #[cfg(debug_assertions)]
    if let Ok(url) = std::env::var("LUNA_RELEASES_URL")
        && !url.trim().is_empty()
    {
        return validate_release_override(&url);
    }
    Ok(RELEASE_BASE.to_owned())
}

fn decode_hex(value: &str) -> anyhow::Result<Vec<u8>> {
    let bytes = value.trim().as_bytes();
    anyhow::ensure!(bytes.len().is_multiple_of(2), "invalid hex length");
    bytes
        .chunks_exact(2)
        .map(|pair| {
            let text = std::str::from_utf8(pair)?;
            u8::from_str_radix(text, 16).context("invalid hex digit")
        })
        .collect()
}

fn update_public_key() -> anyhow::Result<Vec<u8>> {
    #[cfg(debug_assertions)]
    if let Ok(value) = std::env::var("LUNA_UPDATE_TEST_PUBLIC_KEY_HEX") {
        let key = decode_hex(&value)?;
        anyhow::ensure!(key.len() == 32, "test update public key must be 32 bytes");
        return Ok(key);
    }
    let key = decode_hex(include_str!("../../../dist/update-signing-public.hex"))?;
    anyhow::ensure!(key.len() == 32, "pinned update public key must be 32 bytes");
    Ok(key)
}

fn asset_url(manifest: &Manifest, file: &str) -> anyhow::Result<String> {
    let meta = manifest
        .files
        .get(file)
        .context("artifact is missing from signed manifest")?;
    let url = meta.url.as_deref().context("artifact URL is missing")?;
    let parsed = reqwest::Url::parse(url).context("invalid artifact URL")?;
    anyhow::ensure!(
        parsed.username().is_empty()
            && parsed.password().is_none()
            && parsed.query().is_none()
            && parsed.fragment().is_none(),
        "artifact URL has unexpected components"
    );
    let expected = format!("{RELEASE_ASSET_BASE}/v{}/{file}", manifest.version);
    #[cfg(debug_assertions)]
    if release_base()? != RELEASE_BASE {
        let base = reqwest::Url::parse(&release_base()?)?;
        anyhow::ensure!(
            parsed.scheme() == base.scheme()
                && parsed.host() == base.host()
                && parsed.port_or_known_default() == base.port_or_known_default()
                && parsed.path().ends_with(&format!("/{file}")),
            "artifact URL is outside the test feed"
        );
        return Ok(url.to_owned());
    }
    anyhow::ensure!(
        url == expected,
        "artifact URL is outside the pinned GitHub release"
    );
    Ok(url.to_owned())
}

fn validate_manifest(manifest: &Manifest) -> anyhow::Result<()> {
    anyhow::ensure!(
        version_parts(&manifest.version).is_some(),
        "invalid release version"
    );
    anyhow::ensure!(
        !manifest.files.is_empty(),
        "signed manifest has no artifacts"
    );
    for (file, meta) in &manifest.files {
        anyhow::ensure!(
            !file.contains('/')
                && !file.contains('\\')
                && file.starts_with("luna-")
                && file.contains(&format!("luna-{}-", manifest.version)),
            "invalid artifact name"
        );
        let sha = meta
            .sha256
            .as_deref()
            .context("artifact SHA-256 is missing")?;
        anyhow::ensure!(
            sha.len() == 64 && sha.bytes().all(|b| b.is_ascii_hexdigit()),
            "invalid artifact SHA-256"
        );
        asset_url(manifest, file)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Install-kind detection
// ---------------------------------------------------------------------------

/// How this binary was installed — decides the update path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallKind {
    /// `~/.luna/app/<ver>/luna` behind the `current` symlink.
    Managed { app_root: PathBuf },
    /// Running out of a macOS `.app` bundle.
    MacApp { bundle: PathBuf },
    /// Portable Windows package marked as update-managed by its package file.
    #[cfg(windows)]
    WindowsPortable { directory: PathBuf },
    /// Source build or hand-copied binary — updates are report-only.
    Unmanaged,
}

impl InstallKind {
    pub fn supports_desktop_update(&self) -> bool {
        match self {
            Self::MacApp { .. } => true,
            #[cfg(windows)]
            Self::WindowsPortable { .. } => true,
            _ => false,
        }
    }

    pub async fn stage_desktop(
        &self,
        edge_url: &str,
        manifest: &Manifest,
        data_dir: &Path,
    ) -> anyhow::Result<PathBuf> {
        match self {
            Self::MacApp { .. } => stage_mac_app(edge_url, manifest, data_dir).await,
            #[cfg(windows)]
            Self::WindowsPortable { directory } => {
                windows::stage(edge_url, manifest, directory).await
            }
            _ => bail!("this installation does not support desktop updates"),
        }
    }

    /// Install and arrange a relaunch. The UI must quit after this succeeds.
    pub fn apply_desktop(&self, staged: &Path) -> anyhow::Result<()> {
        match self {
            Self::MacApp { bundle } => apply_and_relaunch_mac_app(staged, bundle),
            #[cfg(windows)]
            Self::WindowsPortable { directory } => windows::apply(staged, directory, true),
            _ => bail!("this installation does not support desktop updates"),
        }
    }
}

pub fn detect_install() -> InstallKind {
    let Ok(exe) = std::env::current_exe() else {
        return InstallKind::Unmanaged;
    };
    let home = std::env::var_os("HOME").map(PathBuf::from);
    detect_install_from(&exe, home.as_deref())
}

fn detect_install_from(exe: &Path, home: Option<&Path>) -> InstallKind {
    detect_install_from_for_os(exe, home, std::env::consts::OS)
}

fn detect_install_from_for_os(exe: &Path, home: Option<&Path>, os: &str) -> InstallKind {
    #[cfg(windows)]
    if os == "windows" && windows::is_managed(exe) {
        return InstallKind::WindowsPortable {
            directory: exe.parent().unwrap().to_owned(),
        };
    }
    // Never interpret a coincidental Windows `%HOME%\.luna\app` layout as
    // the Unix symlink-managed installation.
    if !managed_updates_supported(os) {
        return InstallKind::Unmanaged;
    }
    if let Some(home) = home {
        // `current_exe` resolves the `current` symlink to the versioned dir.
        let app_root = home.join(".luna").join("app");
        if exe.starts_with(&app_root) {
            return InstallKind::Managed { app_root };
        }
    }
    for ancestor in exe.ancestors() {
        if ancestor.extension().is_some_and(|ext| ext == "app")
            && exe.starts_with(ancestor.join("Contents").join("MacOS"))
        {
            let production_bundle = ancestor.file_name().is_some_and(|name| name == "LUNA.app");
            #[cfg(debug_assertions)]
            let qa_bundle = std::env::var("LUNA_RELEASES_URL").is_ok()
                && std::env::var("LUNA_UPDATE_TEST_PUBLIC_KEY_HEX").is_ok();
            #[cfg(not(debug_assertions))]
            let qa_bundle = false;
            if !production_bundle && !qa_bundle {
                return InstallKind::Unmanaged;
            }
            return InstallKind::MacApp {
                bundle: ancestor.to_path_buf(),
            };
        }
    }
    InstallKind::Unmanaged
}

// ---------------------------------------------------------------------------
// Download + verify
// ---------------------------------------------------------------------------

/// Stream a signed-manifest artifact to `dest`, requiring its SHA-256.
/// A partial sidecar never becomes an installable artifact.
pub async fn download_release_file(
    _edge_url: &str,
    manifest: &Manifest,
    file: &str,
    dest: &Path,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        manifest.verified,
        "update manifest has not been signature verified"
    );
    validate_manifest(manifest)?;
    let url = asset_url(manifest, file)?;
    let expected = manifest
        .files
        .get(file)
        .and_then(|m| m.sha256.as_deref())
        .context("artifact SHA-256 is missing")?;
    let partial = dest.with_extension("partial");
    let _ = tokio::fs::remove_file(&partial).await;
    let resp = http_client()?
        .get(&url)
        .send()
        .await
        .with_context(|| format!("downloading {url}"))?
        .error_for_status()
        .with_context(|| format!("downloading {url}"))?;
    anyhow::ensure!(
        resp.content_length()
            .is_none_or(|size| size <= MAX_ARTIFACT_BYTES),
        "update artifact is too large"
    );
    let mut out = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&partial)
        .await
        .with_context(|| format!("creating {}", partial.display()))?;
    let mut hasher = Sha256::new();
    let mut stream = resp.bytes_stream();
    let mut total = 0u64;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("reading download stream")?;
        total = total.saturating_add(chunk.len() as u64);
        if total > MAX_ARTIFACT_BYTES {
            drop(out);
            let _ = tokio::fs::remove_file(&partial).await;
            bail!("update artifact is too large");
        }
        hasher.update(&chunk);
        out.write_all(&chunk).await.context("writing download")?;
    }
    out.flush().await.context("flushing update download")?;
    out.sync_all().await.context("syncing update download")?;
    drop(out);
    let actual = format!("{:x}", hasher.finalize());
    if !actual.eq_ignore_ascii_case(expected.trim()) {
        tokio::fs::remove_file(&partial).await.ok();
        bail!("checksum mismatch for {file}: expected {expected}, got {actual}");
    }
    tokio::fs::rename(&partial, dest)
        .await
        .with_context(|| format!("moving {} into place", dest.display()))?;
    Ok(())
}

fn run(program: &str, args: &[&str]) -> anyhow::Result<()> {
    let output = std::process::Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("running {program}"))?;
    if !output.status.success() {
        bail!(
            "{program} {} failed ({}): {}",
            args.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

/// Extract only regular files and directories below the expected archive root.
/// In particular, tar links and absolute or parent paths never reach disk.
fn unpack_release_archive(archive: &Path, dest: &Path, root: &str) -> anyhow::Result<()> {
    let source = std::fs::File::open(archive).context("opening update archive")?;
    let decoder = flate2::read::GzDecoder::new(source);
    let mut tarball = tar::Archive::new(decoder);
    let mut unpacked = 0u64;
    let mut entries = 0u64;
    for item in tarball.entries().context("reading update archive")? {
        let mut entry = item.context("reading update archive entry")?;
        let path = entry.path().context("reading update archive path")?;
        let mut components = path.components();
        let first = components.next();
        anyhow::ensure!(
            matches!(first, Some(std::path::Component::Normal(name)) if name == root),
            "update archive contains an unexpected root: {}",
            path.display()
        );
        let mut relative = PathBuf::new();
        for component in components {
            match component {
                std::path::Component::Normal(part) => relative.push(part),
                _ => bail!("update archive contains an unsafe path"),
            }
        }
        let target = dest.join(&relative);
        let kind = entry.header().entry_type();
        entries += 1;
        anyhow::ensure!(entries <= 100_000, "update archive has too many entries");
        if kind.is_dir() {
            std::fs::create_dir_all(&target)
                .with_context(|| format!("creating {}", target.display()))?;
        } else if kind.is_file() {
            anyhow::ensure!(
                !relative.as_os_str().is_empty(),
                "invalid archive file root"
            );
            unpacked = unpacked.saturating_add(entry.header().size()?);
            anyhow::ensure!(
                unpacked <= MAX_UNPACKED_BYTES,
                "update archive expands too large"
            );
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut output = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)
                .with_context(|| format!("creating {}", target.display()))?;
            let copied = std::io::copy(&mut entry, &mut output)?;
            anyhow::ensure!(
                copied == entry.header().size()?,
                "truncated update archive entry"
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                let mode = entry.header().mode()? & 0o755;
                std::fs::set_permissions(&target, std::fs::Permissions::from_mode(mode))?;
            }
        } else {
            bail!("update archive contains a link or special file");
        }
    }
    anyhow::ensure!(entries > 0, "update archive is empty");
    Ok(())
}

// ---------------------------------------------------------------------------
// Managed (symlink) installs — the daemon/VPS path
// ---------------------------------------------------------------------------

/// Download + safely unpack the headless tarball into `app_root/<ver>`.
pub async fn stage_headless(
    edge_url: &str,
    manifest: &Manifest,
    app_root: &Path,
) -> anyhow::Result<PathBuf> {
    // Reject unsupported targets before creating a stage or making a request.
    require_managed_update_platform()?;
    anyhow::ensure!(
        manifest.verified,
        "update manifest has not been signature verified"
    );
    validate_manifest(manifest)?;
    let version = &manifest.version;
    let dest = app_root.join(version);
    if dest.exists() {
        let current = std::fs::read_link(app_root.join("current")).ok();
        anyhow::ensure!(
            current.as_deref() != Some(dest.as_path()),
            "the staged release is already the active install"
        );
        std::fs::remove_dir_all(&dest).context("removing stale staged release")?;
    }
    let file = headless_artifact(version);
    let stage = app_root.join(format!(".stage-{version}-{}", std::process::id()));
    if stage.exists() {
        std::fs::remove_dir_all(&stage).context("removing stale headless staging directory")?;
    }
    std::fs::create_dir_all(&stage).with_context(|| format!("creating {}", stage.display()))?;
    let result = async {
        let tarball = stage.join(&file);
        download_release_file(edge_url, manifest, &file, &tarball).await?;
        let unpacked = stage.join("unpacked");
        std::fs::create_dir_all(&unpacked)?;
        let root = format!("luna-{version}-{}-{}", platform_key().0, platform_key().1);
        unpack_release_archive(&tarball, &unpacked, &root)?;
        if !unpacked.join("luna").is_file() {
            bail!("tarball {file} did not contain a luna binary");
        }
        match std::fs::rename(&unpacked, &dest) {
            Ok(()) => {}
            // Lost a race with another stager — the staged copy is equivalent.
            Err(_) if dest.join("luna").exists() => {}
            Err(err) => {
                return Err(err).with_context(|| format!("moving {} into place", dest.display()));
            }
        }
        Ok(dest.clone())
    }
    .await;
    let _ = std::fs::remove_dir_all(&stage);
    result
}

/// Atomically repoint `app_root/current` at `app_root/<ver>` (symlink to a temp
/// name, then rename over — never a window with no `current`).
pub fn apply_headless(app_root: &Path, version: &str) -> anyhow::Result<()> {
    anyhow::ensure!(version_parts(version).is_some(), "invalid release version");
    #[cfg(unix)]
    {
        let target = app_root.join(version);
        if !target.join("luna").exists() {
            bail!("{} is not a staged install", target.display());
        }
        let tmp = app_root.join(format!(".current-{}", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        std::os::unix::fs::symlink(&target, &tmp).context("creating current symlink")?;
        std::fs::rename(&tmp, app_root.join("current")).context("swapping current symlink")?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (app_root, version);
        require_managed_update_platform()?;
        unreachable!("supported managed-update platforms are Unix")
    }
}

/// Restart the installed LUNA engine service. Called after a symlink swap so the running daemon
/// picks up the new binary.
pub fn restart_service() -> anyhow::Result<()> {
    require_managed_update_platform()?;
    if cfg!(target_os = "macos") {
        let output = std::process::Command::new("id").arg("-u").output()?;
        let uid = String::from_utf8_lossy(&output.stdout).trim().to_string();
        run(
            "launchctl",
            &["kickstart", "-k", &format!("gui/{uid}/com.novnski.luna")],
        )
    } else {
        run("systemctl", &["--user", "restart", "luna.service"])
    }
}

// ---------------------------------------------------------------------------
// macOS app-bundle installs — the desktop path
// ---------------------------------------------------------------------------

/// Download + safely unpack the app tarball into a private staging directory.
pub async fn stage_mac_app(
    edge_url: &str,
    manifest: &Manifest,
    data_dir: &Path,
) -> anyhow::Result<PathBuf> {
    // Reject unsupported targets before creating a stage or making a request.
    require_mac_app_update_platform()?;
    anyhow::ensure!(
        manifest.verified,
        "update manifest has not been signature verified"
    );
    validate_manifest(manifest)?;
    let version = &manifest.version;
    let dir = data_dir.join("updates").join(version);
    let staged = dir.join("LUNA.app");
    // Re-download on every attempt. A stale extracted bundle is not an
    // authenticated cache, even when its executable still exists.
    if dir.exists() {
        std::fs::remove_dir_all(&dir).context("removing stale app staging directory")?;
    }
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    }
    let file = mac_app_artifact(version);
    let tarball = dir.join(&file);
    download_release_file(edge_url, manifest, &file, &tarball).await?;
    unpack_release_archive(&tarball, &staged, "LUNA.app")?;
    std::fs::remove_file(&tarball).ok();
    verify_mac_bundle(&staged, Some(version))
        .with_context(|| format!("app tarball {file} contains an invalid app"))?;
    Ok(staged)
}

fn bundle_info(bundle: &Path) -> anyhow::Result<(String, String)> {
    let info = plist::Value::from_file(bundle.join("Contents/Info.plist"))
        .context("reading app Info.plist")?;
    let dict = info.as_dictionary().context("invalid app Info.plist")?;
    let identifier = dict
        .get("CFBundleIdentifier")
        .and_then(plist::Value::as_string)
        .context("app bundle has no identifier")?;
    let version = dict
        .get("CFBundleShortVersionString")
        .and_then(plist::Value::as_string)
        .context("app bundle has no version")?;
    Ok((identifier.to_owned(), version.to_owned()))
}

fn verify_mac_bundle(bundle: &Path, expected_version: Option<&str>) -> anyhow::Result<()> {
    anyhow::ensure!(
        bundle.join("Contents/MacOS/luna").is_file(),
        "app executable is missing"
    );
    let (identifier, version) = bundle_info(bundle)?;
    anyhow::ensure!(
        !identifier.is_empty() && version_parts(&version).is_some(),
        "app bundle identity or version is invalid"
    );
    if let Some(expected) = expected_version {
        anyhow::ensure!(
            version == expected,
            "app bundle version does not match release"
        );
    }
    run(
        "codesign",
        &["--verify", "--deep", "--strict", &bundle.to_string_lossy()],
    )?;
    Ok(())
}

fn mac_backup_path(bundle: &Path) -> anyhow::Result<PathBuf> {
    let parent = bundle
        .parent()
        .context("app bundle has no parent directory")?;
    let name = bundle
        .file_name()
        .context("app bundle has no name")?
        .to_string_lossy();
    Ok(parent.join(format!(".{name}.previous")))
}

/// Swap the installed bundle for a verified copy. The previous app remains
/// available until a later update so a failed relaunch can restore it.
pub fn apply_mac_app(staged: &Path, bundle: &Path) -> anyhow::Result<()> {
    require_mac_app_update_platform()?;
    let expected_version = staged
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .context("staged app has no release version")?;
    anyhow::ensure!(
        version_parts(expected_version).is_some(),
        "invalid staged release version"
    );
    verify_mac_bundle(staged, Some(expected_version))?;
    let (installed_id, installed_version) = bundle_info(bundle)?;
    let (incoming_id, incoming_version) = bundle_info(staged)?;
    anyhow::ensure!(
        installed_id == incoming_id,
        "update app identifier does not match this installation"
    );
    anyhow::ensure!(
        version_newer(&incoming_version, &installed_version),
        "update app version is not newer than this installation"
    );
    let parent = bundle
        .parent()
        .context("app bundle has no parent directory")?;
    let name = bundle
        .file_name()
        .context("app bundle has no name")?
        .to_string_lossy();
    let pid = std::process::id();
    let fresh = parent.join(format!(".{name}.new-{pid}"));
    let old = mac_backup_path(bundle)?;
    let _ = std::fs::remove_dir_all(&fresh);
    run(
        "ditto",
        &[&staged.to_string_lossy(), &fresh.to_string_lossy()],
    )?;
    if let Err(err) = verify_mac_bundle(&fresh, Some(expected_version)) {
        let _ = std::fs::remove_dir_all(&fresh);
        return Err(err).context("copied update failed verification");
    }
    if old.exists() {
        std::fs::remove_dir_all(&old).context("removing previous update backup")?;
    }
    std::fs::rename(bundle, &old).context("moving the current app aside")?;
    if let Err(err) = std::fs::rename(&fresh, bundle) {
        std::fs::rename(&old, bundle).context("restoring current app after install failure")?;
        let _ = std::fs::remove_dir_all(&fresh);
        return Err(err).context("installing the new app bundle");
    }
    Ok(())
}

fn apply_and_relaunch_mac_app(staged: &Path, bundle: &Path) -> anyhow::Result<()> {
    apply_mac_app(staged, bundle)?;
    if let Err(err) = relaunch_app_after_exit(bundle) {
        let backup = mac_backup_path(bundle)?;
        let failed = bundle.with_extension(format!("failed-update-{}", std::process::id()));
        std::fs::rename(bundle, &failed).context("moving failed update aside")?;
        std::fs::rename(&backup, bundle).context("restoring app after relauncher failure")?;
        let _ = std::fs::remove_dir_all(failed);
        return Err(err);
    }
    Ok(())
}

/// Detached relauncher waits for this process to quit. If LaunchServices
/// cannot open the replacement, it restores the previous bundle and opens it.
pub fn relaunch_app_after_exit(bundle: &Path) -> anyhow::Result<()> {
    require_mac_app_update_platform()?;
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        let backup = mac_backup_path(bundle)?;
        let script = r#"
old_pid="$1"; bundle="$2"; backup="$3"
attempt=0
while /bin/kill -0 "$old_pid" 2>/dev/null; do
  attempt=$((attempt + 1))
  [ "$attempt" -lt 600 ] || exit 1
  /bin/sleep 0.2
done
if /usr/bin/open -n -a "$bundle"; then
  attempt=0
  stable=0
  while [ "$attempt" -lt 100 ]; do
    if /bin/ps -axo command= | /usr/bin/awk -v exe="$bundle/Contents/MacOS/luna" \
      'index($0, exe) == 1 && (length($0) == length(exe) || substr($0, length(exe)+1, 1) == " ") { found=1 } END { exit !found }'; then
      stable=$((stable + 1))
      [ "$stable" -lt 25 ] || exit 0
    else
      stable=0
    fi
    attempt=$((attempt + 1))
    /bin/sleep 0.2
  done
fi
failed="${bundle}.failed-update-$$"
/bin/mv "$bundle" "$failed" || exit 1
/bin/mv "$backup" "$bundle" || { /bin/mv "$failed" "$bundle"; exit 1; }
/usr/bin/open -n -a "$bundle"
"#;
        std::process::Command::new("/bin/sh")
            .args([
                "-c",
                script,
                "luna-update-relaunch",
                &std::process::id().to_string(),
                &bundle.to_string_lossy(),
                &backup.to_string_lossy(),
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .process_group(0)
            .spawn()
            .context("spawning app relauncher")?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = bundle;
        unreachable!()
    }
}

// ---------------------------------------------------------------------------
// Engine-side checker
// ---------------------------------------------------------------------------

/// What the engine reports over the `UpdateStatus` stream. Version facts only —
/// download/apply progress is owned by whoever drives the update (UI or CLI).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    pub current_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_version: Option<String>,
    #[serde(default)]
    pub update_available: bool,
    /// Epoch ms of the last successful check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl UpdateStatus {
    fn initial() -> Self {
        Self {
            current_version: current_version().to_string(),
            latest_version: None,
            update_available: false,
            checked_at: None,
            error: None,
        }
    }
}

/// `LUNA_AUTO_UPDATE=1|true|yes` — reserved for a future owned update feed.
fn auto_update_enabled() -> bool {
    std::env::var("LUNA_AUTO_UPDATE")
        .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false)
}

/// "Nothing would be interrupted by a restart right now" — wired by the engine
/// to its live-run and open-terminal registries. `None` = no gate.
pub type QuiescentCheck = Arc<dyn Fn() -> bool + Send + Sync>;

/// Background release checker: polls `{edge}/releases` on a 6h cadence and
/// publishes [`UpdateStatus`] over a watch channel (the `UpdateStatus` RPC
/// stream). Managed installs with `LUNA_AUTO_UPDATE` set stage + apply + service
/// restart on their own — but only in a quiet window: while `quiescent` reports
/// activity, the apply defers and re-probes every [`IDLE_RECHECK`].
#[derive(Clone)]
pub struct Updater {
    edge_url: String,
    status_tx: Arc<watch::Sender<UpdateStatus>>,
    check_tx: Arc<watch::Sender<u64>>,
    quiescent: Option<QuiescentCheck>,
    /// Flips to true exactly once; the check loop selects against it so
    /// cancellation lands at any await point (no tokio-util in this crate).
    shutdown_tx: Arc<watch::Sender<bool>>,
    check_task: Arc<std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,
}

impl Updater {
    /// Spawn the check loop (must run on a tokio runtime).
    pub fn spawn(edge_url: String, quiescent: Option<QuiescentCheck>) -> Self {
        let (status_tx, _) = watch::channel(UpdateStatus::initial());
        let (check_tx, _) = watch::channel(0);
        let (shutdown_tx, _) = watch::channel(false);
        let updater = Self {
            edge_url,
            status_tx: Arc::new(status_tx),
            check_tx: Arc::new(check_tx),
            quiescent,
            shutdown_tx: Arc::new(shutdown_tx),
            check_task: Arc::new(std::sync::Mutex::new(None)),
        };
        let for_loop = updater.clone();
        let task = tokio::spawn(async move { for_loop.check_loop().await });
        *updater.check_task.lock().unwrap() = Some(task);
        updater
    }

    /// Stop the check loop and wait for it to exit — a replaced runtime must
    /// not keep polling `{edge}/releases` (or auto-applying) in the background.
    /// Idempotent, and callable from any clone.
    pub async fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
        let task = self
            .check_task
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .take();
        if let Some(task) = task {
            let _ = task.await;
        }
    }

    pub fn watch(&self) -> watch::Receiver<UpdateStatus> {
        self.status_tx.subscribe()
    }

    /// Wake the release checker immediately, for example when authentication
    /// recovers after the process started offline.
    pub fn check_now(&self) {
        self.check_tx
            .send_modify(|epoch| *epoch = epoch.wrapping_add(1));
    }

    fn quiescent_now(&self) -> bool {
        self.quiescent.as_ref().is_none_or(|check| check())
    }

    async fn check_loop(&self) {
        let mut shutdown = self.shutdown_tx.subscribe();
        // Shutdown must cut the loop at ANY await point — including mid
        // `check_once()` / `auto_apply_when_idle()` HTTP — so the whole body
        // races the flag rather than checking it between iterations.
        tokio::select! {
            _ = shutdown.wait_for(|stop| *stop) => {}
            _ = async {
                let mut checks = self.check_tx.subscribe();
                tokio::select! {
                    _ = tokio::time::sleep(CHECK_INITIAL_DELAY) => {}
                    _ = checks.changed() => {}
                }
                loop {
                    let ok = self.check_once().await;
                    if ok
                        && self.status_tx.borrow().update_available
                        && auto_update_enabled()
                        && let InstallKind::Managed { .. } = detect_install()
                    {
                        self.auto_apply_when_idle().await;
                    }
                    tokio::select! {
                        _ = tokio::time::sleep(if ok { CHECK_INTERVAL } else { CHECK_RETRY }) => {}
                        _ = checks.changed() => {}
                    }
                }
            } => {}
        }
    }

    /// Sessions must never die to an update: pre-stage the download now
    /// (harmless while busy), wait for a quiet window (no live runs, no open
    /// terminals), then apply — which re-fetches the manifest (so a long defer
    /// lands on whatever is newest) and reuses the staged dir, keeping the
    /// idle→restart gap to well under a second.
    async fn auto_apply_when_idle(&self) {
        if let InstallKind::Managed { app_root } = detect_install() {
            match fetch_latest(&self.edge_url).await {
                Ok(manifest) if version_newer(&manifest.version, current_version()) => {
                    if let Err(err) = stage_headless(&self.edge_url, &manifest, &app_root).await {
                        tracing::warn!(error = %err, "auto-update staging failed");
                        return;
                    }
                }
                Ok(_) => return,
                Err(err) => {
                    tracing::warn!(error = %err, "auto-update staging fetch failed");
                    return;
                }
            }
        }
        let mut deferred = false;
        while !self.quiescent_now() {
            if !deferred {
                deferred = true;
                tracing::info!("auto-update deferred: sessions or terminals active");
            }
            tokio::time::sleep(IDLE_RECHECK).await;
        }
        match self.apply().await {
            Ok(version) => {
                tracing::info!(%version, "auto-update applied; service restarting")
            }
            Err(err) => tracing::warn!(error = %err, "auto-update failed"),
        }
    }

    /// One check; returns false on fetch failure (retry sooner).
    async fn check_once(&self) -> bool {
        match fetch_latest(&self.edge_url).await {
            Ok(manifest) => {
                let status = UpdateStatus {
                    current_version: current_version().to_string(),
                    update_available: version_newer(&manifest.version, current_version()),
                    latest_version: Some(manifest.version),
                    checked_at: Some(now_ms()),
                    error: None,
                };
                if status.update_available {
                    tracing::info!(
                        latest = status.latest_version.as_deref().unwrap_or(""),
                        current = %status.current_version,
                        "update available"
                    );
                }
                self.status_tx.send_replace(status);
                true
            }
            Err(err) => {
                tracing::debug!(error = %err, "update check failed");
                self.status_tx.send_modify(|status| {
                    status.update_available = false;
                    status.latest_version = None;
                    status.error = Some(format!("{err:#}"));
                });
                false
            }
        }
    }

    /// Stage + apply the newest release on THIS device (managed installs only),
    /// then restart the service after a short delay so the caller's RPC reply
    /// flushes before systemd/launchd kills this process.
    pub async fn apply(&self) -> anyhow::Result<String> {
        let InstallKind::Managed { app_root } = detect_install() else {
            bail!(
                "this install is not update-managed — the desktop app updates from its UI; \
                 source builds update via git"
            );
        };
        let manifest = fetch_latest(&self.edge_url).await?;
        if !version_newer(&manifest.version, current_version()) {
            bail!("already up to date ({})", current_version());
        }
        stage_headless(&self.edge_url, &manifest, &app_root).await?;
        apply_headless(&app_root, &manifest.version)?;
        tokio::spawn(async {
            tokio::time::sleep(std::time::Duration::from_millis(800)).await;
            if let Err(err) = restart_service() {
                tracing::warn!(error = %err, "service restart failed — restart the engine to finish the update");
            }
        });
        Ok(manifest.version)
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_feed_override_requires_an_https_base_url() {
        assert_eq!(
            validate_release_override(" https://example.com/releases/ ").unwrap(),
            "https://example.com/releases"
        );
        for url in [
            "http://example.com/releases",
            "file:///tmp/update",
            "https://user:password@example.com",
            "https://example.com?feed=x",
            "https://example.com/#fragment",
        ] {
            assert!(validate_release_override(url).is_err(), "accepted {url}");
        }
    }

    #[test]
    fn version_compare() {
        assert!(version_newer("0.1.1", "0.1.0"));
        assert!(version_newer("0.2.0", "0.1.9"));
        assert!(version_newer("0.1.10", "0.1.9"));
        assert!(!version_newer("v0.1.1", "0.1.0"));
        assert!(!version_newer("0.1.0.1", "0.1.0"));
        assert!(!version_newer("0.1.0", "0.1.0"));
        assert!(!version_newer("0.1.0", "0.1.1"));
        // Garbage never counts as newer.
        assert!(!version_newer("", "0.1.0"));
        assert!(!version_newer("nightly", "0.1.0"));
    }

    #[test]
    fn install_kind_detection() {
        assert_eq!(
            detect_install_from_for_os(
                Path::new("/home/u/.luna/app/0.1.1/luna"),
                Some(Path::new("/home/u")),
                "linux",
            ),
            InstallKind::Managed {
                app_root: PathBuf::from("/home/u/.luna/app")
            }
        );
        assert_eq!(
            detect_install_from_for_os(
                Path::new("/Applications/LUNA.app/Contents/MacOS/luna"),
                Some(Path::new("/Users/u")),
                "macos",
            ),
            InstallKind::MacApp {
                bundle: PathBuf::from("/Applications/LUNA.app")
            }
        );
        // A path merely containing `.app` without the bundle layout is not a bundle.
        assert_eq!(
            detect_install_from_for_os(Path::new("/tmp/foo.app/luna"), None, "macos"),
            InstallKind::Unmanaged
        );
        assert_eq!(
            detect_install_from_for_os(
                Path::new("/src/target/release/luna"),
                Some(Path::new("/home/u")),
                "linux",
            ),
            InstallKind::Unmanaged
        );
    }

    #[test]
    fn artifact_names_match_packaging() {
        let (os, arch) = platform_key();
        assert!(headless_artifact("0.2.0").starts_with("luna-0.2.0-"));
        assert_eq!(
            headless_artifact("0.2.0"),
            format!("luna-0.2.0-{os}-{arch}.tar.gz")
        );
        assert!(mac_app_artifact("0.2.0").ends_with("-app.tar.gz"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_is_not_misclassified_as_linux() {
        assert_eq!(platform_key().0, "windows");
    }

    #[cfg(windows)]
    #[test]
    fn windows_install_is_always_unmanaged() {
        assert_eq!(
            detect_install_from(
                Path::new(r"C:\Users\u\.luna\app\0.2.0\luna.exe"),
                Some(Path::new(r"C:\Users\u")),
            ),
            InstallKind::Unmanaged
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_rejects_platform_specific_updates_before_side_effects() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest = Manifest {
            version: "9.9.9".into(),
            files: BTreeMap::new(),
            ..Default::default()
        };
        let app_root = tmp.path().join("app");
        let data_dir = tmp.path().join("data");

        let managed_err = stage_headless("http://127.0.0.1:1", &manifest, &app_root)
            .await
            .unwrap_err();
        assert!(managed_err.to_string().contains("not supported on windows"));
        assert!(!app_root.exists(), "managed staging must not touch disk");

        let mac_err = stage_mac_app("http://127.0.0.1:1", &manifest, &data_dir)
            .await
            .unwrap_err();
        assert!(mac_err.to_string().contains("not supported on windows"));
        assert!(!data_dir.exists(), "macOS staging must not touch disk");

        assert!(
            apply_headless(&app_root, &manifest.version)
                .unwrap_err()
                .to_string()
                .contains("not supported on windows")
        );
        assert!(
            apply_mac_app(&data_dir.join("LUNA.app"), &data_dir.join("Installed.app"))
                .unwrap_err()
                .to_string()
                .contains("not supported on windows")
        );
        assert!(
            restart_service()
                .unwrap_err()
                .to_string()
                .contains("not supported on windows")
        );
    }

    #[test]
    fn manifest_parses_with_and_without_files() {
        let full: Manifest = serde_json::from_str(
            r#"{"version":"0.1.1","files":{"luna-0.1.1-linux-x86_64.tar.gz":{"sha256":"abc"}}}"#,
        )
        .unwrap();
        assert_eq!(full.version, "0.1.1");
        assert_eq!(
            full.files["luna-0.1.1-linux-x86_64.tar.gz"]
                .sha256
                .as_deref(),
            Some("abc")
        );
        let bare: Manifest = serde_json::from_str(r#"{"version":"0.1.1"}"#).unwrap();
        assert!(bare.files.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn headless_symlink_swap() {
        let tmp = tempfile::tempdir().unwrap();
        let app_root = tmp.path().join("app");
        for ver in ["0.1.0", "0.1.1"] {
            std::fs::create_dir_all(app_root.join(ver)).unwrap();
            std::fs::write(app_root.join(ver).join("luna"), ver).unwrap();
        }
        apply_headless(&app_root, "0.1.0").unwrap();
        assert_eq!(
            std::fs::read_link(app_root.join("current")).unwrap(),
            app_root.join("0.1.0")
        );
        // Swap over an existing symlink.
        apply_headless(&app_root, "0.1.1").unwrap();
        assert_eq!(
            std::fs::read_link(app_root.join("current")).unwrap(),
            app_root.join("0.1.1")
        );
        // Unstaged version refuses.
        assert!(apply_headless(&app_root, "0.2.0").is_err());
    }
}
