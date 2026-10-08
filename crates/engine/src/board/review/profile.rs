//! Reviewer profile resolution (automatic-review §1): an explicit start
//! override → the ticket's review profile → the project review profile →
//! the implementation model in a separate reviewer side chat. Availability
//! is checked exactly on the target harness/account; an explicitly selected
//! reviewer is never silently replaced, and the observed model is compared
//! at submit time.
use super::*;
use luna_proto::{
    BoardReviewProfile, BoardSettingOrigin, BoardSettingSource, Chat, ResolvedExecution,
    ReviewProfileResolution,
};

/// Snapshot-store key of a project's review profile (JSON
/// `BoardReviewProfile`; absent means "no project reviewer").
const PROJECT_REVIEW_KEY: &str = "board-project-review:";

/// A resolved reviewer, or the structured reason it cannot run (kept with
/// the requested settings so the ticket shows exactly what was asked).
#[derive(Debug, Clone)]
pub(crate) struct ResolvedReviewer {
    pub resolution: ReviewProfileResolution,
    pub failure: Option<(BoardError, BoardAttentionKind)>,
}

/// Provider model ids and readbacks may add a suffix (`opus[1m]`,
/// `gpt-5.5-codex-2026-…`); a different family never matches.
pub(super) fn model_matches(requested: &str, observed: &str) -> bool {
    let requested = requested.trim().to_ascii_lowercase();
    let observed = observed.trim().to_ascii_lowercase();
    observed == requested
        || observed
            .strip_prefix(&requested)
            .is_some_and(|rest| rest.starts_with(['-', '[', '@', ':']))
}

fn explicit(profile: &BoardReviewProfile) -> bool {
    let mut profile = profile.clone();
    profile.disabled = false;
    profile != BoardReviewProfile::default()
}

