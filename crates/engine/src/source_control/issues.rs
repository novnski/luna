//! GitHub issue source for the project board (PB-2 reads, PB-3 writeback).
//!
//! Repository identity comes from the checkout's configured remotes (read
//! with `git remote`/`git remote get-url`, never remote transport), and every
//! GitHub call is one bounded, cancellable `gh api` subprocess on the
//! project's owning host using its existing authentication: fixed argv (no
//! shell, no agent-generated strings), `--include` so status, `ETag`, `Link`
//! and rate-limit headers are visible, conditional reads with
//! `If-None-Match`, and structured failure classes that never carry command
//! output. Caching, account scoping, dedupe and reconciliation policy live in
//! `board/github.rs`; this layer only talks to `git` and `gh`.
use super::*;
use std::collections::BTreeMap;
use std::io::Write as _;

use serde_json::Value;

/// Issue lists carry bodies; allow more than PR metadata but stay bounded.
const ISSUES_OUTPUT_LIMIT: usize = 4 * 1024 * 1024;
/// At most this many remotes are inspected per checkout.
pub const ISSUE_REMOTES_MAX: usize = 8;
/// Test/diagnostic override for the per-call `gh` timeout, clamped.
const TIMEOUT_ENV: &str = "LUNA_BOARD_GITHUB_TIMEOUT_MS";

/// A checkout remote that names a GitHub-style `host/owner/name`. The URL
/// itself (which may carry credentials) is never retained.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueRemote {
    pub name: String,
    pub host: String,
    pub owner: String,
    pub repository: String,
}

impl IssueRemote {
    /// Stable source id: `github:{host}:{owner}/{name}` (lower case).
    pub fn source_id(&self) -> String {
        issue_source_id(&self.host, &self.owner, &self.repository)
    }
}

/// `github:{host}:{owner}/{name}`, lower case.
pub fn issue_source_id(host: &str, owner: &str, repository: &str) -> String {
    format!(
        "github:{}:{}/{}",
        host.to_ascii_lowercase(),
        owner.to_ascii_lowercase(),
        repository.to_ascii_lowercase()
    )
}

/// Parse a source id back into `(host, owner, name)`.
pub fn parse_issue_source_id(source_id: &str) -> Option<(String, String, String)> {
    let rest = source_id.strip_prefix("github:")?;
    let (host, path) = rest.split_once(':')?;
    let (owner, name) = path.split_once('/')?;
    (valid_host(host) && valid_component(owner) && valid_component(name)).then(|| {
        (
            host.to_ascii_lowercase(),
            owner.to_ascii_lowercase(),
            name.to_ascii_lowercase(),
        )
    })
}

/// Safe failure classes. Variants never contain command output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum IssueApiError {
    #[error("Git repository context is unavailable")]
    RepositoryUnavailable,
    #[error("GitHub CLI is not installed")]
    CliUnavailable,
    #[error("GitHub CLI is not authenticated for this host")]
    Authentication,
    #[error("The repository or issue was not found for this account")]
    NotFound,
    #[error("GitHub refused access for this account")]
    Forbidden,
    #[error("GitHub API rate limit reached")]
    RateLimited { retry_after_ms: Option<u64> },
    #[error("GitHub request timed out")]
    Timeout,
    #[error("GitHub response exceeded the size limit")]
    TooLarge,
    #[error("GitHub returned an invalid response")]
    Decode,
    #[error("GitHub rejected the request (HTTP {status})")]
    Rejected { status: u16 },
    #[error("GitHub request failed")]
    Failed,
}

impl IssueApiError {
    /// GitHub refused before applying anything (a write can be retried
    /// safely only after one of these).
    pub fn refused_before_write(self) -> bool {
        matches!(
            self,
            Self::CliUnavailable
                | Self::Authentication
                | Self::NotFound
                | Self::Forbidden
                | Self::RateLimited { .. }
                | Self::Rejected { .. }
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiMethod {
    Get,
    Post,
    Patch,
    Delete,
}

impl ApiMethod {
    fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Patch => "PATCH",
            Self::Delete => "DELETE",
        }
    }
}

/// One `gh api --include` response: status, lower-cased headers, JSON body
/// (`Null` for 204/304).
#[derive(Debug, Clone)]
pub struct ApiResponse {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: Value,
}

impl ApiResponse {
    pub fn not_modified(&self) -> bool {
        self.status == 304
    }

    pub fn etag(&self) -> Option<String> {
        self.headers.get("etag").cloned()
    }

