//! Failure-first E2E: a vanished nested worktree must not turn its task's Git
//! card or a newly selected Git action into operations on an ancestor checkout.
//! The only Git mutation exercised is Stage in disposable local repositories.
#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use luna_engine::{EngineCore, HarnessRegistry};
use luna_proto::HarnessId;
use luna_rpc::methods;
use serde_json::{Value, json};

fn git(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .expect("git starts");
    assert!(
        output.status.success(),
        "git {args:?} in {}: {}",
        cwd.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("git output is UTF-8")
        .trim()
        .to_owned()
}

struct HeldGitMetadata {
    original: PathBuf,
    held: PathBuf,
}

impl HeldGitMetadata {
    fn move_to(original: PathBuf, held: PathBuf) -> Self {
        assert!(original.is_file(), "linked worktree uses a .git file");
        assert!(!held.exists());
        fs::rename(&original, &held).expect("hold only disposable inner .git file");
        Self { original, held }
    }
}

impl Drop for HeldGitMetadata {
    fn drop(&mut self) {
        if self.held.exists() {
            fs::rename(&self.held, &self.original)
                .expect("restore disposable linked worktree .git file");
        }
    }
}

fn has_file(preview: &Value, path: &str) -> bool {
    preview["files"]
        .as_array()
        .is_some_and(|files| files.iter().any(|file| file["path"] == path))
}

#[tokio::test]
async fn vanished_nested_worktree_cannot_preview_or_stage_ancestor_checkout() {
    let temp = tempfile::tempdir().expect("disposable Git fixture root");
    let root = temp.path().canonicalize().expect("canonical temp root");
    let outer = root.join("outer");
    let inner = outer.join("nested/linked");
    let outer_subdir = outer.join("subdir");
    fs::create_dir_all(&outer).unwrap();
    git(&outer, &["init", "-q", "-b", "main"]);
    fs::write(outer.join(".gitignore"), "nested/\n").unwrap();
    fs::write(outer.join("outside.txt"), "original\n").unwrap();
    git(&outer, &["add", "--", ".gitignore", "outside.txt"]);
    git(
        &outer,
        &[
            "-c",
            "user.name=QA",
            "-c",
            "user.email=qa@example.invalid",
            "commit",
            "-qm",
            "Initial disposable checkout",
        ],
    );
    git(
        &outer,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "inner",
            inner.to_str().unwrap(),
            "HEAD",
        ],
    );
    assert!(inner.join(".git").is_file());
    fs::create_dir_all(&outer_subdir).unwrap();
    fs::write(
        outer_subdir.join("legitimate.txt"),
        "subdirectory control\n",
    )
    .unwrap();
    fs::write(inner.join("inner-only.txt"), "linked worktree control\n").unwrap();
    fs::write(outer.join("outside.txt"), "must remain unstaged\n").unwrap();

    let core = EngineCore::assemble(
        &root.join("engine-data"),
        Arc::new(HarnessRegistry::new()),
        HarnessId::Mock,
        None,
    )
    .expect("real EngineCore assembles");
    let client = luna_rpc::memory_client(core.rpc_service());
    for (space_id, task_id, path) in [
        ("inner-space", "inner-task", &inner),
        ("outer-space", "outer-task", &outer_subdir),
    ] {
        client
            .call(
                methods::MUTATE,
                json!({
                    "op": "createSpace", "spaceId": space_id,
                    "deviceId": core.device_id, "path": path,
                    "gitDetected": true,
                }),
            )
            .await
            .expect("create disposable project through memory RPC");
        client
            .call(
                methods::MUTATE,
                json!({
                    "op": "createChat", "chatId": task_id,
                    "spaceId": space_id, "cwd": path,
                }),
            )
            .await
            .expect("create task through memory RPC");
    }

    // Control A: an intact nested linked worktree remains usable.
    let inner_id = core.repos.checkout_identity(&inner).await.unwrap().id;
    core.workspace
        .set_chat_checkout("inner-task", &inner_id)
        .expect("record actual linked checkout identity");
    let inner_preview = client
        .call(
            methods::GET_GIT_ACTION_PREVIEW,
            json!({
                "cwd": inner, "chatId": "inner-task",
                "expectedCheckoutId": inner_id,
            }),
        )
        .await
        .expect("intact linked worktree preview");
    assert_eq!(inner_preview["checkoutId"], inner_id);
    assert_eq!(inner_preview["cwd"], inner.to_str().unwrap());
    assert!(has_file(&inner_preview, "inner-only.txt"));
    let inner_task = core.workspace.chat("inner-task").unwrap().unwrap();
    assert_eq!(inner_task.cwd.as_deref(), inner.to_str());
    assert_eq!(inner_task.checkout_id.as_deref(), Some(inner_id.as_str()));
    let inner_receipt = client
        .call(
            methods::EXECUTE_GIT_ACTION,
            json!({
                "cwd": inner, "chatId": "inner-task",
                "expectedCheckoutId": inner_id,
                "checkoutId": inner_id,
                "expectedToken": inner_preview["token"],
                "kind": "stage", "paths": ["inner-only.txt"],
            }),
        )
        .await
        .expect("stage inside intact linked worktree");
    assert!(has_file(&inner_receipt["preview"], "inner-only.txt"));
    assert_eq!(
        git(&inner, &["diff", "--cached", "--name-only"]),
        "inner-only.txt"
    );
    assert!(!git(&outer, &["diff", "--cached", "--name-only"]).contains("outside.txt"));

    // Control B: a legitimate subdirectory of one checkout remains usable.
    let outer_id = core
        .repos
        .checkout_identity(&outer_subdir)
        .await
        .unwrap()
        .id;
    core.workspace
        .set_chat_checkout("outer-task", &outer_id)
        .expect("record actual outer checkout identity");
    let outer_preview = client
        .call(
            methods::GET_GIT_ACTION_PREVIEW,
            json!({
                "cwd": outer_subdir, "chatId": "outer-task",
                "expectedCheckoutId": outer_id,
            }),
        )
        .await
        .expect("ordinary subdirectory preview");
    assert_eq!(outer_preview["checkoutId"], outer_id);
    assert_ne!(inner_id, outer_id);
    assert_eq!(outer_preview["cwd"], outer.to_str().unwrap());
    assert!(has_file(&outer_preview, "subdir/legitimate.txt"));
    client
        .call(
            methods::EXECUTE_GIT_ACTION,
            json!({
                "cwd": outer_subdir, "chatId": "outer-task",
                "expectedCheckoutId": outer_id,
                "checkoutId": outer_id,
                "expectedToken": outer_preview["token"],
                "kind": "stage", "paths": ["subdir/legitimate.txt"],
            }),
        )
        .await
        .expect("stage from an ordinary repo subdirectory");
    assert_eq!(
        git(&outer, &["diff", "--cached", "--name-only"]),
        "subdir/legitimate.txt"
    );

    // A projectless legacy task in a subdirectory cannot infer provenance
    // from ancestry alone, but an explicit cwd selection binds it safely.
    client
        .call(
            methods::MUTATE,
            json!({
                "op": "createChat", "chatId": "projectless-subdir",
                "deviceId": core.device_id, "cwd": outer_subdir,
            }),
        )
        .await
        .expect("create projectless subdirectory task");
    assert!(
        client
            .call(
                methods::GET_GIT_ACTION_PREVIEW,
                json!({ "cwd": outer_subdir, "chatId": "projectless-subdir" }),
            )
            .await
            .is_err(),
        "unbound subdirectory must wait for explicit selection"
    );
    client
        .call(
            methods::MUTATE,
            json!({
                "op": "setChatCwd", "chatId": "projectless-subdir",
                "cwd": outer_subdir,
            }),
        )
        .await
        .expect("explicitly rebind projectless subdirectory task");
    assert_eq!(
        core.workspace
            .chat("projectless-subdir")
            .unwrap()
            .unwrap()
            .checkout_id
            .as_deref(),
        Some(outer_id.as_str())
    );
    assert_eq!(
        client
            .call(
                methods::GET_GIT_ACTION_PREVIEW,
                json!({ "cwd": outer_subdir, "chatId": "projectless-subdir" }),
            )
            .await
            .expect("bound projectless subdirectory preview")["checkoutId"],
        outer_id
    );

    // The inner task still points to its original checkout. Moving only its
    // synthetic .git file makes Git itself walk up to the outer repository.
    let held = HeldGitMetadata::move_to(inner.join(".git"), root.join("held-inner-git"));
    assert_eq!(
        git(&inner, &["rev-parse", "--show-toplevel"]),
        outer.to_str().unwrap()
    );
    client
        .call(
            methods::MUTATE,
            json!({
                "op": "createChat", "chatId": "unbound-inner-task",
                "deviceId": core.device_id, "cwd": inner,
            }),
        )
        .await
        .expect("create unbound projectless task after metadata loss");
    let unbound_preview = client
        .call(
            methods::GET_GIT_ACTION_PREVIEW,
            json!({ "cwd": inner, "chatId": "unbound-inner-task" }),
        )
        .await;
    let unbound_checkout = core
        .workspace
        .chat("unbound-inner-task")
        .unwrap()
        .unwrap()
        .checkout_id;
    let stale_inner_action = client
        .call(
            methods::EXECUTE_GIT_ACTION,
            json!({
                "cwd": inner, "chatId": "inner-task",
                "expectedCheckoutId": inner_id,
                "checkoutId": inner_id,
                "expectedToken": inner_receipt["preview"]["token"],
                "kind": "stage", "paths": ["outside.txt"],
            }),
        )
        .await;

    let unsafe_preview = client
        .call(
            methods::GET_GIT_ACTION_PREVIEW,
            json!({
                "cwd": inner, "chatId": "inner-task",
                "expectedCheckoutId": inner_id,
            }),
        )
        .await;
    // A pre-fix card may offer a newly selected outer checkout. Exercise its
    // accepted token only in this disposable repo to prove the action boundary.
    let newly_selected_action = if let Ok(preview) = &unsafe_preview {
        assert_eq!(preview["cwd"], outer.to_str().unwrap());
        assert!(has_file(preview, "outside.txt"));
        Some(
            client
                .call(
                    methods::EXECUTE_GIT_ACTION,
                    json!({
                        "cwd": inner, "chatId": "inner-task",
                        "expectedCheckoutId": inner_id,
                        "checkoutId": preview["checkoutId"],
                        "expectedToken": preview["token"],
                        "kind": "stage", "paths": ["outside.txt"],
                    }),
                )
                .await,
        )
    } else {
        None
    };
    let outer_staged_before_restore = git(&outer, &["diff", "--cached", "--name-only"]);
    eprintln!(
        "ancestry pre-fix observation: unsafe_preview={unsafe_preview:?}, \
         unbound_preview={unbound_preview:?}, unbound_checkout={unbound_checkout:?}, \
         stale_inner_action={stale_inner_action:?}, \
         newly_selected_action={newly_selected_action:?}, \
         outer_staged={outer_staged_before_restore:?}"
    );
    drop(held);
    assert!(inner.join(".git").is_file(), "linked metadata restored");
    assert_eq!(
        git(&inner, &["rev-parse", "--show-toplevel"]),
        inner.to_str().unwrap()
    );
    assert!(
        stale_inner_action.is_err(),
        "stale inner token must fail closed"
    );
    assert!(
        unbound_preview.is_err(),
        "unbound task must not adopt an ancestor"
    );
    assert!(
        unbound_checkout.is_none(),
        "diff-sync must not stamp the ancestor"
    );
    assert!(
        unsafe_preview.is_err(),
        "inner task must not preview ancestor checkout after its .git vanishes"
    );
    assert!(
        newly_selected_action.is_none_or(|result| result.is_err()),
        "newly selected outer token must not authorize inner task action"
    );
    assert!(
        !outer_staged_before_restore
            .lines()
            .any(|path| path == "outside.txt"),
        "inner task staged an outer-repository file"
    );
}
