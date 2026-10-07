//! Discovery parity (OC-14) through the Claude-path hooks:
//! - per-location slash commands AND skills for `commands_in` (skills from
//!   `GET /api/skill` as [`SlashCommandKind::Skill`] entries, so the composer
//!   offers them under `$` like Claude's), invalidated when a run's bus
//!   reports `command.updated` / `skill.updated` for that location;
//! - `run_title` as one tool-less generation in a throwaway session (the
//!   hidden `title` agent, every permission denied, `…/generate` adds
//!   nothing to the session, which is removed afterwards), never an ordinary
//!   coding prompt; native OpenCode titles are ignored;
//! - read-only `native_extensions` (commands and skills a launch in that
//!   location loads, with the configuration sources),
//!   `session_mcp_observation` (the run's own `GET /api/mcp` readback,
//!   server names and status words only) and `mcp_control` status (the same
//!   readback from a short-lived server in the selected project, for the
//!   shared integration inventory's Check provider; enable/disable and MCP
//!   Apps reads stay unsupported). Nothing here writes OpenCode state (PD-3).

use std::sync::{Mutex, OnceLock};

use luna_proto::{
    NativeCommandKind, NativeCommandView, NativeExtensionsView, NativeOutputStyleView,
    NativeSettingsScope, NativeSettingsSourceView, ProviderMcpStatus, SlashCommandKind,
};
use sha2::{Digest, Sha256};

use super::*;
use crate::mcp_control::{
    McpLaunchReadback, ProviderMcpServer, SessionMcpObservation, SessionMcpServer,
};

/// Session MCP observations kept in memory (newest wins; bounded).
const MAX_SESSION_OBSERVATIONS: usize = 64;
/// How long a location's extensions readback is reused without a refresh.
const EXTENSIONS_TTL: Duration = Duration::from_secs(10 * 60);
/// How long a location's command and skill catalog is reused: changes a run
/// sees invalidate it at once; this bounds edits made while no run is live.
pub(super) const CATALOG_TTL: Duration = Duration::from_secs(10 * 60);
/// Bound on waiting for a freshly booted location to list its commands.
const LOCATION_LOAD_BUDGET: Duration = Duration::from_secs(8);
const LOCATION_LOAD_POLL: Duration = Duration::from_millis(250);

/// One location's commands and skills readback: `(epoch, read at, value)`.
type ExtensionsEntry = (u64, Instant, (Value, Option<Value>));

fn extensions_cache() -> &'static Mutex<HashMap<String, ExtensionsEntry>> {
    static CACHE: OnceLock<Mutex<HashMap<String, ExtensionsEntry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// A location's extensions readback, unless stale (catalog change or age).
pub(super) fn cached_extensions(location: &str) -> Option<(Value, Option<Value>)> {
    let epoch = catalog_epoch(location);
    extensions_cache().lock().ok().and_then(|cache| {
        cache
            .get(location)
            .filter(|(read_epoch, read_at, _)| {
                *read_epoch == epoch && read_at.elapsed() < EXTENSIONS_TTL
            })
            .map(|(_, _, readback)| readback.clone())
    })
}

/// `epoch` is the location's catalog epoch read before the fetch.
pub(super) fn cache_extensions(location: &str, epoch: u64, readback: (Value, Option<Value>)) {
    if let Ok(mut cache) = extensions_cache().lock() {
        cache.insert(location.to_owned(), (epoch, Instant::now(), readback));
    }
}

// ---------------------------------------------------------------------------
// Catalog epochs: a run that sees `command.updated` / `skill.updated` bumps
// its location, and cached catalogs from an older epoch are re-read.
// ---------------------------------------------------------------------------

fn epochs() -> &'static Mutex<HashMap<String, u64>> {
    static EPOCHS: OnceLock<Mutex<HashMap<String, u64>>> = OnceLock::new();
    EPOCHS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The current catalog generation of a discovery location.
pub(super) fn catalog_epoch(location: &str) -> u64 {
    epochs()
        .lock()
        .ok()
        .and_then(|epochs| epochs.get(location).copied())
        .unwrap_or(0)
}

/// OpenCode announced a command or skill change for `location`.
pub(super) fn bump_catalog_epoch(location: &str) {
    if let Ok(mut epochs) = epochs().lock() {
        *epochs.entry(location.to_owned()).or_default() += 1;
    }
}

