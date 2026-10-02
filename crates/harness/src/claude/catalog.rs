//! Model catalog + effort mapping for Claude Code, ported from luna's
//! `packages/harness/src/claude.ts` (which itself mirrors Claude Code's own
//! picker via t3code's catalog).
//!
//! The TS harness discovers models at runtime through the SDK's
//! `supportedModels()` control request and then OVERLAYS these static effort
//! ladders / option sets (the SDK under-reports both). Until we grow a
//! short-lived control-channel discovery session, [`static_models`] returns the
//! curated list directly; `ClaudeHarness::models` is the single seam where
//! dynamic discovery can later be spliced in.

use std::collections::HashSet;

use luna_proto::{Model, ModelOption, ModelOptionChoice, ReasoningLevel};
use serde_json::Value;

/// The ultrathink directive rides every user message as a prompt prefix — that
/// is how the mode actually works in Claude Code (a prompt convention, not an
/// effort flag). Applied to the initial prompt AND every steer.
pub(crate) const ULTRATHINK_PREFIX: &str = "Ultrathink:\n";

pub(crate) fn apply_ultrathink(reasoning: Option<ReasoningLevel>, text: &str) -> String {
    // A prefix would break Claude's peer classification of LUNA's envelope.
    if reasoning == Some(ReasoningLevel::Ultrathink) && !super::peer::is_luna_peer_envelope(text) {
        format!("{ULTRATHINK_PREFIX}{text}")
    } else {
        text.to_owned()
    }
}

fn contains_any(hay: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| hay.contains(n))
}

/// Models whose CLI accepts `xhigh` natively; elsewhere it clamps to `max`
/// (mirroring Claude Code's own normalization). Substring port of claude.ts's
/// `/fable-5|opus-4-[7-9]|opus-[5-9]|sonnet-[5-9]/`.
pub(crate) fn supports_xhigh(model: &str) -> bool {
    contains_any(
        model,
        &[
            "fable-5", "opus-4-7", "opus-4-8", "opus-4-9", "opus-5", "opus-6", "opus-7", "opus-8",
            "opus-9", "sonnet-5", "sonnet-6", "sonnet-7", "sonnet-8", "sonnet-9",
        ],
    )
}

/// Map the unified level to the `--effort` flag value the CLI accepts for this
/// model. The special modes don't translate directly: `ultrathink` is a prompt
/// prefix (no flag), `ultracode` runs as `xhigh` plus the ultracode setting,
/// and `ultra` is a Codex-only tier (Claude tops out at `max`).
pub(crate) fn to_effort(
    reasoning: Option<ReasoningLevel>,
    model: Option<&str>,
) -> Option<&'static str> {
    let base = match reasoning? {
        ReasoningLevel::Ultrathink => return None,
        ReasoningLevel::Minimal | ReasoningLevel::Low => "low",
        ReasoningLevel::Medium => "medium",
        ReasoningLevel::High => "high",
        ReasoningLevel::XHigh | ReasoningLevel::Ultracode => "xhigh",
        ReasoningLevel::Max | ReasoningLevel::Ultra => "max",
    };
    if base == "xhigh" && !model.is_some_and(supports_xhigh) {
        return Some("max");
    }
    Some(base)
}

/// A boolean toggle rendered as an off/on select (the Rust `ModelOption` wire
/// type has no dedicated boolean kind).
fn toggle(id: &str, label: &str) -> ModelOption {
    ModelOption {
        id: id.into(),
        label: label.into(),
        choices: vec![
            ModelOptionChoice {
                id: "off".into(),
                label: "Off".into(),
            },
            ModelOptionChoice {
                id: "on".into(),
                label: "On".into(),
            },
        ],
        default_choice: "off".into(),
    }
}

/// Fast mode as the shared "Speed" select (Standard / Fast), named like the
/// Codex service tier. The choice ids stay the CLI toggle's `off`/`on`, which
/// persisted picks and the launch flags already use.
pub(crate) fn speed() -> ModelOption {
    ModelOption {
        id: "fastMode".into(),
        label: "Speed".into(),
        choices: vec![
            ModelOptionChoice {
                id: "off".into(),
                label: "Standard".into(),
            },
            ModelOptionChoice {
                id: "on".into(),
                label: "Fast".into(),
            },
        ],
        default_choice: "off".into(),
    }
}

