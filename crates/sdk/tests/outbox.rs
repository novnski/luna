//! RM-04 client-side rules of `luna_sdk::outbox::Outbox`, the shared client
//! runtime state every paired client keeps its task commands in.
//!
//! The end-to-end owners (`luna-engine`'s `remote_task_commands` tests and
//! the `remote_access` mobile outbox campaign) drive a real host and a real
//! mobile runtime. A correct host never sends what this table feeds: a
//! receipt for another command, task, session or payload; a late pending
//! read after a terminal one; another host's or session's saved outbox; a
//! dispatch receipt for another message. This table owns what a client must
//! do when one arrives anyway (refuse it and keep its state), and the local
//! rules no host answer can exercise: per-task order (drafts before a
//! create, held after a refusal), live controls never sent late, discard
//! only of what the host does not own, frozen fences, and bounds.
//! Failure contract: docs/pending-tasks/control-this-mac/rm-04/README.md
//! (O1–O10).

use luna_proto::remote_command::{
    RemoteFollowUp, RemoteTaskCommand, RemoteTaskIntent, remote_actor_id,
};
use luna_proto::{
    CommandReasonCode, CommandReceipt, CommandReceiptReason, CommandReceiptStatus,
    TaskCommandSnapshot, WorkspaceScope,
};
use luna_rpc::remote_auth::remote_command_identity;
use luna_sdk::HostKey;
use luna_sdk::outbox::{
    DeliveryState, MAX_OUTBOX_ENTRIES, Outbox, OutboxError, OutboxStatus, SendPhase,
};

const TASK: &str = "7f1c2a7e-3b7c-4e57-9d4a-5f8f3c1d2e10";

fn key() -> HostKey {
    HostKey {
        device_id: "mac".into(),
        environment_id: "profile".into(),
        workspace_scope: WorkspaceScope::Local,
    }
}

fn actor() -> String {
    remote_actor_id("session-1")
}

fn new_outbox() -> Outbox {
    Outbox::new(key(), actor())
}

fn command(id: &str, task: &str, intent: RemoteTaskIntent) -> RemoteTaskCommand {
    RemoteTaskCommand {
        command_id: id.into(),
        task_id: task.into(),
        expected_task_revision: None,
        expected_binding_generation: None,
        expected_turn_id: None,
        issued_at_ms: Some(1),
        intent,
    }
}

fn create(id: &str) -> RemoteTaskCommand {
    command(
        id,
        TASK,
        RemoteTaskIntent::CreateTask {
            project_id: "project".into(),
            permission_mode: None,
        },
    )
}

fn send(id: &str) -> RemoteTaskCommand {
    command(
        id,
        TASK,
        RemoteTaskIntent::Send {
            message_id: id.into(),
            text: format!("message {id}"),
            follow_up: RemoteFollowUp::Queue,
            new_worktree: None,
        },
    )
}

fn stop(id: &str) -> RemoteTaskCommand {
    command(id, TASK, RemoteTaskIntent::Stop {})
}

/// The receipt a correct host returns for `command`.
fn receipt(command: &RemoteTaskCommand, status: CommandReceiptStatus) -> CommandReceipt {
    CommandReceipt {
        command_id: command.command_id.clone(),
        task_id: command.task_id.clone(),
        intent: "queueMessage".into(),
        payload_hash: remote_command_identity(command),
        status,
        snapshot: TaskCommandSnapshot {
            task_id: command.task_id.clone(),
            task_revision: 1,
            task_sequence: 1,
            ..Default::default()
        },
        event_sequence: None,
        actor_id: actor(),
        client_id: "luna-remote".into(),
        client_session_id: "session-1".into(),
        execution_environment: Some("mac".into()),
        correlation_id: None,
        reason: None,
        recovery_action: None,
        accepted_at_ms: 1,
        committed_at_ms: None,
        observed_at_ms: 1,
    }
}

/// Push, send and answer `command` with `status`.
fn answered(outbox: &mut Outbox, command: RemoteTaskCommand, status: CommandReceiptStatus) {
    let host = receipt(&command, status);
    outbox.push(command.clone(), 1).unwrap();
    outbox.begin_send(&command.command_id, 2).unwrap();
    outbox.apply_receipt(host, 3).unwrap();
}

