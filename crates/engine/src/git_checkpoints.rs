//! Git-backed file checkpoints for local tasks. Private refs contain observed
//! worktree trees; the user's branch and index are never used as capture output.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use luna_proto::{
    GitCheckpoint, GitCheckpointDiff, GitCheckpointList, GitCheckpointRestorePreview,
    GitCheckpointRestoreReceipt, GitCheckpointRestoreRequest,
};
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;

use crate::git_actions::GitActions;
use crate::repos::{CheckoutIdentity, Repos};

const MAX_TURNS: usize = 32;
const MAX_OUTPUT: usize = 1024 * 1024;
const MAX_PATHS: usize = 500;

#[derive(Clone)]
pub struct GitCheckpoints {
    repos: Repos,
    actions: GitActions,
    active: Arc<Mutex<HashMap<(String, String), ActiveTurn>>>,
    failures: Arc<Mutex<HashMap<String, Vec<GitCheckpoint>>>>,
}

#[derive(Clone)]
struct ActiveTurn {
    cwd: String,
    turn_id: String,
    started_at_ms: i64,
    start_error: Option<String>,
}

#[derive(Clone)]
struct Point {
    work_commit: String,
    work_tree: String,
    index_tree: String,
    at_ms: i64,
    outcome: Option<String>,
}

impl GitCheckpoints {
    pub fn new(repos: Repos) -> Self {
        Self {
            actions: GitActions::new(repos.clone()),
            repos,
            active: Arc::default(),
            failures: Arc::default(),
        }
    }

    pub(crate) fn actions(&self) -> GitActions {
        self.actions.clone()
    }

    /// Called before a fresh run starts or when a warm provider confirms its
    /// next turn boundary. A failed capture never becomes a baseline.
    pub async fn start(&self, chat_id: &str, run_id: &str, cwd: &str) {
        let turn = ActiveTurn {
            cwd: cwd.into(),
            turn_id: format!("{:016x}{}", crate::now_ms(), uuid::Uuid::new_v4().simple()),
            started_at_ms: crate::now_ms(),
            start_error: None,
        };
        let key = (chat_id.to_owned(), run_id.to_owned());
        {
            let mut active = self.active.lock().unwrap();
            if active.contains_key(&key) {
                return; // a repeated boundary cannot replace the current run's baseline
            }
            active.insert(key.clone(), turn.clone());
        }
        if let Err(error) = self
            .capture(chat_id, &turn.turn_id, cwd, "before", "started")
            .await
        {
            let mut active = self.active.lock().unwrap();
            if let Some(current) = active
                .get_mut(&key)
                .filter(|current| current.turn_id == turn.turn_id)
            {
                current.start_error = Some(error.clone());
            }
            let mut failed = turn.clone();
            failed.start_error = Some(error.clone());
            self.record_failure(chat_id, &failed, error, None);
        }
    }

    /// Authoritative Done, including interruption and error. No provider
    /// rewind or message mutation is coupled to this file snapshot.
    pub async fn end(&self, chat_id: &str, run_id: &str, outcome: &str) {
        let turn = self
            .active
            .lock()
            .unwrap()
            .remove(&(chat_id.to_owned(), run_id.to_owned()));
        let Some(turn) = turn else { return };
        if let Err(error) = self
            .capture(chat_id, &turn.turn_id, &turn.cwd, "after", outcome)
            .await
        {
            self.record_failure(chat_id, &turn, error, Some(outcome));
        }
    }

    fn record_failure(
        &self,
        chat_id: &str,
        turn: &ActiveTurn,
        error: String,
        outcome: Option<&str>,
    ) {
        let mut failures = self.failures.lock().unwrap();
        let rows = failures.entry(chat_id.into()).or_default();
        rows.retain(|row| row.turn_id != turn.turn_id);
        rows.insert(
            0,
            GitCheckpoint {
                turn_id: turn.turn_id.clone(),
                started_at_ms: turn.started_at_ms,
                ended_at_ms: outcome.map(|_| crate::now_ms()),
                outcome: outcome.map(str::to_owned),
                before_available: turn.start_error.is_none(),
                after_available: false,
                error: Some(match &turn.start_error {
                    Some(start) if start != &error => format!("Before: {start}; after: {error}"),
                    _ => error,
                }),
            },
        );
        rows.truncate(MAX_TURNS);
    }

    async fn identity(&self, cwd: &str) -> Result<CheckoutIdentity, String> {
        self.repos
            .checkout_identity(Path::new(cwd))
            .await
            .map_err(|_| "Git checkpoints are unavailable outside a local Git checkout".into())
    }

