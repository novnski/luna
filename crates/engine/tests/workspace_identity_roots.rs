//! Failure-first EngineCore + memory-RPC coverage for workspace root ownership.
//! Failure cases/design were recorded first in
//! docs/qa/workspace-identity-repair-design-2026-09-26.md.
//! No provider runs, provider metadata clients, real homes or credentials are used.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use luna_engine::{AgentAccountsConfig, EngineCore, EngineProfile, HarnessRegistry};
use luna_proto::{CommandReceipt, CommandReceiptStatus, HarnessId, TaskIdentityView};
use luna_rpc::{RpcClient, methods};
use serde_json::{Value, json};

struct Fixture {
    _temp: tempfile::TempDir,
    data: PathBuf,
    root_a: PathBuf,
    root_b: PathBuf,
    repo: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("disposable identity fixture");
        let base = temp.path().canonicalize().unwrap();
        let root_a = base.join("root-a");
        let root_b = base.join("root-b");
        std::fs::create_dir(&root_a).unwrap();
        std::fs::create_dir(&root_b).unwrap();
        Self {
            data: base.join("data"),
            repo: base.join("repo"),
            root_a,
            root_b,
            _temp: temp,
        }
    }

    fn open(&self) -> EngineCore {
        let accounts = AgentAccountsConfig {
            data_dir: self.data.join("synthetic-provider-state"),
            claude_config_dir: self.data.join("synthetic-claude"),
            claude_config_file: self.data.join("synthetic-claude.json"),
            codex_home: self.data.join("synthetic-codex"),
        };
        accounts.disable_claude_keychain_fallback().unwrap();
        EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
            EngineProfile::local(&self.data).unwrap(),
            Arc::new(HarnessRegistry::new()),
            HarnessId::Mock,
            None,
            accounts,
        )
        .expect("hermetic local engine")
    }

    fn worktrees(&self) {
        std::fs::create_dir(&self.repo).unwrap();
        git(&self.repo, &["init", "-q", "-b", "main"]);
        git(
            &self.repo,
            &["config", "core.hooksPath", "disabled-qa-hooks"],
        );
        std::fs::write(self.repo.join("file.txt"), "fixture\n").unwrap();
        git(&self.repo, &["add", "."]);
        git(
            &self.repo,
            &[
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "-m",
                "fixture",
            ],
        );
        git(
            &self.repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "qa-a",
                self.root_a.to_str().unwrap(),
                "HEAD",
            ],
        );
        git(
            &self.repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "qa-b",
                self.root_b.to_str().unwrap(),
                "HEAD",
            ],
        );
    }
}

fn git(root: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_AUTHOR_NAME", "LUNA Identity Fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "LUNA Identity Fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

async fn create(
    client: &RpcClient,
    task: &str,
    device: &str,
    space: Option<&str>,
    cwd: &Path,
) -> CommandReceipt {
    let mut intent = json!({ "kind": "createChat", "cwd": cwd });
    let space_id = if let Some(space) = space {
        space.to_owned()
    } else {
        let canonical = cwd.canonicalize().unwrap();
        let id = format!(
            "fixture-project-{}",
            canonical.to_string_lossy().replace('/', "_")
        );
        client
            .call(
                methods::MUTATE,
                json!({
                    "op":"createSpace", "spaceId":id, "deviceId":device, "path":canonical,
                }),
            )
            .await
            .expect("create fixture project");
        id
    };
    intent["spaceId"] = json!(space_id);
    let receipt: CommandReceipt = client
        .call_as(
            methods::EXECUTE_TASK_COMMAND,
            json!({
                "commandId": uuid::Uuid::new_v4().to_string(),
                "taskId": task,
                "actorId": "identity-fixture",
                "clientId": "identity-fixture",
                "clientSessionId": "identity-fixture",
                "intent": intent,
            }),
        )
        .await
        .expect("CreateChat RPC");
    assert_eq!(receipt.status, CommandReceiptStatus::Applied, "{receipt:?}");
    receipt
}

async fn identity(client: &RpcClient, task: &str) -> TaskIdentityView {
    let view: Option<TaskIdentityView> = client
        .call_as(methods::GET_TASK_IDENTITY, json!({ "taskId": task }))
        .await
        .expect("GetTaskIdentity RPC");
    view.expect("UUID must enter managed task identity")
}

async fn close(core: EngineCore, client: RpcClient) {
    drop(client);
    assert!(
        core.shutdown().await,
        "engine shutdown must flush the identity snapshot"
    );
    drop(core);
    tokio::task::yield_now().await;
}

fn evidence(case: &str, value: Value) -> PathBuf {
    let directory = std::env::var_os("LUNA_WORKSPACE_IDENTITY_REPORT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/qa/workspace-identity-roots")
        });
    std::fs::create_dir_all(&directory).unwrap();
    let file = directory.join(format!("{case}-{}.json", uuid::Uuid::new_v4()));
    std::fs::write(&file, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
    eprintln!("identity evidence: {}", file.display());
    file
}