    /// A `Link` header advertises a next page.
    pub fn has_next(&self) -> bool {
        self.headers
            .get("link")
            .is_some_and(|link| link.split(',').any(|part| part.contains("rel=\"next\"")))
    }
}

/// The authenticated account behind the host's `gh` credentials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubAccount {
    pub login: String,
    /// Stable numeric id (a login can be renamed).
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawRepository {
    pub id: String,
    pub owner: String,
    pub name: String,
    pub private: bool,
    pub fork: bool,
    pub parent: Option<String>,
    pub has_issues: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawIssue {
    pub id: String,
    pub number: u64,
    pub title: String,
    pub state: String,
    pub state_reason: Option<String>,
    pub labels: Vec<String>,
    pub assignees: Vec<String>,
    pub updated_at: String,
    pub comments: u32,
    pub html_url: String,
    pub repository_url: String,
    pub body: String,
    pub pull_request: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawComment {
    pub id: String,
    pub author: String,
    pub created_at: String,
    pub updated_at: String,
    pub body: String,
    pub html_url: String,
    pub issue_url: String,
}

/// Bounded `git`/`gh api` access for issue sources.
#[derive(Clone)]
pub struct GitHubIssueApi {
    runner: Arc<dyn ProcessRunner>,
    timeout: Duration,
}

impl Default for GitHubIssueApi {
    fn default() -> Self {
        Self::new()
    }
}

impl GitHubIssueApi {
    pub fn new() -> Self {
        let timeout = std::env::var(TIMEOUT_ENV)
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .map(|ms| Duration::from_millis(ms.clamp(500, 60_000)))
            .unwrap_or(GITHUB_TIMEOUT);
        Self {
            runner: Arc::new(SystemProcessRunner),
            timeout,
        }
    }

    /// Remotes of the checkout at `cwd` that parse as `host/owner/name`, in
    /// Git's order. Reads configuration only (never remote transport).
    pub async fn remotes(&self, cwd: &Path) -> Result<Vec<IssueRemote>, IssueApiError> {
        let git = GitCheckoutInspector::new(self.runner.clone());
        let root = git
            .git(cwd, &["rev-parse", "--show-toplevel"])
            .await
            .filter(|root| !root.is_empty())
            .ok_or(IssueApiError::RepositoryUnavailable)?;
        let root = PathBuf::from(root);
        let names = git
            .git(&root, &["remote"])
            .await
            .ok_or(IssueApiError::RepositoryUnavailable)?;
        let mut remotes = Vec::new();
        for name in names
            .lines()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .take(ISSUE_REMOTES_MAX)
        {
            let Some(url) = git.git(&root, &["remote", "get-url", name]).await else {
                continue;
            };
            let Some(parsed) = url.lines().next().and_then(parse_git_remote) else {
                continue;
            };
            if !valid_host(&parsed.host)
                || !valid_component(&parsed.owner)
                || !valid_component(&parsed.repository)
            {
                continue;
            }
            remotes.push(IssueRemote {
                name: name.to_owned(),
                host: parsed.host,
                owner: parsed.owner,
                repository: parsed.repository,
            });
        }
        Ok(remotes)
    }

    /// One REST call. `etag` makes a GET conditional (a 304 comes back as
    /// `status == 304` with a null body). Writes send `payload` as a JSON
    /// file (`--input`), never interpolated into argv.
    pub async fn request(
        &self,
        cwd: &Path,
        host: &str,
        method: ApiMethod,
        endpoint: &str,
        etag: Option<&str>,
        payload: Option<&Value>,
    ) -> Result<ApiResponse, IssueApiError> {
        if !valid_host(host) || !valid_endpoint(endpoint) {
            return Err(IssueApiError::Failed);
        }
        let mut args = vec![
            "api".to_owned(),
            "--hostname".into(),
            host.to_owned(),
            "--include".into(),
            "--method".into(),
            method.as_str().into(),
            endpoint.to_owned(),
        ];
        if let Some(etag) = etag.filter(|etag| valid_etag(etag)) {
            args.extend(["-H".into(), format!("If-None-Match: {etag}")]);
        }
        let mut input = None;
        if let Some(payload) = payload {
            let mut file = tempfile::NamedTempFile::new().map_err(|_| IssueApiError::Failed)?;
            serde_json::to_writer(&mut file, payload).map_err(|_| IssueApiError::Failed)?;
            file.flush().map_err(|_| IssueApiError::Failed)?;
            args.extend(["--input".into(), file.path().to_string_lossy().into_owned()]);
            input = Some(file);
        }
        let output = self
            .runner
            .run(ProcessRequest {
                program: "gh".into(),
                args,
                cwd: cwd.to_owned(),
                env: vec![
                    ("GH_PROMPT_DISABLED".into(), "1".into()),
                    ("GH_PAGER".into(), "cat".into()),
                    ("NO_COLOR".into(), "1".into()),
                ],
                timeout: self.timeout,
                output_limit: ISSUES_OUTPUT_LIMIT,
            })
            .await;
        drop(input);
        let output = output.map_err(|error| match error {
            ProcessRunError::Spawn(io::ErrorKind::NotFound) => IssueApiError::CliUnavailable,
            ProcessRunError::Timeout => IssueApiError::Timeout,
            ProcessRunError::Spawn(_) | ProcessRunError::Io => IssueApiError::Failed,
        })?;
        if output.stdout_truncated {
            return Err(IssueApiError::TooLarge);
        }
        let parsed = parse_included(&output.stdout);
        let (status, headers, body) = match parsed {
            Some(parsed) => parsed,
            None => {
                // No HTTP response was printed: gh refused before sending
                // (not logged in) or failed in transport. `gh` reports an
                // HTTP status in stderr as "(HTTP 404)".
                if let Some(status) = status_from_stderr(&output.stderr) {
                    (status, BTreeMap::new(), Vec::new())
                } else if output.success {
                    return Err(IssueApiError::Decode);
                } else {
                    return Err(match classify_github_failure(&output.stderr) {
                        ChangeRequestError::Authentication => IssueApiError::Authentication,
                        ChangeRequestError::RateLimited => IssueApiError::RateLimited {
                            retry_after_ms: None,
                        },
                        _ => IssueApiError::Failed,
                    });
                }
            }
        };
        let json = if body.iter().all(u8::is_ascii_whitespace) {
            Value::Null
        } else {
            match serde_json::from_slice::<Value>(&body) {
                Ok(value) => value,
                Err(_) if !(200..300).contains(&status) => Value::Null,
                Err(_) => return Err(IssueApiError::Decode),
            }
        };
        match status {
            200..=299 | 304 => Ok(ApiResponse {
                status,
                headers,
                body: json,
            }),
            401 => Err(IssueApiError::Authentication),
            403 | 429 => {
                let message = json["message"].as_str().unwrap_or("").to_ascii_lowercase();
                let exhausted = headers
                    .get("x-ratelimit-remaining")
                    .is_some_and(|remaining| remaining.trim() == "0");
                if status == 429
                    || exhausted
                    || headers.contains_key("retry-after")
                    || message.contains("rate limit")
                {
                    Err(IssueApiError::RateLimited {
                        retry_after_ms: retry_after_ms(&headers),
                    })
                } else {
                    Err(IssueApiError::Forbidden)
                }
            }
            404 | 410 => Err(IssueApiError::NotFound),
            400..=499 => Err(IssueApiError::Rejected { status }),
            _ => Err(IssueApiError::Failed),
        }
    }
}

/// Split `gh api --include` output into status, headers and body.
fn parse_included(stdout: &[u8]) -> Option<(u16, BTreeMap<String, String>, Vec<u8>)> {
    if !stdout.starts_with(b"HTTP/") {
        return None;
    }
    // `gh` ends the status line with LF and each header with CRLF.
    let (head, body) = [
        b"\r\n\r\n".as_slice(),
        b"\n\r\n".as_slice(),
        b"\n\n".as_slice(),
    ]
    .iter()
    .filter_map(|separator| {
        stdout
            .windows(separator.len())
            .position(|window| window == *separator)
            .map(|at| (at, separator.len()))
    })
    .min_by_key(|(at, _)| *at)
    .map(|(at, len)| (&stdout[..at], stdout[at + len..].to_vec()))
    .unwrap_or((stdout, Vec::new()));
    let head = String::from_utf8_lossy(head);
    let mut lines = head.lines();
    let status = lines
        .next()?
        .split_whitespace()
        .nth(1)?
        .parse::<u16>()
        .ok()?;
    let mut headers = BTreeMap::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(
                name.trim().to_ascii_lowercase(),
                value.trim().trim_end_matches('\r').to_owned(),
            );
        }
    }
    Some((status, headers, body))
}

/// `gh: Not Found (HTTP 404)` → 404.
fn status_from_stderr(stderr: &[u8]) -> Option<u16> {
    let text = String::from_utf8_lossy(stderr);
    let at = text.find("HTTP ")?;
    let digits: String = text[at + 5..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits
        .parse::<u16>()
        .ok()
        .filter(|status| (100..600).contains(status))
}

fn retry_after_ms(headers: &BTreeMap<String, String>) -> Option<u64> {
    if let Some(seconds) = headers
        .get("retry-after")
        .and_then(|value| value.trim().parse::<u64>().ok())
    {
        return Some(seconds.saturating_mul(1000).max(1000));
    }
    let reset = headers
        .get("x-ratelimit-reset")
        .and_then(|value| value.trim().parse::<i64>().ok())?;
    let now = Utc::now().timestamp();
    Some(((reset - now).max(1) as u64).saturating_mul(1000))
}

// ---------------------------------------------------------------------------
// Typed views of REST payloads (unknown fields ignored, required ones checked)
// ---------------------------------------------------------------------------

fn id_string(value: &Value) -> Option<String> {
    match value {
        Value::Number(number) => number.as_u64().map(|id| id.to_string()),
        Value::String(id) if !id.is_empty() => Some(id.clone()),
        _ => None,
    }
}

pub fn parse_account(value: &Value) -> Option<GitHubAccount> {
    let login = value["login"].as_str().filter(|login| valid_login(login))?;
    Some(GitHubAccount {
        login: login.to_owned(),
        id: id_string(&value["id"])?,
    })
}

pub fn parse_repository(value: &Value) -> Option<RawRepository> {
    let (owner, name) = value["full_name"].as_str()?.split_once('/')?;
    if !valid_component(owner) || !valid_component(name) {
        return None;
    }
    Some(RawRepository {
        id: id_string(&value["id"])?,
        owner: owner.to_owned(),
        name: name.to_owned(),
        private: value["private"].as_bool().unwrap_or(true),
        fork: value["fork"].as_bool().unwrap_or(false),
        parent: value["parent"]["full_name"].as_str().map(str::to_owned),
        has_issues: value["has_issues"].as_bool().unwrap_or(true),
    })
}

pub fn parse_issue(value: &Value) -> Option<RawIssue> {
    let names = |key: &str, field: &str| -> Vec<String> {
        value[key]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| match item {
                        Value::String(name) => Some(name.clone()),
                        other => other[field].as_str().map(str::to_owned),
                    })
                    .take(64)
                    .collect()
            })
            .unwrap_or_default()
    };
    Some(RawIssue {
        id: id_string(&value["id"])?,
        number: value["number"].as_u64().filter(|number| *number > 0)?,
        title: value["title"].as_str()?.to_owned(),
        state: value["state"].as_str()?.to_ascii_lowercase(),
        state_reason: value["state_reason"].as_str().map(str::to_owned),
        labels: names("labels", "name"),
        assignees: names("assignees", "login"),
        updated_at: value["updated_at"].as_str()?.to_owned(),
        comments: value["comments"]
            .as_u64()
            .map_or(0, |count| count.min(u32::MAX as u64) as u32),
        html_url: value["html_url"].as_str()?.to_owned(),
        repository_url: value["repository_url"].as_str().unwrap_or("").to_owned(),
        // An empty description is `null` in the API.
        body: value["body"].as_str().unwrap_or("").to_owned(),
        pull_request: value.get("pull_request").is_some_and(|pr| !pr.is_null()),
    })
}