    async fn capture(
        &self,
        chat_id: &str,
        turn_id: &str,
        cwd: &str,
        point: &str,
        outcome: &str,
    ) -> Result<(), String> {
        let identity = self.identity(cwd).await?;
        let _guard = self.actions.lane(&identity.root).lock_owned().await;
        self.capture_locked(&identity, chat_id, turn_id, point, outcome)
            .await?;
        self.prune_locked(&identity.root, chat_id).await;
        Ok(())
    }

    async fn capture_locked(
        &self,
        identity: &CheckoutIdentity,
        chat_id: &str,
        turn_id: &str,
        point: &str,
        outcome: &str,
    ) -> Result<String, String> {
        let root = &identity.root;
        if git_optional(root, &["config", "--bool", "core.sparseCheckout"], None)
            .await?
            .as_deref()
            == Some("true")
        {
            return Err("Sparse checkouts are unavailable for checkpoints".into());
        }
        // Keep the temporary index inside this worktree's Git dir. Git's real
        // index (and its staged/unstaged split) is never altered by capture.
        let private_index = tempfile::NamedTempFile::new_in(&identity.git_dir)
            .map_err(|_| "Could not create private checkpoint index".to_string())?;
        let private_path = private_index.path().to_path_buf();
        // Preserve actual tracked membership, including force-added ignored
        // files absent from HEAD. Only the private index receives worktree bytes.
        let index_tree = git_text(root, &["write-tree"], None).await?;
        git(root, &["read-tree", &index_tree], Some(&private_path), None).await?;
        git(root, &["add", "-A", "--", "."], Some(&private_path), None).await?;
        let work_tree = git_text(root, &["write-tree"], Some(&private_path)).await?;
        let at_ms = crate::now_ms();
        let message =
            format!("LUNA file checkpoint\nindex={index_tree}\nat={at_ms}\noutcome={outcome}");
        let index_commit = git_text(
            root,
            &[
                "-c",
                "core.fsync=objects,reference",
                "-c",
                "core.fsyncMethod=fsync",
                "commit-tree",
                &index_tree,
                "-m",
                "LUNA checkpoint index",
            ],
            None,
        )
        .await?;
        let commit = git_text(
            root,
            &[
                "-c",
                "core.fsync=objects,reference",
                "-c",
                "core.fsyncMethod=fsync",
                "commit-tree",
                &work_tree,
                "-p",
                &index_commit,
                "-m",
                &message,
            ],
            None,
        )
        .await?;
        let reference = checkpoint_ref(chat_id, turn_id, point)?;
        git(
            root,
            &[
                "-c",
                "core.fsync=objects,reference",
                "-c",
                "core.fsyncMethod=fsync",
                "update-ref",
                &reference,
                &commit,
            ],
            None,
            None,
        )
        .await?;
        Ok(commit)
    }

    async fn point(
        &self,
        root: &Path,
        chat_id: &str,
        turn_id: &str,
        phase: &str,
    ) -> Result<Point, String> {
        let reference = checkpoint_ref(chat_id, turn_id, phase)?;
        let work_commit = git_text(
            root,
            &["rev-parse", "--verify", &format!("{reference}^{{commit}}")],
            None,
        )
        .await
        .map_err(|_| "Checkpoint is unavailable".to_string())?;
        let work_tree = git_text(
            root,
            &["rev-parse", &format!("{work_commit}^{{tree}}")],
            None,
        )
        .await?;
        let message = git_text(root, &["show", "-s", "--format=%B", &work_commit], None).await?;
        let index_tree = message
            .lines()
            .find_map(|line| line.strip_prefix("index="))
            .filter(|value| valid_oid(value))
            .ok_or("Checkpoint index metadata is invalid")?
            .to_owned();
        let anchored_index = git_text(
            root,
            &["rev-parse", &format!("{work_commit}^1^{{tree}}")],
            None,
        )
        .await?;
        if anchored_index != index_tree {
            return Err("Checkpoint index anchor is invalid".into());
        }
        let at_ms = message
            .lines()
            .find_map(|line| line.strip_prefix("at="))
            .and_then(|value| value.parse().ok())
            .ok_or("Checkpoint time metadata is invalid")?;
        let outcome = message
            .lines()
            .find_map(|line| line.strip_prefix("outcome="))
            .map(str::to_owned);
        Ok(Point {
            work_commit,
            work_tree,
            index_tree,
            at_ms,
            outcome,
        })
    }

