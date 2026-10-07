//! The OpenCode 2 compatibility contract (OC-02 step 3).
//!
//! OpenAPI `info.version` is always `0.0.1` and 2.0.22 has no capabilities
//! document, so the contract is checked structurally: at boot LUNA fetches
//! the authenticated live `/openapi.json` (cached per binary version) and
//! verifies that every operation LUNA calls exists and that its request
//! schema has the fields LUNA sends, with no required field LUNA doesn't
//! send. A missing piece fails the boot with a specific message, before the
//! first prompt. A version outside the verified range proceeds with a
//! one-time warning when the checks pass.
//!
//! The SSE vocabulary is not in the OpenAPI document (events are typed as a
//! JSON string), so the 2.0.22 event-type set is pinned here; first-seen
//! unknown types are logged once (bounded) instead of vanishing silently.

use std::collections::{BTreeSet, HashSet};
use std::sync::{Mutex, OnceLock};

use sha2::{Digest, Sha256};

use super::*;

/// Versions whose contract and event vocabulary were verified live, with
/// the contract fingerprint observed on that version's `/openapi.json`.
const VERIFIED: &[(&str, &str)] = &[("2.0.22", PINNED_2_0_22)];

/// [`check_contract`]'s fingerprint of the live 2.0.22 document.
const PINNED_2_0_22: &str = "7397b270504b42af9a5a2c69ce023a3d5322466b7650612785eb14aee7519759";

/// One operation LUNA depends on: operation id, method and path, the
/// request-body fields LUNA sends (`None` = no body), and the query
/// parameters it uses.
struct Requirement {
    operation: &'static str,
    method: &'static str,
    path: &'static str,
    body: Option<&'static [&'static str]>,
    query: &'static [&'static str],
}

const fn op(
    operation: &'static str,
    method: &'static str,
    path: &'static str,
    body: Option<&'static [&'static str]>,
    query: &'static [&'static str],
) -> Requirement {
    Requirement {
        operation,
        method,
        path,
        body,
        query,
    }
}

