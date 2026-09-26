//! Positive control: current-target execution is correct, not a backend defect.
#![cfg(unix)]
use luna_doc::{SessionCommandPayload, SessionCommandStatus};
use luna_engine::{EngineCore, HarnessRegistry};
use luna_proto::{
    AgentEvent, CommandReceipt, CommandReceiptStatus, DoneStatus, HarnessId, ProjectAction,
    ProjectCommandRun, ProjectCommandStatus, RunRequest, TaskCommandRead,
};
use luna_rpc::{RpcClient, methods};
use serde_json::{Value, json};
use std::{path::Path, process::Command, sync::Arc, time::Duration};
fn git(path: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
fn action(command: &str) -> ProjectAction {
    ProjectAction {
        id: "shared-build".into(),
        name: "Shared ID".into(),
        command: command.into(),
        setup_on_worktree_create: false,
        wait_for_setup: true,
        preview_url: None,
    }
}
async fn task_command(client: &RpcClient, id: &str, intent: Value) -> CommandReceipt {
    let read: TaskCommandRead = client
        .call_as(
            methods::GET_TASK_COMMAND_SNAPSHOT,
            json!({"taskId":"context-task"}),
        )
        .await
        .unwrap();
    let mut request = json!({"commandId":id,"actorId":"fixture-user","clientId":"fixture-client","clientSessionId":"fixture-session","taskId":"context-task","intent":intent,"expectedTaskRevision":read.snapshot.task_revision});
    if let Some(generation) = read.snapshot.binding_generation {
        request["expectedBindingGeneration"] = generation.into();
    }
    client
        .call_as(methods::EXECUTE_TASK_COMMAND, request)
        .await
        .unwrap()
}
async fn completed(core: &EngineCore, id: &str) -> ProjectCommandRun {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let run = core
                .project_commands
                .snapshot("context-task")
                .await
                .unwrap()
                .runs
                .into_iter()
                .find(|run| run.id == id)
                .unwrap();
            if !run.status.is_active() {
                return run;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap()
}
#[tokio::test(flavor = "multi_thread")]
async fn supported_same_chat_context_uses_current_command_and_checkout() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let repo = root.join("repo");
    let nested = repo.join("sub");
    std::fs::create_dir_all(&nested).unwrap();
    git(&repo, &["init", "-b", "main"]);
    git(&repo, &["config", "user.name", "Fixture"]);
    git(&repo, &["config", "user.email", "fixture@invalid.test"]);
    std::fs::write(nested.join("README"), "fixture").unwrap();
    git(&repo, &["add", "sub/README"]);
    git(&repo, &["commit", "-m", "fixture"]);
    let previous_worktrees = std::env::var_os("LUNA_WORKTREES_DIR");
    unsafe {
        std::env::set_var("LUNA_WORKTREES_DIR", root.join("worktrees"));
    }
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(luna_harness::mock::MockHarness {
        script: vec![AgentEvent::Done {
            status: DoneStatus::Completed,
            result: None,
            error: None,
            session_id: None,
        }],
    }));
    let core = EngineCore::assemble(
        &root.join("data"),
        Arc::new(registry),
        HarnessId::Mock,
        None,
    )
    .unwrap();
    let client = luna_rpc::memory_client(core.rpc_service());
    core.workspace
        .create_space(
            "source-project",
            &core.device_id,
            nested.to_str().unwrap(),
            None,
            true,
        )
        .unwrap();
    core.workspace
        .create_space(
            "root-project",
            &core.device_id,
            repo.to_str().unwrap(),
            None,
            true,
        )
        .unwrap();
    let a = action("printf source > context-marker.txt; pwd > context-cwd.txt");
    let b = action("printf destination > context-marker.txt; pwd > context-cwd.txt");
    core.project_commands
        .configure("source-project", &[], vec![a.clone()])
        .unwrap();
    core.project_commands
        .configure("root-project", &[], vec![b.clone()])
        .unwrap();
    let created = task_command(
        &client,
        "create-context",
        json!({"kind":"createChat","spaceId":"source-project","cwd":nested}),
    )
    .await;
    assert_eq!(created.status, CommandReceiptStatus::Applied);
    core.workspace
        .rename_chat("context-task", "Context fixture")
        .unwrap();
    let initial = core
        .project_commands
        .run("context-task", "shared-build", "before-move")
        .await
        .unwrap();
    let initial = completed(&core, &initial.id).await;
    assert_eq!(initial.status, ProjectCommandStatus::Succeeded);
    assert_eq!(
        std::fs::read_to_string(nested.join("context-marker.txt")).unwrap(),
        "source"
    );
    let before = core.workspace.chat("context-task").unwrap().unwrap();
    let moved = task_command(
        &client,
        "move-context",
        json!({"kind":"moveProject","destinationSpaceId":"root-project"}),
    )
    .await;
    assert_eq!(
        moved.status,
        CommandReceiptStatus::Applied,
        "{:?}",
        moved.reason
    );
    let after = core.workspace.chat("context-task").unwrap().unwrap();
    assert_eq!(before.id, after.id);
    assert_eq!(before.cwd, after.cwd);
    assert_eq!(after.space_id.as_deref(), Some("root-project"));
    let destination = core
        .project_commands
        .run("context-task", "shared-build", "after-move")
        .await
        .unwrap();
    let destination = completed(&core, &destination.id).await;
    assert_eq!(destination.status, ProjectCommandStatus::Succeeded);
    assert_eq!(destination.action.command, b.command);
    assert_eq!(
        std::fs::read_to_string(nested.join("context-marker.txt")).unwrap(),
        "destination"
    );
    assert!(
        core.project_commands
            .retry("context-task", &initial.id)
            .await
            .is_err()
    );
    assert!(
        core.project_commands
            .stop("context-task", &initial.id)
            .await
            .is_err()
    );
    let request:RunRequest=serde_json::from_value(json!({"prompt":"Materialize the fixture worktree","cwd":nested,"sandbox":"workspace-write","model":null,"reasoning":null,"resume":null,"worktree":{"repoPath":repo,"base":"main"}})).unwrap();
    let command = core
        .doc_host
        .queue_command(
            "context-task",
            SessionCommandPayload::Run {
                request,
                message_id: "materialize-fixture".into(),
            },
        )
        .unwrap();
    let applied = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Some(entry) = core
                .doc_host
                .open("context-task")
                .unwrap()
                .doc()
                .read_commands()
                .unwrap()
                .into_iter()
                .find(|entry| entry.id == command && entry.status != SessionCommandStatus::Pending)
            {
                break entry;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        applied.status,
        SessionCommandStatus::Applied,
        "{:?}",
        applied.resolution
    );
    let materialized = core.workspace.chat("context-task").unwrap().unwrap();
    let cwd = materialized.cwd.clone().unwrap();
    assert_eq!(materialized.id, before.id);
    assert_eq!(materialized.space_id, after.space_id);
    assert_ne!(cwd, nested.to_str().unwrap());
    assert!(Path::new(&cwd).starts_with(root.join("worktrees")));
    let fresh = core
        .project_commands
        .run("context-task", "shared-build", "after-materialization")
        .await
        .unwrap();
    let fresh = completed(&core, &fresh.id).await;
    assert_eq!(fresh.status, ProjectCommandStatus::Succeeded);
    assert_eq!(fresh.cwd, cwd);
    assert_eq!(
        std::fs::read_to_string(Path::new(&cwd).join("context-cwd.txt"))
            .unwrap()
            .trim(),
        cwd
    );
    assert!(
        core.project_commands
            .retry("context-task", &destination.id)
            .await
            .is_err()
    );
    assert!(
        core.project_commands
            .stop("context-task", &destination.id)
            .await
            .is_err()
    );
    if let Ok(directory) = std::env::var("LUNA_COMMAND_QA_ARTIFACTS") {
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(Path::new(&directory).join("same-chat-backend-current-target.json"),serde_json::to_vec_pretty(&json!({"before":before,"moveReceipt":moved,"afterMove":after,"afterMaterialization":materialized,"sourceRun":initial,"destinationRun":destination,"materializedRun":fresh,"backendCurrentTargetIsExpected":true})).unwrap()).unwrap();
    }
    drop(client);
    core.shutdown().await;
    unsafe {
        match previous_worktrees {
            Some(value) => std::env::set_var("LUNA_WORKTREES_DIR", value),
            None => std::env::remove_var("LUNA_WORKTREES_DIR"),
        }
    }
}
