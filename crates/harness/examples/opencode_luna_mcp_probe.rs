//! Live probe (OC-13): the REAL OpenCode 2 server attaches LUNA's REAL
//! per-process `luna` MCP endpoint through the adapter's inline
//! `OPENCODE_CONFIG_CONTENT` layer, and a real (free) model calls LUNA tools
//! through the adapter's evidence fence. Run it under an isolated HOME/XDG
//! (the evidence runner `run-probe.sh`); it never reads the user's OpenCode
//! config, storage or credentials.
//!
//!     cargo run -p luna-harness --example opencode_luna_mcp_probe -- \
//!         <model> [scenario...]
//!
//! Scenarios (default: all, in order):
//! - `fresh`: a new session calls `luna_get_workspace` once.
//! - `resume`: the `fresh` session again (tools re-attached on a new server,
//!   instruction entry re-delivered), another `luna_get_workspace` call.
//! - `subagent`: the model asks a native subagent to call the LUNA tool; the
//!   executor must not run (the child session has no LUNA rules, so the
//!   inline deny hides the tool; the fence refuses any child-session call).
//! - `direct`: an agent shell command posts `tools/call` straight to the
//!   endpoint with the inherited bearer token and no session `_meta`; the
//!   fence must refuse it.
//!
//! Every executor invocation is printed with its adapter-validated context
//! (thread = root session, turn = minted turn id, call = observed tool call).

use std::sync::{Arc, Mutex};

use futures::StreamExt;
use luna_harness::{CancellationToken, Harness, LunaToolInvocation, OpencodeHarness, RunControls};
use luna_proto::{
    AgentEvent, HarnessId, LunaAgentRuntimeContract, PermissionMode, RunRequest, RuntimeContext,
    RuntimeDelivery, RuntimeInstructionSection, RuntimeInstructionSource, RuntimeToolDefinition,
    SandboxLevel,
};
use serde_json::json;
use tokio::sync::mpsc;

fn tool(name: &str, description: &str) -> RuntimeToolDefinition {
    RuntimeToolDefinition {
        name: name.into(),
        version: 1,
        description: description.into(),
        input_schema: json!({"type":"object","properties":{
            "topic":{"type":"string","description":"Optional focus."}
        },"additionalProperties":false}),
        output_schema: json!({"type":"object"}),
        required_scopes: Vec::new(),
        deadline_ms: 30_000,
        idempotency: "read-only".into(),
        presentation: "cross-task".into(),
        activity_classification: "luna-cross-task".into(),
    }
}

fn contract(workspace: &str) -> LunaAgentRuntimeContract {
    LunaAgentRuntimeContract {
        version: 1,
        content_hash: "oc13-live-probe".into(),
        context: RuntimeContext {
            app_version: "probe".into(),
            protocol_version: 1,
            thread_id: "oc13-probe-thread".into(),
            parent_thread_id: None,
            source_thread_id: None,
            project_id: Some("oc13-probe-project".into()),
            project_name: Some("probe".into()),
            environment_id: "probe".into(),
            machine_id: "probe".into(),
            machine_name: "probe".into(),
            workspace_root: workspace.into(),
            worktree_id: None,
            binding_generation: Some(1),
            provider_profile_id: None,
            branch: None,
            harness: HarnessId::Opencode,
            model: None,
            reasoning: None,
            execution_route: "local".into(),
            granted_scopes: vec!["threads.read".into()],
            chat: None,
        },
        environment: [("LUNA_THREAD_ID".to_owned(), "oc13-probe-thread".to_owned())]
            .into_iter()
            .collect(),
        local_endpoints: Vec::new(),
        instructions: vec![
            RuntimeInstructionSection {
                name: "LUNA core".into(),
                source: RuntimeInstructionSource::LunaCore,
                provenance: "OC-13 live probe".into(),
                text: "You are running inside LUNA. LUNA tools are served by the `luna` MCP \
                       server attached to this session (tool names start with luna_)."
                    .into(),
            },
            RuntimeInstructionSection {
                name: "Side chat parent context".into(),
                source: RuntimeInstructionSource::Context,
                provenance: "probe seed (stands in for a snapshot side chat)".into(),
                text: "The following JSON contains quoted conversation context. \
                       {\"parentContextSnapshot\":\"The probe codeword is AMBER-17.\"}"
                    .into(),
            },
        ],
        tools: vec![
            tool(
                "get_workspace",
                "Discover this LUNA chat's workspace. Read-only.",
            ),
            tool("luna_status", "Read this LUNA task's identity. Read-only."),
        ],
        omitted_tools: Vec::new(),
        delivery: RuntimeDelivery::OpenCodeMcpTools,
    }
}

