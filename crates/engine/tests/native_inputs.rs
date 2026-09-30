//! Offline E2E: UI RPC -> SQLite/CRDT -> staged upload -> actual Codex adapter.
//! The subprocess peer records native payloads; it does not run a model.
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use luna_doc::{QueueMessageStatus, SessionCommandPayload};
use luna_engine::{AgentAccountsConfig, EngineCore, EngineProfile, HarnessRegistry};
use luna_harness::CodexHarness;
use luna_proto::{ComposerDraft, ComposerDraftRead, ComposerInput, HarnessId, RunRequest};
use luna_rpc::methods;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{path::Path, sync::Arc, time::Duration};

const CHAT: &str = "native-inputs-e2e";
fn assemble(root: &Path) -> EngineCore {
    let registry = Arc::new(HarnessRegistry::new());
    registry.register(Arc::new(
        CodexHarness::new().with_executable(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../harness/tests/fixtures/native-inputs-codex.py"),
        ),
    ));
    let data = root.join("data");
    let accounts = AgentAccountsConfig {
        data_dir: data.join("synthetic-provider"),
        claude_config_dir: data.join("synthetic-claude"),
        claude_config_file: data.join("synthetic-claude.json"),
        codex_home: data.join("synthetic-codex"),
    };
    accounts.disable_claude_keychain_fallback().unwrap();
    EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        EngineProfile::development(&data, "native-input-org", "native-input-user"),
        registry,
        HarnessId::Codex,
        None,
        accounts,
    )
    .unwrap()
}

