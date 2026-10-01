//! CC-04 / E26 offline E2E: UI RPC -> upload staging -> durable command,
//! queue and draft ownership -> engine admission -> actual Claude adapter ->
//! `claude-input-peer.py` (2.1.286 frame shapes; records stdin, runs no
//! model). Set `LUNA_CC04_EVIDENCE=<dir>` to keep frames and receipts.
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use luna_doc::{MessagePart, MessageRole, SessionCommandPayload};
use luna_engine::{AgentAccountsConfig, EngineCore, EngineProfile, HarnessRegistry};
use luna_harness::ClaudeHarness;
use luna_proto::{
    CommandReceiptStatus, ComposerDraft, ComposerDraftRead, ComposerInput, DraftSubmissionState,
    HarnessId, RunRequest,
};
use luna_rpc::methods;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{path::Path, sync::Arc, time::Duration};

const PNG_1X1: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jRZkAAAAASUVORK5CYII=";
const SVG: &[u8] = b"<?xml version=\"1.0\"?><svg xmlns=\"http://www.w3.org/2000/svg\"/>";
const TIFF: &[u8] = b"II*\0\x08\0\0\0fixture";

fn assemble(root: &Path) -> EngineCore {
    let registry = Arc::new(HarnessRegistry::new());
    let peer = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../harness/tests/fixtures/claude-input-peer.py");
    registry.register(Arc::new(
        ClaudeHarness::new()
            .with_executable(peer)
            .with_graces(Duration::from_millis(300), Duration::from_millis(300)),
    ));
    let data = root.join("data");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(
        data.join("synthetic-claude.json"),
        r#"{"oauthAccount":{"accountUuid":"cc04-fixture-account"}}"#,
    )
    .unwrap();
    let accounts = AgentAccountsConfig {
        data_dir: data.join("synthetic-provider"),
        claude_config_dir: data.join("synthetic-claude"),
        claude_config_file: data.join("synthetic-claude.json"),
        codex_home: data.join("synthetic-codex"),
    };
    accounts.disable_claude_keychain_fallback().unwrap();
    EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        EngineProfile::development(&data, "cc04-org", "cc04-user"),
        registry,
        HarnessId::ClaudeCode,
        None,
        accounts,
    )
    .unwrap()
}

