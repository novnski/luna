//! Read-only OpenCode Go limits through its published usage endpoint.
//! Credentials are used only in memory; historical OpenCode usage is separate.

use std::path::PathBuf;
use std::time::Duration;

use chrono::DateTime;
use reqwest::redirect::Policy;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::Value;
use sha2::{Digest, Sha256};
use zeron_proto::{LimitAvailability, UsageLimitSnapshot, UsageLimitWindow, UsageProvider};

struct Credential {
    token: String,
    organization: Option<String>,
    verified_account_id: Option<String>,
    endpoint: &'static str,
}

impl Credential {
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

fn data_directory() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("XDG_DATA_HOME") {
        let path = PathBuf::from(path);
        if path.is_absolute() {
            return Some(path.join("opencode"));
        }
    }
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share/opencode"))
}

fn active_v2_credential(directory: &std::path::Path) -> Result<Option<Credential>, ()> {
    type ActiveAccountRow = (String, String, String, Option<i64>, Option<String>);
    let path = directory.join("opencode.db");
    if !path.is_file() {
        return Ok(None);
    }
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|_| ())?;
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
    let valid_issuer = reqwest::Url::parse(&url).is_ok_and(|url| {
        url.origin().ascii_serialization() == "https://opencode.ai"
            && matches!(url.path(), "/" | "/console" | "/console/")
    });
    if !valid_issuer || id.is_empty() || organization.is_empty() || token.is_empty() {
        return Ok(None);
    }
    if expiry.is_some_and(|expiry| expiry <= now_ms()) {
        return Err(());
    }
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

fn api_credential(directory: &std::path::Path) -> Result<Option<Credential>, ()> {
    if let Ok(token) = std::env::var("OPENCODE_API_KEY")
        && !token.trim().is_empty()
    {
        return Ok(Some(Credential {
            token: token.trim().into(),
            organization: None,
            verified_account_id: None,
            endpoint: "https://opencode.ai/zen/go/v1/usage",
        }));
    }
    let content = std::env::var("OPENCODE_AUTH_CONTENT")
        .ok()
        .filter(|content| serde_json::from_str::<Value>(content).is_ok())
        .or_else(|| std::fs::read_to_string(directory.join("auth.json")).ok());
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
                endpoint: "https://opencode.ai/zen/go/v1/usage",
            }));
        }
    }
    Ok(None)
}

fn credential() -> Result<Option<Credential>, ()> {
    let Some(directory) = data_directory() else {
        return Ok(None);
    };
    // Explicit API key wins. The active v2 account otherwise supersedes the
    // legacy auth file; neither store is changed or refreshed here.
    if std::env::var("OPENCODE_API_KEY").is_ok_and(|key| !key.trim().is_empty()) {
        return api_credential(&directory);
    }
    if let Some(account) = active_v2_credential(&directory)? {
        Ok(Some(account))
    } else {
        api_credential(&directory)
    }
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
    let selected = tokio::task::spawn_blocking(credential).await;
    let Some(selected) = selected.ok().and_then(Result::ok) else {
        return unavailable(
            LimitAvailability::ProbeFailed,
            "OpenCode Go credential read failed",
            None,
        );
    };
    let Some(selected) = selected else {
        return unavailable(
            LimitAvailability::Unsupported,
            "No selected OpenCode Go credential",
            None,
        );
    };
    let id = selected.verified_account_id.clone();
    let signature = selected.signature();
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
        .is_none_or(|current| current.signature() != signature)
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
