//! Initialization-only probe (CC-07): the REAL Claude CLI attaches LUNA's
//! REAL per-process `luna` MCP endpoint with the exact flags and environment
//! the Claude adapter produces. No model turn happens: a wrapper withholds
//! LUNA's user line from the CLI and sends only the `initialize` and
//! `mcp_status` control requests, then replies to LUNA with a synthetic
//! init/result so the run ends.
//!
//! The CLI runs with an isolated `CLAUDE_CONFIG_DIR` (no login needed for
//! initialization) whose user config defines a `luna` shadow and an `ext`
//! server, so the summary shows which `luna` wins and what the launch policy
//! keeps. `~/.claude*` is never read or written by this probe.
//!
//!     cargo run -p luna-harness --example claude_luna_mcp_probe -- <out> [claude]

use std::sync::Arc;

use futures::StreamExt;
use luna_harness::{CancellationToken, ClaudeHarness, Harness, RunControls};
use luna_proto::{
    HarnessId, LunaAgentRuntimeContract, PermissionMode, RunRequest, RuntimeContext,
    RuntimeDelivery, RuntimeInstructionSection, RuntimeInstructionSource, RuntimeToolDefinition,
    SandboxLevel,
};
use serde_json::{Value, json};
use tokio::sync::mpsc;

const WRAPPER: &str = r#"#!/bin/sh
# Init-only wrapper: record argv (the token never appears there), withhold
# LUNA's user line, drive the real CLI with control requests only.
out='@OUT@'
printf '%s\n' "$@" > "$out/argv.txt"
[ -n "$LUNA_MCP_BEARER_TOKEN" ] && echo present > "$out/token-env.txt"
read -r first
printf '%s\n' "$first" | sed 's/"content":"[^"]*"/"content":"<withheld>"/' > "$out/withheld-user-line.json"
{
  printf '%s\n' '{"type":"control_request","request_id":"init_1","request":{"subtype":"initialize"}}'
  sleep 6
  printf '%s\n' '{"type":"control_request","request_id":"status_1","request":{"subtype":"mcp_status"}}'
  sleep 4
} | env -i HOME="$HOME" PATH="$PATH" TMPDIR="${TMPDIR:-/tmp}" \
    LUNA_MCP_BEARER_TOKEN="$LUNA_MCP_BEARER_TOKEN" CLAUDE_CONFIG_DIR='@CONFIG@' \
    DISABLE_AUTOUPDATER=1 DISABLE_TELEMETRY=1 DISABLE_ERROR_REPORTING=1 \
    '@CLAUDE@' "$@" > "$out/real-stdout.jsonl" 2> "$out/real-stderr.txt"
echo "$?" > "$out/real-exit.txt"
grep -c -F "$LUNA_MCP_BEARER_TOKEN" "$out/real-stdout.jsonl" "$out/real-stderr.txt" > "$out/token-occurrences.txt"
printf '%s\n' '{"type":"system","subtype":"init","cwd":"'"$PWD"'","session_id":"cc07-init-probe","tools":[],"mcp_servers":[]}'
printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"result":"init-only","session_id":"cc07-init-probe","usage":{"input_tokens":0,"output_tokens":0}}'
"#;

