//! Discovery: the provider/model catalog, reasoning variants, slash
//! commands and the session model switch.
//!
//! OC-10: the catalog keeps what the shared surfaces need from 2.x
//! `Model.Info` — image input (admission), tool calling, release status,
//! base price, context/output limits — plus provider display names from
//! `GET /api/provider`. Variants the effort ladder cannot express become a
//! `variant` model option; a selected variant the model no longer offers is
//! dropped at run setup, never sent.

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
    /// windows: the flat enabled-model list on `/api/model`, folded into the
    /// provider-grouped shape the run setup consumes.
    pub(super) async fn provider_catalog(
        &self,
        directory: Option<&str>,
    ) -> Result<ProviderCatalog, HarnessError> {
        // The catalog syncs from models.dev shortly after boot: an empty list
        // (or a 503 `ServiceUnavailableError`) right then is a race, not a
        // fact — poll briefly before believing it. The sync usually lands
        // within a second, so the first retries are short (8 s in total, as
        // before).
        const BACKOFF_MS: [u64; 7] = [250, 250, 500, 1000, 2000, 2000, 2000];
        for attempt in 0..=BACKOFF_MS.len() {
            let last = attempt == BACKOFF_MS.len();
            match self.get::<V2ModelList>("/api/model", directory).await {
                Ok(list) if !list.data.is_empty() || last => {
                    let mut catalog = catalog_from_v2_models(list.data);
                    self.name_providers(&mut catalog, directory).await;
                    remember_catalog_capabilities(&catalog);
                    return Ok(catalog);
                }
                Ok(_) => {}
                Err(error) if !last && error.to_string().contains(" 503 ") => {}
                Err(error) => return Err(error),
            }
            tokio::time::sleep(Duration::from_millis(BACKOFF_MS[attempt])).await;
        }
        unreachable!("loop returns on the last attempt")
    }

    /// One catalog read without the boot-race polling: a mid-run refresh
    /// after `model.updated`/`provider.updated`/`integration.updated`/
    /// `models-dev.refreshed` must never stall the event loop.
    pub(super) async fn provider_catalog_once(
        &self,
        directory: Option<&str>,
    ) -> Result<ProviderCatalog, HarnessError> {
        let list = self.get::<V2ModelList>("/api/model", directory).await?;
        let mut catalog = catalog_from_v2_models(list.data);
        self.name_providers(&mut catalog, directory).await;
        remember_catalog_capabilities(&catalog);
        Ok(catalog)
    }

    /// Provider display names from `GET /api/provider` (`/api/model` names
    /// none). Best effort: the provider id stays the label on failure.
    async fn name_providers(&self, catalog: &mut ProviderCatalog, directory: Option<&str>) {
        let Ok(list) = self.get::<V2ProviderList>("/api/provider", directory).await else {
            return;
        };
        let names: HashMap<String, String> = list
            .data
            .into_iter()
            .filter(|provider| !provider.name.trim().is_empty())
            .map(|provider| (provider.id, provider.name))
            .collect();
        for provider in catalog.all.iter_mut().flatten() {
            if let Some(name) = provider.id.as_ref().and_then(|id| names.get(id)) {
                provider.name = Some(name.clone());
            }
        }
    }

    /// Slash commands: `GET /api/command` → `{data: [{name, description?}]}`.
    pub(super) async fn commands_wire(
        &self,
        directory: Option<&str>,
    ) -> Result<Value, HarnessError> {
        let raw = self.get_json("/api/command", directory).await?;
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
    /// 2.x facts kept from `/api/model` (never decoded from the 1.x shape,
    /// whose `capabilities` object differs): `capabilities.input` has
    /// `image`, tool calling, release status and the base price.
    #[serde(skip)]
    pub(super) facts: ModelFacts,
}

/// What LUNA keeps of a 2.x `Model.Info` beyond id, name, limits and variants.
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct ModelFacts {
    /// `capabilities.input` contains `image`; `None` when not reported.
    pub(super) image_input: Option<bool>,
    pub(super) tools: Option<bool>,
    /// `alpha` / `beta` / `deprecated` / `active`.
    pub(super) status: Option<String>,
    /// USD per million input/output tokens of the untiered price, when
    /// reported (`0` for free models).
    pub(super) cost: Option<(f64, f64)>,
}

#[derive(Debug, Default, serde::Deserialize)]
pub(super) struct ProviderLimit {
    pub(super) context: Option<u64>,
    #[serde(default)]
    pub(super) output: Option<u64>,
}

/// The arbitrary-variant model option (variants beyond the effort ladder,
/// e.g. `thinking` or a custom `opencode.json` variant). Its choice wins over
/// the effort-derived variant when the model still advertises it.
pub(super) const VARIANT_OPTION: &str = "variant";
pub(super) const DEFAULT_VARIANT_CHOICE: &str = "default";

/// Variants the effort ladder cannot express, as a picker option.
fn variant_option(model: &ProviderModel) -> Option<luna_proto::ModelOption> {
    let extra: Vec<&String> = model
        .variants
        .iter()
        .flat_map(|variants| variants.keys())
        .filter(|id| variant_to_level(id).is_none())
        .collect();
    if extra.is_empty() {
        return None;
    }
    let mut choices = vec![luna_proto::ModelOptionChoice {
        id: DEFAULT_VARIANT_CHOICE.into(),
        label: "Default".into(),
    }];
    choices.extend(extra.into_iter().map(|id| luna_proto::ModelOptionChoice {
        id: id.clone(),
        label: id.clone(),
    }));
    Some(luna_proto::ModelOption {
        id: VARIANT_OPTION.into(),
        label: "Variant".into(),
        choices,
        default_choice: DEFAULT_VARIANT_CHOICE.into(),
    })
}

