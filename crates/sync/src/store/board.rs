//! Host-owned board tickets: idempotent mutation receipts, key allocation,
//! dense ranks and the event journal, all committed in one transaction.
//!
//! Policy (validation, authority, field-level merge) lives in the engine's
//! board service. This module guarantees the storage invariants: one receipt
//! per request id, CAS on the ticket revision and on versioned rows, unique
//! keys that are never reused, at most one active worker per ticket and per
//! chat, one active issue link per project, and exactly one event per
//! committed mutation.
use super::*;
use luna_proto::{
    BoardActor, BoardArchivedFilter, BoardColumn, BoardDependency, BoardExecutionIntent,
    BoardMutationReceipt, BoardPlacement, BoardProjectSummary, IssueReference, LaunchSnapshot,
    OutcomeReference, ReviewRun, TicketAuthoringSession, WorkItem, WorkItemEvent,
    WorkItemEventKind, WorkItemTaskLink, pin_order_between,
};

/// Where a ticket lands within its column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoardRankPlacement {
    Top,
    Bottom,
    /// Directly below this ticket (same project and column).
    After(String),
    /// Directly above this ticket (same project and column).
    Before(String),
}

impl From<BoardPlacement> for BoardRankPlacement {
    fn from(value: BoardPlacement) -> Self {
        match value {
            BoardPlacement::Top => Self::Top,
            BoardPlacement::Bottom => Self::Bottom,
        }
    }
}

/// The ticket row a commit writes.
#[derive(Debug, Clone)]
pub enum BoardItemWrite {
    /// No ticket change (auxiliary rows only, e.g. an intent update).
    None,
    /// Insert a new ticket. The store allocates `number`/`key` (with a
    /// host-unique prefix derived from `prefix_hint` on the project's first
    /// ticket) and the rank. `item.revision` must be 1.
    Create {
        item: Box<WorkItem>,
        prefix_hint: String,
        placement: BoardRankPlacement,
    },
    /// Replace the record when the stored revision equals
    /// `expected_revision`. `placement` re-ranks in the record's column
    /// (used when the column changes).
    Replace {
        expected_revision: u64,
        item: Box<WorkItem>,
        placement: Option<BoardRankPlacement>,
    },
    /// Re-rank only. No revision check or bump: concurrent reorders resolve
    /// last-writer-wins with id tie-breaks, never a conflict.
    Rank {
        work_item_id: String,
        placement: BoardRankPlacement,
        at_ms: i64,
        actor: BoardActor,
    },
}

/// Auxiliary rows committed with the ticket. Versioned rows check
/// `expected_version` (`None`: must not exist yet) and the store writes
/// `version = expected + 1`.
#[derive(Debug, Clone)]
pub enum BoardRowWrite {
    TaskLink(Box<WorkItemTaskLink>),
    IssueReference(Box<IssueReference>),
    Launch(Box<LaunchSnapshot>),
    Outcome(Box<OutcomeReference>),
    Intent {
        expected_version: Option<u64>,
        intent: Box<BoardExecutionIntent>,
    },
    ReviewRun {
        expected_version: Option<u64>,
        run: Box<ReviewRun>,
    },
    AuthoringSession {
        expected_version: Option<u64>,
        session: Box<TicketAuthoringSession>,
    },
    /// Append transcript entries (`seq` must be new).
    AuthoringEntries {
        session_id: String,
        entries: Vec<(u64, String)>,
    },
    /// Remove a discarded/ended session's transcript and row.
    DeleteAuthoringSession {
        session_id: String,
    },
    Dependency(Box<BoardDependency>),
    RemoveDependency {
        work_item_id: String,
        blocked_by_id: String,
    },
}

/// One atomic board mutation.
#[derive(Debug, Clone)]
pub struct BoardCommit {
    /// Engine-normalized request id (caller scope + idempotency key).
    pub request_id: String,
    pub payload_hash: String,
    pub committed_at_ms: i64,
    pub item: BoardItemWrite,
    pub rows: Vec<BoardRowWrite>,
    /// The canonical event; `None` for a no-op that still stores a receipt.
    pub event: Option<(WorkItemEventKind, bool)>,
    /// Receipt template. The store fills `work_item_id`, `key`, `revision`,
    /// `column`, `archived`, `item`, `event_seq` and `committed_at_ms` from the
    /// committed state. Required when the commit touches a ticket.
    pub receipt: Option<BoardMutationReceipt>,
}

/// Keyset cursor for list reads: (project, column order, rank, id).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoardListCursor {
    pub project_id: String,
    pub column_order: i64,
    pub rank: String,
    pub id: String,
}

impl BoardListCursor {
    pub fn encode(&self) -> String {
        format!(
            "c1\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}",
            self.project_id, self.column_order, self.rank, self.id
        )
    }

