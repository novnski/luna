//! RM-04 client runtime state: one paired host's outbox of task commands,
//! kept through disconnects, suspension and restarts until the host's own
//! receipts settle them.
//!
//! The host is authoritative; an [`Outbox`] records only what this client
//! intends and what the host last answered. It is shared by every client:
//! the mobile core drives it from its reconnecting socket. Its rules:
//!
//! - **One host, one session.** An outbox is keyed by [`HostKey`] and by the
//!   receipt actor of one device session. A saved outbox for any other host,
//!   profile or session is refused, so a command never crosses hosts or
//!   principals, and re-pairing starts empty.
//! - **Stable ids.** Each entry is one [`RemoteTaskCommand`] whose id is
//!   minted once ([`new_command_id`]). Every retry and every receipt read
//!   names the same id and the same payload.
//! - **Uncertain means read first.** An entry whose reply was lost is
//!   [`OutboxStatus::Uncertain`]: its receipt is read by id before anything
//!   is resent, and it is resent (unchanged) only when the host has none.
//! - **Only the host settles.** An entry moves past `uncertain` only on a
//!   host receipt that is exactly its own (id, task, this session's actor and
//!   its payload identity) or on a refusal the host answered. A local echo
//!   never counts as accepted, applied or delivered.
//! - **Per task, in order.** An entry is sent only after every earlier entry
//!   for the same task was admitted. A message for a task the host has not
//!   created yet stays a [`OutboxStatus::Draft`]; after an earlier refused
//!   command for its task (a refused create, a refused first message in a
//!   new worktree) it is [`OutboxStatus::Held`] until that one is discarded,
//!   so it never runs somewhere the user did not choose.
//! - **Live controls stay live.** Stop, steer and send-now are fenced from
//!   the host's state when the user acts ([`Outbox::push_fenced`]) and are
//!   never sent later from an offline outbox: one still unsent when its
//!   connection ends is refused locally.
//! - **Bounded.** At most [`MAX_OUTBOX_ENTRIES`] entries and a saved outbox
//!   of at most [`MAX_OUTBOX_BYTES`]; settled entries are pruned oldest first.

use luna_proto::remote_command::{RemoteFollowUp, RemoteTaskCommand, RemoteTaskIntent};
use luna_proto::{CommandReasonCode, CommandReceipt, CommandReceiptStatus, TaskCommandSnapshot};
use serde::{Deserialize, Serialize};

use crate::HostKey;

/// Most entries one outbox holds.
pub const MAX_OUTBOX_ENTRIES: usize = 32;
/// Largest saved outbox written or read.
pub const MAX_OUTBOX_BYTES: usize = 4 * 1024 * 1024;
/// Settled entries kept for display before the oldest are pruned.
pub const MAX_SETTLED_ENTRIES: usize = 16;
const OUTBOX_VERSION: u16 = 1;

/// A fresh command (or task) id for an outbox entry.
pub fn new_command_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Where one entry's transport stands. The host's answer is in the entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SendPhase {
    /// Never sent, or the host proved it never admitted it.
    Unsent,
    /// Sent on the current connection; no answer yet.
    Sending,
    /// Sent, but the answer never arrived.
    Uncertain,
    /// The host answered with a receipt, or refused the call.
    Answered,
}

/// Where one entry stands, as a client shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OutboxStatus {
    /// Kept on this device for a task the host has not created yet.
    Draft,
    /// Waiting behind an earlier command for the same task.
    Waiting,
    /// Not sent: an earlier command for the same task was refused.
    Held,
    /// Sent on the next verified connection.
    Ready,
    Sending,
    /// The answer was lost; its receipt is read before anything is resent.
    Uncertain,
    /// Admitted: the host owns it (`pending` receipt).
    Accepted,
    /// Admitted, but the host could not prove whether its provider accepted
    /// it: reconcile on the Mac before sending it again.
    Unknown,
    Applied,
    /// Refused by the host (a receipt or a refused call) or not sent.
    Refused,
}

impl OutboxStatus {
    /// Settled: the host's terminal answer (or a local refusal) is final.
    pub fn is_settled(self) -> bool {
        matches!(self, Self::Applied | Self::Refused)
    }
}