fn tool(name: &str, read_only: bool) -> RuntimeToolDefinition {
    RuntimeToolDefinition {
        name: name.into(),
        version: 1,
        description: format!("LUNA {name} (probe)"),
        input_schema: json!({"type":"object","properties":{},"additionalProperties":false}),
        output_schema: json!({"type":"object"}),
        required_scopes: Vec::new(),
        deadline_ms: 2_000,
        idempotency: if read_only {
            "read-only".into()
        } else {
            "one-use local approval".into()
        },
        presentation: "cross-task".into(),
        activity_classification: "luna-cross-task".into(),
    }
}

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let out = std::path::PathBuf::from(args.next().expect("usage: <out> [claude]"));
    let real = args.next().unwrap_or_else(|| "claude".into());
    let _ = std::fs::remove_dir_all(&out);
    let config_dir = out.join("isolated-claude-config");
    let project = out.join("project");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    // User-scope stand-ins: a `luna` shadow and an external (Executor-like)
    // server. Port 9 (discard) is never served.
    std::fs::write(
        config_dir.join(".claude.json"),
        json!({"mcpServers": {
            "luna": {"type": "http", "url": "http://127.0.0.1:9/shadow"},
            "ext": {"type": "http", "url": "http://127.0.0.1:9/ext"}
        }})
        .to_string(),
    )
    .unwrap();
    let wrapper = out.join("claude-wrapper.sh");
    let script = WRAPPER
        .replace("@OUT@", &out.display().to_string())
        .replace("@CONFIG@", &config_dir.display().to_string())
        .replace("@CLAUDE@", &real);
    std::fs::write(&wrapper, script).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let tools = vec![
        tool("luna_status", true),
        tool("list_threads", true),
        tool("read_thread", true),
        tool("wait_threads", true),
        tool("send_message_to_thread", false),
        tool("create_thread", false),
    ];
    let contract = LunaAgentRuntimeContract {
        version: 1,
        content_hash: "cc07-init-probe".into(),
        context: RuntimeContext {
            app_version: "probe".into(),
            protocol_version: 1,
            thread_id: "cc07-probe-thread".into(),
            parent_thread_id: None,
            source_thread_id: None,
            project_id: None,
            project_name: None,
            environment_id: "probe".into(),
            machine_id: "probe".into(),
            machine_name: "probe".into(),
            workspace_root: project.display().to_string(),
            worktree_id: None,
            binding_generation: Some(1),
            provider_profile_id: None,
            branch: None,
            harness: HarnessId::ClaudeCode,
            model: None,
            reasoning: None,
            execution_route: "local".into(),
            granted_scopes: Vec::new(),
        },
        environment: Default::default(),
        local_endpoints: Vec::new(),
        instructions: vec![RuntimeInstructionSection {
            name: "LUNA core".into(),
            source: RuntimeInstructionSource::LunaCore,
            provenance: "cc07 init probe".into(),
            text: "Run inside LUNA.".into(),
        }],
        tools,
        omitted_tools: Vec::new(),
        delivery: RuntimeDelivery::ClaudeMcpTools,
    };
    let mut model_options = serde_json::Map::new();
    model_options.insert(
        luna_proto::LUNA_RUNTIME_CONTRACT_OPTION.into(),
        serde_json::to_value(contract).unwrap(),
    );
    let request = RunRequest {
        review: None,
        input_items: Vec::new(),
        prompt: "init-only probe; this line is withheld from the CLI".into(),
        harness: None,
        provider_account_id: None,
        model: None,
        reasoning: None,
        model_options,
        cwd: project.display().to_string(),
        sandbox: SandboxLevel::ReadOnly,
        auto_approve: false,
        permission_mode: PermissionMode::Ask,
        parent_permission_ceiling: None,
        environment_permission_ceiling: None,
        permission_policy: None,
        attachments: Vec::new(),
        worktree: None,
        resume: None,
    };
    let (_steer_tx, steer_rx) = mpsc::channel(1);
    let controls = RunControls {
        request_input: Box::new(|_| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            let _ = tx.send(Vec::new());
            rx
        }),
        steering: steer_rx,
        interrupt: CancellationToken::new(),
        invoke_luna_tool: Some(Arc::new(|_| {
            Box::pin(async { Ok(json!({"probe": "no tool call is expected"})) })
        })),
    };
    let events = ClaudeHarness::new()
        .with_executable(&wrapper)
        .run(request, controls)
        .await
        .expect("run starts")
        .map(|event| format!("{:?}", event.expect("event")))
        .collect::<Vec<_>>()
        .await;

    let read = |name: &str| std::fs::read_to_string(out.join(name)).unwrap_or_default();
    let argv = read("argv.txt");
    let lines = argv.lines().collect::<Vec<_>>();
    let after = |flag: &str| {
        lines
            .iter()
            .position(|line| *line == flag)
            .and_then(|index| lines.get(index + 1))
            .map(|value| value.to_string())
    };
    let mcp_config: Value = after("--mcp-config")
        .and_then(|config| serde_json::from_str(&config).ok())
        .unwrap_or(Value::Null);
    let status = read("real-stdout.jsonl")
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|frame| frame["response"]["request_id"] == "status_1")
        .unwrap_or(Value::Null);
    let servers = status["response"]["response"]["mcpServers"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|server| {
            json!({
                "name": server["name"],
                "status": server["status"],
                "scope": server["scope"],
                "url": server["config"]["url"],
                "configCarriesHeaders": server["config"].get("headers").is_some(),
                "configCarriesHelper": server["config"].get("headersHelper").is_some(),
                "tools": server["tools"].as_array().map(|tools| tools.iter().map(|tool| tool["name"].clone()).collect::<Vec<_>>()),
            })
        })
        .collect::<Vec<_>>();
    let summary = json!({
        "cli": real,
        "mode": "Ask (--restricted --permission-mode acceptEdits)",
        "modelTurn": false,
        "argvHasStrictMcpConfig": lines.contains(&"--strict-mcp-config"),
        "allowedTools": after("--allowedTools"),
        "mcpConfigServers": mcp_config["mcpServers"].as_object().map(|servers| servers.keys().cloned().collect::<Vec<_>>()),
        "mcpConfigUrl": mcp_config["mcpServers"]["luna"]["url"],
        "mcpConfigUsesHeadersHelper": mcp_config["mcpServers"]["luna"].get("headersHelper").is_some(),
        "tokenEnvPresent": read("token-env.txt").trim() == "present",
        "tokenOccurrencesInCliOutput": read("token-occurrences.txt").trim(),
        "realExit": read("real-exit.txt").trim(),
        "mcpStatus": servers,
        "lunaEvents": events,
    });
    std::fs::write(
        out.join("summary.json"),
        serde_json::to_vec_pretty(&summary).unwrap(),
    )
    .unwrap();
    println!(
        "{}",
        serde_json::to_string_pretty(&summary["mcpStatus"]).unwrap()
    );
}
