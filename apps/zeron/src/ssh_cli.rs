//! System-SSH transport for one explicitly selected LUNA environment.
//! The forwarded socket is the L29 guarded RPC listener, never local-owner IPC.

use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, anyhow, bail};
use clap::{Args, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, Command};
use zeron_proto::{EngineInfo, TaskIdentityView, WorkspaceScope};
use zeron_rpc::remote_auth::{
    AuthenticateRemote, CAPABILITY, IssuedPairing, PROTOCOL_VERSION, RedeemPairing, RemoteTarget,
};
use zeron_rpc::{RpcClient, methods};

const REMOTE_SCRIPT: &str = include_str!("ssh_remote.sh");
const READY_WAIT: Duration = Duration::from_secs(20);

struct Launch<'a> {
    version: &'a str,
    sha: &'a str,
    key: &'a str,
    owner: &'a str,
}

#[derive(Debug, Args)]
pub struct SshArgs {
    #[command(subcommand)]
    command: SshCommand,
}

#[derive(Debug, Subcommand)]
enum SshCommand {
    /// Install or reuse the headless artifact, forward RPC, and verify an exact target.
    Probe(ProbeArgs),
    /// Stop only the remote process recorded as owned by this client.
    Stop(StopArgs),
}

#[derive(Debug, Args)]
struct TargetArgs {
    /// SSH config alias or user@host; system known_hosts must already trust it.
    #[arg(long)]
    host: String,
    #[arg(long)]
    machine: String,
    #[arg(long)]
    environment: String,
    #[arg(long, value_enum)]
    workspace_scope: ScopeArg,
    #[arg(long, default_value_t = 37655)]
    remote_port: u16,
    #[arg(long, default_value_t = 37654)]
    ipc_port: u16,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ScopeArg {
    Local,
    Synced,
    Development,
}

impl From<ScopeArg> for WorkspaceScope {
    fn from(value: ScopeArg) -> Self {
        match value {
            ScopeArg::Local => Self::Local,
            ScopeArg::Synced => Self::Synced,
            ScopeArg::Development => Self::Development,
        }
    }
}

impl ScopeArg {
    fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Synced => "synced",
            Self::Development => "development",
        }
    }
}

#[derive(Debug, Args)]
struct ProbeArgs {
    #[command(flatten)]
    target: TargetArgs,
    /// Locally packaged luna-headless-vVERSION-PLATFORM-ARCH binary.
    #[arg(long)]
    artifact: PathBuf,
    /// File containing the existing L29 device session token, mode 0600.
    #[arg(long)]
    session_file: PathBuf,
    /// Explicitly issue and redeem one environment/tasks-read pairing if the session file is absent.
    #[arg(long)]
    enroll: bool,
    /// Redeem an existing one-time L29 pairing credential from a private file.
    #[arg(long, conflicts_with = "enroll")]
    pairing_file: Option<PathBuf>,
    /// Optional exact project and task identity to read.
    #[arg(long, requires = "task")]
    project: Option<String>,
    #[arg(long, requires = "project")]
    task: Option<String>,
}

#[derive(Debug, Args)]
struct StopArgs {
    #[command(flatten)]
    target: TargetArgs,
}

#[derive(Debug, Serialize, Deserialize)]
struct OwnedLaunch {
    host: String,
    machine: String,
    environment: String,
    workspace_scope: WorkspaceScope,
    version: String,
    sha256: String,
    remote_port: u16,
    ipc_port: u16,
    owner: String,
}

pub async fn run(args: SshArgs, data_dir: PathBuf) -> anyhow::Result<()> {
    match args.command {
        SshCommand::Probe(args) => probe(args, &data_dir).await,
        SshCommand::Stop(args) => stop(args, &data_dir).await,
    }
}

