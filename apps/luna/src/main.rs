//! LUNA — headed by default; `luna headless` runs the engine alone. Both start
//! local-only without credentials. `luna login` and `luna logout` select the
//! profile used by the next engine start without mutating a live runtime.

#![cfg_attr(windows, windows_subsystem = "windows")]

mod auth_cli;
mod daemon;
mod inspect_cli;
#[cfg(target_os = "macos")]
mod native_fixture;
mod paths;
mod remote_cli;
mod ssh_cli;
mod update_cli;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "luna",
    version,
    about = "Multi-device controller for coding agents"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    /// Open a LUNA conversation URL.
    #[arg(value_name = "URL")]
    open_url: Option<String>,
    #[cfg(windows)]
    #[arg(long, hide = true)]
    wait_for_exit: Option<u32>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the engine without a UI (local-only unless a saved session enables sync).
    Headless,
    /// Explicit, temporary, mock-only native QA using this packaged executable.
    #[cfg(target_os = "macos")]
    #[command(hide = true)]
    NativeFixture {
        #[arg(long)]
        root: std::path::PathBuf,
        #[arg(long)]
        check_only: bool,
    },
    /// Sign in and enable sync on the next engine start.
    Login,
    /// Remove the saved session and return to local-only on the next start.
    Logout,
    /// Show workspace mode, optional auth, and engine status.
    Status,
    /// Inspect the running local engine through its typed RPC (read only).
    Inspect(inspect_cli::InspectArgs),
    /// Manage one-time remote pairing grants and device sessions on this engine.
    Remote(remote_cli::RemoteArgs),
    /// Connect to one owned environment through system SSH and authenticated RPC.
    Ssh(ssh_cli::SshArgs),
    /// Live sync introspection from the running engine: per-room connection
    /// state, last pushed-frame/ack ages, rejoin/probe/resync counters.
    Sync,
    /// Private BB-style archives. `now` requires LUNA to be stopped.
    Backup {
        #[command(subcommand)]
        command: BackupCommand,
    },
    #[cfg(target_os = "linux")]
    /// Trigger an Appshot in the running headed instance (desktop shortcut fallback).
    Appshot,
    /// Manage `luna headless` as a background service (launchd / systemd --user).
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },
    /// Check or install an update from the signed GitHub Release feed.
    Update {
        #[arg(long)]
        check: bool,
    },
}

#[derive(Subcommand)]
enum DaemonCommand {
    /// Install, enable, and start the service (captures LUNA_* app env).
    Install,
    /// Stop and remove the service.
    Uninstall,
    /// Start the installed service.
    Start,
    /// Stop the service.
    Stop,
    /// Restart the service.
    Restart,
    /// Show the service manager's view of the daemon.
    Status,
}

#[derive(Subcommand)]
enum BackupCommand {
    /// Create and verify an archive while no LUNA engine owns the data root.
    Now,
    /// Show the newest verified automatic or manual archive.
    Status,
    /// Preview counts, storage and compatibility without restoring an archive.
    Preview { archive: std::path::PathBuf },
    /// Restore a verified archive while LUNA is stopped; keep previous files.
    Restore { archive: std::path::PathBuf },
    /// Roll back an interrupted offline restore before starting LUNA.
    Recover,
}

