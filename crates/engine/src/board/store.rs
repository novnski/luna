//! PB-0 persistence and core service: idempotent create (save only), read,
//! bounded list, field-level update, rank, archive, receipts, the event
//! journal and the watch stream. Nothing here creates a chat, sends a prompt
//! or touches execution; Ready/Backlog moves belong to `lifecycle.rs`.
use super::*;
use luna_proto::{
    BOARD_CONTEXT_REFS_MAX, BOARD_DESCRIPTION_MAX_BYTES, BOARD_EVENTS_MAX_LIMIT,
    BOARD_LABEL_MAX_CHARS, BOARD_LABELS_MAX, BOARD_LIST_DEFAULT_LIMIT, BOARD_LIST_MAX_LIMIT,
    BOARD_SCHEMA_VERSION, BOARD_TITLE_MAX_CHARS,
};
use luna_proto::{
    BoardAction, BoardArchiveRequest, BoardChatRole, BoardColumn, BoardContextRef,
    BoardCreateRequest, BoardDeleteRequest, BoardEventPage, BoardEventsRequest,
    BoardExecutionIntent, BoardExecutionProfile, BoardFieldRevisions, BoardItemChanges,
    BoardItemDetail, BoardItemOrigin, BoardListPage, BoardListRequest, BoardNavigationTarget,
    BoardObservation, BoardOperation, BoardPhase, BoardRankRequest, BoardReadRequest,
    BoardReceiptRequest, BoardReviewProfile, BoardScope, BoardStatusReportKind, BoardUpdateRequest,
    BoardWatchFrame, BoardWatchRequest, OriginalContainer, TaskLinkRole, WorkItem, WorkItemEvent,
    WorkItemEventKind, WorkItemTaskLink, WorkItemView, WorkingChatSummary,
};
use luna_sync::{
    BoardCommit, BoardEventQuery, BoardItemWrite, BoardListCursor, BoardListQuery,
    BoardRankPlacement, BoardRowWrite,
};

const HISTORY_DEFAULT: usize = 20;
const HISTORY_MAX: usize = 100;
const EVENTS_DEFAULT: usize = 100;
/// More new events than this in one watch step asks the client to reload.
const WATCH_EVENTS_MAX: usize = 200;
/// Safety net for writers outside this process's notifier (none expected).
const WATCH_POLL: Duration = Duration::from_secs(5);
/// Field-level merges retry a concurrent commit this many times.
const MERGE_ATTEMPTS: usize = 3;

fn storage(error: impl std::fmt::Display) -> BoardError {
    BoardError::Storage {
        message: error.to_string(),
    }
}

/// Per-project prefix suggestion from the project name: up to three ASCII
/// letters, never derived from a ticket title. The store makes it unique.
fn key_prefix_hint(space: &Space) -> String {
    let letters: String = space
        .display_name()
        .chars()
        .filter(char::is_ascii_alphabetic)
        .map(|c| c.to_ascii_uppercase())
        .take(3)
        .collect();
    if letters.is_empty() {
        "TKT".into()
    } else {
        letters
    }
}

fn bounded(field: &str, value: &str, max: usize) -> BoardResult<()> {
    if value.len() > max
        || value
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return Err(BoardError::invalid(
            field,
            format!("use at most {max} bytes without control characters"),
        ));
    }
    Ok(())
}

fn clean_title(title: &str) -> BoardResult<String> {
    let title = title.trim();
    if title.is_empty() {
        return Err(BoardError::invalid("title", "enter a title"));
    }
    if title.chars().count() > BOARD_TITLE_MAX_CHARS || title.chars().any(char::is_control) {
        return Err(BoardError::invalid(
            "title",
            format!("use one line of at most {BOARD_TITLE_MAX_CHARS} characters"),
        ));
    }
    Ok(title.into())
}

fn clean_description(description: &str) -> BoardResult<String> {
    if description.len() > BOARD_DESCRIPTION_MAX_BYTES {
        return Err(BoardError::invalid(
            "description",
            format!("use at most {} KiB", BOARD_DESCRIPTION_MAX_BYTES / 1024),
        ));
    }
    bounded("description", description, BOARD_DESCRIPTION_MAX_BYTES)?;
    Ok(description.trim_end().into())
}

fn clean_labels(labels: &[String]) -> BoardResult<Vec<String>> {
    let mut clean: Vec<String> = Vec::new();
    for label in labels {
        let label = label.trim();
        if label.is_empty()
            || label.chars().count() > BOARD_LABEL_MAX_CHARS
            || label.chars().any(char::is_control)
        {
            return Err(BoardError::invalid(
                "labels",
                format!("labels are 1-{BOARD_LABEL_MAX_CHARS} characters"),
            ));
        }
        if !clean.iter().any(|existing| existing == label) {
            clean.push(label.into());
        }
    }
    if clean.len() > BOARD_LABELS_MAX {
        return Err(BoardError::invalid(
            "labels",
            format!("use at most {BOARD_LABELS_MAX} labels"),
        ));
    }
    Ok(clean)
}

fn bounded_id(field: &str, value: &str) -> BoardResult<()> {
    if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        return Err(BoardError::invalid(
            field,
            "use a nonempty id of at most 256 bytes",
        ));
    }
    Ok(())
}

fn clean_context_refs(refs: &[BoardContextRef]) -> BoardResult<Vec<BoardContextRef>> {
    if refs.len() > BOARD_CONTEXT_REFS_MAX {
        return Err(BoardError::invalid(
            "contextRefs",
            format!("use at most {BOARD_CONTEXT_REFS_MAX} references"),
        ));
    }
    for reference in refs {
        match reference {
            BoardContextRef::File { path } => {
                bounded_id("contextRefs.path", path)?;
                if path.split(['/', '\\']).any(|part| part == "..") {
                    return Err(BoardError::invalid(
                        "contextRefs.path",
                        "use a project-relative path",
                    ));
                }
            }
            BoardContextRef::Url { url } => {
                bounded("contextRefs.url", url, 2048)?;
                if !(url.starts_with("https://") || url.starts_with("http://")) {
                    return Err(BoardError::invalid("contextRefs.url", "use an http(s) URL"));
                }
            }
            BoardContextRef::ChatMessage {
                task_id,
                message_id,
            } => {
                bounded_id("contextRefs.taskId", task_id)?;
                bounded_id("contextRefs.messageId", message_id)?;
            }
            BoardContextRef::Issue { issue_ref_id } => {
                bounded_id("contextRefs.issueRefId", issue_ref_id)?
            }
            BoardContextRef::Note { text } => bounded("contextRefs.text", text, 2000)?,
        }
    }
    Ok(refs.to_vec())
}

fn check_options(field: &str, options: &serde_json::Map<String, Value>) -> BoardResult<()> {
    if options.len() > 32
        || options
            .iter()
            .any(|(key, value)| key.len() > 64 || value.to_string().len() > 256)
    {
        return Err(BoardError::invalid(
            field,
            "use at most 32 provider options with short ids",
        ));
    }
    Ok(())
}

fn check_model(field: &str, model: &Option<String>) -> BoardResult<()> {
    if let Some(model) = model
        && (model.trim().is_empty()
            || model.len() > 512
            || model.trim() != model
            || model.chars().any(char::is_control))
    {
        return Err(BoardError::invalid(field, "use a nonempty model id"));
    }
    Ok(())
}

