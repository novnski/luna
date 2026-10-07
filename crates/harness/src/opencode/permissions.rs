//! Permission rules, the approval card and permission replies (OC-04).
//!
//! OpenCode 2 rules are `[{action, resource, effect}]`, evaluated LAST match
//! wins over the agent's rules followed by the session's rules
//! (`oc2:core/src/permission.ts`), so LUNA's session rules decide. A tool is
//! removed from the model's snapshot when the last rule naming it is a
//! wildcard-resource deny (`oc2:core/src/tool.ts` `whollyDisabled`).
//!
//! Mode compilation. LUNA compiles its mode against the effective agent's
//! own ruleset (`GET /api/agent`), so the result is `min(agent, mode)` per
//! action: the user's configured denies and an agent's own restrictions
//! (the read-only `explore` agent) stay final, and LUNA never widens them.
//! - Ask: every action asks, except reads and discovery (`read`, `glob`,
//!   `grep`, `question`, `skill`, `subagent`), which keep the agent's own
//!   verdict, and `external_directory`, where only explicitly trusted paths
//!   (OpenCode's own data directories, the user's path rules) skip the ask.
//! - Full: allow everything except the agent's denies (its asks become
//!   allows); a stray ask (a plugin hook) is answered `once` automatically.
//!
//! Without the agent list the static rulesets of [`opencode_permission_rules`]
//! apply. Either way the session record is read back before the first prompt
//! ([`verify_session_rules`]); a session without LUNA's rules never runs.
//!
//! Containment (PD-14): every ruleset ends with `opencode_session_move: deny`
//! so the chat's directory stays LUNA-owned. Child (subagent) sessions are
//! capped on creation with the same mode compiled against the CHILD's agent
//! plus `luna_*: deny` — LUNA tools are root-only for every provider.
//!
//! Replies are `POST /api/session/{s}/permission/{id}/reply` with
//! `{decision: "once" | "reject", message?}`. LUNA never sends `always`
//! (PD-8): OpenCode 2 writes it as a project-wide saved rule. `reject` with a
//! message is OpenCode's "decline with feedback" (the model continues);
//! `reject` without one stops the execution.

use std::collections::HashSet;
use std::sync::Mutex;

use super::*;
use crate::CancellationToken;

/// The rule pattern naming every LUNA MCP tool (`luna_<tool>`, OpenCode's
/// `{server}_{tool}` action name). Subagent agents must deny it; the inline
/// config that attaches LUNA tools reuses this pattern.
pub(super) const LUNA_TOOL_ACTION_PATTERN: &str = "luna_*";

/// OpenCode's session-move tool (`opencode` namespace, `session_move`).
pub(super) const SESSION_MOVE_ACTION: &str = "opencode_session_move";

/// Actions LUNA's Ask mode leaves to the agent's own verdict: reads,
/// discovery, questions (answered in LUNA's panel), skill loading and
/// subagent spawns (children are capped at the parent's mode on creation).
const ASK_MODE_AGENT_ACTIONS: &[&str] = &["read", "glob", "grep", "question", "skill", "subagent"];

/// The approval choices, in card order (labels are the wire contract with
/// the engine: the stored options must equal these labels).
pub(super) const CHOICE_DENY: &str = "Deny";
pub(super) const CHOICE_ALLOW_ONCE: &str = "Allow once";
pub(super) const CHOICE_CANCEL_TURN: &str = "Cancel turn";

/// The feedback a Deny sends, so the model continues without the action.
pub(super) const DENY_FEEDBACK: &str = "The user denied this request in LUNA. Continue without it, or ask the user how to proceed.";
/// The feedback when LUNA closes an approval without an answer.
const UNANSWERED_FEEDBACK: &str =
    "LUNA closed this approval without an answer, so it was not approved. Continue without it.";

/// Answered native request ids remembered against resync re-delivery.
const MAX_SETTLED_IDS: usize = 1024;

/// A rule effect, ordered by permissiveness.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum RuleEffect {
    Deny,
    Ask,
    Allow,
}

impl RuleEffect {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "deny" => Some(Self::Deny),
            "ask" => Some(Self::Ask),
            "allow" => Some(Self::Allow),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Deny => "deny",
            Self::Ask => "ask",
            Self::Allow => "allow",
        }
    }
}

/// One v2 rule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Rule {
    pub(super) action: String,
    pub(super) resource: String,
    pub(super) effect: RuleEffect,
}

impl Rule {
    fn new(action: &str, resource: &str, effect: RuleEffect) -> Self {
        Self {
            action: action.to_owned(),
            resource: resource.to_owned(),
            effect,
        }
    }