async fn wait_for(what: &str, mut predicate: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(20), async {
        while !predicate() {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
}

fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn peer_log(cwd: &Path) -> Vec<Value> {
    std::fs::read_to_string(cwd.join("claude-input-peer.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn user_frames(cwd: &Path) -> Vec<Value> {
    peer_log(cwd)
        .into_iter()
        .filter(|entry| entry["kind"] == "frame" && entry["frame"]["type"] == "user")
        .map(|entry| entry["frame"].clone())
        .collect()
}

fn blocks(frame: &Value) -> Vec<String> {
    match &frame["message"]["content"] {
        Value::String(text) => vec![format!("string:{text}")],
        Value::Array(items) => items
            .iter()
            .map(|block| match block["type"].as_str().unwrap() {
                "image" => format!(
                    "image:{}:{}",
                    block["source"]["media_type"].as_str().unwrap(),
                    sha(&BASE64
                        .decode(block["source"]["data"].as_str().unwrap())
                        .unwrap())
                ),
                kind => format!("{kind}:{}", block["text"].as_str().unwrap_or_default()),
            })
            .collect(),
        other => panic!("unexpected content {other}"),
    }
}

fn user_texts(core: &EngineCore, chat: &str) -> Vec<String> {
    core.doc_host
        .open(chat)
        .ok()
        .and_then(|handle| handle.doc().read_entries().ok())
        .unwrap_or_default()
        .iter()
        .filter(|entry| entry.role == MessageRole::User)
        .map(|entry| {
            entry
                .parts
                .iter()
                .filter_map(|part| match part {
                    MessagePart::Text { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<String>()
        })
        .collect()
}

fn evidence(name: &str, value: Value) {
    if let Ok(dir) = std::env::var("LUNA_CC04_EVIDENCE") {
        let dir = Path::new(&dir).join("engine");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(format!("{name}.json")),
            serde_json::to_vec_pretty(&value).unwrap(),
        )
        .unwrap();
    }
}

async fn create_task(core: &EngineCore, client: &luna_rpc::RpcClient, chat: &str, cwd: &Path) {
    std::fs::create_dir_all(cwd).unwrap();
    std::fs::write(cwd.join(".cc04-peer"), b"").unwrap();
    core.workspace
        .create_space(
            &format!("{chat}-project"),
            &core.device_id,
            cwd.to_str().unwrap(),
            None,
            false,
        )
        .unwrap();
    client
        .call(
            methods::MUTATE,
            json!({"op":"createChat","chatId":chat,"spaceId":format!("{chat}-project")}),
        )
        .await
        .unwrap();
    core.workspace
        .rename_chat(chat, "Pre-titled CC-04 fixture")
        .unwrap();
}

async fn upload(client: &luna_rpc::RpcClient, id: &str, name: &str, bytes: &[u8]) -> String {
    client
        .call(
            methods::UPLOAD_CHUNK,
            json!({"uploadId":id,"seq":0,"data":BASE64.encode(bytes)}),
        )
        .await
        .unwrap();
    let committed = client
        .call(
            methods::UPLOAD_COMMIT,
            json!({"uploadId":id,"fileName":name}),
        )
        .await
        .unwrap();
    committed["path"].as_str().unwrap().to_string()
}

fn run_request(cwd: &Path, prompt: &str, attachments: Vec<String>) -> RunRequest {
    serde_json::from_value(
        json!({"prompt":prompt,"harness":"claude-code","model":null,"reasoning":null,
        "cwd":cwd,"sandbox":"workspace-write","resume":null,"attachments":attachments}),
    )
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn claude_images_reach_the_cli_on_every_admitted_turn_type() {
    const CHAT: &str = "cc04-turns";
    let root = tempfile::tempdir().unwrap();
    let cwd = root.path().join("project");
    let core = assemble(root.path());
    let client = luna_rpc::memory_client(core.rpc_service());
    create_task(&core, &client, CHAT, &cwd).await;
    let png = BASE64.decode(PNG_1X1).unwrap();
    let image = upload(&client, "cc04-image", "screen.png", &png).await;
    let expected = format!("image:image/png:{}", sha(&png));
    let skill = cwd.join(".claude/skills/fixture-skill/SKILL.md");
    std::fs::create_dir_all(skill.parent().unwrap()).unwrap();
    std::fs::write(&skill, "---\nname: fixture-skill\n---\nFixture body\n").unwrap();

    // 1. A steer-intent image row queued while idle holds until there is a
    //    turn to steer; Claude images are not "attachments unsupported".
    let queued = client
        .call(
            methods::QUEUE_MESSAGE,
            json!({"chatId":CHAT,"messageId":"queued-steer","text":"queued image steer",
                   "attachments":[image],"followUpBehavior":"steer","holdForTurnEnd":true}),
        )
        .await
        .unwrap();
    let queued_id = queued["id"].as_str().unwrap().to_string();
    let held = core
        .doc_host
        .queued_message(CHAT, &queued_id)
        .unwrap()
        .unwrap();
    // Idle means "steering unavailable" for now, never "attachments unsupported".
    assert_ne!(
        held.hold_reason,
        Some(luna_doc::QueueHoldReason::AttachmentsUnsupported),
        "{held:?}"
    );

    // 2. Cold first turn from a durable Run command with a staged ref. It
    //    holds the provider turn open for the in-turn steer below.
    core.doc_host
        .queue_command(
            CHAT,
            SessionCommandPayload::Run {
                request: run_request(
                    &cwd,
                    "cc04:hold first look",
                    vec!["pending://cc04-image/screen.png".into()],
                ),
                message_id: "cold-run".into(),
            },
        )
        .unwrap();
    wait_for("cold user frame", || user_frames(&cwd).len() == 1).await;
    wait_for("active turn", || {
        core.sessions.active_turn_id(CHAT).is_some()
    })
    .await;

    // 3. Steer now: the queued image row reaches the active Claude turn.
    let steered = client
        .call(
            methods::STEER_QUEUED_MESSAGE_NOW,
            json!({"chatId":CHAT,"id":queued_id}),
        )
        .await
        .unwrap();
    assert_eq!(steered["sent"], true, "{steered}");
    wait_for("steer frame", || user_frames(&cwd).len() == 2).await;
    wait_for("held turn to settle", || {
        !core.sessions.turn_in_flight(CHAT)
    })
    .await;

    // 4. Follow-up on the parked process: images never use Claude's warm
    //    mailbox; the engine replaces it with a resumed process. This turn
    //    holds too, for the composer's preferred auto-steer below.
    core.doc_host
        .queue_command(
            CHAT,
            SessionCommandPayload::Steer {
                prompt: "cc04:hold follow-up with image".into(),
                message_id: Some("follow-up".into()),
                attachments: vec![image.clone()],
                input_items: Vec::new(),
            },
        )
        .unwrap();
    wait_for("resumed follow-up frame", || user_frames(&cwd).len() == 3).await;
    wait_for("resumed active turn", || {
        core.sessions.active_turn_id(CHAT).is_some()
    })
    .await;

    // 5. Queueing a steer-intent image during a turn steers it at once.
    client
        .call(
            methods::QUEUE_MESSAGE,
            json!({"chatId":CHAT,"messageId":"auto-steer","text":"auto steer image",
                   "attachments":[image],"followUpBehavior":"steer","holdForTurnEnd":false}),
        )
        .await
        .unwrap();
    wait_for("auto steer frame", || user_frames(&cwd).len() == 4).await;
    wait_for("follow-up to settle", || {
        !core.sessions.turn_in_flight(CHAT)
    })
    .await;

    // 6. Image-only follow-up (empty prompt over the command plane).
    core.doc_host
        .queue_command(
            CHAT,
            SessionCommandPayload::Steer {
                prompt: String::new(),
                message_id: Some("image-only".into()),
                attachments: vec![image.clone()],
                input_items: Vec::new(),
            },
        )
        .unwrap();
    wait_for("image-only frame", || user_frames(&cwd).len() == 5).await;
    wait_for("image-only to settle", || {
        !core.sessions.turn_in_flight(CHAT)
    })
    .await;

    // 7. Typed Claude skill + image + prose, dispatched natively.
    let encoded: String = skill
        .to_string_lossy()
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'/') {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect();
    core.doc_host
        .queue_command(
            CHAT,
            SessionCommandPayload::Steer {
                prompt: format!(
                    "Compare these [@fixture-skill](skill://{encoded}) using the screenshot"
                ),
                message_id: Some("typed-skill".into()),
                attachments: vec![image.clone()],
                input_items: vec![ComposerInput::Skill {
                    name: "fixture-skill".into(),
                    path: skill.to_string_lossy().into(),
                }],
            },
        )
        .unwrap();
    wait_for("typed skill frame", || user_frames(&cwd).len() == 6).await;

    let frames = user_frames(&cwd);
    let sent: Vec<Vec<String>> = frames.iter().map(blocks).collect();
    let with = |text: &str| vec![expected.clone(), format!("text:{text}")];
    assert_eq!(sent[0], with("cc04:hold first look"));
    // Queued rows also carry the shared host-path trailer; it names the
    // host-owned upload, never a sender path or pending:// ref.
    let queued = |text: &str| {
        with(&format!(
            "{text}\n\nAttached images (local files \u{2014} open them to view):\n- {image}"
        ))
    };
    assert_eq!(sent[1], queued("queued image steer"));
    assert_eq!(sent[2], with("cc04:hold follow-up with image"));
    assert_eq!(sent[3], queued("auto steer image"));
    assert_eq!(sent[4], vec![expected.clone()]);
    assert_eq!(
        sent[5],
        vec![
            "text:Compare these".to_string(),
            expected.clone(),
            "text:/fixture-skill using the screenshot".into()
        ]
    );
    // One native conversation: the cold process starts it and every image
    // follow-up resumes it in a replacement process.
    // CC-12 checks the typed skill (turn 7) against the launch's own effective
    // catalog with ONE initialize-only readback (hooks disabled, no user
    // message) before dispatch; it is not a turn process.
    let (readbacks, argv): (Vec<Value>, Vec<Value>) = peer_log(&cwd)
        .into_iter()
        .filter(|entry| entry["kind"] == "argv")
        .map(|entry| entry["argv"].clone())
        .partition(|args| {
            args.as_array().is_some_and(|args| {
                args.windows(2).any(|pair| {
                    pair[0] == "--settings"
                        && pair[1]
                            .as_str()
                            .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
                            .is_some_and(|settings| settings["disableAllHooks"] == true)
                })
            })
        });
    assert_eq!(
        readbacks.len(),
        1,
        "one CC-12 readback for the typed skill: {readbacks:?}"
    );
    let session = argv
        .get(1)
        .and_then(|args| args.as_array())
        .and_then(|args| {
            args.iter()
                .find_map(|arg| arg.as_str()?.strip_prefix("--resume="))
        })
        .expect("follow-up process resumes the native session")
        .to_string();
    assert!(session.starts_with("cc04-session-"), "{session}");
    assert!(
        argv[0]
            .as_array()
            .unwrap()
            .iter()
            .all(|arg| !arg.as_str().unwrap().starts_with("--resume")),
        "the cold turn starts a fresh native session"
    );
    assert_eq!(argv.len(), 4, "{argv:?}");
    let resume = format!("--resume={session}");
    for args in &argv[1..] {
        assert!(
            args.as_array()
                .unwrap()
                .iter()
                .any(|arg| arg == &json!(resume)),
            "{args}"
        );
    }
    let row = core.doc_host.queued_message(CHAT, &queued_id).unwrap();
    core.sessions.interrupt(CHAT).await.unwrap();
    evidence(
        "turn-types",
        json!({"fixturePngSha256": sha(&png), "hostUpload": image, "blocks": sent, "argv": argv, "cc12Readbacks": readbacks,
               "userTranscript": user_texts(&core, CHAT), "steeredQueueRow": format!("{row:?}"), "peerLog": peer_log(&cwd), "realProvider": false}),
    );
}

fn draft(chat: &str, text: &str, attachments: Value, input_items: Value) -> ComposerDraft {
    serde_json::from_value(
        json!({"version":1,"draftId":chat,"taskId":chat,"revision":1,"text":text,
        "inputItems":input_items,"attachments":attachments,"updatedAtMs":1}),
    )
    .unwrap()
}

async fn read_draft(client: &luna_rpc::RpcClient, chat: &str) -> ComposerDraftRead {
    serde_json::from_value(
        client
            .call(
                methods::GET_COMPOSER_DRAFT,
                json!({"draftId":chat,"taskId":chat}),
            )
            .await
            .unwrap(),
    )
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn invalid_claude_input_rejects_recoverably_with_the_exact_draft_envelope() {
    let root = tempfile::tempdir().unwrap();
    let core = assemble(root.path());
    let client = luna_rpc::memory_client(core.rpc_service());
    let cmyk = {
        let mut bytes = vec![0xFF, 0xD8, 0xFF, 0xC0, 0x00, 0x14, 8, 0, 1, 0, 1, 4];
        for id in 1..=4u8 {
            bytes.extend([id, 0x11, 0]);
        }
        bytes.extend([0xFF, 0xD9]);
        bytes
    };
    let mut cases = Vec::new();
    for (chat, label, file, selections, expected) in [
        (
            "cc04-svg",
            "svg-upload",
            Some(("art.svg", SVG.to_vec())),
            json!([]),
            "SVG vector artwork",
        ),
        (
            "cc04-tiff",
            "tiff-upload",
            Some(("scan.tiff", TIFF.to_vec())),
            json!([]),
            "a TIFF image",
        ),
        (
            "cc04-cmyk",
            "cmyk-jpeg",
            Some(("print.jpg", cmyk.clone())),
            json!([]),
            "CMYK JPEG",
        ),
        (
            "cc04-mention",
            "codex-mention",
            None,
            json!([{"type":"mention","name":"Linear","path":"app://linear"}]),
            "Codex integrations",
        ),
        (
            "cc04-foreign",
            "sender-local-path",
            None,
            json!([]),
            "not available on the computer running this task",
        ),
    ] {
        let cwd = root.path().join(chat);
        create_task(&core, &client, chat, &cwd).await;
        let (attachments, blobs) = match &file {
            Some((name, bytes)) => {
                let path = upload(&client, &format!("{chat}-upload"), name, bytes).await;
                let blob = json!([{"attachmentId":format!("{chat}-upload"),"name":name,"path":path,
                    "sha256":sha(bytes),"size":bytes.len(),"state":"saved"}]);
                (vec![path], blob)
            }
            None if label == "sender-local-path" => (
                vec!["/Users/another-device/Desktop/shot.png".to_string()],
                json!([]),
            ),
            None => (Vec::new(), json!([])),
        };
        let saved = draft(
            chat,
            "Keep every byte of this draft",
            blobs,
            selections.clone(),
        );
        client
            .call(methods::SAVE_COMPOSER_DRAFT, json!({"draft":saved}))
            .await
            .unwrap();
        let command_id = format!("{chat}-send");
        client
            .call(
                methods::BEGIN_DRAFT_SUBMISSION,
                json!({"commandId":command_id,"taskId":chat,"snapshot":saved}),
            )
            .await
            .unwrap();
        let mut request = run_request(&cwd, &saved.text, attachments);
        request.input_items = serde_json::from_value(selections).unwrap();
        client
            .call(
                methods::EXECUTE_TASK_COMMAND,
                json!({"commandId":command_id,"actorId":"fixture-user","clientId":"fixture-client",
                       "clientSessionId":"fixture-session","taskId":chat,
                       "intent":{"kind":"session","command":{"kind":"run","request":request,"messageId":command_id}}}),
            )
            .await
            .unwrap();
        wait_for(label, || {
            core.doc_host
                .task_command_receipt(&command_id)
                .unwrap()
                .is_some_and(|receipt| receipt.status == CommandReceiptStatus::Rejected)
        })
        .await;
        let receipt = core
            .doc_host
            .task_command_receipt(&command_id)
            .unwrap()
            .unwrap();
        let reason = receipt
            .reason
            .as_ref()
            .map(|r| r.message.clone())
            .unwrap_or_default();
        assert!(reason.contains(expected), "{label}: {reason}");
        assert!(reason.contains("remain available"), "{label}: {reason}");
        assert!(
            peer_log(&cwd).is_empty(),
            "{label}: a provider process started"
        );
        assert!(
            user_texts(&core, chat).is_empty(),
            "{label}: a rejected send left a transcript entry"
        );
        // The UI settles any terminal non-applied receipt as recoverable.
        client
            .call(
                methods::SETTLE_DRAFT_SUBMISSION,
                json!({"commandId":command_id,"outcome":"recoverable"}),
            )
            .await
            .unwrap();
        let read = read_draft(&client, chat).await;
        assert!(
            read.submissions.iter().any(|s| s.command_id == command_id
                && s.state == DraftSubmissionState::Recoverable
                && s.snapshot.same_message_content(&saved)),
            "{label}: {read:?}"
        );
        cases.push((chat, label, command_id, saved, receipt));
    }
    drop(client);
    drop(core);
    let core = assemble(root.path());
    let client = luna_rpc::memory_client(core.rpc_service());
    let mut receipts = Vec::new();
    for (chat, label, command_id, saved, receipt) in cases {
        let read = read_draft(&client, chat).await;
        let submission = read
            .submissions
            .iter()
            .find(|s| s.command_id == command_id)
            .unwrap_or_else(|| panic!("{label}: submission lost after restart"));
        assert_eq!(
            submission.state,
            DraftSubmissionState::Recoverable,
            "{label}"
        );
        assert!(submission.snapshot.same_message_content(&saved), "{label}");
        receipts.push(json!({"case":label,"receipt":receipt,"recoverableAfterRestart":submission}));
    }
    evidence(
        "rejections",
        json!({"cases": receipts, "realProvider": false}),
    );
}
