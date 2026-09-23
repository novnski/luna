//! Explicit Git actions for the checkout shown in Changes. All operations run
//! on the owning device; LUNA actions on one checkout share a mutation lane.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;
use zeron_proto::{
    GitActionKind, GitActionPreview, GitActionReceipt, GitActionRequest, GitFileState,
    GitMessageRequest, GitPrStatus, GitPrStatusRequest,
};

use crate::Repos;
use crate::source_control::{
    BranchHeadContext, ChangeRequestError, ChangeRequestResolver, CheckoutSourceContext,
    parse_git_remote,
};

const OUTPUT_LIMIT: usize = 1024 * 1024;
const FILE_LIMIT: u64 = 32 * 1024 * 1024;
const TOTAL_FILE_LIMIT: u64 = 64 * 1024 * 1024;
const FILE_COUNT_LIMIT: usize = 500;

#[derive(Clone)]
pub struct GitActions {
    repos: Repos,
    lanes: Arc<Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>>,
    push_timeout_seconds: u64,
}

impl GitActions {
    pub fn new(repos: Repos) -> Self {
        Self {
            repos,
            lanes: Arc::default(),
            push_timeout_seconds: 120,
        }
    }

    pub(crate) fn lane(&self, root: &Path) -> Arc<tokio::sync::Mutex<()>> {
        self.lanes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(root.to_path_buf())
            .or_default()
            .clone()
    }

    pub async fn preview(&self, cwd: &str) -> Result<GitActionPreview, String> {
        let identity = self
            .repos
            .checkout_identity(Path::new(cwd))
            .await
            .map_err(|_| "Git checkout is unavailable".to_string())?;
        let _guard = self.lane(&identity.root).lock_owned().await;
        inspect(&identity.root, &identity.id).await
    }

    pub async fn staged_context(&self, request: GitMessageRequest) -> Result<String, String> {
        let identity = self
            .repos
            .checkout_identity(Path::new(&request.cwd))
            .await
            .map_err(|_| "Git checkout is unavailable".to_string())?;
        let _guard = self.lane(&identity.root).lock_owned().await;
        let snapshot = inspect(&identity.root, &identity.id).await?;
        validate(&snapshot, &request.checkout_id, &request.expected_token)?;
        selected_staged(&snapshot, &request.paths)?;
        let mut args = vec![
            "diff".to_string(),
            "--cached".into(),
            "--no-ext-diff".into(),
            "--".into(),
        ];
        args.extend(request.paths.iter().map(|path| literal_path(path)));
        let diff = git(&identity.root, &args, 30).await?;
        if diff.len() > 24 * 1024 {
            return Err("Staged diff is too large to generate a message".into());
        }
        Ok(String::from_utf8(diff).map_err(|_| "Staged diff is not UTF-8".to_string())?)
    }

    /// Resolve a link/status through Zeron's existing GitHub provider. The
    /// selected push remote owns this card's PR context; an unrelated tracking
    /// remote must not silently supply a different repository's PR.
    pub async fn pr_status(&self, request: GitPrStatusRequest) -> Result<GitPrStatus, String> {
        let before = self.preview(&request.cwd).await?;
        validate(&before, &request.checkout_id, &request.expected_token)?;
        let mut selected_url = None;
        let result = match before.remote.as_deref() {
            None => GitPrStatus::UnsupportedHost,
            Some(remote) => {
                let root = Path::new(&before.cwd);
                match git_string(root, &["remote", "get-url", "--push", remote]).await {
                    Err(_) => GitPrStatus::Unavailable {
                        reason: "Push remote is unavailable".into(),
                    },
                    Ok(url) if parse_git_remote(&url).is_none() => {
                        selected_url = Some(url);
                        GitPrStatus::UnsupportedHost
                    }
                    Ok(url) => {
                        selected_url = Some(url.clone());
                        let resolver = ChangeRequestResolver::new();
                        match resolver.inspect_checkout(root).await {
                            Err(error) => pr_lookup_error(error),
                            Ok(source) => {
                                match pr_source_for_push(source, &before.branch, remote, &url) {
                                    None => GitPrStatus::Unavailable {
                                        reason: "PR lookup for this push remote is unavailable; configure the branch tracking remote".into(),
                                    },
                                    Some(source) => match resolver.resolve_github_source(&source).await {
                                        Ok(Some(change_request)) => GitPrStatus::Linked { change_request },
                                        Ok(None) => GitPrStatus::NoMatch,
                                        Err(error) => pr_lookup_error(error),
                                    }
                                }
                            }
                        }
                    }
                }
            }
        };
        let after = self.preview(&request.cwd).await?;
        validate_pr_context(&after, &request)?;
        if let Some(remote) = after.remote.as_deref()
            && let Some(url) = selected_url.as_deref()
            && git_string(
                Path::new(&after.cwd),
                &["remote", "get-url", "--push", remote],
            )
            .await
            .ok()
            .as_deref()
                != Some(url)
        {
            return Err("Push remote changed; refresh the preview".into());
        }
        Ok(result)
    }

