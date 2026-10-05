//! Shared work projection and durable exact-target stop receipts.
use super::*;
use crate::sessions::WorkRuntime;
use luna_doc::{QueueMessageStatus, SubagentStatus};
use luna_proto::{
    TaskWorkItem, TaskWorkKind as Kind, TaskWorkSnapshot, TaskWorkState as State,
    TaskWorkStopPreview, TaskWorkStopReceipt, TaskWorkStopResult, TaskWorkStopStatus as StopStatus,
};

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct WorkRead {
    pub task_id: String,
    #[serde(default)]
    pub offset: usize,
    #[serde(default)]
    pub limit: Option<usize>,
}
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct WorkStop {
    pub task_id: String,
    pub work_ids: Vec<String>,
    #[serde(default)]
    pub preview_id: String,
    #[serde(default)]
    pub request_id: String,
}
#[derive(Serialize, Deserialize)]
struct StoredStop {
    request_hash: String,
    receipt: TaskWorkStopReceipt,
}
#[derive(Clone, PartialEq, Eq, Serialize)]
enum Control {
    Turn {
        runtime: WorkRuntime,
    },
    Native {
        runtime: WorkRuntime,
        tool_id: String,
    },
}
#[derive(Clone)]
struct Row {
    item: TaskWorkItem,
    control: Option<Control>,
}
struct WorkEntry {
    doc_id: String,
    entry: SessionMessageEntry,
    origin: luna_proto::WorkOrigin,
}
fn id(parts: &impl Serialize) -> Result<String, crate::EngineError> {
    Ok(format!(
        "work-{}",
        &crate::context_transfers::digest(parts)?[..40]
    ))
}
fn error(message: &str) -> crate::EngineError {
    crate::EngineError::Other(message.into())
}
fn subagent_state(status: Option<SubagentStatus>) -> State {
    match status {
        Some(SubagentStatus::Done) => State::Completed,
        Some(SubagentStatus::Failed) => State::Failed,
        Some(SubagentStatus::Interrupted) => State::Stopped,
        Some(SubagentStatus::Waiting | SubagentStatus::Idle) => State::Waiting,
        Some(SubagentStatus::Running) => State::Running,
        _ => State::Unknown,
    }
}
#[derive(Clone)]
struct WorkProjection {
    work_writes: Option<crate::device_write::DeviceWriteHandle>,
    terminals: Terminals,
    workspace: WorkspaceHost,
    doc_host: DocHost,
    sessions: crate::sessions::SessionsEngine,
    store: std::sync::Arc<DocsStore>,
    engine_info: EngineInfo,
}
impl WorkProjection {
    /// Follow only engine-stamped native child references under this task.
    /// A finished intermediate transcript still owns live nested work.
    fn work_entries(
        &self,
        chat: &Chat,
        entries: Vec<SessionMessageEntry>,
    ) -> Result<(Vec<WorkEntry>, bool), crate::EngineError> {
        let mut root_entries = Some(entries);
        let mut pending = std::collections::VecDeque::from([(chat.id.clone(), None)]);
        let mut visited = HashSet::from([chat.id.clone()]);
        let mut result = Vec::new();
        let mut truncated = false;
        while let Some((doc_id, inherited)) = pending.pop_front() {
            let entries = if doc_id == chat.id {
                root_entries.take().unwrap_or_default()
            } else {
                self.doc_host.open(&doc_id)?.doc().read_entries()?
            };
            let start = entries.len().saturating_sub(2048);
            truncated |= start > 0;
            let mut message = entries
                .iter()
                .take(start)
                .rev()
                .find(|entry| entry.role == MessageRole::User)
                .map(|entry| entry.id.clone());
            for entry in entries.into_iter().skip(start) {
                if entry.role == MessageRole::User && inherited.is_none() {
                    message = Some(entry.id);
                    continue;
                }
                let origin = inherited.clone().or_else(|| {
                    message
                        .as_deref()
                        .map(|message| self.sessions.work_origin(&chat.id, message))
                });
                let Some(origin) = origin else {
                    continue;
                };
                for part in &entry.parts {
                    if let MessagePart::Tool {
                        id,
                        call,
                        subagent_ref: Some(child),
                        ..
                    } = part
                        && call.is_subagent_spawn()
                        && *child == crate::sessions::subagent_doc_id(&chat.id, id)
                        && !visited.contains(child)
                    {
                        if visited.len() >= 64 {
                            truncated = true;
                        } else {
                            visited.insert(child.clone());
                            pending.push_back((child.clone(), Some(origin.clone())));
                        }
                    }
                }
                result.push(WorkEntry {
                    doc_id: doc_id.clone(),
                    entry,
                    origin,
                });
                if result.len() >= 4096 {
                    return Ok((result, true));
                }
            }
        }
        Ok((result, truncated))
    }
    fn work_task(&self, id: &str) -> Result<Chat, crate::EngineError> {
        let row = self
            .workspace
            .chat(id)?
            .ok_or_else(|| error("Task no longer exists"))?;
        if self.store.task_cleanup_state(id)?.is_some()
            || row
                .space_id
                .as_deref()
                .map(|p| self.store.project_cleanup_state(p))
                .transpose()?
                .flatten()
                .is_some()
        {
            return Err(error("Task cleanup is in progress"));
        }
        if row.device_id != self.engine_info.device_id {
            return Err(error(
                "Work state is available only from the task's owning host",
            ));
        }
        Ok(row)
    }
    fn work_rows(&self, owner: &str) -> Result<(Vec<Row>, bool), crate::EngineError> {
        let target = self.work_task(owner)?;
        let mut chats = self.workspace.read_chats()?;
        chats.retain(|c| {
            c.device_id == target.device_id
                && c.space_id == target.space_id
                && (c.id == owner
                    || c.child_thread.as_ref().is_some_and(|child| {
                        child.parent_task_id == owner
                            || child
                                .work_origin
                                .as_ref()
                                .is_some_and(|o| o.owner_task_id == owner)
                    }))
        });
        chats.sort_by(|a, b| a.id.cmp(&b.id));
        let mut truncated = chats.len() > 256;
        chats.truncate(256);
        let mut rows = vec![];
        for chat in &chats {
            let handle = self.doc_host.open(&chat.id)?;
            let entries = handle.doc().read_entries()?;
            let mut native_tasks = handle.doc().native_tasks().tasks;
            native_tasks.extend(handle.doc().native_shell_tasks().tasks);
            let native_shell_tools: HashSet<_> = native_tasks
                .iter()
                .filter(|task| task.kind == luna_proto::NativeTaskKind::Shell)
                .filter_map(|task| task.tool_use_id.as_deref())
                .collect();
            let runtime = self.sessions.work_runtime(&chat.id);
            if chat.id != owner
                && let Some(child) = &chat.child_thread
            {
                let independent = runtime
                    .as_ref()
                    .is_some_and(|r| r.message_id != child.initial_message_id);
                let owned = child
                    .work_origin
                    .as_ref()
                    .is_some_and(|o| o.owner_task_id == owner)
                    && !independent;
                let live = runtime.as_ref().filter(|r| {
                    r.message_id == child.initial_message_id
                        && self.sessions.active_turn_id(&chat.id).is_some()
                });
                let initial = entries
                    .iter()
                    .position(|e| e.id == child.initial_message_id);
                let reply = initial.and_then(|i| {
                    entries
                        .iter()
                        .skip(i + 1)
                        .take_while(|e| e.role != MessageRole::User)
                        .filter(|e| e.role == MessageRole::Assistant)
                        .last()
                });
                let state = if live.is_some() {
                    State::Running
                } else if let Some(reply) = reply {
                    match reply.status {
                        Some(MessageStatus::Complete) => State::Completed,
                        Some(MessageStatus::Aborted) => State::Stopped,
                        _ => State::Unknown,
                    }
                } else if initial.is_some() {
                    State::Unknown
                } else {
                    State::Starting
                };
                let control = if owned {
                    live.map(|r| Control::Turn { runtime: r.clone() })
                } else {
                    None
                };
                rows.push(Row {
                    item: TaskWorkItem {
                        id: id(&("child", &chat.id, &child.initial_message_id))?,
                        task_id: chat.id.clone(),
                        title: chat.title.clone().unwrap_or_else(|| "Child chat".into()),
                        kind: Kind::ChildChat,
                        state,
                        origin: child.work_origin.clone(),
                        owned,
                        blocks_completion: owned && !state.terminal(),
                        needs_attention: state == State::Unknown,
                        can_stop: control.is_some(),
                        detail: if independent {
                            "Initial delegated turn ended; the current turn is independently owned"
                        } else if child.work_origin.is_none() {
                            "Ancestry only; no recorded lifecycle ownership"
                        } else if state == State::Starting {
                            "Durable startup intent; provider acceptance is not yet observed"
                        } else if state.terminal() {
                            "Delegated turn ended; nested work is listed separately"
                        } else {
                            "Exact initial delegated message"
                        }
                        .into(),
                    },
                    control,
                });
                for outcome in child.outcomes.iter().rev().take(32) {
                    if !child.report_results {
                        continue;
                    }
                    let parent = self.workspace.chat(&child.parent_task_id)?;
                    let parent_handle = if parent
                        .as_ref()
                        .is_some_and(|p| p.device_id == self.engine_info.device_id)
                    {
                        Some(self.doc_host.open(&child.parent_task_id)?)
                    } else {
                        None
                    };
                    let message_id = format!("child-report:{}", outcome.id);
                    let queued = parent_handle
                        .as_ref()
                        .map(|h| h.doc().read_queue())
                        .transpose()?
                        .unwrap_or_default()
                        .into_iter()
                        .find(|q| q.id == message_id);
                    let (state, detail) = if !outcome.delivered {
                        (State::Waiting, "Result awaits publication to the parent")
                    } else if let Some(q) = queued {
                        (
                            if matches!(q.status, QueueMessageStatus::DeliveryUnknown { .. }) {
                                State::Unknown
                            } else {
                                State::Waiting
                            },
                            "Result published; parent continuation is still queued or unconfirmed",
                        )
                    } else if child.continue_parent {
                        let confirmed = self.store.task_command_receipt_for_correlation(
                            &child.parent_task_id,
                            &message_id,
                        )?;
                        if confirmed.is_some_and(|r| r.status == CommandReceiptStatus::Applied) {
                            (
                                State::Completed,
                                "Parent provider confirmed result continuation",
                            )
                        } else {
                            if parent_handle
                                .as_ref()
                                .map(|h| h.doc().read_entries())
                                .transpose()?
                                .unwrap_or_default()
                                .iter()
                                .any(|e| {
                                    e.id == format!("child-continuation-skipped:{}", outcome.id)
                                })
                            {
                                (
                                    State::Completed,
                                    "Result published; parent continuation held by provider permission policy",
                                )
                            } else {
                                (
                                    State::Waiting,
                                    "Result published; parent continuation has no confirmed native receipt",
                                )
                            }
                        }
                    } else {
                        (
                            State::Completed,
                            "Result published; no parent wake requested",
                        )
                    };
                    let outcome_origin = entries
                        .iter()
                        .position(|entry| entry.id == outcome.turn_id)
                        .and_then(|index| {
                            entries[..index]
                                .iter()
                                .rev()
                                .find(|entry| entry.role == MessageRole::User)
                        })
                        .map(|entry| self.sessions.work_origin(&chat.id, &entry.id));
                    let outcome_owned = outcome_origin
                        .as_ref()
                        .is_some_and(|origin| origin.owner_task_id == owner);
                    rows.push(Row {
                        item: TaskWorkItem {
                            id: id(&("result", &chat.id, &outcome.id))?,
                            task_id: chat.id.clone(),
                            title: "Child result delivery".into(),
                            kind: Kind::ResultDelivery,
                            state,
                            origin: outcome_origin,
                            owned: outcome_owned,
                            blocks_completion: outcome_owned && !state.terminal(),
                            needs_attention: !state.terminal(),
                            can_stop: false,
                            detail: detail.into(),
                        },
                        control: None,
                    });
                }
            }
            let mut tool_origins = HashMap::new();
            let (sources, limited) = self.work_entries(chat, entries)?;
            truncated |= limited;
            for WorkEntry {
                doc_id,
                entry,
                origin,
            } in sources
            {
                let owned = origin.owner_task_id == owner;
                for part in &entry.parts {
                    let MessagePart::Tool {
                        id: tool,
                        call,
                        resolved,
                        is_error,
                        subagent_status,
                        ..
                    } = part
                    else {
                        continue;
                    };
                    tool_origins.insert(tool.clone(), origin.clone());
                    let kind = if call.is_subagent_spawn() {
                        Kind::NativeChild
                    } else if matches!(call, ToolCall::Exec { .. })
                        && !*resolved
                        && !native_shell_tools.contains(tool.as_str())
                    {
                        Kind::Shell
                    } else {
                        continue;
                    };
                    if !owned && chat.id != owner {
                        continue;
                    }
                    let mut state = if kind == Kind::NativeChild {
                        subagent_state(*subagent_status)
                    } else if *is_error {
                        State::Failed
                    } else {
                        State::Running
                    };
                    let observed = if kind == Kind::NativeChild {
                        self.sessions.work_observed(&chat.id, "child", tool)
                    } else {
                        runtime
                            .as_ref()
                            .is_some_and(|r| r.message_id == origin.launch_turn_id)
                            && self.sessions.active_turn_id(&chat.id).is_some()
                    };
                    if !state.terminal() && !observed {
                        state = State::Unknown;
                    }
                    let control = runtime
                        .as_ref()
                        .filter(|r| {
                            owned
                                && kind == Kind::NativeChild
                                && !state.terminal()
                                && state != State::Unknown
                                && r.provider == HarnessId::ClaudeCode
                                && r.native_session_id.is_some()
                                && (origin.owner_task_id == chat.id
                                    || r.message_id == origin.launch_turn_id)
                        })
                        .map(|r| Control::Native {
                            runtime: r.clone(),
                            tool_id: tool.clone(),
                        });
                    let title = if kind == Kind::NativeChild {
                        match call {
                            ToolCall::Unknown { name, .. } => name.clone(),
                            _ => "Native child".into(),
                        }
                    } else {
                        "Shell command".into()
                    };
                    rows.push(Row{item:TaskWorkItem{id:id(&("native-tool",&chat.id,&doc_id,&entry.id,tool))?,task_id:chat.id.clone(),title,kind,state,origin:Some(origin.clone()),owned,blocks_completion:owned&&!state.terminal(),needs_attention:matches!(state,State::Waiting|State::Unknown|State::Failed),can_stop:control.is_some(),detail:if state==State::Unknown{"Historical observation; live ownership is not confirmed"}else if control.is_none()&&!state.terminal(){"This provider does not expose an addressable stop for this work item"}else{"Provider-observed lifecycle; stop acknowledgement alone is not terminal evidence"}.into()},control});
                }
            }
            for task in native_tasks {
                let origin = task
                    .tool_use_id
                    .as_ref()
                    .and_then(|t| tool_origins.get(t))
                    .cloned();
                let owned = origin.as_ref().is_some_and(|o| o.owner_task_id == owner);
                if !owned && chat.id != owner {
                    continue;
                }
                let kind = if task.kind == luna_proto::NativeTaskKind::Shell {
                    Kind::Shell
                } else {
                    Kind::Workflow
                };
                let mut state = match task.status {
                    luna_proto::NativeTaskStatus::Pending => State::Starting,
                    luna_proto::NativeTaskStatus::Running => State::Running,
                    luna_proto::NativeTaskStatus::Paused => State::Waiting,
                    luna_proto::NativeTaskStatus::Completed => State::Completed,
                    luna_proto::NativeTaskStatus::Failed => State::Failed,
                    luna_proto::NativeTaskStatus::Stopped => State::Stopped,
                };
                if !state.terminal()
                    && !(self.sessions.work_observed(&chat.id, "task", &task.task_id)
                        && runtime.as_ref().is_some_and(|r| {
                            r.native_session_id.is_some()
                                && r.native_session_id == task.provider_session_id
                        }))
                {
                    state = State::Unknown
                }
                rows.push(Row{item:TaskWorkItem{id:id(&("workflow",&chat.id,&task.task_id,&task.provider_session_id))?,task_id:chat.id.clone(),title:task.name.unwrap_or_else(||task.kind.label().into()),kind,state,origin,owned,blocks_completion:owned&&!state.terminal(),needs_attention:matches!(state,State::Unknown|State::Waiting|State::Failed),can_stop:false,detail:"Provider-observed background work; individual stop is unavailable in this adapter".into()},control:None});
            }
        }
        let terminals = self.terminals.list(None)?;
        for run in self
            .store
            .project_command_runs(&self.engine_info.device_id)?
            .into_iter()
            .filter(|r| r.chat_id == owner)
        {
            let mut state = match run.status {
                luna_proto::ProjectCommandStatus::Pending => State::Starting,
                luna_proto::ProjectCommandStatus::Running => State::Running,
                luna_proto::ProjectCommandStatus::Succeeded => State::Completed,
                luna_proto::ProjectCommandStatus::Failed => State::Failed,
                luna_proto::ProjectCommandStatus::Canceled => State::Stopped,
            };
            if state == State::Running {
                state = match terminals
                    .iter()
                    .find(|t| Some(t.id.as_str()) == run.terminal_id.as_deref())
                    .map(|t| t.state)
                {
                    Some(luna_proto::TerminalProcessState::Running) => State::Running,
                    Some(luna_proto::TerminalProcessState::Exited) => State::Stopped,
                    _ => State::Unknown,
                };
            }
            rows.push(Row {
                item: TaskWorkItem {
                    id: id(&("service", &run.id, run.attempt))?,
                    task_id: owner.into(),
                    title: run.action.name,
                    kind: Kind::Service,
                    state,
                    origin: None,
                    owned: false,
                    blocks_completion: false,
                    needs_attention: state == State::Failed,
                    can_stop: false,
                    detail: "Checkout service; use its project command control to stop it".into(),
                },
                control: None,
            });
        }
        rows.sort_by(|a, b| {
            a.item
                .state
                .terminal()
                .cmp(&b.item.state.terminal())
                .then_with(|| a.item.id.cmp(&b.item.id))
        });
        Ok((rows, truncated))
    }
    pub(super) fn task_work(&self, p: WorkRead) -> Result<TaskWorkSnapshot, crate::EngineError> {
        let (rows, truncated) = self.work_rows(&p.task_id)?;
        let total = rows.len();
        let limit = p.limit.unwrap_or(50).clamp(1, 100);
        Ok(TaskWorkSnapshot {
            schema_version: 1,
            task_id: p.task_id,
            host_device_id: self.engine_info.device_id.clone(),
            blocking_count: rows.iter().filter(|r| r.item.blocks_completion).count(),
            service_count: rows
                .iter()
                .filter(|r| r.item.kind == Kind::Service && !r.item.state.terminal())
                .count(),
            unknown_count: rows
                .iter()
                .filter(|r| r.item.state == State::Unknown)
                .count(),
            total_count: total,
            next_offset: (p.offset.saturating_add(limit) < total).then(|| p.offset + limit),
            truncated,
            items: rows
                .into_iter()
                .skip(p.offset)
                .take(limit)
                .map(|r| r.item)
                .collect(),
        })
    }
    fn work_stop_plan(
        &self,
        p: &WorkStop,
    ) -> Result<(TaskWorkStopPreview, Vec<Row>), crate::EngineError> {
        if p.work_ids.is_empty() || p.work_ids.len() > 32 {
            return Err(error("Select between one and 32 exact work items"));
        }
        let (all, _) = self.work_rows(&p.task_id)?;
        let wanted: HashSet<_> = p.work_ids.iter().collect();
        if wanted.len() != p.work_ids.len() {
            return Err(error("Duplicate work selection"));
        }
        let rows: Vec<_> = all
            .into_iter()
            .filter(|r| wanted.contains(&r.item.id))
            .collect();
        if rows.len() != wanted.len() {
            return Err(error("Selected work changed; reload and preview again"));
        }
        let digest = crate::context_transfers::digest(
            &rows
                .iter()
                .map(|r| (&r.item, &r.control))
                .collect::<Vec<_>>(),
        )?;
        Ok((
            TaskWorkStopPreview {
                task_id: p.task_id.clone(),
                preview_id: format!("work-stop-{digest}"),
                items: rows
                    .iter()
                    .filter(|r| r.control.is_some())
                    .map(|r| r.item.clone())
                    .collect(),
                excluded: rows
                    .iter()
                    .filter(|r| r.control.is_none())
                    .map(|r| r.item.clone())
                    .collect(),
            },
            rows,
        ))
    }
    pub(super) fn preview_stop_task_work(
        &self,
        p: WorkStop,
    ) -> Result<TaskWorkStopPreview, crate::EngineError> {
        Ok(self.work_stop_plan(&p)?.0)
    }
    pub(super) async fn stop_task_work(
        &self,
        p: WorkStop,
    ) -> Result<TaskWorkStopReceipt, crate::EngineError> {
        self.work_task(&p.task_id)?;
        let _permit = self
            .work_writes
            .as_ref()
            .map(|w| w.try_admit())
            .transpose()
            .map_err(|e| error(&format!("Work control storage is unavailable: {e:?}")))?;
        if p.request_id.is_empty() || p.request_id.len() > 160 {
            return Err(error("A bounded stable stop request ID is required"));
        }
        let _gate = self.doc_host.task_command_gate(&p.task_id).await;
        let key = format!(
            "luna-work-stop:{}",
            crate::context_transfers::digest(&(&p.task_id, &p.request_id))?
        );
        let mut ids = p.work_ids.clone();
        ids.sort();
        let request_hash = crate::context_transfers::digest(&(&p.task_id, &p.preview_id, ids))?;
        if let Some(bytes) = self.store.load_snapshot(&key)? {
            let stored: StoredStop = serde_json::from_slice(&bytes)?;
            if stored.request_hash != request_hash {
                return Err(error("Stop request ID already used for another preview"));
            }
            return Ok(stored.receipt);
        }
        let (preview, rows) = self.work_stop_plan(&p)?;
        if preview.preview_id != p.preview_id {
            return Err(error(
                "Work changed after preview; review the exact targets again",
            ));
        }
        let mut receipt = TaskWorkStopReceipt {
            request_id: p.request_id,
            task_id: p.task_id,
            preview_id: p.preview_id,
            results: rows
                .iter()
                .map(|r| TaskWorkStopResult {
                    work_id: r.item.id.clone(),
                    status: if r.control.is_some() {
                        StopStatus::Unknown
                    } else {
                        StopStatus::Rejected
                    },
                    detail: if r.control.is_some() {
                        "Stop intent recorded; dispatch outcome not yet known".into()
                    } else {
                        r.item.detail.clone()
                    },
                })
                .collect(),
        };
        // The complete selection is durable before any side effect. Crash recovery
        // returns unknown; it never redispatches these stops.
        self.store.save_snapshot(
            &key,
            &serde_json::to_vec(&StoredStop {
                request_hash: request_hash.clone(),
                receipt: receipt.clone(),
            })?,
        )?;
        for (index, row) in rows.iter().enumerate() {
            if row.control.is_none() {
                continue;
            }
            let still_owned = self.work_task(&row.item.task_id).is_ok()
                && self.work_rows(&receipt.task_id)?.0.iter().any(|current| {
                    current.item.id == row.item.id
                        && current.item.owned
                        && current.item.can_stop
                        && current.control == row.control
                });
            if !still_owned {
                receipt.results[index].status = StopStatus::Rejected;
                receipt.results[index].detail =
                    "Work ownership or execution changed before stop".into();
                self.store.save_snapshot(
                    &key,
                    &serde_json::to_vec(&StoredStop {
                        request_hash: request_hash.clone(),
                        receipt: receipt.clone(),
                    })?,
                )?;
                continue;
            }
            let result = match &row.control {
                Some(Control::Turn { runtime }) => {
                    match self
                        .sessions
                        .stop_work_turn(&row.item.task_id, runtime)
                        .await
                    {
                        Ok(true)
                            if self
                                .sessions
                                .work_runtime(&row.item.task_id)
                                .is_none_or(|r| r.run_id != runtime.run_id) =>
                        {
                            (StopStatus::Stopped, "Launching turn ended".into())
                        }
                        Ok(true) => (StopStatus::Requested, "Awaiting terminal evidence".into()),
                        Ok(false) => (
                            StopStatus::Rejected,
                            "Launching turn changed before stop".into(),
                        ),
                        Err(_) => (
                            StopStatus::Unknown,
                            "Interruption outcome could not be confirmed".into(),
                        ),
                    }
                }
                Some(Control::Native { runtime, tool_id }) => {
                    match self
                        .sessions
                        .stop_work_native(&row.item.task_id, runtime, tool_id)
                        .await
                    {
                        Ok(luna_harness::NativeTaskStop::Requested) => (
                            StopStatus::Requested,
                            "Provider acknowledged stop; awaiting terminal evidence".into(),
                        ),
                        Ok(luna_harness::NativeTaskStop::Rejected(_)) => (
                            StopStatus::Rejected,
                            "Provider could not address this live child".into(),
                        ),
                        Ok(luna_harness::NativeTaskStop::Unknown(_)) | Err(_) => (
                            StopStatus::Unknown,
                            "Provider stop outcome could not be confirmed".into(),
                        ),
                    }
                }
                None => continue,
            };
            receipt.results[index].status = result.0;
            receipt.results[index].detail = result.1;
            self.store.save_snapshot(
                &key,
                &serde_json::to_vec(&StoredStop {
                    request_hash: request_hash.clone(),
                    receipt: receipt.clone(),
                })?,
            )?;
        }
        Ok(receipt)
    }
}

