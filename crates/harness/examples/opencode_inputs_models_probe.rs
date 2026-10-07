//! Live probe for OC-09/OC-10 (inputs, models, usage, compaction) through the
//! native OpenCode driver. Run it in an isolated HOME/XDG (the QA runner);
//! it never touches the user's OpenCode storage.
//!
//!     cargo run -p luna-harness --example opencode_inputs_models_probe -- \
//!         <opencode-exe> <image-model> <switch-model> [variant]
//!
//! Steps, one OpenCode session throughout:
//! 1. catalog: `models()` — ids, provider names, variant options, and the
//!    image capability each model reports.
//! 2. image: an image-only turn (the composer's own image-only text and path
//!    trailer) with a generated PNG fixture outside the workspace, removed
//!    once the model starts answering, so the reply can name its colours only
//!    from the native `files[]` image.
//!    steer: a text turn with a queued steer carrying a second (JPEG) image,
//!    posted at the boundary; the reply must name that image's colour.
//! 3. switch: a follow-up run on the SAME session with another model (and
//!    an optional variant); prints the ProviderAccounting runtime readback.
//! 4. compact: `compact_context` on the session with no live run (a fresh
//!    server is booted); succeeds only after `session.compaction.ended`.
//! 5. reject: a text-only model (when the catalog has one) is refused before
//!    anything boots; an SVG is refused by admission.
//!
//! Exit 0 only when every step met its expectation.

use std::time::{Duration, Instant};

use futures::StreamExt;
use luna_harness::{
    CancellationToken, ContextCompactRequest, Harness, HarnessError, OpencodeHarness, RunControls,
};
use luna_proto::{AgentEvent, DoneStatus, RunRequest, SandboxLevel};
use tokio::sync::mpsc;

const IMAGE_ONLY_TEXT: &str = "See the attached image(s).";
const TRAILER: &str = "Attached images (local files — open them to view):";

fn request(
    prompt: String,
    model: &str,
    cwd: &str,
    attachments: Vec<String>,
    resume: Option<String>,
    variant: Option<&str>,
) -> RunRequest {
    let mut model_options = serde_json::Map::new();
    if let Some(variant) = variant {
        model_options.insert("variant".into(), variant.into());
    }
    RunRequest {
        review: None,
        goal: None,
        input_items: Vec::new(),
        prompt,
        harness: None,
        provider_account_id: None,
        model: Some(model.into()),
        reasoning: None,
        model_options,
        cwd: cwd.into(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: true,
        permission_mode: luna_proto::PermissionMode::Ask,
        parent_permission_ceiling: None,
        environment_permission_ceiling: None,
        permission_policy: None,
        attachments,
        resume,
        worktree: None,
    }
}

fn controls() -> RunControls {
    let (_steer_tx, steering) = mpsc::channel(8);
    RunControls {
        request_input: Box::new(|_| panic!("probe must not ask for input")),
        steering,
        interrupt: CancellationToken::new(),
        invoke_luna_tool: None,
    }
}

struct Turn {
    status: Option<DoneStatus>,
    text: String,
    session: Option<String>,
    /// Tool results that succeeded (a denied or failed call does not count).
    tools_ok: u32,
    /// Tool calls the model started before `remove_after_admission` ran.
    tools_before_removal: u32,
}

/// Remove a staged file once the prompt is admitted (the first model output
/// proves the server already read it): from then on the image exists only
/// as the server's materialized `files[]` copy, so a reply naming its
/// colours cannot come from opening the file with a tool.
fn remove_staged(path: &mut Option<String>, label: &str) {
    if let Some(path) = path.take() {
        let removed = std::fs::remove_file(&path).is_ok();
        eprintln!("[{label}] staged file removed after admission: {removed}");
    }
}

fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default()
}

async fn run_turn(
    harness: &OpencodeHarness,
    label: &str,
    request: RunRequest,
    controls: RunControls,
) -> Turn {
    run_turn_removing(harness, label, request, controls, None).await
}