fn check_implementation(profile: &BoardExecutionProfile) -> BoardResult<()> {
    check_model("implementation.model", &profile.model)?;
    if let Some(account) = &profile.provider_account_id {
        bounded_id("implementation.providerAccountId", account)?;
    }
    check_options("implementation.modelOptions", &profile.model_options)?;
    if let Some(luna_proto::BoardCheckoutChoice::NewWorktree { branch, base }) = &profile.checkout {
        for value in [branch, base].into_iter().flatten() {
            bounded_id("implementation.checkout", value)?;
        }
    }
    Ok(())
}

fn check_review(profile: &BoardReviewProfile) -> BoardResult<()> {
    check_model("review.model", &profile.model)?;
    if let Some(account) = &profile.provider_account_id {
        bounded_id("review.providerAccountId", account)?;
    }
    check_options("review.modelOptions", &profile.model_options)
}

/// The payload a request id is bound to (everything except the key).
fn payload_hash(operation: BoardOperation, payload: &impl Serialize) -> BoardResult<String> {
    board_digest(&(operation.as_str(), payload))
}

/// Creation requests hash everything but the key, including any claimed
/// origin, so a retry replays before origin validation runs again.
fn create_payload_hash(
    operation: BoardOperation,
    request: &BoardCreateRequest,
) -> BoardResult<String> {
    let mut payload = request.clone();
    payload.request_id.clear();
    payload_hash(operation, &payload)
}

fn deterministic_item_id(request_id: &str) -> BoardResult<String> {
    Ok(format!(
        "wi-{}",
        &board_digest(&("board-item", request_id))?[..32]
    ))
}

fn changed_fields(changes: &BoardItemChanges) -> Vec<&'static str> {
    let mut fields = Vec::new();
    if changes.title.is_some() {
        fields.push("title");
    }
    if changes.description.is_some() {
        fields.push("description");
    }
    if changes.attachments.is_some() {
        fields.push("attachments");
    }
    if changes.implementation.is_some() {
        fields.push("implementation");
    }
    if changes.review.is_some() {
        fields.push("review");
    }
    if changes.completion.is_some() {
        fields.push("completion");
    }
    if changes.labels.is_some() {
        fields.push("labels");
    }
    if changes.priority.is_some() {
        fields.push("priority");
    }
    if changes.context_refs.is_some() {
        fields.push("contextRefs");
    }
    fields
}

fn field_revision(revisions: &BoardFieldRevisions, field: &str) -> u64 {
    match field {
        "title" => revisions.title,
        "description" => revisions.description,
        "implementation" => revisions.implementation,
        "review" => revisions.review,
        "completion" => revisions.completion,
        "labels" => revisions.labels,
        "priority" => revisions.priority,
        "contextRefs" => revisions.context_refs,
        "attachments" => revisions.attachments,
        _ => u64::MAX,
    }
}

fn set_field_revision(revisions: &mut BoardFieldRevisions, field: &str, revision: u64) {
    match field {
        "title" => revisions.title = revision,
        "description" => revisions.description = revision,
        "implementation" => revisions.implementation = revision,
        "review" => revisions.review = revision,
        "completion" => revisions.completion = revision,
        "labels" => revisions.labels = revision,
        "priority" => revisions.priority = revision,
        "contextRefs" => revisions.context_refs = revision,
        "attachments" => revisions.attachments = revision,
        _ => {}
    }
}

/// Apply validated changes; returns the fields whose value actually changed.
fn apply_changes(item: &mut WorkItem, changes: BoardItemChanges) -> Vec<&'static str> {
    let mut changed = Vec::new();
    if let Some(attachments) = changes.attachments
        && attachments != item.attachments
    {
        item.attachments = attachments;
        changed.push("attachments");
    }
    if let Some(title) = changes.title
        && title != item.title
    {
        item.title = title;
        changed.push("title");
    }
    if let Some(description) = changes.description
        && description != item.description
    {
        item.description = description;
        changed.push("description");
    }
    if let Some(implementation) = changes.implementation
        && implementation != item.implementation
    {
        item.implementation = implementation;
        changed.push("implementation");
    }
    if let Some(review) = changes.review
        && review != item.review
    {
        item.review = review;
        changed.push("review");
    }
    if let Some(completion) = changes.completion
        && completion != item.completion
    {
        item.completion = completion;
        changed.push("completion");
    }
    if let Some(labels) = changes.labels
        && labels != item.labels
    {
        item.labels = labels;
        changed.push("labels");
    }
    if let Some(priority) = changes.priority
        && priority != item.priority
    {
        item.priority = priority;
        changed.push("priority");
    }
    if let Some(context_refs) = changes.context_refs
        && context_refs != item.context_refs
    {
        item.context_refs = context_refs;
        changed.push("contextRefs");
    }
    changed
}

impl EngineRpc {
    // ---- shared helpers (used by every slice) ----

    /// Load a ticket by id, or by human key on this host.
    pub(crate) fn board_load(&self, id_or_key: &str) -> BoardResult<WorkItem> {
        let item = self.board_load_retained(id_or_key)?;
        if item.deleted {
            return Err(BoardError::NotFound {
                work_item_id: id_or_key.into(),
            });
        }
        Ok(item)
    }

    /// Audit-only lookup. Never use this to admit execution or live mutation.
    fn board_load_retained(&self, id_or_key: &str) -> BoardResult<WorkItem> {
        if let Some(item) = self.store.board_item(id_or_key).map_err(storage)? {
            return Ok(item);
        }
        if luna_proto::parse_board_key(id_or_key).is_some()
            && let Some(item) = self
                .store
                .board_item_by_key(&self.engine_info.device_id, id_or_key)
                .map_err(storage)?
        {
            return Ok(item);
        }
        Err(BoardError::NotFound {
            work_item_id: id_or_key.into(),
        })
    }

    /// Load and authorize a ticket for a caller.
    pub(crate) fn board_item_for(
        &self,
        caller: &BoardCaller,
        id_or_key: &str,
        access: BoardAccess,
    ) -> BoardResult<WorkItem> {
        let item = self.board_load(id_or_key)?;
        if item.owner_host_id != self.engine_info.device_id {
            return Err(BoardError::WrongHost {
                project_id: item.project_id,
                owner_host_id: item.owner_host_id,
            });
        }
        self.board_check_caller(caller, &item.project_id, access)?;
        Ok(item)
    }

    /// A stored receipt for this exact request, or a conflict when the key
    /// was used for different content.
    pub(crate) fn board_replay(
        &self,
        request_id: &str,
        payload_hash: &str,
    ) -> BoardResult<Option<BoardMutationReceipt>> {
        match self.store.board_receipt(request_id).map_err(storage)? {
            Some(receipt) if receipt.payload_hash == payload_hash => Ok(Some(receipt)),
            Some(_) => Err(BoardError::IdempotencyConflict {
                request_id: request_id.into(),
            }),
            None => Ok(None),
        }
    }