    pub async fn list(
        &self,
        chat_id: &str,
        cwd: &str,
        live_turn: bool,
    ) -> Result<GitCheckpointList, String> {
        let identity = self.identity(cwd).await?;
        let _guard = self.actions.lane(&identity.root).lock_owned().await;
        let refs = self.refs_locked(&identity.root, chat_id).await?;
        let active_turns: HashSet<_> = self
            .active
            .lock()
            .unwrap()
            .iter()
            .filter(|((task, _), _)| task == chat_id)
            .map(|(_, turn)| turn.turn_id.clone())
            .collect();
        let mut rows = Vec::new();
        for (turn_id, (before, after)) in refs.into_iter().rev().take(MAX_TURNS) {
            let before = if before {
                self.point(&identity.root, chat_id, &turn_id, "before")
                    .await
                    .ok()
            } else {
                None
            };
            let after = if after {
                self.point(&identity.root, chat_id, &turn_id, "after")
                    .await
                    .ok()
            } else {
                None
            };
            rows.push(GitCheckpoint {
                turn_id: turn_id.clone(),
                started_at_ms: before
                    .as_ref()
                    .map(|point| point.at_ms)
                    .or_else(|| after.as_ref().map(|point| point.at_ms))
                    .unwrap_or(0),
                ended_at_ms: after.as_ref().map(|point| point.at_ms),
                outcome: after
                    .as_ref()
                    .and_then(|point| point.outcome.clone())
                    .or_else(|| {
                        Some(
                            if active_turns.contains(&turn_id) {
                                "in progress"
                            } else {
                                "completion unavailable"
                            }
                            .into(),
                        )
                    }),
                before_available: before.is_some(),
                after_available: after.is_some(),
                error: None,
            });
        }
        if let Some(failures) = self.failures.lock().unwrap().get(chat_id) {
            for failure in failures {
                if let Some(row) = rows.iter_mut().find(|row| row.turn_id == failure.turn_id) {
                    row.error = failure.error.clone();
                    row.before_available |= failure.before_available;
                    row.ended_at_ms = row.ended_at_ms.or(failure.ended_at_ms);
                    row.outcome = row.outcome.clone().or(failure.outcome.clone());
                } else {
                    rows.push(failure.clone());
                }
            }
        }
        rows.sort_by(|a, b| b.turn_id.cmp(&a.turn_id));
        rows.truncate(MAX_TURNS);
        let reason = self.restore_reason(&identity, cwd, live_turn).await;
        Ok(GitCheckpointList {
            checkout_id: identity.id,
            cwd: identity.root.to_string_lossy().into_owned(),
            checkpoints: rows,
            restore_available: reason.is_none(),
            restore_unavailable_reason: reason,
        })
    }

    async fn refs_locked(
        &self,
        root: &Path,
        chat_id: &str,
    ) -> Result<BTreeMap<String, (bool, bool)>, String> {
        let prefix = format!("refs/luna/checkpoints/{}/", task_key(chat_id));
        let raw = git_text(
            root,
            &["for-each-ref", "--format=%(refname)", &prefix],
            None,
        )
        .await?;
        let mut refs = BTreeMap::new();
        for line in raw.lines() {
            let Some(tail) = line.strip_prefix(&prefix) else {
                continue;
            };
            let Some((turn, phase)) = tail.split_once('/') else {
                continue;
            };
            if !valid_turn(turn) {
                continue;
            }
            let entry = refs.entry(turn.to_owned()).or_insert((false, false));
            match phase {
                "before" => entry.0 = true,
                "after" => entry.1 = true,
                _ => {}
            }
        }
        Ok(refs)
    }

    async fn prune_locked(&self, root: &Path, chat_id: &str) {
        let Ok(refs) = self.refs_locked(root, chat_id).await else {
            return;
        };
        for (turn, (before, after)) in refs.into_iter().rev().skip(MAX_TURNS) {
            for (exists, phase) in [(before, "before"), (after, "after")] {
                if exists && let Ok(reference) = checkpoint_ref(chat_id, &turn, phase) {
                    let _ = git(root, &["update-ref", "-d", &reference], None, None).await;
                }
            }
        }
    }

    pub async fn diff(
        &self,
        chat_id: &str,
        cwd: &str,
        turn_id: &str,
    ) -> Result<GitCheckpointDiff, String> {
        let identity = self.identity(cwd).await?;
        let _guard = self.actions.lane(&identity.root).lock_owned().await;
        let before = self
            .point(&identity.root, chat_id, turn_id, "before")
            .await?;
        let after = self
            .point(&identity.root, chat_id, turn_id, "after")
            .await?;
        let raw = git(
            &identity.root,
            &[
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                "--no-color",
                "--find-renames",
                "--unified=3",
                &before.work_commit,
                &after.work_commit,
                "--",
            ],
            None,
            Some(MAX_OUTPUT),
        )
        .await?;
        let patch =
            String::from_utf8(raw).map_err(|_| "Checkpoint diff contains non-UTF-8 text")?;
        let paths = diff_paths(&identity.root, &before.work_tree, &after.work_tree).await?;
        Ok(GitCheckpointDiff { turn_id: turn_id.into(), patch, paths, truncated: false,
            attribution: "Observed checkout changes; ownership cannot be determined when tasks share a checkout".into() })
    }