async fn run_turn_removing(
    harness: &OpencodeHarness,
    label: &str,
    request: RunRequest,
    controls: RunControls,
    mut remove_after_admission: Option<String>,
) -> Turn {
    let started = Instant::now();
    let mut stream = match harness.run(request, controls).await {
        Ok(stream) => stream,
        Err(error) => {
            eprintln!("[{label}] run refused: {error}");
            return Turn {
                status: None,
                text: String::new(),
                session: None,
                tools_ok: 0,
                tools_before_removal: 0,
            };
        }
    };
    let mut turn = Turn {
        status: None,
        text: String::new(),
        session: None,
        tools_ok: 0,
        tools_before_removal: 0,
    };
    loop {
        let ev = match tokio::time::timeout(Duration::from_secs(180), stream.next()).await {
            Ok(Some(ev)) => ev,
            Ok(None) => break,
            Err(_) => {
                eprintln!("[{label}] timed out");
                break;
            }
        };
        match ev {
            Ok(AgentEvent::SessionStarted { session_id, .. }) => {
                eprintln!("[{label}] session {session_id}");
                turn.session = Some(session_id);
            }
            Ok(AgentEvent::TextDelta { text }) => {
                remove_staged(&mut remove_after_admission, label);
                turn.text.push_str(&text);
            }
            Ok(AgentEvent::ReasoningDelta { .. }) => {
                remove_staged(&mut remove_after_admission, label);
            }
            Ok(AgentEvent::ToolCall { call, .. }) => {
                if remove_after_admission.is_some() {
                    turn.tools_before_removal += 1;
                }
                remove_staged(&mut remove_after_admission, label);
                eprintln!("[{label}] TOOL {call:?}");
            }
            Ok(AgentEvent::ToolResult {
                is_error, output, ..
            }) => {
                if !is_error {
                    turn.tools_ok += 1;
                }
                eprintln!(
                    "[{label}] TOOL RESULT error={is_error} {:?}",
                    output
                        .as_deref()
                        .unwrap_or_default()
                        .chars()
                        .take(160)
                        .collect::<String>()
                );
            }
            Ok(AgentEvent::Steered { .. }) => {
                eprintln!("[{label}] STEERED (boundary prompt posted)")
            }
            Ok(AgentEvent::ProviderAccounting { report }) => {
                eprintln!(
                    "[{label}] ACCOUNTING {}",
                    serde_json::to_string(&report).unwrap_or_default()
                );
            }
            Ok(AgentEvent::ContextUsage { tokens, window }) => {
                eprintln!("[{label}] CONTEXT tokens={tokens:?} window={window:?}");
            }
            Ok(AgentEvent::ContextCompaction {
                operation_id,
                status,
                ..
            }) => eprintln!("[{label}] COMPACTION {operation_id} {status:?}"),
            Ok(AgentEvent::Done { status, error, .. }) => {
                if let Some(error) = error {
                    eprintln!("[{label}] ERR {error}");
                }
                turn.status = Some(status);
            }
            Ok(AgentEvent::Error { message }) => eprintln!("[{label}] CHIP {message}"),
            Ok(AgentEvent::Usage {
                input_tokens,
                output_tokens,
            }) => eprintln!("[{label}] USAGE {input_tokens}/{output_tokens}"),
            Ok(_) => {}
            Err(error) => eprintln!("[{label}] ERR {error}"),
        }
    }
    eprintln!(
        "[{label}] done {:?} in {:?}: {:?}",
        turn.status,
        started.elapsed(),
        turn.text.trim()
    );
    turn
}

/// A 256x256 PNG, left half pure green and right half pure blue.
fn write_fixture(dir: &std::path::Path) -> String {
    let image = image::RgbImage::from_fn(256, 256, |x, _| {
        if x < 128 {
            image::Rgb([0, 200, 0])
        } else {
            image::Rgb([0, 0, 230])
        }
    });
    // A space and a `#` in the name exercise the `file://` percent-encoding.
    let path = dir.join("oc09 fixture #1.png");
    image.save(&path).expect("write fixture");
    let bytes = std::fs::read(&path).expect("read fixture");
    use sha2::Digest as _;
    eprintln!(
        "[image] fixture {} {}B sha256:{:x}",
        path.display(),
        bytes.len(),
        sha2::Sha256::digest(&bytes)
    );
    path.to_string_lossy().into_owned()
}