/// The request contract LUNA depends on (tool-and-event-matrix §2/§7).
const REQUIREMENTS: &[Requirement] = &[
    op("server.info", "get", "/api/info", None, &[]),
    op("event.subscribe", "get", "/api/event", None, &[]),
    op(
        "session.create",
        "post",
        "/api/session",
        Some(&["location", "permissions"]),
        &[],
    ),
    op(
        "session.update",
        "patch",
        "/api/session/{sessionID}",
        Some(&["permissions"]),
        &[],
    ),
    op("session.get", "get", "/api/session/{sessionID}", None, &[]),
    op(
        "session.prompt",
        "post",
        "/api/session/{sessionID}/prompt",
        Some(&["text", "files", "id", "delivery"]),
        &[],
    ),
    op(
        "session.permission.reply",
        "post",
        "/api/session/{sessionID}/permission/{requestID}/reply",
        Some(&["decision", "message"]),
        &[],
    ),
    op(
        "session.form.reply",
        "post",
        "/api/session/{sessionID}/form/{formID}/reply",
        Some(&["answer"]),
        &[],
    ),
    op(
        "session.form.cancel",
        "delete",
        "/api/session/{sessionID}/form/{formID}",
        None,
        &["message"],
    ),
    // Reconnect resync of pending asks and forms (OC-04/OC-05, OC-08).
    op(
        "permission.request.list",
        "get",
        "/api/permission/request",
        None,
        &[],
    ),
    op("form.list", "get", "/api/form", None, &[]),
    op(
        "session.command",
        "post",
        "/api/session/{sessionID}/command",
        Some(&["name", "text", "files"]),
        &[],
    ),
    op(
        "session.compact",
        "post",
        "/api/session/{sessionID}/compact",
        Some(&[]),
        &[],
    ),
    op(
        "session.fork",
        "post",
        "/api/session/{sessionID}/fork",
        Some(&["before"]),
        &[],
    ),
    op(
        "session.interrupt",
        "post",
        "/api/session/{sessionID}/interrupt",
        None,
        &[],
    ),
    op(
        "session.switchModel",
        "post",
        "/api/session/{sessionID}/model",
        Some(&["model"]),
        &[],
    ),
    op("session.active", "get", "/api/session/active", None, &[]),
    op(
        "session.message.list",
        "get",
        "/api/session/{sessionID}/message",
        None,
        &["cursor", "order", "limit"],
    ),
    // OC-12 history: orphan-fork cleanup and same-session rewind.
    op(
        "session.remove",
        "delete",
        "/api/session/{sessionID}",
        None,
        &[],
    ),
    op(
        "session.revert.stage",
        "post",
        "/api/session/{sessionID}/revert/stage",
        Some(&["messageID", "files"]),
        &[],
    ),
    op(
        "session.revert.commit",
        "post",
        "/api/session/{sessionID}/revert/commit",
        None,
        &[],
    ),
    op(
        "session.revert.clear",
        "delete",
        "/api/session/{sessionID}/revert",
        None,
        &[],
    ),
    op("model.list", "get", "/api/model", None, &[]),
    op("command.list", "get", "/api/command", None, &[]),
    // OC-07 delivery readback and Stop's withdrawal of undelivered steers.
    op(
        "session.inbox.list",
        "get",
        "/api/session/{sessionID}/inbox",
        None,
        &[],
    ),
    op(
        "session.inbox.cancel",
        "delete",
        "/api/session/{sessionID}/inbox/{inboxID}",
        None,
        &[],
    ),
    op(
        "session.message.get",
        "get",
        "/api/session/{sessionID}/message/{messageID}",
        None,
        &[],
    ),
    // OC-08 reconnect resync of pending asks reuses the OC-04/OC-05
    // permission.request.list and form.list entries above. (The
    // experimental durable log is optional and probed at runtime.)
    // OC-06: live and full shell output by cursor; MCP server names for
    // typing `<server>_<tool>` calls.
    op(
        "shell.output",
        "get",
        "/api/shell/{id}/output",
        None,
        &["cursor", "limit"],
    ),
    op("mcp.list", "get", "/api/mcp", None, &[]),
];

/// The v2 permission rule shape LUNA sends in `permissions`.
const RULE_FIELDS: &[&str] = &["action", "resource", "effect"];

/// The verified 2.0.22 SSE vocabulary (94 types: the event manifest plus
/// `rpc.*` and `server.connected`).
const KNOWN_EVENT_TYPES: &[&str] = &[
    "location.shutdown",
    "models-dev.refreshed",
    "credential.updated",
    "credential.switched",
    "integration.updated",
    "provider.updated",
    "model.updated",
    "agent.updated",
    "session.created",
    "session.agent.selected",
    "session.model.selected",
    "session.moved",
    "session.renamed",
    "session.metadata.updated",
    "session.permissions",
    "session.viewed",
    "session.usage.updated",
    "session.deleted",
    "session.forked",
    "session.inbox.delivered",
    "session.inbox.enqueued",
    "session.inbox.cancelled",
    "session.inbox.delivery.changed",
    "session.execution.started",
    "session.execution.succeeded",
    "session.execution.failed",
    "session.execution.interrupted",
    "session.instructions.updated",
    "session.synthetic",
    "session.skill.activated",
    "session.shell.started",
    "session.shell.ended",
    "session.step.started",
    "session.step.streamed",
    "session.step.ended",
    "session.step.failed",
    "session.text.started",
    "session.text.delta",
    "session.text.ended",
    "session.reasoning.started",
    "session.reasoning.delta",
    "session.reasoning.ended",
    "session.tool.input.started",
    "session.tool.input.delta",
    "session.tool.input.ended",
    "session.tool.called",
    "session.tool.progress",
    "session.tool.success",
    "session.tool.failed",
    "session.retry.scheduled",
    "session.compaction.started",
    "session.compaction.delta",
    "session.compaction.ended",
    "session.compaction.failed",
    "session.revert.staged",
    "session.revert.cleared",
    "session.revert.committed",
    "filesystem.changed",
    "reference.updated",
    "permission.asked",
    "permission.replied",
    "plugin.updated",
    "project.updated",
    "worktree.updated",
    "worktree.resolved",
    "command.updated",
    "config.updated",
    "skill.updated",
    "pty.created",
    "pty.updated",
    "pty.exited",
    "pty.deleted",
    "persistent-pty.added",
    "persistent-pty.removed",
    "shell.created",
    "shell.exited",
    "shell.deleted",
    "form.created",
    "form.replied",
    "form.cancelled",
    "websearch.updated",
    "session.status",
    "session.idle",
    "tui.prompt.append",
    "tui.command.execute",
    "tui.toast.show",
    "tui.session.select",
    "installation.updated",
    "installation.update-available",
    "vcs.branch.updated",
    "mcp.status.changed",
    "mcp.resources.changed",
    "server.connected",
];