    fn from_json(value: &Value) -> Option<Self> {
        Some(Self {
            action: value.get("action")?.as_str()?.to_owned(),
            resource: value.get("resource")?.as_str()?.to_owned(),
            effect: RuleEffect::parse(value.get("effect")?.as_str()?)?,
        })
    }

    fn to_json(&self) -> Value {
        json!({ "action": self.action, "resource": self.resource, "effect": self.effect.as_str() })
    }
}

fn rules_json(rules: &[Rule]) -> Value {
    Value::Array(rules.iter().map(Rule::to_json).collect())
}

/// OpenCode's `Wildcard.match` (`oc2:core/src/util/wildcard.ts`): `*` is any
/// run, `?` one character, backslashes compare as slashes, and a pattern
/// ending in `" *"` also matches its bare prefix (`git *` matches `git`).
pub(super) fn wildcard_match(input: &str, pattern: &str) -> bool {
    let input: Vec<char> = input.replace('\\', "/").chars().collect();
    let pattern = pattern.replace('\\', "/");
    if let Some(prefix) = pattern.strip_suffix(" *")
        && glob_match(&input, &prefix.chars().collect::<Vec<_>>())
    {
        return true;
    }
    glob_match(&input, &pattern.chars().collect::<Vec<_>>())
}

/// Iterative `*`/`?` matching with single-star backtracking.
fn glob_match(input: &[char], pattern: &[char]) -> bool {
    let (mut i, mut p) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while i < input.len() {
        match pattern.get(p) {
            Some('*') => {
                star = Some((p, i));
                p += 1;
            }
            Some(&c) if c == '?' || c == input[i] => {
                i += 1;
                p += 1;
            }
            _ => match star {
                Some((star_p, star_i)) => {
                    p = star_p + 1;
                    i = star_i + 1;
                    star = Some((star_p, star_i + 1));
                }
                None => return false,
            },
        }
    }
    pattern[p..].iter().all(|c| *c == '*')
}

/// Which session a ruleset is compiled for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RuleScope {
    /// The run's own session.
    Root,
    /// A native subagent session the run's session (or a descendant) spawned.
    Subagent,
}

/// The rules every LUNA ruleset ends with (last, so they win): OpenCode-only
/// state changes are contained, and subagents never reach LUNA tools.
fn contained_rules(scope: RuleScope) -> Vec<Rule> {
    let mut rules = Vec::new();
    if scope == RuleScope::Subagent {
        rules.push(Rule::new(LUNA_TOOL_ACTION_PATTERN, "*", RuleEffect::Deny));
    }
    rules.push(Rule::new(SESSION_MOVE_ACTION, "*", RuleEffect::Deny));
    rules
}

/// The rules a native subagent agent must carry wherever LUNA configures it
/// (session rules here; the luna-tools inline config reuses them): LUNA
/// tools denied, session moves denied.
#[allow(dead_code)] // the luna-tools inline config (OC-13) consumes it
pub(super) fn opencode_subagent_contained_rules() -> Value {
    rules_json(&contained_rules(RuleScope::Subagent))
}

/// The static LUNA mode ruleset, used when the agent's own rules are
/// unknown (v2 action names: `shell`, `edit`, `read`, `glob`, `grep`,
/// `webfetch`, `websearch`, `external_directory`, `question`, `skill`,
/// `subagent`, MCP tool names).
#[cfg(test)]
pub(super) fn opencode_permission_rules(mode: luna_proto::PermissionMode) -> Value {
    rules_json(&compile_permission_rules(mode, None, RuleScope::Root))
}

