//! Proposed migration RPC contract, written before runtime implementation.
//! Deliberately not linked into a Cargo test target until API review is complete.
//! Shared helpers are from workspace_identity_roots.rs when later wired explicitly.
//! The baseline has no PreviewWorkspaceIdentityRepair/repairWorkspaceIdentity API:
//! classify those missing-method failures separately from the root-alias repros.
//! Fixtures seed only disposable offline state; no provider is called.

use super::*;
use luna_proto::{CommandReasonCode, TaskCommandRead, TaskCommandSnapshot};

const LOG: &str = "luna-task-identity-events-v1";
const PREVIEW: &str = "PreviewWorkspaceIdentityRepair";

#[derive(Clone, Copy, Debug)]
enum Seed {
    Unbound,
    BoundSibling,
    ForeignStore,
    NativeIdMismatch,
    MovedRoot,
}

struct Legacy {
    fixture: Fixture,
    task_a: String,
    task_b: String,
    creation_a: CommandReceipt,
    prefix: Vec<Value>,
}

async fn seed_legacy(seed: Seed) -> Legacy {
    let fixture = Fixture::new();
    let core = fixture.open();
    let client = luna_rpc::memory_client(core.rpc_service());
    let task_a = uuid::Uuid::new_v4().to_string();
    let task_b = uuid::Uuid::new_v4().to_string();
    let creation_a = create(&client, &task_a, &core.device_id, None, &fixture.root_a).await;
    if matches!(seed, Seed::ForeignStore | Seed::NativeIdMismatch) {
        core.task_identity
            .record_session_binding(
                &task_a,
                HarnessId::Mock,
                "synthetic-native-a",
                fixture.root_a.to_str().unwrap(),
            )
            .unwrap()
            .expect("fixture binding, no provider execution");
    }
    create(&client, &task_b, &core.device_id, None, &fixture.root_b).await;
    if matches!(seed, Seed::BoundSibling) {
        core.task_identity
            .record_session_binding(
                &task_b,
                HarnessId::Mock,
                "synthetic-native-b",
                fixture.root_b.to_str().unwrap(),
            )
            .unwrap()
            .expect("fixture binding, metadata deliberately unavailable");
    }
    if matches!(seed, Seed::MovedRoot) {
        // A real registry-root change must not be mistaken for shared-row damage.
        core.workspace
            .set_chat_cwd(&task_a, fixture.root_b.to_str().unwrap())
            .unwrap();
    }
    let legacy_id = format!("projectless:{}", core.device_id);
    let store = core.store.clone();
    close(core, client).await;

    // Offline downgrade of our own fresh synthetic fixture. This remains a v1
    // collision fixture even after normal new registration starts allocating v2.
    let mut log: Value =
        serde_json::from_slice(&store.load_snapshot(LOG).unwrap().unwrap()).unwrap();
    log["schemaVersion"] = json!(1);
    for event in log["events"].as_array_mut().unwrap() {
        let field = match event["type"].as_str() {
            Some("workspaceObserved") => "workspace",
            Some("taskRegistered") => "task",
            Some("bindingCreated") => "binding",
            _ => continue,
        };
        event[field]["schemaVersion"] = json!(1);
        event[field]["workspaceId"] = json!(legacy_id);
        if field == "binding" && event[field]["taskId"] == task_a {
            if matches!(seed, Seed::ForeignStore) {
                event[field]["providerInstanceId"] = json!("foreign-synthetic-store");
            }
            if matches!(seed, Seed::NativeIdMismatch) {
                // The binding native ID contradicts its recorded observation.
                event[field]["providerTaskId"] = json!("different-synthetic-native-id");
                event[field]["reconciliation"]["provider"] = json!({
                    "providerTaskId": "synthetic-native-a",
                    "cwd": fixture.root_a, "status": "notLoaded", "ephemeral": false,
                });
            }
        }
    }
    let prefix = log["events"].as_array().unwrap().clone();
    store
        .save_snapshot(LOG, &serde_json::to_vec(&log).unwrap())
        .unwrap();
    drop(store);
    evidence(
        "repair-contract-legacy-seed",
        json!({
            "case": format!("{seed:?}"), "taskA": task_a, "taskB": task_b,
            "rootA": fixture.root_a, "rootB": fixture.root_b, "log": log,
        }),
    );
    Legacy {
        fixture,
        task_a,
        task_b,
        creation_a,
        prefix,
    }
}