/// Distinct unknown event types reported per process (bounded diagnostics).
const MAX_UNKNOWN_EVENT_TYPES: usize = 64;

/// How a server version relates to what LUNA verified.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompatStatus {
    /// Contract checks passed on a verified version.
    KnownGood,
    /// Contract checks passed on a version LUNA has not verified (or on a
    /// verified version whose contract fingerprint changed compatibly).
    Untested,
}

/// The outcome of a passing contract check, kept for diagnostics and
/// evidence manifests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatReport {
    pub version: String,
    pub status: CompatStatus,
    /// SHA-256 over the canonical JSON of the checked operations.
    pub fingerprint: String,
}

fn reports() -> &'static Mutex<HashMap<String, Result<CompatReport, String>>> {
    static REPORTS: OnceLock<Mutex<HashMap<String, Result<CompatReport, String>>>> =
        OnceLock::new();
    REPORTS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The cached contract outcome for a server version, if any was checked.
pub fn opencode_compat_report(version: &str) -> Option<Result<CompatReport, String>> {
    reports().lock().ok()?.get(version).cloned()
}

/// Verify the spawned server's live contract (cached per version). Errors
/// name every missing operation or field.
pub(super) async fn ensure_compatible(server: &Server) -> Result<CompatReport, HarnessError> {
    let version = server.version.clone().unwrap_or_default();
    if let Some(cached) = opencode_compat_report(&version) {
        return cached.map_err(HarnessError::Protocol);
    }
    let spec = server
        .get_json("/openapi.json", None)
        .await
        .map_err(|error| {
            HarnessError::Protocol(format!(
                "OpenCode {version}: could not read the API contract (/openapi.json): {error}"
            ))
        })?;
    let outcome = check_contract(&version, &spec);
    if outcome.is_ok() {
        // LUNA tool delivery rides experimental routes outside the boot
        // contract: their absence degrades delivery, never the boot.
        record_delivery_contract(&version, &spec);
    }
    match &outcome {
        Ok(report) if report.status == CompatStatus::Untested => tracing::warn!(
            target: "luna_harness::opencode",
            version = %report.version,
            fingerprint = %report.fingerprint,
            "OpenCode version not verified by LUNA; its API contract checks passed, \
             continuing in degraded trust"
        ),
        Ok(report) => tracing::info!(
            target: "luna_harness::opencode",
            version = %report.version,
            fingerprint = %report.fingerprint,
            "OpenCode API contract verified"
        ),
        Err(message) => tracing::warn!(target: "luna_harness::opencode", "{message}"),
    }
    if let Ok(mut reports) = reports().lock() {
        reports.insert(version, outcome.clone());
    }
    outcome.map_err(HarnessError::Protocol)
}

