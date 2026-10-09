//! Chat-model slice (A3, PB-1): canonical main/side role resolution from
//! durable lineage; `create_side_chat` (fresh/context, main callers only;
//! side callers get a fresh sibling under the same main) and
//! `create_main_chat` (independent root, written handoff brief from a side
//! chat); conversion in place; source/reference links; Go-to-chat
//! navigation; ticket ↔ working-chat title sync; the fixed original parent;
//! legacy nested child/fork migration; unavailable-parent recovery.
//!
//! Contract: docs/design/project-board/pb0-contract.md §4 and §6. The rules
//! are enforced here for every caller (UI, tools, RPC); hiding a control is
//! never the enforcement. Creation reuses the child-thread, side-chat and
//! task-command machinery; nothing here reparents a ticket worker.
//!
//! Lineage sources, in order: the registry side-chat row (`sideChatParentId`)
//! or a side-chat service reservation, `ChildThread.parent_task_id`, then the
//! task-identity lineage (`parentTaskId` of legacy snapshot forks,
//! `sourceTaskId` of persistent history forks). Creator and "handed off
//! from" provenance are not lineage: they are stored in a write-once local
//! record (`luna-board-chat:provenance:<task>`) so a handoff never looks like
//! a fork and never leaves the sidebar.
// Helpers for the lifecycle (A2), tools (A4) and review (A5) slices are
// called once those slices land.
#![allow(dead_code)]
use super::*;
use luna_proto::{
    BoardChatBadges, BoardChatBadgesRequest, BoardChatCreationKind, BoardChatIdentityRequest,
    BoardChatRole, BoardContextOrigin, BoardNavigationRequest, BoardNavigationTarget,
    BoardOperation, BoardTicketContext, BoardTitleOrigin, BoardTitleSyncReceipt,
    CanonicalChatIdentity, ChatPermittedActions, ChatProvenance, ChatProvenanceRelation,
    ChatPurpose, ChatTicketBadge, OriginalContainer, ResolvedExecution, TaskLinkRole, WorkItem,
    WorkItemEventKind, WorkItemTaskLink,
};
use luna_sync::{BoardCommit, BoardItemWrite};

mod create;
mod links;
mod migration;

/// Longest lineage walk to a main chat (legacy nesting is at most four
/// levels; anything longer is treated as a broken chain).
const LINEAGE_WALK_MAX: usize = 16;
/// Badge lookups per call (sidebar rows and tab strips are bounded).
const BADGES_MAX: usize = 200;
/// Title-sync commits retry a concurrent ticket change this many times.
const TITLE_ATTEMPTS: usize = 3;

fn storage(error: impl std::fmt::Display) -> BoardError {
    BoardError::Storage {
        message: error.to_string(),
    }
}

/// Local key/value record under the profile store (backed up with
/// `docs.sqlite3`, never synced).
fn kv_key(kind: &str, id: &str) -> String {
    format!("luna-board-chat:{kind}:{id}")
}

/// A stable UUID task id derived from a namespaced seed.
fn derived_task_id(namespace: &str, seed: &str) -> String {
    use sha2::Digest as _;
    let digest = sha2::Sha256::digest(format!("luna-board-chat-v1\0{namespace}\0{seed}"));
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    uuid::Uuid::from_bytes(bytes).to_string()
}

/// Validate a title the same way the store does (one trimmed line).
fn clean_chat_title(title: &str) -> BoardResult<String> {
    let title = title.trim();
    if title.is_empty() {
        return Err(BoardError::invalid("title", "enter a title"));
    }
    if title.chars().count() > luna_proto::BOARD_TITLE_MAX_CHARS
        || title.chars().any(char::is_control)
    {
        return Err(BoardError::invalid(
            "title",
            format!(
                "use one line of at most {} characters",
                luna_proto::BOARD_TITLE_MAX_CHARS
            ),
        ));
    }
    Ok(title.into())
}

fn kind_operation(kind: BoardChatCreationKind) -> &'static str {
    match kind {
        BoardChatCreationKind::SideFresh => "createSideChat",
        BoardChatCreationKind::SideWithContext => "createSideChatWithContext",
        BoardChatCreationKind::NativeFork => "nativeFork",
        BoardChatCreationKind::MessageFork => "messageFork",
        BoardChatCreationKind::ChildThread => "createChildThread",
        BoardChatCreationKind::MainChat => "createMainChat",
    }
}

/// Write-once creation provenance for a chat this slice created.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ChatProvenanceRecord {
    pub task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creator: Option<ChatProvenance>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<ChatProvenance>,
    pub context_origin: BoardContextOrigin,
    pub role: BoardChatRole,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub main_parent_id: Option<String>,
    pub request_id: String,
    pub created_at_ms: i64,
}