/// Compile a LUNA mode against an agent's ruleset (see the module docs).
pub(super) fn compile_permission_rules(
    mode: luna_proto::PermissionMode,
    agent: Option<&[Rule]>,
    scope: RuleScope,
) -> Vec<Rule> {
    use RuleEffect::{Allow, Ask, Deny};
    let mut out = match (mode, agent) {
        (luna_proto::PermissionMode::Full, None) => vec![Rule::new("*", "*", Allow)],
        (luna_proto::PermissionMode::Full, Some(rules)) => {
            // Everything the agent allows or asks is allowed; its denies stay.
            let mut out = vec![Rule::new("*", "*", Allow)];
            out.extend(rules.iter().map(|rule| Rule {
                effect: if rule.effect == Deny { Deny } else { Allow },
                ..rule.clone()
            }));
            out
        }
        // Auto is resolved to Ask by the engine for OpenCode.
        (_, None) => vec![
            Rule::new("*", "*", Ask),
            Rule::new("read", "*", Allow),
            Rule::new("glob", "*", Allow),
            Rule::new("grep", "*", Allow),
            // The native question tool asks through LUNA's panel; gating it
            // behind an approval first would double-ask.
            Rule::new("question", "*", Allow),
            Rule::new("skill", "*", Allow),
            Rule::new("subagent", "*", Allow),
            Rule::new("shell", "*", Ask),
            Rule::new("edit", "*", Ask),
            Rule::new("external_directory", "*", Ask),
            Rule::new("webfetch", "*", Ask),
            Rule::new("websearch", "*", Ask),
            // OpenCode's own defaults: sensitive env files ask, example
            // templates are readable.
            Rule::new("read", "*.env", Ask),
            Rule::new("read", "*.env.*", Ask),
            Rule::new("read", "*.env.example", Allow),
        ],
        (_, Some(rules)) => {
            // min(agent, ask) for every action...
            let mut out = vec![Rule::new("*", "*", Ask)];
            out.extend(rules.iter().map(|rule| Rule {
                effect: rule.effect.min(Ask),
                ..rule.clone()
            }));
            // ...then the agent's exact verdict for the actions Ask leaves
            // to it: its rules replayed for that action alone, after
            // OpenCode's no-match default.
            for action in ASK_MODE_AGENT_ACTIONS {
                out.push(Rule::new(action, "*", Ask));
                out.extend(
                    rules
                        .iter()
                        .filter(|rule| wildcard_match(action, &rule.action))
                        .map(|rule| Rule::new(action, &rule.resource, rule.effect)),
                );
            }
            // External paths: explicitly trusted paths (OpenCode's data
            // directories, the user's path rules) keep the agent's verdict;
            // a wildcard allow does not cover every directory on disk.
            out.push(Rule::new("external_directory", "*", Ask));
            out.extend(
                rules
                    .iter()
                    .filter(|rule| wildcard_match("external_directory", &rule.action))
                    .map(|rule| {
                        let effect = if rule.resource == "*" {
                            rule.effect.min(Ask)
                        } else {
                            rule.effect
                        };
                        Rule::new("external_directory", &rule.resource, effect)
                    }),
            );
            out
        }
    };
    out.extend(contained_rules(scope));
    out
}

/// Every agent's ruleset at the run's location (`GET /api/agent`). The
/// first entry is OpenCode's selected default agent.
#[derive(Clone, Debug, Default)]
pub(super) struct AgentRules {
    default: Option<String>,
    by_id: HashMap<String, Vec<Rule>>,
}

impl AgentRules {
    pub(super) fn from_list(list: &Value) -> Option<Self> {
        let agents = list
            .get("data")
            .unwrap_or(list)
            .as_array()
            .filter(|agents| !agents.is_empty())?;
        let mut out = Self::default();
        for agent in agents {
            let Some(id) = agent.get("id").and_then(Value::as_str) else {
                continue;
            };
            // An agent with any rule LUNA can't read is left out, so the
            // static ruleset applies to it instead of a partial reading.
            let Some(rules) = agent
                .get("permissions")
                .and_then(Value::as_array)
                .and_then(|rules| rules.iter().map(Rule::from_json).collect::<Option<Vec<_>>>())
            else {
                continue;
            };
            if out.default.is_none() {
                out.default = Some(id.to_owned());
            }
            out.by_id.insert(id.to_owned(), rules);
        }
        (!out.by_id.is_empty()).then_some(out)
    }

    fn get(&self, agent: Option<&str>) -> Option<&[Rule]> {
        let id = agent.or(self.default.as_deref())?;
        self.by_id.get(id).map(Vec::as_slice)
    }
}

/// What a run compiles every session's rules from.
#[derive(Clone, Debug)]
pub(super) struct SessionRuleSet {
    mode: luna_proto::PermissionMode,
    agents: Option<AgentRules>,
}

impl SessionRuleSet {
    /// Read the location's agents. Failure is not fatal: the static ruleset
    /// applies (and is still read back), with a warning.
    pub(super) async fn load(
        server: &Server,
        dir: Option<&str>,
        mode: luna_proto::PermissionMode,
    ) -> Self {
        let agents = match server.get_json("/api/agent", dir).await {
            Ok(list) => AgentRules::from_list(&list),
            Err(error) => {
                tracing::warn!(target: "luna_harness::opencode", %error, "agent list unavailable");
                None
            }
        };
        if agents.is_none() {
            tracing::warn!(
                target: "luna_harness::opencode",
                "OpenCode agent rules unknown; LUNA applies its static permission ruleset, so \
                 the user's own OpenCode denies may not stay final"
            );
        }
        Self { mode, agents }
    }

