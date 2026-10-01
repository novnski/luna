//! CC-04 / E26: Claude Code image and typed-skill input through the real
//! adapter process boundary. `fixtures/claude-input-peer.py` stands in for
//! the 2.1.286 CLI (frame shapes from the installed binary) and records every
//! stdin frame; no model runs. Set `LUNA_CC04_EVIDENCE=<dir>` to keep the
//! recorded frames and a hash manifest.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine as _;
use futures::StreamExt;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::{mpsc, oneshot};

use luna_harness::{
    CancellationToken, ClaudeHarness, Harness, HarnessError, RunControls, SteerAcceptance,
    SteerDelivery, SteerMessage,
};
use luna_proto::{AgentEvent, ComposerInput, DoneStatus, ReasoningLevel, RunRequest, SandboxLevel};

const PNG_1X1: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jRZkAAAAASUVORK5CYII=";

fn png() -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode(PNG_1X1)
        .unwrap()
}

/// Minimal baseline JPEG structure (SOI, APP0, SOF0, SOS, EOI) with
/// `components` channels: 3 = YCbCr, 4 = CMYK.
fn jpeg(components: u8) -> Vec<u8> {
    let mut bytes = vec![0xFF, 0xD8];
    bytes.extend([0xFF, 0xE0, 0x00, 0x10]);
    bytes.extend(b"JFIF\0");
    bytes.extend([1, 1, 0, 0, 1, 0, 1, 0, 0]);
    let len = 8 + 3 * u16::from(components);
    bytes.extend([0xFF, 0xC0]);
    bytes.extend(len.to_be_bytes());
    bytes.extend([8, 0, 1, 0, 1, components]);
    for id in 1..=components {
        bytes.extend([id, 0x11, 0]);
    }
    bytes.extend([0xFF, 0xDA, 0x00, 0x08, 1, 1, 0, 0, 0x3F, 0]);
    bytes.extend([0x12, 0x34, 0xFF, 0xD9]);
    bytes
}

fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn peer() -> PathBuf {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude-input-peer.py");
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
    path
}

/// A working directory the peer records into (`.cc04-peer` opt-in marker).
fn peer_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(".cc04-peer"), b"").unwrap();
    dir
}

fn harness() -> ClaudeHarness {
    ClaudeHarness::new()
        .with_executable(peer())
        .with_graces(Duration::from_millis(200), Duration::from_millis(200))
}