/// How a chat's immediate parent is recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EdgeKind {
    /// Side-chat service row (`sideChatParentId` or a reservation).
    SideRow,
    /// `ChildThread` metadata (child chats and fresh side chats).
    Child,
    /// Persistent history fork (`TaskLineage.source_task_id`).
    Fork,
    /// Legacy snapshot fork recorded as `TaskLineage.parent_task_id`.
    LegacyParent,
}

/// The canonical identity plus the evidence callers need without a second
/// lookup.
pub(crate) struct ResolvedChat {
    pub chat: Chat,
    pub identity: CanonicalChatIdentity,
    /// The immediate recorded parent (a legacy nested record's direct
    /// parent differs from its main parent).
    pub immediate_parent_id: Option<String>,
    pub working: Option<WorkItemTaskLink>,
    pub reviewer: Option<WorkItemTaskLink>,
}

impl EngineRpc {
    // ---- local records ----

    pub(crate) fn board_kv_load<T: serde::de::DeserializeOwned>(
        &self,
        key: &str,
    ) -> BoardResult<Option<T>> {
        self.store
            .load_snapshot(key)
            .map_err(board_store_error)?
            .map(|bytes| serde_json::from_slice(&bytes).map_err(storage))
            .transpose()
    }

    pub(crate) fn board_kv_save<T: Serialize>(&self, key: &str, value: &T) -> BoardResult<()> {
        let bytes = serde_json::to_vec(value).map_err(storage)?;
        self.board_save_snapshot(key, &bytes)
    }

    /// Write-once provenance: an existing record for the chat is kept (a
    /// retry never rewrites who created it).
    pub(crate) fn board_record_provenance(&self, record: &ChatProvenanceRecord) -> BoardResult<()> {
        let key = kv_key("provenance", &record.task_id);
        if self.board_kv_load::<ChatProvenanceRecord>(&key)?.is_some() {
            return Ok(());
        }
        self.board_kv_save(&key, record)
    }

    pub(crate) fn board_provenance(
        &self,
        task_id: &str,
    ) -> BoardResult<Option<ChatProvenanceRecord>> {
        self.board_kv_load(&kv_key("provenance", task_id))
    }

    // ---- lineage ----

    fn board_parent_edge(&self, task_id: &str, chat: Option<&Chat>) -> Option<(String, EdgeKind)> {
        if let Some(parent) = self
            .workspace
            .side_chat_parent(task_id)
            .filter(|parent| parent != task_id)
        {
            return Some((parent, EdgeKind::SideRow));
        }
        if let Some(service) = &self.side_chats
            && let Ok(Some((parent, state))) = service.layout_identity(task_id)
            && state != "deleted"
            && parent != task_id
        {
            return Some((parent, EdgeKind::SideRow));
        }
        if let Some(child) = chat.and_then(|chat| chat.child_thread.as_ref())
            && child.parent_task_id != task_id
        {
            return Some((child.parent_task_id.clone(), EdgeKind::Child));
        }
        let lineage = self.task_identity.view(task_id)?.task.lineage;
        if let Some(parent) = lineage.parent_task_id.filter(|parent| parent != task_id) {
            return Some((parent, EdgeKind::LegacyParent));
        }
        lineage
            .source_task_id
            .filter(|source| source != task_id)
            .map(|source| (source, EdgeKind::Fork))
    }

    /// A parent is available when its row exists, is not archived and its
    /// identity was not deleted.
    fn board_chat_available(&self, task_id: &str) -> BoardResult<bool> {
        let chat = self.workspace.chat(task_id).map_err(storage)?;
        Ok(chat.is_some_and(|chat| !chat.archived)
            && !self
                .task_identity
                .view(task_id)
                .is_some_and(|view| view.task.tombstoned))
    }

    /// Whether `task_id` is something other than an ordinary chat.
    fn board_not_a_chat(&self, task_id: &str) -> BoardResult<bool> {
        Ok(self.derived_fork_document(task_id)
            || self
                .store
                .is_board_authoring_session(task_id)
                .map_err(board_store_error)?)
    }