    /// The ruleset for a session running `agent` (`None` = OpenCode's
    /// default agent).
    pub(super) fn rules_for(&self, agent: Option<&str>, scope: RuleScope) -> Value {
        let agent_rules = self.agents.as_ref().and_then(|agents| agents.get(agent));
        rules_json(&compile_permission_rules(self.mode, agent_rules, scope))
    }
}

/// Read the session record back and require exactly LUNA's rules on it
/// (OCF-153): a server that dropped or changed them must not run the turn.
pub(super) async fn verify_session_rules(
    server: &Server,
    session_id: &str,
    dir: Option<&str>,
    expected: &Value,
) -> Result<(), HarnessError> {
    let info = server.session_info(session_id, dir).await?;
    let problem = match info.get("permissions") {
        Some(applied) if applied == expected => return Ok(()),
        Some(_) => "the session carries different rules",
        None => "the session carries no rules",
    };
    Err(HarnessError::Protocol(format!(
        "OpenCode did not apply LUNA's permission rules to session {session_id} ({problem}). \
         The turn was not started, so nothing runs without the protection you chose."
    )))
}

/// 2.x persists `always` grants into project-wide rules that outlive the
/// task, so there is no task-scoped allowance to offer (PD-8).
pub(super) fn opencode_supports_task_allowance() -> bool {
    false
}

/// The card's choices, Claude's shape: Deny continues, Cancel turn stops.
pub(super) fn opencode_approval_choices() -> Vec<luna_proto::ApprovalChoice> {
    vec![
        luna_proto::ApprovalChoice {
            label: CHOICE_DENY.into(),
            scope: "none".into(),
            description: "Reject this request; OpenCode continues without it.".into(),
        },
        luna_proto::ApprovalChoice {
            label: CHOICE_ALLOW_ONCE.into(),
            scope: "request".into(),
            description: "Allow only this request.".into(),
        },
        luna_proto::ApprovalChoice {
            label: CHOICE_CANCEL_TURN.into(),
            scope: "none".into(),
            description: "Reject this request and stop OpenCode's turn.".into(),
        },
    ]
}

pub(super) fn opencode_approval_options() -> Vec<String> {
    opencode_approval_choices()
        .into_iter()
        .map(|choice| choice.label)
        .collect()
}

/// The v2 reply body for a LUNA answer. Only an exact "Allow once" allows;
/// "Cancel turn" stops the execution; anything else — Deny, an empty
/// (fail-closed) answer, a forged or stale "Allow for task" — rejects with
/// feedback so the model continues.
pub(super) fn opencode_permission_reply_body(decision: Option<&str>) -> Value {
    match decision {
        Some(CHOICE_ALLOW_ONCE) => json!({ "decision": "once" }),
        Some(CHOICE_CANCEL_TURN) | Some("Cancel") => json!({ "decision": "reject" }),
        Some(_) => json!({ "decision": "reject", "message": DENY_FEEDBACK }),
        None => json!({ "decision": "reject", "message": UNANSWERED_FEEDBACK }),
    }
}

/// A session this run owns (its root, or a descendant subagent session).
#[derive(Clone, Debug, Default)]
struct OwnedSession {
    /// How a request from it is attributed ("general: Explore the repo").
    label: Option<String>,
}

/// Per-run bookkeeping for requests that wait on the user (permission asks
/// and forms) and for the rules LUNA applied to each owned session.
///
/// Every pending prompt hangs off `token`, a child of the run's interrupt
/// token that is also cancelled when the run loop ends: Stop or the end of
/// the turn drops every receiver (the engine retires the visible card) and
/// nothing is posted afterwards (OCF-41).
pub(super) struct UserRequests {
    pub(super) root: String,
    pub(super) token: CancellationToken,
    rules: SessionRuleSet,
    owned: HashMap<String, OwnedSession>,
    /// The ruleset LUNA applied per owned session (readback).
    expected: HashMap<String, Value>,
    /// Native request id (`per_…` / `frm_…`) → its pending prompt.
    pub(super) pending: Arc<Mutex<HashMap<String, CancellationToken>>>,
    /// Ids already answered or cancelled (resync must not re-show them).
    pub(super) settled: Arc<Mutex<HashSet<String>>>,
}

