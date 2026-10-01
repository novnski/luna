//! CC-05 / E28: ClaudeHarness against the deterministic 2.1.286-shaped peer in
//! `tests/fixtures/tool-results-claude.py`. Covers the failure matrix in
//! `evidence/cc05-tools/failure-cases.md` at the adapter boundary (real
//! subprocess, real stream-json decoding and normalization). No model calls.

#![cfg(unix)]

use std::path::PathBuf;
use std::time::Duration;

use futures::StreamExt;
use tokio::sync::{mpsc, oneshot};

use luna_harness::{CancellationToken, ClaudeHarness, Harness, RunControls};
use luna_proto::{AgentEvent, DoneStatus, RunRequest, SandboxLevel, ToolCall, ToolDiff};
use serde_json::{Value, json};

fn harness() -> ClaudeHarness {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("tool-results-claude.py");
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
    }
    ClaudeHarness::new().with_executable(path)
}

fn request(prompt: &str) -> RunRequest {
    RunRequest {
        goal: None,
        review: None,
        input_items: Vec::new(),
        prompt: prompt.into(),
        harness: None,
        provider_account_id: None,
        model: None,
        reasoning: None,
        model_options: serde_json::Map::new(),
        cwd: String::new(),
        sandbox: SandboxLevel::ReadOnly,
        auto_approve: false,
        permission_mode: luna_proto::PermissionMode::Ask,
        parent_permission_ceiling: None,
        environment_permission_ceiling: None,
        permission_policy: None,
        attachments: Vec::new(),
        worktree: None,
        resume: None,
    }
}

async fn run(prompt: &str) -> Vec<AgentEvent> {
    let (_steer_tx, steer_rx) = mpsc::channel(8);
    let controls = RunControls {
        request_input: Box::new(|_| {
            let (_tx, rx) = oneshot::channel();
            rx
        }),
        steering: steer_rx,
        interrupt: CancellationToken::new(),
        invoke_luna_tool: None,
    };
    let stream = harness()
        .run(request(prompt), controls)
        .await
        .expect("run starts");
    tokio::time::timeout(
        Duration::from_secs(20),
        stream.map(|r| r.expect("stream event")).collect::<Vec<_>>(),
    )
    .await
    .expect("run finished in time")
}

/// Root results in arrival order: (id, is_error, output, diff).
fn root_results(events: &[AgentEvent]) -> Vec<(&str, bool, Option<&str>, Option<&ToolDiff>)> {
    events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolResult {
                id,
                is_error,
                output,
                diff,
            } => Some((id.as_str(), *is_error, output.as_deref(), diff.as_ref())),
            _ => None,
        })
        .collect()
}

fn result_for<'a>(
    events: &'a [AgentEvent],
    wanted: &str,
) -> (bool, Option<&'a str>, Option<&'a ToolDiff>) {
    let matches: Vec<_> = root_results(events)
        .into_iter()
        .filter(|(id, ..)| *id == wanted)
        .collect();
    assert_eq!(matches.len(), 1, "exactly one root result for {wanted}");
    let (_, is_error, output, diff) = matches[0];
    (is_error, output, diff)
}

fn payload(output: Option<&str>) -> Value {
    let value: Value =
        serde_json::from_str(output.expect("output present")).expect("envelope JSON");
    let payload = value["lunaToolOutput"].clone();
    assert_eq!(payload["kind"], "native");
    assert_eq!(payload["version"], 1);
    assert_eq!(payload["provider"], "claudeCode");
    payload
}

fn types(payload: &Value) -> Vec<&str> {
    payload["content"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["type"].as_str().unwrap())
        .collect()
}

