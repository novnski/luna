//! Actual LUNA CLI over fake system SSH and a copy-isolated guarded RPC engine.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, copy_bidirectional};
use tokio::net::{TcpListener, TcpStream};
use zeron_engine::{
    AgentAccountsConfig, Auth, AuthConfig, EngineCore, EngineProfile, HarnessId, HarnessRegistry,
};
use zeron_rpc::remote_auth::{CreatePairing, IssuedPairing, RemoteScope, SessionView};

const FAKE_SSH: &str = r#"#!/bin/sh
set -eu
root=$FAKE_SSH_ROOT
for arg do
  if [ "$arg" = 'StrictHostKeyChecking=yes' ]; then printf 'strict\n' >> "$root/options"; fi
  if [ "$arg" = 'ForwardAgent=no' ]; then printf 'no-agent\n' >> "$root/options"; fi
done
if [ "$1" = '-N' ]; then
  forward=
  while [ "$#" -gt 0 ]; do
    if [ "$1" = '-L' ]; then shift; forward=$1; fi
    shift
  done
  local_port=${forward#127.0.0.1:}
  local_port=${local_port%%:*}
  printf '%s\n' "$local_port" > "$root/forward-port"
  if [ -e "$root/fail-forward" ]; then exit 255; fi
  count=0
  while [ ! -e "$root/drop-forward" ] && [ "$count" -lt 300 ]; do
    count=$((count + 1)); sleep 0.1
  done
  exit 0
fi
while [ "$#" -gt 0 ] && [ "$1" != 'sh' ]; do shift; done
if [ "$#" -eq 0 ]; then
  cat > "$root/uploaded"
  exit 0
fi
shift 3
mode=$1 version=$2 sha=$3 port=$4 ipc=$5 key=$6 owner=$7
cat >/dev/null
printf '%s\n' "$mode" >> "$root/modes"
if [ "$mode" = 'platform' ] && [ -e "$root/hang-platform" ]; then sleep 30; fi
case "$mode" in
  platform) cat "$root/platform" ;;
  inspect)
    if [ -e "$root/installed" ]; then printf 'installed\n'; else printf 'missing\n'; fi ;;
  install)
    [ -s "$root/uploaded" ] || exit 1
    if [ -e "$root/remote-version-check" ]; then
      chmod 700 "$root/uploaded"
      remote_version=$(FAKE_REMOTE_EXECUTION=1 "$root/uploaded" --version)
      [ "$remote_version" = "luna-headless $version" ] || {
        printf 'staged artifact version mismatch\n' >&2; exit 1;
      }
    fi
    printf '%s\n' "$sha" > "$root/installed" ;;
  start)
    if [ -e "$root/remote-owner" ]; then
      saved=$(cat "$root/remote-owner")
      if [ "$saved" = "$owner" ]; then kind=owned; else kind=reused; fi
    else
      printf '%s\n' "$owner" > "$root/remote-owner"
      kind=owned
    fi
    if [ -e "$root/deny-state-finalize" ]; then chmod 500 "$LUNA_DATA_DIR/ssh-launch"; fi
    if [ -e "$root/fail-start-reply" ]; then exit 255; fi
    printf 'port=%s\nkind=%s\n' "$port" "$kind" ;;
  status)
    [ -e "$root/remote-owner" ] || exit 1
    saved=$(cat "$root/remote-owner")
    if [ "$saved" = "$owner" ]; then printf 'running-owned\n'; else printf 'running-reused\n'; fi ;;
  pair) cat "$root/pairing.json" ;;
  stop)
    if [ -e "$root/fail-stop" ]; then exit 255; fi
    [ "$(cat "$root/remote-owner")" = "$owner" ] || exit 1
    rm "$root/remote-owner"
    printf 'stopped\n' ;;
  *) exit 2 ;;
esac
"#;

fn accounts(root: &Path) -> AgentAccountsConfig {
    AgentAccountsConfig {
        data_dir: root.join("provider-state"),
        claude_config_dir: root.join("synthetic-claude"),
        claude_config_file: root.join("synthetic-claude.json"),
        codex_home: root.join("synthetic-codex"),
        cursor_sdk_auth_file: root.join("synthetic-cursor-auth.json"),
    }
}