// ---------------------------------------------------------------------------
// Wire reads
// ---------------------------------------------------------------------------

impl Server {
    /// `GET /api/skill` → `{location, data: [Skill.Info]}`.
    pub(super) async fn skills_wire(&self, directory: Option<&str>) -> Result<Value, HarnessError> {
        let raw = self.get_json("/api/skill", directory).await?;
        Ok(unwrap_data(raw))
    }

    /// One location's commands and skills. A probe server's location that is
    /// still booting answers EMPTY lists (observed on 2.0.22 right after the
    /// url line); OpenCode 2 always has built-in commands (`init`, `review`)
    /// and skills (`opencode`), so with `await_load` an empty list is polled
    /// briefly before it is believed. A run's location is already warm
    /// (session created, model set) and reads once.
    pub(super) async fn discovery_wire(
        &self,
        directory: Option<&str>,
        await_load: bool,
    ) -> Result<(Value, Option<Value>), HarnessError> {
        let budget = if await_load {
            LOCATION_LOAD_BUDGET
        } else {
            Duration::ZERO
        };
        let deadline = tokio::time::Instant::now() + budget;
        let commands = loop {
            let commands = self.commands_wire(directory).await?;
            let loaded = commands.as_array().is_some_and(|list| !list.is_empty());
            if loaded || tokio::time::Instant::now() >= deadline {
                break commands;
            }
            tokio::time::sleep(LOCATION_LOAD_POLL).await;
        };
        // A server without the skill route still answers commands.
        let skills = loop {
            match self.skills_wire(directory).await {
                Ok(skills)
                    if skills.as_array().is_some_and(|list| list.is_empty())
                        && tokio::time::Instant::now() < deadline =>
                {
                    tokio::time::sleep(LOCATION_LOAD_POLL).await;
                }
                Ok(skills) => break Some(skills),
                Err(_) => break None,
            }
        };
        Ok((commands, skills))
    }

    /// Commands plus skills for one location: the composer catalog. `await_load`
    /// for a freshly booted probe server (see [`Self::discovery_wire`]).
    pub(super) async fn discovery_catalog(
        &self,
        directory: Option<&str>,
        await_load: bool,
    ) -> Result<Vec<SlashCommand>, HarnessError> {
        let (commands, skills) = self.discovery_wire(directory, await_load).await?;
        Ok(merge_catalog(&commands, skills.as_ref()))
    }

