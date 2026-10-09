//! `create_side_chat` and `create_main_chat`: the one creation service that
//! Board Start (A2), reviewers (A5), agent tools (A4) and the UI share.
//!
//! * Fresh side chats are draft child chats under the **main** chat (the
//!   engine resolves it; a side caller cannot supply a parent), with the
//!   creator's settings capped by the creator's and the main's access.
//! * With-context side chats reuse the side-chat service (native fork when
//!   the provider supports it, else a visible context snapshot) and are only
//!   allowed for main callers.
//! * New main chats use the ordinary `createChat` command (Cmd+N), never fork
//!   lineage, so they stay in the sidebar; "handed off from" is provenance.
//!
//! A first message is sent once, only with `start: true`, through the
//! ordinary queued-message command keyed by the chat id (a retry reads the
//! same receipt). A ticket's first message always comes from Start.
use super::*;
use luna_proto::{
    BoardChatCreated, BoardChatLaunchState, BoardContextMode, BoardCreateMainChatRequest,
    BoardCreateSideChatRequest, BoardExecutionProfile, BoardSettingOrigin, BoardSettingSource,
    CreateChildThreadRequest,
};

/// Written handoff briefs are messages, not synced rows.
const HANDOFF_BRIEF_MAX_BYTES: usize = 32 * 1024;
const PROMPT_MAX_BYTES: usize = 64 * 1024;
/// Child rows carry at most this many title characters.
const CHILD_TITLE_MAX_CHARS: usize = 160;
/// Worker-link commits retry a concurrent ticket edit this many times.
const LINK_ATTEMPTS: usize = 3;

/// A5's reviewer binding for a side chat created through the same rules.
#[derive(Debug, Clone)]
pub(crate) struct BoardReviewerSpec {
    pub work_item_id: String,
    pub review_run_id: String,
}

/// Write-once idempotency claim for one creation request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreationClaim {
    payload_hash: String,
    task_id: String,
}

fn settings_sources(origin: BoardSettingOrigin) -> Vec<BoardSettingSource> {
    ["harness", "model", "reasoning", "permissionMode"]
        .into_iter()
        .map(|field| BoardSettingSource {
            field: field.into(),
            source: origin,
        })
        .collect()
}

/// A start-ready configuration for a provider without a source chat.
fn standalone_config(
    harness: HarnessId,
    model: Option<String>,
    mode: PermissionMode,
) -> ChatConfig {
    ChatConfig {
        harness,
        provider_account_id: None,
        model,
        reasoning: None,
        model_options: Default::default(),
        sandbox: if mode == PermissionMode::Full {
            luna_proto::SandboxLevel::DangerFullAccess
        } else {
            luna_proto::SandboxLevel::WorkspaceWrite
        },
        permission_mode: mode,
        effective_permission_mode: mode,
        permission_ceiling_source: None,
        permission_degradation_reason: None,
        permission_audit: Vec::new(),
    }
}

impl EngineRpc {
    /// Claim a request id for one payload. The same key with different
    /// content is `idempotencyConflict`; a retry returns the claimed chat.
    fn board_claim_creation(
        &self,
        request_id: &str,
        payload_hash: &str,
        task_id: &str,
    ) -> BoardResult<()> {
        let key = kv_key("request", request_id);
        match self.board_kv_load::<CreationClaim>(&key)? {
            Some(claim) if claim.payload_hash == payload_hash => Ok(()),
            Some(_) => Err(BoardError::IdempotencyConflict {
                request_id: request_id.into(),
            }),
            None => self.board_kv_save(
                &key,
                &CreationClaim {
                    payload_hash: payload_hash.into(),
                    task_id: task_id.into(),
                },
            ),
        }
    }

    /// Resolve settings from a source configuration: inherit it unchanged
    /// when nothing is overridden, else validate the overrides against the
    /// provider catalog with access capped at the source's ceiling.
    pub(crate) async fn board_derived_execution(
        &self,
        base: &ChatConfig,
        profile: Option<&BoardExecutionProfile>,
    ) -> BoardResult<ChatConfig> {
        let Some(profile) = profile.filter(|profile| !profile.is_inherited()) else {
            let mut config = base.clone();
            config.permission_audit.clear();
            config.permission_mode = config.effective_permission_mode;
            return Ok(config);
        };
        let harness = profile.harness.unwrap_or(base.harness);
        let same_provider = harness == base.harness;
        let model = profile
            .model
            .clone()
            .or_else(|| same_provider.then(|| base.model.clone()).flatten());
        let same_model = same_provider && model == base.model;
        let request = CreateChildThreadRequest {
            prompt: String::new(),
            idempotency_key: "board-settings".into(),
            title: None,
            provider: (!same_provider).then_some(harness),
            reasoning: profile
                .reasoning
                .or_else(|| same_model.then_some(base.reasoning).flatten()),
            model_options: if profile.model_options.is_empty() && same_model {
                base.model_options.clone()
            } else {
                profile.model_options.clone()
            },
            model,
            cwd: None,
            report_results: false,
            continue_parent: false,
            checkout: None,
            permission_mode: profile.permission_mode,
        };
        let mut config = self
            .child_execution_from(base, &request)
            .await
            .map_err(|error| BoardError::invalid("execution", error.to_string()))?;
        // The requested mode never outruns the applied one: a handoff or a
        // sibling cannot upgrade access on a later turn.
        config.permission_mode = config.effective_permission_mode;
        config.permission_audit.clear();
        Ok(config)
    }