/// A 128x128 JPEG filled with pure red (the steer route's fixture).
fn write_second_fixture(dir: &std::path::Path) -> String {
    let image = image::RgbImage::from_pixel(128, 128, image::Rgb([220, 0, 0]));
    let path = dir.join("steer-fixture.jpg");
    image.save(&path).expect("write fixture");
    let bytes = std::fs::read(&path).expect("read fixture");
    use sha2::Digest as _;
    eprintln!(
        "[steer] fixture {} {}B sha256:{:x}",
        path.display(),
        bytes.len(),
        sha2::Sha256::digest(&bytes)
    );
    path.to_string_lossy().into_owned()
}

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let exe = args.next().expect("opencode executable");
    let image_model = args.next().expect("image model (provider/model)");
    let switch_model = args.next().expect("switch model (provider/model)");
    let variant = args.next();
    let harness = OpencodeHarness::new().with_executable(&exe);
    let workspace = tempfile::tempdir().expect("workspace");
    let cwd = workspace.path().to_str().expect("utf-8").to_owned();
    let mut ok = true;

    // 1. catalog
    let started = Instant::now();
    let mut text_only = None;
    match harness.models().await {
        Ok(models) => {
            for model in &models {
                let images = luna_harness::opencode::model_accepts_images(&model.id);
                if images == Some(false) && text_only.is_none() {
                    text_only = Some(model.id.clone());
                }
                eprintln!(
                    "[catalog] {:44} {:30} desc={:?} levels={:?} images={images:?} options={}",
                    model.id,
                    model.label,
                    model.description.as_deref().unwrap_or(""),
                    model.reasoning_levels,
                    serde_json::to_string(&model.options).unwrap_or_default()
                );
            }
            eprintln!(
                "[catalog] {} models in {:?}",
                models.len(),
                started.elapsed()
            );
        }
        Err(error) => {
            eprintln!("[catalog] failed: {error}");
            ok = false;
        }
    }

    // 2. image-only turn. The fixture lives OUTSIDE the workspace and is
    // removed as soon as the model starts answering (the server read it when
    // it admitted the prompt), so the reply can only come from the native
    // `files[]` image. Approvals are granted: a denial would end OpenCode's
    // execution (OC-04 owns Deny vs Cancel); a late read simply fails.
    let images = tempfile::tempdir().expect("fixture dir");
    let fixture = write_fixture(images.path());
    let prompt = format!("{IMAGE_ONLY_TEXT}\n\n{TRAILER}\n- {fixture}");
    let image_request = request(
        prompt,
        &image_model,
        &cwd,
        vec![fixture.clone()],
        None,
        None,
    );
    let image_turn = run_turn_removing(
        &harness,
        "image",
        image_request,
        controls(),
        Some(fixture.clone()),
    )
    .await;
    let reply = image_turn.text.to_lowercase();
    let saw_image = reply.contains("green") || reply.contains("blue");
    eprintln!(
        "[image] reply names a fixture colour: {saw_image}; tool calls before the file was removed: {}",
        image_turn.tools_before_removal
    );
    ok &= image_turn.status == Some(DoneStatus::Completed)
        && saw_image
        && image_turn.tools_before_removal == 0;
    let Some(session) = image_turn.session.clone() else {
        eprintln!("--- no session; stopping");
        std::process::exit(1);
    };

    // 2b. a steer carrying an image (the boundary route): a text turn, then
    // a queued steer whose image rides `files[]` when the turn goes idle.
    let second = write_second_fixture(images.path());
    let (steer_tx, steering) = mpsc::channel(8);
    let steer_controls = RunControls {
        request_input: Box::new(|questions| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            let _ = tx.send(
                questions
                    .into_iter()
                    .map(|q| luna_proto::UserInputAnswer {
                        question_id: q.id,
                        labels: vec!["Deny".into()],
                    })
                    .collect(),
            );
            rx
        }),
        steering,
        interrupt: CancellationToken::new(),
        invoke_luna_tool: None,
    };
    let steer_prompt = format!(
        "Without using any tools, name the single colour that fills the attached image. Reply with one word.\n\n{TRAILER}\n- {second}"
    );
    steer_tx
        .send(luna_harness::SteerMessage {
            prompt: steer_prompt,
            attachments: vec![second.clone()],
            input_items: Vec::new(),
            message_id: Some("probe-steer-1".into()),
            delivery: luna_harness::SteerDelivery::AllowFollowUpTurn,
            acceptance: None,
            goal: None,
        })
        .await
        .expect("queue steer");
    let mut steer_request = request(
        "Reply with exactly: FIRST".into(),
        &image_model,
        &cwd,
        Vec::new(),
        Some(session.clone()),
        None,
    );
    steer_request.auto_approve = false;
    let steer_turn = run_turn(&harness, "steer", steer_request, steer_controls).await;
    drop(steer_tx);
    let steer_reply = steer_turn.text.to_lowercase();
    let saw_steer_image = steer_reply.contains("red");
    eprintln!("[steer] steered reply names the second fixture's colour (red): {saw_steer_image}");
    ok &= steer_turn.status == Some(DoneStatus::Completed) && saw_steer_image;

    // 3. model/variant switch on the same session
    let switch_turn = run_turn(
        &harness,
        "switch",
        request(
            "Reply with exactly: SWITCHED".into(),
            &switch_model,
            &cwd,
            Vec::new(),
            Some(session.clone()),
            variant.as_deref(),
        ),
        controls(),
    )
    .await;
    ok &= switch_turn.status == Some(DoneStatus::Completed);

    // 4. manual compaction with no live run (a server is booted for it)
    let started = Instant::now();
    eprintln!("[compact] called at t_ms={}", now_ms());
    let compacted = harness
        .compact_context(ContextCompactRequest {
            session_id: session.clone(),
            cwd: cwd.clone(),
            model: Some(switch_model.clone()),
        })
        .await;
    eprintln!(
        "[compact] returned at t_ms={} result {:?} in {:?}",
        now_ms(),
        compacted.as_ref().map_err(ToString::to_string),
        started.elapsed()
    );
    ok &= compacted.is_ok();

    // 4b. manual compaction while a turn runs: it goes to the live run's
    // server (no second server), steers in at a step boundary, and the turn
    // still completes once.
    let (live_turn, live_compaction) = tokio::join!(
        run_turn(
            &harness,
            "live",
            request(
                "Count from 1 to 30, one number per line, then write DONE.".into(),
                &switch_model,
                &cwd,
                Vec::new(),
                Some(session.clone()),
                None,
            ),
            controls(),
        ),
        async {
            tokio::time::sleep(Duration::from_millis(1500)).await;
            let started = Instant::now();
            eprintln!("[live-compact] called at t_ms={}", now_ms());
            let result = harness
                .compact_context(ContextCompactRequest {
                    session_id: session.clone(),
                    cwd: cwd.clone(),
                    model: Some(switch_model.clone()),
                })
                .await;
            eprintln!(
                "[live-compact] returned at t_ms={} result {:?} in {:?}",
                now_ms(),
                result.as_ref().map_err(ToString::to_string),
                started.elapsed()
            );
            result
        }
    );
    ok &= live_turn.status == Some(DoneStatus::Completed) && live_compaction.is_ok();

    // 5. rejections before anything boots
    if let Some(text_only) = text_only {
        let started = Instant::now();
        let refused = harness
            .run(
                request(
                    "What is this?".into(),
                    &text_only,
                    &cwd,
                    vec![fixture.clone()],
                    None,
                    None,
                ),
                controls(),
            )
            .await;
        let rejected = matches!(&refused, Err(HarnessError::RejectedBeforeAcceptance(_)));
        eprintln!(
            "[reject] text-only {text_only}: rejected-before-acceptance={rejected} in {:?}: {}",
            started.elapsed(),
            refused.err().map(|e| e.to_string()).unwrap_or_default()
        );
        ok &= rejected;
    } else {
        eprintln!("[reject] no text-only model in this catalog; skipped");
    }
    let svg = workspace.path().join("vector.svg");
    std::fs::write(&svg, "<svg xmlns=\"http://www.w3.org/2000/svg\"/>").expect("svg");
    let refused = luna_harness::opencode::validate_inputs(
        Some(&image_model),
        &[svg.to_string_lossy().into_owned()],
        &[],
    );
    eprintln!(
        "[reject] svg: {:?}",
        refused.as_ref().map_err(ToString::to_string)
    );
    ok &= matches!(refused, Err(HarnessError::RejectedBeforeAcceptance(_)));

    eprintln!("--- all steps ok: {ok}");
    if !ok {
        std::process::exit(1);
    }
}
