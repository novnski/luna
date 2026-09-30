//! Offline protocol peer -> real Codex adapter -> engine -> journal/doc -> lazy
//! RPC and reopened history. Not evidence of a live provider or native UI.
#![cfg(unix)]

use luna_doc::{MessagePart, MessageStatus, SessionMessageEntry};
use luna_engine::{AgentAccountsConfig, EngineCore, EngineProfile, HarnessRegistry};
use luna_harness::CodexHarness;
use luna_proto::{HarnessId, RunRequest, SandboxLevel, SessionStatus};
use luna_rpc::{RpcReply, RpcService};
use std::{path::Path, sync::Arc, time::Duration};

const CHAT: &str = "lossless-tool-output";

fn assemble(dir: &Path) -> EngineCore {
    let registry = Arc::new(HarnessRegistry::new());
    registry.register(Arc::new(
        CodexHarness::new().with_executable(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../harness/tests/fixtures/tool-output-codex.py"),
        ),
    ));
    let accounts = AgentAccountsConfig {
        data_dir: dir.join("synthetic-state"),
        claude_config_dir: dir.join("synthetic-claude"),
        claude_config_file: dir.join("synthetic-claude.json"),
        codex_home: dir.join("synthetic-codex"),
    };
    accounts.disable_claude_keychain_fallback().unwrap();
    EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        EngineProfile::development(dir, "fixture-org", "fixture-user"),
        registry,
        HarnessId::Codex,
        None,
        accounts,
    )
    .unwrap()
}

fn entries(core: &EngineCore) -> Vec<SessionMessageEntry> {
    core.doc_host
        .open(CHAT)
        .unwrap()
        .doc()
        .read_entries()
        .unwrap_or_default()
}

async fn wait_for(core: &EngineCore, condition: impl Fn(&[SessionMessageEntry]) -> bool) {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if condition(&entries(core)) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("fixture state arrives");
}

