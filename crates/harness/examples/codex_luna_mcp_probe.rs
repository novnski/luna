//! Live probe: a RESUMED real Codex thread calls LUNA tools through the
//! per-process `luna` MCP endpoint.
//!
//! Run A starts a thread with no LUNA contract (like a pre-tools chat).
//! Run B resumes it with a contract and asks the model to call
//! `list_threads` and then `create_thread`; a synthetic executor answers.
//! A tee wrapper saves every app-server frame, per spawn, under `<out>`.
//! The probe thread is archived at the end. `~/.codex` is never edited: the
//! endpoint rides `-c` overrides on the spawned process only.
//!
//!     cargo run -p luna-harness --example codex_luna_mcp_probe -- <out> [codex] [model]

use std::sync::{Arc, Mutex};

use futures::StreamExt;
use luna_harness::{CancellationToken, CodexHarness, Harness, LunaToolInvocation, RunControls};
use luna_proto::{
    AgentEvent, DoneStatus, HarnessId, LunaAgentRuntimeContract, ReasoningLevel, RunRequest,
    RuntimeContext, RuntimeDelivery, RuntimeInstructionSection, RuntimeInstructionSource,
    RuntimeToolDefinition, SandboxLevel,
};
use serde_json::{Value, json};
use tokio::sync::mpsc;

const WRAPPER: &str = r#"#!/bin/sh
# Records one app-server spawn: argv, token presence (never its value) and
# both stdio directions as JSONL.
out="$LUNA_PROBE_OUT"
n=$(ls "$out"/spawn-*-argv.txt 2>/dev/null | wc -l | tr -d ' '); n=$((n + 1))
printf '%s\n' "$@" > "$out/spawn-$n-argv.txt"
if [ -n "$LUNA_MCP_BEARER_TOKEN" ]; then echo 'LUNA_MCP_BEARER_TOKEN=<present, redacted>' > "$out/spawn-$n-env.txt"; fi
tee -a "$out/spawn-$n-client.jsonl" | "$LUNA_PROBE_REAL_CODEX" "$@" | tee -a "$out/spawn-$n-server.jsonl"
"#;

fn tool(name: &str, description: &str, schema: Value, read_only: bool) -> RuntimeToolDefinition {
    RuntimeToolDefinition {
        name: name.into(),
        version: 1,
        description: description.into(),
        input_schema: schema,
        output_schema: json!({"type":"object"}),
        required_scopes: vec!["threads.read".into()],
        deadline_ms: if read_only { 2_000 } else { 40_000 },
        idempotency: if read_only { "read-only" } else { "durable" }.into(),
        presentation: "cross-task".into(),
        activity_classification: "luna-cross-task".into(),
    }
}

fn contract(cwd: &str) -> LunaAgentRuntimeContract {
    LunaAgentRuntimeContract {
        version: 1,
        content_hash: "luna-mcp-probe".into(),
        context: RuntimeContext {
            app_version: "probe".into(),
            protocol_version: 1,
            thread_id: "luna-mcp-probe-task".into(),
            parent_thread_id: None,
            source_thread_id: None,
            project_id: Some("probe-project".into()),
            project_name: Some("probe".into()),
            environment_id: "probe-environment".into(),
            machine_id: "probe-machine".into(),
            machine_name: "probe".into(),
            workspace_root: cwd.into(),
            worktree_id: None,
            binding_generation: Some(1),
            provider_profile_id: None,
            branch: None,
            harness: HarnessId::Codex,
            model: None,
            reasoning: None,
            execution_route: "local".into(),
            granted_scopes: vec!["threads.read".into()],
            chat: None,
            board: None,
        },
        environment: Default::default(),
        local_endpoints: Vec::new(),
        instructions: vec![RuntimeInstructionSection {
            name: "LUNA core".into(),
            source: RuntimeInstructionSource::Tool,
            provenance: "luna-mcp probe".into(),
            text: "LUNA tools are served by the `luna` MCP server attached to this session.".into(),
        }],
        tools: vec![
            tool(
                "list_threads",
                "List visible local LUNA tasks with bounded metadata and opaque paging.",
                json!({"type":"object","properties":{"limit":{"type":"integer","minimum":1,"maximum":20}},"additionalProperties":false}),
                true,
            ),
            tool(
                "create_thread",
                "Create a persistent visible child chat in this project. Requires exact one-use local user approval.",
                json!({"type":"object","properties":{
                    "prompt":{"type":"string","minLength":1,"maxLength":8000},
                    "idempotencyKey":{"type":"string","minLength":1,"maxLength":128}
                },"required":["prompt","idempotencyKey"],"additionalProperties":false}),
                false,
            ),
        ],
        omitted_tools: Vec::new(),
        delivery: RuntimeDelivery::CodexMcpTools,
    }
}

fn request(cwd: &str, prompt: &str, model: &str, resume: Option<String>) -> RunRequest {
    let mut model_options = serde_json::Map::new();
    model_options.insert(
        luna_harness::codex::LUNA_NATIVE_THREAD_NAME_OPTION.into(),
        "[LUNA] luna-mcp probe".into(),
    );
    RunRequest {
        review: None,
        goal: None,
        input_items: Vec::new(),
        prompt: prompt.into(),
        harness: None,
        provider_account_id: None,
        model: Some(model.into()),
        reasoning: Some(ReasoningLevel::Low),
        model_options,
        cwd: cwd.into(),
        sandbox: SandboxLevel::ReadOnly,
        auto_approve: false,
        permission_mode: luna_proto::PermissionMode::Ask,
        parent_permission_ceiling: None,
        environment_permission_ceiling: None,
        permission_policy: None,
        attachments: Vec::new(),
        resume,
        worktree: None,
    }
}