    pub async fn execute(&self, request: GitActionRequest) -> Result<GitActionReceipt, String> {
        let identity = self
            .repos
            .checkout_identity(Path::new(&request.cwd))
            .await
            .map_err(|_| "Git checkout is unavailable".to_string())?;
        let _guard = self.lane(&identity.root).lock_owned().await;
        let before = inspect(&identity.root, &identity.id).await?;
        validate(&before, &request.checkout_id, &request.expected_token)?;
        let wants_push = matches!(
            request.kind,
            GitActionKind::Push | GitActionKind::CommitAndPush
        );
        let push_target = if wants_push {
            if before.branch == "HEAD" {
                return Err("Cannot push a detached HEAD".into());
            }
            let remote = before
                .remote
                .as_deref()
                .ok_or("Choose a push remote in Git first")?;
            if request.remote.as_deref() != Some(remote) {
                return Err("Push remote changed; refresh the preview".into());
            }
            Some(
                git_string(&identity.root, &["remote", "get-url", "--push", remote])
                    .await
                    .map_err(|_| "Push remote is unavailable".to_string())?,
            )
        } else {
            None
        };
        let mut committed = false;
        let mut pushed = false;
        let mut push_uncertain = false;
        let mut commit_sha = None;
        let mut error = None;
        match request.kind {
            GitActionKind::Stage | GitActionKind::Unstage => {
                if request.paths.is_empty() {
                    return Err("Choose files first".into());
                }
                let selected: HashSet<_> = request.paths.iter().collect();
                if selected.len() != request.paths.len() {
                    return Err("Duplicate file selection".into());
                }
                for path in &request.paths {
                    let Some(file) = before.files.iter().find(|file| file.path == *path) else {
                        return Err("Selected files changed; refresh the preview".into());
                    };
                    if matches!(request.kind, GitActionKind::Stage) && !file.unstaged
                        || matches!(request.kind, GitActionKind::Unstage) && !file.staged
                    {
                        return Err("Selected files changed; refresh the preview".into());
                    }
                }
                let mut args = if matches!(request.kind, GitActionKind::Stage) {
                    vec!["add".to_string(), "--".into()]
                } else if before.head == "UNBORN" {
                    vec![
                        "rm".to_string(),
                        "--cached".into(),
                        "-q".into(),
                        "--".into(),
                    ]
                } else {
                    vec!["restore".to_string(), "--staged".into(), "--".into()]
                };
                args.extend(request.paths.iter().map(|path| literal_path(path)));
                git_mutation(&identity.root, &args, 30)
                    .await
                    .map_err(|error| error.to_string())?;
            }
            GitActionKind::Commit | GitActionKind::CommitAndPush => {
                selected_staged(&before, &request.paths)?;
                let message = request.message.as_deref().map(str::trim).unwrap_or("");
                if message.is_empty() || message.len() > 4096 || message.contains('\0') {
                    return Err("Enter a commit message under 4096 characters".into());
                }
                let mut args = vec![
                    "commit".to_string(),
                    "--only".into(),
                    "-m".into(),
                    message.into(),
                    "--".into(),
                ];
                args.extend(request.paths.iter().map(|path| literal_path(path)));
                if git_mutation(&identity.root, &args, 120).await.is_err() {
                    if let Ok(after) = git_string(&identity.root, &["rev-parse", "HEAD"]).await
                        && after != before.head
                    {
                        let preview = inspect(&identity.root, &identity.id)
                            .await
                            .unwrap_or_else(|_| before.clone());
                        return Ok(GitActionReceipt {
                            commit_sha: Some(after),
                            committed: false,
                            pushed: false,
                            push_uncertain: false,
                            error: Some("Commit outcome is uncertain: HEAD changed while Git reported a failure. Inspect the commit before retrying.".into()),
                            preview,
                        });
                    }
                    return Err(
                        "Git commit failed; check identity, hooks, and changed files".into(),
                    );
                }
                committed = true;
                commit_sha = match git_string(&identity.root, &["rev-parse", "HEAD"]).await {
                    Ok(sha) => Some(sha),
                    Err(_) => {
                        return Ok(GitActionReceipt {
                            commit_sha: None,
                            committed: true,
                            pushed: false,
                            push_uncertain: false,
                            error: Some("Commit succeeded, but HEAD could not be read. Refresh before pushing.".into()),
                            preview: before,
                        });
                    }
                };
            }
            GitActionKind::Push => {}
        }
        if wants_push {
            let current = match inspect(&identity.root, &identity.id).await {
                Ok(current) => current,
                Err(_) if committed => {
                    return Ok(GitActionReceipt {
                        commit_sha,
                        committed: true,
                        pushed: false,
                        push_uncertain: false,
                        error: Some("Commit succeeded, but checkout status could not be refreshed. Refresh before pushing.".into()),
                        preview: before,
                    });
                }
                Err(error) => return Err(error),
            };
            let remote = before.remote.as_deref().unwrap();
            let current_target =
                git_string(&identity.root, &["remote", "get-url", "--push", remote])
                    .await
                    .ok();
            if current.branch != before.branch
                || current.remote != before.remote
                || current_target.as_deref() != push_target.as_deref()
            {
                error = Some(if committed {
                    "Commit succeeded; branch or push remote changed. Refresh before pushing."
                        .into()
                } else {
                    "Branch or push remote changed; refresh before pushing.".into()
                });
            } else {
                let refspec = format!("HEAD:refs/heads/{}", before.branch);
                let args = ["push", "--porcelain", "--", remote, &refspec];
                match git_mutation(&identity.root, &args, self.push_timeout_seconds).await {
                    Ok(_) => pushed = true,
                    Err(failure) if failure.push_outcome_uncertain() => {
                        push_uncertain = true;
                        error = Some(if committed {
                            "Commit succeeded; push outcome is uncertain. Check the remote branch before retrying Push.".into()
                        } else {
                            "Push outcome is uncertain. Check the remote branch before retrying Push.".into()
                        });
                    }
                    Err(_) => {
                        error = Some(if committed {
                            "Commit succeeded; push failed. Retry Push without committing again."
                                .into()
                        } else {
                            "Push failed; no commit was created.".into()
                        })
                    }
                }
            }
        }
        let preview = match inspect(&identity.root, &identity.id).await {
            Ok(preview) => preview,
            Err(_) => {
                let notice = "Git action finished, but checkout status could not be refreshed. Refresh before the next action.";
                error = Some(match error {
                    Some(previous) => format!("{previous} {notice}"),
                    None => notice.into(),
                });
                before
            }
        };
        Ok(GitActionReceipt {
            commit_sha,
            committed,
            pushed,
            push_uncertain,
            error,
            preview,
        })
    }
}