/// A queued message's delivery, after its queue receipt was applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DeliveryState {
    /// In the host's queue, waiting for the task (or its worktree setup).
    Queued,
    /// The host claimed it and is starting its turn.
    Dispatching,
    /// Its turn started on the host.
    Delivered,
    /// Held on the host (for example behind a failed or cancelled worktree
    /// setup); it stays queued until an explicit send.
    Held,
    /// The host claimed it but could not prove the provider accepted it.
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Delivery {
    pub state: DeliveryState,
    /// The host's reason, for `held` and `unknown`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// One command and what the host answered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutboxEntry {
    pub command: RemoteTaskCommand,
    /// The fences in `command` are frozen. An unfenced entry gets them from
    /// a fresh snapshot just before its first send.
    pub fenced: bool,
    pub phase: SendPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt: Option<CommandReceipt>,
    /// A refusal without a receipt: the host refused the call, or this
    /// device did not send it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<Delivery>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    /// Times it was sent (the same id and payload each time).
    pub sends: u32,
}

impl OutboxEntry {
    pub fn command_id(&self) -> &str {
        &self.command.command_id
    }

    /// Whether this entry's queued-message delivery is followed on the host:
    /// a plain send the host queued (its `queueMessage` receipt applied). A
    /// send the host started as a turn (`run`) is delivered by its own
    /// receipt.
    pub fn tracks_delivery(&self) -> bool {
        matches!(
            self.command.intent,
            RemoteTaskIntent::Send {
                follow_up: RemoteFollowUp::Queue,
                new_worktree: None,
                ..
            }
        ) && self.receipt.as_ref().is_some_and(|receipt| {
            receipt.status == CommandReceiptStatus::Applied && receipt.intent == "queueMessage"
        })
    }

    /// The host receipt that dispatches this queued message from the queue
    /// (`DocHost::drain_queue`'s id for an unedited row), when tracked.
    pub fn delivery_probe(&self) -> Option<String> {
        let RemoteTaskIntent::Send { message_id, .. } = &self.command.intent else {
            return None;
        };
        self.tracks_delivery()
            .then(|| format!("queue-dispatch:{}:{}:1", self.command.task_id, message_id))
    }

    /// Delivery still worth reading: queued or dispatching.
    pub fn delivery_open(&self) -> bool {
        self.tracks_delivery()
            && self.delivery.as_ref().is_none_or(|delivery| {
                matches!(
                    delivery.state,
                    DeliveryState::Queued | DeliveryState::Dispatching
                )
            })
    }

    fn earlier_state(&self) -> Earlier {
        match self.phase {
            SendPhase::Unsent | SendPhase::Sending | SendPhase::Uncertain => Earlier::NotAdmitted,
            SendPhase::Answered => match &self.receipt {
                None => Earlier::Refused,
                Some(receipt) => match receipt.status {
                    CommandReceiptStatus::Pending => Earlier::Admitted { applied: false },
                    CommandReceiptStatus::Applied => Earlier::Admitted { applied: true },
                    _ => Earlier::Refused,
                },
            },
        }
    }
}

/// How an earlier entry for the same task gates a later one.
enum Earlier {
    NotAdmitted,
    Admitted { applied: bool },
    Refused,
}

/// Why the outbox refused a change. Nothing changes when one is returned.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OutboxError {
    #[error("{0}")]
    Invalid(String),
    #[error("the outbox already holds command {0}")]
    Duplicate(String),
    #[error("the outbox is full ({MAX_OUTBOX_ENTRIES} commands); let them settle or discard some")]
    Full,
    #[error("no command {0} in the outbox")]
    Missing(String),
    /// A receipt that is not this entry's (another command, task, session
    /// or payload). Never applied.
    #[error("{0}")]
    NotThisCommand(String),
    /// The host may own this entry: it cannot be discarded until settled.
    #[error("{0}")]
    InUse(String),
    #[error("the saved outbox is larger than its bound")]
    TooLarge,
    #[error("the saved outbox is unreadable: {0}")]
    Unreadable(String),
    #[error("the saved outbox belongs to another host, profile or session")]
    OtherHost,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SavedOutbox {
    version: u16,
    key: HostKey,
    actor_id: String,
    entries: Vec<OutboxEntry>,
}

/// One paired host's outbox. See the module documentation.
pub struct Outbox {
    key: HostKey,
    actor_id: String,
    entries: Vec<OutboxEntry>,
    revision: u64,
}