fn fixture(root: &Path) -> (PathBuf, PathBuf) {
    let ssh = root.join("fake-ssh");
    fs::write(&ssh, FAKE_SSH).unwrap();
    fs::set_permissions(&ssh, fs::Permissions::from_mode(0o700)).unwrap();
    let (platform, os, arch) = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => ("Darwin arm64", "darwin", "arm64"),
        ("macos", "x86_64") => ("Darwin x86_64", "darwin", "x64"),
        ("linux", "aarch64") => ("Linux aarch64", "linux", "arm64"),
        ("linux", "x86_64") => ("Linux x86_64", "linux", "x64"),
        other => panic!("unsupported test platform: {other:?}"),
    };
    fs::write(root.join("platform"), format!("{platform}\n")).unwrap();
    let name = format!("luna-headless-v{}-{os}-{arch}", env!("CARGO_PKG_VERSION"));
    let artifact = root.join(&name);
    fs::write(&artifact, format!("#!/bin/sh\nif [ \"$1\" = '--version' ]; then echo 'luna-headless {}'; else exit 2; fi\n", env!("CARGO_PKG_VERSION"))).unwrap();
    fs::set_permissions(&artifact, fs::Permissions::from_mode(0o700)).unwrap();
    let sha = format!("{:x}", Sha256::digest(fs::read(&artifact).unwrap()));
    fs::write(
        root.join(format!("{name}.sha256")),
        format!("{sha}  {name}\n"),
    )
    .unwrap();
    (ssh, artifact)
}

async fn invoke(root: &Path, ssh: &Path, args: &[&str], server_port: u16) -> std::process::Output {
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_luna"));
    command
        .arg("ssh")
        .args(args)
        .env("LUNA_DATA_DIR", root.join("client-profile"))
        .env("LUNA_SSH_BIN", ssh)
        .env("FAKE_SSH_ROOT", root)
        .env("CODEX_HOME", root.join("synthetic-codex"))
        .env("CLAUDE_CONFIG_DIR", root.join("synthetic-claude"))
        .env("ZERON_WORKOS_CLIENT_ID", "")
        .env("ZERON_EDGE_URL", "http://127.0.0.1:1");
    let _ = server_port;
    command.output().await.unwrap()
}

async fn relay_from_forward_file(root: PathBuf, remote_port: u16) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let file = root.join("forward-port");
        let local_port = loop {
            if let Ok(value) = fs::read_to_string(&file)
                && let Ok(port) = value.trim().parse::<u16>()
            {
                break port;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        let listener = TcpListener::bind(("127.0.0.1", local_port)).await.unwrap();
        while let Ok((mut local, _)) = listener.accept().await {
            tokio::spawn(async move {
                if let Ok(mut remote) = TcpStream::connect(("127.0.0.1", remote_port)).await {
                    let _ = copy_bidirectional(&mut local, &mut remote).await;
                }
            });
        }
    })
}

async fn relay_all_forward_files(root: PathBuf, remote_port: u16) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ports = std::collections::HashSet::new();
        let mut listeners = tokio::task::JoinSet::new();
        loop {
            if let Ok(value) = fs::read_to_string(root.join("forward-port"))
                && let Ok(port) = value.trim().parse::<u16>()
                && ports.insert(port)
            {
                listeners.spawn(async move {
                    let listener = TcpListener::bind(("127.0.0.1", port)).await.unwrap();
                    while let Ok((mut local, _)) = listener.accept().await {
                        tokio::spawn(async move {
                            if let Ok(mut remote) =
                                TcpStream::connect(("127.0.0.1", remote_port)).await
                            {
                                let _ = copy_bidirectional(&mut local, &mut remote).await;
                            }
                        });
                    }
                });
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
}

async fn next_view_state(
    lines: &mut tokio::io::Lines<tokio::io::BufReader<tokio::process::ChildStdout>>,
    expected: &str,
) -> serde_json::Value {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let line = lines
                .next_line()
                .await
                .unwrap()
                .expect("view process ended");
            let value: serde_json::Value = serde_json::from_str(&line).unwrap();
            if value["state"] == expected {
                return value;
            }
        }
    })
    .await
    .expect("remote view state timed out")
}

