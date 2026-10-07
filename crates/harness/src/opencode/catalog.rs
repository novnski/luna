//! Discovery: the provider/model catalog, reasoning variants, slash
//! commands and the session model switch.

use super::*;

/// Preference-ordered variant ids for a requested effort (opencode's
/// reasoning variants: models.dev metadata or `variants` in opencode.json).
pub(super) fn variant_candidates(reasoning: Option<ReasoningLevel>) -> Vec<&'static str> {
    let Some(level) = reasoning else {
        return Vec::new();
    };
    match level {
        ReasoningLevel::Minimal => vec!["minimal", "low"],
        ReasoningLevel::Low => vec!["low", "minimal"],
        ReasoningLevel::Medium => vec!["medium"],
        ReasoningLevel::High => vec!["high"],
        ReasoningLevel::XHigh => vec!["xhigh", "x-high", "high"],
        ReasoningLevel::Max => vec!["max", "xhigh", "high"],
        ReasoningLevel::Ultra | ReasoningLevel::Ultracode | ReasoningLevel::Ultrathink => {
            vec!["ultra", "max", "high"]
        }
    }
}

pub(super) fn variant_to_level(id: &str) -> Option<ReasoningLevel> {
    match id {
        "minimal" => Some(ReasoningLevel::Minimal),
        "low" => Some(ReasoningLevel::Low),
        "medium" => Some(ReasoningLevel::Medium),
        "high" => Some(ReasoningLevel::High),
        "xhigh" | "x-high" => Some(ReasoningLevel::XHigh),
        "max" => Some(ReasoningLevel::Max),
        _ => None,
    }
}