    pub fn decode(value: &str) -> Option<Self> {
        let mut parts = value.split('\u{1f}');
        (parts.next()? == "c1").then_some(())?;
        let cursor = Self {
            project_id: parts.next()?.into(),
            column_order: parts.next()?.parse().ok()?,
            rank: parts.next()?.into(),
            id: parts.next()?.into(),
        };
        parts.next().is_none().then_some(cursor)
    }
}

#[derive(Debug, Clone, Default)]
pub struct BoardListQuery {
    pub owner_host_id: String,
    pub project_id: Option<String>,
    /// Empty: all columns.
    pub columns: Vec<BoardColumn>,
    pub archived: BoardArchivedFilter,
    /// Lower-cased substring matched against key, title and description.
    pub text: Option<String>,
    pub after: Option<BoardListCursor>,
    pub limit: usize,
}

#[derive(Debug, Clone, Default)]
pub struct BoardEventQuery {
    pub after_seq: u64,
    pub work_item_id: Option<String>,
    pub project_id: Option<String>,
    pub activity_only: bool,
    pub limit: usize,
}

fn json<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, StoreError> {
    Ok(serde_json::to_vec(value)?)
}

fn decode<T: serde::de::DeserializeOwned>(bytes: Vec<u8>) -> Result<T, StoreError> {
    Ok(serde_json::from_slice(&bytes)?)
}

fn unique_violation(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(failure, _)
            if failure.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE
                || failure.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_PRIMARYKEY
    )
}

fn constrained(error: rusqlite::Error, what: &str) -> StoreError {
    if unique_violation(&error) {
        StoreError::BoardConflict(what.into())
    } else {
        error.into()
    }
}

fn search_text(item: &WorkItem) -> String {
    format!("{}\n{}\n{}", item.key, item.title, item.description).to_lowercase()
}

fn get_item(c: &Connection, id: &str) -> Result<Option<WorkItem>, StoreError> {
    c.query_row("SELECT record FROM board_items WHERE id=?1", [id], |r| {
        r.get::<_, Vec<u8>>(0)
    })
    .optional()?
    .map(decode)
    .transpose()
}

fn write_item(c: &Connection, item: &WorkItem) -> Result<(), StoreError> {
    c.execute(
        "INSERT INTO board_items(id,project_id,owner_host_id,number,key,column_name,column_order,archived,rank,revision,search_text,created_at_ms,updated_at_ms,record)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)
         ON CONFLICT(id) DO UPDATE SET column_name=excluded.column_name,column_order=excluded.column_order,archived=excluded.archived,rank=excluded.rank,revision=excluded.revision,search_text=excluded.search_text,updated_at_ms=excluded.updated_at_ms,record=excluded.record",
        params![
            item.id,
            item.project_id,
            item.owner_host_id,
            item.number,
            item.key,
            item.column.as_str(),
            item.column.order(),
            item.archived,
            item.rank,
            item.revision,
            search_text(item),
            item.created_at_ms,
            item.updated_at_ms,
            json(item)?,
        ],
    )
    .map_err(|e| constrained(e, "ticket number already allocated"))?;
    Ok(())
}

/// Ordered (id, rank) of a column, excluding archived tickets and `skip`.
fn column_ranks(
    c: &Connection,
    project_id: &str,
    column: BoardColumn,
    skip: Option<&str>,
) -> Result<Vec<(String, String)>, StoreError> {
    let mut q = c.prepare(
        "SELECT id,rank FROM board_items WHERE project_id=?1 AND column_order=?2 AND archived=0 ORDER BY rank,id",
    )?;
    let rows = q
        .query_map(params![project_id, column.order()], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows
        .into_iter()
        .filter(|(id, _)| Some(id.as_str()) != skip)
        .collect())
}

fn between(lower: Option<&str>, upper: Option<&str>) -> Result<String, StoreError> {
    pin_order_between(lower, upper)
        .or_else(|_| pin_order_between(lower, None))
        .map_err(|e| StoreError::BoardConflict(format!("rank: {e}")))
}

/// A rank strictly between the requested neighbors, computed from the
/// column's current order inside the transaction.
fn place(
    c: &Connection,
    project_id: &str,
    column: BoardColumn,
    moving: &str,
    placement: &BoardRankPlacement,
) -> Result<String, StoreError> {
    let order = column_ranks(c, project_id, column, Some(moving))?;
    let index_of = |id: &str| {
        order
            .iter()
            .position(|(candidate, _)| candidate == id)
            .ok_or_else(|| {
                StoreError::BoardConflict(format!(
                    "neighbor {id} is not an active ticket in this project's {} column",
                    column.label()
                ))
            })
    };
    let (lower, upper) = match placement {
        BoardRankPlacement::Top => (None, order.first().map(|(_, rank)| rank.as_str())),
        BoardRankPlacement::Bottom => (order.last().map(|(_, rank)| rank.as_str()), None),
        BoardRankPlacement::After(id) => {
            let index = index_of(id)?;
            (
                Some(order[index].1.as_str()),
                order.get(index + 1).map(|(_, rank)| rank.as_str()),
            )
        }
        BoardRankPlacement::Before(id) => {
            let index = index_of(id)?;
            (
                index
                    .checked_sub(1)
                    .map(|previous| order[previous].1.as_str()),
                Some(order[index].1.as_str()),
            )
        }
    };
    between(lower, upper)
}

