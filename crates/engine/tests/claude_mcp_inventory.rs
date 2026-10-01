//! CC-11 / E16 through the production engine RPC surface: Claude's effective
//! MCP state in the shared integration inventory, provider-owned per-project
//! enable/disable with readback, inert MCP Apps resource reads, and selected
//! chat session evidence from a real Claude run path. The provider is the
//! deterministic fake CLI (2.1.286 frame shapes); provider homes are temporary.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use luna_doc::SessionCommandPayload;
use luna_engine::{AgentAccountsConfig, EngineCore, EngineProfile, HarnessRegistry};
use luna_harness::ClaudeHarness;
use luna_proto::{
    ChatConfig, HarnessId, IntegrationActionAvailability, IntegrationInventoryItem,
    IntegrationInventoryView, IntegrationKind, PermissionMode, ProviderMcpActionOutcome,
    ProviderMcpActionReceipt, ProviderMcpAppResourceView, ProviderMcpAppSupport, ProviderMcpStatus,
    RunRequest, SandboxLevel,
};
use luna_rpc::{RpcClient, methods};
use serde_json::{Value, json};

const SECRET_HEADER: &str = "header-value-must-not-leak";
const SECRET_ARG: &str = "--token=arg-secret-must-not-leak";
const WIDGET: &str = "<!doctype html><html><body><h1>LUNA-CC11-WIDGET-MARKER</h1></body></html>";
const TASK: &str = "cc11-claude-task";

fn fake() -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../harness/tests/fixtures/fake-claude-mcp.py");
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
    path
}

fn state() -> Value {
    json!({
        "servers": [
            {
                "name": "userdemo", "status": "connected", "scope": "user", "source": "user",
                "config": {"type": "stdio", "command": "/opt/secret/bin/python3", "args": [SECRET_ARG]},
                "serverInfo": {"name": "luna-cc11-demo", "version": "0.0.1"},
                "tools": [
                    {"name": "show_widget", "annotations": {}, "_meta": {"ui": {"resourceUri": "ui://demo/widget.html", "visibility": ["model", "app"]}}},
                    {"name": "echo", "annotations": {}}
                ],
                "resources": {"ui://demo/widget.html": [{
                    "uri": "ui://demo/widget.html", "mimeType": "text/html;profile=mcp-app", "text": WIDGET,
                    "_meta": {"ui": {"csp": {"connectDomains": [], "resourceDomains": ["https://cdn.example.test"]}}}
                }]}
            },
            {
                "name": "userauth", "status": "failed", "scope": "user", "source": "user",
                "error": "Dynamic Client Registration rejected (HTTP 401) at https://mcp.example.test/mcp?token=abc",
                "config": {"type": "http", "url": "https://mcp.example.test/mcp?token=abc", "headers": {"Authorization": SECRET_HEADER}}
            },
            {
                "name": "projdemo", "status": "connected", "scope": "project", "source": "project",
                "config": {"type": "stdio", "command": "python3", "args": []},
                "tools": [{"name": "echo", "annotations": {}}]
            },
            {
                "name": "dyndemo", "status": "connected", "scope": "dynamic", "source": "dynamic",
                "config": {"type": "stdio", "command": "python3", "args": []},
                "tools": []
            }
        ],
        "sessionInit": {
            "type": "system", "subtype": "init", "session_id": "sess-cc11-engine",
            "model": "claude-haiku-4-5-20251001", "tools": ["Bash"],
            "mcp_servers": [
                {"name": "userdemo", "status": "connected", "source": "user"},
                {"name": "userauth", "status": "needs-auth", "source": "user"}
            ],
            "capabilities": ["mcp_read_resource_v1", "mcp_tool_ui_meta_v1"]
        }
    })
}

struct World {
    dir: tempfile::TempDir,
    core: EngineCore,
    client: RpcClient,
    project: PathBuf,
}

