//! Claude's structured review findings (CC-15) on LUNA's shared findings UI.
//!
//! Claude 2.1.286 has a `ReportFindings` tool: "Report code-review findings
//! as a typed list so the host UI can render them". `/code-review` reports
//! through it only when the host opts in with `CLAUDE_CODE_REPORT_FINDINGS`
//! (LUNA does, see `mod.rs`). Input (binary schema):
//! `{level?, findings: [{file, line?, summary, short_summary?,
//! failure_scenario, category?, verdict?, outcome?}] (≤32)}`.
//!
//! Each root call becomes one immutable `NativeReview` entry keyed by its
//! tool_use id, with provenance captured when the result lands: Claude's
//! session, the call id, the CLI version, the effort level, the cwd, the
//! host-observed git HEAD/dirty state and a hash of the exact payload. A
//! re-report (Claude sets `outcome` after applying fixes) is a new entry,
//! never an overwrite. Tagged (subagent) calls stay in the subagent feed.

use std::collections::HashMap;
use std::fmt::Write as _;

use luna_proto::AgentEvent;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::goals::{content_text, is_root};

pub(crate) const TOOL: &str = "ReportFindings";
const MAX_FINDINGS: usize = 32;
const MAX_FILE: usize = 300;
const MAX_SUMMARY: usize = 600;
const MAX_SCENARIO: usize = 900;
const MAX_SLUG: usize = 40;