fn pr_lookup_error(error: ChangeRequestError) -> GitPrStatus {
    match error {
        ChangeRequestError::UnsupportedRepository => GitPrStatus::UnsupportedHost,
        other => GitPrStatus::Unavailable {
            reason: other.to_string(),
        },
    }
}

fn validate_pr_context(
    preview: &GitActionPreview,
    request: &GitPrStatusRequest,
) -> Result<(), String> {
    if preview.checkout_id != request.checkout_id
        || preview.branch != request.branch
        || preview.remote != request.remote
    {
        Err("Checkout branch or remote changed; refresh the preview".into())
    } else {
        Ok(())
    }
}

fn pr_source_for_push(
    mut source: CheckoutSourceContext,
    branch: &str,
    remote: &str,
    url: &str,
) -> Option<CheckoutSourceContext> {
    // The existing gh resolver runs inside the checkout. If its selected
    // repository differs from Push, it cannot prove which repository it queried.
    if source.branch.remote_name.as_deref() != Some(remote) {
        return None;
    }
    // Push uses HEAD:refs/heads/<local branch>. An upstream with a different
    // name must not link this card to that other branch's PR.
    source.branch = BranchHeadContext::resolve(branch, None, Some(remote), Some(url));
    Some(source)
}

fn validate(snapshot: &GitActionPreview, checkout_id: &str, token: &str) -> Result<(), String> {
    if snapshot.checkout_id != checkout_id || snapshot.token != token {
        Err("Checkout changed; refresh the preview before acting".into())
    } else {
        Ok(())
    }
}

