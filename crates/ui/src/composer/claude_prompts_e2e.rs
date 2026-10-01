//! Headless GPUI fixture for the CC-03 Claude prompts (`claude-prompts-fixture`).
//!
//! Failure cases: `evidence/cc03-permissions/failure-cases.md` (G3). The
//! questions are read from the harness E2E artifacts (`LUNA_CC03_EVIDENCE_DIR`),
//! so every card rendered here is exactly what the real Claude adapter produced
//! against the deterministic 2.1.286-shaped peer. The production composer
//! question wizard and transcript run in a hidden window; no engine, provider,
//! LUNA app or user data is involved.
use super::*;
use gpui::{AnyWindowHandle, AsyncApp};
use luna_doc::MessageStatus;
use luna_proto::SessionStatus;
use serde_json::{Value, json};
use std::path::Path;

const CHAT: &str = "cc03-claude-prompts-fixture-chat";

fn act(
    window: AnyWindowHandle,
    cx: &mut AsyncApp,
    f: impl FnOnce(&mut Window, &mut App),
) -> anyhow::Result<()> {
    window.update(cx, |_, window, cx| f(window, cx))
}

/// Capture after the question panel's enter animation has settled.
async fn capture(window: AnyWindowHandle, cx: &mut AsyncApp, path: &Path) -> anyhow::Result<()> {
    for _ in 0..8 {
        act(window, cx, |window, cx| {
            window.draw(cx).clear();
        })?;
        cx.background_executor()
            .timer(std::time::Duration::from_millis(120))
            .await;
    }
    window.update(cx, |_, window, cx| -> anyhow::Result<()> {
        window.draw(cx).clear();
        window.render_to_image()?.save(path)?;
        Ok(())
    })?
}

/// Question set `index` parked by one adapter E2E scenario.
fn adapter_questions(
    artifacts: &Path,
    scenario: &str,
    index: usize,
) -> anyhow::Result<Vec<UserInputQuestion>> {
    let path = artifacts.join(format!("{scenario}.json"));
    let value: Value = serde_json::from_slice(
        &std::fs::read(&path).map_err(|error| anyhow::anyhow!("{}: {error}", path.display()))?,
    )?;
    let set = value["questions"]
        .as_array()
        .and_then(|sets| sets.get(index))
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("{scenario}: no question set {index}"))?;
    Ok(serde_json::from_value(set)?)
}

fn session(status: SessionStatus) -> luna_proto::Session {
    let now = chrono::Utc::now();
    luna_proto::Session {
        last_completed_turn: None,
        chat_id: CHAT.into(),
        device_id: "fixture-device".into(),
        status,
        started_at: Some(now - chrono::Duration::seconds(20)),
        updated_at: now,
        recovery: None,
    }
}

fn seed(
    state: &Entity<AppState>,
    cx: &mut AsyncApp,
    status: SessionStatus,
    reply: &str,
    request: &str,
    questions: Vec<UserInputQuestion>,
) -> anyhow::Result<()> {
    let now = chrono::Utc::now().timestamp_millis();
    let transcript = vec![
        SessionMessageEntry {
            id: "fixture-user".into(),
            role: MessageRole::User,
            parts: vec![MessagePart::Text {
                id: "u0".into(),
                text: "Run the tests and set up the tracker project.".into(),
            }],
            created_at: now - 20_000,
            device_id: "fixture-device".into(),
            status: Some(MessageStatus::Complete),
            continuation_of: None,
        },
        SessionMessageEntry {
            id: "fixture-assistant".into(),
            role: MessageRole::Assistant,
            parts: vec![
                MessagePart::Text {
                    id: "t0".into(),
                    text: reply.into(),
                },
                MessagePart::Input {
                    id: format!("{request}-part"),
                    request_id: request.into(),
                    questions,
                    resolved: false,
                },
            ],
            created_at: now - 19_000,
            device_id: "fixture-device".into(),
            status: Some(MessageStatus::Complete),
            continuation_of: None,
        },
    ];
    cx.update(|cx| {
        state.update(cx, |state, cx| {
            state.selected_chat = Some(CHAT.into());
            state.transcript_replayed = true;
            state.sessions = vec![session(status)];
            state.transcript = transcript;
            state.transcript_revision += 1;
            state.provider_notice = None;
            cx.notify();
        })
    });
    Ok(())
}