async fn probe(args: ProbeArgs, data_dir: &Path) -> anyhow::Result<()> {
    validate_target(&args.target)?;
    let version = env!("CARGO_PKG_VERSION");
    let sha = verify_local_artifact(&args.artifact, version).await?;
    let pairing_credential = args
        .pairing_file
        .as_deref()
        .map(read_private_token)
        .transpose()?;
    let token = if args.enroll || pairing_credential.is_some() {
        if args.session_file.exists() {
            bail!("session file already exists; reuse it without pairing options");
        }
        None
    } else {
        Some(read_private_token(&args.session_file)?)
    };
    let key = state_key(&args.target);
    let state_path = state_path(data_dir, &key);
    let prior = read_owned(&state_path)?;
    let had_matching_prior = prior
        .as_ref()
        .is_some_and(|state| owned_target_matches(state, &args.target, version, &sha));
    let owner = prior
        .as_ref()
        .filter(|state| owned_target_matches(state, &args.target, version, &sha))
        .map(|state| state.owner.clone())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    let platform = remote_script(
        &args.target.host,
        "platform",
        version,
        &sha,
        &args.target,
        &key,
        &owner,
    )
    .await?;
    check_platform(&args.artifact, version, platform.trim())?;
    let installed = remote_script(
        &args.target.host,
        "inspect",
        version,
        &sha,
        &args.target,
        &key,
        &owner,
    )
    .await?;
    if installed.trim() == "missing" {
        if let Err(error) =
            upload_artifact(&args.target.host, &args.artifact, version, &owner).await
        {
            let _ = remote_script(
                &args.target.host,
                "discard",
                version,
                &sha,
                &args.target,
                &key,
                &owner,
            )
            .await;
            return Err(error);
        }
        remote_script(
            &args.target.host,
            "install",
            version,
            &sha,
            &args.target,
            &key,
            &owner,
        )
        .await?;
    } else if installed.trim() != "installed" {
        bail!("unexpected remote artifact status");
    }
    let started = remote_script(
        &args.target.host,
        "start",
        version,
        &sha,
        &args.target,
        &key,
        &owner,
    )
    .await?;
    let kind = parse_started(&started, args.target.remote_port)?;
    let launch = Launch {
        version,
        sha: &sha,
        key: &key,
        owner: &owner,
    };
    let result = async {
        let readback = probe_forward(
            &args,
            token.as_deref(),
            pairing_credential.as_deref(),
            &launch,
        )
        .await?;
        let status = remote_script(
            &args.target.host,
            "status",
            version,
            &sha,
            &args.target,
            &key,
            &owner,
        )
        .await?;
        if status.trim() != format!("running-{kind}") {
            bail!("remote process identity changed after authenticated RPC readback");
        }
        Ok::<_, anyhow::Error>(readback)
    }
    .await;
    if result.is_err() && kind == "owned" && !had_matching_prior {
        // The new process never passed target/auth readback. Keep failure loud
        // and do not leave an unverified process running when cleanup works.
        if let Err(error) = remote_script(
            &args.target.host,
            "stop",
            version,
            &sha,
            &args.target,
            &key,
            &owner,
        )
        .await
        {
            eprintln!("new remote process cleanup uncertain: {error}");
        }
    }
    let (readback, issued_token) = result?;
    if let Some(token) = issued_token
        && let Err(error) = write_private_token(&args.session_file, &token)
    {
        if kind == "owned" && !had_matching_prior {
            let _ = remote_script(
                &args.target.host,
                "stop",
                version,
                &sha,
                &args.target,
                &key,
                &owner,
            )
            .await;
        }
        return Err(error.context("persist newly issued session; process cleanup may be uncertain"));
    }
    if kind == "owned" {
        let state = OwnedLaunch {
            host: args.target.host.clone(),
            machine: args.target.machine.clone(),
            environment: args.target.environment.clone(),
            workspace_scope: args.target.workspace_scope.into(),
            version: version.into(),
            sha256: sha,
            remote_port: args.target.remote_port,
            ipc_port: args.target.ipc_port,
            owner,
        };
        if let Err(error) = write_owned(&state_path, &state) {
            if !had_matching_prior {
                let _ = remote_script(
                    &args.target.host,
                    "stop",
                    version,
                    &state.sha256,
                    &args.target,
                    &key,
                    &state.owner,
                )
                .await;
            }
            return Err(error.context("persist owned remote process; cleanup may be uncertain"));
        }
    } else if prior.is_some() {
        fs::remove_file(&state_path)
            .with_context(|| format!("clear stale ownership at {}", state_path.display()))?;
    }
    println!(
        "Remote RPC verified: machine={} environment={} scope={:?} process={kind}",
        args.target.machine, args.target.environment, args.target.workspace_scope
    );
    if let Some((project, task)) = readback {
        println!("Task verified: project={project} task={task}");
    }
    Ok(())
}