    async fn restore_reason(
        &self,
        identity: &CheckoutIdentity,
        cwd: &str,
        live_turn: bool,
    ) -> Option<String> {
        if live_turn {
            return Some(
                "File restore requires this worktree to have one task owner and no active turn"
                    .into(),
            );
        }
        let active_cwds: Vec<_> = self
            .active
            .lock()
            .unwrap()
            .values()
            .map(|turn| turn.cwd.clone())
            .collect();
        for cwd in active_cwds {
            if self
                .repos
                .checkout_identity(Path::new(&cwd))
                .await
                .is_ok_and(|active| active.root == identity.root)
            {
                return Some("Wait for the checkpoint turn in this checkout to finish".into());
            }
        }
        let root = &identity.root;
        if Path::new(cwd).canonicalize().ok().as_deref() != Some(root.as_path()) {
            return Some("File restore requires the task cwd to be the exact worktree root".into());
        }
        let managed = self.repos.managed_worktrees_root().canonicalize().ok();
        if !root.join(".git").is_file()
            || managed
                .as_ref()
                .is_none_or(|base| !root.starts_with(base) || root == base)
        {
            return Some("File restore requires a LUNA-managed isolated worktree; shared checkouts are read-only".into());
        }
        let Ok(raw) = git_text(root, &["worktree", "list", "--porcelain"], None).await else {
            return Some("Cannot verify the Git worktree registration".into());
        };
        if !raw.lines().any(|line| {
            line.strip_prefix("worktree ").is_some_and(|path| {
                Path::new(path).canonicalize().ok().as_deref() == Some(root.as_path())
            })
        }) {
            return Some("This worktree is no longer registered in Git".into());
        }
        match git_optional(root, &["config", "--bool", "core.sparseCheckout"], None).await {
            Ok(Some(value)) if value == "true" => {
                return Some("File restore is unavailable in a sparse checkout".into());
            }
            Err(_) => return Some("Cannot verify sparse checkout configuration".into()),
            _ => {}
        }
        None
    }

    pub async fn preview_restore(
        &self,
        chat_id: &str,
        cwd: &str,
        turn_id: &str,
        point: &str,
        live_turn: bool,
    ) -> Result<GitCheckpointRestorePreview, String> {
        let identity = self.identity(cwd).await?;
        let _guard = self.actions.lane(&identity.root).lock_owned().await;
        if let Some(reason) = self.restore_reason(&identity, cwd, live_turn).await {
            return Err(reason);
        }
        let target = self.point(&identity.root, chat_id, turn_id, point).await?;
        let current_work = self.current_tree(&identity).await?;
        let current_index = git_text(&identity.root, &["write-tree"], None).await?;
        let head = git_optional(&identity.root, &["rev-parse", "--verify", "HEAD"], None)
            .await?
            .unwrap_or_default();
        let paths = restore_paths(&identity.root, &current_work, &current_index, &target).await?;
        let token = restore_token(&identity.id, &head, &current_work, &current_index, &target);
        Ok(GitCheckpointRestorePreview {
            checkout_id: identity.id,
            cwd: identity.root.to_string_lossy().into_owned(),
            turn_id: turn_id.into(),
            point: point.into(),
            paths,
            token,
        })
    }

