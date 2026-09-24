//! Local IPC administration of L29 device grants. The pairing credential is
//! printed once; it is never stored in the authority file or command history.

use std::time::Duration;

use clap::{Args, Subcommand, ValueEnum};
use serde::de::DeserializeOwned;
use serde_json::json;
use zeron_proto::EngineInfo;
use zeron_rpc::remote_auth::{CreatePairing, IssuedPairing, PairingView, RemoteScope, SessionView};
use zeron_rpc::{RpcClient, methods};

#[derive(Debug, Args)]
pub struct RemoteArgs {
    /// Local engine IPC port (defaults to LUNA_IPC_PORT or 59778).
    #[arg(long, global = true)]
    port: Option<u16>,
    #[command(subcommand)]
    command: RemoteCommand,
}

#[derive(Debug, Subcommand)]
enum RemoteCommand {
    /// Issue a one-time pairing credential for an explicitly scoped device.
    Pair {
        #[arg(long)]
        label: String,
        #[arg(long = "scope", required = true, value_enum)]
        scopes: Vec<ScopeArg>,
        #[arg(long, default_value_t = 300)]
        pairing_ttl_secs: u64,
        #[arg(long, default_value_t = 86_400)]
        session_ttl_secs: u64,
    },
    /// Show unexpired pairing grants without their credentials.
    Pairings,
    /// Invalidate a pairing grant before redemption.
    RevokePairing { id: String },
    /// Show issued device sessions without their tokens.
    Sessions,
    /// Revoke a device session, including current streams and reconnects.
    RevokeSession { id: String },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ScopeArg {
    EnvironmentRead,
    TasksRead,
    FilesRead,
    TerminalOperate,
}

impl From<ScopeArg> for RemoteScope {
    fn from(value: ScopeArg) -> Self {
        match value {
            ScopeArg::EnvironmentRead => Self::EnvironmentRead,
            ScopeArg::TasksRead => Self::TasksRead,
            ScopeArg::FilesRead => Self::FilesRead,
            ScopeArg::TerminalOperate => Self::TerminalOperate,
        }
    }
}

pub async fn run(args: RemoteArgs, default_port: u16) -> anyhow::Result<()> {
    let port = args.port.unwrap_or(default_port);
    let url = format!("ws://127.0.0.1:{port}");
    let client = tokio::time::timeout(Duration::from_secs(5), zeron_rpc::connect_ws(&url))
        .await
        .map_err(|_| anyhow::anyhow!("local LUNA engine timed out at {url}"))??;
    let _: serde_json::Value = call(&client, methods::ENGINE_READY, json!({})).await?;
    let info: EngineInfo = call(&client, methods::ENGINE_INFO, json!({})).await?;
    if !info.supports(zeron_proto::capabilities::LUNA_OWNER_IPC_V1) {
        anyhow::bail!("local listener is not a compatible LUNA engine");
    }
    if !info.supports(zeron_proto::capabilities::REMOTE_AUTH_V1) {
        anyhow::bail!("connected engine does not support L29 remote authority");
    }
    match args.command {
        RemoteCommand::Pair {
            label,
            scopes,
            pairing_ttl_secs,
            session_ttl_secs,
        } => {
            let issued: IssuedPairing = call(
                &client,
                methods::CREATE_REMOTE_PAIRING,
                serde_json::to_value(CreatePairing {
                    label,
                    scopes: scopes.into_iter().map(Into::into).collect(),
                    pairing_ttl_secs,
                    session_ttl_secs,
                })?,
            )
            .await?;
            if issued.target.device_id != info.device_id
                || issued.target.workspace_scope != info.workspace_scope
            {
                anyhow::bail!("engine identity changed while issuing pairing; discard the result");
            }
            println!(
                "Pairing {} for machine {} ({:?})",
                issued.pairing.id, info.device_id, info.workspace_scope
            );
            println!("Environment: {}", issued.target.environment_id);
            println!(
                "Expires at {} ms; scopes: {:?}",
                issued.pairing.expires_at_ms, issued.pairing.scopes
            );
            println!("Credential (shown once): {}", issued.credential);
            println!(
                "Protocol: {}; capability: {}",
                issued.target.protocol_version,
                issued.target.required_capabilities.join(",")
            );
        }
        RemoteCommand::Pairings => {
            let pairings: Vec<PairingView> =
                call(&client, methods::LIST_REMOTE_PAIRINGS, json!({})).await?;
            for pairing in pairings {
                println!(
                    "{}  {}  expires={}  {:?}",
                    pairing.id, pairing.label, pairing.expires_at_ms, pairing.scopes
                );
            }
        }
        RemoteCommand::RevokePairing { id } => {
            let value: serde_json::Value =
                call(&client, methods::REVOKE_REMOTE_PAIRING, json!({ "id": id })).await?;
            println!("pairing revoked: {}", value["revoked"] == true);
        }
        RemoteCommand::Sessions => {
            let sessions: Vec<SessionView> =
                call(&client, methods::LIST_REMOTE_SESSIONS, json!({})).await?;
            for session in sessions {
                println!(
                    "{}  {}  expires={}  revoked={}  {:?}",
                    session.id,
                    session.client_label,
                    session.expires_at_ms,
                    session.revoked,
                    session.scopes
                );
            }
        }
        RemoteCommand::RevokeSession { id } => {
            let value: serde_json::Value =
                call(&client, methods::REVOKE_REMOTE_SESSION, json!({ "id": id })).await?;
            println!("session revoked: {}", value["revoked"] == true);
        }
    }
    Ok(())
}

async fn call<T: DeserializeOwned>(
    client: &RpcClient,
    method: &str,
    params: serde_json::Value,
) -> anyhow::Result<T> {
    let value = tokio::time::timeout(Duration::from_secs(5), client.call(method, params))
        .await
        .map_err(|_| anyhow::anyhow!("{method} timed out"))??;
    Ok(serde_json::from_value(value)?)
}
