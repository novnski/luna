//! Interactive L12 task-organization QA fixture.
//!
//! The only accepted argument is an explicit synthetic root. Engine, UI,
//! provider homes, repository folders, credentials, and temporary files stay
//! below that root. Auth uses an in-memory credential broker; Codex native
//! metadata access and every platform-Keychain fallback are disabled. Only
//! the opt-in L24/L25 paths may start the pinned repository fake Codex process.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use gpui::{AnyWindowHandle, AppContext, AsyncApp, Bounds, WindowBounds, WindowOptions, px, size};
use luna_doc::{
    MessagePart, MessageRole, MessageStatus, QueueHoldReason, QueueMessageStatus, QueuedMessage,
    SessionMessageEntry,
};
use luna_engine::{
    AgentAccountsConfig, Auth, AuthConfig, EngineCore, EngineProfile, GitCheckpoints,
    HarnessRegistry, Repos,
};
use luna_harness::CodexHarness;
use luna_proto::{ChatConfig, HarnessId, PermissionMode, SandboxLevel};
use luna_rpc::{RpcClient, methods};
use luna_ui::*;

const TASK_PINNED_A: &str = "12121212-1212-4121-8121-121212121212";
const TASK_PINNED_B: &str = "23232323-2323-4232-8232-232323232323";
const TASK_PERSONAL: &str = "34343434-3434-4343-8343-343434343434";
const TASK_ARCHIVED: &str = "45454545-4545-4454-8454-454545454545";
const TASK_REMOTE: &str = "56565656-5656-4565-8565-565656565656";
const TASK_NON_GIT: &str = "67676767-6767-4676-8676-676767676767";
const TASK_L20_SHARED: &str = "78787878-7878-4787-8787-787878787878";
const TASK_L20_ISOLATED: &str = "89898989-8989-4898-8898-898989898989";
const TASK_L24_OTHER: &str = "90909090-9090-4090-8090-909090909090";
const TASK_L25_SOURCE: &str = "a1a1a1a1-a1a1-41a1-81a1-a1a1a1a1a1a1";
const TASK_L25_TARGET: &str = "b2b2b2b2-b2b2-42b2-82b2-b2b2b2b2b2b2";
const TASK_L25_FOREIGN: &str = "c3c3c3c3-c3c3-43c3-83c3-c3c3c3c3c3c3";
const L20_OPT_IN: &str = "LUNA_L20_CHECKPOINT_QA";
const L24_OPT_IN: &str = "LUNA_L24_INTEGRATION_QA";
const L25_OPT_IN: &str = "LUNA_L25_SEND_QA";
const SIDEBAR_SCALE_OPT_IN: &str = "LUNA_SIDEBAR_SCALE_QA";
const SIDEBAR_HOVER_CAPTURE_OPT_IN: &str = "LUNA_SIDEBAR_HOVER_CAPTURE";
const SIDEBAR_HOVER_TARGET_Y: f32 = 360.0;
const REMOTE_DEVICE: &str = "fixture-offline-machine";
const L24_GLOBAL_INSTRUCTIONS: &str =
    "L24 QA global: identify the selected synthetic task before reporting integration evidence.";
const L24_APP_INSTRUCTIONS: &str =
    "L24 QA Fieldnotes Desktop: this is the synthetic project A instruction.";
const L24_OTHER_INSTRUCTIONS: &str =
    "L24 QA Unrelated checkout: this is the synthetic project B instruction.";
const FAKE_CODEX_BYTES: &[u8] = include_bytes!("../../harness/tests/fixtures/fake-codex.sh");

fn capture_hover_frame(
    window: AnyWindowHandle,
    cx: &mut AsyncApp,
    output: &Path,
    name: &str,
) -> anyhow::Result<()> {
    window.update(cx, |_, window, cx| {
        window.draw(cx).clear();
        window.render_to_image()?.save(output.join(name))?;
        Ok(())
    })?
}

/// The canonical system temp root an opt-in seed may live in (macOS resolves
/// `/tmp` to `/private/tmp`).
const OPT_IN_PARENT: &str = if cfg!(target_os = "macos") {
    "/private/tmp"
} else {
    "/tmp"
};

fn validate_opt_in_root(root: &Path) -> anyhow::Result<()> {
    anyhow::ensure!(
        root.parent() == Some(Path::new(OPT_IN_PARENT))
            && root
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("luna-l12-")),
        "opt-in fixture requires an explicit canonical {OPT_IN_PARENT}/luna-l12-* root"
    );
    Ok(())
}

fn validate_fake_codex() -> anyhow::Result<PathBuf> {
    let source =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../harness/tests/fixtures/fake-codex.sh");
    anyhow::ensure!(
        std::fs::symlink_metadata(&source)?.file_type().is_file(),
        "opt-in fake Codex must be a regular, non-symlink repository file"
    );
    let expected = source.canonicalize()?;
    let supplied = PathBuf::from(std::env::var_os("CODEX_EXECUTABLE").ok_or_else(|| {
        anyhow::anyhow!("opt-in requires the explicit repository fake CODEX_EXECUTABLE")
    })?);
    anyhow::ensure!(
        supplied.is_absolute()
            && !std::fs::symlink_metadata(&supplied)?
                .file_type()
                .is_symlink(),
        "CODEX_EXECUTABLE must be the absolute, non-symlink repository fake"
    );
    anyhow::ensure!(
        supplied.canonicalize()? == expected,
        "CODEX_EXECUTABLE differs from the audited repository fake"
    );
    anyhow::ensure!(
        std::fs::read(&expected)? == FAKE_CODEX_BYTES,
        "fake Codex changed after this fixture was built; rebuild before QA"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        anyhow::ensure!(
            std::fs::metadata(&expected)?.permissions().mode() & 0o111 != 0,
            "repository fake Codex is not executable"
        );
    }
    Ok(expected)
}

fn account_config(root: &Path) -> AgentAccountsConfig {
    AgentAccountsConfig {
        data_dir: root.join("provider-state"),
        claude_config_dir: root.join("synthetic-claude"),
        claude_config_file: root.join("synthetic-claude.json"),
        codex_home: root.join("synthetic-codex"),
    }
}

fn fixture_auth_config(root: &Path) -> AuthConfig {
    let mut config = AuthConfig::new("http://127.0.0.1:1", root.join("synthetic-auth"));
    config.dev_user_id = "l12-fixture-user".into();
    config.use_in_memory_credentials_for_tests();
    config
}

fn task_config() -> ChatConfig {
    ChatConfig {
        harness: HarnessId::Mock,
        provider_account_id: None,
        model: Some("fixture-only".into()),
        reasoning: None,
        model_options: Default::default(),
        sandbox: SandboxLevel::WorkspaceWrite,
        permission_mode: PermissionMode::Ask,
        effective_permission_mode: PermissionMode::Ask,
        permission_ceiling_source: None,
        permission_degradation_reason: None,
        permission_audit: Vec::new(),
    }
}

fn init_git_repo(path: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(path)?;
    let status = std::process::Command::new("git")
        .arg("init")
        .arg("--quiet")
        .arg(path)
        .status()?;
    anyhow::ensure!(status.success(), "fixture git init failed");
    Ok(())
}

fn fixture_git(path: &Path, args: &[&str]) -> anyhow::Result<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null())
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "synthetic git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    anyhow::ensure!(
        output.stdout.len() <= 16 * 1024,
        "synthetic Git output exceeded fixture limit"
    );
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

fn init_l20_repo(path: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(path)?;
    fixture_git(path, &["init", "-q", "-b", "main"])?;
    fixture_git(path, &["config", "user.name", "LUNA Fixture"])?;
    fixture_git(path, &["config", "user.email", "fixture@example.invalid"])?;
    std::fs::write(path.join("sample.txt"), "before checkpoint\n")?;
    fixture_git(path, &["add", "sample.txt"])?;
    fixture_git(
        path,
        &[
            "-c",
            "core.hooksPath=/dev/null",
            "commit",
            "-q",
            "-m",
            "synthetic base",
        ],
    )?;
    Ok(())
}

