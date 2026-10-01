//! CC-11: Claude MCP control plane through the real `ClaudeHarness` process
//! path against `tests/fixtures/fake-claude-mcp.py`, whose frames mirror
//! isolated 2.1.286 captures. Failure cases: cc11 `failure-cases.md`.

#![cfg(unix)]

use std::path::{Path, PathBuf};

use futures::StreamExt;
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

use luna_harness::mcp_control::{
    McpControlOp, McpControlReply, McpControlRequest, McpWriteOutcome,
};
use luna_harness::{CancellationToken, ClaudeHarness, Harness, RunControls};
use luna_proto::{PermissionMode, ProviderMcpStatus, RunRequest, SandboxLevel};

const SECRET_HEADER: &str = "header-value-must-not-leak";
const SECRET_ARG: &str = "--token=arg-secret-must-not-leak";
const WIDGET: &str = "<!doctype html><html><body><h1>LUNA-CC11-WIDGET-MARKER</h1><script>window.parent.postMessage({},'*')</script></body></html>";

fn fake() -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-claude-mcp.py");
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
    path
}

fn harness() -> ClaudeHarness {
    ClaudeHarness::new().with_executable(fake())
}

/// Real 2.1.286 `mcp_status` row shapes, including credential-bearing config.
fn state() -> Value {
    json!({
        "servers": [
            {
                "name": "userdemo", "status": "connected", "scope": "user", "source": "user",
                "config": {"type": "stdio", "command": "/opt/secret/bin/python3", "args": ["/srv/demo_mcp.py", SECRET_ARG]},
                "serverInfo": {"name": "luna-cc11-demo", "version": "0.0.1"},
                "tools": [
                    {"name": "show_widget", "annotations": {}, "_meta": {"ui": {"resourceUri": "ui://demo/widget.html", "visibility": ["model", "app"]}}},
                    {"name": "echo", "annotations": {}}
                ],
                "resources": {"ui://demo/widget.html": [{
                    "uri": "ui://demo/widget.html", "mimeType": "text/html;profile=mcp-app", "text": WIDGET,
                    "_meta": {"ui": {"csp": {"connectDomains": [], "resourceDomains": ["https://cdn.example.test"]}, "permissions": {"camera": {}}, "prefersBorder": true}, "com.anthropic/reserved": "x"}
                }]}
            },
            {
                "name": "userauth", "status": "failed", "scope": "user", "source": "user",
                "error": "Dynamic Client Registration rejected (HTTP 401) at https://mcp.example.test/mcp?token=abc for /Users/someone/.config/x",
                "config": {"type": "http", "url": "https://mcp.example.test/mcp?token=abc", "headers": {"Authorization": SECRET_HEADER}}
            },
            {
                "name": "projdemo", "status": "connected", "scope": "project", "source": "project",
                "config": {"type": "stdio", "command": "python3", "args": []},
                "serverInfo": {"name": "luna-cc11-demo", "version": "0.0.1"},
                "tools": [{"name": "echo", "annotations": {}}],
                "slowPolls": 2
            },
            {
                "name": "dyndemo", "status": "connected", "scope": "dynamic", "source": "dynamic",
                "config": {"type": "stdio", "command": "python3", "args": []},
                "serverInfo": {"name": "luna-cc11-demo", "version": "0.0.1"},
                "tools": []
            },
            {"name": "future", "status": "hibernating", "scope": "user", "source": "user", "config": {"type": "stdio", "command": "x"}}
        ],
        "sessionInit": {
            "type": "system", "subtype": "init", "session_id": "sess-mcp-1", "model": "claude-haiku-4-5-20251001",
            "tools": ["Bash", "mcp__userdemo__show_widget"],
            "mcp_servers": [
                {"name": "userdemo", "status": "connected", "source": "user"},
                {"name": "userauth", "status": "needs-auth", "source": "user"}
            ],
            "capabilities": ["interrupt_receipt_v1", "interrupt_cancel_queued_v1", "msg_lifecycle_v1", "mcp_read_resource_v1", "mcp_tool_ui_meta_v1"]
        }
    })
}