/// O1: a saved outbox is restored only for its own host and session, within
/// its bounds and whole; an entry in flight comes back uncertain.
#[test]
fn o1_a_saved_outbox_restores_only_for_its_host_and_session() {
    let mut saved = new_outbox();
    saved.push(create("create"), 1).unwrap();
    saved.push(send("sent"), 1).unwrap();
    saved.begin_send("create", 2).unwrap();
    let bytes = saved.encode();

    let restored = Outbox::decode(&bytes, &key(), &actor()).unwrap();
    assert_eq!(restored.entries().len(), 2);
    assert_eq!(restored.status("create"), Some(OutboxStatus::Uncertain));
    assert_eq!(restored.status("sent"), Some(OutboxStatus::Draft));

    let mut other = key();
    other.environment_id = "another-profile".into();
    assert_eq!(
        Outbox::decode(&bytes, &other, &actor()).err(),
        Some(OutboxError::OtherHost)
    );
    assert_eq!(
        Outbox::decode(&bytes, &key(), &remote_actor_id("session-2")).err(),
        Some(OutboxError::OtherHost),
        "a re-paired session starts empty"
    );
    assert!(matches!(
        Outbox::decode(b"{not json", &key(), &actor()),
        Err(OutboxError::Unreadable(_))
    ));
    let mut doubled: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let first = doubled["entries"][0].clone();
    doubled["entries"].as_array_mut().unwrap().push(first);
    assert!(matches!(
        Outbox::decode(&serde_json::to_vec(&doubled).unwrap(), &key(), &actor()),
        Err(OutboxError::Unreadable(_))
    ));
    let mut stolen: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let mut foreign = receipt(&send("sent"), CommandReceiptStatus::Applied);
    foreign.command_id = "someone-else".into();
    stolen["entries"][1]["receipt"] = serde_json::to_value(foreign).unwrap();
    assert!(matches!(
        Outbox::decode(&serde_json::to_vec(&stolen).unwrap(), &key(), &actor()),
        Err(OutboxError::Unreadable(_))
    ));
    assert_eq!(
        Outbox::decode(
            &vec![b' '; luna_sdk::outbox::MAX_OUTBOX_BYTES + 1],
            &key(),
            &actor()
        )
        .err(),
        Some(OutboxError::TooLarge)
    );
}

/// O2: a receipt for another task, session or payload is never this entry's;
/// nothing changes when one is refused.
#[test]
fn o2_a_receipt_that_is_not_exactly_this_commands_is_refused() {
    let mut outbox = new_outbox();
    let sent = send("sent");
    outbox.push(sent.clone(), 1).unwrap();
    outbox.begin_send("sent", 2).unwrap();
    let revision = outbox.revision();

    let mut other_task = receipt(&sent, CommandReceiptStatus::Applied);
    other_task.task_id = "another-task".into();
    let mut other_actor = receipt(&sent, CommandReceiptStatus::Applied);
    other_actor.actor_id = remote_actor_id("session-2");
    let mut other_payload = receipt(&sent, CommandReceiptStatus::Applied);
    other_payload.payload_hash = remote_command_identity(&send("another"));
    for refused in [other_task, other_actor, other_payload] {
        assert!(matches!(
            outbox.apply_receipt(refused, 3),
            Err(OutboxError::NotThisCommand(_))
        ));
    }
    let mut unknown = receipt(&sent, CommandReceiptStatus::Applied);
    unknown.command_id = "never-pushed".into();
    assert!(matches!(
        outbox.apply_receipt(unknown, 3),
        Err(OutboxError::Missing(_))
    ));
    assert_eq!(outbox.revision(), revision);
    assert_eq!(outbox.status("sent"), Some(OutboxStatus::Sending));
}

