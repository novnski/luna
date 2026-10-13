//! PB-4A feature groups (docs/pending-tasks/devin-project-board/pb-4a/README.md).
//!
//! A feature group is a local Board record owned by one home project on its
//! owning host. Its members are existing tickets, including tickets of other
//! projects on this host that the user linked explicitly (related
//! repositories). The group has its own acceptance: criteria, optional
//! integration gates and one explicit Accept, each bound to the members'
//! state when recorded. Members completing never accept the group.
//!
//! The group only reads its members. Nothing here edits a ticket or its rows,
//! creates a chat, launch, review, task link, worktree or dependency, or adds
//! a caller scope; every member's project, checkout, branch, worker and
//! reviewer are read from that ticket's own owners.
//!
//! Storage: one host-owned record in the profile's keyed Board store
//! (`board_kv_*`, device write admission), changed only under the
//! engine-wide `feature-groups` gate, so a mutation, its history entry and
//! its idempotency receipt are written together. No schema migration.
use super::*;
use luna_proto::{
    BoardColumn, BoardEvidenceRef, BoardFeatureGroupMutateRequest, BoardFeatureGroupPage,
    BoardFeatureGroupsRequest, BoardScope, FEATURE_GROUP_ACCEPTANCE_MAX_BYTES,
    FEATURE_GROUP_EVIDENCE_MAX, FEATURE_GROUP_GATE_LABEL_MAX_CHARS, FEATURE_GROUP_GATES_MAX,
    FEATURE_GROUP_HISTORY_MAX, FEATURE_GROUP_MEMBERS_MAX, FEATURE_GROUP_NOTE_MAX_CHARS,
    FEATURE_GROUP_SCHEMA_VERSION, FEATURE_GROUP_TITLE_MAX_CHARS, FEATURE_GROUPS_MAX, FeatureGroup,
    FeatureGroupAcceptance, FeatureGroupAcceptanceState, FeatureGroupAction,
    FeatureGroupBasisEntry, FeatureGroupBlocker, FeatureGroupCheckout, FeatureGroupCheckoutSource,
    FeatureGroupGate, FeatureGroupGateResult, FeatureGroupGateState, FeatureGroupGateStatus,
    FeatureGroupGateView, FeatureGroupHistoryEntry, FeatureGroupHistoryKind, FeatureGroupMember,
    FeatureGroupMemberAvailability, FeatureGroupMemberView, FeatureGroupMutation,
    FeatureGroupReceipt, FeatureGroupReviewSummary, FeatureGroupSummary, FeatureGroupUnmet,
    FeatureGroupView,
};
use serde::{Deserialize, Serialize};

/// The host's one record (`board-feature-groups:<deviceId>`).
const STORE_KEY: &str = "board-feature-groups:";
/// The engine-wide gate every group mutation takes.
const GROUPS_GATE: &str = "feature-groups";
/// Idempotency receipts kept (oldest dropped first).
const RECEIPTS_MAX: usize = 200;
const EVIDENCE_TEXT_MAX: usize = 2_000;

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GroupStore {
    #[serde(default)]
    groups: Vec<FeatureGroup>,
    #[serde(default)]
    receipts: Vec<FeatureGroupReceipt>,
}

fn storage(error: impl std::fmt::Display) -> BoardError {
    BoardError::Storage {
        message: error.to_string(),
    }
}

/// A trimmed single-line label.
fn one_line(field: &str, value: &str, max_chars: usize) -> BoardResult<String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(BoardError::invalid(field, "must not be empty"));
    }
    if value.chars().any(char::is_control) {
        return Err(BoardError::invalid(
            field,
            "use one line without control characters",
        ));
    }
    if value.chars().count() > max_chars {
        return Err(BoardError::invalid(
            field,
            format!("use at most {max_chars} characters"),
        ));
    }
    Ok(value.to_owned())
}

/// The shared criteria: Markdown, bounded, no control characters besides
/// line breaks and tabs.
fn clean_criteria(value: &str) -> BoardResult<String> {
    let value = value.trim();
    if value.len() > FEATURE_GROUP_ACCEPTANCE_MAX_BYTES {
        return Err(BoardError::invalid(
            "acceptance",
            format!("use at most {FEATURE_GROUP_ACCEPTANCE_MAX_BYTES} bytes"),
        ));
    }
    if value
        .chars()
        .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
    {
        return Err(BoardError::invalid(
            "acceptance",
            "remove control characters",
        ));
    }
    Ok(value.to_owned())
}

