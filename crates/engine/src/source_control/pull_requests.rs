//! Explicit draft creation. Reuses Git actions' checkout lane and freshness
//! token, source-control inspection and the bounded/cancellable gh runner.
use super::*;
use crate::{device_write::DeviceWriteHandle, git_actions::GitActions};
use luna_proto::{
    GitActionPreview, GitPrCheck, GitPrCreateOutcome, GitPrCreateReceipt, GitPrCreateRequest,
    GitPrDetails, GitPrDraftPreview,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::io::Write;

const DETAILS_FIELDS: &str = "number,title,url,state,baseRefName,headRefName,isDraft,body,isCrossRepository,headRepositoryOwner,headRepository,statusCheckRollup";

#[derive(Clone)]
pub struct GitHubPullRequests {
    actions: GitActions,
    journal: PathBuf,
    authority: Option<DeviceWriteHandle>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct Target {
    repository: String,
    base: String,
    head: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    version: u32,
    target: Target,
    attempts: Vec<Attempt>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Attempt {
    operation_id: String,
    head_sha: String,
    payload_hash: String,
    confirmed: Option<Confirmation>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Confirmation {
    number: u64,
    state: ChangeRequestState,
}
const HISTORY_LIMIT: usize = 256;

impl GitHubPullRequests {
    /// Standalone service for embeddings and deterministic subprocess fixtures.
    pub fn new(actions: GitActions, journal: PathBuf) -> Self {
        Self {
            actions,
            journal,
            authority: None,
        }
    }

    pub(crate) fn guarded(mut self, authority: DeviceWriteHandle) -> Self {
        self.authority = Some(authority);
        self
    }

    pub async fn preview(&self, cwd: &str) -> Result<GitPrDraftPreview, String> {
        tokio::time::timeout(Duration::from_secs(60), self.preview_inner(cwd))
            .await
            .map_err(|_| "PR preview timed out; retry".to_owned())?
    }

    async fn preview_inner(&self, cwd: &str) -> Result<GitPrDraftPreview, String> {
        let checkout = self.actions.preview(cwd).await?;
        let (source, repository) = self.source(&checkout).await?;
        let upstream = source.branch.upstream_ref.clone().ok_or_else(|| {
            "Push branch first with an upstream, then refresh the PR preview".to_owned()
        })?;
        if source.branch.remote_name != checkout.remote
            || source.branch.head_branch != checkout.branch
        {
            return Err("Tracking branch and selected push remote differ; align the upstream before creating a PR".into());
        }
        let info = self.api(&source, &repository, "", None).await?;
        if info.get("fork").and_then(Value::as_bool) != Some(false) {
            return Err("Draft creation currently supports same-repository branches; fork-to-upstream PRs must be created on GitHub".into());
        }
        let expected_name = repository
            .split_once('/')
            .map(|(_, name)| name)
            .unwrap_or("");
        if !info
            .get("full_name")
            .and_then(Value::as_str)
            .is_some_and(|name| name.eq_ignore_ascii_case(expected_name))
        {
            return Err("GitHub repository identity changed; refresh the preview".into());
        }
        self.verify_remote_head(&source, &repository, &checkout.branch, &checkout.head)
            .await?;
        let base = required(&info, "default_branch")?.to_owned();
        if base == checkout.branch {
            return Err("Choose a feature branch before creating a draft PR".into());
        }
        let title = self
            .git(&source.checkout_root, &["log", "-1", "--format=%s"])
            .await?;
        let after = self.actions.preview(cwd).await?;
        same_checkout(&checkout, &after)?;
        Ok(GitPrDraftPreview {
            operation_id: uuid::Uuid::new_v4().to_string(),
            head: checkout.branch.clone(),
            checkout,
            repository,
            upstream,
            base,
            title,
        })
    }

    async fn source(
        &self,
        checkout: &GitActionPreview,
    ) -> Result<(CheckoutSourceContext, String), String> {
        if checkout.branch == "HEAD" || checkout.head == "UNBORN" {
            return Err("Choose a committed branch before creating a PR".into());
        }
        let remote = checkout
            .remote
            .as_deref()
            .ok_or("Push branch first with an upstream, then refresh the PR preview")?;
        let mut source = ChangeRequestResolver::new()
            .inspect_checkout(Path::new(&checkout.cwd))
            .await
            .map_err(safe_error)?;
        let fetch = self
            .git(
                &source.checkout_root,
                &["remote", "get-url", "--all", remote],
            )
            .await?;
        let push = self
            .git(
                &source.checkout_root,
                &["remote", "get-url", "--push", "--all", remote],
            )
            .await?;
        // Multiple push URLs and split fetch/push identities cannot be inferred safely.
        if fetch.lines().count() != 1
            || push.lines().count() != 1
            || parse_git_remote(&fetch) != parse_git_remote(&push)
        {
            return Err("Fetch and push repositories differ; align the selected remote before creating a PR".into());
        }
        let parsed = parse_git_remote(&push).ok_or("GitHub is unavailable for this remote")?;
        if !valid_host(&parsed.host)
            || !valid_component(&parsed.owner)
            || !valid_component(&parsed.repository)
        {
            return Err("GitHub repository identity is invalid".into());
        }
        let repository =
            format!("{}/{}/{}", parsed.host, parsed.owner, parsed.repository).to_ascii_lowercase();
        // Keep upstream identity from the shared inspector; do not silently replace it.
        if source.branch.local_branch != checkout.branch {
            return Err("Checkout changed; refresh the PR preview".into());
        }
        if source.branch.remote_name.as_deref() != Some(remote) {
            return Err(
                "Tracking remote and selected push remote differ; align the upstream".into(),
            );
        }
        source.branch.remote_url = None; // Credential-bearing URLs never leave inspection.
        source.branch.host = Some(parsed.host.to_ascii_lowercase());
        source.branch.owner = Some(parsed.owner.to_ascii_lowercase());
        source.branch.repository = Some(parsed.repository.to_ascii_lowercase());
        Ok((source, repository))
    }

    pub async fn details(&self, cwd: &str, number: u64) -> Result<GitPrDetails, String> {
        tokio::time::timeout(Duration::from_secs(60), async {
            let before = self.actions.preview(cwd).await?;
            let (source, repository) = self.source(&before).await?;
            let details = self.read_details(&source, &repository, number).await?;
            if details.summary.head_ref != before.branch {
                return Err("This PR belongs to another branch".into());
            }
            same_checkout(&before, &self.actions.preview(cwd).await?)?;
            Ok(details)
        })
        .await
        .map_err(|_| "PR details timed out; retry".to_owned())?
    }

    pub async fn create(&self, request: GitPrCreateRequest) -> Result<GitPrCreateReceipt, String> {
        // Dropping this future kills any gh subprocess. Once journaled, cancellation
        // is uncertain by definition and a later call can only reconcile.
        match tokio::time::timeout(Duration::from_secs(100), self.create_inner(request)).await {
            Ok(Ok(receipt)) => Ok(receipt),
            Ok(Err(message)) => Ok(receipt(GitPrCreateOutcome::Rejected, None, &message)),
            Err(_) => Ok(receipt(
                GitPrCreateOutcome::Uncertain,
                None,
                "PR request timed out. Check GitHub to reconcile; an uncertain attempt is never sent twice",
            )),
        }
    }

    async fn create_inner(
        &self,
        request: GitPrCreateRequest,
    ) -> Result<GitPrCreateReceipt, String> {
        if request.title.trim().is_empty()
            || request.title.len() > 256
            || request.body.len() > 65536
        {
            return Err("Enter a title (up to 256 bytes) and a description under 64 KiB".into());
        }
        if request.head != request.preview.head || request.head != request.preview.checkout.branch {
            return Err("Head must match the current checkout branch; switch branches and refresh to change it".into());
        }
        if !valid_branch(&request.base) || request.base == request.head {
            return Err("Choose a valid base branch different from head".into());
        }
        let prior_target = Target {
            repository: request.preview.repository.clone(),
            base: request.base.clone(),
            head: request.head.clone(),
        };
        let current = match self.preview(&request.preview.checkout.cwd).await {
            Ok(current) => current,
            Err(error) if self.attempt_path(&prior_target).exists() => {
                return Ok(reconciliation_error(error));
            }
            Err(error) => return Err(error),
        };
        same_checkout(&request.preview.checkout, &current.checkout)?;
        if current.repository != request.preview.repository
            || current.upstream != request.preview.upstream
        {
            return Err("Repository or upstream changed; refresh the PR preview".into());
        }
        let root = Path::new(&current.checkout.cwd);
        let _lane = self.actions.lane(root).lock_owned().await;
        let _permit = self
            .authority
            .as_ref()
            .map(|a| a.try_admit())
            .transpose()
            .map_err(|_| {
                "This profile is retiring or being backed up; retry when it is ready".to_owned()
            })?;
        let target = Target {
            repository: current.repository.clone(),
            base: request.base.clone(),
            head: request.head.clone(),
        };
        let (source, repository) = self.source(&current.checkout).await?;
        let path = self.attempt_path(&target);
        let _file_lock = self.lock_target(&path)?;
        let mut journal = self.read_journal(&path, &target)?;
        let operation_id = &request.preview.operation_id;
        uuid::Uuid::parse_str(operation_id)
            .map_err(|_| "Invalid PR operation ID; refresh the preview")?;
        let payload_hash = format!(
            "{:x}",
            Sha256::digest(
                serde_json::to_vec(&json!({
                    "target": target, "title": request.title.trim(), "body": request.body,
                    "headSha": request.preview.checkout.head,
                }))
                .map_err(|_| "Could not hash PR intent")?
            )
        );
        if let Some(index) = journal
            .attempts
            .iter()
            .position(|a| &a.operation_id == operation_id)
        {
            if journal.attempts[index].payload_hash != payload_hash {
                return Err("This PR operation ID was already used with different content; open a fresh preview".into());
            }
            let details = if let Some(confirmed) = &journal.attempts[index].confirmed {
                Some(
                    match self.exact_details(&source, &target, confirmed.number).await {
                        Ok(details) => details,
                        Err(error) => return Ok(reconciliation_error(error)),
                    },
                )
            } else {
                match self.matching(&source, &target).await {
                    Ok(details) => details,
                    Err(error) => return Ok(reconciliation_error(error)),
                }
            };
            if let Some(details) = details {
                self.confirm(&path, &mut journal, index, &details)?;
                return Ok(receipt(
                    GitPrCreateOutcome::Existing,
                    Some(details),
                    "Linked this operation's pull request",
                ));
            }
            return Ok(unresolved());
        }
        // New intent: an unresolved predecessor can only be reconciled. A known
        // predecessor must be authoritatively terminal before a new POST.
        if let Some(index) = journal.attempts.len().checked_sub(1) {
            if let Some(confirmed) = &journal.attempts[index].confirmed {
                let details = match self.exact_details(&source, &target, confirmed.number).await {
                    Ok(details) => details,
                    Err(error) => return Ok(reconciliation_error(error)),
                };
                self.confirm(&path, &mut journal, index, &details)?;
                if details.summary.state == ChangeRequestState::Open || request.reconcile_only {
                    return Ok(receipt(
                        GitPrCreateOutcome::Existing,
                        Some(details),
                        "Linked the existing pull request",
                    ));
                }
            } else {
                let matching = match self.matching(&source, &target).await {
                    Ok(details) => details,
                    Err(error) => return Ok(reconciliation_error(error)),
                };
                if let Some(details) = matching {
                    self.confirm(&path, &mut journal, index, &details)?;
                    return Ok(receipt(
                        GitPrCreateOutcome::Existing,
                        Some(details),
                        "Reconciled the previous create attempt",
                    ));
                }
                return Ok(unresolved());
            }
        }
        if request.reconcile_only {
            return Ok(unresolved());
        }
        if journal.attempts.len() >= HISTORY_LIMIT {
            return Err("PR operation history reached its 256-attempt limit for this branch/base. Use a new branch; history is retained for duplicate protection".into());
        }
        if let Some(details) = self.matching(&source, &target).await? {
            journal.attempts.push(Attempt {
                operation_id: operation_id.clone(),
                head_sha: current.checkout.head.clone(),
                payload_hash,
                confirmed: None,
            });
            let index = journal.attempts.len() - 1;
            self.confirm(&path, &mut journal, index, &details)?;
            return Ok(receipt(
                GitPrCreateOutcome::Existing,
                Some(details),
                "Linked the existing pull request",
            ));
        }
        // Validate base existence and exact published head with API reads only.
        self.api(
            &source,
            &repository,
            &format!("/branches/{}", encode(&request.base)),
            None,
        )
        .await?;
        self.verify_remote_head(&source, &repository, &request.head, &current.checkout.head)
            .await?;
        let fresh = crate::git_actions::inspect(root, &current.checkout.checkout_id).await?;
        same_checkout(&current.checkout, &fresh)?;
        let (fresh_source, fresh_repository) = self.source(&fresh).await?;
        if fresh_repository != repository
            || fresh_source.branch.upstream_ref.as_deref() != Some(&current.upstream)
            || fresh_source.branch.head_branch != request.head
        {
            return Err("Repository or upstream changed; refresh the PR preview".into());
        }
        // The locked history is atomically replaced and synced BEFORE POST.
        journal.attempts.push(Attempt {
            operation_id: operation_id.clone(),
            head_sha: fresh.head,
            payload_hash,
            confirmed: None,
        });
        self.publish_journal(&path, &journal)?;
        let payload = json!({"title": request.title.trim(), "body": request.body,
            "base": request.base, "head": request.head, "draft": true});
        let created = self
            .api(&source, &repository, "/pulls", Some(payload))
            .await;
        // Even a successful CLI exit must be read back and identity-checked.
        match self.matching(&source, &target).await {
            Ok(Some(details)) => {
                let index = journal.attempts.len() - 1;
                if self.confirm(&path, &mut journal, index, &details).is_err() {
                    return Ok(receipt(
                        GitPrCreateOutcome::Uncertain,
                        None,
                        "GitHub returned the PR, but its confirmation could not be saved. Check GitHub to reconcile",
                    ));
                }
                Ok(receipt(
                    if created.is_ok() {
                        GitPrCreateOutcome::Created
                    } else {
                        GitPrCreateOutcome::Existing
                    },
                    Some(details),
                    "Draft pull request is linked",
                ))
            }
            Ok(None) => Ok(receipt(
                GitPrCreateOutcome::Uncertain,
                None,
                &format!(
                    "{}. Check GitHub to reconcile without creating a duplicate",
                    created
                        .err()
                        .unwrap_or_else(|| "GitHub has not confirmed this create attempt".into())
                ),
            )),
            Err(error) => Ok(receipt(
                GitPrCreateOutcome::Uncertain,
                None,
                &format!(
                    "Create outcome unresolved: {error}. Retry to check GitHub without sending another create request"
                ),
            )),
        }
    }

    async fn matching(
        &self,
        source: &CheckoutSourceContext,
        target: &Target,
    ) -> Result<Option<GitPrDetails>, String> {
        // Reuse the shared provider's typed candidate validation and selection,
        // but bound the query to this exact base and open state before selection.
        let value = self
            .gh(
                source,
                vec![
                    "pr".into(),
                    "list".into(),
                    "--repo".into(),
                    target.repository.clone(),
                    "--head".into(),
                    target.head.clone(),
                    "--base".into(),
                    target.base.clone(),
                    "--state".into(),
                    "open".into(),
                    "--limit".into(),
                    "100".into(),
                    "--json".into(),
                    GITHUB_JSON_FIELDS.into(),
                ],
            )
            .await?;
        let candidates: Vec<GhPullRequest> =
            serde_json::from_value(value).map_err(|_| "GitHub returned invalid PR candidates")?;
        let candidate =
            select_pull_request("github", source, None, candidates).map_err(safe_error)?;
        let Some(candidate) = candidate else {
            return Ok(None);
        };
        if candidate.state != ChangeRequestState::Open
            || candidate.base_ref != target.base
            || candidate.head_ref != target.head
        {
            return Ok(None);
        }
        let details = self
            .read_details(source, &target.repository, candidate.number)
            .await?;
        if details.summary.base_ref != target.base
            || details.summary.head_ref != target.head
            || details.summary.state != ChangeRequestState::Open
        {
            return Ok(None);
        }
        Ok(Some(details))
    }

    async fn exact_details(
        &self,
        source: &CheckoutSourceContext,
        target: &Target,
        number: u64,
    ) -> Result<GitPrDetails, String> {
        let details = self
            .read_details(source, &target.repository, number)
            .await?;
        if details.summary.base_ref != target.base || details.summary.head_ref != target.head {
            return Err(
                "Previous PR target changed on GitHub; cannot safely create another PR".into(),
            );
        }
        Ok(details)
    }

    async fn read_details(
        &self,
        source: &CheckoutSourceContext,
        repository: &str,
        number: u64,
    ) -> Result<GitPrDetails, String> {
        let value = self
            .gh(
                source,
                vec![
                    "pr".into(),
                    "view".into(),
                    number.to_string(),
                    "--repo".into(),
                    repository.into(),
                    "--json".into(),
                    DETAILS_FIELDS.into(),
                ],
            )
            .await?;
        if value["isCrossRepository"].as_bool() != Some(false)
            || !value["headRepositoryOwner"]["login"]
                .as_str()
                .is_some_and(|s| Some(s.to_ascii_lowercase()) == source.branch.owner)
            || !value["headRepository"]["name"]
                .as_str()
                .is_some_and(|s| Some(s.to_ascii_lowercase()) == source.branch.repository)
        {
            return Err("PR repository identity does not match this checkout; fork PR details are not supported yet".into());
        }
        let url = required(&value, "url")?;
        let expected = format!("https://{repository}/pull/{number}");
        if !url.eq_ignore_ascii_case(&expected) || value["number"].as_u64() != Some(number) {
            return Err("GitHub returned a different pull request identity".into());
        }
        let state = match required(&value, "state")? {
            "OPEN" => ChangeRequestState::Open,
            "CLOSED" => ChangeRequestState::Closed,
            "MERGED" => ChangeRequestState::Merged,
            _ => return Err("GitHub returned an invalid PR state".into()),
        };
        let checks = value["statusCheckRollup"]
            .as_array()
            .ok_or("GitHub returned invalid check data")?
            .iter()
            .take(100)
            .map(|check| {
                let name = check["name"]
                    .as_str()
                    .or(check["context"].as_str())
                    .unwrap_or("Check")
                    .to_owned();
                let status = check["conclusion"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .or(check["status"].as_str())
                    .or(check["state"].as_str())
                    .unwrap_or("UNKNOWN")
                    .to_owned();
                let url = check["detailsUrl"]
                    .as_str()
                    .or(check["targetUrl"].as_str())
                    .filter(|u| valid_web_url(u))
                    .map(str::to_owned);
                GitPrCheck { name, status, url }
            })
            .collect();
        Ok(GitPrDetails {
            summary: ChangeRequestSummary {
                provider: "github".into(),
                number,
                title: required(&value, "title")?.into(),
                url: expected,
                state,
                base_ref: required(&value, "baseRefName")?.into(),
                head_ref: required(&value, "headRefName")?.into(),
            },
            is_draft: value["isDraft"]
                .as_bool()
                .ok_or("GitHub returned invalid draft data")?,
            body: value["body"].as_str().unwrap_or("").to_owned(),
            checks,
        })
    }

    async fn verify_remote_head(
        &self,
        source: &CheckoutSourceContext,
        repository: &str,
        branch: &str,
        sha: &str,
    ) -> Result<(), String> {
        let value = self
            .api(
                source,
                repository,
                &format!("/branches/{}", encode(branch)),
                None,
            )
            .await.map_err(|error| if error == "GitHub request failed" {
                "Push branch first, then refresh. GitHub could not read this branch; check its publication and your repository access".to_owned()
            } else { error })?;
        if value["commit"]["sha"].as_str() != Some(sha) {
            return Err("Push branch first: GitHub's branch does not match this checkout. Refresh after publishing or reconciling divergence".into());
        }
        Ok(())
    }

    async fn api(
        &self,
        source: &CheckoutSourceContext,
        repository: &str,
        suffix: &str,
        payload: Option<Value>,
    ) -> Result<Value, String> {
        let (host, name) = repository.split_once('/').ok_or("Invalid repository")?;
        let mut args = vec![
            "api".into(),
            "--hostname".into(),
            host.into(),
            format!("repos/{name}{suffix}"),
        ];
        let mut input = None;
        if let Some(payload) = payload {
            let mut file =
                tempfile::NamedTempFile::new().map_err(|_| "Could not prepare PR request")?;
            serde_json::to_writer(&mut file, &payload)
                .map_err(|_| "Could not encode PR request")?;
            file.flush().map_err(|_| "Could not prepare PR request")?;
            args.extend([
                "--method".into(),
                "POST".into(),
                "--input".into(),
                file.path().to_string_lossy().into_owned(),
            ]);
            input = Some(file);
        }
        let result = self.gh(source, args).await;
        drop(input);
        result
    }

    async fn gh(&self, source: &CheckoutSourceContext, args: Vec<String>) -> Result<Value, String> {
        let output = SystemProcessRunner
            .run(ProcessRequest {
                program: "gh".into(),
                args,
                cwd: source.checkout_root.clone(),
                env: vec![
                    ("GH_PROMPT_DISABLED".into(), "1".into()),
                    ("GH_PAGER".into(), "cat".into()),
                ],
                timeout: GITHUB_TIMEOUT,
                output_limit: GITHUB_OUTPUT_LIMIT,
            })
            .await
            .map_err(|e| safe_error(classify_run_error(e)))?;
        if !output.success {
            return Err(safe_error(classify_github_failure(&output.stderr)));
        }
        if output.stdout_truncated {
            return Err("GitHub response exceeded the output limit".into());
        }
        serde_json::from_slice(&output.stdout)
            .map_err(|_| "GitHub returned an invalid response".into())
    }

    async fn git(&self, root: &Path, args: &[&str]) -> Result<String, String> {
        GitCheckoutInspector::new(Arc::new(SystemProcessRunner))
            .git_required(root, args)
            .await
            .map_err(safe_error)
    }

    fn attempt_path(&self, target: &Target) -> PathBuf {
        let digest = Sha256::digest(serde_json::to_vec(target).expect("serializable PR target"));
        self.journal.join(format!("{digest:x}.json"))
    }

    fn lock_target(&self, path: &Path) -> Result<std::fs::File, String> {
        std::fs::create_dir_all(&self.journal)
            .map_err(|_| "Could not create PR attempt directory")?;
        let metadata = std::fs::symlink_metadata(&self.journal)
            .map_err(|_| "Could not inspect PR attempt directory")?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err("Unsafe PR attempt directory".into());
        }
        let lock_path = path.with_extension("lock");
        if let Ok(meta) = std::fs::symlink_metadata(&lock_path) {
            if !meta.is_file() || meta.file_type().is_symlink() {
                return Err("Unsafe PR operation lock".into());
            }
        }
        let mut options = std::fs::OpenOptions::new();
        options.create(true).read(true).write(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let file = options
            .open(lock_path)
            .map_err(|_| "Cannot open PR operation lock; check profile permissions")?;
        match file.try_lock() {
            Ok(()) => Ok(file),
            Err(std::fs::TryLockError::WouldBlock) => Err("Another PR operation is active for this repository/branch. Check GitHub again shortly".into()),
            Err(std::fs::TryLockError::Error(_)) => Err("Cannot lock PR operation history; creation is blocked".into()),
        }
    }

    fn read_journal(&self, path: &Path, target: &Target) -> Result<Journal, String> {
        match std::fs::symlink_metadata(path) {
            Ok(meta)
                if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > 1024 * 1024 =>
            {
                return Err("Previous PR history is unsafe; creation is blocked".into());
            }
            Err(error) if error.kind() != io::ErrorKind::NotFound => {
                return Err("Could not inspect previous PR history".into());
            }
            _ => {}
        }
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Ok(Journal {
                    version: 2,
                    target: target.clone(),
                    attempts: vec![],
                });
            }
            Err(_) => return Err("Could not read previous PR history; creation is blocked".into()),
        };
        let journal: Journal = serde_json::from_slice(&bytes)
            .map_err(|_| "Previous PR history is invalid; creation is blocked")?;
        let mut ids = HashSet::new();
        if journal.version != 2
            || journal.target != *target
            || journal.attempts.len() > HISTORY_LIMIT
            || journal.attempts.iter().any(|a| {
                uuid::Uuid::parse_str(&a.operation_id).is_err()
                    || !ids.insert(&a.operation_id)
                    || !matches!(a.head_sha.len(), 40 | 64)
                    || !a.head_sha.bytes().all(|c| c.is_ascii_hexdigit())
                    || a.payload_hash.len() != 64
                    || !a.payload_hash.bytes().all(|c| c.is_ascii_hexdigit())
                    || a.confirmed.as_ref().is_some_and(|c| c.number == 0)
            })
        {
            return Err("Previous PR operation identity is invalid; creation is blocked".into());
        }
        Ok(journal)
    }

    fn confirm(
        &self,
        path: &Path,
        journal: &mut Journal,
        index: usize,
        details: &GitPrDetails,
    ) -> Result<(), String> {
        journal.attempts[index].confirmed = Some(Confirmation {
            number: details.summary.number,
            state: details.summary.state,
        });
        self.publish_journal(path, journal)
    }

    fn publish_journal(&self, path: &Path, journal: &Journal) -> Result<(), String> {
        #[cfg(unix)]
        if let Some(parent) = self.journal.parent() {
            std::fs::File::open(parent)
                .and_then(|f| f.sync_all())
                .map_err(|_| "Could not sync PR history parent")?;
        }
        let mut file = tempfile::Builder::new()
            .prefix(".pending-pr-")
            .tempfile_in(&self.journal)
            .map_err(|_| "Could not persist PR history")?;
        serde_json::to_writer(&mut file, journal).map_err(|_| "Could not encode PR history")?;
        file.as_file()
            .sync_all()
            .map_err(|_| "Could not sync PR history")?;
        file.persist(path)
            .map_err(|_| "Could not publish PR history")?;
        #[cfg(unix)]
        std::fs::File::open(&self.journal)
            .and_then(|f| f.sync_all())
            .map_err(|_| "Could not sync PR history directory")?;
        Ok(())
    }
}

fn reconciliation_error(error: String) -> GitPrCreateReceipt {
    receipt(
        GitPrCreateOutcome::Uncertain,
        None,
        &format!(
            "Previous PR outcome could not be reconciled: {error}. Check GitHub again after resolving access or identity; no create request was sent"
        ),
    )
}

fn unresolved() -> GitPrCreateReceipt {
    receipt(
        GitPrCreateOutcome::Uncertain,
        None,
        "The previous create outcome is unresolved. Check GitHub to reconcile; Luna will not send another create request",
    )
}

fn same_checkout(a: &GitActionPreview, b: &GitActionPreview) -> Result<(), String> {
    if a.checkout_id != b.checkout_id
        || a.token != b.token
        || a.cwd != b.cwd
        || a.branch != b.branch
        || a.remote != b.remote
        || a.head != b.head
    {
        Err("Checkout changed; refresh the PR preview before creating".into())
    } else {
        Ok(())
    }
}
fn receipt(
    outcome: GitPrCreateOutcome,
    details: Option<GitPrDetails>,
    message: &str,
) -> GitPrCreateReceipt {
    GitPrCreateReceipt {
        outcome,
        details,
        message: message.into(),
    }
}
fn safe_error(error: ChangeRequestError) -> String {
    match error {
        ChangeRequestError::CliUnavailable => {
            "GitHub CLI is not installed. Install gh, then retry".into()
        }
        ChangeRequestError::Authentication => {
            "GitHub CLI is not authenticated for this host. Run gh auth login, then retry".into()
        }
        other => other.to_string(),
    }
}
fn required<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value[key]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "GitHub returned an invalid response".into())
}
fn valid_component(s: &str) -> bool {
    !s.is_empty()
        && s != "."
        && s != ".."
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
}
fn valid_host(s: &str) -> bool {
    s.contains('.') && s.split('.').all(valid_component)
}
fn valid_branch(s: &str) -> bool {
    !s.is_empty()
        && s.len() < 256
        && !s.starts_with('-')
        && !s.contains("..")
        && !s.contains("@{")
        && !s
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || "~^:?*[\\".contains(c))
}
fn encode(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}
fn valid_web_url(s: &str) -> bool {
    url::Url::parse(s).is_ok_and(|u| {
        u.scheme() == "https"
            && u.host_str().is_some()
            && u.username().is_empty()
            && u.password().is_none()
    })
}
