//! Repeatable adapter subprocess proof. The peer is deterministic, not a model.
#![cfg(unix)]

use futures::StreamExt;
use luna_harness::{
    CancellationToken, CodexHarness, Harness, RunControls, SteerDelivery, SteerMessage,
};
use luna_proto::{AgentEvent, ComposerInput, RunRequest};
use serde_json::{Value, json};
use std::{path::Path, time::Duration};
use tokio::sync::{mpsc, oneshot};

fn request(root: &Path) -> RunRequest {
    serde_json::from_value(json!({
        "prompt": "", "model": null, "reasoning": null, "cwd": root,
        "sandbox": "workspace-write", "resume": null
    }))
    .unwrap()
}

fn fixture() -> CodexHarness {
    CodexHarness::new().with_executable(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/native-inputs-codex.py"),
    )
}

fn controls() -> (RunControls, mpsc::Sender<SteerMessage>, CancellationToken) {
    let (tx, rx) = mpsc::channel(8);
    let token = CancellationToken::new();
    (
        RunControls {
            request_input: Box::new(|_| {
                let (tx, rx) = oneshot::channel();
                let _ = tx.send(vec![]);
                rx
            }),
            steering: rx,
            interrupt: token.clone(),
            invoke_luna_tool: None,
        },
        tx,
        token,
    )
}

#[tokio::test]
async fn image_only_start_and_attached_steer_preserve_native_identity() {
    use base64::Engine;
    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("pixel.png");
    std::fs::write(&image, base64::engine::general_purpose::STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jRZkAAAAASUVORK5CYII=").unwrap()).unwrap();
    let skill = dir.path().join("SKILL.md");
    std::fs::write(&skill, "fixture skill").unwrap();
    let selections = vec![
        ComposerInput::Mention {
            name: "Same name".into(),
            path: "plugin://native-plugin-id".into(),
        },
        ComposerInput::Mention {
            name: "Same name".into(),
            path: "app://native-app-id".into(),
        },
        ComposerInput::Skill {
            name: "fixture".into(),
            path: skill.to_string_lossy().into(),
        },
    ];
    let mut req = request(dir.path());
    req.attachments = vec![
        image.to_string_lossy().into(),
        image.to_string_lossy().into(),
    ];
    req.input_items = selections.clone();
    let (controls, tx, token) = controls();
    let mut stream = fixture().run(req, controls).await.unwrap();
    let mut sent = false;
    tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(event) = stream.next().await {
            let event = event.unwrap();
            if matches!(event, AgentEvent::TextDelta { .. }) && !sent {
                sent = true;
                tx.send(SteerMessage {
                    prompt: "attached steer".into(),
                    attachments: vec![image.to_string_lossy().into()],
                    input_items: selections.clone(),
                    message_id: Some("steer-input-id".into()),
                    delivery: SteerDelivery::RequireActiveTurn,
                    acceptance: None,
                })
                .await
                .unwrap();
            }
            if matches!(event, AgentEvent::Done { .. }) {
                break;
            }
        }
    })
    .await
    .unwrap();
    token.cancel();
    assert!(sent);
    let log = std::fs::read_to_string(dir.path().join("native-inputs-rpc.jsonl")).unwrap();
    let requests: Vec<Value> = log
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    for method in ["turn/start", "turn/steer"] {
        let input = requests.iter().find(|r| r["method"] == method).unwrap()["params"]["input"]
            .as_array()
            .unwrap();
        assert_eq!(
            input
                .iter()
                .filter(|item| item["type"] == "localImage")
                .count(),
            1
        );
        assert!(
            input
                .iter()
                .any(|item| item == &json!({"type":"localImage", "path":image}))
        );
        assert!(input.iter().any(|item| item
            == &json!({"type":"mention", "name":"Same name", "path":"plugin://native-plugin-id"})));
        assert!(input.iter().any(|item| item
            == &json!({"type":"mention", "name":"Same name", "path":"app://native-app-id"})));
        assert!(
            input
                .iter()
                .any(|item| item == &json!({"type":"skill", "name":"fixture", "path":skill}))
        );
    }
    let start = requests
        .iter()
        .find(|r| r["method"] == "turn/start")
        .unwrap();
    assert!(
        !start["params"]["input"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["type"] == "text")
    );
    if let Ok(output) = std::env::var("LUNA_NATIVE_INPUTS_EVIDENCE") {
        std::fs::create_dir_all(&output).unwrap();
        std::fs::write(
            Path::new(&output).join("adapter-native-inputs-rpc.jsonl"),
            &log,
        )
        .unwrap();
        std::fs::write(
            Path::new(&output).join("adapter-result.json"),
            json!({"passed":true,"proof":"deterministic adapter subprocess", "realProvider":false})
                .to_string(),
        )
        .unwrap();
    }
}

#[tokio::test]
async fn invalid_image_and_selection_fail_before_spawning_provider() {
    let dir = tempfile::tempdir().unwrap();
    let invalid = dir.path().join("bad.png");
    std::fs::write(&invalid, "not image bytes").unwrap();
    for path in [
        "relative.png".to_string(),
        dir.path().join("missing.png").to_string_lossy().into(),
        dir.path().to_string_lossy().into(),
        invalid.to_string_lossy().into(),
    ] {
        let mut req = request(dir.path());
        req.attachments = vec![path];
        assert!(fixture().run(req, controls().0).await.is_err());
    }
    let mut req = request(dir.path());
    req.input_items = vec![ComposerInput::Mention {
        name: "pasted".into(),
        path: "https://unselected.invalid".into(),
    }];
    assert!(fixture().run(req, controls().0).await.is_err());
    assert!(!dir.path().join("native-inputs-rpc.jsonl").exists());
}