/// What the composer's question panel exposes for the current page.
fn wizard_readback(composer: &Entity<Composer>, cx: &mut AsyncApp) -> Value {
    cx.update(|cx| {
        let composer = composer.read(cx);
        let Some(wizard) = composer.wizard.as_ref() else {
            return json!({"open": false});
        };
        let question = wizard.current();
        json!({
            "open": true,
            "requestId": wizard.request_id,
            "page": wizard.counter(),
            "header": question.map(|q| q.header.clone()),
            "question": question.map(|q| q.question.clone()),
            "options": question.map(|q| q.options.clone()),
            "descriptions": question.map(|q| (0..q.options.len()).map(|ix| q.option_description(ix)).collect::<Vec<_>>()),
            "typedAnswerOffered": question.map(|q| q.accepts_typed_answer()),
            "choiceDescriptions": question
                .and_then(|q| q.approval.as_ref())
                .and_then(|a| a.details.as_ref())
                .map(|d| d.choices.iter().map(|c| c.description.clone()).collect::<Vec<_>>()),
            "detailRows": question
                .and_then(|q| q.approval.as_ref())
                .and_then(|a| a.details.as_ref())
                .map(|d| d.display_rows()),
            "expiryRow": question.and_then(|q| q.approval.as_ref()).map(approval_expiry_row),
        })
    })
}

fn has_row(readback: &Value, label: &str, needle: &str) -> bool {
    readback["detailRows"].as_array().is_some_and(|rows| {
        rows.iter().any(|row| {
            row[0] == label && row[1].as_str().is_some_and(|value| value.contains(needle))
        })
    })
}

