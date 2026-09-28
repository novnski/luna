//! Failure-first memory-RPC PR preview: a nested task cannot silently switch
//! to an ancestor checkout when its linked-worktree .git file disappears.
//! All GitHub calls go to the local fake gh; no create request is sent.
#![cfg(unix)]

use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use luna_engine::{EngineCore, HarnessRegistry};
use luna_proto::HarnessId;
use luna_rpc::methods;
use serde_json::json;

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

struct FakeGhEnvironment {
    old_path: OsString,
    old_fixture: Option<OsString>,
    old_no_login_shell: Option<OsString>,
}

impl FakeGhEnvironment {
    fn install(root: &Path) -> Self {
        let fixture_bin = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../scripts/fixtures/github-pr")
            .canonicalize()
            .expect("checked-in fake gh fixture");
        assert!(fixture_bin.join("gh").is_file());
        let old_path = env::var_os("PATH").expect("git and python need PATH");
        let old_fixture = env::var_os("LUNA_PR_FIXTURE");
        let old_no_login_shell = env::var_os("LUNA_NO_LOGIN_SHELL");
        // SAFETY: this integration binary has one test and no provider process
        // starts until the fake executable is the first PATH entry.
        unsafe {
            env::set_var(
                "PATH",
                format!("{}:{}", fixture_bin.display(), old_path.to_string_lossy()),
            );
            env::set_var("LUNA_PR_FIXTURE", root);
            env::set_var("LUNA_NO_LOGIN_SHELL", "1");
        }
        Self {
            old_path,
            old_fixture,
            old_no_login_shell,
        }
    }
}

impl Drop for FakeGhEnvironment {
    fn drop(&mut self) {
        // SAFETY: only this test mutates the process environment.
        unsafe {
            env::set_var("PATH", &self.old_path);
            match &self.old_fixture {
                Some(value) => env::set_var("LUNA_PR_FIXTURE", value),
                None => env::remove_var("LUNA_PR_FIXTURE"),
            }
            match &self.old_no_login_shell {
                Some(value) => env::set_var("LUNA_NO_LOGIN_SHELL", value),
                None => env::remove_var("LUNA_NO_LOGIN_SHELL"),
            }
        }
    }
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

#[tokio::test]
async fn vanished_nested_worktree_cannot_preview_ancestor_pull_request() {
    let temp = tempfile::tempdir().expect("disposable PR fixture root");
    let root = temp.path().canonicalize().unwrap();
    let outer = root.join("outer");
    let inner = outer.join("nested/linked");
    fs::create_dir_all(&outer).unwrap();
    git(&outer, &["init", "-q", "-b", "main"]);
    fs::write(outer.join(".gitignore"), "nested/\n").unwrap();
    fs::write(outer.join("README.md"), "disposable PR fixture\n").unwrap();
    git(&outer, &["add", "--", ".gitignore", "README.md"]);
    git(
        &outer,
        &[
            "-c",
            "user.name=QA",
            "-c",
            "user.email=qa@example.invalid",
            "commit",
            "-qm",
            "Initial disposable PR checkout",
        ],
    );
    git(&outer, &["checkout", "-qb", "outer-feature"]);
    git(
        &outer,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "inner-feature",
            inner.to_str().unwrap(),
            "HEAD",
        ],
    );
    git(
        &outer,
        &["remote", "add", "origin", "git@github.com:acme/luna.git"],
    );
    let sha = git(&outer, &["rev-parse", "HEAD"]);
    for branch in ["outer-feature", "inner-feature"] {
        git(
            &outer,
            &["update-ref", &format!("refs/remotes/origin/{branch}"), &sha],
        );
        git(
            &outer,
            &["config", &format!("branch.{branch}.remote"), "origin"],
        );
        git(
            &outer,
            &[
                "config",
                &format!("branch.{branch}.merge"),
                &format!("refs/heads/{branch}"),
            ],
        );
    }
    git(&outer, &["update-ref", "refs/remotes/origin/main", &sha]);
    fs::write(root.join("mode"), "success").unwrap();
    fs::write(root.join("sha"), &sha).unwrap();
    let _fake_gh = FakeGhEnvironment::install(&root);

    let core = EngineCore::assemble(
        &root.join("engine-data"),
        Arc::new(HarnessRegistry::new()),
        HarnessId::Mock,
        None,
    )
    .expect("real EngineCore assembles");
    core.workspace
        .create_space(
            "space-pr-inner",
            &core.device_id,
            inner.to_str().unwrap(),
            None,
            true,
        )
        .expect("create project");
    let client = luna_rpc::memory_client(core.rpc_service());
    client
        .call(
            methods::MUTATE,
            json!({
                "op": "createChat", "chatId": "pr-inner-task",
                "spaceId": "space-pr-inner", "cwd": inner,
            }),
        )
        .await
        .expect("create project task through memory RPC");
    let inner_identity = core.repos.checkout_identity(&inner).await.unwrap();
    core.workspace
        .set_chat_checkout("pr-inner-task", &inner_identity.id)
        .expect("record trusted task checkout identity");
    let task = core.workspace.chat("pr-inner-task").unwrap().unwrap();
    assert_eq!(task.cwd.as_deref(), inner.to_str());
    assert_eq!(
        task.checkout_id.as_deref(),
        Some(inner_identity.id.as_str())
    );

    let params = json!({
        "cwd": inner, "chatId": "pr-inner-task",
        "expectedCheckoutId": inner_identity.id,
    });
    let intact = client
        .call(methods::GET_GITHUB_PR_PREVIEW, params.clone())
        .await
        .expect("intact linked checkout PR preview via fake gh");
    assert_eq!(intact["head"], "inner-feature");
    assert_eq!(intact["checkout"]["cwd"], inner.to_str().unwrap());
    assert_eq!(intact["checkout"]["checkoutId"], inner_identity.id);
    assert_eq!(intact["repository"], "github.com/acme/luna");
    assert!(
        root.join("calls.jsonl").is_file(),
        "fake gh handled the reads"
    );
    assert!(
        !root.join("writes.jsonl").exists(),
        "no synthetic POST before stale preview"
    );

    let held = HeldGitMetadata::move_to(inner.join(".git"), root.join("held-inner-git"));
    assert_eq!(
        git(&inner, &["rev-parse", "--show-toplevel"]),
        outer.to_str().unwrap()
    );
    let stale = client.call(methods::GET_GITHUB_PR_PREVIEW, params).await;
    let fake_calls = fs::read_to_string(root.join("calls.jsonl")).unwrap();
    let synthetic_posts = fs::read_to_string(root.join("writes.jsonl"))
        .unwrap_or_default()
        .lines()
        .count();
    eprintln!(
        "PR ancestry pre-fix observation: stale={stale:?}, fake_read_calls={}, synthetic_posts={synthetic_posts}",
        fake_calls.lines().count()
    );
    drop(held);
    assert!(inner.join(".git").is_file(), "linked metadata restored");
    assert_eq!(
        git(&inner, &["rev-parse", "--show-toplevel"]),
        inner.to_str().unwrap()
    );
    assert_eq!(synthetic_posts, 0, "no PR create POST, even to fake gh");
    assert!(
        stale.is_err(),
        "inner task must not preview ancestor PR context after its .git vanishes"
    );
}