fn selected_staged(snapshot: &GitActionPreview, paths: &[String]) -> Result<(), String> {
    if paths.is_empty() {
        return Err("Choose staged files first".into());
    }
    let mut seen = HashSet::new();
    for path in paths {
        if !seen.insert(path) {
            return Err("Duplicate file selection".into());
        }
        let Some(file) = snapshot.files.iter().find(|file| file.path == *path) else {
            return Err("Selected files changed; refresh the preview".into());
        };
        if !file.staged || file.unstaged {
            return Err("Stage the current version of selected files before committing".into());
        }
    }
    Ok(())
}

async fn inspect(root: &Path, checkout_id: &str) -> Result<GitActionPreview, String> {
    let branch = git_string(root, &["symbolic-ref", "--quiet", "--short", "HEAD"])
        .await
        .unwrap_or_else(|_| "HEAD".into());
    let head = git_string(root, &["rev-parse", "--verify", "HEAD"])
        .await
        .unwrap_or_else(|_| "UNBORN".into());
    let status = git(
        root,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--no-renames",
        ],
        30,
    )
    .await?;
    let mut files = Vec::new();
    for record in status
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        if record.len() < 4 || record[2] != b' ' {
            return Err("Could not read Git status".into());
        }
        let path = std::str::from_utf8(&record[3..])
            .map_err(|_| "A changed filename is not UTF-8".to_string())?;
        files.push(GitFileState {
            path: path.into(),
            staged: record[0] != b' ' && record[0] != b'?',
            unstaged: record[1] != b' ' || record[0] == b'?',
        });
        if files.len() > FILE_COUNT_LIMIT {
            return Err("Too many changed files for Git actions".into());
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let remote = if branch == "HEAD" {
        None
    } else {
        resolve_remote(root, &branch).await
    };
    let remote_url = if let Some(name) = remote.as_deref() {
        git_string(root, &["remote", "get-url", "--push", name])
            .await
            .ok()
    } else {
        None
    };
    let mut hash = Sha256::new();
    hash.update(checkout_id.as_bytes());
    hash.update([0]);
    hash.update(branch.as_bytes());
    hash.update([0]);
    hash.update(head.as_bytes());
    hash.update([0]);
    hash.update(remote.as_deref().unwrap_or("").as_bytes());
    hash.update([0]);
    hash.update(remote_url.as_deref().unwrap_or("").as_bytes());
    hash.update([0]);
    let mut total_bytes = 0_u64;
    for file in &files {
        hash.update(file.path.as_bytes());
        hash.update([0]);
        hash.update([file.staged as u8, file.unstaged as u8]);
        let path = root.join(&file.path);
        if let Ok(metadata) = tokio::fs::symlink_metadata(&path).await {
            if metadata.is_file() {
                hash.update([b'f']);
                if metadata.len() > FILE_LIMIT {
                    return Err("Changed file is too large for safe Git actions".into());
                }
                let input = tokio::fs::File::open(&path)
                    .await
                    .map_err(|_| "Changed file is unreadable".to_string())?;
                let mut input = input.take(FILE_LIMIT + 1);
                let mut bytes = Vec::with_capacity(metadata.len() as usize);
                input
                    .read_to_end(&mut bytes)
                    .await
                    .map_err(|_| "Changed file is unreadable".to_string())?;
                if bytes.len() as u64 > FILE_LIMIT {
                    return Err("Changed file is too large for safe Git actions".into());
                }
                total_bytes += bytes.len() as u64;
                if total_bytes > TOTAL_FILE_LIMIT {
                    return Err("Changed files are too large for safe Git actions".into());
                }
                hash.update((bytes.len() as u64).to_le_bytes());
                hash.update(&bytes);
            } else if metadata.file_type().is_symlink() {
                hash.update([b'l']);
                hash.update(
                    tokio::fs::read_link(&path)
                        .await
                        .map_err(|_| "Changed link is unreadable".to_string())?
                        .as_os_str()
                        .as_encoded_bytes(),
                );
            } else if metadata.is_dir() {
                return Err("Submodule actions are not supported yet".into());
            }
        } else {
            hash.update([b'm']);
        }
    }
    if !files.is_empty() {
        let mut args = vec![
            "ls-files".to_string(),
            "--stage".into(),
            "-z".into(),
            "--".into(),
        ];
        args.extend(files.iter().map(|file| literal_path(&file.path)));
        hash.update(git(root, &args, 30).await?);
    }
    Ok(GitActionPreview {
        checkout_id: checkout_id.into(),
        cwd: root.to_string_lossy().into_owned(),
        branch,
        head,
        remote,
        files,
        token: format!("{:x}", hash.finalize()),
    })
}