fn probe_args<'a>(
    machine: &'a str,
    environment: &'a str,
    artifact: &'a Path,
    session: &'a Path,
) -> Vec<String> {
    vec![
        "probe".into(),
        "--host".into(),
        "fake-host".into(),
        "--machine".into(),
        machine.into(),
        "--environment".into(),
        environment.into(),
        "--workspace-scope".into(),
        "development".into(),
        "--artifact".into(),
        artifact.display().to_string(),
        "--session-file".into(),
        session.display().to_string(),
        "--remote-port".into(),
        "37655".into(),
        "--ipc-port".into(),
        "37654".into(),
    ]
}

#[tokio::test]
async fn ssh_cli_enrolls_reconnects_rejects_wrong_target_and_stops_only_owned() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();
    let (ssh, artifact) = fixture(root);
    let accounts = accounts(root);
    accounts.disable_claude_keychain_fallback().unwrap();
    let core = EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        EngineProfile::development(&root.join("host-profile"), "l30-org", "l30-user"),
        Arc::new(HarnessRegistry::new()),
        HarnessId::Mock,
        None,
        accounts,
    )
    .unwrap();
    let mut config = AuthConfig::new("http://127.0.0.1:1", root.join("host-profile"));
    config.use_in_memory_credentials_for_tests();
    core.set_auth(Auth::new(config));
    let local = zeron_rpc::memory_client(core.rpc_service());
    let project = "30303030-3030-4030-8030-303030303030";
    let task = "31313131-3131-4131-8131-313131313131";
    let project_path = root.join("project");
    fs::create_dir_all(&project_path).unwrap();
    local
        .call(
            zeron_rpc::methods::MUTATE,
            serde_json::json!({
                "op": "createSpace", "spaceId": project, "deviceId": core.device_id,
                "path": project_path,
            }),
        )
        .await
        .unwrap();
    local
        .call(
            zeron_rpc::methods::MUTATE,
            serde_json::json!({
                "op": "createChat", "chatId": task, "spaceId": project,
                "config": { "harness": "mock", "model": null, "reasoning": null,
                    "sandbox": "workspace-write" },
            }),
        )
        .await
        .unwrap();
    let issued: IssuedPairing = local
        .call_as(
            zeron_rpc::methods::CREATE_REMOTE_PAIRING,
            serde_json::to_value(CreatePairing {
                label: "synthetic SSH".into(),
                scopes: vec![RemoteScope::EnvironmentRead, RemoteScope::TasksRead],
                pairing_ttl_secs: 300,
                session_ttl_secs: 86_400,
            })
            .unwrap(),
        )
        .await
        .unwrap();
    fs::write(
        root.join("pairing.json"),
        serde_json::to_vec(&issued).unwrap(),
    )
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let remote_port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(zeron_rpc::serve_ws_listener_with_factory(
        listener,
        core.remote_rpc_factory().unwrap(),
    ));

    let session = root.join("client-profile/session.token");
    let mut args = probe_args(
        &core.device_id,
        &issued.target.environment_id,
        &artifact,
        &session,
    );
    let pairing_file = root.join("pairing.secret");
    fs::write(&pairing_file, format!("{}\n", issued.credential)).unwrap();
    fs::set_permissions(&pairing_file, fs::Permissions::from_mode(0o600)).unwrap();
    args.extend(["--pairing-file".into(), pairing_file.display().to_string()]);
    let relay = relay_from_forward_file(root.to_path_buf(), remote_port).await;
    let refs: Vec<_> = args.iter().map(String::as_str).collect();
    let first = invoke(root, &ssh, &refs, remote_port).await;
    relay.abort();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(String::from_utf8_lossy(&first.stdout).contains("process=owned"));
    assert!(!String::from_utf8_lossy(&first.stdout).contains(&issued.credential));
    assert_eq!(
        fs::metadata(&session).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(
        fs::read_to_string(root.join("options"))
            .unwrap()
            .contains("strict")
    );
    assert!(
        fs::read_to_string(root.join("options"))
            .unwrap()
            .contains("no-agent")
    );
    assert!(
        fs::read_to_string(root.join("modes"))
            .unwrap()
            .contains("install")
    );
    let unauthenticated = zeron_rpc::connect_ws(&format!("ws://127.0.0.1:{remote_port}"))
        .await
        .unwrap();
    assert!(
        unauthenticated
            .call(zeron_rpc::methods::ENGINE_INFO, serde_json::json!({}))
            .await
            .is_err(),
        "another socket must not inherit the CLI's principal"
    );

    fs::remove_file(root.join("forward-port")).unwrap();
    args = probe_args(
        &core.device_id,
        &issued.target.environment_id,
        &artifact,
        &session,
    );
    let relay = relay_from_forward_file(root.to_path_buf(), remote_port).await;
    let refs: Vec<_> = args.iter().map(String::as_str).collect();
    let again = invoke(root, &ssh, &refs, remote_port).await;
    relay.abort();
    assert!(
        again.status.success(),
        "{}",
        String::from_utf8_lossy(&again.stderr)
    );
    assert!(String::from_utf8_lossy(&again.stdout).contains("process=owned"));

    let modes_before = fs::read_to_string(root.join("modes")).unwrap();
    let mut conflict = args.clone();
    let scope = conflict
        .iter_mut()
        .find(|value| *value == "development")
        .unwrap();
    *scope = "local".into();
    fs::write(root.join("fail-forward"), "1").unwrap();
    let rejected_scope = invoke(
        root,
        &ssh,
        &conflict.iter().map(String::as_str).collect::<Vec<_>>(),
        remote_port,
    )
    .await;
    fs::remove_file(root.join("fail-forward")).unwrap();
    assert!(!rejected_scope.status.success());
    assert_eq!(
        fs::read_to_string(root.join("modes")).unwrap(),
        modes_before,
        "scope conflict must fail before SSH remote side effects"
    );

    fs::remove_file(root.join("forward-port")).unwrap();
    let mut exact_task = args.clone();
    exact_task.extend([
        "--project".into(),
        project.into(),
        "--task".into(),
        task.into(),
    ]);
    let relay = relay_from_forward_file(root.to_path_buf(), remote_port).await;
    let refs: Vec<_> = exact_task.iter().map(String::as_str).collect();
    let exact = invoke(root, &ssh, &refs, remote_port).await;
    relay.abort();
    assert!(
        exact.status.success(),
        "{}",
        String::from_utf8_lossy(&exact.stderr)
    );
    assert!(String::from_utf8_lossy(&exact.stdout).contains(&format!("task={task}")));

    fs::remove_file(root.join("forward-port")).unwrap();
    let relay = relay_all_forward_files(root.to_path_buf(), remote_port).await;
    let view_pairing: IssuedPairing = local
        .call_as(
            zeron_rpc::methods::CREATE_REMOTE_PAIRING,
            serde_json::to_value(CreatePairing {
                label: "synthetic desktop view".into(),
                scopes: vec![RemoteScope::EnvironmentRead, RemoteScope::TasksRead],
                pairing_ttl_secs: 300,
                session_ttl_secs: 86_400,
            })
            .unwrap(),
        )
        .await
        .unwrap();
    let view_pairing_file = root.join("view-pairing.secret");
    fs::write(&view_pairing_file, view_pairing.credential).unwrap();
    fs::set_permissions(&view_pairing_file, fs::Permissions::from_mode(0o600)).unwrap();
    let view_session = root.join("client-profile/view-session.token");
    let mut view_args = args.clone();
    view_args[0] = "view".into();
    let session_index = view_args
        .iter()
        .position(|value| value == "--session-file")
        .unwrap()
        + 1;
    view_args[session_index] = view_session.display().to_string();
    view_args.extend([
        "--project".into(),
        project.into(),
        "--task".into(),
        task.into(),
        "--pairing-file".into(),
        view_pairing_file.display().to_string(),
        "--fixture-stdio".into(),
    ]);
    let mut view = tokio::process::Command::new(env!("CARGO_BIN_EXE_luna"))
        .arg("ssh")
        .args(&view_args)
        .env("LUNA_DATA_DIR", root.join("client-profile"))
        .env("LUNA_SSH_BIN", &ssh)
        .env("FAKE_SSH_ROOT", root)
        .env("CODEX_HOME", root.join("synthetic-codex"))
        .env("CLAUDE_CONFIG_DIR", root.join("synthetic-claude"))
        .env("ZERON_WORKOS_CLIENT_ID", "")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut view_input = view.stdin.take().unwrap();
    let mut view_output = tokio::io::BufReader::new(view.stdout.take().unwrap()).lines();
    let connected = next_view_state(&mut view_output, "connected").await;
    assert_eq!(connected["data"]["task"], task);
    view_input.write_all(b"disconnect\n").await.unwrap();
    let disconnected = next_view_state(&mut view_output, "disconnected").await;
    assert!(disconnected["data"].as_str().unwrap().contains("by user"));
    view_input.write_all(b"reconnect\n").await.unwrap();
    next_view_state(&mut view_output, "connected").await;
    fs::write(root.join("drop-forward"), "1").unwrap();
    let lost = next_view_state(&mut view_output, "disconnected").await;
    assert!(
        lost["data"]
            .as_str()
            .unwrap()
            .contains("SSH transport closed")
    );
    fs::remove_file(root.join("drop-forward")).unwrap();
    view_input.write_all(b"reconnect\n").await.unwrap();
    next_view_state(&mut view_output, "connected").await;
    view_input.write_all(b"exit\n").await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(8), view.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    relay.abort();

    fs::remove_file(root.join("forward-port")).unwrap();
    let mut wrong_task = args.clone();
    wrong_task.extend([
        "--project".into(),
        "other-project".into(),
        "--task".into(),
        "missing-task".into(),
    ]);
    let relay = relay_from_forward_file(root.to_path_buf(), remote_port).await;
    let refs: Vec<_> = wrong_task.iter().map(String::as_str).collect();
    let task_rejected = invoke(root, &ssh, &refs, remote_port).await;
    relay.abort();
    assert!(!task_rejected.status.success());

    fs::remove_file(root.join("forward-port")).unwrap();
    let mut wrong = probe_args(
        "11111111-1111-4111-8111-111111111111",
        &issued.target.environment_id,
        &artifact,
        &session,
    );
    let relay = relay_from_forward_file(root.to_path_buf(), remote_port).await;
    let refs: Vec<_> = wrong.iter().map(String::as_str).collect();
    let rejected = invoke(root, &ssh, &refs, remote_port).await;
    relay.abort();
    assert!(!rejected.status.success());
    assert!(root.join("remote-owner").exists());

    let sessions: Vec<SessionView> = local
        .call_as(
            zeron_rpc::methods::LIST_REMOTE_SESSIONS,
            serde_json::json!({}),
        )
        .await
        .unwrap();
    for session in sessions {
        let _: serde_json::Value = local
            .call_as(
                zeron_rpc::methods::REVOKE_REMOTE_SESSION,
                serde_json::json!({"id": session.id}),
            )
            .await
            .unwrap();
    }
    fs::remove_file(root.join("forward-port")).unwrap();
    let relay = relay_from_forward_file(root.to_path_buf(), remote_port).await;
    let refs: Vec<_> = args.iter().map(String::as_str).collect();
    let revoked = invoke(root, &ssh, &refs, remote_port).await;
    relay.abort();
    assert!(!revoked.status.success());
    assert!(root.join("remote-owner").exists());

    let stop = [
        "stop",
        "--host",
        "fake-host",
        "--machine",
        &core.device_id,
        "--environment",
        &issued.target.environment_id,
        "--workspace-scope",
        "development",
        "--remote-port",
        "37655",
        "--ipc-port",
        "37654",
    ];
    let stopped = invoke(root, &ssh, &stop, remote_port).await;
    assert!(
        stopped.status.success(),
        "{}",
        String::from_utf8_lossy(&stopped.stderr)
    );
    assert!(!root.join("remote-owner").exists());

    // A fresh explicit --enroll can also issue a target-bound pairing over SSH.
    fs::remove_file(&session).unwrap();
    let next_pairing: IssuedPairing = local
        .call_as(
            zeron_rpc::methods::CREATE_REMOTE_PAIRING,
            serde_json::to_value(CreatePairing {
                label: "synthetic automatic SSH".into(),
                scopes: vec![RemoteScope::EnvironmentRead, RemoteScope::TasksRead],
                pairing_ttl_secs: 300,
                session_ttl_secs: 86_400,
            })
            .unwrap(),
        )
        .await
        .unwrap();
    fs::write(
        root.join("pairing.json"),
        serde_json::to_vec(&next_pairing).unwrap(),
    )
    .unwrap();
    fs::remove_file(root.join("forward-port")).unwrap();
    let mut automatic = args.clone();
    automatic.push("--enroll".into());
    let relay = relay_from_forward_file(root.to_path_buf(), remote_port).await;
    let refs: Vec<_> = automatic.iter().map(String::as_str).collect();
    let enrolled = invoke(root, &ssh, &refs, remote_port).await;
    relay.abort();
    assert!(
        enrolled.status.success(),
        "{}",
        String::from_utf8_lossy(&enrolled.stderr)
    );
    assert!(session.exists());
    let stopped = invoke(root, &ssh, &stop, remote_port).await;
    assert!(stopped.status.success());
    assert!(!root.join("remote-owner").exists());

    fs::write(root.join("fail-forward"), "1").unwrap();
    fs::remove_file(root.join("forward-port")).unwrap();
    let refs: Vec<_> = args.iter().map(String::as_str).collect();
    let lost = invoke(root, &ssh, &refs, remote_port).await;
    assert!(!lost.status.success());
    assert!(String::from_utf8_lossy(&lost.stderr).contains("forward exited"));
    assert!(
        !root.join("remote-owner").exists(),
        "new failed launch should be stopped"
    );
    fs::remove_file(root.join("fail-forward")).unwrap();
    fs::write(root.join("fail-start-reply"), "1").unwrap();
    let uncertain = invoke(
        root,
        &ssh,
        &args.iter().map(String::as_str).collect::<Vec<_>>(),
        remote_port,
    )
    .await;
    assert!(!uncertain.status.success());
    assert!(
        root.join("remote-owner").exists(),
        "fake remote launch completed before SSH lost reply"
    );
    assert!(
        root.join("client-profile/ssh-launch")
            .read_dir()
            .unwrap()
            .any(|entry| entry
                .unwrap()
                .path()
                .extension()
                .is_some_and(|ext| ext == "json")),
        "uncertain launch must retain a durable ownership intent"
    );
    fs::remove_file(root.join("fail-start-reply")).unwrap();
    fs::remove_file(root.join("forward-port")).ok();
    let relay = relay_from_forward_file(root.to_path_buf(), remote_port).await;
    let recovered = invoke(
        root,
        &ssh,
        &args.iter().map(String::as_str).collect::<Vec<_>>(),
        remote_port,
    )
    .await;
    relay.abort();
    assert!(
        recovered.status.success(),
        "{}",
        String::from_utf8_lossy(&recovered.stderr)
    );
    assert!(String::from_utf8_lossy(&recovered.stdout).contains("process=owned"));
    let stopped = invoke(root, &ssh, &stop, remote_port).await;
    assert!(stopped.status.success());
    assert!(!root.join("remote-owner").exists());
    fs::write(root.join("fail-forward"), "1").unwrap();
    fs::write(root.join("fail-stop"), "1").unwrap();
    fs::remove_file(root.join("forward-port")).ok();
    let failed_cleanup = invoke(
        root,
        &ssh,
        &args.iter().map(String::as_str).collect::<Vec<_>>(),
        remote_port,
    )
    .await;
    assert!(!failed_cleanup.status.success());
    assert!(String::from_utf8_lossy(&failed_cleanup.stderr).contains("ownership intent retained"));
    assert!(root.join("remote-owner").exists());
    fs::remove_file(root.join("fail-stop")).unwrap();
    fs::remove_file(root.join("fail-forward")).unwrap();
    let stopped = invoke(root, &ssh, &stop, remote_port).await;
    assert!(stopped.status.success());
    assert!(!root.join("remote-owner").exists());

    fs::write(root.join("deny-state-finalize"), "1").unwrap();
    fs::remove_file(root.join("forward-port")).ok();
    let relay = relay_from_forward_file(root.to_path_buf(), remote_port).await;
    let state_failure = invoke(
        root,
        &ssh,
        &args.iter().map(String::as_str).collect::<Vec<_>>(),
        remote_port,
    )
    .await;
    relay.abort();
    fs::remove_file(root.join("deny-state-finalize")).unwrap();
    fs::set_permissions(
        root.join("client-profile/ssh-launch"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    assert!(!state_failure.status.success());
    assert!(
        String::from_utf8_lossy(&state_failure.stderr)
            .contains("ownership intent could not be finalized")
    );
    let state_path = fs::read_dir(root.join("client-profile/ssh-launch"))
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.extension().is_some_and(|ext| ext == "json"))
        .unwrap();
    let state: serde_json::Value = serde_json::from_slice(&fs::read(state_path).unwrap()).unwrap();
    assert_eq!(state["phase"], "intent");
    assert!(
        invoke(root, &ssh, &stop, remote_port)
            .await
            .status
            .success()
    );

    let blocker = root.join("blocker");
    fs::write(&blocker, "file").unwrap();
    let new_pairing: IssuedPairing = local
        .call_as(
            zeron_rpc::methods::CREATE_REMOTE_PAIRING,
            serde_json::to_value(CreatePairing {
                label: "synthetic token failure".into(),
                scopes: vec![RemoteScope::EnvironmentRead, RemoteScope::TasksRead],
                pairing_ttl_secs: 300,
                session_ttl_secs: 86_400,
            })
            .unwrap(),
        )
        .await
        .unwrap();
    fs::write(
        root.join("pairing.json"),
        serde_json::to_vec(&new_pairing).unwrap(),
    )
    .unwrap();
    let mut invalid_token = probe_args(
        &core.device_id,
        &issued.target.environment_id,
        &artifact,
        &blocker.join("token"),
    );
    invalid_token.push("--enroll".into());
    fs::remove_file(root.join("forward-port")).ok();
    let relay = relay_from_forward_file(root.to_path_buf(), remote_port).await;
    let token_failure = invoke(
        root,
        &ssh,
        &invalid_token.iter().map(String::as_str).collect::<Vec<_>>(),
        remote_port,
    )
    .await;
    relay.abort();
    assert!(!token_failure.status.success());
    assert!(String::from_utf8_lossy(&token_failure.stderr).contains("ownership intent retained"));
    assert!(root.join("remote-owner").exists());
    assert!(
        invoke(root, &ssh, &stop, remote_port)
            .await
            .status
            .success()
    );
    wrong[2] = "-bad-host".into();
    let refs: Vec<_> = wrong.iter().map(String::as_str).collect();
    let invalid_host = invoke(root, &ssh, &refs, remote_port).await;
    assert!(!invalid_host.status.success());
    server.abort();
}

#[tokio::test]
async fn ssh_cli_rejects_local_checksum_and_version_before_transport() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();
    let (ssh, artifact) = fixture(root);
    let session = root.join("session.token");
    fs::write(&session, "synthetic-token\n").unwrap();
    fs::set_permissions(&session, fs::Permissions::from_mode(0o600)).unwrap();
    let args = probe_args(
        "22222222-2222-4222-8222-222222222222",
        &"0".repeat(64),
        &artifact,
        &session,
    );
    fs::write(&artifact, "corrupted").unwrap();
    let refs: Vec<_> = args.iter().map(String::as_str).collect();
    let bad_hash = invoke(root, &ssh, &refs, 0).await;
    assert!(!bad_hash.status.success());
    assert!(String::from_utf8_lossy(&bad_hash.stderr).contains("checksum"));
    assert!(!root.join("modes").exists());

    let (_, artifact) = fixture(root);
    fs::write(&artifact, "#!/bin/sh\necho 'luna-headless 0.0.0'\n").unwrap();
    fs::write(root.join("remote-version-check"), "1").unwrap();
    let name = artifact.file_name().unwrap().to_str().unwrap();
    let hash = format!("{:x}", Sha256::digest(fs::read(&artifact).unwrap()));
    fs::write(
        root.join(format!("{name}.sha256")),
        format!("{hash}  {name}\n"),
    )
    .unwrap();
    let bad_version = invoke(root, &ssh, &refs, 0).await;
    assert!(!bad_version.status.success());
    assert!(String::from_utf8_lossy(&bad_version.stderr).contains("version"));
    assert!(
        fs::read_to_string(root.join("modes"))
            .unwrap()
            .contains("install")
    );
}

