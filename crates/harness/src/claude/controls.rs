//! CC-03: one correlated dispatcher for every Claude CLI → host control
//! request, written against the Claude Code 2.1.286 SDK control schema
//! (`can_use_tool`, `elicitation`, `request_user_dialog`,
//! `control_cancel_request`; excerpts kept with the cc03 evidence).
//!
//! Guarantees (failure cases: `evidence/cc03-permissions/failure-cases.md`):
//! - One terminal response per request id. A request leaves `pending` exactly
//!   once — by its answer, a native `control_cancel_request`, Stop or session
//!   end — and only the answer path writes a `control_response`.
//! - A replayed request id, live or already settled, is rendered once and
//!   never answered twice.
//! - Approvals wait for the user: no LUNA deadline (`APPROVAL_NO_EXPIRY`). A
//!   native cancel drops the answer receiver, so the engine retires the card.
//! - Client labels never manufacture a grant: an answer selects one of the
//!   responses held here; anything else denies.
//! - Plan tools stay denied (Plan/Build is cancelled), and a tool whose own
//!   card is the interaction surface is never one-tap approved.
//! - The native permission mode Claude reports is checked against the mode
//!   LUNA launched (`profile::mode_args`, owned by the G1 baseline).

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};

use serde_json::{Map, Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use luna_proto::{
    APPROVAL_NO_EXPIRY, ApprovalActionKind, ApprovalChoice, ApprovalDetails, ApprovalRequest,
    ApprovalToolContext, HarnessId, PermissionMode, PermissionPolicy, UserInputAnswer,
    UserInputDetails, UserInputQuestion,
};

use super::wire::control_response_line;
use super::{RequestInputFn, StdinMsg};

/// Display bound for any provider-authored text placed on a synced card.
const MAX_DISPLAY: usize = 4096;
/// Settled/withdrawn ids remembered to ignore replays.
const MAX_RETIRED: usize = 1024;
const PLAN_TOOLS: [&str; 2] = ["EnterPlanMode", "ExitPlanMode"];

/// Outcome of comparing Claude's reported permission mode with the launch.
#[derive(Debug)]
pub(super) struct ModeVerdict {
    pub message: String,
    /// The native mode is weaker than (or unknown next to) the launched one:
    /// the run must stop rather than act without the chosen protection.
    pub fail_closed: bool,
}

#[derive(Clone)]
pub(super) struct Controls {
    inner: Arc<Inner>,
}

struct Inner {
    title_only: bool,
    mode: PermissionMode,
    policy: PermissionPolicy,
    request_input: Arc<RequestInputFn>,
    stdin_tx: mpsc::UnboundedSender<StdinMsg>,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    /// Live requests and the token that withdraws their answer task.
    pending: HashMap<String, CancellationToken>,
    retired: HashSet<String>,
    retired_order: VecDeque<String>,
    session_id: String,
    cwd: String,
    reported_mode: Option<String>,
}

impl State {
    fn retire(&mut self, id: &str) {
        if self.retired.insert(id.to_owned()) {
            self.retired_order.push_back(id.to_owned());
            while self.retired_order.len() > MAX_RETIRED {
                if let Some(old) = self.retired_order.pop_front() {
                    self.retired.remove(&old);
                }
            }
        }
    }
}

/// `request_id` of a CLI `control_cancel_request` line, if `line` is one.
pub(super) fn cancelled_request_id(line: &str) -> Option<String> {
    if !line.contains("control_cancel_request") {
        return None;
    }
    let value: Value = serde_json::from_str(line).ok()?;
    if value.get("type").and_then(Value::as_str) != Some("control_cancel_request") {
        return None;
    }
    value
        .get("request_id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
}

impl Controls {
    pub(super) fn new(
        title_only: bool,
        mode: PermissionMode,
        policy: PermissionPolicy,
        request_input: Arc<RequestInputFn>,
        stdin_tx: mpsc::UnboundedSender<StdinMsg>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                title_only,
                mode,
                policy,
                request_input,
                stdin_tx,
                state: Mutex::new(State::default()),
            }),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Admit a new request id; `None` for a replay or an id already settled.
    fn admit(&self, id: &str) -> Option<CancellationToken> {
        let mut state = self.state();
        if state.pending.contains_key(id) || state.retired.contains(id) {
            return None;
        }
        let token = CancellationToken::new();
        state.pending.insert(id.to_owned(), token.clone());
        Some(token)
    }

    /// The single terminal response for `id`. False (nothing written) when the
    /// request was already answered or withdrawn.
    fn settle(&self, id: &str, line: String) -> bool {
        let mut state = self.state();
        if state.pending.remove(id).is_none() {
            return false;
        }
        state.retire(id);
        // Written under the lock so a racing cancel cannot interleave.
        let _ = self.inner.stdin_tx.send(StdinMsg::line(line));
        true
    }

    fn answer(&self, id: &str, response: Value) -> bool {
        self.settle(id, control_response_line(id, response))
    }

    fn fail(&self, id: &str, error: &str) -> bool {
        let line = json!({
            "type": "control_response",
            "response": {"subtype": "error", "request_id": id, "error": error},
        })
        .to_string();
        self.settle(id, line)
    }

    /// The CLI withdrew `id`: stop waiting and never answer it.
    pub(super) fn cancel(&self, id: &str) {
        let mut state = self.state();
        if let Some(token) = state.pending.remove(id) {
            token.cancel();
        }
        state.retire(id);
    }

    /// Stop or session end: withdraw every pending request without answering.
    pub(super) fn cancel_all(&self) {
        let mut state = self.state();
        let ids: Vec<String> = state.pending.keys().cloned().collect();
        for id in ids {
            if let Some(token) = state.pending.remove(&id) {
                token.cancel();
            }
            state.retire(&id);
        }
    }

    /// Record init/status context and verify the reported native mode.
    pub(super) fn observe_system(
        &self,
        subtype: &str,
        session_id: &str,
        cwd: &str,
        permission_mode: Option<&str>,
    ) -> Option<ModeVerdict> {
        {
            let mut state = self.state();
            if subtype == "init" {
                if !session_id.is_empty() {
                    state.session_id = session_id.to_owned();
                }
                if !cwd.is_empty() {
                    state.cwd = cwd.to_owned();
                }
            }
        }
        if !matches!(subtype, "init" | "status") {
            return None;
        }
        self.check_mode(permission_mode?)
    }

    fn check_mode(&self, reported: &str) -> Option<ModeVerdict> {
        if self.inner.title_only {
            return None;
        }
        let expected = expected_native_mode(self.inner.mode)?;
        {
            let mut state = self.state();
            if state.reported_mode.as_deref() == Some(reported) {
                return None;
            }
            state.reported_mode = Some(reported.to_owned());
        }
        if reported == expected {
            return None;
        }
        let label = mode_label(self.inner.mode);
        let reported_text = clean(reported, 64);
        match (native_strength(reported), native_strength(expected)) {
            (Some(now), Some(launched)) if now >= launched => Some(ModeVerdict {
                message: format!(
                    "Claude reports permission mode `{reported_text}`, not the `{expected}` LUNA launched for {label}. \
                     It is at least as strict, so Claude may ask before more actions{}.",
                    if reported == "plan" {
                        "; LUNA does not support plan mode, so tools will not run until Claude leaves it"
                    } else {
                        ""
                    }
                ),
                fail_closed: false,
            }),
            _ => Some(ModeVerdict {
                message: format!(
                    "Claude reports permission mode `{reported_text}`, which is weaker than or unknown next to the \
                     `{expected}` LUNA launched for {label}. LUNA stopped the turn so nothing runs without the \
                     protection you chose."
                ),
                fail_closed: true,
            }),
        }
    }

    /// Dispatch one CLI `control_request`.
    pub(super) fn on_request(&self, id: &str, request: &Value) {
        if id.is_empty() || id.len() > 256 {
            tracing::debug!(target: "luna_harness::claude", "control_request without a usable request_id ignored");
            return;
        }
        let subtype = request
            .get("subtype")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let Some(token) = self.admit(id) else {
            tracing::debug!(target: "luna_harness::claude", %id, "replayed control_request ignored");
            return;
        };
        match subtype.as_str() {
            "can_use_tool" => self.can_use_tool(id, request, token),
            "elicitation" => self.elicitation(id, request, token),
            // LUNA declares no `supportedDialogKinds`, so the CLI should never
            // park one here. If it does, the protocol forbids answering an
            // undeclared kind (an error is discarded; "cancelled" is a real
            // settlement): leave it to the CLI's own dialog deadline.
            "request_user_dialog" => {
                tracing::debug!(target: "luna_harness::claude", %id, "undeclared request_user_dialog left to the CLI deadline");
            }
            other => {
                self.fail(id, &format!("Unsupported control request subtype: {other}"));
            }
        }
    }

    fn can_use_tool(&self, id: &str, request: &Value, token: CancellationToken) {
        let Some(tool) = text(request, "tool_name", 256) else {
            self.fail(id, "can_use_tool request without a tool_name");
            return;
        };
        let input = request.get("input").cloned().unwrap_or_else(|| json!({}));
        let tool_use_id = text(request, "tool_use_id", 256);
        let deny = |message: &str| deny_response(message, tool_use_id.as_deref(), false);
        if self.inner.title_only {
            self.answer(id, deny("Tools are disabled for title generation"));
            return;
        }
        if tool == "AskUserQuestion" {
            self.ask_user_question(id, request, input, tool_use_id.clone(), token);
            return;
        }
        if PLAN_TOOLS.contains(&tool.as_str()) {
            self.answer(
                id,
                deny(
                    "Plan mode is not available in LUNA; continue without entering or leaving it.",
                ),
            );
            return;
        }
        if request
            .get("requires_user_interaction")
            .and_then(Value::as_bool)
            == Some(true)
        {
            self.answer(
                id,
                deny(&format!(
                    "LUNA cannot display the interactive {tool} prompt, so it was declined."
                )),
            );
            return;
        }
        // Full is explicit native bypass. A stray ask is allowed once, never
        // widened into a rule, unless the user's own ask rule forced it.
        if self.inner.mode == PermissionMode::Full && request.get("matched_ask_rule").is_none() {
            self.answer(
                id,
                allow_response(&input, tool_use_id.as_deref(), None, None),
            );
            return;
        }
        let state = self.state();
        let context = CardContext {
            session_id: state.session_id.clone(),
            cwd: state.cwd.clone(),
        };
        drop(state);
        let card = approval_card(id, &tool, request, &input, &self.inner.policy, &context);
        let choices = card.choices;
        let fallback = deny("Denied in LUNA: the request was not approved.");
        self.park(id, vec![card.question], token, move |question, answers| {
            exact_label(question, &answers)
                .and_then(|label| choices.iter().find(|choice| choice.display.label == label))
                .map(|choice| choice.response.clone())
                .unwrap_or(fallback)
        });
    }

    fn ask_user_question(
        &self,
        id: &str,
        request: &Value,
        input: Value,
        tool_use_id: Option<String>,
        token: CancellationToken,
    ) {
        let mut questions = parse_questions(&input);
        if let Some(agent) = text(request, "agent_id", 128) {
            for question in &mut questions {
                question.header = format!("{} · sub-agent {agent}", question.header);
            }
        }
        if questions.is_empty() {
            self.answer(
                id,
                deny_response(
                    "LUNA received no questions to show.",
                    tool_use_id.as_deref(),
                    false,
                ),
            );
            return;
        }
        let asked = questions.clone();
        self.park_many(id, questions, token, move |answers| {
            match answers.filter(|answers| !answers.is_empty()) {
                Some(answers) => allow_response(
                    &updated_input_with_answers(&input, &asked, &answers),
                    tool_use_id.as_deref(),
                    None,
                    Some("user_temporary"),
                ),
                None => user_deny(
                    "The user dismissed the question in LUNA.",
                    tool_use_id.as_deref(),
                    false,
                ),
            }
        });
    }

    fn elicitation(&self, id: &str, request: &Value, token: CancellationToken) {
        if self.inner.title_only {
            self.answer(id, json!({"action": "cancel"}));
            return;
        }
        let session = {
            let state = self.state();
            if state.session_id.is_empty() {
                "claude-session".to_owned()
            } else {
                state.session_id.clone()
            }
        };
        // Same MCP 2025-11-25 form/URL validation and wizard questions the
        // Codex adapter uses; only the Claude key spelling differs.
        let params = json!({
            "serverName": request.get("mcp_server_name"),
            "threadId": session,
            "message": request.get("message"),
            "mode": request.get("mode").and_then(Value::as_str).unwrap_or("form"),
            "url": request.get("url"),
            "elicitationId": request.get("elicitation_id"),
            "requestedSchema": request.get("requested_schema"),
        });
        let questions = match crate::codex::elicitation::questions(
            &params,
            &Value::String(id.into()),
            id,
        ) {
            Ok(questions) => questions,
            Err(reason) => {
                tracing::debug!(target: "luna_harness::claude", %id, reason, "elicitation declined");
                self.answer(id, json!({"action": "decline"}));
                return;
            }
        };
        let asked = questions.clone();
        self.park_many(id, questions, token, move |answers| {
            match answers.filter(|answers| !answers.is_empty()) {
                Some(answers) => luna_proto::mcp_response(&asked, &answers)
                    .unwrap_or_else(|_| json!({"action": "decline"})),
                None => json!({"action": "cancel"}),
            }
        });
    }

    /// Ask one approval question and answer from its exact label.
    fn park(
        &self,
        id: &str,
        questions: Vec<UserInputQuestion>,
        token: CancellationToken,
        finish: impl FnOnce(&UserInputQuestion, Option<Vec<UserInputAnswer>>) -> Value + Send + 'static,
    ) {
        let question = questions[0].clone();
        self.park_many(id, questions, token, move |answers| {
            finish(&question, answers)
        });
    }

    /// Hand questions to the engine's input bridge (the sole owner of the
    /// `InputRequested`/`InputResolved` lifecycle) and answer once. A
    /// withdrawn request drops the receiver and writes nothing.
    fn park_many(
        &self,
        id: &str,
        questions: Vec<UserInputQuestion>,
        token: CancellationToken,
        finish: impl FnOnce(Option<Vec<UserInputAnswer>>) -> Value + Send + 'static,
    ) {
        let receiver = (self.inner.request_input)(questions);
        let controls = self.clone();
        let id = id.to_owned();
        tokio::spawn(async move {
            let answers = tokio::select! {
                biased;
                _ = token.cancelled() => return,
                answers = receiver => answers.ok(),
            };
            let response = finish(answers);
            controls.answer(&id, response);
        });
    }
}

fn expected_native_mode(mode: PermissionMode) -> Option<&'static str> {
    let args = super::profile::mode_args(mode);
    args.iter()
        .position(|arg| *arg == "--permission-mode")
        .and_then(|index| args.get(index + 1))
        .copied()
}