/// The 200K/1M context-window select carried by the long-context models. The
/// 1M window is selected via a model-id suffix (`<model>[1m]`), exactly how the
/// CLI itself does it.
pub(crate) fn context_window() -> ModelOption {
    ModelOption {
        id: "contextWindow".into(),
        label: "Context Window".into(),
        choices: vec![
            ModelOptionChoice {
                id: "200k".into(),
                label: "200K".into(),
            },
            ModelOptionChoice {
                id: "1m".into(),
                label: "1M".into(),
            },
        ],
        default_choice: "200k".into(),
    }
}

const FULL_LADDER: &[ReasoningLevel] = &[
    ReasoningLevel::Low,
    ReasoningLevel::Medium,
    ReasoningLevel::High,
    ReasoningLevel::XHigh,
    ReasoningLevel::Max,
    ReasoningLevel::Ultracode,
    ReasoningLevel::Ultrathink,
];

/// opus-4-7 / sonnet-5+ tier (claude.ts `claudeEffortsFor`): xhigh native,
/// no ultracode.
const XHIGH_LADDER: &[ReasoningLevel] = &[
    ReasoningLevel::Low,
    ReasoningLevel::Medium,
    ReasoningLevel::High,
    ReasoningLevel::XHigh,
    ReasoningLevel::Max,
    ReasoningLevel::Ultrathink,
];

fn model(
    id: &str,
    label: &str,
    description: &str,
    ladder: &[ReasoningLevel],
    options: Vec<ModelOption>,
) -> Model {
    Model {
        id: id.into(),
        label: label.into(),
        description: (!description.is_empty()).then(|| description.into()),
        reasoning_levels: ladder.to_vec(),
        options,
    }
}

/// The curated model list, mirroring claude.ts's `claudeEffortsFor` /
/// `claudeOptionsFor` ladders: full ladder (through ultracode/ultrathink) on
/// Fable 5, `max`-topped ladders on Opus/Sonnet, no efforts but a thinking
/// toggle on Haiku; context-window select on the long-context families and
/// fast mode on Opus 4.5+.
///
/// `pub`: besides the discovery-side enrichment here, the UI's display-side
/// normalization borrows these labels so alias rows served by older engines
/// still read with their version numbers ("Opus 5", not "Opus").
pub fn static_models() -> Vec<Model> {
    vec![
        model(
            "claude-fable-5-1",
            "Fable 5.1",
            "Most intelligent model for building agents",
            FULL_LADDER,
            vec![context_window()],
        ),
        model(
            "claude-fable-5",
            "Fable 5",
            "Previous generation Fable",
            FULL_LADDER,
            vec![context_window()],
        ),
        model(
            "claude-opus-5",
            "Opus 5",
            "Powerful model for complex work",
            FULL_LADDER,
            vec![context_window(), speed()],
        ),
        model(
            "claude-opus-4-8",
            "Opus 4.8",
            "Previous generation Opus",
            FULL_LADDER,
            vec![speed()],
        ),
        model(
            "claude-opus-4-7",
            "Opus 4.7",
            "Older generation Opus",
            XHIGH_LADDER,
            vec![speed()],
        ),
        model(
            "claude-sonnet-5",
            "Sonnet 5",
            "Balanced speed and intelligence",
            XHIGH_LADDER,
            vec![context_window()],
        ),
        model(
            "claude-haiku-4-5",
            "Haiku 4.5",
            "Fastest model for everyday tasks",
            &[],
            vec![toggle("thinking", "Thinking")],
        ),
    ]
}

