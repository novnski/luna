//! CC-10 Claude native history, end to end through the production
//! `ClaudeHarness` against `fixtures/fake-claude-history.py`, a deterministic
//! 2.1.286 peer (frame and transcript shapes from the installed binary's SDK
//! schema and init-only probes). Failure cases:
//! `$B/evidence/cc10-history/failure-cases.md` (A*, B*, C*, D*).

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use sha2::Digest as _;
use tokio::sync::{mpsc, oneshot};

use luna_harness::{
    CancellationToken, ClaudeHarness, Harness, NativeFileRewindRequest, NativeHistoryScope,
    NativeRewindOutcome, NativeRewindProgress, NativeRewindRequest, ProviderForkProgress,
    RunControls, SteerDelivery, SteerMessage,
};
use luna_proto::{
    AgentEvent, DoneStatus, ProviderForkBoundary, ProviderForkCapability, ProviderForkRequest,
    RunRequest, SandboxLevel,
};

fn fake() -> PathBuf {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-claude-history.py");
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
    path
}

fn harness() -> ClaudeHarness {
    ClaudeHarness::new().with_executable(fake())
}

/// The fake's config root for a workspace (never a real Claude config).
fn home(cwd: &Path) -> PathBuf {
    cwd.join(".fake-claude-config")
}

fn request(cwd: &Path, prompt: &str, resume: Option<&str>) -> RunRequest {
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
        cwd: cwd.to_string_lossy().into_owned(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: false,
        permission_mode: luna_proto::PermissionMode::Ask,
        parent_permission_ceiling: None,
        environment_permission_ceiling: None,
        permission_policy: None,
        attachments: Vec::new(),
        worktree: None,
        resume: resume.map(str::to_owned),
    }
}

fn steer(prompt: &str) -> SteerMessage {
    SteerMessage {
        goal: None,
        input_items: Vec::new(),
        attachments: Vec::new(),
        prompt: prompt.into(),
        message_id: None,
        delivery: SteerDelivery::AllowFollowUpTurn,
        acceptance: None,
    }
}

struct Turns {
    session_id: String,
    /// One entry per `Done`: its status and the checkpoint forwarded just
    /// before it, if any.
    dones: Vec<(DoneStatus, Option<String>)>,
    checkpoint_events: usize,
}

/// Run `turns` as one warm session: each later prompt is sent once the
/// previous prompt's expected `Done`s arrived. `folded` prompts are queued
/// immediately (the fake folds them into the first turn).
async fn drive(
    cwd: &Path,
    resume: Option<&str>,
    turns: &[(&str, usize)],
    folded: &[&str],
) -> Turns {
    let (steer_tx, steer_rx) = mpsc::channel(8);
    let controls = RunControls {
        request_input: Box::new(|_| oneshot::channel().1),
        steering: steer_rx,
        interrupt: CancellationToken::new(),
        invoke_luna_tool: None,
    };
    for prompt in folded {
        steer_tx.send(steer(prompt)).await.unwrap();
    }
    let mut steer_tx = Some(steer_tx);
    let mut stream = harness()
        .with_config_dir(home(cwd))
        .run(request(cwd, turns[0].0, resume), controls)
        .await
        .expect("run starts");
    let mut out = Turns {
        session_id: String::new(),
        dones: Vec::new(),
        checkpoint_events: 0,
    };
    let mut next = 1;
    let mut owed = turns[0].1;
    let mut last_checkpoint = None;
    tokio::time::timeout(Duration::from_secs(20), async {
        while let Some(event) = stream.next().await {
            match event.expect("stream event") {
                AgentEvent::SessionStarted { session_id, .. } => out.session_id = session_id,
                AgentEvent::ProviderCheckpoint { checkpoint_id } => {
                    out.checkpoint_events += 1;
                    last_checkpoint = Some(checkpoint_id);
                }
                AgentEvent::Done { status, .. } => {
                    out.dones.push((status, last_checkpoint.take()));
                    owed = owed.saturating_sub(1);
                    if owed == 0 {
                        match turns.get(next) {
                            Some((prompt, dones)) => {
                                owed = *dones;
                                next += 1;
                                let sender = steer_tx.as_ref().expect("mailbox open");
                                sender.send(steer(prompt)).await.unwrap();
                            }
                            None => steer_tx = None,
                        }
                    }
                }
                _ => {}
            }
        }
    })
    .await
    .expect("run finished in time");
    out
}