struct L20Seed {
    shared: PathBuf,
    isolated: PathBuf,
    newly_seeded: bool,
}

async fn seed_l20_with_services(
    core: &EngineCore,
    root: &Path,
    repos: &Repos,
    checkpoints: &GitCheckpoints,
) -> anyhow::Result<L20Seed> {
    validate_opt_in_root(root)?;
    let shared = root.join("l20/shared");
    let shared_row = core.workspace.chat(TASK_L20_SHARED)?;
    let isolated_row = core.workspace.chat(TASK_L20_ISOLATED)?;
    if let (Some(shared_row), Some(isolated_row)) = (&shared_row, &isolated_row) {
        let shared_cwd = PathBuf::from(shared_row.cwd.as_deref().unwrap_or(""));
        let isolated_cwd = PathBuf::from(isolated_row.cwd.as_deref().unwrap_or(""));
        anyhow::ensure!(
            shared_cwd.canonicalize()? == shared
                && isolated_cwd
                    .canonicalize()?
                    .starts_with(root.join("l20-worktrees")),
            "existing L20 tasks no longer point to this synthetic root"
        );
        return Ok(L20Seed {
            shared,
            isolated: isolated_cwd,
            newly_seeded: false,
        });
    }
    anyhow::ensure!(
        shared_row.is_none() && isolated_row.is_none() && !root.join("l20").exists(),
        "partial L20 seed found; use a fresh synthetic root"
    );
    let source = root.join("l20/source");
    init_l20_repo(&shared)?;
    init_l20_repo(&source)?;
    let worktree = repos.create_worktree(&source, "main").await?;
    let isolated = PathBuf::from(worktree.path).canonicalize()?;
    anyhow::ensure!(
        isolated.starts_with(root.join("l20-worktrees")) && isolated.join(".git").is_file(),
        "created worktree escaped the synthetic managed root"
    );
    anyhow::ensure!(
        repos.checkout_identity(&isolated).await?.root == isolated,
        "created worktree did not resolve to its registered checkout"
    );
    core.workspace.create_space(
        "fixture-l20-shared",
        &core.device_id,
        shared.to_str().unwrap(),
        Some("L20 Shared checkout".into()),
        true,
    )?;
    core.workspace.create_space(
        "fixture-l20-isolated",
        &core.device_id,
        isolated.to_str().unwrap(),
        Some("L20 Isolated checkout".into()),
        true,
    )?;
    for (task_id, space_id, cwd, title, turn_id) in [
        (
            TASK_L20_SHARED,
            "fixture-l20-shared",
            shared.as_path(),
            "L20 Shared checkpoint (restore refused)",
            "l20-shared-turn",
        ),
        (
            TASK_L20_ISOLATED,
            "fixture-l20-isolated",
            isolated.as_path(),
            "L20 Isolated checkpoint (restore available)",
            "l20-isolated-turn",
        ),
    ] {
        core.workspace.create_chat(
            task_id,
            Some(space_id),
            Some(&core.device_id),
            Some(task_config()),
            Some(cwd.to_string_lossy().into_owned()),
        )?;
        core.workspace.rename_chat(task_id, title)?;
        checkpoints
            .start(task_id, turn_id, cwd.to_str().unwrap())
            .await;
        std::fs::write(cwd.join("sample.txt"), "after checkpoint\n")?;
        std::fs::write(cwd.join("turn-created.txt"), "created during turn\n")?;
        checkpoints.end(task_id, turn_id, "completed").await;
        std::fs::write(cwd.join("sample.txt"), "current post-checkpoint edit\n")?;
        std::fs::write(cwd.join("post-edit.txt"), "created after checkpoint\n")?;
        let list = checkpoints
            .list(task_id, cwd.to_str().unwrap(), false)
            .await
            .map_err(anyhow::Error::msg)?;
        anyhow::ensure!(
            list.checkpoints.len() == 1
                && list.checkpoints[0].before_available
                && list.checkpoints[0].after_available
                && list.checkpoints[0].error.is_none(),
            "L20 checkpoint capture did not persist both boundaries"
        );
        let diff = checkpoints
            .diff(task_id, cwd.to_str().unwrap(), &list.checkpoints[0].turn_id)
            .await
            .map_err(anyhow::Error::msg)?;
        anyhow::ensure!(
            diff.paths == ["sample.txt", "turn-created.txt"],
            "L20 checkpoint diff paths changed: {:?}",
            diff.paths
        );
    }
    let shared_list = checkpoints
        .list(TASK_L20_SHARED, shared.to_str().unwrap(), false)
        .await
        .map_err(anyhow::Error::msg)?;
    let isolated_list = checkpoints
        .list(TASK_L20_ISOLATED, isolated.to_str().unwrap(), false)
        .await
        .map_err(anyhow::Error::msg)?;
    anyhow::ensure!(
        !shared_list.restore_available && isolated_list.restore_available,
        "L20 shared/isolated restore gates did not match the registered checkouts"
    );
    let preview = checkpoints
        .preview_restore(
            TASK_L20_ISOLATED,
            isolated.to_str().unwrap(),
            &isolated_list.checkpoints[0].turn_id,
            "before",
            false,
        )
        .await
        .map_err(anyhow::Error::msg)?;
    anyhow::ensure!(
        preview.paths == ["post-edit.txt", "sample.txt", "turn-created.txt"],
        "L20 restore preview paths changed: {:?}",
        preview.paths
    );
    std::fs::write(
        root.join("L20-FIXTURE.txt"),
        format!(
            "Synthetic L20 Git checkpoints only. No provider or live user workspace.\nshared_task={TASK_L20_SHARED}\nshared_cwd={}\nisolated_task={TASK_L20_ISOLATED}\nisolated_cwd={}\ncheckpoint_before=sample.txt contains 'before checkpoint'\ncheckpoint_after=sample.txt contains 'after checkpoint'; turn-created.txt exists\ncurrent=sample.txt contains 'current post-checkpoint edit'; post-edit.txt exists\n",
            shared.display(),
            isolated.display()
        ),
    )?;
    Ok(L20Seed {
        shared,
        isolated,
        newly_seeded: true,
    })
}

async fn seed_l20(core: &EngineCore, root: &Path) -> anyhow::Result<()> {
    let checkpoints = GitCheckpoints::new(core.repos.clone());
    let seeded = seed_l20_with_services(core, root, &core.repos, &checkpoints).await?;
    // The UI uses the engine's RPC-owned checkpoint service, which reads these
    // same hidden refs. Prove both tasks resolve through that product path.
    let client = luna_rpc::memory_client(core.rpc_service());
    for (task_id, cwd, restore_available) in [
        (TASK_L20_SHARED, seeded.shared.as_path(), false),
        (TASK_L20_ISOLATED, seeded.isolated.as_path(), true),
    ] {
        let cwd = cwd
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("non-UTF-8 synthetic cwd"))?;
        let list: luna_proto::GitCheckpointList = client
            .call_as(
                methods::LIST_GIT_CHECKPOINTS,
                serde_json::json!({ "chatId": task_id, "cwd": cwd }),
            )
            .await?;
        anyhow::ensure!(
            list.checkpoints.len() == 1 && list.restore_available == restore_available,
            "L20 RPC checkpoint list/gate readback failed for {task_id}: {:?}",
            list.restore_unavailable_reason
        );
        let _: luna_proto::GitCheckpointDiff = client.call_as(methods::DIFF_GIT_CHECKPOINT,
            serde_json::json!({ "chatId": task_id, "cwd": cwd, "turnId": list.checkpoints[0].turn_id })).await?;
        if restore_available && seeded.newly_seeded {
            let _: luna_proto::GitCheckpointRestorePreview = client.call_as(methods::PREVIEW_GIT_CHECKPOINT_RESTORE,
                serde_json::json!({ "chatId": task_id, "cwd": cwd, "turnId": list.checkpoints[0].turn_id, "point": "before" })).await?;
        }
    }
    Ok(())
}