fn request(cwd: &Path, prompt: &str, attachments: Vec<String>) -> RunRequest {
    RunRequest {
        review: None,
        input_items: Vec::new(),
        prompt: prompt.into(),
        harness: None,
        provider_account_id: None,
        model: None,
        reasoning: None,
        model_options: serde_json::Map::new(),
        cwd: cwd.to_string_lossy().into(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: false,
        permission_mode: luna_proto::PermissionMode::Ask,
        parent_permission_ceiling: None,
        environment_permission_ceiling: None,
        permission_policy: None,
        attachments,
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

/// Run until `dones` terminal events, then close stdin and drain to EOF.
async fn run_turns(
    harness: &ClaudeHarness,
    request: RunRequest,
    dones: usize,
    steers: Vec<SteerMessage>,
) -> Vec<AgentEvent> {
    let (controls, steer_tx) = controls();
    let mut stream = harness.run(request, controls).await.expect("run starts");
    for steer in steers {
        steer_tx.send(steer).await.expect("steer queued");
    }
    let mut events = Vec::new();
    let mut seen = 0;
    tokio::time::timeout(Duration::from_secs(15), async {
        while seen < dones {
            let event = stream.next().await.expect("stream open").expect("event");
            if matches!(event, AgentEvent::Done { .. }) {
                seen += 1;
            }
            events.push(event);
        }
        drop(steer_tx);
        while let Some(event) = stream.next().await {
            events.push(event.expect("event"));
        }
    })
    .await
    .expect("turns finished in time");
    events
}

fn log(cwd: &Path) -> Vec<Value> {
    std::fs::read_to_string(cwd.join("claude-input-peer.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn user_frames(cwd: &Path) -> Vec<Value> {
    log(cwd)
        .into_iter()
        .filter(|entry| entry["kind"] == "frame" && entry["frame"]["type"] == "user")
        .map(|entry| entry["frame"].clone())
        .collect()
}

/// `[("image", media/sha) | ("text", text)]` for one recorded user frame.
fn blocks(frame: &Value) -> Vec<(String, String)> {
    match &frame["message"]["content"] {
        Value::String(text) => vec![("string".into(), text.clone())],
        Value::Array(items) => items
            .iter()
            .map(|block| match block["type"].as_str().unwrap() {
                "image" => {
                    let data = base64::engine::general_purpose::STANDARD
                        .decode(block["source"]["data"].as_str().unwrap())
                        .unwrap();
                    (
                        "image".into(),
                        format!(
                            "{} {}",
                            block["source"]["media_type"].as_str().unwrap(),
                            sha(&data)
                        ),
                    )
                }
                kind => (
                    kind.into(),
                    block["text"].as_str().unwrap_or_default().into(),
                ),
            })
            .collect(),
        other => panic!("unexpected content {other}"),
    }
}

fn evidence(name: &str, value: Value) {
    if let Ok(dir) = std::env::var("LUNA_CC04_EVIDENCE") {
        let dir = Path::new(&dir).join("harness");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(format!("{name}.json")),
            serde_json::to_vec_pretty(&value).unwrap(),
        )
        .unwrap();
    }
}

fn write(dir: &Path, name: &str, bytes: &[u8]) -> String {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path.to_string_lossy().into()
}

fn completed(events: &[AgentEvent]) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(
                event,
                AgentEvent::Done {
                    status: DoneStatus::Completed,
                    ..
                }
            )
        })
        .count()
}

#[tokio::test]
async fn first_turn_sends_validated_bytes_in_order_and_image_only_without_text() {
    let root = peer_dir();
    let files = tempfile::tempdir().unwrap();
    let png_path = write(files.path(), "shot.png", &png());
    // JPEG bytes behind a .png name: the media type must come from the bytes.
    let misnamed = write(files.path(), "photo.png", &jpeg(3));
    let attachments = vec![png_path.clone(), misnamed.clone(), png_path.clone()];

    let events = run_turns(
        &harness(),
        request(root.path(), "What differs?", attachments),
        1,
        Vec::new(),
    )
    .await;
    assert_eq!(completed(&events), 1, "{events:?}");
    let frames = user_frames(root.path());
    assert_eq!(frames.len(), 1);
    let first = blocks(&frames[0]);
    assert_eq!(
        first,
        vec![
            ("image".into(), format!("image/png {}", sha(&png()))),
            ("image".into(), format!("image/jpeg {}", sha(&jpeg(3)))),
            ("text".into(), "What differs?".into()),
        ],
        "duplicate path sent once, bytes decide the media type, text last"
    );

    // Image-only over RPC: no empty text block.
    let image_only = peer_dir();
    run_turns(
        &harness(),
        request(image_only.path(), "", vec![png_path.clone()]),
        1,
        Vec::new(),
    )
    .await;
    let only = blocks(&user_frames(image_only.path())[0]);
    assert_eq!(
        only,
        vec![("image".into(), format!("image/png {}", sha(&png())))]
    );

    // Text-only turns keep the historical plain-string content.
    let text_only = peer_dir();
    run_turns(
        &harness(),
        request(text_only.path(), "plain", Vec::new()),
        1,
        Vec::new(),
    )
    .await;
    assert_eq!(
        blocks(&user_frames(text_only.path())[0]),
        vec![("string".into(), "plain".into())]
    );

    // Title generation never carries the turn's images.
    let title = peer_dir();
    let (title_controls, _steer) = controls();
    let stream = harness()
        .run_title(
            request(title.path(), "Title this", vec![png_path.clone()]),
            title_controls,
        )
        .await
        .expect("title run");
    drop(_steer);
    let _ = tokio::time::timeout(Duration::from_secs(10), stream.collect::<Vec<_>>()).await;
    assert_eq!(
        blocks(&user_frames(title.path())[0]),
        vec![("string".into(), "Title this".into())]
    );

    evidence(
        "first-turn",
        json!({"textAndImages": first, "imageOnly": only, "fixturePngSha256": sha(&png()),
               "fixtureJpegSha256": sha(&jpeg(3)), "frames": log(root.path()), "realProvider": false}),
    );
}

#[tokio::test]
async fn invalid_inputs_reject_before_any_process_or_stdin_line() {
    let files = tempfile::tempdir().unwrap();
    let dir = files.path();
    std::fs::create_dir(dir.join("folder.png")).unwrap();
    let mut oversized = std::fs::File::create(dir.join("huge.png")).unwrap();
    std::io::Write::write_all(&mut oversized, &png()[..33]).unwrap();
    oversized.set_len(21 * 1024 * 1024).unwrap();
    for name in ["big-a.png", "big-b.png"] {
        let mut file = std::fs::File::create(dir.join(name)).unwrap();
        std::io::Write::write_all(&mut file, &png()[..33]).unwrap();
        file.set_len(17 * 1024 * 1024).unwrap();
    }
    let codex_skill = dir.join(".codex/skills/fixture-skill/SKILL.md");
    std::fs::create_dir_all(codex_skill.parent().unwrap()).unwrap();
    std::fs::write(&codex_skill, "---\nname: fixture-skill\n---\n").unwrap();
    let claude_skill = dir.join(".claude/skills/fixture-skill/SKILL.md");
    std::fs::create_dir_all(claude_skill.parent().unwrap()).unwrap();
    std::fs::write(&claude_skill, "---\nname: fixture-skill\n---\n").unwrap();
    let path = |name: &str| dir.join(name).to_string_lossy().into_owned();

    let cases: Vec<(&str, Vec<String>, Vec<ComposerInput>, &str)> = vec![
        (
            "missing",
            vec![path("gone.png")],
            vec![],
            "not available on the computer running this task",
        ),
        (
            "relative",
            vec!["shot.png".into()],
            vec![],
            "needs an absolute path",
        ),
        (
            "pending",
            vec!["pending://upload-1/shot.png".into()],
            vec![],
            "has not arrived",
        ),
        (
            "directory",
            vec![path("folder.png")],
            vec![],
            "not a regular file",
        ),
        (
            "empty",
            vec![write(dir, "empty.png", b"")],
            vec![],
            "Image is empty",
        ),
        (
            "svg",
            vec![write(
                dir,
                "art.png",
                b"<?xml version=\"1.0\"?><svg xmlns=\"http://www.w3.org/2000/svg\"/>",
            )],
            vec![],
            "SVG vector artwork",
        ),
        (
            "tiff",
            vec![write(dir, "scan.tiff", b"II*\0\x08\0\0\0rest")],
            vec![],
            "a TIFF image",
        ),
        (
            "bmp",
            vec![write(dir, "old.bmp", b"BM\x3a\0\0\0\0\0\0\0")],
            vec![],
            "a BMP image",
        ),
        (
            "heic",
            vec![write(
                dir,
                "phone.heic",
                b"\0\0\0\x18ftypheic\0\0\0\0mif1heic",
            )],
            vec![],
            "HEIC/HEIF",
        ),
        (
            "unknown",
            vec![write(dir, "notes.png", b"just some text")],
            vec![],
            "not a recognised image",
        ),
        (
            "truncated-png",
            vec![write(dir, "cut.png", &png()[..8])],
            vec![],
            "damaged or incomplete",
        ),
        (
            "truncated-jpeg",
            vec![write(dir, "cut.jpg", &jpeg(3)[..24])],
            vec![],
            "damaged or incomplete",
        ),
        (
            "cmyk-jpeg",
            vec![write(dir, "print.jpg", &jpeg(4))],
            vec![],
            "CMYK JPEG",
        ),
        (
            "oversized",
            vec![path("huge.png")],
            vec![],
            "too large for Claude Code",
        ),
        (
            "aggregate",
            vec![path("big-a.png"), path("big-b.png")],
            vec![],
            "too large to send together",
        ),
        (
            "mention",
            vec![],
            vec![ComposerInput::Mention {
                name: "Linear".into(),
                path: "app://linear".into(),
            }],
            "Codex integrations",
        ),
        (
            "codex-skill",
            vec![],
            vec![ComposerInput::Skill {
                name: "fixture-skill".into(),
                path: codex_skill.to_string_lossy().into(),
            }],
            "selected from Codex's catalog",
        ),
        (
            "missing-skill",
            vec![],
            vec![ComposerInput::Skill {
                name: "gone".into(),
                path: path(".claude/skills/gone/SKILL.md"),
            }],
            "is not available",
        ),
        (
            "skill-name-mismatch",
            vec![],
            vec![ComposerInput::Skill {
                name: "other".into(),
                path: claude_skill.to_string_lossy().into(),
            }],
            "does not match its folder",
        ),
        (
            "skill-bad-name",
            vec![],
            vec![ComposerInput::Skill {
                name: "two words".into(),
                path: claude_skill.to_string_lossy().into(),
            }],
            "not a Claude Code skill command name",
        ),
    ];
    let mut receipts = Vec::new();
    for (label, attachments, selections, expected) in cases {
        let cwd = peer_dir();
        let admission = luna_harness::claude::validate_inputs(&attachments, &selections);
        let mut req = request(cwd.path(), "Keep this prompt", attachments);
        req.input_items = selections;
        let (controls, _steer) = controls();
        let Err(error) = harness().run(req, controls).await else {
            panic!("{label}: run started");
        };
        let HarnessError::RejectedBeforeAcceptance(message) = &error else {
            panic!("{label}: wrong error kind {error:?}");
        };
        assert!(message.contains(expected), "{label}: {message}");
        assert!(message.contains("remain available"), "{label}: {message}");
        assert!(
            matches!(admission, Err(HarnessError::RejectedBeforeAcceptance(_))),
            "{label}: engine admission disagreed: {admission:?}"
        );
        assert!(
            log(cwd.path()).is_empty(),
            "{label}: a provider process started"
        );
        receipts.push(json!({"case": label, "message": message}));
    }

    // Changed after admission: the adapter judges the bytes it would send.
    let changed = write(dir, "changing.png", &png());
    luna_harness::claude::validate_inputs(&[changed.clone()], &[]).expect("valid at admission");
    std::fs::write(&changed, b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>").unwrap();
    let cwd = peer_dir();
    let (controls, _steer) = controls();
    let result = harness()
        .run(request(cwd.path(), "changed", vec![changed]), controls)
        .await;
    assert!(matches!(
        result,
        Err(HarnessError::RejectedBeforeAcceptance(_))
    ));
    assert!(log(cwd.path()).is_empty());
    receipts.push(json!({"case": "changed-after-admission", "rejected": true}));
    evidence(
        "invalid-inputs",
        json!({"cases": receipts, "realProvider": false}),
    );
}

#[tokio::test]
async fn explicit_steer_carries_images_and_invalid_steers_are_rejected_unwritten() {
    let root = peer_dir();
    let files = tempfile::tempdir().unwrap();
    let png_path = write(files.path(), "steer.png", &png());
    let svg = write(
        files.path(),
        "steer.svg",
        b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>",
    );
    let (bad_tx, bad_rx) = oneshot::channel();
    let (good_tx, good_rx) = oneshot::channel();
    let steer = |attachments: Vec<String>, prompt: &str, acceptance| SteerMessage {
        input_items: Vec::new(),
        attachments,
        prompt: prompt.into(),
        message_id: Some(format!("steer-{prompt}")),
        delivery: SteerDelivery::RequireActiveTurn,
        acceptance: Some(acceptance),
    };
    let events = run_turns(
        &harness(),
        request(root.path(), "cc04:hold start", Vec::new()),
        1,
        vec![
            steer(vec![svg], "bad", bad_tx),
            steer(vec![png_path], "look", good_tx),
        ],
    )
    .await;
    let bad = bad_rx.await.unwrap();
    assert!(
        matches!(&bad, SteerAcceptance::Rejected(message) if message.contains("SVG")),
        "{bad:?}"
    );
    assert_eq!(good_rx.await.unwrap(), SteerAcceptance::Accepted);
    assert_eq!(completed(&events), 1);
    let frames = user_frames(root.path());
    assert_eq!(frames.len(), 2, "the rejected steer was never written");
    let steered = blocks(&frames[1]);
    assert_eq!(
        steered,
        vec![
            ("image".into(), format!("image/png {}", sha(&png()))),
            ("text".into(), "look".into()),
        ]
    );
    evidence(
        "explicit-steer",
        json!({"rejected": format!("{bad:?}"), "steer": steered, "frames": log(root.path()), "realProvider": false}),
    );
}

#[tokio::test]
async fn resumed_process_receives_images_in_its_first_line() {
    let root = peer_dir();
    let files = tempfile::tempdir().unwrap();
    let png_path = write(files.path(), "resume.png", &png());
    let mut req = request(root.path(), "after resume", vec![png_path]);
    req.resume = Some("cc04-resumed-session".into());
    let events = run_turns(&harness(), req, 1, Vec::new()).await;
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::Done { session_id: Some(id), .. } if id == "cc04-resumed-session"
    )));
    let entries = log(root.path());
    assert!(
        entries[0]["argv"]
            .as_array()
            .unwrap()
            .iter()
            .any(|arg| arg == "--resume=cc04-resumed-session")
    );
    let first = blocks(&user_frames(root.path())[0]);
    assert_eq!(
        first,
        vec![
            ("image".into(), format!("image/png {}", sha(&png()))),
            ("text".into(), "after resume".into()),
        ]
    );
    evidence(
        "resumed",
        json!({"argv": entries[0]["argv"], "first": first, "realProvider": false}),
    );
}