/// Public model options only (engine-private keys never cross chats).
fn public_options(options: &serde_json::Map<String, Value>) -> serde_json::Map<String, Value> {
    options
        .iter()
        .filter(|(key, _)| !key.starts_with("__") && !key.starts_with("luna"))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

impl EngineRpc {
    pub(crate) fn board_project_review_profile(
        &self,
        project_id: &str,
    ) -> BoardResult<Option<BoardReviewProfile>> {
        let Some(bytes) = self
            .store
            .load_snapshot(&format!("{PROJECT_REVIEW_KEY}{project_id}"))
            .map_err(storage)?
        else {
            return Ok(None);
        };
        Ok(serde_json::from_slice::<Option<BoardReviewProfile>>(&bytes)
            .ok()
            .flatten()
            .filter(explicit))
    }

    /// Save (or clear with `None`) a project's review profile. Shape only;
    /// availability is checked when a review starts.
    pub(crate) fn board_set_project_review_profile(
        &self,
        project_id: &str,
        profile: Option<&BoardReviewProfile>,
    ) -> BoardResult<()> {
        let bytes = serde_json::to_vec(&profile).map_err(storage)?;
        let key = format!("{PROJECT_REVIEW_KEY}{project_id}");
        match self.board_write_permit()? {
            Some(permit) => self
                .store
                .save_snapshot_admitted(permit.ticket(), &key, &bytes)
                .map_err(storage),
            None => self.store.save_snapshot(&key, &bytes).map_err(storage),
        }
    }

    /// Resolve the reviewer for a ticket's current workflow. Never fails:
    /// an unavailable reviewer comes back with `failure` set.
    pub(crate) async fn board_resolve_review_profile(
        &self,
        item: &WorkItem,
        primary: &Chat,
        start_override: Option<&BoardReviewProfile>,
    ) -> ResolvedReviewer {
        let project = self
            .board_project_review_profile(&item.project_id)
            .ok()
            .flatten();
        let (requested, origin) = if let Some(profile) = start_override.filter(|p| explicit(p)) {
            (profile.clone(), BoardSettingOrigin::StartOverride)
        } else if explicit(&item.review) {
            (item.review.clone(), BoardSettingOrigin::Ticket)
        } else if let Some(profile) = project {
            (profile, BoardSettingOrigin::Project)
        } else {
            (
                BoardReviewProfile::default(),
                BoardSettingOrigin::ImplementationFallback,
            )
        };
        let implementation = primary.config.clone();
        let fallback = requested.harness.is_none();
        let source = |field: &str, origin: BoardSettingOrigin| BoardSettingSource {
            field: field.into(),
            source: origin,
        };
        let mut sources = Vec::new();
        let harness = match (requested.harness, implementation.as_ref()) {
            (Some(harness), _) => {
                sources.push(source("harness", origin));
                Some(harness)
            }
            (None, Some(config)) => {
                sources.push(source(
                    "harness",
                    BoardSettingOrigin::ImplementationFallback,
                ));
                Some(config.harness)
            }
            (None, None) => None,
        };
        let from_implementation = fallback.then_some(implementation.as_ref()).flatten();
        let model = match (&requested.model, from_implementation) {
            (Some(model), _) => {
                sources.push(source("model", origin));
                Some(model.clone())
            }
            (None, Some(config)) if config.model.is_some() => {
                sources.push(source("model", BoardSettingOrigin::ImplementationFallback));
                config.model.clone()
            }
            _ => {
                sources.push(source("model", BoardSettingOrigin::CatalogDefault));
                None
            }
        };
        let reasoning = match (requested.reasoning, from_implementation) {
            (Some(reasoning), _) => {
                sources.push(source("reasoning", origin));
                Some(reasoning)
            }
            (None, Some(config)) if config.reasoning.is_some() => {
                sources.push(source(
                    "reasoning",
                    BoardSettingOrigin::ImplementationFallback,
                ));
                config.reasoning
            }
            _ => None,
        };
        let provider_account_id = requested
            .provider_account_id
            .clone()
            .or_else(|| from_implementation.and_then(|config| config.provider_account_id.clone()));
        let model_options = if !requested.model_options.is_empty() {
            requested.model_options.clone()
        } else {
            from_implementation
                .map(|config| public_options(&config.model_options))
                .unwrap_or_default()
        };
        sources.push(source(
            "permissionMode",
            BoardSettingOrigin::ProviderDefault,
        ));
        let resolved = ResolvedExecution {
            harness: harness.unwrap_or(HarnessId::Mock),
            provider_account_id,
            model,
            reasoning,
            model_options,
            // Review-constrained access: approvals for anything beyond reads,
            // and a read-only sandbox on the delivered turn.
            permission_mode: PermissionMode::Ask,
            host_device_id: self.engine_info.device_id.clone(),
            project_id: item.project_id.clone(),
            cwd: primary.cwd.clone().unwrap_or_default(),
            checkout_id: primary.checkout_id.clone(),
            branch: primary.branch.clone(),
            sources,
        };
        let resolution = ReviewProfileResolution {
            requested,
            resolved: resolved.clone(),
            observed_model: None,
            observed_harness: None,
            implementation_fallback: fallback,
        };
        let failure = if harness.is_none() {
            Some((
                BoardError::Unavailable {
                    reason: "No reviewer is configured and the working chat has no implementation provider to fall back to.".into(),
                },
                BoardAttentionKind::ReviewUnavailable,
            ))
        } else {
            self.review_check_available(&resolved).await.err()
        };
        ResolvedReviewer {
            resolution,
            failure,
        }
    }

    /// Exact availability on the target harness/account: registered and
    /// installed, able to run review-constrained (read-only) access, the
    /// requested model in that account's catalog with the requested effort.
    pub(super) async fn review_check_available(
        &self,
        resolved: &ResolvedExecution,
    ) -> Result<(), (BoardError, BoardAttentionKind)> {
        let unavailable = |reason: String| {
            (
                BoardError::Unavailable { reason },
                BoardAttentionKind::ReviewUnavailable,
            )
        };
        let harness = self.registry.resolve(resolved.harness).map_err(|_| {
            (
                BoardError::UnavailableProvider {
                    harness: resolved.harness,
                },
                BoardAttentionKind::ReviewUnavailable,
            )
        })?;
        if !harness.installed() {
            return Err((
                BoardError::UnavailableProvider {
                    harness: resolved.harness,
                },
                BoardAttentionKind::ReviewUnavailable,
            ));
        }
        let capabilities = crate::permissions::provider_capabilities_for(
            resolved.harness,
            resolved.model.as_deref(),
        );
        if !capabilities.supports(PermissionMode::Ask) || !capabilities.supports_workspace_sandbox {
            return Err(unavailable(format!(
                "{} cannot run a review-constrained (read-only) session on this host, so it cannot review.",
                harness.display_name()
            )));
        }
        let Some(model) = resolved.model.as_deref() else {
            return Ok(());
        };
        let models = self
            .model_cache
            .list(
                resolved.harness,
                resolved.provider_account_id.as_deref(),
                false,
            )
            .await
            .map_err(|error| {
                unavailable(format!(
                    "{model} could not be verified on {}: {error}",
                    harness.display_name()
                ))
            })?;
        let Some(found) = models.iter().find(|candidate| candidate.id == model) else {
            return Err((
                BoardError::UnavailableModel {
                    harness: resolved.harness,
                    model: model.into(),
                    supported: models.iter().take(20).map(|m| m.id.clone()).collect(),
                },
                BoardAttentionKind::ModelUnavailable,
            ));
        };
        if let Some(effort) = resolved.reasoning
            && !found.reasoning_levels.is_empty()
            && !found.reasoning_levels.contains(&effort)
        {
            return Err((
                BoardError::UnsupportedEffort {
                    model: model.into(),
                    effort,
                    supported: found.reasoning_levels.clone(),
                },
                BoardAttentionKind::ModelUnavailable,
            ));
        }
        Ok(())
    }
}