fn clean_note(note: Option<&str>) -> BoardResult<Option<String>> {
    let Some(note) = note.map(str::trim).filter(|note| !note.is_empty()) else {
        return Ok(None);
    };
    if note.chars().count() > FEATURE_GROUP_NOTE_MAX_CHARS {
        return Err(BoardError::invalid(
            "note",
            format!("use at most {FEATURE_GROUP_NOTE_MAX_CHARS} characters"),
        ));
    }
    if note
        .chars()
        .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
    {
        return Err(BoardError::invalid("note", "remove control characters"));
    }
    Ok(Some(note.to_owned()))
}

fn clean_evidence(evidence: &[BoardEvidenceRef]) -> BoardResult<Vec<BoardEvidenceRef>> {
    if evidence.len() > FEATURE_GROUP_EVIDENCE_MAX {
        return Err(BoardError::invalid(
            "evidence",
            format!("reference at most {FEATURE_GROUP_EVIDENCE_MAX} items"),
        ));
    }
    let bounded = |field: &str, value: &Option<String>| -> BoardResult<Option<String>> {
        match value.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
            Some(value)
                if value.len() > EVIDENCE_TEXT_MAX || value.chars().any(char::is_control) =>
            {
                Err(BoardError::invalid(
                    field,
                    format!("use one line of at most {EVIDENCE_TEXT_MAX} bytes"),
                ))
            }
            other => Ok(other.map(str::to_owned)),
        }
    };
    evidence
        .iter()
        .map(|item| -> BoardResult<BoardEvidenceRef> {
            Ok(BoardEvidenceRef {
                label: one_line(
                    "evidence.label",
                    &item.label,
                    FEATURE_GROUP_GATE_LABEL_MAX_CHARS,
                )?,
                path: bounded("evidence.path", &item.path)?,
                url: bounded("evidence.url", &item.url)?,
                outcome_id: bounded("evidence.outcomeId", &item.outcome_id)?,
            })
        })
        .collect()
}

/// The members' state a gate result or acceptance is bound to, sorted by
/// ticket id. Deleted, missing or foreign tickets carry no column.
fn basis_of(members: &[FeatureGroupMemberView]) -> Vec<FeatureGroupBasisEntry> {
    let mut basis: Vec<FeatureGroupBasisEntry> = members
        .iter()
        .map(|member| FeatureGroupBasisEntry {
            work_item_id: member.work_item_id.clone(),
            column: member
                .availability
                .present()
                .then_some(member.column)
                .flatten(),
            execution_generation: member.execution_generation,
        })
        .collect();
    basis.sort_by(|left, right| left.work_item_id.cmp(&right.work_item_id));
    basis
}

fn push_history(
    group: &mut FeatureGroup,
    revision: u64,
    kind: FeatureGroupHistoryKind,
    actor: &BoardActor,
    at_ms: i64,
) {
    group.history.push(FeatureGroupHistoryEntry {
        revision,
        kind,
        actor: actor.clone(),
        at_ms,
    });
    if group.history.len() > FEATURE_GROUP_HISTORY_MAX {
        let excess = group.history.len() - FEATURE_GROUP_HISTORY_MAX;
        group.history.drain(..excess);
    }
}

/// A change to the group's own contract withdraws a recorded acceptance.
fn clear_acceptance(
    group: &mut FeatureGroup,
    revision: u64,
    reason: &str,
    actor: &BoardActor,
    at_ms: i64,
) {
    if group.acceptance_record.take().is_some() {
        push_history(
            group,
            revision,
            FeatureGroupHistoryKind::AcceptanceCleared {
                reason: reason.into(),
            },
            actor,
            at_ms,
        );
    }
}

fn add_gate(
    group: &mut FeatureGroup,
    revision: u64,
    label: String,
    actor: &BoardActor,
    at_ms: i64,
) -> BoardResult<()> {
    if group.gates.len() >= FEATURE_GROUP_GATES_MAX {
        return Err(BoardError::invalid(
            "gates",
            format!("a feature group has at most {FEATURE_GROUP_GATES_MAX} gates"),
        ));
    }
    group.next_gate += 1;
    let gate_id = format!("g{}", group.next_gate);
    group.gates.push(FeatureGroupGate {
        id: gate_id.clone(),
        label: label.clone(),
        result: None,
    });
    push_history(
        group,
        revision,
        FeatureGroupHistoryKind::GateAdded { gate_id, label },
        actor,
        at_ms,
    );
    Ok(())
}

fn not_found() -> BoardError {
    BoardError::invalid("groupId", "This feature group no longer exists.")
}

impl EngineRpc {
    fn feature_group_key(&self) -> String {
        format!("{STORE_KEY}{}", self.engine_info.device_id)
    }