fn scope(cwd: &Path, session_id: &str) -> NativeHistoryScope {
    NativeHistoryScope {
        session_id: session_id.into(),
        cwd: cwd.to_string_lossy().into_owned(),
        provider_home: Some(home(cwd).to_string_lossy().into_owned()),
    }
}

fn transcript(cwd: &Path, session_id: &str) -> PathBuf {
    let projects = home(cwd).join("projects");
    std::fs::read_dir(&projects)
        .unwrap()
        .flatten()
        .map(|dir| dir.path().join(format!("{session_id}.jsonl")))
        .find(|path| path.is_file())
        .expect("native transcript")
}

fn sha(path: &Path) -> String {
    format!("{:x}", sha2::Sha256::digest(std::fs::read(path).unwrap()))
}

fn argv_log(cwd: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(home(cwd).join("fake-claude-argv.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn knobs(cwd: &Path, value: serde_json::Value) {
    std::fs::write(home(cwd).join("fake-claude-knobs.json"), value.to_string()).unwrap();
}

fn fork_request(cwd: &Path, source: &str, boundary: ProviderForkBoundary) -> ProviderForkRequest {
    ProviderForkRequest {
        source_session_id: source.into(),
        boundary,
        cwd: cwd.to_string_lossy().into_owned(),
        model: None,
        target_name: None,
        provider_home: Some(home(cwd).to_string_lossy().into_owned()),
        force_file_auth: false,
    }
}

type Progress = Arc<Mutex<Vec<String>>>;

async fn fork(
    cwd: &Path,
    source: &str,
    boundary: ProviderForkBoundary,
) -> (
    Result<luna_proto::ProviderForkResult, luna_harness::HarnessError>,
    Vec<String>,
) {
    let seen: Progress = Arc::default();
    let tracked = seen.clone();
    let reporter = move |event: ProviderForkProgress| {
        tracked.lock().unwrap().push(match event {
            ProviderForkProgress::MutationStarted => "started".to_string(),
            ProviderForkProgress::Acknowledged {
                provider_session_id,
            } => format!("ack:{provider_session_id}"),
        });
        Ok::<(), luna_harness::HarnessError>(())
    };
    let result = harness()
        .fork_session_staged(fork_request(cwd, source, boundary), &reporter)
        .await;
    let progress = seen.lock().unwrap().clone();
    (result, progress)
}

async fn rewind(
    cwd: &Path,
    session: &str,
    target: &str,
    last_seen: &str,
) -> (
    Result<NativeRewindOutcome, luna_harness::HarnessError>,
    bool,
) {
    let written = Arc::new(Mutex::new(false));
    let tracked = written.clone();
    let reporter = move |event: NativeRewindProgress| {
        assert_eq!(event, NativeRewindProgress::RequestWritten);
        *tracked.lock().unwrap() = true;
    };
    let result = harness()
        .rewind_native_conversation(
            NativeRewindRequest {
                scope: scope(cwd, session),
                target_checkpoint: target.into(),
                last_seen_checkpoint: last_seen.into(),
            },
            &reporter,
        )
        .await;
    let written = *written.lock().unwrap();
    (result, written)
}

/// History controls must not join a launching Claude's peer bridge or invoke
/// managed SessionStart hooks. Run the production harness in a separate test
/// process so injected environment cannot race other tests or modify our host.
#[tokio::test]
async fn history_controls_disable_hooks_and_drop_inherited_session_identity() {
    const CHILD: &str = "LUNA_HISTORY_ISOLATION_REGRESSION_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
        command.args([
            "--exact",
            "history_controls_disable_hooks_and_drop_inherited_session_identity",
            "--nocapture",
        ]);
        command.env(CHILD, "1");
        for name in [
            "CLAUDE_CODE_MESSAGING_SOCKET",
            "CLAUDE_CODE_MESSAGING_TOKEN",
            "CLAUDE_CODE_BRIDGE_SESSION_ID",
            "CLAUDE_CODE_SESSION_ID",
            "CLAUDE_CODE_CHILD_SESSION",
            "CLAUDE_CODE_SESSION_ATTENDED",
            "CLAUDE_JOB_DIR",
        ] {
            command.env(name, "synthetic-parent-session");
        }
        let output = command.output().await.unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let workspace = tempfile::tempdir().unwrap();
    let cwd = workspace.path();
    std::fs::write(cwd.join("notes.txt"), "before\n").unwrap();
    let run = drive(cwd, None, &[("edit:notes.txt=after", 1)], &[]).await;
    let checkpoint = run.dones[0].1.as_deref().unwrap();
    fork(cwd, &run.session_id, ProviderForkBoundary::Tip)
        .await
        .0
        .unwrap();
    let preview = harness()
        .rewind_native_files(NativeFileRewindRequest {
            scope: scope(cwd, &run.session_id),
            checkpoint: checkpoint.into(),
            dry_run: true,
        })
        .await
        .unwrap();
    assert!(preview.can_rewind);
    assert_eq!(
        rewind(cwd, &run.session_id, checkpoint, checkpoint)
            .await
            .0
            .unwrap(),
        NativeRewindOutcome::Rewound
    );
    let controls: Vec<_> = argv_log(cwd)
        .into_iter()
        .filter(|entry| {
            entry["argv"]
                .as_array()
                .unwrap()
                .iter()
                .any(|arg| arg.as_str() == Some("--strict-mcp-config"))
        })
        .collect();
    assert_eq!(
        controls.len(),
        3,
        "fork validation, file preview and conversation rewind"
    );
    for entry in controls {
        assert_eq!(
            entry["inherited_session_env"],
            serde_json::json!([]),
            "history child retained foreign session identity"
        );
        let argv = entry["argv"].as_array().unwrap();
        let settings = argv
            .windows(2)
            .find(|pair| pair[0].as_str() == Some("--settings"))
            .and_then(|pair| pair[1].as_str())
            .map(|settings| serde_json::from_str::<serde_json::Value>(settings).unwrap())
            .unwrap_or_default();
        assert_eq!(
            settings["disableAllHooks"], true,
            "history initialization can execute managed hooks"
        );
    }
}

/// A stopped LUNA runtime does not establish exclusive access to the native
/// session: an external writer must block both kinds of history mutation.
#[tokio::test]
async fn history_rewinds_refuse_an_external_session_writer_before_mutation() {
    let workspace = tempfile::tempdir().unwrap();
    let cwd = workspace.path();
    let notes = cwd.join("notes.txt");
    std::fs::write(&notes, "before\n").unwrap();
    let run = drive(cwd, None, &[("edit:notes.txt=after", 1)], &[]).await;
    let checkpoint = run.dones[0].1.as_deref().unwrap();
    let session_file = transcript(cwd, &run.session_id);
    let before = (sha(&session_file), sha(&notes));
    // This process is alive and is not a child of itself; the fixture uses
    // its PID to model a foreign supervisor row without spawning a daemon.
    knobs(
        cwd,
        serde_json::json!({"external_sessions": [{
            "pid": std::process::id(), "id": "foreign-writer", "kind": "background",
            "sessionId": run.session_id, "state": "working",
        }]}),
    );
    let (conversation, written) = rewind(cwd, &run.session_id, checkpoint, checkpoint).await;
    let files = harness()
        .rewind_native_files(NativeFileRewindRequest {
            scope: scope(cwd, &run.session_id),
            checkpoint: checkpoint.into(),
            dry_run: false,
        })
        .await;
    assert!(matches!(
        conversation,
        Err(luna_harness::HarnessError::RejectedBeforeAcceptance(_))
    ));
    assert!(
        !written,
        "conversation rewind was sent to a concurrently written session"
    );
    assert!(matches!(
        files,
        Err(luna_harness::HarnessError::RejectedBeforeAcceptance(_))
    ));
    assert_eq!((sha(&session_file), sha(&notes)), before);
    assert!(
        argv_log(cwd).iter().all(|entry| !entry["argv"]
            .as_array()
            .unwrap()
            .iter()
            .any(|arg| arg.as_str() == Some("--strict-mcp-config"))),
        "a history control process launched before exclusive access was checked"
    );
}

/// A1–A6: every LUNA prompt carries a client uuid; only completed turns that
/// consumed LUNA's own uuid get exactly one checkpoint, equal to the native
/// transcript's turn-start uuid.
#[tokio::test]
async fn completed_turns_report_native_prompt_uuids_as_checkpoints() {
    let workspace = tempfile::tempdir().unwrap();
    let cwd = workspace.path();
    let run = drive(
        cwd,
        None,
        &[
            ("first prompt", 1),
            ("second prompt", 1),
            ("scenario:no-uuid", 1),
            ("scenario:foreign-uuid", 1),
            ("scenario:error", 1),
            // CC-02 drops a repeated result uuid: one Done, one checkpoint.
            ("scenario:dup-result", 1),
            ("scenario:autonomous", 2),
        ],
        &[],
    )
    .await;
    let checkpoints: Vec<Option<&str>> = run.dones.iter().map(|(_, id)| id.as_deref()).collect();
    assert_eq!(run.dones.len(), 8, "{:?}", run.dones);
    assert!(checkpoints[0].is_some() && checkpoints[1].is_some());
    assert_eq!(
        checkpoints[2], None,
        "A2: no native uuid echo, no checkpoint"
    );
    assert_eq!(checkpoints[3], None, "A4: a foreign uuid never checkpoints");
    assert_eq!(checkpoints[4], None, "A5: errored turn");
    assert_eq!(run.dones[4].0, DoneStatus::Errored);
    assert!(checkpoints[5].is_some(), "A6: duplicate result");
    assert!(
        checkpoints[6].is_some() && checkpoints[7].is_none(),
        "A4: native wake turn"
    );
    assert_eq!(run.checkpoint_events, 4);

    // The checkpoints are the native transcript's own prompt uuids, and the
    // native history also holds turns LUNA never checkpointed (C6 input).
    let native = harness()
        .native_history_turns(scope(cwd, &run.session_id))
        .await
        .unwrap();
    assert!(!native.compacted);
    let ours: Vec<&str> = checkpoints.iter().flatten().copied().collect();
    assert_eq!(
        native.turn_starts.len(),
        8,
        "7 LUNA prompts + 1 native wake"
    );
    for id in &ours {
        assert!(native.turn_starts.iter().any(|native| native == id), "{id}");
    }
    // Every launch enabled native file checkpoints (D1).
    assert!(
        argv_log(cwd)
            .iter()
            .all(|entry| entry["checkpointing"] == true)
    );
}

/// A3: a prompt batch consumes two LUNA uuids; the turn's checkpoint is the
/// first one, and the folded prompt is never an eligible boundary.
#[tokio::test]
async fn batched_prompts_checkpoint_the_first_consumed_uuid() {
    let workspace = tempfile::tempdir().unwrap();
    let cwd = workspace.path();
    let run = drive(cwd, None, &[("scenario:batch", 1)], &["folded follow-up"]).await;
    let first = run.dones[0].1.clone().expect("batch checkpoint");
    let native = harness()
        .native_history_turns(scope(cwd, &run.session_id))
        .await
        .unwrap();
    assert_eq!(native.turn_starts.first(), Some(&first));
    assert_eq!(
        native.turn_starts.len(),
        2,
        "both prompts are native user rows"
    );
}

/// B1–B6: forks copy the active prefix into a new session with fresh uuids,
/// leave the source untouched, remap checkpoints, validate with the CLI and
/// can continue natively.
#[tokio::test]
async fn forks_copy_the_native_prefix_into_an_independent_session() {
    let workspace = tempfile::tempdir().unwrap();
    let cwd = workspace.path();
    assert_eq!(
        harness().fork_capability(),
        ProviderForkCapability::Checkpoint
    );
    let run = drive(cwd, None, &[("one", 1), ("two", 1), ("three", 1)], &[]).await;
    let [c1, c2, c3] = [0, 1, 2].map(|i| run.dones[i].1.clone().unwrap());
    let source = transcript(cwd, &run.session_id);
    let before = sha(&source);

    let (through, progress) = fork(
        cwd,
        &run.session_id,
        ProviderForkBoundary::ThroughCheckpoint {
            checkpoint_id: c2.clone(),
        },
    )
    .await;
    let through = through.expect("through fork");
    assert_ne!(through.provider_session_id, run.session_id, "B1");
    assert_eq!(
        progress,
        vec![
            "started".to_string(),
            format!("ack:{}", through.provider_session_id)
        ]
    );
    let forked = harness()
        .native_history_turns(scope(cwd, &through.provider_session_id))
        .await
        .unwrap();
    assert_eq!(forked.turn_starts.len(), 2, "B2: through turn two");
    assert_eq!(through.checkpoint_map.len(), 2, "B3");
    assert_eq!(forked.turn_starts[0], through.checkpoint_map[&c1]);
    assert_eq!(forked.turn_starts[1], through.checkpoint_map[&c2]);
    assert!(!forked.turn_starts.contains(&c1), "B3: fresh uuids");
    assert_eq!(sha(&source), before, "B1: source transcript unchanged");
    let fork_rows = std::fs::read_to_string(transcript(cwd, &through.provider_session_id)).unwrap();
    assert!(fork_rows.contains("\"forkedFrom\""));
    assert!(fork_rows.contains("\"custom-title\""));
    assert!(
        !fork_rows.contains("file-history-snapshot"),
        "no undo history in forks"
    );
    let validation = argv_log(cwd)
        .into_iter()
        .rev()
        .find(|entry| entry["argv"].to_string().contains("--resume-session-at"))
        .expect("CLI loader validation ran");
    let argv = validation["argv"].to_string();
    assert!(argv.contains("--fork-session") && argv.contains("--setting-sources"));
    assert!(!argv.contains("dangerously"));

    let (before_turn, _) = fork(
        cwd,
        &run.session_id,
        ProviderForkBoundary::BeforeCheckpoint {
            checkpoint_id: c3.clone(),
        },
    )
    .await;
    let before_turn = before_turn.unwrap();
    let turns = harness()
        .native_history_turns(scope(cwd, &before_turn.provider_session_id))
        .await
        .unwrap();
    assert_eq!(turns.turn_starts.len(), 2, "B2: before turn three");
    assert!(!before_turn.checkpoint_map.contains_key(&c3));

    let (tip, _) = fork(cwd, &run.session_id, ProviderForkBoundary::Tip).await;
    assert_eq!(tip.unwrap().checkpoint_map.len(), 3, "B2: tip");

    // Before the first turn: an empty native conversation (the CLI's own
    // explicit null leaf) that still resumes, e.g. to edit the first prompt.
    let (first, progress) = fork(
        cwd,
        &run.session_id,
        ProviderForkBoundary::BeforeCheckpoint { checkpoint_id: c1 },
    )
    .await;
    let first = first.expect("fork before the first turn");
    assert!(first.checkpoint_map.is_empty());
    assert_eq!(progress.len(), 2);
    let empty = harness()
        .native_history_turns(scope(cwd, &first.provider_session_id))
        .await
        .unwrap();
    assert!(empty.turn_starts.is_empty());
    let edited = drive(
        cwd,
        Some(&first.provider_session_id),
        &[("edited first prompt", 1)],
        &[],
    )
    .await;
    let edited_turns = harness()
        .native_history_turns(scope(cwd, &first.provider_session_id))
        .await
        .unwrap();
    assert_eq!(
        edited_turns.turn_starts,
        vec![edited.dones[0].1.clone().unwrap()]
    );
    // B5: unknown checkpoint, refused before mutation.
    let (unknown, progress) = fork(
        cwd,
        &run.session_id,
        ProviderForkBoundary::ThroughCheckpoint {
            checkpoint_id: uuid::Uuid::new_v4().to_string(),
        },
    )
    .await;
    assert!(unknown.is_err() && progress.is_empty());

    // The fork is an ordinary resumable native session.
    let continued = drive(cwd, Some(&through.provider_session_id), &[("four", 1)], &[]).await;
    assert_eq!(continued.session_id, through.provider_session_id);
    let resumed = harness()
        .native_history_turns(scope(cwd, &through.provider_session_id))
        .await
        .unwrap();
    assert_eq!(resumed.turn_starts.len(), 3);
    assert_eq!(
        resumed.turn_starts[2],
        continued.dones[0].1.clone().unwrap()
    );
    assert_eq!(sha(&source), before, "B1: source still unchanged");

    // B4/B6: a fork the CLI cannot load is removed and reported after start.
    knobs(cwd, serde_json::json!({ "fork_validation_fail": true }));
    let (failed, progress) = fork(cwd, &run.session_id, ProviderForkBoundary::Tip).await;
    assert!(
        failed
            .unwrap_err()
            .to_string()
            .contains("load the forked session")
    );
    assert_eq!(progress, vec!["started".to_string()]);
    let sessions = std::fs::read_dir(source.parent().unwrap()).unwrap().count();
    assert_eq!(
        sessions, 5,
        "source + four forks; the failed fork was removed"
    );
}

/// C1–C5, C8: same-session rewind uses the CLI's `rewind_conversation`,
/// surfaces native refusals and leaves uncertain outcomes for readback.
#[tokio::test]
async fn same_session_rewind_truncates_or_refuses_natively() {
    let workspace = tempfile::tempdir().unwrap();
    let cwd = workspace.path();
    let run = drive(cwd, None, &[("one", 1), ("two", 1), ("three", 1)], &[]).await;
    let [c1, c2, c3] = [0, 1, 2].map(|i| run.dones[i].1.clone().unwrap());
    let session = run.session_id.clone();

    // C2: a later turn LUNA has not reviewed.
    let (stale, written) = rewind(cwd, &session, &c2, &c2).await;
    assert!(written);
    assert_eq!(
        stale.unwrap(),
        NativeRewindOutcome::Refused {
            reason: "unseen_later_turn".into(),
            message: "unseen later turn".into()
        }
    );
    // C3: unknown target.
    let (missing, _) = rewind(cwd, &session, &uuid::Uuid::new_v4().to_string(), &c3).await;
    assert!(
        matches!(missing.unwrap(), NativeRewindOutcome::Refused { reason, .. } if reason == "target_not_found")
    );
    // C1: a busy CLI refuses before any change.
    knobs(
        cwd,
        serde_json::json!({ "rewind_conversation": { "reply": {
            "rewound": false, "prefillText": null, "precedingAssistantUuid": null,
            "error": "turn running", "reason": "turn_running" } } }),
    );
    let (busy, _) = rewind(cwd, &session, &c3, &c3).await;
    assert!(
        matches!(busy.unwrap(), NativeRewindOutcome::Refused { reason, .. } if reason == "turn_running")
    );
    // C4: `state_changed` is uncertain, never a refusal.
    knobs(
        cwd,
        serde_json::json!({ "rewind_conversation": { "reply": {
            "rewound": false, "prefillText": null, "precedingAssistantUuid": null,
            "error": "state changed", "reason": "state_changed" } } }),
    );
    let (changed, written) = rewind(cwd, &session, &c3, &c3).await;
    assert!(written && changed.unwrap_err().to_string().contains("uncertain"));
    let unchanged = harness()
        .native_history_turns(scope(cwd, &session))
        .await
        .unwrap();
    assert_eq!(
        unchanged.turn_starts,
        vec![c1.clone(), c2.clone(), c3.clone()]
    );

    // C5: the process dies after persisting; readback proves the cut.
    knobs(
        cwd,
        serde_json::json!({ "rewind_conversation": { "die_after_request": true, "persist_before_dying": true } }),
    );
    let (died, written) = rewind(cwd, &session, &c3, &c3).await;
    assert!(written && died.is_err());
    let after_crash = harness()
        .native_history_turns(scope(cwd, &session))
        .await
        .unwrap();
    assert_eq!(after_crash.turn_starts, vec![c1.clone(), c2.clone()]);

    // Success: truncate turn two on the same session id.
    knobs(cwd, serde_json::json!({}));
    let (rewound, written) = rewind(cwd, &session, &c2, &c2).await;
    assert!(written);
    assert_eq!(rewound.unwrap(), NativeRewindOutcome::Rewound);
    let cut = harness()
        .native_history_turns(scope(cwd, &session))
        .await
        .unwrap();
    assert_eq!(cut.turn_starts, vec![c1.clone()]);
    // The same session continues from the retained prefix.
    let continued = drive(cwd, Some(&session), &[("after rewind", 1)], &[]).await;
    assert_eq!(continued.session_id, session);
    let resumed = harness()
        .native_history_turns(scope(cwd, &session))
        .await
        .unwrap();
    assert_eq!(resumed.turn_starts.len(), 2);
    assert_eq!(resumed.turn_starts[0], c1);

    // C8: history children never bypass permissions and load no settings.
    for entry in argv_log(cwd) {
        let argv = entry["argv"].to_string();
        assert!(!argv.contains("thread/rollback") && !argv.contains("thread/fork"));
        if argv.contains("rewind") || argv.contains("--strict-mcp-config") {
            assert!(!argv.contains("dangerously"));
        }
    }
}

/// D2, D3, D6: native file checkpoints preview without touching files and
/// restore Claude-edited files per turn.
#[tokio::test]
async fn file_checkpoints_preview_then_restore_claude_edits() {
    let workspace = tempfile::tempdir().unwrap();
    let cwd = workspace.path();
    let notes = cwd.join("notes.txt");
    std::fs::write(&notes, "alpha\n").unwrap();
    let run = drive(
        cwd,
        None,
        &[("edit:notes.txt=beta", 1), ("edit:notes.txt=gamma", 1)],
        &[],
    )
    .await;
    let [c1, c2] = [0, 1].map(|i| run.dones[i].1.clone().unwrap());
    let files = |checkpoint: &str, dry_run: bool| NativeFileRewindRequest {
        scope: scope(cwd, &run.session_id),
        checkpoint: checkpoint.into(),
        dry_run,
    };
    let before = sha(&notes);
    let preview = harness()
        .rewind_native_files(files(&c2, true))
        .await
        .unwrap();
    assert!(preview.can_rewind && preview.error.is_none());
    assert_eq!(preview.files_changed.len(), 1);
    assert!(preview.files_changed[0].ends_with("notes.txt"));
    assert_eq!((preview.insertions, preview.deletions), (1, 1));
    assert_eq!(sha(&notes), before, "D3: dry run changes nothing");

    let applied = harness()
        .rewind_native_files(files(&c2, false))
        .await
        .unwrap();
    assert!(applied.can_rewind);
    assert_eq!(std::fs::read_to_string(&notes).unwrap(), "beta\n");
    harness()
        .rewind_native_files(files(&c1, false))
        .await
        .unwrap();
    assert_eq!(std::fs::read_to_string(&notes).unwrap(), "alpha\n");

    // D2: no checkpoint for an identity that is not a prompt turn.
    let none = harness()
        .rewind_native_files(files(&uuid::Uuid::new_v4().to_string(), true))
        .await
        .unwrap();
    assert!(!none.can_rewind);
    assert_eq!(
        none.error.as_deref(),
        Some("No file checkpoint found for this message.")
    );
    // D6: a failed apply is reported, never claimed as restored.
    let failed = harness()
        .rewind_native_files(files(&uuid::Uuid::new_v4().to_string(), false))
        .await
        .unwrap();
    assert!(!failed.can_rewind && failed.error.unwrap().contains("Failed to rewind"));
}

/// The transcript reader against rows the REAL 2.1.286 CLI wrote during an
/// init-only `rewind_conversation` probe (explicit `last-prompt` leaf,
/// `atis-latch`, `mode`, `cost-state`): only the first turn remains active.
#[tokio::test]
async fn real_cli_rewound_transcript_reads_back_the_retained_prefix() {
    let workspace = tempfile::tempdir().unwrap();
    let cwd = workspace.path();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/claude/cc10-real-2.1.286-rewound-session.jsonl");
    let text = std::fs::read_to_string(&fixture).unwrap();
    let session = serde_json::from_str::<serde_json::Value>(text.lines().nth(1).unwrap()).unwrap()
        ["sessionId"]
        .as_str()
        .unwrap()
        .to_owned();
    let directory = home(cwd).join("projects").join("probe-project");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join(format!("{session}.jsonl")), &text).unwrap();
    let turns = harness()
        .native_history_turns(scope(cwd, &session))
        .await
        .unwrap();
    let first_prompt = text
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .find(|row| row["type"] == "user")
        .unwrap()["uuid"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(turns.turn_starts, vec![first_prompt]);
}
