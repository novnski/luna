//! RM-00 probe host: serve the harmless pinned descriptor on one interface.
//!
//!   cargo run -p luna-remote --example probe -- --identity <dir> --bind <ip:port>
//!       [--advertise https://name:port]...
//!
//! Prints one JSON line per event to stdout. The first is `ready` (with the
//! pin, target and a host ticket for the advertised endpoints) or `refused`
//! (with the reason, exit status 2). A new identity directory gets a fresh
//! local target; an existing one is reused exactly or refused.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use luna_proto::WorkspaceScope;
use luna_proto::remote_control::{HOST_TICKET_VERSION, HostTicket, RemoteTarget};
use luna_remote::host;
use luna_remote::identity::HostIdentity;

struct Args {
    identity: PathBuf,
    bind: SocketAddr,
    advertise: Vec<String>,
}

fn parse() -> Result<Args, String> {
    let mut identity = None;
    let mut bind = None;
    let mut advertise = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or(format!("{flag} needs a value"));
        match flag.as_str() {
            "--identity" => identity = Some(PathBuf::from(value()?)),
            "--bind" => bind = Some(value()?.parse().map_err(|e| format!("--bind: {e}"))?),
            "--advertise" => advertise.push(value()?),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(Args {
        identity: identity.ok_or("--identity is required")?,
        bind: bind.ok_or("--bind is required")?,
        advertise,
    })
}

fn emit(mut value: serde_json::Value) {
    let at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64);
    value["atMs"] = at_ms.into();
    println!("{value}");
}

fn refuse(error: impl std::fmt::Display) -> ! {
    emit(serde_json::json!({ "event": "refused", "error": error.to_string() }));
    std::process::exit(2);
}

fn new_target() -> RemoteTarget {
    RemoteTarget {
        device_id: uuid::Uuid::new_v4().to_string(),
        workspace_scope: WorkspaceScope::Local,
        environment_id: uuid::Uuid::new_v4().to_string(),
        protocol_version: luna_rpc::remote_auth::PROTOCOL_VERSION,
        required_capabilities: vec![luna_rpc::remote_auth::CAPABILITY.to_owned()],
    }
}

#[tokio::main]
async fn main() {
    let args = parse().unwrap_or_else(|error| refuse(error));
    let identity =
        HostIdentity::open_or_create(&args.identity, new_target).unwrap_or_else(|e| refuse(e));
    let config = identity.server_config().unwrap_or_else(|e| refuse(e));
    let listener = host::bind(args.bind).await.unwrap_or_else(|e| refuse(e));
    let bound = listener.local_addr().unwrap_or_else(|e| refuse(e));
    let mut endpoints = vec![format!("https://{bound}")];
    endpoints.extend(args.advertise);
    let descriptor = luna_remote::descriptor(&identity);
    let ticket = HostTicket {
        version: HOST_TICKET_VERSION,
        target: identity.target().clone(),
        endpoints,
        spki_sha256: identity.spki_sha256().to_owned(),
    };
    emit(serde_json::json!({
        "event": "ready",
        "bound": bound,
        "pin": identity.spki_sha256(),
        "target": identity.target(),
        "ticket": ticket,
    }));
    let events: Arc<dyn Fn(host::HostEvent) + Send + Sync> = Arc::new(|event| {
        emit(serde_json::to_value(event).expect("event serializes"));
    });
    tokio::select! {
        result = host::serve(listener, config, &descriptor, events) => {
            if let Err(error) = result {
                refuse(format!("listener failed: {error}"));
            }
        }
        _ = tokio::signal::ctrl_c() => emit(serde_json::json!({ "event": "stopped" })),
    }
}
