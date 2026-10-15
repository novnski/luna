//! Codex background terminals: a unified-exec command the agent leaves running
//! when its turn ends (a dev or preview server). Its `commandExecution` item
//! stays `inProgress` past `turn/completed`; the `item/completed` that carries
//! the exit code arrives whenever the process exits, under the original
//! `turnId` and possibly several turns later. The process belongs to this
//! app-server: ending the app-server ends it (verified against Codex 0.162.1).
//!
//! Each one is mirrored as a provider-native Shell task, so the parked session
//! shows it, the engine keeps the provider process (and so the server) alive
//! while it runs, and the ledger records how it ended.

use std::collections::HashMap;

use luna_proto::{AgentEvent, NativeTaskKind, NativeTaskSnapshot, NativeTaskStatus};
use serde_json::Value;

/// Commands tracked at once, in flight or in the background.
const MAX_TRACKED: usize = 64;
const MAX_DESCRIPTION_CHARS: usize = 200;

struct Command {
    turn_id: String,
    description: String,
    /// A Running snapshot was emitted when its turn ended.
    background: bool,
}

/// Root-thread command executions for one app-server process.
#[derive(Default)]
pub(crate) struct BackgroundTerminals {
    commands: HashMap<String, Command>,
    revision: u64,
}

impl BackgroundTerminals {
    /// `item/started`: only a unified-exec session (it has a `processId`) can
    /// outlive its turn.
    pub(crate) fn started(&mut self, turn_id: &str, item: &Value) {
        let Some(id) = command_id(item) else {
            return;
        };
        let has_process = item
            .get("processId")
            .and_then(Value::as_str)
            .is_some_and(|process| !process.is_empty());
        if !has_process
            || turn_id.is_empty()
            || self.commands.contains_key(id)
            || self.commands.len() >= MAX_TRACKED
        {
            return;
        }
        self.commands.insert(
            id.to_owned(),
            Command {
                turn_id: turn_id.to_owned(),
                description: description(item),
                background: false,
            },
        );
    }

    /// `turn/completed`: that turn's commands still running are now
    /// background terminals.
    pub(crate) fn turn_completed(&mut self, thread_id: &str, turn_id: &str) -> Vec<AgentEvent> {
        let mut ids: Vec<String> = self
            .commands
            .iter()
            .filter(|(_, command)| !command.background && command.turn_id == turn_id)
            .map(|(id, _)| id.clone())
            .collect();
        ids.sort();
        ids.into_iter()
            .map(|id| {
                let command = self.commands.get_mut(&id).expect("listed above");
                command.background = true;
                let description = command.description.clone();
                self.snapshot(thread_id, id, description, NativeTaskStatus::Running, None)
            })
            .collect()
    }

    /// `item/completed`: the process exited. A background terminal reports
    /// its exit; a command that finished inside its turn reports nothing new.
    pub(crate) fn completed(&mut self, thread_id: &str, item: &Value) -> Option<AgentEvent> {
        let id = command_id(item)?;
        let command = self.commands.remove(id)?;
        if !command.background {
            return None;
        }
        let exit_code = item
            .get("exitCode")
            .or_else(|| item.get("exit_code"))
            .and_then(Value::as_i64);
        let completed = item.get("status").and_then(Value::as_str) == Some("completed")
            && exit_code.unwrap_or(0) == 0;
        let outcome = match exit_code {
            Some(code) => format!("Exited with code {code}"),
            None if completed => "Exited".to_owned(),
            None => "Exited without an exit code".to_owned(),
        };
        let status = if completed {
            NativeTaskStatus::Completed
        } else {
            NativeTaskStatus::Failed
        };
        Some(self.snapshot(
            thread_id,
            id.to_owned(),
            command.description,
            status,
            Some(outcome),
        ))
    }

    fn snapshot(
        &mut self,
        thread_id: &str,
        id: String,
        description: String,
        status: NativeTaskStatus,
        outcome: Option<String>,
    ) -> AgentEvent {
        self.revision += 1;
        AgentEvent::NativeTaskChanged {
            task: Box::new(NativeTaskSnapshot {
                task_id: id.clone(),
                kind: NativeTaskKind::Shell,
                provider_session_id: Some(thread_id.to_owned()),
                // The command's own transcript chip.
                tool_use_id: Some(id),
                description,
                status,
                error: outcome
                    .clone()
                    .filter(|_| status == NativeTaskStatus::Failed),
                summary: outcome,
                revision: self.revision,
                ..Default::default()
            }),
        }
    }
}

fn command_id(item: &Value) -> Option<&str> {
    if !matches!(
        item.get("type").and_then(Value::as_str),
        Some("commandExecution" | "command_execution")
    ) {
        return None;
    }
    item.get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
}

/// The command as the agent wrote it: Codex wraps it in the user's shell
/// (`/bin/bash -lc '…'`) and reports the unwrapped text as its one action.
fn description(item: &Value) -> String {
    let actions = item
        .get("commandActions")
        .or_else(|| item.get("command_actions"))
        .and_then(Value::as_array);
    let unwrapped = match actions.map(Vec::as_slice) {
        Some([action]) => action.get("command").and_then(Value::as_str),
        _ => None,
    };
    let text = unwrapped
        .or_else(|| item.get("command").and_then(Value::as_str))
        .unwrap_or_default()
        .trim();
    match text.char_indices().nth(MAX_DESCRIPTION_CHARS) {
        Some((cut, _)) => format!("{}…", &text[..cut]),
        None => text.to_owned(),
    }
}