    /// `GET /api/mcp` → `[{name, status: {status, error?}}]`, reduced to
    /// server names and status words (no URLs, headers or commands).
    pub(super) async fn mcp_servers(
        &self,
        directory: Option<&str>,
    ) -> Result<Vec<SessionMcpServer>, HarnessError> {
        let rows = unwrap_data(self.get_json("/api/mcp", directory).await?);
        Ok(rows
            .as_array()
            .map(|rows| {
                rows.iter()
                    .filter_map(|row| {
                        Some(SessionMcpServer {
                            name: row.get("name")?.as_str()?.to_owned(),
                            status: mcp_status(
                                row.pointer("/status/status")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default(),
                            ),
                            source: Some("OpenCode /api/mcp".into()),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    /// `GET /api/mcp` as provider readback rows for the shared integration
    /// inventory: names, status words and bounded, URL/path-redacted error
    /// text only. A freshly booted location answers empty lists until its
    /// configuration loads (2.0.22), so the read waits for the location's
    /// built-in commands first; its servers then start `pending` and are
    /// polled briefly, and still-pending rows stay pending (`settled` false)
    /// rather than being guessed.
    pub(super) async fn mcp_readback(
        &self,
        directory: Option<&str>,
    ) -> Result<(Vec<ProviderMcpServer>, bool), HarnessError> {
        let deadline = tokio::time::Instant::now() + LOCATION_LOAD_BUDGET;
        while !self
            .commands_wire(directory)
            .await?
            .as_array()
            .is_some_and(|list| !list.is_empty())
            && tokio::time::Instant::now() < deadline
        {
            tokio::time::sleep(LOCATION_LOAD_POLL).await;
        }
        loop {
            let rows = unwrap_data(self.get_json("/api/mcp", directory).await?);
            let servers: Vec<ProviderMcpServer> = rows
                .as_array()
                .map(|rows| {
                    rows.iter()
                        .filter_map(|row| {
                            let name = row.get("name")?.as_str()?.to_owned();
                            let status = mcp_status(
                                row.pointer("/status/status")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default(),
                            );
                            let error = row
                                .pointer("/status/error")
                                .and_then(Value::as_str)
                                .filter(|text| !text.trim().is_empty())
                                .map(|text| crate::claude::redact_provider_text(text, 240));
                            Some(ProviderMcpServer {
                                name,
                                status,
                                error,
                                ..ProviderMcpServer::default()
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            let settled = !servers
                .iter()
                .any(|server| server.status == ProviderMcpStatus::Pending);
            if settled || tokio::time::Instant::now() >= deadline {
                return Ok((servers, settled));
            }
            tokio::time::sleep(LOCATION_LOAD_POLL).await;
        }
    }

    /// One tool-less title generation through a throwaway session: created
    /// with OpenCode's hidden `title` agent and a deny-everything ruleset (no
    /// tool reaches the model), asked once with `POST …/generate` (text from
    /// the session's context WITHOUT adding to it), then removed. The
    /// stateless `/api/experimental/generate` is not used: OpenCode's free
    /// tier refuses requests made outside a session ("can only be used from
    /// within OpenCode", 2.0.22). `model` is `(provider, model id)`.
    pub(super) async fn title_text(
        &self,
        prompt: &str,
        model: Option<(&str, &str)>,
        directory: &str,
    ) -> Result<String, HarnessError> {
        if model.is_some() {
            // A freshly booted location resolves no model until its catalog
            // has synced: wait for it like a run does.
            self.provider_catalog(Some(directory)).await?;
        }
        let mut body = json!({
            "location": { "directory": directory },
            "agent": TITLE_AGENT,
            "permissions": [{ "action": "*", "resource": "*", "effect": "deny" }],
        });
        if let Some((provider, id)) = model {
            body["model"] = json!({ "providerID": provider, "id": id });
        }
        let created = self
            .post_json("/api/session", Some(directory), &body)
            .await?;
        let session_id = created
            .pointer("/data/id")
            .or_else(|| created.get("id"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| {
                HarnessError::Protocol("opencode session create returned no id".into())
            })?;
        let answer = self
            .post_json(
                &format!("/api/session/{session_id}/generate"),
                Some(directory),
                &json!({ "prompt": prompt }),
            )
            .await;
        // Best effort: the throwaway session must not linger in the user's
        // session list.
        let path = format!("/api/session/{session_id}");
        if let Err(error) = self
            .exchange(
                reqwest::Method::DELETE,
                &path,
                Some(directory),
                None,
                Some(CALL_TIMEOUT),
            )
            .await
        {
            tracing::debug!(target: "luna_harness::opencode", "title session cleanup failed: {error}");
        }
        answer?
            .pointer("/data/text")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| {
                HarnessError::Protocol("opencode title generation returned no text".into())
            })
    }
}

/// OpenCode's hidden built-in title agent: primary, every permission denied.
const TITLE_AGENT: &str = "title";

/// One readback answers every requested LUNA mode: OpenCode loads the same
/// configuration (and so the same MCP servers) in each of them; LUNA's mode
/// only compiles permission rules. LUNA's own `luna` server is attached per
/// run, so a probe server never lists it (the session readback does).
pub(super) fn mcp_launch_readback(
    modes: Vec<luna_proto::PermissionMode>,
    servers: Vec<ProviderMcpServer>,
    settled: bool,
) -> McpLaunchReadback {
    McpLaunchReadback {
        modes,
        restricted: false,
        settled,
        servers,
        app_read_supported: None,
    }
}

fn mcp_status(word: &str) -> ProviderMcpStatus {
    match word {
        "connected" => ProviderMcpStatus::Connected,
        "failed" => ProviderMcpStatus::Failed,
        "needs_auth" => ProviderMcpStatus::NeedsAuth,
        "pending" => ProviderMcpStatus::Pending,
        "disabled" => ProviderMcpStatus::Disabled,
        _ => ProviderMcpStatus::Unknown,
    }
}

/// Skills (`Skill.Info`) as composer skill entries. The name is the skill
/// id, the token OpenCode's typed `skills[]` input takes.
pub(super) fn skills_from_wire(skills: &Value) -> Vec<SlashCommand> {
    skills
        .as_array()
        .map(|skills| {
            skills
                .iter()
                .filter_map(|skill| {
                    let id = skill.get("id").and_then(Value::as_str)?;
                    Some(SlashCommand {
                        name: id.to_owned(),
                        description: skill
                            .get("description")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        input_hint: None,
                        kind: SlashCommandKind::Skill,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Commands first, then skills whose name no command already takes.
pub(super) fn merge_catalog(commands: &Value, skills: Option<&Value>) -> Vec<SlashCommand> {
    let mut catalog = commands_from_wire(commands);
    for skill in skills.map(skills_from_wire).unwrap_or_default() {
        if !catalog.iter().any(|entry| entry.name == skill.name) {
            catalog.push(skill);
        }
    }
    catalog
}

// ---------------------------------------------------------------------------
// Session MCP observations
// ---------------------------------------------------------------------------

fn observations() -> &'static Mutex<VecDeque<SessionMcpObservation>> {
    static OBSERVATIONS: OnceLock<Mutex<VecDeque<SessionMcpObservation>>> = OnceLock::new();
    OBSERVATIONS.get_or_init(|| Mutex::new(VecDeque::new()))
}

/// Keep what a run's server reported about MCP at its start.
pub(super) fn record_session_mcp(session_id: &str, cwd: &str, servers: Vec<SessionMcpServer>) {
    let Ok(mut observations) = observations().lock() else {
        return;
    };
    observations.retain(|observation| observation.session_id != session_id);
    observations.push_back(SessionMcpObservation {
        session_id: session_id.to_owned(),
        cwd: cwd.to_owned(),
        observed_at_ms: now_ms(),
        servers,
        capabilities_reported: false,
        app_read_capability: false,
        app_tool_meta_capability: false,
    });
    while observations.len() > MAX_SESSION_OBSERVATIONS {
        observations.pop_front();
    }
}

pub(super) fn session_mcp(session_id: &str) -> Option<SessionMcpObservation> {
    observations().lock().ok().and_then(|observations| {
        observations
            .iter()
            .rev()
            .find(|observation| observation.session_id == session_id)
            .cloned()
    })
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

// ---------------------------------------------------------------------------
// Native extensions (read-only)
// ---------------------------------------------------------------------------

/// The effective commands and skills a launch in `location` loads, from the
/// server's own lists, plus the configuration sources that shape them.
pub(super) fn extensions_view(
    location: &str,
    mode: luna_proto::PermissionMode,
    readback: Result<(Value, Option<Value>), String>,
) -> NativeExtensionsView {
    let home = crate::executable::home_dir();
    // The server's global config root, through a QA profile wrapper too;
    // unavailable (no user-scope rows) when the wrapper cannot be evaluated.
    let config_home = OpencodeStorage::current()
        .global_config_dir()
        .ok()
        .flatten();
    let project = std::path::Path::new(location);
    let display = |path: &std::path::Path| -> String {
        if let Ok(rest) = path.strip_prefix(project) {
            return format!("./{}", rest.display());
        }
        match home
            .as_deref()
            .and_then(|home| path.strip_prefix(home).ok())
        {
            Some(rest) => format!("~/{}", rest.display()),
            None => path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
        }
    };
    let mut sources = Vec::new();
    if let Some(dir) = config_home.as_deref() {
        for name in ["opencode.json", "opencode.jsonc", "config.json"] {
            let path = dir.join(name);
            let present = path.is_file();
            sources.push(NativeSettingsSourceView {
                scope: NativeSettingsScope::User,
                display_path: display(&path),
                present,
                loaded: present,
                reason: None,
            });
        }
    }
    let project_disabled = [
        "OPENCODE_CONFIG_PROJECT_DISABLE",
        "OPENCODE_DISABLE_PROJECT_CONFIG",
    ]
    .iter()
    .any(|name| std::env::var(name).is_ok_and(|value| matches!(value.as_str(), "1" | "true")));
    for name in ["opencode.json", "opencode.jsonc", ".opencode"] {
        let path = project.join(name);
        let present = path.exists();
        sources.push(NativeSettingsSourceView {
            scope: NativeSettingsScope::Project,
            display_path: display(&path),
            present,
            loaded: present && !project_disabled,
            reason: (present && project_disabled)
                .then(|| "OpenCode project configuration is disabled for this launch".into()),
        });
    }
    let scope_of = |path: Option<&str>| -> NativeSettingsScope {
        let Some(path) = path.map(std::path::Path::new) else {
            return NativeSettingsScope::BuiltIn;
        };
        if path.starts_with(project) {
            NativeSettingsScope::Project
        } else if config_home
            .as_deref()
            .is_some_and(|dir| path.starts_with(dir))
            || home.as_deref().is_some_and(|home| path.starts_with(home))
        {
            NativeSettingsScope::User
        } else {
            NativeSettingsScope::BuiltIn
        }
    };
    let command_scope = |name: &str| -> NativeSettingsScope {
        let file = format!("{name}.md");
        let in_dir = |root: &std::path::Path| {
            ["command", "commands"]
                .iter()
                .any(|folder| root.join(folder).join(&file).is_file())
        };
        if in_dir(&project.join(".opencode")) {
            NativeSettingsScope::Project
        } else if config_home.as_deref().is_some_and(in_dir) {
            NativeSettingsScope::User
        } else {
            NativeSettingsScope::BuiltIn
        }
    };
    let (commands, native_error) = match &readback {
        Ok((commands, skills)) => {
            let mut views: Vec<NativeCommandView> = commands_from_wire(commands)
                .into_iter()
                .map(|command| NativeCommandView {
                    scope: command_scope(&command.name),
                    name: command.name,
                    kind: NativeCommandKind::Command,
                    description: command.description,
                    argument_hint: None,
                    configured_effective: true,
                    native: Some(true),
                    shadowed_by: None,
                    reason: None,
                })
                .collect();
            for skill in skills
                .as_ref()
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let Some(id) = skill.get("id").and_then(Value::as_str) else {
                    continue;
                };
                views.push(NativeCommandView {
                    name: id.to_owned(),
                    kind: NativeCommandKind::Skill,
                    scope: scope_of(skill.get("path").and_then(Value::as_str)),
                    description: skill
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    argument_hint: None,
                    configured_effective: true,
                    native: Some(true),
                    shadowed_by: None,
                    reason: None,
                });
            }
            (views, None)
        }
        Err(error) => (Vec::new(), Some(error.clone())),
    };
    let fingerprint = {
        let mut hasher = Sha256::new();
        for command in &commands {
            hasher.update(format!(
                "{:?}\0{}\0{:?}\n",
                command.kind, command.name, command.scope
            ));
        }
        for source in &sources {
            hasher.update(format!("{}\0{}\n", source.display_path, source.present));
        }
        format!("{:x}", hasher.finalize())
    };
    NativeExtensionsView {
        provider: HarnessId::Opencode,
        permission_mode: mode,
        launch_policy: "OpenCode loads its global and project configuration; LUNA adds one \
                        inline layer (OPENCODE_CONFIG_CONTENT) that carries only its own `luna` \
                        MCP server and writes nothing to disk."
            .into(),
        sources,
        output_style: NativeOutputStyleView {
            configured_effective: "default".into(),
            ..Default::default()
        },
        hooks: Vec::new(),
        native_verified: native_error.is_none(),
        native_error,
        commands,
        fingerprint,
        limitations: vec![
            "OpenCode does not report where a command is defined; scopes come from its command \
             folders, and anything else shows as built-in."
                .into(),
            "Output styles and hooks have no OpenCode counterpart; OpenCode plugins are not listed."
                .into(),
        ],
    }
}

// ---------------------------------------------------------------------------
// Titles
// ---------------------------------------------------------------------------

/// `provider/model` → `(provider, model)`.
pub(super) fn model_ref(model: Option<&str>) -> Option<(&str, &str)> {
    model.and_then(|model| model.split_once('/'))
}

/// A finished title generation as the event stream the engine collects.
pub(super) fn title_events(text: String) -> Vec<Result<AgentEvent, HarnessError>> {
    vec![
        Ok(AgentEvent::TextDelta { text }),
        Ok(AgentEvent::Done {
            status: DoneStatus::Completed,
            result: None,
            error: None,
            session_id: None,
        }),
    ]
}