impl Server {
    /// Provider catalog for the picker, variant picking, and context
    /// windows. V1 advertises it on `/provider`; 2.x splits it into a flat
    /// enabled-model list on `/api/model` that folds into the same shape.
    pub(super) async fn provider_catalog(
        &self,
        directory: Option<&str>,
    ) -> Result<ProviderCatalog, HarnessError> {
        match self.protocol().await {
            Protocol::V1 => self.get("/provider", directory).await,
            Protocol::V2 => {
                // The 2.x catalog syncs from models.dev shortly after the
                // health endpoint opens: an empty list right then is a race,
                // not a fact — poll briefly before believing it.
                for attempt in 0..5 {
                    let list: V2ModelList = self.get("/api/model", directory).await?;
                    if !list.data.is_empty() || attempt == 4 {
                        return Ok(catalog_from_v2_models(list.data));
                    }
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
                unreachable!("loop returns on the last attempt")
            }
        }
    }

    /// Slash commands; both wires carry `{name, description}` entries (2.x
    /// wrapped in `{data}`).
    pub(super) async fn commands_wire(
        &self,
        directory: Option<&str>,
    ) -> Result<Value, HarnessError> {
        let path = match self.protocol().await {
            Protocol::V1 => "/command",
            Protocol::V2 => "/api/command",
        };
        let raw = self.get_json(path, directory).await?;
        Ok(unwrap_data(raw))
    }

    /// 2.x: model and reasoning variant ride the SESSION (set once per run),
    /// not the prompt.
    pub(super) async fn set_model(
        &self,
        session_id: &str,
        provider: &str,
        model: &str,
        variant: Option<&str>,
        directory: Option<&str>,
    ) -> Result<(), HarnessError> {
        let mut model_ref = json!({ "providerID": provider, "id": model });
        if let Some(variant) = variant {
            model_ref["variant"] = json!(variant);
        }
        let path = format!("/api/session/{session_id}/model");
        self.post_json(&path, directory, &json!({ "model": model_ref }))
            .await
            .map(|_| ())
    }
}

/// `GET /provider` → picker models: `{providerID}/{modelID}` ids, effort
/// ladder from the model's reasoning variants.
///
/// Only retain what the picker and run setup consume. `/provider` includes
/// the entire models.dev catalog, with large nested capabilities/config maps.
/// Building and cloning a Value for it measured a 112 MB engine heap peak.
/// Serde skips unknown fields and variant bodies without allocating trees.
#[derive(Debug, Default, serde::Deserialize)]
pub(super) struct ProviderCatalog {
    pub(super) all: Option<Vec<Provider>>,
    pub(super) connected: Option<Vec<String>>,
}

#[derive(Debug, serde::Deserialize)]
pub(super) struct Provider {
    pub(super) id: Option<String>,
    pub(super) name: Option<String>,
    pub(super) models: Option<std::collections::BTreeMap<String, ProviderModel>>,
}

#[derive(Debug, serde::Deserialize)]
pub(super) struct ProviderModel {
    #[serde(default)]
    pub(super) limit: ProviderLimit,
    pub(super) name: Option<String>,
    pub(super) variants: Option<std::collections::BTreeMap<String, serde::de::IgnoredAny>>,
}

#[derive(Debug, Default, serde::Deserialize)]
pub(super) struct ProviderLimit {
    pub(super) context: Option<u64>,
}

/// Filtered to CONNECTED providers: `all` is the entire models.dev catalog
/// (measured live: 194 providers / 7,203 models, nearly all needing an API
/// key the user hasn't set) — offering it verbatim made the picker slow to
/// open, slow to scroll, and full of models every run of which fails with
/// "Model not found" (field report, v0.2.21). `connected` names exactly
/// the usable set (credentialed + config-declared + the anonymous Zen
/// tier). An absent/empty `connected` (older server) falls back to `all`.
pub(super) fn models_from_providers(providers: &ProviderCatalog) -> Vec<Model> {
    let connected: std::collections::HashSet<&str> = providers
        .connected
        .iter()
        .flatten()
        .map(String::as_str)
        .collect();
    let mut out = Vec::new();
    for provider in providers.all.iter().flatten() {
        let Some(provider_id) = provider.id.as_deref() else {
            continue;
        };
        if !connected.is_empty() && !connected.contains(provider_id) {
            continue;
        }
        let provider_name = provider.name.as_deref().unwrap_or(provider_id);
        let Some(models) = &provider.models else {
            continue;
        };
        let mut provider_models: Vec<Model> = models
            .iter()
            .map(|(model_id, model)| {
                let label = model.name.as_deref().unwrap_or(model_id).to_owned();
                let mut levels: Vec<ReasoningLevel> = model
                    .variants
                    .as_ref()
                    .map(|variants| {
                        variants
                            .keys()
                            .filter_map(|k| variant_to_level(k))
                            .collect()
                    })
                    .unwrap_or_default();
                levels.sort();
                levels.dedup();
                Model {
                    id: format!("{provider_id}/{model_id}"),
                    label,
                    description: Some(provider_name.to_owned()),
                    reasoning_levels: levels,
                    options: Vec::new(),
                    first_seen_at_ms: None,
                }
            })
            .collect();
        provider_models.sort_by(|a, b| a.label.cmp(&b.label));
        out.extend(provider_models);
    }
    out
}

pub(super) fn commands_from_wire(commands: &Value) -> Vec<SlashCommand> {
    commands
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|c| {
                    let name = c.get("name").and_then(Value::as_str)?;
                    Some(SlashCommand {
                        name: name.to_owned(),
                        description: c
                            .get("description")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        input_hint: None,
                        kind: Default::default(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `GET /api/model` (2.x): `{"location": .., "data": [Model.Info]}`.
#[derive(Debug, serde::Deserialize)]
pub(super) struct V2ModelList {
    #[serde(default)]
    pub(super) data: Vec<V2Model>,
}

#[derive(Debug, serde::Deserialize)]
pub(super) struct V2Model {
    #[serde(rename = "providerID")]
    pub(super) provider_id: String,
    pub(super) id: String,
    pub(super) name: Option<String>,
    #[serde(default)]
    pub(super) limit: ProviderLimit,
    #[serde(default)]
    pub(super) variants: Vec<V2Variant>,
    pub(super) enabled: Option<bool>,
}

/// 2.x carries each variant as `{id, settings}` (settings ignored).
#[derive(Debug, serde::Deserialize)]
pub(super) struct V2Variant {
    pub(super) id: String,
}

/// 2.x's flat model list folds into the 1.x provider-catalog shape, so
/// variant picking and context windows stay wire-agnostic downstream.
/// Providers keep their id as display name (`/api/model` names no
/// providers).
pub(super) fn catalog_from_v2_models(models: Vec<V2Model>) -> ProviderCatalog {
    let mut order: Vec<String> = Vec::new();
    let mut grouped: HashMap<String, Vec<(String, ProviderModel)>> = HashMap::new();
    for model in models {
        if model.enabled == Some(false) {
            continue;
        }
        let provider_id = model.provider_id.clone();
        let entry = grouped.entry(provider_id.clone()).or_insert_with(|| {
            order.push(provider_id);
            Vec::new()
        });
        entry.push((
            model.id.clone(),
            ProviderModel {
                limit: model.limit,
                name: model.name,
                variants: Some(
                    model
                        .variants
                        .into_iter()
                        .map(|v| (v.id, serde::de::IgnoredAny))
                        .collect(),
                ),
            },
        ));
    }
    let all = order
        .into_iter()
        .map(|id| {
            let mut models = grouped.remove(&id).unwrap_or_default();
            models.sort_by(|a, b| a.0.cmp(&b.0));
            Provider {
                id: Some(id.clone()),
                name: Some(id),
                models: Some(models.into_iter().collect()),
            }
        })
        .collect();
    ProviderCatalog {
        all: Some(all),
        connected: None,
    }
}

/// The requested effort as a variant id the model actually advertises.
pub(super) fn pick_variant(
    providers: &ProviderCatalog,
    provider_id: &str,
    model_id: &str,
    reasoning: Option<ReasoningLevel>,
) -> Option<String> {
    let candidates = variant_candidates(reasoning);
    if candidates.is_empty() {
        return None;
    }
    let variants = providers
        .all
        .as_ref()?
        .iter()
        .find(|p| p.id.as_deref() == Some(provider_id))?
        .models
        .as_ref()?
        .get(model_id)?
        .variants
        .as_ref()?;
    candidates
        .into_iter()
        .find(|c| variants.contains_key(*c))
        .map(str::to_owned)
}
