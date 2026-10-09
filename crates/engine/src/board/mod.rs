//! Project board service (docs/design/project-board/pb0-contract.md).
//!
//! Mounted inside `rpc` (like `child_threads.rs`) so each slice can use the
//! engine services `EngineRpc` owns: task commands, child/side chat creation,
//! shared Stop, archive-with-close, model catalogs and source control. The
//! `rpc/board.rs` dispatcher is the only RPC entry; agent tools call the same
//! functions with an agent-bound [`BoardCaller`].
//!
//! Ownership: `store.rs` (PB-0 persistence and core service) is complete. The
//! other modules declare their slice's full public surface and return
//! [`BoardError::NotImplemented`] until the owning slice lands.
use super::*;

pub(crate) mod authoring;
pub(crate) mod chat_identity;
pub(crate) mod context;
pub(crate) mod github;
pub(crate) mod lifecycle;
pub(crate) mod review;
mod store;
pub(crate) mod tools;

pub(crate) use luna_proto::{BoardActor, BoardError, BoardMutationReceipt, BoardResult, Space};

/// The reason a CAS lost to a concurrent writer (safe to retry with the
/// same request id).
pub(crate) const BOARD_CONCURRENT_CHANGE: &str =
    "The ticket changed concurrently; retry with the same request id.";

/// Scope a provider run needs to read the board of its own project.
pub(crate) const BOARD_SCOPE_READ: &str = "board.read";
/// Scope a provider run needs to mutate tickets in its own project.
pub(crate) const BOARD_SCOPE_WRITE: &str = "board.write";

/// Engine-wide board state shared by every `EngineRpc` instance of one
/// engine: per-ticket async gates (serialize lifecycle work on one ticket
/// without blocking others) and the non-journaled observation channel that
/// wakes `BoardWatch` when a phase/attention projection changes.
#[derive(Clone)]
pub struct BoardHub(std::sync::Arc<BoardHubInner>);

#[allow(dead_code)] // gates are taken by the lifecycle/review slices
pub(crate) struct BoardHubInner {
    gates: std::sync::Mutex<
        std::collections::HashMap<String, std::sync::Weak<tokio::sync::Mutex<()>>>,
    >,
    observations: tokio::sync::broadcast::Sender<Vec<String>>,
}

impl Default for BoardHub {
    fn default() -> Self {
        Self(std::sync::Arc::new(BoardHubInner {
            gates: Default::default(),
            observations: tokio::sync::broadcast::channel(256).0,
        }))
    }
}

#[allow(dead_code)] // used by the lifecycle/review slices
impl BoardHub {
    /// Serialize async work on one ticket (other tickets proceed).
    pub(crate) async fn gate(&self, work_item_id: &str) -> tokio::sync::OwnedMutexGuard<()> {
        let gate = {
            let mut gates = self
                .0
                .gates
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            gates.retain(|_, gate| gate.strong_count() > 0);
            match gates.get(work_item_id).and_then(std::sync::Weak::upgrade) {
                Some(gate) => gate,
                None => {
                    let gate = std::sync::Arc::new(tokio::sync::Mutex::new(()));
                    gates.insert(work_item_id.into(), std::sync::Arc::downgrade(&gate));
                    gate
                }
            }
        };
        gate.lock_owned().await
    }

    /// Announce that these tickets' observed phase/attention changed.
    pub(crate) fn notify_observed(&self, work_item_ids: Vec<String>) {
        if !work_item_ids.is_empty() {
            let _ = self.0.observations.send(work_item_ids);
        }
    }

    pub(crate) fn observed(&self) -> tokio::sync::broadcast::Receiver<Vec<String>> {
        self.0.observations.subscribe()
    }
}

/// Who is calling, bound by the engine from the transport or the active run.
/// Never deserialized from request or tool arguments.
#[derive(Debug, Clone)]
#[allow(dead_code)] // agent/authoring/engine callers are bound by slices A2-A6
pub(crate) enum BoardCaller {
    /// The local owner/UI acting for the human.
    User { device_id: String },
    /// A provider run in an ordinary main/side chat (from its
    /// `CrossTaskAuthority`).
    Agent {
        task_id: String,
        project_ids: HashSet<String>,
        granted_scopes: HashSet<String>,
        explicit_user_request: bool,
    },
    /// A temporary authoring session's runtime (never a chat).
    Authoring {
        session_id: String,
        project_id: String,
    },
    /// Engine-internal reconciliation, lifecycle or review automation.
    Engine { component: &'static str },
}

/// What a caller wants to do with a project's board.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BoardAccess {
    Read,
    Write,
}