async fn stop(args: StopArgs, data_dir: &Path) -> anyhow::Result<()> {
    validate_target(&args.target)?;
    let key = state_key(&args.target);
    let path = state_path(data_dir, &key);
    let Some(state) = read_owned(&path)? else {
        println!("No owned remote process recorded for this target");
        return Ok(());
    };
    if !owned_target_matches(&state, &args.target, &state.version, &state.sha256) {
        bail!("recorded ownership target differs; remote process was not stopped");
    }
    let reply = remote_script(
        &state.host,
        "stop",
        &state.version,
        &state.sha256,
        &args.target,
        &key,
        &state.owner,
    )
    .await?;
    if reply.trim() != "stopped" {
        bail!("remote process stop was not confirmed: {}", reply.trim());
    }
    fs::remove_file(&path)?;
    println!(
        "Owned remote process stopped for machine={} environment={}",
        state.machine, state.environment
    );
    Ok(())
}

fn validate_target(target: &TargetArgs) -> anyhow::Result<()> {
    let host = target.host.as_bytes();
    if host.is_empty()
        || host[0] == b'-'
        || host
            .iter()
            .any(|byte| !byte.is_ascii_alphanumeric() && !matches!(byte, b'@' | b'.' | b'_' | b'-'))
        || host.iter().filter(|byte| **byte == b'@').count() > 1
    {
        bail!("SSH host must be a plain alias or user@host; use ~/.ssh/config for options");
    }
    if uuid::Uuid::parse_str(&target.machine).is_err()
        || target.environment.len() != 64
        || !target
            .environment
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        bail!("machine must be a UUID and environment a 64-digit profile fingerprint");
    }
    if target.remote_port == 0 || target.ipc_port == 0 || target.remote_port == target.ipc_port {
        bail!("remote and local-owner IPC ports must be nonzero and distinct");
    }
    Ok(())
}

async fn verify_local_artifact(path: &Path, version: &str) -> anyhow::Result<String> {
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| anyhow!("artifact has no UTF-8 filename"))?;
    if !name.starts_with(&format!("luna-headless-v{version}-")) {
        bail!("artifact filename does not match this LUNA version {version}");
    }
    let mut source = File::open(path).context("open headless artifact")?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let size = source.read(&mut buffer)?;
        if size == 0 {
            break;
        }
        digest.update(&buffer[..size]);
    }
    let actual = format!("{:x}", digest.finalize());
    let manifest = fs::read_to_string(path.with_file_name(format!("{name}.sha256")))
        .context("read artifact checksum manifest")?;
    let mut fields = manifest.split_whitespace();
    if fields.next() != Some(actual.as_str())
        || fields.next() != Some(name)
        || fields.next().is_some()
    {
        bail!("headless artifact checksum manifest mismatch");
    }
    let output = Command::new(path)
        .arg("--version")
        .output()
        .await
        .context("execute headless artifact version check")?;
    if !output.status.success()
        || String::from_utf8_lossy(&output.stdout).trim() != format!("luna-headless {version}")
    {
        bail!("headless artifact version check failed");
    }
    Ok(actual)
}

