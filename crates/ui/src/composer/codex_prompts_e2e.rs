//! Headless GPUI fixture for the codex-adapter prompts (`codex-prompts-fixture`).
//!
//! Failure cases: `evidence/codex-adapter/failure-cases.md`. The questions are
//! read from the harness E2E artifacts (`LUNA_CODEX_ADAPTER_EVIDENCE_DIR`), so
//! the cards rendered here are exactly what the real adapter produced against
//! the deterministic Codex peer. The production composer, question wizard,
//! transcript working trailer and answer input run in a hidden window; no
//! engine, provider, LUNA app or user data is involved.
use super::*;
use gpui::{AnyWindowHandle, AsyncApp};
use serde_json::{Value, json};
use std::path::Path;

const CHAT: &str = "codex-adapter-fixture-chat";

fn act(
    window: AnyWindowHandle,
    cx: &mut AsyncApp,
    f: impl FnOnce(&mut Window, &mut App),
) -> anyhow::Result<()> {
    window.update(cx, |_, window, cx| f(window, cx))
}

fn capture(window: AnyWindowHandle, cx: &mut AsyncApp, path: &Path) -> anyhow::Result<()> {
    window.update(cx, |_, window, cx| -> anyhow::Result<()> {
        window.draw(cx).clear();
        window.draw(cx).clear();
        window.render_to_image()?.save(path)?;
        Ok(())
    })?
}

/// The first parked question set of one adapter E2E scenario.
fn adapter_questions(artifacts: &Path, scenario: &str) -> anyhow::Result<Vec<UserInputQuestion>> {
    let path = artifacts.join(format!("{scenario}.json"));
    let value: Value = serde_json::from_slice(
        &std::fs::read(&path).map_err(|error| anyhow::anyhow!("{}: {error}", path.display()))?,
    )?;
    let first = value["questions"]
        .as_array()
        .and_then(|sets| sets.first())
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("{scenario}: no parked questions"))?;
    Ok(serde_json::from_value(first)?)
}

fn session(status: SessionStatus, started_secs_ago: i64) -> luna_proto::Session {
    let now = chrono::Utc::now();
    luna_proto::Session {
        last_completed_turn: None,
        chat_id: CHAT.into(),
        device_id: "fixture-device".into(),
        status,
        started_at: Some(now - chrono::Duration::seconds(started_secs_ago)),
        updated_at: now,
        recovery: None,
    }
}

fn entries(
    reply: &str,
    status: MessageStatus,
    extra: Vec<MessagePart>,
) -> Vec<SessionMessageEntry> {
    let now = chrono::Utc::now().timestamp_millis();
    let mut parts = vec![MessagePart::Text {
        id: "t0".into(),
        text: reply.into(),
    }];
    parts.extend(extra);
    vec![
        SessionMessageEntry {
            id: "fixture-user".into(),
            role: MessageRole::User,
            parts: vec![MessagePart::Text {
                id: "u0".into(),
                text: "Index the repository and file an issue for anything broken.".into(),
            }],
            created_at: now - 20_000,
            device_id: "fixture-device".into(),
            status: Some(MessageStatus::Complete),
            continuation_of: None,
        },
        SessionMessageEntry {
            id: "fixture-assistant".into(),
            role: MessageRole::Assistant,
            parts,
            created_at: now - 19_000,
            device_id: "fixture-device".into(),
            status: Some(status),
            continuation_of: None,
        },
    ]
}

fn seed(
    state: &Entity<AppState>,
    cx: &mut AsyncApp,
    session: luna_proto::Session,
    transcript: Vec<SessionMessageEntry>,
    notice: Option<luna_proto::ProviderNotice>,
) -> anyhow::Result<()> {
    cx.update(|cx| {
        state.update(cx, |state, cx| {
            state.selected_chat = Some(CHAT.into());
            state.transcript_replayed = true;
            state.sessions = vec![session];
            state.transcript = transcript;
            state.transcript_revision += 1;
            state.provider_notice = notice;
            cx.notify();
        })
    });
    Ok(())
}

fn input_part(request: &str, questions: Vec<UserInputQuestion>) -> MessagePart {
    MessagePart::Input {
        id: format!("{request}-part"),
        request_id: request.into(),
        questions,
        resolved: false,
    }
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
            "options": question.map(|q| q.options.clone()),
            "descriptions": question.map(|q| (0..q.options.len()).map(|ix| q.option_description(ix)).collect::<Vec<_>>()),
            "typedAnswerOffered": question.map(|q| q.accepts_typed_answer()),
            "secret": question.map(|q| q.is_secret()),
            "detailRows": question
                .and_then(|q| q.approval.as_ref())
                .and_then(|a| a.details.as_ref())
                .map(|d| d.display_rows()),
            "expiryRow": question.and_then(|q| q.approval.as_ref()).map(approval_expiry_row),
            "answerInputMasked": composer.answer_input.read(cx).masked,
            "answerInputDisplay": composer.answer_input.read(cx).projection.display.clone(),
        })
    })
}

