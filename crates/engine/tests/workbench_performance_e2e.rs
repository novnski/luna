//! Repeatable host/document/RPC measurements, kept separate from native UI timing.
//! Failure cases were written first in docs/design/harness-evolution/slice-6.md.
//! Run alone with --ignored --test-threads=1 and an absolute evidence directory.
// This fixture samples Unix process accounting; don't break Windows all-target builds.
#![cfg(unix)]

use std::{
    collections::HashMap,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use luna_doc::{
    MessagePart, MessageRole, MessageStatus, SegmentWriter, SessionMessageEntry, TranscriptUpdate,
};
use luna_engine::{
    AgentAccountsConfig, EngineCore, EngineProfile, HarnessRegistry, run_journal::RunJournal,
};
use luna_harness::mock::MockHarness;
use luna_proto::{AgentEvent, HarnessId};
use luna_rpc::RpcClient;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

// Match LUNA Dev and the production app rather than macOS system-malloc
// retention. This is specific to this measured E2E executable.
#[cfg(target_os = "macos")]
#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn cpu_seconds() -> f64 {
    // SAFETY: getrusage writes into a correctly sized initialized structure.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    assert_eq!(unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) }, 0);
    usage.ru_utime.tv_sec as f64
        + usage.ru_stime.tv_sec as f64
        + (usage.ru_utime.tv_usec + usage.ru_stime.tv_usec) as f64 / 1_000_000.0
}

fn summary(mut samples: Vec<f64>) -> Value {
    samples.sort_by(f64::total_cmp);
    let at = |p: f64| samples[((samples.len() - 1) as f64 * p).round() as usize];
    json!({"count":samples.len(),"p50Ms":at(0.5),"p95Ms":at(0.95),"maxMs":at(1.0)})
}

fn memory() -> Value {
    let Some(helper) = std::env::var_os("LUNA_PERF_STAT_HELPER") else {
        return Value::Null;
    };
    let result = std::process::Command::new(helper)
        .arg(std::process::id().to_string())
        .output()
        .unwrap();
    assert!(result.status.success());
    let rows: Vec<Value> = serde_json::from_slice(&result.stdout).unwrap();
    rows.into_iter()
        .next()
        .expect("native process memory sample")
}

async fn next(rx: &mut tokio::sync::mpsc::Receiver<Value>) -> Value {
    tokio::time::timeout(Duration::from_secs(20), rx.recv())
        .await
        .unwrap()
        .unwrap()
}

fn apply(entries: &mut Vec<SessionMessageEntry>, value: &Value) -> TranscriptUpdate {
    let update: TranscriptUpdate = serde_json::from_value(value.clone()).unwrap();
    luna_doc::transcript_delta::apply_transcript_frame(entries, update.frame.clone()).unwrap();
    update
}

fn replace_atomically(doc: &luna_doc::SessionDoc, entries: &[SessionMessageEntry]) {
    // Native rewind prepares a detached projection, then imports it. Editing a
    // live Loro list row by row lets a concurrent publisher observe construction
    // in progress and is not the production replacement boundary.
    let detached = luna_doc::SessionDoc::from_snapshot(&doc.export_snapshot().unwrap()).unwrap();
    detached
        .replace_conversation_entries(entries, &HashMap::new())
        .unwrap();
    doc.doc()
        .import(&detached.export_snapshot().unwrap())
        .unwrap();
}

fn assert_entries(actual: &[SessionMessageEntry], expected: &[SessionMessageEntry]) {
    if actual == expected {
        return;
    }
    let out = std::path::PathBuf::from(std::env::var("LUNA_PERF_EVIDENCE").unwrap());
    std::fs::write(
        out.join("failure-observed.json"),
        serde_json::to_vec(actual).unwrap(),
    )
    .unwrap();
    std::fs::write(
        out.join("failure-expected.json"),
        serde_json::to_vec(expected).unwrap(),
    )
    .unwrap();
    panic!(
        "transcript mismatch: {} actual rows, {} expected; JSON preserved in {}",
        actual.len(),
        expected.len(),
        out.display()
    );
}