async fn resolve_remote(root: &Path, branch: &str) -> Option<String> {
    for key in [
        format!("branch.{branch}.pushRemote"),
        "remote.pushDefault".into(),
        format!("branch.{branch}.remote"),
    ] {
        if let Ok(name) = git_string(root, &["config", "--get", &key]).await {
            if name != "."
                && git_string(root, &["remote", "get-url", "--push", &name])
                    .await
                    .is_ok()
            {
                return Some(name);
            }
        }
    }
    let remotes = git_string(root, &["remote"]).await.ok()?;
    let names: Vec<_> = remotes.lines().collect();
    (names.len() == 1).then(|| names[0].to_string())
}

fn literal_path(path: &str) -> String {
    format!(":(literal){path}")
}

async fn git_string(root: &Path, args: &[&str]) -> Result<String, String> {
    let output = git(root, args, 15).await?;
    Ok(String::from_utf8(output)
        .map_err(|_| "Git output is not UTF-8".to_string())?
        .trim()
        .to_string())
}

async fn git(root: &Path, args: &[impl AsRef<str>], seconds: u64) -> Result<Vec<u8>, String> {
    let mut command = tokio::process::Command::new("git");
    command
        .arg("-C")
        .arg(root)
        .args(args.iter().map(AsRef::as_ref))
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(seconds), command.output())
        .await
        .map_err(|_| "Git action timed out".to_string())?
        .map_err(|_| "Could not run Git".to_string())?;
    if !output.status.success() {
        return Err("Git command failed".into());
    }
    if output.stdout.len() > OUTPUT_LIMIT {
        return Err("Git output is too large".into());
    }
    Ok(output.stdout)
}

/// Mutation success is the process exit status. Its output may be arbitrarily
/// verbose and must never turn a completed commit or push into a false failure.
#[derive(Debug, thiserror::Error)]
enum GitMutationError {
    #[error("Could not start Git")]
    Spawn,
    #[error("Git action timed out")]
    TimedOut,
    #[error("Git process ended without an exit status")]
    Wait,
    #[error("Git command failed")]
    NonZeroExit,
}

impl GitMutationError {
    fn push_outcome_uncertain(&self) -> bool {
        matches!(self, Self::TimedOut | Self::Wait)
    }
}