async fn wait_for(mut predicate: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(15), async {
        while !predicate() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
}

fn requests(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn draft_restart_queue_edit_and_native_image_steer_preserve_inputs() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("SKILL.md"), "fixture skill").unwrap();
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
            path: root.path().join("SKILL.md").to_string_lossy().into(),
        },
    ];
    let png = BASE64.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jRZkAAAAASUVORK5CYII=").unwrap();
    let core = assemble(root.path());
    core.workspace
        .create_space(
            "native-input-project",
            &core.device_id,
            root.path().to_str().unwrap(),
            None,
            false,
        )
        .unwrap();
    let client = luna_rpc::memory_client(core.rpc_service());
    client
        .call(
            methods::MUTATE,
            json!({"op":"createChat","chatId":CHAT,"spaceId":"native-input-project"}),
        )
        .await
        .unwrap();
    core.workspace
        .rename_chat(CHAT, "Pre-titled native input fixture")
        .unwrap();
    client
        .call(
            methods::UPLOAD_CHUNK,
            json!({"uploadId":"native-image","seq":0,"data":BASE64.encode(&png)}),
        )
        .await
        .unwrap();
    let upload = client
        .call(
            methods::UPLOAD_COMMIT,
            json!({"uploadId":"native-image","fileName":"image.png"}),
        )
        .await
        .unwrap();
    let image_path = upload["path"].as_str().unwrap().to_string();
    let draft: ComposerDraft = serde_json::from_value(json!({
        "version":1,"draftId":CHAT,"taskId":CHAT,"revision":1,"text":"",
        "inputItems":selections,"attachments":[{"attachmentId":"native-image","name":"image.png","path":image_path,"sha256":format!("{:x}",Sha256::digest(&png)),"size":png.len(),"state":"saved"}],"updatedAtMs":1
    })).unwrap();
    client
        .call(methods::SAVE_COMPOSER_DRAFT, json!({"draft":draft}))
        .await
        .unwrap();
    drop(client);
    drop(core);
    let core = assemble(root.path());
    let client = luna_rpc::memory_client(core.rpc_service());
    let read: ComposerDraftRead = serde_json::from_value(
        client
            .call(
                methods::GET_COMPOSER_DRAFT,
                json!({"draftId":CHAT,"taskId":CHAT}),
            )
            .await
            .unwrap(),
    )
    .unwrap();
    let restored = read.draft.unwrap();
    assert_eq!(restored.input_items, selections);
    assert_eq!(
        restored.attachments[0].path.as_deref(),
        Some(image_path.as_str())
    );

    // Steer intent holds while idle. Legacy edit omission must keep the vector.
    let queued = client.call(methods::QUEUE_MESSAGE,json!({"chatId":CHAT,"messageId":"typed-queue","text":"queued image","attachments":[image_path],"inputItems":selections,"followUpBehavior":"steer","holdForTurnEnd":true})).await.unwrap();
    let id = queued["id"].as_str().unwrap();
    let lease = client
        .call(
            methods::BEGIN_QUEUED_MESSAGE_EDIT,
            json!({"chatId":CHAT,"id":id,"editorDeviceId":core.device_id,"editorInstanceId":"native-input-editor"}),
        )
        .await
        .unwrap();
    assert_eq!(lease["inputItems"], json!(selections));
    client.call(methods::FINISH_QUEUED_MESSAGE_EDIT,json!({"chatId":CHAT,"id":id,"leaseId":lease["leaseId"],"action":"commit","text":"edited queued image","expectedTextHash":lease["baseTextHash"],"expectedQueueRevision":lease["revision"],"attachments":[image_path]})).await.unwrap();
    let row = core.doc_host.queued_message(CHAT, id).unwrap().unwrap();
    assert_eq!(row.input_items, selections);
    assert!(matches!(row.status, QueueMessageStatus::Held));
    // Explicit [] clears selections; a later selection vector restores them.
    for inputs in [Vec::new(), selections.clone()] {
        let lease = client.call(methods::BEGIN_QUEUED_MESSAGE_EDIT,json!({"chatId":CHAT,"id":id,"editorDeviceId":core.device_id,"editorInstanceId":"native-input-editor"})).await.unwrap();
        client.call(methods::FINISH_QUEUED_MESSAGE_EDIT,json!({"chatId":CHAT,"id":id,"leaseId":lease["leaseId"],"action":"commit","text":"edited queued image","expectedTextHash":lease["baseTextHash"],"expectedQueueRevision":lease["revision"],"attachments":[image_path],"inputItems":inputs})).await.unwrap();
        assert_eq!(
            core.doc_host
                .queued_message(CHAT, id)
                .unwrap()
                .unwrap()
                .input_items,
            inputs
        );
    }
    let row = core.doc_host.queued_message(CHAT, id).unwrap().unwrap();

    let request: RunRequest = serde_json::from_value(json!({"prompt":"","harness":"codex","model":null,"reasoning":null,"cwd":root.path(),"sandbox":"workspace-write","resume":null,"attachments":["pending://native-image/image.png"],"inputItems":selections})).unwrap();
    core.doc_host
        .queue_command(
            CHAT,
            SessionCommandPayload::Run {
                request,
                message_id: "typed-start".into(),
            },
        )
        .unwrap();
    let log_path = root.path().join("native-inputs-rpc.jsonl");
    wait_for(|| {
        requests(&log_path)
            .iter()
            .any(|request| request["method"] == "turn/start")
    })
    .await;
    let steered = client
        .call(
            methods::STEER_QUEUED_MESSAGE_NOW,
            json!({"chatId":CHAT,"id":id}),
        )
        .await
        .unwrap();
    assert_eq!(steered["sent"], true);
    wait_for(|| {
        requests(&log_path)
            .iter()
            .any(|request| request["method"] == "turn/steer")
    })
    .await;
    let log = requests(&log_path);
    for method in ["turn/start", "turn/steer"] {
        let input = &log
            .iter()
            .find(|request| request["method"] == method)
            .unwrap()["params"]["input"];
        let input = input.as_array().unwrap();
        assert!(
            input
                .iter()
                .any(|item| item == &json!({"type":"localImage","path":image_path}))
        );
        assert!(!input.iter().any(|item| {
            item["path"]
                .as_str()
                .is_some_and(|path| path.starts_with("pending://"))
        }));
        for selection in &selections {
            assert!(input.contains(&serde_json::to_value(selection).unwrap()));
        }
    }
    core.sessions.interrupt(CHAT).await.unwrap();
    if let Ok(output) = std::env::var("LUNA_NATIVE_INPUTS_EVIDENCE") {
        let output = Path::new(&output);
        std::fs::create_dir_all(output).unwrap();
        std::fs::copy(log_path, output.join("engine-native-inputs-rpc.jsonl")).unwrap();
        std::fs::write(
            output.join("restored-draft.json"),
            serde_json::to_vec_pretty(&restored).unwrap(),
        )
        .unwrap();
        std::fs::write(
            output.join("edited-queue.json"),
            serde_json::to_vec_pretty(&row).unwrap(),
        )
        .unwrap();
        std::fs::write(output.join("engine-result.json"),json!({"passed":true,"proof":"offline engine RPC, SQLite restart, queue edit, upload staging and adapter subprocess","realProvider":false}).to_string()).unwrap();
    }
}