async fn snapshot(
    client: &RpcClient,
    task_id: &str,
) -> anyhow::Result<luna_proto::TaskCommandRead> {
    Ok(client
        .call_as(
            methods::GET_TASK_COMMAND_SNAPSHOT,
            serde_json::json!({ "taskId": task_id }),
        )
        .await?)
}

async fn command(
    client: &RpcClient,
    command_id: &str,
    task_id: &str,
    intent: serde_json::Value,
) -> anyhow::Result<()> {
    let read = snapshot(client, task_id).await?;
    let mut envelope = serde_json::json!({
        "commandId": command_id,
        "actorId": "l12-native-fixture",
        "clientId": "l12-native-fixture",
        "clientSessionId": "l12-native-fixture-bootstrap",
        "taskId": task_id,
        "expectedTaskRevision": read.snapshot.task_revision,
        "intent": intent,
    });
    if let Some(generation) = read.snapshot.binding_generation {
        envelope["expectedBindingGeneration"] = generation.into();
    }
    let receipt: luna_proto::CommandReceipt = client
        .call_as(methods::EXECUTE_TASK_COMMAND, envelope)
        .await?;
    anyhow::ensure!(
        receipt.status == luna_proto::CommandReceiptStatus::Applied,
        "fixture command {command_id} ended {:?}: {:?}",
        receipt.status,
        receipt.reason
    );
    Ok(())
}

async fn seed(core: &EngineCore, root: &Path) -> anyhow::Result<()> {
    if !core.workspace.read_chats()?.is_empty() {
        return Ok(());
    }
    let repo = root.join("workspaces/fieldnotes");
    let app = repo.join("apps/desktop");
    let other = root.join("workspaces/unrelated");
    let notes = root.join("workspaces/notes");
    let other_notes = root.join("workspaces/other-notes");
    init_git_repo(&repo)?;
    init_git_repo(&other)?;
    std::fs::create_dir_all(&app)?;
    std::fs::create_dir_all(&notes)?;
    std::fs::create_dir_all(&other_notes)?;
    core.workspace.create_space(
        "fixture-app",
        &core.device_id,
        app.to_str().unwrap(),
        Some("Fieldnotes Desktop".into()),
        true,
    )?;
    core.workspace.create_space(
        "fixture-root",
        &core.device_id,
        repo.to_str().unwrap(),
        Some("Fieldnotes".into()),
        true,
    )?;
    core.workspace.create_space(
        "fixture-other",
        &core.device_id,
        other.to_str().unwrap(),
        Some("Unrelated checkout".into()),
        true,
    )?;
    core.workspace.create_space(
        "fixture-remote",
        REMOTE_DEVICE,
        "/synthetic/remote/archived-client",
        Some("Offline client project".into()),
        true,
    )?;
    core.workspace.create_space(
        "fixture-notes",
        &core.device_id,
        notes.to_str().unwrap(),
        Some("Notes (non-Git)".into()),
        false,
    )?;
    core.workspace.create_space(
        "fixture-other-notes",
        &core.device_id,
        other_notes.to_str().unwrap(),
        Some("Other notes (different root)".into()),
        false,
    )?;

    for (id, space, cwd, title) in [
        (
            TASK_PINNED_A,
            Some("fixture-app"),
            Some(app.clone()),
            "Pinned design review",
        ),
        (
            TASK_PINNED_B,
            Some("fixture-root"),
            Some(repo.clone()),
            "Pinned release checklist",
        ),
        (TASK_PERSONAL, None, None, "Personal scratch task"),
        (
            TASK_ARCHIVED,
            Some("fixture-root"),
            Some(repo.clone()),
            "Archived migration notes",
        ),
    ] {
        core.workspace.create_chat(
            id,
            space,
            Some(&core.device_id),
            Some(task_config()),
            cwd.map(|path| path.to_string_lossy().into_owned()),
        )?;
        core.workspace.rename_chat(id, title)?;
    }
    core.workspace.create_chat(
        TASK_REMOTE,
        Some("fixture-remote"),
        Some(REMOTE_DEVICE),
        Some(task_config()),
        Some("/synthetic/remote/archived-client".into()),
    )?;
    core.workspace
        .rename_chat(TASK_REMOTE, "Disconnected project task")?;
    core.workspace.create_chat(
        TASK_NON_GIT,
        None,
        Some(&core.device_id),
        Some(task_config()),
        Some(notes.to_string_lossy().into_owned()),
    )?;
    core.workspace
        .rename_chat(TASK_NON_GIT, "Non-Git workspace task")?;
    core.workspace.set_chat_harness_session(
        TASK_PINNED_A,
        "fixture-provider-session-a",
        app.to_str().unwrap(),
    );
    let client = luna_rpc::memory_client(core.rpc_service());
    command(
        &client,
        "fixture-pin-a",
        TASK_PINNED_A,
        serde_json::json!({ "kind": "setPinned", "pinned": true }),
    )
    .await?;
    command(
        &client,
        "fixture-pin-b",
        TASK_PINNED_B,
        serde_json::json!({ "kind": "setPinned", "pinned": true }),
    )
    .await?;
    command(
        &client,
        "fixture-unread",
        TASK_PINNED_B,
        serde_json::json!({ "kind": "setRead", "read": false }),
    )
    .await?;
    command(
        &client,
        "fixture-archive",
        TASK_ARCHIVED,
        serde_json::json!({ "kind": "setArchived", "archived": true }),
    )
    .await?;
    Ok(())
}