/// O3: only the host settles an entry, and a late pending read never undoes
/// a terminal receipt; a pending receipt naming an unknown provider outcome
/// reads as unknown.
#[test]
fn o3_host_receipts_settle_entries_and_never_regress() {
    let mut outbox = new_outbox();
    let sent = send("sent");
    outbox.push(sent.clone(), 1).unwrap();
    outbox.begin_send("sent", 2).unwrap();
    outbox
        .send_failed("sent", true, "the reply was lost", 3)
        .unwrap();
    assert_eq!(outbox.status("sent"), Some(OutboxStatus::Uncertain));
    assert_eq!(outbox.uncertain(), vec!["sent".to_owned()]);

    let mut unknown = receipt(&sent, CommandReceiptStatus::Pending);
    unknown.reason = Some(CommandReceiptReason {
        code: CommandReasonCode::ProviderAcceptanceUnknown,
        message: "restarted after claiming it".into(),
    });
    outbox.apply_receipt(unknown, 4).unwrap();
    assert_eq!(outbox.status("sent"), Some(OutboxStatus::Unknown));
    outbox
        .apply_receipt(receipt(&sent, CommandReceiptStatus::Applied), 5)
        .unwrap();
    assert_eq!(outbox.status("sent"), Some(OutboxStatus::Applied));
    outbox
        .apply_receipt(receipt(&sent, CommandReceiptStatus::Pending), 6)
        .unwrap();
    assert_eq!(
        outbox.status("sent"),
        Some(OutboxStatus::Applied),
        "a late pending read does not undo the terminal receipt"
    );
    let entry = outbox.entry("sent").unwrap();
    assert_eq!(entry.sends, 1, "a lost reply is read back, never resent");
}

/// O4: per task, in order. A message for a task the host has not created
/// yet is a draft; one behind an unadmitted command waits; after a refused
/// command it is held, and discarding that command releases it.
#[test]
fn o4_entries_of_one_task_go_in_order_and_hold_after_a_refusal() {
    let mut outbox = new_outbox();
    outbox.push(create("create"), 1).unwrap();
    outbox.push(send("first"), 1).unwrap();
    outbox.push(send("second"), 1).unwrap();
    assert_eq!(outbox.status("first"), Some(OutboxStatus::Draft));
    assert_eq!(outbox.blocked_by("first"), Some("create"));
    assert_eq!(
        outbox.next_ready().map(|entry| entry.command_id()),
        Some("create")
    );

    // The create is refused: its messages are kept, held behind it.
    outbox.begin_send("create", 2).unwrap();
    let mut refused = receipt(&create("create"), CommandReceiptStatus::Rejected);
    refused.reason = Some(CommandReceiptReason {
        code: CommandReasonCode::OperationRejected,
        message: "no such project".into(),
    });
    outbox.apply_receipt(refused, 3).unwrap();
    assert_eq!(outbox.status("first"), Some(OutboxStatus::Held));
    assert_eq!(outbox.blocked_by("first"), Some("create"));
    assert!(outbox.next_ready().is_none(), "nothing runs elsewhere");
    outbox.discard("create").unwrap();
    assert_eq!(outbox.status("first"), Some(OutboxStatus::Ready));
    assert_eq!(outbox.status("second"), Some(OutboxStatus::Waiting));

    // The first send admitted (pending) releases the next one.
    outbox.begin_send("first", 4).unwrap();
    assert_eq!(outbox.status("second"), Some(OutboxStatus::Waiting));
    outbox
        .apply_receipt(receipt(&send("first"), CommandReceiptStatus::Pending), 5)
        .unwrap();
    assert_eq!(outbox.status("first"), Some(OutboxStatus::Accepted));
    assert_eq!(outbox.status("second"), Some(OutboxStatus::Ready));

    // Other tasks never wait for this one.
    let other = command(
        "other",
        "another-task",
        RemoteTaskIntent::SetArchived { archived: true },
    );
    outbox.push(other, 6).unwrap();
    assert_eq!(outbox.status("other"), Some(OutboxStatus::Ready));
}

/// O5: live controls are fenced when the user acts and never sent later:
/// one still unsent when its connection ends is refused; anything in flight
/// becomes uncertain.
#[test]
fn o5_live_controls_are_never_sent_from_an_offline_outbox() {
    let mut outbox = new_outbox();
    assert!(matches!(
        outbox.push(stop("stop"), 1),
        Err(OutboxError::Invalid(_))
    ));
    let mut steer = send("steer");
    if let RemoteTaskIntent::Send { follow_up, .. } = &mut steer.intent {
        *follow_up = RemoteFollowUp::Steer;
    }
    assert!(matches!(
        outbox.push(steer, 1),
        Err(OutboxError::Invalid(_))
    ));

    outbox.push(send("queued"), 1).unwrap();
    outbox.push_fenced(stop("stop"), 1).unwrap();
    outbox.begin_send("queued", 2).unwrap();
    outbox.connection_ended(3);
    assert_eq!(outbox.status("queued"), Some(OutboxStatus::Uncertain));
    assert_eq!(outbox.status("stop"), Some(OutboxStatus::Refused));
    let entry = outbox.entry("stop").unwrap();
    assert_eq!(entry.sends, 0);
    assert!(
        entry
            .refusal
            .as_deref()
            .is_some_and(|why| why.contains("Try again"))
    );
}

