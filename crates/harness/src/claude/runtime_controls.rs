//! CC-06 runtime controls for the Claude stream-json driver (CLI 2.1.286).
//!
//! Probe-verified control requests (zero user messages, isolated config):
//! - `get_settings` → `{applied:{model, effort, …}, effective, sources}`. The
//!   `applied` block is the authoritative readback: `--effort high` on Haiku
//!   reads back `effort: null`.
//! - `apply_flag_settings {settings:{effortLevel, fastMode, alwaysThinkingEnabled}}`
//!   acks `{subtype:"success"}` even for an invalid level ("turbo"); only the
//!   readback shows whether it applied. Session-only; no settings file write.
//! - `set_model {model}` validates the model (needs auth) and answers
//!   `{subtype:"error", error, error_code}` when it cannot.
//!
//! So every live change is: send the change → all acks → `get_settings` →
//! compare `applied` → only then write the user's prompt. Anything else fails
//! closed (the prompt is not written under the old configuration).
//!
//! This module owns a minimal outbound control-request correlation (ids with
//! the `luna-cc06-` prefix). The transport package (CC-01) can absorb it into a
//! general registry; the contract is `request_line` + `ControlReply::parse`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use luna_proto::{ReasoningLevel, RuntimeReadback, RuntimeReadbackSource, UsageProvider};
use serde_json::{Value, json};

use super::catalog::to_effort;
use crate::LiveRuntimeSelection;

const ID_PREFIX: &str = "luna-cc06-";
/// Bound on staged switches nobody consumed (a steer that never arrived).
const MAX_STAGED: usize = 32;
/// The CLI's own effort levels, in order.
const LEVELS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];
/// Model options a live switch may change; every other option must be equal.
pub const SWITCHABLE_OPTIONS: [&str; 3] = ["contextWindow", "fastMode", "thinking"];

fn option_is_on(options: &serde_json::Map<String, Value>, key: &str) -> bool {
    match options.get(key) {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => s == "on" || s == "true",
        _ => false,
    }
}

/// The `--model` value LUNA launches with (`<id>[1m]` selects the 1M window).
pub(crate) fn model_flag(
    model: Option<&str>,
    options: &serde_json::Map<String, Value>,
) -> Option<String> {
    let model = model?;
    let one_m = options.get("contextWindow").and_then(Value::as_str) == Some("1m");
    Some(if one_m && !model.ends_with("[1m]") {
        format!("{model}[1m]")
    } else {
        model.to_owned()
    })
}

// ---------------------------------------------------------------------------
// Effort levels from discovery (B1/B2)
// ---------------------------------------------------------------------------

/// Per-executable effort ladders from the CLI's own `initialize.models`.
/// `None` for a model = effort support unknown (fall back to the heuristic);
/// `Some([])` = the CLI reports no effort for that model.
type EffortCatalog = HashMap<String, Option<Vec<&'static str>>>;

fn catalogs() -> &'static Mutex<HashMap<PathBuf, EffortCatalog>> {
    static CATALOGS: OnceLock<Mutex<HashMap<PathBuf, EffortCatalog>>> = OnceLock::new();
    CATALOGS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Remember the effort ladders an `initialize` response advertised for `exe`.
pub(crate) fn remember_catalog(exe: &Path, response: &Value) {
    let Some(entries) = response
        .get("response")
        .and_then(|body| body.get("models"))
        .and_then(Value::as_array)
    else {
        return;
    };
    let mut catalog = EffortCatalog::new();
    for entry in entries {
        let levels = match entry.get("supportedEffortLevels").and_then(Value::as_array) {
            Some(levels) => Some(
                LEVELS
                    .into_iter()
                    .filter(|level| levels.iter().any(|v| v.as_str() == Some(*level)))
                    .collect(),
            ),
            None if entry.get("supportsEffort").and_then(Value::as_bool) == Some(true) => None,
            // `supportsEffort` false/null with no ladder: the CLI applies none.
            None => Some(Vec::new()),
        };
        for key in ["value", "resolvedModel"] {
            if let Some(id) = entry.get(key).and_then(Value::as_str).map(str::trim)
                && !id.is_empty()
            {
                catalog.insert(id.to_owned(), levels.clone());
            }
        }
    }
    if let Ok(mut all) = catalogs().lock() {
        all.insert(exe.to_path_buf(), catalog);
    }
}

fn catalog_levels(exe: &Path, model: &str) -> Option<Vec<&'static str>> {
    let all = catalogs().lock().ok()?;
    let catalog = all.get(exe)?;
    let base = model.strip_suffix("[1m]").unwrap_or(model);
    catalog
        .get(model)
        .or_else(|| catalog.get(base))
        .cloned()
        .flatten()
}

