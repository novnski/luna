//! Read-only OpenCode Go limits through its published usage endpoint.
//! Credentials are used only in memory; historical OpenCode usage is separate.
//! The account database, `auth.json` and credential variables are the ones
//! the OpenCode server LUNA would spawn sees
//! (`luna_harness::opencode::OpencodeStorage`): a QA app whose executable is
//! a profile wrapper reads the profile's store, never the user's.
//!
//! Failure cases this reader covers:
//! - OCG-01 An OpenCode 2 sign-in reads as "No selected OpenCode Go
//!   credential". OpenCode 2 saves `auth login` keys in the database's
//!   `credential` table (integration `opencode-go`, value `{type: "key",
//!   key}`), not in `auth.json`. The integration's newest row is the server's
//!   active connection and precedes `OPENCODE_API_KEY`
//!   (`Integration.connection.active`), so it is read first.
//! - OCG-02 The read writes into OpenCode's directory. The database opens
//!   through the history reader's opener: read-only, immutable while
//!   OpenCode has it closed, with a short busy timeout.
//! - OCG-03 A saved Go credential LUNA cannot use (not a key, malformed)
//!   silently falls back to another key the server would not use. It is
//!   reported as a failed credential read instead.

use std::path::PathBuf;
use std::time::Duration;

use chrono::DateTime;
use luna_harness::opencode::OpencodeStorage;
use luna_proto::{LimitAvailability, UsageLimitSnapshot, UsageLimitWindow, UsageProvider};
use reqwest::redirect::Policy;
use rusqlite::{Connection, OptionalExtension};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// The Go usage route for an API key (saved, `OPENCODE_API_KEY` or legacy).
const GO_KEY_ENDPOINT: &str = "https://opencode.ai/zen/go/v1/usage";

struct Credential {
    token: String,
    organization: Option<String>,
    verified_account_id: Option<String>,
    endpoint: &'static str,
}

impl Credential {
    fn scope_signature(&self) -> String {
        self.verified_account_id
            .clone()
            .unwrap_or_else(|| self.signature())
    }

    fn signature(&self) -> String {
        format!(
            "{:x}",
            Sha256::digest(
                format!(
                    "{}:{}:{}",
                    self.endpoint,
                    self.organization.as_deref().unwrap_or(""),
                    self.token
                )
                .as_bytes()
            )
        )
    }
}

/// Where the selected credential lives for the server LUNA would spawn.
struct Store {
    database: Option<PathBuf>,
    auth_file: Option<PathBuf>,
    api_key: Option<String>,
    auth_content: Option<String>,
}

const UNDETERMINED: &str = "OpenCode data directory could not be determined";

fn store() -> Result<Store, &'static str> {
    let storage = OpencodeStorage::current();
    let text = |name: &str| -> Result<Option<String>, &'static str> {
        Ok(storage
            .var(name)
            .map_err(|_| UNDETERMINED)?
            .and_then(|value| value.into_string().ok()))
    };
    let api_key = text("OPENCODE_API_KEY")?.filter(|key| !key.trim().is_empty());
    let auth_content = text("OPENCODE_AUTH_CONTENT")?;
    // An explicit OPENCODE_DATA_DIR names the data root for this reader as
    // it does for history; otherwise the server's own data root and database.
    let (database, data) = if text(crate::usage::OPENCODE_DATA_DIR_VARIABLE)?.is_some() {
        let root = crate::usage::opencode_storage_roots(&storage)
            .map_err(|_| UNDETERMINED)?
            .into_iter()
            .next();
        (root.as_ref().map(|root| root.join("opencode.db")), root)
    } else {
        (
            storage.database().map_err(|_| UNDETERMINED)?,
            storage.data_dir().map_err(|_| UNDETERMINED)?,
        )
    };
    tracing::debug!(
        target: "luna_engine::usage",
        database = ?database,
        data = ?data,
        "OpenCode Go credential store"
    );
    Ok(Store {
        database,
        auth_file: data.map(|data| data.join("auth.json")),
        api_key,
        auth_content,
    })
}

