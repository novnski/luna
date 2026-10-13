//! AW-11 failure-first acceptance over real Git checkouts, the real engine,
//! its catalogued RPC methods and real PTY commands. A checked-in
//! `.luna/project.toml` sits UNDER this device's commands, is untrusted input,
//! and never runs before this device accepts its exact content; any change
//! makes it unaccepted again. Refused files never break the project. Every
//! command here only writes a marker file in its temporary checkout.
#![cfg(unix)]
use luna_engine::{EngineCore, HarnessRegistry};
use luna_proto::{
    HarnessId, ProjectAction, ProjectCommandRun, ProjectCommandSource, ProjectCommandStatus,
    ProjectCommandTrust, ProjectCommandsSnapshot, ProjectConfigState, ProjectRepositoryConfig,
};
use luna_rpc::methods;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    time::Duration,
};

fn evidence(case: &str, value: &Value) {
    let root = std::env::var_os("LUNA_PROJECT_CONFIG_QA_ARTIFACTS")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("luna-project-config-evidence"));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join(format!("{case}.json"));
    std::fs::write(&path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
    eprintln!("project config evidence: {}", path.display());
}
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
fn repository(root: &Path) -> PathBuf {
    let repo = root.join("repo");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-b", "main"]);
    git(&repo, &["config", "user.name", "QA"]);
    git(&repo, &["config", "user.email", "qa@invalid.test"]);
    std::fs::write(repo.join("README"), "fixture").unwrap();
    git(&repo, &["add", "README"]);
    git(&repo, &["commit", "-m", "fixture"]);
    repo
}
fn write_config(checkout: &Path, text: &str) {
    std::fs::create_dir_all(checkout.join(".luna")).unwrap();
    std::fs::write(checkout.join(".luna/project.toml"), text).unwrap();
}
fn personal(id: &str, command: &str) -> ProjectAction {
    ProjectAction {
        id: id.into(),
        name: id.into(),
        command: command.into(),
        setup_on_worktree_create: false,
        wait_for_setup: true,
        preview_url: None,
    }
}
fn ids(actions: &[ProjectAction]) -> Vec<&str> {
    actions.iter().map(|action| action.id.as_str()).collect()
}
fn trust_of(config: &ProjectRepositoryConfig, id: &str) -> (ProjectCommandTrust, bool) {
    let command = config
        .commands
        .iter()
        .find(|command| command.action.id == id)
        .unwrap_or_else(|| panic!("{id} is declared"));
    (command.trust, command.overridden)
}
fn digest_of(config: &ProjectRepositoryConfig, id: &str) -> String {
    config
        .commands
        .iter()
        .find(|command| command.action.id == id)
        .unwrap()
        .digest
        .clone()
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

const SHARED: &str = r#"version = 1

[[commands]]
id = "build"
name = "Build"
command = "printf repo-build > ran-build"

[[commands]]
id = "lint"
name = "Lint"
command = "printf repo-lint > ran-repo-lint"

[[commands]]
id = "web"
name = "Web"
command = """
printf line-one > ran-web
printf line-two >> ran-web
"""
preview-url = "http://localhost:5173"
"#;

#[tokio::test(flavor = "multi_thread")]
async fn repository_layer_precedence_trust_and_no_autoexecution() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let repo = repository(&root);
    write_config(&repo, SHARED);
    git(&repo, &["add", ".luna/project.toml"]);
    git(&repo, &["commit", "-m", "shared commands"]);
    let data = root.join("data");
    let registry = Arc::new(HarnessRegistry::new());
    let core = EngineCore::assemble(&data, registry.clone(), HarnessId::Mock, None).unwrap();
    core.workspace
        .create_space("p", &core.device_id, repo.to_str().unwrap(), None, true)
        .unwrap();
    core.workspace
        .create_chat(
            "a",
            Some("p"),
            None,
            None,
            Some(repo.to_string_lossy().into()),
        )
        .unwrap();
    let client = luna_rpc::memory_client(core.rpc_service());
    // This device's own commands; `lint` overrides the repository's `lint`.
    let own = vec![
        personal("lint", "printf personal-lint > ran-personal-lint"),
        personal("notes", "printf notes > ran-notes"),
    ];
    let _: Vec<ProjectAction> = client
        .call_as(
            methods::SAVE_PROJECT_COMMANDS,
            json!({"projectId":"p","expectedActions":[],"actions":own}),
        )
        .await
        .unwrap();

    // Discovery shows provenance and trust; nothing becomes runnable or runs.
    let discovered: ProjectCommandsSnapshot = client
        .call_as(methods::PROJECT_COMMAND_STATUS, json!({"chatId":"a"}))
        .await
        .unwrap();
    let layer = discovered.repository.clone().expect("repository layer");
    assert_eq!(layer.state, ProjectConfigState::Loaded);
    assert_eq!(layer.file, ".luna/project.toml");
    assert_eq!(
        trust_of(&layer, "build"),
        (ProjectCommandTrust::Unaccepted, false)
    );
    assert_eq!(
        trust_of(&layer, "lint"),
        (ProjectCommandTrust::Unaccepted, true)
    );
    assert_eq!(
        ids(&discovered.actions),
        ["lint", "notes"],
        "only this device's commands are runnable before acceptance"
    );
    assert!(discovered.runs.is_empty());
    let folder_view: ProjectRepositoryConfig = client
        .call_as(methods::GET_PROJECT_CONFIGURATION, json!({"projectId":"p"}))
        .await
        .unwrap();
    assert_eq!(
        folder_view.commands, layer.commands,
        "Settings reads the same layer from the project folder"
    );

    // An unaccepted repository command is refused and leaves no receipt.
    let unaccepted = client
        .call(
            methods::RUN_PROJECT_COMMAND,
            json!({"chatId":"a","actionId":"build","requestId":"build-unaccepted"}),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(unaccepted.contains("not accepted"), "{unaccepted}");
    // Precedence: this device's `lint` runs, never the repository's.
    let lint: ProjectCommandRun = client
        .call_as(
            methods::RUN_PROJECT_COMMAND,
            json!({"chatId":"a","actionId":"lint","requestId":"lint"}),
        )
        .await
        .unwrap();
    let lint = completed(&core, "a", &lint.id).await;
    assert_eq!(lint.status, ProjectCommandStatus::Succeeded);
    assert_eq!(lint.source, ProjectCommandSource::Personal);
    assert!(repo.join("ran-personal-lint").exists());
    assert!(!repo.join("ran-repo-lint").exists());
    assert!(!repo.join("ran-build").exists());

    // Acceptance pins the reviewed digest; a stale one is refused.
    let stale = client
        .call(
            methods::SET_PROJECT_COMMAND_TRUST,
            json!({"projectId":"p","chatId":"a","actionId":"build","digest":"0".repeat(64),"trusted":true}),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(stale.contains("changed after you reviewed it"), "{stale}");
    let reviewed = digest_of(&layer, "build");
    let accepted: ProjectRepositoryConfig = client
        .call_as(
            methods::SET_PROJECT_COMMAND_TRUST,
            json!({"projectId":"p","chatId":"a","actionId":"build","digest":reviewed,"trusted":true}),
        )
        .await
        .unwrap();
    assert_eq!(
        trust_of(&accepted, "build"),
        (ProjectCommandTrust::Accepted, false)
    );
    // Accepting is a trust decision, never a run.
    let after_accept = core.project_commands.snapshot("a").await.unwrap();
    assert_eq!(ids(&after_accept.actions), ["lint", "notes", "build"]);
    assert!(after_accept.runs.iter().all(|run| run.action.id != "build"));
    assert!(!repo.join("ran-build").exists());
    let run: ProjectCommandRun = client
        .call_as(
            methods::RUN_PROJECT_COMMAND,
            json!({"chatId":"a","actionId":"build","requestId":"build-accepted"}),
        )
        .await
        .unwrap();
    let built = completed(&core, "a", &run.id).await;
    assert_eq!(built.status, ProjectCommandStatus::Succeeded);
    assert_eq!(built.source, ProjectCommandSource::Repository);
    assert_eq!(
        built.action_digest, reviewed,
        "the receipt captures exactly the accepted content"
    );
    assert_eq!(
        std::fs::read_to_string(repo.join("ran-build")).unwrap(),
        "repo-build"
    );

    // Scope: each checkout reads its own file. A worktree whose branch
    // changed `build` sees it as changed while the main checkout stays
    // accepted, because trust pins content, not the id.
    let worktree = root.join("feature checkout");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-b",
            "qa-feature",
            worktree.to_str().unwrap(),
        ],
    );
    write_config(
        &worktree,
        &SHARED.replace(
            "printf repo-build > ran-build",
            "printf feature > ran-feature",
        ),
    );
    core.workspace
        .create_chat(
            "b",
            Some("p"),
            None,
            None,
            Some(worktree.to_string_lossy().into()),
        )
        .unwrap();
    let feature = core.project_commands.snapshot("b").await.unwrap();
    assert_eq!(
        trust_of(feature.repository.as_ref().unwrap(), "build"),
        (ProjectCommandTrust::Changed, false)
    );
    assert_eq!(ids(&feature.actions), ["lint", "notes"]);
    assert_eq!(
        trust_of(
            core.project_commands
                .snapshot("a")
                .await
                .unwrap()
                .repository
                .as_ref()
                .unwrap(),
            "build"
        ),
        (ProjectCommandTrust::Accepted, false)
    );
    assert!(
        core.project_commands
            .run("b", "build", "feature-build")
            .await
            .unwrap_err()
            .to_string()
            .contains("changed")
    );
    assert!(!worktree.join("ran-feature").exists());

    // A pulled change to the accepted command makes it unaccepted again:
    // Run and Retry are refused, and nothing executes.
    std::fs::write(
        repo.join(".luna/project.toml"),
        SHARED.replace(
            "printf repo-build > ran-build",
            "printf changed > ran-changed",
        ),
    )
    .unwrap();
    let changed = core.project_commands.snapshot("a").await.unwrap();
    let changed_layer = changed.repository.clone().unwrap();
    assert_eq!(
        trust_of(&changed_layer, "build"),
        (ProjectCommandTrust::Changed, false)
    );
    assert_eq!(ids(&changed.actions), ["lint", "notes"]);
    let rerun = client
        .call(
            methods::RUN_PROJECT_COMMAND,
            json!({"chatId":"a","actionId":"build","requestId":"build-changed"}),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(rerun.contains("changed"), "{rerun}");
    let retry = client
        .call(
            methods::RETRY_PROJECT_COMMAND,
            json!({"chatId":"a","runId":built.id}),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(retry.contains("since it ran"), "{retry}");
    assert!(!repo.join("ran-changed").exists());

    // Accept the new content, then revoke it from the project folder view:
    // a revoked command is unaccepted again.
    let reaccepted: ProjectRepositoryConfig = client
        .call_as(
            methods::SET_PROJECT_COMMAND_TRUST,
            json!({"projectId":"p","chatId":"a","actionId":"build","digest":digest_of(&changed_layer, "build"),"trusted":true}),
        )
        .await
        .unwrap();
    assert_eq!(
        trust_of(&reaccepted, "build"),
        (ProjectCommandTrust::Accepted, false)
    );
    let revoked: ProjectRepositoryConfig = client
        .call_as(
            methods::SET_PROJECT_COMMAND_TRUST,
            json!({"projectId":"p","actionId":"build","trusted":false}),
        )
        .await
        .unwrap();
    assert_eq!(
        trust_of(&revoked, "build"),
        (ProjectCommandTrust::Unaccepted, false)
    );
    // Removing the personal override returns the repository `lint` in its
    // own, unaccepted state; it still does not run.
    core.project_commands
        .configure("p", &own, vec![own[1].clone()])
        .unwrap();
    let restored = core.project_commands.snapshot("a").await.unwrap();
    assert_eq!(
        trust_of(restored.repository.as_ref().unwrap(), "lint"),
        (ProjectCommandTrust::Unaccepted, false)
    );
    assert_eq!(ids(&restored.actions), ["notes"]);

    // Trust is durable, device-local state; a restart never runs anything.
    let web: ProjectRepositoryConfig = client
        .call_as(
            methods::SET_PROJECT_COMMAND_TRUST,
            json!({"projectId":"p","actionId":"web","digest":digest_of(restored.repository.as_ref().unwrap(), "web"),"trusted":true}),
        )
        .await
        .unwrap();
    assert_eq!(
        trust_of(&web, "web"),
        (ProjectCommandTrust::Accepted, false)
    );
    core.terminals.shutdown();
    core.workspace.flush();
    drop(client);
    core.shutdown().await;
    drop(core);
    let reopened = EngineCore::assemble(&data, registry, HarnessId::Mock, None).unwrap();
    let after_restart = reopened.project_commands.snapshot("a").await.unwrap();
    let restart_layer = after_restart.repository.clone().unwrap();
    assert_eq!(
        trust_of(&restart_layer, "web"),
        (ProjectCommandTrust::Accepted, false)
    );
    assert_eq!(
        trust_of(&restart_layer, "build"),
        (ProjectCommandTrust::Unaccepted, false)
    );
    assert_eq!(ids(&after_restart.actions), ["notes", "web"]);
    let web = &after_restart.actions[1];
    assert_eq!(
        web.command,
        "printf line-one > ran-web\nprintf line-two >> ran-web\n"
    );
    assert_eq!(web.preview_url.as_deref(), Some("http://localhost:5173"));
    assert!(!web.setup_on_worktree_create);
    assert!(!repo.join("ran-web").exists());
    assert!(!repo.join("ran-repo-lint").exists());
    evidence(
        "precedence-trust-scope",
        &json!({
            "discovered": discovered,
            "folderView": folder_view,
            "refusedUnaccepted": unaccepted,
            "refusedStaleDigest": stale,
            "accepted": accepted,
            "personalReceipt": lint,
            "repositoryReceipt": built,
            "featureCheckout": feature,
            "changed": changed,
            "refusedChangedRun": rerun,
            "refusedChangedRetry": retry,
            "revoked": revoked,
            "overrideRemoved": restored,
            "afterRestart": after_restart,
            "markers": {
                "ran-build": repo.join("ran-build").exists(),
                "ran-personal-lint": repo.join("ran-personal-lint").exists(),
                "ran-repo-lint": repo.join("ran-repo-lint").exists(),
                "ran-changed": repo.join("ran-changed").exists(),
                "ran-web": repo.join("ran-web").exists(),
                "ran-feature": worktree.join("ran-feature").exists(),
            },
        }),
    );
    reopened.shutdown().await;
}

enum Fixture {
    Absent,
    LunaIsAFile,
    Text(String),
    Bytes(Vec<u8>),
    LinkToFileOutside,
    LinkLunaDirOutside,
    LinkInside,
    Fifo,
}

fn clear(repo: &Path) {
    let luna = repo.join(".luna");
    match std::fs::symlink_metadata(&luna) {
        Ok(metadata) if metadata.is_dir() => std::fs::remove_dir_all(&luna).unwrap(),
        Ok(_) => std::fs::remove_file(&luna).unwrap(),
        Err(_) => {}
    }
}

fn install(repo: &Path, outside: &Path, fixture: &Fixture) {
    clear(repo);
    let file = repo.join(".luna/project.toml");
    match fixture {
        Fixture::Absent => {}
        Fixture::LunaIsAFile => std::fs::write(repo.join(".luna"), "not a directory").unwrap(),
        Fixture::Text(text) => write_config(repo, text),
        Fixture::Bytes(bytes) => {
            std::fs::create_dir_all(repo.join(".luna")).unwrap();
            std::fs::write(&file, bytes).unwrap();
        }
        Fixture::LinkToFileOutside => {
            std::fs::create_dir_all(repo.join(".luna")).unwrap();
            std::os::unix::fs::symlink(outside.join("project.toml"), &file).unwrap();
        }
        Fixture::LinkLunaDirOutside => {
            std::os::unix::fs::symlink(outside, repo.join(".luna")).unwrap();
        }
        Fixture::LinkInside => {
            std::fs::create_dir_all(repo.join(".luna")).unwrap();
            std::fs::create_dir_all(repo.join("shared")).unwrap();
            std::fs::write(repo.join("shared/luna.toml"), SHARED).unwrap();
            std::os::unix::fs::symlink("../shared/luna.toml", &file).unwrap();
        }
        Fixture::Fifo => {
            std::fs::create_dir_all(repo.join(".luna")).unwrap();
            let made = Command::new("mkfifo").arg(&file).status().unwrap();
            assert!(made.success());
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn untrusted_files_are_refused_without_breaking_the_project() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let repo = repository(&root);
    // A valid file outside the checkout that the repository must not be able
    // to point LUNA at.
    let outside = root.join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("project.toml"), SHARED).unwrap();
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
        .create_chat(
            "a",
            Some("p"),
            None,
            None,
            Some(repo.to_string_lossy().into()),
        )
        .unwrap();
    let client = luna_rpc::memory_client(core.rpc_service());
    core.project_commands
        .configure(
            "p",
            &[],
            vec![personal("notes", "printf notes >> ran-notes")],
        )
        .unwrap();
    let command = |extra: &str| {
        format!("version = 1\n[[commands]]\nid = \"a\"\nname = \"A\"\ncommand = \"true\"\n{extra}")
    };
    use Fixture::*;
    use ProjectConfigState::{Absent as NoFile, Loaded, Refused};
    let cases: Vec<(&str, Fixture, ProjectConfigState, &str)> = vec![
        ("absent", Absent, NoFile, ""),
        ("luna-is-a-file", LunaIsAFile, NoFile, ""),
        (
            "unknown-version",
            Text("version = 2\n".into()),
            Refused,
            "Unsupported schema version 2",
        ),
        (
            "missing-version",
            Text(command("").replacen("version = 1\n", "", 1)),
            Refused,
            "Missing `version = 1`",
        ),
        (
            "string-version",
            Text("version = \"1\"\n".into()),
            Refused,
            "`version` must be the integer 1",
        ),
        (
            "malformed",
            Text("version = 1\n[[commands]\n".into()),
            Refused,
            "Invalid TOML",
        ),
        (
            "setup-key-is-unknown",
            Text(command("setup-on-worktree-create = true\n")),
            Refused,
            "unknown field `setup-on-worktree-create`",
        ),
        (
            "unknown-top-level-key",
            Text("version = 1\nenv = { TOKEN = \"x\" }\n".into()),
            Refused,
            "unknown field `env`",
        ),
        (
            "duplicate-id",
            Text(format!(
                "{}[[commands]]\nid = \"a\"\nname = \"Again\"\ncommand = \"true\"\n",
                command("")
            )),
            Refused,
            "declared twice",
        ),
        (
            "path-like-id",
            Text(command("").replace("id = \"a\"", "id = \"../a\"")),
            Refused,
            "an ID has",
        ),
        (
            "hidden-direction",
            Text(command("").replace(
                "command = \"true\"",
                "command = \"echo safe \u{202E}txt.exe\"",
            )),
            Refused,
            "text-direction",
        ),
        (
            "escape-sequence",
            Text(command("").replace("command = \"true\"", r#"command = "printf \u001b[2J""#)),
            Refused,
            "control characters",
        ),
        (
            "file-preview",
            Text(command("preview-url = \"file:///etc/passwd\"\n")),
            Refused,
            "preview-url",
        ),
        (
            "link-to-file-outside",
            LinkToFileOutside,
            Refused,
            "outside the checkout",
        ),
        (
            "link-luna-dir-outside",
            LinkLunaDirOutside,
            Refused,
            "outside the checkout",
        ),
        ("fifo", Fifo, Refused, "not a regular file"),
        (
            "oversized",
            Text(format!("version = 1\n# {}\n", "x".repeat(70 * 1024))),
            Refused,
            "larger than 64 KiB",
        ),
        (
            "not-utf8",
            Bytes(b"version = 1\n# \xff\n".to_vec()),
            Refused,
            "UTF-8",
        ),
        ("link-inside", LinkInside, Loaded, ""),
    ];
    let mut results = Vec::new();
    for (case, fixture, state, detail) in &cases {
        install(&repo, &outside, fixture);
        // Opening a FIFO must never block the engine's read.
        let snapshot =
            tokio::time::timeout(Duration::from_secs(5), core.project_commands.snapshot("a"))
                .await
                .unwrap_or_else(|_| panic!("{case}: reading the file blocked"))
                .unwrap_or_else(|error| panic!("{case}: the project broke: {error}"));
        let layer = snapshot.repository.clone().unwrap();
        assert_eq!(layer.state, *state, "{case}: {:?}", layer.detail);
        assert!(
            layer.detail.as_deref().unwrap_or_default().contains(detail),
            "{case}: {:?}",
            layer.detail
        );
        assert_eq!(
            ids(&snapshot.actions),
            ["notes"],
            "{case}: personal commands stay runnable and nothing else is"
        );
        let folder: ProjectRepositoryConfig = client
            .call_as(methods::GET_PROJECT_CONFIGURATION, json!({"projectId":"p"}))
            .await
            .unwrap();
        assert_eq!(folder.state, *state, "{case}: project folder view");
        let accept = client
            .call(
                methods::SET_PROJECT_COMMAND_TRUST,
                json!({"projectId":"p","chatId":"a","actionId":"a","digest":"0".repeat(64),"trusted":true}),
            )
            .await;
        if *state != Loaded {
            let refusal = accept.unwrap_err().to_string();
            assert!(refusal.contains("is not in use here"), "{case}: {refusal}");
        }
        results.push(json!({
            "case": case,
            "state": layer.state,
            "detail": layer.detail,
            "commands": layer.commands.len(),
            "runnable": ids(&snapshot.actions),
            "acceptRefused": *state != Loaded,
        }));
    }
    // With a refused file in place, this device's command still runs.
    install(&repo, &outside, &Text("version = 3\n".into()));
    let run: ProjectCommandRun = client
        .call_as(
            methods::RUN_PROJECT_COMMAND,
            json!({"chatId":"a","actionId":"notes","requestId":"notes"}),
        )
        .await
        .unwrap();
    let done = completed(&core, "a", &run.id).await;
    assert_eq!(done.status, ProjectCommandStatus::Succeeded);
    assert_eq!(
        std::fs::read_to_string(repo.join("ran-notes")).unwrap(),
        "notes"
    );
    assert_eq!(
        std::fs::read_to_string(outside.join("project.toml")).unwrap(),
        SHARED,
        "reading never writes outside the checkout"
    );
    evidence(
        "refusals",
        &json!({"cases": results, "personalRunWithRefusedFile": done}),
    );
    core.terminals.shutdown();
    drop(client);
    core.shutdown().await;
}