pub async fn run_claude_prompts_e2e(
    composer: Entity<Composer>,
    state: Entity<AppState>,
    window: AnyWindowHandle,
    output: &Path,
    artifacts: &Path,
    cx: &mut AsyncApp,
) -> anyhow::Result<Value> {
    let mut evidence = serde_json::Map::new();
    let waits = json!([
        "Approval waits",
        "Until you answer, the agent stops or its turn ends"
    ]);

    // 1. Bash approval: command, cwd, sanitized reason, the exact session rule.
    seed(
        &state,
        cx,
        SessionStatus::AwaitingApproval,
        "Running the test suite first.",
        "claude-bash",
        adapter_questions(artifacts, "approval-details", 0)?,
    )?;
    capture(window, cx, &output.join("01-claude-command-approval.png")).await?;
    let readback = wizard_readback(&composer, cx);
    anyhow::ensure!(
        readback["header"] == "Approve command"
            && readback["options"]
                == json!(["Deny", "Allow once", "Allow for session", "Cancel turn"])
            && readback["expiryRow"] == waits
            && has_row(&readback, "Command / input", "npm test -- --watch=false")
            && has_row(&readback, "Working directory", "/tmp/cc03-peer")
            && has_row(
                &readback,
                "Reason",
                "This command requires approval (other)"
            )
            && has_row(&readback, "Proposed command rule", "Bash(npm test:*)"),
        "Claude command approval card: {readback}"
    );
    evidence.insert("commandApproval".into(), readback);

    // 2. Edit outside the workspace: blocked path and directory grant.
    seed(
        &state,
        cx,
        SessionStatus::AwaitingApproval,
        "Updating the hosts file.",
        "claude-edit",
        adapter_questions(artifacts, "approval-details", 1)?,
    )?;
    capture(window, cx, &output.join("02-claude-path-approval.png")).await?;
    let readback = wizard_readback(&composer, cx);
    anyhow::ensure!(
        readback["header"] == "Approve file change"
            && has_row(&readback, "Filesystem permissions", "/etc/hosts")
            && has_row(&readback, "Proposed write root", "/etc"),
        "Claude path approval card: {readback}"
    );
    evidence.insert("pathApproval".into(), readback);

    // 3. A native sub-agent's request reaches the user, attributed.
    seed(
        &state,
        cx,
        SessionStatus::AwaitingApproval,
        "The checker sub-agent is compiling.",
        "claude-subagent",
        adapter_questions(artifacts, "subagent-mcp", 0)?,
    )?;
    capture(window, cx, &output.join("03-claude-subagent-approval.png")).await?;
    let readback = wizard_readback(&composer, cx);
    anyhow::ensure!(
        readback["header"] == "Approve command · sub-agent a1b2c3"
            && has_row(&readback, "Requested by sub-agent", "a1b2c3")
            && readback["options"] == json!(["Deny", "Allow once", "Cancel turn"]),
        "Claude sub-agent approval card: {readback}"
    );
    evidence.insert("subagentApproval".into(), readback);

    // 4. MCP tool approval with server/tool/arguments context.
    seed(
        &state,
        cx,
        SessionStatus::AwaitingApproval,
        "Filing the issue in Linear.",
        "claude-mcp-tool",
        adapter_questions(artifacts, "subagent-mcp", 1)?,
    )?;
    capture(window, cx, &output.join("04-claude-mcp-tool-approval.png")).await?;
    let readback = wizard_readback(&composer, cx);
    anyhow::ensure!(
        has_row(&readback, "MCP server", "linear")
            && has_row(&readback, "Tool", "create_issue")
            && has_row(&readback, "Argument · title", "Bug"),
        "Claude MCP tool approval card: {readback}"
    );
    evidence.insert("mcpToolApproval".into(), readback);

    // 5. AskUserQuestion: option descriptions plus a typed "Other" answer.
    seed(
        &state,
        cx,
        SessionStatus::AwaitingInput,
        "A couple of choices before I scaffold.",
        "claude-ask",
        adapter_questions(artifacts, "ask-user", 0)?,
    )?;
    capture(window, cx, &output.join("05-claude-ask-user-question.png")).await?;
    let readback = wizard_readback(&composer, cx);
    anyhow::ensure!(
        readback["options"] == json!(["Postgres", "SQLite"])
            && readback["descriptions"] == json!(["Relational, robust", "Embedded file"])
            && readback["typedAnswerOffered"] == true,
        "Claude AskUserQuestion page: {readback}"
    );
    evidence.insert("askUserQuestion".into(), readback);

    // 6. MCP form elicitation through the shared wizard.
    seed(
        &state,
        cx,
        SessionStatus::AwaitingInput,
        "The tracker server needs project details.",
        "claude-form",
        adapter_questions(artifacts, "elicitation", 0)?,
    )?;
    capture(window, cx, &output.join("06-claude-mcp-form.png")).await?;
    let readback = wizard_readback(&composer, cx);
    anyhow::ensure!(
        readback["open"] == true && readback["page"] == "1/3",
        "Claude MCP form wizard: {readback}"
    );
    evidence.insert("mcpForm".into(), readback);

    // 7. MCP URL elicitation: origin only, never the secret-bearing URL.
    seed(
        &state,
        cx,
        SessionStatus::AwaitingInput,
        "The tracker server asks you to sign in.",
        "claude-url",
        adapter_questions(artifacts, "elicitation", 1)?,
    )?;
    capture(window, cx, &output.join("07-claude-mcp-url.png")).await?;
    let readback = wizard_readback(&composer, cx);
    anyhow::ensure!(
        readback["options"] == json!(["Accept", "Decline", "Cancel"])
            && readback["question"]
                .as_str()
                .is_some_and(|q| q.contains("https://auth.example.com") && !q.contains("SECRET123")),
        "Claude MCP URL card: {readback}"
    );
    evidence.insert("mcpUrl".into(), readback);
    Ok(Value::Object(evidence))
}
