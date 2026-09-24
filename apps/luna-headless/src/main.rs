//! UI-free LUNA environment engine for direct SSH access.

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};
use zeron_engine::{Engine, EngineConfig, HarnessId, WorkspaceScope};
use zeron_rpc::methods;
use zeron_rpc::remote_auth::{CreatePairing, RemoteScope};

#[derive(Parser)]
#[command(
    name = "luna-headless",
    version,
    about = "LUNA headless environment engine"
)]
struct Args {
    #[command(subcommand)]
    command: Option<Command>,
    /// Authenticated remote RPC port on remote loopback.
    #[arg(long)]
    remote_port: Option<u16>,
    /// Separate local-owner IPC port. Never forward this over SSH.
    #[arg(long)]
    ipc_port: Option<u16>,
}

#[derive(Subcommand)]
enum Command {
    /// Issue one L29 pairing through the remote machine's local-owner IPC.
    Pair {
        #[arg(long)]
        ipc_port: u16,
        #[arg(long)]
        label: String,
        #[arg(long = "scope", required = true, value_enum)]
        scopes: Vec<Scope>,
        #[arg(long)]
        expect_machine: Option<String>,
        #[arg(long)]
        expect_environment: Option<String>,
        #[arg(long, value_enum)]
        expect_workspace_scope: Option<WorkspaceScopeArg>,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum WorkspaceScopeArg {
    Local,
    Synced,
    Development,
}

impl From<WorkspaceScopeArg> for WorkspaceScope {
    fn from(value: WorkspaceScopeArg) -> Self {
        match value {
            WorkspaceScopeArg::Local => Self::Local,
            WorkspaceScopeArg::Synced => Self::Synced,
            WorkspaceScopeArg::Development => Self::Development,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum Scope {
    EnvironmentRead,
    TasksRead,
    FilesRead,
    TerminalOperate,
}

impl From<Scope> for RemoteScope {
    fn from(scope: Scope) -> Self {
        match scope {
            Scope::EnvironmentRead => Self::EnvironmentRead,
            Scope::TasksRead => Self::TasksRead,
            Scope::FilesRead => Self::FilesRead,
            Scope::TerminalOperate => Self::TerminalOperate,
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    if let Some(Command::Pair {
        ipc_port,
        label,
        scopes,
        expect_machine,
        expect_environment,
        expect_workspace_scope,
    }) = args.command
    {
        let expected = match (expect_machine, expect_environment, expect_workspace_scope) {
            (None, None, None) => None,
            (Some(machine), Some(environment), Some(scope)) => {
                Some((machine, environment, WorkspaceScope::from(scope)))
            }
            _ => anyhow::bail!(
                "expected machine, environment, and workspace scope must be given together"
            ),
        };
        let client = zeron_rpc::connect_ws(&format!("ws://127.0.0.1:{ipc_port}")).await?;
        let _: serde_json::Value = client
            .call_as(methods::ENGINE_READY, serde_json::json!({}))
            .await?;
        let issued: zeron_rpc::remote_auth::IssuedPairing = client
            .call_as(
                methods::CREATE_REMOTE_PAIRING,
                serde_json::to_value(CreatePairing {
                    label,
                    scopes: scopes.into_iter().map(Into::into).collect(),
                    pairing_ttl_secs: 300,
                    session_ttl_secs: 86_400,
                })?,
            )
            .await?;
        if let Some((machine, environment, scope)) = expected
            && (issued.target.device_id != machine
                || issued.target.environment_id != environment
                || issued.target.workspace_scope != scope)
        {
            let result: serde_json::Value = client
                .call_as(
                    methods::REVOKE_REMOTE_PAIRING,
                    serde_json::json!({"id": issued.pairing.id}),
                )
                .await
                .map_err(|error| {
                    anyhow::anyhow!("pairing target mismatch; revocation uncertain: {error}")
                })?;
            if result["revoked"] != true {
                anyhow::bail!("pairing target mismatch; revocation unconfirmed");
            }
            anyhow::bail!("pairing target mismatch; issued grant revoked");
        }
        println!("{}", serde_json::to_string(&issued)?);
        return Ok(());
    }
    let remote_port = args
        .remote_port
        .ok_or_else(|| anyhow::anyhow!("--remote-port is required"))?;
    let ipc_port = args
        .ipc_port
        .ok_or_else(|| anyhow::anyhow!("--ipc-port is required"))?;
    if remote_port == 0 || ipc_port == 0 || remote_port == ipc_port {
        anyhow::bail!("remote port must differ from local IPC port");
    }
    let data_dir = std::env::var_os("LUNA_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").expect("HOME must be set")).join(".luna")
        });
    let edge_token = std::env::var("ZERON_EDGE_TOKEN").ok();
    let workos_client_id = match std::env::var("ZERON_WORKOS_CLIENT_ID") {
        Ok(value) if value.trim().is_empty() => None,
        Ok(value) => Some(value),
        Err(_) if edge_token.is_some() => None,
        Err(_) => Some("client_01KWD0EAKZKD50YCQJNYSRE4BY".into()),
    };
    let config = EngineConfig {
        data_dir,
        edge_url: std::env::var("ZERON_EDGE_URL")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "https://edge.zeron.sh".into()),
        edge_token,
        ipc_port,
        default_harness: match std::env::var("ZERON_HARNESS").as_deref() {
            Ok("mock") => HarnessId::Mock,
            Ok("codex") => HarnessId::Codex,
            Ok("cursor") => HarnessId::Cursor,
            Ok("devin") => HarnessId::Devin,
            Ok("grok") => HarnessId::Grok,
            Ok("hermes") => HarnessId::Hermes,
            Ok("pi") => HarnessId::Pi,
            Ok("antigravity") => HarnessId::Antigravity,
            _ => HarnessId::ClaudeCode,
        },
        org_id: std::env::var("ZERON_ORG_ID").ok(),
        workos_client_id,
    };
    Engine::new(config)
        .run_with_remote_port(Some(remote_port))
        .await
}
