//! Filesystem/Git regressions for the workspace audit. Every root is disposable.
//! Failure modes (recorded before implementation):
//! - Short upload IDs and sanitized names alias a previously published attachment.
//! - A replay with different bytes or a concurrent collision replaces durable bytes.
//! - A preexisting symlink is adopted as a successful attachment publication.
//! - Replaying a generated-image identity after its source changes rewrites history.
//! - HEAD-seeded checkpoints omit force-added ignored files and fail to detect edits.
//! - Restoring such a checkpoint loses the worktree version of a staged ignored file.
//! - Failed Git worktree removal falls back to deleting unrelated/locked directories.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use luna_engine::{Repos, git_checkpoints::GitCheckpoints, uploads::Uploads};
use luna_proto::GitCheckpointRestoreRequest;

fn stage(uploads: &Uploads, id: &str, bytes: &[u8]) {
    uploads.append(id, &BASE64.encode(bytes), Some(0)).unwrap();
}

#[test]
fn committed_upload_collisions_preserve_original_and_staging() {
    let dir = tempfile::tempdir().unwrap();
    let uploads = Uploads::new(dir.path());
    stage(&uploads, "samehead-original", b"original");
    let original = uploads.commit("samehead-original", "image?.png").unwrap();
    stage(&uploads, "samehead-collision", b"replacement");
    let result = uploads.commit("samehead-collision", "image*.png");
    assert!(
        result.is_err(),
        "colliding publication replaced {original}: {result:?}"
    );
    assert_eq!(std::fs::read(&original).unwrap(), b"original");
    let retried = uploads
        .commit("samehead-collision", "different.png")
        .unwrap();
    assert_eq!(std::fs::read(retried).unwrap(), b"replacement");

    // Replayed immutable bytes are safe; a changed replay must fail closed.
    stage(&uploads, "samehead-original", b"original");
    assert_eq!(
        uploads.commit("samehead-original", "image?.png").unwrap(),
        original
    );
    stage(&uploads, "samehead-original", b"changed replay");
    assert!(uploads.commit("samehead-original", "image?.png").is_err());
    assert_eq!(std::fs::read(original).unwrap(), b"original");
}

#[test]
fn concurrent_upload_collision_publishes_only_one_payload() {
    let dir = tempfile::tempdir().unwrap();
    let uploads = Uploads::new(dir.path());
    stage(&uploads, "parallel-one", b"one");
    stage(&uploads, "parallel-two", b"two");
    let barrier = Arc::new(Barrier::new(2));
    let workers: Vec<_> = ["parallel-one", "parallel-two"]
        .into_iter()
        .map(|id| {
            let uploads = uploads.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                uploads.commit(id, "image.png")
            })
        })
        .collect();
    let results: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect();
    assert_eq!(
        results.iter().filter(|result| result.is_ok()).count(),
        1,
        "{results:?}"
    );
    let winner = results.iter().position(Result::is_ok).unwrap();
    assert_eq!(
        std::fs::read(results[winner].as_ref().unwrap()).unwrap(),
        [b"one", b"two"][winner]
    );
}

#[cfg(unix)]
#[test]
fn upload_publication_rejects_existing_symlink() {
    let dir = tempfile::tempdir().unwrap();
    let uploads = Uploads::new(dir.path());
    let outside = dir.path().join("outside.png");
    std::fs::write(&outside, b"same bytes").unwrap();
    stage(&uploads, "symlink-id", b"same bytes");
    let target = uploads.pending_target("symlink-id", "image.png");
    std::os::unix::fs::symlink(&outside, &target).unwrap();
    assert!(uploads.commit("symlink-id", "image.png").is_err());
    assert!(
        std::fs::symlink_metadata(target)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(std::fs::read(outside).unwrap(), b"same bytes");
}

#[test]
fn generated_image_replay_cannot_replace_published_history() {
    let temp = tempfile::tempdir().unwrap();
    let source_root = temp.path().join("provider");
    std::fs::create_dir(&source_root).unwrap();
    let source = source_root.join("image.png");
    let original = b"\x89PNG\r\n\x1a\noriginal";
    std::fs::write(&source, original).unwrap();
    let uploads = Uploads::new(&temp.path().join("data"));
    let published = uploads
        .import_generated_image(&source, &source_root, "chat-image-key")
        .unwrap();
    assert_eq!(
        uploads
            .import_generated_image(&source, &source_root, "chat-image-key")
            .unwrap(),
        published
    );
    std::fs::write(&source, b"\x89PNG\r\n\x1a\nmodified").unwrap();
    assert!(
        uploads
            .import_generated_image(&source, &source_root, "chat-image-key")
            .is_err()
    );
    assert_eq!(std::fs::read(published.path).unwrap(), original);
    assert_eq!(std::fs::read_dir(uploads.dir()).unwrap().count(), 1);
}

fn git(root: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "LUNA QA")
        .env("GIT_AUTHOR_EMAIL", "qa@example.invalid")
        .env("GIT_COMMITTER_NAME", "LUNA QA")
        .env("GIT_COMMITTER_EMAIL", "qa@example.invalid")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn checkpoint_fixture() -> (tempfile::TempDir, GitCheckpoints, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join(".gitignore"), "ignored.txt\n").unwrap();
    git(&repo, &["add", ".gitignore"]);
    git(&repo, &["commit", "-q", "-m", "base"]);
    let managed = temp.path().join("managed");
    std::fs::create_dir(&managed).unwrap();
    let worktree = managed.join("task");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "task",
            worktree.to_str().unwrap(),
        ],
    );
    let repos = Repos::with_worktrees_root(&temp.path().join("data"), "qa", managed);
    std::fs::write(worktree.join("ignored.txt"), "staged\n").unwrap();
    git(&worktree, &["add", "-f", "ignored.txt"]);
    std::fs::write(worktree.join("ignored.txt"), "before\n").unwrap();
    (temp, GitCheckpoints::new(repos), worktree)
}