impl EngineRpc {
    pub(super) async fn handle_task_work(
        &self,
        method: &str,
        params: Value,
    ) -> Result<RpcReply, RpcError> {
        let view = WorkProjection {
            work_writes: self.work_writes.clone(),
            terminals: self.terminals.clone(),
            workspace: self.workspace.clone(),
            doc_host: self.doc_host.clone(),
            sessions: self.sessions.clone(),
            store: self.store.clone(),
            engine_info: self.engine_info.clone(),
        };
        let failed = |e: crate::EngineError| RpcError::Failed(e.to_string());
        match method {
            methods::GET_TASK_WORK => {
                RpcReply::value(&view.task_work(parse_params(params)?).map_err(failed)?)
            }
            methods::PREVIEW_STOP_TASK_WORK => RpcReply::value(
                &view
                    .preview_stop_task_work(parse_params(params)?)
                    .map_err(failed)?,
            ),
            methods::STOP_TASK_WORK => RpcReply::value(
                &view
                    .stop_task_work(parse_params(params)?)
                    .await
                    .map_err(failed)?,
            ),
            methods::WATCH_TASK_WORK => {
                let p: WorkRead = parse_params(params)?;
                let initial = view.task_work(p.clone()).map_err(failed)?;
                Ok(RpcReply::Stream(
                    futures::stream::unfold(
                        (view, p, Some(initial), None),
                        |(view, p, initial, mut previous)| async move {
                            let mut current = initial;
                            loop {
                                let next = if let Some(first) = current.take() {
                                    first
                                } else {
                                    tokio::time::sleep(Duration::from_millis(750)).await;
                                    view.task_work(p.clone()).ok()?
                                };
                                if previous.as_ref() == Some(&next) {
                                    continue;
                                }
                                let value = serde_json::to_value(&next).ok()?;
                                previous = Some(next);
                                return Some((value, (view, p, None, previous)));
                            }
                        },
                    )
                    .boxed(),
                ))
            }
            _ => Err(RpcError::UnknownMethod(method.into())),
        }
    }
}
