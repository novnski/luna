//! Shared fixtures for the CORE-11 / E11 suites (`opencode_package_*.rs`):
//! a real engine on a disposable profile, the real first-party package
//! assembled from the `luna-opencode-worker` binary Cargo built for this
//! test target, LUNA's own OpenCode 2 adapter (in that worker, or inside the
//! engine for the built-in baseline) and a scripted OpenCode 2 server
//! (`fake-opencode-package.py`) standing in for `opencode serve`.
//!
//! Readback the engine does not produce: the server's own JSON-line records
//! in each chat checkout (`.fake-opencode/`), process existence from the OS,
//! the installed package files re-hashed here, and the worker binary's bytes.
//! Set `LUNA_CORE11_EVIDENCE_DIR` to keep each test's readback as JSON.
#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Once};
use std::time::{Duration, Instant};

use luna_doc::{
    MessagePart, MessageRole, MessageStatus, SessionCommandPayload, SessionMessageEntry,
};
use luna_engine::{AgentAccountsConfig, EngineCore, EngineProfile, HarnessId, HarnessRegistry};
use luna_proto::{PermissionMode, RunRequest, SandboxLevel, TaskRecovery};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

pub const WAIT: Duration = Duration::from_secs(30);
pub const PACKAGE_ID: &str = "luna.provider/opencode";
pub const WORKER_ENV: &str = "LUNA_OPENCODE_WORKER";
pub const MODEL: &str = "fake/package-model";

pub fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The scripted `opencode`, made executable.
pub fn fake_opencode() -> PathBuf {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/fake-opencode-package.py");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o755));
    }
    path
}

/// The worker Cargo built for this test target.
pub fn worker_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_luna-opencode-worker"))
}

/// Process-wide settings, once, before any engine starts: the scripted
/// `opencode` (the in-process adapter and the worker, which inherits the
/// engine's environment, both resolve it), no login-shell probe, and the
/// bundled worker when `bundled`.
pub fn environment(bundled: bool) {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        // SAFETY: set before this test binary starts any engine or thread
        // that reads the environment.
        unsafe {
            std::env::set_var("OPENCODE_EXECUTABLE", fake_opencode());
            std::env::set_var("LUNA_NO_LOGIN_SHELL", "1");
            if bundled {
                std::env::set_var(WORKER_ENV, worker_binary());
            } else {
                std::env::set_var(WORKER_ENV, "off");
            }
        }
    });
}

/// Point the engine at the bundled worker from now on. Called only while no
/// engine of this test binary is open.
pub fn bundle_worker() {
    // SAFETY: the previous engine was shut down and dropped; nothing in this
    // single-test binary reads the environment concurrently.
    unsafe { std::env::set_var(WORKER_ENV, worker_binary()) };
}

/// A registry with the compiled OpenCode adapter, as `default_registry`
/// registers it; the first-party package must replace it.
pub fn builtin_registry() -> HarnessRegistry {
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(luna_harness::OpencodeHarness::new()));
    registry
}

pub struct Engine {
    pub core: EngineCore,
    pub client: luna_rpc::RpcClient,
    pub root: PathBuf,
}