#[tokio::test]
async fn checkpoints_capture_and_restore_force_added_ignored_files() {
    let (_temp, checkpoints, root) = checkpoint_fixture();
    let cwd = root.to_str().unwrap();
    let original_index = git(&root, &["write-tree"]);
    let original_head = git(&root, &["rev-parse", "HEAD"]);
    checkpoints.start("qa", "run", cwd).await;
    std::fs::write(root.join("ignored.txt"), "after\n").unwrap();
    checkpoints.end("qa", "run", "completed").await;
    assert_eq!(git(&root, &["write-tree"]), original_index);
    let turn = checkpoints
        .list("qa", cwd, false)
        .await
        .unwrap()
        .checkpoints[0]
        .turn_id
        .clone();
    let diff = checkpoints.diff("qa", cwd, &turn).await.unwrap();
    assert_eq!(diff.paths, ["ignored.txt"]);
    assert!(
        diff.patch.contains("-before") && diff.patch.contains("+after"),
        "{}",
        diff.patch
    );
    let preview = checkpoints
        .preview_restore("qa", cwd, &turn, "before", false)
        .await
        .unwrap();
    assert_eq!(preview.paths, ["ignored.txt"]);
    checkpoints
        .restore(
            GitCheckpointRestoreRequest {
                chat_id: "qa".into(),
                cwd: cwd.into(),
                turn_id: turn,
                point: "before".into(),
                expected_token: preview.token,
            },
            false,
            |_| false,
        )
        .await
        .unwrap();
    assert_eq!(
        std::fs::read(root.join("ignored.txt")).unwrap(),
        b"before\n"
    );
    assert_eq!(git(&root, &["show", ":ignored.txt"]), "staged");
    assert_eq!(git(&root, &["write-tree"]), original_index);
    assert_eq!(git(&root, &["rev-parse", "HEAD"]), original_head);
}

#[tokio::test]
async fn checkpoint_restore_rejects_ignored_file_edits_after_preview() {
    let (_temp, checkpoints, root) = checkpoint_fixture();
    let cwd = root.to_str().unwrap();
    checkpoints.start("qa", "run", cwd).await;
    checkpoints.end("qa", "run", "completed").await;
    let turn = checkpoints
        .list("qa", cwd, false)
        .await
        .unwrap()
        .checkpoints[0]
        .turn_id
        .clone();
    let preview = checkpoints
        .preview_restore("qa", cwd, &turn, "before", false)
        .await
        .unwrap();
    std::fs::write(root.join("ignored.txt"), "concurrent edit\n").unwrap();
    let error = checkpoints
        .restore(
            GitCheckpointRestoreRequest {
                chat_id: "qa".into(),
                cwd: cwd.into(),
                turn_id: turn,
                point: "before".into(),
                expected_token: preview.token,
            },
            false,
            |_| false,
        )
        .await
        .unwrap_err();
    assert!(error.contains("Checkout changed after"), "{error}");
    assert_eq!(
        std::fs::read(root.join("ignored.txt")).unwrap(),
        b"concurrent edit\n"
    );
}

#[tokio::test]
async fn refused_worktree_deletion_preserves_unregistered_directory() {
    refused_worktree_deletion("unregistered").await;
}

#[tokio::test]
async fn refused_worktree_deletion_preserves_primary_checkout() {
    refused_worktree_deletion("primary").await;
}

#[tokio::test]
async fn refused_worktree_deletion_preserves_locked_worktree() {
    refused_worktree_deletion("locked").await;
}

async fn refused_worktree_deletion(scenario: &str) {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "luna/primary"]);
    std::fs::write(repo.join("keep.txt"), "primary data\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "base"]);
    let repos =
        Repos::with_worktrees_root(&temp.path().join("data"), "qa", temp.path().join("managed"));
    let target = match scenario {
        "unregistered" => {
            let folder = temp.path().join("unrelated-project");
            std::fs::create_dir(&folder).unwrap();
            std::fs::write(folder.join("keep.txt"), "unrelated data\n").unwrap();
            folder
        }
        "primary" => repo.clone(),
        "locked" => {
            let worktree = repos.create_worktree(&repo, "HEAD").await.unwrap();
            git(&repo, &["worktree", "lock", &worktree.path]);
            PathBuf::from(worktree.path)
        }
        _ => unreachable!(),
    };
    let branches = git(&repo, &["branch", "--format=%(refname)"]);
    let original = std::fs::read(target.join("keep.txt")).unwrap();
    let result = repos.delete_worktree(&repo, &target).await;
    assert!(
        result.is_err(),
        "{scenario}: Git refusal was discarded: {result:?}"
    );
    assert_eq!(
        std::fs::read(target.join("keep.txt")).unwrap(),
        original,
        "{scenario}"
    );
    assert_eq!(
        git(&repo, &["branch", "--format=%(refname)"]),
        branches,
        "{scenario}"
    );
}