pub async fn run_codex_prompts_e2e(
    composer: Entity<Composer>,
    state: Entity<AppState>,
    window: AnyWindowHandle,
    output: &Path,
    artifacts: &Path,
    cx: &mut AsyncApp,
) -> anyhow::Result<Value> {
    let mut evidence = serde_json::Map::new();

    // 1. A Codex stream retry is a transient working-indicator notice.
    seed(
        &state,
        cx,
        session(SessionStatus::Working, 14),
        entries(
            "Scanning the workspace for failing tests.",
            MessageStatus::Streaming,
            Vec::new(),
        ),
        Some(luna_proto::ProviderNotice {
            kind: luna_proto::ProviderNoticeKind::Retrying,
            message: "Reconnecting... 2/5".into(),
            at_ms: chrono::Utc::now().timestamp_millis(),
        }),
    )?;
    capture(window, cx, &output.join("01-retry-notice.png"))?;
    evidence.insert(
        "retryNotice".into(),
        cx.update(|cx| json!(state.read(cx).provider_notice)),
    );

    // 2. MCP tool-call approval as an approval card (adapter output).
    let tool = adapter_questions(artifacts, "mcp-tool-session")?;
    seed(
        &state,
        cx,
        session(SessionStatus::AwaitingApproval, 30),
        entries(
            "I found two failures. Filing the issue now.",
            MessageStatus::Complete,
            vec![input_part("mcp-tool-request", tool)],
        ),
        None,
    )?;
    capture(window, cx, &output.join("02-mcp-tool-approval.png"))?;
    let readback = wizard_readback(&composer, cx);
    anyhow::ensure!(
        readback["header"] == "Approve MCP tool"
            && readback["options"]
                == json!([
                    "Allow once",
                    "Allow for session",
                    "Always allow",
                    "Deny",
                    "Cancel"
                ]),
        "MCP tool approval card: {readback}"
    );
    evidence.insert("mcpToolApproval".into(), readback);

    // 3. A native sub-agent's command approval, attributed to the sub-agent.
    let child = adapter_questions(artifacts, "subagent-command")?;
    seed(
        &state,
        cx,
        session(SessionStatus::AwaitingApproval, 45),
        entries(
            "Alpha is writing the fixture file.",
            MessageStatus::Complete,
            vec![input_part("subagent-request", child)],
        ),
        None,
    )?;
    capture(window, cx, &output.join("03-subagent-approval.png"))?;
    let readback = wizard_readback(&composer, cx);
    anyhow::ensure!(
        readback["header"] == "Approve command · sub-agent alpha",
        "sub-agent approval card: {readback}"
    );
    evidence.insert("subagentApproval".into(), readback);

    // 4. requestUserInput: option descriptions; isOther:false hides free text.
    let ask = adapter_questions(artifacts, "request-input")?;
    seed(
        &state,
        cx,
        session(SessionStatus::AwaitingInput, 50),
        entries(
            "I need two details before continuing.",
            MessageStatus::Complete,
            vec![input_part("ask-request", ask)],
        ),
        None,
    )?;
    capture(window, cx, &output.join("04-request-input-options.png"))?;
    let readback = wizard_readback(&composer, cx);
    anyhow::ensure!(
        readback["descriptions"] == json!(["Calm", "Loud"])
            && readback["typedAnswerOffered"] == false,
        "option page: {readback}"
    );
    evidence.insert("requestInputOptions".into(), readback);

    // 5. Pick "Blue", advance to the secret page and type: the input masks it.
    act(window, cx, |_, cx| {
        composer.update(cx, |composer, cx| {
            composer.wizard_select(0, cx);
            composer.wizard_advance(cx);
        })
    })?;
    act(window, cx, |window, cx| {
        composer.update(cx, |composer, cx| {
            composer.answer_input.update(cx, |input, cx| {
                input.set_text("s3cret-token", cx);
                window.focus(&input.focus_handle, cx);
            })
        })
    })?;
    capture(window, cx, &output.join("05-request-input-secret.png"))?;
    let readback = wizard_readback(&composer, cx);
    anyhow::ensure!(
        readback["secret"] == true
            && readback["answerInputMasked"] == true
            && readback["answerInputDisplay"] == "************",
        "secret page: {readback}"
    );
    evidence.insert("requestInputSecret".into(), readback);
    Ok(Value::Object(evidence))
}