impl Outbox {
    pub fn new(key: HostKey, actor_id: impl Into<String>) -> Self {
        Self {
            key,
            actor_id: actor_id.into(),
            entries: Vec::new(),
            revision: 1,
        }
    }

    pub fn key(&self) -> &HostKey {
        &self.key
    }

    /// The receipt actor this outbox's commands carry.
    pub fn actor_id(&self) -> &str {
        &self.actor_id
    }

    /// Moves on every change, so a reader re-reads only when it moved.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Every entry, oldest first.
    pub fn entries(&self) -> &[OutboxEntry] {
        &self.entries
    }

    pub fn entry(&self, command_id: &str) -> Option<&OutboxEntry> {
        self.entries
            .iter()
            .find(|entry| entry.command.command_id == command_id)
    }

    /// The entry's status, computed from it and the earlier entries for the
    /// same task.
    pub fn status(&self, command_id: &str) -> Option<OutboxStatus> {
        let index = self.index(command_id)?;
        Some(self.status_at(index))
    }

    /// The earlier entry this one waits for or is held by, if any.
    pub fn blocked_by(&self, command_id: &str) -> Option<&str> {
        let index = self.index(command_id)?;
        let entry = &self.entries[index];
        if entry.phase != SendPhase::Unsent {
            return None;
        }
        self.entries[..index]
            .iter()
            .filter(|earlier| earlier.command.task_id == entry.command.task_id)
            .find(|earlier| {
                let creates = matches!(earlier.command.intent, RemoteTaskIntent::CreateTask { .. });
                match earlier.earlier_state() {
                    Earlier::Refused | Earlier::NotAdmitted => true,
                    Earlier::Admitted { applied } => creates && !applied,
                }
            })
            .map(|earlier| earlier.command.command_id.as_str())
    }

    /// Add a command whose fences are read just before its first send.
    pub fn push(&mut self, command: RemoteTaskCommand, now_ms: i64) -> Result<(), OutboxError> {
        if command.intent.live_only() {
            return Err(OutboxError::Invalid(format!(
                "{} acts on the current turn or queue: fence it when the user acts",
                command.intent.kind()
            )));
        }
        self.insert(command, false, now_ms)
    }

    /// Add a command fenced from the host's state when the user acted. A
    /// live control (stop, steer, send now) must come through here.
    pub fn push_fenced(
        &mut self,
        command: RemoteTaskCommand,
        now_ms: i64,
    ) -> Result<(), OutboxError> {
        self.insert(command, true, now_ms)
    }

    fn insert(
        &mut self,
        command: RemoteTaskCommand,
        fenced: bool,
        now_ms: i64,
    ) -> Result<(), OutboxError> {
        command.validate().map_err(OutboxError::Invalid)?;
        if self.index(&command.command_id).is_some() {
            return Err(OutboxError::Duplicate(command.command_id));
        }
        self.prune();
        if self.entries.len() >= MAX_OUTBOX_ENTRIES {
            return Err(OutboxError::Full);
        }
        self.entries.push(OutboxEntry {
            command,
            fenced,
            phase: SendPhase::Unsent,
            receipt: None,
            refusal: None,
            delivery: None,
            created_at_ms: now_ms,
            updated_at_ms: now_ms,
            sends: 0,
        });
        self.changed();
        Ok(())
    }

    /// The oldest entry that may be sent now (its task's earlier entries all
    /// admitted), if any.
    pub fn next_ready(&self) -> Option<&OutboxEntry> {
        (0..self.entries.len())
            .find(|&index| self.status_at(index) == OutboxStatus::Ready)
            .map(|index| &self.entries[index])
    }

    /// Entries whose answer was lost, oldest first.
    pub fn uncertain(&self) -> Vec<String> {
        self.entries
            .iter()
            .filter(|entry| entry.phase == SendPhase::Uncertain)
            .map(|entry| entry.command.command_id.clone())
            .collect()
    }

    /// Admitted entries whose receipt is still pending, oldest first.
    pub fn pending(&self) -> Vec<String> {
        self.entries
            .iter()
            .filter(|entry| {
                entry.phase == SendPhase::Answered
                    && entry
                        .receipt
                        .as_ref()
                        .is_some_and(|receipt| receipt.status == CommandReceiptStatus::Pending)
            })
            .map(|entry| entry.command.command_id.clone())
            .collect()
    }