#[tokio::test]
async fn foreign_platform_artifact_is_never_executed_on_client() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();
    let (ssh, _) = fixture(root);
    fs::write(root.join("platform"), "Linux x86_64\n").unwrap();
    fs::write(root.join("remote-version-check"), "1").unwrap();
    let name = format!("luna-headless-v{}-linux-x64", env!("CARGO_PKG_VERSION"));
    let artifact = root.join(&name);
    let marker = root.join("client-executed-foreign");
    fs::write(&artifact, format!(
        "#!/bin/sh\nif [ -z \"${{FAKE_REMOTE_EXECUTION:-}}\" ]; then touch '{}'; echo 'luna-headless {}'; else echo 'luna-headless 0.0.0'; fi\n",
        marker.display(), env!("CARGO_PKG_VERSION"),
    )).unwrap();
    fs::set_permissions(&artifact, fs::Permissions::from_mode(0o700)).unwrap();
    let hash = format!("{:x}", Sha256::digest(fs::read(&artifact).unwrap()));
    fs::write(
        root.join(format!("{name}.sha256")),
        format!("{hash}  {name}\n"),
    )
    .unwrap();
    let session = root.join("session.token");
    fs::write(&session, "synthetic-token\n").unwrap();
    fs::set_permissions(&session, fs::Permissions::from_mode(0o600)).unwrap();
    let args = probe_args(
        "22222222-2222-4222-8222-222222222222",
        &"0".repeat(64),
        &artifact,
        &session,
    );
    let output = invoke(
        root,
        &ssh,
        &args.iter().map(String::as_str).collect::<Vec<_>>(),
        0,
    )
    .await;
    assert!(!output.status.success());
    assert!(!marker.exists(), "foreign executable ran on the client");
    assert!(String::from_utf8_lossy(&output.stderr).contains("staged artifact version mismatch"));
}

