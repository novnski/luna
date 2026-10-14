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
//! so the chat's directory stays LUNA-owned. Child (subagent) sessions
//! inherit their parent's session rules at creation (`oc2:core/src/
//! session.ts:276`), so they start at the parent's ceiling; on
//! `session.created` LUNA replaces those with the same mode compiled against
//! the CHILD's agent plus `luna_*: deny` — LUNA tools are root-only for
//! every provider, and an agent's own restrictions survive.
//!
//! Replies are `POST /api/session/{s}/permission/{id}/reply` with
//! `{decision: "once" | "reject", message?}`. LUNA never sends `always`
//! (PD-8): OpenCode 2 writes it as a project-wide saved rule. `reject` with a
//! message is OpenCode's "decline with feedback" (the model continues);
//! `reject` without one stops the execution.
//!
//! "Allow for session" (Claude's and Codex's session allowance) is a
//! LUNA-owned session rule instead (OC-04): the request's own `save`
//! patterns (what OpenCode's `always` would save project-wide) become
//! `allow` rules on the asking session only, PATCHed and read back before
//! the request is answered `once`. The harness keeps them per session and
//! re-applies them at every run start while the chat's permission mode is
//! unchanged ([`SessionGrants`]); a mode change or an engine restart ends
//! them, as it ends Claude's and Codex's warm sessions. The agent's and the
//! user's denies stay final ([`with_session_grants`]).

use std::collections::HashSet;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

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
pub(super) const CHOICE_ALLOW_SESSION: &str = "Allow for session";
pub(super) const CHOICE_CANCEL_TURN: &str = "Cancel turn";

/// Bounds on LUNA's session grants: patterns one answer may add, grant
/// rules one session keeps, and sessions the harness remembers.
const MAX_GRANT_PATTERNS: usize = 16;
const MAX_SESSION_GRANT_RULES: usize = 64;
const MAX_GRANT_SESSIONS: usize = 256;
/// Rulesets remembered per owned session as LUNA's own (their echoes).
const MAX_APPLIED_RULESETS: usize = 16;

/// The feedback a Deny sends, so the model continues without the action.
pub(super) const DENY_FEEDBACK: &str =
    "The user denied this request in LUNA. Continue without it, or ask the user how to proceed.";
/// The feedback when LUNA closes an approval without an answer.
const UNANSWERED_FEEDBACK: &str =
    "LUNA closed this approval without an answer, so it was not approved. Continue without it.";

/// Answered native request ids remembered against resync re-delivery.
const MAX_SETTLED_IDS: usize = 1024;

/// The agent list right after boot: up to 5 s at 250 ms.
const AGENT_LIST_ATTEMPTS: u32 = 20;
const AGENT_LIST_POLL: Duration = Duration::from_millis(250);

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
            let Some(rules) =
                agent
                    .get("permissions")
                    .and_then(Value::as_array)
                    .and_then(|rules| {
                        rules
                            .iter()
                            .map(Rule::from_json)
                            .collect::<Option<Vec<_>>>()
                    })
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
        // A freshly booted location registers its agents shortly after the
        // first request (observed live on 2.0.22: `{data: []}` right after
        // boot), so an empty list or a 503 is polled briefly.
        let mut agents = None;
        for attempt in 0..AGENT_LIST_ATTEMPTS {
            let last = attempt + 1 == AGENT_LIST_ATTEMPTS;
            match server.get_json("/api/agent", dir).await {
                Ok(list) => {
                    let empty = list
                        .get("data")
                        .and_then(Value::as_array)
                        .is_some_and(Vec::is_empty);
                    if !empty || last {
                        agents = AgentRules::from_list(&list);
                        break;
                    }
                }
                Err(error) if !last && error.to_string().contains(" 503 ") => {}
                Err(error) => {
                    tracing::warn!(target: "luna_harness::opencode", %error, "agent list unavailable");
                    break;
                }
            }
            tokio::time::sleep(AGENT_LIST_POLL).await;
        }
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

    pub(super) fn mode(&self) -> luna_proto::PermissionMode {
        self.mode
    }
}