    fn feature_group_store(&self) -> BoardResult<GroupStore> {
        Ok(self
            .board_kv_load::<GroupStore>(&self.feature_group_key())?
            .unwrap_or_default())
    }

    /// A ticket the caller may link: live, on this host, in a project of
    /// this host that the caller may read. Linking writes nothing to it.
    fn feature_group_member(
        &self,
        caller: &BoardCaller,
        id_or_key: &str,
        actor: &BoardActor,
        at_ms: i64,
    ) -> BoardResult<FeatureGroupMember> {
        let item = self.board_item_for(caller, id_or_key, BoardAccess::Read)?;
        let space = self.board_project(caller, &item.project_id, BoardAccess::Read)?;
        if space.is_removed() {
            return Err(BoardError::ProjectNotFound {
                project_id: item.project_id,
            });
        }
        Ok(FeatureGroupMember {
            work_item_id: item.id,
            project_id: item.project_id,
            key: item.key,
            added_by: actor.clone(),
            added_at_ms: at_ms,
        })
    }

    /// One member as its own owners report it (the ticket's read model, its
    /// latest launch, its working chat, its latest review run).
    fn feature_group_member_view(
        &self,
        group: &FeatureGroup,
        member: &FeatureGroupMember,
    ) -> BoardResult<FeatureGroupMemberView> {
        let space = self.workspace.space(&member.project_id).ok().flatten();
        let mut view = FeatureGroupMemberView {
            work_item_id: member.work_item_id.clone(),
            key: member.key.clone(),
            project_id: member.project_id.clone(),
            project_name: space.as_ref().map(|space| space.display_name().to_owned()),
            project_path: space.as_ref().map(|space| space.path.clone()),
            availability: FeatureGroupMemberAvailability::Missing,
            title: None,
            column: None,
            phase: None,
            attention: Vec::new(),
            source: None,
            issue: None,
            working_chat: None,
            checkout: None,
            review: None,
            blocked_by: Vec::new(),
            execution_generation: 0,
            revision: 0,
        };
        let Some(item) = self
            .store
            .board_item(&member.work_item_id)
            .map_err(storage)?
        else {
            return Ok(view);
        };
        view.key = item.key.clone();
        view.title = Some(item.title.clone());
        view.revision = item.revision;
        if item.deleted {
            view.availability = FeatureGroupMemberAvailability::Deleted;
            return Ok(view);
        }
        if item.owner_host_id != self.engine_info.device_id {
            view.availability = FeatureGroupMemberAvailability::OtherHost;
            return Ok(view);
        }
        view.availability = if item.archived {
            FeatureGroupMemberAvailability::Archived
        } else {
            FeatureGroupMemberAvailability::Available
        };
        let generation = self
            .store
            .board_intent(&item.id)
            .map_err(storage)?
            .map_or(0, |intent| intent.execution_generation);
        view.execution_generation = generation;
        let launch = self
            .store
            .board_launches(&item.id, 1)
            .map_err(storage)?
            .into_iter()
            .next();
        let review = self
            .store
            .board_review_runs(&item.id, 1)
            .map_err(storage)?
            .into_iter()
            .next();
        let requested = item.implementation.checkout.clone();
        let ticket = self.board_view(item)?;
        view.column = Some(ticket.item.column);
        view.phase = Some(ticket.phase);
        view.attention = ticket.attention.iter().map(|reason| reason.kind).collect();
        view.source = Some(ticket.item.origin.source_kind());
        view.issue = ticket.issue.clone();
        for blocker in &ticket.blocked_by {
            let key = self
                .store
                .board_item(blocker)
                .map_err(storage)?
                .map_or_else(|| blocker.clone(), |item| item.key);
            view.blocked_by.push(FeatureGroupBlocker {
                work_item_id: blocker.clone(),
                key,
                in_group: group
                    .members
                    .iter()
                    .any(|other| &other.work_item_id == blocker),
            });
        }
        let chat = ticket
            .working_chat
            .as_ref()
            .and_then(|working| self.workspace.chat(&working.task_id).ok().flatten())
            .filter(|chat| chat.cwd.is_some());
        view.checkout = if let Some(chat) = chat {
            Some(FeatureGroupCheckout {
                source: FeatureGroupCheckoutSource::WorkingChat,
                cwd: chat.cwd.clone().unwrap_or_default(),
                checkout_id: chat.checkout_id.clone(),
                recorded_branch: chat.branch.clone(),
                live_branch: None,
                requested: requested.clone(),
            })
        } else if let Some(launch) = &launch {
            Some(FeatureGroupCheckout {
                source: FeatureGroupCheckoutSource::Launch,
                cwd: launch.resolved.cwd.clone(),
                checkout_id: launch.resolved.checkout_id.clone(),
                recorded_branch: launch.resolved.branch.clone(),
                live_branch: None,
                requested: requested.clone(),
            })
        } else {
            space.as_ref().map(|space| FeatureGroupCheckout {
                source: FeatureGroupCheckoutSource::Project,
                cwd: space.path.clone(),
                checkout_id: space.checkout_id.clone(),
                recorded_branch: None,
                live_branch: None,
                requested: requested.clone(),
            })
        };
        view.review = review.map(|run| FeatureGroupReviewSummary {
            review_run_id: run.id.clone(),
            state: run.state,
            verdict: run.result.as_ref().map(|result| result.verdict),
            execution_generation: run.execution_generation,
            current: run.execution_generation == generation,
        });
        view.working_chat = ticket.working_chat;
        Ok(view)
    }