async fn run(
    harness: &CodexHarness,
    request: RunRequest,
    executor: Option<luna_harness::LunaToolExecutor>,
    log: &mut Vec<Value>,
) -> (Option<DoneStatus>, Option<String>) {
    let (_steer, steering) = mpsc::channel(8);
    let mut stream = harness
        .run(
            request,
            RunControls {
                request_input: Box::new(|_| {
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    drop(tx); // any provider approval is declined: none is expected
                    rx
                }),
                steering,
                interrupt: CancellationToken::new(),
                invoke_luna_tool: executor,
            },
        )
        .await
        .expect("run starts");
    loop {
        let next = tokio::time::timeout(std::time::Duration::from_secs(240), stream.next()).await;
        let Ok(Some(event)) = next else {
            return (None, None);
        };
        let event = event.expect("stream event");
        log.push(serde_json::to_value(&event).unwrap_or(Value::Null));
        if let AgentEvent::Done {
            status, session_id, ..
        } = event
        {
            return (Some(status), session_id);
        }
    }
}

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let out = std::path::PathBuf::from(args.next().expect("output directory"));
    let real_codex = args.next().unwrap_or_else(|| "codex".into());
    let model = args.next().unwrap_or_else(|| "gpt-6.1-sol".into());
    std::fs::create_dir_all(&out).unwrap();
    let wrapper = out.join("codex-tee.sh");
    std::fs::write(&wrapper, WRAPPER).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    // SAFETY: single-threaded setup before any task reads the environment.
    unsafe {
        std::env::set_var("LUNA_PROBE_OUT", &out);
        std::env::set_var("LUNA_PROBE_REAL_CODEX", &real_codex);
    }
    let workspace = tempfile::tempdir().unwrap();
    let cwd = workspace.path().to_str().unwrap().to_owned();
    let harness = CodexHarness::new().with_executable(&wrapper);
    let mut events = Vec::new();

    // Run A: no LUNA contract, so no tools, like a chat from before tools.
    let (status_a, thread) = run(
        &harness,
        request(&cwd, "Reply with exactly: READY", &model, None),
        None,
        &mut events,
    )
    .await;
    let thread = thread.expect("run A thread id");
    println!("run A: {status_a:?} thread {thread}");

    // Run B: resume that thread with a contract; the tools arrive over MCP.
    let calls = Arc::new(Mutex::new(Vec::<Value>::new()));
    let observed = Arc::clone(&calls);
    let executor: luna_harness::LunaToolExecutor = Arc::new(
        move |invocation: LunaToolInvocation| {
            let observed = Arc::clone(&observed);
            Box::pin(async move {
                observed.lock().unwrap().push(json!({
                    "tool": invocation.name,
                    "arguments": invocation.arguments,
                    "providerThreadId": invocation.context.provider_thread_id(),
                    "providerTurnId": invocation.context.provider_turn_id(),
                    "providerCallId": invocation.context.provider_call_id(),
                }));
                Ok(match invocation.name.as_str() {
                    "list_threads" => {
                        json!({"tasks":[{"taskId":"probe-sibling","projectId":"probe-project","environmentId":"probe-environment","title":"Probe sibling","status":"idle","archived":false,"updatedAtMs":1}],"nextCursor":null,"hasMore":false})
                    }
                    _ => {
                        json!({"taskId":"probe-child","parentTaskId":"luna-mcp-probe-task","command":{"status":"applied"},"initialMessageId":"child-initial:probe-child","startupState":"queued"})
                    }
                })
            })
        },
    );
    let mut resumed = request(
        &cwd,
        "Use the tools from the `luna` MCP server. First call list_threads with {\"limit\": 5}. Then call create_thread with {\"prompt\": \"probe child brief\", \"idempotencyKey\": \"luna-mcp-probe-1\"}. Then reply with exactly: DONE",
        &model,
        Some(thread.clone()),
    );
    resumed.model_options.insert(
        luna_proto::LUNA_RUNTIME_CONTRACT_OPTION.into(),
        serde_json::to_value(contract(&cwd)).unwrap(),
    );
    resumed.model_options.insert(
        luna_harness::codex::LUNA_STRICT_RESUME_OPTION.into(),
        true.into(),
    );
    let (status_b, thread_b) = run(&harness, resumed, Some(executor), &mut events).await;
    println!("run B: {status_b:?} thread {thread_b:?}");

    let archived = luna_harness::codex::native::CodexNativeClient::new(
        std::env::var_os("CODEX_HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::path::PathBuf::from(std::env::var_os("HOME").unwrap()).join(".codex")
            }),
    )
    .with_executable(&real_codex)
    .set_archived(&thread, true)
    .await;
    let calls = calls.lock().unwrap().clone();
    let summary = json!({
        "runA": {"status": format!("{status_a:?}"), "thread": thread},
        "runB": {"status": format!("{status_b:?}"), "thread": thread_b, "resumedSameThread": thread_b.as_deref() == Some(thread.as_str())},
        "executorCalls": calls,
        "probeThreadArchived": archived.is_ok(),
        "model": model,
    });
    std::fs::write(
        out.join("summary.json"),
        serde_json::to_vec_pretty(&summary).unwrap(),
    )
    .unwrap();
    std::fs::write(
        out.join("agent-events.json"),
        serde_json::to_vec_pretty(&events).unwrap(),
    )
    .unwrap();
    println!("{}", serde_json::to_string_pretty(&summary).unwrap());
    let tools = calls
        .iter()
        .filter_map(|call| call["tool"].as_str())
        .collect::<Vec<_>>();
    let ok = status_b == Some(DoneStatus::Completed)
        && thread_b.as_deref() == Some(thread.as_str())
        && tools.contains(&"list_threads")
        && tools.contains(&"create_thread");
    std::process::exit(if ok { 0 } else { 1 });
}