fn world() -> World {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("engine");
    let providers = dir.path().join("providers");
    let project = dir.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let state_path = dir.path().join("state.json");
    std::fs::write(&state_path, serde_json::to_vec_pretty(&state()).unwrap()).unwrap();
    std::fs::write(
        project.join(".fake-claude-mcp.json"),
        json!({"state": state_path, "log": dir.path().join("log.jsonl")}).to_string(),
    )
    .unwrap();
    // A synthetic provider-native account: the engine refuses Claude runs
    // without one (no real credential exists anywhere in this test).
    std::fs::create_dir_all(&providers).unwrap();
    std::fs::write(
        providers.join("claude.json"),
        json!({"oauthAccount": {"accountUuid": "cc11-fixture", "emailAddress": "cc11@example.test"}}).to_string(),
    )
    .unwrap();
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(ClaudeHarness::new().with_executable(fake())));
    let core = EngineCore::assemble_with_profile_and_agent_accounts_config(
        EngineProfile::development(&data, "cc11-org", "cc11-user"),
        Arc::new(registry),
        HarnessId::ClaudeCode,
        None,
        AgentAccountsConfig {
            data_dir: data.clone(),
            claude_config_dir: providers.join("claude"),
            claude_config_file: providers.join("claude.json"),
            codex_home: providers.join("codex"),
        },
    )
    .unwrap();
    core.workspace
        .create_space(
            "cc11-project",
            &core.device_id,
            project.to_str().unwrap(),
            Some("CC-11 project".into()),
            false,
        )
        .unwrap();
    let client = luna_rpc::memory_client(core.rpc_service());
    World {
        dir,
        core,
        client,
        project,
    }
}

async fn inventory(client: &RpcClient, params: Value) -> IntegrationInventoryView {
    client
        .call_as(methods::GET_INTEGRATION_INVENTORY, params)
        .await
        .unwrap()
}