#[tokio::test]
async fn typed_skill_with_images_and_prose_expands_from_the_last_text_block() {
    let files = tempfile::tempdir().unwrap();
    let png_path = write(files.path(), "skill.png", &png());
    let skill = files
        .path()
        .join("project/.claude/skills/fixture-skill/SKILL.md");
    std::fs::create_dir_all(skill.parent().unwrap()).unwrap();
    std::fs::write(
        &skill,
        "---\nname: fixture-skill\ndescription: fixture\n---\nBody\n",
    )
    .unwrap();
    let skill_path = skill.to_string_lossy().into_owned();
    let encoded: String = skill_path
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'/') {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect();
    let chip = format!("[@fixture-skill](skill://{encoded})");
    let selection = ComposerInput::Skill {
        name: "fixture-skill".into(),
        path: skill_path.clone(),
    };
    let image = ("image".to_string(), format!("image/png {}", sha(&png())));

    let mut outcomes = Vec::new();
    for (label, prompt, reasoning, selections, expected) in [
        (
            "chip-mid-prose",
            format!("Look at this first {chip} then summarize"),
            None,
            vec![selection.clone()],
            vec![
                ("text".to_string(), "Look at this first".to_string()),
                image.clone(),
                ("text".into(), "/fixture-skill then summarize".into()),
            ],
        ),
        (
            "chip-ultrathink",
            format!("Look at this first {chip} then summarize"),
            Some(ReasoningLevel::Ultrathink),
            vec![selection.clone()],
            vec![
                ("text".into(), "Ultrathink:\nLook at this first".into()),
                image.clone(),
                ("text".into(), "/fixture-skill then summarize".into()),
            ],
        ),
        (
            "selection-without-chip",
            "summarize the screenshot".to_string(),
            None,
            vec![selection.clone()],
            vec![
                image.clone(),
                (
                    "text".into(),
                    "/fixture-skill summarize the screenshot".into(),
                ),
            ],
        ),
        (
            "typed-slash-ultrathink",
            "/fixture-skill go".to_string(),
            Some(ReasoningLevel::Ultrathink),
            Vec::new(),
            vec![
                ("text".into(), "Ultrathink:".into()),
                image.clone(),
                ("text".into(), "/fixture-skill go".into()),
            ],
        ),
    ] {
        let cwd = peer_dir();
        let mut req = request(cwd.path(), &prompt, vec![png_path.clone()]);
        req.reasoning = reasoning;
        req.input_items = selections;
        let events = run_turns(&harness(), req, 1, Vec::new()).await;
        assert_eq!(completed(&events), 1, "{label}: {events:?}");
        let sent = blocks(&user_frames(cwd.path())[0]);
        assert_eq!(sent, expected, "{label}");
        outcomes.push(json!({"case": label, "prompt": prompt, "sent": sent}));
    }
    evidence(
        "typed-skill",
        json!({"cases": outcomes, "realProvider": false}),
    );
}