    /// Canonical identity with its evidence, from durable records only.
    pub(crate) fn board_resolve_chat(&self, task_id: &str) -> BoardResult<ResolvedChat> {
        if self.board_not_a_chat(task_id)? {
            return Err(BoardError::InvalidRole {
                operation: "chatIdentity".into(),
                reason: "Temporary ticket discussions and native agent transcripts are not chats."
                    .into(),
                allowed: Vec::new(),
            });
        }
        let chat = self
            .workspace
            .chat(task_id)
            .map_err(storage)?
            .filter(|_| {
                !self
                    .task_identity
                    .view(task_id)
                    .is_some_and(|view| view.task.tombstoned)
            })
            .ok_or_else(|| BoardError::invalid("taskId", "that chat does not exist"))?;
        let links = self
            .store
            .board_task_links_for_task(task_id, true)
            .map_err(board_store_error)?;
        let working = links
            .iter()
            .find(|link| link.role == TaskLinkRole::Working)
            .cloned();
        let reviewer = links
            .iter()
            .find(|link| link.role == TaskLinkRole::Reviewer)
            .cloned();
        let provenance = self.board_provenance(task_id)?;
        let edge = self.board_parent_edge(task_id, Some(&chat));

        let (role, main_parent_id, immediate, legacy_nested, mut parent_unavailable, edge_kind) =
            match &edge {
                None => (BoardChatRole::Main, None, None, false, false, None),
                Some((immediate, kind)) => {
                    let mut seen = vec![task_id.to_owned(), immediate.clone()];
                    let mut cursor = immediate.clone();
                    let mut broken = false;
                    for _ in 0..LINEAGE_WALK_MAX {
                        let parent = self.workspace.chat(&cursor).map_err(storage)?;
                        let Some(parent) = parent else {
                            // A deleted ancestor ends the walk: keep the last
                            // known reference as a tombstoned parent.
                            broken = true;
                            break;
                        };
                        match self.board_parent_edge(&cursor, Some(&parent)) {
                            None => break,
                            Some((next, _)) if seen.contains(&next) => {
                                broken = true;
                                break;
                            }
                            Some((next, _)) => {
                                seen.push(next.clone());
                                cursor = next;
                            }
                        }
                    }
                    let unavailable = broken || !self.board_chat_available(&cursor)?;
                    (
                        BoardChatRole::Side,
                        Some(cursor.clone()),
                        Some(immediate.clone()),
                        &cursor != immediate,
                        unavailable,
                        Some(*kind),
                    )
                }
            };

        // Ticket workers and reviewers display in their frozen container.
        let frozen = working
            .as_ref()
            .or(reviewer.as_ref())
            .and_then(|link| link.original_container.clone());
        let migrated = legacy_nested && self.board_legacy_migrated(task_id)?;
        let display_container_id = match (&frozen, role) {
            (Some(OriginalContainer::MainChat), _) => task_id.to_owned(),
            (Some(OriginalContainer::SideOf { main_task_id }), _) => main_task_id.clone(),
            (None, BoardChatRole::Main) => task_id.to_owned(),
            // A legacy nested record keeps its immediate placement until its
            // migration (deferred while active) records the main container.
            (None, BoardChatRole::Side) if legacy_nested && !migrated => {
                immediate.clone().unwrap_or_else(|| task_id.to_owned())
            }
            (None, BoardChatRole::Side) => {
                main_parent_id.clone().unwrap_or_else(|| task_id.to_owned())
            }
        };
        if let Some(OriginalContainer::SideOf { main_task_id }) = &frozen {
            parent_unavailable = !self.board_chat_available(main_task_id)?;
        }

        let context_origin = match (&provenance, edge_kind) {
            (Some(record), _) => record.context_origin,
            (None, None) => BoardContextOrigin::Fresh,
            (None, Some(EdgeKind::Child)) => BoardContextOrigin::Fresh,
            (None, Some(EdgeKind::Fork)) => BoardContextOrigin::NativeFork,
            (None, Some(EdgeKind::LegacyParent)) => BoardContextOrigin::ContextSnapshot,
            (None, Some(EdgeKind::SideRow)) => {
                let native = self.task_identity.view(task_id).is_some_and(|view| {
                    view.bindings
                        .iter()
                        .any(|binding| binding.origin == luna_proto::BindingOrigin::Forked)
                });
                if native {
                    BoardContextOrigin::NativeFork
                } else {
                    BoardContextOrigin::ContextSnapshot
                }
            }
        };
        let creator = provenance
            .as_ref()
            .and_then(|record| record.creator.clone())
            .or_else(|| {
                immediate.clone().map(|task_id| ChatProvenance {
                    task_id,
                    relation: ChatProvenanceRelation::CreatedBy,
                })
            });
        let source = provenance
            .as_ref()
            .and_then(|record| record.source.clone())
            .or_else(|| match (edge_kind, &immediate) {
                (Some(EdgeKind::SideRow | EdgeKind::Fork | EdgeKind::LegacyParent), Some(id)) => {
                    Some(ChatProvenance {
                        task_id: id.clone(),
                        relation: ChatProvenanceRelation::ForkedFrom,
                    })
                }
                _ => None,
            });

        let purpose = if let Some(link) = &working {
            ChatPurpose::TicketWorker {
                work_item_id: link.work_item_id.clone(),
                key: self.board_item_key(&link.work_item_id)?,
            }
        } else if let Some(link) = &reviewer {
            let review_run_id = self
                .store
                .board_review_runs_for_reviewer(task_id)
                .map_err(board_store_error)?
                .into_iter()
                .find(|run| run.work_item_id == link.work_item_id)
                .map(|run| run.id)
                .unwrap_or_default();
            ChatPurpose::Reviewer {
                work_item_id: link.work_item_id.clone(),
                key: self.board_item_key(&link.work_item_id)?,
                review_run_id,
            }
        } else {
            ChatPurpose::Ordinary
        };
        let parent_locked = working.is_some() || reviewer.is_some();
        let live = !chat.archived;
        let side = role == BoardChatRole::Side;
        let permitted = ChatPermittedActions {
            create_side_fresh: live && !side,
            create_side_with_context: live && !side,
            create_sibling_fresh: live && side && !parent_unavailable,
            create_main: live,
            create_main_with_handoff: live,
            convert_to_ticket: live && !parent_locked,
            start_ticket: live,
            // Reparenting is not a feature for any chat.
            reparent: false,
        };
        let identity = CanonicalChatIdentity {
            task_id: task_id.to_owned(),
            role,
            main_parent_id: main_parent_id.clone(),
            display_container_id,
            creator,
            source,
            context_origin,
            purpose,
            original_container: frozen,
            parent_locked,
            parent_unavailable,
            legacy_nested,
            permitted,
        };
        Ok(ResolvedChat {
            chat,
            identity,
            immediate_parent_id: immediate,
            working,
            reviewer,
        })
    }

