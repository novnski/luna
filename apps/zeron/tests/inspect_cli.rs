//! The real CLI executable over a synthetic engine's local RPC socket.

use std::path::Path;
use std::sync::Arc;

use tempfile::TempDir;
use tokio::net::TcpListener;
use zeron_engine::{
    AgentAccountsConfig, Auth, AuthConfig, EngineCore, EngineProfile, HarnessId, HarnessRegistry,
};
use zeron_rpc::{RpcError, RpcReply, RpcService, methods};

fn synthetic_accounts(root: &Path) -> AgentAccountsConfig {
    AgentAccountsConfig {
        data_dir: root.join("provider-state"),
        claude_config_dir: root.join("synthetic-claude"),
        claude_config_file: root.join("synthetic-claude.json"),
        codex_home: root.join("synthetic-codex"),
        cursor_sdk_auth_file: root.join("synthetic-cursor-auth.json"),
    }
}

async fn cli(root: &Path, port: u16, args: &[&str]) -> (i32, serde_json::Value) {
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_luna"))
        .args(["inspect", "--json", "--port", &port.to_string()])
        .args(args)
        .env("LUNA_DATA_DIR", root.join("child-data"))
        .env("CODEX_HOME", root.join("synthetic-codex"))
        .env("CLAUDE_CONFIG_DIR", root.join("synthetic-claude"))
        .env("ZERON_WORKOS_CLIENT_ID", "")
        .env("ZERON_EDGE_URL", "http://127.0.0.1:1")
        .output()
        .await
        .unwrap();
    let code = output.status.code().unwrap();
    let body: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "CLI JSON parse failed ({error}), stdout={}, stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    (code, body)
}