/// Match the selected active credential in current and legacy OpenCode stores.
fn saved_go_key(db: &Connection) -> Result<Option<Credential>, ()> {
    let mut statement = db
        .prepare("SELECT name FROM pragma_table_info('credential')")
        .map_err(|_| ())?;
    let columns: Vec<String> = statement
        .query_map([], |row| row.get(0))
        .map_err(|_| ())?
        .collect::<rusqlite::Result<_>>()
        .map_err(|_| ())?;
    if !["integration_id", "value", "time_created"]
        .iter()
        .all(|c| columns.iter().any(|v| v == c))
    {
        return Ok(None);
    }
    let active = if columns.iter().any(|v| v == "active") {
        " AND active = 1"
    } else {
        ""
    };
    let tie = if columns.iter().any(|v| v == "id") {
        "id"
    } else {
        "rowid"
    };
    let query = format!(
        "SELECT value FROM credential WHERE integration_id = ?1{active} ORDER BY time_created DESC, {tie} DESC LIMIT 1"
    );
    for provider in ["opencode-go", "opencode"] {
        let value: Option<String> = db
            .query_row(&query, [provider], |row| row.get(0))
            .optional()
            .map_err(|_| ())?;
        let Some(value) = value else { continue };
        let value: Value = serde_json::from_str(&value).map_err(|_| ())?;
        match value["type"].as_str() {
            Some("key" | "api") => {
                let token = value["key"]
                    .as_str()
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .ok_or(())?;
                return Ok(Some(Credential {
                    token: token.into(),
                    organization: None,
                    verified_account_id: None,
                    endpoint: GO_KEY_ENDPOINT,
                }));
            }
            Some("oauth") if value["methodID"] == "device" => {
                let metadata = &value["metadata"];
                let issuer = metadata["server"]
                    .as_str()
                    .unwrap_or("https://opencode.ai/console");
                if !official_console(issuer) {
                    continue;
                }
                let token = value["access"]
                    .as_str()
                    .filter(|v| !v.is_empty())
                    .ok_or(())?;
                let account = metadata["accountID"]
                    .as_str()
                    .filter(|v| !v.is_empty())
                    .ok_or(())?;
                let org = metadata["orgID"]
                    .as_str()
                    .filter(|v| !v.is_empty())
                    .ok_or(())?;
                return Ok(Some(Credential {
                    token: token.into(),
                    organization: Some(org.into()),
                    verified_account_id: Some(format!(
                        "opencode-go:{:x}",
                        Sha256::digest(format!("{org}:{account}").as_bytes())
                    )),
                    endpoint: "https://opencode.ai/inference/go/v1/usage",
                }));
            }
            _ => continue,
        }
    }
    Ok(None)
}

fn official_console(issuer: &str) -> bool {
    reqwest::Url::parse(issuer).is_ok_and(|url| {
        url.origin().ascii_serialization() == "https://opencode.ai"
            && matches!(url.path(), "/" | "/console" | "/console/")
    })
}

/// The active OpenCode console account (`account` and `account_state`).
fn active_account_credential(db: &Connection) -> Result<Option<Credential>, ()> {
    type ActiveAccountRow = (String, String, String, Option<i64>, Option<String>);
    let tables: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN ('account', 'account_state')",
            [],
            |row| row.get(0),
        )
        .map_err(|_| ())?;
    if tables != 2 {
        return Ok(None);
    }
    let row: Option<ActiveAccountRow> = db
        .query_row(
            "SELECT a.id,a.url,a.access_token,a.token_expiry,s.active_org_id \
             FROM account a JOIN account_state s ON s.active_account_id=a.id WHERE s.id=1",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()
        .map_err(|_| ())?;
    let Some((id, url, token, expiry, Some(organization))) = row else {
        return Ok(None);
    };
    let valid_issuer = official_console(&url);
    if !valid_issuer || id.is_empty() || organization.is_empty() || token.is_empty() {
        return Ok(None);
    }
    let _ = expiry;
    let verified_account_id = format!(
        "opencode-go:{:x}",
        Sha256::digest(format!("{organization}:{id}").as_bytes())
    );
    Ok(Some(Credential {
        token,
        organization: Some(organization),
        verified_account_id: Some(verified_account_id),
        endpoint: "https://opencode.ai/inference/go/v1/usage",
    }))
}

fn api_credential(store: &Store) -> Result<Option<Credential>, ()> {
    if let Some(token) = store.api_key.as_deref() {
        return Ok(Some(Credential {
            token: token.trim().into(),
            organization: None,
            verified_account_id: None,
            endpoint: GO_KEY_ENDPOINT,
        }));
    }
    let content = store
        .auth_content
        .clone()
        .filter(|content| serde_json::from_str::<Value>(content).is_ok())
        .or_else(|| {
            store
                .auth_file
                .as_ref()
                .and_then(|path| std::fs::read_to_string(path).ok())
        });
    let Some(content) = content else {
        return Ok(None);
    };
    let auth: Value = serde_json::from_str(&content).map_err(|_| ())?;
    for provider in ["opencode-go", "opencode"] {
        let Some(value) = auth.get(provider) else {
            continue;
        };
        if value.get("type").and_then(Value::as_str) != Some("api") {
            continue;
        }
        if let Some(token) = value
            .get("key")
            .and_then(Value::as_str)
            .filter(|key| !key.trim().is_empty())
        {
            return Ok(Some(Credential {
                token: token.trim().into(),
                organization: None,
                verified_account_id: None,
                endpoint: GO_KEY_ENDPOINT,
            }));
        }
    }
    Ok(None)
}