    /// Canonical identity for engine-internal callers (A2/A4/A5): no caller
    /// authority, durable records only.
    pub(crate) fn board_canonical_identity(
        &self,
        task_id: &str,
    ) -> BoardResult<CanonicalChatIdentity> {
        Ok(self.board_resolve_chat(task_id)?.identity)
    }

    fn board_item_key(&self, work_item_id: &str) -> BoardResult<String> {
        Ok(self
            .store
            .board_item(work_item_id)
            .map_err(board_store_error)?
            .map(|item| item.key)
            .unwrap_or_default())
    }

    /// The active Working link of a ticket.
    pub(crate) fn board_working_link(
        &self,
        work_item_id: &str,
    ) -> BoardResult<Option<WorkItemTaskLink>> {
        Ok(self
            .store
            .board_task_links_for_item(work_item_id, true)
            .map_err(board_store_error)?
            .into_iter()
            .find(|link| link.role == TaskLinkRole::Working))
    }

    /// Authorize a caller for one chat's project.
    fn board_check_chat(
        &self,
        caller: &BoardCaller,
        chat: &Chat,
        access: BoardAccess,
    ) -> BoardResult<()> {
        match (&chat.space_id, caller) {
            (Some(project), _) => self.board_check_caller(caller, project, access),
            (None, BoardCaller::User { .. } | BoardCaller::Engine { .. }) => Ok(()),
            (None, _) => Err(BoardError::NotAuthorized {
                reason: "That chat is outside this chat's project.".into(),
            }),
        }
    }

    /// The chat acting for a caller: tools bind their own run (a different
    /// claimed id is refused), the UI names the chat it acts in, temporary
    /// authoring sessions are not chats.
    pub(crate) fn board_bound_chat(
        &self,
        caller: &BoardCaller,
        claimed: &str,
        field: &str,
        operation: &str,
    ) -> BoardResult<String> {
        match caller {
            BoardCaller::Agent { task_id, .. } if claimed.is_empty() || claimed == task_id => {
                Ok(task_id.clone())
            }
            BoardCaller::Agent { .. } => Err(BoardError::NotAuthorized {
                reason: format!(
                    "A chat's tools act for that chat only; {field} must name the calling chat."
                ),
            }),
            BoardCaller::Authoring { .. } => Err(BoardError::InvalidRole {
                operation: operation.into(),
                reason: "A temporary ticket discussion is not a chat.".into(),
                allowed: vec![
                    "board_create to save the ticket".into(),
                    "board_start with destination newMain when the user asks to start it".into(),
                ],
            }),
            BoardCaller::User { .. } | BoardCaller::Engine { .. } if claimed.is_empty() => {
                Err(BoardError::invalid(field, "name the chat this acts in"))
            }
            BoardCaller::User { .. } | BoardCaller::Engine { .. } => Ok(claimed.to_owned()),
        }
    }

    // ---- RPC/tool operations ----