struct Fixture {
    _dir: tempfile::TempDir,
    project: PathBuf,
    state: PathBuf,
    log: PathBuf,
}

fn fixture(state_value: Value) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let state = dir.path().join("state.json");
    let log = dir.path().join("log.jsonl");
    std::fs::write(&state, serde_json::to_vec_pretty(&state_value).unwrap()).unwrap();
    point(&project, &state, &log);
    Fixture {
        _dir: dir,
        project,
        state,
        log,
    }
}

fn point(cwd: &Path, state: &Path, log: &Path) {
    std::fs::write(
        cwd.join(".fake-claude-mcp.json"),
        json!({"state": state, "log": log}).to_string(),
    )
    .unwrap();
}

fn log_entries(log: &Path) -> Vec<Value> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn assert_no_grants_or_turns(log: &Path) {
    for entry in log_entries(log) {
        let event = entry["event"].as_str().unwrap_or_default();
        assert_ne!(event, "bypass_flag_rejected", "{entry}");
        assert_ne!(event, "user_message_on_control_session", "{entry}");
        assert_ne!(
            event, "user_message",
            "control sessions never write a user turn: {entry}"
        );
        let argv: Vec<&str> = entry["argv"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a.as_str().unwrap())
            .collect();
        assert!(!argv.contains(&"--permission-mode"), "{argv:?}");
        assert!(
            !argv.contains(&"--dangerously-skip-permissions"),
            "{argv:?}"
        );
    }
}

async fn control(
    fixture: &Fixture,
    op: McpControlOp,
) -> Result<McpControlReply, luna_harness::HarnessError> {
    harness()
        .mcp_control(McpControlRequest {
            cwd: fixture.project.clone(),
            op,
        })
        .await
}

#[tokio::test]
async fn status_reads_back_per_launch_mode_and_redacts_configuration() {
    let fixture = fixture(state());
    let reply = control(
        &fixture,
        McpControlOp::Status {
            modes: vec![
                PermissionMode::Ask,
                PermissionMode::Auto,
                PermissionMode::Full,
            ],
        },
    )
    .await
    .unwrap();
    let McpControlReply::Status(readbacks) = &reply else {
        panic!("unexpected reply {reply:?}");
    };
    // Ask and Auto share the restricted launch flags: one provider process.
    assert_eq!(readbacks.len(), 2, "{readbacks:?}");
    let restricted = &readbacks[0];
    assert_eq!(
        restricted.modes,
        [PermissionMode::Ask, PermissionMode::Auto]
    );
    assert!(restricted.restricted);
    let names: Vec<&str> = restricted.servers.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        ["dyndemo"],
        "restricted mode loads only explicit dynamic config"
    );
    let full = &readbacks[1];
    assert_eq!(full.modes, [PermissionMode::Full]);
    assert!(!full.restricted);
    assert!(full.settled, "slow server settles within the deadline");
    let by_name = |name: &str| full.servers.iter().find(|s| s.name == name).unwrap();
    let demo = by_name("userdemo");
    assert_eq!(demo.status, ProviderMcpStatus::Connected);
    assert_eq!(demo.scope.as_deref(), Some("user"));
    assert_eq!(demo.transport.as_deref(), Some("stdio"));
    assert_eq!(demo.server_version.as_deref(), Some("luna-cc11-demo 0.0.1"));
    assert_eq!(demo.tools.len(), 2);
    assert_eq!(
        demo.tools[0].ui_resource_uri.as_deref(),
        Some("ui://demo/widget.html")
    );
    assert_eq!(demo.tools[0].ui_visibility, ["model", "app"]);
    assert_eq!(by_name("projdemo").status, ProviderMcpStatus::Connected);
    assert_eq!(by_name("future").status, ProviderMcpStatus::Unknown);
    let auth = by_name("userauth");
    assert_eq!(auth.status, ProviderMcpStatus::Failed);
    let error = auth.error.as_deref().unwrap();
    assert!(
        error.contains("[url]") && error.contains("[path]"),
        "{error}"
    );
    assert_eq!(full.app_read_supported, Some(true));
    let debug = format!("{reply:?}");
    for secret in [
        SECRET_HEADER,
        SECRET_ARG,
        "token=abc",
        "/opt/secret",
        "/Users/someone",
    ] {
        assert!(!debug.contains(secret), "secret {secret} leaked: {debug}");
    }
    let entries = log_entries(&fixture.log);
    let restricted_argv: Vec<Vec<String>> = entries
        .iter()
        .filter(|e| e["subtype"] == "initialize")
        .map(|e| serde_json::from_value(e["argv"].clone()).unwrap())
        .collect();
    assert_eq!(restricted_argv.len(), 2);
    assert!(
        restricted_argv
            .iter()
            .any(|argv| argv.contains(&"--restricted".to_owned()))
    );
    assert_no_grants_or_turns(&fixture.log);
}