    /// Applied queued messages whose delivery is still open, oldest first.
    pub fn deliveries(&self) -> Vec<String> {
        self.entries
            .iter()
            .filter(|entry| entry.delivery_open())
            .map(|entry| entry.command.command_id.clone())
            .collect()
    }

    /// Freeze the entry's fences from `snapshot`, the host's current state
    /// for its task, before its first send. A fenced or sent entry keeps its
    /// fences (a retry is always the identical command).
    pub fn fence(
        &mut self,
        command_id: &str,
        snapshot: &TaskCommandSnapshot,
        now_ms: i64,
    ) -> Result<(), OutboxError> {
        let index = self.require(command_id)?;
        let entry = &mut self.entries[index];
        if entry.fenced || entry.sends > 0 || entry.phase != SendPhase::Unsent {
            return Ok(());
        }
        if snapshot.task_id != entry.command.task_id {
            return Err(OutboxError::NotThisCommand(format!(
                "a snapshot of task {} cannot fence a command for {}",
                snapshot.task_id, entry.command.task_id
            )));
        }
        apply_fences(&mut entry.command, snapshot);
        entry.fenced = true;
        entry.updated_at_ms = now_ms;
        self.changed();
        Ok(())
    }

    /// The entry goes out (again, unchanged): `Unsent` or `Uncertain` →
    /// `Sending`. Returns the exact command to send.
    pub fn begin_send(
        &mut self,
        command_id: &str,
        now_ms: i64,
    ) -> Result<RemoteTaskCommand, OutboxError> {
        let index = self.require(command_id)?;
        let entry = &mut self.entries[index];
        if !matches!(entry.phase, SendPhase::Unsent | SendPhase::Uncertain) {
            return Err(OutboxError::InUse(format!(
                "command {command_id} is not waiting to be sent"
            )));
        }
        entry.phase = SendPhase::Sending;
        entry.sends = entry.sends.saturating_add(1);
        entry.updated_at_ms = now_ms;
        let command = entry.command.clone();
        self.changed();
        Ok(command)
    }

    /// The host's receipt for the entry. Refused unless it is exactly this
    /// entry's; a pending receipt never replaces a terminal one.
    pub fn apply_receipt(
        &mut self,
        receipt: CommandReceipt,
        now_ms: i64,
    ) -> Result<(), OutboxError> {
        let index = self.require(&receipt.command_id)?;
        let entry = &self.entries[index];
        if receipt.task_id != entry.command.task_id {
            return Err(OutboxError::NotThisCommand(format!(
                "receipt for command {} names task {}, not {}",
                receipt.command_id, receipt.task_id, entry.command.task_id
            )));
        }
        if receipt.actor_id != self.actor_id {
            return Err(OutboxError::NotThisCommand(format!(
                "command {} belongs to another client",
                receipt.command_id
            )));
        }
        if receipt.payload_hash != luna_rpc::remote_auth::remote_command_identity(&entry.command) {
            return Err(OutboxError::NotThisCommand(format!(
                "command {} names another payload on the host",
                receipt.command_id
            )));
        }
        if entry
            .receipt
            .as_ref()
            .is_some_and(|held| held.status.is_terminal() && !receipt.status.is_terminal())
        {
            // A late read of an older state.
            return Ok(());
        }
        if entry.phase == SendPhase::Answered && entry.receipt.as_ref() == Some(&receipt) {
            return Ok(());
        }
        let entry = &mut self.entries[index];
        entry.phase = SendPhase::Answered;
        entry.receipt = Some(receipt);
        entry.refusal = None;
        entry.updated_at_ms = now_ms;
        self.changed();
        Ok(())
    }

    /// The send failed. `uncertain`: it may have been admitted (a lost or
    /// unreadable answer) and is read back before any resend. Otherwise the
    /// host refused the call: the entry settles with `detail`.
    pub fn send_failed(
        &mut self,
        command_id: &str,
        uncertain: bool,
        detail: &str,
        now_ms: i64,
    ) -> Result<(), OutboxError> {
        let index = self.require(command_id)?;
        let entry = &mut self.entries[index];
        if entry.phase != SendPhase::Sending {
            return Ok(());
        }
        if uncertain {
            entry.phase = SendPhase::Uncertain;
        } else {
            entry.phase = SendPhase::Answered;
            entry.refusal = Some(detail.to_owned());
        }
        entry.updated_at_ms = now_ms;
        self.changed();
        Ok(())
    }

