//! Conversion in place, source/reference/attempt links and explicit
//! replacement attempts. Every Working link freezes the chat's canonical
//! container once; no path rewrites it.
use super::*;
use luna_proto::{
    BoardConvertChatRequest, BoardConvertReceipt, BoardCreateRequest, BoardItemOrigin,
    BoardLinkChatRequest, BoardRunAssociation,
};
use luna_sync::BoardRowWrite;

/// Link commits retry a concurrent ticket edit this many times.
const LINK_ATTEMPTS: usize = 3;

/// The container a chat lives in, from its canonical identity.
pub(crate) fn identity_container(identity: &CanonicalChatIdentity) -> OriginalContainer {
    match (&identity.original_container, identity.role) {
        (Some(container), _) => container.clone(),
        (None, BoardChatRole::Main) => OriginalContainer::MainChat,
        (None, BoardChatRole::Side) => OriginalContainer::SideOf {
            main_task_id: identity
                .main_parent_id
                .clone()
                .unwrap_or_else(|| identity.display_container_id.clone()),
        },
    }
}

fn container_parent(container: &OriginalContainer) -> Option<String> {
    match container {
        OriginalContainer::MainChat => None,
        OriginalContainer::SideOf { main_task_id } => Some(main_task_id.clone()),
    }
}

impl EngineRpc {
    /// The run association recorded when a chat was converted.
    pub(crate) fn board_converted_run(
        &self,
        work_item_id: &str,
    ) -> BoardResult<Option<BoardRunAssociation>> {
        self.board_kv_load(&kv_key("conversion", work_item_id))
    }

    /// What is running in a chat right now (LUNA identities only).
    fn board_run_association(&self, task_id: &str) -> BoardRunAssociation {
        let binding = self
            .task_identity
            .view(task_id)
            .and_then(|view| view.current_binding);
        let active_turn_id = self.sessions.active_turn_id(task_id);
        BoardRunAssociation {
            task_id: task_id.into(),
            running: self.sessions.has_live_run(task_id) || active_turn_id.is_some(),
            active_message_id: self
                .sessions
                .work_runtime(task_id)
                .map(|runtime| runtime.message_id)
                .filter(|_| active_turn_id.is_some()),
            active_turn_id,
            binding_id: binding.as_ref().map(|binding| binding.binding_id.clone()),
            binding_generation: binding.as_ref().map(|binding| binding.generation),
            observed_at_ms: crate::now_ms(),
        }
    }