#[tokio::test]
async fn pending_server_stays_pending_at_the_deadline() {
    let mut value = state();
    value["servers"][2]["slowPolls"] = json!(-1);
    let fixture = fixture(value);
    let McpControlReply::Status(readbacks) = control(
        &fixture,
        McpControlOp::Status {
            modes: vec![PermissionMode::Full],
        },
    )
    .await
    .unwrap() else {
        panic!("status reply expected");
    };
    assert!(!readbacks[0].settled);
    let slow = readbacks[0]
        .servers
        .iter()
        .find(|s| s.name == "projdemo")
        .unwrap();
    assert_eq!(slow.status, ProviderMcpStatus::Pending);
}

#[tokio::test]
async fn toggle_is_provider_owned_persistent_and_per_project() {
    let fixture = fixture(state());
    let reply = control(
        &fixture,
        McpControlOp::SetEnabled {
            server: "userdemo".into(),
            enabled: false,
        },
    )
    .await
    .unwrap();
    let McpControlReply::SetEnabled {
        outcome, readback, ..
    } = reply
    else {
        panic!("set reply expected");
    };
    assert_eq!(outcome, McpWriteOutcome::Applied);
    let after = readback.unwrap();
    assert_eq!(
        after.iter().find(|s| s.name == "userdemo").unwrap().status,
        ProviderMcpStatus::Disabled
    );
    // A new provider process in the same project still sees it disabled.
    let McpControlReply::Status(readbacks) = control(
        &fixture,
        McpControlOp::Status {
            modes: vec![PermissionMode::Full],
        },
    )
    .await
    .unwrap() else {
        panic!("status reply expected");
    };
    assert_eq!(
        readbacks[0]
            .servers
            .iter()
            .find(|s| s.name == "userdemo")
            .unwrap()
            .status,
        ProviderMcpStatus::Disabled
    );
    // Another project keeps its own preference.
    let other = fixture.project.parent().unwrap().join("other");
    std::fs::create_dir_all(&other).unwrap();
    point(&other, &fixture.state, &fixture.log);
    let McpControlReply::Status(other_readbacks) = harness()
        .mcp_control(McpControlRequest {
            cwd: other.clone(),
            op: McpControlOp::Status {
                modes: vec![PermissionMode::Full],
            },
        })
        .await
        .unwrap()
    else {
        panic!("status reply expected");
    };
    assert_eq!(
        other_readbacks[0]
            .servers
            .iter()
            .find(|s| s.name == "userdemo")
            .unwrap()
            .status,
        ProviderMcpStatus::Connected
    );
    let McpControlReply::SetEnabled {
        outcome, readback, ..
    } = control(
        &fixture,
        McpControlOp::SetEnabled {
            server: "userdemo".into(),
            enabled: true,
        },
    )
    .await
    .unwrap()
    else {
        panic!("set reply expected");
    };
    assert_eq!(outcome, McpWriteOutcome::Applied);
    assert_eq!(
        readback
            .unwrap()
            .iter()
            .find(|s| s.name == "userdemo")
            .unwrap()
            .status,
        ProviderMcpStatus::Connected
    );
    assert_no_grants_or_turns(&fixture.log);
}

