//! Destination and settings resolution for a ticket's first working chat.
//!
//! Precedence (proposal §2 "Execution settings from UI or chat"): explicit
//! Start override → saved ticket profile → compatible same-project parent
//! (side destinations) → project defaults → saved provider defaults →
//! catalog default. Every requested model/effort/option is validated against
//! the real catalog of the target account; nothing unavailable is silently
//! replaced. Access never exceeds the creating parent's or caller's ceiling,
//! and deferred choices stay unresolved until chosen.
use super::*;
use luna_proto::{
    BoardCheckoutChoice, BoardSettingOrigin, BoardSettingSource, Model, ProjectCheckout,
};

/// The concrete place a new worker is created, as resolved for one caller.
#[derive(Debug, Clone)]
pub(crate) struct DestinationPlan {
    /// `NewMain`, `SideOf { main }` or `SiblingOf { side }`.
    pub destination: BoardDestination,
    /// The main chat a side worker is displayed under.
    pub parent_task_id: Option<String>,
    /// The chat the creation request acts from (A3 `caller_task_id`, or the
    /// "handed off from" source of a new main chat).
    pub acting_task_id: Option<String>,
}

pub(crate) struct SettingsResolution {
    pub resolved: ResolvedExecution,
    pub unresolved: Vec<String>,
}

fn source(field: &str, origin: BoardSettingOrigin) -> BoardSettingSource {
    BoardSettingSource {
        field: field.into(),
        source: origin,
    }
}

fn harness_wire(harness: HarnessId) -> String {
    serde_json::to_value(harness)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| format!("{harness:?}"))
}

/// Validate one model choice against its catalog. No fallback: the error
/// names the supported choices.
pub(crate) fn check_model_choice<'a>(
    harness: HarnessId,
    models: &'a [Model],
    model: &str,
) -> BoardResult<&'a Model> {
    models
        .iter()
        .find(|candidate| candidate.id == model)
        .ok_or_else(|| BoardError::UnavailableModel {
            harness,
            model: model.into(),
            supported: models
                .iter()
                .take(50)
                .map(|model| model.id.clone())
                .collect(),
        })
}

pub(crate) fn check_reasoning(model: &Model, reasoning: ReasoningLevel) -> BoardResult<()> {
    if !model.reasoning_levels.is_empty() && !model.reasoning_levels.contains(&reasoning) {
        return Err(BoardError::UnsupportedEffort {
            model: model.id.clone(),
            effort: reasoning,
            supported: model.reasoning_levels.clone(),
        });
    }
    Ok(())
}