#[allow(dead_code)] // agent binding is used by the tools slice
impl BoardCaller {
    pub(crate) fn user(device_id: &str) -> Self {
        Self::User {
            device_id: device_id.into(),
        }
    }

    /// Bind a provider run. Authority comes only from the engine-built run
    /// context, never from model arguments.
    pub(crate) fn from_authority(authority: &CrossTaskAuthority) -> Self {
        Self::Agent {
            task_id: authority.caller_task_id.clone(),
            project_ids: authority.visible_project_ids.clone(),
            granted_scopes: authority.granted_scopes.clone(),
            explicit_user_request: authority.explicit_user_request,
        }
    }

    /// The recorded actor. Reviewer attribution is resolved by the review
    /// slice from the recorded review run, never from this binding.
    pub(crate) fn actor(&self) -> BoardActor {
        match self {
            Self::User { device_id } => BoardActor::User {
                device_id: device_id.clone(),
            },
            Self::Agent { task_id, .. } => BoardActor::Agent {
                task_id: task_id.clone(),
                role: None,
            },
            Self::Authoring { session_id, .. } => BoardActor::Authoring {
                session_id: session_id.clone(),
            },
            Self::Engine { component } => BoardActor::Engine {
                component: (*component).into(),
            },
        }
    }

    /// The chat behind the call, when the caller is a chat run.
    pub(crate) fn task_id(&self) -> Option<&str> {
        match self {
            Self::Agent { task_id, .. } => Some(task_id),
            _ => None,
        }
    }

    /// Idempotency keys are namespaced per caller: two chats reusing the
    /// same key never collide, and a model cannot replay another caller's.
    fn request_scope(&self) -> String {
        match self {
            Self::User { .. } => "user".into(),
            Self::Agent { task_id, .. } => format!("agent:{task_id}"),
            Self::Authoring { session_id, .. } => format!("authoring:{session_id}"),
            Self::Engine { component } => format!("engine:{component}"),
        }
    }

    /// The stored request id for an idempotency key.
    pub(crate) fn normalized_request_id(&self, key: &str) -> BoardResult<String> {
        if !luna_proto::board_request_id_valid(key) {
            return Err(BoardError::invalid(
                "requestId",
                "use 1-128 characters from A-Z a-z 0-9 _ - : .",
            ));
        }
        Ok(format!(
            "br-{}",
            &board_digest(&(self.request_scope(), key))?[..40]
        ))
    }
}

/// Stable SHA-256 hex of a value's canonical JSON.
pub(crate) fn board_digest(value: &impl Serialize) -> BoardResult<String> {
    crate::context_transfers::digest(value).map_err(|error| BoardError::Storage {
        message: error.to_string(),
    })
}

/// Map storage failures onto structured board errors.
pub(crate) fn board_store_error(error: luna_sync::StoreError) -> BoardError {
    use luna_sync::StoreError as E;
    match error {
        E::BoardRequestConflict => BoardError::IdempotencyConflict {
            request_id: String::new(),
        },
        E::BoardNotFound(id) => BoardError::NotFound { work_item_id: id },
        E::BoardRevisionConflict { .. } => BoardError::Busy {
            reason: BOARD_CONCURRENT_CHANGE.into(),
        },
        E::BoardConflict(reason) => BoardError::Busy { reason },
        E::AdmissionRequired | E::AdmissionMismatch => BoardError::Unavailable {
            reason: "Board writes are unavailable while this profile is read-only".into(),
        },
        other => BoardError::Storage {
            message: other.to_string(),
        },
    }
}

/// Encode a board result for RPC.
pub(crate) fn board_reply<T: Serialize>(result: BoardResult<T>) -> Result<RpcReply, RpcError> {
    match result {
        Ok(value) => RpcReply::value(&value),
        Err(error) => Err(RpcError::Failed(error.to_rpc_message())),
    }
}