#[tokio::test]
async fn toggle_of_unknown_server_is_rejected_before_any_write() {
    let fixture = fixture(state());
    let McpControlReply::SetEnabled { outcome, .. } = control(
        &fixture,
        McpControlOp::SetEnabled {
            server: "nosuch".into(),
            enabled: false,
        },
    )
    .await
    .unwrap() else {
        panic!("set reply expected");
    };
    assert_eq!(outcome, McpWriteOutcome::Rejected);
    assert!(
        !log_entries(&fixture.log)
            .iter()
            .any(|e| e["subtype"] == "mcp_toggle"),
        "no toggle may be written for a server Claude does not report"
    );
}

#[tokio::test]
async fn unconfirmed_toggle_is_indeterminate() {
    let mut value = state();
    value["dieAfterWrite"] = json!(["mcp_toggle"]);
    let fixture = fixture(value);
    let McpControlReply::SetEnabled {
        outcome, message, ..
    } = control(
        &fixture,
        McpControlOp::SetEnabled {
            server: "userdemo".into(),
            enabled: false,
        },
    )
    .await
    .unwrap()
    else {
        panic!("set reply expected");
    };
    assert_eq!(outcome, McpWriteOutcome::Indeterminate);
    assert!(message.unwrap().contains("may have been applied"));
}

#[tokio::test]
async fn app_resource_read_is_ui_only_bounded_and_never_rendered() {
    let fixture = fixture(state());
    let McpControlReply::AppResource(view) = control(
        &fixture,
        McpControlOp::ReadAppResource {
            server: "userdemo".into(),
            uri: "ui://demo/widget.html".into(),
        },
    )
    .await
    .unwrap() else {
        panic!("resource reply expected");
    };
    use sha2::Digest as _;
    assert_eq!(
        view.sha256,
        format!("{:x}", sha2::Sha256::digest(WIDGET.as_bytes()))
    );
    assert_eq!(view.byte_len, WIDGET.len());
    assert_eq!(view.mime_type.as_deref(), Some("text/html;profile=mcp-app"));
    assert_eq!(view.text_preview.as_deref(), Some(WIDGET));
    assert!(!view.truncated && !view.binary && !view.rendered);
    assert_eq!(view.csp_resource_domains, ["https://cdn.example.test"]);
    assert_eq!(view.permissions, ["camera"]);
    assert_eq!(view.prefers_border, Some(true));
    assert!(!format!("{view:?}").contains("com.anthropic"));

    let spawned_before = log_entries(&fixture.log).len();
    let refused = control(
        &fixture,
        McpControlOp::ReadAppResource {
            server: "userdemo".into(),
            uri: "note://demo/readme".into(),
        },
    )
    .await
    .unwrap_err();
    assert!(refused.to_string().contains("ui://"), "{refused}");
    assert_eq!(
        log_entries(&fixture.log).len(),
        spawned_before,
        "refused before spawning"
    );

    let failed = control(
        &fixture,
        McpControlOp::ReadAppResource {
            server: "userauth".into(),
            uri: "ui://demo/widget.html".into(),
        },
    )
    .await
    .unwrap_err();
    assert!(failed.to_string().contains("not connected"), "{failed}");

    let mut old = state();
    old["unsupported"] = json!(["mcp_read_resource"]);
    let old_cli = self::fixture(old);
    let unsupported = control(
        &old_cli,
        McpControlOp::ReadAppResource {
            server: "userdemo".into(),
            uri: "ui://demo/widget.html".into(),
        },
    )
    .await
    .unwrap_err();
    assert!(
        unsupported.to_string().contains("does not support"),
        "{unsupported}"
    );
    let McpControlReply::Status(readbacks) = control(
        &old_cli,
        McpControlOp::Status {
            modes: vec![PermissionMode::Full],
        },
    )
    .await
    .unwrap() else {
        panic!("status reply expected");
    };
    assert_eq!(readbacks[0].app_read_supported, Some(false));
    assert_no_grants_or_turns(&fixture.log);
}