/// Higher is stricter. `plan` and `dontAsk` run nothing unapproved.
fn native_strength(mode: &str) -> Option<u8> {
    match mode {
        "bypassPermissions" => Some(0),
        "auto" => Some(1),
        "acceptEdits" => Some(2),
        "default" => Some(3),
        "dontAsk" | "plan" => Some(4),
        _ => None,
    }
}

fn mode_label(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::Ask => "Ask",
        PermissionMode::Auto => "Auto",
        PermissionMode::Full => "Full access",
    }
}

/// An automatic host denial (no user decision, so no classification).
fn deny_response(message: &str, tool_use_id: Option<&str>, interrupt: bool) -> Value {
    let mut response = json!({"behavior": "deny", "message": message});
    if interrupt {
        response["interrupt"] = Value::Bool(true);
    }
    if let Some(tool_use_id) = tool_use_id {
        response["toolUseID"] = tool_use_id.into();
    }
    response
}

/// A denial the user chose in LUNA.
fn user_deny(message: &str, tool_use_id: Option<&str>, interrupt: bool) -> Value {
    let mut response = deny_response(message, tool_use_id, interrupt);
    response["decisionClassification"] = "user_reject".into();
    response
}

fn allow_response(
    input: &Value,
    tool_use_id: Option<&str>,
    permissions: Option<&[Value]>,
    classification: Option<&str>,
) -> Value {
    let mut response = json!({"behavior": "allow", "updatedInput": input});
    if let Some(permissions) = permissions {
        response["updatedPermissions"] = Value::Array(permissions.to_vec());
    }
    if let Some(classification) = classification {
        response["decisionClassification"] = classification.into();
    }
    if let Some(tool_use_id) = tool_use_id {
        response["toolUseID"] = tool_use_id.into();
    }
    response
}