fn requested_level(reasoning: ReasoningLevel) -> Option<&'static str> {
    Some(match reasoning {
        ReasoningLevel::Ultrathink => return None,
        ReasoningLevel::Minimal | ReasoningLevel::Low => "low",
        ReasoningLevel::Medium => "medium",
        ReasoningLevel::High => "high",
        ReasoningLevel::XHigh | ReasoningLevel::Ultracode => "xhigh",
        ReasoningLevel::Max | ReasoningLevel::Ultra => "max",
    })
}

/// The `--effort` value for a run. With a discovered ladder for the model,
/// the ladder decides (the nearest supported level, preferring higher like the
/// CLI's own xhigh→max; nothing when the model has no ladder). Without one,
/// the historical name heuristic applies unchanged.
pub(crate) fn effort_flag(
    exe: &Path,
    reasoning: Option<ReasoningLevel>,
    model: Option<&str>,
) -> Option<&'static str> {
    let fallback = to_effort(reasoning, model);
    let (Some(level), Some(model)) = (reasoning.and_then(requested_level), model) else {
        return fallback;
    };
    let Some(ladder) = catalog_levels(exe, model) else {
        return fallback;
    };
    if ladder.contains(&level) {
        return Some(level);
    }
    let rank = LEVELS.iter().position(|l| *l == level).unwrap_or(0);
    LEVELS[rank..]
        .iter()
        .find(|l| ladder.contains(*l))
        .or_else(|| LEVELS[..rank].iter().rev().find(|l| ladder.contains(*l)))
        .copied()
}

// ---------------------------------------------------------------------------
// Staged live switches (C1–C5)
// ---------------------------------------------------------------------------

/// What one warm steer must change before its prompt is written.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PlannedSwitch {
    pub set_model: Option<String>,
    pub settings: serde_json::Map<String, Value>,
    pub expect_model: Option<String>,
    pub expect_effort: Option<String>,
    pub reasoning: Option<ReasoningLevel>,
}

impl PlannedSwitch {
    pub fn needs_native_change(&self) -> bool {
        self.set_model.is_some() || !self.settings.is_empty()
    }
}

#[derive(Default)]
pub(crate) struct StagedSwitches {
    order: Vec<String>,
    plans: HashMap<String, PlannedSwitch>,
}

pub(crate) type SharedStaged = Arc<Mutex<StagedSwitches>>;

pub(crate) fn take_staged(
    staged: &SharedStaged,
    message_id: Option<&str>,
) -> Option<PlannedSwitch> {
    let id = message_id?;
    let mut staged = staged.lock().ok()?;
    staged.order.retain(|existing| existing != id);
    staged.plans.remove(id)
}

/// Plan `from` → `to` for a warm run, or `None` when only a restart can apply
/// it (C3/C4): ultrathink/ultracode on either side, a reset to the default
/// model, an effort that would have to be cleared, or any other option change.
pub(crate) fn plan_switch(
    exe: &Path,
    from: &LiveRuntimeSelection,
    to: &LiveRuntimeSelection,
) -> Option<PlannedSwitch> {
    let special = |level: Option<ReasoningLevel>| {
        matches!(
            level,
            Some(ReasoningLevel::Ultrathink | ReasoningLevel::Ultracode)
        )
    };
    if special(from.reasoning) || special(to.reasoning) {
        return None;
    }
    let others = |options: &serde_json::Map<String, Value>| {
        options
            .iter()
            .filter(|(key, _)| !SWITCHABLE_OPTIONS.contains(&key.as_str()))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<serde_json::Map<_, _>>()
    };
    if others(&from.model_options) != others(&to.model_options) {
        return None;
    }
    let from_model = model_flag(from.model.as_deref(), &from.model_options);
    let to_model = model_flag(to.model.as_deref(), &to.model_options);
    if to_model.is_none() && from_model.is_some() {
        return None;
    }
    let from_effort = effort_flag(exe, from.reasoning, from_model.as_deref());
    let to_effort = effort_flag(exe, to.reasoning, to_model.as_deref());
    if to_effort.is_none() && from_effort.is_some() {
        return None;
    }
    let mut settings = serde_json::Map::new();
    if to_effort != from_effort
        && let Some(effort) = to_effort
    {
        settings.insert("effortLevel".into(), Value::from(effort));
    }
    for (option, key) in [
        ("fastMode", "fastMode"),
        ("thinking", "alwaysThinkingEnabled"),
    ] {
        let (before, after) = (
            option_is_on(&from.model_options, option),
            option_is_on(&to.model_options, option),
        );
        if before != after {
            settings.insert(key.into(), Value::Bool(after));
        }
    }
    Some(PlannedSwitch {
        set_model: (to_model != from_model).then(|| to_model.clone()).flatten(),
        settings,
        expect_model: to_model,
        expect_effort: to_effort.map(str::to_owned),
        reasoning: to.reasoning,
    })
}