    pub async fn restore<F: Fn(&Path) -> bool + Send + Sync>(
        &self,
        request: GitCheckpointRestoreRequest,
        live_turn: bool,
        owner_probe: F,
    ) -> Result<GitCheckpointRestoreReceipt, String> {
        let identity = self.identity(&request.cwd).await?;
        let _guard = self.actions.lane(&identity.root).lock_owned().await;
        if let Some(reason) = self
            .restore_reason(&identity, &request.cwd, live_turn)
            .await
        {
            return Err(reason);
        }
        let target = self
            .point(
                &identity.root,
                &request.chat_id,
                &request.turn_id,
                &request.point,
            )
            .await?;
        let current_work = self.current_tree(&identity).await?;
        let current_index = git_text(&identity.root, &["write-tree"], None).await?;
        let head = git_optional(&identity.root, &["rev-parse", "--verify", "HEAD"], None)
            .await?
            .unwrap_or_default();
        if restore_token(&identity.id, &head, &current_work, &current_index, &target)
            != request.expected_token
        {
            return Err("Checkout changed after the restore preview; refresh it".into());
        }
        let paths = restore_paths(&identity.root, &current_work, &current_index, &target).await?;
        let removed_paths = deleted_paths(&identity.root, &current_work, &target.work_tree).await?;
        if let Some(reason) = self
            .restore_reason(&identity, &request.cwd, owner_probe(&identity.root))
            .await
        {
            return Err(reason);
        }
        let recovery_id = format!("{:016x}{}", crate::now_ms(), uuid::Uuid::new_v4().simple());
        let recovery_ref = format!(
            "refs/luna/checkpoints/{}/recovery/{recovery_id}",
            task_key(&request.chat_id)
        );
        let recovery_message = format!(
            "LUNA file restore recovery\nindex={current_index}\nat={}",
            crate::now_ms()
        );
        let recovery_index_commit = git_text(
            &identity.root,
            &["commit-tree", &current_index, "-m", "LUNA recovery index"],
            None,
        )
        .await?;
        let recovery_commit = git_text(
            &identity.root,
            &[
                "commit-tree",
                &current_work,
                "-p",
                &recovery_index_commit,
                "-m",
                &recovery_message,
            ],
            None,
        )
        .await?;
        git(
            &identity.root,
            &["update-ref", &recovery_ref, &recovery_commit],
            None,
            None,
        )
        .await?;
        if let Some(reason) = self
            .restore_reason(&identity, &request.cwd, owner_probe(&identity.root))
            .await
        {
            return Err(reason);
        }
        if self.identity(&request.cwd).await? != identity {
            return Err(
                "Checkout identity changed during restore preparation; no files were restored"
                    .into(),
            );
        }
        let fresh_work = self.current_tree(&identity).await?;
        let fresh_index = git_text(&identity.root, &["write-tree"], None).await?;
        let fresh_head = git_optional(&identity.root, &["rev-parse", "--verify", "HEAD"], None)
            .await?
            .unwrap_or_default();
        if restore_token(
            &identity.id,
            &fresh_head,
            &fresh_work,
            &fresh_index,
            &target,
        ) != request.expected_token
        {
            return Err(format!(
                "Checkout changed during restore preparation; no files were restored. Recovery ref: {recovery_ref}"
            ));
        }
        // The gate above excludes shared checkouts. Only the exact isolated
        // worktree can be cleaned. A recovery ref is published before mutation.
        let mutation = async {
            git(
                &identity.root,
                &[
                    "restore",
                    "--source",
                    &target.work_commit,
                    "--worktree",
                    "--staged",
                    "--",
                    ".",
                ],
                None,
                None,
            )
            .await?;
            if !removed_paths.is_empty() {
                let mut clean_args = vec!["clean".to_string(), "-f".into(), "--".into()];
                clean_args.extend(removed_paths.iter().map(|path| format!(":(literal){path}")));
                let args: Vec<_> = clean_args.iter().map(String::as_str).collect();
                git(&identity.root, &args, None, None).await?;
            }
            git(
                &identity.root,
                &["read-tree", "--reset", &target.index_tree],
                None,
                None,
            )
            .await?;
            git(
                &identity.root,
                &[
                    "restore",
                    "--source",
                    &target.work_commit,
                    "--worktree",
                    "--",
                    ".",
                ],
                None,
                None,
            )
            .await?;
            let restored_work = self.current_tree(&identity).await?;
            let restored_index = git_text(&identity.root, &["write-tree"], None).await?;
            if restored_work != target.work_tree || restored_index != target.index_tree {
                return Err(
                    "Git restore completed, but file/index readback differs from the checkpoint"
                        .into(),
                );
            }
            Ok::<(), String>(())
        }
        .await;
        mutation.map_err(|error| {
            format!("File restore did not verify ({error}); inspect recovery ref {recovery_ref}")
        })?;
        self.prune_recovery_locked(&identity.root, &request.chat_id)
            .await;
        Ok(GitCheckpointRestoreReceipt {
            restored: true,
            recovery_ref,
            paths,
        })
    }

    async fn prune_recovery_locked(&self, root: &Path, chat_id: &str) {
        let prefix = format!("refs/luna/checkpoints/{}/recovery/", task_key(chat_id));
        let Ok(raw) = git_text(
            root,
            &["for-each-ref", "--format=%(refname)", &prefix],
            None,
        )
        .await
        else {
            return;
        };
        let refs: Vec<_> = raw
            .lines()
            .filter(|line| line.starts_with(&prefix))
            .collect();
        for reference in refs.into_iter().rev().skip(MAX_TURNS) {
            let _ = git(root, &["update-ref", "-d", reference], None, None).await;
        }
    }

    async fn current_tree(&self, identity: &CheckoutIdentity) -> Result<String, String> {
        let private_index = tempfile::NamedTempFile::new_in(&identity.git_dir)
            .map_err(|_| "Could not create private checkpoint index".to_string())?;
        let path = private_index.path();
        let index_tree = git_text(&identity.root, &["write-tree"], None).await?;
        git(
            &identity.root,
            &["read-tree", &index_tree],
            Some(path),
            None,
        )
        .await?;
        git(&identity.root, &["add", "-A", "--", "."], Some(path), None).await?;
        git_text(&identity.root, &["write-tree"], Some(path)).await
    }
}