fn exact_label<'a>(
    question: &UserInputQuestion,
    answers: &'a Option<Vec<UserInputAnswer>>,
) -> Option<&'a str> {
    let [answer] = answers.as_deref()? else {
        return None;
    };
    (answer.question_id == question.id && answer.labels.len() == 1)
        .then(|| answer.labels[0].as_str())
}

struct Choice {
    display: ApprovalChoice,
    response: Value,
}

struct Card {
    question: UserInputQuestion,
    choices: Vec<Choice>,
}

struct CardContext {
    session_id: String,
    cwd: String,
}

/// Provider-authored text, ANSI/control stripped and bounded.
fn clean(raw: &str, max: usize) -> String {
    let mut out = String::with_capacity(raw.len().min(max));
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            match chars.peek() {
                Some('[') => {
                    chars.next();
                    for next in chars.by_ref() {
                        if ('@'..='~').contains(&next) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    while let Some(next) = chars.next() {
                        if next == '\u{7}' {
                            break;
                        }
                        if next == '\u{1b}' {
                            if chars.peek() == Some(&'\\') {
                                chars.next();
                            }
                            break;
                        }
                    }
                }
                _ => {
                    chars.next();
                }
            }
            continue;
        }
        if c.is_control() && c != '\n' && c != '\t' {
            continue;
        }
        out.push(c);
    }
    let out = out.trim();
    if out.len() <= max {
        return out.to_owned();
    }
    let mut end = max;
    while !out.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… (truncated)", &out[..end])
}

