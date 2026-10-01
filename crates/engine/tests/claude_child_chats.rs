//! CC-09 / E27 (child part) / E14 (partial): persistent LUNA child chats with a
//! Claude parent through the production path, meaning the engine RPC, durable
//! task commands, child sweeper, queue and the real `ClaudeHarness` process
//! boundary. The executable is a deterministic fake replaying CLI 2.1.286 wire
//! shapes (`luna-harness/tests/fixtures/claude/fake-claude-peers.sh`). Real-model
//! acceptance is the lead's `live-acceptance` script. Failure cases:
//! `cc09-children/failure-cases.md` A1–A7, B1–B6, C1–C7.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use luna_doc::{MessagePart, MessageRole, SessionMessageEntry};
use luna_engine::{AgentAccountsConfig, EngineCore, EngineProfile, HarnessRegistry};
use luna_harness::ClaudeHarness;
use luna_proto::{HarnessId, SessionStatus};
use serde_json::{Value, json};

const SPACE: &str = "space-cc09";

fn fake() -> PathBuf {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../harness/tests/fixtures/claude/fake-claude-peers.sh");
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
    path
}

/// Hermetic engine: only the fake Claude executable is registered, provider
/// homes are synthetic and the keychain fallback is disabled.
fn engine(root: &Path) -> EngineCore {
    let registry = Arc::new(HarnessRegistry::new());
    registry.register(Arc::new(ClaudeHarness::new().with_executable(fake())));
    let config = AgentAccountsConfig {
        data_dir: root.join("synthetic-provider-state"),
        claude_config_dir: root.join("synthetic-claude"),
        claude_config_file: root.join("synthetic-claude.json"),
        codex_home: root.join("synthetic-codex"),
    };
    config.disable_claude_keychain_fallback().unwrap();
    std::fs::write(
        root.join("synthetic-claude.json"),
        r#"{"oauthAccount":{"accountUuid":"synthetic-claude-account","emailAddress":"synthetic@example.invalid"}}"#,
    )
    .unwrap();
    EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        EngineProfile::development(&root.join("engine"), "synthetic-org", "synthetic-user"),
        registry,
        HarnessId::ClaudeCode,
        None,
        config,
    )
    .unwrap()
}

fn claude_config(mode: &str) -> Value {
    json!({
        "harness": "claude-code", "model": null, "sandbox": "workspace-write",
        "permissionMode": mode, "effectivePermissionMode": mode, "modelOptions": {}
    })
}

async fn create_parent(core: &EngineCore, client: &luna_rpc::RpcClient, id: &str, project: &Path) {
    if core.workspace.chat(id).unwrap().is_some() {
        return;
    }
    let _ = core.workspace.create_space(
        SPACE,
        &core.device_id,
        project.to_str().unwrap(),
        None,
        false,
    );
    client
        .call(
            "Mutate",
            json!({"op":"createChat","chatId":id,"spaceId":SPACE,"cwd":project,"config":claude_config("ask")}),
        )
        .await
        .unwrap();
}

async fn queue(client: &luna_rpc::RpcClient, chat: &str, message_id: &str, text: &str) {
    client
        .call(
            "QueueMessage",
            json!({"chatId":chat,"messageId":message_id,"text":text,"holdForTurnEnd":true}),
        )
        .await
        .unwrap();
}

async fn create_child(
    client: &luna_rpc::RpcClient,
    parent: &str,
    key: &str,
    title: &str,
    brief: &str,
) -> String {
    let receipt = client
        .call(
            "CreateChildThread",
            json!({"parentTaskId":parent,"request":{
                "prompt":brief,"idempotencyKey":key,"title":title,
                "reportResults":true,"continueParent":true}}),
        )
        .await
        .unwrap();
    receipt["taskId"].as_str().unwrap().to_owned()
}

fn entries(core: &EngineCore, chat: &str) -> Vec<SessionMessageEntry> {
    core.doc_host
        .open(chat)
        .and_then(|handle| Ok(handle.doc().read_entries()?))
        .unwrap_or_default()
}