#[tokio::test]
async fn reads_exact_synthetic_engine_and_reports_bounded_targets() {
    let root = TempDir::new().unwrap();
    let accounts = synthetic_accounts(root.path());
    accounts.disable_claude_keychain_fallback().unwrap();
    let core = EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        EngineProfile::development(&root.path().join("data"), "l26-org", "l26-user"),
        Arc::new(HarnessRegistry::new()),
        HarnessId::Mock,
        None,
        accounts,
    )
    .unwrap();
    let mut auth = AuthConfig::new("http://127.0.0.1:1", root.path().join("data"));
    auth.use_in_memory_credentials_for_tests();
    core.set_auth(Auth::new(auth));
    let machine = core.device_id.clone();
    let service = core.rpc_service();
    let client = zeron_rpc::memory_client(service.clone());
    let project = "26262626-2626-4262-8262-262626262626";
    let task = "27272727-2727-4272-8272-272727272727";
    let second_task = "28282828-2828-4282-8282-282828282828";
    let second_project = "29292929-2929-4292-8292-292929292929";
    let path = root.path().join("project");
    std::fs::create_dir_all(&path).unwrap();
    client
        .call(
            methods::MUTATE,
            serde_json::json!({
                "op": "createSpace", "spaceId": project,
                "deviceId": machine, "path": path,
            }),
        )
        .await
        .unwrap();
    client
        .call(
            methods::MUTATE,
            serde_json::json!({
                "op": "createSpace", "spaceId": second_project,
                "deviceId": machine, "path": root.path().join("second-project"),
            }),
        )
        .await
        .unwrap();
    client
        .call(
            methods::MUTATE,
            serde_json::json!({
                "op": "createChat", "chatId": task, "spaceId": project,
                "config": {
                    "harness": "mock", "model": null, "reasoning": null,
                    "sandbox": "workspace-write"
                }
            }),
        )
        .await
        .unwrap();
    client
        .call(
            methods::MUTATE,
            serde_json::json!({
                "op": "createChat", "chatId": second_task, "spaceId": project,
                "config": {
                    "harness": "mock", "model": null, "reasoning": null,
                    "sandbox": "workspace-write"
                }
            }),
        )
        .await
        .unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(zeron_rpc::serve_ws_listener(listener, service.clone()));

    let (code, status) = cli(root.path(), port, &["status"]).await;
    assert_eq!(code, 0);
    assert_eq!(status["data"]["machineId"], machine);
    assert_eq!(status["data"]["ready"], true);
    println!("status: {}", status);
    let (code, doctor) = cli(root.path(), port, &["doctor", "--machine", &machine]).await;
    assert_eq!(code, 0);
    assert_eq!(doctor["data"]["healthy"], true);
    assert!(
        doctor["data"]["writeCommands"]
            .as_str()
            .unwrap()
            .contains("unavailable")
    );
    let (code, version) = cli(root.path(), port, &["version", "--machine", &machine]).await;
    assert_eq!(code, 0);
    assert!(version["data"]["cliVersion"].as_str().is_some());
    assert!(version["data"]["capabilities"].as_array().is_some());

    let exact = ["--machine", &machine, "--environment", &machine];
    let (code, projects) = cli(
        root.path(),
        port,
        &[
            "projects", exact[0], exact[1], exact[2], exact[3], "--limit", "1",
        ],
    )
    .await;
    assert_eq!(code, 0);
    assert_eq!(projects["data"]["projects"].as_array().unwrap().len(), 1);
    assert_eq!(projects["data"]["projects"][0]["id"], project);
    assert_eq!(projects["data"]["truncated"], true);
    assert_eq!(projects["data"]["nextCursor"], project);
    println!("projects page 1: {}", projects);
    let (code, next_projects) = cli(
        root.path(),
        port,
        &[
            "projects",
            "--machine",
            &machine,
            "--environment",
            &machine,
            "--limit",
            "1",
            "--after",
            project,
        ],
    )
    .await;
    assert_eq!(code, 0);
    assert_eq!(next_projects["data"]["projects"][0]["id"], second_project);
    let (code, tasks) = cli(
        root.path(),
        port,
        &[
            "tasks",
            "--machine",
            &machine,
            "--environment",
            &machine,
            "--project",
            project,
            "--limit",
            "1",
        ],
    )
    .await;
    assert_eq!(code, 0);
    assert_eq!(tasks["data"]["tasks"].as_array().unwrap().len(), 1);
    assert_eq!(tasks["data"]["tasks"][0]["id"], task);
    assert_eq!(tasks["data"]["truncated"], true);
    assert_eq!(tasks["data"]["nextCursor"], task);
    println!("tasks page 1: {}", tasks);
    let (code, next_tasks) = cli(
        root.path(),
        port,
        &[
            "tasks",
            "--machine",
            &machine,
            "--environment",
            &machine,
            "--project",
            project,
            "--limit",
            "1",
            "--after",
            task,
        ],
    )
    .await;
    assert_eq!(code, 0);
    assert_eq!(next_tasks["data"]["tasks"][0]["id"], second_task);
    let (code, read) = cli(
        root.path(),
        port,
        &[
            "read",
            "--machine",
            &machine,
            "--environment",
            &machine,
            "--project",
            project,
            "--task",
            task,
            "--limit",
            "1",
        ],
    )
    .await;
    assert_eq!(code, 0);
    assert_eq!(read["data"]["task"]["id"], task);
    assert_eq!(read["data"]["commands"]["snapshot"]["taskId"], task);
    assert_eq!(
        read["data"]["commands"]["events"].as_array().unwrap().len(),
        1
    );
    assert_eq!(
        read["data"]["commands"]["events"][0]["receipt"]["taskId"],
        task
    );
    assert_eq!(
        read["data"]["commands"]["events"][0]["receipt"]["snapshot"]["taskId"],
        task
    );
    assert_eq!(read["data"]["transcript"]["available"], false);
    println!("read: {}", read);
    let human = tokio::process::Command::new(env!("CARGO_BIN_EXE_luna"))
        .args([
            "inspect",
            "--port",
            &port.to_string(),
            "read",
            "--machine",
            &machine,
            "--environment",
            &machine,
            "--project",
            project,
            "--task",
            task,
        ])
        .output()
        .await
        .unwrap();
    assert!(human.status.success());
    assert!(String::from_utf8_lossy(&human.stdout).contains("Command receipts:"));
    assert!(String::from_utf8_lossy(&human.stdout).contains("Transcript: unavailable"));

    let identity = client
        .call(
            methods::GET_TASK_IDENTITY,
            serde_json::json!({ "taskId": task }),
        )
        .await
        .unwrap();
    let commands = client
        .call(
            methods::GET_TASK_COMMAND_SNAPSHOT,
            serde_json::json!({
                "taskId": task, "afterTaskSequence": 0, "limit": 1
            }),
        )
        .await
        .unwrap();
    assert_eq!(commands["events"].as_array().unwrap().len(), 1);
    let mut wrong_identity = identity.clone();
    wrong_identity["task"]["taskId"] = serde_json::json!("foreign-task");
    let mut wrong_environment = identity.clone();
    wrong_environment["workspace"]["environmentId"] = serde_json::json!("foreign-environment");
    let mut wrong_binding = identity.clone();
    wrong_binding["task"]["currentBindingId"] = serde_json::json!("fixture-binding");
    wrong_binding["currentBinding"] = serde_json::json!({
        "schemaVersion": 1, "bindingId": "fixture-binding", "taskId": "foreign-task",
        "generation": 1, "providerId": "mock", "providerInstanceId": "fixture-provider",
        "providerTaskId": "fixture-provider-task", "environmentId": machine,
        "workspaceId": project, "origin": "lunaCreated", "provenance": "fixture",
        "reconciliation": { "state": "current", "reason": "fixture", "observedAtMs": 1 },
        "createdAtMs": 1, "lastObservedAtMs": 1
    });
    wrong_binding["bindings"] = serde_json::json!([wrong_binding["currentBinding"].clone()]);
    assert!(serde_json::from_value::<zeron_proto::TaskIdentityView>(wrong_binding.clone()).is_ok());
    let mut wrong_binding_environment = wrong_binding.clone();
    wrong_binding_environment["currentBinding"]["taskId"] = serde_json::json!(task);
    wrong_binding_environment["currentBinding"]["environmentId"] =
        serde_json::json!("foreign-environment");
    wrong_binding_environment["bindings"] =
        serde_json::json!([wrong_binding_environment["currentBinding"].clone()]);
    let mut wrong_history_binding = identity.clone();
    wrong_history_binding["bindings"] =
        serde_json::json!([wrong_binding["currentBinding"].clone()]);
    let mut wrong_snapshot = commands.clone();
    wrong_snapshot["snapshot"]["taskId"] = serde_json::json!("foreign-task");
    let mut wrong_snapshot_binding = commands.clone();
    wrong_snapshot_binding["snapshot"]["bindingId"] = serde_json::json!("foreign-binding");
    let mut wrong_receipt = commands.clone();
    wrong_receipt["events"][0]["receipt"]["taskId"] = serde_json::json!("foreign-task");
    let mut wrong_receipt_snapshot = commands.clone();
    wrong_receipt_snapshot["events"][0]["receipt"]["snapshot"]["taskId"] =
        serde_json::json!("foreign-task");
    let mut wrong_receipt_binding = commands.clone();
    wrong_receipt_binding["events"][0]["receipt"]["snapshot"]["bindingId"] =
        serde_json::json!("foreign-binding");
    for (method, reply, code, kind) in [
        (
            methods::GET_TASK_IDENTITY,
            wrong_identity,
            7,
            "malformedResponse",
        ),
        (
            methods::GET_TASK_IDENTITY,
            wrong_environment,
            4,
            "targetMismatch",
        ),
        (
            methods::GET_TASK_IDENTITY,
            wrong_binding,
            7,
            "malformedResponse",
        ),
        (
            methods::GET_TASK_IDENTITY,
            wrong_binding_environment,
            4,
            "targetMismatch",
        ),
        (
            methods::GET_TASK_IDENTITY,
            wrong_history_binding,
            7,
            "malformedResponse",
        ),
        (
            methods::GET_TASK_COMMAND_SNAPSHOT,
            wrong_snapshot,
            7,
            "malformedResponse",
        ),
        (
            methods::GET_TASK_COMMAND_SNAPSHOT,
            wrong_snapshot_binding,
            4,
            "targetMismatch",
        ),
        (
            methods::GET_TASK_COMMAND_SNAPSHOT,
            wrong_receipt,
            7,
            "malformedResponse",
        ),
        (
            methods::GET_TASK_COMMAND_SNAPSHOT,
            wrong_receipt_snapshot,
            7,
            "malformedResponse",
        ),
        (
            methods::GET_TASK_COMMAND_SNAPSHOT,
            wrong_receipt_binding,
            7,
            "malformedResponse",
        ),
    ] {
        let (actual_code, body) = corrupt_read(
            root.path(),
            service.clone(),
            method,
            reply,
            &machine,
            project,
            task,
        )
        .await;
        assert_eq!(actual_code, code, "{method}: {body}");
        assert_eq!(body["error"]["kind"], kind);
        println!("corrupt {method}: code={code} kind={kind}");
    }

    let (code, mismatch) = cli(
        root.path(),
        port,
        &[
            "tasks",
            "--machine",
            "other-machine",
            "--environment",
            "other-machine",
            "--project",
            project,
        ],
    )
    .await;
    assert_eq!(code, 4);
    assert_eq!(mismatch["error"]["kind"], "targetMismatch");
    let (code, missing) = cli(
        root.path(),
        port,
        &[
            "read",
            "--machine",
            &machine,
            "--environment",
            &machine,
            "--project",
            project,
            "--task",
            "missing",
        ],
    )
    .await;
    assert_eq!(code, 5);
    assert_eq!(missing["error"]["kind"], "notFound");
    let (code, invalid) = cli(
        root.path(),
        port,
        &[
            "tasks",
            "--machine",
            &machine,
            "--environment",
            &machine,
            "--project",
            project,
            "--limit",
            "101",
        ],
    )
    .await;
    assert_eq!(code, 2);
    assert_eq!(invalid["error"]["kind"], "invalidArgument");

    server.abort();
    let _ = server.await;
    drop(client);
    drop(core);
    let (code, offline) = cli(root.path(), port, &["status"]).await;
    assert_eq!(code, 3);
    assert_eq!(offline["error"]["kind"], "offline");
    println!("exit kinds: targetMismatch=4 notFound=5 invalidArgument=2 offline=3");
}