    /// The working chat's summary as Board shows it.
    fn board_working_chat(&self, link: &WorkItemTaskLink) -> WorkingChatSummary {
        let chat = self.workspace.chat(&link.task_id).ok().flatten();
        let (role, main_parent_id) = match &link.original_container {
            Some(OriginalContainer::SideOf { main_task_id }) => {
                (BoardChatRole::Side, Some(main_task_id.clone()))
            }
            Some(OriginalContainer::MainChat) => (BoardChatRole::Main, None),
            None => (link.chat_role.unwrap_or(BoardChatRole::Main), None),
        };
        let parent = main_parent_id
            .as_deref()
            .and_then(|id| self.workspace.chat(id).ok().flatten());
        WorkingChatSummary {
            task_id: link.task_id.clone(),
            role,
            main_parent_title: parent.as_ref().and_then(|parent| parent.title.clone()),
            parent_available: main_parent_id.is_none()
                || parent.as_ref().is_some_and(|parent| !parent.archived),
            main_parent_id,
            harness: chat
                .as_ref()
                .and_then(|chat| chat.config.as_ref())
                .map(|config| config.harness),
            model: chat
                .as_ref()
                .and_then(|chat| chat.config.as_ref())
                .and_then(|config| config.model.clone()),
        }
    }

    /// Canonical Go-to-chat target from the active working link. Navigation
    /// never relaunches, resends or reparents.
    pub(crate) fn board_default_navigation(&self, item: &WorkItem) -> BoardNavigationTarget {
        let link = self
            .store
            .board_task_links_for_item(&item.id, true)
            .ok()
            .and_then(|links| links.into_iter().find(|l| l.role == TaskLinkRole::Working));
        let Some(link) = link else {
            return BoardNavigationTarget::NoChat {
                work_item_id: item.id.clone(),
            };
        };
        let summary = self.board_working_chat(&link);
        match summary.main_parent_id {
            None => BoardNavigationTarget::MainChat {
                task_id: link.task_id,
            },
            Some(main) if summary.parent_available => BoardNavigationTarget::SideChat {
                main_task_id: main,
                side_task_id: link.task_id,
            },
            Some(main) => BoardNavigationTarget::Direct {
                task_id: link.task_id,
                original_parent_id: Some(main),
            },
        }
    }

    /// Observation used until the lifecycle slice provides real evidence:
    /// honest defaults that never claim running, stopped or complete work
    /// without proof.
    fn board_fallback_observation(
        &self,
        item: &WorkItem,
        intent: Option<&BoardExecutionIntent>,
        working: Option<&WorkingChatSummary>,
    ) -> BoardObservation {
        let phase = if item.column == BoardColumn::Done {
            BoardPhase::Complete
        } else if working.is_none() && intent.is_none_or(|intent| intent.execution_generation == 0)
        {
            BoardPhase::NotStarted
        } else {
            intent.map_or(BoardPhase::Unknown, |intent| intent.observed_phase)
        };
        let mut attention = Vec::new();
        if let Some(report) = &item.last_report
            && report.report.kind == BoardStatusReportKind::Blocked
        {
            attention.push(luna_proto::BoardAttentionReason {
                kind: luna_proto::BoardAttentionKind::AgentBlocked,
                detail: report.report.summary.clone(),
                attention_id: None,
                task_id: report.actor.task_id().map(str::to_owned),
            });
        }
        BoardObservation {
            work_item_id: item.id.clone(),
            phase,
            attention,
            queue_reason: intent.and_then(|intent| intent.queue.clone()),
            working_chat: working.cloned(),
            observed_at_ms: intent.map_or(item.updated_at_ms, |intent| intent.updated_at_ms),
        }
    }

    /// Engine-derived read model for one ticket.
    pub(crate) fn board_view(&self, item: WorkItem) -> BoardResult<WorkItemView> {
        let intent = self.store.board_intent(&item.id).map_err(storage)?;
        let links = self
            .store
            .board_task_links_for_item(&item.id, true)
            .map_err(storage)?;
        let working_chat = links
            .iter()
            .find(|link| link.role == TaskLinkRole::Working)
            .map(|link| self.board_working_chat(link));
        let issue = self
            .store
            .board_issue_refs_for_item(&item.id)
            .map_err(storage)?
            .into_iter()
            .find(|issue| issue.active && issue.role == luna_proto::IssueLinkRole::Source)
            .map(|issue| issue.summary());
        let mut blocked_by = Vec::new();
        for dependency in self.store.board_dependencies(&item.id).map_err(storage)? {
            let blocker = self
                .store
                .board_item(&dependency.blocked_by_id)
                .map_err(storage)?;
            if blocker.is_some_and(|b| b.column != BoardColumn::Done && !b.archived) {
                blocked_by.push(dependency.blocked_by_id);
            }
        }
        let observation = match self.board_observe(&item, intent.as_ref()) {
            Ok(observation) => observation,
            Err(BoardError::NotImplemented { .. }) => {
                self.board_fallback_observation(&item, intent.as_ref(), working_chat.as_ref())
            }
            Err(error) => return Err(error),
        };
        let working_chat = observation.working_chat.clone().or(working_chat);
        let mut actions = vec![BoardAction::Edit];
        if item.column != BoardColumn::Ready
            && !self
                .store
                .board_has_active_review(&item.id)
                .map_err(storage)?
            && matches!(
                observation.phase,
                BoardPhase::NotStarted
                    | BoardPhase::Idle
                    | BoardPhase::Stopped
                    | BoardPhase::Failed
                    | BoardPhase::Complete
            )
            // Done describes the accepted result, but the preserved working
            // chat may have a later user turn or queued/background work.
            && !links.iter().filter(|link| matches!(link.role, TaskLinkRole::Working | TaskLinkRole::Reviewer)).any(|link| {
                self.sessions.active_turn_id(&link.task_id).is_some()
                    || self.sessions.turn_in_flight(&link.task_id)
                    || self.sessions.has_live_background_work(&link.task_id)
                    || match self.workspace.chat(&link.task_id) {
                        Ok(None) => false,
                        Ok(Some(chat)) if chat.device_id == self.engine_info.device_id => self.doc_host.open(&link.task_id)
                            .ok().and_then(|handle| handle.doc().read_queue().ok())
                            .is_none_or(|queue| !queue.is_empty()),
                        _ => true,
                    }
            })
        {
            actions.push(BoardAction::Delete);
        }
        if item.archived {
            actions.push(BoardAction::Unarchive);
        } else {
            match item.column {
                BoardColumn::Backlog => {
                    actions.push(BoardAction::MoveToReady);
                    actions.push(if working_chat.is_some() {
                        BoardAction::Continue
                    } else {
                        BoardAction::Start
                    });
                }
                BoardColumn::Ready => {
                    actions.push(BoardAction::MoveToBacklog);
                    if working_chat.is_some() {
                        actions.push(BoardAction::Stop);
                    } else if intent.as_ref().is_some_and(|intent| {
                        intent.launch_failure.is_some()
                            || matches!(
                                intent.queue,
                                Some(luna_proto::BoardAdmissionReason::Dependency { .. })
                            )
                    }) {
                        actions.push(BoardAction::Start);
                    }
                }
                BoardColumn::Done => actions.push(BoardAction::Reopen),
            }
            // Recovery is explicit. The detail UI already renders Retry
            // from this canonical action list; without it unknown delivery
            // and exhausted review cycles cannot be recovered in the app.
            if item.column != BoardColumn::Done
                && matches!(
                    observation.phase,
                    BoardPhase::Idle
                        | BoardPhase::Failed
                        | BoardPhase::Stopped
                        | BoardPhase::Unknown
                        | BoardPhase::NotStarted
                )
                && (intent
                    .as_ref()
                    .is_some_and(|intent| intent.launch_failure.is_some())
                    || observation.attention.iter().any(|reason| {
                        matches!(
                            reason.kind,
                            luna_proto::BoardAttentionKind::DeliveryUnknown
                                | luna_proto::BoardAttentionKind::ReviewBlocked
                                | luna_proto::BoardAttentionKind::ReviewUnavailable
                                | luna_proto::BoardAttentionKind::ModelUnavailable
                                | luna_proto::BoardAttentionKind::ProviderFailure
                        )
                    })
                    || self
                        .board_review_runs_of(&item.id)
                        .first()
                        .is_some_and(|run| {
                            Some(run.execution_generation)
                                == intent.as_ref().map(|intent| intent.execution_generation)
                                && matches!(
                                    run.outcome,
                                    Some(luna_proto::ReviewRunOutcome::NeedsAttention { .. })
                                )
                        }))
            {
                actions.push(BoardAction::Retry);
            }
            actions.push(BoardAction::Archive);
        }
        if working_chat.is_some() {
            actions.push(BoardAction::GoToChat);
            actions.push(BoardAction::SendUpdate);
        }
        let last_activity_ms = item.updated_at_ms.max(observation.observed_at_ms);
        Ok(WorkItemView {
            phase: observation.phase,
            attention: observation.attention,
            working_chat,
            queue_reason: observation.queue_reason,
            issue,
            blocked_by,
            last_activity_ms,
            actions,
            item,
        })
    }