/// Disposable sidebar load case: 60 fresh rows, one automatically settled
/// older row, one manually settled recent row, one reactivated recent row,
/// one review-only activity failure, plus the archived task. No real provider
/// profile is touched.
async fn seed_sidebar_scale(core: &EngineCore, root: &Path) -> anyhow::Result<()> {
    let marker = root.join("SIDEBAR-QA.json");
    if marker.exists() {
        let prior: serde_json::Value = serde_json::from_slice(&std::fs::read(&marker)?)?;
        anyhow::ensure!(
            prior["fixture"] == "sidebar-scale-v5" && core.workspace.read_chats()?.len() == 67,
            "sidebar fixture is incomplete"
        );
        return Ok(());
    }
    let now = chrono::Utc::now();
    // These tasks were created within the same millisecond on fast machines,
    // making their sidebar order depend on the tie-breaker. Keep capture
    // coordinates stable across native QA runs.
    for (id, seconds_ago) in [(TASK_REMOTE, 10), (TASK_NON_GIT, 20), (TASK_PERSONAL, 30)] {
        let at = now - chrono::Duration::seconds(seconds_ago);
        anyhow::ensure!(
            core.workspace.set_chat_activity(
                id,
                Some(at.timestamp_millis()),
                Some(at.timestamp_millis())
            )?,
            "sidebar fixture task activity update did not apply"
        );
    }
    let mut ids = Vec::new();
    for index in 0..=60u16 {
        let id = format!("d349{index:04x}-64d7-4aeb-b686-a751fb570467");
        core.workspace.create_chat(
            &id,
            Some("fixture-root"),
            Some(&core.device_id),
            Some(task_config()),
            None,
        )?;
        let title = if index == 0 {
            "Scale keyword older thread".to_string()
        } else {
            format!("Scale keyword recent thread {index:02}")
        };
        core.workspace.rename_chat(&id, &title)?;
        let at = if index == 0 {
            now - chrono::Duration::days(30)
        } else {
            now - chrono::Duration::minutes(i64::from(index))
        };
        anyhow::ensure!(
            core.workspace.set_chat_activity(
                &id,
                Some(at.timestamp_millis()),
                Some(at.timestamp_millis())
            )?,
            "sidebar task activity update did not apply"
        );
        ids.push(id);
    }
    let client = luna_rpc::memory_client(core.rpc_service());
    command(
        &client,
        "fixture-sidebar-old-seen",
        &ids[0],
        serde_json::json!({ "kind": "setRead", "read": true }),
    )
    .await?;
    command(
        &client,
        "fixture-sidebar-review-seen",
        &ids[1],
        serde_json::json!({
            "kind": "markSeen",
            "at": (now - chrono::Duration::seconds(10)).timestamp_millis()
        }),
    )
    .await?;
    core.doc_host
        .open(&ids[1])?
        .doc()
        .push_queued(&QueuedMessage {
            input_items: Vec::new(),
            id: "sidebar-review-failure".into(),
            text: "Fixture-only queued message requiring review".into(),
            attachments: Vec::new(),
            hold_for_turn_end: false,
            follow_up_behavior: Default::default(),
            hold_reason: Some(QueueHoldReason::ProviderRejected),
            issued_by: core.device_id.clone(),
            issued_at: now.timestamp_millis(),
            edited_at: None,
            revision: 1,
            execution: None,
            status: QueueMessageStatus::Held,
            delivery_gate: None,
        })?;
    command(
        &client,
        "fixture-sidebar-manual-settle",
        &ids[60],
        serde_json::json!({ "kind": "setSettled", "settled": true }),
    )
    .await?;
    anyhow::ensure!(
        core.workspace.chat(&ids[60])?.is_some_and(|chat| {
            chat.settlement_override.is_some_and(|settlement| {
                settlement.disposition == luna_proto::SettlementDisposition::Settled
            })
        }),
        "manual settlement did not persist to the synced task record"
    );
    command(
        &client,
        "fixture-sidebar-reactivation-settle",
        &ids[59],
        serde_json::json!({ "kind": "setSettled", "settled": true }),
    )
    .await?;
    let settled_at = core
        .workspace
        .chat(&ids[59])?
        .and_then(|chat| chat.settlement_override)
        .ok_or_else(|| anyhow::anyhow!("reactivation seed did not settle"))?
        .at;
    anyhow::ensure!(
        core.workspace.set_chat_activity(
            &ids[59],
            Some((settled_at + chrono::Duration::seconds(1)).timestamp_millis()),
            None,
        )?,
        "reactivated task was not found"
    );
    anyhow::ensure!(
        core.workspace.chat(&ids[59])?.is_some_and(|chat| {
            chat.settlement_override.is_some_and(|settlement| {
                chat.last_message_at
                    .is_some_and(|activity| activity > settlement.at)
            })
        }),
        "new activity did not supersede manual settlement"
    );
    for (task_id, message_id, body, created_at) in [
        (
            ids[0].as_str(),
            "sidebar-old-content",
            "buriedmarker appears in this older conversation",
            (now - chrono::Duration::days(30)).timestamp_millis(),
        ),
        (
            TASK_ARCHIVED,
            "sidebar-archived-content",
            "archiveonlymarker appears in this archived conversation",
            now.timestamp_millis(),
        ),
    ] {
        core.doc_host
            .open(task_id)?
            .doc()
            .push_message(&SessionMessageEntry {
                id: message_id.into(),
                role: MessageRole::User,
                parts: vec![MessagePart::Text {
                    id: "text".into(),
                    text: body.into(),
                }],
                created_at,
                device_id: core.device_id.clone(),
                status: Some(MessageStatus::Complete),
                continuation_of: None,
            })?;
    }
    let rebuilt: luna_proto::HistoryRebuildResult = client
        .call_as(
            methods::REBUILD_SESSION_HISTORY,
            serde_json::json!({"taskIds": [ids[0], TASK_ARCHIVED]}),
        )
        .await?;
    anyhow::ensure!(
        rebuilt.indexed_tasks == 2,
        "sidebar content index is incomplete"
    );
    anyhow::ensure!(
        core.workspace.read_chats()?.len() == 67,
        "sidebar task count differs"
    );
    std::fs::write(
        marker,
        serde_json::to_vec_pretty(&serde_json::json!({
            "fixture": "sidebar-scale-v5",
            "recentCount": 60,
            "olderId": ids[0],
            "recentIds": &ids[1..],
            "manualArchivedId": TASK_ARCHIVED,
            "reviewTaskId": ids[1],
            "manualSettledId": ids[60],
            "reactivatedId": ids[59],
            "expectedActivePage": 20,
            "contentQueries": ["buriedmarker", "archiveonlymarker"],
        }))?,
    )?;
    Ok(())
}

async fn seed_l24(core: &EngineCore, root: &Path) -> anyhow::Result<bool> {
    let marker = root.join("L24-FIXTURE.txt");
    if marker.exists() {
        anyhow::ensure!(
            core.workspace.chat(TASK_L24_OTHER)?.is_some(),
            "partial L24 seed found; use a fresh synthetic root"
        );
        return Ok(false);
    }
    anyhow::ensure!(
        core.workspace.chat(TASK_L24_OTHER)?.is_none(),
        "partial L24 task seed found; use a fresh synthetic root"
    );
    let app = root.join("workspaces/fieldnotes/apps/desktop");
    let other = root.join("workspaces/unrelated");
    let files = [
        (
            root.join("synthetic-codex/skills/l24-global/SKILL.md"),
            "# L24 synthetic global skill\n",
        ),
        (
            root.join("synthetic-codex/config.toml"),
            "[mcp_servers.l24-global]\ncommand = '/usr/bin/false'\nenabled = true\n",
        ),
        (
            app.join(".agents/skills/imagegen/SKILL.md"),
            "# L24 synthetic project A skill\n",
        ),
        (
            other.join(".agents/skills/imagegen/SKILL.md"),
            "# L24 synthetic project B skill\n",
        ),
        (
            app.join(".luna-mcp-project-a"),
            "synthetic fake Codex project A\n",
        ),
        (
            other.join(".luna-mcp-project-b"),
            "synthetic fake Codex project B\n",
        ),
    ];
    anyhow::ensure!(
        files.iter().all(|(path, _)| !path.exists()),
        "partial L24 file seed found; use a fresh synthetic root"
    );
    let client = luna_rpc::memory_client(core.rpc_service());
    let empty: luna_proto::InstructionSettingsView = client
        .call_as(methods::GET_INSTRUCTION_SETTINGS, serde_json::json!({}))
        .await?;
    anyhow::ensure!(
        empty.revision == 0 && empty.global.is_empty(),
        "existing instruction settings found; L24 requires a fresh synthetic root"
    );
    for (path, content) in files {
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(path, content)?;
    }
    core.workspace.create_chat(
        TASK_L24_OTHER,
        Some("fixture-other"),
        Some(&core.device_id),
        Some(task_config()),
        Some(other.to_string_lossy().into_owned()),
    )?;
    core.workspace
        .rename_chat(TASK_L24_OTHER, "L24 Other project task")?;
    let global: luna_proto::InstructionSettingsView = client
        .call_as(
            methods::SET_INSTRUCTION_SETTINGS,
            serde_json::json!({"text": L24_GLOBAL_INSTRUCTIONS, "expectedRevision": empty.revision}),
        )
        .await?;
    let app_view: luna_proto::InstructionSettingsView = client
        .call_as(
            methods::SET_INSTRUCTION_SETTINGS,
            serde_json::json!({"projectId": "fixture-app", "text": L24_APP_INSTRUCTIONS, "expectedRevision": global.revision}),
        )
        .await?;
    let _: luna_proto::InstructionSettingsView = client
        .call_as(
            methods::SET_INSTRUCTION_SETTINGS,
            serde_json::json!({"projectId": "fixture-other", "text": L24_OTHER_INSTRUCTIONS, "expectedRevision": app_view.revision}),
        )
        .await?;
    std::fs::write(
        marker,
        format!(
            "Synthetic L24 integrations only. CODEX_EXECUTABLE is the pinned repository fake; no real provider or MCP server is started.\nproject_a=fixture-app / Fieldnotes Desktop\nproject_b=fixture-other / Unrelated checkout\ntask_a={TASK_PINNED_A}\ntask_b={TASK_L24_OTHER}\nglobal_instructions={L24_GLOBAL_INSTRUCTIONS}\nproject_a_instructions={L24_APP_INSTRUCTIONS}\nproject_b_instructions={L24_OTHER_INSTRUCTIONS}\nexpected_project_mcp=project-a,project-b (fake config/read)\nexpected_project_skill=imagegen (fake skills/list)\n"
        ),
    )?;
    Ok(true)
}