    /// The engine-derived aggregate: members, gate status, unmet reasons and
    /// whether the group can be accepted now. Reads never write the record.
    fn feature_group_assess(
        &self,
        group: &FeatureGroup,
        home: bool,
    ) -> BoardResult<FeatureGroupView> {
        let space = self.workspace.space(&group.project_id).ok().flatten();
        let members = group
            .members
            .iter()
            .map(|member| self.feature_group_member_view(group, member))
            .collect::<BoardResult<Vec<_>>>()?;
        let basis = basis_of(&members);
        let mut unmet = Vec::new();
        if members.is_empty() {
            unmet.push(FeatureGroupUnmet::NoMembers);
        }
        for member in &members {
            if !member.availability.present() {
                unmet.push(FeatureGroupUnmet::MemberUnavailable {
                    work_item_id: member.work_item_id.clone(),
                    key: member.key.clone(),
                    availability: member.availability,
                });
                continue;
            }
            let column = member.column.unwrap_or_default();
            if column != BoardColumn::Done {
                unmet.push(FeatureGroupUnmet::MemberNotDone {
                    work_item_id: member.work_item_id.clone(),
                    key: member.key.clone(),
                    column,
                    phase: member.phase.unwrap_or_default(),
                });
            }
            if !member.blocked_by.is_empty() {
                unmet.push(FeatureGroupUnmet::MemberBlocked {
                    work_item_id: member.work_item_id.clone(),
                    key: member.key.clone(),
                    blocked_by: member
                        .blocked_by
                        .iter()
                        .map(|blocker| blocker.key.clone())
                        .collect(),
                });
            }
        }
        let gates: Vec<FeatureGroupGateView> = group
            .gates
            .iter()
            .map(|gate| FeatureGroupGateView {
                gate: gate.clone(),
                status: match &gate.result {
                    None => FeatureGroupGateStatus::Pending,
                    Some(result) if result.basis != basis => FeatureGroupGateStatus::Stale,
                    Some(result) => match result.state {
                        FeatureGroupGateState::Passed => FeatureGroupGateStatus::Passed,
                        FeatureGroupGateState::Failed => FeatureGroupGateStatus::Failed,
                    },
                },
            })
            .collect();
        for view in &gates {
            let (gate_id, label) = (view.gate.id.clone(), view.gate.label.clone());
            match view.status {
                FeatureGroupGateStatus::Pending => {
                    unmet.push(FeatureGroupUnmet::GatePending { gate_id, label })
                }
                FeatureGroupGateStatus::Failed => {
                    unmet.push(FeatureGroupUnmet::GateFailed { gate_id, label })
                }
                FeatureGroupGateStatus::Stale => {
                    unmet.push(FeatureGroupUnmet::GateStale { gate_id, label })
                }
                FeatureGroupGateStatus::Passed => {}
            }
        }
        let acceptance = match &group.acceptance_record {
            None => FeatureGroupAcceptanceState::NotAccepted,
            Some(record) if record.basis == basis && unmet.is_empty() => {
                FeatureGroupAcceptanceState::Accepted
            }
            Some(_) => FeatureGroupAcceptanceState::Stale,
        };
        let mut repositories: Vec<&str> = members
            .iter()
            .map(|member| member.project_id.as_str())
            .collect();
        repositories.sort_unstable();
        repositories.dedup();
        let summary = FeatureGroupSummary {
            members: members.len() as u32,
            done: members
                .iter()
                .filter(|member| {
                    member.availability.present() && member.column == Some(BoardColumn::Done)
                })
                .count() as u32,
            repositories: repositories.len() as u32,
            gates: gates.len() as u32,
            gates_passed: gates
                .iter()
                .filter(|gate| gate.status == FeatureGroupGateStatus::Passed)
                .count() as u32,
        };
        Ok(FeatureGroupView {
            group: group.clone(),
            project_name: space.as_ref().map(|space| space.display_name().to_owned()),
            project_path: space.as_ref().map(|space| space.path.clone()),
            home,
            can_accept: unmet.is_empty() && acceptance != FeatureGroupAcceptanceState::Accepted,
            members,
            gates,
            acceptance,
            unmet,
            summary,
        })
    }

