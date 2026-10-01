//! CC-05 / E28: deterministic 2.1.286-shaped Claude peer -> real Claude adapter
//! -> engine -> journal/doc -> lazy RPC -> engine restart. Root and child
//! ownership, ordering without duplication, privacy of synced previews and a
//! safe unknown fallback. Offline: not evidence of a live provider or native UI.
#![cfg(unix)]

use luna_doc::{MessagePart, MessageStatus, SessionMessageEntry, ToolDiffStat};
use luna_engine::{AgentAccountsConfig, EngineCore, EngineProfile, HarnessRegistry};
use luna_harness::ClaudeHarness;
use luna_proto::{HarnessId, RunRequest, SandboxLevel, SessionStatus};
use luna_rpc::{RpcReply, RpcService};
use std::{path::Path, sync::Arc, time::Duration};

const CHAT: &str = "claude-tool-results";
const CHILD_DOC: &str = "claude-tool-results--sub--toolu_agent";
const ROOT_TOOLS: [&str; 20] = [
    "toolu_bash",
    "toolu_read",
    "toolu_edit",
    "toolu_staged",
    "toolu_badpatch",
    "toolu_write",
    "toolu_nb",
    "toolu_grep",
    "toolu_mcp",
    "toolu_empty",
    "toolu_cu",
    "toolu_denied",
    "toolu_future",
    "toolu_large",
    "toolu_unavailable",
    "toolu_glob_a",
    "toolu_glob_b",
    "toolu_weird",
    "toolu_agent",
    "toolu_so",
];

fn assemble(dir: &Path) -> EngineCore {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../harness/tests/fixtures/tool-results-claude.py");
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&fixture, std::fs::Permissions::from_mode(0o755));
    }
    let registry = Arc::new(HarnessRegistry::new());
    registry.register(Arc::new(ClaudeHarness::new().with_executable(fixture)));
    let accounts = AgentAccountsConfig {
        data_dir: dir.join("synthetic-state"),
        claude_config_dir: dir.join("synthetic-claude"),
        claude_config_file: dir.join("synthetic-claude.json"),
        codex_home: dir.join("synthetic-codex"),
    };
    std::fs::write(
        &accounts.claude_config_file,
        r#"{"oauthAccount":{"accountUuid":"claude-fixture","emailAddress":"claude@example.test"}}"#,
    )
    .unwrap();
    accounts.disable_claude_keychain_fallback().unwrap();
    EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        EngineProfile::development(dir, "fixture-org", "fixture-user"),
        registry,
        HarnessId::ClaudeCode,
        None,
        accounts,
    )
    .unwrap()
}

fn entries(core: &EngineCore, doc: &str) -> Vec<SessionMessageEntry> {
    core.doc_host
        .open(doc)
        .unwrap()
        .doc()
        .read_entries()
        .unwrap_or_default()
}

fn tool<'a>(entries: &'a [SessionMessageEntry], id: &str) -> &'a MessagePart {
    let found: Vec<_> = entries
        .iter()
        .flat_map(|e| &e.parts)
        .filter(|p| p.id() == id)
        .collect();
    assert_eq!(found.len(), 1, "one chip for {id}");
    found[0]
}

fn output_summary(part: &MessagePart) -> Option<&str> {
    match part {
        MessagePart::Tool { output, .. } => output.as_deref(),
        _ => panic!("not a tool part"),
    }
}