    /// Convert the caller's existing ordinary chat to a ticket in place: one
    /// atomic create + Working link (`board_insert_item` with origin
    /// `Converted` and event `Converted`). Preserves id, history, provider
    /// binding, placement, draft and active run; sends nothing.
    pub(crate) async fn board_convert_chat(
        &self,
        caller: &BoardCaller,
        request: BoardConvertChatRequest,
    ) -> BoardResult<BoardConvertReceipt> {
        if let BoardCaller::Authoring { .. } = caller {
            return Err(BoardError::NotConvertible {
                reason: "A temporary ticket discussion is not a chat; save its ticket with board_create."
                    .into(),
            });
        }
        let task_id = self.board_bound_chat(caller, &request.task_id, "taskId", "convertChat")?;
        let request_id = caller.normalized_request_id(&request.request_id)?;
        if self.board_not_a_chat(&task_id)? {
            return Err(BoardError::NotConvertible {
                reason: "Temporary ticket discussions and native agent transcripts are not chats and cannot be converted."
                    .into(),
            });
        }
        let item_id = format!("wi-{}", &board_digest(&("board-item", &request_id))?[..32]);
        // A retry returns the committed conversion, whatever the chat's
        // title or links say now.
        if let Some(receipt) = self
            .store
            .board_receipt(&request_id)
            .map_err(board_store_error)?
        {
            if receipt.operation != BoardOperation::ConvertChat
                || receipt.task_id.as_deref() != Some(task_id.as_str())
            {
                return Err(BoardError::IdempotencyConflict { request_id });
            }
            return self.board_conversion_receipt(receipt, &task_id);
        }
        let resolved = self.board_resolve_chat(&task_id)?;
        self.board_check_chat(caller, &resolved.chat, BoardAccess::Write)?;
        if let Some(link) = &resolved.working {
            let key = self.board_item_key(&link.work_item_id)?;
            return Err(BoardError::NotConvertible {
                reason: format!(
                    "This chat already works on {key}; open that ticket instead of converting again."
                ),
            });
        }
        if resolved.reviewer.is_some() {
            return Err(BoardError::NotConvertible {
                reason: "Reviewer chats report to their ticket and cannot become tickets.".into(),
            });
        }
        if resolved.chat.archived {
            return Err(BoardError::NotConvertible {
                reason: "Unarchive this chat before converting it.".into(),
            });
        }
        let project_id =
            resolved
                .chat
                .space_id
                .clone()
                .ok_or_else(|| BoardError::NotConvertible {
                    reason: "This chat has no project.".into(),
                })?;
        let title = request
            .title
            .clone()
            .filter(|title| !title.trim().is_empty())
            .or_else(|| resolved.chat.title.clone())
            .filter(|title| !title.trim().is_empty())
            .ok_or_else(|| {
                BoardError::invalid("title", "this chat has no title yet; name the ticket")
            })?;
        let title = clean_chat_title(&title)?;
        // Frozen from the chat's canonical relationship, never from layout.
        let container = identity_container(&resolved.identity);
        let run_key = kv_key("conversion", &item_id);
        if self
            .board_kv_load::<BoardRunAssociation>(&run_key)?
            .is_none()
        {
            self.board_kv_save(&run_key, &self.board_run_association(&task_id))?;
        }
        let link = WorkItemTaskLink {
            id: format!(
                "wl-{}",
                &board_digest(&("working", &request_id, &task_id))?[..32]
            ),
            work_item_id: String::new(),
            task_id: task_id.clone(),
            role: TaskLinkRole::Working,
            active: true,
            chat_role: Some(resolved.identity.role),
            original_container: Some(container),
            linked_by: caller.actor(),
            linked_at_ms: crate::now_ms(),
            unlinked_at_ms: None,
            source_message_id: None,
            context_refs: Vec::new(),
            launch_snapshot_id: None,
            result_subscriptions: Vec::new(),
        };
        let create = BoardCreateRequest {
            request_id: request.request_id.clone(),
            project_id: Some(project_id),
            title: title.clone(),
            description: request.description.clone(),
            implementation: request.implementation.clone(),
            review: request.review.clone(),
            completion: request.completion.clone(),
            labels: Vec::new(),
            priority: None,
            attachments: Vec::new(),
            input_items: Vec::new(),
            context_refs: Vec::new(),
            origin: None,
            placement: Default::default(),
        };
        let receipt = self.board_insert_item(
            caller,
            BoardOperation::ConvertChat,
            create,
            BoardItemOrigin::Converted {
                task_id: task_id.clone(),
            },
            vec![BoardRowWrite::TaskLink(Box::new(link))],
            Some(WorkItemEventKind::Converted {
                task_id: task_id.clone(),
            }),
            Some(task_id.clone()),
        )?;
        // The ticket and its working chat share one title; nothing is sent.
        self.board_rename_working_chat(&task_id, &receipt.item.title, receipt.revision)
            .await?;
        self.board_conversion_receipt(receipt, &task_id)
    }

    fn board_conversion_receipt(
        &self,
        mut mutation: BoardMutationReceipt,
        task_id: &str,
    ) -> BoardResult<BoardConvertReceipt> {
        // The stored receipt predates its own link; Go to chat is derived
        // from the committed relationship on every read.
        mutation.navigation = self.board_default_navigation(&mutation.item);
        let link = self
            .store
            .board_task_links_for_item(&mutation.work_item_id, false)
            .map_err(board_store_error)?
            .into_iter()
            .find(|link| link.task_id == task_id && link.original_container.is_some())
            .ok_or_else(|| storage("the conversion link is missing"))?;
        let identity = self.board_canonical_identity(task_id)?;
        let run = self.board_converted_run(&mutation.work_item_id)?;
        Ok(BoardConvertReceipt {
            mutation,
            link,
            identity,
            run,
        })
    }

