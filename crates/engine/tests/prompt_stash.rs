//! File-backed RPC acceptance for saved prompts and upload ownership.
//! Failure cases are listed first in docs/qa/prompt-stash.md.

use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use luna_engine::{AgentAccountsConfig, EngineCore, EngineProfile, HarnessRegistry};
use luna_proto::{
    ComposerDraft, ComposerDraftRead, DraftBlob, DraftBlobState, DraftContext, HarnessId,
    PromptStashEntry,
};
use luna_rpc::methods;
use sha2::{Digest, Sha256};

fn assemble(root: &std::path::Path) -> EngineCore {
    EngineCore::assemble_with_profile_and_agent_accounts_config(
        EngineProfile::development(&root.join("data"), "stash-org", "stash-user"),
        Arc::new(HarnessRegistry::new()),
        HarnessId::Mock,
        None,
        AgentAccountsConfig {
            data_dir: root.join("provider-state"),
            claude_config_dir: root.join("synthetic-claude"),
            claude_config_file: root.join("synthetic-claude.json"),
            codex_home: root.join("synthetic-codex"),
        },
    )
    .unwrap()
}

fn draft(id: &str, revision: u64, text: &str, attachments: Vec<DraftBlob>) -> ComposerDraft {
    ComposerDraft {
        version: luna_proto::COMPOSER_DRAFT_VERSION,
        draft_id: id.into(),
        task_id: Some(id.into()),
        revision,
        text: text.into(),
        attachments,
        appshots: Vec::new(),
        context: DraftContext::default(),
        execution: serde_json::Value::Null,
        updated_at_ms: revision as i64,
    }
}