    /// Canonical identity of one chat, derived from the workspace side-chat
    /// row, `ChildThread` metadata and `TaskLineage` (walking legacy nesting
    /// up to the top-level main). Never from tabs, titles or model claims.
    /// Temporary authoring sessions and derived native-agent documents are
    /// not chats: `InvalidRole`.
    pub(crate) fn board_chat_identity(
        &self,
        caller: &BoardCaller,
        request: BoardChatIdentityRequest,
    ) -> BoardResult<CanonicalChatIdentity> {
        let resolved = self.board_resolve_chat(&request.task_id)?;
        self.board_check_chat(caller, &resolved.chat, BoardAccess::Read)?;
        Ok(resolved.identity)
    }

    /// Ticket badges for chats (sidebar rows, tabs, headers). Working links
    /// give the ticket badge, reviewer links the reviewer badge; source,
    /// reference and attempt chats only get `related` receipts.
    pub(crate) fn board_chat_badges(
        &self,
        caller: &BoardCaller,
        request: BoardChatBadgesRequest,
    ) -> BoardResult<BoardChatBadges> {
        if request.task_ids.len() > BADGES_MAX {
            return Err(BoardError::invalid(
                "taskIds",
                format!("ask for at most {BADGES_MAX} chats at once"),
            ));
        }
        if let BoardCaller::Agent { granted_scopes, .. } = caller
            && !granted_scopes.contains(BOARD_SCOPE_READ)
        {
            return Err(BoardError::InsufficientScope {
                required: BOARD_SCOPE_READ.into(),
            });
        }
        let mut badges = Vec::new();
        let mut related = Vec::new();
        let mut views: HashMap<String, Option<(WorkItem, luna_proto::BoardPhase)>> = HashMap::new();
        for task_id in &request.task_ids {
            let links = self
                .store
                .board_task_links_for_task(task_id, true)
                .map_err(board_store_error)?;
            if links.is_empty() {
                continue;
            }
            let harness = self
                .workspace
                .chat(task_id)
                .map_err(storage)?
                .and_then(|chat| chat.config)
                .map(|config| config.harness);
            for link in links {
                if !views.contains_key(&link.work_item_id) {
                    let entry = match self
                        .store
                        .board_item(&link.work_item_id)
                        .map_err(board_store_error)?
                    {
                        Some(item)
                            if item.owner_host_id == self.engine_info.device_id
                                && self
                                    .board_check_caller(caller, &item.project_id, BoardAccess::Read)
                                    .is_ok() =>
                        {
                            let phase = self.board_view(item.clone())?.phase;
                            Some((item, phase))
                        }
                        _ => None,
                    };
                    views.insert(link.work_item_id.clone(), entry);
                }
                let Some(Some((item, phase))) = views.get(&link.work_item_id) else {
                    continue;
                };
                let badge = ChatTicketBadge {
                    task_id: task_id.clone(),
                    work_item_id: item.id.clone(),
                    key: item.key.clone(),
                    title: item.title.clone(),
                    column: item.column,
                    phase: *phase,
                    role: link.role,
                    harness,
                };
                match link.role {
                    TaskLinkRole::Working | TaskLinkRole::Reviewer => badges.push(badge),
                    TaskLinkRole::Source | TaskLinkRole::Reference | TaskLinkRole::Attempt => {
                        related.push(badge)
                    }
                }
            }
        }
        Ok(BoardChatBadges { badges, related })
    }

    /// Go to chat: the main chat, or its main parent's workbench with the
    /// exact side chat selected, or a direct view when the parent is
    /// unavailable. Never relaunches, resends, reparents or promotes.
    pub(crate) fn board_navigation(
        &self,
        caller: &BoardCaller,
        request: BoardNavigationRequest,
    ) -> BoardResult<BoardNavigationTarget> {
        match (request.work_item_id, request.task_id) {
            (Some(work_item_id), _) => {
                let item = self.board_item_for(caller, &work_item_id, BoardAccess::Read)?;
                Ok(self.board_default_navigation(&item))
            }
            (None, Some(task_id)) => {
                let resolved = self.board_resolve_chat(&task_id)?;
                self.board_check_chat(caller, &resolved.chat, BoardAccess::Read)?;
                Ok(Self::board_identity_navigation(&resolved.identity))
            }
            (None, None) => Err(BoardError::invalid(
                "workItemId",
                "name a ticket or a chat to open",
            )),
        }
    }

    /// Navigation from a canonical identity (layout and titles never count).
    pub(crate) fn board_identity_navigation(
        identity: &CanonicalChatIdentity,
    ) -> BoardNavigationTarget {
        if identity.display_container_id == identity.task_id {
            BoardNavigationTarget::MainChat {
                task_id: identity.task_id.clone(),
            }
        } else if identity.parent_unavailable {
            BoardNavigationTarget::Direct {
                task_id: identity.task_id.clone(),
                original_parent_id: Some(identity.display_container_id.clone()),
            }
        } else {
            BoardNavigationTarget::SideChat {
                main_task_id: identity.display_container_id.clone(),
                side_task_id: identity.task_id.clone(),
            }
        }
    }