fn run_request(cwd: &Path) -> RunRequest {
    RunRequest {
        review: None,
        input_items: Vec::new(),
        prompt: "hello".into(),
        harness: None,
        provider_account_id: None,
        model: None,
        reasoning: None,
        model_options: serde_json::Map::new(),
        cwd: cwd.to_string_lossy().into_owned(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: false,
        permission_mode: PermissionMode::Ask,
        parent_permission_ceiling: None,
        environment_permission_ceiling: None,
        permission_policy: None,
        attachments: Vec::new(),
        worktree: None,
        resume: None,
    }
}

fn run_controls() -> RunControls {
    let (_steer_tx, steer_rx) = mpsc::channel(1);
    RunControls {
        request_input: Box::new(|_| {
            let (_tx, rx) = oneshot::channel();
            rx
        }),
        steering: steer_rx,
        interrupt: CancellationToken::new(),
        invoke_luna_tool: None,
    }
}

async fn run_to_end(harness: &ClaudeHarness, cwd: &Path) {
    let mut stream = harness.run(run_request(cwd), run_controls()).await.unwrap();
    while let Some(event) = stream.next().await {
        if matches!(event, Ok(luna_proto::AgentEvent::Done { .. })) {
            break;
        }
    }
}

#[tokio::test]
async fn session_evidence_comes_only_from_the_runs_own_init_frame() {
    let fixture = fixture(state());
    let harness = harness();
    assert!(harness.session_mcp_observation("sess-mcp-1").is_none());
    run_to_end(&harness, &fixture.project).await;
    let observed = harness.session_mcp_observation("sess-mcp-1").unwrap();
    assert_eq!(observed.servers.len(), 2);
    assert_eq!(observed.servers[0].name, "userdemo");
    assert_eq!(observed.servers[0].status, ProviderMcpStatus::Connected);
    assert_eq!(observed.servers[1].status, ProviderMcpStatus::NeedsAuth);
    assert!(observed.capabilities_reported);
    assert!(observed.app_read_capability && observed.app_tool_meta_capability);
    assert!(harness.session_mcp_observation("sess-other").is_none());
    // A different harness instance (LUNA restart) has no evidence.
    assert!(
        self::harness()
            .session_mcp_observation("sess-mcp-1")
            .is_none()
    );

    // A malformed row discards the whole observation (never "not attached").
    let mut bad = state();
    bad["sessionInit"]["session_id"] = json!("sess-mcp-bad");
    bad["sessionInit"]["mcp_servers"] =
        json!([{"name": "ok", "status": "connected"}, {"status": "connected"}]);
    let malformed = self::fixture(bad);
    run_to_end(&harness, &malformed.project).await;
    assert!(harness.session_mcp_observation("sess-mcp-bad").is_none());
    // A restricted session's empty list is real evidence: zero servers.
    let mut empty = state();
    empty["sessionInit"]["session_id"] = json!("sess-mcp-empty");
    empty["sessionInit"]["mcp_servers"] = json!([]);
    let restricted = self::fixture(empty);
    run_to_end(&harness, &restricted.project).await;
    let observed = harness.session_mcp_observation("sess-mcp-empty").unwrap();
    assert!(observed.servers.is_empty());
}