/// Translate the CLI's account-visible `initialize.response.models` list into
/// picker rows. Its aliases may omit older accepted model IDs, so preserve
/// curated rows that are not represented by a live resolved model.
pub(crate) fn models_from_initialize(response: &Value) -> Vec<Model> {
    let Some(entries) = response
        .get("response")
        .and_then(|value| value.get("models"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    let curated = static_models();
    let mut models = Vec::new();
    let mut seen = HashSet::new();
    for entry in entries {
        let Some(value) = entry.get("value").and_then(Value::as_str).map(str::trim) else {
            continue;
        };
        if value.is_empty() || value == "default" {
            continue;
        }
        let resolved = entry
            .get("resolvedModel")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("");
        let id = if value.ends_with("[1m]") && !resolved.ends_with("[1m]") {
            value
        } else if resolved.starts_with("claude-") {
            resolved
        } else {
            value
        };
        if !seen.insert(id.to_owned()) {
            continue;
        }
        let base = id.strip_suffix("[1m]").unwrap_or(id);
        let template = curated.iter().find(|model| model.id == base).or_else(|| {
            let (stem, date) = base.rsplit_once('-')?;
            (date.len() == 8 && date.chars().all(|ch| ch.is_ascii_digit()))
                .then(|| curated.iter().find(|model| model.id == stem))
                .flatten()
        });
        let mut reasoning_levels = entry
            .get("supportedEffortLevels")
            .and_then(Value::as_array)
            .map(|levels| {
                levels
                    .iter()
                    .filter_map(Value::as_str)
                    .filter_map(|level| match level {
                        "low" => Some(ReasoningLevel::Low),
                        "medium" => Some(ReasoningLevel::Medium),
                        "high" => Some(ReasoningLevel::High),
                        "xhigh" => Some(ReasoningLevel::XHigh),
                        "max" => Some(ReasoningLevel::Max),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_else(|| {
                template.map_or_else(Vec::new, |model| model.reasoning_levels.clone())
            });
        if entry.get("supportsEffort").and_then(Value::as_bool) == Some(true)
            && !reasoning_levels.contains(&ReasoningLevel::Ultrathink)
        {
            reasoning_levels.push(ReasoningLevel::Ultrathink);
        }
        if template.is_some_and(|model| model.reasoning_levels.contains(&ReasoningLevel::Ultracode))
            && !reasoning_levels.contains(&ReasoningLevel::Ultracode)
        {
            reasoning_levels.push(ReasoningLevel::Ultracode);
        }
        let mut options = template.map_or_else(Vec::new, |model| model.options.clone());
        if id.ends_with("[1m]") && !options.iter().any(|option| option.id == "contextWindow") {
            options.push(context_window());
        }
        if id.ends_with("[1m]")
            && let Some(option) = options
                .iter_mut()
                .find(|option| option.id == "contextWindow")
        {
            option.default_choice = "1m".into();
        }
        if entry.get("supportsFastMode").and_then(Value::as_bool) == Some(true)
            && !options.iter().any(|option| option.id == "fastMode")
        {
            options.push(speed());
        }
        models.push(Model {
            id: id.to_owned(),
            label: template
                .map(|model| model.label.clone())
                .or_else(|| {
                    entry
                        .get("displayName")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .unwrap_or_else(|| id.to_owned()),
            description: entry
                .get("description")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
                .map(str::to_owned)
                .or_else(|| template.and_then(|model| model.description.clone())),
            reasoning_levels,
            options,
        });
    }
    if models.is_empty() {
        return models;
    }
    for model in curated {
        let represented = seen.iter().any(|id| {
            id == &model.id
                || id.strip_suffix("[1m]") == Some(model.id.as_str())
                || id.strip_prefix(&model.id).is_some_and(|tail| {
                    tail.len() == 9
                        && tail.starts_with('-')
                        && tail[1..].chars().all(|ch| ch.is_ascii_digit())
                })
        });
        if !represented {
            models.push(model);
        }
    }
    models
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effort_maps_special_modes() {
        assert_eq!(to_effort(None, None), None);
        assert_eq!(to_effort(Some(ReasoningLevel::Ultrathink), None), None);
        assert_eq!(
            to_effort(Some(ReasoningLevel::Minimal), Some("claude-fable-5")),
            Some("low")
        );
        assert_eq!(
            to_effort(Some(ReasoningLevel::Ultra), Some("claude-fable-5")),
            Some("max")
        );
        // ultracode -> xhigh where supported…
        assert_eq!(
            to_effort(Some(ReasoningLevel::Ultracode), Some("claude-fable-5")),
            Some("xhigh")
        );
        // …and xhigh clamps to max elsewhere.
        assert_eq!(
            to_effort(Some(ReasoningLevel::XHigh), Some("claude-opus-4-5")),
            Some("max")
        );
        assert_eq!(to_effort(Some(ReasoningLevel::XHigh), None), Some("max"));
    }

    #[test]
    fn xhigh_family_matching() {
        assert!(supports_xhigh("claude-fable-5"));
        assert!(supports_xhigh("claude-fable-5-1"));
        assert!(supports_xhigh("claude-opus-5"));
        assert!(supports_xhigh("claude-opus-5[1m]"));
        assert!(supports_xhigh("claude-opus-4-7-20260101"));
        assert!(!supports_xhigh("claude-opus-4-5"));
        assert!(!supports_xhigh("claude-sonnet-4-5"));
    }

    #[test]
    fn ultrathink_prefixes_prompt() {
        assert_eq!(
            apply_ultrathink(Some(ReasoningLevel::Ultrathink), "do it"),
            "Ultrathink:\ndo it"
        );
        assert_eq!(
            apply_ultrathink(Some(ReasoningLevel::Max), "do it"),
            "do it"
        );
    }
}