/// The structural check over one OpenAPI document.
pub(super) fn check_contract(version: &str, spec: &Value) -> Result<CompatReport, String> {
    let mut missing: Vec<String> = Vec::new();
    let mut fingerprint_input: Vec<Value> = Vec::new();
    let operations = index_operations(spec);
    for requirement in REQUIREMENTS {
        let Some((method, path, operation)) = operations.get(requirement.operation) else {
            missing.push(format!("operation {}", requirement.operation));
            continue;
        };
        if *method != requirement.method || *path != requirement.path {
            missing.push(format!(
                "{} at {} {} (now {} {path})",
                requirement.operation,
                requirement.method.to_uppercase(),
                requirement.path,
                method.to_uppercase(),
            ));
        }
        for name in requirement.query {
            let present = operation
                .get("parameters")
                .and_then(Value::as_array)
                .is_some_and(|params| {
                    params.iter().any(|param| {
                        let param = resolve(spec, param);
                        param.get("in").and_then(Value::as_str) == Some("query")
                            && param.get("name").and_then(Value::as_str) == Some(name)
                    })
                });
            if !present {
                missing.push(format!(
                    "{} query parameter `{name}`",
                    requirement.operation
                ));
            }
        }
        let body = operation
            .pointer("/requestBody/content/application~1json/schema")
            .map(|schema| object_schema(spec, schema));
        if let Some(sent) = requirement.body {
            match &body {
                None => missing.push(format!("{} request body", requirement.operation)),
                Some(schema) => {
                    if schema.get("type").and_then(Value::as_str) != Some("object") {
                        missing.push(format!("{} object request body", requirement.operation));
                    }
                    let properties = schema.get("properties").and_then(Value::as_object);
                    for field in sent {
                        if !properties.is_some_and(|props| props.contains_key(*field)) {
                            missing.push(format!("{} field `{field}`", requirement.operation));
                        }
                    }
                    for required in required_fields(schema) {
                        if !sent.contains(&required.as_str()) {
                            missing.push(format!(
                                "{} requires `{required}`, which LUNA does not send",
                                requirement.operation
                            ));
                        }
                    }
                }
            }
        } else if let Some(schema) = &body
            && !required_fields(schema).is_empty()
        {
            missing.push(format!(
                "{} now requires a request body",
                requirement.operation
            ));
        }
        fingerprint_input.push(json!({
            "operation": requirement.operation,
            "method": method,
            "path": path,
            "body": body.map(|schema| canonical_schema(spec, &schema, 0)),
        }));
    }
    check_rule_shape(spec, &operations, &mut missing);

    let fingerprint = {
        // Keys sorted explicitly: the app builds serde_json with
        // `preserve_order` (GPUI enables it) while harness-only builds sort,
        // and the pin must not depend on which binary computed it.
        let mut hasher = Sha256::new();
        hasher.update(
            serde_json::to_vec(&sorted_keys(&Value::Array(fingerprint_input))).unwrap_or_default(),
        );
        format!("{:x}", hasher.finalize())
    };
    if !missing.is_empty() {
        return Err(format!(
            "OpenCode {version} is not compatible with LUNA's OpenCode 2 contract: {}. \
             Update OpenCode (`opencode upgrade`) or LUNA.",
            missing.join("; ")
        ));
    }
    let status = if VERIFIED
        .iter()
        .any(|(verified, pinned)| *verified == version && *pinned == fingerprint)
    {
        CompatStatus::KnownGood
    } else {
        CompatStatus::Untested
    };
    Ok(CompatReport {
        version: version.to_owned(),
        status,
        fingerprint,
    })
}

/// `permissions` must be an array of `{action, resource, effect}` rules.
fn check_rule_shape(
    spec: &Value,
    operations: &HashMap<String, (String, String, Value)>,
    missing: &mut Vec<String>,
) {
    let Some((_, _, create)) = operations.get("session.create") else {
        return;
    };
    let Some(schema) = create.pointer("/requestBody/content/application~1json/schema") else {
        return;
    };
    let schema = object_schema(spec, schema);
    let Some(permissions) = schema.pointer("/properties/permissions") else {
        return;
    };
    let array = object_schema(spec, permissions);
    let item = array
        .get("items")
        .map(|item| object_schema(spec, item))
        .unwrap_or(Value::Null);
    let properties = item.get("properties").and_then(Value::as_object);
    for field in RULE_FIELDS {
        if !properties.is_some_and(|props| props.contains_key(*field)) {
            missing.push(format!("session.create permission rule field `{field}`"));
        }
    }
}