    fn board_receipt_template(
        &self,
        caller: &BoardCaller,
        operation: BoardOperation,
        item: &WorkItem,
    ) -> BoardMutationReceipt {
        BoardMutationReceipt {
            request_id: String::new(),
            payload_hash: String::new(),
            operation,
            work_item_id: item.id.clone(),
            key: item.key.clone(),
            revision: item.revision,
            column: item.column,
            archived: item.archived,
            item: item.clone(),
            event_seq: None,
            actor: caller.actor(),
            committed_at_ms: 0,
            task_id: None,
            navigation: self.board_default_navigation(item),
        }
    }

    /// Validate requested settings at save. The lifecycle slice may add
    /// catalog checks; until then only the shape is validated here.
    fn board_check_settings(
        &self,
        space: &Space,
        implementation: &BoardExecutionProfile,
        review: &BoardReviewProfile,
    ) -> BoardResult<()> {
        check_implementation(implementation)?;
        check_review(review)?;
        match self.board_validate_requested_settings(space, implementation, review) {
            Ok(()) | Err(BoardError::NotImplemented { .. }) => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// The origin a caller may record. Agents and authoring sessions get a
    /// bound origin; the UI may name the ordinary chat it created from.
    fn board_bound_origin(
        &self,
        caller: &BoardCaller,
        claimed: Option<BoardItemOrigin>,
        project_id: &str,
    ) -> BoardResult<BoardItemOrigin> {
        let check_chat = |task_id: &str| -> BoardResult<()> {
            let chat = self
                .workspace
                .chat(task_id)
                .map_err(storage)?
                .ok_or_else(|| BoardError::invalid("origin", "that chat no longer exists"))?;
            if chat.space_id.as_deref() != Some(project_id)
                || self.derived_fork_document(task_id)
                || self
                    .store
                    .is_board_authoring_session(task_id)
                    .map_err(storage)?
            {
                return Err(BoardError::invalid(
                    "origin",
                    "the source must be an ordinary chat in this project",
                ));
            }
            Ok(())
        };
        match (caller, claimed) {
            (BoardCaller::Agent { task_id, .. }, None) => Ok(BoardItemOrigin::Chat {
                task_id: task_id.clone(),
                message_id: None,
            }),
            (
                BoardCaller::Agent { task_id, .. },
                Some(BoardItemOrigin::Chat {
                    task_id: claimed,
                    message_id,
                }),
            ) if claimed == *task_id => Ok(BoardItemOrigin::Chat {
                task_id: claimed,
                message_id,
            }),
            (BoardCaller::Authoring { session_id, .. }, None) => Ok(BoardItemOrigin::Authoring {
                session_id: session_id.clone(),
            }),
            (
                BoardCaller::Authoring { session_id, .. },
                Some(BoardItemOrigin::Authoring {
                    session_id: claimed,
                }),
            ) if claimed == *session_id => Ok(BoardItemOrigin::Authoring {
                session_id: claimed,
            }),
            (BoardCaller::Agent { .. } | BoardCaller::Authoring { .. }, Some(_)) => {
                Err(BoardError::NotAuthorized {
                    reason: "A chat can only record itself as a ticket's source.".into(),
                })
            }
            (BoardCaller::User { .. } | BoardCaller::Engine { .. }, None) => {
                Ok(BoardItemOrigin::Manual)
            }
            (BoardCaller::User { .. }, Some(BoardItemOrigin::Manual)) => {
                Ok(BoardItemOrigin::Manual)
            }
            (
                BoardCaller::User { .. },
                Some(BoardItemOrigin::Chat {
                    task_id,
                    message_id,
                }),
            ) => {
                check_chat(&task_id)?;
                Ok(BoardItemOrigin::Chat {
                    task_id,
                    message_id,
                })
            }
            (BoardCaller::User { .. }, Some(_)) => Err(BoardError::invalid(
                "origin",
                "conversion, authoring and issue imports use their own operations",
            )),
            (BoardCaller::Engine { .. }, Some(origin)) => Ok(origin),
        }
    }

    /// Insert a new ticket with an already-bound origin, optional extra rows
    /// (a conversion's working link, an import's issue reference) and one
    /// canonical event. Shared by create, conversion and issue import.
    #[allow(clippy::too_many_arguments)] // one atomic creation seam
    pub(crate) fn board_insert_item(
        &self,
        caller: &BoardCaller,
        operation: BoardOperation,
        request: BoardCreateRequest,
        origin: BoardItemOrigin,
        rows: Vec<BoardRowWrite>,
        event: Option<WorkItemEventKind>,
        task_id: Option<String>,
    ) -> BoardResult<BoardMutationReceipt> {
        let request_id = caller.normalized_request_id(&request.request_id)?;
        let hash = create_payload_hash(operation, &request)?;
        if let Some(receipt) = self.board_replay(&request_id, &hash)? {
            return Ok(receipt);
        }
        let item_id = deterministic_item_id(&request_id)?;
        let mut rows = rows;
        for row in &mut rows {
            match row {
                BoardRowWrite::TaskLink(link) if link.work_item_id.is_empty() => {
                    link.work_item_id = item_id.clone();
                }
                BoardRowWrite::IssueReference(issue) if issue.work_item_id.is_empty() => {
                    issue.work_item_id = item_id.clone();
                }
                _ => {}
            }
        }
        let project_id = request
            .project_id
            .clone()
            .ok_or(BoardError::AmbiguousProject)?;
        let space = self.board_project(caller, &project_id, BoardAccess::Write)?;
        let title = clean_title(&request.title)?;
        let description = clean_description(&request.description)?;
        let implementation = request.implementation.unwrap_or_default();
        let review = request.review.unwrap_or_default();
        self.board_check_settings(&space, &implementation, &review)?;
        let labels = clean_labels(&request.labels)?;
        let context_refs = clean_context_refs(&request.context_refs)?;
        let (attachments, input_items) =
            self.board_clean_brief_inputs(&request.attachments, &request.input_items)?;
        self.board_authorize_attachments(caller, &attachments, &[])?;
        let now = crate::now_ms();
        let actor = caller.actor();
        let item = WorkItem {
            schema_version: BOARD_SCHEMA_VERSION,
            id: item_id,
            project_id: space.id.clone(),
            owner_host_id: self.engine_info.device_id.clone(),
            key: String::new(),
            number: 0,
            title,
            description,
            brief_revision: 1,
            column: BoardColumn::Backlog,
            archived: false,
            deleted: false,
            rank: String::new(),
            revision: 1,
            field_revisions: BoardFieldRevisions {
                title: 1,
                description: 1,
                implementation: 1,
                review: 1,
                completion: 1,
                labels: 1,
                priority: 1,
                context_refs: 1,
                attachments: 1,
            },
            implementation,
            review,
            completion: request.completion.unwrap_or_default(),
            labels,
            priority: request.priority,
            origin,
            context_refs,
            attachments,
            input_items,
            last_report: None,
            created_at_ms: now,
            updated_at_ms: now,
            created_by: actor.clone(),
            updated_by: actor,
        };
        let mut receipt = self.board_receipt_template(caller, operation, &item);
        receipt.task_id = task_id;
        let rows_link_worker = rows
            .iter()
            .any(|row| matches!(row, BoardRowWrite::TaskLink(link) if link.role == TaskLinkRole::Working));
        if !rows_link_worker {
            receipt.navigation = BoardNavigationTarget::NoChat {
                work_item_id: item.id.clone(),
            };
        }
        let event = event.unwrap_or(WorkItemEventKind::Created {
            key: String::new(),
            column: BoardColumn::Backlog,
        });
        let committed = self.board_commit(BoardCommit {
            request_id,
            payload_hash: hash,
            committed_at_ms: now,
            item: BoardItemWrite::Create {
                item: Box::new(item),
                prefix_hint: key_prefix_hint(&space),
                placement: request.placement.into(),
            },
            rows,
            event: Some((event.clone(), event.is_activity())),
            receipt: Some(receipt),
        })?;
        let mut receipt = committed.ok_or_else(|| storage("create returned no receipt"))?;
        if rows_link_worker {
            receipt.navigation = self.board_default_navigation(&receipt.item);
        }
        Ok(receipt)
    }

    // ---- RPC/tool operations ----

    /// An agent can reuse actual user inputs of its bound chat/discussion, not
    /// another upload it found by guessing a path or reading a text trailer.
    fn board_authorize_attachments(
        &self,
        caller: &BoardCaller,
        paths: &[String],
        existing: &[String],
    ) -> BoardResult<()> {
        if paths.iter().all(|p| existing.contains(p)) {
            return Ok(());
        }
        let mut allowed: HashSet<String> = existing.iter().cloned().collect();
        match caller {
            BoardCaller::User { .. } | BoardCaller::Engine { .. } => return Ok(()),
            BoardCaller::Agent { task_id, .. } => {
                allowed.extend(
                    self.store
                        .chat_attachments(&self.engine_info.device_id, task_id)
                        .map_err(storage)?,
                );
                // Legacy sends already have structured attachments in the
                // immutable command ledger. Read those without trusting prose.
                let handle = self.doc_host.open(task_id).map_err(storage)?;
                for mut command in handle.doc().read_commands().map_err(storage)? {
                    if !matches!(
                        command.status,
                        luna_doc::SessionCommandStatus::Pending
                            | luna_doc::SessionCommandStatus::Applied
                    ) {
                        continue;
                    }
                    // A replicated row alone is not proof of a user upload:
                    // match its immutable payload to this host's local grant.
                    // The grant predates the terminal status projection.
                    command.status = luna_doc::SessionCommandStatus::Pending;
                    command.resolution = None;
                    let hash = crate::doc_host::executable_row_hash(&command).map_err(storage)?;
                    if self
                        .store
                        .local_executable_hash(
                            task_id,
                            luna_sync::LocalExecutableKind::Command,
                            &command.id,
                        )
                        .map_err(storage)?
                        .as_deref()
                        != Some(hash.as_str())
                    {
                        continue;
                    }
                    let inputs = match command.payload {
                        luna_doc::SessionCommandPayload::Run { request, .. } => request.attachments,
                        luna_doc::SessionCommandPayload::Steer { attachments, .. } => attachments,
                        _ => Vec::new(),
                    };
                    allowed.extend(
                        inputs
                            .iter()
                            .filter_map(|p| self.uploads.trusted_blob_path(p)),
                    );
                }
            }
            BoardCaller::Authoring { session_id, .. } => {
                let mut before = None;
                loop {
                    let entries = self
                        .store
                        .board_authoring_entries(session_id, before, 100)
                        .map_err(storage)?;
                    if entries.is_empty() {
                        break;
                    }
                    before = entries.last().map(|(seq, _)| *seq);
                    for (_, entry) in entries {
                        let entry: Value = serde_json::from_str(&entry).map_err(storage)?;
                        if entry["role"] == "user"
                            && let Some(images) = entry["attachments"].as_array()
                        {
                            allowed.extend(
                                images
                                    .iter()
                                    .filter_map(Value::as_str)
                                    .filter_map(|p| self.uploads.trusted_blob_path(p)),
                            );
                        }
                    }
                    if paths.iter().all(|p| allowed.contains(p)) {
                        break;
                    }
                }
            }
        }
        if paths.iter().any(|p| !allowed.contains(p)) {
            return Err(BoardError::invalid(
                "attachments",
                "Use images supplied to this chat or ticket discussion as attachments; file paths mentioned in text and other chats' uploads are not authorized.",
            ));
        }
        Ok(())
    }

    /// A brief's images and `@`/`$` selections: only this host's own
    /// uploads (never an arbitrary path), bounded like a chat message's.
    pub(crate) fn board_clean_brief_inputs(
        &self,
        attachments: &[String],
        input_items: &[luna_proto::ComposerInput],
    ) -> BoardResult<(Vec<String>, Vec<luna_proto::ComposerInput>)> {
        const ATTACHMENTS_MAX: usize = 10;
        const INPUT_ITEMS_MAX: usize = 32;
        if attachments.len() > ATTACHMENTS_MAX {
            return Err(BoardError::invalid(
                "attachments",
                format!("attach at most {ATTACHMENTS_MAX} images to a ticket"),
            ));
        }
        if input_items.len() > INPUT_ITEMS_MAX {
            return Err(BoardError::invalid(
                "inputItems",
                format!("use at most {INPUT_ITEMS_MAX} mentions and skills in a ticket"),
            ));
        }
        let attachments = attachments
            .iter()
            .map(|path| {
                self.uploads.trusted_blob_path(path).ok_or_else(|| {
                    BoardError::invalid("attachments", "attach images uploaded to this host")
                })
            })
            .collect::<BoardResult<Vec<String>>>()?;
        Ok((attachments, input_items.to_vec()))
    }

    /// `board_create`: save only. No chat, no launch, no provider usage.
    pub(crate) async fn board_create(
        &self,
        caller: &BoardCaller,
        request: BoardCreateRequest,
    ) -> BoardResult<BoardMutationReceipt> {
        let request_id = caller.normalized_request_id(&request.request_id)?;
        let hash = create_payload_hash(BoardOperation::Create, &request)?;
        if let Some(receipt) = self.board_replay(&request_id, &hash)? {
            return Ok(receipt);
        }
        let project_id = request
            .project_id
            .clone()
            .ok_or(BoardError::AmbiguousProject)?;
        let origin = self.board_bound_origin(caller, request.origin.clone(), &project_id)?;
        let mut rows = Vec::new();
        // A source discussion is provenance only: a Source link, never the
        // worker and never a badge.
        if let BoardItemOrigin::Chat {
            task_id,
            message_id,
        } = &origin
        {
            rows.push(BoardRowWrite::TaskLink(Box::new(WorkItemTaskLink {
                id: format!(
                    "wl-{}",
                    &board_digest(&("source", &request_id, task_id))?[..32]
                ),
                work_item_id: String::new(),
                task_id: task_id.clone(),
                role: TaskLinkRole::Source,
                active: true,
                chat_role: None,
                original_container: None,
                linked_by: caller.actor(),
                linked_at_ms: crate::now_ms(),
                unlinked_at_ms: None,
                source_message_id: message_id.clone(),
                context_refs: Vec::new(),
                launch_snapshot_id: None,
                result_subscriptions: Vec::new(),
            })));
        }
        self.board_insert_item(
            caller,
            BoardOperation::Create,
            request,
            origin,
            rows,
            None,
            None,
        )
    }

    pub(crate) async fn board_read(
        &self,
        caller: &BoardCaller,
        request: BoardReadRequest,
    ) -> BoardResult<BoardItemDetail> {
        let item = self.board_item_for(caller, &request.work_item_id, BoardAccess::Read)?;
        if let Some(project) = &request.project_id
            && project != &item.project_id
        {
            return Err(BoardError::NotFound {
                work_item_id: request.work_item_id,
            });
        }
        let limit = request
            .history_limit
            .map_or(HISTORY_DEFAULT, |limit| limit as usize)
            .clamp(1, HISTORY_MAX);
        let id = item.id.clone();
        let links = self
            .store
            .board_task_links_for_item(&id, false)
            .map_err(storage)?;
        let issues = self.store.board_issue_refs_for_item(&id).map_err(storage)?;
        let launches = self.store.board_launches(&id, limit).map_err(storage)?;
        let outcomes = self.store.board_outcomes(&id, limit).map_err(storage)?;
        let review_runs = self.store.board_review_runs(&id, limit).map_err(storage)?;
        let dependencies = self.store.board_dependencies(&id).map_err(storage)?;
        // The other end of blocked-by links (A7): visible on both tickets.
        let dependents = self.store.board_dependents(&id).map_err(storage)?;
        let intent = self.store.board_intent(&id).map_err(storage)?;
        let recent_events = self
            .store
            .board_recent_events(&id, limit)
            .map_err(storage)?;
        Ok(BoardItemDetail {
            view: self.board_view(item)?,
            links,
            issues,
            launches,
            outcomes,
            review_runs,
            dependencies,
            dependents,
            intent,
            recent_events,
        })
    }

    /// The project a non-user caller's All-projects scope collapses to.
    fn board_scope_project(
        &self,
        caller: &BoardCaller,
        scope: &BoardScope,
    ) -> BoardResult<Option<String>> {
        match (caller, scope) {
            (_, BoardScope::Project { project_id }) => {
                self.board_project(caller, project_id, BoardAccess::Read)?;
                Ok(Some(project_id.clone()))
            }
            (BoardCaller::User { .. } | BoardCaller::Engine { .. }, BoardScope::AllProjects) => {
                Ok(None)
            }
            (BoardCaller::Agent { project_ids, .. }, BoardScope::AllProjects) => {
                let mut ids = project_ids.iter();
                match (ids.next(), ids.next()) {
                    (Some(project), None) => {
                        self.board_project(caller, project, BoardAccess::Read)?;
                        Ok(Some(project.clone()))
                    }
                    _ => Err(BoardError::NotAuthorized {
                        reason: "Name the project to list.".into(),
                    }),
                }
            }
            (BoardCaller::Authoring { project_id, .. }, BoardScope::AllProjects) => {
                self.board_project(caller, project_id, BoardAccess::Read)?;
                Ok(Some(project_id.clone()))
            }
        }
    }

    pub(crate) async fn board_list(
        &self,
        caller: &BoardCaller,
        request: BoardListRequest,
    ) -> BoardResult<BoardListPage> {
        let project_id = self.board_scope_project(caller, &request.scope)?;
        let limit = request
            .limit
            .unwrap_or(BOARD_LIST_DEFAULT_LIMIT)
            .clamp(1, BOARD_LIST_MAX_LIMIT) as usize;
        let after = match &request.cursor {
            Some(cursor) => Some(
                BoardListCursor::decode(cursor)
                    .ok_or_else(|| BoardError::invalid("cursor", "unknown cursor"))?,
            ),
            None => None,
        };
        let text = request
            .text
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_lowercase);
        if text.as_ref().is_some_and(|text| text.len() > 200) {
            return Err(BoardError::invalid("text", "search at most 200 bytes"));
        }
        // Read the head first: a watch from here cannot miss a later change.
        let as_of_seq = self.store.board_event_head().map_err(storage)?;
        let mut rows = self
            .store
            .board_list(&BoardListQuery {
                owner_host_id: self.engine_info.device_id.clone(),
                project_id: project_id.clone(),
                columns: request.columns.clone(),
                archived: request.archived,
                text,
                after,
                limit: limit + 1,
            })
            .map_err(storage)?;
        let has_more = rows.len() > limit;
        rows.truncate(limit);
        let next_cursor = has_more.then(|| rows.last()).flatten().map(|item| {
            BoardListCursor {
                project_id: item.project_id.clone(),
                column_order: item.column.order(),
                rank: item.rank.clone(),
                id: item.id.clone(),
            }
            .encode()
        });
        let mut items = Vec::with_capacity(rows.len());
        for item in rows {
            let view = self.board_view(item)?;
            if request.attention_only && view.attention.is_empty() {
                continue;
            }
            if let Some(source) = request.source
                && view.item.origin.source_kind() != source
            {
                continue;
            }
            if let Some(provider) = request.provider
                && view
                    .working_chat
                    .as_ref()
                    .and_then(|chat| chat.harness)
                    .or(view.item.implementation.harness)
                    != Some(provider)
            {
                continue;
            }
            items.push(view);
        }
        let projects = self
            .store
            .board_project_summaries(&self.engine_info.device_id, project_id.as_deref())
            .map_err(storage)?;
        Ok(BoardListPage {
            items,
            next_cursor,
            projects,
            as_of_seq,
        })
    }

    /// `board_update`: field-level changes against an expected revision. A
    /// field conflicts only when it changed after that revision; unrelated
    /// concurrent edits merge. Launch snapshots are never touched.
    pub(crate) async fn board_update(
        &self,
        caller: &BoardCaller,
        request: BoardUpdateRequest,
    ) -> BoardResult<BoardMutationReceipt> {
        let request_id = caller.normalized_request_id(&request.request_id)?;
        let mut payload = request.clone();
        payload.request_id.clear();
        let hash = payload_hash(BoardOperation::Update, &payload)?;
        if let Some(receipt) = self.board_replay(&request_id, &hash)? {
            return Ok(receipt);
        }
        if request.changes.is_empty() {
            return Err(BoardError::invalid("changes", "name at least one field"));
        }
        let mut changes = request.changes.clone();
        if let Some(title) = &changes.title {
            changes.title = Some(clean_title(title)?);
        }
        if let Some(description) = &changes.description {
            changes.description = Some(clean_description(description)?);
        }
        if let Some(labels) = &changes.labels {
            changes.labels = Some(clean_labels(labels)?);
        }
        if let Some(refs) = &changes.context_refs {
            changes.context_refs = Some(clean_context_refs(refs)?);
        }
        if let Some(paths) = &changes.attachments {
            changes.attachments = Some(self.board_clean_brief_inputs(paths, &[])?.0);
        }
        let requested = changed_fields(&changes);
        for attempt in 0..MERGE_ATTEMPTS {
            let current = self.board_item_for(caller, &request.work_item_id, BoardAccess::Write)?;
            if let Some(paths) = &changes.attachments {
                self.board_authorize_attachments(caller, paths, &current.attachments)?;
            }
            if request.expected_revision > current.revision {
                return Err(BoardError::invalid(
                    "expectedRevision",
                    "is newer than the ticket",
                ));
            }
            if requested.iter().any(|field| {
                field_revision(&current.field_revisions, field) > request.expected_revision
            }) {
                return Err(BoardError::StaleRevision {
                    work_item_id: current.id.clone(),
                    expected: request.expected_revision,
                    current: Box::new(self.board_view(current)?),
                });
            }
            if changes.implementation.is_some() || changes.review.is_some() {
                let space = self.board_project(caller, &current.project_id, BoardAccess::Write)?;
                self.board_check_settings(
                    &space,
                    changes
                        .implementation
                        .as_ref()
                        .unwrap_or(&current.implementation),
                    changes.review.as_ref().unwrap_or(&current.review),
                )?;
            }
            let expected = current.revision;
            let mut item = current.clone();
            let changed = apply_changes(&mut item, changes.clone());
            let event = if changed.is_empty() {
                None
            } else {
                item.revision += 1;
                for field in &changed {
                    set_field_revision(&mut item.field_revisions, field, item.revision);
                }
                if changed
                    .iter()
                    .any(|field| matches!(*field, "title" | "description" | "attachments"))
                {
                    item.brief_revision += 1;
                }
                item.updated_at_ms = crate::now_ms();
                item.updated_by = caller.actor();
                Some((
                    WorkItemEventKind::Updated {
                        fields: changed.iter().map(|field| (*field).to_owned()).collect(),
                        brief_revision: item.brief_revision,
                    },
                    true,
                ))
            };
            let title_changed = changed.contains(&"title");
            let receipt = self.board_receipt_template(caller, BoardOperation::Update, &item);
            match self.board_commit(BoardCommit {
                request_id: request_id.clone(),
                payload_hash: hash.clone(),
                committed_at_ms: crate::now_ms(),
                item: BoardItemWrite::Replace {
                    expected_revision: expected,
                    item: Box::new(item),
                    placement: None,
                },
                rows: Vec::new(),
                event,
                receipt: Some(receipt),
            }) {
                Ok(Some(receipt)) => {
                    if title_changed {
                        // One rename operation keeps the working chat's
                        // title in sync (chat-model slice).
                        match self
                            .board_title_changed(
                                &receipt.item,
                                luna_proto::BoardTitleOrigin::Ticket {
                                    work_item_id: receipt.work_item_id.clone(),
                                },
                            )
                            .await
                        {
                            Ok(()) | Err(BoardError::NotImplemented { .. }) => {}
                            Err(error) => {
                                tracing::warn!(%error, work_item_id = %receipt.work_item_id, "working chat title sync failed");
                            }
                        }
                    }
                    return Ok(receipt);
                }
                Ok(None) => return Err(storage("update returned no receipt")),
                Err(BoardError::Busy { reason })
                    if attempt + 1 < MERGE_ATTEMPTS && reason == BOARD_CONCURRENT_CHANGE =>
                {
                    continue;
                }
                Err(error) => return Err(error),
            }
        }
        Err(BoardError::Busy {
            reason: "The ticket kept changing; try again.".into(),
        })
    }

    /// `board_rank`: reorder within the current column. No revision bump,
    /// no execution effect, one non-activity event.
    pub(crate) async fn board_rank(
        &self,
        caller: &BoardCaller,
        request: BoardRankRequest,
    ) -> BoardResult<BoardMutationReceipt> {
        let request_id = caller.normalized_request_id(&request.request_id)?;
        let mut payload = request.clone();
        payload.request_id.clear();
        let hash = payload_hash(BoardOperation::Rank, &payload)?;
        if let Some(receipt) = self.board_replay(&request_id, &hash)? {
            return Ok(receipt);
        }
        let item = self.board_item_for(caller, &request.work_item_id, BoardAccess::Write)?;
        let placement = match (&request.after_id, &request.before_id) {
            (Some(after), _) => BoardRankPlacement::After(self.board_load(after)?.id),
            (None, Some(before)) => BoardRankPlacement::Before(self.board_load(before)?.id),
            (None, None) => {
                return Err(BoardError::invalid(
                    "afterId",
                    "name the neighbor above (afterId) or below (beforeId)",
                ));
            }
        };
        if matches!(&placement, BoardRankPlacement::After(id) | BoardRankPlacement::Before(id) if *id == item.id)
        {
            return Err(BoardError::invalid(
                "afterId",
                "a ticket cannot be its own neighbor",
            ));
        }
        let now = crate::now_ms();
        let receipt = self.board_receipt_template(caller, BoardOperation::Rank, &item);
        self.board_commit(BoardCommit {
            request_id,
            payload_hash: hash,
            committed_at_ms: now,
            item: BoardItemWrite::Rank {
                work_item_id: item.id.clone(),
                placement,
                at_ms: now,
                actor: caller.actor(),
            },
            rows: Vec::new(),
            event: Some((
                WorkItemEventKind::Reordered {
                    column: item.column,
                    rank: String::new(),
                },
                false,
            )),
            receipt: Some(receipt),
        })
        .and_then(|receipt| receipt.ok_or_else(|| storage("rank returned no receipt")))
        .map_err(|error| match error {
            BoardError::Busy { reason } => BoardError::invalid("afterId", reason),
            other => other,
        })
    }

    /// `board_archive`: hide or restore. Never stops work or touches an
    /// external issue.
    pub(crate) async fn board_archive(
        &self,
        caller: &BoardCaller,
        request: BoardArchiveRequest,
    ) -> BoardResult<BoardMutationReceipt> {
        let request_id = caller.normalized_request_id(&request.request_id)?;
        let mut payload = request.clone();
        payload.request_id.clear();
        let operation = if request.archived {
            BoardOperation::Archive
        } else {
            BoardOperation::Unarchive
        };
        let hash = payload_hash(operation, &payload)?;
        if let Some(receipt) = self.board_replay(&request_id, &hash)? {
            return Ok(receipt);
        }
        let current = self.board_item_for(caller, &request.work_item_id, BoardAccess::Write)?;
        if request
            .expected_revision
            .is_some_and(|expected| expected != current.revision)
        {
            return Err(BoardError::StaleRevision {
                work_item_id: current.id.clone(),
                expected: request.expected_revision.unwrap_or_default(),
                current: Box::new(self.board_view(current)?),
            });
        }
        let expected = current.revision;
        let mut item = current.clone();
        let event = if item.archived == request.archived {
            None
        } else {
            item.archived = request.archived;
            item.revision += 1;
            item.updated_at_ms = crate::now_ms();
            item.updated_by = caller.actor();
            Some((
                if request.archived {
                    WorkItemEventKind::Archived
                } else {
                    WorkItemEventKind::Unarchived
                },
                true,
            ))
        };
        let receipt = self.board_receipt_template(caller, operation, &item);
        self.board_commit(BoardCommit {
            request_id,
            payload_hash: hash,
            committed_at_ms: crate::now_ms(),
            item: BoardItemWrite::Replace {
                expected_revision: expected,
                item: Box::new(item),
                placement: None,
            },
            rows: Vec::new(),
            event,
            receipt: Some(receipt),
        })?
        .ok_or_else(|| storage("archive returned no receipt"))
    }

    /// Permanent deletion is a settled lifecycle operation, never an implicit
    /// Stop, chat deletion or external issue mutation.
    pub(crate) async fn board_delete(
        &self,
        caller: &BoardCaller,
        request: BoardDeleteRequest,
    ) -> BoardResult<BoardMutationReceipt> {
        let request_id = caller.normalized_request_id(&request.request_id)?;
        let mut payload = request.clone();
        payload.request_id.clear();
        let hash = payload_hash(BoardOperation::Delete, &payload)?;
        if let Some(receipt) = self.board_replay(&request_id, &hash)? {
            return Ok(receipt);
        }
        let current = self.board_item_for(caller, &request.work_item_id, BoardAccess::Write)?;
        let _gate = self.board_lifecycle_gate(&current.id).await;
        if let Some(receipt) = self.board_replay(&request_id, &hash)? {
            return Ok(receipt);
        }
        let mut item = self.board_item_for(caller, &current.id, BoardAccess::Write)?;
        if request.expected_revision != item.revision {
            return Err(BoardError::StaleRevision {
                work_item_id: item.id.clone(),
                expected: request.expected_revision,
                current: Box::new(self.board_view(item)?),
            });
        }
        let view = self.board_view(item.clone())?;
        if !view.actions.contains(&BoardAction::Delete) {
            return Err(BoardError::Busy { reason: "Finish or stop the ticket's active and queued chat work and reviews before deleting it. Move Ready tickets to Backlog first.".into() });
        }
        let expected_revision = item.revision;
        item.deleted = true;
        item.archived = true;
        item.revision += 1;
        item.updated_at_ms = crate::now_ms();
        item.updated_by = caller.actor();
        let receipt = self.board_receipt_template(caller, BoardOperation::Delete, &item);
        self.board_commit(BoardCommit {
            request_id,
            payload_hash: hash,
            committed_at_ms: item.updated_at_ms,
            item: BoardItemWrite::Replace {
                expected_revision,
                item: Box::new(item),
                placement: None,
            },
            rows: Vec::new(),
            event: Some((WorkItemEventKind::Deleted, true)),
            receipt: Some(receipt),
        })?
        .ok_or_else(|| storage("delete returned no receipt"))
    }

    /// Reconcile an uncertain mutation by its original key.
    pub(crate) fn board_get_receipt(
        &self,
        caller: &BoardCaller,
        request: BoardReceiptRequest,
    ) -> BoardResult<Option<BoardMutationReceipt>> {
        let request_id = caller.normalized_request_id(&request.request_id)?;
        let receipt = self.store.board_receipt(&request_id).map_err(storage)?;
        if let Some(receipt) = &receipt {
            self.board_check_caller(caller, &receipt.item.project_id, BoardAccess::Read)?;
        }
        Ok(receipt)
    }

    /// Bounded journal read, oldest first after the cursor.
    pub(crate) fn board_events(
        &self,
        caller: &BoardCaller,
        request: BoardEventsRequest,
    ) -> BoardResult<BoardEventPage> {
        let retained = request
            .work_item_id
            .as_deref()
            .map(|id| self.board_load_retained(id))
            .transpose()?;
        let project_id = match &retained {
            Some(item) => {
                self.board_check_caller(caller, &item.project_id, BoardAccess::Read)?;
                if item.owner_host_id != self.engine_info.device_id {
                    return Err(BoardError::WrongHost {
                        project_id: item.project_id.clone(),
                        owner_host_id: item.owner_host_id.clone(),
                    });
                }
                Some(item.project_id.clone())
            }
            None => self.board_scope_project(caller, &request.scope)?,
        };
        let work_item_id = retained.map(|item| item.id);
        let limit = request
            .limit
            .map_or(EVENTS_DEFAULT, |limit| limit as usize)
            .clamp(1, BOARD_EVENTS_MAX_LIMIT as usize);
        let after = request.after_seq.unwrap_or(0);
        let mut events = self
            .store
            .board_events(&BoardEventQuery {
                after_seq: after,
                work_item_id,
                project_id,
                activity_only: request.activity_only,
                limit: limit + 1,
            })
            .map_err(storage)?;
        let has_more = events.len() > limit;
        events.truncate(limit);
        Ok(BoardEventPage {
            cursor: events.last().map_or(after, |event| event.seq),
            events,
            has_more,
        })
    }

    /// `BoardWatch`: one frame now, then a frame after each committed board
    /// change or observation change in scope.
    pub(crate) fn board_watch(
        &self,
        caller: &BoardCaller,
        request: BoardWatchRequest,
    ) -> BoardResult<BoxStream<'static, Value>> {
        let project_id = self.board_scope_project(caller, &request.scope)?;
        let store = self.store.clone();
        let head = store.board_event_head().map_err(storage)?;
        let changes = store.board_changes();
        let observed = self.board.observed();
        struct State {
            store: std::sync::Arc<DocsStore>,
            project_id: Option<String>,
            cursor: u64,
            first: bool,
            changes: tokio::sync::watch::Receiver<u64>,
            observed: tokio::sync::broadcast::Receiver<Vec<String>>,
        }
        let state = State {
            store,
            project_id,
            cursor: request.after_seq.unwrap_or(head),
            first: true,
            changes,
            observed,
        };
        let frame = |state: &mut State, observed: Vec<String>| -> Option<BoardWatchFrame> {
            let events: Vec<WorkItemEvent> = state
                .store
                .board_events(&BoardEventQuery {
                    after_seq: state.cursor,
                    work_item_id: None,
                    project_id: state.project_id.clone(),
                    activity_only: false,
                    limit: WATCH_EVENTS_MAX + 1,
                })
                .ok()?;
            let head = state.store.board_event_head().ok()?;
            let reset = events.len() > WATCH_EVENTS_MAX || state.cursor > head;
            let frame = BoardWatchFrame {
                seq: head,
                events: if reset { Vec::new() } else { events },
                observed,
                reset,
            };
            state.cursor = head;
            Some(frame)
        };
        Ok(
            futures::stream::unfold((state, frame), |(mut state, frame)| async move {
                if state.first {
                    state.first = false;
                    let first = frame(&mut state, Vec::new())?;
                    return Some((serde_json::to_value(&first).ok()?, (state, frame)));
                }
                loop {
                    let mut observed = Vec::new();
                    tokio::select! {
                        changed = state.changes.changed() => changed.ok()?,
                        ids = state.observed.recv() => match ids {
                            Ok(ids) => observed = ids,
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
                        },
                        _ = tokio::time::sleep(WATCH_POLL) => {}
                    }
                    let next = frame(&mut state, observed)?;
                    if next.events.is_empty() && next.observed.is_empty() && !next.reset {
                        continue;
                    }
                    return Some((serde_json::to_value(&next).ok()?, (state, frame)));
                }
            })
            .boxed(),
        )
    }
}