pub(crate) fn check_options(
    model: &Model,
    options: &serde_json::Map<String, Value>,
) -> BoardResult<()> {
    for (key, value) in options {
        let Some(option) = model.options.iter().find(|option| option.id == *key) else {
            return Err(BoardError::invalid(
                "modelOptions",
                format!(
                    "{} has no option {key:?}; supported: {}",
                    model.id,
                    model
                        .options
                        .iter()
                        .map(|option| option.id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            ));
        };
        if !option
            .choices
            .iter()
            .any(|choice| value.as_str() == Some(choice.id.as_str()))
        {
            return Err(BoardError::invalid(
                "modelOptions",
                format!(
                    "{key:?} supports {}",
                    option
                        .choices
                        .iter()
                        .map(|choice| choice.id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            ));
        }
    }
    Ok(())
}

impl EngineRpc {
    /// Whether this host can execute a provider (installed and enabled in
    /// Settings → Agents; the deterministic Mock peer is always local).
    pub(crate) fn board_provider_available(&self, harness: HarnessId) -> bool {
        if self.registry.resolve(harness).is_err() {
            return false;
        }
        harness == HarnessId::Mock
            || self.registry.descriptors().iter().any(|entry| {
                entry.id == harness && entry.installed && crate::registry::descriptor_enabled(entry)
            })
    }

    /// The engine's configured default provider for fresh chats.
    pub(crate) fn board_default_harness(&self) -> HarnessId {
        self.doc_host.harness_for("")
    }

    /// The chat a caller acts from. Agent runs are bound to themselves; the
    /// UI names the chat it started from; authoring sessions are not chats.
    pub(crate) fn board_acting_chat(
        &self,
        caller: &BoardCaller,
        claimed: Option<&str>,
    ) -> BoardResult<Option<String>> {
        match caller {
            BoardCaller::Agent { task_id, .. } => {
                if claimed.is_some_and(|claimed| claimed != task_id) {
                    return Err(BoardError::NotAuthorized {
                        reason: "A chat can only start ticket work as itself.".into(),
                    });
                }
                Ok(Some(task_id.clone()))
            }
            BoardCaller::User { .. } | BoardCaller::Engine { .. } => Ok(claimed.map(str::to_owned)),
            BoardCaller::Authoring { .. } => {
                if claimed.is_some() {
                    return Err(BoardError::invalid(
                        "callerTaskId",
                        "a ticket discussion is not a chat",
                    ));
                }
                Ok(None)
            }
        }
    }

    fn board_project_chat(&self, item: &WorkItem, task_id: &str) -> BoardResult<Chat> {
        let chat = self
            .workspace
            .chat(task_id)
            .map_err(storage)?
            .ok_or_else(|| BoardError::InvalidDestination {
                reason: "That chat no longer exists.".into(),
                canonical: None,
            })?;
        if chat.space_id.as_deref() != Some(item.project_id.as_str()) || chat.archived {
            return Err(BoardError::InvalidDestination {
                reason: "Ticket work starts only from an active chat in the ticket's project."
                    .into(),
                canonical: None,
            });
        }
        if chat.device_id != self.engine_info.device_id {
            return Err(BoardError::WrongHost {
                project_id: item.project_id.clone(),
                owner_host_id: chat.device_id,
            });
        }
        Ok(chat)
    }

    /// Resolve where a first working chat goes for this caller (Board/User →
    /// new main; main caller → side under it; side caller → fresh sibling
    /// under its main). Roles come from the chat-model slice.
    pub(crate) fn board_plan_destination(
        &self,
        caller: &BoardCaller,
        item: &WorkItem,
        requested: &BoardDestination,
        acting: Option<&str>,
    ) -> BoardResult<DestinationPlan> {
        let new_main = |acting: Option<&str>| DestinationPlan {
            destination: BoardDestination::NewMain,
            parent_task_id: None,
            acting_task_id: acting.map(str::to_owned),
        };
        if matches!(caller, BoardCaller::Authoring { .. }) {
            return match requested {
                BoardDestination::Auto | BoardDestination::NewMain => Ok(new_main(None)),
                _ => Err(BoardError::InvalidRole {
                    operation: "start".into(),
                    reason: "A ticket discussion can only start work in a new main chat.".into(),
                    allowed: vec!["newMain".into()],
                }),
            };
        }
        let identity = |task_id: &str| -> BoardResult<CanonicalChatIdentity> {
            self.board_project_chat(item, task_id)?;
            self.board_chat_identity(
                caller,
                BoardChatIdentityRequest {
                    task_id: task_id.into(),
                },
            )
        };
        let acting_identity = acting.map(identity).transpose()?;
        let agent = matches!(caller, BoardCaller::Agent { .. });
        let side_under = |main: &str, acting: &str| DestinationPlan {
            destination: BoardDestination::SideOf {
                main_task_id: main.into(),
            },
            parent_task_id: Some(main.into()),
            acting_task_id: Some(acting.into()),
        };
        let sibling = |side: &str, main: Option<String>| -> BoardResult<DestinationPlan> {
            let main = main.ok_or_else(|| BoardError::InvalidDestination {
                reason: "That side chat has no available main parent.".into(),
                canonical: None,
            })?;
            Ok(DestinationPlan {
                destination: BoardDestination::SiblingOf {
                    side_task_id: side.into(),
                },
                parent_task_id: Some(main),
                acting_task_id: Some(side.into()),
            })
        };
        let refused = |reason: &str| BoardError::InvalidDestination {
            reason: reason.into(),
            canonical: None,
        };
        match requested {
            BoardDestination::Auto => match (&acting_identity, acting) {
                (None, _) | (_, None) => Ok(new_main(None)),
                (Some(identity), Some(acting)) => match identity.role {
                    BoardChatRole::Main => Ok(side_under(acting, acting)),
                    BoardChatRole::Side => sibling(acting, identity.main_parent_id.clone()),
                },
            },
            BoardDestination::NewMain => Ok(new_main(acting)),
            BoardDestination::SideOf { main_task_id } => match (agent, &acting_identity, acting) {
                (true, Some(identity), Some(acting)) => match identity.role {
                    BoardChatRole::Main if acting == main_task_id.as_str() => {
                        Ok(side_under(acting, acting))
                    }
                    BoardChatRole::Side
                        if identity.main_parent_id.as_deref() == Some(main_task_id.as_str()) =>
                    {
                        sibling(acting, identity.main_parent_id.clone())
                    }
                    _ => Err(refused(
                        "A chat can only start ticket work under its own main chat.",
                    )),
                },
                (true, _, _) => Err(refused("This chat's role could not be resolved.")),
                (false, _, _) => {
                    let main = identity(main_task_id)?;
                    if main.role != BoardChatRole::Main {
                        return Err(BoardError::InvalidRole {
                                operation: "start".into(),
                                reason: "Side chats cannot hold side chats; choose its main chat or a sibling.".into(),
                                allowed: vec!["siblingOf".into(), "newMain".into()],
                            });
                    }
                    Ok(side_under(main_task_id, main_task_id))
                }
            },
            BoardDestination::SiblingOf { side_task_id } => {
                if agent && acting != Some(side_task_id.as_str()) {
                    return Err(refused("A side chat can only create a sibling of itself."));
                }
                let side = match (&acting_identity, acting) {
                    (Some(identity), Some(acting)) if acting == side_task_id.as_str() => {
                        identity.clone()
                    }
                    _ => identity(side_task_id)?,
                };
                if side.role != BoardChatRole::Side {
                    return Err(refused("A sibling needs a side chat to stand beside."));
                }
                sibling(side_task_id, side.main_parent_id)
            }
        }
    }

    /// Resolve and validate the full settings combination for a new worker.
    pub(crate) async fn board_resolve_new_worker(
        &self,
        caller: &BoardCaller,
        item: &WorkItem,
        explicit: Option<&BoardExecutionProfile>,
        plan: &DestinationPlan,
    ) -> BoardResult<SettingsResolution> {
        let space = self
            .workspace
            .space(&item.project_id)
            .map_err(storage)?
            .ok_or_else(|| BoardError::ProjectNotFound {
                project_id: item.project_id.clone(),
            })?;
        let empty = BoardExecutionProfile::default();
        let explicit = explicit.unwrap_or(&empty);
        let ticket = &item.implementation;
        let parent = match &plan.parent_task_id {
            Some(parent) => Some(self.board_project_chat(item, parent)?),
            None => None,
        };
        let parent_config = parent.as_ref().and_then(|chat| chat.config.clone());
        let acting_config = plan
            .acting_task_id
            .as_deref()
            .filter(|_| matches!(caller, BoardCaller::Agent { .. }))
            .and_then(|task| self.workspace.chat(task).ok().flatten())
            .and_then(|chat| chat.config);
        let project = &space.execution_defaults;
        let saved = self
            .provider_defaults
            .as_ref()
            .map(|service| service.get())
            .transpose()
            .map_err(storage)?
            .unwrap_or_default();
        let mut sources = Vec::new();
        let mut unresolved = Vec::new();
        let deferred = ticket.deferred && explicit.is_inherited();

        // ---- harness ----
        let (harness, origin) = if let Some(harness) = explicit.harness {
            (harness, BoardSettingOrigin::StartOverride)
        } else if let Some(harness) = ticket.harness {
            (harness, BoardSettingOrigin::Ticket)
        } else if let Some(config) = &parent_config {
            (config.harness, BoardSettingOrigin::Parent)
        } else if let Some(provider) = &project.provider {
            (provider.harness, BoardSettingOrigin::Project)
        } else {
            if deferred {
                unresolved.push("harness".to_owned());
            }
            (
                self.board_default_harness(),
                BoardSettingOrigin::ProviderDefault,
            )
        };
        sources.push(source("harness", origin));
        if !self.board_provider_available(harness) {
            return Err(BoardError::UnavailableProvider { harness });
        }
        // A choice made for another provider never carries over.
        let ticket_applies = explicit.harness.is_none_or(|h| ticket.harness == Some(h))
            && ticket.harness.is_none_or(|h| h == harness);
        let parent_applies = parent_config
            .as_ref()
            .is_some_and(|config| config.harness == harness);
        let project_model = project
            .provider
            .as_ref()
            .filter(|provider| provider.harness == harness);

        // ---- account ----
        let account = explicit
            .provider_account_id
            .clone()
            .or_else(|| {
                ticket_applies
                    .then(|| ticket.provider_account_id.clone())
                    .flatten()
            })
            .or_else(|| {
                parent_applies
                    .then(|| parent_config.as_ref()?.provider_account_id.clone())
                    .flatten()
            });
        let account = if matches!(harness, HarnessId::Codex | HarnessId::ClaudeCode) {
            Some(
                self.agent_accounts
                    .resolve_execution(harness, account.as_deref())
                    .map_err(|error| BoardError::Unavailable {
                        reason: error.to_string(),
                    })?
                    .account_id,
            )
        } else {
            account
        };

        // ---- model ----
        let provider_default = saved.providers.get(&harness).cloned().unwrap_or_default();
        let requested_model = explicit
            .model
            .clone()
            .map(|model| (model, BoardSettingOrigin::StartOverride))
            .or_else(|| {
                ticket_applies
                    .then(|| ticket.model.clone())
                    .flatten()
                    .map(|model| (model, BoardSettingOrigin::Ticket))
            });
        let catalog = self
            .model_cache
            .list(harness, account.as_deref(), false)
            .await;
        let models = match catalog {
            Ok(models) => models,
            Err(error) => {
                if requested_model.is_some()
                    || explicit.reasoning.is_some()
                    || (ticket_applies && ticket.reasoning.is_some())
                {
                    return Err(BoardError::Unavailable {
                        reason: format!(
                            "The {} model catalog is unavailable, so the requested model cannot be verified: {error}",
                            harness_wire(harness)
                        ),
                    });
                }
                Vec::new()
            }
        };
        let (model_id, origin) = match requested_model {
            Some(choice) => (Some(choice.0), choice.1),
            None => {
                if deferred {
                    unresolved.push("model".to_owned());
                }
                if let Some(model) = parent_applies
                    .then(|| parent_config.as_ref()?.model.clone())
                    .flatten()
                {
                    (Some(model), BoardSettingOrigin::Parent)
                } else if let Some(model) = project_model.and_then(|p| p.model.clone()) {
                    (Some(model), BoardSettingOrigin::Project)
                } else if let Some(model) = provider_default.model.clone() {
                    (Some(model), BoardSettingOrigin::ProviderDefault)
                } else {
                    (
                        models.first().map(|model| model.id.clone()),
                        BoardSettingOrigin::CatalogDefault,
                    )
                }
            }
        };
        let model = match (&model_id, models.is_empty()) {
            (Some(id), false) => Some(check_model_choice(harness, &models, id)?),
            _ => None,
        };
        if model_id.is_some() {
            sources.push(source("model", origin));
        }

        // ---- reasoning ----
        let uses_default_model = model_id.is_some() && model_id == provider_default.model;
        let parent_same_model =
            parent_applies && parent_config.as_ref().and_then(|c| c.model.clone()) == model_id;
        let (reasoning, origin) = if let Some(level) = explicit.reasoning {
            (Some(level), BoardSettingOrigin::StartOverride)
        } else if let Some(level) = ticket_applies.then_some(ticket.reasoning).flatten() {
            (Some(level), BoardSettingOrigin::Ticket)
        } else if let Some(level) = parent_same_model
            .then(|| parent_config.as_ref()?.reasoning)
            .flatten()
        {
            (Some(level), BoardSettingOrigin::Parent)
        } else if let Some(level) = uses_default_model
            .then_some(provider_default.reasoning)
            .flatten()
        {
            (Some(level), BoardSettingOrigin::ProviderDefault)
        } else {
            (
                model
                    .and_then(|model| luna_proto::default_reasoning_level(&model.reasoning_levels)),
                BoardSettingOrigin::CatalogDefault,
            )
        };
        if let (Some(model), Some(level)) = (model, reasoning) {
            check_reasoning(model, level)?;
        }
        if reasoning.is_some() {
            sources.push(source("reasoning", origin));
        }

        // ---- provider options ----
        let mut model_options = model
            .map(luna_proto::default_model_options)
            .unwrap_or_default();
        if uses_default_model {
            model_options.extend(provider_default.model_options.clone());
        }
        if parent_same_model && let Some(config) = &parent_config {
            model_options.extend(config.model_options.clone());
        }
        if ticket_applies
            && ticket
                .model
                .as_ref()
                .is_none_or(|m| Some(m) == model_id.as_ref())
        {
            model_options.extend(ticket.model_options.clone());
        }
        model_options.extend(explicit.model_options.clone());
        if let Some(model) = model {
            check_options(model, &model_options)?;
        }

        // ---- access (never broadened) ----
        let ceiling = [
            parent_config
                .as_ref()
                .map(|config| config.effective_permission_mode),
            acting_config
                .as_ref()
                .map(|config| config.effective_permission_mode),
        ]
        .into_iter()
        .flatten()
        .min();
        let (requested_mode, origin) = if let Some(mode) = explicit.permission_mode {
            (mode, BoardSettingOrigin::StartOverride)
        } else if let Some(mode) = ticket.permission_mode {
            (mode, BoardSettingOrigin::Ticket)
        } else if let Some(config) = &parent_config {
            (config.effective_permission_mode, BoardSettingOrigin::Parent)
        } else if let Some(mode) = project.permission_mode {
            (mode, BoardSettingOrigin::Project)
        } else {
            if deferred {
                unresolved.push("permissionMode".to_owned());
            }
            (PermissionMode::Ask, BoardSettingOrigin::ProviderDefault)
        };
        if let Some(ceiling) = ceiling
            && requested_mode > ceiling
            && matches!(
                origin,
                BoardSettingOrigin::StartOverride | BoardSettingOrigin::Ticket
            )
        {
            return Err(BoardError::NotAuthorized {
                reason: format!(
                    "The requested {requested_mode:?} access exceeds the creating chat's {ceiling:?} access."
                ),
            });
        }
        let capped = ceiling.map_or(requested_mode, |ceiling| requested_mode.min(ceiling));
        let capabilities =
            crate::permissions::provider_capabilities_for(harness, model_id.as_deref());
        let permission_mode = if capabilities.supported_modes.contains(&capped) {
            capped
        } else if matches!(
            origin,
            BoardSettingOrigin::StartOverride | BoardSettingOrigin::Ticket
        ) {
            return Err(BoardError::invalid(
                "permissionMode",
                format!(
                    "{} supports {:?}",
                    harness_wire(harness),
                    capabilities.supported_modes
                ),
            ));
        } else {
            capabilities
                .supported_modes
                .iter()
                .copied()
                .filter(|mode| *mode <= capped)
                .max()
                .ok_or_else(|| BoardError::Unavailable {
                    reason: "No supported access mode fits the creating chat's ceiling.".into(),
                })?
        };
        sources.push(source("permissionMode", origin));

        // ---- checkout ----
        let (checkout, origin) = if let Some(choice) = &explicit.checkout {
            (choice.clone(), BoardSettingOrigin::StartOverride)
        } else if let Some(choice) = &ticket.checkout {
            (choice.clone(), BoardSettingOrigin::Ticket)
        } else if parent.is_some() {
            (BoardCheckoutChoice::Current, BoardSettingOrigin::Parent)
        } else if let Some(choice) = project.checkout {
            (
                match choice {
                    ProjectCheckout::Local => BoardCheckoutChoice::Current,
                    ProjectCheckout::NewWorktree => BoardCheckoutChoice::NewWorktree {
                        branch: None,
                        base: None,
                    },
                },
                BoardSettingOrigin::Project,
            )
        } else {
            (
                BoardCheckoutChoice::Current,
                BoardSettingOrigin::ProviderDefault,
            )
        };
        sources.push(source("checkout", origin));
        let (cwd, checkout_id, branch) = match (&checkout, &parent) {
            (BoardCheckoutChoice::Current, Some(parent)) => (
                parent.cwd.clone().unwrap_or_else(|| space.path.clone()),
                parent.checkout_id.clone(),
                parent.branch.clone(),
            ),
            (BoardCheckoutChoice::Current, None) => {
                (space.path.clone(), space.checkout_id.clone(), None)
            }
            (BoardCheckoutChoice::NewWorktree { branch, .. }, _) => {
                (space.path.clone(), None, branch.clone())
            }
        };
        Ok(SettingsResolution {
            resolved: ResolvedExecution {
                harness,
                provider_account_id: account,
                model: model_id,
                reasoning,
                model_options,
                permission_mode,
                host_device_id: self.engine_info.device_id.clone(),
                project_id: item.project_id.clone(),
                cwd,
                checkout_id,
                branch,
                sources,
            },
            unresolved,
        })
    }

    /// The configuration an existing worker keeps (it is never changed by a
    /// card move).
    pub(crate) fn board_worker_settings(
        &self,
        item: &WorkItem,
        task_id: &str,
    ) -> BoardResult<ResolvedExecution> {
        let chat = self
            .workspace
            .chat(task_id)
            .map_err(storage)?
            .ok_or_else(|| BoardError::Unavailable {
                reason: "The ticket's working chat no longer exists.".into(),
            })?;
        let config = chat.config.clone().ok_or_else(|| BoardError::Unavailable {
            reason: "The working chat has no execution configuration.".into(),
        })?;
        let fields = [
            "harness",
            "model",
            "reasoning",
            "permissionMode",
            "checkout",
        ];
        Ok(ResolvedExecution {
            harness: config.harness,
            provider_account_id: config.provider_account_id.clone(),
            model: config.model.clone(),
            reasoning: config.reasoning,
            model_options: config.model_options.clone(),
            permission_mode: config.permission_mode,
            host_device_id: chat.device_id.clone(),
            project_id: item.project_id.clone(),
            cwd: chat.cwd.clone().unwrap_or_default(),
            checkout_id: chat.checkout_id.clone(),
            branch: chat.branch.clone(),
            sources: fields
                .into_iter()
                .map(|field| source(field, BoardSettingOrigin::ExistingWorker))
                .collect(),
        })
    }

    /// Reviewer settings: the ticket's review profile, else the
    /// implementation model in a separate reviewer chat. Reviewer access is
    /// constrained by the engine (Ask), never inherited write access.
    pub(crate) async fn board_resolve_reviewer(
        &self,
        item: &WorkItem,
        implementation: &ResolvedExecution,
    ) -> BoardResult<(ResolvedExecution, bool)> {
        let review = &item.review;
        let configured = review.harness.is_some() || review.model.is_some();
        if !configured {
            let mut resolved = implementation.clone();
            resolved.permission_mode = PermissionMode::Ask;
            resolved.sources = ["harness", "model", "reasoning"]
                .into_iter()
                .map(|field| source(field, BoardSettingOrigin::ImplementationFallback))
                .collect();
            return Ok((resolved, true));
        }
        let harness = review.harness.unwrap_or(implementation.harness);
        if !self.board_provider_available(harness) {
            return Err(BoardError::UnavailableProvider { harness });
        }
        let models = self
            .model_cache
            .list(harness, review.provider_account_id.as_deref(), false)
            .await
            .map_err(|error| BoardError::Unavailable {
                reason: format!("The reviewer model catalog is unavailable: {error}"),
            })?;
        let model_id = review
            .model
            .clone()
            .or_else(|| models.first().map(|model| model.id.clone()));
        let model = model_id
            .as_deref()
            .map(|id| check_model_choice(harness, &models, id))
            .transpose()?;
        let reasoning = review.reasoning.or_else(|| {
            model.and_then(|model| luna_proto::default_reasoning_level(&model.reasoning_levels))
        });
        if let (Some(model), Some(level)) = (model, reasoning) {
            check_reasoning(model, level)?;
        }
        let mut model_options = model
            .map(luna_proto::default_model_options)
            .unwrap_or_default();
        model_options.extend(review.model_options.clone());
        if let Some(model) = model {
            check_options(model, &model_options)?;
        }
        let mut sources = vec![source("harness", BoardSettingOrigin::Ticket)];
        sources.push(source(
            "model",
            if review.model.is_some() {
                BoardSettingOrigin::Ticket
            } else {
                BoardSettingOrigin::CatalogDefault
            },
        ));
        Ok((
            ResolvedExecution {
                harness,
                provider_account_id: review.provider_account_id.clone(),
                model: model_id,
                reasoning,
                model_options,
                permission_mode: PermissionMode::Ask,
                host_device_id: implementation.host_device_id.clone(),
                project_id: implementation.project_id.clone(),
                cwd: implementation.cwd.clone(),
                checkout_id: implementation.checkout_id.clone(),
                branch: implementation.branch.clone(),
                sources,
            },
            false,
        ))
    }

    /// Save-time validation against the last-known-good catalog (no provider
    /// call on the save path). Without a cached catalog only the shape is
    /// known; Start validates against the live catalog again.
    pub(crate) fn board_validate_profiles(
        &self,
        project: &Space,
        implementation: &BoardExecutionProfile,
        review: &BoardReviewProfile,
    ) -> BoardResult<()> {
        let default_harness = project
            .execution_defaults
            .provider
            .as_ref()
            .map(|provider| provider.harness);
        let check = |harness: Option<HarnessId>,
                     account: Option<&str>,
                     model: Option<&str>,
                     reasoning: Option<ReasoningLevel>,
                     options: &serde_json::Map<String, Value>|
         -> BoardResult<()> {
            // An inherited provider is only known at Start (a side chat may
            // inherit its parent's), so only an explicit one is checked here.
            let Some(harness) = harness else {
                return Ok(());
            };
            if !self.board_provider_available(harness) {
                return Err(BoardError::UnavailableProvider { harness });
            }
            let Some(models) = self.model_cache.cached(harness, account) else {
                return Ok(());
            };
            let Some(model) = model else {
                return Ok(());
            };
            let model = check_model_choice(harness, &models, model)?;
            if let Some(level) = reasoning {
                check_reasoning(model, level)?;
            }
            check_options(model, options)
        };
        check(
            implementation.harness,
            implementation.provider_account_id.as_deref(),
            implementation.model.as_deref(),
            implementation.reasoning,
            &implementation.model_options,
        )?;
        if let Some(mode) = implementation.permission_mode
            && let Some(harness) = implementation.harness.or(default_harness)
        {
            let capabilities = crate::permissions::provider_capabilities_for(
                harness,
                implementation.model.as_deref(),
            );
            if !capabilities.supported_modes.contains(&mode) {
                return Err(BoardError::invalid(
                    "implementation.permissionMode",
                    format!(
                        "{} supports {:?}",
                        harness_wire(harness),
                        capabilities.supported_modes
                    ),
                ));
            }
        }
        check(
            review.harness.or(implementation.harness),
            review.provider_account_id.as_deref(),
            review.model.as_deref(),
            review.reasoning,
            &review.model_options,
        )
    }
}