async fn snapshot(client: &RpcClient, task: &str) -> TaskCommandSnapshot {
    let read: TaskCommandRead = client
        .call_as(
            methods::GET_TASK_COMMAND_SNAPSHOT,
            json!({ "taskId": task }),
        )
        .await
        .unwrap();
    read.snapshot
}

async fn preview(client: &RpcClient, task: &str) -> Value {
    let result = client
        .call(
            PREVIEW,
            json!({
                "taskId": task, "providerProof": "localOnly",
            }),
        )
        .await;
    evidence(
        "repair-contract-preview",
        json!({
            "taskId": task, "response": result.as_ref().ok(),
            "error": result.as_ref().err().map(ToString::to_string),
            "baselineClassification": "proposed API may be absent; not another root-alias reproduction",
        }),
    );
    result.expect("proposed repair API contract has not been implemented yet")
}

fn envelope(task: &str, snapshot: &TaskCommandSnapshot, intent: Value) -> Value {
    json!({
        "commandId": uuid::Uuid::new_v4().to_string(), "taskId": task,
        "actorId": "repair-fixture", "clientId": "repair-fixture",
        "clientSessionId": "repair-fixture",
        "expectedTaskRevision": snapshot.task_revision,
        "expectedBindingGeneration": snapshot.binding_generation,
        "intent": intent,
    })
}

fn repair_command(task: &str, before: &TaskCommandSnapshot, plan: &Value) -> Value {
    assert_eq!(plan["status"], "ready", "{plan}");
    assert!(plan["previewId"].is_string() && plan["evidenceDigest"].is_string());
    envelope(
        task,
        before,
        json!({
            "kind": "repairWorkspaceIdentity", "previewId": plan["previewId"],
            "evidenceDigest": plan["evidenceDigest"],
        }),
    )
}

async fn execute(client: &RpcClient, command: Value) -> CommandReceipt {
    let result = client.call_as(methods::EXECUTE_TASK_COMMAND, command).await;
    evidence(
        "repair-contract-command",
        json!({
            "response": result.as_ref().ok(), "error": result.as_ref().err().map(ToString::to_string),
        }),
    );
    result.expect("proposed repair command contract")
}