#[tokio::test(flavor = "multi_thread")]
async fn received_tool_payloads_and_live_activity_survive_steer_and_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let core = assemble(dir.path());
    core.sessions
        .dispatch(
            CHAT,
            HarnessId::Codex,
            RunRequest {
                prompt: "offline fixture".into(),
                harness: None,
                provider_account_id: None,
                model: None,
                reasoning: None,
                model_options: Default::default(),
                cwd: dir.path().display().to_string(),
                sandbox: SandboxLevel::ReadOnly,
                auto_approve: true,
                permission_mode: luna_proto::PermissionMode::Ask,
                parent_permission_ceiling: None,
                environment_permission_ceiling: None,
                permission_policy: None,
                attachments: vec![],
                worktree: None,
                resume: None,
            },
            Some("fixture-user".into()),
        )
        .await
        .unwrap();
    wait_for(&core, |entries| {
        entries.iter().flat_map(|e| &e.parts).any(|p| {
            matches!(p,
                MessagePart::Tool { id, resolved: false, output: Some(output), .. }
                if id == "command" && output.contains("INPUT_BEFORE_STEER")
            )
        })
    })
    .await;
    let original = entries(&core)
        .into_iter()
        .find(|e| e.parts.iter().any(|p| p.id() == "command"))
        .unwrap()
        .id;
    assert!(
        core.sessions
            .tool_output(CHAT, "command")
            .unwrap()
            .unwrap()
            .contains("LIVE_BEFORE_STEER")
    );
    core.sessions
        .steer(CHAT, "fixture steer", Some("fixture-steer".into()))
        .await
        .unwrap();
    wait_for(&core, |entries| {
        entries.iter().flat_map(|e| &e.parts).any(|p| {
            matches!(p,
                MessagePart::Tool { id, resolved: false, output: Some(output), .. }
                if id == "command" && output.contains("LIVE_AFTER_STEER")
            )
        })
    })
    .await;
    wait_for(&core, |entries| {
        core.sessions
            .session_status(CHAT)
            .is_some_and(|s| s.status == SessionStatus::Idle)
            && entries
                .iter()
                .any(|e| e.status == Some(MessageStatus::Complete))
            && entries
                .iter()
                .flat_map(|e| &e.parts)
                .any(|p| p.id() == "unknown")
    })
    .await;
    let completed = entries(&core);
    let command_entries: Vec<_> = completed
        .iter()
        .filter(|e| e.parts.iter().any(|p| p.id() == "command"))
        .collect();
    assert_eq!(command_entries.len(), 1);
    assert_eq!(command_entries[0].id, original);
    for id in ["patch", "patch#1", "patch#2"] {
        let owner = completed
            .iter()
            .find(|entry| entry.parts.iter().any(|part| part.id() == id))
            .unwrap();
        assert_eq!(
            owner.id, original,
            "patch fan-out stays at original call site: {id}"
        );
    }
    for id in [
        "command",
        "patch",
        "patch#1",
        "patch#2",
        "web",
        "rich",
        "list_threads",
        "read_thread",
        "send_message_to_thread",
        "wait_threads",
        "luna_status",
        "unknown",
    ] {
        assert_eq!(
            completed
                .iter()
                .flat_map(|e| &e.parts)
                .filter(|p| p.id() == id)
                .count(),
            1,
            "one chip for {id}"
        );
        assert!(
            completed.iter().flat_map(|e| &e.parts).any(
                |p| matches!(p, MessagePart::Tool { id: pid, resolved: true, .. } if pid == id)
            ),
            "resolved {id}"
        );
    }
    let command = core.sessions.tool_output(CHAT, "command").unwrap().unwrap();
    for marker in [
        "COMMAND_HEAD",
        "COMMAND_TAIL",
        "LIVE_BEFORE_STEER",
        "LIVE_AFTER_STEER",
        "INPUT_BEFORE_STEER",
    ] {
        assert!(command.contains(marker));
    }
    assert!(command.len() > 100000);
    let patch = core
        .sessions
        .tool_output(CHAT, "patch.diff")
        .unwrap()
        .unwrap();
    assert!(patch.contains("PATCH_HEAD") && patch.contains("PATCH_TAIL") && patch.len() > 80000);
    assert!(
        core.sessions
            .tool_output(CHAT, "patch")
            .unwrap()
            .unwrap()
            .contains("PATCH_PROGRESS"),
        "duplicate completion keeps live history"
    );
    assert!(
        core.sessions
            .tool_output(CHAT, "web")
            .unwrap()
            .unwrap()
            .contains("WEB_TAIL")
    );
    let rich = core.sessions.tool_output(CHAT, "rich").unwrap().unwrap();
    for marker in [
        "MCP_TAIL",
        "SU1BR0U=",
        "QVVESU8=",
        "RESOURCE_TAIL",
        "STRUCTURED_TAIL",
        "META_TAIL",
        "MCP_ERROR_TAIL",
        "ui://fixture",
    ] {
        assert!(rich.contains(marker), "retains {marker}");
    }
    let child_doc = format!("{CHAT}--sub--spawn-child");
    let child_entries = core
        .doc_host
        .open(&child_doc)
        .unwrap()
        .doc()
        .read_entries()
        .unwrap();
    assert!(
        child_entries
            .iter()
            .flat_map(|e| &e.parts)
            .any(|p| matches!(p,
                MessagePart::Tool { id, output_ref: Some(reference), resolved: true, .. }
                if id == "command" && reference == &format!("{child_doc}/command")
            ))
    );
    let child_output = core
        .sessions
        .tool_output(&child_doc, "command")
        .unwrap()
        .unwrap();
    assert!(child_output.contains("CHILD_LIVE") && child_output.contains("CHILD_TAIL"));
    assert!(
        !command.contains("CHILD_TAIL"),
        "colliding child item ID does not replace parent output"
    );
    let (preview_events, _) = core.sessions.subscribe(CHAT, 0).unwrap();
    let command_preview = preview_events
        .iter()
        .find_map(|row| match &row.event {
            luna_proto::AgentEvent::ToolResult {
                id,
                output: Some(output),
                ..
            } if id == "command" => Some(output),
            _ => None,
        })
        .unwrap();
    assert!(command_preview.len() < 66000 && !command_preview.contains("COMMAND_TAIL"));
    let preview_json = serde_json::to_string(
        &preview_events
            .iter()
            .map(|row| serde_json::json!({"seq":row.seq,"event":row.event}))
            .collect::<Vec<_>>(),
    )
    .unwrap();
    assert!(!preview_json.contains("DYNAMIC_RESULT_SECRET") && !preview_json.contains("SU1BR0U="));
    let doc_json = serde_json::to_string(&completed).unwrap();
    for secret in [
        "ARGUMENT_SECRET",
        "DYNAMIC_ARGUMENT_SECRET",
        "DYNAMIC_RESULT_SECRET",
        "UNKNOWN_SECRET",
        "SU1BR0U=",
        "QVVESU8=",
    ] {
        assert!(
            !doc_json.contains(secret),
            "private payload excluded from synced doc: {secret}"
        );
    }
    assert!(doc_json.len() < 25000);
    assert!(
        doc_json.contains("image") && doc_json.contains("audio") && doc_json.contains("structured")
    );
    assert!(
        completed
            .iter()
            .flat_map(|e| &e.parts)
            .any(|p| matches!(p, MessagePart::Tool { id, is_error: true, .. } if id == "rich"))
    );
    let RpcReply::Value(lazy) = core
        .rpc_service()
        .handle(
            luna_rpc::methods::FETCH_TOOL_BLOB,
            serde_json::json!({"blobRef":format!("{CHAT}/command")}),
        )
        .await
        .unwrap()
    else {
        panic!("expected lazy value")
    };
    assert_eq!(lazy["text"].as_str(), Some(command.as_str()));
    core.shutdown().await;
    drop(core);
    let reopened = assemble(dir.path());
    assert_eq!(entries(&reopened), completed);
    assert_eq!(
        reopened
            .sessions
            .tool_output(CHAT, "command")
            .unwrap()
            .as_deref(),
        Some(command.as_str())
    );
    assert_eq!(
        reopened
            .doc_host
            .open(&child_doc)
            .unwrap()
            .doc()
            .read_entries()
            .unwrap(),
        child_entries
    );
    assert_eq!(
        reopened
            .sessions
            .tool_output(&child_doc, "command")
            .unwrap()
            .as_deref(),
        Some(child_output.as_str())
    );
    if let Some(root) = std::env::var_os("LUNA_TOOL_OUTPUT_EVIDENCE") {
        let root = std::path::PathBuf::from(root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("doc.json"),
            serde_json::to_vec_pretty(&completed).unwrap(),
        )
        .unwrap();
        std::fs::write(root.join("preview-events.json"), preview_json).unwrap();
        std::fs::write(
            root.join("child-doc.json"),
            serde_json::to_vec_pretty(&child_entries).unwrap(),
        )
        .unwrap();
        std::fs::write(root.join("child-output.txt"), child_output).unwrap();
        let journal = EngineProfile::development(dir.path(), "fixture-org", "fixture-user")
            .store_root()
            .join("journals")
            .join(format!("{CHAT}.jsonl"));
        std::fs::copy(journal, root.join("journal.jsonl")).unwrap();
        std::fs::write(root.join("command.txt"), command).unwrap();
        std::fs::write(root.join("patch.json"), patch).unwrap();
        std::fs::write(root.join("mcp.json"), rich).unwrap();
        std::fs::write(root.join("result.json"), b"{\"offlineFixture\":true,\"adapterEngineJournalDocLazyReopen\":\"passed\",\"nativeUi\":\"not-tested\",\"realProvider\":\"not-tested\"}\n").unwrap();
    }
    reopened.shutdown().await;
}