pub fn parse_comment(value: &Value) -> Option<RawComment> {
    Some(RawComment {
        id: id_string(&value["id"])?,
        author: value["user"]["login"]
            .as_str()
            .unwrap_or("ghost")
            .to_owned(),
        created_at: value["created_at"].as_str()?.to_owned(),
        updated_at: value["updated_at"]
            .as_str()
            .or(value["created_at"].as_str())?
            .to_owned(),
        body: value["body"].as_str().unwrap_or("").to_owned(),
        html_url: value["html_url"].as_str().unwrap_or("").to_owned(),
        issue_url: value["issue_url"].as_str().unwrap_or("").to_owned(),
    })
}

/// `…/repos/{owner}/{name}` in an API URL names this repository.
pub fn api_url_names_repository(url: &str, owner: &str, name: &str) -> bool {
    url.to_ascii_lowercase().ends_with(&format!(
        "/repos/{}/{}",
        owner.to_ascii_lowercase(),
        name.to_ascii_lowercase()
    ))
}

/// A browser URL is https on the expected host (no credentials).
pub fn valid_issue_web_url(url: &str, host: &str) -> bool {
    url::Url::parse(url).is_ok_and(|parsed| {
        parsed.scheme() == "https"
            && parsed.username().is_empty()
            && parsed.password().is_none()
            && parsed
                .host_str()
                .is_some_and(|actual| actual.eq_ignore_ascii_case(host))
    })
}

/// Percent-encode one query/path component.
pub fn encode_component(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

pub fn valid_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 100
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
}

pub fn valid_host(value: &str) -> bool {
    value.len() <= 253 && value.contains('.') && value.split('.').all(valid_component)
}

/// GitHub logins: 1-39 ASCII letters, digits and single hyphens.
pub fn valid_login(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 39
        && !value.starts_with('-')
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_' || c == b'[' || c == b']')
}

fn valid_endpoint(endpoint: &str) -> bool {
    !endpoint.is_empty()
        && endpoint.len() <= 2048
        && !endpoint.starts_with('-')
        && !endpoint.starts_with('/')
        && !endpoint.contains("..")
        && endpoint.bytes().all(|c| c.is_ascii_graphic() && c != b'\\')
}

fn valid_etag(etag: &str) -> bool {
    etag.len() <= 256 && etag.bytes().all(|c| c.is_ascii_graphic() || c == b' ')
}