/// The `allow` rules an "Allow for session" answer adds for a v2
/// `permission.asked` request: OpenCode's own `save` patterns for it (what
/// its `always` would save project-wide), scoped to the asking session.
/// Empty — the choice is not offered — when OpenCode suggests nothing, the
/// suggestion is malformed or oversized, or the action is one LUNA keeps
/// denied (session moves) or engine-owned (LUNA tools).
pub(super) fn session_grant_rules(props: &Value) -> Vec<Rule> {
    let Some(action) = props
        .get("action")
        .and_then(Value::as_str)
        .filter(|action| !action.is_empty() && action.len() <= 256)
    else {
        return Vec::new();
    };
    // A rule's action is itself a pattern: a wildcard here would grant more
    // than the request's own action.
    if action.contains(['*', '?'])
        || action == SESSION_MOVE_ACTION
        || wildcard_match(action, LUNA_TOOL_ACTION_PATTERN)
    {
        return Vec::new();
    }
    let Some(save) = props
        .get("save")
        .and_then(Value::as_array)
        .filter(|save| !save.is_empty() && save.len() <= MAX_GRANT_PATTERNS)
    else {
        return Vec::new();
    };
    let mut rules = Vec::new();
    for pattern in save {
        let Some(pattern) = pattern
            .as_str()
            .filter(|pattern| !pattern.is_empty() && pattern.len() <= 1024)
        else {
            return Vec::new();
        };
        let rule = Rule::new(action, pattern, RuleEffect::Allow);
        if !rules.contains(&rule) {
            rules.push(rule);
        }
    }
    rules
}

/// `base` (LUNA's compiled rules for a session) with the session's grants.
/// Last match wins, so the grants go last; then, for each granted action,
/// the base's own rules for it from its first deny onward are replayed
/// (narrowed to that action), so a deny — the agent's, the user's
/// configured one, or LUNA's containment — stays final over a grant, and
/// every other action evaluates exactly as in `base`. A base LUNA cannot
/// read gets no grants (more asks, never fewer).
pub(super) fn with_session_grants(base: &Value, grants: &[Rule]) -> Value {
    if grants.is_empty() {
        return base.clone();
    }
    let Some(base_rules) = base.as_array().and_then(|rules| {
        rules
            .iter()
            .map(Rule::from_json)
            .collect::<Option<Vec<_>>>()
    }) else {
        return base.clone();
    };
    let mut out = base_rules.clone();
    out.extend(grants.iter().cloned());
    let mut actions: Vec<&str> = Vec::new();
    for grant in grants {
        if !actions.contains(&grant.action.as_str()) {
            actions.push(&grant.action);
        }
    }
    for action in actions {
        let applies = |rule: &Rule| wildcard_match(action, &rule.action);
        let Some(first_deny) = base_rules
            .iter()
            .position(|rule| rule.effect == RuleEffect::Deny && applies(rule))
        else {
            continue;
        };
        out.extend(
            base_rules[first_deny..]
                .iter()
                .filter(|rule| applies(rule))
                .map(|rule| Rule::new(action, &rule.resource, rule.effect)),
        );
    }
    rules_json(&out)
}

/// One session's grants and the permission mode they were made under.
struct GrantSet {
    mode: luna_proto::PermissionMode,
    rules: Vec<Rule>,
    /// Insertion order, for evicting the oldest session at the bound.
    seq: u64,
}

/// LUNA-owned "Allow for session" grants by OpenCode session id, shared by
/// every run of one harness (one engine): a chat's later turns, each its
/// own run, keep them like Claude's and Codex's warm sessions do. Nothing
/// is written to OpenCode's own config or saved rules.
#[derive(Clone, Default)]
pub(super) struct SessionGrants {
    sets: Arc<Mutex<HashMap<String, GrantSet>>>,
    seq: Arc<AtomicU64>,
}

impl SessionGrants {
    /// The session's grants for a run in `mode`. Grants made under another
    /// mode are dropped: a permission change ends the session allowance.
    pub(super) fn current(&self, session: &str, mode: luna_proto::PermissionMode) -> Vec<Rule> {
        let Ok(mut sets) = self.sets.lock() else {
            return Vec::new();
        };
        match sets.get(session) {
            Some(set) if set.mode == mode => set.rules.clone(),
            Some(_) => {
                sets.remove(session);
                Vec::new()
            }
            None => Vec::new(),
        }
    }

