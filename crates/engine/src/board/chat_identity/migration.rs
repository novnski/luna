//! Legacy nested child/fork records (a side chat whose recorded parent is
//! itself a side chat). Migration is purely additive: once a record is
//! verifiably quiescent, a local record adopts its top-level main chat as
//! the display container. Ids, history, provider bindings, child metadata
//! (permissions, delivery receipts, result routing) and saved workbenches
//! are never rewritten; the legacy parent stays as creator provenance.
//! Active records are deferred and picked up by a later sweep. Further
//! nesting is blocked by the creation guard regardless of migration.
use super::*;
use luna_proto::BoardLegacyMigrationReport;

/// Additive migration record for one legacy nested chat.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LegacyMigrationRecord {
    pub task_id: String,
    /// The immediate recorded parent (kept; never rewritten).
    pub legacy_parent_id: String,
    pub main_parent_id: String,
    /// SHA-256 of the lineage evidence observed at migration (child row
    /// metadata, task lineage and bindings), for audit readback.
    pub evidence_sha256: String,
    pub migrated_at_ms: i64,
}

impl EngineRpc {
    pub(crate) fn board_legacy_migrated(&self, task_id: &str) -> BoardResult<bool> {
        Ok(self
            .board_kv_load::<LegacyMigrationRecord>(&kv_key("migration", task_id))?
            .is_some())
    }

    /// The recorded parent from in-memory lineage only (the sweep's cheap
    /// pre-filter; reservations still preparing are not legacy records).
    fn board_recorded_parent(&self, chat: &Chat) -> Option<String> {
        self.workspace
            .side_chat_parent(&chat.id)
            .or_else(|| {
                chat.child_thread
                    .as_ref()
                    .map(|child| child.parent_task_id.clone())
            })
            .or_else(|| {
                let lineage = self.task_identity.view(&chat.id)?.task.lineage;
                lineage.parent_task_id.or(lineage.source_task_id)
            })
            .filter(|parent| parent != &chat.id)
    }

    /// No run, turn, queued message, background work, undelivered startup
    /// or undelivered result: ownership can be adopted without changing
    /// anything that is in flight.
    fn board_quiescent(&self, chat: &Chat) -> bool {
        if self.sessions.has_live_run(&chat.id)
            || self.sessions.active_turn_id(&chat.id).is_some()
            || self.sessions.turn_in_flight(&chat.id)
            || self.sessions.has_live_background_work(&chat.id)
        {
            return false;
        }
        if self
            .sessions
            .session_status(&chat.id)
            .is_some_and(|session| {
                matches!(
                    session.status,
                    luna_proto::SessionStatus::Working
                        | luna_proto::SessionStatus::AwaitingApproval
                        | luna_proto::SessionStatus::AwaitingInput
                )
            })
        {
            return false;
        }
        if let Some(child) = &chat.child_thread {
            if !child.initial_prompt.is_empty()
                && !self
                    .store
                    .task_command_receipt(&format!("child-start:{}", chat.id))
                    .ok()
                    .flatten()
                    .is_some_and(|receipt| receipt.status == CommandReceiptStatus::Applied)
            {
                return false;
            }
            if child.outcomes.iter().any(|outcome| !outcome.delivered) {
                return false;
            }
        }
        !self
            .doc_host
            .open(&chat.id)
            .ok()
            .and_then(|handle| handle.doc().read_queue().ok())
            .is_some_and(|queue| !queue.is_empty())
    }

    /// Non-destructive migration of legacy nested children/forks under their
    /// top-level main chat; active descendants are deferred until verified
    /// quiescence. Never recreates chats, rewrites provider ids or replays.
    pub(crate) async fn board_migrate_legacy_nesting(
        &self,
    ) -> BoardResult<BoardLegacyMigrationReport> {
        let chats = self.workspace.read_chats().map_err(storage)?;
        let by_id: HashMap<&str, &Chat> =
            chats.iter().map(|chat| (chat.id.as_str(), chat)).collect();
        let mut report = BoardLegacyMigrationReport {
            examined: 0,
            migrated: 0,
            deferred: Vec::new(),
            unchanged: 0,
        };
        for chat in chats
            .iter()
            .filter(|chat| chat.device_id == self.engine_info.device_id)
        {
            let Some(parent) = self.board_recorded_parent(chat) else {
                continue;
            };
            report.examined += 1;
            // Only a parent that itself has a recorded parent is nesting.
            let nested = by_id
                .get(parent.as_str())
                .is_some_and(|parent| self.board_recorded_parent(parent).is_some());
            if !nested || self.board_legacy_migrated(&chat.id)? {
                report.unchanged += 1;
                continue;
            }
            let Ok(resolved) = self.board_resolve_chat(&chat.id) else {
                report.unchanged += 1;
                continue;
            };
            let (Some(main), Some(legacy_parent)) = (
                resolved.identity.main_parent_id.clone(),
                resolved.immediate_parent_id.clone(),
            ) else {
                report.unchanged += 1;
                continue;
            };
            if !resolved.identity.legacy_nested {
                report.unchanged += 1;
                continue;
            }
            if !self.board_quiescent(chat) {
                report.deferred.push(chat.id.clone());
                continue;
            }
            let view = self.task_identity.view(&chat.id);
            let evidence_sha256 = board_digest(&(
                &chat.child_thread,
                view.as_ref().map(|view| &view.task.lineage),
                view.as_ref().map(|view| {
                    view.bindings
                        .iter()
                        .map(|binding| (&binding.binding_id, binding.generation))
                        .collect::<Vec<_>>()
                }),
            ))?;
            self.board_kv_save(
                &kv_key("migration", &chat.id),
                &LegacyMigrationRecord {
                    task_id: chat.id.clone(),
                    legacy_parent_id: legacy_parent,
                    main_parent_id: main,
                    evidence_sha256,
                    migrated_at_ms: crate::now_ms(),
                },
            )?;
            report.migrated += 1;
        }
        if report.migrated > 0 {
            tracing::info!(
                migrated = report.migrated,
                deferred = report.deferred.len(),
                "legacy nested chats migrated under their main chats"
            );
        } else if !report.deferred.is_empty() {
            tracing::debug!(
                deferred = report.deferred.len(),
                "legacy nested chat migration waits for quiescence"
            );
        }
        Ok(report)
    }
}