fn check_platform(path: &Path, version: &str, remote: &str) -> anyhow::Result<()> {
    let (os, arch) = remote
        .split_once(' ')
        .ok_or_else(|| anyhow!("remote platform readback malformed"))?;
    let os = match os {
        "Darwin" => "darwin",
        "Linux" => "linux",
        _ => bail!("remote OS unsupported: {os}"),
    };
    let arch = match arch {
        "arm64" | "aarch64" => "arm64",
        "x86_64" | "amd64" => "x64",
        _ => bail!("remote architecture unsupported: {arch}"),
    };
    let expected = format!("luna-headless-v{version}-{os}-{arch}");
    if path.file_name().and_then(|value| value.to_str()) != Some(expected.as_str()) {
        bail!("artifact platform mismatch; remote requires {expected}");
    }
    Ok(())
}

fn state_key(target: &TargetArgs) -> String {
    let mut digest = Sha256::new();
    for field in [&target.host, &target.machine, &target.environment] {
        digest.update((field.len() as u64).to_be_bytes());
        digest.update(field.as_bytes());
    }
    format!("{:x}", digest.finalize())[..24].to_owned()
}

fn state_path(data_dir: &Path, key: &str) -> PathBuf {
    data_dir.join("ssh-launch").join(format!("{key}.json"))
}

fn owned_target_matches(
    state: &OwnedLaunch,
    target: &TargetArgs,
    version: &str,
    sha: &str,
) -> bool {
    state.host == target.host
        && state.machine == target.machine
        && state.environment == target.environment
        && state.workspace_scope == target.workspace_scope.into()
        && state.version == version
        && state.sha256 == sha
        && state.remote_port == target.remote_port
        && state.ipc_port == target.ipc_port
}

fn read_owned(path: &Path) -> anyhow::Result<Option<OwnedLaunch>> {
    if !path.exists() {
        return Ok(None);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.file_type().is_file() || metadata.permissions().mode() & 0o077 != 0 {
            bail!("SSH ownership file is not private and regular");
        }
    }
    let state: OwnedLaunch = serde_json::from_slice(&fs::read(path)?)?;
    if uuid::Uuid::parse_str(&state.owner).is_err()
        || !valid_version(&state.version)
        || state.sha256.len() != 64
        || !state.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
        || state.remote_port == 0
        || state.ipc_port == 0
        || state.remote_port == state.ipc_port
    {
        bail!("SSH ownership record is malformed");
    }
    Ok(Some(state))
}

fn valid_version(version: &str) -> bool {
    !version.is_empty()
        && version
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'.')
}

fn write_owned(path: &Path, value: &OwnedLaunch) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("ownership path has no parent"))?;
    fs::create_dir_all(parent)?;
    let temp = parent.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temp)?;
    serde_json::to_writer(&mut file, value)?;
    std::io::Write::write_all(&mut file, b"\n")?;
    file.sync_all()?;
    fs::rename(&temp, path)?;
    Ok(())
}

fn read_private_token(path: &Path) -> anyhow::Result<String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.file_type().is_file() || metadata.permissions().mode() & 0o077 != 0 {
            bail!("session token file must be a regular mode-0600 file");
        }
    }
    let token = fs::read_to_string(path)?.trim().to_owned();
    if token.is_empty() || token.len() > 512 || token.chars().any(char::is_whitespace) {
        bail!("session token file is empty or malformed");
    }
    Ok(token)
}

fn write_private_token(path: &Path, token: &str) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    let result = (|| -> anyhow::Result<()> {
        std::io::Write::write_all(&mut file, token.as_bytes())?;
        std::io::Write::write_all(&mut file, b"\n")?;
        file.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(path);
    }
    result
}

fn ssh_command(host: &str) -> Command {
    let mut cmd = Command::new(std::env::var_os("LUNA_SSH_BIN").unwrap_or_else(|| "ssh".into()));
    cmd.args([
        "-T",
        "-o",
        "BatchMode=yes",
        "-o",
        "StrictHostKeyChecking=yes",
        "-o",
        "ForwardAgent=no",
        "-o",
        "PermitLocalCommand=no",
        "-o",
        "ConnectTimeout=10",
        host,
    ]);
    cmd.kill_on_drop(true);
    cmd
}