fn text(value: &Value, key: &str, max: usize) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(|raw| clean(raw, max))
        .filter(|text| !text.is_empty())
}

fn input_summary(tool: &str, input: &Value) -> Option<String> {
    let field = |key: &str| input.get(key).and_then(Value::as_str);
    let primary = match tool {
        "Bash" | "PowerShell" => field("command").map(str::to_owned),
        "Read" | "Write" | "Edit" | "MultiEdit" => field("file_path").map(str::to_owned),
        "NotebookEdit" => field("notebook_path").map(str::to_owned),
        "WebFetch" => field("url").map(str::to_owned),
        "WebSearch" => field("query").map(str::to_owned),
        "Glob" | "Grep" => field("pattern").map(|pattern| match field("path") {
            Some(path) => format!("{pattern} in {path}"),
            None => pattern.to_owned(),
        }),
        _ => None,
    };
    primary
        .or_else(|| match input {
            Value::Null => None,
            Value::Object(map) if map.is_empty() => None,
            other => Some(other.to_string()),
        })
        .map(|summary| clean(&summary, MAX_DISPLAY))
        .filter(|summary| !summary.is_empty())
}

fn action_kind(tool: &str, blocked_path: bool) -> ApprovalActionKind {
    match tool {
        "Bash" | "PowerShell" => ApprovalActionKind::Command,
        "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => ApprovalActionKind::FileChange,
        "WebFetch" | "WebSearch" => ApprovalActionKind::Network,
        _ if blocked_path => ApprovalActionKind::ExternalPath,
        _ => ApprovalActionKind::Tool,
    }
}