fn edge_url_from_env() -> String {
    std::env::var("LUNA_EDGE_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| luna_engine::OFFLINE_EDGE_URL.into())
}

/// An owned endpoint is required before auth or a dev bearer can opt in.
fn workos_client_id_from_env(edge_url: &str) -> Option<String> {
    (edge_url != luna_engine::OFFLINE_EDGE_URL)
        .then(|| std::env::var("LUNA_WORKOS_CLIENT_ID").ok())
        .flatten()
        .filter(|id| !id.trim().is_empty())
}

fn edge_token_from_env(edge_url: &str) -> Option<String> {
    (edge_url != luna_engine::OFFLINE_EDGE_URL)
        .then(|| std::env::var("LUNA_EDGE_TOKEN").ok())
        .flatten()
        .filter(|token| !token.trim().is_empty())
}

fn validate_owned_service_config() -> anyhow::Result<()> {
    let edge = std::env::var("LUNA_EDGE_URL").unwrap_or_default();
    if edge.trim().is_empty() || edge.trim() == luna_engine::OFFLINE_EDGE_URL {
        anyhow::ensure!(
            std::env::var_os("LUNA_WORKOS_CLIENT_ID").is_none()
                && std::env::var_os("LUNA_EDGE_TOKEN").is_none(),
            "LUNA auth requires an explicit owned LUNA_EDGE_URL"
        );
        return Ok(());
    }
    luna_engine::canonical_owned_edge_url(&edge)?;
    Ok(())
}

/// mimalloc, macOS only: libmalloc never returns the streaming churn's
/// high-water pages, so transient allocation became permanent RSS
/// (docs/memory-plan.md §1). Pinned to mimalloc v2 in the workspace manifest —
/// the crate's default v3 has the same pathology (churn retained as permanent
/// RSS, ~6x glibc's growth on identical workloads, no idle recovery). Linux
/// measured flat on glibc, so it keeps the system allocator.
#[cfg(target_os = "macos")]
#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() -> anyhow::Result<()> {
    #[cfg(windows)]
    attach_parent_console();
    let argv: Vec<_> = std::env::args_os().collect();
    let inspect_json = argv.get(1).is_some_and(|arg| arg == "inspect")
        && argv.iter().skip(2).any(|arg| arg == "--json");
    #[cfg(target_os = "macos")]
    let qa_root = native_fixture::qa_root_from_executable(&std::env::current_exe()?)?;
    #[cfg(target_os = "macos")]
    let cli = if let Some(root) = qa_root {
        anyhow::ensure!(argv.len() == 1, "QA bundle accepts no launch arguments");
        Cli {
            command: Some(Command::NativeFixture {
                root,
                check_only: false,
            }),
            open_url: None,
        }
    } else {
        parse_cli(argv, inspect_json)
    };
    #[cfg(not(target_os = "macos"))]
    let cli = parse_cli(argv, inspect_json);
    #[cfg(target_os = "macos")]
    let native_root = match &cli.command {
        Some(Command::NativeFixture { root, .. }) => Some(native_fixture::validate_root(root)?),
        _ => None,
    };
    #[cfg(target_os = "macos")]
    if native_root.is_none() {
        validate_owned_service_config()?;
    }
    #[cfg(not(target_os = "macos"))]
    validate_owned_service_config()?;
    #[cfg(windows)]
    if let Some(pid) = cli.wait_for_exit {
        luna_update::windows::wait_for_exit(pid)?;
    }
    // Long-running modes log at info, one-shot CLI commands at warn (RUST_LOG
    // overrides either).
    // loro's internal block-encode diagnostics log at info and flood
    // journald on every snapshot export — enough to fill a disk on a
    // long-running headless host. Quiet them by default (RUST_LOG still
    // overrides the whole filter).
    let long_running = matches!(&cli.command, None | Some(Command::Headless)) || {
        #[cfg(target_os = "macos")]
        {
            matches!(
                &cli.command,
                Some(Command::NativeFixture {
                    check_only: false,
                    ..
                })
            )
        }
        #[cfg(not(target_os = "macos"))]
        {
            false
        }
    };
    let default_filter = if long_running {
        "info,loro_internal=warn,loro=warn"
    } else {
        "warn"
    };
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| default_filter.into());
    // Long-running modes mirror stdout logging to {data_dir}/logs — a headed
    // app launched from Finder has no visible stdout, which left every sync
    // wedge report ("stale until restart") with zero diagnostics even though
    // the engine logs the exact failure line. One file per launch, previous
    // launch kept as `.old`.
    #[cfg(target_os = "macos")]
    let fixture_log = native_root
        .as_ref()
        .filter(|_| long_running)
        .map(|root| {
            open_log_file_in(&root.join("logs"), "native-fixture")
                .ok_or_else(|| anyhow::anyhow!("cannot open synthetic native fixture log"))
        })
        .transpose()?;
    let log_file = if long_running {
        #[cfg(target_os = "macos")]
        if native_root.is_some() {
            fixture_log
        } else {
            let mode = if cli.command.is_some() {
                "headless"
            } else {
                "headed"
            };
            open_log_file(mode)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let mode = if cli.command.is_some() {
                "headless"
            } else {
                "headed"
            };
            open_log_file(mode)
        }
    } else {
        None
    };
    {
        use tracing_subscriber::layer::SubscriberExt;
        use tracing_subscriber::util::SubscriberInitExt;
        let registry = tracing_subscriber::registry()
            .with(filter)
            .with(tracing_subscriber::fmt::layer());
        match log_file {
            Some(file) => registry
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_ansi(false)
                        .with_writer(std::sync::Arc::new(file)),
                )
                .init(),
            None => registry.init(),
        }
    }

    if long_running {
        // Finder launches have no visible stderr. Mirror the panic location
        // and backtrace into the same rotating log as engine diagnostics.
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            tracing::error!(panic = %info,
                backtrace = %std::backtrace::Backtrace::force_capture(),
                "application panic");
            default_hook(info);
        }));
    }

    match cli.command {
        #[cfg(target_os = "macos")]
        Some(Command::NativeFixture { check_only, .. }) => {
            native_fixture::run(native_root.expect("validated fixture root"), check_only)
        }
        Some(Command::Headless) => {
            let runtime = tokio::runtime::Runtime::new()?;
            runtime.block_on(async {
                let engine = luna_engine::Engine::new(engine_config_from_env());
                engine.run().await
            })
        }
        Some(Command::Login) => {
            let runtime = tokio::runtime::Runtime::new()?;
            runtime.block_on(auth_cli::login(engine_config_from_env()))
        }
        Some(Command::Logout) => {
            let runtime = tokio::runtime::Runtime::new()?;
            runtime.block_on(auth_cli::logout(engine_config_from_env()))
        }
        Some(Command::Status) => {
            let runtime = tokio::runtime::Runtime::new()?;
            runtime.block_on(auth_cli::status(engine_config_from_env()))
        }
        Some(Command::Inspect(args)) => {
            let runtime = tokio::runtime::Runtime::new()?;
            let code = runtime.block_on(inspect_cli::run(args));
            std::process::exit(code);
        }
        Some(Command::Remote(args)) => {
            let runtime = tokio::runtime::Runtime::new()?;
            runtime.block_on(remote_cli::run(args, engine_config_from_env().ipc_port))
        }
        Some(Command::Ssh(args)) => ssh_cli::run(args, paths::data_dir()),
        Some(Command::Sync) => {
            let runtime = tokio::runtime::Runtime::new()?;
            runtime.block_on(sync_cli(engine_config_from_env().ipc_port))
        }
        Some(Command::Backup { command }) => {
            let root = engine_config_from_env().data_dir;
            match command {
                BackupCommand::Now => {
                    let now_ms = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)?
                        .as_millis() as i64;
                    let receipt =
                        luna_engine::backup::BackupService::run_managed_offline_now(&root, now_ms)?;
                    println!("{}", receipt.archive.display());
                    println!("sha256 {}", receipt.archive_sha256);
                }
                BackupCommand::Status => {
                    let (destination, latest) =
                        luna_engine::backup::BackupService::managed_offline_status(&root)?;
                    println!("destination {}", destination.display());
                    if let Some(receipt) = latest {
                        println!("latest {}", receipt.archive.display());
                        println!("sha256 {}", receipt.archive_sha256);
                    } else {
                        println!("no verified backup");
                    }
                }
                BackupCommand::Preview { archive } => {
                    let preview = luna_engine::backup::BackupService::preview_managed_offline(
                        &root, &archive,
                    )?;
                    println!("archive {}", archive.display());
                    println!("source {}", preview.manifest.source_root);
                    println!("destination {}", root.display());
                    println!("entries {}", preview.manifest.entries.len());
                    println!("databases {}", preview.manifest.databases.len());
                    println!(
                        "attachments {}",
                        preview
                            .manifest
                            .entries
                            .iter()
                            .filter(|entry| entry.category == "attachment")
                            .count()
                    );
                    println!("required bytes {}", preview.required_bytes);
                    println!("conflicts {}", preview.conflicts.len());
                    for conflict in &preview.conflicts {
                        println!("conflict {conflict}");
                    }
                    println!("unsupported {}", preview.unsupported.len());
                    for reason in &preview.unsupported {
                        println!("unsupported: {reason}");
                    }
                    for category in &preview.manifest.excluded_categories {
                        println!("excluded: {category}");
                    }
                }
                BackupCommand::Restore { archive } => {
                    let receipt = luna_engine::backup::BackupService::restore_managed_offline(
                        &root, &archive,
                    )?;
                    println!("restored {} files", receipt.restored_entries);
                    println!("previous files {}", receipt.rollback_dir.display());
                }
                BackupCommand::Recover => {
                    if luna_engine::backup::BackupService::recover_managed_restore(&root)? {
                        println!("interrupted restore rolled back");
                    } else {
                        println!("no interrupted restore");
                    }
                }
            }
            Ok(())
        }
        #[cfg(target_os = "linux")]
        Some(Command::Appshot) => {
            luna_ui::appshots::request_running_appshot(&engine_config_from_env().data_dir)
                .map_err(anyhow::Error::msg)
        }
        Some(Command::Update { check }) => {
            let runtime = tokio::runtime::Runtime::new()?;
            runtime.block_on(update_cli::update(&edge_url_from_env(), check))
        }
        Some(Command::Daemon { command }) => match command {
            DaemonCommand::Install => daemon::install(&engine_config_from_env().data_dir),
            DaemonCommand::Uninstall => daemon::uninstall(),
            DaemonCommand::Start => daemon::start(),
            DaemonCommand::Stop => daemon::stop(),
            DaemonCommand::Restart => daemon::restart(),
            DaemonCommand::Status => daemon::status(),
        },
        None => {
            let edge_url = edge_url_from_env();
            let edge_token = edge_token_from_env(&edge_url);
            // Headed: the UI probes LUNA_IPC_PORT and connects to a running
            // daemon, or embeds the engine in-process (ARCHITECTURE §1).
            luna_ui::run_app(luna_ui::UiConfig {
                data_dir: paths::data_dir(),
                ipc_port: std::env::var("LUNA_IPC_PORT")
                    .ok()
                    .and_then(|p| p.parse().ok())
                    .unwrap_or(luna_engine::DEFAULT_LUNA_IPC_PORT),
                edge_url: edge_url.clone(),
                workos_client_id: workos_client_id_from_env(&edge_url),
                edge_token,
                org_id: std::env::var("LUNA_ORG_ID").ok(),
                default_harness: luna_ui::HarnessId::ClaudeCode,
                initial_url: cli.open_url,
                synthetic_core: None,
            });
            Ok(())
        }
    }
}