impl Engine {
    pub fn open(root: &Path, registry: HarnessRegistry) -> Self {
        let accounts = AgentAccountsConfig {
            data_dir: root.join("synthetic-provider-state"),
            claude_config_dir: root.join("synthetic-claude"),
            claude_config_file: root.join("synthetic-claude.json"),
            codex_home: root.join("synthetic-codex"),
        };
        accounts
            .disable_claude_keychain_fallback()
            .expect("seal synthetic Claude home");
        let core =
            EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
                EngineProfile::development(root, "org-a", "user-a"),
                Arc::new(registry),
                HarnessId::Opencode,
                None,
                accounts,
            )
            .expect("assemble profile");
        let client = luna_rpc::memory_client(core.rpc_service());
        Self {
            core,
            client,
            root: root.to_path_buf(),
        }
    }

    pub async fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        self.client
            .call(method, params)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn close(self) {
        drop(self.client);
        self.core.shutdown().await;
        drop(self.core);
    }

    /// A chat on this device whose checkout is `root/<id>`, marked for the
    /// scripted server's records.
    pub fn task(&self, root: &Path, id: &str) -> PathBuf {
        let checkout = root.join(id);
        fs::create_dir_all(&checkout).unwrap();
        fs::write(checkout.join(".fake-opencode-checkout"), b"").unwrap();
        self.core
            .workspace
            .create_chat(
                id,
                None,
                Some(&self.core.device_id),
                None,
                Some(checkout.to_string_lossy().into_owned()),
            )
            .expect("create task");
        self.core
            .workspace
            .rename_chat(id, "OpenCode package task")
            .unwrap();
        checkout
    }

    /// A chat inside a project, as the composer creates one: the project
    /// (space) is the chat's checkout. Workspace-scoped reads such as
    /// `GetNativeExtensions` resolve the chat through its space.
    pub fn project_task(&self, root: &Path, id: &str) -> PathBuf {
        let checkout = root.join(id);
        fs::create_dir_all(&checkout).unwrap();
        fs::write(checkout.join(".fake-opencode-checkout"), b"").unwrap();
        let space = format!("project-{id}");
        self.core
            .workspace
            .create_space(
                &space,
                &self.core.device_id,
                checkout.to_str().unwrap(),
                None,
                false,
            )
            .expect("create project");
        self.core
            .workspace
            .create_chat(
                id,
                Some(space.as_str()),
                None,
                None,
                Some(checkout.to_string_lossy().into_owned()),
            )
            .expect("create project task");
        self.core
            .workspace
            .rename_chat(id, "OpenCode package project task")
            .unwrap();
        checkout
    }

    /// Send like the composer does: a `Run` command in the chat's queue.
    pub fn send(&self, chat: &str, message_id: &str, request: RunRequest) {
        self.core
            .doc_host
            .queue_command(
                chat,
                SessionCommandPayload::Run {
                    request,
                    message_id: message_id.into(),
                },
            )
            .expect("queue run");
    }

    /// Steer like the queue's Steer action does.
    pub fn steer(&self, chat: &str, message_id: &str, prompt: &str) {
        self.core
            .doc_host
            .queue_command(
                chat,
                SessionCommandPayload::Steer {
                    input_items: Vec::new(),
                    attachments: Vec::new(),
                    prompt: prompt.into(),
                    message_id: Some(message_id.into()),
                },
            )
            .expect("queue steer");
    }

    /// Press Stop like the composer does.
    pub fn stop(&self, chat: &str) {
        self.core
            .doc_host
            .queue_command(chat, SessionCommandPayload::Interrupt {})
            .expect("queue interrupt");
    }

    pub fn entries(&self, chat: &str) -> Vec<SessionMessageEntry> {
        self.core
            .doc_host
            .open(chat)
            .ok()
            .and_then(|handle| handle.doc().read_entries().ok())
            .unwrap_or_default()
    }

    pub fn assistant(&self, chat: &str) -> Vec<SessionMessageEntry> {
        self.entries(chat)
            .into_iter()
            .filter(|entry| entry.role == MessageRole::Assistant)
            .collect()
    }

    pub fn errors(&self, chat: &str) -> Vec<String> {
        self.entries(chat)
            .iter()
            .flat_map(|entry| entry.parts.iter())
            .filter_map(|part| match part {
                MessagePart::Error { message, .. } => Some(message.clone()),
                _ => None,
            })
            .collect()
    }

    pub fn texts(&self, chat: &str) -> String {
        self.assistant(chat)
            .iter()
            .flat_map(|entry| entry.parts.iter())
            .filter_map(|part| match part {
                MessagePart::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn has_approval_card(&self, chat: &str) -> bool {
        self.entries(chat)
            .iter()
            .flat_map(|entry| entry.parts.iter())
            .any(|part| {
                matches!(part, MessagePart::Input { questions, .. }
                    if questions.iter().any(|question| question.approval.is_some()))
            })
    }

    pub fn recovery(&self, chat: &str) -> Option<TaskRecovery> {
        self.core
            .sessions
            .session_status(chat)
            .and_then(|session| session.recovery)
    }

    /// No live run, and the chat shows how the last message ended: a
    /// finished assistant entry or a recovery notice.
    pub fn settled(&self, chat: &str) -> bool {
        !self.core.sessions.has_live_run(chat)
            && (self.recovery(chat).is_some()
                || self.assistant(chat).last().is_some_and(|entry| {
                    matches!(
                        entry.status,
                        Some(MessageStatus::Complete | MessageStatus::Aborted)
                    )
                }))
    }

    pub async fn catalog(&self) -> Value {
        self.call("GetProviderCatalog", json!({}))
            .await
            .expect("catalog")
    }

    pub async fn providers(&self) -> Value {
        self.call("ListExtensionProviders", json!({}))
            .await
            .expect("list extension providers")
    }

    /// LUNA's own OpenCode package as `ListExtensions` shows it.
    pub async fn package(&self) -> Option<Value> {
        self.call("ListExtensions", json!({}))
            .await
            .expect("list extensions")["packages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|package| package["id"] == PACKAGE_ID)
            .cloned()
    }

    pub async fn instances(&self) -> Vec<Value> {
        self.call("ListExtensionInstances", json!({}))
            .await
            .expect("list instances")["instances"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|view| view["extensionId"] == PACKAGE_ID)
            .cloned()
            .collect()
    }
}

pub fn opencode_rows(catalog: &Value) -> Vec<Value> {
    catalog["providers"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["id"] == "opencode")
        .cloned()
        .collect()
}

pub fn run_request(prompt: &str, cwd: &Path, mode: PermissionMode) -> RunRequest {
    RunRequest {
        review: None,
        goal: None,
        input_items: Vec::new(),
        prompt: prompt.into(),
        harness: Some(HarnessId::Opencode),
        provider_account_id: None,
        model: Some(MODEL.into()),
        reasoning: None,
        model_options: Default::default(),
        cwd: cwd.to_string_lossy().into_owned(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: false,
        permission_mode: mode,
        parent_permission_ceiling: None,
        environment_permission_ceiling: None,
        permission_policy: None,
        resume: None,
        attachments: Vec::new(),
        worktree: None,
    }
}

/// The scripted server's own JSON-line records of `kind` in `checkout`.
pub fn records(checkout: &Path, kind: &str) -> Vec<Value> {
    fs::read_to_string(
        checkout
            .join(".fake-opencode")
            .join(format!("{kind}.jsonl")),
    )
    .map(|text| {
        text.lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    })
    .unwrap_or_default()
}

/// The ordinary (non-steer) prompts the server received.
pub fn turn_prompts(checkout: &Path) -> Vec<Value> {
    records(checkout, "prompts")
        .into_iter()
        .filter(|prompt| prompt["delivery"] != "steer")
        .collect()
}

pub async fn wait_until(within: Duration, what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + within;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Whether a process with this pid exists (zombies included), by the OS.
pub fn process_exists(pid: u64) -> bool {
    let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

pub async fn wait_process_gone(pid: u64, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if !process_exists(pid) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    !process_exists(pid)
}

/// The scripted server's boot records in `checkout`, waited for.
pub async fn server_boots(checkout: &Path, count: usize) -> Vec<Value> {
    let deadline = Instant::now() + WAIT;
    loop {
        let boots = records(checkout, "servers");
        if boots.len() >= count {
            return boots;
        }
        assert!(
            Instant::now() < deadline,
            "{} never booted {count} servers: {boots:?}",
            checkout.display()
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// What a chat shows, without the identities that differ between two runs
/// of the same conversation: every string of the projection goes through
/// [`scrub`] (provider ids, UUIDs, loopback ports, the era's chat id and
/// checkout path). Entry ids and times are left out.
pub fn transcript(engine: &Engine, chat: &str, checkout: &Path) -> Value {
    let entries: Vec<Value> = engine
        .entries(chat)
        .iter()
        .map(|entry| {
            let parts: Vec<Value> = entry
                .parts
                .iter()
                .map(|part| match part {
                    MessagePart::Text { text, .. } => json!({"text": text}),
                    MessagePart::Reasoning { text, .. } => json!({"reasoning": text}),
                    MessagePart::Tool {
                        call,
                        is_error,
                        resolved,
                        output,
                        ..
                    } => json!({
                        "tool": serde_json::to_value(call).unwrap(),
                        "isError": is_error,
                        "resolved": resolved,
                        "output": output,
                    }),
                    MessagePart::Input {
                        questions,
                        resolved,
                        ..
                    } => {
                        let asked: Vec<&str> = questions
                            .iter()
                            .map(|question| question.question.as_str())
                            .collect();
                        let approval = questions.iter().any(|question| question.approval.is_some());
                        json!({"input": asked, "approval": approval, "resolved": resolved})
                    }
                    MessagePart::Error { message, .. } => json!({"error": message}),
                    MessagePart::Image { name, .. } => json!({"image": name}),
                    MessagePart::AsyncQuestion { resolved, .. } => {
                        json!({"asyncQuestion": resolved})
                    }
                })
                .collect();
            json!({
                "role": serde_json::to_value(entry.role).unwrap(),
                "status": serde_json::to_value(entry.status).unwrap(),
                "parts": parts,
            })
        })
        .collect();
    let recovery = engine.recovery(chat).map(|recovery| {
        json!({
            "acceptance": serde_json::to_value(recovery.acceptance).unwrap(),
            "detail": recovery.detail,
        })
    });
    let mut projection = json!({"entries": entries, "recovery": recovery});
    let checkout = checkout.to_string_lossy().into_owned();
    scrub_strings(&mut projection, &|text| scrub(text, chat, &checkout));
    projection
}

fn scrub_strings(value: &mut Value, scrub: &dyn Fn(&str) -> String) {
    match value {
        Value::String(text) => *text = scrub(text),
        Value::Array(items) => items.iter_mut().for_each(|item| scrub_strings(item, scrub)),
        Value::Object(map) => map.values_mut().for_each(|item| scrub_strings(item, scrub)),
        _ => {}
    }
}

/// Provider id prefixes OpenCode and the adapter mint per server or run
/// (sessions, messages, parts, permission requests, forms, bus events).
const ID_PREFIXES: [&str; 6] = ["ses_", "msg_", "prt_", "per_", "frm_", "evt_"];

/// `text` with every identity that differs between two eras replaced: the
/// checkout path, the chat id (each era has its own), provider ids
/// (`ses_…` → `ses_*`), UUIDs and loopback ports.
pub fn scrub(text: &str, chat: &str, checkout: &str) -> String {
    let text = text.replace(checkout, "<checkout>").replace(chat, "<chat>");
    let mut out = String::with_capacity(text.len());
    let mut rest = text.as_str();
    while let Some(ch) = rest.chars().next() {
        let boundary = out
            .chars()
            .next_back()
            .is_none_or(|before| !before.is_ascii_alphanumeric() && before != '_');
        if boundary {
            if let Some(prefix) = ID_PREFIXES.iter().find(|prefix| rest.starts_with(**prefix)) {
                let body = rest[prefix.len()..]
                    .bytes()
                    .take_while(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
                    .count();
                if body > 0 {
                    out.push_str(prefix);
                    out.push('*');
                    rest = &rest[prefix.len() + body..];
                    continue;
                }
            }
            if is_uuid(rest) {
                out.push_str("<uuid>");
                rest = &rest[36..];
                continue;
            }
        }
        if let Some(port) = ["127.0.0.1:", "localhost:"]
            .iter()
            .find(|host| rest.starts_with(**host))
        {
            let digits = rest[port.len()..]
                .bytes()
                .take_while(u8::is_ascii_digit)
                .count();
            if digits > 0 {
                out.push_str(port);
                out.push('*');
                rest = &rest[port.len() + digits..];
                continue;
            }
        }
        out.push(ch);
        rest = &rest[ch.len_utf8()..];
    }
    out
}

/// `8-4-4-4-12` hex digits at the start of `text`.
fn is_uuid(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() >= 36
        && bytes[..36]
            .iter()
            .enumerate()
            .all(|(index, byte)| match index {
                8 | 13 | 18 | 23 => *byte == b'-',
                _ => byte.is_ascii_hexdigit(),
            })
        && bytes
            .get(36)
            .is_none_or(|next| !next.is_ascii_alphanumeric())
}

/// Keep `value` as `<dir>/<name>.json` when `LUNA_CORE11_EVIDENCE_DIR` is set.
pub fn evidence(name: &str, value: &Value) {
    if let Ok(dir) = std::env::var("LUNA_CORE11_EVIDENCE_DIR") {
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            Path::new(&dir).join(format!("{name}.json")),
            serde_json::to_vec_pretty(value).unwrap(),
        )
        .unwrap();
    }
}
