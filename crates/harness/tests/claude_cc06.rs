//! CC-06 adapter E2E against `fixtures/fake-claude-cc06.py`, a deterministic
//! peer speaking Claude Code 2.1.286 frame shapes (taken from the installed
//! binary's SDK schema and real initialization-only probes). Failure cases are
//! listed in the CC-06 evidence `failure-cases.md` (A*, B*, C*, D*).

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use futures::StreamExt;
use futures::stream::BoxStream;
use tokio::sync::{mpsc, oneshot};

use luna_harness::{
    CancellationToken, ClaudeHarness, Harness, HarnessError, LiveRuntimeSelection, RunControls,
    SteerAcceptance, SteerDelivery, SteerMessage,
};
use luna_proto::{
    AgentEvent, DoneStatus, ProviderAccountingReport, ReasoningLevel, RunRequest, RuntimeReadback,
    RuntimeReadbackSource, SandboxLevel,
};

type Events = BoxStream<'static, Result<AgentEvent, HarnessError>>;

fn fake() -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("fake-claude-cc06.py");
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
    path
}

fn harness() -> ClaudeHarness {
    ClaudeHarness::new()
        .with_executable(fake())
        .with_graces(Duration::from_millis(200), Duration::from_millis(200))
}

/// A run directory whose fake logs every spawn argv and stdin line.
fn workdir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(".cc06-log"), "").unwrap();
    dir
}

fn log_lines(dir: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(dir.join("cc06-fake.log"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn stdin_frames(dir: &Path) -> Vec<serde_json::Value> {
    log_lines(dir)
        .into_iter()
        .filter(|entry| entry["event"] == "stdin")
        .map(|entry| serde_json::from_str(entry["line"].as_str().unwrap()).unwrap())
        .collect()
}

fn request(prompt: &str, cwd: &Path) -> RunRequest {
    RunRequest {
        review: None,
        input_items: Vec::new(),
        prompt: prompt.into(),
        harness: None,
        provider_account_id: None,
        model: None,
        reasoning: None,
        model_options: serde_json::Map::new(),
        cwd: cwd.to_string_lossy().into_owned(),
        sandbox: SandboxLevel::DangerFullAccess,
        auto_approve: true,
        permission_mode: luna_proto::PermissionMode::Ask,
        parent_permission_ceiling: None,
        environment_permission_ceiling: None,
        permission_policy: None,
        attachments: Vec::new(),
        worktree: None,
        resume: None,
    }
}

fn controls() -> (RunControls, mpsc::Sender<SteerMessage>) {
    let (steer_tx, steer_rx) = mpsc::channel(8);
    let controls = RunControls {
        request_input: Box::new(|_| oneshot::channel().1),
        steering: steer_rx,
        interrupt: CancellationToken::new(),
        invoke_luna_tool: None,
    };
    (controls, steer_tx)
}

fn steer(prompt: &str, id: &str, delivery: SteerDelivery) -> SteerMessage {
    SteerMessage {
        prompt: prompt.into(),
        attachments: Vec::new(),
        input_items: Vec::new(),
        message_id: Some(id.into()),
        delivery,
        acceptance: None,
    }
}

async fn collect_until_end(stream: Events) -> Vec<AgentEvent> {
    tokio::time::timeout(
        Duration::from_secs(15),
        stream.map(|r| r.expect("stream event")).collect::<Vec<_>>(),
    )
    .await
    .expect("run ended in time")
}

/// Read until `count` Done events were seen (the session stays warm).
async fn until_dones(stream: &mut Events, count: usize, seen: &mut Vec<AgentEvent>) {
    tokio::time::timeout(Duration::from_secs(15), async {
        let mut dones = seen
            .iter()
            .filter(|e| matches!(e, AgentEvent::Done { .. }))
            .count();
        while dones < count {
            let event = stream.next().await.expect("stream open").expect("event");
            if matches!(event, AgentEvent::Done { .. }) {
                dones += 1;
            }
            seen.push(event);
        }
    })
    .await
    .expect("expected Done in time");
}

fn reports(events: &[AgentEvent]) -> Vec<&ProviderAccountingReport> {
    events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ProviderAccounting { report } => Some(report.as_ref()),
            _ => None,
        })
        .collect()
}

fn readbacks(events: &[AgentEvent]) -> Vec<&RuntimeReadback> {
    reports(events)
        .into_iter()
        .filter_map(|report| match report {
            ProviderAccountingReport::Runtime(readback) => Some(readback),
            _ => None,
        })
        .collect()
}

fn texts(events: &[AgentEvent]) -> Vec<&str> {
    events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::TextDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// A1/A2/A4/A9/A10: an explicit steer is confirmed only by the CLI replay of
/// its own native uuid — not by text, not twice, never as transcript content.
#[tokio::test]
async fn explicit_steer_settles_only_on_its_own_native_replay() {
    let dir = workdir();
    let (controls, steer_tx) = controls();
    let (accepted_tx, accepted_rx) = oneshot::channel();
    let mut explicit = steer(
        "redirect please",
        "queued-steer-1",
        SteerDelivery::RequireActiveTurn,
    );
    explicit.acceptance = Some(accepted_tx);
    steer_tx.send(explicit).await.unwrap();
    drop(steer_tx);
    let stream = harness()
        .run(request("cc06:delivery go", dir.path()), controls)
        .await
        .unwrap();
    let events = collect_until_end(stream).await;

    assert_eq!(accepted_rx.await.unwrap(), SteerAcceptance::Accepted);
    let steered: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::Steered {
                user_message_id, ..
            } => Some(user_message_id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        steered,
        vec![Some("queued-steer-1".to_owned())],
        "exactly one receipt, for the exact engine id: {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::UserMessage { .. })),
        "replay frames are receipts, never transcript content"
    );
    assert_eq!(texts(&events), ["working", "steered:redirect please"]);
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AgentEvent::Done { .. }))
            .count(),
        1
    );

    let spawn = log_lines(dir.path())
        .into_iter()
        .find(|entry| entry["event"] == "spawn")
        .unwrap();
    assert!(
        spawn["argv"]
            .as_array()
            .unwrap()
            .iter()
            .any(|arg| arg == "--replay-user-messages")
    );
    let users: Vec<_> = stdin_frames(dir.path())
        .into_iter()
        .filter(|frame| frame["type"] == "user")
        .collect();
    assert_eq!(users.len(), 2);
    for frame in &users {
        let uuid = frame["uuid"]
            .as_str()
            .expect("every user line carries a uuid");
        assert!(
            uuid::Uuid::parse_str(uuid).is_ok(),
            "A10: valid native uuid"
        );
    }
    assert_ne!(
        users[1]["uuid"], "queued-steer-1",
        "non-UUID engine ids are mapped"
    );
}