fn dedup(payload: &Value) -> Vec<&str> {
    payload["deduplicated"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}

#[tokio::test]
async fn rich_results_keep_content_arguments_diffs_and_ownership() {
    let events = run("scenario:rich-results").await;
    if let Some(dir) = std::env::var_os("LUNA_CC05_HARNESS_EVIDENCE") {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            PathBuf::from(dir).join("harness-events.json"),
            serde_json::to_vec_pretty(&events).unwrap(),
        )
        .unwrap();
    }

    // Ordering and no duplication (F4 replay, F13 child id collision).
    let order: Vec<&str> = root_results(&events).iter().map(|(id, ..)| *id).collect();
    assert_eq!(
        order,
        [
            "toolu_bash",
            "toolu_read",
            "toolu_edit",
            "toolu_staged",
            "toolu_badpatch",
            "toolu_write",
            "toolu_nb",
            "toolu_grep",
            "toolu_mcp",
            "toolu_empty",
            "toolu_cu",
            "toolu_denied",
            "toolu_future",
            "toolu_large",
            "toolu_unavailable",
            "toolu_glob_a",
            "toolu_glob_b",
            "toolu_weird",
            "toolu_agent",
            "toolu_so",
        ],
        "root results in native order, replayed Bash frame delivered once"
    );

    // F1/F9: Bash text, stderr and complete arguments; envelope echoes deduplicated.
    let (is_error, output, diff) = result_for(&events, "toolu_bash");
    assert!(!is_error && diff.is_none());
    let bash = payload(output);
    assert_eq!(bash["tool"], "Bash");
    assert_eq!(types(&bash), ["text"]);
    let text = bash["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("BASH_STDOUT_MARKER") && text.contains("BASH_STDERR_MARKER"));
    assert_eq!(bash["input"]["description"], "BASH_DESCRIPTION_ARG");
    assert_eq!(bash["input"]["timeout"], 120000);
    assert_eq!(bash["input"]["run_in_background"], false);
    assert_eq!(bash["result"]["interrupted"], false);
    assert!(bash["result"].get("stdout").is_none() && bash["result"].get("stderr").is_none());
    assert!(dedup(&bash).contains(&"stdout:content") && dedup(&bash).contains(&"stderr:content"));
    assert!(
        !output.unwrap().contains("CHILD_TAIL"),
        "child output never replaces the root's"
    );

    // F9: Read pagination arguments; file text kept once (canonical numbered block).
    let read = payload(result_for(&events, "toolu_read").1);
    assert_eq!(read["input"]["offset"], 10);
    assert_eq!(read["input"]["limit"], 2);
    assert_eq!(read["result"]["file"]["startLine"], 10);
    assert_eq!(read["result"]["file"]["totalLines"], 200);
    assert!(read["result"]["file"].get("content").is_none());
    assert_eq!(read["omitted"]["file.content"]["bytes"], 30);
    assert!(
        read["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("READ_LINE_ELEVEN")
    );

    // F8: diff only from the native structuredPatch; pre-image not duplicated.
    let (_, output, diff) = result_for(&events, "toolu_edit");
    assert_eq!(
        diff,
        Some(&ToolDiff {
            path: "/fixture/a.rs".into(),
            old_text: None,
            new_text: String::new(),
            unified_diff: Some(
                "@@ -1,3 +1,3 @@\n fn main() {\n-    before();\n+    after();\n }\n".into()
            ),
        })
    );
    let edit = payload(output);
    assert!(!output.unwrap().contains("ORIGINAL_FILE_MARKER"));
    assert_eq!(edit["omitted"]["originalFile"]["bytes"], 52);
    assert_eq!(edit["input"]["replace_all"], true);
    assert_eq!(edit["result"]["replaceAll"], true);
    for key in ["structuredPatch", "oldString", "newString", "originalFile"] {
        assert!(edit["result"].get(key).is_none(), "{key} not duplicated");
    }
    assert!(dedup(&edit).contains(&"structuredPatch:diff"));
    assert!(dedup(&edit).contains(&"oldString:input"));

    let (_, output, diff) = result_for(&events, "toolu_staged");
    assert!(diff.is_none(), "a staged edit changed nothing");
    assert_eq!(payload(output)["result"]["staged"], true);
    let (_, output, diff) = result_for(&events, "toolu_badpatch");
    assert!(diff.is_none(), "malformed native hunks never become a diff");
    assert!(payload(output)["result"]["structuredPatch"].is_array());

    let (_, output, diff) = result_for(&events, "toolu_write");
    assert_eq!(
        diff,
        Some(&ToolDiff {
            path: "/fixture/new.txt".into(),
            old_text: None,
            new_text: "CREATE_BODY_MARKER line one\nline two\n".into(),
            unified_diff: None,
        })
    );
    let write = payload(output);
    assert!(write["result"].get("content").is_none());
    assert_eq!(write["result"]["type"], "create");
    assert!(
        !output.unwrap().contains("CREATE_BODY_MARKER"),
        "Write body kept once (diff + call)"
    );

    let (_, _, diff) = result_for(&events, "toolu_nb");
    assert_eq!(
        diff,
        Some(&ToolDiff {
            path: "/fixture/n.ipynb#cell-c1".into(),
            old_text: Some("print(1)".into()),
            new_text: "print(2)".into(),
            unified_diff: None,
        })
    );

    let grep = payload(result_for(&events, "toolu_grep").1);
    for (key, value) in [
        ("glob", json!("*.rs")),
        ("output_mode", json!("content")),
        ("-n", json!(true)),
        ("-i", json!(true)),
        ("head_limit", json!(5)),
    ] {
        assert_eq!(grep["input"][key], value, "Grep argument {key}");
    }

    // F5: ordered mixed MCP content and protocol metadata survive.
    let (is_error, output, _) = result_for(&events, "toolu_mcp");
    assert!(!is_error);
    let mcp = payload(output);
    assert_eq!(types(&mcp), ["text", "image", "text", "text"]);
    assert_eq!(mcp["content"][1]["mimeType"], "image/png");
    assert!(
        mcp["content"][1]["data"]
            .as_str()
            .unwrap()
            .starts_with("iVBOR")
    );
    assert!(
        mcp["content"][2]["text"]
            .as_str()
            .unwrap()
            .contains("RESOURCE_TEXT_MARKER")
    );
    assert_eq!(mcp["structuredContent"]["marker"], "STRUCTURED_MARKER");
    assert_eq!(mcp["meta"]["fixture/trace"], "META_MARKER");
    assert_eq!(mcp["resourceLinks"][0]["uri"], "file:///fixture/report.pdf");
    assert_eq!(mcp["input"]["query"], "MCP_ARGUMENT_MARKER");
    assert!(dedup(&mcp).contains(&"content"));
    assert_eq!(
        output.unwrap().matches("MCP_TEXT_MARKER").count(),
        1,
        "MCP content kept once"
    );

    // F3: received-empty differs from unavailable.
    let empty = payload(result_for(&events, "toolu_empty").1);
    assert_eq!(empty["empty"], true);
    assert_eq!(types(&empty), Vec::<&str>::new());
    assert_eq!(result_for(&events, "toolu_unavailable").1, None);

    // F7: computer-use screenshot bytes never retained.
    let (_, output, _) = result_for(&events, "toolu_cu");
    assert!(!output.unwrap().contains("SCREENSHOT_SECRET"));
    let cu = payload(output);
    assert!(cu["content"][0]["dataOmitted"]["bytes"].as_u64().unwrap() > 0);

    // F12: error plus non-execution evidence.
    let (is_error, output, _) = result_for(&events, "toolu_denied");
    assert!(is_error);
    let denied = payload(output);
    assert_eq!(denied["nonExecution"]["kind"], "permission-rule");
    assert_eq!(denied["isError"], true);

    // F11/F19: unknown block is visible and retained, bounded.
    let future = payload(result_for(&events, "toolu_future").1);
    assert_eq!(types(&future), ["text", "unsupported"]);
    assert_eq!(future["content"][1]["nativeType"], "future_widget");
    assert_eq!(
        future["content"][1]["native"]["payload"],
        "FUTURE_BLOCK_MARKER"
    );
    assert_eq!(future["input"]["payload"], "FUTURE_ARGUMENT_MARKER");

    // F10: large output complete at the adapter.
    let (_, output, _) = result_for(&events, "toolu_large");
    let large = output.unwrap();
    assert!(large.contains("LARGE_HEAD") && large.contains("LARGE_TAIL") && large.len() > 80_000);
    assert_eq!(
        large.matches("LARGE_TAIL").count(),
        1,
        "stdout echo deduplicated"
    );

    // F14: an envelope that cannot be attributed is not applied.
    for id in ["toolu_glob_a", "toolu_glob_b"] {
        let (_, output, _) = result_for(&events, id);
        assert!(!output.unwrap().contains("MISATTRIBUTED_ENVELOPE"), "{id}");
    }
    // F18: malformed envelope shape is retained as received, no panic.
    assert_eq!(payload(result_for(&events, "toolu_weird").1)["result"], 42);

    // F2/F13: the child's colliding id is tagged to its spawn with its own body.
    let child: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Subagent {
                parent_tool_use_id,
                event,
            } if parent_tool_use_id == "toolu_agent" => match event.as_ref() {
                AgentEvent::ToolResult { id, output, .. } => Some((id.clone(), output.clone())),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(child.len(), 1);
    assert_eq!(child[0].0, "toolu_bash");
    let child_payload = payload(child[0].1.as_deref());
    assert!(
        child_payload["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("CHILD_TAIL")
    );
    assert_eq!(
        child_payload["input"]["description"],
        "CHILD_DESCRIPTION_ARG"
    );
    let agent = payload(result_for(&events, "toolu_agent").1);
    assert_eq!(agent["result"]["status"], "completed");
    assert_eq!(agent["result"]["totalTokens"], 20);
    assert!(
        agent["result"].get("content").is_none(),
        "report text kept once"
    );

    // F17: schema output is a typed part, the prose result stays prose, and the
    // matching result-frame `structured_output` adds no second part.
    let so = payload(result_for(&events, "toolu_so").1);
    assert_eq!(
        so["structuredContent"],
        json!({"verdict": "pass", "score": 3})
    );
    assert!(so.get("input").is_none());
    assert!(!events.iter().any(|e| matches!(
        e,
        AgentEvent::ToolCall { id, .. } if id.starts_with("structured-output")
    )));
    assert!(events.contains(&AgentEvent::ToolCall {
        id: "toolu_so".into(),
        call: ToolCall::Unknown {
            name: "StructuredOutput".into(),
            input: Some(json!({"verdict": "pass", "score": 3})),
        },
    }));
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Done { status: DoneStatus::Completed, result: Some(prose), .. })
            if prose == "Fixture prose result."
    ));
}

#[tokio::test]
async fn result_frame_structured_output_without_a_call_becomes_its_own_part() {
    let events = run("scenario:structured-only").await;
    let ids: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolCall { id, call } => {
                assert_eq!(
                    call,
                    &ToolCall::Unknown {
                        name: "StructuredOutput".into(),
                        input: None
                    }
                );
                Some(id.as_str())
            }
            _ => None,
        })
        .collect();
    assert_eq!(ids, ["structured-output-1"]);
    let structured = payload(result_for(&events, "structured-output-1").1);
    assert_eq!(structured["source"], "resultFrame");
    assert_eq!(
        structured["structuredContent"]["answer"],
        "SYNTHETIC_STRUCTURED_MARKER"
    );
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Done { result: Some(prose), .. }) if prose == "Fixture prose result."
    ));
}