fn text_entry(index: usize) -> SessionMessageEntry {
    SessionMessageEntry {
        id: format!("settled-{index:05}"),
        role: MessageRole::Assistant,
        parts: vec![MessagePart::Text {
            id: "text".into(),
            text: format!("History {index}: {}", "settled history text ".repeat(40)),
        }],
        created_at: index as i64,
        device_id: "workbench-perf".into(),
        status: Some(MessageStatus::Complete),
        continuation_of: None,
    }
}

async fn call(client: &RpcClient, method: &str, params: Value) -> Value {
    client.call_as(method, params).await.unwrap()
}

async fn setup(root: &Path) -> (EngineCore, RpcClient, std::path::PathBuf) {
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(MockHarness { script: vec![] }));
    let profile =
        EngineProfile::development(&root.join("data"), "workbench-perf", "workbench-perf");
    let journal_dir = profile.store_root().join("journals");
    let accounts = AgentAccountsConfig {
        data_dir: root.join("accounts"),
        claude_config_dir: root.join("claude"),
        claude_config_file: root.join("claude.json"),
        codex_home: root.join("codex"),
    };
    accounts.disable_claude_keychain_fallback().unwrap();
    let engine =
        EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
            profile,
            Arc::new(registry),
            HarnessId::Mock,
            None,
            accounts,
        )
        .unwrap();
    let client = luna_rpc::memory_client(engine.rpc_service());
    call(&client, "Mutate", json!({"op":"createSpace","spaceId":"perf-project","deviceId":engine.device_id,"path":root})).await;
    for task in ["perf-main", "perf-hidden"] {
        call(&client, "Mutate", json!({"op":"createChat","chatId":task,"spaceId":"perf-project","cwd":root,"config":{"harness":"mock","model":"fixture","permissionMode":"ask","sandbox":"workspace-write"}})).await;
    }
    (engine, client, journal_dir)
}

