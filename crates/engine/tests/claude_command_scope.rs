//! E23 through the real engine RPC: `ListCommands` discovers Claude commands
//! under the scope the chat/space's next run launches with (cwd, effective
//! permission mode, provider home and account), using the deterministic
//! Claude Code 2.1.286 peer from the harness fixtures.
//! Failure cases: `$B/evidence/cc00-baseline/failure-cases.md` (G-15, G-16).

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use luna_engine::{AgentAccountsConfig, EngineCore, EngineProfile, HarnessRegistry};
use luna_harness::ClaudeHarness;
use luna_proto::{ChatConfig, HarnessId, SlashCommand};
use luna_rpc::methods;
use serde_json::{Value, json};

fn fake() -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../harness/tests/fixtures/fake-claude-config.py");
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
    path
}

fn git(cwd: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@test")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@test")
        .output()
        .expect("git spawns");
    assert!(output.status.success(), "git {args:?}");
}

fn project(root: &Path) -> PathBuf {
    let project = root.join("project");
    std::fs::create_dir_all(project.join(".claude/commands")).unwrap();
    std::fs::write(
        project.join(".claude/settings.json"),
        r#"{"outputStyle":"Explanatory"}"#,
    )
    .unwrap();
    std::fs::write(
        project.join(".claude/commands/luna-project-canary.md"),
        "---\ndescription: project canary\n---\nNever invoked.\n",
    )
    .unwrap();
    git(&project, &["init", "-b", "main"]);
    git(&project, &["add", "."]);
    git(&project, &["commit", "-m", "fixture"]);
    project.canonicalize().unwrap()
}

fn account_file(path: &Path, uuid: &str) {
    std::fs::write(
        path,
        json!({"oauthAccount": {"accountUuid": uuid}}).to_string(),
    )
    .unwrap();
}

/// Command-discovery probes only: background model discovery (no policy
/// flags) may also initialize the peer and is not part of this contract.
fn log(home: &Path) -> Vec<Value> {
    std::fs::read_to_string(home.join("fake-claude-log.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|entry| {
            entry["argv"]
                .as_array()
                .is_some_and(|argv| argv.iter().any(|arg| arg == "--permission-prompt-tool"))
        })
        .collect()
}

fn has(commands: &Value, name: &str) -> bool {
    serde_json::from_value::<Vec<SlashCommand>>(commands.clone())
        .unwrap()
        .iter()
        .any(|command| command.name == name)
}

fn config(mode: &str) -> ChatConfig {
    serde_json::from_value(json!({
        "harness": "claude-code",
        "sandbox": "workspace-write",
        "permissionMode": mode,
    }))
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_commands_uses_the_chat_or_space_run_scope() {
    let root = tempfile::tempdir().unwrap();
    let project = project(root.path());
    let home = root.path().join("claude-home");
    std::fs::create_dir_all(home.join("commands")).unwrap();
    std::fs::write(
        home.join("commands/luna-user-canary.md"),
        "---\ndescription: user canary\n---\nNever invoked.\n",
    )
    .unwrap();
    let identity = root.path().join("claude.json");
    account_file(&identity, "synthetic-account-1");

    let registry = HarnessRegistry::new();
    registry.register(Arc::new(
        ClaudeHarness::new()
            .with_executable(fake())
            .with_config_dir(&home),
    ));
    let data = root.path().join("device");
    let core = EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        EngineProfile::development(&data, "synthetic-org", "synthetic-user"),
        Arc::new(registry),
        HarnessId::ClaudeCode,
        None,
        AgentAccountsConfig {
            data_dir: data.clone(),
            claude_config_dir: home.clone(),
            claude_config_file: identity.clone(),
            codex_home: root.path().join("codex"),
        },
    )
    .expect("engine assembles");
    let project_text = project.to_string_lossy().into_owned();
    core.workspace
        .create_space("space-1", &core.device_id, &project_text, None, true)
        .unwrap();
    for (chat, mode) in [("chat-ask", "ask"), ("chat-full", "full")] {
        core.workspace
            .create_chat(
                chat,
                Some("space-1"),
                Some(&core.device_id),
                Some(config(mode)),
                Some(project_text.clone()),
            )
            .unwrap();
    }
    let client = luna_rpc::memory_client(core.rpc_service());

    // Ask chat: restricted run → neither custom command loads.
    let ask = client
        .call(
            methods::LIST_COMMANDS,
            json!({"harness": "claude-code", "chatId": "chat-ask"}),
        )
        .await
        .unwrap();
    assert!(!has(&ask, "luna-project-canary") && !has(&ask, "luna-user-canary"));
    let entry = log(&home).pop().unwrap();
    assert_eq!(entry["cwd"], project_text, "discovery ran in the chat cwd");
    assert_eq!(entry["restricted"], true);
    assert_eq!(entry["permission_mode"], "acceptEdits");

    // The composer's selected mode wins over the stored one.
    let selected = client
        .call(
            methods::LIST_COMMANDS,
            json!({"harness": "claude-code", "chatId": "chat-ask", "permissionMode": "full"}),
        )
        .await
        .unwrap();
    assert!(has(&selected, "luna-project-canary") && has(&selected, "luna-user-canary"));

    // Full chat by its own config; repeated call is a cache hit.
    let probes = log(&home).len();
    let full = client
        .call(
            methods::LIST_COMMANDS,
            json!({"harness": "claude-code", "chatId": "chat-full"}),
        )
        .await
        .unwrap();
    assert!(has(&full, "luna-project-canary"));
    assert_eq!(
        log(&home).len(),
        probes,
        "same scope as the selected-full call"
    );

    // Account switch re-probes the same chat.
    account_file(&identity, "synthetic-account-2");
    client
        .call(
            methods::LIST_COMMANDS,
            json!({"harness": "claude-code", "chatId": "chat-full"}),
        )
        .await
        .unwrap();
    assert_eq!(log(&home).len(), probes + 1, "new account instance");

    // Space scope (new-chat drafts).
    let space = client
        .call(
            methods::LIST_COMMANDS,
            json!({"harness": "claude-code", "spaceId": "space-1", "permissionMode": "full"}),
        )
        .await
        .unwrap();
    assert!(has(&space, "luna-project-canary"));
    assert_eq!(log(&home).pop().unwrap()["cwd"], project_text);

    // Unknown chat is rejected, never a silent global catalog.
    let unknown = client
        .call(
            methods::LIST_COMMANDS,
            json!({"harness": "claude-code", "chatId": "missing"}),
        )
        .await;
    assert!(unknown.is_err(), "{unknown:?}");

    // Older clients: unscoped request still answers (default Ask, no workspace).
    let legacy = client
        .call(methods::LIST_COMMANDS, json!({"harness": "claude-code"}))
        .await
        .unwrap();
    assert!(!has(&legacy, "luna-user-canary"));
    assert_eq!(log(&home).pop().unwrap()["restricted"], true);
}
