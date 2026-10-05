//! Read-only PR observation using the existing bounded, cancellable gh runner.
//! No body, URL or repository value is interpreted as a command or instruction.
use super::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[async_trait]
pub trait GithubReadApi: Send + Sync {
    async fn get(&self, cwd: &Path, path: &str) -> Result<Value, String>;
}
#[async_trait]
impl GithubReadApi for GitHubCli {
    async fn get(&self, cwd: &Path, path: &str) -> Result<Value, String> {
        let output = self
            .runner
            .run(ProcessRequest {
                program: "gh".into(),
                args: vec![
                    "api".into(),
                    "--hostname".into(),
                    "github.com".into(),
                    "--method".into(),
                    "GET".into(),
                    path.into(),
                ],
                cwd: cwd.into(),
                env: vec![
                    ("GH_PROMPT_DISABLED".into(), "1".into()),
                    ("GH_PAGER".into(), "cat".into()),
                ],
                timeout: GITHUB_TIMEOUT,
                output_limit: GITHUB_OUTPUT_LIMIT,
            })
            .await
            .map_err(|e| classify_run_error(e).to_string())?;
        if !output.success {
            return Err(classify_github_failure(&output.stderr).to_string());
        }
        if output.stdout_truncated {
            return Err(
                "GitHub observation exceeded the bounded output limit; state is unknown".into(),
            );
        }
        serde_json::from_slice(&output.stdout)
            .map_err(|_| "GitHub returned invalid JSON; state is unknown".into())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrObservation {
    pub head_oid: String,
    pub state: String,
    pub checks: BTreeMap<String, String>,
    pub remarks: BTreeMap<String, String>,
}
fn required<'a>(v: &'a Value, key: &str) -> Result<&'a str, String> {
    v.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("GitHub response is missing {key}; state is unknown"))
}
fn rows<'a>(v: &'a Value, key: Option<&str>) -> Result<&'a Vec<Value>, String> {
    let v = key.map_or(v, |k| &v[k]);
    v.as_array()
        .ok_or_else(|| "GitHub returned an incomplete page; state is unknown".into())
}
async fn pages(
    api: &dyn GithubReadApi,
    cwd: &Path,
    path: &str,
    key: Option<&str>,
) -> Result<Vec<Value>, String> {
    let mut result = Vec::new();
    let mut expected = None;
    let mut sha = None;
    for page in 1..=10 {
        let v = api
            .get(cwd, &format!("{path}?per_page=100&page={page}"))
            .await?;
        if key.is_some() {
            let count = v
                .get("total_count")
                .and_then(Value::as_u64)
                .ok_or("GitHub omitted the page count; state is unknown")?;
            if expected.is_some_and(|n| n != count) {
                return Err("GitHub pages changed during observation; state is unknown".into());
            }
            expected = Some(count);
        }
        if key == Some("statuses") {
            let current = required(&v, "sha")?;
            if sha
                .as_ref()
                .is_some_and(|previous: &String| previous != current)
                || !path.ends_with(&format!("/commits/{current}/status"))
            {
                return Err("Commit statuses belong to a different head".into());
            }
            sha = Some(current.to_owned());
        }
        let page_rows = rows(&v, key)?;
        let len = page_rows.len();
        result.extend(page_rows.iter().cloned());
        if len < 100 {
            if expected.is_some_and(|n| n != result.len() as u64) {
                return Err("GitHub pagination is incomplete; state is unknown".into());
            }
            return Ok(result);
        }
    }
    Err("GitHub observation exceeds ten pages; state is unknown".into())
}
/// Read the entire bounded observation, then recheck head/state. Partial reads
/// never update a wake cursor and checks are always requested by the exact SHA.
pub async fn read_pr_watch(
    api: &dyn GithubReadApi,
    cwd: &Path,
    repository: &str,
    number: u64,
) -> Result<PrObservation, String> {
    let pieces: Vec<_> = repository.split('/').collect();
    if pieces.len() != 2
        || pieces.iter().any(|s| {
            s.is_empty()
                || s.starts_with('.')
                || !s
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        })
        || number == 0
    {
        return Err("Invalid GitHub repository or PR number".into());
    }
    let base = format!("repos/{repository}");
    let path = format!("{base}/pulls/{number}");
    let pr = api.get(cwd, &path).await?;
    if pr["number"].as_u64() != Some(number)
        || pr["base"]["repo"]["full_name"]
            .as_str()
            .is_none_or(|name| !name.eq_ignore_ascii_case(repository))
    {
        return Err("GitHub PR repository identity did not match".into());
    }
    let head = required(&pr["head"], "sha")?.to_owned();
    if !(40..=64).contains(&head.len()) || !head.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("GitHub returned an invalid head SHA".into());
    }
    let state = required(&pr, "state")?.to_owned();
    if state != "open" && state != "closed" {
        return Err("GitHub PR state is unknown".into());
    }
    let user = api.get(cwd, "user").await?;
    let viewer = required(&user, "login")?;
    let mut checks = BTreeMap::new();
    for c in pages(
        api,
        cwd,
        &format!("{base}/commits/{head}/check-runs"),
        Some("check_runs"),
    )
    .await?
    {
        if required(&c, "head_sha")? != head {
            return Err("Check results belong to a different head; state is unknown".into());
        }
        let name = required(&c, "name")?;
        let status = required(&c, "status")?;
        if !matches!(
            status,
            "queued" | "in_progress" | "completed" | "waiting" | "pending" | "requested"
        ) {
            return Err("Unknown check status".into());
        }
        let conclusion = if status == "completed" {
            required(&c, "conclusion")?
        } else {
            "pending"
        };
        let key = format!(
            "check:{}:{name}:{}",
            c["app"]["id"]
                .as_u64()
                .ok_or("Check app identity missing")?,
            c["id"].as_u64().ok_or("Check identity missing")?
        );
        if checks
            .insert(key, format!("{status}:{conclusion}"))
            .is_some()
        {
            return Err("Duplicate check across GitHub pages; state is unknown".into());
        }
    }
    // Combined statuses are a distinct API, also scoped to the observed SHA.
    for s in pages(
        api,
        cwd,
        &format!("{base}/commits/{head}/status"),
        Some("statuses"),
    )
    .await?
    {
        let state = required(&s, "state")?;
        if !matches!(state, "success" | "pending" | "failure" | "error") {
            return Err("Unknown commit status".into());
        }
        if checks
            .insert(format!("status:{}", required(&s, "context")?), state.into())
            .is_some()
        {
            return Err("Duplicate status across GitHub pages; state is unknown".into());
        }
    }
    let mut remarks = BTreeMap::new();
    for (kind, endpoint) in [
        ("comment", format!("{base}/issues/{number}/comments")),
        ("review-comment", format!("{base}/pulls/{number}/comments")),
        ("review", format!("{base}/pulls/{number}/reviews")),
    ] {
        for r in pages(api, cwd, &endpoint, None).await? {
            let author = required(&r["user"], "login")?;
            let id = r["id"].as_u64().ok_or("GitHub remark identity missing")?;
            if author.eq_ignore_ascii_case(viewer) {
                continue;
            }
            let body = r["body"]
                .as_str()
                .unwrap_or("")
                .chars()
                .take(1600)
                .collect::<String>();
            let text = format!("{author}: {} {body}", r["state"].as_str().unwrap_or(""));
            if remarks.insert(format!("{kind}:{id}"), text).is_some() {
                return Err("Duplicate remark across GitHub pages; state is unknown".into());
            }
        }
    }
    let confirm = api.get(cwd, &path).await?;
    if confirm["head"]["sha"].as_str() != Some(&head) || confirm["state"].as_str() != Some(&state) {
        return Err("PR head or state changed during observation; retry at the next poll".into());
    }
    Ok(PrObservation {
        head_oid: head,
        state,
        checks,
        remarks,
    })
}