    /// The settings a chat actually runs with, for receipts.
    pub(crate) fn board_chat_settings(
        &self,
        chat: &Chat,
        sources: Vec<luna_proto::BoardSettingSource>,
    ) -> BoardResult<ResolvedExecution> {
        let config = chat
            .config
            .as_ref()
            .ok_or_else(|| BoardError::Unavailable {
                reason: "This chat has no provider yet; open it to choose one.".into(),
            })?;
        Ok(ResolvedExecution {
            harness: config.harness,
            provider_account_id: config.provider_account_id.clone(),
            model: config.model.clone(),
            reasoning: config.reasoning,
            model_options: config.model_options.clone(),
            permission_mode: config.effective_permission_mode,
            host_device_id: chat.device_id.clone(),
            project_id: chat.space_id.clone().unwrap_or_default(),
            cwd: chat.cwd.clone().unwrap_or_default(),
            checkout_id: chat.checkout_id.clone(),
            branch: chat.branch.clone(),
            sources,
        })
    }

    /// Ticket context for a worker or reviewer run (A4 builds the runtime
    /// context from this and [`Self::board_canonical_identity`]).
    pub(crate) fn board_ticket_context(
        &self,
        task_id: &str,
    ) -> BoardResult<Option<BoardTicketContext>> {
        let links = self
            .store
            .board_task_links_for_task(task_id, true)
            .map_err(board_store_error)?;
        let Some(link) = links
            .iter()
            .find(|link| link.role == TaskLinkRole::Working)
            .or_else(|| {
                links
                    .iter()
                    .find(|link| link.role == TaskLinkRole::Reviewer)
            })
        else {
            return Ok(None);
        };
        let Some(item) = self
            .store
            .board_item(&link.work_item_id)
            .map_err(board_store_error)?
        else {
            return Ok(None);
        };
        let intent = self
            .store
            .board_intent(&item.id)
            .map_err(board_store_error)?;
        let review_run_id = if link.role == TaskLinkRole::Reviewer {
            self.store
                .board_review_runs_for_reviewer(task_id)
                .map_err(board_store_error)?
                .into_iter()
                .find(|run| run.work_item_id == item.id)
                .map(|run| run.id)
        } else {
            None
        };
        let requested_outcome = item
            .description
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .map(|line| line.chars().take(200).collect());
        Ok(Some(BoardTicketContext {
            work_item_id: item.id.clone(),
            key: item.key.clone(),
            title: item.title.clone(),
            project_id: item.project_id.clone(),
            link_role: link.role,
            brief_revision: item.brief_revision,
            column: item.column,
            execution_generation: intent.map_or(0, |intent| intent.execution_generation),
            completion: item.completion.mode,
            review_run_id,
            requested_outcome,
        }))
    }

    // ---- the shared creation guard ----

    /// Shared guard for every creation entry point (side chats, message
    /// forks, native forks, `create_thread`, child tabs, context copies,
    /// board creation): returns the caller's canonical identity when `kind`
    /// is allowed, else `InvalidRole` with the allowed alternatives. A side
    /// chat may only create a fresh sibling under its main (resolved by the
    /// caller of this guard from `main_parent_id`) or a new main chat.
    pub(crate) fn board_validate_creation(
        &self,
        caller_task_id: &str,
        kind: BoardChatCreationKind,
    ) -> BoardResult<CanonicalChatIdentity> {
        let identity = self.board_canonical_identity(caller_task_id)?;
        let operation = kind_operation(kind).to_owned();
        let main = identity.main_parent_id.clone().unwrap_or_default();
        let alternatives = || {
            let mut allowed = Vec::new();
            if !identity.parent_unavailable {
                allowed.push(format!(
                    "create_side_chat with context fresh: a fresh sibling under main chat {main}"
                ));
            }
            allowed.push(
                "create_main_chat with a written handoffBrief: a new independent main chat".into(),
            );
            allowed
        };
        match (identity.role, kind) {
            (_, BoardChatCreationKind::MainChat) | (BoardChatRole::Main, _) => Ok(identity),
            (BoardChatRole::Side, BoardChatCreationKind::SideFresh)
                if !identity.parent_unavailable =>
            {
                Ok(identity)
            }
            (BoardChatRole::Side, BoardChatCreationKind::SideFresh) => {
                Err(BoardError::InvalidRole {
                    operation,
                    reason: format!(
                        "This side chat's main chat {main} is unavailable, so no sibling can be created under it."
                    ),
                    allowed: alternatives(),
                })
            }
            (BoardChatRole::Side, BoardChatCreationKind::ChildThread) => {
                Err(BoardError::InvalidRole {
                    operation,
                    reason:
                        "Side chats cannot own side chats; new side chats belong to the main chat."
                            .into(),
                    allowed: alternatives(),
                })
            }
            (
                BoardChatRole::Side,
                BoardChatCreationKind::SideWithContext
                | BoardChatCreationKind::NativeFork
                | BoardChatCreationKind::MessageFork,
            ) => Err(BoardError::InvalidRole {
                operation,
                reason: "A side chat cannot fork or copy its conversation into another chat."
                    .into(),
                allowed: alternatives(),
            }),
        }
    }