struct CorruptingReadService {
    inner: Arc<dyn RpcService>,
    method: &'static str,
    reply: serde_json::Value,
}

#[async_trait::async_trait]
impl RpcService for CorruptingReadService {
    async fn handle(&self, method: &str, params: serde_json::Value) -> Result<RpcReply, RpcError> {
        if method == self.method {
            Ok(RpcReply::Value(self.reply.clone()))
        } else {
            self.inner.handle(method, params).await
        }
    }
}

async fn corrupt_read(
    root: &Path,
    inner: Arc<dyn RpcService>,
    method: &'static str,
    reply: serde_json::Value,
    machine: &str,
    project: &str,
    task: &str,
) -> (i32, serde_json::Value) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(zeron_rpc::serve_ws_listener(
        listener,
        Arc::new(CorruptingReadService {
            inner,
            method,
            reply,
        }),
    ));
    let result = cli(
        root,
        port,
        &[
            "read",
            "--machine",
            machine,
            "--environment",
            machine,
            "--project",
            project,
            "--task",
            task,
            "--limit",
            "1",
        ],
    )
    .await;
    server.abort();
    let _ = server.await;
    result
}

#[derive(Clone, Copy)]
enum FailureKind {
    Unavailable,
    Malformed,
    Failed,
}

struct FailureService {
    ready: FailureKind,
}