#[tokio::test]
async fn persistent_remote_read_view_is_available() {
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_luna"))
        .args(["ssh", "view", "--help"])
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("--task"));
}

#[tokio::test]
async fn remote_read_window_exit_cancels_a_pending_ssh_connect() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();
    let (ssh, artifact) = fixture(root);
    fs::write(root.join("hang-platform"), "1").unwrap();
    let session = root.join("session.token");
    fs::write(&session, "synthetic-token\n").unwrap();
    fs::set_permissions(&session, fs::Permissions::from_mode(0o600)).unwrap();
    let mut args = probe_args(
        "22222222-2222-4222-8222-222222222222",
        &"0".repeat(64),
        &artifact,
        &session,
    );
    args[0] = "view".into();
    args.extend([
        "--project".into(),
        "30303030-3030-4030-8030-303030303030".into(),
        "--task".into(),
        "31313131-3131-4131-8131-313131313131".into(),
        "--fixture-stdio".into(),
    ]);
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_luna"))
        .arg("ssh")
        .args(&args)
        .env("LUNA_DATA_DIR", root.join("client-profile"))
        .env("LUNA_SSH_BIN", &ssh)
        .env("FAKE_SSH_ROOT", root)
        .env("ZERON_WORKOS_CLIENT_ID", "")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = tokio::io::BufReader::new(child.stdout.take().unwrap()).lines();
    next_view_state(&mut output, "connecting").await;
    input.write_all(b"exit\n").await.unwrap();
    let status = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
    assert!(
        status.is_ok(),
        "closing the read view should cancel pending SSH"
    );
}