#[tokio::test]
async fn stash_survives_source_clear_restart_and_shared_owner_release() {
    let root = tempfile::tempdir().unwrap();
    let bytes = BASE64
        .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=")
        .unwrap();
    let core = assemble(root.path());
    let client = luna_rpc::memory_client(core.rpc_service());
    client
        .call(
            methods::UPLOAD_CHUNK,
            serde_json::json!({"uploadId":"stash-fixture", "data":BASE64.encode(&bytes), "seq":0}),
        )
        .await
        .unwrap();
    let upload = client
        .call(
            methods::UPLOAD_COMMIT,
            serde_json::json!({"uploadId":"stash-fixture", "fileName":"fixture.png"}),
        )
        .await
        .unwrap();
    let path = upload["path"].as_str().unwrap().to_string();
    assert!(std::path::Path::new(&path).is_file());
    let blob = DraftBlob {
        attachment_id: "fixture-image".into(),
        name: "fixture.png".into(),
        path: Some(path.clone()),
        sha256: Some(format!("{:x}", Sha256::digest(&bytes))),
        size: Some(bytes.len() as u64),
        state: DraftBlobState::Saved,
        error: None,
    };
    client
        .call(
            methods::SAVE_COMPOSER_DRAFT,
            serde_json::json!({"draft":draft("source-task", 1, "keep this image", vec![blob.clone()])}),
        )
        .await
        .unwrap();
    let entry = PromptStashEntry {
        id: uuid::Uuid::new_v4().to_string(),
        label: "keep this image".into(),
        text: "keep this image".into(),
        attachments: vec![blob.clone()],
        appshots: Vec::new(),
        context: DraftContext::default(),
        created_at_ms: 123,
    };
    for _ in 0..2 {
        client
            .call(
                methods::SAVE_PROMPT_STASH,
                serde_json::json!({"entry":entry}),
            )
            .await
            .expect("exact retry is idempotent");
    }
    let mut changed = entry.clone();
    changed.text = "different content".into();
    assert!(
        client
            .call(
                methods::SAVE_PROMPT_STASH,
                serde_json::json!({"entry":changed})
            )
            .await
            .is_err()
    );
    let mut outside = entry.clone();
    outside.id = uuid::Uuid::new_v4().to_string();
    outside.attachments[0].path = Some(root.path().join("outside.png").to_string_lossy().into());
    assert!(
        client
            .call(
                methods::SAVE_PROMPT_STASH,
                serde_json::json!({"entry":outside})
            )
            .await
            .is_err()
    );
    for context in [
        DraftContext {
            quoted: Some(serde_json::json!({"messageId":"quoted-context"})),
            ..DraftContext::default()
        },
        DraftContext {
            terminal: Some(serde_json::json!({"terminalId":"terminal-context"})),
            ..DraftContext::default()
        },
    ] {
        let mut unsupported = entry.clone();
        unsupported.id = uuid::Uuid::new_v4().to_string();
        unsupported.context = context;
        assert!(
            client
                .call(
                    methods::SAVE_PROMPT_STASH,
                    serde_json::json!({"entry":unsupported})
                )
                .await
                .is_err()
        );
    }
    let mut malformed_comment = entry.clone();
    malformed_comment.id = uuid::Uuid::new_v4().to_string();
    malformed_comment
        .context
        .review_comments
        .push(serde_json::json!({
            "id":"comment-without-anchor"
        }));
    assert!(
        client
            .call(
                methods::SAVE_PROMPT_STASH,
                serde_json::json!({"entry":malformed_comment}),
            )
            .await
            .is_err()
    );
    let source: ComposerDraftRead = client
        .call_as(
            methods::GET_COMPOSER_DRAFT,
            serde_json::json!({"taskId":"source-task"}),
        )
        .await
        .unwrap();
    assert_eq!(source.draft.unwrap().text, "keep this image");
    client
        .call(
            methods::SAVE_COMPOSER_DRAFT,
            serde_json::json!({"draft":draft("source-task", 2, "", Vec::new())}),
        )
        .await
        .unwrap();
    assert!(core.store.attachment_has_owner(&path).unwrap());
    core.store
        .retire_task_local_records(&core.device_id, "source-task")
        .unwrap();
    assert!(core.store.attachment_has_owner(&path).unwrap());
    drop(client);
    core.shutdown().await;
    drop(core);

    let reopened = assemble(root.path());
    let client = luna_rpc::memory_client(reopened.rpc_service());
    let entries: Vec<PromptStashEntry> = client
        .call_as(methods::LIST_PROMPT_STASH, serde_json::json!({}))
        .await
        .unwrap();
    assert_eq!(entries, vec![entry.clone()]);
    assert!(std::path::Path::new(&path).is_file());
    client
        .call(
            methods::SAVE_COMPOSER_DRAFT,
            serde_json::json!({"draft":draft("target-task", 1, &entry.text, entry.attachments.clone())}),
        )
        .await
        .unwrap();
    let read: ComposerDraftRead = client
        .call_as(
            methods::GET_COMPOSER_DRAFT,
            serde_json::json!({"taskId":"target-task"}),
        )
        .await
        .unwrap();
    assert_eq!(read.draft.unwrap().attachments, entry.attachments);
    let deleted = client
        .call(
            methods::DELETE_PROMPT_STASH,
            serde_json::json!({"id":entry.id}),
        )
        .await
        .unwrap();
    assert_eq!(deleted["deleted"], true);
    assert!(reopened.store.attachment_has_owner(&path).unwrap());
    client
        .call(
            methods::SAVE_COMPOSER_DRAFT,
            serde_json::json!({"draft":draft("target-task", 2, "", Vec::new())}),
        )
        .await
        .unwrap();
    assert!(!reopened.store.attachment_has_owner(&path).unwrap());
    assert!(reopened.store.remove_unowned_attachment(&path).unwrap());
    assert!(!std::path::Path::new(&path).exists());

    let oversized = PromptStashEntry {
        id: uuid::Uuid::new_v4().to_string(),
        label: "oversized".into(),
        text: "x".repeat(luna_proto::MAX_PROMPT_STASH_ENTRY_BYTES),
        attachments: Vec::new(),
        appshots: Vec::new(),
        context: DraftContext::default(),
        created_at_ms: 52,
    };
    assert!(
        client
            .call(
                methods::SAVE_PROMPT_STASH,
                serde_json::json!({"entry":oversized})
            )
            .await
            .is_err()
    );

    client
        .call(
            methods::UPLOAD_CHUNK,
            serde_json::json!({"uploadId":"missing-fixture", "data":BASE64.encode(&bytes), "seq":0}),
        )
        .await
        .unwrap();
    let missing_upload = client
        .call(
            methods::UPLOAD_COMMIT,
            serde_json::json!({"uploadId":"missing-fixture", "fileName":"missing.png"}),
        )
        .await
        .unwrap();
    let missing_path = missing_upload["path"].as_str().unwrap().to_string();
    let mut missing = entry.clone();
    missing.id = uuid::Uuid::new_v4().to_string();
    missing.attachments[0].path = Some(missing_path.clone());
    client
        .call(
            methods::SAVE_PROMPT_STASH,
            serde_json::json!({"entry":missing}),
        )
        .await
        .unwrap();
    std::fs::remove_file(&missing_path).unwrap();
    let entries: Vec<PromptStashEntry> = client
        .call_as(methods::LIST_PROMPT_STASH, serde_json::json!({}))
        .await
        .unwrap();
    let marked = entries
        .iter()
        .find(|candidate| candidate.id == missing.id)
        .unwrap();
    assert_eq!(
        marked.attachments[0].state,
        DraftBlobState::NeedsReattachment
    );
    assert!(marked.attachments[0].path.is_none());
    client
        .call(
            methods::DELETE_PROMPT_STASH,
            serde_json::json!({"id":missing.id}),
        )
        .await
        .unwrap();

    client
        .call(
            methods::UPLOAD_CHUNK,
            serde_json::json!({"uploadId":"last-owner-fixture", "data":BASE64.encode(&bytes), "seq":0}),
        )
        .await
        .unwrap();
    let lone_upload = client
        .call(
            methods::UPLOAD_COMMIT,
            serde_json::json!({"uploadId":"last-owner-fixture", "fileName":"lone.png"}),
        )
        .await
        .unwrap();
    let lone_path = lone_upload["path"].as_str().unwrap().to_string();
    let mut lone = entry.clone();
    lone.id = uuid::Uuid::new_v4().to_string();
    lone.attachments[0].path = Some(lone_path.clone());
    client
        .call(
            methods::SAVE_PROMPT_STASH,
            serde_json::json!({"entry":lone}),
        )
        .await
        .unwrap();
    assert!(std::path::Path::new(&lone_path).is_file());
    client
        .call(
            methods::DELETE_PROMPT_STASH,
            serde_json::json!({"id":lone.id}),
        )
        .await
        .unwrap();
    assert!(!std::path::Path::new(&lone_path).exists());

    for index in 0..50 {
        let slot = PromptStashEntry {
            id: uuid::Uuid::new_v4().to_string(),
            label: format!("slot {index}"),
            text: format!("slot {index}"),
            attachments: Vec::new(),
            appshots: Vec::new(),
            context: DraftContext::default(),
            created_at_ms: index,
        };
        client
            .call(
                methods::SAVE_PROMPT_STASH,
                serde_json::json!({"entry":slot}),
            )
            .await
            .unwrap();
    }
    let extra = PromptStashEntry {
        id: uuid::Uuid::new_v4().to_string(),
        label: "overflow".into(),
        text: "overflow".into(),
        attachments: Vec::new(),
        appshots: Vec::new(),
        context: DraftContext::default(),
        created_at_ms: 51,
    };
    assert!(
        client
            .call(
                methods::SAVE_PROMPT_STASH,
                serde_json::json!({"entry":extra})
            )
            .await
            .is_err()
    );
    let entries: Vec<PromptStashEntry> = client
        .call_as(methods::LIST_PROMPT_STASH, serde_json::json!({}))
        .await
        .unwrap();
    assert_eq!(entries.len(), 50);
    reopened.shutdown().await;
}
