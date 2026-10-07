//! Live probe (OC-12): native history against a real OpenCode 2 binary,
//! through the adapter's own hooks.
//!
//!     opencode_history_probe <exe> <provider/model> scenario <workspace>
//!     opencode_history_probe <exe> <provider/model> turn <workspace> <prompt> [session]
//!
//! `scenario` runs three marker turns in one session (the second writes
//! `beta.txt`), then checks: the persisted checkpoints are the turns' first
//! user message ids and equal `native_history_turns`; forks before turn 2,
//! through turn 2 and at the tip hold the expected turn prefixes and leave
//! the source unchanged; a fork at a non-turn id is refused and leaves no
//! session behind; rewinding the source before turn 2 keeps `beta.txt` and
//! leaves only turn 1; a stale rewind is refused; follow-up turns on the
//! rewound source and on the before-turn-2 fork run. Every line starting
//! `CHECK` is an assertion; the exit code is 0 only when all pass.
//!
//! `turn` runs one turn (resuming `session` when given) and prints the
//! outcome, for the external-delete and plan-agent checks.

use futures::StreamExt;
use luna_harness::{
    CancellationToken, Harness, NativeHistoryScope, NativeRewindOutcome, NativeRewindRequest,
    OpencodeHarness, RunControls,
};
use luna_proto::{
    AgentEvent, DoneStatus, ProviderForkBoundary, ProviderForkRequest, RunRequest, SandboxLevel,
};
use tokio::sync::mpsc;

struct Turn {
    session_id: Option<String>,
    checkpoint: Option<String>,
    status: Option<DoneStatus>,
    error: Option<String>,
    text: String,
}

