use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use luna_relay::{Target, read_private_secret};
use tokio::net::TcpListener;

#[derive(Parser)]
#[command(name = "luna-relay", version, about = "Owner-run LUNA RPC relay")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Serve routes on loopback; put owner-managed TLS in front for external use.
    Serve {
        #[arg(long)]
        config: PathBuf,
        #[arg(long, default_value = "127.0.0.1:37670")]
        bind: SocketAddr,
    },
    /// Connect an environment outward to the relay and its guarded RPC port.
    Host {
        #[arg(long)]
        server: String,
        #[arg(long)]
        route: String,
        #[arg(long)]
        target_file: PathBuf,
        #[arg(long)]
        host_secret_file: PathBuf,
        #[arg(long)]
        guarded_rpc_port: u16,
    },
    /// Forward local typed RPC clients to one exact remote environment.
    Client {
        #[arg(long)]
        server: String,
        #[arg(long)]
        route: String,
        #[arg(long)]
        target_file: PathBuf,
        #[arg(long)]
        client_id: String,
        #[arg(long)]
        client_secret_file: PathBuf,
        #[arg(long, default_value = "127.0.0.1:37671")]
        bind: SocketAddr,
    },
    /// Print one 256-bit route credential for private owner setup.
    GenerateSecret,
}

fn target(path: &PathBuf) -> Result<Target> {
    luna_relay::protocol::private_regular_file(path)?;
    let raw = std::fs::read(path).context("read exact target file")?;
    if raw.len() > 4096 {
        anyhow::bail!("target file too large");
    }
    serde_json::from_slice(&raw).context("parse L29 target")
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .init();
    match Args::parse().command {
        Command::Serve { config, bind } => {
            if !bind.ip().is_loopback() {
                anyhow::bail!("relay must bind loopback");
            }
            let listener = TcpListener::bind(bind).await?;
            luna_relay::serve(listener, config, async {
                let _ = tokio::signal::ctrl_c().await;
            })
            .await
        }
        Command::Host {
            server,
            route,
            target_file,
            host_secret_file,
            guarded_rpc_port,
        } => {
            let target = target(&target_file)?;
            let secret = read_private_secret(&host_secret_file)?;
            let (stop_tx, stop) = tokio::sync::watch::channel(false);
            tokio::select! {
                result = luna_relay::run_host(&server, &route, &target, &secret, guarded_rpc_port, stop) => result,
                _ = tokio::signal::ctrl_c() => { stop_tx.send_replace(true); Ok(()) },
            }
        }
        Command::Client {
            server,
            route,
            target_file,
            client_id,
            client_secret_file,
            bind,
        } => {
            if !bind.ip().is_loopback() {
                anyhow::bail!("client must bind loopback");
            }
            let target = target(&target_file)?;
            let secret = read_private_secret(&client_secret_file)?;
            let listener = TcpListener::bind(bind).await?;
            luna_relay::run_client_forward(
                listener,
                server,
                route,
                target,
                client_id,
                secret,
                async {
                    let _ = tokio::signal::ctrl_c().await;
                },
            )
            .await
        }
        Command::GenerateSecret => {
            println!("{}", luna_relay::protocol::random_hex()?);
            Ok(())
        }
    }
}