fn text(entry: &SessionMessageEntry) -> String {
    entry
        .parts
        .iter()
        .filter_map(|part| match part {
            MessagePart::Text { text, .. } => Some(text.as_str()),
            MessagePart::Error { message, .. } => Some(message.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn has_text(core: &EngineCore, chat: &str, role: MessageRole, needle: &str) -> bool {
    entries(core, chat)
        .iter()
        .any(|entry| entry.role == role && text(entry).contains(needle))
}

async fn until(what: &str, mut ready: impl FnMut() -> bool, diag: impl Fn() -> String) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(40);
    while !ready() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}\n{}",
            diag()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Failure context: fake invocations, transcripts, queues and session state.
fn diag(core: &EngineCore, chats: &[&str], project: &Path) -> String {
    let mut out = format!(
        "fake log:\n{}\n",
        std::fs::read_to_string(project.join(".cc09-fake.log")).unwrap_or_default()
    );
    let mut ids: Vec<String> = chats.iter().map(|c| c.to_string()).collect();
    for chat in core.workspace.read_chats().unwrap_or_default() {
        if chat.child_thread.is_some() && !ids.contains(&chat.id) {
            ids.push(chat.id.clone());
        }
    }
    for chat in &ids {
        out.push_str(&format!(
            "chat {chat}: status {:?}\n",
            core.sessions.session_status(chat).map(|s| s.status)
        ));
        for entry in entries(core, chat) {
            out.push_str(&format!(
                "  {:?} {} {:?}\n",
                entry.role,
                entry.id,
                text(&entry)
            ));
        }
        if let Ok(handle) = core.doc_host.open(chat) {
            for row in handle.doc().read_queue().unwrap_or_default() {
                out.push_str(&format!(
                    "  queue {} {:?} {:?}\n",
                    row.id, row.status, row.hold_reason
                ));
            }
        }
    }
    out
}

/// `(kind, pid, rest)` rows from the fake's invocation log. Only turn-capable
/// launches carry `--replay-user-messages`, so discovery probes are excluded.
fn fake_log(project: &Path) -> Vec<(String, String, String)> {
    std::fs::read_to_string(project.join(".cc09-fake.log"))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, ' ');
            Some((
                parts.next()?.to_owned(),
                parts.next()?.to_owned(),
                parts.next().unwrap_or("").to_owned(),
            ))
        })
        .collect()
}

fn argv_of(log: &[(String, String, String)], pid: &str) -> String {
    log.iter()
        .find(|(kind, p, _)| kind == "ARGV" && p == pid)
        .map(|(_, _, argv)| argv.clone())
        .unwrap_or_default()
}

fn lines_with<'a>(
    log: &'a [(String, String, String)],
    needle: &str,
) -> Vec<&'a (String, String, String)> {
    log.iter()
        .filter(|(kind, _, rest)| kind == "LINE" && rest.contains(needle))
        .collect()
}