/// A5/A6/D1/D2/D5: a Claude-initiated result never completes the outstanding
/// human turn; accounting is still kept; an idle foreign result still ends.
#[tokio::test]
async fn foreign_result_never_completes_the_outstanding_human_turn() {
    let dir = workdir();
    let (controls, steer_tx) = controls();
    drop(steer_tx);
    let stream = harness()
        .run(request("cc06:foreign go", dir.path()), controls)
        .await
        .unwrap();
    let events = collect_until_end(stream).await;

    let first_done = events
        .iter()
        .position(|e| matches!(e, AgentEvent::Done { .. }))
        .expect("a Done");
    let human = events
        .iter()
        .position(|e| matches!(e, AgentEvent::TextDelta { text } if text == "human answer"))
        .expect("human answer streamed");
    assert!(
        human < first_done,
        "the background result must not end the human turn: {events:?}"
    );
    let dones: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Done { status, result, .. } => Some((*status, result.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        dones,
        [
            (DoneStatus::Completed, Some("answer".into())),
            (DoneStatus::Completed, Some("wake".into()))
        ],
        "own result, then the idle wake"
    );

    let reports = reports(&events);
    let results: Vec<_> = reports
        .iter()
        .filter_map(|r| match r {
            ProviderAccountingReport::Result(result) => Some(result),
            _ => None,
        })
        .collect();
    assert_eq!(results.len(), 3, "accounting kept for every result");
    assert_eq!(results[0].origin.as_deref(), Some("task-notification"));
    let last = results.last().unwrap();
    assert_eq!(
        last.cumulative_cost_usd,
        Some(0.0375),
        "running total, latest wins"
    );
    assert_eq!(last.turn_tokens.cached_input, 4000);
    assert_eq!(last.turn_tokens.cache_creation, 300);
    assert_eq!(last.cumulative_tokens().cached_input, 12_000);
    assert_eq!(last.models[0].context_window, Some(200_000));
    let rate = reports
        .iter()
        .find_map(|r| match r {
            ProviderAccountingReport::RateLimit(rate) => Some(rate),
            _ => None,
        })
        .expect("allowed_warning is retained, not dropped");
    assert_eq!(rate.status, "allowed_warning");
    assert_eq!(rate.windows.len(), 2);
    assert!(rate.resets_at_ms.is_some());
    assert!(reports.iter().any(|r| matches!(
        r,
        ProviderAccountingReport::ApiRetry(retry) if retry.error_status == Some(529)
    )));
}

/// B1/B2/B3/B4: `--effort` follows the discovered ladder and the CLI's own
/// `get_settings` readback is reported, including a requested-but-unapplied
/// effort.
#[tokio::test]
async fn launch_flags_follow_discovery_and_readback_reports_the_applied_runtime() {
    let harness = harness();
    let models = harness.live_models().await.expect("fake initialize");
    assert!(models.iter().any(|m| m.id == "claude-haiku-4-5-20251001"));

    for (model, reasoning, prompt, flag, effective, matches) in [
        // Haiku advertises no effort ladder: no flag, nothing to apply.
        (
            "claude-haiku-4-5-20251001",
            ReasoningLevel::High,
            "cc06:readback",
            None,
            None,
            true,
        ),
        // The ladder lacks xhigh: nearest higher supported level.
        (
            "claude-sonnet-5-5",
            ReasoningLevel::XHigh,
            "cc06:readback",
            Some("max"),
            Some("max"),
            true,
        ),
        // Acknowledged/launched but not applied: reported, not hidden.
        (
            "claude-opus-5-5",
            ReasoningLevel::High,
            "cc06:readback-drop",
            Some("high"),
            None,
            false,
        ),
    ] {
        let dir = workdir();
        let (controls, steer_tx) = controls();
        drop(steer_tx);
        let mut req = request(prompt, dir.path());
        req.model = Some(model.into());
        req.reasoning = Some(reasoning);
        let events = collect_until_end(harness.run(req, controls).await.unwrap()).await;

        let argv: Vec<String> = log_lines(dir.path())
            .into_iter()
            .find(|entry| entry["event"] == "spawn")
            .unwrap()["argv"]
            .as_array()
            .unwrap()
            .iter()
            .map(|arg| arg.as_str().unwrap().to_owned())
            .collect();
        let sent_effort = argv
            .iter()
            .position(|arg| arg == "--effort")
            .map(|index| argv[index + 1].clone());
        assert_eq!(sent_effort.as_deref(), flag, "{model}: {argv:?}");

        let readback = readbacks(&events);
        assert_eq!(readback.len(), 1, "{model}: one launch readback");
        let readback = readback[0];
        assert_eq!(readback.source, RuntimeReadbackSource::Launch);
        assert_eq!(readback.requested_effort.as_deref(), flag);
        assert_eq!(readback.effective_effort.as_deref(), effective, "{model}");
        assert_eq!(readback.effective_model.as_deref(), Some(model));
        assert_eq!(readback.matches, matches, "{model}: {readback:?}");
        if !matches {
            assert!(
                readback
                    .detail
                    .as_deref()
                    .unwrap()
                    .contains("effort high requested, none applied")
            );
        }
    }
}

fn selection(model: &str, reasoning: ReasoningLevel, fast: bool) -> LiveRuntimeSelection {
    let mut model_options = serde_json::Map::new();
    if fast {
        model_options.insert("fastMode".into(), "on".into());
    }
    LiveRuntimeSelection {
        model: Some(model.into()),
        reasoning: Some(reasoning),
        model_options,
    }
}

/// C1/C3: a staged model+effort+fast change is applied natively, acknowledged
/// and read back BEFORE the prompt is written; ultrathink is never live.
#[tokio::test]
async fn staged_live_switch_applies_and_reads_back_before_the_prompt() {
    let harness = harness();
    harness.live_models().await.unwrap();
    let dir = workdir();
    let (controls, steer_tx) = controls();
    let mut req = request("cc06:switch hi", dir.path());
    req.model = Some("claude-sonnet-5-5".into());
    req.reasoning = Some(ReasoningLevel::High);
    let mut stream = harness.run(req, controls).await.unwrap();
    let mut events = Vec::new();
    until_dones(&mut stream, 1, &mut events).await;

    let from = selection("claude-sonnet-5-5", ReasoningLevel::High, false);
    assert!(
        !harness.stage_live_reconfiguration(
            "never",
            &from,
            &selection("claude-sonnet-5-5", ReasoningLevel::Ultrathink, false)
        ),
        "C3: ultrathink is a prompt convention, never a live switch"
    );
    assert!(harness.stage_live_reconfiguration(
        "follow-1",
        &from,
        &selection("claude-opus-5-5", ReasoningLevel::Max, true)
    ));
    steer_tx
        .send(steer(
            "follow-up",
            "follow-1",
            SteerDelivery::AllowFollowUpTurn,
        ))
        .await
        .unwrap();
    until_dones(&mut stream, 2, &mut events).await;
    drop(steer_tx);
    events.extend(collect_until_end(stream).await);

    assert!(
        texts(&events).contains(&"echo:follow-up model=claude-opus-5-5 effort=max"),
        "the prompt ran under the new runtime: {:?}",
        texts(&events)
    );
    let switch = readbacks(&events)
        .into_iter()
        .find(|r| r.source == RuntimeReadbackSource::LiveSwitch)
        .expect("live switch readback");
    assert!(switch.matches, "{switch:?}");
    assert_eq!(switch.effective_model.as_deref(), Some("claude-opus-5-5"));
    assert_eq!(switch.effective_effort.as_deref(), Some("max"));

    let frames = stdin_frames(dir.path());
    let position = |pred: &dyn Fn(&serde_json::Value) -> bool| frames.iter().position(|f| pred(f));
    let set_model = position(&|f| f["request"]["subtype"] == "set_model").unwrap();
    let apply = position(&|f| f["request"]["subtype"] == "apply_flag_settings").unwrap();
    let prompt = position(&|f| f["type"] == "user" && f["message"]["content"] == "follow-up")
        .expect("prompt written");
    let readback = frames
        .iter()
        .rposition(|f| f["request"]["subtype"] == "get_settings")
        .unwrap();
    assert!(set_model < readback && apply < readback && readback < prompt);
    assert_eq!(frames[set_model]["request"]["model"], "claude-opus-5-5");
    assert_eq!(
        frames[apply]["request"]["settings"],
        serde_json::json!({"effortLevel": "max", "fastMode": true})
    );
    let spawns = log_lines(dir.path())
        .into_iter()
        .filter(|entry| entry["event"] == "spawn")
        .count();
    assert_eq!(spawns, 1, "no process restart");
}

/// C2/B3: a rejected `set_model`, or an acked effort that readback shows was
/// not applied, never writes the prompt under the old runtime; the run ends so
/// the engine can re-dispatch the message.
#[tokio::test]
async fn failed_live_switch_never_writes_the_prompt() {
    for (scenario, to, needle) in [
        (
            "cc06:switch-reject hi",
            selection("claude-opus-5-5", ReasoningLevel::High, false),
            "check_failed",
        ),
        (
            "cc06:switch-noapply hi",
            selection("claude-sonnet-5-5", ReasoningLevel::Low, false),
            "effort low requested, high applied",
        ),
    ] {
        let harness = harness();
        harness.live_models().await.unwrap();
        let dir = workdir();
        let (controls, steer_tx) = controls();
        let mut req = request(scenario, dir.path());
        req.model = Some("claude-sonnet-5-5".into());
        req.reasoning = Some(ReasoningLevel::High);
        let mut stream = harness.run(req, controls).await.unwrap();
        let mut events = Vec::new();
        until_dones(&mut stream, 1, &mut events).await;
        let from = selection("claude-sonnet-5-5", ReasoningLevel::High, false);
        assert!(harness.stage_live_reconfiguration("follow-2", &from, &to));
        steer_tx
            .send(steer(
                "follow-up",
                "follow-2",
                SteerDelivery::AllowFollowUpTurn,
            ))
            .await
            .unwrap();
        // The run ends by itself (stdin closed after the failure).
        events.extend(collect_until_end(stream).await);
        drop(steer_tx);

        let failed = readbacks(&events)
            .into_iter()
            .find(|r| r.source == RuntimeReadbackSource::LiveSwitch)
            .expect("failure is reported");
        assert!(!failed.matches);
        assert!(
            failed.detail.as_deref().unwrap().contains(needle),
            "{scenario}: {failed:?}"
        );
        assert!(
            !stdin_frames(dir.path())
                .iter()
                .any(|f| f["type"] == "user" && f["message"]["content"] == "follow-up"),
            "{scenario}: the prompt must not reach the CLI"
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, AgentEvent::Steered { .. })),
            "{scenario}: no boundary for an unwritten prompt"
        );
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, AgentEvent::Done { .. }))
                .count(),
            1,
            "{scenario}: no synthetic Done for the unwritten follow-up"
        );
    }
}