#[async_trait::async_trait]
impl RpcService for FailureService {
    async fn handle(&self, method: &str, _params: serde_json::Value) -> Result<RpcReply, RpcError> {
        match method {
            methods::ENGINE_INFO => Ok(RpcReply::Value(serde_json::json!({
                "deviceId": "fixture-machine", "workspaceScope": "development",
                "capabilities": []
            }))),
            methods::ENGINE_READY => match self.ready {
                FailureKind::Unavailable => Err(RpcError::UnknownMethod(method.into())),
                FailureKind::Malformed => {
                    Ok(RpcReply::Value(serde_json::json!({ "ready": "yes" })))
                }
                FailureKind::Failed => Err(RpcError::Failed("fixture failure".into())),
            },
            _ => Err(RpcError::UnknownMethod(method.into())),
        }
    }
}

#[tokio::test]
async fn reports_unavailable_malformed_and_engine_failures() {
    let root = TempDir::new().unwrap();
    for (ready, expected_code, expected_kind) in [
        (FailureKind::Unavailable, 6, "unavailable"),
        (FailureKind::Malformed, 7, "malformedResponse"),
        (FailureKind::Failed, 8, "engineError"),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(zeron_rpc::serve_ws_listener(
            listener,
            Arc::new(FailureService { ready }),
        ));
        let (code, body) = cli(root.path(), port, &["status"]).await;
        assert_eq!(code, expected_code);
        assert_eq!(body["error"]["kind"], expected_kind);
        println!("failure: code={code} kind={expected_kind}");
        server.abort();
    }
}