async fn run_turn(
    harness: &OpencodeHarness,
    cwd: &str,
    model: Option<String>,
    prompt: &str,
    resume: Option<String>,
) -> Turn {
    let (_steer_tx, steering) = mpsc::channel(8);
    let request = RunRequest {
        review: None,
        goal: None,
        input_items: Vec::new(),
        prompt: prompt.into(),
        harness: None,
        provider_account_id: None,
        model,
        reasoning: None,
        model_options: serde_json::Map::new(),
        cwd: cwd.into(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: true,
        permission_mode: luna_proto::PermissionMode::Full,
        parent_permission_ceiling: None,
        environment_permission_ceiling: None,
        permission_policy: None,
        attachments: Vec::new(),
        resume,
        worktree: None,
    };
    let mut turn = Turn {
        session_id: None,
        checkpoint: None,
        status: None,
        error: None,
        text: String::new(),
    };
    let mut stream = match harness
        .run(
            request,
            RunControls {
                request_input: Box::new(|_| panic!("probe must not ask for input")),
                steering,
                interrupt: CancellationToken::new(),
                invoke_luna_tool: None,
            },
        )
        .await
    {
        Ok(stream) => stream,
        Err(error) => {
            turn.error = Some(error.to_string());
            return turn;
        }
    };
    loop {
        let event =
            match tokio::time::timeout(std::time::Duration::from_secs(180), stream.next()).await {
                Ok(Some(event)) => event,
                Ok(None) => break,
                Err(_) => {
                    turn.error = Some("turn timed out".into());
                    break;
                }
            };
        match event {
            Ok(AgentEvent::SessionStarted { session_id, .. }) => {
                turn.session_id = Some(session_id);
            }
            Ok(AgentEvent::ProviderCheckpoint { checkpoint_id }) => {
                eprintln!("CHECKPOINT {checkpoint_id}");
                turn.checkpoint = Some(checkpoint_id);
            }
            Ok(AgentEvent::TextDelta { text }) => turn.text.push_str(&text),
            Ok(AgentEvent::Error { message }) => eprintln!("CHIP {message}"),
            Ok(AgentEvent::Done {
                status,
                error,
                session_id,
                ..
            }) => {
                turn.status = Some(status);
                turn.error = error;
                if turn.session_id.is_none() {
                    turn.session_id = session_id;
                }
                // Keep draining: the adapter's teardown closes the stream.
            }
            Ok(_) => {}
            Err(error) => turn.error = Some(error.to_string()),
        }
    }
    eprintln!(
        "TURN status={:?} session={:?} checkpoint={:?} error={:?} text={:?}",
        turn.status,
        turn.session_id,
        turn.checkpoint,
        turn.error,
        turn.text.trim()
    );
    turn
}

struct Checks {
    failed: u32,
}

impl Checks {
    fn check(&mut self, ok: bool, what: &str) {
        eprintln!("CHECK {} {what}", if ok { "ok" } else { "FAIL" });
        if !ok {
            self.failed += 1;
        }
    }
}

fn scope(session_id: &str, cwd: &str) -> NativeHistoryScope {
    NativeHistoryScope {
        session_id: session_id.into(),
        cwd: cwd.into(),
        provider_home: None,
    }
}

fn fork_request(source: &str, cwd: &str, boundary: ProviderForkBoundary) -> ProviderForkRequest {
    ProviderForkRequest {
        source_session_id: source.into(),
        boundary,
        cwd: cwd.into(),
        model: None,
        target_name: None,
        provider_home: None,
        force_file_auth: false,
    }
}

fn digest(path: &std::path::Path) -> Option<String> {
    use sha2::Digest as _;
    std::fs::read(path)
        .ok()
        .map(|bytes| format!("{:x}", sha2::Sha256::digest(&bytes)))
}

async fn scenario(harness: &OpencodeHarness, model: Option<String>, cwd: &str) -> u32 {
    let mut checks = Checks { failed: 0 };
    let first = run_turn(
        harness,
        cwd,
        model.clone(),
        "Reply with exactly: MARKER-ALPHA",
        None,
    )
    .await;
    let Some(session) = first.session_id.clone() else {
        checks.check(false, "turn 1 started a session");
        return checks.failed;
    };
    let second = run_turn(
        harness,
        cwd,
        model.clone(),
        "Use your write tool to create a file named beta.txt containing the single word BETA. \
         Then reply with exactly: MARKER-BETA",
        Some(session.clone()),
    )
    .await;
    let third = run_turn(
        harness,
        cwd,
        model.clone(),
        "Reply with exactly: MARKER-GAMMA",
        Some(session.clone()),
    )
    .await;
    let checkpoints: Vec<String> = [&first, &second, &third]
        .iter()
        .filter_map(|turn| turn.checkpoint.clone())
        .collect();
    checks.check(
        [&first, &second, &third]
            .iter()
            .all(|turn| turn.status == Some(DoneStatus::Completed)),
        "three marker turns completed",
    );
    checks.check(
        checkpoints.len() == 3
            && checkpoints
                .iter()
                .all(|id| id.starts_with("msg_") && id != "usage"),
        "every turn persisted a real msg_ checkpoint (never \"usage\")",
    );
    let beta = std::path::Path::new(cwd).join("beta.txt");
    let beta_digest = digest(&beta);
    checks.check(beta_digest.is_some(), "turn 2 wrote beta.txt");

    let source_turns = harness.native_history_turns(scope(&session, cwd)).await;
    eprintln!("SOURCE TURNS {source_turns:?}");
    checks.check(
        source_turns
            .as_ref()
            .is_ok_and(|turns| turns.turn_starts == checkpoints && !turns.compacted),
        "native_history_turns lists exactly the persisted checkpoints",
    );
    if checkpoints.len() != 3 {
        return checks.failed + 1;
    }
    let (cp1, cp2, cp3) = (&checkpoints[0], &checkpoints[1], &checkpoints[2]);

    // Forks: before turn 2 (a user-row fork), through turn 2 (an
    // assistant-row fork) and at the tip.
    for (label, boundary, expect) in [
        (
            "before turn 2",
            ProviderForkBoundary::BeforeCheckpoint {
                checkpoint_id: cp2.clone(),
            },
            vec![cp1.clone()],
        ),
        (
            "through turn 2",
            ProviderForkBoundary::ThroughCheckpoint {
                checkpoint_id: cp2.clone(),
            },
            vec![cp1.clone(), cp2.clone()],
        ),
        (
            "at the tip",
            ProviderForkBoundary::Tip,
            vec![cp1.clone(), cp2.clone(), cp3.clone()],
        ),
    ] {
        let fork = harness
            .fork_session_staged(fork_request(&session, cwd, boundary), &|progress| {
                eprintln!("FORK PROGRESS {progress:?}");
                Ok(())
            })
            .await;
        eprintln!("FORK {label}: {fork:?}");
        let Ok(fork) = fork else {
            checks.check(false, &format!("fork {label} succeeded"));
            continue;
        };
        let mapped: Option<Vec<String>> = expect
            .iter()
            .map(|id| fork.checkpoint_map.get(id).cloned())
            .collect();
        let fork_turns = harness
            .native_history_turns(scope(&fork.provider_session_id, cwd))
            .await;
        eprintln!("FORK {label} TURNS {fork_turns:?}");
        checks.check(
            mapped.is_some_and(|mapped| {
                fork_turns
                    .as_ref()
                    .is_ok_and(|turns| turns.turn_starts == mapped)
            }),
            &format!("fork {label} holds exactly the expected turns, mapped to its own ids"),
        );
        if label == "before turn 2" {
            let follow = run_turn(
                harness,
                cwd,
                model.clone(),
                "Which MARKER words have you replied with so far in this conversation? \
                 List them on one line.",
                Some(fork.provider_session_id.clone()),
            )
            .await;
            checks.check(
                follow.status == Some(DoneStatus::Completed),
                "a turn on the before-turn-2 fork runs",
            );
            eprintln!(
                "OBSERVE fork-recall mentions ALPHA={} BETA={} GAMMA={}",
                follow.text.contains("ALPHA"),
                follow.text.contains("BETA"),
                follow.text.contains("GAMMA")
            );
        }
    }
    let after_forks = harness.native_history_turns(scope(&session, cwd)).await;
    checks.check(
        after_forks
            .as_ref()
            .is_ok_and(|turns| turns.turn_starts == checkpoints),
        "the source turn list is unchanged by the forks",
    );

    // A fork at an id that is not a turn start is refused before mutation.
    let refused = harness
        .fork_session_staged(
            fork_request(
                &session,
                cwd,
                ProviderForkBoundary::BeforeCheckpoint {
                    checkpoint_id: "msg_not_a_turn".into(),
                },
            ),
            &|progress| {
                eprintln!("UNEXPECTED FORK PROGRESS {progress:?}");
                Ok(())
            },
        )
        .await;
    eprintln!("FORK refused: {refused:?}");
    checks.check(
        refused
            .as_ref()
            .is_err_and(|error| error.to_string().contains("not a turn start")),
        "a fork at a non-turn checkpoint is refused before any mutation",
    );

    // Same-session rewind before turn 2, files kept.
    let rewound = harness
        .rewind_native_conversation(
            NativeRewindRequest {
                scope: scope(&session, cwd),
                target_checkpoint: cp2.clone(),
                last_seen_checkpoint: cp3.clone(),
            },
            &|progress| eprintln!("REWIND PROGRESS {progress:?}"),
        )
        .await;
    eprintln!("REWIND {rewound:?}");
    checks.check(
        matches!(rewound, Ok(NativeRewindOutcome::Rewound)),
        "rewind before turn 2 was applied",
    );
    let after_rewind = harness.native_history_turns(scope(&session, cwd)).await;
    eprintln!("REWOUND TURNS {after_rewind:?}");
    checks.check(
        after_rewind
            .as_ref()
            .is_ok_and(|turns| turns.turn_starts == vec![cp1.clone()]),
        "after the rewind only turn 1 remains",
    );
    checks.check(
        digest(&beta) == beta_digest,
        "beta.txt is unchanged by the rewind (files kept)",
    );
    let stale = harness
        .rewind_native_conversation(
            NativeRewindRequest {
                scope: scope(&session, cwd),
                target_checkpoint: cp2.clone(),
                last_seen_checkpoint: cp3.clone(),
            },
            &|progress| eprintln!("UNEXPECTED REWIND PROGRESS {progress:?}"),
        )
        .await;
    eprintln!("REWIND stale: {stale:?}");
    checks.check(
        matches!(stale, Ok(NativeRewindOutcome::Refused { .. })),
        "a rewind to a removed turn is refused before any mutation",
    );
    let follow = run_turn(
        harness,
        cwd,
        model.clone(),
        "Which MARKER words have you replied with so far in this conversation? \
         List them on one line.",
        Some(session.clone()),
    )
    .await;
    checks.check(
        follow.status == Some(DoneStatus::Completed) && follow.checkpoint.is_some(),
        "the rewound session continues and persists a new checkpoint",
    );
    eprintln!(
        "OBSERVE rewound-recall mentions ALPHA={} BETA={} GAMMA={}",
        follow.text.contains("ALPHA"),
        follow.text.contains("BETA"),
        follow.text.contains("GAMMA")
    );
    if let Some(next) = follow.checkpoint.clone() {
        let turns = harness.native_history_turns(scope(&session, cwd)).await;
        checks.check(
            turns
                .as_ref()
                .is_ok_and(|turns| turns.turn_starts == vec![cp1.clone(), next]),
            "the continued session lists turn 1 and the new turn",
        );
    }
    eprintln!("SESSION {session}");
    checks.failed
}

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let exe = args.next().expect("executable");
    let model = args.next().filter(|m| m.contains('/'));
    let mode = args.next().unwrap_or_else(|| "scenario".into());
    let cwd = args.next().expect("workspace");
    std::fs::create_dir_all(&cwd).expect("create workspace");
    let harness = OpencodeHarness::new().with_executable(exe);
    let failed = match mode.as_str() {
        "scenario" => scenario(&harness, model, &cwd).await,
        "turn" => {
            let prompt = args.next().expect("prompt");
            let resume = args.next();
            let turn = run_turn(&harness, &cwd, model, &prompt, resume).await;
            u32::from(turn.status != Some(DoneStatus::Completed))
        }
        other => panic!("unknown mode {other}"),
    };
    if let Some(resolution) = luna_harness::opencode::last_opencode_resolution() {
        let version = resolution.version.as_deref().unwrap_or("unknown");
        eprintln!("RESOLVED {} version={version}", resolution.path.display());
        if let Some(report) = luna_harness::opencode::opencode_compat_report(version) {
            eprintln!("COMPAT {report:?}");
        }
    }
    eprintln!("RESULT failed={failed}");
    std::process::exit(if failed == 0 { 0 } else { 1 });
}