    /// Add `rules` to the session's grants: `(every grant, the newly added
    /// ones)`, or `None` at the per-session bound.
    fn add(
        &self,
        session: &str,
        mode: luna_proto::PermissionMode,
        rules: &[Rule],
    ) -> Option<(Vec<Rule>, Vec<Rule>)> {
        let mut sets = self.sets.lock().ok()?;
        if !sets.contains_key(session)
            && sets.len() >= MAX_GRANT_SESSIONS
            && let Some(oldest) = sets
                .iter()
                .min_by_key(|(_, set)| set.seq)
                .map(|(id, _)| id.clone())
        {
            sets.remove(&oldest);
        }
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        let set = sets.entry(session.to_owned()).or_insert_with(|| GrantSet {
            mode,
            rules: Vec::new(),
            seq,
        });
        if set.mode != mode {
            *set = GrantSet {
                mode,
                rules: Vec::new(),
                seq,
            };
        }
        let added: Vec<Rule> = rules
            .iter()
            .filter(|rule| !set.rules.contains(rule))
            .cloned()
            .collect();
        if set.rules.len() + added.len() > MAX_SESSION_GRANT_RULES {
            return None;
        }
        set.rules.extend(added.iter().cloned());
        Some((set.rules.clone(), added))
    }

    /// Withdraw `rules` (a grant OpenCode did not apply).
    fn remove(&self, session: &str, rules: &[Rule]) {
        if let Ok(mut sets) = self.sets.lock()
            && let Some(set) = sets.get_mut(session)
        {
            set.rules.retain(|rule| !rules.contains(rule));
            if set.rules.is_empty() {
                sets.remove(session);
            }
        }
    }

    /// Every session's grants as one JSON object keyed
    /// `grants:<session id>` (CORE-11). A hosted provider worker runs one
    /// turn; it exports them when its run ends and the next run's worker
    /// imports them, so a chat's later turns keep them as one in-process
    /// harness did. A session whose grants were dropped is absent.
    pub(super) fn export(&self) -> Value {
        let Ok(sets) = self.sets.lock() else {
            return json!({});
        };
        let mut state = serde_json::Map::new();
        for (session, set) in sets.iter() {
            state.insert(
                format!("{GRANT_STATE_PREFIX}{session}"),
                json!({"mode": set.mode, "seq": set.seq, "rules": rules_json(&set.rules)}),
            );
        }
        Value::Object(state)
    }

    /// Restore what [`Self::export`] wrote; unreadable entries are skipped
    /// and the bounds still hold.
    pub(super) fn import(&self, state: &Value) {
        let Some(entries) = state.as_object() else {
            return;
        };
        let Ok(mut sets) = self.sets.lock() else {
            return;
        };
        for (key, entry) in entries {
            let Some(session) = key.strip_prefix(GRANT_STATE_PREFIX) else {
                continue;
            };
            let Some(mode) = entry
                .get("mode")
                .cloned()
                .and_then(|mode| serde_json::from_value::<luna_proto::PermissionMode>(mode).ok())
            else {
                continue;
            };
            let rules: Vec<Rule> = entry
                .get("rules")
                .and_then(Value::as_array)
                .map(|rules| {
                    rules
                        .iter()
                        .filter_map(Rule::from_json)
                        .take(MAX_SESSION_GRANT_RULES)
                        .collect()
                })
                .unwrap_or_default();
            if session.is_empty() || rules.is_empty() {
                continue;
            }
            if !sets.contains_key(session) && sets.len() >= MAX_GRANT_SESSIONS {
                continue;
            }
            let seq = entry.get("seq").and_then(Value::as_u64).unwrap_or(0);
            self.seq.fetch_max(seq.saturating_add(1), Ordering::Relaxed);
            sets.insert(session.to_owned(), GrantSet { mode, rules, seq });
        }
    }
}

/// Key prefix of one session's grants in [`SessionGrants::export`].
const GRANT_STATE_PREFIX: &str = "grants:";

