//! Failure-first real Git/PTY integration. Never runs provider or hosted commands.
#![cfg(unix)]
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use luna_engine::{EngineCore, HarnessRegistry};
use luna_proto::{
    HarnessId, ProjectAction, ProjectCommandRun, ProjectCommandStatus, TerminalEvent,
};
use std::{path::Path, process::Command, sync::Arc, time::Duration};

fn git(cwd: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
fn action(id: &str, command: &str, setup: bool) -> ProjectAction {
    ProjectAction {
        id: id.into(),
        name: id.into(),
        command: command.into(),
        setup_on_worktree_create: setup,
        wait_for_setup: true,
        preview_url: None,
    }
}
async fn completed(core: &EngineCore, chat: &str, id: &str) -> ProjectCommandRun {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let run = core
                .project_commands
                .snapshot(chat)
                .await
                .unwrap()
                .runs
                .into_iter()
                .find(|r| r.id == id)
                .unwrap();
            if !run.status.is_active() {
                return run;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .expect("command completion")
}
async fn output(core: &EngineCore, run: &ProjectCommandRun) -> String {
    let mut rx = core
        .terminals
        .subscribe(run.terminal_id.as_deref().unwrap(), None)
        .unwrap();
    let mut bytes = Vec::new();
    while let Some(event) = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .unwrap()
    {
        match event {
            TerminalEvent::Data { data, .. } => bytes.extend(BASE64.decode(data).unwrap()),
            TerminalEvent::Exit { .. } => break,
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}
#[tokio::test(flavor = "multi_thread")]
async fn checkout_commands_and_setup_lifecycle() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let repo = root.join("repo");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-b", "main"]);
    git(&repo, &["config", "user.name", "QA"]);
    git(&repo, &["config", "user.email", "qa@invalid.test"]);
    std::fs::write(repo.join("README"), "fixture").unwrap();
    git(&repo, &["add", "README"]);
    git(&repo, &["commit", "-m", "fixture"]);
    let a = root.join("checkout A $literal");
    let b = root.join("checkout B");
    git(
        &repo,
        &["worktree", "add", "-b", "qa-a", a.to_str().unwrap()],
    );
    git(
        &repo,
        &["worktree", "add", "-b", "qa-b", b.to_str().unwrap()],
    );
    let data = root.join("data");
    let registry = Arc::new(HarnessRegistry::new());
    let core = EngineCore::assemble(&data, registry.clone(), HarnessId::Mock, None).unwrap();
    core.workspace
        .create_space("p", &core.device_id, repo.to_str().unwrap(), None, true)
        .unwrap();
    for (id, path) in [("a", &a), ("b", &b)] {
        core.workspace
            .create_chat(
                id,
                Some("p"),
                None,
                None,
                Some(path.to_string_lossy().into()),
            )
            .unwrap();
    }
    let actions = vec![
        action(
            "build",
            "pwd; printf 'unique-%s\\n' \"$PWD\" > marker; cat marker",
            false,
        ),
        action("fail", "echo expected-failure; exit 7", false),
        action("stop", "echo running; sleep 60", false),
        action(
            "setup",
            "echo setup >> setup-count; test -f allow-setup",
            true,
        ),
    ];
    let client = luna_rpc::memory_client(core.rpc_service());
    let saved: Vec<ProjectAction> = client
        .call_as(
            luna_rpc::methods::SAVE_PROJECT_COMMANDS,
            serde_json::json!({"projectId":"p","actions":actions}),
        )
        .await
        .unwrap();
    assert_eq!(saved, actions);
    assert!(
        client
            .call(
                luna_rpc::methods::PROJECT_COMMAND_STATUS,
                serde_json::json!({"chatId":"a","cwd":repo})
            )
            .await
            .is_err(),
        "raw paths are not command authority"
    );

    assert!(
        core.project_commands
            .configure("p", vec![action("x", "", false)])
            .is_err()
    );
    assert!(
        core.project_commands
            .run("missing", "build", "invalid")
            .await
            .is_err()
    );
    // Existing worktree navigation does not automatically execute setup.
    assert!(
        core.project_commands
            .snapshot("a")
            .await
            .unwrap()
            .runs
            .is_empty()
    );
    assert!(!a.join("setup-count").exists());
    for chat in ["a", "b"] {
        let run: ProjectCommandRun = client.call_as(luna_rpc::methods::RUN_PROJECT_COMMAND,
            serde_json::json!({"chatId":chat,"actionId":"build","requestId":format!("run-{chat}")})).await.unwrap();
        let done = completed(&core, chat, &run.id).await;
        assert_eq!(done.status, ProjectCommandStatus::Succeeded);
        assert!(output(&core, &done).await.contains(&done.cwd));
        assert_eq!(
            core.project_commands
                .run(chat, "build", &format!("run-{chat}"))
                .await
                .unwrap()
                .id,
            run.id
        );
    }
    assert!(
        std::fs::read_to_string(a.join("marker"))
            .unwrap()
            .contains("checkout A $literal")
    );
    assert!(
        std::fs::read_to_string(b.join("marker"))
            .unwrap()
            .contains("checkout B")
    );
    let failed = core
        .project_commands
        .run("a", "fail", "failure")
        .await
        .unwrap();
    let failed = completed(&core, "a", &failed.id).await;
    assert_eq!(failed.status, ProjectCommandStatus::Failed);
    assert_eq!(failed.exit_code, Some(7));
    assert!(output(&core, &failed).await.contains("expected-failure"));
    let retry = core.project_commands.retry("a", &failed.id).await.unwrap();
    assert_eq!(retry.attempt, 2);
    assert!(
        !core
            .store
            .save_project_command_run(&failed, Some(1), None)
            .unwrap(),
        "stale completion must not replace a retry"
    );
    completed(&core, "a", &retry.id).await;
    // A surviving/reused process group must block retry without signaling it.
    use std::os::unix::process::CommandExt;
    let mut survivor = Command::new("/bin/sleep")
        .arg("60")
        .process_group(0)
        .spawn()
        .unwrap();
    let mut held = core
        .project_commands
        .snapshot("a")
        .await
        .unwrap()
        .runs
        .into_iter()
        .find(|r| r.id == failed.id)
        .unwrap();
    held.process_group_id = Some(survivor.id());
    held.status = ProjectCommandStatus::Canceled;
    assert!(
        core.store
            .save_project_command_run(&held, Some(held.attempt), None)
            .unwrap()
    );
    assert!(
        core.project_commands
            .retry("a", &held.id)
            .await
            .unwrap_err()
            .to_string()
            .contains("process group")
    );
    assert!(
        survivor.try_wait().unwrap().is_none(),
        "retry must never signal a saved PID"
    );
    survivor.kill().unwrap();
    survivor.wait().unwrap();
    let stopped = core
        .project_commands
        .run("a", "stop", "stop")
        .await
        .unwrap();
    core.project_commands.stop("a", &stopped.id).await.unwrap();
    assert_eq!(
        completed(&core, "a", &stopped.id).await.status,
        ProjectCommandStatus::Canceled
    );
    // Fresh materialization seam: duplicate/concurrent calls admit one setup.
    let (s1, s2) = tokio::join!(
        core.project_commands
            .setup_new_worktree("a", a.to_str().unwrap()),
        core.project_commands
            .setup_new_worktree("a", a.to_str().unwrap())
    );
    let s1 = s1.unwrap().unwrap();
    assert_eq!(s1.id, s2.unwrap().unwrap().id);
    assert_eq!(
        completed(&core, "a", &s1.id).await.status,
        ProjectCommandStatus::Failed
    );
    assert!(
        core.project_commands
            .wait_for_setup("a", a.to_str().unwrap())
            .await
            .is_err()
    );
    assert!(
        core.project_commands
            .run("a", "build", "blocked")
            .await
            .is_err()
    );
    std::fs::write(a.join("allow-setup"), "ok").unwrap();
    let success = core.project_commands.retry("a", &s1.id).await.unwrap();
    assert_eq!(
        completed(&core, "a", &success.id).await.status,
        ProjectCommandStatus::Succeeded
    );
    core.project_commands
        .wait_for_setup("a", a.to_str().unwrap())
        .await
        .unwrap();
    core.project_commands
        .setup_new_worktree("a", a.to_str().unwrap())
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(a.join("setup-count"))
            .unwrap()
            .lines()
            .count(),
        2
    );
    let mut changed = actions;
    changed[3].command = "echo must-not-rerun >> setup-count".into();
    core.project_commands.configure("p", changed).unwrap();
    core.project_commands
        .setup_new_worktree("a", a.to_str().unwrap())
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(a.join("setup-count"))
            .unwrap()
            .lines()
            .count(),
        2
    );
    // Stale task cwd refuses retry/stop of its earlier receipt.
    core.workspace
        .set_chat_cwd("a", b.to_str().unwrap())
        .unwrap();
    assert!(core.project_commands.retry("a", &failed.id).await.is_err());
    core.workspace
        .set_chat_cwd("a", a.to_str().unwrap())
        .unwrap();
    let running = core
        .project_commands
        .run("b", "stop", "restart")
        .await
        .unwrap();
    core.terminals.shutdown();
    core.workspace.flush_checked().unwrap();
    drop(client);
    core.shutdown().await;
    drop(core);
    let restored = EngineCore::assemble(&data, registry, HarnessId::Mock, None).unwrap();
    let snapshot = restored.project_commands.snapshot("b").await.unwrap();
    let interrupted = snapshot.runs.iter().find(|r| r.id == running.id).unwrap();
    assert_eq!(interrupted.status, ProjectCommandStatus::Canceled);
    assert_eq!(snapshot.actions.len(), 4);
    let old = restored
        .project_commands
        .snapshot("a")
        .await
        .unwrap()
        .runs
        .into_iter()
        .find(|r| r.id == "run-a")
        .unwrap();
    assert!(
        output(&restored, &old)
            .await
            .contains("checkout A $literal")
    );
    // Normal task creation neither starts a command nor creates a setup receipt.
    restored
        .workspace
        .create_chat("normal", Some("p"), None, None, None)
        .unwrap();
    assert!(
        restored
            .project_commands
            .snapshot("normal")
            .await
            .unwrap()
            .runs
            .is_empty()
    );
    if let Ok(dir) = std::env::var("LUNA_COMMAND_QA_ARTIFACTS") {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            Path::new(&dir).join("receipts.json"),
            serde_json::to_vec_pretty(&snapshot).unwrap(),
        )
        .unwrap();
        std::fs::write(Path::new(&dir).join("result.txt"), "PASS: two checkouts, retained output, failure/retry/stop, setup duplicate/gate, restart, normal task\n").unwrap();
    }
    restored.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn generation_ownership_deletion_and_backup() {
    use luna_engine::backup::{BackupService, IsolatedRoot};
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let repo = root.join("repo");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-b", "main"]);
    git(&repo, &["config", "user.name", "QA"]);
    git(&repo, &["config", "user.email", "qa@invalid.test"]);
    std::fs::write(repo.join("README"), "fixture").unwrap();
    git(&repo, &["add", "README"]);
    git(&repo, &["commit", "-m", "fixture"]);
    let checkout = root.join("recreated");
    git(
        &repo,
        &["worktree", "add", "--detach", checkout.to_str().unwrap()],
    );
    let data = root.join("data");
    let core = EngineCore::assemble(
        &data,
        Arc::new(HarnessRegistry::new()),
        HarnessId::Mock,
        None,
    )
    .unwrap();
    core.workspace
        .create_space("p", &core.device_id, repo.to_str().unwrap(), None, true)
        .unwrap();
    core.workspace
        .create_space(
            "foreign",
            "another-device",
            root.to_str().unwrap(),
            None,
            false,
        )
        .unwrap();
    assert!(
        core.project_commands
            .configure("foreign", vec![action("x", "true", false)])
            .is_err()
    );
    core.workspace
        .create_chat(
            "a",
            Some("p"),
            None,
            None,
            Some(checkout.to_string_lossy().into()),
        )
        .unwrap();
    core.project_commands
        .configure("p", vec![action("setup", "echo prepared", true)])
        .unwrap();
    let first = core
        .project_commands
        .setup_new_worktree("a", checkout.to_str().unwrap())
        .await
        .unwrap()
        .unwrap();
    completed(&core, "a", &first.id).await;
    git(&repo, &["worktree", "remove", checkout.to_str().unwrap()]);
    git(
        &repo,
        &["worktree", "add", "--detach", checkout.to_str().unwrap()],
    );
    let replacement = core
        .project_commands
        .setup_new_worktree("a", checkout.to_str().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_ne!(first.checkout_generation, replacement.checkout_generation);
    assert_ne!(first.id, replacement.id);
    completed(&core, "a", &replacement.id).await;
    core.workspace.flush_checked().unwrap();
    core.terminals.shutdown();
    core.shutdown().await;
    drop(core);
    std::fs::write(
        data.join(".luna-l17-isolated-root"),
        "L17 synthetic or copy-isolated root\n",
    )
    .unwrap();
    let service = BackupService::new_bb_style(IsolatedRoot::open(&data).unwrap());
    let receipt = service.export(&root.join("backups")).unwrap();
    let staged = service.stage_restore(&receipt.archive).unwrap();
    let mut databases = vec![];
    fn find_db(path: &Path, found: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                find_db(&path, found);
            } else if path.file_name().unwrap() == "docs.sqlite3" {
                found.push(path);
            }
        }
    }
    find_db(&staged.path, &mut databases);
    assert!(databases.iter().any(|db| {
        let conn = rusqlite::Connection::open(db).unwrap();
        conn.query_row(
            "SELECT count(*) FROM project_command_configs WHERE project_id='p'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap()
            == 1
    }));
    let reopened = EngineCore::assemble(
        &data,
        Arc::new(HarnessRegistry::new()),
        HarnessId::Mock,
        None,
    )
    .unwrap();
    reopened.store.retire_project_local_records("p").unwrap();
    assert!(
        reopened
            .project_commands
            .configuration("p")
            .unwrap()
            .is_empty()
    );
    reopened.workspace.delete_space("p").unwrap();
    assert!(
        reopened
            .project_commands
            .configure("p", vec![action("x", "true", false)])
            .is_err()
    );
    reopened.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn durable_task_run_waits_for_setup_and_records_failure() {
    use luna_doc::{SessionCommandPayload, SessionCommandStatus};
    use luna_proto::RunRequest;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let repo = root.join("repo");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-b", "main"]);
    git(&repo, &["config", "user.name", "QA"]);
    git(&repo, &["config", "user.email", "qa@invalid.test"]);
    std::fs::write(repo.join("README"), "fixture").unwrap();
    git(&repo, &["add", "README"]);
    git(&repo, &["commit", "-m", "fixture"]);
    let worktrees = root.join("worktrees");
    unsafe {
        std::env::set_var("LUNA_WORKTREES_DIR", &worktrees);
    }
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(luna_harness::mock::MockHarness {
        script: vec![luna_proto::AgentEvent::Done {
            status: luna_proto::DoneStatus::Completed,
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
    core.workspace
        .create_space("p", &core.device_id, repo.to_str().unwrap(), None, true)
        .unwrap();
    core.workspace
        .create_chat("queued", Some("p"), None, None, None)
        .unwrap();
    core.workspace
        .rename_chat("queued", "Setup gate QA")
        .unwrap();
    core.project_commands
        .configure(
            "p",
            vec![action(
                "setup",
                "echo attempted >> setup-attempts; test -f allow-setup",
                true,
            )],
        )
        .unwrap();
    let payload = |message: &str| {
        let request: RunRequest = serde_json::from_value(serde_json::json!({"prompt":"fixture", "cwd":repo, "sandbox":"workspaceWrite", "model":null, "reasoning":null, "resume":null, "worktree":{"repoPath":repo,"base":"main"}})).unwrap();
        SessionCommandPayload::Run {
            request,
            message_id: message.into(),
        }
    };
    let first = core
        .doc_host
        .queue_command("queued", payload("first"))
        .unwrap();
    async fn receipt(core: &EngineCore, id: &str) -> luna_doc::SessionCommandEntry {
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                if let Some(entry) = core
                    .doc_host
                    .open("queued")
                    .unwrap()
                    .doc()
                    .read_commands()
                    .unwrap()
                    .into_iter()
                    .find(|c| c.id == id && c.status != SessionCommandStatus::Pending)
                {
                    return entry;
                }
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
        })
        .await
        .unwrap()
    }
    let rejected = receipt(&core, &first).await;
    assert_eq!(rejected.status, SessionCommandStatus::Rejected);
    assert!(rejected.resolution.unwrap().contains("setup"));
    let view = core.project_commands.snapshot("queued").await.unwrap();
    let setup = view.runs.into_iter().find(|r| r.setup).unwrap();
    assert_eq!(setup.status, ProjectCommandStatus::Failed);
    let cwd = core.workspace.chat("queued").unwrap().unwrap().cwd.unwrap();
    assert!(Path::new(&cwd).starts_with(&worktrees));
    std::fs::write(Path::new(&cwd).join("allow-setup"), "ok").unwrap();
    let retried = core
        .project_commands
        .retry("queued", &setup.id)
        .await
        .unwrap();
    assert_eq!(
        completed(&core, "queued", &retried.id).await.status,
        ProjectCommandStatus::Succeeded
    );
    let second = core
        .doc_host
        .queue_command("queued", payload("second"))
        .unwrap();
    assert_eq!(
        receipt(&core, &second).await.status,
        SessionCommandStatus::Applied
    );
    assert_eq!(
        std::fs::read_to_string(Path::new(&cwd).join("setup-attempts"))
            .unwrap()
            .lines()
            .count(),
        2
    );
    assert_eq!(
        core.workspace
            .chat("queued")
            .unwrap()
            .unwrap()
            .cwd
            .as_deref(),
        Some(cwd.as_str())
    );
    core.shutdown().await;
    unsafe {
        std::env::remove_var("LUNA_WORKTREES_DIR");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn pending_setup_survives_both_checkout_publication_boundaries() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let repo = root.join("repo");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-b", "main"]);
    git(&repo, &["config", "user.name", "QA"]);
    git(&repo, &["config", "user.email", "qa@invalid.test"]);
    std::fs::write(repo.join("README"), "fixture").unwrap();
    git(&repo, &["add", "README"]);
    git(&repo, &["commit", "-m", "fixture"]);
    let a = root.join("before-publication");
    let b = root.join("after-publication");
    git(&repo, &["worktree", "add", "--detach", a.to_str().unwrap()]);
    git(&repo, &["worktree", "add", "--detach", b.to_str().unwrap()]);
    let data = root.join("data");
    let registry = Arc::new(HarnessRegistry::new());
    let core = EngineCore::assemble(&data, registry.clone(), HarnessId::Mock, None).unwrap();
    core.workspace
        .create_space("p", &core.device_id, repo.to_str().unwrap(), None, true)
        .unwrap();
    for id in ["before", "after"] {
        core.workspace
            .create_chat(id, Some("p"), None, None, None)
            .unwrap();
    }
    core.project_commands
        .configure(
            "p",
            vec![action("setup", "echo launched >> launches", true)],
        )
        .unwrap();
    core.project_commands
        .prepare_new_worktree("before", a.to_str().unwrap())
        .await
        .unwrap();
    core.project_commands
        .prepare_new_worktree("after", b.to_str().unwrap())
        .await
        .unwrap();
    let reserved = core.store.project_command_runs(&core.device_id).unwrap();
    assert_eq!(reserved.len(), 2);
    assert!(
        reserved
            .iter()
            .all(|r| r.status == ProjectCommandStatus::Pending && r.terminal_id.is_none())
    );
    // Mirror the existing materialization receipt without releasing the handshake.
    core.workspace
        .set_chat_cwd("after", b.to_str().unwrap())
        .unwrap();
    core.workspace.flush_checked().unwrap();
    core.shutdown().await;
    drop(core);
    let restarted = EngineCore::assemble(&data, registry, HarnessId::Mock, None).unwrap();
    assert!(!a.join("launches").exists() && !b.join("launches").exists());
    assert_eq!(
        restarted
            .workspace
            .chat("before")
            .unwrap()
            .unwrap()
            .cwd
            .as_deref(),
        repo.to_str()
    );
    assert_eq!(
        restarted
            .workspace
            .chat("after")
            .unwrap()
            .unwrap()
            .cwd
            .as_deref(),
        b.to_str()
    );
    let canceled = restarted
        .project_commands
        .snapshot("after")
        .await
        .unwrap()
        .runs
        .into_iter()
        .find(|r| r.setup)
        .unwrap();
    assert_eq!(canceled.status, ProjectCommandStatus::Canceled);
    assert!(
        restarted
            .project_commands
            .wait_for_setup("after", b.to_str().unwrap())
            .await
            .is_err()
    );
    // Even a duplicate materialization notification may not replay canceled setup.
    assert_eq!(
        restarted
            .project_commands
            .setup_new_worktree("after", b.to_str().unwrap())
            .await
            .unwrap()
            .unwrap()
            .status,
        ProjectCommandStatus::Canceled
    );
    assert!(!b.join("launches").exists());
    let retry = restarted
        .project_commands
        .retry("after", &canceled.id)
        .await
        .unwrap();
    assert_eq!(
        completed(&restarted, "after", &retry.id).await.status,
        ProjectCommandStatus::Succeeded
    );
    assert_eq!(
        std::fs::read_to_string(b.join("launches"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    if let Ok(dir) = std::env::var("LUNA_COMMAND_QA_ARTIFACTS") {
        std::fs::write(
            Path::new(&dir).join("publication-boundaries.json"),
            serde_json::to_vec_pretty(&reserved).unwrap(),
        )
        .unwrap();
    }
    restarted.shutdown().await;
}