    /// The guard for legacy entry points, which keep their own errors for
    /// unknown chats and non-chat documents.
    pub(crate) fn board_guard_legacy_creation(
        &self,
        caller_task_id: &str,
        kind: BoardChatCreationKind,
    ) -> BoardResult<()> {
        if self.board_not_a_chat(caller_task_id)?
            || self
                .workspace
                .chat(caller_task_id)
                .map_err(storage)?
                .is_none()
        {
            return Ok(());
        }
        self.board_validate_creation(caller_task_id, kind)
            .map(|_| ())
    }

    /// One sentence plus the allowed alternatives, for receipts and tool
    /// results that carry plain text.
    pub(crate) fn board_error_text(error: &BoardError) -> String {
        match error {
            BoardError::InvalidRole { allowed, .. } if !allowed.is_empty() => {
                format!("{} Instead: {}.", error.message(), allowed.join("; or "))
            }
            _ => error.message(),
        }
    }

    /// Ticket workers and reviewers keep their original container: moves
    /// that would change it are refused before they happen.
    pub(crate) fn board_guard_container_change(&self, task_id: &str) -> BoardResult<()> {
        let links = self
            .store
            .board_task_links_for_task(task_id, true)
            .map_err(board_store_error)?;
        let Some(link) = links
            .iter()
            .find(|link| matches!(link.role, TaskLinkRole::Working | TaskLinkRole::Reviewer))
        else {
            return Ok(());
        };
        Err(BoardError::ParentLocked {
            task_id: task_id.to_owned(),
            original_parent_id: match &link.original_container {
                Some(OriginalContainer::SideOf { main_task_id }) => Some(main_task_id.clone()),
                _ => None,
            },
        })
    }

    // ---- title sync ----

    /// One rename operation for a ticket and its current primary working
    /// chat (from either surface). Parent/source titles never change;
    /// historical attempts keep their recorded title; provider auto-titling
    /// adopts the ticket title instead of drifting.
    pub(crate) async fn board_sync_title(
        &self,
        caller: &BoardCaller,
        origin: BoardTitleOrigin,
        title: &str,
    ) -> BoardResult<BoardTitleSyncReceipt> {
        self.board_sync_title_inner(caller, origin, title, true)
            .await
    }

    /// `rename_chat: false` is the chat-rename hook: the chat already has
    /// its new title, and a nested rename command must never be issued from
    /// inside that command.
    async fn board_sync_title_inner(
        &self,
        caller: &BoardCaller,
        origin: BoardTitleOrigin,
        title: &str,
        rename_chat: bool,
    ) -> BoardResult<BoardTitleSyncReceipt> {
        let title = clean_chat_title(title)?;
        for attempt in 0..TITLE_ATTEMPTS {
            let (item, link) = match &origin {
                BoardTitleOrigin::Ticket { work_item_id } => {
                    let item = self.board_item_for(caller, work_item_id, BoardAccess::Write)?;
                    let link = self.board_working_link(&item.id)?;
                    (item, link)
                }
                BoardTitleOrigin::WorkingChat { task_id } => {
                    let link = self
                        .store
                        .board_task_links_for_task(task_id, true)
                        .map_err(board_store_error)?
                        .into_iter()
                        .find(|link| link.role == TaskLinkRole::Working)
                        .ok_or_else(|| {
                            BoardError::invalid(
                                "taskId",
                                "that chat is not the working chat of a ticket",
                            )
                        })?;
                    let item =
                        self.board_item_for(caller, &link.work_item_id, BoardAccess::Write)?;
                    (item, Some(link))
                }
            };
            let task_id = link.as_ref().map(|link| link.task_id.clone());
            let mut revision = item.revision;
            if item.title != title {
                let mut next = item.clone();
                next.title = title.clone();
                next.revision += 1;
                next.field_revisions.title = next.revision;
                next.brief_revision += 1;
                next.updated_at_ms = crate::now_ms();
                next.updated_by = caller.actor();
                let key = format!(
                    "title-sync:{}:{}:{}",
                    item.id,
                    item.revision,
                    &board_digest(&title)?[..16]
                );
                let request_id = caller.normalized_request_id(&key)?;
                let receipt = self.board_title_receipt(caller, &next, task_id.clone());
                match self.board_commit(BoardCommit {
                    request_id,
                    payload_hash: board_digest(&("rename", &item.id, &title))?,
                    committed_at_ms: crate::now_ms(),
                    item: BoardItemWrite::Replace {
                        expected_revision: item.revision,
                        item: Box::new(next),
                        placement: None,
                    },
                    rows: Vec::new(),
                    event: Some((
                        WorkItemEventKind::Renamed {
                            title: title.clone(),
                            task_id: task_id.clone(),
                        },
                        true,
                    )),
                    receipt: Some(receipt),
                }) {
                    Ok(Some(receipt)) => revision = receipt.revision,
                    Ok(None) => return Err(storage("rename returned no receipt")),
                    Err(BoardError::Busy { reason })
                        if attempt + 1 < TITLE_ATTEMPTS && reason == BOARD_CONCURRENT_CHANGE =>
                    {
                        continue;
                    }
                    Err(error) => return Err(error),
                }
            }
            if let Some(task_id) = &task_id
                && rename_chat
            {
                self.board_rename_working_chat(task_id, &title, revision)
                    .await?;
            }
            return Ok(BoardTitleSyncReceipt {
                work_item_id: item.id,
                task_id,
                title,
                revision,
            });
        }
        Err(BoardError::Busy {
            reason: "The ticket kept changing; try again.".into(),
        })
    }