impl EngineRpc {
    /// The engine-shared board hub.
    #[allow(dead_code)] // used by the lifecycle/review slices
    pub(crate) fn board_hub(&self) -> &BoardHub {
        &self.board
    }

    /// Device write admission for one board commit.
    pub(crate) fn board_write_permit(
        &self,
    ) -> BoardResult<Option<crate::device_write::DeviceWritePermit>> {
        self.work_writes
            .as_ref()
            .map(|writes| {
                writes.try_admit().map_err(|error| BoardError::Unavailable {
                    reason: format!("Board writes unavailable: {error:?}"),
                })
            })
            .transpose()
    }

    /// Save one board-owned keyed blob (reply caches, review packets and
    /// profiles, chat-model records) under device write admission.
    ///
    /// The permit fences retired writers; the store decides whether it needs
    /// it. An engine's store is unguarded, so `save_snapshot` writes directly
    /// and an admitted save on it would always be `AdmissionMismatch`; a
    /// guarded store answers `AdmissionRequired` without writing, and the
    /// admitted save follows. (`board_commit` needs no such step: the store's
    /// command writer ignores the permit when unguarded and checks it when
    /// guarded.)
    pub(crate) fn board_save_snapshot(&self, key: &str, bytes: &[u8]) -> BoardResult<()> {
        let permit = self.board_write_permit()?;
        match (self.store.save_snapshot(key, bytes), permit) {
            (Err(luna_sync::StoreError::AdmissionRequired), Some(permit)) => self
                .store
                .save_snapshot_admitted(permit.ticket(), key, bytes),
            (result, _) => result,
        }
        .map_err(board_store_error)
    }

    /// Commit one mutation through the store's atomic primitive.
    pub(crate) fn board_commit(
        &self,
        commit: luna_sync::BoardCommit,
    ) -> BoardResult<Option<BoardMutationReceipt>> {
        let request_id = commit.request_id.clone();
        let permit = self.board_write_permit()?;
        self.store
            .board_commit(commit, permit.as_ref().map(|permit| permit.ticket()))
            .map_err(|error| match board_store_error(error) {
                BoardError::IdempotencyConflict { .. } => {
                    BoardError::IdempotencyConflict { request_id }
                }
                other => other,
            })
    }

    /// Resolve and authorize a project on this host for a caller.
    pub(crate) fn board_project(
        &self,
        caller: &BoardCaller,
        project_id: &str,
        access: BoardAccess,
    ) -> BoardResult<Space> {
        let space = self
            .workspace
            .space(project_id)
            .map_err(|error| BoardError::Storage {
                message: error.to_string(),
            })?
            .ok_or_else(|| BoardError::ProjectNotFound {
                project_id: project_id.into(),
            })?;
        if space.device_id != self.engine_info.device_id {
            return Err(BoardError::WrongHost {
                project_id: project_id.into(),
                owner_host_id: space.device_id,
            });
        }
        self.board_check_caller(caller, project_id, access)?;
        Ok(space)
    }

    /// Caller-bound authority for one project. Reading a ticket never grants
    /// authority over other projects; temporary authoring is confined to its
    /// own project.
    pub(crate) fn board_check_caller(
        &self,
        caller: &BoardCaller,
        project_id: &str,
        access: BoardAccess,
    ) -> BoardResult<()> {
        match caller {
            BoardCaller::User { .. } | BoardCaller::Engine { .. } => Ok(()),
            BoardCaller::Agent {
                project_ids,
                granted_scopes,
                ..
            } => {
                let required = match access {
                    BoardAccess::Read => BOARD_SCOPE_READ,
                    BoardAccess::Write => BOARD_SCOPE_WRITE,
                };
                if !granted_scopes.contains(required) {
                    return Err(BoardError::InsufficientScope {
                        required: required.into(),
                    });
                }
                if !project_ids.contains(project_id) {
                    return Err(BoardError::NotAuthorized {
                        reason: "That ticket is outside this chat's project.".into(),
                    });
                }
                Ok(())
            }
            BoardCaller::Authoring {
                project_id: session_project,
                ..
            } => {
                if session_project != project_id {
                    return Err(BoardError::NotAuthorized {
                        reason: "This discussion can only use its own project's board.".into(),
                    });
                }
                Ok(())
            }
        }
    }
}