fn task_key(chat_id: &str) -> String {
    format!("{:x}", Sha256::digest(chat_id.as_bytes()))
}
fn valid_turn(turn: &str) -> bool {
    turn.len() == 48 && turn.bytes().all(|byte| byte.is_ascii_hexdigit())
}
fn valid_oid(oid: &str) -> bool {
    (oid.len() == 40 || oid.len() == 64) && oid.bytes().all(|byte| byte.is_ascii_hexdigit())
}
fn checkpoint_ref(chat_id: &str, turn: &str, phase: &str) -> Result<String, String> {
    if !valid_turn(turn) || !matches!(phase, "before" | "after") {
        return Err("Invalid checkpoint selector".into());
    }
    Ok(format!(
        "refs/luna/checkpoints/{}/{turn}/{phase}",
        task_key(chat_id)
    ))
}
fn restore_token(checkout: &str, head: &str, work: &str, index: &str, target: &Point) -> String {
    let mut sha = Sha256::new();
    for part in [
        checkout,
        head,
        work,
        index,
        &target.work_commit,
        &target.index_tree,
    ] {
        sha.update(part.as_bytes());
        sha.update([0]);
    }
    format!("{:x}", sha.finalize())
}
async fn diff_paths(root: &Path, from: &str, to: &str) -> Result<Vec<String>, String> {
    let raw = git(
        root,
        &["diff", "--name-only", "-z", from, to, "--"],
        None,
        Some(MAX_OUTPUT),
    )
    .await?;
    parse_paths(raw)
}
async fn deleted_paths(root: &Path, from: &str, to: &str) -> Result<Vec<String>, String> {
    let raw = git(
        root,
        &[
            "diff",
            "--name-only",
            "-z",
            "--diff-filter=D",
            from,
            to,
            "--",
        ],
        None,
        Some(MAX_OUTPUT),
    )
    .await?;
    parse_paths(raw)
}
fn parse_paths(raw: Vec<u8>) -> Result<Vec<String>, String> {
    let mut paths = Vec::new();
    for path in raw.split(|byte| *byte == 0).filter(|path| !path.is_empty()) {
        if paths.len() == MAX_PATHS {
            return Err("Checkpoint changes exceed the path preview limit".into());
        }
        paths.push(String::from_utf8(path.to_vec()).map_err(|_| "Checkpoint path is not UTF-8")?);
    }
    Ok(paths)
}
async fn restore_paths(
    root: &Path,
    work: &str,
    index: &str,
    target: &Point,
) -> Result<Vec<String>, String> {
    let mut paths = diff_paths(root, work, &target.work_tree).await?;
    for path in diff_paths(root, index, &target.index_tree).await? {
        if !paths.contains(&path) {
            paths.push(path);
        }
        if paths.len() > MAX_PATHS {
            return Err("Checkpoint changes exceed the path preview limit".into());
        }
    }
    paths.sort();
    Ok(paths)
}