/// The rules LUNA applied to one owned session in this run.
struct AppliedRules {
    /// LUNA's compiled rules without session grants.
    base: Value,
    /// Every ruleset LUNA applied, newest last: the `session.permissions`
    /// echo of any of them is LUNA's own (a grant's PATCH can overtake the
    /// echo of the one before it).
    applied: VecDeque<Value>,
}

impl AppliedRules {
    fn new(base: Value, applied: Value) -> Self {
        Self {
            base,
            applied: VecDeque::from([applied]),
        }
    }

    fn current(&self) -> Option<&Value> {
        self.applied.back()
    }

    fn accept(&mut self, rules: Value) {
        if self.applied.len() >= MAX_APPLIED_RULESETS {
            self.applied.pop_front();
        }
        self.applied.push_back(rules);
    }
}

/// Permission state a run shares with its reply tasks.
pub(super) struct RuleState {
    root: String,
    mode: luna_proto::PermissionMode,
    grants: SessionGrants,
    sessions: Mutex<HashMap<String, AppliedRules>>,
    /// Serializes session-grant rule changes (PATCH, readback).
    apply: tokio::sync::Mutex<()>,
    /// LUNA answered "Cancel turn" in the current turn: the interruption
    /// that follows is the user's, not a provider failure.
    turn_cancelled: AtomicBool,
}

impl RuleState {
    /// Does LUNA know the rules of `session` (so it can add a grant)?
    fn tracks(&self, session: &str) -> bool {
        self.sessions
            .lock()
            .is_ok_and(|sessions| sessions.contains_key(session))
    }

