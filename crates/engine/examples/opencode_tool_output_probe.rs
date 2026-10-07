//! Live OC-06 probe: one OpenCode 2 turn through the native driver, with
//! every event appended to a real host `RunJournal` and folded into synced
//! document parts, then each tool's full output retrieved through the
//! journal's lazy-fetch path (`tool_output`/`tool_diff`) and rendered with
//! the shared detail-view helper (`luna_doc::native_display`).
//!
//!     opencode_tool_output_probe <opencode-exe> <provider/model> <out-dir> <prompt>
//!
//! - `PROBE_WORKSPACE=<dir>`: the run's working directory (else a temp dir).
//! - `PROBE_MARKERS=A,B,…`: strings every retrieved shell output must contain
//!   (the head and tail of a large command); the probe fails otherwise.
//! - `PROBE_INTERRUPT_AFTER_MS=<ms>`: press Stop after the delay.
//!
//! Writes into `<out-dir>`: `events.jsonl` (every event as received),
//! `journal/` (the host journal), `doc-parts.json` (the folded synced parts),
//! `tool-<n>-output.txt` / `tool-<n>-display.txt` / `tool-<n>-diff.json`
//! (journal retrievals) and `summary.json`. Exits 0 when the turn settled as
//! expected and every marker was found.

use std::collections::BTreeMap;
use std::io::Write;

use futures::StreamExt;
use luna_engine::RunJournal;
use luna_harness::{CancellationToken, Harness, OpencodeHarness, RunControls};
use luna_proto::{AgentEvent, DoneStatus, RunRequest, SandboxLevel, ToolActivityKind};
use serde_json::{Value, json};
use tokio::sync::mpsc;

const CHAT: &str = "oc06-probe";