/// Session-scoped grants derived from native `permission_suggestions`. Only
/// allow-rule and directory additions qualify; each is re-scoped to the
/// session (narrower than a settings-file rule, and no file is written).
fn session_grants(suggestions: Option<&Value>) -> Vec<Value> {
    let Some(items) = suggestions.and_then(Value::as_array) else {
        return Vec::new();
    };
    items
        .iter()
        .take(8)
        .filter_map(|suggestion| {
            let object = suggestion.as_object()?;
            match object.get("type")?.as_str()? {
                "addRules" => {
                    if object.get("behavior")?.as_str()? != "allow" {
                        return None;
                    }
                    let rules = object.get("rules")?.as_array()?;
                    if rules.is_empty() || rules.len() > 16 {
                        return None;
                    }
                    let mut exact = Vec::new();
                    for rule in rules {
                        let rule = rule.as_object()?;
                        if rule
                            .keys()
                            .any(|key| key != "toolName" && key != "ruleContent")
                        {
                            return None;
                        }
                        let tool = rule
                            .get("toolName")?
                            .as_str()
                            .filter(|tool| !tool.is_empty() && tool.len() <= 256)?;
                        exact.push(match rule.get("ruleContent") {
                            None => json!({"toolName": tool}),
                            Some(content) => {
                                let content =
                                    content.as_str().filter(|content| content.len() <= 1024)?;
                                json!({"toolName": tool, "ruleContent": content})
                            }
                        });
                    }
                    Some(json!({
                        "type": "addRules",
                        "rules": exact,
                        "behavior": "allow",
                        "destination": "session",
                    }))
                }
                "addDirectories" => {
                    let directories = object.get("directories")?.as_array()?;
                    let valid = !directories.is_empty()
                        && directories.len() <= 8
                        && directories.iter().all(|directory| {
                            directory
                                .as_str()
                                .is_some_and(|d| !d.is_empty() && d.len() <= 1024)
                        });
                    valid.then(|| {
                        json!({
                            "type": "addDirectories",
                            "directories": directories,
                            "destination": "session",
                        })
                    })
                }
                _ => None,
            }
        })
        .collect()
}