async fn fetch(core: &EngineCore, blob_ref: &str) -> String {
    let RpcReply::Value(value) = core
        .rpc_service()
        .handle(
            luna_rpc::methods::FETCH_TOOL_BLOB,
            serde_json::json!({ "blobRef": blob_ref }),
        )
        .await
        .unwrap()
    else {
        panic!("expected lazy value for {blob_ref}")
    };
    value["text"].as_str().unwrap().to_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn claude_rich_results_survive_journal_lazy_fetch_and_restart() {
    let dir = tempfile::tempdir().unwrap();
    let core = assemble(dir.path());
    core.sessions
        .dispatch(
            CHAT,
            HarnessId::ClaudeCode,
            RunRequest {
                prompt: "scenario:rich-results".into(),
                harness: None,
                provider_account_id: None,
                model: None,
                reasoning: None,
                model_options: Default::default(),
                cwd: dir.path().display().to_string(),
                sandbox: SandboxLevel::ReadOnly,
                auto_approve: false,
                permission_mode: luna_proto::PermissionMode::Ask,
                parent_permission_ceiling: None,
                environment_permission_ceiling: None,
                permission_policy: None,
                attachments: vec![],
                input_items: Vec::new(),
                worktree: None,
                resume: None,
                review: None,
            },
            Some("fixture-user".into()),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let done = core
                .sessions
                .session_status(CHAT)
                .is_some_and(|s| s.status == SessionStatus::Idle)
                && entries(&core, CHAT)
                    .iter()
                    .any(|e| e.status == Some(MessageStatus::Complete))
                && entries(&core, CHAT)
                    .iter()
                    .flat_map(|e| &e.parts)
                    .any(|p| p.id() == "toolu_so");
            if done {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("fixture run settles");

    let completed = entries(&core, CHAT);
    // One resolved chip per root call, the replayed Bash result included.
    for id in ROOT_TOOLS {
        assert!(
            matches!(
                tool(&completed, id),
                MessagePart::Tool { resolved: true, .. }
            ),
            "resolved {id}"
        );
    }
    // Native order is preserved across the assistant entries.
    let order: Vec<&str> = completed
        .iter()
        .flat_map(|e| &e.parts)
        .filter(|p| matches!(p, MessagePart::Tool { .. }))
        .map(|p| p.id())
        .collect();
    assert_eq!(order, ROOT_TOOLS, "root chips keep native order");

    // Synced previews: typed text summary for built-ins, type/count only for
    // private (MCP/unknown) calls, and nothing for unavailable output.
    assert!(
        output_summary(tool(&completed, "toolu_bash"))
            .is_some_and(|s| s.starts_with("BASH_STDOUT_MARKER"))
    );
    assert_eq!(
        output_summary(tool(&completed, "toolu_mcp")),
        Some("Completed · 3 text · 1 image · 1 resource link · structured content")
    );
    assert_eq!(
        output_summary(tool(&completed, "toolu_empty")),
        Some("Completed · empty result")
    );
    assert_eq!(
        output_summary(tool(&completed, "toolu_future")),
        Some("Completed · 1 text · 1 unsupported block")
    );
    assert_eq!(
        output_summary(tool(&completed, "toolu_denied")),
        Some("Permission to use Bash with command rm -rf /tmp/fixture has been denied.")
    );
    assert!(matches!(
        tool(&completed, "toolu_denied"),
        MessagePart::Tool { is_error: true, .. }
    ));
    assert_eq!(
        output_summary(tool(&completed, "toolu_so")),
        Some("Completed · 1 text · structured content")
    );
    assert!(matches!(
        tool(&completed, "toolu_unavailable"),
        MessagePart::Tool {
            output: None,
            output_ref: None,
            output_bytes: None,
            ..
        }
    ));
    assert!(matches!(
        tool(&completed, "toolu_large"),
        MessagePart::Tool { output_bytes: Some(bytes), .. } if *bytes > 80_000
    ));
    // Native diffs reach the doc as stats + a lazy ref, never inline.
    for (id, path, additions, deletions) in [
        ("toolu_edit", "/fixture/a.rs", 1, 1),
        ("toolu_write", "/fixture/new.txt", 2, 0),
        ("toolu_nb", "/fixture/n.ipynb#cell-c1", 1, 1),
    ] {
        let MessagePart::Tool {
            diff,
            diff_ref,
            diff_stats,
            ..
        } = tool(&completed, id)
        else {
            unreachable!()
        };
        assert!(diff.is_none(), "{id}: no inline diff");
        assert_eq!(
            diff_ref.as_deref(),
            Some(format!("{CHAT}/{id}.diff").as_str())
        );
        assert_eq!(
            diff_stats.as_deref(),
            Some(
                &[ToolDiffStat {
                    path: path.into(),
                    additions,
                    deletions
                }][..]
            )
        );
    }
    for id in ["toolu_staged", "toolu_badpatch"] {
        assert!(
            matches!(
                tool(&completed, id),
                MessagePart::Tool {
                    diff_ref: None,
                    diff_stats: None,
                    ..
                }
            ),
            "{id}: no fabricated diff"
        );
    }

    let doc_json = serde_json::to_string(&completed).unwrap();
    for private in [
        "iVBORw0KGgo",
        "SCREENSHOT_SECRET",
        "MCP_ARGUMENT_MARKER",
        "STRUCTURED_MARKER",
        "META_MARKER",
        "RESOURCE_TEXT_MARKER",
        "FUTURE_BLOCK_MARKER",
        "FUTURE_ARGUMENT_MARKER",
        "LARGE_TAIL",
        "ORIGINAL_FILE_MARKER",
        "BASH_DESCRIPTION_ARG",
        "lunaToolOutput",
        "\"verdict\"",
    ] {
        assert!(
            !doc_json.contains(private),
            "kept out of synced doc: {private}"
        );
    }

    // Host journal + lazy RPC: complete envelopes and native diffs.
    let mcp = fetch(&core, &format!("{CHAT}/toolu_mcp")).await;
    assert_eq!(
        core.sessions
            .tool_output(CHAT, "toolu_mcp")
            .unwrap()
            .as_deref(),
        Some(mcp.as_str())
    );
    for marker in [
        "MCP_TEXT_MARKER",
        "iVBORw0KGgo",
        "RESOURCE_TEXT_MARKER",
        "STRUCTURED_MARKER",
        "META_MARKER",
        "file:///fixture/report.pdf",
        "MCP_ARGUMENT_MARKER",
    ] {
        assert!(mcp.contains(marker), "lazy MCP result retains {marker}");
    }
    let edit_diff: luna_proto::ToolDiff =
        serde_json::from_str(&fetch(&core, &format!("{CHAT}/toolu_edit.diff")).await).unwrap();
    assert_eq!(
        edit_diff.unified_diff.as_deref(),
        Some("@@ -1,3 +1,3 @@\n fn main() {\n-    before();\n+    after();\n }\n")
    );
    let large = fetch(&core, &format!("{CHAT}/toolu_large")).await;
    assert!(large.contains("LARGE_HEAD") && large.contains("LARGE_TAIL"));
    let bash = fetch(&core, &format!("{CHAT}/toolu_bash")).await;
    assert!(bash.contains("BASH_DESCRIPTION_ARG") && bash.contains("BASH_STDERR_MARKER"));
    assert!(
        !bash.contains("CHILD_TAIL"),
        "child id collision never replaces root output"
    );
    let cu = fetch(&core, &format!("{CHAT}/toolu_cu")).await;
    assert!(
        !cu.contains("SCREENSHOT_SECRET"),
        "computer-use bytes never journaled"
    );
    assert!(
        core.sessions
            .tool_output(CHAT, "toolu_unavailable")
            .unwrap()
            .is_none(),
        "unavailable stays unavailable"
    );

    // Child ownership: its colliding id lives in the subagent doc and journal scope.
    let child_entries = entries(&core, CHILD_DOC);
    assert!(matches!(
        tool(&child_entries, "toolu_bash"),
        MessagePart::Tool { resolved: true, output_ref: Some(reference), .. }
            if reference == &format!("{CHILD_DOC}/toolu_bash")
    ));
    let child_bash = fetch(&core, &format!("{CHILD_DOC}/toolu_bash")).await;
    assert!(child_bash.contains("CHILD_TAIL") && child_bash.contains("CHILD_DESCRIPTION_ARG"));
    assert!(!child_bash.contains("BASH_STDOUT_MARKER"));

    // Live previews: bounded, type/count only for native envelopes.
    let (preview_events, _) = core.sessions.subscribe(CHAT, 0).unwrap();
    let preview_json = serde_json::to_string(
        &preview_events
            .iter()
            .map(|row| serde_json::json!({"seq": row.seq, "event": row.event}))
            .collect::<Vec<_>>(),
    )
    .unwrap();
    for private in [
        "iVBORw0KGgo",
        "SCREENSHOT_SECRET",
        "LARGE_TAIL",
        "STRUCTURED_MARKER",
        "RESOURCE_TEXT_MARKER",
        "lunaToolOutput",
    ] {
        assert!(
            !preview_json.contains(private),
            "kept out of live preview: {private}"
        );
    }

    // Restart: identical docs and lazily fetched bodies.
    let journal_path = EngineProfile::development(dir.path(), "fixture-org", "fixture-user")
        .store_root()
        .join("journals")
        .join(format!("{CHAT}.jsonl"));
    let journal = std::fs::read_to_string(&journal_path).unwrap();
    assert_eq!(
        journal.matches("BASH_STDOUT_MARKER").count(),
        1,
        "replayed result journaled once"
    );
    core.shutdown().await;
    drop(core);
    let reopened = assemble(dir.path());
    assert_eq!(entries(&reopened, CHAT), completed);
    assert_eq!(entries(&reopened, CHILD_DOC), child_entries);
    assert_eq!(fetch(&reopened, &format!("{CHAT}/toolu_mcp")).await, mcp);
    assert_eq!(
        fetch(&reopened, &format!("{CHILD_DOC}/toolu_bash")).await,
        child_bash
    );

    if let Some(root) = std::env::var_os("LUNA_CC05_EVIDENCE") {
        let root = std::path::PathBuf::from(root);
        std::fs::create_dir_all(&root).unwrap();
        let write = |name: &str, bytes: &[u8]| std::fs::write(root.join(name), bytes).unwrap();
        write("doc.json", &serde_json::to_vec_pretty(&completed).unwrap());
        write(
            "child-doc.json",
            &serde_json::to_vec_pretty(&child_entries).unwrap(),
        );
        write("preview-events.json", preview_json.as_bytes());
        write("journal.jsonl", journal.as_bytes());
        for id in [
            "toolu_bash",
            "toolu_read",
            "toolu_edit",
            "toolu_write",
            "toolu_mcp",
            "toolu_empty",
            "toolu_cu",
            "toolu_denied",
            "toolu_future",
            "toolu_so",
        ] {
            write(
                &format!("{id}.json"),
                fetch(&reopened, &format!("{CHAT}/{id}")).await.as_bytes(),
            );
        }
        for id in ["toolu_edit", "toolu_write", "toolu_nb"] {
            write(
                &format!("{id}.diff.json"),
                fetch(&reopened, &format!("{CHAT}/{id}.diff"))
                    .await
                    .as_bytes(),
            );
        }
        write("child-toolu_bash.json", child_bash.as_bytes());
        write(
            "result.json",
            b"{\"offlineFixture\":true,\"cliShapes\":\"2.1.286 bundle schema\",\"adapterEngineJournalDocLazyReopen\":\"passed\",\"nativeUi\":\"fixture-png-only\",\"realProvider\":\"not-tested\"}\n",
        );
    }
    reopened.shutdown().await;
}