async fn remote_script(
    host: &str,
    mode: &str,
    version: &str,
    sha: &str,
    target: &TargetArgs,
    key: &str,
    owner: &str,
) -> anyhow::Result<String> {
    let mut cmd = ssh_command(host);
    cmd.args([
        "sh",
        "-s",
        "--",
        mode,
        version,
        sha,
        &target.remote_port.to_string(),
        &target.ipc_port.to_string(),
        key,
        owner,
        &target.machine,
        &target.environment,
        target.workspace_scope.as_str(),
    ]);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().context("launch system SSH")?;
    child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("SSH stdin unavailable"))?
        .write_all(REMOTE_SCRIPT.as_bytes())
        .await?;
    let output = tokio::time::timeout(Duration::from_secs(60), child.wait_with_output())
        .await
        .map_err(|_| anyhow!("SSH {mode} timed out"))??;
    if !output.status.success() {
        bail!(
            "SSH {mode} failed (exit {}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

async fn upload_artifact(
    host: &str,
    path: &Path,
    version: &str,
    owner: &str,
) -> anyhow::Result<()> {
    let command = format!(
        "umask 077; mkdir -p \"$HOME/.luna/runtime/versions/{version}\" && cat > \"$HOME/.luna/runtime/versions/{version}/.incoming-{owner}\""
    );
    let file = File::open(path)?;
    let mut cmd = ssh_command(host);
    cmd.arg(command).stdin(Stdio::from(file));
    let output = tokio::time::timeout(Duration::from_secs(120), cmd.output())
        .await
        .map_err(|_| anyhow!("SSH artifact upload timed out"))??;
    if !output.status.success() {
        bail!(
            "SSH artifact upload failed (exit {}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

fn parse_started(output: &str, port: u16) -> anyhow::Result<&'static str> {
    let mut lines = output.lines();
    if lines.next() != Some(format!("port={port}").as_str()) {
        bail!("remote port readback mismatch");
    }
    let kind = match lines.next() {
        Some("kind=owned") => "owned",
        Some("kind=reused") => "reused",
        _ => bail!("remote process ownership readback malformed"),
    };
    if lines.next().is_some() {
        bail!("remote process readback has extra data");
    }
    Ok(kind)
}

async fn probe_forward(
    args: &ProbeArgs,
    token: Option<&str>,
    pairing_credential: Option<&str>,
    launch: &Launch<'_>,
) -> anyhow::Result<(Option<(String, String)>, Option<String>)> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let local_port = listener.local_addr()?.port();
    drop(listener);
    let forward = format!(
        "127.0.0.1:{local_port}:127.0.0.1:{}",
        args.target.remote_port
    );
    let mut tunnel = Command::new(std::env::var_os("LUNA_SSH_BIN").unwrap_or_else(|| "ssh".into()));
    tunnel.args([
        "-N",
        "-T",
        "-o",
        "BatchMode=yes",
        "-o",
        "StrictHostKeyChecking=yes",
        "-o",
        "ForwardAgent=no",
        "-o",
        "ExitOnForwardFailure=yes",
        "-o",
        "ConnectTimeout=10",
        "-L",
        &forward,
        &args.target.host,
    ]);
    tunnel
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut tunnel = tunnel.spawn().context("launch SSH port forward")?;
    let result = authenticate_forward(
        &mut tunnel,
        local_port,
        args,
        token,
        pairing_credential,
        launch,
    )
    .await;
    let _ = tunnel.start_kill();
    let _ = tunnel.wait().await;
    result
}

async fn authenticate_forward(
    tunnel: &mut Child,
    local_port: u16,
    args: &ProbeArgs,
    token: Option<&str>,
    pairing_credential: Option<&str>,
    launch: &Launch<'_>,
) -> anyhow::Result<(Option<(String, String)>, Option<String>)> {
    let url = format!("ws://127.0.0.1:{local_port}");
    let deadline = tokio::time::Instant::now() + READY_WAIT;
    let client = loop {
        if let Some(status) = tunnel.try_wait()? {
            bail!("SSH port forward exited before RPC readiness: {status}");
        }
        if let Ok(Ok(client)) =
            tokio::time::timeout(Duration::from_secs(1), zeron_rpc::connect_ws(&url)).await
        {
            break client;
        }
        if tokio::time::Instant::now() >= deadline {
            bail!("remote RPC did not become ready through SSH");
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    };
    let target = RemoteTarget {
        device_id: args.target.machine.clone(),
        workspace_scope: args.target.workspace_scope.into(),
        environment_id: args.target.environment.clone(),
        protocol_version: PROTOCOL_VERSION,
        required_capabilities: vec![CAPABILITY.into()],
    };
    let new_token = if let Some(token) = token {
        token.to_owned()
    } else {
        let credential = if let Some(credential) = pairing_credential {
            credential.to_owned()
        } else {
            let issued: IssuedPairing = serde_json::from_str(
                &remote_script(
                    &args.target.host,
                    "pair",
                    launch.version,
                    launch.sha,
                    &args.target,
                    launch.key,
                    launch.owner,
                )
                .await?,
            )?;
            if issued.target != target {
                bail!("SSH pairing was issued for another remote target");
            }
            issued.credential
        };
        let session = client
            .redeem_remote_pairing(RedeemPairing {
                target: target.clone(),
                credential,
                client_label: "LUNA SSH client".into(),
            })
            .await?;
        if session.target != target {
            bail!("remote session target changed during pairing");
        }
        session.session_token
    };
    let _: zeron_rpc::remote_auth::SessionView = tokio::time::timeout(
        Duration::from_secs(5),
        client.authenticate_remote(AuthenticateRemote {
            target,
            session_token: new_token.clone(),
        }),
    )
    .await
    .map_err(|_| anyhow!("remote authentication timed out"))??;
    let info: EngineInfo = rpc_call(&client, methods::ENGINE_INFO, json!({})).await?;
    if info.device_id != args.target.machine
        || info.workspace_scope != args.target.workspace_scope.into()
        || !info.supports(CAPABILITY)
    {
        bail!("authenticated RPC answered for another machine or incompatible capability");
    }
    let _: serde_json::Value = rpc_call(&client, methods::ENGINE_READY, json!({})).await?;
    if let (Some(project), Some(task)) = (&args.project, &args.task) {
        let view: TaskIdentityView =
            rpc_call(&client, methods::GET_TASK_IDENTITY, json!({"taskId": task})).await?;
        // L29 authenticates the profile fingerprint above. L06 task records
        // retain the owning machine ID as their workspace environment ID.
        if view.task.task_id != *task
            || view.task.workspace_id != view.workspace.workspace_id
            || view.workspace.environment_id != info.device_id
            || view.workspace.source_space_id.as_deref() != Some(project.as_str())
            || view.bindings.iter().any(|binding| binding.task_id != *task)
            || view.current_binding.as_ref().is_some_and(|binding| {
                binding.task_id != *task
                    || view.task.current_binding_id.as_deref() != Some(binding.binding_id.as_str())
                    || binding.environment_id != info.device_id
                    || binding.workspace_id != view.workspace.workspace_id
            })
        {
            bail!("remote task identity differs from exact project/task target");
        }
        return Ok((
            Some((project.clone(), task.clone())),
            token.is_none().then_some(new_token),
        ));
    }
    Ok((None, token.is_none().then_some(new_token)))
}

async fn rpc_call<T: serde::de::DeserializeOwned>(
    client: &RpcClient,
    method: &str,
    params: serde_json::Value,
) -> anyhow::Result<T> {
    let result = tokio::time::timeout(Duration::from_secs(5), client.call_as(method, params))
        .await
        .map_err(|_| anyhow!("{method} timed out"))??;
    Ok(result)
}