    /// The branch checked out in each member's checkout right now (one
    /// read-only `git branch --show-current` per distinct folder).
    async fn feature_group_live_branches(&self, views: &mut [FeatureGroupView]) {
        let mut branches: std::collections::HashMap<String, Option<String>> =
            std::collections::HashMap::new();
        for view in views.iter_mut() {
            for member in &mut view.members {
                let Some(checkout) = member.checkout.as_mut() else {
                    continue;
                };
                if !branches.contains_key(&checkout.cwd) {
                    let path = std::path::PathBuf::from(&checkout.cwd);
                    let branch = if path.is_dir() {
                        self.repos.current_branch(&path).await.ok()
                    } else {
                        None
                    };
                    branches.insert(checkout.cwd.clone(), branch);
                }
                checkout.live_branch = branches.get(&checkout.cwd).cloned().flatten();
            }
        }
    }

    /// `BoardFeatureGroups`: a project scope returns its home groups and
    /// the groups that contain one of its tickets; All projects every group
    /// on this host. Groups of a removed or unknown home project are hidden.
    pub(crate) async fn board_feature_groups(
        &self,
        caller: &BoardCaller,
        request: BoardFeatureGroupsRequest,
    ) -> BoardResult<BoardFeatureGroupPage> {
        let scope_project = match &request.scope {
            BoardScope::Project { project_id } => {
                self.board_project(caller, project_id, BoardAccess::Read)?;
                Some(project_id.clone())
            }
            BoardScope::AllProjects => None,
        };
        let wanted = request
            .work_item_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty());
        let store = self.feature_group_store()?;
        let mut views = Vec::new();
        for group in &store.groups {
            if group.archived && !request.include_archived {
                continue;
            }
            if request.group_id.as_deref().is_some_and(|id| id != group.id) {
                continue;
            }
            if let Some(wanted) = wanted
                && !group
                    .members
                    .iter()
                    .any(|member| member.work_item_id == wanted || member.key == wanted)
            {
                continue;
            }
            match self.workspace.space(&group.project_id).ok().flatten() {
                Some(space) if !space.is_removed() => {}
                _ => continue,
            }
            let home = match &scope_project {
                Some(project) if group.project_id == *project => true,
                Some(project)
                    if group
                        .members
                        .iter()
                        .any(|member| member.project_id == *project) =>
                {
                    false
                }
                Some(_) => continue,
                None => true,
            };
            if self
                .board_check_caller(caller, &group.project_id, BoardAccess::Read)
                .is_err()
            {
                continue;
            }
            views.push(self.feature_group_assess(group, home)?);
        }
        views.sort_by(|left, right| {
            right
                .home
                .cmp(&left.home)
                .then(right.group.updated_at_ms.cmp(&left.group.updated_at_ms))
                .then(left.group.id.cmp(&right.group.id))
        });
        self.feature_group_live_branches(&mut views).await;
        Ok(BoardFeatureGroupPage { groups: views })
    }

    /// `BoardFeatureGroupMutate`: one idempotent change under the groups
    /// gate. Returns the receipt and the group as it is now.
    pub(crate) async fn board_feature_group_mutate(
        &self,
        caller: &BoardCaller,
        request: BoardFeatureGroupMutateRequest,
    ) -> BoardResult<FeatureGroupMutation> {
        let request_id = caller.normalized_request_id(&request.request_id)?;
        let payload_hash = board_digest(&("featureGroup", &request.action))?;
        let (receipt, touched) = {
            let _gate = self.board_hub().gate(GROUPS_GATE).await;
            let mut store = self.feature_group_store()?;
            let existing = store
                .receipts
                .iter()
                .find(|receipt| receipt.request_id == request_id)
                .cloned();
            match existing {
                Some(receipt) if receipt.payload_hash == payload_hash => (receipt, Vec::new()),
                Some(_) => {
                    return Err(BoardError::IdempotencyConflict {
                        request_id: request.request_id.clone(),
                    });
                }
                None => {
                    let (receipt, touched) = self.feature_group_apply(
                        caller,
                        &request_id,
                        &payload_hash,
                        request.action.clone(),
                        &mut store,
                    )?;
                    store.receipts.push(receipt.clone());
                    if store.receipts.len() > RECEIPTS_MAX {
                        let excess = store.receipts.len() - RECEIPTS_MAX;
                        store.receipts.drain(..excess);
                    }
                    self.board_kv_save(&self.feature_group_key(), &store)?;
                    (receipt, touched)
                }
            }
        };
        // Every open Board re-reads these tickets' cards (and with them the
        // group panels); nothing is started or stopped.
        self.board_hub().notify_observed(touched);
        let group = if receipt.deleted {
            None
        } else {
            let store = self.feature_group_store()?;
            match store
                .groups
                .iter()
                .find(|group| group.id == receipt.group_id)
            {
                Some(group) => {
                    let mut views = vec![self.feature_group_assess(group, true)?];
                    self.feature_group_live_branches(&mut views).await;
                    views.pop()
                }
                None => None,
            }
        };
        Ok(FeatureGroupMutation { receipt, group })
    }

    /// Apply one action to the record in memory. Errors leave it unchanged.
    /// Returns the receipt and the tickets whose Board views should refresh.
    fn feature_group_apply(
        &self,
        caller: &BoardCaller,
        request_id: &str,
        payload_hash: &str,
        action: FeatureGroupAction,
        store: &mut GroupStore,
    ) -> BoardResult<(FeatureGroupReceipt, Vec<String>)> {
        let actor = caller.actor();
        let now = crate::now_ms();
        let kind = action.kind();
        let receipt =
            |group_id: &str, revision: u64, changed: bool, deleted: bool| FeatureGroupReceipt {
                request_id: request_id.to_owned(),
                payload_hash: payload_hash.to_owned(),
                action: kind,
                group_id: group_id.to_owned(),
                revision,
                changed,
                deleted,
                actor: actor.clone(),
                committed_at_ms: now,
            };
        let (project_id, title, acceptance, gates, members) = match action {
            FeatureGroupAction::Create {
                project_id,
                title,
                acceptance,
                gates,
                members,
            } => (project_id, title, acceptance, gates, members),
            other => {
                return self
                    .feature_group_apply_existing(caller, other, store, &actor, now, &receipt);
            }
        };
        {
            let space = self.board_project(caller, &project_id, BoardAccess::Write)?;
            if space.is_removed() {
                return Err(BoardError::ProjectNotFound { project_id });
            }
            let title = one_line("title", &title, FEATURE_GROUP_TITLE_MAX_CHARS)?;
            let acceptance = clean_criteria(&acceptance)?;
            if gates.len() > FEATURE_GROUP_GATES_MAX {
                return Err(BoardError::invalid(
                    "gates",
                    format!("a feature group has at most {FEATURE_GROUP_GATES_MAX} gates"),
                ));
            }
            if members.len() > FEATURE_GROUP_MEMBERS_MAX {
                return Err(BoardError::invalid(
                    "members",
                    format!("a feature group has at most {FEATURE_GROUP_MEMBERS_MAX} tickets"),
                ));
            }
            if store.groups.len() >= FEATURE_GROUPS_MAX {
                return Err(BoardError::invalid(
                    "projectId",
                    format!(
                        "This host keeps at most {FEATURE_GROUPS_MAX} feature groups; delete one first."
                    ),
                ));
            }
            let id = format!(
                "fg-{}",
                &board_digest(&("feature-group", request_id))?[..24]
            );
            if store.groups.iter().any(|group| group.id == id) {
                return Err(BoardError::IdempotencyConflict {
                    request_id: request_id.to_owned(),
                });
            }
            let mut group = FeatureGroup {
                schema_version: FEATURE_GROUP_SCHEMA_VERSION,
                id,
                project_id: space.id,
                owner_host_id: self.engine_info.device_id.clone(),
                title,
                acceptance,
                gates: Vec::new(),
                members: Vec::new(),
                acceptance_record: None,
                archived: false,
                revision: 1,
                next_gate: 0,
                created_at_ms: now,
                updated_at_ms: now,
                created_by: actor.clone(),
                updated_by: actor.clone(),
                history: Vec::new(),
            };
            push_history(&mut group, 1, FeatureGroupHistoryKind::Created, &actor, now);
            for label in gates {
                let label = one_line("gates", &label, FEATURE_GROUP_GATE_LABEL_MAX_CHARS)?;
                add_gate(&mut group, 1, label, &actor, now)?;
            }
            for member in members {
                let member = self.feature_group_member(caller, &member, &actor, now)?;
                if group
                    .members
                    .iter()
                    .any(|existing| existing.work_item_id == member.work_item_id)
                {
                    continue;
                }
                push_history(
                    &mut group,
                    1,
                    FeatureGroupHistoryKind::MemberAdded {
                        work_item_id: member.work_item_id.clone(),
                        key: member.key.clone(),
                        project_id: member.project_id.clone(),
                    },
                    &actor,
                    now,
                );
                group.members.push(member);
            }
            let touched = group
                .members
                .iter()
                .map(|member| member.work_item_id.clone())
                .collect();
            let committed = receipt(&group.id, 1, true, false);
            store.groups.push(group);
            Ok((committed, touched))
        }
    }

    /// Every action but `create`: the group must exist, the caller may write
    /// its home project, and the expected revision must be current.
    fn feature_group_apply_existing(
        &self,
        caller: &BoardCaller,
        action: FeatureGroupAction,
        store: &mut GroupStore,
        actor: &BoardActor,
        now: i64,
        receipt: &dyn Fn(&str, u64, bool, bool) -> FeatureGroupReceipt,
    ) -> BoardResult<(FeatureGroupReceipt, Vec<String>)> {
        let Some((group_id, expected)) = action.target() else {
            return Err(storage("a feature group action without a target"));
        };
        let group_id = group_id.to_owned();
        let index = store
            .groups
            .iter()
            .position(|group| group.id == group_id)
            .ok_or_else(not_found)?;
        self.board_check_caller(caller, &store.groups[index].project_id, BoardAccess::Write)?;
        let current = store.groups[index].revision;
        if current != expected {
            return Err(BoardError::invalid(
                "expectedRevision",
                format!("The feature group is at revision {current}; reload it and retry."),
            ));
        }
        let mut touched: Vec<String> = store.groups[index]
            .members
            .iter()
            .map(|member| member.work_item_id.clone())
            .collect();
        if matches!(action, FeatureGroupAction::Delete { .. }) {
            store.groups.remove(index);
            return Ok((receipt(&group_id, current + 1, true, true), touched));
        }
        let mut group = store.groups[index].clone();
        let next = current + 1;
        let changed = self.feature_group_change(caller, &mut group, action, next, actor, now)?;
        if changed {
            group.revision = next;
            group.updated_at_ms = now;
            group.updated_by = actor.clone();
            for member in &group.members {
                if !touched.contains(&member.work_item_id) {
                    touched.push(member.work_item_id.clone());
                }
            }
            store.groups[index] = group;
        } else {
            touched.clear();
        }
        Ok((
            receipt(&group_id, store.groups[index].revision, changed, false),
            touched,
        ))
    }

    /// One non-create, non-delete action. `Ok(false)`: already in the
    /// requested state (no revision bump, no history).
    fn feature_group_change(
        &self,
        caller: &BoardCaller,
        group: &mut FeatureGroup,
        action: FeatureGroupAction,
        next: u64,
        actor: &BoardActor,
        now: i64,
    ) -> BoardResult<bool> {
        match action {
            FeatureGroupAction::Update {
                title, acceptance, ..
            } => {
                let mut fields = Vec::new();
                if let Some(title) = title {
                    let title = one_line("title", &title, FEATURE_GROUP_TITLE_MAX_CHARS)?;
                    if title != group.title {
                        group.title = title;
                        fields.push("title".to_owned());
                    }
                }
                let mut criteria_changed = false;
                if let Some(acceptance) = acceptance {
                    let acceptance = clean_criteria(&acceptance)?;
                    if acceptance != group.acceptance {
                        group.acceptance = acceptance;
                        fields.push("acceptance".to_owned());
                        criteria_changed = true;
                    }
                }
                if fields.is_empty() {
                    return Ok(false);
                }
                push_history(
                    group,
                    next,
                    FeatureGroupHistoryKind::Updated { fields },
                    actor,
                    now,
                );
                if criteria_changed {
                    clear_acceptance(group, next, "acceptance criteria changed", actor, now);
                }
                Ok(true)
            }
            FeatureGroupAction::AddMember { work_item_id, .. } => {
                let member = self.feature_group_member(caller, &work_item_id, actor, now)?;
                if group
                    .members
                    .iter()
                    .any(|existing| existing.work_item_id == member.work_item_id)
                {
                    return Ok(false);
                }
                if group.members.len() >= FEATURE_GROUP_MEMBERS_MAX {
                    return Err(BoardError::invalid(
                        "workItemId",
                        format!("a feature group has at most {FEATURE_GROUP_MEMBERS_MAX} tickets"),
                    ));
                }
                push_history(
                    group,
                    next,
                    FeatureGroupHistoryKind::MemberAdded {
                        work_item_id: member.work_item_id.clone(),
                        key: member.key.clone(),
                        project_id: member.project_id.clone(),
                    },
                    actor,
                    now,
                );
                group.members.push(member);
                clear_acceptance(group, next, "membership changed", actor, now);
                Ok(true)
            }
            FeatureGroupAction::RemoveMember { work_item_id, .. } => {
                let wanted = work_item_id.trim();
                let position = group
                    .members
                    .iter()
                    .position(|member| member.work_item_id == wanted || member.key == wanted)
                    .ok_or_else(|| {
                        BoardError::invalid(
                            "workItemId",
                            "That ticket is not in this feature group.",
                        )
                    })?;
                let removed = group.members.remove(position);
                push_history(
                    group,
                    next,
                    FeatureGroupHistoryKind::MemberRemoved {
                        work_item_id: removed.work_item_id,
                        key: removed.key,
                    },
                    actor,
                    now,
                );
                clear_acceptance(group, next, "membership changed", actor, now);
                Ok(true)
            }
            FeatureGroupAction::AddGate { label, .. } => {
                let label = one_line("label", &label, FEATURE_GROUP_GATE_LABEL_MAX_CHARS)?;
                add_gate(group, next, label, actor, now)?;
                clear_acceptance(group, next, "gates changed", actor, now);
                Ok(true)
            }
            FeatureGroupAction::RemoveGate { gate_id, .. } => {
                let position = group
                    .gates
                    .iter()
                    .position(|gate| gate.id == gate_id)
                    .ok_or_else(|| BoardError::invalid("gateId", "No such gate in this group."))?;
                group.gates.remove(position);
                push_history(
                    group,
                    next,
                    FeatureGroupHistoryKind::GateRemoved { gate_id },
                    actor,
                    now,
                );
                clear_acceptance(group, next, "gates changed", actor, now);
                Ok(true)
            }
            FeatureGroupAction::RecordGate {
                gate_id,
                state,
                note,
                evidence,
                ..
            } => {
                let note = clean_note(note.as_deref())?;
                let evidence = clean_evidence(&evidence)?;
                let position = group
                    .gates
                    .iter()
                    .position(|gate| gate.id == gate_id)
                    .ok_or_else(|| BoardError::invalid("gateId", "No such gate in this group."))?;
                let was_passed = group.gates[position]
                    .result
                    .as_ref()
                    .is_some_and(|result| result.state == FeatureGroupGateState::Passed);
                let result = match state {
                    None if group.gates[position].result.is_none() => return Ok(false),
                    None => None,
                    Some(state) => {
                        // Bound to the members' state right now.
                        let current = self.feature_group_assess(group, true)?;
                        Some(FeatureGroupGateResult {
                            state,
                            note,
                            evidence,
                            basis: basis_of(&current.members),
                            recorded_by: actor.clone(),
                            recorded_at_ms: now,
                        })
                    }
                };
                group.gates[position].result = result;
                push_history(
                    group,
                    next,
                    FeatureGroupHistoryKind::GateRecorded { gate_id, state },
                    actor,
                    now,
                );
                if was_passed && state != Some(FeatureGroupGateState::Passed) {
                    clear_acceptance(group, next, "gate result changed", actor, now);
                }
                Ok(true)
            }
            FeatureGroupAction::Accept { .. } => {
                let current = self.feature_group_assess(group, true)?;
                if current.acceptance == FeatureGroupAcceptanceState::Accepted {
                    return Ok(false);
                }
                if !current.unmet.is_empty() {
                    let reasons: Vec<String> =
                        current.unmet.iter().map(FeatureGroupUnmet::label).collect();
                    return Err(BoardError::invalid(
                        "acceptance",
                        format!("Not ready to accept: {}.", reasons.join("; ")),
                    ));
                }
                group.acceptance_record = Some(FeatureGroupAcceptance {
                    basis: basis_of(&current.members),
                    accepted_by: actor.clone(),
                    accepted_at_ms: now,
                    group_revision: next,
                });
                push_history(group, next, FeatureGroupHistoryKind::Accepted, actor, now);
                Ok(true)
            }
            FeatureGroupAction::Reopen { .. } => {
                if group.acceptance_record.take().is_none() {
                    return Ok(false);
                }
                push_history(group, next, FeatureGroupHistoryKind::Reopened, actor, now);
                Ok(true)
            }
            FeatureGroupAction::Archive { archived, .. } => {
                if group.archived == archived {
                    return Ok(false);
                }
                group.archived = archived;
                let kind = if archived {
                    FeatureGroupHistoryKind::Archived
                } else {
                    FeatureGroupHistoryKind::Unarchived
                };
                push_history(group, next, kind, actor, now);
                Ok(true)
            }
            FeatureGroupAction::Create { .. } | FeatureGroupAction::Delete { .. } => Err(storage(
                "create and delete are applied by the caller of feature_group_change",
            )),
        }
    }
}