/// O6: only what the host does not own can be discarded.
#[test]
fn o6_discard_never_forgets_a_command_the_host_may_own() {
    let mut outbox = new_outbox();
    let sent = send("sent");
    outbox.push(sent.clone(), 1).unwrap();
    outbox.push(send("unsent"), 1).unwrap();
    outbox.begin_send("sent", 2).unwrap();
    assert!(matches!(outbox.discard("sent"), Err(OutboxError::InUse(_))));
    outbox.send_failed("sent", true, "lost", 3).unwrap();
    assert!(matches!(outbox.discard("sent"), Err(OutboxError::InUse(_))));
    outbox
        .apply_receipt(receipt(&sent, CommandReceiptStatus::Pending), 4)
        .unwrap();
    assert!(matches!(outbox.discard("sent"), Err(OutboxError::InUse(_))));
    outbox
        .apply_receipt(receipt(&sent, CommandReceiptStatus::Applied), 5)
        .unwrap();
    outbox.discard("sent").unwrap();
    outbox.discard("unsent").unwrap();
    assert!(outbox.entries().is_empty());
}

/// O7: fences come from the task's own snapshot, per intent, and freeze
/// before the first send: a retry is always the identical command.
#[test]
fn o7_fences_freeze_per_intent_before_the_first_send() {
    let snapshot = TaskCommandSnapshot {
        task_id: TASK.into(),
        task_revision: 7,
        task_sequence: 9,
        binding_id: Some("binding".into()),
        binding_generation: Some(3),
        active_turn_id: Some("turn-1".into()),
    };
    let mut outbox = new_outbox();
    outbox.push(send("sent"), 1).unwrap();
    let archived = command(
        "archive",
        TASK,
        RemoteTaskIntent::SetArchived { archived: true },
    );
    outbox.push(archived, 1).unwrap();
    let mut other = snapshot.clone();
    other.task_id = "another-task".into();
    assert!(matches!(
        outbox.fence("sent", &other, 2),
        Err(OutboxError::NotThisCommand(_))
    ));
    outbox.fence("sent", &snapshot, 2).unwrap();
    outbox.fence("archive", &snapshot, 2).unwrap();
    let sent = &outbox.entry("sent").unwrap().command;
    assert_eq!(
        (
            sent.expected_task_revision,
            sent.expected_binding_generation,
            sent.expected_turn_id.as_deref()
        ),
        (None, Some(3), None),
        "a send is fenced by its binding only"
    );
    let archive = &outbox.entry("archive").unwrap().command;
    assert_eq!(
        (
            archive.expected_task_revision,
            archive.expected_binding_generation
        ),
        (Some(7), Some(3))
    );
    let frozen = outbox.entry("sent").unwrap().command.clone();
    let mut later = snapshot.clone();
    later.binding_generation = Some(4);
    outbox.fence("sent", &later, 3).unwrap();
    assert_eq!(outbox.entry("sent").unwrap().command, frozen, "frozen");
    let mut stop_snapshot = snapshot;
    stop_snapshot.active_turn_id = Some("turn-2".into());
    let mut control = stop("stop");
    luna_sdk::outbox::apply_fences(&mut control, &stop_snapshot);
    assert_eq!(control.expected_turn_id.as_deref(), Some("turn-2"));
    assert_eq!(control.expected_binding_generation, Some(3));
}

/// O8: bounded. A full outbox refuses more; settled entries are pruned
/// (oldest first) to make room, never one that holds a later entry back.
#[test]
fn o8_the_outbox_is_bounded_and_prunes_settled_entries() {
    let mut outbox = new_outbox();
    for index in 0..MAX_OUTBOX_ENTRIES {
        let task = format!("task-{index}");
        outbox
            .push(
                command(
                    &format!("unsent-{index}"),
                    &task,
                    RemoteTaskIntent::SetArchived { archived: true },
                ),
                1,
            )
            .unwrap();
    }
    assert_eq!(
        outbox.push(send("one-more"), 1).err(),
        Some(OutboxError::Full)
    );
    let mut settled = Outbox::new(key(), actor());
    for index in 0..MAX_OUTBOX_ENTRIES {
        let task = format!("task-{index}");
        let archived = command(
            &format!("settled-{index}"),
            &task,
            RemoteTaskIntent::SetArchived { archived: true },
        );
        answered(&mut settled, archived, CommandReceiptStatus::Applied);
    }
    settled.push(send("room"), 1).unwrap();
    assert!(settled.entries().len() <= MAX_OUTBOX_ENTRIES);
    assert!(
        settled.entry("settled-0").is_none(),
        "the oldest went first"
    );
    assert!(settled.entry("room").is_some());
    assert!(settled.encode().len() <= luna_sdk::outbox::MAX_OUTBOX_BYTES);
    assert_eq!(
        settled.push(send("room"), 1).err(),
        Some(OutboxError::Duplicate("room".into()))
    );
}