impl Drop for UserRequests {
    fn drop(&mut self) {
        self.token.cancel();
    }
}

impl UserRequests {
    pub(super) fn new(
        root: String,
        token: CancellationToken,
        rules: SessionRuleSet,
        root_rules: Value,
    ) -> Self {
        let mut owned = HashMap::new();
        owned.insert(root.clone(), OwnedSession::default());
        let mut expected = HashMap::new();
        expected.insert(root.clone(), root_rules);
        Self {
            root,
            token,
            rules,
            owned,
            expected,
            pending: Arc::new(Mutex::new(HashMap::new())),
            settled: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    /// Is `session` this run's root or one of its descendants?
    pub(super) fn owns(&self, session: &str) -> bool {
        self.owned.contains_key(session)
    }

    /// Child attribution for a request raised by `session`.
    pub(super) fn subagent_label(&self, session: &str) -> Option<String> {
        if session == self.root {
            return None;
        }
        Some(
            self.owned
                .get(session)
                .and_then(|owned| owned.label.clone())
                .unwrap_or_else(|| session.to_owned()),
        )
    }

    /// Already shown or answered?
    pub(super) fn known(&self, id: &str) -> bool {
        self.pending.lock().is_ok_and(|pending| pending.contains_key(id))
            || self.settled.lock().is_ok_and(|settled| settled.contains(id))
    }

    /// Register a pending prompt for a native request id.
    pub(super) fn track(&self, id: &str) -> CancellationToken {
        let token = self.token.child_token();
        if let Ok(mut pending) = self.pending.lock() {
            pending.insert(id.to_owned(), token.clone());
        }
        token
    }

    /// The request was settled outside LUNA (answered elsewhere, cancelled,
    /// or settled with a sibling): close its prompt.
    pub(super) fn close(&self, id: &str) {
        let token = self.pending.lock().ok().and_then(|mut p| p.remove(id));
        if let Some(token) = token {
            token.cancel();
        }
        mark_settled(&self.settled, id);
    }

    /// A child session appeared: when its parent is owned, own it too and
    /// cap it at the run's mode before it can act (F-PERM-12, OCF-123).
    pub(super) fn observe_session_created(
        &mut self,
        info: &Value,
        server: &Server,
        dir: Option<&str>,
        event_tx: &mpsc::Sender<Result<AgentEvent, HarnessError>>,
    ) {
        let (Some(id), Some(parent)) = (
            info.get("id").and_then(Value::as_str),
            info.get("parentID").and_then(Value::as_str),
        ) else {
            return;
        };
        if self.owns(id) || !self.owns(parent) {
            return;
        }
        let agent = info.get("agent").and_then(Value::as_str);
        let title = info.get("title").and_then(Value::as_str).filter(|t| !t.is_empty());
        let label = match (agent, title) {
            (Some(agent), Some(title)) => format!("{agent}: {title}"),
            (Some(agent), None) => agent.to_owned(),
            (None, Some(title)) => title.to_owned(),
            (None, None) => id.to_owned(),
        };
        self.owned.insert(
            id.to_owned(),
            OwnedSession {
                label: Some(label.clone()),
            },
        );
        let rules = self.rules.rules_for(agent, RuleScope::Subagent);
        self.expected.insert(id.to_owned(), rules.clone());
        let server = server.detached();
        let dir = dir.map(str::to_owned);
        let failures = event_tx.clone();
        let session = id.to_owned();
        let token = self.token.clone();
        tokio::spawn(async move {
            let path = format!("/api/session/{session}");
            if let Err(error) = server
                .patch_json(&path, dir.as_deref(), &json!({ "permissions": rules }))
                .await
            {
                if token.is_cancelled() {
                    return;
                }
                // An uncapped child must not keep running.
                let _ = server.interrupt_session(&session, dir.as_deref()).await;
                let _ = failures
                    .send(Ok(AgentEvent::Error {
                        message: format!(
                            "LUNA could not apply this chat's permission mode to the OpenCode \
                             sub-agent {label}, so it was stopped: {error}"
                        ),
                    }))
                    .await;
            }
        });
    }

    /// `session.permissions` (the echo of every rules change): an owned
    /// session whose rules differ from LUNA's was changed outside LUNA, so
    /// its execution is stopped (fail closed).
    pub(super) fn observe_session_permissions(
        &self,
        props: &Value,
        server: &Server,
        dir: Option<&str>,
        event_tx: &mpsc::Sender<Result<AgentEvent, HarnessError>>,
    ) {
        let Some(session) = props.get("sessionID").and_then(Value::as_str) else {
            return;
        };
        let Some(expected) = self.expected.get(session) else {
            return;
        };
        if props.get("permissions") == Some(expected) {
            return;
        }
        let server = server.detached();
        let dir = dir.map(str::to_owned);
        let failures = event_tx.clone();
        let session = session.to_owned();
        let who = match self.subagent_label(&session) {
            Some(label) => format!("the OpenCode sub-agent {label}"),
            None => "this OpenCode session".to_owned(),
        };
        tokio::spawn(async move {
            let _ = server.interrupt_session(&session, dir.as_deref()).await;
            let _ = failures
                .send(Ok(AgentEvent::Error {
                    message: format!(
                        "The permission rules of {who} were changed outside LUNA, so LUNA \
                         stopped it."
                    ),
                }))
                .await;
        });
    }
}

pub(super) fn mark_settled(settled: &Arc<Mutex<HashSet<String>>>, id: &str) {
    if let Ok(mut settled) = settled.lock() {
        if settled.len() >= MAX_SETTLED_IDS {
            settled.clear();
        }
        settled.insert(id.to_owned());
    }
}

/// A native `permission.asked` request, with what answering it needs.
pub(super) struct PermissionAsk<'a> {
    pub(super) props: &'a Value,
    pub(super) event_session: Option<&'a str>,
    pub(super) children: &'a HashMap<String, ChildRun>,
    pub(super) unbound_children: &'a HashMap<String, String>,
    pub(super) server: &'a Server,
    pub(super) dir: Option<&'a str>,
    pub(super) event_tx: &'a mpsc::Sender<Result<AgentEvent, HarnessError>>,
    pub(super) request_input: &'a Arc<RequestInput>,
    pub(super) auto_approve: bool,
    pub(super) permission_policy: &'a luna_proto::PermissionPolicy,
    pub(super) requests: &'a UserRequests,
}

/// The shared approval card for a v2 `permission.asked` request:
/// `{id, sessionID, action, resources[], save?, metadata?, source?: {type,
/// messageID, id}, message?}`.
pub(super) fn permission_question(
    props: &Value,
    session: &str,
    subagent: Option<String>,
    cwd: Option<&str>,
    policy: &luna_proto::PermissionPolicy,
) -> Option<UserInputQuestion> {
    let id = props.get("id").and_then(Value::as_str)?;
    let action = props
        .get("action")
        .and_then(Value::as_str)
        .filter(|action| !action.is_empty())
        .unwrap_or("tool");
    let clip = |text: &str, max: usize| -> String {
        if text.len() <= max {
            return text.to_owned();
        }
        let mut end = max;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &text[..end])
    };
    let resources: Vec<String> = props
        .get("resources")
        .and_then(Value::as_array)
        .map(|resources| {
            resources
                .iter()
                .filter_map(Value::as_str)
                .filter(|resource| *resource != "*")
                .take(32)
                .map(|resource| clip(resource, 1024))
                .collect()
        })
        .unwrap_or_default();
    let metadata = props.get("metadata").filter(|m| m.is_object());
    let meta_text = |key: &str| {
        metadata
            .and_then(|m| m.get(key))
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .map(|text| clip(text, 4096))
    };
    let action_kind = match action {
        "shell" => luna_proto::ApprovalActionKind::Command,
        "edit" => luna_proto::ApprovalActionKind::FileChange,
        "webfetch" | "websearch" => luna_proto::ApprovalActionKind::Network,
        "external_directory" => luna_proto::ApprovalActionKind::ExternalPath,
        _ => luna_proto::ApprovalActionKind::Tool,
    };
    // Command / input: the shell's parsed commands, the fetched URL, the
    // search query, or the edited paths.
    let command = match action {
        "shell" => meta_text("command").or_else(|| (!resources.is_empty()).then(|| resources.join("\n"))),
        "webfetch" => meta_text("url").or_else(|| resources.first().cloned()),
        "websearch" => meta_text("query").or_else(|| resources.first().cloned()),
        "edit" => meta_text("filepath").or_else(|| (!resources.is_empty()).then(|| resources.join(", "))),
        _ => (!resources.is_empty()).then(|| resources.join(", ")),
    };
    let file_changes = metadata
        .and_then(|m| m.get("files"))
        .filter(|files| files.as_array().is_some_and(|files| !files.is_empty()))
        .map(|files| {
            Value::Array(
                files
                    .as_array()
                    .into_iter()
                    .flatten()
                    .take(16)
                    .map(|file| {
                        json!({
                            "path": file.get("file").cloned().unwrap_or(Value::Null),
                            "status": file.get("status").cloned().unwrap_or(Value::Null),
                            "additions": file.get("additions").cloned().unwrap_or(Value::Null),
                            "deletions": file.get("deletions").cloned().unwrap_or(Value::Null),
                            "diff": file
                                .get("patch")
                                .and_then(Value::as_str)
                                .map(|patch| clip(patch, 16 * 1024)),
                        })
                    })
                    .collect(),
            )
        })
        .or_else(|| meta_text("diff").map(|diff| json!([{ "diff": clip(&diff, 16 * 1024) }])));
    let network = match action {
        "webfetch" => command
            .as_deref()
            .and_then(|url| reqwest::Url::parse(url).ok())
            .and_then(|url| {
                Some(luna_proto::ApprovalNetworkContext {
                    host: url.host_str()?.to_owned(),
                    protocol: url.scheme().to_owned(),
                })
            }),
        _ => None,
    };
    // The tool chip this ask belongs to: the same id the tool frames use.
    let item_id = props
        .get("source")
        .filter(|source| source.get("type").and_then(Value::as_str) == Some("tool"))
        .and_then(|source| {
            Some(format!(
                "{session}:{}:{}",
                source.get("messageID")?.as_str()?,
                source.get("id")?.as_str()?
            ))
        });
    let reason = props
        .get("message")
        .and_then(Value::as_str)
        .filter(|message| !message.is_empty())
        .map(|message| clip(message, 2048));
    let target = if policy.machine_name.is_empty() {
        "this machine"
    } else {
        policy.machine_name.as_str()
    };
    let who = match &subagent {
        Some(label) => format!("OpenCode sub-agent {label}"),
        None => "OpenCode".to_owned(),
    };
    let summary = match (action, resources.is_empty()) {
        ("shell", false) => format!("{who} requests to run {}", clip(&resources.join("; "), 512)),
        ("edit", false) => format!("{who} requests to edit {}", clip(&resources.join(", "), 512)),
        ("external_directory", false) => format!(
            "{who} requests access outside the workspace: {}",
            clip(&resources.join(", "), 512)
        ),
        (_, false) => format!(
            "{who} requests {action} permission for {}",
            clip(&resources.join(", "), 512)
        ),
        (_, true) => format!("{who} requests {action} permission"),
    };
    let mut header = match action_kind {
        luna_proto::ApprovalActionKind::Command => "Approve command",
        luna_proto::ApprovalActionKind::FileChange => "Approve file change",
        luna_proto::ApprovalActionKind::Network => "Approve network access",
        luna_proto::ApprovalActionKind::ExternalPath => "Approve path access",
        _ => "Approve tool",
    }
    .to_owned();
    if let Some(label) = &subagent {
        header.push_str(&format!(" · sub-agent {label}"));
    }
    let choices = opencode_approval_choices();
    let details = luna_proto::ApprovalDetails {
        provider_thread_id: Some(session.to_owned()),
        item_id,
        approval_id: Some(id.to_owned()),
        kind: Some(action.to_owned()),
        cwd: cwd.filter(|cwd| !cwd.is_empty()).map(str::to_owned),
        command,
        reason,
        file_changes,
        grant_root: (action == "external_directory" && !resources.is_empty())
            .then(|| resources.join(", ")),
        network,
        choices,
        subagent,
        ..Default::default()
    };
    Some(UserInputQuestion {
        id: format!("permission:{id}"),
        header,
        question: format!("{summary}. Target: {target}."),
        options: opencode_approval_options(),
        multi_select: false,
        mcp: None,
        approval: Some(luna_proto::ApprovalRequest {
            provider_request_id: id.to_owned(),
            provider: HarnessId::Opencode,
            turn_id: props
                .pointer("/source/messageID")
                .and_then(Value::as_str)
                .map(str::to_owned),
            machine_name: policy.machine_name.clone(),
            action_kind,
            summary,
            requested_scope: policy.scope,
            // No LUNA deadline (Codex and Claude parity): the card waits
            // until the user answers, OpenCode settles the request, Stop,
            // or the turn ends.
            expires_at_ms: luna_proto::APPROVAL_NO_EXPIRY,
            supports_task_allowance: opencode_supports_task_allowance(),
            details: Some(details),
        }),
        input: None,
    })
}