/// `operationId` → (method, path, operation).
fn index_operations(spec: &Value) -> HashMap<String, (String, String, Value)> {
    let mut out = HashMap::new();
    let Some(paths) = spec.get("paths").and_then(Value::as_object) else {
        return out;
    };
    for (path, methods) in paths {
        let Some(methods) = methods.as_object() else {
            continue;
        };
        for (method, operation) in methods {
            if let Some(id) = operation.get("operationId").and_then(Value::as_str) {
                out.insert(
                    id.to_owned(),
                    (method.to_lowercase(), path.clone(), operation.clone()),
                );
            }
        }
    }
    out
}

/// Follow `$ref`s (bounded) to the referenced schema.
fn resolve<'a>(spec: &'a Value, mut schema: &'a Value) -> &'a Value {
    for _ in 0..16 {
        let Some(reference) = schema.get("$ref").and_then(Value::as_str) else {
            break;
        };
        let Some(pointer) = reference.strip_prefix('#') else {
            break;
        };
        match spec.pointer(pointer) {
            Some(next) => schema = next,
            None => break,
        }
    }
    schema
}

/// The non-null branch of a nullable schema (`anyOf: [X, {type: null}]`),
/// with `$ref`s followed.
fn object_schema(spec: &Value, schema: &Value) -> Value {
    let schema = resolve(spec, schema);
    for key in ["anyOf", "oneOf"] {
        if let Some(branches) = schema.get(key).and_then(Value::as_array) {
            let concrete: Vec<&Value> = branches
                .iter()
                .map(|branch| resolve(spec, branch))
                .filter(|branch| branch.get("type").and_then(Value::as_str) != Some("null"))
                .collect();
            if concrete.len() == 1 {
                return concrete[0].clone();
            }
        }
    }
    schema.clone()
}

fn required_fields(schema: &Value) -> BTreeSet<String> {
    schema
        .get("required")
        .and_then(Value::as_array)
        .map(|fields| {
            fields
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// A copy with every object's keys in sorted order, whatever map ordering
/// serde_json was built with.
fn sorted_keys(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<(String, Value)> = map
                .iter()
                .map(|(key, value)| (key.clone(), sorted_keys(value)))
                .collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            Value::Object(entries.into_iter().collect())
        }
        Value::Array(items) => Value::Array(items.iter().map(sorted_keys).collect()),
        other => other.clone(),
    }
}

/// A schema with `$ref`s inlined (bounded depth) and documentation keys
/// dropped, for a stable fingerprint.
fn canonical_schema(spec: &Value, schema: &Value, depth: usize) -> Value {
    if depth > 8 {
        return json!("…");
    }
    let schema = resolve(spec, schema);
    match schema {
        Value::Object(map) => Value::Object(
            map.iter()
                .filter(|(key, _)| {
                    !matches!(
                        key.as_str(),
                        "description" | "title" | "examples" | "example"
                    )
                })
                .map(|(key, value)| (key.clone(), canonical_schema(spec, value, depth + 1)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| canonical_schema(spec, item, depth + 1))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Is `kind` part of the verified SSE vocabulary?
pub(super) fn is_known_event_type(kind: &str) -> bool {
    kind.starts_with("rpc.") || KNOWN_EVENT_TYPES.contains(&kind)
}

/// Log the first sighting of an event type outside the verified
/// vocabulary — type name only, never the payload. Bounded per process.
pub(super) fn note_event_type(kind: &str) {
    if kind.is_empty() || is_known_event_type(kind) {
        return;
    }
    static SEEN: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let Ok(mut seen) = SEEN.get_or_init(|| Mutex::new(HashSet::new())).lock() else {
        return;
    };
    if seen.len() >= MAX_UNKNOWN_EVENT_TYPES || !seen.insert(kind.to_owned()) {
        return;
    }
    tracing::warn!(
        target: "luna_harness::opencode",
        event_type = %kind,
        "OpenCode sent an event type outside the verified 2.0.22 vocabulary; it is ignored"
    );
}