async fn preflight_l24(core: &EngineCore, newly_seeded: bool) -> anyhow::Result<()> {
    let client = luna_rpc::memory_client(core.rpc_service());
    for (project_id, task_id, mcp_name, expected_project_text) in [
        (
            "fixture-app",
            TASK_PINNED_A,
            "project-a",
            L24_APP_INSTRUCTIONS,
        ),
        (
            "fixture-other",
            TASK_L24_OTHER,
            "project-b",
            L24_OTHER_INSTRUCTIONS,
        ),
    ] {
        let instructions: luna_proto::InstructionSettingsView = client
            .call_as(
                methods::GET_INSTRUCTION_SETTINGS,
                serde_json::json!({"projectId": project_id}),
            )
            .await?;
        anyhow::ensure!(
            instructions.project_id.as_deref() == Some(project_id),
            "L24 instruction readback selected the wrong project"
        );
        if newly_seeded {
            anyhow::ensure!(
                instructions.global == L24_GLOBAL_INSTRUCTIONS
                    && instructions.project == expected_project_text,
                "L24 instruction seed did not persist through the product RPC"
            );
        }
        let inventory: luna_proto::IntegrationInventoryView = client
            .call_as(
                methods::GET_INTEGRATION_INVENTORY,
                serde_json::json!({"provider": "codex", "projectId": project_id, "probeProvider": true}),
            )
            .await?;
        anyhow::ensure!(
            inventory.unavailable_reason.is_none()
                && inventory.items.iter().any(|item| {
                    item.name == "imagegen"
                        && item.scope == "project"
                        && item.available == Some(true)
                        && item.attached.is_none()
                        && item
                            .reason
                            .as_deref()
                            .is_some_and(|reason| reason.contains("skills/list"))
                })
                && inventory.items.iter().any(|item| {
                    item.name == mcp_name
                        && item.source == "Codex app-server / config/read"
                        && item.scope == "selected workspace"
                        && item.attached.is_none()
                })
                && inventory.items.iter().any(|item| {
                    item.name == "l24-global" && item.source == "Codex home / config.toml"
                }),
            "L24 fake provider inventory failed for {project_id}: {:?}",
            inventory.unavailable_reason
        );
        let diagnostics: luna_proto::RuntimeContractView = client
            .call_as(
                methods::GET_RUNTIME_CONTRACT,
                serde_json::json!({"taskId": task_id}),
            )
            .await?;
        anyhow::ensure!(
            diagnostics.task_id == task_id && diagnostics.effective.is_none(),
            "L24 selected task diagnostics are not honest for {task_id}"
        );
        println!("l24_preflight=ok project={project_id} task={task_id} fake_mcp={mcp_name}");
    }
    Ok(())
}