async fn git_mutation(
    root: &Path,
    args: &[impl AsRef<str>],
    seconds: u64,
) -> Result<(), GitMutationError> {
    let mut command = tokio::process::Command::new("git");
    command
        .arg("-C")
        .arg(root)
        .args(args.iter().map(AsRef::as_ref))
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let mut child = command.spawn().map_err(|_| GitMutationError::Spawn)?;
    let status = tokio::time::timeout(Duration::from_secs(seconds), child.wait())
        .await
        .map_err(|_| GitMutationError::TimedOut)?
        .map_err(|_| GitMutationError::Wait)?;
    if status.success() {
        Ok(())
    } else {
        Err(GitMutationError::NonZeroExit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(root: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }

    fn fixture() -> (tempfile::TempDir, GitActions, String) {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        run(&repo, &["init", "-q", "-b", "main"]);
        run(&repo, &["config", "user.name", "LUNA Test"]);
        run(
            &repo,
            &["config", "user.email", "luna-test@example.invalid"],
        );
        std::fs::write(repo.join("base.txt"), "base\n").unwrap();
        run(&repo, &["add", "--", "base.txt"]);
        run(&repo, &["commit", "-q", "-m", "base"]);
        let service = GitActions::new(Repos::new(&dir.path().join("data"), "fixture"));
        (dir, service, repo.to_string_lossy().into_owned())
    }

    fn request(
        preview: &GitActionPreview,
        kind: GitActionKind,
        paths: &[&str],
    ) -> GitActionRequest {
        GitActionRequest {
            cwd: preview.cwd.clone(),
            checkout_id: preview.checkout_id.clone(),
            expected_token: preview.token.clone(),
            kind,
            paths: paths.iter().map(|path| (*path).into()).collect(),
            message: Some("Update selected file".into()),
            remote: preview.remote.clone(),
        }
    }

    fn pr_request(preview: &GitActionPreview) -> GitPrStatusRequest {
        GitPrStatusRequest {
            cwd: preview.cwd.clone(),
            checkout_id: preview.checkout_id.clone(),
            expected_token: preview.token.clone(),
            branch: preview.branch.clone(),
            remote: preview.remote.clone(),
        }
    }

    #[test]
    fn pr_link_uses_pushed_branch_even_when_upstream_tracks_another_name() {
        let url = "git@github.invalid:alice/repo.git";
        let source = CheckoutSourceContext {
            checkout_root: PathBuf::from("/not-used"),
            branch: BranchHeadContext::resolve(
                "local-feature",
                Some("origin/other-feature"),
                Some("origin"),
                Some(url),
            ),
            default_branch: Some("main".into()),
        };
        assert_eq!(source.branch.head_branch, "other-feature");
        let pushed = pr_source_for_push(source, "local-feature", "origin", url).unwrap();
        assert_eq!(pushed.branch.head_branch, "local-feature");
        assert_eq!(
            pushed.branch.head_selectors,
            ["alice:local-feature", "local-feature"]
        );
        assert!(pr_source_for_push(pushed, "local-feature", "fork", url).is_none());
    }

    #[tokio::test]
    async fn pr_status_explicitly_rejects_local_bare_remote() {
        let (dir, service, cwd) = fixture();
        let root = Path::new(&cwd);
        let bare = dir.path().join("bare.git");
        std::fs::create_dir(&bare).unwrap();
        run(&bare, &["init", "-q", "--bare"]);
        run(root, &["remote", "add", "origin", bare.to_str().unwrap()]);
        let preview = service.preview(&cwd).await.unwrap();
        assert_eq!(
            service.pr_status(pr_request(&preview)).await.unwrap(),
            GitPrStatus::UnsupportedHost
        );
    }

    #[tokio::test]
    async fn pr_status_rejects_stale_branch_preview_before_provider_lookup() {
        let (_dir, service, cwd) = fixture();
        let root = Path::new(&cwd);
        let preview = service.preview(&cwd).await.unwrap();
        run(root, &["switch", "-q", "-c", "topic"]);
        assert!(
            service
                .pr_status(pr_request(&preview))
                .await
                .unwrap_err()
                .contains("changed")
        );
    }

    #[tokio::test]
    async fn pr_context_remains_valid_when_only_file_token_changes() {
        let (_dir, service, cwd) = fixture();
        let preview = service.preview(&cwd).await.unwrap();
        let request = pr_request(&preview);
        let mut after = preview;
        after.token = "new worktree contents".into();
        assert!(validate_pr_context(&after, &request).is_ok());
    }

    #[tokio::test]
    async fn selected_commit_preserves_other_staged_and_unstaged_work() {
        let (_dir, service, cwd) = fixture();
        let root = Path::new(&cwd);
        std::fs::write(root.join("selected.txt"), "selected\n").unwrap();
        std::fs::write(root.join("other.txt"), "other\n").unwrap();
        std::fs::write(root.join("loose.txt"), "loose\n").unwrap();
        run(root, &["add", "--", "selected.txt", "other.txt"]);
        let before = service.preview(&cwd).await.unwrap();
        let receipt = service
            .execute(request(&before, GitActionKind::Commit, &["selected.txt"]))
            .await
            .unwrap();
        assert!(receipt.committed);
        assert!(!receipt.pushed);
        assert!(
            receipt
                .preview
                .files
                .iter()
                .any(|file| file.path == "other.txt" && file.staged)
        );
        assert!(
            receipt
                .preview
                .files
                .iter()
                .any(|file| file.path == "loose.txt" && file.unstaged)
        );
        assert!(
            !receipt
                .preview
                .files
                .iter()
                .any(|file| file.path == "selected.txt")
        );
    }

    #[tokio::test]
    async fn nested_chat_directory_uses_the_same_checkout_identity() {
        let (_dir, service, cwd) = fixture();
        let root = Path::new(&cwd);
        std::fs::create_dir(root.join("nested")).unwrap();
        std::fs::write(root.join("nested/file.txt"), "new\n").unwrap();
        let nested = root.join("nested").to_string_lossy().into_owned();
        let preview = service.preview(&nested).await.unwrap();
        let mut request = request(&preview, GitActionKind::Stage, &["nested/file.txt"]);
        request.cwd = nested;
        let receipt = service.execute(request).await.unwrap();
        assert!(
            receipt
                .preview
                .files
                .iter()
                .any(|file| file.path == "nested/file.txt" && file.staged)
        );
    }

    #[tokio::test]
    async fn unusual_filename_does_not_expand_as_a_pathspec() {
        let (_dir, service, cwd) = fixture();
        let root = Path::new(&cwd);
        std::fs::write(root.join(":(glob)*.txt"), "literal\n").unwrap();
        std::fs::write(root.join("different.txt"), "other\n").unwrap();
        let preview = service.preview(&cwd).await.unwrap();
        let receipt = service
            .execute(request(&preview, GitActionKind::Stage, &[":(glob)*.txt"]))
            .await
            .unwrap();
        assert!(
            receipt
                .preview
                .files
                .iter()
                .any(|file| file.path == ":(glob)*.txt" && file.staged)
        );
        assert!(
            receipt
                .preview
                .files
                .iter()
                .any(|file| file.path == "different.txt" && !file.staged)
        );
    }

    #[tokio::test]
    async fn stale_preview_and_changed_remote_are_rejected() {
        let (dir, service, cwd) = fixture();
        let root = Path::new(&cwd);
        std::fs::write(root.join("selected.txt"), "first\n").unwrap();
        let before = service.preview(&cwd).await.unwrap();
        std::fs::write(root.join("selected.txt"), "second\n").unwrap();
        assert!(
            service
                .execute(request(&before, GitActionKind::Stage, &["selected.txt"]))
                .await
                .unwrap_err()
                .contains("changed")
        );
        let bare = dir.path().join("bare.git");
        std::fs::create_dir(&bare).unwrap();
        run(&bare, &["init", "-q", "--bare"]);
        run(root, &["remote", "add", "origin", bare.to_str().unwrap()]);
        let remote_preview = service.preview(&cwd).await.unwrap();
        run(
            root,
            &[
                "remote",
                "set-url",
                "origin",
                dir.path().join("other.git").to_str().unwrap(),
            ],
        );
        assert!(
            service
                .execute(request(&remote_preview, GitActionKind::Push, &[]))
                .await
                .unwrap_err()
                .contains("changed")
        );
    }

    #[tokio::test]
    async fn switched_branch_invalidates_the_preview() {
        let (_dir, service, cwd) = fixture();
        let root = Path::new(&cwd);
        std::fs::write(root.join("selected.txt"), "selected\n").unwrap();
        let before = service.preview(&cwd).await.unwrap();
        run(root, &["switch", "-q", "-c", "topic"]);
        assert!(
            service
                .execute(request(&before, GitActionKind::Stage, &["selected.txt"]))
                .await
                .unwrap_err()
                .contains("changed")
        );
    }

    #[tokio::test]
    async fn failed_push_reports_committed_sha_and_retry_does_not_commit_again() {
        let (dir, service, cwd) = fixture();
        let root = Path::new(&cwd);
        run(
            root,
            &[
                "remote",
                "add",
                "origin",
                dir.path().join("missing.git").to_str().unwrap(),
            ],
        );
        std::fs::write(root.join("selected.txt"), "selected\n").unwrap();
        let before = service.preview(&cwd).await.unwrap();
        let staged = service
            .execute(request(&before, GitActionKind::Stage, &["selected.txt"]))
            .await
            .unwrap()
            .preview;
        let receipt = service
            .execute(request(
                &staged,
                GitActionKind::CommitAndPush,
                &["selected.txt"],
            ))
            .await
            .unwrap();
        assert!(receipt.committed);
        assert!(!receipt.pushed);
        assert!(!receipt.push_uncertain);
        assert!(
            receipt
                .error
                .as_deref()
                .unwrap()
                .contains("Commit succeeded")
        );
        let sha = receipt.commit_sha.unwrap();
        let retry = service
            .execute(request(&receipt.preview, GitActionKind::Push, &[]))
            .await
            .unwrap();
        assert!(!retry.committed);
        assert!(!retry.pushed);
        assert!(!retry.push_uncertain);
        assert_eq!(git_string(root, &["rev-parse", "HEAD"]).await.unwrap(), sha);
    }

    #[cfg(unix)]
    fn install_slow_local_push_hook(root: &Path) {
        use std::os::unix::fs::PermissionsExt;
        let hook = root.join(".git/hooks/pre-push");
        std::fs::write(&hook, "#!/bin/sh\nsleep 3\n").unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn timed_out_push_reports_uncertain_remote_outcome() {
        let (dir, mut service, cwd) = fixture();
        let root = Path::new(&cwd);
        let bare = dir.path().join("bare.git");
        std::fs::create_dir(&bare).unwrap();
        run(&bare, &["init", "-q", "--bare"]);
        run(root, &["remote", "add", "origin", bare.to_str().unwrap()]);
        install_slow_local_push_hook(root);
        service.push_timeout_seconds = 1;
        let preview = service.preview(&cwd).await.unwrap();
        let receipt = service
            .execute(request(&preview, GitActionKind::Push, &[]))
            .await
            .unwrap();
        assert!(!receipt.committed);
        assert!(!receipt.pushed);
        assert!(receipt.push_uncertain);
        assert!(
            receipt
                .error
                .unwrap()
                .contains("Check the remote branch before retrying")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn timed_out_combined_push_preserves_commit_success_receipt() {
        let (dir, mut service, cwd) = fixture();
        let root = Path::new(&cwd);
        let bare = dir.path().join("bare.git");
        std::fs::create_dir(&bare).unwrap();
        run(&bare, &["init", "-q", "--bare"]);
        run(root, &["remote", "add", "origin", bare.to_str().unwrap()]);
        install_slow_local_push_hook(root);
        service.push_timeout_seconds = 1;
        std::fs::write(root.join("selected.txt"), "selected\n").unwrap();
        let before = service.preview(&cwd).await.unwrap();
        let staged = service
            .execute(request(&before, GitActionKind::Stage, &["selected.txt"]))
            .await
            .unwrap()
            .preview;
        let receipt = service
            .execute(request(
                &staged,
                GitActionKind::CommitAndPush,
                &["selected.txt"],
            ))
            .await
            .unwrap();
        assert!(receipt.committed);
        assert!(!receipt.pushed);
        assert!(receipt.push_uncertain);
        assert_eq!(
            receipt.commit_sha.as_deref(),
            Some(receipt.preview.head.as_str())
        );
        assert!(
            receipt
                .error
                .unwrap()
                .contains("Commit succeeded; push outcome is uncertain")
        );
    }

    #[tokio::test]
    async fn combined_action_without_remote_does_not_commit() {
        let (_dir, service, cwd) = fixture();
        let root = Path::new(&cwd);
        std::fs::write(root.join("selected.txt"), "selected\n").unwrap();
        let before = service.preview(&cwd).await.unwrap();
        let staged = service
            .execute(request(&before, GitActionKind::Stage, &["selected.txt"]))
            .await
            .unwrap()
            .preview;
        let head = staged.head.clone();
        assert!(
            service
                .execute(request(
                    &staged,
                    GitActionKind::CommitAndPush,
                    &["selected.txt"]
                ))
                .await
                .unwrap_err()
                .contains("remote")
        );
        assert_eq!(
            git_string(root, &["rev-parse", "HEAD"]).await.unwrap(),
            head
        );
    }

    #[tokio::test]
    async fn push_uses_only_the_local_bare_remote() {
        let (dir, service, cwd) = fixture();
        let root = Path::new(&cwd);
        let bare = dir.path().join("bare.git");
        std::fs::create_dir(&bare).unwrap();
        run(&bare, &["init", "-q", "--bare"]);
        run(root, &["remote", "add", "origin", bare.to_str().unwrap()]);
        let before = service.preview(&cwd).await.unwrap();
        let receipt = service
            .execute(request(&before, GitActionKind::Push, &[]))
            .await
            .unwrap();
        assert!(receipt.pushed);
        assert!(!receipt.committed);
        let remote_head = git_string(&bare, &["rev-parse", "refs/heads/main"])
            .await
            .unwrap();
        assert_eq!(remote_head, before.head);
    }
}