    fn board_title_receipt(
        &self,
        caller: &BoardCaller,
        item: &WorkItem,
        task_id: Option<String>,
    ) -> BoardMutationReceipt {
        BoardMutationReceipt {
            request_id: String::new(),
            payload_hash: String::new(),
            operation: BoardOperation::Rename,
            work_item_id: item.id.clone(),
            key: item.key.clone(),
            revision: item.revision,
            column: item.column,
            archived: item.archived,
            item: item.clone(),
            event_seq: None,
            actor: caller.actor(),
            committed_at_ms: 0,
            task_id,
            navigation: self.board_default_navigation(item),
        }
    }

    /// Rename the working chat through the ordinary rename command (native
    /// Codex names follow). A chat already carrying the title is untouched.
    async fn board_rename_working_chat(
        &self,
        task_id: &str,
        title: &str,
        revision: u64,
    ) -> BoardResult<()> {
        let current = self.workspace.chat(task_id).map_err(storage)?;
        if current
            .as_ref()
            .is_none_or(|chat| chat.title.as_deref() == Some(title))
        {
            return Ok(());
        }
        let mut command = legacy_task_command(
            task_id.to_owned(),
            TaskCommandIntent::Rename {
                title: title.to_owned(),
            },
        );
        command.command_id = format!(
            "board-title:{task_id}:{revision}:{}",
            &board_digest(&title)?[..12]
        );
        command.actor_id = "luna-board:title-sync".into();
        let receipt = self
            .execute_task_command(command, true)
            .await
            .map_err(|error| BoardError::Unavailable {
                reason: format!("The working chat could not be renamed: {error}"),
            })?;
        if receipt.status != CommandReceiptStatus::Applied {
            return Err(BoardError::Unavailable {
                reason: format!(
                    "The working chat could not be renamed: {}",
                    receipt
                        .reason
                        .as_ref()
                        .map_or("the rename was refused", |reason| reason.message.as_str())
                ),
            });
        }
        Ok(())
    }

    /// Hook called by `board_update` after a committed title change (the
    /// ticket side of the sync): the current working chat follows. Source,
    /// parent and attempt chats never change.
    pub(crate) async fn board_title_changed(
        &self,
        item: &WorkItem,
        origin: BoardTitleOrigin,
    ) -> BoardResult<()> {
        let _ = origin;
        let Some(link) = self.board_working_link(&item.id)? else {
            return Ok(());
        };
        self.board_rename_working_chat(&link.task_id, &item.title, item.revision)
            .await
    }

    /// Hook from the chat rename command (`Mutate renameChat` and task
    /// commands): a working chat's new title becomes the ticket title. Other
    /// chats are unaffected; failures never undo the chat rename.
    pub(crate) async fn board_chat_renamed(&self, task_id: &str, title: &str) {
        let is_worker = self
            .store
            .board_task_links_for_task(task_id, true)
            .is_ok_and(|links| links.iter().any(|link| link.role == TaskLinkRole::Working));
        if !is_worker || title.trim().is_empty() {
            return;
        }
        let caller = BoardCaller::Engine {
            component: "chat-rename",
        };
        if let Err(error) = self
            .board_sync_title_inner(
                &caller,
                BoardTitleOrigin::WorkingChat {
                    task_id: task_id.to_owned(),
                },
                title,
                false,
            )
            .await
        {
            tracing::warn!(%error, task = %task_id, "ticket title sync after chat rename failed");
        }
    }
}