/// Failure-first serialized RPC acceptance for optional confirmed reuse.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn transcript_resume_confirms_exact_content_and_falls_back_safely() {
    let root = tempfile::tempdir().unwrap();
    let (engine, client, _) = setup(root.path()).await;
    let handle = engine.doc_host.open("perf-main").unwrap();
    let mut entries: Vec<_> = (0..1200).map(text_entry).collect();
    replace_atomically(handle.doc(), &entries);
    let mut legacy = client
        .subscribe("WatchDocMessages", json!({"chatId":"perf-main"}))
        .await
        .unwrap();
    let original = next(&mut legacy).await;
    assert!(original["reset"].is_array());
    assert!(original.get("transcriptToken").is_none());
    let mut initial = client
        .subscribe(
            "WatchDocMessages",
            json!({"chatId":"perf-main","transcriptResume":true}),
        )
        .await
        .unwrap();
    let first = next(&mut initial).await;
    let token = first["transcriptToken"]
        .as_str()
        .expect("opt-in opening token")
        .to_owned();
    assert!(!token.is_empty());
    assert_eq!(first["reset"], original["reset"]);
    let mut observed = entries.clone();
    let mut resumed = client
        .subscribe(
            "WatchDocMessages",
            json!({"chatId":"perf-main","transcriptResume":true,"knownTranscriptToken":token}),
        )
        .await
        .unwrap();
    let unchanged = next(&mut resumed).await;
    assert!(unchanged.get("reset").is_none());
    assert_eq!(unchanged["count"], entries.len());
    assert_eq!(unchanged["transcriptToken"], token);
    assert!(
        unchanged["replayBaseline"]["entries"]
            .get("settled-00000")
            .is_some()
    );
    apply(&mut observed, &unchanged);
    assert_eq!(observed, entries);
    assert!(
        serde_json::to_vec(&unchanged).unwrap().len()
            < serde_json::to_vec(&first).unwrap().len() / 5
    );

    handle
        .doc()
        .update_context_usage(Some(321), Some(4096))
        .unwrap();
    let metadata = next(&mut resumed).await;
    assert_eq!(metadata["transcriptToken"], token);
    assert_eq!(metadata["contextUsage"]["tokens"], 321);
    assert!(apply(&mut observed, &metadata).frame.is_empty_delta());
    for (task, known, opt_in) in [
        ("perf-main", "forged", true),
        ("perf-hidden", token.as_str(), true),
        ("perf-main", token.as_str(), false),
    ] {
        let mut watch = client
            .subscribe(
                "WatchDocMessages",
                json!({"chatId":task,"transcriptResume":opt_in,"knownTranscriptToken":known}),
            )
            .await
            .unwrap();
        let frame = next(&mut watch).await;
        assert!(frame["reset"].is_array());
        if !opt_in {
            assert!(frame.get("transcriptToken").is_none());
        }
    }
    // Same-id, same-length replacement must not satisfy the old token.
    if let MessagePart::Text { text, .. } = &mut entries[0].parts[0] {
        text.replace_range(..7, "Revised");
    }
    replace_atomically(handle.doc(), &entries);
    let changed = next(&mut resumed).await;
    let changed_token = changed["transcriptToken"].as_str().unwrap().to_owned();
    assert_ne!(changed_token, token);
    apply(&mut observed, &changed);
    assert_eq!(observed, entries);
    // Drop all views, then change and subscribe immediately without waiting for
    // the asynchronous publisher. Attach must reconcile pending raw changes.
    drop(legacy);
    drop(initial);
    drop(resumed);
    entries.reverse();
    entries.pop();
    entries.push(text_entry(9999));
    replace_atomically(handle.doc(), &entries);
    let mut stale = client.subscribe("WatchDocMessages",json!({"chatId":"perf-main","transcriptResume":true,"knownTranscriptToken":changed_token})).await.unwrap();
    let replacement = next(&mut stale).await;
    assert!(replacement["reset"].is_array());
    apply(&mut observed, &replacement);
    assert_eq!(observed, entries);
    let before_restart = replacement["transcriptToken"].as_str().unwrap().to_owned();
    drop(stale);
    engine.doc_host.flush_all();
    assert!(engine.shutdown().await);
    drop(handle);
    drop(client);
    drop(engine);
    let (restarted, client, _) = setup(root.path()).await;
    let mut after = client.subscribe("WatchDocMessages",json!({"chatId":"perf-main","transcriptResume":true,"knownTranscriptToken":before_restart})).await.unwrap();
    let fresh = next(&mut after).await;
    assert!(fresh["reset"].is_array());
    assert_ne!(fresh["transcriptToken"], before_restart);
    apply(&mut observed, &fresh);
    assert_eq!(observed, entries);
    if let Some(path) = std::env::var_os("LUNA_PERF_EVIDENCE") {
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(std::path::PathBuf::from(path).join("resume.json"),serde_json::to_vec_pretty(&json!({
            "passed":true,"entries":entries.len(),"legacyReset":true,"exactMatch":true,"metadataPreserved":true,
            "crossChatAndForgedReset":true,"sameLengthRewrite":true,"closedReplacement":true,"restartReset":true,
            "resetBytes":serde_json::to_vec(&first).unwrap().len(),"resumeBytes":serde_json::to_vec(&unchanged).unwrap().len()
        })).unwrap()).unwrap();
    }
    drop(after);
    assert!(restarted.shutdown().await);
}