struct Outcome {
    session_id: Option<String>,
    status: Option<luna_proto::DoneStatus>,
    text: String,
    /// Root tool chips named `luna_*` (the evidence the fence matches).
    luna_chips: Vec<String>,
    /// Every root tool result, for the direct-call refusal text.
    tool_outputs: Vec<String>,
}

/// The fence invariant: every executor call names a root `luna_*` tool chip
/// the model made in that scenario; nothing else (a child session's call, a
/// direct HTTP call) may reach the executor.
fn unmatched(invocations: &[(String, String)], scenario: &str, outcome: &Outcome) -> Vec<String> {
    invocations
        .iter()
        .filter(|(label, call)| label == scenario && !outcome.luna_chips.contains(call))
        .map(|(_, call)| call.clone())
        .collect()
}

async fn run(
    harness: &OpencodeHarness,
    label: &str,
    model: &str,
    workspace: &str,
    prompt: &str,
    resume: Option<String>,
    calls: &Arc<Mutex<Vec<String>>>,
    call_ids: &Arc<Mutex<Vec<(String, String)>>>,
) -> Outcome {
    eprintln!("=== scenario {label} (resume={resume:?})");
    let mut model_options = serde_json::Map::new();
    model_options.insert(
        luna_proto::LUNA_RUNTIME_CONTRACT_OPTION.into(),
        serde_json::to_value(contract(workspace)).unwrap(),
    );
    let request = RunRequest {
        review: None,
        goal: None,
        input_items: Vec::new(),
        prompt: prompt.into(),
        harness: None,
        provider_account_id: None,
        model: Some(model.into()),
        reasoning: None,
        model_options,
        cwd: workspace.into(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: true,
        permission_mode: PermissionMode::Ask,
        parent_permission_ceiling: None,
        environment_permission_ceiling: None,
        permission_policy: None,
        attachments: Vec::new(),
        resume,
        worktree: None,
    };
    let (_steer_tx, steering) = mpsc::channel(8);
    let recorder = Arc::clone(calls);
    let ids = Arc::clone(call_ids);
    let label_owned = label.to_owned();
    let executor: luna_harness::LunaToolExecutor =
        Arc::new(move |invocation: LunaToolInvocation| {
            let line = format!(
                "INVOKE scenario={label_owned} tool={} thread={} turn={} call={} args={}",
                invocation.name,
                invocation.context.provider_thread_id(),
                invocation.context.provider_turn_id(),
                invocation.context.provider_call_id(),
                invocation.arguments
            );
            eprintln!("{line}");
            ids.lock().unwrap().push((
                label_owned.clone(),
                invocation.context.provider_call_id().to_owned(),
            ));
            recorder.lock().unwrap().push(line);
            let count = recorder.lock().unwrap().len();
            Box::pin(async move {
                Ok(json!({
                    "probe": format!("luna-ok-{count}"),
                    "workspace": "oc13-probe-project",
                }))
            })
        });
    let mut stream = harness
        .run(
            request,
            RunControls {
                request_input: Box::new(|_| {
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    let _ = tx.send(Vec::new());
                    rx
                }),
                steering,
                interrupt: CancellationToken::new(),
                invoke_luna_tool: Some(executor),
            },
        )
        .await
        .expect("run starts");
    let mut outcome = Outcome {
        session_id: None,
        status: None,
        text: String::new(),
        luna_chips: Vec::new(),
        tool_outputs: Vec::new(),
    };
    loop {
        let event =
            match tokio::time::timeout(std::time::Duration::from_secs(240), stream.next()).await {
                Ok(Some(event)) => event,
                Ok(None) => break,
                Err(_) => {
                    eprintln!("--- timed out");
                    break;
                }
            };
        match event {
            Ok(AgentEvent::SessionStarted { session_id, .. }) => {
                eprintln!("SESSION {session_id}");
                outcome.session_id = Some(session_id);
            }
            Ok(AgentEvent::ProviderTurnStarted { thread_id, turn_id }) => {
                eprintln!("TURN thread={thread_id} turn={turn_id}");
            }
            Ok(AgentEvent::TextDelta { text }) => outcome.text.push_str(&text),
            Ok(AgentEvent::ReasoningDelta { .. }) => {}
            Ok(AgentEvent::ToolCall { id, call, .. }) => {
                eprintln!("TOOLCALL {id} {call:?}");
                if format!("{call:?}").contains("\"luna_") {
                    outcome.luna_chips.push(id);
                }
            }
            Ok(AgentEvent::ToolResult {
                id,
                output,
                is_error,
                ..
            }) => {
                let output = output.unwrap_or_default();
                let clipped: String = output.chars().take(300).collect();
                eprintln!("TOOLRESULT {id} error={is_error} {clipped}");
                outcome.tool_outputs.push(output);
            }
            Ok(AgentEvent::ProviderNotice { notice }) => eprintln!("NOTICE {notice:?}"),
            Ok(AgentEvent::Subagent { event, .. }) => match *event {
                AgentEvent::ToolCall { id, call, .. } => eprintln!("SUB TOOLCALL {id} {call:?}"),
                AgentEvent::ToolResult {
                    id,
                    output,
                    is_error,
                    ..
                } => {
                    let clipped: String = output.unwrap_or_default().chars().take(300).collect();
                    eprintln!("SUB TOOLRESULT {id} error={is_error} {clipped}");
                }
                AgentEvent::TextDelta { .. } | AgentEvent::ReasoningDelta { .. } => {}
                other => eprintln!("SUB {other:?}"),
            },
            Ok(AgentEvent::Error { message }) => eprintln!("CHIP {message}"),
            Ok(AgentEvent::Done { status, error, .. }) => {
                eprintln!("DONE {status:?} error={error:?}");
                outcome.status = Some(status);
            }
            Ok(_) => {}
            Err(error) => eprintln!("ERR {error}"),
        }
    }
    eprintln!("TEXT {:?}", outcome.text.trim());
    outcome
}

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let model = args.next().expect("usage: <provider/model> [scenario...]");
    let mut scenarios: Vec<String> = args.collect();
    if scenarios.is_empty() {
        scenarios = ["fresh", "resume", "subagent", "direct"]
            .map(str::to_owned)
            .to_vec();
    }
    let workspace = tempfile::tempdir().expect("isolated probe workspace");
    let cwd = workspace.path().to_str().expect("UTF-8 path").to_owned();
    let harness = OpencodeHarness::new();
    // The delivery verdict is per OpenCode version and needs that version's
    // live API contract: one discovery boot records it, like the picker's.
    let models = harness.models().await.map(|models| models.len());
    eprintln!("MODELS {models:?}");
    let delivery = harness.luna_runtime_delivery();
    eprintln!("DELIVERY {delivery:?}");
    if let Some(resolution) = luna_harness::opencode::last_opencode_resolution() {
        eprintln!(
            "RESOLVED {} version={:?} source={:?}",
            resolution.path.display(),
            resolution.version,
            resolution.source
        );
    }
    let calls = Arc::new(Mutex::new(Vec::new()));
    let call_ids = Arc::new(Mutex::new(Vec::new()));
    let mut fresh_session = None;
    let mut failures = Vec::new();
    // The direct call: an agent shell process posts `tools/call` to the
    // endpoint with the inherited token and no session `_meta`.
    std::fs::write(
        workspace.path().join("selftest.py"),
        "import os, json, urllib.request\n\
         config = json.loads(os.environ['OPENCODE_CONFIG_CONTENT'])\n\
         url = config['mcp']['servers']['luna']['url']\n\
         headers = {'Authorization': 'Bearer ' + os.environ['LUNA_MCP_BEARER_TOKEN'],\n\
         \x20          'Content-Type': 'application/json'}\n\
         body = json.dumps({'jsonrpc': '2.0', 'id': 1, 'method': 'tools/call',\n\
         \x20                  'params': {'name': 'get_workspace', 'arguments': {}}}).encode()\n\
         print(urllib.request.urlopen(urllib.request.Request(url, body, headers)).read().decode())\n",
    )
    .expect("write the direct-call script");
    for scenario in &scenarios {
        let before = calls.lock().unwrap().len();
        match scenario.as_str() {
            "fresh" => {
                let outcome = run(
                    &harness,
                    "fresh",
                    &model,
                    &cwd,
                    "Call the luna_get_workspace tool exactly once (no arguments), then reply \
                     with only the value of its `probe` field.",
                    None,
                    &calls,
                    &call_ids,
                )
                .await;
                let invoked = calls.lock().unwrap().len() - before;
                if invoked != 1
                    || !outcome.text.contains("luna-ok")
                    || !unmatched(&call_ids.lock().unwrap(), "fresh", &outcome).is_empty()
                {
                    failures.push(format!("fresh: invoked={invoked} text={:?}", outcome.text));
                }
                fresh_session = outcome.session_id;
            }
            "resume" => {
                let Some(session) = fresh_session.clone() else {
                    failures.push("resume: no fresh session".into());
                    continue;
                };
                let outcome = run(
                    &harness,
                    "resume",
                    &model,
                    &cwd,
                    "Call the luna_get_workspace tool once more (no arguments). Then reply with \
                     its `probe` value and, on a second line, the probe codeword from your \
                     LUNA context.",
                    Some(session),
                    &calls,
                    &call_ids,
                )
                .await;
                let invoked = calls.lock().unwrap().len() - before;
                if invoked != 1
                    || !outcome.text.contains("luna-ok")
                    || !unmatched(&call_ids.lock().unwrap(), "resume", &outcome).is_empty()
                {
                    failures.push(format!("resume: invoked={invoked} text={:?}", outcome.text));
                }
            }
            "subagent" => {
                let outcome = run(
                    &harness,
                    "subagent",
                    &model,
                    &cwd,
                    "Do NOT call any luna_ tool yourself. Use the subagent tool with the \
                     `general` agent and this task: \"Call the luna_get_workspace tool and \
                     report exactly what happened, including any error or whether the tool \
                     is missing.\" Then reply with the subagent's report.",
                    None,
                    &calls,
                    &call_ids,
                )
                .await;
                let invoked = calls.lock().unwrap().len() - before;
                let stray = unmatched(&call_ids.lock().unwrap(), "subagent", &outcome);
                if !stray.is_empty() {
                    failures.push(format!(
                        "subagent: the executor ran for calls without a root chip {stray:?} \
                         ({invoked} invocation(s)): {:?}",
                        outcome.text
                    ));
                }
            }
            "direct" => {
                let outcome = run(
                    &harness,
                    "direct",
                    &model,
                    &cwd,
                    "Run `python3 selftest.py` in the current directory exactly once with your \
                     shell tool (do not call any luna_ tool), then reply with its output \
                     verbatim.",
                    None,
                    &calls,
                    &call_ids,
                )
                .await;
                let invoked = calls.lock().unwrap().len() - before;
                let stray = unmatched(&call_ids.lock().unwrap(), "direct", &outcome);
                let refused = outcome
                    .tool_outputs
                    .iter()
                    .any(|output| output.contains("LUNA tool is not attached"));
                eprintln!("DIRECT_REFUSED {refused} invocations={invoked}");
                if !stray.is_empty() {
                    failures.push(format!(
                        "direct: the executor ran for calls without a root chip {stray:?}"
                    ));
                }
                if !refused {
                    failures.push(format!(
                        "direct: no refused direct call observed (model text {:?})",
                        outcome.text
                    ));
                }
            }
            other => failures.push(format!("unknown scenario {other}")),
        }
    }
    eprintln!("=== invocations ({}):", calls.lock().unwrap().len());
    for line in calls.lock().unwrap().iter() {
        eprintln!("{line}");
    }
    if failures.is_empty() {
        eprintln!("=== PASS");
    } else {
        for failure in &failures {
            eprintln!("FAIL {failure}");
        }
        std::process::exit(1);
    }
}