fn log(core: &EngineCore) -> Value {
    serde_json::from_slice(&core.store.load_snapshot(LOG).unwrap().unwrap()).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repair_contract_revision_fence_duplicate_and_restart() {
    let legacy = seed_legacy(Seed::Unbound).await;
    let core = legacy.fixture.open();
    let client = luna_rpc::memory_client(core.rpc_service());
    let before = snapshot(&client, &legacy.task_a).await;
    let stale_rename = envelope(
        &legacy.task_a,
        &before,
        json!({"kind":"rename","title":"must not apply"}),
    );
    let plan = preview(&client, &legacy.task_a).await;
    let command = repair_command(&legacy.task_a, &before, &plan);
    let applied = execute(&client, command.clone()).await;
    let after = snapshot(&client, &legacy.task_a).await;
    let repaired = identity(&client, &legacy.task_a).await;
    let after_log = log(&core);
    let duplicate = execute(&client, command).await;
    let duplicate_log = log(&core);
    let stale = execute(&client, stale_rename).await;
    evidence(
        "repair-contract-revision-replay",
        json!({
            "before": before, "after": after, "plan": plan, "applied": applied,
            "duplicate": duplicate, "stale": stale, "identity": repaired, "log": after_log,
        }),
    );
    assert_eq!(applied.status, CommandReceiptStatus::Applied);
    assert_eq!(after.task_revision, before.task_revision + 1);
    assert_eq!(after.binding_id, before.binding_id);
    assert_eq!(after.binding_generation, before.binding_generation);
    assert_eq!(duplicate, applied);
    assert_eq!(
        duplicate_log, after_log,
        "duplicate must not append an association event"
    );
    assert_eq!(stale.status, CommandReceiptStatus::Conflict);
    assert_eq!(
        stale.reason.unwrap().code,
        CommandReasonCode::StaleTaskRevision
    );
    assert_eq!(
        repaired.workspace.current_path,
        legacy.fixture.root_a.to_string_lossy()
    );
    assert!(repaired.workspace.workspace_id.starts_with("workspace:v2:"));
    assert_eq!(
        &after_log["events"].as_array().unwrap()[..legacy.prefix.len()],
        legacy.prefix.as_slice()
    );
    close(core, client).await;

    let core = legacy.fixture.open();
    let client = luna_rpc::memory_client(core.rpc_service());
    let reopened = identity(&client, &legacy.task_a).await;
    let reopened_snapshot = snapshot(&client, &legacy.task_a).await;
    let creation: Option<CommandReceipt> = client
        .call_as(
            methods::GET_TASK_COMMAND_RECEIPT,
            json!({"commandId":legacy.creation_a.command_id}),
        )
        .await
        .unwrap();
    let repair: Option<CommandReceipt> = client
        .call_as(
            methods::GET_TASK_COMMAND_RECEIPT,
            json!({"commandId":applied.command_id}),
        )
        .await
        .unwrap();
    evidence(
        "repair-contract-reopened",
        json!({"identity":reopened,"snapshot":reopened_snapshot,"creation":creation,"repair":repair}),
    );
    close(core, client).await;
    assert_eq!(reopened.workspace, repaired.workspace);
    assert_eq!(reopened_snapshot.task_revision, after.task_revision);
    assert_eq!(creation.as_ref(), Some(&legacy.creation_a));
    assert_eq!(repair.as_ref(), Some(&applied));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repair_contract_stale_preview_cannot_cross_a_real_root_edit() {
    let legacy = seed_legacy(Seed::Unbound).await;
    let core = legacy.fixture.open();
    let client = luna_rpc::memory_client(core.rpc_service());
    let before = snapshot(&client, &legacy.task_a).await;
    let plan = preview(&client, &legacy.task_a).await;
    let old_repair = repair_command(&legacy.task_a, &before, &plan);
    let moved = execute(
        &client,
        envelope(
            &legacy.task_a,
            &before,
            json!({"kind":"setCwd","cwd":legacy.fixture.root_b}),
        ),
    )
    .await;
    assert_eq!(moved.status, CommandReceiptStatus::Applied);
    let after_move = snapshot(&client, &legacy.task_a).await;
    let untouched_log = log(&core);
    let rejected = execute(&client, old_repair).await;
    let after_rejection = snapshot(&client, &legacy.task_a).await;
    evidence(
        "repair-contract-stale-preview",
        json!({"plan":plan,"move":moved,"rejected":rejected,"before":after_move,"after":after_rejection}),
    );
    assert_eq!(rejected.status, CommandReceiptStatus::Conflict);
    assert_eq!(
        rejected.reason.unwrap().code,
        CommandReasonCode::StaleTaskRevision
    );
    assert_eq!(log(&core), untouched_log);
    assert_eq!(after_rejection.task_revision, after_move.task_revision);
    close(core, client).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repair_contract_unverifiable_bound_sibling_does_not_veto_unbound_repair() {
    let legacy = seed_legacy(Seed::BoundSibling).await;
    let core = legacy.fixture.open();
    let client = luna_rpc::memory_client(core.rpc_service());
    let sibling = identity(&client, &legacy.task_b).await;
    let sibling_snapshot = snapshot(&client, &legacy.task_b).await;
    let blocked = preview(&client, &legacy.task_b).await;
    assert_eq!(blocked["status"], "blocked");
    let before = snapshot(&client, &legacy.task_a).await;
    let ready = preview(&client, &legacy.task_a).await;
    let applied = execute(&client, repair_command(&legacy.task_a, &before, &ready)).await;
    let after_a = identity(&client, &legacy.task_a).await;
    let after_b = identity(&client, &legacy.task_b).await;
    let after_b_snapshot = snapshot(&client, &legacy.task_b).await;
    evidence(
        "repair-contract-independent-task",
        json!({"blocked":blocked,"ready":ready,"receipt":applied,"a":after_a,"bBefore":sibling,"bAfter":after_b}),
    );
    close(core, client).await;
    assert_eq!(applied.status, CommandReceiptStatus::Applied);
    assert_eq!(
        after_a.workspace.current_path,
        legacy.fixture.root_a.to_string_lossy()
    );
    assert_eq!(after_b.current_binding, sibling.current_binding);
    assert_eq!(after_b.task, sibling.task);
    assert_eq!(after_b_snapshot, sibling_snapshot);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repair_contract_foreign_store_native_id_and_moved_root_remain_fenced() {
    for seed in [Seed::ForeignStore, Seed::NativeIdMismatch, Seed::MovedRoot] {
        let legacy = seed_legacy(seed).await;
        let core = legacy.fixture.open();
        let client = luna_rpc::memory_client(core.rpc_service());
        let before_log = log(&core);
        let before = snapshot(&client, &legacy.task_a).await;
        let before_identity = identity(&client, &legacy.task_a).await;
        let plan = preview(&client, &legacy.task_a).await;
        let after = snapshot(&client, &legacy.task_a).await;
        let after_identity = identity(&client, &legacy.task_a).await;
        evidence(
            "repair-contract-refusal",
            json!({"case":format!("{seed:?}"),"plan":plan,"before":before_identity,"after":after_identity}),
        );
        assert_eq!(plan["status"], "blocked", "{seed:?}: {plan}");
        assert!(!plan["blockers"].as_array().unwrap().is_empty());
        assert!(
            plan["previewId"].is_null(),
            "blocked evidence must not mint an applicable preview"
        );
        assert_eq!(after, before);
        assert_eq!(after_identity, before_identity);
        assert_eq!(log(&core), before_log);
        close(core, client).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repair_contract_pending_queued_work_is_preserved_and_blocks_correction() {
    let legacy = seed_legacy(Seed::Unbound).await;
    let core = legacy.fixture.open();
    let client = luna_rpc::memory_client(core.rpc_service());
    core.doc_host
        .fence_queue_for_stop(&legacy.task_a)
        .await
        .unwrap();
    let before_queue = snapshot(&client, &legacy.task_a).await;
    let queued = execute(
        &client,
        envelope(
            &legacy.task_a,
            &before_queue,
            json!({
                "kind":"queueMessage","messageId":uuid::Uuid::new_v4().to_string(),
                "text":"fixture-only held work; never dispatch", "holdForTurnEnd":true,
                "followUpBehavior":"queue",
            }),
        ),
    )
    .await;
    assert_eq!(queued.status, CommandReceiptStatus::Applied);
    let rows = core
        .doc_host
        .open(&legacy.task_a)
        .unwrap()
        .doc()
        .read_queue()
        .unwrap();
    assert_eq!(rows.len(), 1);
    let before = snapshot(&client, &legacy.task_a).await;
    let before_log = log(&core);
    let plan = preview(&client, &legacy.task_a).await;
    let after_rows = core
        .doc_host
        .open(&legacy.task_a)
        .unwrap()
        .doc()
        .read_queue()
        .unwrap();
    let after = snapshot(&client, &legacy.task_a).await;
    evidence(
        "repair-contract-pending-work",
        json!({"plan":plan,"queueBefore":rows,"queueAfter":after_rows,"before":before,"after":after}),
    );
    assert_eq!(plan["status"], "blocked");
    assert_eq!(
        after_rows, rows,
        "refusal may not cancel, release or recreate queued work"
    );
    assert_eq!(after, before);
    assert_eq!(log(&core), before_log);
    close(core, client).await;
}