#[derive(Default)]
struct ToolSeen {
    call: Value,
    stdout_chunks: usize,
    stdout_bytes: usize,
    progress: Vec<String>,
    result_bytes: Option<usize>,
    is_error: Option<bool>,
    diff: bool,
}

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let exe = args.next().expect("opencode executable");
    let model = args.next().expect("provider/model");
    let out = std::path::PathBuf::from(args.next().expect("output directory"));
    let prompt = args.next().expect("prompt");
    std::fs::create_dir_all(&out).expect("create output directory");
    let temp = tempfile::tempdir().expect("temp workspace");
    let workspace = std::env::var("PROBE_WORKSPACE")
        .ok()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| temp.path().to_path_buf());
    let markers: Vec<String> = std::env::var("PROBE_MARKERS")
        .ok()
        .map(|m| {
            m.split(',')
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let interrupt_after = std::env::var("PROBE_INTERRUPT_AFTER_MS")
        .ok()
        .and_then(|ms| ms.parse::<u64>().ok())
        .map(std::time::Duration::from_millis);

    let request = RunRequest {
        review: None,
        goal: None,
        input_items: Vec::new(),
        prompt,
        harness: None,
        provider_account_id: None,
        model: Some(model),
        reasoning: None,
        model_options: serde_json::Map::new(),
        cwd: workspace.to_string_lossy().into_owned(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: true,
        permission_mode: luna_proto::PermissionMode::Ask,
        parent_permission_ceiling: None,
        environment_permission_ceiling: None,
        permission_policy: None,
        attachments: Vec::new(),
        resume: None,
        worktree: None,
    };
    let interrupt = CancellationToken::new();
    if let Some(after) = interrupt_after {
        let interrupt = interrupt.clone();
        tokio::spawn(async move {
            tokio::time::sleep(after).await;
            eprintln!("--- pressing Stop");
            interrupt.cancel();
        });
    }
    let journal = RunJournal::open(out.join("journal")).expect("open journal");
    let mut events_file =
        std::fs::File::create(out.join("events.jsonl")).expect("create events.jsonl");
    let (_steer_tx, steering) = mpsc::channel(8);
    let started = std::time::Instant::now();
    let harness = OpencodeHarness::new().with_executable(exe);
    let mut stream = harness
        .run(
            request,
            RunControls {
                request_input: Box::new(|_| panic!("probe must not ask for input")),
                steering,
                interrupt: interrupt.clone(),
                invoke_luna_tool: None,
            },
        )
        .await
        .expect("start run");

    let mut parts = Vec::new();
    let mut tools: BTreeMap<String, ToolSeen> = BTreeMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut done = None;
    let mut text = String::new();
    while let Some(event) = stream.next().await {
        let event = match event {
            Ok(event) => event,
            Err(error) => {
                println!("ERR {error}");
                continue;
            }
        };
        // The engine journals every event before previews are limited, and
        // skips empty reasoning deltas (pure liveness heartbeats).
        if matches!(&event, AgentEvent::ReasoningDelta { text } if text.is_empty()) {
            continue;
        }
        journal.append(CHAT, &event).expect("journal append");
        luna_doc::fold_event_into_parts(&mut parts, &event);
        let _ = writeln!(
            events_file,
            "{}",
            serde_json::to_string(&event).unwrap_or_default()
        );
        match &event {
            AgentEvent::ToolCall { id, call } => {
                if !tools.contains_key(id) {
                    order.push(id.clone());
                }
                tools.entry(id.clone()).or_default().call =
                    serde_json::to_value(call).unwrap_or_default();
                println!(
                    "CALL {id} {}",
                    serde_json::to_string(call).unwrap_or_default()
                );
            }
            AgentEvent::ToolActivity { id, kind, text } => {
                let seen = tools.entry(id.clone()).or_default();
                match kind {
                    ToolActivityKind::Stdout | ToolActivityKind::Stdin => {
                        seen.stdout_chunks += 1;
                        seen.stdout_bytes += text.len();
                    }
                    _ => seen.progress.push(text.clone()),
                }
            }
            AgentEvent::ToolResult {
                id,
                is_error,
                output,
                diff,
            } => {
                let seen = tools.entry(id.clone()).or_default();
                seen.result_bytes = output.as_ref().map(String::len);
                seen.is_error = Some(*is_error);
                seen.diff = diff.is_some();
                println!(
                    "RESULT {id} error={is_error} output_bytes={:?} diff={}",
                    output.as_ref().map(String::len),
                    diff.is_some()
                );
            }
            AgentEvent::TextDelta { text: delta } => text.push_str(delta),
            AgentEvent::Done { status, error, .. } => {
                println!("DONE {status:?} error={error:?}");
                done = Some(*status);
            }
            AgentEvent::Error { message } => println!("ERROR {message}"),
            _ => {}
        }
    }
    println!(
        "--- stream closed after {:.1}s; text={:?}",
        started.elapsed().as_secs_f64(),
        text.chars().take(200).collect::<String>()
    );

    let mut summary_tools = Vec::new();
    let mut markers_ok = true;
    for (n, id) in order.iter().enumerate() {
        let seen = &tools[id];
        let output = journal.tool_output(CHAT, id).expect("tool_output");
        let diff = journal.tool_diff(CHAT, id).expect("tool_diff");
        let display = output.as_deref().and_then(luna_doc::native_display);
        if let Some(output) = &output {
            std::fs::write(out.join(format!("tool-{n}-output.txt")), output).ok();
        }
        if let Some(display) = &display {
            std::fs::write(out.join(format!("tool-{n}-display.txt")), display).ok();
        }
        if let Some(diff) = &diff {
            std::fs::write(out.join(format!("tool-{n}-diff.json")), diff).ok();
        }
        let is_exec = seen.call.get("kind").and_then(Value::as_str) == Some("exec");
        let missing: Vec<&String> = if is_exec {
            markers
                .iter()
                .filter(|m| !display.as_deref().unwrap_or_default().contains(m.as_str()))
                .collect()
        } else {
            Vec::new()
        };
        if is_exec && !missing.is_empty() {
            markers_ok = false;
        }
        let doc_part = parts.iter().find_map(|part| {
            let value = serde_json::to_value(part).ok()?;
            (value.get("id").and_then(Value::as_str) == Some(id.as_str())).then_some(value)
        });
        let envelope = output
            .as_deref()
            .and_then(luna_doc::native_payload)
            .map(|payload| {
                json!({
                    "tool": payload["tool"],
                    "isError": payload["isError"],
                    "empty": payload["empty"],
                    "contentTypes": payload["content"].as_array().map(|items| items
                        .iter()
                        .map(|item| item["type"].clone())
                        .collect::<Vec<_>>()),
                    "metadata": payload["metadata"],
                    "liveOutput": payload["liveOutput"],
                    "input": payload["input"],
                    "deduplicated": payload["deduplicated"],
                })
            });
        let line = json!({
            "index": n,
            "id": id,
            "call": seen.call,
            "stdoutChunks": seen.stdout_chunks,
            "stdoutBytes": seen.stdout_bytes,
            "progress": seen.progress,
            "resultOutputBytes": seen.result_bytes,
            "isError": seen.is_error,
            "resultDiff": seen.diff,
            "journalOutputBytes": output.as_ref().map(String::len),
            "journalDiffBytes": diff.as_ref().map(String::len),
            "displayBytes": display.as_ref().map(String::len),
            "displayHead": display.as_deref().map(|d| d.chars().take(240).collect::<String>()),
            "markersMissing": missing,
            "envelope": envelope,
            "docPart": doc_part.map(|mut part| {
                // The synced preview, not the (stripped) call arguments.
                if let Some(fields) = part.as_object_mut() {
                    fields.remove("call");
                }
                part
            }),
        });
        println!("TOOL {}", serde_json::to_string(&line).unwrap_or_default());
        summary_tools.push(line);
    }
    let summary = json!({
        "done": done.map(|s| format!("{s:?}")),
        "elapsedSeconds": started.elapsed().as_secs_f64(),
        "text": text,
        "markers": markers,
        "markersOk": markers_ok,
        "tools": summary_tools,
    });
    std::fs::write(
        out.join("summary.json"),
        serde_json::to_string_pretty(&summary).unwrap_or_default(),
    )
    .ok();
    std::fs::write(
        out.join("doc-parts.json"),
        serde_json::to_string_pretty(&parts).unwrap_or_default(),
    )
    .ok();
    let expected = if interrupt_after.is_some() {
        DoneStatus::Interrupted
    } else {
        DoneStatus::Completed
    };
    print_resolution();
    let ok = done == Some(expected) && markers_ok;
    println!("--- ok={ok}");
    std::process::exit(if ok { 0 } else { 1 });
}

/// The binary and API contract this process used.
fn print_resolution() {
    let Some(resolution) = luna_harness::opencode::last_opencode_resolution() else {
        println!("RESOLVED none");
        return;
    };
    let version = resolution.version.as_deref().unwrap_or("unknown");
    println!(
        "RESOLVED {} version={version} source={:?}",
        resolution.path.display(),
        resolution.source
    );
    if let Some(report) = luna_harness::opencode::opencode_compat_report(version) {
        println!("COMPAT {report:?}");
    }
}