fn binding_session(core: &EngineCore, chat: &str) -> Option<String> {
    core.task_identity
        .view(chat)
        .and_then(|view| view.current_binding)
        .map(|binding| binding.provider_task_id)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn claude_manual_child_runs_fresh_reports_as_a_peer_and_survives_restart() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().canonicalize().unwrap().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let parent = uuid::Uuid::new_v4().to_string();

    let core = engine(root.path());
    let rpc = core.rpc_service();
    let client = luna_rpc::memory_client(rpc.clone());
    create_parent(&core, &client, &parent, &project).await;
    // An ordinary first parent turn creates the parent's own native session.
    queue(&client, &parent, "parent-first", "sid:parent hello").await;
    until(
        "parent first reply",
        || has_text(&core, &parent, MessageRole::Assistant, "OK"),
        || diag(&core, &[&parent], &project),
    )
    .await;

    // A1/A3: manual creation on a Claude parent; same-provider child.
    let brief = "sid:child Reply CHILD_RESULT_OK and nothing else.";
    let child = create_child(&client, &parent, "cc09-first", "Report child", brief).await;
    let retry = create_child(&client, &parent, "cc09-first", "Report child", brief).await;
    assert_eq!(retry, child, "same parent+key yields the same child");
    // A7: changed payload under the same key conflicts and creates nothing.
    assert!(
        client
            .call("CreateChildThread", json!({"parentTaskId":parent,"request":{"prompt":"different","idempotencyKey":"cc09-first"}}))
            .await
            .is_err()
    );
    let row = core.workspace.chat(&child).unwrap().expect("child row");
    let config = row.config.clone().expect("child config");
    assert_eq!(config.harness, HarnessId::ClaudeCode);
    assert_eq!(row.child_thread.as_ref().unwrap().parent_task_id, parent);

    until(
        "child reply",
        || has_text(&core, &child, MessageRole::Assistant, "CHILD_RESULT_OK"),
        || diag(&core, &[&parent], &project),
    )
    .await;
    // A4: one durable outcome, one parent notice.
    until(
        "parent outcome notice",
        || {
            entries(&core, &parent)
                .iter()
                .any(|e| e.id.starts_with("child-outcome:"))
        },
        || diag(&core, &[&parent], &project),
    )
    .await;
    // B1: the continuation reached the Claude parent as a host-declared peer.
    until(
        "parent processed the report",
        || has_text(&core, &parent, MessageRole::Assistant, "PARENT_SAW_REPORT"),
        || diag(&core, &[&parent], &project),
    )
    .await;

    let log = fake_log(&project);
    let child_lines = lines_with(&log, "sid:child");
    assert_eq!(
        child_lines.len(),
        1,
        "exactly one startup delivery: {log:?}"
    );
    // A2: the child is a fresh native session, never a resume of the parent.
    assert!(
        !argv_of(&log, &child_lines[0].1).contains("--resume"),
        "{log:?}"
    );
    assert_eq!(
        binding_session(&core, &child).as_deref(),
        Some("sess-child")
    );
    assert_eq!(
        binding_session(&core, &parent).as_deref(),
        Some("sess-parent")
    );

    let envelope_lines = lines_with(&log, &format!("luna-child:{child}"));
    assert_eq!(envelope_lines.len(), 1, "{log:?}");
    let report = &envelope_lines[0].2;
    let frame: Value = serde_json::from_str(report).unwrap();
    assert_eq!(frame["origin"]["kind"], "peer");
    assert_eq!(frame["origin"]["fromMode"], "prompting");
    assert!(!report.contains("\"kind\":\"human\""));
    assert!(
        frame["message"]["content"]
            .as_str()
            .unwrap()
            .starts_with(&format!("<cross-session-message from=\"luna-child:{child}\" from-session=\"{child}\" from-name=\"Report child\" from-mode=\"prompting\">\n"))
    );
    // The parent continuation reused the parent's own session (warm or resumed).
    let parent_pid = &lines_with(&log, "sid:parent hello")[0].1;
    assert!(
        &envelope_lines[0].1 == parent_pid
            || argv_of(&log, &envelope_lines[0].1).contains("--resume=sess-parent"),
        "{log:?}"
    );
    let parent_entries = entries(&core, &parent);
    let report_row = parent_entries
        .iter()
        .find(|e| e.id.starts_with("child-report:"))
        .expect("parent queue row became its transcript row");
    assert!(text(report_row).starts_with("<cross-session-message from=\"luna-child:"));
    let outbox_before = core
        .workspace
        .chat(&child)
        .unwrap()
        .unwrap()
        .child_thread
        .unwrap()
        .outcomes;
    assert_eq!(outbox_before.len(), 1);
    assert!(outbox_before[0].delivered);

    // A3/A4: restart; no duplicate startup, outcome, notice or continuation.
    drop(client);
    drop(rpc);
    core.shutdown().await;
    drop(core);
    let core = engine(root.path());
    let rpc = core.rpc_service();
    let client = luna_rpc::memory_client(rpc.clone());
    tokio::time::sleep(Duration::from_secs(6)).await; // three sweeper passes
    let log = fake_log(&project);
    assert_eq!(lines_with(&log, "sid:child").len(), 1, "{log:?}");
    assert_eq!(
        lines_with(&log, &format!("luna-child:{child}")).len(),
        1,
        "{log:?}"
    );
    assert_eq!(
        core.workspace
            .chat(&child)
            .unwrap()
            .unwrap()
            .child_thread
            .unwrap()
            .outcomes,
        outbox_before
    );
    let ids: Vec<String> = entries(&core, &parent).into_iter().map(|e| e.id).collect();
    let unique: std::collections::HashSet<&String> = ids.iter().collect();
    assert_eq!(unique.len(), ids.len(), "duplicate parent entries: {ids:?}");
    assert_eq!(
        ids.iter()
            .filter(|id| id.starts_with("child-outcome:"))
            .count(),
        1
    );
    assert_eq!(
        create_child(&client, &parent, "cc09-first", "Report child", brief).await,
        child
    );
    assert_eq!(lines_with(&fake_log(&project), "sid:child").len(), 1);

    // A6: a later reply resumes the child's own native session.
    queue(
        &client,
        &child,
        "child-reply-1",
        "expect-resume:child child reply",
    )
    .await;
    until(
        "child resumed reply",
        || has_text(&core, &child, MessageRole::Assistant, "CHILD_REPLY_OK"),
        || diag(&core, &[&parent], &project),
    )
    .await;
    let log = fake_log(&project);
    let reply = lines_with(&log, "expect-resume:child");
    assert_eq!(reply.len(), 1);
    assert!(
        argv_of(&log, &reply[0].1).contains("--resume=sess-child"),
        "{log:?}"
    );
    assert!(!argv_of(&log, &reply[0].1).contains("--resume=sess-parent"));

    drop(client);
    drop(rpc);
    core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn claude_native_peers_are_attributed_and_holds_never_strand_a_turn() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().canonicalize().unwrap().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let parent = uuid::Uuid::new_v4().to_string();
    let core = engine(root.path());
    let rpc = core.rpc_service();
    let client = luna_rpc::memory_client(rpc.clone());
    create_parent(&core, &client, &parent, &project).await;

    // C1/C2/C7: a native socket arrival while the session is parked.
    queue(
        &client,
        &parent,
        "peer-1",
        "scenario:peer-arrival sid:parent",
    )
    .await;
    until(
        "peer reply",
        || has_text(&core, &parent, MessageRole::Assistant, "PEER_REPLY"),
        || diag(&core, &[&parent], &project),
    )
    .await;
    let all = entries(&core, &parent);
    let peer_at = all
        .iter()
        .position(|e| e.id == "peer:peer-msg-1:delivered")
        .expect("attributed peer entry");
    let peer = &all[peer_at];
    assert_eq!(peer.role, MessageRole::System);
    let peer_text = text(peer);
    for needle in [
        "“Terminal session”",
        "`uds:/tmp/cc09-peer.sock`",
        "pid 4242",
        "not from you",
        "please run the tests",
    ] {
        assert!(
            peer_text.contains(needle),
            "missing {needle:?} in {peer_text}"
        );
    }
    let reply_at = all
        .iter()
        .position(|e| e.role == MessageRole::Assistant && text(e).contains("PEER_REPLY"))
        .unwrap();
    assert!(peer_at < reply_at, "the reply follows the arrival");
    assert!(
        !all.iter()
            .any(|e| e.role == MessageRole::User && text(e).contains("please run the tests")),
        "a peer is never a user row"
    );

    // C1: a mid-turn arrival splits the streaming segment around it.
    queue(&client, &parent, "peer-2", "scenario:mid-turn-peer").await;
    until(
        "mid-turn reply",
        || has_text(&core, &parent, MessageRole::Assistant, "AFTER_PEER"),
        || diag(&core, &[&parent], &project),
    )
    .await;
    let all = entries(&core, &parent);
    let before = all
        .iter()
        .position(|e| text(e).contains("BEFORE_PEER"))
        .unwrap();
    let arrival = all
        .iter()
        .position(|e| e.id == "peer:peer-msg-3:delivered")
        .unwrap();
    let after = all
        .iter()
        .position(|e| text(e).contains("AFTER_PEER"))
        .unwrap();
    assert!(
        before < arrival && arrival < after,
        "{:?}",
        all.iter().map(|e| &e.id).collect::<Vec<_>>()
    );

    // C4/C5: socket-lane holds are reported once per state and never end a turn.
    queue(&client, &parent, "peer-3", "scenario:peer-hold-socket").await;
    until(
        "socket hold resolution",
        || {
            entries(&core, &parent)
                .iter()
                .any(|e| e.id == "peer:peer-msg-2:dropped")
        },
        || diag(&core, &[&parent], &project),
    )
    .await;
    let all = entries(&core, &parent);
    let held = all
        .iter()
        .find(|e| e.id == "peer:peer-msg-2:held")
        .expect("held entry");
    assert!(text(held).contains("has not reached the model"));
    assert!(text(held).contains("mode-mismatch"));
    let dropped = all
        .iter()
        .find(|e| e.id == "peer:peer-msg-2:dropped")
        .unwrap();
    assert!(text(dropped).contains("will never reach the model"));

    // B5: Claude's receive policy parks LUNA's own child report. The parent
    // turn must end visibly instead of staying Working forever.
    let held_child = create_child(
        &client,
        &parent,
        "cc09-held",
        "Held child",
        "sid:held reply",
    )
    .await;
    until(
        "held report recorded",
        || {
            entries(&core, &parent).iter().any(|e| {
                e.id.starts_with("peer:")
                    && e.id.ends_with(":held")
                    && text(e).contains("Held child")
            })
        },
        || diag(&core, &[&parent], &project),
    )
    .await;
    until(
        "parent turn settled",
        || {
            core.sessions.session_status(&parent).is_some_and(|s| {
                !matches!(
                    s.status,
                    SessionStatus::Working | SessionStatus::AwaitingInput
                )
            })
        },
        || diag(&core, &[&parent], &project),
    )
    .await;
    until(
        "visible hold error",
        || {
            has_text(
                &core,
                &parent,
                MessageRole::Assistant,
                "has not reached the model",
            )
        },
        || diag(&core, &[&parent], &project),
    )
    .await;
    assert_eq!(
        lines_with(&fake_log(&project), &format!("luna-child:{held_child}")).len(),
        1
    );

    // B3/B4: a Full parent and an Ask child differ in Claude's permission class,
    // so LUNA skips the continuation with a notice instead of queueing a report
    // Claude would hold. Children default to the parent's access, so the
    // creator asks for Ask explicitly.
    core.workspace
        .set_chat_config(
            &parent,
            &serde_json::from_value(claude_config("full")).unwrap(),
        )
        .unwrap();
    let parity_child = client
        .call(
            "CreateChildThread",
            json!({"parentTaskId":parent,"request":{
                "prompt":"sid:parity reply","idempotencyKey":"cc09-parity","title":"Parity child",
                "reportResults":true,"continueParent":true,"permissionMode":"ask"}}),
        )
        .await
        .unwrap()["taskId"]
        .as_str()
        .unwrap()
        .to_owned();
    let child_mode = core
        .workspace
        .chat(&parity_child)
        .unwrap()
        .unwrap()
        .config
        .unwrap()
        .effective_permission_mode;
    assert_eq!(child_mode, luna_proto::PermissionMode::Ask);
    until(
        "continuation skipped notice",
        || {
            entries(&core, &parent)
                .iter()
                .any(|e| e.id.starts_with("child-continuation-skipped:"))
        },
        || diag(&core, &[&parent], &project),
    )
    .await;
    let notice = entries(&core, &parent)
        .into_iter()
        .find(|e| e.id.starts_with("child-continuation-skipped:"))
        .unwrap();
    assert!(text(&notice).contains("Full access") && text(&notice).contains("Ask"));
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(lines_with(&fake_log(&project), &format!("luna-child:{parity_child}")).is_empty());
    let outcomes = core
        .workspace
        .chat(&parity_child)
        .unwrap()
        .unwrap()
        .child_thread
        .unwrap()
        .outcomes;
    assert!(
        outcomes.iter().all(|outcome| outcome.delivered),
        "skipped continuation still settles the outbox"
    );
    assert!(!entries(&core, &parent).iter().any(|e| {
        e.id.starts_with("child-report:") && outcomes.iter().any(|o| e.id.ends_with(&o.id))
    }));

    drop(client);
    drop(rpc);
    core.shutdown().await;
}