fn bounded(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Markdown inline code without breaking out of the span.
fn code(text: &str) -> String {
    let text = text.replace('`', "'");
    format!("`{text}`")
}

#[derive(Debug, Clone)]
pub(crate) struct ReportCall {
    pub id: String,
    pub input: Value,
}

/// A completed call waiting for host provenance before it is rendered.
#[derive(Debug, Clone)]
pub(crate) struct ProvenanceRequest {
    pub call: ReportCall,
    pub is_error: bool,
    pub result_text: String,
    pub session_id: String,
    pub cli_version: Option<String>,
    pub cwd: String,
}

pub(crate) struct FindingsTracker {
    pending: HashMap<String, ReportCall>,
    session_id: String,
    cli_version: Option<String>,
}

impl FindingsTracker {
    pub(crate) fn new() -> Self {
        Self {
            pending: HashMap::new(),
            session_id: String::new(),
            cli_version: None,
        }
    }

    pub(crate) fn observe(
        &mut self,
        frame: &Value,
        cwd: &str,
    ) -> (Vec<AgentEvent>, Vec<ProvenanceRequest>) {
        if !is_root(frame) {
            return (Vec::new(), Vec::new());
        }
        match frame.get("type").and_then(Value::as_str) {
            Some("system") => {
                if frame.get("subtype").and_then(Value::as_str) == Some("init") {
                    if let Some(id) = frame.get("session_id").and_then(Value::as_str) {
                        self.session_id = id.to_owned();
                    }
                    if let Some(version) = frame.get("claude_code_version").and_then(Value::as_str)
                    {
                        self.cli_version = Some(bounded(version, 40));
                    }
                }
                (Vec::new(), Vec::new())
            }
            Some("assistant") => {
                let mut out = Vec::new();
                for block in blocks(frame) {
                    if block.get("type").and_then(Value::as_str) != Some("tool_use")
                        || block.get("name").and_then(Value::as_str) != Some(TOOL)
                    {
                        continue;
                    }
                    let Some(id) = block.get("id").and_then(Value::as_str) else {
                        continue;
                    };
                    if self.pending.len() >= 64 {
                        continue;
                    }
                    let input = block.get("input").cloned().unwrap_or(Value::Null);
                    let level = input
                        .get("level")
                        .and_then(Value::as_str)
                        .map(|level| format!(" · {} effort", bounded(level, 12)))
                        .unwrap_or_default();
                    out.push(AgentEvent::NativeReview {
                        id: id.to_owned(),
                        completed: false,
                        text: format!("Claude code review{level}"),
                    });
                    self.pending.insert(
                        id.to_owned(),
                        ReportCall {
                            id: id.to_owned(),
                            input,
                        },
                    );
                }
                (out, Vec::new())
            }
            Some("user") => {
                let mut requests = Vec::new();
                for block in blocks(frame) {
                    if block.get("type").and_then(Value::as_str) != Some("tool_result") {
                        continue;
                    }
                    let Some(call) = block
                        .get("tool_use_id")
                        .and_then(Value::as_str)
                        .and_then(|id| self.pending.remove(id))
                    else {
                        continue;
                    };
                    requests.push(ProvenanceRequest {
                        call,
                        is_error: block.get("is_error").and_then(Value::as_bool) == Some(true),
                        result_text: content_text(block.get("content").unwrap_or(&Value::Null)),
                        session_id: self.session_id.clone(),
                        cli_version: self.cli_version.clone(),
                        cwd: cwd.to_owned(),
                    });
                }
                (Vec::new(), requests)
            }
            _ => (Vec::new(), Vec::new()),
        }
    }
}

fn blocks(frame: &Value) -> impl Iterator<Item = &Value> {
    frame
        .get("message")
        .and_then(|message| message.get("content"))
        .and_then(Value::as_array)
        .map(|blocks| blocks.as_slice())
        .unwrap_or_default()
        .iter()
}

/// Host-observed source state at report time. Read-only git queries with
/// optional locks disabled; absent outside a repository.
#[derive(Debug, Clone, Default)]
pub(crate) struct SourceRevision {
    pub head: Option<String>,
    pub dirty: Option<bool>,
}

pub(crate) async fn observe_revision(cwd: &str) -> SourceRevision {
    if cwd.is_empty() {
        return SourceRevision::default();
    }
    async fn git(cwd: &str, args: &[&str]) -> Option<String> {
        let mut command = tokio::process::Command::new("git");
        command
            .arg("-C")
            .arg(cwd)
            .args(args)
            .env("GIT_OPTIONAL_LOCKS", "0")
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        let output = tokio::time::timeout(std::time::Duration::from_secs(3), command.output())
            .await
            .ok()?
            .ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
    }
    let head = git(cwd, &["rev-parse", "--verify", "HEAD"])
        .await
        .map(|sha| sha.trim().to_owned())
        .filter(|sha| sha.len() >= 7 && sha.bytes().all(|b| b.is_ascii_hexdigit()));
    let dirty = if head.is_some() {
        git(cwd, &["status", "--porcelain", "--untracked-files=no"])
            .await
            .map(|status| !status.trim().is_empty())
    } else {
        None
    };
    SourceRevision { head, dirty }
}

/// Render one completed call as the shared findings text.
pub(crate) fn render(request: &ProvenanceRequest, revision: &SourceRevision) -> AgentEvent {
    let input = &request.call.input;
    let payload = serde_json::to_vec(input).unwrap_or_default();
    let digest = Sha256::digest(&payload);
    let hash: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
    let mut text = String::new();
    if request.is_error {
        let detail = bounded(&request.result_text, MAX_SCENARIO);
        let _ = write!(
            text,
            "Claude's findings report failed, so no findings are claimed for this review.\n\n> {}",
            if detail.is_empty() {
                "No error detail."
            } else {
                &detail
            }
        );
    } else {
        let findings = input.get("findings").and_then(Value::as_array);
        match findings {
            None => text.push_str(
                "Claude reported findings in a shape LUNA could not read; no findings are claimed.",
            ),
            Some(findings) if findings.is_empty() => text.push_str(
                "Review completed with no findings: nothing survived Claude's verification.",
            ),
            Some(findings) => {
                let mut shown = 0;
                let mut skipped = 0;
                for finding in findings.iter().take(MAX_FINDINGS) {
                    let Some(file) = finding.get("file").and_then(Value::as_str) else {
                        skipped += 1;
                        continue;
                    };
                    let summary = finding
                        .get("summary")
                        .and_then(Value::as_str)
                        .or_else(|| finding.get("short_summary").and_then(Value::as_str))
                        .unwrap_or("");
                    if summary.trim().is_empty() {
                        skipped += 1;
                        continue;
                    }
                    shown += 1;
                    let location = match finding.get("line").and_then(Value::as_u64) {
                        Some(line) => format!("{}:{line}", bounded(file, MAX_FILE)),
                        None => bounded(file, MAX_FILE),
                    };
                    let _ = writeln!(
                        text,
                        "{shown}. **{}** {}",
                        code(&location),
                        bounded(summary, MAX_SUMMARY)
                    );
                    let tags: Vec<String> = ["category", "verdict", "outcome"]
                        .iter()
                        .filter_map(|key| finding.get(*key).and_then(Value::as_str))
                        .map(|tag| bounded(tag, MAX_SLUG))
                        .filter(|tag| !tag.is_empty())
                        .collect();
                    if !tags.is_empty() {
                        let _ = writeln!(text, "   _{}_", tags.join(" · "));
                    }
                    if let Some(scenario) = finding
                        .get("failure_scenario")
                        .and_then(Value::as_str)
                        .filter(|s| !s.trim().is_empty())
                    {
                        let _ = writeln!(text, "   Failure: {}", bounded(scenario, MAX_SCENARIO));
                    }
                }
                skipped += findings.len().saturating_sub(MAX_FINDINGS);
                if shown == 0 {
                    text.push_str(
                        "Claude reported findings LUNA could not read; no findings are claimed.\n",
                    );
                }
                if skipped > 0 {
                    let _ = writeln!(text, "\n{skipped} malformed or extra finding(s) not shown.");
                }
            }
        }
    }
    let mut provenance = vec![format!(
        "Claude Code{} `ReportFindings`",
        request
            .cli_version
            .as_deref()
            .map(|v| format!(" {v}"))
            .unwrap_or_default()
    )];
    if let Some(level) = input.get("level").and_then(Value::as_str) {
        provenance.push(format!("{} effort", bounded(level, 12)));
    }
    match (&revision.head, revision.dirty) {
        (Some(head), Some(true)) => provenance.push(format!(
            "source {} + uncommitted changes",
            code(&head[..12.min(head.len())])
        )),
        (Some(head), _) => provenance.push(format!("source {}", code(&head[..12.min(head.len())]))),
        (None, _) => provenance.push("source revision unknown".into()),
    }
    if !request.session_id.is_empty() {
        provenance.push(format!(
            "session {}",
            code(&bounded(&request.session_id, 64))
        ));
    }
    provenance.push(format!("call {}", code(&bounded(&request.call.id, 64))));
    provenance.push(format!("payload sha256 {}", code(&hash)));
    let _ = write!(text, "\n\n---\n_Provenance: {}_", provenance.join(" · "));
    AgentEvent::NativeReview {
        id: request.call.id.clone(),
        completed: true,
        text,
    }
}