    /// Keep `grant` on `session`: add it to the harness's grants, PATCH the
    /// session's rules with every grant, and read them back. On failure the
    /// grant is withdrawn and LUNA's previous rules are put back.
    async fn grant(
        &self,
        server: &Server,
        dir: Option<&str>,
        session: &str,
        grant: &[Rule],
    ) -> Result<(), String> {
        let _serial = self.apply.lock().await;
        let Some((all, added)) = self.grants.add(session, self.mode, grant) else {
            return Err(
                "this OpenCode session already keeps the most session approvals LUNA \
                        allows"
                    .into(),
            );
        };
        let rules = self.sessions.lock().ok().and_then(|mut sessions| {
            let entry = sessions.get_mut(session)?;
            let previous = entry.current()?.clone();
            let rules = with_session_grants(&entry.base, &all);
            entry.accept(rules.clone());
            Some((previous, rules))
        });
        let Some((previous, rules)) = rules else {
            self.grants.remove(session, &added);
            return Err("LUNA no longer tracks this OpenCode session's rules".into());
        };
        let path = format!("/api/session/{session}");
        let applied = match server
            .patch_json(&path, dir, &json!({ "permissions": rules }))
            .await
        {
            Ok(_) => match server.session_info(session, dir).await {
                Ok(info) if info.get("permissions") == Some(&rules) => Ok(()),
                Ok(_) => Err("OpenCode did not keep the rule on the session".to_owned()),
                Err(error) => Err(format!("the rule could not be read back: {error}")),
            },
            Err(error) => Err(error.to_string()),
        };
        if applied.is_err() {
            self.grants.remove(session, &added);
            let _ = server
                .patch_json(&path, dir, &json!({ "permissions": previous }))
                .await;
            if let Ok(mut sessions) = self.sessions.lock()
                && let Some(entry) = sessions.get_mut(session)
            {
                entry.accept(previous);
            }
        }
        applied
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

/// The card's choices, Claude's shape: Deny continues, Cancel turn stops,
/// and "Allow for session" appears when the request carries a session
/// grant (`grant`, from [`session_grant_rules`]).
pub(super) fn opencode_approval_choices(grant: &[Rule]) -> Vec<luna_proto::ApprovalChoice> {
    let mut choices = vec![
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
    ];
    if let Some(first) = grant.first() {
        let patterns: Vec<&str> = grant.iter().map(|rule| rule.resource.as_str()).collect();
        let covered = if patterns == ["*"] {
            format!("every {} request", first.action)
        } else {
            format!("{} requests matching {}", first.action, patterns.join(", "))
        };
        let mut description = format!(
            "Allow this request and {covered} in this OpenCode session until LUNA restarts or \
             the chat's permission mode changes. LUNA keeps it as a session rule and does not \
             save OpenCode rules."
        );
        if description.len() > 2048 {
            let mut end = 2048;
            while !description.is_char_boundary(end) {
                end -= 1;
            }
            description.truncate(end);
            description.push('…');
        }
        choices.push(luna_proto::ApprovalChoice {
            label: CHOICE_ALLOW_SESSION.into(),
            scope: "session".into(),
            description,
        });
    }
    choices.push(luna_proto::ApprovalChoice {
        label: CHOICE_CANCEL_TURN.into(),
        scope: "none".into(),
        description: "Reject this request and stop OpenCode's turn.".into(),
    });
    choices
}

/// The v2 reply body for a LUNA answer. "Allow once" allows, and so does
/// "Allow for session" on a card that offered it (`session_offered`; its
/// rule is applied first, the reply is still `once`); "Cancel turn" stops
/// the execution; anything else — Deny, an empty (fail-closed) answer, a
/// forged or stale session/task allowance — rejects with feedback so the
/// model continues.
pub(super) fn opencode_permission_reply_body(
    decision: Option<&str>,
    session_offered: bool,
) -> Value {
    match decision {
        Some(CHOICE_ALLOW_ONCE) => json!({ "decision": "once" }),
        Some(CHOICE_ALLOW_SESSION) if session_offered => json!({ "decision": "once" }),
        Some(CHOICE_CANCEL_TURN) | Some("Cancel") => json!({ "decision": "reject" }),
        Some(_) => json!({ "decision": "reject", "message": DENY_FEEDBACK }),
        None => json!({ "decision": "reject", "message": UNANSWERED_FEEDBACK }),
    }
}

/// Does this answer cancel the turn?
fn cancels_turn(decision: Option<&str>) -> bool {
    matches!(decision, Some(CHOICE_CANCEL_TURN) | Some("Cancel"))
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
    /// The rules LUNA applied per owned session (readback, grants) and the
    /// turn's cancel mark, shared with reply tasks.
    state: Arc<RuleState>,
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
    /// `root_base` is LUNA's compiled root ruleset; `root_rules` what was
    /// applied to the session (the base with the session's grants).
    pub(super) fn new(
        root: String,
        token: CancellationToken,
        rules: SessionRuleSet,
        root_base: Value,
        root_rules: Value,
        grants: SessionGrants,
    ) -> Self {
        let mut owned = HashMap::new();
        owned.insert(root.clone(), OwnedSession::default());
        let mut sessions = HashMap::new();
        sessions.insert(root.clone(), AppliedRules::new(root_base, root_rules));
        let state = Arc::new(RuleState {
            root: root.clone(),
            mode: rules.mode(),
            grants,
            sessions: Mutex::new(sessions),
            apply: tokio::sync::Mutex::new(()),
            turn_cancelled: AtomicBool::new(false),
        });
        Self {
            root,
            token,
            rules,
            owned,
            state,
            pending: Arc::new(Mutex::new(HashMap::new())),
            settled: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    /// LUNA answered "Cancel turn" during the current turn.
    pub(super) fn turn_cancelled(&self) -> bool {
        self.state.turn_cancelled.load(Ordering::Acquire)
    }

    /// A new turn starts: an earlier Cancel turn no longer explains an
    /// interruption.
    pub(super) fn begin_turn(&self) {
        self.state.turn_cancelled.store(false, Ordering::Release);
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
        self.pending
            .lock()
            .is_ok_and(|pending| pending.contains_key(id))
            || self
                .settled
                .lock()
                .is_ok_and(|settled| settled.contains(id))
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
        let title = info
            .get("title")
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty());
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
        let base = self.rules.rules_for(agent, RuleScope::Subagent);
        let rules = with_session_grants(&base, &self.state.grants.current(id, self.state.mode));
        if let Ok(mut sessions) = self.state.sessions.lock() {
            sessions.insert(id.to_owned(), AppliedRules::new(base, rules.clone()));
        }
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
    /// session whose rules differ from every ruleset LUNA applied to it was
    /// changed outside LUNA, so its execution is stopped (fail closed).
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
        let lunas_own = {
            let Ok(sessions) = self.state.sessions.lock() else {
                return;
            };
            let Some(entry) = sessions.get(session) else {
                return;
            };
            props
                .get("permissions")
                .is_some_and(|rules| entry.applied.contains(rules))
        };
        if lunas_own {
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
/// messageID, id}, message?}`. `grant` is the session rule "Allow for
/// session" would add (empty: not offered).
pub(super) fn permission_question(
    props: &Value,
    session: &str,
    subagent: Option<String>,
    cwd: Option<&str>,
    policy: &luna_proto::PermissionPolicy,
    grant: &[Rule],
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
    let command =
        match action {
            "shell" => meta_text("command")
                .or_else(|| (!resources.is_empty()).then(|| resources.join("\n"))),
            "webfetch" => meta_text("url").or_else(|| resources.first().cloned()),
            "websearch" => meta_text("query").or_else(|| resources.first().cloned()),
            "edit" => meta_text("filepath")
                .or_else(|| (!resources.is_empty()).then(|| resources.join(", "))),
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
        ("edit", false) => format!(
            "{who} requests to edit {}",
            clip(&resources.join(", "), 512)
        ),
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
    let choices = opencode_approval_choices(grant);
    let options = choices.iter().map(|choice| choice.label.clone()).collect();
    let details = luna_proto::ApprovalDetails {
        provider_thread_id: Some(session.to_owned()),
        item_id,
        approval_id: Some(id.to_owned()),
        kind: Some(action.to_owned()),
        cwd: cwd.filter(|cwd| !cwd.is_empty()).map(str::to_owned),
        command,
        reason,
        file_changes,
        // External directories ride `command`: OpenCode's grant covers any
        // access there, so it is not presented as a write root.
        network,
        choices,
        subagent,
        ..Default::default()
    };
    Some(UserInputQuestion {
        id: format!("permission:{id}"),
        header,
        question: format!("{summary}. Target: {target}."),
        options,
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
            supports_task_allowance: !grant.is_empty(),
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
    // "Allow for session" needs the session's own LUNA rules to extend.
    let grant = if auto_approve || !requests.state.tracks(session) {
        Vec::new()
    } else {
        session_grant_rules(props)
    };
    let mut reply = PermissionReply {
        server: server.detached(),
        path: format!("/api/session/{session}/permission/{id}/reply"),
        dir: dir.map(str::to_owned),
        failures: event_tx.clone(),
        id: id.to_owned(),
        pending: Arc::clone(&requests.pending),
        settled: Arc::clone(&requests.settled),
        session: session.to_owned(),
        grant: Vec::new(),
        state: Arc::clone(&requests.state),
    };
    if auto_approve {
        // Full: a stray ask (the compiled rules allow everything else) is
        // allowed once, never parked on a LUNA card (OCF-53).
        let token = requests.track(id);
        tokio::spawn(async move {
            if let Err(detail) = reply.send(&token, Some(CHOICE_ALLOW_ONCE)).await {
                reply.settle();
                reply.report(&detail).await;
            }
        });
        return BusOutcome::Continue;
    }
    let Some(question) =
        permission_question(props, session, subagent, dir, permission_policy, &grant)
    else {
        return BusOutcome::Continue;
    };
    reply.grant = grant;
    let token = requests.track(id);
    let question_id = question.id.clone();
    let request_input = Arc::clone(request_input);
    let mut receiver = (request_input)(vec![question.clone()]);
    tokio::spawn(async move {
        loop {
            let answers = tokio::select! {
                biased;
                // Stop, the end of the turn, or OpenCode settling it elsewhere.
                _ = token.cancelled() => {
                    reply.forget();
                    return;
                }
                answers = &mut receiver => answers,
            };
            let answered = answers.is_ok();
            let label = match answers {
                Ok(answers) => answers
                    .into_iter()
                    .find(|answer| answer.question_id == question_id)
                    .and_then(|answer| answer.labels.into_iter().next()),
                // The engine dropped the request without an answer.
                Err(_) => None,
            };
            let Err(detail) = reply.send(&token, label.as_deref()).await else {
                return;
            };
            if !answered {
                reply.settle();
                reply.report(&detail).await;
                return;
            }
            // OpenCode never got the answer and its tool still waits: show
            // the card again so the turn never hangs on a request the user
            // can no longer see.
            let _ = reply
                .failures
                .send(Ok(AgentEvent::Error {
                    message: format!(
                        "LUNA could not deliver your answer to OpenCode's permission request \
                         ({detail}). The request is shown again."
                    ),
                }))
                .await;
            receiver = (request_input)(vec![question.clone()]);
        }
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
    /// The session that asked.
    session: String,
    /// The session rule "Allow for session" adds (empty: not offered).
    grant: Vec<Rule>,
    state: Arc<RuleState>,
}

impl PermissionReply {
    fn forget(&self) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.remove(&self.id);
        }
    }

    /// The request is settled for LUNA: no card, and a resync must not show
    /// it again.
    fn settle(&self) {
        self.forget();
        mark_settled(&self.settled, &self.id);
    }

    /// A reply that could not be delivered and will not be asked again
    /// leaves the native tool blocked: say so instead of silently
    /// pretending the approval was delivered (OCF-151).
    async fn report(&self, detail: &str) {
        let _ = self
            .failures
            .send(Ok(AgentEvent::Error {
                message: format!("OpenCode did not accept the permission reply: {detail}"),
            }))
            .await;
    }

    /// Deliver the user's answer. `Err` = OpenCode never took it: the
    /// request still waits upstream, so it stays pending here (a resync
    /// shows no duplicate, a settle elsewhere still closes it) and the
    /// caller shows the card again or reports it. Stop and the end of the
    /// turn send nothing.
    async fn send(&self, token: &CancellationToken, label: Option<&str>) -> Result<(), String> {
        if token.is_cancelled() {
            self.settle();
            return Ok(());
        }
        let session_offered = !self.grant.is_empty();
        // "Allow for session": the session rule lands (and reads back)
        // before the request is allowed, so the next matching request no
        // longer asks. A rule that did not land still allows this request.
        if label == Some(CHOICE_ALLOW_SESSION)
            && session_offered
            && let Err(detail) = self
                .state
                .grant(
                    &self.server,
                    self.dir.as_deref(),
                    &self.session,
                    &self.grant,
                )
                .await
            && !token.is_cancelled()
        {
            let _ = self
                .failures
                .send(Ok(AgentEvent::Error {
                    message: format!(
                        "LUNA could not keep this approval for the OpenCode session ({detail}), \
                         so only this request was allowed."
                    ),
                }))
                .await;
        }
        if token.is_cancelled() {
            self.settle();
            return Ok(());
        }
        // Cancel turn: the interruption OpenCode reports next is the
        // user's own (2.0.22 labels it `shutdown`), marked before the reply
        // so the mark is never late.
        let cancel = cancels_turn(label);
        if cancel {
            self.state.turn_cancelled.store(true, Ordering::Release);
        }
        let body = opencode_permission_reply_body(label, session_offered);
        let (failure, rejected) = match self
            .server
            .deliver(
                reqwest::Method::POST,
                &self.path,
                self.dir.as_deref(),
                Some(&body),
            )
            .await
        {
            Ok(sent) if sent.status.is_success() => (None, true),
            // Already settled (a sibling reject, Stop, another client), or
            // an earlier attempt of this same reply landed.
            Ok(sent) if sent.status == reqwest::StatusCode::NOT_FOUND => {
                tracing::debug!(target: "luna_harness::opencode", id = %self.id, "permission already settled");
                (None, sent.retried)
            }
            Ok(sent) => (
                Some(post_error_message(&self.path, sent.status, &sent.body)),
                false,
            ),
            Err(error) => (Some(error.to_string()), false),
        };
        if cancel {
            if !rejected {
                // The reject did not land, so it cancelled nothing.
                self.state.turn_cancelled.store(false, Ordering::Release);
            } else if self.session != self.state.root {
                // A sub-agent's reject stops only that sub-agent; Cancel
                // turn stops OpenCode's turn, as it does for Claude and
                // Codex.
                let _ = self
                    .server
                    .interrupt_session(&self.state.root, self.dir.as_deref())
                    .await;
            }
        }
        match failure {
            // Stop, or a settle elsewhere, overtook a failed reply.
            Some(detail) if !token.is_cancelled() => Err(detail),
            _ => {
                self.settle();
                Ok(())
            }
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
