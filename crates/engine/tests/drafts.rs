//! L08 restart/blob validation at the real local UI-RPC -> engine -> SQLite
//! boundary. Provider homes are explicit synthetic fixtures; no credential or
//! live-provider path is consulted.

use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use sha2::{Digest, Sha256};
use zeron_engine::{AgentAccountsConfig, EngineCore, EngineProfile, HarnessRegistry};
use zeron_proto::{
    ComposerDraft, ComposerDraftRead, DraftAppshot, DraftBlob, DraftBlobState, DraftContext,
    HarnessId,
};
use zeron_rpc::methods;

fn account_config(root: &std::path::Path) -> AgentAccountsConfig {
    AgentAccountsConfig {
        data_dir: root.join("provider-state"),
        claude_config_dir: root.join("synthetic-claude"),
        claude_config_file: root.join("synthetic-claude.json"),
        codex_home: root.join("synthetic-codex"),
        cursor_sdk_auth_file: root.join("synthetic-cursor-auth.json"),
    }
}

fn assemble(root: &std::path::Path) -> EngineCore {
    EngineCore::assemble_with_profile_and_agent_accounts_config(
        EngineProfile::development(&root.join("data"), "l08-org", "l08-user"),
        Arc::new(HarnessRegistry::new()),
        HarnessId::Mock,
        None,
        account_config(root),
    )
    .unwrap()
}

fn rich_draft(path: String, bytes: &[u8]) -> ComposerDraft {
    ComposerDraft {
        version: zeron_proto::COMPOSER_DRAFT_VERSION,
        draft_id: "task-l08".into(),
        task_id: Some("task-l08".into()),
        revision: 11,
        text: "Keep the rich draft".into(),
        attachments: vec![DraftBlob {
            attachment_id: "fixture-image".into(),
            name: "fixture.png".into(),
            path: Some(path),
            sha256: Some(format!("{:x}", Sha256::digest(bytes))),
            size: Some(bytes.len() as u64),
            state: DraftBlobState::Saved,
            error: None,
        }],
        appshots: vec![DraftAppshot {
            id: "fixture-appshot".into(),
            screenshot_attachment_id: "fixture-image".into(),
            app_name: "Synthetic App".into(),
            bundle_identifier: Some("test.synthetic".into()),
            window_title: Some("Synthetic Window".into()),
            accessibility_format_version: 1,
            accessibility_content: "full captured synthetic context".into(),
            accessibility_truncated: false,
            screenshot_dimensions: Some((1, 1)),
            captured_at_ms: 123,
        }],
        context: DraftContext {
            review_comments: vec![serde_json::json!({
                "id":"review-1", "path":"src/lib.rs", "line":7,
                "body":"Keep this", "source":{"kind":"file"}
            })],
            quoted: Some(serde_json::json!({"messageId":"quoted-1"})),
            terminal: Some(serde_json::json!({"terminalId":"terminal-1"})),
        },
        execution: serde_json::json!({
            "draft": {
                "harness":"mock", "providerAccountId":"fixture-account",
                "model":"fixture-model", "reasoning":"medium",
                "checkout":"local", "permissionMode":"auto",
                "modelOptions":{"contextWindow":"extended"}
            }
        }),
        updated_at_ms: 123,
    }
}

#[tokio::test]
async fn rich_draft_restarts_and_missing_blob_stays_visibly_unsaved() {
    // One-pixel PNG, synthetic fixture only.
    let bytes = BASE64
        .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=")
        .unwrap();
    let root = tempfile::tempdir().unwrap();
    let core = assemble(root.path());
    let client = zeron_rpc::memory_client(core.rpc_service());
    let upload_id = "draft-fixture";
    client
        .call(
            methods::UPLOAD_CHUNK,
            serde_json::json!({
                "uploadId":upload_id,
                "data":BASE64.encode(&bytes),
                "seq":0
            }),
        )
        .await
        .unwrap();
    let committed = client
        .call(
            methods::UPLOAD_COMMIT,
            serde_json::json!({"uploadId":upload_id,"fileName":"fixture.png"}),
        )
        .await
        .unwrap();
    let path = committed["path"].as_str().unwrap().to_string();
    let draft = rich_draft(path.clone(), &bytes);
    client
        .call(
            methods::SAVE_COMPOSER_DRAFT,
            serde_json::json!({"draft":draft}),
        )
        .await
        .unwrap();
    drop(client);
    drop(core);

    let reopened = assemble(root.path());
    let client = zeron_rpc::memory_client(reopened.rpc_service());
    let read: ComposerDraftRead = client
        .call_as(
            methods::GET_COMPOSER_DRAFT,
            serde_json::json!({"taskId":"task-l08"}),
        )
        .await
        .unwrap();
    let restored = read.draft.unwrap();
    assert_eq!(restored.text, "Keep the rich draft");
    assert_eq!(
        restored.appshots[0].accessibility_content,
        "full captured synthetic context"
    );
    assert_eq!(restored.context.review_comments.len(), 1);
    assert_eq!(restored.execution["draft"]["model"], "fixture-model");
    assert_eq!(restored.attachments[0].state, DraftBlobState::Saved);

    std::fs::remove_file(&path).unwrap();
    let read: ComposerDraftRead = client
        .call_as(
            methods::GET_COMPOSER_DRAFT,
            serde_json::json!({"taskId":"task-l08"}),
        )
        .await
        .unwrap();
    let missing = &read.draft.unwrap().attachments[0];
    assert_eq!(missing.state, DraftBlobState::NeedsReattachment);
    assert!(missing.path.is_none());
    assert!(
        missing
            .error
            .as_deref()
            .is_some_and(|error| !error.is_empty())
    );

    let mut copy_failed = rich_draft(String::new(), &bytes);
    copy_failed.draft_id = "task-copy-failed".into();
    copy_failed.task_id = Some("task-copy-failed".into());
    copy_failed.attachments[0].path = None;
    copy_failed.attachments[0].sha256 = None;
    copy_failed.attachments[0].size = None;
    copy_failed.attachments[0].state = DraftBlobState::NeedsReattachment;
    copy_failed.attachments[0].error = Some("synthetic copy failure".into());
    client
        .call(
            methods::SAVE_COMPOSER_DRAFT,
            serde_json::json!({"draft":copy_failed}),
        )
        .await
        .unwrap();
    let read: ComposerDraftRead = client
        .call_as(
            methods::GET_COMPOSER_DRAFT,
            serde_json::json!({"taskId":"task-copy-failed"}),
        )
        .await
        .unwrap();
    let marker = &read.draft.unwrap().attachments[0];
    assert_eq!(marker.state, DraftBlobState::NeedsReattachment);
    assert_eq!(marker.error.as_deref(), Some("synthetic copy failure"));
}