/// Route a permission request owned by this run (or one of its live
/// children) to the approval surface and reply on the native route.
pub(super) fn handle_permission_asked(ask: PermissionAsk<'_>) -> BusOutcome {
    let PermissionAsk {
        props,
        event_session,
        children,
        unbound_children,
        server,
        dir,
        event_tx,
        request_input,
        auto_approve,
        permission_policy,
        requests,
    } = ask;
    // A global bus includes unrelated sessions. Never answer their
    // requests, or a malformed request without an explicit owner.
    let Some(session) = event_session.filter(|session| {
        requests.owns(session)
            || children.get(*session).is_some_and(|child| !child.done)
            || unbound_children.contains_key(*session)
    }) else {
        return BusOutcome::Continue;
    };
    let Some(id) = props.get("id").and_then(Value::as_str) else {
        return BusOutcome::Continue;
    };
    // A resync or a re-delivered frame must not open a second card.
    if requests.known(id) {
        return BusOutcome::Continue;
    }
    let subagent = requests.subagent_label(session);
    let reply = PermissionReply {
        server: server.detached(),
        path: format!("/api/session/{session}/permission/{id}/reply"),
        dir: dir.map(str::to_owned),
        failures: event_tx.clone(),
        id: id.to_owned(),
        pending: Arc::clone(&requests.pending),
        settled: Arc::clone(&requests.settled),
    };
    if auto_approve {
        // Full: a stray ask (the compiled rules allow everything else) is
        // allowed once, never parked on a LUNA card (OCF-53).
        let token = requests.track(id);
        tokio::spawn(async move {
            reply.send(&token, Some(CHOICE_ALLOW_ONCE)).await;
        });
        return BusOutcome::Continue;
    }
    let Some(question) = permission_question(props, session, subagent, dir, permission_policy)
    else {
        return BusOutcome::Continue;
    };
    let token = requests.track(id);
    let question_id = question.id.clone();
    let receiver = (request_input)(vec![question]);
    tokio::spawn(async move {
        let answers = tokio::select! {
            biased;
            // Stop, the end of the turn, or OpenCode settling it elsewhere.
            _ = token.cancelled() => {
                reply.forget();
                return;
            }
            answers = receiver => answers,
        };
        let label = match answers {
            Ok(answers) => answers
                .into_iter()
                .find(|answer| answer.question_id == question_id)
                .and_then(|answer| answer.labels.into_iter().next()),
            // The engine dropped the request without an answer.
            Err(_) => None,
        };
        reply.send(&token, label.as_deref()).await;
    });
    BusOutcome::Continue
}