#[tokio::test]
async fn inspect_json_parse_errors_are_machine_readable_without_changing_help_or_other_commands() {
    let root = TempDir::new().unwrap();
    for args in [
        vec!["tasks", "--project", "fixture-project"],
        vec![
            "tasks",
            "--machine",
            "fixture",
            "--environment",
            "fixture",
            "--project",
            "fixture-project",
            "--limit",
            "nope",
        ],
        vec!["status", "--unknown-flag"],
        vec!["unknown-subcommand"],
    ] {
        let (code, body) = cli(root.path(), 0, &args).await;
        assert_eq!(code, 2, "{args:?}: {body}");
        assert_eq!(body["schemaVersion"], 1);
        assert_eq!(body["error"]["kind"], "invalidArgument");
        println!("parse {:?}: code={code} kind=invalidArgument", args);
    }
    let help = tokio::process::Command::new(env!("CARGO_BIN_EXE_luna"))
        .args(["inspect", "--json", "--help"])
        .output()
        .await
        .unwrap();
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("Usage:"));
    let version = tokio::process::Command::new(env!("CARGO_BIN_EXE_luna"))
        .arg("--version")
        .output()
        .await
        .unwrap();
    assert!(version.status.success());
    assert!(String::from_utf8_lossy(&version.stdout).contains("luna"));
    let plain_error = tokio::process::Command::new(env!("CARGO_BIN_EXE_luna"))
        .args(["inspect", "tasks", "--limit", "nope"])
        .output()
        .await
        .unwrap();
    assert_eq!(plain_error.status.code(), Some(2));
    assert!(plain_error.stdout.is_empty());
    assert!(String::from_utf8_lossy(&plain_error.stderr).contains("error:"));
    let other_command_error = tokio::process::Command::new(env!("CARGO_BIN_EXE_luna"))
        .args(["status", "--unknown-flag"])
        .output()
        .await
        .unwrap();
    assert_eq!(other_command_error.status.code(), Some(2));
    assert!(other_command_error.stdout.is_empty());
    assert!(String::from_utf8_lossy(&other_command_error.stderr).contains("error:"));
}