/// O9: a queued message's delivery follows only its own dispatch receipt
/// and never regresses from delivered.
#[test]
fn o9_delivery_follows_only_this_messages_dispatch_receipt() {
    let mut outbox = new_outbox();
    let sent = send("sent");
    answered(&mut outbox, sent.clone(), CommandReceiptStatus::Applied);
    let entry = outbox.entry("sent").unwrap();
    let probe = entry
        .delivery_probe()
        .expect("a queued message is followed");
    assert_eq!(probe, format!("queue-dispatch:{TASK}:sent:1"));
    assert_eq!(outbox.deliveries(), vec!["sent".to_owned()]);

    let mut dispatch = receipt(&sent, CommandReceiptStatus::Rejected);
    dispatch.command_id = "queue-dispatch:another-task:sent:1".into();
    assert!(matches!(
        outbox.apply_delivery("sent", Some(&dispatch), 2),
        Err(OutboxError::NotThisCommand(_))
    ));
    outbox.apply_delivery("sent", None, 3).unwrap();
    assert_eq!(
        outbox
            .entry("sent")
            .unwrap()
            .delivery
            .as_ref()
            .map(|d| d.state),
        Some(DeliveryState::Queued)
    );
    dispatch.command_id = probe.clone();
    dispatch.task_id = TASK.into();
    dispatch.reason = Some(CommandReceiptReason {
        code: CommandReasonCode::OperationRejected,
        message: "Worktree setup failed.".into(),
    });
    outbox.apply_delivery("sent", Some(&dispatch), 4).unwrap();
    let held = outbox.entry("sent").unwrap().delivery.clone().unwrap();
    assert_eq!(held.state, DeliveryState::Held);
    assert_eq!(held.detail.as_deref(), Some("Worktree setup failed."));
    assert!(
        outbox.deliveries().is_empty(),
        "a held message is not polled"
    );

    let mut delivered = new_outbox();
    answered(&mut delivered, sent.clone(), CommandReceiptStatus::Applied);
    let mut applied = receipt(&sent, CommandReceiptStatus::Applied);
    applied.command_id = probe;
    delivered.apply_delivery("sent", Some(&applied), 2).unwrap();
    delivered.apply_delivery("sent", None, 3).unwrap();
    assert_eq!(
        delivered
            .entry("sent")
            .unwrap()
            .delivery
            .as_ref()
            .map(|d| d.state),
        Some(DeliveryState::Delivered),
        "never back from delivered"
    );
}

/// O10: an uncertain entry the host never admitted is sent again unchanged,
/// and a definite refusal settles it without a receipt.
#[test]
fn o10_an_unadmitted_entry_is_resent_unchanged_and_a_refusal_settles() {
    let mut outbox = new_outbox();
    outbox.push(send("sent"), 1).unwrap();
    outbox
        .fence(
            "sent",
            &TaskCommandSnapshot {
                task_id: TASK.into(),
                binding_generation: Some(2),
                ..Default::default()
            },
            1,
        )
        .unwrap();
    let first = outbox.begin_send("sent", 2).unwrap();
    outbox.send_failed("sent", true, "lost", 3).unwrap();
    outbox.unadmitted("sent", 4).unwrap();
    assert_eq!(outbox.entry("sent").unwrap().phase, SendPhase::Unsent);
    let again = outbox.begin_send("sent", 5).unwrap();
    assert_eq!(again, first, "the identical command, same id and payload");
    assert_eq!(outbox.entry("sent").unwrap().sends, 2);
    outbox
        .refuse("sent", "remote authorization denied: revoked", 6)
        .unwrap();
    assert_eq!(outbox.status("sent"), Some(OutboxStatus::Refused));
    assert!(outbox.entry("sent").unwrap().receipt.is_none());
}