/// Picker description: the provider's display name, plus the release
/// status when the model is not generally available.
fn model_description(provider_name: &str, model: &ProviderModel) -> String {
    match model.facts.status.as_deref() {
        Some(status @ ("alpha" | "beta" | "deprecated")) => format!("{provider_name} · {status}"),
        _ => provider_name.to_owned(),
    }
}

/// Feed the process-wide image-capability table (engine admission reads it
/// before any server boots).
pub(super) fn remember_catalog_capabilities(catalog: &ProviderCatalog) {
    let entries: Vec<(String, bool)> = catalog
        .all
        .iter()
        .flatten()
        .filter_map(|provider| Some((provider.id.as_deref()?, provider.models.as_ref()?)))
        .flat_map(|(provider_id, models)| {
            models.iter().filter_map(move |(model_id, model)| {
                Some((
                    format!("{provider_id}/{model_id}"),
                    model.facts.image_input?,
                ))
            })
        })
        .collect();
    remember_image_capabilities(entries);
}

/// One model of the folded catalog.
pub(super) fn catalog_model<'a>(
    providers: &'a ProviderCatalog,
    provider_id: &str,
    model_id: &str,
) -> Option<&'a ProviderModel> {
    providers
        .all
        .as_ref()?
        .iter()
        .find(|p| p.id.as_deref() == Some(provider_id))?
        .models
        .as_ref()?
        .get(model_id)
}

/// The variant a run asks for, and why a requested one was dropped: the
/// arbitrary-variant option when the model still advertises it, else the
/// effort ladder (OCF-101: a stale selection never reaches the server).
pub(super) fn select_variant(
    providers: &ProviderCatalog,
    provider_id: &str,
    model_id: &str,
    reasoning: Option<ReasoningLevel>,
    options: &serde_json::Map<String, Value>,
) -> (Option<String>, Option<String>) {
    let explicit = options
        .get(VARIANT_OPTION)
        .and_then(Value::as_str)
        .filter(|choice| !choice.is_empty() && *choice != DEFAULT_VARIANT_CHOICE);
    let mut dropped = None;
    if let Some(choice) = explicit {
        let offered = catalog_model(providers, provider_id, model_id)
            .and_then(|model| model.variants.as_ref())
            .is_some_and(|variants| variants.contains_key(choice));
        if offered {
            return (Some(choice.to_owned()), None);
        }
        dropped = Some(format!(
            "variant \"{choice}\" is not offered by {provider_id}/{model_id}; the default was used"
        ));
    }
    (
        pick_variant(providers, provider_id, model_id, reasoning),
        dropped,
    )
}

/// `GET /api/provider` → `{data: [Provider.Info]}`; only id and name kept.
#[derive(Debug, serde::Deserialize)]
pub(super) struct V2ProviderList {
    #[serde(default)]
    pub(super) data: Vec<V2Provider>,
}

#[derive(Debug, serde::Deserialize)]
pub(super) struct V2Provider {
    pub(super) id: String,
    #[serde(default)]
    pub(super) name: String,
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
                    description: Some(model_description(provider_name, model)),
                    reasoning_levels: levels,
                    options: variant_option(model).into_iter().collect(),
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
    #[serde(default)]
    pub(super) capabilities: Option<V2Capabilities>,
    #[serde(default)]
    pub(super) cost: Vec<V2Cost>,
    #[serde(default)]
    pub(super) status: Option<String>,
}

/// 2.x carries each variant as `{id, settings}` (settings ignored).
#[derive(Debug, serde::Deserialize)]
pub(super) struct V2Variant {
    pub(super) id: String,
}

/// `Model.Capabilities`: `{tools, input: [..], output: [..]}`.
#[derive(Debug, serde::Deserialize)]
pub(super) struct V2Capabilities {
    #[serde(default)]
    pub(super) tools: Option<bool>,
    #[serde(default)]
    pub(super) input: Vec<String>,
}

/// One `Model.Cost` row (USD per million tokens); a `tier` row applies
/// only above a context size, so the untiered row is the base price.
#[derive(Debug, serde::Deserialize)]
pub(super) struct V2Cost {
    #[serde(default)]
    pub(super) tier: Option<serde::de::IgnoredAny>,
    #[serde(default)]
    pub(super) input: Option<f64>,
    #[serde(default)]
    pub(super) output: Option<f64>,
}

fn v2_facts(model: &V2Model) -> ModelFacts {
    ModelFacts {
        image_input: model
            .capabilities
            .as_ref()
            .map(|caps| caps.input.iter().any(|kind| kind == "image")),
        tools: model.capabilities.as_ref().and_then(|caps| caps.tools),
        status: model.status.clone(),
        cost: model
            .cost
            .iter()
            .find(|row| row.tier.is_none())
            .and_then(|row| Some((row.input?, row.output?))),
    }
}

/// 2.x's flat model list folds into the 1.x provider-catalog shape, so
/// variant picking and context windows stay wire-agnostic downstream.
/// Providers keep their id as display name here; `GET /api/provider`
/// supplies the real names afterwards (`name_providers`).
pub(super) fn catalog_from_v2_models(models: Vec<V2Model>) -> ProviderCatalog {
    let mut order: Vec<String> = Vec::new();
    let mut grouped: HashMap<String, Vec<(String, ProviderModel)>> = HashMap::new();
    for model in models {
        if model.enabled == Some(false) {
            continue;
        }
        let provider_id = model.provider_id.clone();
        let facts = v2_facts(&model);
        let entry = grouped.entry(provider_id.clone()).or_insert_with(|| {
            order.push(provider_id);
            Vec::new()
        });
        entry.push((
            model.id.clone(),
            ProviderModel {
                limit: model.limit,
                name: model.name,
                facts,
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