pub(crate) fn stage(
    staged: &SharedStaged,
    exe: &Path,
    message_id: &str,
    from: &LiveRuntimeSelection,
    to: &LiveRuntimeSelection,
) -> bool {
    let Some(plan) = plan_switch(exe, from, to) else {
        return false;
    };
    let Ok(mut staged) = staged.lock() else {
        return false;
    };
    staged.order.retain(|existing| existing != message_id);
    staged.order.push(message_id.to_owned());
    staged.plans.insert(message_id.to_owned(), plan);
    while staged.order.len() > MAX_STAGED {
        let old = staged.order.remove(0);
        staged.plans.remove(&old);
    }
    true
}

// ---------------------------------------------------------------------------
// Control request correlation + readback
// ---------------------------------------------------------------------------

#[derive(Default)]
pub(crate) struct ControlIds {
    next: u64,
}

impl ControlIds {
    pub fn mint(&mut self, label: &str) -> String {
        self.next += 1;
        format!("{ID_PREFIX}{label}-{}", self.next)
    }
}

pub(crate) fn request_line(request_id: &str, request: Value) -> String {
    json!({
        "type": "control_request",
        "request_id": request_id,
        "request": request,
    })
    .to_string()
}

pub(crate) fn set_model_request(model: &str) -> Value {
    json!({ "subtype": "set_model", "model": model })
}

pub(crate) fn apply_settings_request(settings: &serde_json::Map<String, Value>) -> Value {
    json!({ "subtype": "apply_flag_settings", "settings": settings })
}

pub(crate) fn get_settings_request() -> Value {
    json!({ "subtype": "get_settings" })
}

/// One `control_response` addressed to an id this module minted.
#[derive(Debug)]
pub(crate) struct ControlReply {
    pub request_id: String,
    pub ok: bool,
    pub error: Option<String>,
    pub body: Value,
}

impl ControlReply {
    pub fn parse(frame: &Value) -> Option<Self> {
        let response = frame.get("response")?;
        let request_id = response.get("request_id")?.as_str()?;
        if !request_id.starts_with(ID_PREFIX) {
            return None;
        }
        let ok = response.get("subtype").and_then(Value::as_str) == Some("success");
        Some(Self {
            request_id: request_id.to_owned(),
            ok,
            error: (!ok).then(|| {
                let message = response
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("control request failed");
                match response.get("error_code").and_then(Value::as_str) {
                    Some(code) => format!("{message} ({code})"),
                    None => message.to_owned(),
                }
            }),
            body: response.get("response").cloned().unwrap_or(Value::Null),
        })
    }
}

fn base_model(model: &str) -> &str {
    model.strip_suffix("[1m]").unwrap_or(model)
}

/// Compare a `get_settings` body with what LUNA requested. An alias request
/// (`opus`, `haiku`) resolves inside the CLI, so only a full model id is
/// compared; a requested effort must be applied exactly.
pub(crate) fn readback(
    source: RuntimeReadbackSource,
    session_id: Option<String>,
    requested_model: Option<String>,
    requested_effort: Option<String>,
    reply: Option<&ControlReply>,
) -> RuntimeReadback {
    let applied = reply
        .filter(|reply| reply.ok)
        .and_then(|reply| reply.body.get("applied"));
    let effective_model = applied
        .and_then(|applied| applied.get("model"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let effective_effort = applied
        .and_then(|applied| applied.get("effort"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let mut problems = Vec::new();
    if applied.is_none() {
        problems.push(match reply {
            Some(reply) if !reply.ok => format!(
                "settings readback failed: {}",
                reply.error.as_deref().unwrap_or("unknown error")
            ),
            Some(_) => "settings readback had no applied block".to_owned(),
            None => "settings readback unavailable".to_owned(),
        });
    } else {
        if let Some(requested) = requested_model.as_deref()
            && requested.starts_with("claude-")
            && effective_model.as_deref().map(base_model) != Some(base_model(requested))
        {
            problems.push(format!(
                "model {requested} requested, {} applied",
                effective_model.as_deref().unwrap_or("none")
            ));
        }
        if let Some(requested) = requested_effort.as_deref()
            && effective_effort.as_deref() != Some(requested)
        {
            problems.push(format!(
                "effort {requested} requested, {} applied",
                effective_effort.as_deref().unwrap_or("none")
            ));
        }
    }
    RuntimeReadback {
        provider: UsageProvider::Claude,
        native_session_id: session_id,
        observed_at_ms: super::accounting::now_ms(),
        source,
        requested_model,
        effective_model,
        requested_effort,
        effective_effort,
        matches: problems.is_empty(),
        detail: (!problems.is_empty()).then(|| problems.join("; ")),
    }
}