    /// Link/unlink a Source, Reference or Attempt chat. Working links only
    /// come from Start or conversion; linking never resends context. An
    /// Attempt must live in the ticket's frozen original container.
    pub(crate) async fn board_link_chat(
        &self,
        caller: &BoardCaller,
        request: BoardLinkChatRequest,
    ) -> BoardResult<BoardMutationReceipt> {
        let request_id = caller.normalized_request_id(&request.request_id)?;
        let mut payload = request.clone();
        payload.request_id.clear();
        let hash = board_digest(&(BoardOperation::LinkChat.as_str(), &payload))?;
        if let Some(receipt) = self.board_replay(&request_id, &hash)? {
            return Ok(receipt);
        }
        match request.role {
            TaskLinkRole::Working => {
                return Err(BoardError::InvalidRole {
                    operation: "linkChat".into(),
                    reason: "A ticket's working chat comes only from Start or from converting that chat."
                        .into(),
                    allowed: vec!["board_start".into(), "board_convert_chat".into()],
                });
            }
            TaskLinkRole::Reviewer => {
                return Err(BoardError::InvalidRole {
                    operation: "linkChat".into(),
                    reason: "Reviewer chats are created by review_start.".into(),
                    allowed: vec!["review_start".into()],
                });
            }
            TaskLinkRole::Source | TaskLinkRole::Reference | TaskLinkRole::Attempt => {}
        }
        for attempt in 0..LINK_ATTEMPTS {
            let item = self.board_item_for(caller, &request.work_item_id, BoardAccess::Write)?;
            if request.expected_revision > item.revision {
                return Err(BoardError::invalid(
                    "expectedRevision",
                    "is newer than the ticket",
                ));
            }
            let resolved = self.board_resolve_chat(&request.task_id)?;
            self.board_check_chat(caller, &resolved.chat, BoardAccess::Read)?;
            if resolved.chat.space_id.as_deref() != Some(item.project_id.as_str()) {
                return Err(BoardError::invalid(
                    "taskId",
                    "link chats from the ticket's own project",
                ));
            }
            let links = self
                .store
                .board_task_links_for_item(&item.id, true)
                .map_err(board_store_error)?;
            let existing = links
                .iter()
                .find(|link| link.task_id == request.task_id && link.role == request.role)
                .cloned();
            let now = crate::now_ms();
            let change = if request.linked {
                if existing.is_some() {
                    None
                } else {
                    let mut original_container = None;
                    if request.role == TaskLinkRole::Attempt {
                        let chat_container = identity_container(&resolved.identity);
                        let frozen = links
                            .iter()
                            .filter(|link| {
                                matches!(link.role, TaskLinkRole::Working | TaskLinkRole::Attempt)
                            })
                            .find_map(|link| link.original_container.clone());
                        if links.iter().any(|link| {
                            link.role == TaskLinkRole::Working && link.task_id == request.task_id
                        }) {
                            return Err(BoardError::invalid(
                                "taskId",
                                "that chat is the ticket's current working chat",
                            ));
                        }
                        if let Some(frozen) = frozen
                            && frozen != chat_container
                        {
                            // An attempt never moves the ticket's work to
                            // another container (no reparenting route).
                            return Err(BoardError::ParentLocked {
                                task_id: request.task_id.clone(),
                                original_parent_id: container_parent(&frozen),
                            });
                        }
                        original_container = Some(chat_container);
                    }
                    Some((
                        WorkItemTaskLink {
                            id: format!(
                                "wl-{}",
                                &board_digest(&(
                                    request.role.as_str(),
                                    &request_id,
                                    &request.task_id
                                ))?[..32]
                            ),
                            work_item_id: item.id.clone(),
                            task_id: request.task_id.clone(),
                            role: request.role,
                            active: true,
                            chat_role: Some(resolved.identity.role),
                            original_container,
                            linked_by: caller.actor(),
                            linked_at_ms: now,
                            unlinked_at_ms: None,
                            source_message_id: None,
                            context_refs: Vec::new(),
                            launch_snapshot_id: None,
                            result_subscriptions: Vec::new(),
                        },
                        WorkItemEventKind::ChatLinked {
                            task_id: request.task_id.clone(),
                            role: request.role,
                        },
                    ))
                }
            } else {
                existing.map(|mut link| {
                    link.active = false;
                    link.unlinked_at_ms = Some(now);
                    (
                        link,
                        WorkItemEventKind::ChatUnlinked {
                            task_id: request.task_id.clone(),
                            role: request.role,
                        },
                    )
                })
            };
            let mut next = item.clone();
            let (rows, event) = match change {
                Some((link, event)) => {
                    next.revision += 1;
                    next.updated_at_ms = now;
                    next.updated_by = caller.actor();
                    (
                        vec![BoardRowWrite::TaskLink(Box::new(link))],
                        Some((event, true)),
                    )
                }
                // Already in the requested state: a receipt, no event.
                None => (Vec::new(), None),
            };
            let mut receipt =
                self.board_title_receipt(caller, &next, Some(request.task_id.clone()));
            receipt.operation = BoardOperation::LinkChat;
            match self.board_commit(BoardCommit {
                request_id: request_id.clone(),
                payload_hash: hash.clone(),
                committed_at_ms: now,
                item: BoardItemWrite::Replace {
                    expected_revision: item.revision,
                    item: Box::new(next),
                    placement: None,
                },
                rows,
                event,
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

    /// An explicit replacement attempt (A2 Retry with another attempt): the
    /// current worker stays linked as an Attempt (its title and history kept)
    /// and `new_task_id` becomes the Working chat — only inside the ticket's
    /// frozen original container, which the new link copies verbatim.
    pub(crate) fn board_replace_worker(
        &self,
        caller: &BoardCaller,
        request_id: &str,
        work_item_id: &str,
        new_task_id: &str,
    ) -> BoardResult<BoardMutationReceipt> {
        let link_request = format!("{request_id}:replace");
        if let Some(receipt) = self
            .store
            .board_receipt(&link_request)
            .map_err(board_store_error)?
        {
            return Ok(receipt);
        }
        for attempt in 0..LINK_ATTEMPTS {
            let item = self.board_item_for(caller, work_item_id, BoardAccess::Write)?;
            let current = self.board_working_link(&item.id)?.ok_or_else(|| {
                BoardError::invalid(
                    "workItemId",
                    "the ticket has no worker to replace; start it instead",
                )
            })?;
            let container = current
                .original_container
                .clone()
                .ok_or_else(|| storage("working link without its original container"))?;
            let identity = self.board_canonical_identity(new_task_id)?;
            if current.task_id == new_task_id
                || identity_container(&identity) != container
                || identity.parent_locked
            {
                return Err(BoardError::ParentLocked {
                    task_id: current.task_id.clone(),
                    original_parent_id: container_parent(&container),
                });
            }
            let now = crate::now_ms();
            let mut previous = current.clone();
            previous.role = TaskLinkRole::Attempt;
            let replacement = WorkItemTaskLink {
                id: format!(
                    "wl-{}",
                    &board_digest(&("working", request_id, new_task_id))?[..32]
                ),
                work_item_id: item.id.clone(),
                task_id: new_task_id.into(),
                role: TaskLinkRole::Working,
                active: true,
                chat_role: Some(identity.role),
                original_container: Some(container),
                linked_by: caller.actor(),
                linked_at_ms: now,
                unlinked_at_ms: None,
                source_message_id: None,
                context_refs: Vec::new(),
                launch_snapshot_id: None,
                result_subscriptions: Vec::new(),
            };
            let mut next = item.clone();
            next.revision += 1;
            next.updated_at_ms = now;
            next.updated_by = caller.actor();
            let mut receipt = self.board_title_receipt(caller, &next, Some(new_task_id.into()));
            receipt.operation = BoardOperation::LinkChat;
            match self.board_commit(BoardCommit {
                request_id: link_request.clone(),
                payload_hash: board_digest(&("replace", work_item_id, new_task_id))?,
                committed_at_ms: now,
                item: BoardItemWrite::Replace {
                    expected_revision: item.revision,
                    item: Box::new(next),
                    placement: None,
                },
                // Demote first: one active Working link per ticket.
                rows: vec![
                    BoardRowWrite::TaskLink(Box::new(previous)),
                    BoardRowWrite::TaskLink(Box::new(replacement)),
                ],
                event: Some((
                    WorkItemEventKind::ChatLinked {
                        task_id: new_task_id.into(),
                        role: TaskLinkRole::Working,
                    },
                    true,
                )),
                receipt: Some(receipt),
            }) {
                Ok(Some(receipt)) => return Ok(receipt),
                Ok(None) => return Err(storage("replacement returned no receipt")),
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
}