async fn workload(root: &Path, count: usize) -> Value {
    let (engine, client, journal_dir) = setup(root).await;
    let handle = engine.doc_host.open("perf-main").unwrap();
    let hidden = engine.doc_host.open("perf-hidden").unwrap();
    let mut seeded: Vec<_> = (0..count).map(text_entry).collect();
    let journal = RunJournal::open(&journal_dir).unwrap();
    let full_output = format!("FULL_OUTPUT_MARKER\n{}", "x".repeat(256 * 1024));
    let digest = format!("{:x}", Sha256::digest(full_output.as_bytes()));
    for (index, entry) in seeded.iter_mut().take(200).enumerate() {
        let id = format!("large-output-{index}");
        journal
            .append(
                "perf-main",
                &AgentEvent::ToolResult {
                    id: id.clone(),
                    is_error: false,
                    output: Some(full_output.clone()),
                    diff: None,
                },
            )
            .unwrap();
        let part:MessagePart = serde_json::from_value(json!({"kind":"tool","id":id,"call":{"kind":"exec","command":"fixture only"},"resolved":true,"output":luna_doc::summarize_tool_output(&full_output),"outputRef":format!("perf-main/{id}"),"outputBytes":full_output.len()})).unwrap();
        entry.parts.push(part);
    }
    handle
        .doc()
        .replace_conversation_entries(&seeded, &HashMap::new())
        .unwrap();
    hidden
        .doc()
        .replace_conversation_entries(&seeded, &HashMap::new())
        .unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;

    let cold_at = Instant::now();
    let cold_cpu = cpu_seconds();
    let mut rx = client
        .subscribe("WatchDocMessages", json!({"chatId":"perf-main"}))
        .await
        .unwrap();
    let first = next(&mut rx).await;
    let cold = json!({"wallMs":cold_at.elapsed().as_secs_f64()*1000.0,"cpuMs":(cpu_seconds()-cold_cpu)*1000.0,"payloadBytes":serde_json::to_vec(&first).unwrap().len()});
    let mut observed = Vec::new();
    apply(&mut observed, &first);
    assert_eq!(observed.len(), count);
    assert!(
        !serde_json::to_string(&first)
            .unwrap()
            .contains(&"x".repeat(1024))
    );
    let blob = call(
        &client,
        "FetchToolBlob",
        json!({"blobRef":"perf-main/large-output-199"}),
    )
    .await;
    assert_eq!(blob["text"].as_str().unwrap(), full_output);

    let inspection = handle.watch_messages();
    let mut last_projection = inspection.borrow().entries.clone();
    let mut reused_projections = 0;
    let memory_before = memory();
    let mut latencies = Vec::new();
    let mut payload_bytes = 0usize;
    let phase_cpu = cpu_seconds();
    let phase_at = Instant::now();
    for index in 1..=100u64 {
        let start = Instant::now();
        handle
            .doc()
            .update_context_usage(Some(index), Some(200_000))
            .unwrap();
        let frame = next(&mut rx).await;
        let current_projection = inspection.borrow().entries.clone();
        reused_projections += usize::from(Arc::ptr_eq(&last_projection, &current_projection));
        last_projection = current_projection;
        latencies.push(start.elapsed().as_secs_f64() * 1000.0);
        payload_bytes += serde_json::to_vec(&frame).unwrap().len();
        let update = apply(&mut observed, &frame);
        assert!(
            update.frame.is_empty_delta(),
            "metadata must not rewrite transcript rows"
        );
        assert_eq!(update.context_usage.unwrap().tokens, Some(index));
    }
    let metadata = json!({"timing":summary(latencies),"wallMs":phase_at.elapsed().as_secs_f64()*1000.0,"cpuMs":(cpu_seconds()-phase_cpu)*1000.0,"payloadBytes":payload_bytes,"reusedProjections":reused_projections});
    if std::env::var_os("LUNA_PERF_EXPECT_REUSE").is_some() {
        assert_eq!(
            reused_projections, 100,
            "metadata-only updates reuse settled transcript contents"
        );
    }
    drop(inspection);
    drop(last_projection);

    // A visible stream changes one text part while another long chat is hidden.
    let mut visible_writer =
        SegmentWriter::begin(handle.doc(), "visible-live", "host", 9999).unwrap();
    let mut hidden_writer =
        SegmentWriter::begin(hidden.doc(), "hidden-live", "host", 9999).unwrap();
    loop {
        let value = next(&mut rx).await;
        apply(&mut observed, &value);
        if observed.iter().any(|e| e.id == "visible-live") {
            break;
        }
    }
    let phase_cpu = cpu_seconds();
    let phase_at = Instant::now();
    let mut latencies = Vec::new();
    let mut stream_bytes = 0usize;
    for index in 1..=60 {
        let start = Instant::now();
        hidden_writer
            .sync(&[MessagePart::Text {
                id: "text".into(),
                text: "hidden ".repeat(index),
            }])
            .unwrap();
        visible_writer
            .sync(&[MessagePart::Text {
                id: "text".into(),
                text: "visible ".repeat(index),
            }])
            .unwrap();
        let value = next(&mut rx).await;
        latencies.push(start.elapsed().as_secs_f64() * 1000.0);
        stream_bytes += serde_json::to_vec(&value).unwrap().len();
        apply(&mut observed, &value);
        assert_eq!(
            observed.last().unwrap().parts[0],
            MessagePart::Text {
                id: "text".into(),
                text: "visible ".repeat(index)
            }
        );
    }
    let streaming = json!({"timing":summary(latencies),"wallMs":phase_at.elapsed().as_secs_f64()*1000.0,"cpuMs":(cpu_seconds()-phase_cpu)*1000.0,"payloadBytes":stream_bytes});
    drop(visible_writer);
    drop(hidden_writer);

    // Same ids and lengths, including an old tool summary, must replace contents.
    let mut rewritten = observed.clone();
    let MessagePart::Text { text, .. } = &mut rewritten[400].parts[0] else {
        panic!()
    };
    *text = "R".repeat(text.len());
    let MessagePart::Tool { output, .. } = &mut rewritten[0].parts[1] else {
        panic!()
    };
    *output = Some("S".repeat(output.as_ref().unwrap().len()));
    replace_atomically(handle.doc(), &rewritten);
    let value = next(&mut rx).await;
    apply(&mut observed, &value);
    assert_entries(&observed, &rewritten);
    drop(rx);

    let mut reopen = Vec::new();
    let mut reopen_bytes = 0usize;
    let mut navigation_memory = vec![json!({"reopens":0,"memory":memory()})];
    for index in 0..20 {
        tokio::time::sleep(Duration::from_millis(15)).await;
        let start = Instant::now();
        let mut reopened = client
            .subscribe("WatchDocMessages", json!({"chatId":"perf-main"}))
            .await
            .unwrap();
        let value = next(&mut reopened).await;
        reopen.push(start.elapsed().as_secs_f64() * 1000.0);
        reopen_bytes += serde_json::to_vec(&value).unwrap().len();
        let mut rows = Vec::new();
        apply(&mut rows, &value);
        assert_entries(&rows, &rewritten);
        drop(reopened);
        if matches!(index, 4 | 9 | 19) {
            navigation_memory.push(json!({"reopens":index+1,"memory":memory()}));
        }
    }
    // Reopen timing above keeps the full long history. Check rewind separately
    // afterwards, so its shorter transcript cannot improve the navigation score.
    let mut rewind_rx = client
        .subscribe("WatchDocMessages", json!({"chatId":"perf-main"}))
        .await
        .unwrap();
    apply(&mut observed, &next(&mut rewind_rx).await);
    rewritten.truncate(500);
    replace_atomically(handle.doc(), &rewritten);
    apply(&mut observed, &next(&mut rewind_rx).await);
    assert_entries(&observed, &rewritten);
    drop(rewind_rx);
    // Host-local full output remains byte-identical after projection rewrites.
    let blob = call(
        &client,
        "FetchToolBlob",
        json!({"blobRef":"perf-main/large-output-199"}),
    )
    .await;
    assert_eq!(
        format!(
            "{:x}",
            Sha256::digest(blob["text"].as_str().unwrap().as_bytes())
        ),
        digest
    );
    let result = json!({"historyEntries":count,"toolOutputs":200,"durableFullOutputBytes":200*full_output.len(),"fullOutputSha256":digest,"cold":cold,"metadataUpdates":metadata,"streamingWithHiddenChat":streaming,"reopenAfterRewrite":{"timing":summary(reopen),"payloadBytes":reopen_bytes,"memory":navigation_memory},"memoryBefore":memory_before,"memoryAfter":memory(),"sameIdSameLengthTextAndToolRewrite":true,"rewindNoStaleSuffix":true});
    drop(client);
    drop(handle);
    drop(hidden);
    drop(journal);
    assert!(engine.shutdown().await);
    result
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "explicit isolated performance run; not a routine timing assertion"]
async fn host_history_wire_and_rewrite_measurements() {
    let output = std::path::PathBuf::from(
        std::env::var("LUNA_PERF_EVIDENCE").expect("absolute evidence directory"),
    );
    assert!(output.is_absolute());
    std::fs::create_dir_all(&output).unwrap();
    let mut workloads = Vec::new();
    for count in [1200, 4200] {
        let root = tempfile::tempdir().unwrap();
        workloads.push(workload(root.path(), count).await);
    }
    let artifact = json!({"passed":true,"pid":std::process::id(),"profile":"dev","measurement":"host plus in-process serialized RPC client; no GUI or live provider","workloads":workloads});
    std::fs::write(
        output.join("host.json"),
        serde_json::to_vec_pretty(&artifact).unwrap(),
    )
    .unwrap();
    println!("{}", serde_json::to_string(&artifact).unwrap());
}