/// Where and how one permission request is answered.
struct PermissionReply {
    server: Server,
    path: String,
    dir: Option<String>,
    failures: mpsc::Sender<Result<AgentEvent, HarnessError>>,
    id: String,
    pending: Arc<Mutex<HashMap<String, CancellationToken>>>,
    settled: Arc<Mutex<HashSet<String>>>,
}

impl PermissionReply {
    fn forget(&self) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.remove(&self.id);
        }
    }

    async fn send(self, token: &CancellationToken, label: Option<&str>) {
        self.forget();
        mark_settled(&self.settled, &self.id);
        if token.is_cancelled() {
            return;
        }
        let body = opencode_permission_reply_body(label);
        let failure = match self
            .server
            .post_json_raw(&self.path, self.dir.as_deref(), &body)
            .await
        {
            Ok((status, _)) if status.is_success() => None,
            // Already settled (a sibling reject, Stop, another client).
            Ok((status, _)) if status == reqwest::StatusCode::NOT_FOUND => {
                tracing::debug!(target: "luna_harness::opencode", id = %self.id, "permission already settled");
                None
            }
            Ok((status, text)) => Some(post_error_message(&self.path, status, &text)),
            Err(error) => Some(error.to_string()),
        };
        // A failed reply leaves the native tool blocked: say so instead of
        // silently pretending the approval was delivered (OCF-151).
        if let Some(detail) = failure
            && !token.is_cancelled()
        {
            let _ = self
                .failures
                .send(Ok(AgentEvent::Error {
                    message: format!("OpenCode did not accept the permission reply: {detail}"),
                }))
                .await;
        }
    }
}

/// `permission.replied {sessionID, requestID, reply}`: the request was
/// settled (by LUNA, a sibling reject, Stop, or another client); close any
/// card still open for it (OCF-54).
pub(super) fn handle_permission_replied(props: &Value, requests: &UserRequests) -> BusOutcome {
    if let Some(id) = props.get("requestID").and_then(Value::as_str)
        && props
            .get("sessionID")
            .and_then(Value::as_str)
            .is_some_and(|session| requests.owns(session))
    {
        requests.close(id);
    }
    BusOutcome::Continue
}