fn seed_l25(core: &EngineCore, root: &Path) -> anyhow::Result<()> {
    let app = root.join("workspaces/fieldnotes/apps/desktop");
    let other = root.join("workspaces/unrelated");
    let existing = [TASK_L25_SOURCE, TASK_L25_TARGET, TASK_L25_FOREIGN]
        .map(|id| core.workspace.chat(id))
        .into_iter()
        .collect::<Result<Vec<_>, _>>()?;
    anyhow::ensure!(
        existing.iter().all(Option::is_none) || existing.iter().all(Option::is_some),
        "partial L25 seed found; use a fresh synthetic root"
    );
    if existing.iter().all(Option::is_none) {
        for (id, project, cwd, title, harness) in [
            (
                TASK_L25_SOURCE,
                "fixture-app",
                &app,
                "L25 source",
                HarnessId::Codex,
            ),
            (
                TASK_L25_TARGET,
                "fixture-app",
                &app,
                "L25 target",
                HarnessId::Mock,
            ),
            (
                TASK_L25_FOREIGN,
                "fixture-other",
                &other,
                "L25 foreign project",
                HarnessId::Mock,
            ),
        ] {
            let mut config = task_config();
            config.harness = harness;
            core.workspace.create_chat(
                id,
                Some(project),
                Some(&core.device_id),
                Some(config),
                Some(cwd.display().to_string()),
            )?;
            core.task_identity.register_new_task(
                id,
                Some(project),
                None,
                cwd.to_str(),
                luna_proto::TaskLineage::default(),
            )?;
            core.workspace.rename_chat(id, title)?;
        }
    }
    std::fs::write(
        root.join("L25-FIXTURE.txt"),
        format!(
            "Synthetic L25 local send only; the pinned fake Codex executable is the source provider.\nsource={TASK_L25_SOURCE}\ntarget={TASK_L25_TARGET}\nforeign={TASK_L25_FOREIGN}\nsource_project=fixture-app\ntarget_project=fixture-app\nforeign_project=fixture-other\nprompt=scenario:l25-send:{TASK_L25_TARGET}:{TASK_L25_FOREIGN}\nexpected=three local approval cards; allow first and second, deny third. The second reuses the first command ID with changed text and must conflict. One accepted follow-up is visible in target Activity or queue, with no foreign or denied message. The queued receipt proves persistence only.\n"
        ),
    )?;
    Ok(())
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter("warn").init();
    let root = PathBuf::from(std::env::args().nth(1).expect("isolated fixture root"));
    let seed_action = std::env::args().nth(2);
    let l20_seed_only = seed_action.as_deref() == Some("--l20-seed-only");
    let l24_seed_only = seed_action.as_deref() == Some("--l24-seed-only");
    let l25_seed_only = seed_action.as_deref() == Some("--l25-seed-only");
    anyhow::ensure!(
        seed_action.is_none() || l20_seed_only || l24_seed_only || l25_seed_only,
        "unknown fixture seed-only action"
    );
    anyhow::ensure!(root.is_absolute(), "fixture root must be absolute");
    let l20 = match std::env::var(L20_OPT_IN) {
        Ok(value) if value == "1" => true,
        Ok(value) => anyhow::bail!("{L20_OPT_IN} must be 1 when set, got {value:?}"),
        Err(std::env::VarError::NotPresent) => false,
        Err(error) => return Err(error.into()),
    };
    let l24 = match std::env::var(L24_OPT_IN) {
        Ok(value) if value == "1" => true,
        Ok(value) => anyhow::bail!("{L24_OPT_IN} must be 1 when set, got {value:?}"),
        Err(std::env::VarError::NotPresent) => false,
        Err(error) => return Err(error.into()),
    };
    let l25 = match std::env::var(L25_OPT_IN) {
        Ok(value) if value == "1" => true,
        Ok(value) => anyhow::bail!("{L25_OPT_IN} must be 1 when set, got {value:?}"),
        Err(std::env::VarError::NotPresent) => false,
        Err(error) => return Err(error.into()),
    };
    let sidebar_scale = match std::env::var(SIDEBAR_SCALE_OPT_IN) {
        Ok(value) if value == "1" => true,
        Ok(value) => anyhow::bail!("{SIDEBAR_SCALE_OPT_IN} must be 1 when set, got {value:?}"),
        Err(std::env::VarError::NotPresent) => false,
        Err(error) => return Err(error.into()),
    };
    let hover_capture = match std::env::var(SIDEBAR_HOVER_CAPTURE_OPT_IN) {
        Ok(value) if value == "1" => true,
        Ok(value) => {
            anyhow::bail!("{SIDEBAR_HOVER_CAPTURE_OPT_IN} must be 1 when set, got {value:?}")
        }
        Err(std::env::VarError::NotPresent) => false,
        Err(error) => return Err(error.into()),
    };
    anyhow::ensure!(
        !hover_capture || sidebar_scale,
        "{SIDEBAR_HOVER_CAPTURE_OPT_IN} requires {SIDEBAR_SCALE_OPT_IN}=1"
    );
    anyhow::ensure!(
        !l20_seed_only || l20,
        "--l20-seed-only requires {L20_OPT_IN}=1"
    );
    anyhow::ensure!(
        !l24_seed_only || l24,
        "--l24-seed-only requires {L24_OPT_IN}=1"
    );
    anyhow::ensure!(
        !l25_seed_only || l25,
        "--l25-seed-only requires {L25_OPT_IN}=1"
    );
    if l20 || l24 || l25 {
        anyhow::ensure!(
            root.is_dir() && !std::fs::symlink_metadata(&root)?.file_type().is_symlink(),
            "opt-in fixture requires a pre-existing, non-symlink synthetic root"
        );
    } else {
        std::fs::create_dir_all(&root)?;
    }
    let root = root.canonicalize()?;
    if l20 || l24 || l25 {
        validate_opt_in_root(&root)?;
    }
    if l20 {
        // EngineCore and its RPC service must resolve the same managed root.
        unsafe {
            std::env::set_var("LUNA_WORKTREES_DIR", root.join("l20-worktrees"));
        }
    }
    let codex_executable = if l24 || l25 {
        validate_fake_codex()?
    } else {
        // A manual Check provider in the ordinary L12/L20 fixture must not
        // fall through to the user's installed Codex binary.
        root.join("synthetic-codex/no-provider-executable")
    };
    unsafe {
        std::env::set_var("CODEX_HOME", root.join("synthetic-codex"));
        std::env::set_var("CODEX_EXECUTABLE", &codex_executable);
    }
    let engine_data = root.join("engine");
    let ui_data = root.join("ui");
    let temp_data = root.join("tmp");
    std::fs::create_dir_all(&engine_data)?;
    std::fs::create_dir_all(&ui_data)?;
    std::fs::create_dir_all(&temp_data)?;
    unsafe {
        std::env::set_var("TMPDIR", &temp_data);
        std::env::set_var("TMP", &temp_data);
        std::env::set_var("TEMP", &temp_data);
    }

    let runtime = tokio::runtime::Runtime::new()?;
    let accounts = account_config(&root);
    accounts.disable_claude_keychain_fallback()?;
    let registry = Arc::new(HarnessRegistry::new());
    if l25 {
        registry.register(Arc::new(
            CodexHarness::new().with_executable(codex_executable.clone()),
        ));
    }
    let core = runtime.block_on(async {
        EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
            EngineProfile::development(&engine_data, "l12-fixture-org", "l12-fixture-user"),
            registry,
            HarnessId::Mock,
            None,
            accounts,
        )
    })?;
    // Attach hermetic auth before seed creates an in-memory RPC client and
    // before IPC/bootstrap can lazily request EngineCore::auth().
    core.set_auth(Auth::new(fixture_auth_config(&root)));
    runtime.block_on(seed(&core, &root))?;
    if sidebar_scale {
        runtime.block_on(seed_sidebar_scale(&core, &root))?;
    }
    if l20 {
        runtime.block_on(seed_l20(&core, &root))?;
    }
    if l25 {
        seed_l25(&core, &root)?;
    }
    let l24_newly_seeded = if l24 {
        runtime.block_on(seed_l24(&core, &root))?
    } else {
        false
    };
    if l24 && (l24_newly_seeded || l24_seed_only) {
        runtime.block_on(preflight_l24(&core, l24_newly_seeded))?;
    }
    let safety = if l20 {
        "L12 + opt-in L20 synthetic fixture only. Git checkpoints and restore targets stay under this root. Auth credential broker is in-memory; native provider metadata and platform Keychain fallbacks are disabled. No live provider/account/store access.\n"
    } else {
        "L12 synthetic fixture only. Auth credential broker is in-memory; native provider metadata and platform Keychain fallbacks are disabled. No live provider/account/store access.\n"
    };
    std::fs::write(
        root.join("SAFETY.txt"),
        if l24 || l25 {
            format!(
                "{safety}L24/L25 opt-in uses the pinned repository fake Codex executable and synthetic CODEX_HOME. No real Codex, Keychain, login, MCP server or remote edge is used.\n"
            )
        } else {
            safety.to_owned()
        },
    )?;
    if l20_seed_only || l24_seed_only || l25_seed_only {
        if l25_seed_only {
            let local = runtime.block_on(state::EngineHandle::fixture_from_core(&core))?;
            anyhow::ensure!(
                local.mode() == state::EngineMode::InProcess,
                "L25 preflight did not attach the local engine"
            );
            let outcome = runtime.block_on(local.respond_luna_local_ui_approval(
                TASK_L25_SOURCE,
                "missing-synthetic-approval",
                luna_proto::ApprovalDecision::Deny,
            ))?;
            anyhow::ensure!(
                outcome == luna_engine::sessions::LunaApprovalOutcome::Stale,
                "L25 local approval route did not reach the seeded SessionsEngine"
            );
            println!("l25_local_approval_route=ok source={TASK_L25_SOURCE}");
        }
        runtime.block_on(core.shutdown());
        return Ok(());
    }

    let ipc_port = std::net::TcpListener::bind(("127.0.0.1", 0))?
        .local_addr()?
        .port();
    let _ipc = runtime.block_on(luna_engine::serve_ipc(
        ipc_port,
        core.rpc_service(),
        &engine_data,
    ))?;
    let boot = EngineBootConfig {
        data_dir: ui_data.clone(),
        ipc_port,
        edge_url: String::new(),
        edge_token: None,
        org_id: None,
        workos_client_id: None,
        default_harness: HarnessId::Mock,
    };
    let handle = if l25 || sidebar_scale {
        runtime.block_on(state::EngineHandle::fixture_from_core(&core))?
    } else {
        runtime.block_on(state::EngineHandle::bootstrap(boot.clone()))?
    };
    let chats = core.workspace.read_chats()?;
    let spaces = core.workspace.read_spaces()?;
    let device = core.device_id.clone();
    let hover_capture_output = root.join("HOVER-QA");

    gpui_platform::application()
        .with_assets(icons::Assets)
        .run(move |cx| {
            gpui_tokio::init(cx);
            gpui_base::init(cx);
            let settings = settings::UiSettings::load(&ui_data);
            settings::init(settings.clone(), ui_data.clone(), cx);
            let fonts = typography::register_fonts(cx);
            typography::init(
                settings.ui_font_family.clone(),
                settings.ui_font_size,
                settings.terminal_font_family.clone(),
                settings.terminal_font_size,
                settings.code_font_family.clone(),
                settings.code_font_size,
                fonts,
                cx,
            );
            theme_library::init(ui_data.clone(), cx);
            appearance::init(
                appearance::AppearanceMode::Dark,
                settings.theme_selection,
                settings.accent,
                settings.surface,
                cx,
            );
            history::init(
                settings.git_history_columns,
                settings.git_history_column_widths,
                settings.git_history_column_order,
                settings.git_history_author_display,
                cx,
            );
            composer::init(cx, settings.composer_send_behavior);
            terminal::panel::init(cx);
            app_menus::init(cx);
            let state = cx.new(|cx| {
                let mut state = state::AppState::new();
                state.devices = vec![
                    serde_json::from_value(serde_json::json!({
                        "id": device,
                        "name": "Synthetic L12 Mac",
                        "platform": "macos",
                        "lastSeenAt": null
                    }))
                    .unwrap(),
                    serde_json::from_value(serde_json::json!({
                        "id": REMOTE_DEVICE,
                        "name": "Offline QA Mac",
                        "platform": "macos",
                        "lastSeenAt": "2024-01-01T00:00:00Z"
                    }))
                    .unwrap(),
                ];
                state.chats = chats;
                state.spaces = spaces;
                state.chats_synced = true;
                state.spaces_synced = true;
                state.select_chat(
                    Some(
                        if l25 {
                            TASK_L25_SOURCE
                        } else if l20 {
                            TASK_L20_ISOLATED
                        } else {
                            TASK_PINNED_A
                        }
                        .into(),
                    ),
                    cx,
                );
                state.fixture_attach_engine(handle, cx);
                state
            });
            let window = cx.open_window(
                WindowOptions {
                    window_background: theme::Theme::of(cx).window_background_appearance(),
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        gpui::point(px(80.), px(80.)),
                        size(px(1100.), px(850.)),
                    ))),
                    ..Default::default()
                },
                |_, cx| cx.new(|cx| shell::Shell::new(state.clone(), boot, cx)),
            )
            .unwrap();
            state.update(cx, |_, cx| cx.notify());
            cx.activate(true);
            if hover_capture {
                cx.spawn(async move |cx| {
                    let result: anyhow::Result<()> = async {
                        std::fs::create_dir_all(&hover_capture_output)?;
                        cx.background_executor().timer(Duration::from_millis(900)).await;
                        let window = AnyWindowHandle::from(window);
                        capture_hover_frame(window, cx, &hover_capture_output, "before.png")?;
                        window.update(cx, |_, window, cx| {
                            window.draw(cx).clear();
                            window.dispatch_event(
                                gpui::PlatformInput::MouseMove(gpui::MouseMoveEvent {
                                    position: gpui::point(px(100.0), px(SIDEBAR_HOVER_TARGET_Y)),
                                    ..Default::default()
                                }),
                                cx,
                            );
                        })?;
                        cx.background_executor().timer(Duration::from_millis(650)).await;
                        capture_hover_frame(window, cx, &hover_capture_output, "hover-650ms.png")?;
                        // Root frames continue while the sidebar row is
                        // cached; the fade's liveness clock must not erase a
                        // still-hovered row during those frames.
                        window.update(cx, |_, window, cx| {
                            for _ in 0..3 {
                                window.refresh();
                                window.draw(cx).clear();
                            }
                        })?;
                        cx.background_executor().timer(Duration::from_millis(3200)).await;
                        capture_hover_frame(window, cx, &hover_capture_output, "hover-3850ms.png")?;
                        let long_hover_cursor =
                            window.update(cx, |_, window, _| window.debug_cursor_style())?;
                        anyhow::ensure!(
                            long_hover_cursor == gpui::CursorStyle::PointingHand,
                            "cursor changed after stationary tooltip: {long_hover_cursor:?}"
                        );
                        window.update(cx, |_, window, cx| {
                            window.dispatch_event(
                                gpui::PlatformInput::MouseMove(gpui::MouseMoveEvent {
                                    position: gpui::point(px(170.0), px(SIDEBAR_HOVER_TARGET_Y)),
                                    ..Default::default()
                                }),
                                cx,
                            );
                        })?;
                        cx.background_executor().timer(Duration::from_millis(400)).await;
                        capture_hover_frame(window, cx, &hover_capture_output, "body-right.png")?;
                        window.update(cx, |_, window, cx| {
                            window.dispatch_event(
                                gpui::PlatformInput::MouseMove(gpui::MouseMoveEvent {
                                    position: gpui::point(px(190.0), px(SIDEBAR_HOVER_TARGET_Y)),
                                    ..Default::default()
                                }),
                                cx,
                            );
                        })?;
                        cx.background_executor().timer(Duration::from_millis(500)).await;
                        capture_hover_frame(window, cx, &hover_capture_output, "action.png")?;
                        for (name, x) in [("settle.png", 210.0), ("archive.png", 230.0)] {
                            window.update(cx, |_, window, cx| {
                                window.dispatch_event(
                                    gpui::PlatformInput::MouseMove(gpui::MouseMoveEvent {
                                        position: gpui::point(px(x), px(SIDEBAR_HOVER_TARGET_Y)),
                                        ..Default::default()
                                    }),
                                    cx,
                                );
                            })?;
                            cx.background_executor().timer(Duration::from_millis(450)).await;
                            capture_hover_frame(window, cx, &hover_capture_output, name)?;
                        }
                        window.update(cx, |_, window, cx| {
                            window.dispatch_event(
                                gpui::PlatformInput::MouseMove(gpui::MouseMoveEvent {
                                    position: gpui::point(px(600.0), px(SIDEBAR_HOVER_TARGET_Y)),
                                    ..Default::default()
                                }),
                                cx,
                            );
                        })?;
                        cx.background_executor().timer(Duration::from_millis(300)).await;
                        capture_hover_frame(window, cx, &hover_capture_output, "left.png")?;
                        window.update(cx, |_, window, cx| {
                            window.dispatch_event(
                                gpui::PlatformInput::MouseMove(gpui::MouseMoveEvent {
                                    position: gpui::point(px(100.0), px(SIDEBAR_HOVER_TARGET_Y)),
                                    ..Default::default()
                                }),
                                cx,
                            );
                        })?;
                        cx.background_executor().timer(Duration::from_millis(650)).await;
                        capture_hover_frame(window, cx, &hover_capture_output, "reenter.png")?;
                        for (name, y) in [
                            ("git-project.png", 295.0),
                            ("non-git-project.png", 425.0),
                            ("projectless.png", 490.0),
                        ] {
                            window.update(cx, |_, window, cx| {
                                window.dispatch_event(
                                    gpui::PlatformInput::MouseMove(gpui::MouseMoveEvent {
                                        position: gpui::point(px(100.0), px(y)),
                                        ..Default::default()
                                    }),
                                    cx,
                                );
                            })?;
                            cx.background_executor().timer(Duration::from_millis(450)).await;
                            capture_hover_frame(window, cx, &hover_capture_output, name)?;
                        }
                        let (pinned_failures, vertical_failures, horizontal_failures) =
                            window.update(cx, |_, window, cx| {
                                let mut pinned_failures = Vec::new();
                                for y in 130..=245 {
                                    window.dispatch_event(
                                        gpui::PlatformInput::MouseMove(gpui::MouseMoveEvent {
                                            position: gpui::point(px(100.0), px(y as f32)),
                                            ..Default::default()
                                        }),
                                        cx,
                                    );
                                    window.draw(cx).clear();
                                    let cursor = window.debug_cursor_style();
                                    if cursor != gpui::CursorStyle::PointingHand {
                                        pinned_failures.push(serde_json::json!({
                                            "y": y,
                                            "cursor": format!("{cursor:?}")
                                        }));
                                    }
                                }
                                let mut vertical_failures = Vec::new();
                                for y in 280..=520 {
                                    window.dispatch_event(
                                        gpui::PlatformInput::MouseMove(gpui::MouseMoveEvent {
                                            position: gpui::point(px(100.0), px(y as f32)),
                                            ..Default::default()
                                        }),
                                        cx,
                                    );
                                    window.draw(cx).clear();
                                    let cursor = window.debug_cursor_style();
                                    if cursor != gpui::CursorStyle::PointingHand {
                                        vertical_failures.push(serde_json::json!({
                                            "y": y,
                                            "cursor": format!("{cursor:?}")
                                        }));
                                    }
                                }
                                let mut horizontal_failures = Vec::new();
                                for x in 20..=235 {
                                    window.dispatch_event(
                                        gpui::PlatformInput::MouseMove(gpui::MouseMoveEvent {
                                            position: gpui::point(px(x as f32), px(360.0)),
                                            ..Default::default()
                                        }),
                                        cx,
                                    );
                                    window.draw(cx).clear();
                                    let cursor = window.debug_cursor_style();
                                    if cursor != gpui::CursorStyle::PointingHand {
                                        horizontal_failures.push(serde_json::json!({
                                            "x": x,
                                            "cursor": format!("{cursor:?}")
                                        }));
                                    }
                                }
                                (pinned_failures, vertical_failures, horizontal_failures)
                            })?;
                        std::fs::write(
                            hover_capture_output.join("cursor-sweep.json"),
                            serde_json::to_vec_pretty(&serde_json::json!({
                                "longHoverCursor": format!("{long_hover_cursor:?}"),
                                "expected": "PointingHand",
                                "pinned": {"x": 100, "yStart": 130, "yEnd": 245, "failures": pinned_failures},
                                "vertical": {"x": 100, "yStart": 280, "yEnd": 520, "failures": vertical_failures},
                                "horizontal": {"y": 360, "xStart": 20, "xEnd": 235, "failures": horizontal_failures},
                            }))?,
                        )?;
                        anyhow::ensure!(
                            pinned_failures.is_empty()
                                && vertical_failures.is_empty()
                                && horizontal_failures.is_empty(),
                            "sidebar cursor changed inside active thread list: pinned={pinned_failures:?} vertical={vertical_failures:?} horizontal={horizontal_failures:?}"
                        );
                        std::fs::write(
                            hover_capture_output.join("readback.json"),
                            serde_json::to_vec_pretty(&serde_json::json!({
                                "fixture": "sidebar-scale-v5",
                                "target": "Disconnected project task",
                                "pointerLogical": {"x": 100, "y": SIDEBAR_HOVER_TARGET_Y},
                                "forcedCachedFrames": 3,
                                "cursorSweep": "cursor-sweep.json",
                                "frames": ["before.png", "hover-650ms.png", "hover-3850ms.png", "body-right.png", "action.png", "settle.png", "archive.png", "left.png", "reenter.png", "git-project.png", "non-git-project.png", "projectless.png"],
                                "status": "captured"
                            }))?,
                        )?;
                        Ok(())
                    }
                    .await;
                    if let Err(error) = result {
                        eprintln!("sidebar hover capture failed: {error:#}");
                        let _ = std::fs::write(
                            hover_capture_output.join("ERROR.txt"),
                            format!("{error:#}\n"),
                        );
                    }
                })
                .detach();
            }
        });
    runtime.block_on(core.shutdown());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_auth_broker_is_hermetic_and_has_no_platform_fallback() {
        let root = tempfile::tempdir().unwrap();
        let config = fixture_auth_config(root.path());
        assert_eq!(config.data_dir, root.path().join("synthetic-auth"));
        assert_eq!(config.dev_user_id, "l12-fixture-user");

        let broker = config.test_credential_broker();
        assert!(
            broker.independent_in_memory_handle_for_tests().is_some(),
            "only the in-memory broker exposes an independent test handle"
        );
        assert!(
            broker.set_in_memory_store_locked_for_tests(true),
            "production/platform brokers reject this in-memory-only hook"
        );
        assert!(broker.set_in_memory_store_locked_for_tests(false));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn l20_opt_in_seed_uses_only_synthetic_registered_git_checkouts() {
        let temp = tempfile::Builder::new()
            .prefix("luna-l12-l20-seed-")
            .tempdir_in(OPT_IN_PARENT)
            .unwrap();
        let root = temp.path().canonicalize().unwrap();
        validate_opt_in_root(&root).unwrap();
        let accounts = account_config(&root);
        accounts.disable_claude_keychain_fallback().unwrap();
        let core =
            EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
                EngineProfile::development(
                    &root.join("engine"),
                    "l12-fixture-org",
                    "l12-fixture-user",
                ),
                Arc::new(HarnessRegistry::new()),
                HarnessId::Mock,
                None,
                accounts,
            )
            .unwrap();
        core.set_auth(Auth::new(fixture_auth_config(&root)));
        seed(&core, &root).await.unwrap();
        let repos = Repos::with_worktrees_root(
            &root.join("synthetic-repos"),
            &core.device_id,
            root.join("l20-worktrees"),
        );
        let checkpoints = GitCheckpoints::new(repos.clone());
        let seeded = seed_l20_with_services(&core, &root, &repos, &checkpoints)
            .await
            .unwrap();
        assert!(seeded.newly_seeded);
        assert!(
            core.workspace.chat(TASK_PINNED_A).unwrap().is_some(),
            "normal L12 seed must survive opt-in"
        );
        let shared = checkpoints
            .list(TASK_L20_SHARED, seeded.shared.to_str().unwrap(), false)
            .await
            .unwrap();
        let isolated = checkpoints
            .list(TASK_L20_ISOLATED, seeded.isolated.to_str().unwrap(), false)
            .await
            .unwrap();
        assert!(!shared.restore_available);
        assert!(isolated.restore_available);
        assert!(
            checkpoints
                .preview_restore(
                    TASK_L20_SHARED,
                    seeded.shared.to_str().unwrap(),
                    &shared.checkpoints[0].turn_id,
                    "before",
                    false
                )
                .await
                .unwrap_err()
                .contains("shared checkouts")
        );
        let preview = checkpoints
            .preview_restore(
                TASK_L20_ISOLATED,
                seeded.isolated.to_str().unwrap(),
                &isolated.checkpoints[0].turn_id,
                "before",
                false,
            )
            .await
            .unwrap();
        assert_eq!(
            preview.paths,
            ["post-edit.txt", "sample.txt", "turn-created.txt"]
        );
        let receipt = checkpoints
            .restore(
                luna_proto::GitCheckpointRestoreRequest {
                    chat_id: TASK_L20_ISOLATED.into(),
                    cwd: seeded.isolated.to_string_lossy().into_owned(),
                    turn_id: isolated.checkpoints[0].turn_id.clone(),
                    point: "before".into(),
                    expected_token: preview.token,
                },
                false,
                |_| false,
            )
            .await
            .unwrap();
        assert!(receipt.restored && receipt.recovery_ref.starts_with("refs/luna/checkpoints/"));
        assert_eq!(
            std::fs::read_to_string(seeded.isolated.join("sample.txt")).unwrap(),
            "before checkpoint\n"
        );
        assert!(!seeded.isolated.join("turn-created.txt").exists());
        assert!(!seeded.isolated.join("post-edit.txt").exists());
        assert!(
            fixture_git(&seeded.isolated, &["status", "--porcelain"])
                .unwrap()
                .is_empty()
        );
        let reopened = seed_l20_with_services(&core, &root, &repos, &checkpoints)
            .await
            .unwrap();
        assert!(!reopened.newly_seeded && reopened.isolated == seeded.isolated);
        assert_eq!(
            std::fs::read_to_string(seeded.isolated.join("sample.txt")).unwrap(),
            "before checkpoint\n"
        );
        core.shutdown().await;
    }
}