    /// The receipt for a ticket that already has a worker: that chat in its
    /// original location. Nothing is moved, linked or sent.
    fn board_reused_worker(
        &self,
        request_id: &str,
        item: &WorkItem,
        existing: &WorkItemTaskLink,
    ) -> BoardResult<BoardChatCreated> {
        let resolved = self.board_resolve_chat(&existing.task_id)?;
        Ok(BoardChatCreated {
            request_id: request_id.into(),
            task_id: existing.task_id.clone(),
            role: resolved.identity.role,
            main_parent_id: resolved.identity.main_parent_id.clone(),
            creator: resolved.identity.creator.clone(),
            context_origin: resolved.identity.context_origin,
            work_item_id: Some(item.id.clone()),
            launch: BoardChatLaunchState::Created,
            settings: self.board_chat_settings(&resolved.chat, Vec::new())?,
            navigation: self.board_default_navigation(item),
            reused: true,
        })
    }

    /// Load a ticket for binding a new worker. `Ok(Err(created))` is the
    /// existing worker to reuse instead.
    async fn board_binding_target(
        &self,
        caller: &BoardCaller,
        request_id: &str,
        binding: &luna_proto::BoardTicketBinding,
        project_id: &str,
        chat_id: &str,
    ) -> BoardResult<Result<WorkItem, Box<BoardChatCreated>>> {
        let item = self.board_item_for(caller, &binding.work_item_id, BoardAccess::Write)?;
        if let Some(existing) = self.board_working_link(&item.id)? {
            if existing.task_id == chat_id {
                // This request's own worker: finish idempotently.
                return Ok(Ok(item));
            }
            // A second requester reuses the worker where it already lives.
            return Ok(Err(Box::new(
                self.board_reused_worker(request_id, &item, &existing)?,
            )));
        }
        if item.project_id != project_id {
            return Err(BoardError::InvalidDestination {
                reason:
                    "That ticket belongs to another project; start it from a chat in its project."
                        .into(),
                canonical: None,
            });
        }
        if item.archived {
            return Err(BoardError::invalid(
                "ticket",
                "unarchive the ticket before starting work on it",
            ));
        }
        if binding.expected_revision != item.revision {
            return Err(BoardError::StaleRevision {
                work_item_id: item.id.clone(),
                expected: binding.expected_revision,
                current: Box::new(self.board_view(item)?),
            });
        }
        Ok(Ok(item))
    }