fn assert_roots(a: &TaskIdentityView, b: &TaskIdentityView, fixture: &Fixture, artifact: &Path) {
    assert_eq!(
        a.workspace.current_path,
        fixture.root_a.to_string_lossy(),
        "A was retargeted by B; {}",
        artifact.display()
    );
    assert_eq!(
        b.workspace.current_path,
        fixture.root_b.to_string_lossy(),
        "B root; {}",
        artifact.display()
    );
    assert_ne!(
        a.workspace.workspace_id, b.workspace.workspace_id,
        "different roots cannot share one mutable identity"
    );
    assert_eq!(a.task.workspace_id, a.workspace.workspace_id);
    assert_eq!(b.task.workspace_id, b.workspace.workspace_id);
    assert!(
        a.current_binding.is_none() && b.current_binding.is_none(),
        "registration must not start or bind a provider"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn project_uuid_registration_preserves_each_root() {
    let fixture = Fixture::new();
    let core = fixture.open();
    let client = luna_rpc::memory_client(core.rpc_service());
    let task_a = uuid::Uuid::new_v4().to_string();
    let task_b = uuid::Uuid::new_v4().to_string();
    let receipt_a = create(&client, &task_a, &core.device_id, None, &fixture.root_a).await;
    let before_a = identity(&client, &task_a).await;
    assert_eq!(
        before_a.workspace.current_path,
        fixture.root_a.to_string_lossy()
    );
    let receipt_b = create(&client, &task_b, &core.device_id, None, &fixture.root_b).await;
    let after_a = identity(&client, &task_a).await;
    let after_b = identity(&client, &task_b).await;
    let artifact = evidence(
        "project-registration",
        json!({
            "expectedRootA": fixture.root_a, "expectedRootB": fixture.root_b,
            "beforeA": before_a, "afterA": after_a, "afterB": after_b,
            "receiptA": receipt_a, "receiptB": receipt_b,
            "chatA": core.workspace.chat(&task_a).unwrap(), "chatB": core.workspace.chat(&task_b).unwrap(),
        }),
    );
    close(core, client).await;
    assert_roots(&after_a, &after_b, &fixture, &artifact);
    assert_eq!(
        after_a.task, before_a.task,
        "creating B must not change A's identity or lineage"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn project_uuid_roots_and_receipts_survive_restart() {
    let fixture = Fixture::new();
    let core = fixture.open();
    let device = core.device_id.clone();
    let client = luna_rpc::memory_client(core.rpc_service());
    let task_a = uuid::Uuid::new_v4().to_string();
    let task_b = uuid::Uuid::new_v4().to_string();
    let receipt_a = create(&client, &task_a, &device, None, &fixture.root_a).await;
    let receipt_b = create(&client, &task_b, &device, None, &fixture.root_b).await;
    let before_a = identity(&client, &task_a).await;
    let before_b = identity(&client, &task_b).await;
    close(core, client).await;

    let core = fixture.open();
    assert_eq!(core.device_id, device);
    let client = luna_rpc::memory_client(core.rpc_service());
    let after_a = identity(&client, &task_a).await;
    let after_b = identity(&client, &task_b).await;
    let saved_a: Option<CommandReceipt> = client
        .call_as(
            methods::GET_TASK_COMMAND_RECEIPT,
            json!({ "commandId": receipt_a.command_id }),
        )
        .await
        .unwrap();
    let saved_b: Option<CommandReceipt> = client
        .call_as(
            methods::GET_TASK_COMMAND_RECEIPT,
            json!({ "commandId": receipt_b.command_id }),
        )
        .await
        .unwrap();
    let artifact = evidence(
        "project-restart",
        json!({
            "expectedRootA": fixture.root_a, "expectedRootB": fixture.root_b,
            "beforeA": before_a, "beforeB": before_b, "afterA": after_a, "afterB": after_b,
            "originalReceipts": [&receipt_a, &receipt_b], "savedReceipts": [&saved_a, &saved_b],
            "chatA": core.workspace.chat(&task_a).unwrap(), "chatB": core.workspace.chat(&task_b).unwrap(),
        }),
    );
    close(core, client).await;
    assert_eq!(
        saved_a.as_ref(),
        Some(&receipt_a),
        "A creation receipt changed on replay"
    );
    assert_eq!(
        saved_b.as_ref(),
        Some(&receipt_b),
        "B creation receipt changed on replay"
    );
    assert_eq!(after_a.task, before_a.task);
    assert_eq!(after_b.task, before_b.task);
    assert_roots(&after_a, &after_b, &fixture, &artifact);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn project_reused_worktrees_preserve_roots_and_source_space() {
    let fixture = Fixture::new();
    fixture.worktrees();
    let core = fixture.open();
    let client = luna_rpc::memory_client(core.rpc_service());
    let space = uuid::Uuid::new_v4().to_string();
    client
        .call(
            methods::MUTATE,
            json!({
                "op": "createSpace", "spaceId": space, "deviceId": core.device_id,
                "path": fixture.repo, "gitDetected": true,
            }),
        )
        .await
        .expect("create disposable local Git project");
    let task_a = uuid::Uuid::new_v4().to_string();
    let task_b = uuid::Uuid::new_v4().to_string();
    let receipt_a = create(
        &client,
        &task_a,
        &core.device_id,
        Some(&space),
        &fixture.root_a,
    )
    .await;
    let before_a = identity(&client, &task_a).await;
    let receipt_b = create(
        &client,
        &task_b,
        &core.device_id,
        Some(&space),
        &fixture.root_b,
    )
    .await;
    let after_a = identity(&client, &task_a).await;
    let after_b = identity(&client, &task_b).await;
    let artifact = evidence(
        "project-reused-worktrees",
        json!({
            "expectedRootA": fixture.root_a, "expectedRootB": fixture.root_b, "sourceSpace": space,
            "gitWorktrees": git(&fixture.repo, &["worktree", "list", "--porcelain"]),
            "beforeA": before_a, "afterA": after_a, "afterB": after_b,
            "receiptA": receipt_a, "receiptB": receipt_b,
            "chatA": core.workspace.chat(&task_a).unwrap(), "chatB": core.workspace.chat(&task_b).unwrap(),
        }),
    );
    close(core, client).await;
    assert_eq!(
        after_a.workspace.source_space_id.as_deref(),
        Some(space.as_str())
    );
    assert_eq!(
        after_b.workspace.source_space_id.as_deref(),
        Some(space.as_str())
    );
    assert_roots(&after_a, &after_b, &fixture, &artifact);
    assert_eq!(after_a.task, before_a.task);
}