    /// Settle an entry in flight or awaiting its read-back as refused: the
    /// host answered definitely without a receipt of its own (a denied or
    /// malformed call, or an id that names another command on the host).
    pub fn refuse(
        &mut self,
        command_id: &str,
        detail: &str,
        now_ms: i64,
    ) -> Result<(), OutboxError> {
        let index = self.require(command_id)?;
        let entry = &mut self.entries[index];
        if !matches!(entry.phase, SendPhase::Sending | SendPhase::Uncertain)
            && !(entry.phase == SendPhase::Unsent && entry.sends == 0)
        {
            return Ok(());
        }
        entry.phase = SendPhase::Answered;
        entry.refusal = Some(detail.to_owned());
        entry.updated_at_ms = now_ms;
        self.changed();
        Ok(())
    }

    /// The host has no receipt for an uncertain entry: it was never admitted
    /// and is sent again, unchanged.
    pub fn unadmitted(&mut self, command_id: &str, now_ms: i64) -> Result<(), OutboxError> {
        let index = self.require(command_id)?;
        let entry = &mut self.entries[index];
        if entry.phase != SendPhase::Uncertain {
            return Ok(());
        }
        entry.phase = SendPhase::Unsent;
        entry.updated_at_ms = now_ms;
        self.changed();
        Ok(())
    }

    /// The connection ended. Anything in flight becomes uncertain, and a
    /// live control not sent yet is refused here: it is never sent later.
    pub fn connection_ended(&mut self, now_ms: i64) {
        let mut changed = false;
        for entry in &mut self.entries {
            match entry.phase {
                SendPhase::Sending => {
                    entry.phase = SendPhase::Uncertain;
                    entry.updated_at_ms = now_ms;
                    changed = true;
                }
                SendPhase::Unsent if entry.command.intent.live_only() => {
                    entry.phase = SendPhase::Answered;
                    entry.refusal =
                        Some("Not sent: the connection to the Mac ended first. Try again.".into());
                    entry.updated_at_ms = now_ms;
                    changed = true;
                }
                _ => {}
            }
        }
        if changed {
            self.changed();
        }
    }

    /// The dispatch receipt [`OutboxEntry::delivery_probe`] names, or `None`
    /// while the host has not claimed the message.
    pub fn apply_delivery(
        &mut self,
        command_id: &str,
        dispatch: Option<&CommandReceipt>,
        now_ms: i64,
    ) -> Result<(), OutboxError> {
        let index = self.require(command_id)?;
        let entry = &self.entries[index];
        let Some(probe) = entry.delivery_probe() else {
            return Ok(());
        };
        let delivery = match dispatch {
            None => Delivery {
                state: DeliveryState::Queued,
                detail: None,
            },
            Some(receipt) => {
                if receipt.command_id != probe || receipt.task_id != entry.command.task_id {
                    return Err(OutboxError::NotThisCommand(format!(
                        "receipt {} is not the dispatch of command {command_id}",
                        receipt.command_id
                    )));
                }
                delivery_of(receipt)
            }
        };
        if entry.delivery.as_ref() == Some(&delivery)
            || entry
                .delivery
                .as_ref()
                .is_some_and(|held| held.state == DeliveryState::Delivered)
        {
            return Ok(());
        }
        let entry = &mut self.entries[index];
        entry.delivery = Some(delivery);
        entry.updated_at_ms = now_ms;
        self.changed();
        Ok(())
    }

    /// Remove an entry the host does not own: never sent, or settled.
    pub fn discard(&mut self, command_id: &str) -> Result<(), OutboxError> {
        let index = self.require(command_id)?;
        let entry = &self.entries[index];
        let owned_by_host = match entry.phase {
            SendPhase::Unsent => entry.sends > 0,
            SendPhase::Sending | SendPhase::Uncertain => true,
            SendPhase::Answered => entry
                .receipt
                .as_ref()
                .is_some_and(|receipt| !receipt.status.is_terminal()),
        };
        if owned_by_host {
            return Err(OutboxError::InUse(format!(
                "command {command_id} may have reached the Mac; it can be removed once settled"
            )));
        }
        self.entries.remove(index);
        self.changed();
        Ok(())
    }