fn rule_text(rule: &Value) -> String {
    let tool = rule["toolName"].as_str().unwrap_or("");
    match rule.get("ruleContent").and_then(Value::as_str) {
        Some(content) => format!("{tool}({content})"),
        None => tool.to_owned(),
    }
}

fn approval_card(
    request_id: &str,
    tool: &str,
    request: &Value,
    input: &Value,
    policy: &PermissionPolicy,
    context: &CardContext,
) -> Card {
    let tool_use_id = text(request, "tool_use_id", 256);
    let display = text(request, "display_name", 256).unwrap_or_else(|| tool.to_owned());
    let agent = text(request, "agent_id", 128);
    let blocked_path = text(request, "blocked_path", MAX_DISPLAY);
    let mcp_server = request
        .get("mcp_server")
        .and_then(|server| server.get("name").or(Some(server)))
        .and_then(Value::as_str)
        .map(|name| clean(name, 128))
        .filter(|name| !name.is_empty());
    let suppress_rules = request
        .get("suppress_always_allow_rule")
        .and_then(Value::as_bool)
        == Some(true);

    let mut reasons = Vec::new();
    if let Some(description) = text(request, "description", MAX_DISPLAY) {
        reasons.push(description);
    }
    let reason_type = text(request, "decision_reason_type", 64);
    match (text(request, "decision_reason", MAX_DISPLAY), &reason_type) {
        (Some(reason), Some(kind)) => reasons.push(format!("{reason} ({kind})")),
        (Some(reason), None) => reasons.push(reason),
        (None, Some(kind)) => reasons.push(format!("Escalated by {kind}")),
        (None, None) => {}
    }
    if request
        .get("classifier_approvable")
        .and_then(Value::as_bool)
        == Some(false)
    {
        reasons.push("A safety check requires manual approval.".into());
    }
    if let Some(rule) = request.get("matched_ask_rule") {
        let source = text(rule, "source", 64).unwrap_or_else(|| "settings".into());
        let tool = text(rule, "tool_name", 256).unwrap_or_default();
        reasons.push(match text(rule, "rule_content", 1024) {
            Some(content) => {
                format!("Your ask rule ({source}) requires a prompt: {tool}({content})")
            }
            None => format!("Your ask rule ({source}) requires a prompt: {tool}"),
        });
    }

    let grants = if suppress_rules {
        Vec::new()
    } else {
        session_grants(request.get("permission_suggestions"))
    };
    let rules: Vec<String> = grants
        .iter()
        .flat_map(|grant| {
            grant["rules"]
                .as_array()
                .map(|rules| rules.iter().map(rule_text).collect::<Vec<_>>())
                .unwrap_or_default()
        })
        .collect();
    let directories: Vec<String> = grants
        .iter()
        .flat_map(|grant| {
            grant["directories"]
                .as_array()
                .map(|dirs| {
                    dirs.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        })
        .collect();

    let tool_use = tool_use_id.as_deref();
    let mut choices = vec![
        Choice {
            display: ApprovalChoice {
                label: "Deny".into(),
                scope: "none".into(),
                description: "Reject this tool call; Claude may continue.".into(),
            },
            response: user_deny("Denied in LUNA by the user.", tool_use, false),
        },
        Choice {
            display: ApprovalChoice {
                label: "Allow once".into(),
                scope: "request".into(),
                description: "Allow only this tool call.".into(),
            },
            response: allow_response(input, tool_use, None, Some("user_temporary")),
        },
    ];
    if !grants.is_empty() {
        let mut granted = rules.clone();
        granted.extend(directories.iter().map(|dir| format!("directory {dir}")));
        choices.push(Choice {
            display: ApprovalChoice {
                label: "Allow for session".into(),
                scope: "session".into(),
                description: format!(
                    "Allow this call and remember {} for this Claude session. LUNA does not write Claude settings files.",
                    granted.join(", ")
                ),
            },
            response: allow_response(input, tool_use, Some(&grants), Some("user_permanent")),
        });
    }
    choices.push(Choice {
        display: ApprovalChoice {
            label: "Cancel turn".into(),
            scope: "none".into(),
            description: "Reject this tool call and stop Claude's turn.".into(),
        },
        response: user_deny("Denied in LUNA; the user stopped the turn.", tool_use, true),
    });

    let kind = action_kind(tool, blocked_path.is_some());
    let mut header = match kind {
        ApprovalActionKind::Command => "Approve command",
        ApprovalActionKind::FileChange => "Approve file change",
        ApprovalActionKind::Network => "Approve network access",
        ApprovalActionKind::ExternalPath => "Approve path access",
        _ => "Approve tool",
    }
    .to_owned();
    if let Some(agent) = &agent {
        header.push_str(&format!(" · sub-agent {agent}"));
    }
    let summary = text(request, "title", 512).unwrap_or_else(|| match &agent {
        Some(agent) => format!("Claude sub-agent {agent} requests the {display} tool"),
        None => format!("Claude requests the {display} tool"),
    });
    let tool_context = mcp_server.map(|server| ApprovalToolContext {
        server,
        connector: None,
        tool: Some(display.clone()),
        description: None,
        arguments: input
            .as_object()
            .map(|arguments| {
                arguments
                    .iter()
                    .take(16)
                    .map(|(name, value)| {
                        let rendered = match value {
                            Value::String(text) => text.clone(),
                            other => other.to_string(),
                        };
                        (clean(name, 128), clean(&rendered, 512))
                    })
                    .collect()
            })
            .unwrap_or_default(),
    });
    let command = if tool_context.is_some() {
        None
    } else {
        input_summary(tool, input)
    };
    let details = ApprovalDetails {
        provider_thread_id: (!context.session_id.is_empty()).then(|| context.session_id.clone()),
        item_id: tool_use_id.clone(),
        approval_id: Some(request_id.to_owned()),
        // An MCP card already names the tool; use Codex's kind for parity.
        kind: Some(if tool_context.is_some() {
            "mcp_tool_call".to_owned()
        } else {
            display.clone()
        }),
        cwd: (!context.cwd.is_empty()).then(|| context.cwd.clone()),
        command: command.clone(),
        reason: (!reasons.is_empty()).then(|| reasons.join(" · ")),
        grant_root: (!directories.is_empty()).then(|| directories.join(", ")),
        permissions: blocked_path
            .as_ref()
            .map(|path| json!({"fileSystem": {"blockedPath": path}})),
        proposed_exec_policy_amendment: (!rules.is_empty()).then(|| json!(rules)),
        choices: choices
            .iter()
            .map(|choice| choice.display.clone())
            .collect(),
        subagent: agent,
        tool: tool_context,
        ..Default::default()
    };
    let target = if policy.machine_name.is_empty() {
        "this machine"
    } else {
        policy.machine_name.as_str()
    };
    let question = UserInputQuestion {
        id: uuid::Uuid::new_v4().to_string(),
        header,
        question: format!(
            "{summary}. {}Target: {target}.",
            command
                .filter(|command| !summary.contains(command.as_str()))
                .map(|command| format!("{command} "))
                .unwrap_or_default()
        ),
        options: choices
            .iter()
            .map(|choice| choice.display.label.clone())
            .collect(),
        multi_select: false,
        mcp: None,
        input: None,
        approval: Some(ApprovalRequest {
            provider_request_id: request_id.to_owned(),
            provider: HarnessId::ClaudeCode,
            turn_id: None,
            machine_name: policy.machine_name.clone(),
            action_kind: kind,
            summary,
            requested_scope: policy.scope,
            expires_at_ms: APPROVAL_NO_EXPIRY,
            supports_task_allowance: !grants.is_empty(),
            details: Some(details),
        }),
    };
    Card { question, choices }
}

/// Parse Claude's `AskUserQuestion` tool input into [`UserInputQuestion`]s,
/// tolerant of `header`/`title`, `question`/`prompt` and string or object
/// options. Option descriptions ride `input.option_descriptions`; the native
/// tool always lets the user type an "Other" answer.
fn parse_questions(input: &Value) -> Vec<UserInputQuestion> {
    let raw = input.get("questions").and_then(Value::as_array);
    raw.map(|a| a.as_slice())
        .unwrap_or_default()
        .iter()
        .map(|q| {
            let field =
                |keys: [&str; 2]| keys.iter().find_map(|k| q.get(*k).and_then(Value::as_str));
            let options = q
                .get("options")
                .and_then(Value::as_array)
                .map(|a| a.as_slice())
                .unwrap_or_default();
            let labels = options
                .iter()
                .map(|op| match op {
                    Value::String(s) => s.clone(),
                    other => other
                        .get("label")
                        .or_else(|| other.get("value"))
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .into(),
                })
                .collect();
            let option_descriptions = options
                .iter()
                .map(|op| {
                    op.get("description")
                        .and_then(Value::as_str)
                        .map(|description| clean(description, 1024))
                        .unwrap_or_default()
                })
                .collect();
            UserInputQuestion {
                id: uuid::Uuid::new_v4().to_string(),
                header: field(["header", "title"]).unwrap_or("Question").into(),
                question: field(["question", "prompt"]).unwrap_or("").into(),
                multi_select: ["multiSelect", "multi_select"]
                    .iter()
                    .find_map(|k| q.get(*k).and_then(Value::as_bool))
                    .unwrap_or(false),
                options: labels,
                mcp: None,
                approval: None,
                input: Some(UserInputDetails {
                    option_descriptions,
                    allow_other: true,
                    secret: false,
                }),
            }
        })
        .collect()
}

/// Merge the user's answers back into the tool input, keyed by question text
/// (single-select ⇒ a string, multi-select ⇒ an array), as the tool expects.
fn updated_input_with_answers(
    input: &Value,
    questions: &[UserInputQuestion],
    answers: &[UserInputAnswer],
) -> Value {
    let mut updated = match input {
        Value::Object(map) => map.clone(),
        _ => Map::new(),
    };
    let mut by_question = Map::new();
    for q in questions {
        let labels: Vec<String> = answers
            .iter()
            .find(|a| a.question_id == q.id)
            .map(|a| a.labels.clone())
            .unwrap_or_default();
        let value = if q.multi_select {
            Value::Array(labels.into_iter().map(Value::String).collect())
        } else {
            Value::String(labels.into_iter().next().unwrap_or_default())
        };
        by_question.insert(q.question.clone(), value);
    }
    updated.insert("answers".into(), Value::Object(by_question));
    Value::Object(updated)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_questions_tolerantly() {
        let input = json!({
            "questions": [
                {
                    "header": "Choice",
                    "question": "Pick one",
                    "options": ["A", {"label": "B", "description": "second"}],
                    "multiSelect": false
                },
                { "title": "Alt", "prompt": "Pick many", "multi_select": true }
            ]
        });
        let qs = parse_questions(&input);
        assert_eq!(qs.len(), 2);
        assert_eq!(qs[0].header, "Choice");
        assert_eq!(qs[0].options, vec!["A".to_string(), "B".to_string()]);
        assert_eq!(qs[0].option_description(1), Some("second"));
        assert!(!qs[0].multi_select);
        assert_eq!(qs[1].header, "Alt");
        assert_eq!(qs[1].question, "Pick many");
        assert!(qs[1].multi_select);
    }

    #[test]
    fn answers_key_by_question_text() {
        let input =
            json!({"questions": [{"header": "H", "question": "Pick one", "options": ["A", "B"]}]});
        let qs = parse_questions(&input);
        let answers = vec![UserInputAnswer {
            question_id: qs[0].id.clone(),
            labels: vec!["B".into()],
        }];
        let updated = updated_input_with_answers(&input, &qs, &answers);
        assert_eq!(updated["answers"]["Pick one"], json!("B"));
        // Original input is preserved alongside the answers.
        assert!(updated["questions"].is_array());
    }
}
