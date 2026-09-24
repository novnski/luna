//! Direct native entrypoint for the isolated L30 fixture. No shell/exec wrapper.
#[path = "../src/ssh_cli.rs"]
mod ssh_cli;

use anyhow::{Context, ensure};
use clap::Parser;
use std::{fs, path::PathBuf};

#[derive(Parser)]
struct FixtureArgs {
    #[command(flatten)]
    ssh: ssh_cli::SshArgs,
}

fn main() -> anyhow::Result<()> {
    let executable = std::env::current_exe()?;
    let root = executable
        .ancestors()
        .nth(4)
        .context("fixture executable must be inside a temporary app bundle")?
        .canonicalize()?;
    let temporary = std::env::temp_dir().canonicalize()?;
    ensure!(
        root.starts_with(&temporary) && root != temporary,
        "fixture root must be temporary"
    );
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("manifest.json"))?)?;
    let value = |name: &str| -> anyhow::Result<String> {
        Ok(manifest[name]
            .as_str()
            .with_context(|| format!("missing {name}"))?
            .into())
    };
    ensure!(
        PathBuf::from(value("root")?).canonicalize()? == root,
        "fixture root mismatch"
    );
    let fake_ssh = PathBuf::from(value("fakeSsh")?).canonicalize()?;
    let session = PathBuf::from(value("sessionFile")?).canonicalize()?;
    ensure!(
        fake_ssh.starts_with(&root) && session.starts_with(&root),
        "fixture paths escaped root"
    );
    ensure!(
        manifest["ready"] == true && !root.join("stop").exists(),
        "fixture is stopped"
    );
    // Before the SSH/GPUI runtimes or any threads exist; no live provider state.
    unsafe {
        std::env::set_var("LUNA_SSH_BIN", &fake_ssh);
        std::env::set_var("FAKE_SSH_ROOT", &root);
        std::env::set_var("LUNA_L30_QA_ROOT", &root);
        std::env::set_var("CODEX_HOME", root.join("synthetic-codex"));
        std::env::set_var("CLAUDE_CONFIG_DIR", root.join("synthetic-claude"));
        std::env::set_var("ZERON_WORKOS_CLIENT_ID", "");
        std::env::set_var("ZERON_EDGE_URL", "http://127.0.0.1:1");
        std::env::remove_var("ZERON_EDGE_TOKEN");
    }
    fs::write(root.join("direct-view.pid"), std::process::id().to_string())?;
    let mut args = vec!["remote-read-native-fixture".to_owned(), "view".to_owned()];
    for (flag, field) in [
        ("--machine", "machine"),
        ("--environment", "environment"),
        ("--workspace-scope", "workspaceScope"),
        ("--project", "project"),
        ("--task", "task"),
        ("--artifact", "artifact"),
        ("--session-file", "sessionFile"),
    ] {
        args.extend([flag.into(), value(field)?]);
    }
    args.extend(["--host".into(), "fixture-host".into()]);
    for (flag, field) in [
        ("--remote-port", "guardedPort"),
        ("--ipc-port", "localOwnerPort"),
    ] {
        let port = manifest[field].as_u64().context("missing fixture port")?;
        ensure!(port > 0 && port <= u16::MAX.into(), "invalid fixture port");
        args.extend([flag.into(), port.to_string()]);
    }
    ssh_cli::run(FixtureArgs::try_parse_from(args)?.ssh, root.join("client"))
}