    /// Save for the next run, bounded: the oldest settled entries go first.
    pub fn encode(&self) -> Vec<u8> {
        let mut saved = SavedOutbox {
            version: OUTBOX_VERSION,
            key: self.key.clone(),
            actor_id: self.actor_id.clone(),
            entries: self.entries.clone(),
        };
        loop {
            let bytes = serde_json::to_vec(&saved).unwrap_or_default();
            if bytes.len() <= MAX_OUTBOX_BYTES {
                return bytes;
            }
            match saved.entries.iter().position(|entry| {
                entry.phase == SendPhase::Answered
                    && entry
                        .receipt
                        .as_ref()
                        .is_none_or(|receipt| receipt.status.is_terminal())
            }) {
                Some(index) => {
                    saved.entries.remove(index);
                }
                None => {
                    saved.entries.pop();
                }
            }
        }
    }

    /// Restore a saved outbox for this host and session. Entries in flight
    /// when it was saved come back uncertain.
    pub fn decode(bytes: &[u8], key: &HostKey, actor_id: &str) -> Result<Self, OutboxError> {
        if bytes.len() > MAX_OUTBOX_BYTES {
            return Err(OutboxError::TooLarge);
        }
        let saved: SavedOutbox = serde_json::from_slice(bytes)
            .map_err(|error| OutboxError::Unreadable(error.to_string()))?;
        if saved.version != OUTBOX_VERSION {
            return Err(OutboxError::Unreadable(format!(
                "version {} is not {OUTBOX_VERSION}",
                saved.version
            )));
        }
        if saved.key != *key || saved.actor_id != actor_id {
            return Err(OutboxError::OtherHost);
        }
        if saved.entries.len() > MAX_OUTBOX_ENTRIES {
            return Err(OutboxError::Unreadable("too many entries".into()));
        }
        let mut outbox = Self::new(key.clone(), actor_id);
        for mut entry in saved.entries {
            entry.command.validate().map_err(OutboxError::Unreadable)?;
            if outbox.index(&entry.command.command_id).is_some() {
                return Err(OutboxError::Unreadable(format!(
                    "command {} appears twice",
                    entry.command.command_id
                )));
            }
            if let Some(receipt) = &entry.receipt
                && (receipt.command_id != entry.command.command_id
                    || receipt.task_id != entry.command.task_id
                    || receipt.actor_id != actor_id)
            {
                return Err(OutboxError::Unreadable(format!(
                    "command {} holds another command's receipt",
                    entry.command.command_id
                )));
            }
            if entry.phase == SendPhase::Sending {
                entry.phase = SendPhase::Uncertain;
            }
            outbox.entries.push(entry);
        }
        Ok(outbox)
    }

    fn status_at(&self, index: usize) -> OutboxStatus {
        let entry = &self.entries[index];
        match entry.phase {
            SendPhase::Sending => return OutboxStatus::Sending,
            SendPhase::Uncertain => return OutboxStatus::Uncertain,
            SendPhase::Answered => {
                return match &entry.receipt {
                    None => OutboxStatus::Refused,
                    Some(receipt) => receipt_status(receipt),
                };
            }
            SendPhase::Unsent => {}
        }
        let mut status = OutboxStatus::Ready;
        for earlier in self.entries[..index]
            .iter()
            .filter(|earlier| earlier.command.task_id == entry.command.task_id)
        {
            let creates = matches!(earlier.command.intent, RemoteTaskIntent::CreateTask { .. });
            match earlier.earlier_state() {
                Earlier::Refused => return OutboxStatus::Held,
                Earlier::Admitted { applied: true } => {}
                Earlier::Admitted { applied: false } | Earlier::NotAdmitted if creates => {
                    status = OutboxStatus::Draft;
                }
                Earlier::NotAdmitted => {
                    if status == OutboxStatus::Ready {
                        status = OutboxStatus::Waiting;
                    }
                }
                Earlier::Admitted { applied: false } => {}
            }
        }
        status
    }