fn credential() -> Result<Option<Credential>, &'static str> {
    const FAILED: &str = "OpenCode Go credential read failed";
    let store = store()?;
    let database = match store.database.as_deref() {
        Some(path) if path.is_file() => {
            Some(crate::usage::open_opencode_database(path).map_err(|_| FAILED)?)
        }
        _ => None,
    };
    // OpenCode 2's saved Go key is what the server uses (OCG-01).
    if let Some(db) = database.as_ref()
        && let Some(saved) = saved_go_key(db).map_err(|_| FAILED)?
    {
        return Ok(Some(saved));
    }
    // An explicit API key wins next. The active console account otherwise
    // supersedes the legacy auth file; no store is changed or refreshed here.
    if store.api_key.is_some() {
        return api_credential(&store).map_err(|_| FAILED);
    }
    let account = match database.as_ref() {
        Some(db) => active_account_credential(db).map_err(|_| FAILED)?,
        None => None,
    };
    match account {
        Some(account) => Ok(Some(account)),
        None => api_credential(&store).map_err(|_| FAILED),
    }
}

pub(crate) fn selection_signature() -> Result<String, &'static str> {
    let location = store()?;
    let selected = credential()?;
    let account = match selected {
        Some(credential) => credential.scope_signature(),
        None => "no-selected-account".into(),
    };
    // Two CLI profiles may use the same Go account but have different local
    // history. Fence the shared snapshot by both its owner and selected store.
    Ok(format!(
        "{:?}:{:?}:{account}",
        location.database, location.auth_file
    ))
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |time| time.as_millis() as i64)
}

fn unavailable(
    availability: LimitAvailability,
    detail: &'static str,
    id: Option<String>,
) -> UsageLimitSnapshot {
    UsageLimitSnapshot {
        provider: UsageProvider::OpenCodeGo,
        verified_account_id: id,
        account_label: None,
        plan: None,
        checked_at_ms: None,
        availability,
        windows: Vec::new(),
        reset_credits: None,
        detail: Some(detail.into()),
    }
}

fn parse_limit_response(value: &Value, id: Option<String>) -> Option<UsageLimitSnapshot> {
    let usage = value.get("usage")?;
    let mut windows = Vec::with_capacity(3);
    for (field, label, duration) in [
        ("rolling", "Five-hour", Some(300)),
        ("weekly", "Weekly", Some(10_080)),
        ("monthly", "Monthly", None),
    ] {
        let Some(window) = usage.get(field) else {
            continue;
        };
        if !matches!(window.get("status")?.as_str()?, "ok" | "rate-limited") {
            return None;
        }
        let percent = window.get("percent")?.as_f64()?;
        if !percent.is_finite() || percent < 0.0 {
            return None;
        }
        let reset = DateTime::parse_from_rfc3339(window.get("resetsAt")?.as_str()?).ok()?;
        windows.push(UsageLimitWindow {
            id: field.into(),
            label: label.into(),
            used_percent: percent.min(100.0) as f32,
            resets_at_ms: Some(reset.timestamp_millis()),
            duration_minutes: duration,
        });
    }
    if windows.is_empty() {
        return None;
    }
    Some(UsageLimitSnapshot {
        provider: UsageProvider::OpenCodeGo,
        verified_account_id: id,
        account_label: Some("Selected OpenCode Go account".into()),
        plan: Some("OpenCode Go".into()),
        checked_at_ms: Some(now_ms()),
        availability: LimitAvailability::Ready,
        windows,
        reset_credits: None,
        detail: Some("Read-only OpenCode Go usage endpoint · selected machine".into()),
    })
}

