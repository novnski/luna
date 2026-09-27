//! E2E: an existing task keeps its creation workspace. Legacy directory
//! mutations remain decodable but cannot relocate that task.
#![cfg(unix)]

use std::{fs, path::Path, process::Command, sync::Arc};

use luna_engine::{EngineCore, HarnessRegistry};
use luna_proto::{CommandReceipt, CommandReceiptStatus, HarnessId, TaskCommandRead};
use luna_rpc::methods;
use serde_json::json;

const TASK_A: &str = "60000000-0000-4000-8000-000000000001";
const TASK_B: &str = "60000000-0000-4000-8000-000000000002";

fn init_checkout(path: &Path) {
    fs::create_dir_all(path).unwrap();
    let output = Command::new("git")
        .args(["init", "-q"])
        .current_dir(path)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn existing_task_directory_is_immutable_across_direct_legacy_and_restart_paths() {
    let fixture = tempfile::tempdir().unwrap();
    let a = fixture.path().join("workspace-a");
    let b = fixture.path().join("workspace-b");
    init_checkout(&a);
    init_checkout(&b);
    let data = fixture.path().join("engine-data");
    let core = EngineCore::assemble(
        &data,
        Arc::new(HarnessRegistry::new()),
        HarnessId::Mock,
        None,
    )
    .unwrap();
    let client = luna_rpc::memory_client(core.rpc_service());
    for (space, task, path) in [("space-a", TASK_A, &a), ("space-b", TASK_B, &b)] {
        client
            .call(
                methods::MUTATE,
                json!({"op":"createSpace", "spaceId":space,
                    "deviceId":core.device_id, "path":path}),
            )
            .await
            .unwrap();
        client
            .call(
                methods::MUTATE,
                json!({"op":"createChat", "chatId":task,
                    "spaceId":space, "cwd":path}),
            )
            .await
            .unwrap();
    }
    let checkout_a = core.repos.checkout_identity(&a).await.unwrap().id;
    core.workspace
        .set_chat_checkout(TASK_A, &checkout_a)
        .unwrap();
    let before = core.workspace.chat(TASK_A).unwrap().unwrap();
    let before_snapshot: TaskCommandRead = client
        .call_as(methods::GET_TASK_COMMAND_SNAPSHOT, json!({"taskId":TASK_A}))
        .await
        .unwrap();
    let command = |id: &str, cwd: &Path| {
        json!({"commandId":id, "taskId":TASK_A,
            "expectedTaskRevision":before_snapshot.snapshot.task_revision,
            "expectedBindingGeneration":before_snapshot.snapshot.binding_generation,
            "intent":{"kind":"setCwd", "cwd":cwd}})
    };
    let moved: CommandReceipt = client
        .call_as(methods::EXECUTE_TASK_COMMAND, command("move-to-b", &b))
        .await
        .unwrap();
    let same: CommandReceipt = client
        .call_as(methods::EXECUTE_TASK_COMMAND, command("select-a-again", &a))
        .await
        .unwrap();
    let legacy = client
        .call(
            methods::MUTATE,
            json!({"op":"setChatCwd", "chatId":TASK_A, "cwd":b}),
        )
        .await
        .unwrap();
    let after = core.workspace.chat(TASK_A).unwrap().unwrap();
    let after_snapshot: TaskCommandRead = client
        .call_as(methods::GET_TASK_COMMAND_SNAPSHOT, json!({"taskId":TASK_A}))
        .await
        .unwrap();
    let new_task = core.workspace.chat(TASK_B).unwrap().unwrap();
    if let Some(dir) = std::env::var_os("LUNA_QA_REPORT_DIR") {
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            Path::new(&dir).join("task-cwd-immutability.json"),
            serde_json::to_vec_pretty(&json!({
                "before":before,"after":after,"moved":moved,"same":same,
                "legacy":legacy,"beforeSnapshot":before_snapshot,
                "afterSnapshot":after_snapshot,"newTask":new_task,
            }))
            .unwrap(),
        )
        .unwrap();
    }
    assert_eq!(moved.status, CommandReceiptStatus::Rejected);
    assert_eq!(same.status, CommandReceiptStatus::Rejected);
    assert_eq!(legacy["receipt"]["status"], "rejected");
    assert_eq!(after.cwd, before.cwd);
    assert_eq!(after.checkout_id, before.checkout_id);
    assert_eq!(
        after_snapshot.snapshot.task_revision,
        before_snapshot.snapshot.task_revision
    );
    assert_eq!(new_task.cwd.as_deref(), b.to_str());
    drop(client);
    core.shutdown().await;
    drop(core);
    let reopened = EngineCore::assemble(
        &data,
        Arc::new(HarnessRegistry::new()),
        HarnessId::Mock,
        None,
    )
    .unwrap();
    let persisted = reopened.workspace.chat(TASK_A).unwrap().unwrap();
    assert_eq!(persisted.cwd, before.cwd);
    assert_eq!(persisted.checkout_id, before.checkout_id);
    reopened.shutdown().await;
}