    /// Drop the oldest settled entries beyond [`MAX_SETTLED_ENTRIES`], never
    /// one that holds a later entry of its task back.
    fn prune(&mut self) {
        loop {
            let settled: Vec<usize> = (0..self.entries.len())
                .filter(|&index| self.prunable(index))
                .collect();
            let all_settled = (0..self.entries.len())
                .filter(|&index| self.status_at(index).is_settled())
                .count();
            if all_settled <= MAX_SETTLED_ENTRIES && self.entries.len() < MAX_OUTBOX_ENTRIES {
                return;
            }
            let Some(&oldest) = settled.first() else {
                return;
            };
            self.entries.remove(oldest);
            self.changed();
        }
    }

    fn prunable(&self, index: usize) -> bool {
        let entry = &self.entries[index];
        match self.status_at(index) {
            OutboxStatus::Applied => !entry.delivery_open(),
            OutboxStatus::Refused => !self.entries[index + 1..]
                .iter()
                .any(|later| later.command.task_id == entry.command.task_id),
            _ => false,
        }
    }

    fn index(&self, command_id: &str) -> Option<usize> {
        self.entries
            .iter()
            .position(|entry| entry.command.command_id == command_id)
    }

    fn require(&self, command_id: &str) -> Result<usize, OutboxError> {
        self.index(command_id)
            .ok_or_else(|| OutboxError::Missing(command_id.to_owned()))
    }

    fn changed(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }
}

/// How a receipt reads as an entry status.
fn receipt_status(receipt: &CommandReceipt) -> OutboxStatus {
    match receipt.status {
        CommandReceiptStatus::Pending
            if receipt.reason.as_ref().is_some_and(|reason| {
                reason.code == CommandReasonCode::ProviderAcceptanceUnknown
            }) =>
        {
            OutboxStatus::Unknown
        }
        CommandReceiptStatus::Pending => OutboxStatus::Accepted,
        CommandReceiptStatus::Applied => OutboxStatus::Applied,
        CommandReceiptStatus::Rejected
        | CommandReceiptStatus::Conflict
        | CommandReceiptStatus::Superseded
        | CommandReceiptStatus::Failed => OutboxStatus::Refused,
    }
}

fn delivery_of(dispatch: &CommandReceipt) -> Delivery {
    let detail = dispatch
        .reason
        .as_ref()
        .map(|reason| reason.message.clone());
    let state = match dispatch.status {
        CommandReceiptStatus::Pending
            if dispatch.reason.as_ref().is_some_and(|reason| {
                reason.code == CommandReasonCode::ProviderAcceptanceUnknown
            }) =>
        {
            DeliveryState::Unknown
        }
        CommandReceiptStatus::Pending => DeliveryState::Dispatching,
        CommandReceiptStatus::Applied => DeliveryState::Delivered,
        CommandReceiptStatus::Rejected
        | CommandReceiptStatus::Conflict
        | CommandReceiptStatus::Superseded
        | CommandReceiptStatus::Failed => DeliveryState::Held,
    };
    Delivery { state, detail }
}

/// The fences each intent needs, from the task's current snapshot: the
/// engine refuses a fenced intent sent without them, and a fence it does not
/// need would refuse an unrelated change (a revision moved by another send).
pub fn apply_fences(command: &mut RemoteTaskCommand, snapshot: &TaskCommandSnapshot) {
    let (revision, binding, turn) = match &command.intent {
        RemoteTaskIntent::CreateTask { .. } | RemoteTaskIntent::CancelCapacityWait { .. } => {
            (false, false, false)
        }
        // A steer belongs to the turn the user saw: a newer or ended turn is
        // refused as stale instead of receiving it.
        RemoteTaskIntent::Send {
            follow_up: RemoteFollowUp::Steer,
            ..
        } => (false, true, true),
        RemoteTaskIntent::Send { .. } | RemoteTaskIntent::RemoveQueued { .. } => {
            (false, true, false)
        }
        RemoteTaskIntent::SendQueuedNow { .. }
        | RemoteTaskIntent::SteerQueuedNow { .. }
        | RemoteTaskIntent::Stop {} => (false, true, true),
        RemoteTaskIntent::SetArchived { .. } => (true, true, false),
    };
    command.expected_task_revision = revision.then_some(snapshot.task_revision);
    command.expected_binding_generation = snapshot.binding_generation.filter(|_| binding);
    command.expected_turn_id = snapshot.active_turn_id.clone().filter(|_| turn);
}