#[tokio::test]
async fn unavailable_native_selection_rejects_before_turn_start() {
    let dir = tempfile::tempdir().unwrap();
    let mut req = request(dir.path());
    req.input_items = vec![ComposerInput::Mention {
        name: "missing".into(),
        path: "plugin://unavailable-id".into(),
    }];
    let error = fixture().run(req, controls().0).await.err().unwrap();
    assert!(matches!(
        error,
        luna_harness::HarnessError::RejectedBeforeAcceptance(_)
    ));
    let log = std::fs::read_to_string(dir.path().join("native-inputs-rpc.jsonl")).unwrap();
    assert!(
        !log.lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .any(|r| r["method"] == "turn/start")
    );
    if let Ok(output) = std::env::var("LUNA_NATIVE_INPUTS_EVIDENCE") {
        std::fs::create_dir_all(&output).unwrap();
        std::fs::write(Path::new(&output).join("catalog-rejection-rpc.jsonl"), log).unwrap();
    }
}

#[tokio::test]
async fn correlated_warm_steer_fallback_keeps_the_complete_input() {
    use base64::Engine;
    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("pixel.png");
    std::fs::write(&image, base64::engine::general_purpose::STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jRZkAAAAASUVORK5CYII=").unwrap()).unwrap();
    let selection = ComposerInput::Mention {
        name: "Same name".into(),
        path: "plugin://native-plugin-id".into(),
    };
    let (controls, tx, token) = controls();
    let mut initial = request(dir.path());
    initial.prompt = "Initial fixture turn".into();
    let mut stream = fixture().run(initial, controls).await.unwrap();
    let mut sent = false;
    let mut done = 0;
    tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(event) = stream.next().await {
            let event = event.unwrap();
            if matches!(event, AgentEvent::TextDelta { .. }) && !sent {
                sent = true;
                tx.send(SteerMessage {
                    prompt: "fallback-native".into(),
                    attachments: vec![image.to_string_lossy().into()],
                    input_items: vec![selection.clone()],
                    message_id: Some("fallback-id".into()),
                    delivery: SteerDelivery::AllowFollowUpTurn,
                    acceptance: None,
                })
                .await
                .unwrap();
            }
            if matches!(event, AgentEvent::Done { .. }) {
                done += 1;
                if done == 2 {
                    break;
                }
            }
        }
    })
    .await
    .unwrap();
    token.cancel();
    assert_eq!(done, 2);
    let log = std::fs::read_to_string(dir.path().join("native-inputs-rpc.jsonl")).unwrap();
    let requests: Vec<Value> = log
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let starts: Vec<_> = requests
        .iter()
        .filter(|r| r["method"] == "turn/start")
        .collect();
    assert_eq!(starts.len(), 2);
    let input = starts[1]["params"]["input"].as_array().unwrap();
    assert!(input.contains(&json!({"type":"text","text":"fallback-native"})));
    assert!(input.contains(&json!({"type":"localImage","path":image})));
    assert!(input.contains(&serde_json::to_value(selection).unwrap()));
    if let Ok(output) = std::env::var("LUNA_NATIVE_INPUTS_EVIDENCE") {
        std::fs::create_dir_all(&output).unwrap();
        std::fs::write(
            Path::new(&output).join("fallback-native-inputs-rpc.jsonl"),
            log,
        )
        .unwrap();
    }
}

#[tokio::test]
async fn host_native_tiff_and_unsupported_svg_follow_explicit_policy() {
    let dir = tempfile::tempdir().unwrap();
    let tiff = dir.path().join("native.tiff");
    std::fs::write(&tiff, b"II\x2a\x00\x08\x00\x00\x00").unwrap();
    let mut req = request(dir.path());
    req.prompt = "TIFF fixture input".into();
    req.attachments = vec![tiff.to_string_lossy().into()];
    let (tiff_controls, _, token) = controls();
    if cfg!(target_os = "macos") {
        let mut stream = fixture().run(req, tiff_controls).await.unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while let Some(event) = stream.next().await {
                if matches!(event.unwrap(), AgentEvent::TextDelta { .. }) {
                    break;
                }
            }
        })
        .await
        .unwrap();
        token.cancel();
        let log = std::fs::read_to_string(dir.path().join("native-inputs-rpc.jsonl")).unwrap();
        let rows: Vec<Value> = log
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert!(
            rows.iter().find(|r| r["method"] == "turn/start").unwrap()["params"]["input"]
                .as_array()
                .unwrap()
                .contains(&json!({"type":"localImage","path":tiff}))
        );
    } else {
        assert!(fixture().run(req, tiff_controls).await.is_err());
    }
    let svg = dir.path().join("unsupported.svg");
    std::fs::write(&svg, b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>").unwrap();
    let mut req = request(dir.path());
    req.prompt = "Keep ordinary text and all selections".into();
    req.attachments = vec![svg.to_string_lossy().into()];
    req.input_items = vec![ComposerInput::Mention {
        name: "Selected".into(),
        path: "plugin://native-plugin-id".into(),
    }];
    assert!(matches!(
        fixture()
            .run(req.clone(), controls().0)
            .await
            .err()
            .unwrap(),
        luna_harness::HarnessError::RejectedBeforeAcceptance(_)
    ));
    assert_eq!(req.input_items.len(), 1);
    assert_eq!(req.attachments.len(), 1);
}