pub(crate) async fn read_go_limits() -> UsageLimitSnapshot {
    let selected = match tokio::task::spawn_blocking(credential).await {
        Ok(Ok(selected)) => selected,
        Ok(Err(detail)) => return unavailable(LimitAvailability::ProbeFailed, detail, None),
        Err(_) => {
            return unavailable(
                LimitAvailability::ProbeFailed,
                "OpenCode Go credential read failed",
                None,
            );
        }
    };
    let Some(selected) = selected else {
        return unavailable(
            LimitAvailability::Unsupported,
            "No selected OpenCode Go credential",
            None,
        );
    };
    let id = selected.verified_account_id.clone();
    let signature = selected.scope_signature();
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .redirect(Policy::none())
        .build()
    {
        Ok(client) => client,
        Err(_) => {
            return unavailable(
                LimitAvailability::ProbeFailed,
                "OpenCode Go HTTP client failed",
                id,
            );
        }
    };
    let mut request = client
        .get(selected.endpoint)
        .bearer_auth(&selected.token)
        .header(reqwest::header::ACCEPT, "application/json")
        .header(reqwest::header::USER_AGENT, "luna-usage-read/0.1");
    if let Some(organization) = selected.organization.as_ref() {
        request = request.header("x-opencode-org-id", organization);
    }
    let response = match request.send().await {
        Ok(response) => response,
        Err(error) if error.is_timeout() => {
            return unavailable(
                LimitAvailability::ProbeFailed,
                "OpenCode Go usage request timed out",
                id,
            );
        }
        Err(_) => {
            return unavailable(
                LimitAvailability::ProbeFailed,
                "OpenCode Go usage request failed",
                id,
            );
        }
    };
    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return unavailable(
            LimitAvailability::ProbeFailed,
            "OpenCode Go credential expired (401)",
            id,
        );
    }
    if status == reqwest::StatusCode::FORBIDDEN {
        return unavailable(
            LimitAvailability::Unsupported,
            "OpenCode Go subscription unavailable (403)",
            id,
        );
    }
    if !status.is_success() {
        return unavailable(
            LimitAvailability::ProbeFailed,
            "OpenCode Go usage endpoint returned an error",
            id,
        );
    }
    let bytes = match response.bytes().await {
        Ok(bytes) if bytes.len() <= 64 * 1024 => bytes,
        _ => {
            return unavailable(
                LimitAvailability::ProbeFailed,
                "OpenCode Go usage response exceeded the read bound",
                id,
            );
        }
    };
    let value: Value = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(_) => {
            return unavailable(
                LimitAvailability::ProbeFailed,
                "OpenCode Go usage response was invalid",
                id,
            );
        }
    };
    // A selected account or key switch while the network request was in
    // flight cannot publish data under the new selection.
    if tokio::task::spawn_blocking(credential)
        .await
        .ok()
        .and_then(Result::ok)
        .flatten()
        .is_none_or(|current| current.scope_signature() != signature)
    {
        return unavailable(
            LimitAvailability::Unverified,
            "OpenCode Go account changed during refresh",
            None,
        );
    }
    parse_limit_response(&value, id).unwrap_or_else(|| {
        unavailable(
            LimitAvailability::ProbeFailed,
            "OpenCode Go usage response was incomplete",
            None,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn maps_only_reported_go_windows_without_a_fixed_month_duration() {
        let value = json!({"usage": {
            "rolling": {"status":"ok","percent":15,"resetsAt":"2026-09-23T16:00:00Z"},
            "weekly": {"status":"rate-limited","percent":122,"resetsAt":"2026-09-25T00:00:00Z"},
            "monthly": {"status":"ok","percent":50,"resetsAt":"2026-10-01T00:00:00Z"}
        }});
        let limit = parse_limit_response(&value, None).unwrap();
        assert_eq!(limit.windows.len(), 3);
        assert_eq!(limit.windows[0].duration_minutes, Some(300));
        assert_eq!(limit.windows[1].used_percent, 100.0);
        let sparse = parse_limit_response(
            &json!({"usage": {"weekly": {"status":"ok","percent":25,"resetsAt":"2026-09-25T00:00:00Z"}}}),
            None,
        )
        .unwrap();
        assert_eq!(sparse.windows.len(), 1);
        assert_eq!(sparse.windows[0].id, "weekly");
        assert_eq!(limit.windows[2].duration_minutes, None);
        assert!(limit.verified_account_id.is_none());
        assert!(parse_limit_response(&json!({"usage":{"rolling":null}}), None).is_none());
    }

    #[tokio::test]
    #[ignore = "explicit opt-in read-only selected OpenCode Go account check"]
    async fn selected_go_usage_readback() {
        if std::env::var_os("LUNA_USAGE_REAL_READBACK").as_deref()
            != Some(std::ffi::OsStr::new("1"))
        {
            return;
        }
        let observed = read_go_limits().await;
        eprintln!(
            "OpenCode Go readback: {:?}, {} reported windows",
            observed.availability,
            observed.windows.len()
        );
        assert_eq!(observed.availability, LimitAvailability::Ready);
        assert_eq!(observed.windows.len(), 3);
    }
}