fn parse_cli(argv: Vec<std::ffi::OsString>, inspect_json: bool) -> Cli {
    match Cli::try_parse_from(argv) {
        Ok(cli) => cli,
        Err(error) => {
            if inspect_json
                && !matches!(
                    error.kind(),
                    clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
                )
            {
                std::process::exit(inspect_cli::print_parse_error(&error));
            }
            error.exit();
        }
    }
}

#[cfg(windows)]
fn attach_parent_console() {
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::System::Console::{
        ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE,
        STD_OUTPUT_HANDLE, SetStdHandle,
    };

    // The GUI subsystem prevents Explorer from creating a console at startup.
    // Reuse an existing parent's console for CLI output and cargo run, without
    // allocating one. Attach before Clap so help and argument errors work too.
    // Preserve redirected pipes/files: attaching may replace standard handles.
    unsafe {
        let saved = [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE]
            .map(|id| (id, GetStdHandle(id)));
        if AttachConsole(ATTACH_PARENT_PROCESS) != 0 {
            for (id, handle) in saved {
                if !handle.is_null() && handle != INVALID_HANDLE_VALUE {
                    SetStdHandle(id, handle);
                }
            }
        }
    }
}

/// The env-resolved engine configuration shared by `headless`, `login`,
/// `logout`, and `status` — one resolution so the CLI auth commands always
/// operate on the exact session the daemon will load.
fn engine_config_from_env() -> luna_engine::EngineConfig {
    let edge_url = edge_url_from_env();
    let edge_token = edge_token_from_env(&edge_url);
    luna_engine::EngineConfig {
        data_dir: paths::data_dir(),
        edge_url: edge_url.clone(),
        ipc_port: std::env::var("LUNA_IPC_PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(luna_engine::DEFAULT_LUNA_IPC_PORT),
        default_harness: harness_from_env(),
        org_id: std::env::var("LUNA_ORG_ID").ok(),
        workos_client_id: workos_client_id_from_env(&edge_url),
        edge_token,
    }
}

/// `LUNA_HARNESS` (kebab-case id) picks the default harness for chats without a
/// config row — `mock` powers the e2e smoke; default `claude-code`.
fn harness_from_env() -> luna_engine::HarnessId {
    match std::env::var("LUNA_HARNESS").as_deref().map(str::trim) {
        Ok("mock") => luna_engine::HarnessId::Mock,
        Ok("codex") => luna_engine::HarnessId::Codex,
        Ok("opencode") => luna_engine::HarnessId::Opencode,
        _ => luna_engine::HarnessId::ClaudeCode,
    }
}

/// `luna sync`: dial the running engine's IPC and print per-room sync state.
/// The introspection surface every 2026-08 incident was missing — "is this
/// device's workspace room actually receiving?" as a one-liner.
async fn sync_cli(ipc_port: u16) -> anyhow::Result<()> {
    let client = luna_rpc::connect_ws(&format!("ws://127.0.0.1:{ipc_port}"))
        .await
        .map_err(|e| {
            anyhow::anyhow!("no engine listening on 127.0.0.1:{ipc_port} ({e}) — is LUNA running?")
        })?;
    let info: luna_proto::EngineInfo = client
        .call_as(luna_rpc::methods::ENGINE_INFO, serde_json::json!({}))
        .await?;
    anyhow::ensure!(
        info.supports(luna_proto::capabilities::LUNA_OWNER_IPC_V1),
        "local listener is not a compatible LUNA engine"
    );
    paths::verify_local_owner(&client, &paths::data_dir()).await?;
    let status = client
        .call(luna_rpc::methods::SYNC_STATUS, serde_json::json!({}))
        .await
        .map_err(|e| anyhow::anyhow!("SyncStatus failed: {e}"))?;
    let now = status.get("nowMs").and_then(|v| v.as_i64()).unwrap_or(0);
    let age = |ms: i64| -> String {
        if ms <= 0 {
            return "never".into();
        }
        let s = (now - ms).max(0) / 1000;
        if s >= 3600 {
            format!("{}h{}m ago", s / 3600, (s % 3600) / 60)
        } else if s >= 60 {
            format!("{}m{}s ago", s / 60, s % 60)
        } else {
            format!("{s}s ago")
        }
    };
    let room_line = |room: Option<&serde_json::Value>| -> String {
        let Some(room) = room else {
            return "no room (dialing or edge-less)".into();
        };
        let get = |k: &str| room.get(k).and_then(|v| v.as_i64()).unwrap_or(0);
        // REJECTED is loud and only shown when nonzero: rejected writes with
        // a fresh-looking room is exactly the latched-session wedge
        // (2026-08-04) this readout previously masked.
        let rejected = get("rejected");
        format!(
            "{} pushed {} · acked {} · rejoins {} probes {} resyncs {} drops {}{}",
            if room.get("connected").and_then(|v| v.as_bool()) == Some(true) {
                "connected ·"
            } else {
                "DISCONNECTED ·"
            },
            age(get("lastPushedMs")),
            age(get("lastAckMs")),
            get("rejoins"),
            get("probes"),
            get("fullResyncs"),
            get("disconnects"),
            if rejected > 0 {
                format!(" REJECTED {rejected}")
            } else {
                String::new()
            },
        )
    };
    println!(
        "Device:    {}",
        status
            .get("deviceId")
            .and_then(|v| v.as_str())
            .unwrap_or("?")
    );
    println!(
        "Workspace: {}",
        room_line(status.get("workspace").filter(|v| !v.is_null()))
    );
    let chats = status
        .get("chats")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if chats.is_empty() {
        println!("Chats:     none open");
    }
    // Chat rooms speak chat2: cursor/head tell "am I caught up?", pending
    // tells "did my writes leave?", resets/rejected are the loud tells.
    let chat_line = |room: Option<&serde_json::Value>| -> String {
        let Some(room) = room else {
            return "no room (dialing or edge-less)".into();
        };
        let get = |k: &str| room.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
        let resets = get("serverResets");
        let rejected = get("rejected");
        format!(
            "{} cursor {}/{} · pending {} · rows {} ({}KB) · rejoins {} drops {}{}{}",
            if room.get("connected").and_then(|v| v.as_bool()) == Some(true) {
                "connected ·"
            } else {
                "DISCONNECTED ·"
            },
            get("cursor"),
            get("headSeq"),
            get("pendingPushes"),
            get("rowCount"),
            get("rowBytes") / 1024,
            get("rejoins"),
            get("disconnects"),
            if resets > 0 {
                format!(" RESETS {resets}")
            } else {
                String::new()
            },
            if rejected > 0 {
                format!(" REJECTED {rejected}")
            } else {
                String::new()
            },
        )
    };
    for chat in &chats {
        println!(
            "Chat {}: {}",
            chat.get("chatId")
                .and_then(|v| v.as_str())
                .map(|s| &s[..s.len().min(8)])
                .unwrap_or("?"),
            chat_line(chat.get("room").filter(|v| !v.is_null()))
        );
    }
    Ok(())
}

/// `{data_dir}/logs/luna-{mode}.log`, previous launch preserved as `.old`.
/// Headed and headless are separate files so an embedded-engine app and a
/// daemon on the same machine never interleave writes.
///
/// The returned file holds an exclusive `flock` for the process lifetime:
/// rotate-on-launch is only safe when nothing is still WRITING the current
/// file. On 2026-08-04 a dev build launched twice next to the running
/// installed app — the first rename put the daemon's live log at `.old`, the
/// second unlinked it entirely, and the daemon spent the rest of the incident
/// logging to an orphaned inode (an entire day of sync diagnostics gone at
/// the exact moment they were needed). A launch that finds the canonical file
/// locked logs to `luna-{mode}.{pid}.log` instead; the next lock-holding
/// launch sweeps pid-suffixed files older than a week.
fn open_log_file(mode: &str) -> Option<std::fs::File> {
    let dir = paths::data_dir().join("logs");
    open_log_file_in(&dir, mode)
}

/// Dir-parameterized body of [`open_log_file`] (unit-testable without env).
fn open_log_file_in(dir: &std::path::Path, mode: &str) -> Option<std::fs::File> {
    std::fs::create_dir_all(dir).ok()?;
    let path = dir.join(format!("luna-{mode}.log"));
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        // Probe the CURRENT inode for a live writer before touching it.
        let preexisting = path.exists();
        let existing = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .ok()?;
        let rc = unsafe { libc::flock(existing.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc != 0 {
            // A live process owns the canonical log — leave it alone.
            return std::fs::File::create(
                dir.join(format!("luna-{mode}.{}.log", std::process::id())),
            )
            .ok();
        }
        // No live writer: rotate, create fresh, and lock it as ours. (The
        // probe's flock dies with `existing`; a first-ever launch has nothing
        // to rotate — the probe itself created the empty file.)
        drop(existing);
        if preexisting {
            let _ = std::fs::rename(&path, dir.join(format!("luna-{mode}.log.old")));
        }
        let file = std::fs::File::create(&path).ok()?;
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        sweep_stale_pid_logs(dir, mode);
        Some(file)
    }
    #[cfg(not(unix))]
    {
        let _ = std::fs::rename(&path, dir.join(format!("luna-{mode}.log.old")));
        std::fs::File::create(&path).ok()
    }
}

#[cfg(all(test, unix))]
mod log_file_tests {
    use super::open_log_file_in;

    #[test]
    fn second_launch_never_rotates_a_live_processes_log() {
        let dir = tempfile::tempdir().unwrap();
        let dir = dir.path();
        // First launch owns the canonical file and keeps writing.
        let first = open_log_file_in(dir, "headed").expect("first log");
        assert!(dir.join("luna-headed.log").is_file());
        // Second launch while the first is alive: canonical file untouched,
        // pid-suffixed overflow file instead (the 2026-08-04 clobber).
        let second = open_log_file_in(dir, "headed").expect("second log");
        let pid_path = dir.join(format!("luna-headed.{}.log", std::process::id()));
        assert!(pid_path.is_file(), "expected pid-suffixed overflow log");
        assert!(
            !dir.join("luna-headed.log.old").exists(),
            "live canonical log must not be rotated away"
        );
        drop(second);
        // After the owner exits, a fresh launch rotates normally.
        drop(first);
        let third = open_log_file_in(dir, "headed").expect("third log");
        assert!(
            dir.join("luna-headed.log.old").is_file(),
            "rotation resumes"
        );
        drop(third);
    }
}

/// Delete `luna-{mode}.{pid}.log` overflow files older than a week — they
/// only exist when a second instance raced a live one for the canonical log.
#[cfg(unix)]
fn sweep_stale_pid_logs(dir: &std::path::Path, mode: &str) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let prefix = format!("luna-{mode}.");
    let week = std::time::Duration::from_secs(7 * 24 * 60 * 60);
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(middle) = name
            .strip_prefix(&prefix)
            .and_then(|rest| rest.strip_suffix(".log"))
        else {
            continue;
        };
        if !middle.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > week);
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}