    /// Commit one Working (or Reviewer) link with its frozen original
    /// container, one `ChatLinked` event and a revision bump. Retries replay
    /// the stored receipt; a ticket that meanwhile got another worker is a
    /// conflict, never a second worker.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn board_link_worker(
        &self,
        caller: &BoardCaller,
        request_id: &str,
        operation: BoardOperation,
        work_item_id: &str,
        task_id: &str,
        role: TaskLinkRole,
        container: OriginalContainer,
        chat_role: BoardChatRole,
    ) -> BoardResult<BoardMutationReceipt> {
        let link_request = format!("{request_id}:{}", role.as_str());
        if let Some(receipt) = self
            .store
            .board_receipt(&link_request)
            .map_err(board_store_error)?
        {
            return Ok(receipt);
        }
        let link = WorkItemTaskLink {
            id: format!(
                "wl-{}",
                &board_digest(&(role.as_str(), request_id, task_id))?[..32]
            ),
            work_item_id: work_item_id.into(),
            task_id: task_id.into(),
            role,
            active: true,
            chat_role: Some(chat_role),
            original_container: Some(container.clone()),
            linked_by: caller.actor(),
            linked_at_ms: crate::now_ms(),
            unlinked_at_ms: None,
            source_message_id: None,
            context_refs: Vec::new(),
            launch_snapshot_id: None,
            result_subscriptions: Vec::new(),
        };
        let navigation = match &container {
            OriginalContainer::MainChat => BoardNavigationTarget::MainChat {
                task_id: task_id.into(),
            },
            OriginalContainer::SideOf { main_task_id } => BoardNavigationTarget::SideChat {
                main_task_id: main_task_id.clone(),
                side_task_id: task_id.into(),
            },
        };
        for attempt in 0..LINK_ATTEMPTS {
            let item = self.board_load(work_item_id)?;
            if role == TaskLinkRole::Working
                && let Some(existing) = self.board_working_link(&item.id)?
                && existing.task_id != task_id
            {
                return Err(BoardError::InvalidDestination {
                    reason:
                        "The ticket already has a working chat; it keeps its original location."
                            .into(),
                    canonical: Some(self.board_default_navigation(&item)),
                });
            }
            let mut next = item.clone();
            next.revision += 1;
            next.updated_at_ms = crate::now_ms();
            next.updated_by = caller.actor();
            let mut receipt = self.board_title_receipt(caller, &next, Some(task_id.into()));
            receipt.operation = operation;
            receipt.navigation = navigation.clone();
            match self.board_commit(BoardCommit {
                request_id: link_request.clone(),
                payload_hash: board_digest(&(
                    operation.as_str(),
                    role.as_str(),
                    work_item_id,
                    task_id,
                ))?,
                committed_at_ms: crate::now_ms(),
                item: BoardItemWrite::Replace {
                    expected_revision: item.revision,
                    item: Box::new(next),
                    placement: None,
                },
                rows: vec![luna_sync::BoardRowWrite::TaskLink(Box::new(link.clone()))],
                event: Some((
                    WorkItemEventKind::ChatLinked {
                        task_id: task_id.into(),
                        role,
                    },
                    true,
                )),
                receipt: Some(receipt),
            }) {
                Ok(Some(receipt)) => return Ok(receipt),
                Ok(None) => return Err(storage("link returned no receipt")),
                Err(BoardError::Busy { reason })
                    if attempt + 1 < LINK_ATTEMPTS && reason == BOARD_CONCURRENT_CHANGE =>
                {
                    continue;
                }
                Err(error) => return Err(error),
            }
        }
        Err(BoardError::Busy {
            reason: "The ticket kept changing; retry with the same request id.".into(),
        })
    }

    /// Queue the first message once (keyed by the chat id) through the
    /// ordinary task-command path.
    async fn board_send_first(
        &self,
        task_id: &str,
        text: &str,
    ) -> BoardResult<BoardChatLaunchState> {
        let mut command = legacy_task_command(
            task_id.to_owned(),
            TaskCommandIntent::QueueMessage {
                message_id: format!("board-first:{task_id}"),
                text: text.to_owned(),
                attachments: Vec::new(),
                input_items: Vec::new(),
                hold_for_turn_end: true,
                follow_up_behavior: FollowUpBehavior::Queue,
                execution: None,
            },
        );
        command.command_id = format!("board-first:{task_id}");
        command.actor_id = "luna-board:first-message".into();
        match self.execute_task_command(command, true).await {
            Ok(receipt) if receipt.status == CommandReceiptStatus::Applied => {
                Ok(BoardChatLaunchState::Queued)
            }
            Ok(receipt) if receipt.status == CommandReceiptStatus::Pending => {
                Ok(BoardChatLaunchState::Unknown)
            }
            Ok(receipt) => Err(BoardError::Unavailable {
                reason: format!(
                    "Created chat {task_id}, but its first message was refused: {}. Retry with the same request id.",
                    receipt
                        .reason
                        .as_ref()
                        .map_or("no reason recorded", |reason| reason.message.as_str())
                ),
            }),
            Err(_) => Ok(BoardChatLaunchState::Unknown),
        }
    }

    /// A draft child under the main chat with an explicit configuration:
    /// nothing starts until a message is queued.
    async fn board_create_fresh_side(
        &self,
        main: &Chat,
        task_id: &str,
        key: &str,
        title: Option<&str>,
        config: ChatConfig,
    ) -> BoardResult<()> {
        let main_config = main.config.as_ref().ok_or_else(|| {
            BoardError::invalid(
                "callerTaskId",
                "send a message in the main chat first to choose its provider",
            )
        })?;
        let request = CreateChildThreadRequest {
            prompt: String::new(),
            idempotency_key: key.into(),
            title: title
                .filter(|title| title.chars().count() <= CHILD_TITLE_MAX_CHARS)
                .map(str::to_owned),
            provider: (config.harness != main_config.harness).then_some(config.harness),
            model: config.model.clone(),
            reasoning: config.reasoning,
            model_options: config.model_options.clone(),
            cwd: None,
            report_results: false,
            continue_parent: false,
            checkout: None,
            permission_mode: None,
        };
        let mut command = legacy_task_command(
            task_id.to_owned(),
            TaskCommandIntent::CreateChildChat {
                parent_task_id: main.id.clone(),
                request,
                execution: Some(config),
                access_grant: None,
                draft: true,
            },
        );
        command.command_id = format!("child-create:{task_id}");
        command.actor_id = "luna-board:side-chat".into();
        let receipt = self
            .execute_task_command(command, true)
            .await
            .map_err(|error| BoardError::Unavailable {
                reason: format!("Side chat creation did not complete: {error}"),
            })?;
        if receipt.status != CommandReceiptStatus::Applied {
            return Err(BoardError::Unavailable {
                reason: format!(
                    "Side chat creation was refused: {}",
                    receipt
                        .reason
                        .as_ref()
                        .map_or("no reason recorded", |reason| reason.message.as_str())
                ),
            });
        }
        Ok(())
    }

    // ---- create_side_chat ----

    /// From a main caller: one side chat under it (fresh or with permitted
    /// context). From a side caller: a fresh sibling under the caller's main
    /// parent (context requests → `InvalidRole` with allowed alternatives);
    /// creator provenance records the side caller; access never exceeds the
    /// creator's ceiling. Optional ticket binding links the worker with a
    /// frozen `OriginalContainer::SideOf`.
    pub(crate) async fn board_create_side_chat(
        &self,
        caller: &BoardCaller,
        request: BoardCreateSideChatRequest,
    ) -> BoardResult<BoardChatCreated> {
        self.board_spawn_side_chat(caller, request, None, true)
            .await
    }

    /// The same creation for a caller that already holds
    /// `board_hub().gate(ticket)` (Board Start): the gate is not re-taken.
    pub(crate) async fn board_create_side_chat_gated(
        &self,
        caller: &BoardCaller,
        request: BoardCreateSideChatRequest,
    ) -> BoardResult<BoardChatCreated> {
        self.board_spawn_side_chat(caller, request, None, false)
            .await
    }

    /// A5: a reviewer side chat through the same placement rules — under a
    /// main ticket chat, or a fresh sibling under a side ticket's main. It
    /// gets a `Reviewer` link (never `Working`).
    pub(crate) async fn board_create_reviewer_chat(
        &self,
        caller: &BoardCaller,
        request: BoardCreateSideChatRequest,
        reviewer: BoardReviewerSpec,
    ) -> BoardResult<BoardChatCreated> {
        if request.ticket.is_some() {
            return Err(BoardError::invalid(
                "ticket",
                "reviewers link to their ticket through the review run",
            ));
        }
        if request.context != BoardContextMode::Fresh {
            return Err(BoardError::invalid(
                "context",
                "reviewers start fresh with an explicit review brief",
            ));
        }
        self.board_spawn_side_chat(caller, request, Some(reviewer), false)
            .await
    }

    async fn board_spawn_side_chat(
        &self,
        caller: &BoardCaller,
        request: BoardCreateSideChatRequest,
        reviewer: Option<BoardReviewerSpec>,
        take_gate: bool,
    ) -> BoardResult<BoardChatCreated> {
        let request_id = caller.normalized_request_id(&request.request_id)?;
        let creator_id = self.board_bound_chat(
            caller,
            &request.caller_task_id,
            "callerTaskId",
            "createSideChat",
        )?;
        let prompt = request
            .prompt
            .as_deref()
            .map(str::trim)
            .filter(|prompt| !prompt.is_empty());
        match (request.start, prompt) {
            (true, None) => {
                return Err(BoardError::invalid(
                    "prompt",
                    "a started side chat needs its first message",
                ));
            }
            (false, Some(_)) => {
                return Err(BoardError::invalid(
                    "start",
                    "the prompt is sent only with start: true",
                ));
            }
            (_, Some(prompt)) if prompt.len() > PROMPT_MAX_BYTES => {
                return Err(BoardError::invalid(
                    "prompt",
                    format!("use at most {} KiB", PROMPT_MAX_BYTES / 1024),
                ));
            }
            _ => {}
        }
        if request.start && request.ticket.is_some() {
            return Err(BoardError::invalid(
                "start",
                "a ticket's first message comes from board_start (it freezes the launch); create its worker with start: false",
            ));
        }
        let title = request.title.as_deref().map(clean_chat_title).transpose()?;
        let kind = match request.context {
            BoardContextMode::Fresh => BoardChatCreationKind::SideFresh,
            BoardContextMode::WithContext { .. } => BoardChatCreationKind::SideWithContext,
        };
        let creator = self.board_validate_creation(&creator_id, kind)?;
        let creator_chat = self
            .workspace
            .chat(&creator_id)
            .map_err(storage)?
            .ok_or_else(|| BoardError::invalid("callerTaskId", "that chat does not exist"))?;
        self.board_check_chat(caller, &creator_chat, BoardAccess::Write)?;
        let project_id = creator_chat
            .space_id
            .clone()
            .ok_or_else(|| BoardError::invalid("callerTaskId", "that chat has no project"))?;
        if creator_chat.archived {
            return Err(BoardError::invalid(
                "callerTaskId",
                "unarchive the chat before creating side chats from it",
            ));
        }
        if creator_chat.device_id != self.engine_info.device_id {
            return Err(BoardError::Unavailable {
                reason: "Create side chats on the chat's owning host.".into(),
            });
        }
        // The engine resolves the parent: the caller itself when main, its
        // existing main when side. No request field can name another one.
        let main_id = match creator.role {
            BoardChatRole::Main => creator_id.clone(),
            BoardChatRole::Side => creator
                .main_parent_id
                .clone()
                .ok_or_else(|| storage("side chat without a main parent"))?,
        };
        let main_chat = if main_id == creator_id {
            creator_chat.clone()
        } else {
            self.workspace
                .chat(&main_id)
                .map_err(storage)?
                .filter(|chat| !chat.archived)
                .ok_or_else(|| BoardError::InvalidRole {
                    operation: "createSideChat".into(),
                    reason: "This side chat's main chat is unavailable.".into(),
                    allowed: vec!["create_main_chat with a written handoffBrief".into()],
                })?
        };
        let chat_id = match request.context {
            BoardContextMode::Fresh => {
                crate::cross_task_tools::child_task_id(&main_id, &request_id)
            }
            BoardContextMode::WithContext { .. } => derived_task_id("side-context", &request_id),
        };
        let mut payload = request.clone();
        payload.request_id.clear();
        payload.caller_task_id = creator_id.clone();
        let payload_hash = board_digest(&(
            "createSideChat",
            &payload,
            reviewer
                .as_ref()
                .map(|spec| (&spec.work_item_id, &spec.review_run_id)),
        ))?;

        let _gate = match &request.ticket {
            Some(binding) if take_gate => Some(self.board_hub().gate(&binding.work_item_id).await),
            _ => None,
        };
        let ticket = match &request.ticket {
            Some(binding) => match self
                .board_binding_target(caller, &request_id, binding, &project_id, &chat_id)
                .await?
            {
                Ok(item) => Some(item),
                Err(reused) => return Ok(*reused),
            },
            None => None,
        };
        let reviewer_item = match &reviewer {
            Some(spec) => {
                let item = self.board_item_for(caller, &spec.work_item_id, BoardAccess::Write)?;
                if item.project_id != project_id {
                    return Err(BoardError::InvalidDestination {
                        reason: "A reviewer is created next to its ticket's working chat.".into(),
                        canonical: None,
                    });
                }
                Some(item)
            }
            None => None,
        };
        let explicit = request
            .execution
            .clone()
            .filter(|profile| !profile.is_inherited());
        let saved = ticket
            .as_ref()
            .map(|item| item.implementation.clone())
            .filter(|profile| !profile.is_inherited() && !profile.deferred);
        let origin = if explicit.is_some() {
            BoardSettingOrigin::StartOverride
        } else if saved.is_some() {
            BoardSettingOrigin::Ticket
        } else {
            BoardSettingOrigin::Parent
        };
        let profile = explicit.or(saved);
        let wanted_title = ticket
            .as_ref()
            .map(|item| item.title.clone())
            .or_else(|| {
                reviewer_item
                    .as_ref()
                    .map(|item| format!("Review · {} · {}", item.key, item.title))
                    .filter(|_| title.is_none())
            })
            .or(title);
        // Claimed only once the request is valid: a refused request can be
        // corrected and retried with the same key.
        self.board_claim_creation(&request_id, &payload_hash, &chat_id)?;

        if self.workspace.chat(&chat_id).map_err(storage)?.is_none() {
            let with_context = matches!(request.context, BoardContextMode::WithContext { .. });
            if with_context && profile.is_some() {
                return Err(BoardError::invalid(
                    "execution",
                    "a with-context side chat continues its parent's provider session; create a fresh side chat to choose other settings",
                ));
            }
            // How the context is obtained is decided once per chat (a retry
            // reuses the recorded mode, so a reservation is never replayed
            // with another one).
            let native = with_context
                && match self.board_provenance(&chat_id)? {
                    Some(record) => record.context_origin == BoardContextOrigin::NativeFork,
                    None => {
                        let capable = main_chat.config.as_ref().is_some_and(|config| {
                            capability_for(config.harness) != ProviderForkCapability::None
                        });
                        // The main chat's own agent always calls its tools
                        // mid-turn, when its provider history cannot be forked
                        // yet: snapshot the visible transcript so far instead.
                        // Any other actor keeps the settle requirement.
                        let own_turn = matches!(
                            caller,
                            BoardCaller::Agent { task_id, .. } if *task_id == main_id
                        );
                        capable
                            && !(own_turn && self.ensure_history_operation_ready(&main_id).is_err())
                    }
                };
            let (source, context_origin) = match &request.context {
                BoardContextMode::Fresh => (None, BoardContextOrigin::Fresh),
                BoardContextMode::WithContext { .. } => (
                    Some(ChatProvenance {
                        task_id: creator_id.clone(),
                        relation: ChatProvenanceRelation::ForkedFrom,
                    }),
                    if native {
                        BoardContextOrigin::NativeFork
                    } else {
                        BoardContextOrigin::ContextSnapshot
                    },
                ),
            };
            self.board_record_provenance(&ChatProvenanceRecord {
                task_id: chat_id.clone(),
                creator: Some(ChatProvenance {
                    task_id: creator_id.clone(),
                    relation: ChatProvenanceRelation::CreatedBy,
                }),
                source,
                context_origin,
                role: BoardChatRole::Side,
                main_parent_id: Some(main_id.clone()),
                request_id: request_id.clone(),
                created_at_ms: crate::now_ms(),
            })?;
            match &request.context {
                BoardContextMode::Fresh => {
                    // The creator's own settings, never the main's: a side
                    // creator keeps its model and its access ceiling.
                    let base = creator_chat.config.clone().ok_or_else(|| {
                        BoardError::invalid(
                            "callerTaskId",
                            "send a message in this chat first to choose its provider",
                        )
                    })?;
                    let config = self
                        .board_derived_execution(&base, profile.as_ref())
                        .await?;
                    self.board_create_fresh_side(
                        &main_chat,
                        &chat_id,
                        &request_id,
                        wanted_title.as_deref(),
                        config,
                    )
                    .await?;
                }
                BoardContextMode::WithContext {
                    checkpoint_message_id,
                } => {
                    self.create_side_chat_fork(
                        &main_id,
                        &chat_id,
                        checkpoint_message_id.as_deref(),
                        None,
                        !native,
                        None,
                    )
                    .await
                    .map_err(|error| match error {
                        RpcError::Failed(message) => BoardError::from_rpc_message(&message)
                            .unwrap_or(BoardError::Unavailable { reason: message }),
                        other => BoardError::Unavailable {
                            reason: other.to_string(),
                        },
                    })?;
                }
            }
            let _ = self.place_child_in_background(&main_id, &chat_id);
        }
        if let Some(title) = &wanted_title {
            self.board_rename_working_chat(&chat_id, title, 0).await?;
        }
        let container = OriginalContainer::SideOf {
            main_task_id: main_id.clone(),
        };
        if let Some(item) = &ticket {
            self.board_link_worker(
                caller,
                &request_id,
                BoardOperation::CreateSideChat,
                &item.id,
                &chat_id,
                TaskLinkRole::Working,
                container.clone(),
                BoardChatRole::Side,
            )?;
        }
        if let Some(item) = &reviewer_item {
            self.board_link_worker(
                caller,
                &request_id,
                BoardOperation::ReviewStart,
                &item.id,
                &chat_id,
                TaskLinkRole::Reviewer,
                container.clone(),
                BoardChatRole::Side,
            )?;
        }
        let launch = match prompt {
            Some(prompt) if request.start => self.board_send_first(&chat_id, prompt).await?,
            _ => BoardChatLaunchState::Created,
        };
        let resolved = self.board_resolve_chat(&chat_id)?;
        Ok(BoardChatCreated {
            request_id,
            task_id: chat_id,
            role: resolved.identity.role,
            main_parent_id: resolved.identity.main_parent_id.clone(),
            creator: resolved.identity.creator.clone(),
            context_origin: resolved.identity.context_origin,
            work_item_id: ticket
                .as_ref()
                .or(reviewer_item.as_ref())
                .map(|item| item.id.clone()),
            launch,
            settings: self.board_chat_settings(&resolved.chat, settings_sources(origin))?,
            navigation: Self::board_identity_navigation(&resolved.identity),
            reused: false,
        })
    }

    // ---- create_main_chat ----

    /// A distinct new independent main chat (Cmd+N semantics) with an
    /// optional "Handed off from…" link that is neither parentage nor fork
    /// provenance. The caller never moves, never becomes its child, gains no
    /// lifecycle ownership. `start: false` saves without sending.
    pub(crate) async fn board_create_main_chat(
        &self,
        caller: &BoardCaller,
        request: BoardCreateMainChatRequest,
    ) -> BoardResult<BoardChatCreated> {
        self.board_spawn_main_chat(caller, request, true).await
    }

    /// The same creation for a caller that already holds
    /// `board_hub().gate(ticket)` (Board Start): the gate is not re-taken.
    pub(crate) async fn board_create_main_chat_gated(
        &self,
        caller: &BoardCaller,
        request: BoardCreateMainChatRequest,
    ) -> BoardResult<BoardChatCreated> {
        self.board_spawn_main_chat(caller, request, false).await
    }

    async fn board_spawn_main_chat(
        &self,
        caller: &BoardCaller,
        request: BoardCreateMainChatRequest,
        take_gate: bool,
    ) -> BoardResult<BoardChatCreated> {
        let request_id = caller.normalized_request_id(&request.request_id)?;
        let from_id = match caller {
            BoardCaller::Agent { task_id, .. } => {
                if request
                    .from_task_id
                    .as_ref()
                    .is_some_and(|from| !from.is_empty() && from != task_id)
                {
                    return Err(BoardError::NotAuthorized {
                        reason: "A chat's tools act for that chat only; fromTaskId must name the calling chat."
                            .into(),
                    });
                }
                Some(task_id.clone())
            }
            // A temporary discussion is not a chat: it hands nothing off.
            BoardCaller::Authoring { .. } if request.from_task_id.is_some() => {
                return Err(BoardError::invalid(
                    "fromTaskId",
                    "a temporary ticket discussion is not a chat to hand off from",
                ));
            }
            _ => request.from_task_id.clone().filter(|from| !from.is_empty()),
        };
        let brief = request
            .handoff_brief
            .as_deref()
            .map(str::trim)
            .filter(|brief| !brief.is_empty())
            .map(str::to_owned);
        if brief
            .as_ref()
            .is_some_and(|brief| brief.len() > HANDOFF_BRIEF_MAX_BYTES)
        {
            return Err(BoardError::invalid(
                "handoffBrief",
                format!(
                    "keep the brief under {} KiB; it is a summary, not a transcript",
                    HANDOFF_BRIEF_MAX_BYTES / 1024
                ),
            ));
        }
        if request.start && request.ticket.is_some() {
            return Err(BoardError::invalid(
                "start",
                "a ticket's first message comes from board_start (it freezes the launch); create its worker with start: false",
            ));
        }
        if request.start && brief.is_none() {
            return Err(BoardError::invalid(
                "handoffBrief",
                "nothing to send: write the brief the new chat starts from, or create it without start",
            ));
        }
        let title = request.title.as_deref().map(clean_chat_title).transpose()?;
        let space = self.board_project(caller, &request.project_id, BoardAccess::Write)?;
        let from = match &from_id {
            Some(from_id) => {
                let identity =
                    self.board_validate_creation(from_id, BoardChatCreationKind::MainChat)?;
                let chat = self
                    .workspace
                    .chat(from_id)
                    .map_err(storage)?
                    .ok_or_else(|| BoardError::invalid("fromTaskId", "that chat does not exist"))?;
                self.board_check_chat(caller, &chat, BoardAccess::Read)?;
                if identity.role == BoardChatRole::Side && brief.is_none() {
                    return Err(BoardError::invalid(
                        "handoffBrief",
                        "a handoff from a side chat needs a written brief: goal, completed work, relevant files, decisions, constraints and the next step",
                    ));
                }
                Some(chat)
            }
            None => None,
        };
        let chat_id = derived_task_id("main", &request_id);
        let mut payload = request.clone();
        payload.request_id.clear();
        payload.from_task_id = from_id.clone();
        let payload_hash = board_digest(&("createMainChat", &payload))?;
        let _gate = match &request.ticket {
            Some(binding) if take_gate => Some(self.board_hub().gate(&binding.work_item_id).await),
            _ => None,
        };
        let ticket = match &request.ticket {
            Some(binding) => match self
                .board_binding_target(caller, &request_id, binding, &space.id, &chat_id)
                .await?
            {
                Ok(item) => Some(item),
                Err(reused) => return Ok(*reused),
            },
            None => None,
        };
        let explicit = request
            .execution
            .clone()
            .filter(|profile| !profile.is_inherited());
        let saved = ticket
            .as_ref()
            .map(|item| item.implementation.clone())
            .filter(|profile| !profile.is_inherited() && !profile.deferred);
        let mut origin = if explicit.is_some() {
            BoardSettingOrigin::StartOverride
        } else if saved.is_some() {
            BoardSettingOrigin::Ticket
        } else if from.is_some() {
            BoardSettingOrigin::Parent
        } else {
            BoardSettingOrigin::Project
        };
        let profile = explicit.or(saved);
        self.board_claim_creation(&request_id, &payload_hash, &chat_id)?;

        if self.workspace.chat(&chat_id).map_err(storage)?.is_none() {
            // A handoff inherits its source's settings and never exceeds its
            // access; a standalone chat uses the project's defaults.
            let base = match from.as_ref().and_then(|chat| chat.config.clone()) {
                Some(config) => Some(config),
                None => {
                    let defaults = &space.execution_defaults;
                    let harness = profile
                        .as_ref()
                        .and_then(|profile| profile.harness)
                        .or(defaults.provider.as_ref().map(|provider| provider.harness));
                    harness.map(|harness| {
                        let model = defaults
                            .provider
                            .as_ref()
                            .filter(|provider| provider.harness == harness)
                            .and_then(|provider| provider.model.clone());
                        let mode = profile
                            .as_ref()
                            .and_then(|profile| profile.permission_mode)
                            .or(defaults.permission_mode)
                            .unwrap_or(PermissionMode::Ask);
                        standalone_config(harness, model, mode)
                    })
                }
            };
            let base = base.ok_or_else(|| {
                BoardError::invalid(
                    "execution",
                    "choose a provider for the new chat (execution.harness)",
                )
            })?;
            if from.is_none() && profile.is_none() {
                origin = BoardSettingOrigin::Project;
            }
            let config = self
                .board_derived_execution(&base, profile.as_ref())
                .await?;
            self.board_record_provenance(&ChatProvenanceRecord {
                task_id: chat_id.clone(),
                creator: from_id.clone().map(|task_id| ChatProvenance {
                    task_id,
                    relation: ChatProvenanceRelation::CreatedBy,
                }),
                source: from_id
                    .clone()
                    .filter(|_| brief.is_some())
                    .map(|task_id| ChatProvenance {
                        task_id,
                        relation: ChatProvenanceRelation::HandedOffFrom,
                    }),
                context_origin: if brief.is_some() {
                    BoardContextOrigin::HandoffBrief
                } else {
                    BoardContextOrigin::Fresh
                },
                role: BoardChatRole::Main,
                main_parent_id: None,
                request_id: request_id.clone(),
                created_at_ms: crate::now_ms(),
            })?;
            // Cmd+N: an ordinary project chat with no lineage at all.
            let mut command = legacy_task_command(
                chat_id.clone(),
                TaskCommandIntent::CreateChat {
                    side_parent_id: None,
                    space_id: Some(space.id.clone()),
                    device_id: None,
                    config: Some(config),
                    branch: None,
                    cwd: None,
                    lineage: TaskLineage::default(),
                },
            );
            command.command_id = format!("board-main:{chat_id}");
            command.actor_id = "luna-board:main-chat".into();
            let receipt = self
                .execute_task_command(command, true)
                .await
                .map_err(|error| BoardError::Unavailable {
                    reason: format!("Main chat creation did not complete: {error}"),
                })?;
            if receipt.status != CommandReceiptStatus::Applied {
                return Err(BoardError::Unavailable {
                    reason: format!(
                        "Main chat creation was refused: {}",
                        receipt
                            .reason
                            .as_ref()
                            .map_or("no reason recorded", |reason| reason.message.as_str())
                    ),
                });
            }
        }
        let wanted_title = ticket.as_ref().map(|item| item.title.clone()).or(title);
        if let Some(title) = &wanted_title {
            self.board_rename_working_chat(&chat_id, title, 0).await?;
        }
        if let Some(item) = &ticket {
            self.board_link_worker(
                caller,
                &request_id,
                BoardOperation::CreateMainChat,
                &item.id,
                &chat_id,
                TaskLinkRole::Working,
                OriginalContainer::MainChat,
                BoardChatRole::Main,
            )?;
        }
        let launch = match &brief {
            Some(brief) if request.start => self.board_send_first(&chat_id, brief).await?,
            _ => BoardChatLaunchState::Created,
        };
        let resolved = self.board_resolve_chat(&chat_id)?;
        Ok(BoardChatCreated {
            request_id,
            task_id: chat_id,
            role: resolved.identity.role,
            main_parent_id: None,
            creator: resolved.identity.creator.clone(),
            context_origin: resolved.identity.context_origin,
            work_item_id: ticket.as_ref().map(|item| item.id.clone()),
            launch,
            settings: self.board_chat_settings(&resolved.chat, settings_sources(origin))?,
            navigation: Self::board_identity_navigation(&resolved.identity),
            reused: false,
        })
    }
}