/// Allocate the next number for a project, creating its prefix row on the
/// first ticket. Prefixes are unique per host (`LUN`, then `LUN2`, …).
fn allocate_number(
    c: &Connection,
    project_id: &str,
    owner_host_id: &str,
    prefix_hint: &str,
    now: i64,
) -> Result<(String, u64), StoreError> {
    let existing: Option<(String, u64)> = c
        .query_row(
            "SELECT key_prefix,next_number FROM board_projects WHERE project_id=?1",
            [project_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let (prefix, number) = match existing {
        Some(found) => found,
        None => {
            let mut chosen = None;
            for suffix in 1..=99u32 {
                let candidate = if suffix == 1 {
                    prefix_hint.to_owned()
                } else {
                    format!("{prefix_hint}{suffix}")
                };
                let taken: bool = c.query_row(
                    "SELECT EXISTS(SELECT 1 FROM board_projects WHERE owner_host_id=?1 AND key_prefix=?2)",
                    params![owner_host_id, candidate],
                    |r| r.get(0),
                )?;
                if !taken {
                    chosen = Some(candidate);
                    break;
                }
            }
            let prefix = chosen.ok_or_else(|| {
                StoreError::BoardConflict("no free ticket key prefix for this project".into())
            })?;
            c.execute(
                "INSERT INTO board_projects(project_id,owner_host_id,key_prefix,next_number,updated_at_ms) VALUES(?1,?2,?3,1,?4)",
                params![project_id, owner_host_id, prefix, now],
            )?;
            (prefix, 1)
        }
    };
    c.execute(
        "UPDATE board_projects SET next_number=?2,updated_at_ms=?3 WHERE project_id=?1",
        params![project_id, number + 1, now],
    )?;
    Ok((prefix, number))
}

fn write_row(c: &Connection, row: &BoardRowWrite) -> Result<(), StoreError> {
    match row {
        BoardRowWrite::TaskLink(link) => {
            c.execute(
                "INSERT INTO board_task_links(id,work_item_id,task_id,role,active,record) VALUES(?1,?2,?3,?4,?5,?6)
                 ON CONFLICT(id) DO UPDATE SET active=excluded.active,role=excluded.role,record=excluded.record",
                params![link.id, link.work_item_id, link.task_id, link.role.as_str(), link.active, json(link.as_ref())?],
            )
            .map_err(|e| constrained(e, "the ticket already has a working chat, or the chat already works on another ticket"))?;
        }
        BoardRowWrite::IssueReference(issue) => {
            c.execute(
                "INSERT INTO board_issue_refs(id,work_item_id,project_id,dedupe_key,active,record) VALUES(?1,?2,?3,?4,?5,?6)
                 ON CONFLICT(id) DO UPDATE SET active=excluded.active,record=excluded.record",
                params![issue.id, issue.work_item_id, issue.project_id, issue.dedupe_key(), issue.active, json(issue.as_ref())?],
            )
            .map_err(|e| constrained(e, "that issue is already linked to a ticket in this project"))?;
        }
        BoardRowWrite::Launch(launch) => {
            c.execute(
                "INSERT INTO board_launches(id,work_item_id,launch_request_id,execution_generation,created_at_ms,record) VALUES(?1,?2,?3,?4,?5,?6)",
                params![launch.id, launch.work_item_id, launch.launch_request_id, launch.execution_generation, launch.created_at_ms, json(launch.as_ref())?],
            )
            .map_err(|e| constrained(e, "launch snapshots are immutable"))?;
        }
        BoardRowWrite::Outcome(outcome) => {
            c.execute(
                "INSERT INTO board_outcomes(id,work_item_id,kind,observed_at_ms,record) VALUES(?1,?2,?3,?4,?5)
                 ON CONFLICT(id) DO UPDATE SET record=excluded.record",
                params![outcome.id, outcome.work_item_id, outcome.kind.as_str(), outcome.observed_at_ms, json(outcome.as_ref())?],
            )?;
        }
        BoardRowWrite::Intent {
            expected_version,
            intent,
        } => {
            let current: Option<u64> = c
                .query_row(
                    "SELECT version FROM board_intents WHERE work_item_id=?1",
                    [&intent.work_item_id],
                    |r| r.get(0),
                )
                .optional()?;
            if current != *expected_version {
                return Err(StoreError::BoardConflict("execution intent changed".into()));
            }
            let mut intent = intent.as_ref().clone();
            intent.version = expected_version.unwrap_or(0) + 1;
            c.execute(
                "INSERT INTO board_intents(work_item_id,version,intent_revision,execution_generation,desired,needs_reconcile,updated_at_ms,record) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)
                 ON CONFLICT(work_item_id) DO UPDATE SET version=excluded.version,intent_revision=excluded.intent_revision,execution_generation=excluded.execution_generation,desired=excluded.desired,needs_reconcile=excluded.needs_reconcile,updated_at_ms=excluded.updated_at_ms,record=excluded.record",
                params![intent.work_item_id, intent.version, intent.intent_revision, intent.execution_generation, intent.desired.as_str(), intent.needs_reconcile, intent.updated_at_ms, json(&intent)?],
            )?;
        }
        BoardRowWrite::ReviewRun {
            expected_version,
            run,
        } => {
            let current: Option<u64> = c
                .query_row(
                    "SELECT version FROM board_review_runs WHERE id=?1",
                    [&run.id],
                    |r| r.get(0),
                )
                .optional()?;
            if current != *expected_version {
                return Err(StoreError::BoardConflict("review run changed".into()));
            }
            let mut run = run.as_ref().clone();
            run.version = expected_version.unwrap_or(0) + 1;
            c.execute(
                "INSERT INTO board_review_runs(id,work_item_id,reviewer_task_id,state,execution_generation,version,updated_at_ms,record) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)
                 ON CONFLICT(id) DO UPDATE SET reviewer_task_id=excluded.reviewer_task_id,state=excluded.state,version=excluded.version,updated_at_ms=excluded.updated_at_ms,record=excluded.record",
                params![run.id, run.work_item_id, run.reviewer_task_id, run.state.as_str(), run.execution_generation, run.version, run.updated_at_ms, json(&run)?],
            )?;
        }
        BoardRowWrite::AuthoringSession {
            expected_version,
            session,
        } => {
            let current: Option<u64> = c
                .query_row(
                    "SELECT version FROM board_authoring_sessions WHERE id=?1",
                    [&session.id],
                    |r| r.get(0),
                )
                .optional()?;
            if current != *expected_version {
                return Err(StoreError::BoardConflict(
                    "authoring session changed".into(),
                ));
            }
            let mut session = session.as_ref().clone();
            session.version = expected_version.unwrap_or(0) + 1;
            c.execute(
                "INSERT INTO board_authoring_sessions(id,project_id,owner_host_id,state,version,updated_at_ms,record) VALUES(?1,?2,?3,?4,?5,?6,?7)
                 ON CONFLICT(id) DO UPDATE SET state=excluded.state,version=excluded.version,updated_at_ms=excluded.updated_at_ms,record=excluded.record",
                params![session.id, session.project_id, session.owner_host_id, session.state.as_str(), session.version, session.updated_at_ms, json(&session)?],
            )?;
        }
        BoardRowWrite::AuthoringEntries {
            session_id,
            entries,
        } => {
            for (seq, entry) in entries {
                c.execute(
                    "INSERT INTO board_authoring_entries(session_id,seq,entry) VALUES(?1,?2,?3)",
                    params![session_id, seq, entry],
                )
                .map_err(|e| constrained(e, "authoring entry already recorded"))?;
            }
        }
        BoardRowWrite::DeleteAuthoringSession { session_id } => {
            c.execute(
                "DELETE FROM board_authoring_entries WHERE session_id=?1",
                [session_id],
            )?;
            c.execute(
                "DELETE FROM board_authoring_sessions WHERE id=?1",
                [session_id],
            )?;
        }
        BoardRowWrite::Dependency(dependency) => {
            c.execute(
                "INSERT INTO board_dependencies(work_item_id,blocked_by_id,created_at_ms,record) VALUES(?1,?2,?3,?4)
                 ON CONFLICT(work_item_id,blocked_by_id) DO NOTHING",
                params![dependency.work_item_id, dependency.blocked_by_id, dependency.created_at_ms, json(dependency.as_ref())?],
            )?;
        }
        BoardRowWrite::RemoveDependency {
            work_item_id,
            blocked_by_id,
        } => {
            c.execute(
                "DELETE FROM board_dependencies WHERE work_item_id=?1 AND blocked_by_id=?2",
                params![work_item_id, blocked_by_id],
            )?;
        }
    }
    Ok(())
}

fn query_records<T: serde::de::DeserializeOwned>(
    c: &Connection,
    sql: &str,
    params: impl rusqlite::Params,
) -> Result<Vec<T>, StoreError> {
    let mut q = c.prepare(sql)?;
    let rows = q
        .query_map(params, |r| r.get::<_, Vec<u8>>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter().map(decode).collect()
}

impl DocsStore {
    /// Wakes after every committed board mutation (value is a counter).
    pub fn board_changes(&self) -> tokio::sync::watch::Receiver<u64> {
        self.board_changes.subscribe()
    }

    /// Apply one board mutation atomically. A repeated request id with the
    /// same payload hash returns the stored receipt without effects; a
    /// different payload is [`StoreError::BoardRequestConflict`].
    pub fn board_commit(
        &self,
        commit: BoardCommit,
        permit: Option<&WritePermit>,
    ) -> Result<Option<BoardMutationReceipt>, StoreError> {
        let mut c = self.command_writer(permit)?;
        let tx = c.transaction()?;
        let previous: Option<(String, Vec<u8>)> = tx
            .query_row(
                "SELECT payload_hash,receipt FROM board_mutations WHERE request_id=?1",
                [&commit.request_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((hash, receipt)) = previous {
            if hash != commit.payload_hash {
                return Err(StoreError::BoardRequestConflict);
            }
            return Ok(Some(decode(receipt)?));
        }
        let now = commit.committed_at_ms;
        let item = match commit.item {
            BoardItemWrite::None => None,
            BoardItemWrite::Create {
                item,
                prefix_hint,
                placement,
            } => {
                let mut item = *item;
                if get_item(&tx, &item.id)?.is_some() {
                    return Err(StoreError::BoardConflict(format!(
                        "ticket {} already exists",
                        item.id
                    )));
                }
                let (prefix, number) = allocate_number(
                    &tx,
                    &item.project_id,
                    &item.owner_host_id,
                    &prefix_hint,
                    now,
                )?;
                item.number = number;
                item.key = format!("{prefix}-{number}");
                item.rank = place(&tx, &item.project_id, item.column, &item.id, &placement)?;
                write_item(&tx, &item)?;
                Some(item)
            }
            BoardItemWrite::Replace {
                expected_revision,
                item,
                placement,
            } => {
                let mut item = *item;
                let current = get_item(&tx, &item.id)?
                    .ok_or_else(|| StoreError::BoardNotFound(item.id.clone()))?;
                if current.revision != expected_revision
                    || current.project_id != item.project_id
                    || current.key != item.key
                {
                    return Err(StoreError::BoardRevisionConflict {
                        work_item_id: item.id,
                        current_revision: current.revision,
                    });
                }
                if let Some(placement) = placement {
                    item.rank = place(&tx, &item.project_id, item.column, &item.id, &placement)?;
                }
                write_item(&tx, &item)?;
                Some(item)
            }
            BoardItemWrite::Rank {
                work_item_id,
                placement,
                at_ms,
                actor,
            } => {
                let mut item = get_item(&tx, &work_item_id)?
                    .ok_or_else(|| StoreError::BoardNotFound(work_item_id.clone()))?;
                if item.archived {
                    return Err(StoreError::BoardConflict(
                        "archived tickets cannot be reordered".into(),
                    ));
                }
                item.rank = place(&tx, &item.project_id, item.column, &item.id, &placement)?;
                item.updated_at_ms = at_ms;
                item.updated_by = actor;
                write_item(&tx, &item)?;
                Some(item)
            }
        };
        for row in &commit.rows {
            write_row(&tx, row)?;
        }
        let event_seq = match (&commit.event, &item) {
            (Some((kind, activity)), Some(item)) => {
                // Fill values only known after allocation inside this
                // transaction (the new key, the computed rank).
                let mut kind = kind.clone();
                match &mut kind {
                    WorkItemEventKind::Created { key, column } => {
                        *key = item.key.clone();
                        *column = item.column;
                    }
                    WorkItemEventKind::Reordered { column, rank } => {
                        *column = item.column;
                        *rank = item.rank.clone();
                    }
                    _ => {}
                }
                let actor = commit
                    .receipt
                    .as_ref()
                    .map(|receipt| receipt.actor.clone())
                    .unwrap_or_else(|| item.updated_by.clone());
                let mut event = WorkItemEvent {
                    seq: 0,
                    id: format!("ev-{}", uuid::Uuid::new_v4().simple()),
                    work_item_id: item.id.clone(),
                    project_id: item.project_id.clone(),
                    revision: item.revision,
                    actor,
                    request_id: commit.request_id.clone(),
                    at_ms: now,
                    activity: *activity,
                    kind,
                };
                tx.execute(
                    "INSERT INTO board_events(id,work_item_id,project_id,revision,kind,activity,at_ms,record) VALUES(?1,?2,?3,?4,?5,?6,?7,X'')",
                    params![event.id, event.work_item_id, event.project_id, event.revision, event.kind.name(), event.activity, event.at_ms],
                )?;
                event.seq = tx.last_insert_rowid() as u64;
                tx.execute(
                    "UPDATE board_events SET record=?2 WHERE seq=?1",
                    params![event.seq, json(&event)?],
                )?;
                Some(event.seq)
            }
            (Some(_), None) => {
                return Err(StoreError::BoardConflict(
                    "a board event needs a ticket".into(),
                ));
            }
            (None, _) => None,
        };
        let receipt = match (commit.receipt, item) {
            (Some(mut receipt), Some(item)) => {
                receipt.request_id = commit.request_id.clone();
                receipt.payload_hash = commit.payload_hash.clone();
                receipt.work_item_id = item.id.clone();
                receipt.key = item.key.clone();
                receipt.revision = item.revision;
                receipt.column = item.column;
                receipt.archived = item.archived;
                receipt.event_seq = event_seq;
                receipt.committed_at_ms = now;
                receipt.item = item;
                tx.execute(
                    "INSERT INTO board_mutations(request_id,work_item_id,operation,payload_hash,committed_at_ms,receipt) VALUES(?1,?2,?3,?4,?5,?6)",
                    params![receipt.request_id, receipt.work_item_id, receipt.operation.as_str(), receipt.payload_hash, now, json(&receipt)?],
                )?;
                Some(receipt)
            }
            (Some(_), None) => {
                return Err(StoreError::BoardConflict(
                    "a ticket receipt needs a ticket".into(),
                ));
            }
            (None, _) => None,
        };
        tx.commit()?;
        self.board_changes
            .send_modify(|counter| *counter = counter.wrapping_add(1));
        Ok(receipt)
    }

    pub fn board_receipt(
        &self,
        request_id: &str,
    ) -> Result<Option<BoardMutationReceipt>, StoreError> {
        self.conn()
            .query_row(
                "SELECT receipt FROM board_mutations WHERE request_id=?1",
                [request_id],
                |r| r.get::<_, Vec<u8>>(0),
            )
            .optional()?
            .map(decode)
            .transpose()
    }

    pub fn board_item(&self, id: &str) -> Result<Option<WorkItem>, StoreError> {
        get_item(&self.conn(), id)
    }

    pub fn board_item_by_key(
        &self,
        owner_host_id: &str,
        key: &str,
    ) -> Result<Option<WorkItem>, StoreError> {
        self.conn()
            .query_row(
                "SELECT record FROM board_items WHERE owner_host_id=?1 AND key=?2",
                params![owner_host_id, key],
                |r| r.get::<_, Vec<u8>>(0),
            )
            .optional()?
            .map(decode)
            .transpose()
    }

    /// Bounded keyset page ordered by project, column, rank, id.
    pub fn board_list(&self, query: &BoardListQuery) -> Result<Vec<WorkItem>, StoreError> {
        let c = self.conn();
        let columns: Vec<i64> = if query.columns.is_empty() {
            BoardColumn::ALL.iter().map(|c| c.order()).collect()
        } else {
            query.columns.iter().map(|c| c.order()).collect()
        };
        let columns = columns
            .iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let archived = match query.archived {
            BoardArchivedFilter::Exclude => "archived=0",
            BoardArchivedFilter::Include => "1",
            BoardArchivedFilter::Only => "archived=1",
        };
        let pattern = query.text.as_ref().map(|text| {
            let escaped = text
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_");
            format!("%{escaped}%")
        });
        let after = query.after.as_ref();
        let sql = format!(
            "SELECT record FROM board_items WHERE owner_host_id=?1 AND (?2 IS NULL OR project_id=?2)
             AND column_order IN ({columns}) AND {archived}
             AND (?3 IS NULL OR search_text LIKE ?3 ESCAPE '\\')
             AND (?4 IS NULL OR (project_id,column_order,rank,id) > (?4,?5,?6,?7))
             ORDER BY project_id,column_order,rank,id LIMIT ?8"
        );
        query_records(
            &c,
            &sql,
            params![
                query.owner_host_id,
                query.project_id,
                pattern,
                after.map(|a| a.project_id.clone()),
                after.map(|a| a.column_order),
                after.map(|a| a.rank.clone()),
                after.map(|a| a.id.clone()),
                query.limit.clamp(1, 1001) as i64,
            ],
        )
    }

    /// Per-project counts and prefixes for this host (optionally one project).
    pub fn board_project_summaries(
        &self,
        owner_host_id: &str,
        project_id: Option<&str>,
    ) -> Result<Vec<BoardProjectSummary>, StoreError> {
        let c = self.conn();
        let mut q = c.prepare(
            "SELECT i.project_id,
                    sum(i.archived=0 AND i.column_order=0), sum(i.archived=0 AND i.column_order=1),
                    sum(i.archived=0 AND i.column_order=2), sum(i.archived=1),
                    (SELECT key_prefix FROM board_projects p WHERE p.project_id=i.project_id)
             FROM board_items i WHERE i.owner_host_id=?1 AND (?2 IS NULL OR i.project_id=?2)
             GROUP BY i.project_id ORDER BY i.project_id LIMIT 1000",
        )?;
        let rows = q
            .query_map(params![owner_host_id, project_id], |r| {
                Ok(BoardProjectSummary {
                    project_id: r.get(0)?,
                    owner_host_id: owner_host_id.into(),
                    backlog: r.get::<_, i64>(1)? as u64,
                    ready: r.get::<_, i64>(2)? as u64,
                    done: r.get::<_, i64>(3)? as u64,
                    archived: r.get::<_, i64>(4)? as u64,
                    key_prefix: r.get(5)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// The project's key prefix and next number, once allocated.
    pub fn board_project_keys(
        &self,
        project_id: &str,
    ) -> Result<Option<(String, u64)>, StoreError> {
        Ok(self
            .conn()
            .query_row(
                "SELECT key_prefix,next_number FROM board_projects WHERE project_id=?1",
                [project_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
    }

    pub fn board_event_head(&self) -> Result<u64, StoreError> {
        Ok(self
            .conn()
            .query_row("SELECT coalesce(max(seq),0) FROM board_events", [], |r| {
                r.get::<_, i64>(0)
            })? as u64)
    }

    /// Journal events after `after_seq`, oldest first (bounded).
    pub fn board_events(&self, query: &BoardEventQuery) -> Result<Vec<WorkItemEvent>, StoreError> {
        query_records(
            &self.conn(),
            "SELECT record FROM board_events WHERE seq>?1 AND (?2 IS NULL OR work_item_id=?2)
             AND (?3 IS NULL OR project_id=?3) AND (NOT ?4 OR activity=1) ORDER BY seq LIMIT ?5",
            params![
                query.after_seq as i64,
                query.work_item_id,
                query.project_id,
                query.activity_only,
                query.limit.clamp(1, 1001) as i64
            ],
        )
    }

    /// Newest events for one ticket (newest first, bounded).
    pub fn board_recent_events(
        &self,
        work_item_id: &str,
        limit: usize,
    ) -> Result<Vec<WorkItemEvent>, StoreError> {
        query_records(
            &self.conn(),
            "SELECT record FROM board_events WHERE work_item_id=?1 ORDER BY seq DESC LIMIT ?2",
            params![work_item_id, limit.clamp(1, 1001) as i64],
        )
    }

    pub fn board_task_links_for_item(
        &self,
        work_item_id: &str,
        active_only: bool,
    ) -> Result<Vec<WorkItemTaskLink>, StoreError> {
        query_records(
            &self.conn(),
            "SELECT record FROM board_task_links WHERE work_item_id=?1 AND (NOT ?2 OR active=1) ORDER BY rowid LIMIT 256",
            params![work_item_id, active_only],
        )
    }

    pub fn board_task_links_for_task(
        &self,
        task_id: &str,
        active_only: bool,
    ) -> Result<Vec<WorkItemTaskLink>, StoreError> {
        query_records(
            &self.conn(),
            "SELECT record FROM board_task_links WHERE task_id=?1 AND (NOT ?2 OR active=1) ORDER BY rowid LIMIT 256",
            params![task_id, active_only],
        )
    }

    pub fn board_issue_refs_for_item(
        &self,
        work_item_id: &str,
    ) -> Result<Vec<IssueReference>, StoreError> {
        query_records(
            &self.conn(),
            "SELECT record FROM board_issue_refs WHERE work_item_id=?1 ORDER BY rowid LIMIT 64",
            [work_item_id],
        )
    }

    pub fn board_issue_ref_by_dedupe(
        &self,
        project_id: &str,
        dedupe_key: &str,
    ) -> Result<Option<IssueReference>, StoreError> {
        Ok(query_records(
            &self.conn(),
            "SELECT record FROM board_issue_refs WHERE project_id=?1 AND dedupe_key=?2 AND active=1",
            params![project_id, dedupe_key],
        )?
        .pop())
    }

    pub fn board_launches(
        &self,
        work_item_id: &str,
        limit: usize,
    ) -> Result<Vec<LaunchSnapshot>, StoreError> {
        query_records(
            &self.conn(),
            "SELECT record FROM board_launches WHERE work_item_id=?1 ORDER BY created_at_ms DESC,id DESC LIMIT ?2",
            params![work_item_id, limit.clamp(1, 1001) as i64],
        )
    }

    pub fn board_launch_by_request(
        &self,
        launch_request_id: &str,
    ) -> Result<Option<LaunchSnapshot>, StoreError> {
        Ok(query_records(
            &self.conn(),
            "SELECT record FROM board_launches WHERE launch_request_id=?1",
            [launch_request_id],
        )?
        .pop())
    }

    pub fn board_outcomes(
        &self,
        work_item_id: &str,
        limit: usize,
    ) -> Result<Vec<OutcomeReference>, StoreError> {
        query_records(
            &self.conn(),
            "SELECT record FROM board_outcomes WHERE work_item_id=?1 ORDER BY observed_at_ms DESC,id DESC LIMIT ?2",
            params![work_item_id, limit.clamp(1, 1001) as i64],
        )
    }

    pub fn board_intent(
        &self,
        work_item_id: &str,
    ) -> Result<Option<BoardExecutionIntent>, StoreError> {
        Ok(query_records(
            &self.conn(),
            "SELECT record FROM board_intents WHERE work_item_id=?1",
            [work_item_id],
        )?
        .pop())
    }

    /// Intents a restart must reconcile before acting (bounded).
    pub fn board_intents_needing_reconcile(
        &self,
        limit: usize,
    ) -> Result<Vec<BoardExecutionIntent>, StoreError> {
        query_records(
            &self.conn(),
            "SELECT record FROM board_intents WHERE needs_reconcile=1 OR desired='ready' ORDER BY updated_at_ms LIMIT ?1",
            [limit.clamp(1, 1001) as i64],
        )
    }

    pub fn board_review_runs(
        &self,
        work_item_id: &str,
        limit: usize,
    ) -> Result<Vec<ReviewRun>, StoreError> {
        query_records(
            &self.conn(),
            "SELECT record FROM board_review_runs WHERE work_item_id=?1 ORDER BY updated_at_ms DESC,id DESC LIMIT ?2",
            params![work_item_id, limit.clamp(1, 1001) as i64],
        )
    }

    pub fn board_review_run(&self, id: &str) -> Result<Option<ReviewRun>, StoreError> {
        Ok(query_records(
            &self.conn(),
            "SELECT record FROM board_review_runs WHERE id=?1",
            [id],
        )?
        .pop())
    }

    pub fn board_review_runs_for_reviewer(
        &self,
        reviewer_task_id: &str,
    ) -> Result<Vec<ReviewRun>, StoreError> {
        query_records(
            &self.conn(),
            "SELECT record FROM board_review_runs WHERE reviewer_task_id=?1 ORDER BY updated_at_ms DESC LIMIT 64",
            [reviewer_task_id],
        )
    }

    pub fn board_authoring_session(
        &self,
        id: &str,
    ) -> Result<Option<TicketAuthoringSession>, StoreError> {
        Ok(query_records(
            &self.conn(),
            "SELECT record FROM board_authoring_sessions WHERE id=?1",
            [id],
        )?
        .pop())
    }

    /// Sessions for a host (optionally one project) in the given states,
    /// newest first.
    pub fn board_authoring_sessions(
        &self,
        owner_host_id: &str,
        project_id: Option<&str>,
        states: &[luna_proto::AuthoringState],
    ) -> Result<Vec<TicketAuthoringSession>, StoreError> {
        let wanted: Vec<&str> = states.iter().map(|state| state.as_str()).collect();
        let sessions: Vec<TicketAuthoringSession> = query_records(
            &self.conn(),
            "SELECT record FROM board_authoring_sessions WHERE owner_host_id=?1 AND (?2 IS NULL OR project_id=?2) ORDER BY updated_at_ms DESC LIMIT 200",
            params![owner_host_id, project_id],
        )?;
        Ok(sessions
            .into_iter()
            .filter(|session| wanted.is_empty() || wanted.contains(&session.state.as_str()))
            .collect())
    }

    /// Whether this id names a temporary authoring session (exclusion
    /// predicate for ordinary chat listings, search, sync and import).
    pub fn is_board_authoring_session(&self, id: &str) -> Result<bool, StoreError> {
        Ok(self.conn().query_row(
            "SELECT EXISTS(SELECT 1 FROM board_authoring_sessions WHERE id=?1)",
            [id],
            |r| r.get(0),
        )?)
    }

    /// Transcript entries older than `before` (newest first, bounded).
    pub fn board_authoring_entries(
        &self,
        session_id: &str,
        before: Option<u64>,
        limit: usize,
    ) -> Result<Vec<(u64, String)>, StoreError> {
        let c = self.conn();
        let mut q = c.prepare(
            "SELECT seq,entry FROM board_authoring_entries WHERE session_id=?1 AND (?2 IS NULL OR seq<?2) ORDER BY seq DESC LIMIT ?3",
        )?;
        let rows = q
            .query_map(
                params![
                    session_id,
                    before.map(|b| b as i64),
                    limit.clamp(1, 1001) as i64
                ],
                |r| Ok((r.get::<_, i64>(0)? as u64, r.get::<_, String>(1)?)),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// What `work_item_id` is blocked by.
    pub fn board_dependencies(
        &self,
        work_item_id: &str,
    ) -> Result<Vec<BoardDependency>, StoreError> {
        query_records(
            &self.conn(),
            "SELECT record FROM board_dependencies WHERE work_item_id=?1 ORDER BY created_at_ms LIMIT 256",
            [work_item_id],
        )
    }

    /// Tickets blocked by `work_item_id`.
    pub fn board_dependents(&self, work_item_id: &str) -> Result<Vec<BoardDependency>, StoreError> {
        query_records(
            &self.conn(),
            "SELECT record FROM board_dependencies WHERE blocked_by_id=?1 ORDER BY created_at_ms LIMIT 256",
            [work_item_id],
        )
    }
}