async fn agrees_with_document(
    rx: &mut tokio::sync::mpsc::Receiver<Value>,
    observed: &mut Vec<SessionMessageEntry>,
    doc: &luna_doc::SessionDoc,
) -> Value {
    let expected = luna_doc::join_continuation_entries(doc.read_entries().unwrap());
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let value = next(rx).await;
            apply(observed, &value);
            if *observed == expected {
                return value;
            }
        }
    })
    .await
    .expect("published projection converges to the complete document")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn changed_rows_structural_rewrites_and_context_match_durable_history() {
    let root = tempfile::tempdir().unwrap();
    let (engine, client, _) = setup(root.path()).await;
    let handle = engine.doc_host.open("perf-main").unwrap();
    let mut rx = client
        .subscribe("WatchDocMessages", json!({"chatId":"perf-main"}))
        .await
        .unwrap();
    let mut observed = Vec::new();
    apply(&mut observed, &next(&mut rx).await);
    replace_atomically(handle.doc(), &(0..8).map(text_entry).collect::<Vec<_>>());
    agrees_with_document(&mut rx, &mut observed, handle.doc()).await;
    let mut trace = Vec::new();
    {
        let mut a = SegmentWriter::begin(handle.doc(), "live-a", "fixture", 100).unwrap();
        let mut b = SegmentWriter::begin(handle.doc(), "live-b", "fixture", 101).unwrap();
        let tool = |label: &str| {
            serde_json::from_value::<MessagePart>(json!({"kind":"tool","id":"nested","call":{"kind":"exec","command":"fixture"},"resolved":true,"output":label,"subagentActivity":{"summary":label}})).unwrap()
        };
        a.sync(&[
            MessagePart::Text {
                id: "text".into(),
                text: "AAAA".into(),
            },
            tool("first"),
        ])
        .unwrap();
        b.sync(&[MessagePart::Text {
            id: "text".into(),
            text: "BBBB".into(),
        }])
        .unwrap();
        agrees_with_document(&mut rx, &mut observed, handle.doc()).await;
        a.sync(&[
            MessagePart::Text {
                id: "text".into(),
                text: "CCCC".into(),
            },
            tool("other"),
        ])
        .unwrap();
        b.sync(&[MessagePart::Text {
            id: "text".into(),
            text: "DDDD".into(),
        }])
        .unwrap();
        agrees_with_document(&mut rx, &mut observed, handle.doc()).await;
        trace.push("coalesced same-length text, tool output and nested activity rewrites");
        a.finish(
            &[
                MessagePart::Text {
                    id: "text".into(),
                    text: "CCCC".into(),
                },
                tool("other"),
            ],
            MessageStatus::Complete,
        )
        .unwrap();
        b.finish(
            &[MessagePart::Text {
                id: "text".into(),
                text: "DDDD".into(),
            }],
            MessageStatus::Complete,
        )
        .unwrap();
        agrees_with_document(&mut rx, &mut observed, handle.doc()).await;
    }
    let mut reordered = observed.clone();
    reordered.reverse();
    reordered.remove(4);
    replace_atomically(handle.doc(), &reordered);
    agrees_with_document(&mut rx, &mut observed, handle.doc()).await;
    trace.push("reordered and removed entries after whole-document import");
    replace_atomically(handle.doc(), &[text_entry(0)]);
    agrees_with_document(&mut rx, &mut observed, handle.doc()).await;
    {
        let mut continuation =
            SegmentWriter::begin(handle.doc(), "continuation", "fixture", 200).unwrap();
        let Some(loro::ValueOrContainer::Container(loro::Container::Map(map))) =
            handle.doc().doc().get_list("messages").get(1)
        else {
            panic!("continuation map")
        };
        map.insert("continuationOf", "settled-00000").unwrap();
        handle.doc().doc().commit();
        continuation
            .sync(&[MessagePart::Text {
                id: "continuation-text".into(),
                text: "AAAA".into(),
            }])
            .unwrap();
        agrees_with_document(&mut rx, &mut observed, handle.doc()).await;
        assert_eq!(observed.len(), 1);
        continuation
            .sync(&[MessagePart::Text {
                id: "continuation-text".into(),
                text: "BBBB".into(),
            }])
            .unwrap();
        agrees_with_document(&mut rx, &mut observed, handle.doc()).await;
        assert_eq!(observed.len(), 1);
    }
    trace.push("continuation slots join into one current visible row");
    let messages = handle.doc().doc().get_list("messages");
    messages.push("unsalvageable primitive slot").unwrap();
    handle.doc().doc().commit();
    let mut after_bad = SegmentWriter::begin(handle.doc(), "after-bad", "fixture", 300).unwrap();
    after_bad
        .sync(&[MessagePart::Text {
            id: "text".into(),
            text: "AAAA".into(),
        }])
        .unwrap();
    agrees_with_document(&mut rx, &mut observed, handle.doc()).await;
    after_bad
        .sync(&[MessagePart::Text {
            id: "text".into(),
            text: "BBBB".into(),
        }])
        .unwrap();
    agrees_with_document(&mut rx, &mut observed, handle.doc()).await;
    drop(after_bad);
    trace.push("malformed raw slot does not shift the visible-row identity");
    let body = observed.clone();
    let context = luna_proto::ContextControlState {
        notice: Some("fixture context boundary".into()),
        ..Default::default()
    };
    handle.doc().set_context_control(&context).unwrap();
    loop {
        let value = next(&mut rx).await;
        let update = apply(&mut observed, &value);
        if update.context_control == context {
            break;
        }
    }
    assert_entries(&observed, &body);
    trace.push("context state updates without corrupting transcript contents");
    drop(rx);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let closed = vec![text_entry(900)];
    replace_atomically(handle.doc(), &closed);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let mut reopened = client
        .subscribe("WatchDocMessages", json!({"chatId":"perf-main"}))
        .await
        .unwrap();
    let mut current = Vec::new();
    apply(&mut current, &next(&mut reopened).await);
    assert_entries(&current, &closed);
    trace.push("closed-view history replacement is current on reopen");
    if let Ok(output) = std::env::var("LUNA_PERF_EVIDENCE") {
        let output = std::path::PathBuf::from(output);
        assert!(output.is_absolute());
        std::fs::create_dir_all(&output).unwrap();
        std::fs::write(
            output.join("projection-correctness.json"),
            serde_json::to_vec_pretty(
                &json!({"passed":true,"scenarios":trace,"finalRows":current}),
            )
            .unwrap(),
        )
        .unwrap();
    }
    drop(reopened);
    drop(client);
    drop(handle);
    assert!(engine.shutdown().await);
}