fn mcp<'a>(view: &'a IntegrationInventoryView, name: &str) -> &'a IntegrationInventoryItem {
    view.items
        .iter()
        .find(|item| item.kind == IntegrationKind::McpServer && item.name == name)
        .unwrap_or_else(|| panic!("{name} missing from {view:?}"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn claude_effective_mcp_inventory_actions_and_resources() {
    let world = world();
    let client = &world.client;

    // Ordinary Settings read: no provider process, honest hint.
    let quiet = inventory(
        client,
        json!({"provider": "claude-code", "projectId": "cc11-project"}),
    )
    .await;
    assert!(
        quiet
            .items
            .iter()
            .all(|item| item.kind == IntegrationKind::Skill)
    );
    assert!(quiet.help.contains("Check provider"), "{}", quiet.help);

    // No project selected: provider readback refuses truthfully.
    let no_project = inventory(
        client,
        json!({"provider": "claude-code", "probeProvider": true}),
    )
    .await;
    assert!(
        no_project
            .unavailable_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("Select a project")),
        "{no_project:?}"
    );

    let view = inventory(
        client,
        json!({"provider": "claude-code", "projectId": "cc11-project", "probeProvider": true}),
    )
    .await;
    assert_eq!(view.unavailable_reason, None, "{view:?}");
    let serialized = serde_json::to_string(&view).unwrap();
    for secret in [
        SECRET_HEADER,
        SECRET_ARG,
        "token=abc",
        "/opt/secret",
        world.project.to_str().unwrap(),
    ] {
        assert!(
            !serialized.contains(secret),
            "{secret} leaked: {serialized}"
        );
    }
    let demo = mcp(&view, "userdemo");
    let details = demo.mcp.as_ref().unwrap();
    assert_eq!(demo.scope, "user");
    assert_eq!(demo.available, Some(true));
    assert_eq!(
        demo.attached, None,
        "no session evidence without a selected chat"
    );
    assert_eq!(details.status, ProviderMcpStatus::Connected);
    assert_eq!(details.transport.as_deref(), Some("stdio"));
    assert_eq!(details.tool_count, 2);
    assert_eq!(details.app_tools.len(), 1);
    assert_eq!(details.app_support, ProviderMcpAppSupport::ReadSupported);
    // Ask and Auto share Claude's restricted launch: configured servers absent.
    assert_eq!(details.launches.len(), 2, "{:?}", details.launches);
    let restricted = details.launches.iter().find(|l| l.restricted).unwrap();
    assert_eq!(
        restricted.modes,
        [PermissionMode::Ask, PermissionMode::Auto]
    );
    assert_eq!(restricted.status, None);
    let full = details.launches.iter().find(|l| !l.restricted).unwrap();
    assert_eq!(full.status, Some(ProviderMcpStatus::Connected));
    assert!(demo.reason.as_deref().unwrap().contains("restricted mode"));
    assert_eq!(
        demo.actions.disable,
        IntegrationActionAvailability::Supported
    );
    assert_eq!(
        demo.actions.enable,
        IntegrationActionAvailability::Unavailable
    );
    assert_eq!(
        demo.actions.authenticate,
        IntegrationActionAvailability::Unavailable
    );
    let auth = mcp(&view, "userauth");
    assert_eq!(auth.available, Some(false));
    assert!(
        auth.mcp
            .as_ref()
            .unwrap()
            .error
            .as_deref()
            .unwrap()
            .contains("[url]")
    );
    let dynamic = mcp(&view, "dyndemo");
    assert_eq!(
        dynamic.actions.disable,
        IntegrationActionAvailability::Unavailable
    );
    assert!(
        dynamic
            .mcp
            .as_ref()
            .unwrap()
            .launches
            .iter()
            .all(|launch| launch.status == Some(ProviderMcpStatus::Connected)),
        "explicit --mcp-config servers load in every launch mode"
    );

    // Provider-owned disable with readback, then the inventory reflects it.
    let identity = demo.identity.clone().unwrap();
    let rejected = client
        .call(
            methods::SET_PROVIDER_MCP_SERVER_ENABLED,
            json!({"provider": "claude-code", "projectId": "cc11-project", "identity": "intg_mcp_v1_forged", "server": "userdemo", "enabled": false}),
        )
        .await;
    assert!(rejected.is_err(), "a forged identity must be refused");
    let remote = client
        .call(
            methods::SET_PROVIDER_MCP_SERVER_ENABLED,
            json!({"provider": "claude-code", "projectId": "cc11-project", "targetDeviceId": "another-device", "identity": identity, "server": "userdemo", "enabled": false}),
        )
        .await;
    assert!(remote.is_err(), "MCP admin is owner-local");
    let codex = client
        .call(
            methods::SET_PROVIDER_MCP_SERVER_ENABLED,
            json!({"provider": "codex", "projectId": "cc11-project", "identity": identity, "server": "userdemo", "enabled": false}),
        )
        .await;
    assert!(
        codex.is_err(),
        "no other provider is routed to Claude's control"
    );
    let receipt: ProviderMcpActionReceipt = client
        .call_as(
            methods::SET_PROVIDER_MCP_SERVER_ENABLED,
            json!({"provider": "claude-code", "projectId": "cc11-project", "identity": identity, "server": "userdemo", "enabled": false}),
        )
        .await
        .unwrap();
    assert_eq!(
        receipt.outcome,
        ProviderMcpActionOutcome::Verified,
        "{receipt:?}"
    );
    assert_eq!(receipt.observed_status, Some(ProviderMcpStatus::Disabled));
    assert!(receipt.message.contains("new Claude sessions"));
    let after = inventory(
        client,
        json!({"provider": "claude-code", "projectId": "cc11-project", "probeProvider": true}),
    )
    .await;
    let demo_after = mcp(&after, "userdemo");
    assert_eq!(
        demo_after.mcp.as_ref().unwrap().status,
        ProviderMcpStatus::Disabled
    );
    assert_eq!(
        demo_after.actions.enable,
        IntegrationActionAvailability::Supported
    );
    let dyn_receipt: ProviderMcpActionReceipt = client
        .call_as(
            methods::SET_PROVIDER_MCP_SERVER_ENABLED,
            json!({"provider": "claude-code", "projectId": "cc11-project", "identity": mcp(&after, "dyndemo").identity, "server": "dyndemo", "enabled": false}),
        )
        .await
        .unwrap();
    assert_eq!(dyn_receipt.outcome, ProviderMcpActionOutcome::Rejected);
    let enabled: ProviderMcpActionReceipt = client
        .call_as(
            methods::SET_PROVIDER_MCP_SERVER_ENABLED,
            json!({"provider": "claude-code", "projectId": "cc11-project", "identity": identity, "server": "userdemo", "enabled": true}),
        )
        .await
        .unwrap();
    assert_eq!(
        enabled.outcome,
        ProviderMcpActionOutcome::Verified,
        "{enabled:?}"
    );

    // Inert MCP Apps resource read: declared ui:// only.
    let resource: ProviderMcpAppResourceView = client
        .call_as(
            methods::READ_PROVIDER_MCP_APP_RESOURCE,
            json!({"provider": "claude-code", "projectId": "cc11-project", "identity": identity, "server": "userdemo", "uri": "ui://demo/widget.html"}),
        )
        .await
        .unwrap();
    assert_eq!(resource.identity, identity);
    assert_eq!(resource.text_preview.as_deref(), Some(WIDGET));
    assert!(!resource.rendered);
    assert_eq!(resource.csp_resource_domains, ["https://cdn.example.test"]);
    for (uri, why) in [
        ("ui://demo/other.html", "undeclared"),
        ("note://demo/readme", "non-ui"),
    ] {
        let refused = client
            .call(
                methods::READ_PROVIDER_MCP_APP_RESOURCE,
                json!({"provider": "claude-code", "projectId": "cc11-project", "identity": identity, "server": "userdemo", "uri": uri}),
            )
            .await;
        assert!(refused.is_err(), "{why} resource must be refused");
    }

    // Session evidence from a real Claude run in this project.
    let config = ChatConfig {
        harness: HarnessId::ClaudeCode,
        provider_account_id: None,
        model: None,
        reasoning: None,
        model_options: Default::default(),
        sandbox: SandboxLevel::WorkspaceWrite,
        permission_mode: PermissionMode::Ask,
        effective_permission_mode: PermissionMode::Ask,
        permission_ceiling_source: None,
        permission_degradation_reason: None,
        permission_audit: Vec::new(),
    };
    world
        .core
        .workspace
        .create_chat(
            TASK,
            Some("cc11-project"),
            Some(&world.core.device_id),
            Some(config),
            Some(world.project.to_string_lossy().into_owned()),
        )
        .unwrap();
    world.core.doc_host.open(TASK).unwrap();
    world
        .core
        .doc_host
        .queue_command_with_id_and_transfers(
            TASK,
            "cc11-run-1",
            SessionCommandPayload::Run {
                request: run_request(&world.project),
                message_id: "cc11-message-1".into(),
            },
            Vec::new(),
        )
        .unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let attached = loop {
        let view = inventory(
            client,
            json!({"provider": "claude-code", "projectId": "cc11-project", "taskId": TASK}),
        )
        .await;
        if view.items.iter().any(|item| item.attached.is_some()) {
            break view;
        }
        if tokio::time::Instant::now() >= deadline {
            let doc = world.core.doc_host.open(TASK).unwrap();
            let identity = client
                .call(methods::GET_TASK_IDENTITY, json!({"taskId": TASK}))
                .await;
            panic!(
                "no session evidence: {view:?}\nentries: {:#?}\ncommands: {:#?}\nchat: {:#?}\nidentity: {identity:?}\nfake log: {}",
                doc.doc().read_entries(),
                doc.doc().read_commands(),
                world.core.workspace.chat(TASK),
                std::fs::read_to_string(world.dir.path().join("log.jsonl")).unwrap_or_default()
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let session_demo = mcp(&attached, "userdemo");
    assert_eq!(session_demo.attached, Some(true));
    assert_eq!(session_demo.source, "Claude session start");
    assert_eq!(
        session_demo
            .mcp
            .as_ref()
            .unwrap()
            .session
            .as_ref()
            .unwrap()
            .status,
        ProviderMcpStatus::Connected
    );
    let session_auth = mcp(&attached, "userauth");
    assert_eq!(session_auth.attached, Some(false));
    assert_eq!(
        session_auth.mcp.as_ref().unwrap().status,
        ProviderMcpStatus::NeedsAuth
    );
    // Evidence is never borrowed by a chat from another project.
    let other = inventory(
        client,
        json!({"provider": "claude-code", "projectId": null, "taskId": TASK}),
    )
    .await;
    assert!(other.items.iter().all(|item| item.attached.is_none()));
    // With the provider readback, session-only absence is "not attached".
    let combined = inventory(
        client,
        json!({"provider": "claude-code", "projectId": "cc11-project", "taskId": TASK, "probeProvider": true}),
    )
    .await;
    assert_eq!(mcp(&combined, "projdemo").attached, Some(false));
    assert_eq!(mcp(&combined, "userdemo").attached, Some(true));
    assert!(
        mcp(&combined, "projdemo")
            .reason
            .as_deref()
            .unwrap()
            .contains("did not load it")
    );
}

fn run_request(cwd: &Path) -> RunRequest {
    RunRequest {
        review: None,
        input_items: Vec::new(),
        prompt: "hello".into(),
        harness: Some(HarnessId::ClaudeCode),
        provider_account_id: None,
        model: None,
        reasoning: None,
        model_options: Default::default(),
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