async fn git_text(root: &Path, args: &[&str], index: Option<&Path>) -> Result<String, String> {
    let raw = git(root, args, index, Some(MAX_OUTPUT)).await?;
    Ok(String::from_utf8(raw)
        .map_err(|_| "Git output is not UTF-8")?
        .trim()
        .to_owned())
}
async fn git_optional(
    root: &Path,
    args: &[&str],
    index: Option<&Path>,
) -> Result<Option<String>, String> {
    match git_text(root, args, index).await {
        Ok(value) => Ok(Some(value)),
        Err(error) if error == "Git command failed" => Ok(None),
        Err(error) => Err(error),
    }
}
async fn git(
    root: &Path,
    args: &[&str],
    index: Option<&Path>,
    max: Option<usize>,
) -> Result<Vec<u8>, String> {
    let limit = max.unwrap_or(MAX_OUTPUT);
    let mut cmd = tokio::process::Command::new("git");
    cmd.arg("-C")
        .arg(root)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env("GIT_AUTHOR_NAME", "LUNA Checkpoint")
        .env(
            "GIT_AUTHOR_EMAIL",
            "luna-checkpoint@users.noreply.github.com",
        )
        .env("GIT_COMMITTER_NAME", "LUNA Checkpoint")
        .env(
            "GIT_COMMITTER_EMAIL",
            "luna-checkpoint@users.noreply.github.com",
        )
        .kill_on_drop(true);
    if let Some(path) = index {
        cmd.env("GIT_INDEX_FILE", path);
    }
    let mut child = cmd.spawn().map_err(|_| "Could not run Git".to_string())?;
    let stdout = child.stdout.take().ok_or("Could not read Git output")?;
    let read = async {
        let mut bytes = Vec::new();
        stdout
            .take((limit + 1) as u64)
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| "Could not read Git output")?;
        if bytes.len() > limit {
            let _ = child.kill().await;
            return Err("Git output exceeds checkpoint limit".into());
        }
        let status = child
            .wait()
            .await
            .map_err(|_| "Git process ended without a result")?;
        if !status.success() {
            return Err("Git command failed".into());
        }
        Ok(bytes)
    };
    tokio::time::timeout(Duration::from_secs(30), read)
        .await
        .map_err(|_| "Git checkpoint command timed out".to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn run(root: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    fn fixture() -> (
        tempfile::TempDir,
        GitCheckpoints,
        std::path::PathBuf,
        std::path::PathBuf,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        run(&repo, &["init", "-q", "-b", "main"]);
        run(&repo, &["config", "user.name", "LUNA Fixture"]);
        run(&repo, &["config", "user.email", "fixture@example.invalid"]);
        std::fs::write(repo.join("staged.txt"), "base\n").unwrap();
        std::fs::write(repo.join("unstaged.txt"), "base\n").unwrap();
        run(&repo, &["add", "."]);
        run(&repo, &["commit", "-q", "-m", "base"]);
        let managed = dir.path().join("managed");
        let worktree = managed.join("repo").join("turn");
        std::fs::create_dir_all(worktree.parent().unwrap()).unwrap();
        run(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "turn",
                worktree.to_str().unwrap(),
            ],
        );
        let repos = Repos::with_worktrees_root(&dir.path().join("data"), "fixture", managed);
        (dir, GitCheckpoints::new(repos), repo, worktree)
    }

    #[tokio::test]
    async fn private_capture_diff_and_isolated_restore_preserve_index_split() {
        let (_dir, service, shared, worktree) = fixture();
        let cwd = worktree.to_str().unwrap();
        std::fs::write(worktree.join("staged.txt"), "staged\n").unwrap();
        run(&worktree, &["add", "staged.txt"]);
        std::fs::write(worktree.join("unstaged.txt"), "before\n").unwrap();
        std::fs::write(worktree.join("untracked.txt"), "before\n").unwrap();
        let original_status = run(&worktree, &["status", "--porcelain"]);
        service.start("task", "fixture-run", cwd).await;
        assert_eq!(run(&worktree, &["status", "--porcelain"]), original_status);
        std::fs::write(worktree.join("unstaged.txt"), "after\n").unwrap();
        std::fs::write(worktree.join("untracked.txt"), "after\n").unwrap();
        service.end("task", "fixture-run", "interrupted").await;
        let list = service.list("task", cwd, false).await.unwrap();
        let row = &list.checkpoints[0];
        assert!(row.before_available && row.after_available, "{row:?}");
        assert_eq!(row.outcome.as_deref(), Some("interrupted"));
        assert!(list.restore_available);
        let diff = service.diff("task", cwd, &row.turn_id).await.unwrap();
        assert!(diff.paths.contains(&"unstaged.txt".into()));
        assert!(diff.paths.contains(&"untracked.txt".into()));
        assert!(!diff.paths.contains(&"staged.txt".into()));
        assert!(
            service
                .preview_restore("task", cwd, &row.turn_id, "before", true)
                .await
                .unwrap_err()
                .contains("one task owner")
        );
        let nested = worktree.join("nested");
        std::fs::create_dir(&nested).unwrap();
        assert!(
            service
                .preview_restore(
                    "task",
                    nested.to_str().unwrap(),
                    &row.turn_id,
                    "before",
                    false
                )
                .await
                .unwrap_err()
                .contains("exact worktree root")
        );
        let preview = service
            .preview_restore("task", cwd, &row.turn_id, "before", false)
            .await
            .unwrap();
        assert_eq!(preview.paths, ["unstaged.txt", "untracked.txt"]);
        std::fs::write(worktree.join("new.txt"), "intervening\n").unwrap();
        std::fs::write(nested.join("extra.txt"), "nested untracked\n").unwrap();
        assert!(
            service
                .restore(
                    GitCheckpointRestoreRequest {
                        chat_id: "task".into(),
                        cwd: cwd.into(),
                        turn_id: row.turn_id.clone(),
                        point: "before".into(),
                        expected_token: preview.token.clone()
                    },
                    false,
                    |_| false
                )
                .await
                .unwrap_err()
                .contains("changed")
        );
        let refreshed = service
            .preview_restore("task", cwd, &row.turn_id, "before", false)
            .await
            .unwrap();
        let before_refusal = run(&worktree, &["status", "--porcelain"]);
        assert!(
            service
                .restore(
                    GitCheckpointRestoreRequest {
                        chat_id: "task".into(),
                        cwd: cwd.into(),
                        turn_id: row.turn_id.clone(),
                        point: "before".into(),
                        expected_token: refreshed.token.clone()
                    },
                    false,
                    |_| true
                )
                .await
                .unwrap_err()
                .contains("one task owner")
        );
        assert_eq!(run(&worktree, &["status", "--porcelain"]), before_refusal);
        let checks = AtomicUsize::new(0);
        assert!(
            service
                .restore(
                    GitCheckpointRestoreRequest {
                        chat_id: "task".into(),
                        cwd: cwd.into(),
                        turn_id: row.turn_id.clone(),
                        point: "before".into(),
                        expected_token: refreshed.token
                    },
                    false,
                    |_| {
                        if checks.fetch_add(1, Ordering::SeqCst) == 1 {
                            std::fs::write(
                                worktree.join("late.txt"),
                                "intervening during restore\n",
                            )
                            .unwrap();
                        }
                        false
                    }
                )
                .await
                .unwrap_err()
                .contains("changed during restore preparation")
        );
        assert!(worktree.join("late.txt").exists());
        let refreshed = service
            .preview_restore("task", cwd, &row.turn_id, "before", false)
            .await
            .unwrap();
        let receipt = service
            .restore(
                GitCheckpointRestoreRequest {
                    chat_id: "task".into(),
                    cwd: cwd.into(),
                    turn_id: row.turn_id.clone(),
                    point: "before".into(),
                    expected_token: refreshed.token,
                },
                false,
                |_| false,
            )
            .await
            .unwrap();
        assert!(receipt.restored);
        assert!(receipt.recovery_ref.starts_with("refs/luna/checkpoints/"));
        assert_eq!(run(&worktree, &["status", "--porcelain"]), original_status);
        assert_eq!(
            std::fs::read_to_string(worktree.join("untracked.txt")).unwrap(),
            "before\n"
        );
        assert!(!worktree.join("new.txt").exists());
        assert!(!worktree.join("late.txt").exists());
        assert!(!nested.join("extra.txt").exists());
        assert!(
            nested.is_dir(),
            "unrelated empty directories must survive restore"
        );
        assert!(
            service
                .preview_restore(
                    "task",
                    shared.to_str().unwrap(),
                    &row.turn_id,
                    "before",
                    false
                )
                .await
                .unwrap_err()
                .contains("shared checkouts")
        );
    }

    #[tokio::test]
    async fn sparse_capture_failure_is_visible() {
        let (_dir, service, _shared, worktree) = fixture();
        run(&worktree, &["config", "core.sparseCheckout", "true"]);
        let cwd = worktree.to_str().unwrap();
        service.start("task", "fixture-run", cwd).await;
        service.end("task", "fixture-run", "errored").await;
        let row = service
            .list("task", cwd, false)
            .await
            .unwrap()
            .checkpoints
            .remove(0);
        assert!(!row.before_available && !row.after_available);
        assert!(row.error.unwrap().contains("Sparse checkouts"));
    }

    #[tokio::test]
    async fn late_done_only_closes_its_own_run() {
        let (_dir, service, _shared, worktree) = fixture();
        let cwd = worktree.to_str().unwrap();
        service.start("task", "old-run", cwd).await;
        std::fs::write(worktree.join("old.txt"), "old\n").unwrap();
        service.start("task", "new-run", cwd).await;
        service.end("task", "old-run", "interrupted").await;
        let rows = service.list("task", cwd, false).await.unwrap().checkpoints;
        assert_eq!(rows.len(), 2);
        assert_eq!(rows.iter().filter(|row| row.after_available).count(), 1);
        let new = rows
            .iter()
            .find(|row| row.outcome.as_deref() == Some("in progress"))
            .unwrap();
        assert!(new.before_available && !new.after_available);
        std::fs::write(worktree.join("new.txt"), "new\n").unwrap();
        service.end("task", "new-run", "completed").await;
        let rows = service.list("task", cwd, false).await.unwrap().checkpoints;
        let old = rows
            .iter()
            .find(|row| row.outcome.as_deref() == Some("interrupted"))
            .unwrap();
        let new = rows
            .iter()
            .find(|row| row.outcome.as_deref() == Some("completed"))
            .unwrap();
        assert_eq!(
            service.diff("task", cwd, &old.turn_id).await.unwrap().paths,
            ["old.txt"]
        );
        assert_eq!(
            service.diff("task", cwd, &new.turn_id).await.unwrap().paths,
            ["new.txt"]
        );
    }
}
