//! E25 (CC-08): the real ClaudeHarness driving a 2.1.286-shaped native agent
//! graph through the engine's document writer. Offline: the fake CLI replays
//! `harness/tests/fixtures/claude/native-agents-2.1.286.jsonl`; no installed
//! agent or model account is involved.
//!
//! Failure cases: `$B/evidence/cc08-native-agents/failure-cases.md`.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use luna_doc::{
    MessagePart, MessageRole, MessageStatus, SessionMessageEntry, SubagentActivity, SubagentStatus,
};
use luna_engine::{AgentAccountsConfig, EngineCore, EngineProfile, HarnessRegistry};
use luna_harness::ClaudeHarness;
use luna_proto::{HarnessId, RunRequest, SandboxLevel, SessionStatus};

const CHAT: &str = "claude-native-agents";

fn assemble(dir: &Path) -> EngineCore {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../harness/tests/fixtures/fake-claude.sh");
    let registry = Arc::new(HarnessRegistry::new());
    registry.register(Arc::new(ClaudeHarness::new().with_executable(fixture)));
    let profile = EngineProfile::development(dir, "test-org", "test-user");
    let accounts = AgentAccountsConfig {
        data_dir: dir.join("synthetic-provider-state"),
        claude_config_dir: dir.join("synthetic-claude"),
        claude_config_file: dir.join("synthetic-claude.json"),
        codex_home: dir.join("synthetic-codex"),
    };
    std::fs::write(
        &accounts.claude_config_file,
        r#"{"oauthAccount":{"accountUuid":"synthetic-claude-account"}}"#,
    )
    .unwrap();
    accounts.disable_claude_keychain_fallback().unwrap();
    EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        profile,
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

fn text(entries: &[SessionMessageEntry], role: MessageRole) -> String {
    entries
        .iter()
        .filter(|e| e.role == role)
        .flat_map(|e| &e.parts)
        .filter_map(|p| match p {
            MessagePart::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// (subagent ref, status, activity, output) of the tool chip `id`.
type Chip = (
    Option<String>,
    Option<SubagentStatus>,
    Option<SubagentActivity>,
    Option<String>,
);

fn chip(entries: &[SessionMessageEntry], id: &str) -> Option<Chip> {
    entries.iter().flat_map(|e| &e.parts).find_map(|p| match p {
        MessagePart::Tool {
            id: got,
            subagent_ref,
            subagent_status,
            subagent_activity,
            output,
            ..
        } if got == id => Some((
            subagent_ref.clone(),
            *subagent_status,
            subagent_activity.clone(),
            output.clone(),
        )),
        _ => None,
    })
}

fn sub(id: &str) -> String {
    format!("{CHAT}--sub--{id}")
}

#[tokio::test(flavor = "multi_thread")]
async fn nested_native_agents_land_in_their_owning_transcripts() {
    let dir = tempfile::tempdir().unwrap();
    let core = assemble(dir.path());
    let request = RunRequest {
        goal: None,
        review: None,
        input_items: Vec::new(),
        prompt: "scenario:native-nested".into(),
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
    };
    core.sessions
        .dispatch(
            CHAT,
            HarnessId::ClaudeCode,
            request,
            Some("user-prompt".into()),
        )
        .await
        .unwrap();
    // Wait for the wake turn (second root result) and every child settling.
    let settled = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let root = entries(&core, CHAT);
            let settled = ["toolu_C", "toolu_D"].iter().all(|id| {
                chip(&root, id).is_some_and(|(_, status, ..)| {
                    matches!(
                        status,
                        Some(SubagentStatus::Interrupted | SubagentStatus::Failed)
                    )
                })
            });
            if settled
                && text(&root, MessageRole::Assistant).contains("Background agents finished.")
                && core
                    .sessions
                    .session_status(CHAT)
                    .is_some_and(|s| s.status == SessionStatus::Idle)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    if settled.is_err() {
        let dump: Vec<String> = entries(&core, CHAT)
            .iter()
            .map(|e| {
                let parts: Vec<String> = e
                    .parts
                    .iter()
                    .map(|p| match p {
                        MessagePart::Text { text, .. } => format!("text:{text:?}"),
                        MessagePart::Tool {
                            id,
                            subagent_status,
                            subagent_ref,
                            resolved,
                            ..
                        } => format!("tool:{id}:{subagent_status:?}:{subagent_ref:?}:{resolved}"),
                        other => format!("{other:?}").chars().take(60).collect(),
                    })
                    .collect();
                format!("{:?}/{:?}: {}", e.role, e.status, parts.join(" | "))
            })
            .collect();
        panic!(
            "did not settle; session={:?}\n{}",
            core.sessions.session_status(CHAT).map(|s| s.status),
            dump.join("\n")
        );
    }

    let root = entries(&core, CHAT);
    let root_text = text(&root, MessageRole::Assistant);
    // No child content, unknown/duplicate/workflow notification or hook row
    // reaches the root transcript.
    for leaked in [
        "Splitting",
        "Lexer",
        "Token kinds",
        "Parser mapped:",
        "Build is green",
        "Checking links",
        "ghost",
        "spec workflow",
        "late duplicate",
    ] {
        assert!(!root_text.contains(leaked), "{leaked} leaked: {root_text}");
    }
    // Root spawn chips: transcript link, terminal status and the provider's
    // reported model, task usage and background flag. Usage is a total,
    // never an input/output split.
    let (a_ref, a_status, a_activity, _) = chip(&root, "toolu_A").expect("A chip");
    assert_eq!(a_ref.as_deref(), Some(sub("toolu_A").as_str()));
    assert_eq!(a_status, Some(SubagentStatus::Done));
    let a_activity = a_activity.expect("A activity");
    assert_eq!(
        a_activity.model.as_deref(),
        Some("claude-haiku-4-5-20251001")
    );
    assert_eq!(
        (
            a_activity.total_tokens,
            a_activity.tool_uses,
            a_activity.duration_ms
        ),
        (Some(2400), Some(3), Some(9000))
    );
    assert_eq!(
        (a_activity.input_tokens, a_activity.output_tokens),
        (None, None)
    );
    assert_eq!(a_activity.result.as_deref(), Some("Parser mapped"));
    let (_, c_status, c_activity, _) = chip(&root, "toolu_C").expect("C chip");
    assert_eq!(c_status, Some(SubagentStatus::Interrupted));
    let c_activity = c_activity.expect("C activity");
    assert_eq!(c_activity.background, Some(true));
    assert_eq!(
        c_activity.error.as_deref(),
        Some("Stopped: the Claude worker restarted")
    );
    let (_, d_status, d_activity, _) = chip(&root, "toolu_D").expect("D chip");
    assert_eq!(d_status, Some(SubagentStatus::Failed));
    assert_eq!(
        d_activity.and_then(|a| a.error).as_deref(),
        Some("Docs site unreachable")
    );
    // Nested chips live in A's transcript, never in the root's.
    for nested in ["toolu_B", "toolu_E", "toolu_A_bg"] {
        assert!(chip(&root, nested).is_none(), "{nested} chip in root");
    }

    // A's transcript: opening prompt, forwarded text, its nested spawn chips
    // linked to their own transcripts with lifecycle and model, and its
    // background job's completion on the job's own chip (not a spawn link).
    let a = entries(&core, &sub("toolu_A"));
    assert_eq!(
        text(&a, MessageRole::User),
        "Map the parser module and delegate the lexer."
    );
    let a_text = text(&a, MessageRole::Assistant);
    assert!(a_text.contains("Splitting the work."), "{a_text}");
    assert!(a_text.contains("Parser mapped: lexer and parser stages."));
    assert!(
        !a_text.contains("Lexer has three states.\n\n"),
        "B text in A"
    );
    assert!(
        a.iter().flat_map(|e| &e.parts).any(|p| matches!(p,
            MessagePart::Reasoning { text, .. } if text.contains("The lexer is separable")
        )),
        "forwarded thinking missing: {a:?}"
    );
    let (b_ref, b_status, b_activity, _) = chip(&a, "toolu_B").expect("nested B chip in A");
    assert_eq!(b_ref.as_deref(), Some(sub("toolu_B").as_str()));
    assert_eq!(b_status, Some(SubagentStatus::Done));
    let b_activity = b_activity.expect("B activity");
    assert_eq!(b_activity.model.as_deref(), Some("claude-sonnet-5"));
    assert_eq!(b_activity.total_tokens, Some(800));
    assert_eq!(b_activity.result.as_deref(), Some("Lexer has three states"));
    let (e_ref, e_status, e_activity, _) = chip(&a, "toolu_E").expect("nested E chip in A");
    assert_eq!(e_ref.as_deref(), Some(sub("toolu_E").as_str()));
    assert_eq!(e_status, Some(SubagentStatus::Done));
    assert_eq!(e_activity.and_then(|a| a.background), Some(true));
    let (bg_ref, bg_status, _, bg_output) = chip(&a, "toolu_A_bg").expect("A's job chip");
    assert_eq!((bg_ref, bg_status), (None, None), "job chip is not a spawn");
    assert!(
        bg_output
            .as_deref()
            .is_some_and(|o| o.contains("Background job completed")),
        "{bg_output:?}"
    );

    // B and E transcripts: their own assignment and content only.
    let b = entries(&core, &sub("toolu_B"));
    assert_eq!(text(&b, MessageRole::User), "Summarize lexer.rs states.");
    assert_eq!(
        text(&b, MessageRole::Assistant),
        "Lexer has three states.\n\n"
    );
    let e = entries(&core, &sub("toolu_E"));
    assert_eq!(
        text(&e, MessageRole::User),
        "List the token kinds.Include keywords too."
    );
    assert_eq!(
        text(&e, MessageRole::Assistant),
        "Token kinds: ident, number, keyword.\n\n"
    );
    for doc in [&a, &b, &e] {
        assert!(
            doc.iter()
                .filter(|e| e.role == MessageRole::Assistant)
                .all(|e| e.status == Some(MessageStatus::Complete)),
            "{doc:?}"
        );
    }
    core.shutdown().await;
}