/// Live acceptance (lead, QA time): replay frames captured from the real CLI by
/// `evidence/cc05-tools/live/live-capture.py` through the production adapter.
/// `CC05_REPLAY_FRAMES=/abs/frames.jsonl … -- --ignored live_capture`
#[tokio::test]
#[ignore = "needs CC05_REPLAY_FRAMES from a live capture"]
async fn live_capture_replay_projects_every_received_result() {
    let path = std::env::var("CC05_REPLAY_FRAMES").expect("CC05_REPLAY_FRAMES");
    let captured: Vec<Value> = std::fs::read_to_string(&path)
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    // Native tool names by id, and every result block that carried content.
    let mut names = std::collections::HashMap::new();
    let mut with_content = Vec::new();
    for frame in &captured {
        let blocks = frame["message"]["content"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        for block in blocks {
            match (frame["type"].as_str(), block["type"].as_str()) {
                (Some("assistant"), Some("tool_use")) => {
                    names.insert(
                        block["id"].as_str().unwrap().to_owned(),
                        block["name"].as_str().unwrap().to_owned(),
                    );
                }
                (Some("user"), Some("tool_result")) if !block["content"].is_null() => {
                    with_content.push(block["tool_use_id"].as_str().unwrap().to_owned());
                }
                _ => {}
            }
        }
    }
    let events = run("live replay").await;
    let mut projected = std::collections::HashMap::new();
    for event in &events {
        let leaf = match event {
            AgentEvent::Subagent { event, .. } => event.as_ref(),
            other => other,
        };
        if let AgentEvent::ToolResult {
            id, output, diff, ..
        } = leaf
        {
            projected.insert(id.clone(), (output.clone(), diff.clone()));
        }
    }
    for id in &with_content {
        let (output, _) = projected
            .get(id)
            .expect("every received result is projected");
        let body = payload(output.as_deref());
        assert_eq!(body["tool"], names[id].as_str(), "{id}");
    }
    for (id, name) in &names {
        let Some((output, diff)) = projected.get(id) else {
            continue;
        };
        match name.as_str() {
            "Edit" => assert!(
                diff.as_ref()
                    .and_then(|d| d.unified_diff.as_ref())
                    .is_some()
            ),
            "mcp__cc05-fixture__rich" => {
                let rich = payload(output.as_deref());
                assert!(types(&rich).contains(&"image"));
                assert_eq!(rich["structuredContent"]["marker"], "LIVE_STRUCTURED");
                assert_eq!(rich["resourceLinks"][0]["name"], "live-report");
            }
            _ => {}
        }
    }
    if let Some(dir) = std::env::var_os("LUNA_CC05_HARNESS_EVIDENCE") {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            PathBuf::from(dir).join("live-replay-events.json"),
            serde_json::to_vec_pretty(&events).unwrap(),
        )
        .unwrap();
    }
}
