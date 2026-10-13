//! AgentAccounts — metadata-only discovery and provider-owned sign-in for the
//! Claude Code and Codex accounts on this device.
//!
//! Provider-native authentication remains authoritative:
//!
//! - **Claude Code** — credentials in `~/.claude/.credentials.json`
//!   (`$CLAUDE_CONFIG_DIR` relocates the dir); LUNA never accesses the OS credential
//!   store. The account identity (`oauthAccount`, `userID`)
//!   lives in `~/.claude.json`.
//! - **Codex** — the default account uses `$CODEX_HOME/auth.json` (default
//!   `~/.codex`). Named accounts use LUNA-owned T3-style auth overlays: only
//!   `auth.json` and `models_cache.json` are private while the canonical
//!   session/config home is shared through symlinks.
//!
//! LUNA persists only lifecycle metadata (Claude enabled/disabled, Codex named
//! account labels and the selected default). It never puts provider tokens in
//! the sync vault, copies a whole provider home, edits provider databases, or
//! deletes provider credential material. Historical plaintext slot files are
//! only detected and reported; they are not opened.
//!
//! Legacy explicit account probes remain available to non-UI callers. The
//! engine's shared usage cache owns periodic native quota refreshes; UI lists
//! use cached rows without waiting for the network. Successful account windows
//! remain available across a transient failed refresh, scoped to account keys.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as BASE64_URL;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

use luna_proto::{
    AgentAccount, AgentAccountWarning, AgentAccountsSnapshot, AgentAuthKind,
    AgentAuthenticationState, AgentLoginMode, AgentLoginPoll, AgentLoginStart, AgentLoginStatus,
    AgentUsageWindow, HarnessId,
};

use crate::repos::home_dir;
use crate::{EngineError, new_id, now_ms};

// Claude Code's public OAuth client (the one the CLI itself uses for the manual
// "paste the code" flow — no secret involved, PKCE carries the proof).
const CLAUDE_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const CLAUDE_REDIRECT: &str = "https://console.anthropic.com/oauth/code/callback";
const CLAUDE_SCOPES: &str = "org:create_api_key user:profile user:inference";
const CLAUDE_TOKEN_URL: &str = "https://console.anthropic.com/v1/oauth/token";
const CLAUDE_PROFILE_URL: &str = "https://api.anthropic.com/api/oauth/profile";
const CLAUDE_USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const CODEX_USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";

/// Claude Code stores these next to `claudeAiOauth` in the same credential
/// blob, but they are machine-shared (MCP server OAuth, plugin secrets) and
/// rotate independently of any account slot. On activate the live copies win.
const CLAUDE_SHARED_CREDENTIAL_KEYS: &[&str] = &[
    "mcpOAuth",
    "mcpOAuthClientConfig",
    "mcpXaaIdp",
    "mcpXaaIdpConfig",
    "pluginSecrets",
];

/// An abandoned login flow (dialog dismissed without Cancel) is reaped past this.
const FLOW_TTL: Duration = Duration::from_secs(15 * 60);
const HTTP_TIMEOUT: Duration = Duration::from_secs(8);
pub const CLAUDE_NATIVE_ACCOUNT_ID: &str = "claude-native";
pub const CODEX_NATIVE_ACCOUNT_ID: &str = "codex-native";
const PROVIDER_ACCOUNT_STATE_VERSION: u32 = 1;
const CODEX_SHARED_DIRECTORIES: &[&str] = &[
    "sessions",
    "archived_sessions",
    "sqlite",
    "shell_snapshots",
    "worktrees",
    "skills",
    "plugins",
    "cache",
    "logs",
    "mcp-oauth-locks",
];
const CODEX_PRIVATE_ENTRIES: &[&str] = &["auth.json", "models_cache.json"];
const CODEX_SHADOW_LOCAL_ENTRIES: &[&str] = &["log", "memories", "tmp"];
const DISABLE_CLAUDE_KEYCHAIN_FALLBACK: &str = ".disable-claude-keychain-fallback";

/// Filesystem knobs — env-resolved in production ([`AgentAccountsConfig::detect`]),
/// explicit in tests.
#[derive(Debug, Clone)]
pub struct AgentAccountsConfig {
    /// Engine data dir; historical slots may remain under
    /// `{data_dir}/agent-accounts/` but LUNA no longer opens them.
    pub data_dir: PathBuf,
    /// Claude config dir (`$CLAUDE_CONFIG_DIR` or `~/.claude`) — holds `.credentials.json`.
    pub claude_config_dir: PathBuf,
    /// Claude identity file (`~/.claude.json`, or `$CLAUDE_CONFIG_DIR/.claude.json`).
    pub claude_config_file: PathBuf,
    /// Codex home (`$CODEX_HOME` or `~/.codex`) — holds `auth.json`.
    pub codex_home: PathBuf,
}

impl AgentAccountsConfig {
    /// Production resolution: `CLAUDE_CONFIG_DIR` relocates both the Claude config
    /// json and the credentials file; `CODEX_HOME` relocates the Codex auth file.
    pub fn detect(data_dir: &Path) -> Self {
        let env_dir = |name: &str| {
            std::env::var_os(name)
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
        };
        let claude_dir = env_dir("CLAUDE_CONFIG_DIR");
        let claude_config_file = match &claude_dir {
            Some(dir) => dir.join(".claude.json"),
            None => home_dir().join(".claude.json"),
        };
        Self {
            data_dir: data_dir.to_path_buf(),
            claude_config_dir: claude_dir.unwrap_or_else(|| home_dir().join(".claude")),
            claude_config_file,
            codex_home: env_dir("CODEX_HOME").unwrap_or_else(|| home_dir().join(".codex")),
        }
    }

    fn claude_creds_file(&self) -> PathBuf {
        self.claude_config_dir.join(".credentials.json")
    }

    /// Legacy fixture marker retained for existing isolated test profiles.
    /// Production never accesses Keychain, with or without this marker.
    #[doc(hidden)]
    pub fn disable_claude_keychain_fallback(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.claude_config_dir)?;
        std::fs::write(
            self.claude_config_dir
                .join(DISABLE_CLAUDE_KEYCHAIN_FALLBACK),
            b"fixture-only\n",
        )
    }

    fn codex_auth_file(&self) -> PathBuf {
        self.codex_home.join("auth.json")
    }

    fn root_dir(&self) -> PathBuf {
        self.data_dir.join("agent-accounts")
    }

    fn state_file(&self) -> PathBuf {
        self.data_dir.join("provider-account-state-v1.json")
    }

    fn codex_shadow_root(&self) -> PathBuf {
        self.data_dir.join("provider-auth").join("codex")
    }

    fn codex_shadow_home(&self, account_id: &str) -> PathBuf {
        self.codex_shadow_root().join(account_id)
    }
}

// ── slot storage ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SlotProfile {
    email: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    organization: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    plan: Option<String>,
    auth_kind: AgentAuthKind,
}

/// Ephemeral provider login material used only while probing usage or
/// completing an explicit provider-native sign-in. Never persisted by LUNA.
#[derive(Clone)]
struct Slot {
    id: String,
    /// The provider-side identity the slot is keyed by (account uuid/email).
    account_key: String,
    profile: SlotProfile,
    /// Claude: an explicitly available `.credentials.json` payload. Codex: `auth.json`.
    credentials: serde_json::Value,
    /// Claude only: `{oauthAccount, userID}` merged into the provider config
    /// while completing an explicit sign-in.
    claude_config: Option<serde_json::Value>,
}

/// A live provider-native detection result.
#[derive(Clone)]
struct Detected {
    account_key: String,
    profile: SlotProfile,
    /// `None` ⇒ we know a login exists but couldn't read the secret.
    credentials: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CodexAccountRecord {
    id: String,
    #[serde(default)]
    label: String,
    account_key: String,
    profile: SlotProfile,
    created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProviderAccountState {
    schema_version: u32,
    #[serde(default = "enabled_by_default")]
    claude_enabled: bool,
    #[serde(default = "native_codex_account")]
    default_codex_account_id: String,
    #[serde(default)]
    codex_accounts: Vec<CodexAccountRecord>,
}

fn enabled_by_default() -> bool {
    true
}

fn native_codex_account() -> String {
    CODEX_NATIVE_ACCOUNT_ID.to_string()
}

fn next_codex_label(state: &ProviderAccountState) -> String {
    format!("Codex {}", state.codex_accounts.len() + 2)
}

fn safe_account_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

impl Default for ProviderAccountState {
    fn default() -> Self {
        Self {
            schema_version: PROVIDER_ACCOUNT_STATE_VERSION,
            claude_enabled: true,
            default_codex_account_id: native_codex_account(),
            codex_accounts: Vec::new(),
        }
    }
}

/// Engine-only execution identity. `auth_instance_id` deliberately does not
/// participate in L06's provider binding: Codex continuations stay keyed to
/// the canonical session store while this selects only the authentication
/// overlay used to launch the runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderExecutionAccount {
    pub account_id: String,
    pub auth_instance_id: String,
    pub codex_home: Option<PathBuf>,
    /// Named Codex overlays must force the CLI's documented file credential
    /// store. The provider-native default deliberately keeps its own policy.
    pub codex_file_auth: bool,
}

// ── login flows ─────────────────────────────────────────────────────────────

enum LoginFlow {
    Claude {
        verifier: String,
        started_at: Instant,
    },
    /// A provider-owned login child polled to completion.
    Spawned {
        harness: HarnessId,
        /// Named Codex account being created. Other spawned providers do not
        /// have LUNA-owned account metadata.
        account_id: Option<String>,
        /// The login child; monitored (try_wait) + killable from cancel.
        child: Arc<Mutex<Option<luna_harness::process::Child>>>,
        /// Provider credential dir watched for completion.
        home: PathBuf,
        /// Digest of the provider-native auth file before explicit sign-in.
        /// Prevents an already-authenticated account from being mistaken for
        /// completion of the new browser flow.
        baseline_fingerprint: Option<[u8; 32]>,
        started_at: Instant,
        output: Arc<Mutex<String>>,
        /// `Some(code)` once the child exited (`None` code = killed by signal).
        exit: Arc<Mutex<Option<Option<i32>>>>,
    },
}

impl LoginFlow {
    fn started_at(&self) -> Instant {
        match self {
            LoginFlow::Claude { started_at, .. } | LoginFlow::Spawned { started_at, .. } => {
                *started_at
            }
        }
    }
}

// ── service ─────────────────────────────────────────────────────────────────

/// Cached usage probe result: the windows (or a remembered miss) + fetch time.
type CachedUsage = (Option<UsageSnapshot>, Instant);

/// One live usage probe: rate-limit windows plus the plan label the provider
/// reported alongside them (Codex's usage endpoint carries a live `plan_type`,
/// which supersedes the login-time JWT claim — plan changes show up here
/// without a re-login). Claude's usage endpoint has no plan field.
#[derive(Clone, Default)]
struct UsageSnapshot {
    windows: Vec<AgentUsageWindow>,
    plan_label: Option<String>,
}

struct Inner {
    config: AgentAccountsConfig,
    http: reqwest::Client,
    flows: Mutex<HashMap<String, LoginFlow>>,
    /// `"{harness}:{accountKey}"` → cached usage windows.
    usage_cache: Mutex<HashMap<String, CachedUsage>>,
    state: Mutex<ProviderAccountState>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Clone)]
pub struct AgentAccounts {
    inner: Arc<Inner>,
}

impl AgentAccounts {
    /// IDs only: background catalog refresh does not need usage or profile
    /// reads, and must not touch another provider's credential store.
    pub(crate) fn codex_account_ids(&self) -> Vec<String> {
        let state = lock(&self.inner.state);
        let mut ids = vec![CODEX_NATIVE_ACCOUNT_ID.to_owned()];
        for account in &state.codex_accounts {
            if !ids.contains(&account.id) {
                ids.push(account.id.clone());
            }
        }
        ids
    }

    pub fn new(config: AgentAccountsConfig) -> Self {
        let http = reqwest::Client::builder()
            .timeout(HTTP_TIMEOUT)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        let state = load_account_state(&config);
        Self {
            inner: Arc::new(Inner {
                config,
                http,
                flows: Mutex::new(HashMap::new()),
                usage_cache: Mutex::new(HashMap::new()),
                state: Mutex::new(state),
            }),
        }
    }

    pub fn resolve_execution(
        &self,
        harness: HarnessId,
        requested_account_id: Option<&str>,
    ) -> Result<ProviderExecutionAccount, EngineError> {
        let state = lock(&self.inner.state).clone();
        match (harness).kind() {
            luna_proto::BuiltinHarnessId::ClaudeCode => {
                if requested_account_id.is_some_and(|id| id != CLAUDE_NATIVE_ACCOUNT_ID) {
                    return Err(EngineError::Other(
                        "Claude Code supports one provider-native account on this device.".into(),
                    ));
                }
                if !state.claude_enabled {
                    return Err(EngineError::Other(
                        "Claude Code is disconnected in LUNA. Reconnect it from Accounts before sending."
                            .into(),
                    ));
                }
                let config = read_json(&self.inner.config.claude_config_file).ok_or_else(|| {
                    EngineError::Other(
                        "Claude Code has no detectable provider-native account on this device."
                            .into(),
                    )
                })?;
                let oauth = config.get("oauthAccount").ok_or_else(|| {
                    EngineError::Other(
                        "Claude Code has no detectable provider-native account on this device."
                            .into(),
                    )
                })?;
                let account_key = str_field(oauth, "accountUuid")
                    .or_else(|| str_field(oauth, "emailAddress"))
                    .ok_or_else(|| {
                        EngineError::Other(
                            "Claude Code's native account identity is unavailable.".into(),
                        )
                    })?;
                Ok(ProviderExecutionAccount {
                    account_id: CLAUDE_NATIVE_ACCOUNT_ID.into(),
                    auth_instance_id: format!(
                        "claude-auth:{}",
                        &crate::repos::hex(&Sha256::digest(account_key.as_bytes()))[..16]
                    ),
                    codex_home: None,
                    codex_file_auth: false,
                })
            }
            luna_proto::BuiltinHarnessId::Codex => {
                if state.default_codex_account_id.is_empty() {
                    return Err(EngineError::Other(
                        "No Codex account is connected in LUNA. Connect one in Providers first."
                            .into(),
                    ));
                }
                let account_id = requested_account_id
                    .filter(|id| !id.trim().is_empty())
                    .unwrap_or(&state.default_codex_account_id);
                let home = self.codex_home_for_account_with_state(&state, account_id)?;
                let detected = read_json(&home.join("auth.json")).and_then(parse_codex_auth);
                if account_id != CODEX_NATIVE_ACCOUNT_ID && detected.is_none() {
                    return Err(EngineError::Other(
                        "The selected Codex account no longer has a usable provider login.".into(),
                    ));
                }
                let identity = detected
                    .as_ref()
                    .map(|detected| detected.account_key.as_str())
                    .unwrap_or("provider-managed-native");
                Ok(ProviderExecutionAccount {
                    account_id: account_id.to_string(),
                    auth_instance_id: format!(
                        "codex-auth:{account_id}:{}",
                        &crate::repos::hex(&Sha256::digest(identity.as_bytes()))[..16]
                    ),
                    codex_home: Some(home),
                    codex_file_auth: account_id != CODEX_NATIVE_ACCOUNT_ID,
                })
            }
            _ => Ok(ProviderExecutionAccount {
                account_id: requested_account_id.unwrap_or_default().to_string(),
                auth_instance_id: format!("{}:native", harness_slug(harness)),
                codex_home: None,
                codex_file_auth: false,
            }),
        }
    }

    pub fn codex_home_for_account(&self, account_id: &str) -> Result<PathBuf, EngineError> {
        let state = lock(&self.inner.state).clone();
        self.codex_home_for_account_with_state(&state, account_id)
    }

    /// Resolve the selected provider-owned configuration location for a
    /// read-only inventory. Unlike execution resolution this neither reads
    /// auth material nor creates/refreshes a named account's shadow home.
    pub fn codex_home_for_inventory(&self) -> Result<PathBuf, EngineError> {
        self.codex_usage_target().map(|(home, _)| home)
    }

    /// Provider-owned skill roots; no account or credential read is involved.
    /// OpenCode has no account configuration in LUNA, so use the config root
    /// the OpenCode server LUNA would spawn uses (`OPENCODE_CONFIG_DIR`, else
    /// `$XDG_CONFIG_HOME/opencode`, else `~/.config/opencode`), as a QA
    /// profile wrapper sets it. When that wrapper cannot be evaluated the
    /// root is empty, which the skill library refuses as not absolute rather
    /// than falling back to the user's config.
    /// Exact provider-owned MCP/account settings source for inventory freshness.
    pub(crate) fn claude_settings_source(&self) -> &Path {
        &self.inner.config.claude_config_file
    }

    pub fn skill_config_dirs(&self) -> (PathBuf, PathBuf) {
        let storage = luna_harness::opencode::OpencodeStorage::current();
        let opencode = match storage.config_dir() {
            Ok(Some(dir)) => dir,
            Ok(None) => home_dir().join(".config/opencode"),
            Err(_) => PathBuf::new(),
        };
        tracing::debug!(
            target: "luna_engine::skills",
            root = %opencode.display(),
            "OpenCode skill config root"
        );
        (self.inner.config.claude_config_dir.clone(), opencode)
    }

    /// Provider-owned transcript locations and LUNA's own cache directory.
    /// This does not open credentials or materialize a named provider home.
    pub fn usage_history_paths(&self) -> (Option<PathBuf>, PathBuf, PathBuf) {
        let (codex, claude, data) = self.usage_history_selection();
        (codex.map(|(home, _)| home), claude, data)
    }

    /// Capture the selected home together with its auth-store mode so history
    /// and quota refreshes share one selection across asynchronous work.
    pub(crate) fn usage_history_selection(&self) -> (Option<(PathBuf, bool)>, PathBuf, PathBuf) {
        (
            self.codex_usage_target().ok(),
            self.inner.config.claude_config_dir.clone(),
            self.inner.config.data_dir.clone(),
        )
    }

    /// The selected Codex home and its provider-owned auth-store mode for a
    /// read-only app-server usage request. This never materializes a home or
    /// opens an auth file; the provider remains responsible for credentials.
    pub fn codex_usage_target(&self) -> Result<(PathBuf, bool), EngineError> {
        let state = lock(&self.inner.state);
        let id = state.default_codex_account_id.as_str();
        if id == CODEX_NATIVE_ACCOUNT_ID {
            return Ok((self.inner.config.codex_home.clone(), false));
        }
        if !safe_account_id(id) || !state.codex_accounts.iter().any(|account| account.id == id) {
            return Err(EngineError::Other(
                "The selected Codex account has no known provider home".into(),
            ));
        }
        Ok((self.inner.config.codex_shadow_home(id), true))
    }

    fn codex_home_for_account_with_state(
        &self,
        state: &ProviderAccountState,
        account_id: &str,
    ) -> Result<PathBuf, EngineError> {
        if account_id == CODEX_NATIVE_ACCOUNT_ID {
            // A native Codex login may live in the OS keyring, which LUNA must
            // not probe. Preserve provider-native execution even when no
            // inspectable auth.json exists; Codex remains authoritative.
            return Ok(self.inner.config.codex_home.clone());
        }
        if !safe_account_id(account_id) {
            return Err(EngineError::Other("Invalid Codex account identity.".into()));
        }
        if !state
            .codex_accounts
            .iter()
            .any(|record| record.id == account_id)
        {
            return Err(EngineError::Other(format!(
                "Unknown Codex account selection: {account_id}"
            )));
        }
        let home = self.inner.config.codex_shadow_home(account_id);
        materialize_codex_shadow_home(&self.inner.config.codex_home, &home)?;
        if read_json(&home.join("auth.json"))
            .and_then(parse_codex_auth)
            .is_none()
        {
            return Err(EngineError::Other(
                "That named Codex account no longer has a usable provider login.".into(),
            ));
        }
        Ok(home)
    }

    pub fn invalidate_caches(&self, harness: HarnessId) {
        let prefix = format!("{}:", harness_slug(harness));
        lock(&self.inner.usage_cache).retain(|key, _| !key.starts_with(&prefix));
    }

    fn update_state(
        &self,
        update: impl FnOnce(&mut ProviderAccountState),
    ) -> Result<(), EngineError> {
        let mut state = lock(&self.inner.state);
        let mut next = state.clone();
        update(&mut next);
        persist_account_state(&self.inner.config, &next)?;
        *state = next;
        Ok(())
    }

    fn finish_codex_login(
        &self,
        account_id: Option<&str>,
        detected: &Detected,
    ) -> Result<(), EngineError> {
        let account_id = account_id.ok_or_else(|| {
            EngineError::Other("Codex named-account login lost its account identity.".into())
        })?;
        let native_available = self.detect_codex().is_some();
        self.update_state(|state| {
            state
                .codex_accounts
                .retain(|record| record.id != account_id);
            let label = next_codex_label(state);
            state.codex_accounts.push(CodexAccountRecord {
                id: account_id.to_string(),
                label,
                account_key: detected.account_key.clone(),
                profile: detected.profile.clone(),
                created_at: now_ms(),
            });
            if !native_available {
                state.default_codex_account_id = account_id.to_string();
            }
        })?;
        self.invalidate_caches(HarnessId::Codex);
        Ok(())
    }

    pub async fn rename(
        &self,
        harness: HarnessId,
        account_id: &str,
        label: &str,
    ) -> Result<AgentAccountsSnapshot, EngineError> {
        if harness != HarnessId::Codex || account_id == CODEX_NATIVE_ACCOUNT_ID {
            return Err(EngineError::Other(
                "Only named Codex accounts can be renamed in LUNA.".into(),
            ));
        }
        let label = label.trim();
        if label.is_empty() || label.chars().count() > 64 {
            return Err(EngineError::Other(
                "Account labels must contain 1 to 64 characters.".into(),
            ));
        }
        let mut found = false;
        self.update_state(|state| {
            if let Some(record) = state
                .codex_accounts
                .iter_mut()
                .find(|record| record.id == account_id)
            {
                record.label = label.to_string();
                found = true;
            }
        })?;
        if !found {
            return Err(EngineError::Other(format!(
                "Unknown Codex account selection: {account_id}"
            )));
        }
        self.list(false).await
    }

    // ── list ────────────────────────────────────────────────────────────────

    /// Detect provider-native live logins and assemble a metadata-only view.
    pub async fn list(&self, force_usage: bool) -> Result<AgentAccountsSnapshot, EngineError> {
        self.list_filtered(force_usage, None).await
    }

    /// Provider-scoped listing for surfaces such as the Codex model picker.
    /// This boundary matters on macOS: asking only for Codex must never probe
    /// Claude's native account metadata as a side effect.
    pub async fn list_harness(
        &self,
        harness: HarnessId,
        force_usage: bool,
    ) -> Result<AgentAccountsSnapshot, EngineError> {
        self.list_filtered(force_usage, Some(harness)).await
    }

    async fn list_filtered(
        &self,
        force_usage: bool,
        only_harness: Option<HarnessId>,
    ) -> Result<AgentAccountsSnapshot, EngineError> {
        let includes = |harness| only_harness.is_none_or(|only| only == harness);
        let state = lock(&self.inner.state).clone();
        let mut warnings: Vec<AgentAccountWarning> = Vec::new();
        let (claude, claude_warning) = if includes(HarnessId::ClaudeCode) {
            self.detect_claude().await
        } else {
            (None, None)
        };
        if let Some(message) = claude_warning {
            warnings.push(AgentAccountWarning {
                harness: HarnessId::ClaudeCode,
                message,
            });
        }
        let codex = includes(HarnessId::Codex)
            .then(|| self.detect_codex())
            .flatten();
        let mut accounts: Vec<AgentAccount> = Vec::new();
        if let Some(detected) = claude {
            let slot = slot_from_detected(CLAUDE_NATIVE_ACCOUNT_ID, &detected);
            let usage = self
                .usage_for(
                    HarnessId::ClaudeCode,
                    &slot,
                    state.claude_enabled,
                    force_usage && state.claude_enabled,
                )
                .await;
            accounts.push(account_from_slot(
                HarnessId::ClaudeCode,
                slot,
                usage,
                state.claude_enabled,
                !state.claude_enabled,
                Some(if !state.claude_enabled {
                    AgentAuthenticationState::Unavailable
                } else if detected
                    .credentials
                    .as_ref()
                    .and_then(|credentials| credentials.get("claudeAiOauth"))
                    .and_then(|oauth| str_field(oauth, "accessToken"))
                    .is_none_or(|token| token.is_empty())
                {
                    AgentAuthenticationState::Unknown
                } else {
                    AgentAuthenticationState::Authenticated
                }),
                None,
                None,
            ));
            if !state.claude_enabled {
                warnings.push(AgentAccountWarning {
                    harness: HarnessId::ClaudeCode,
                    message: "Claude Code is disconnected in LUNA. Its provider login and history were left untouched; reconnect explicitly to use it again."
                        .into(),
                });
            }
        }

        if let Some(detected) = codex {
            let slot = slot_from_detected(CODEX_NATIVE_ACCOUNT_ID, &detected);
            let active = state.default_codex_account_id == CODEX_NATIVE_ACCOUNT_ID;
            let usage = self
                .usage_for(HarnessId::Codex, &slot, active, force_usage)
                .await;
            accounts.push(account_from_slot(
                HarnessId::Codex,
                slot,
                usage,
                active,
                !active,
                None,
                None,
                None,
            ));
        } else if includes(HarnessId::Codex) {
            let active = state.default_codex_account_id == CODEX_NATIVE_ACCOUNT_ID;
            accounts.push(AgentAccount {
                id: CODEX_NATIVE_ACCOUNT_ID.into(),
                harness: HarnessId::Codex,
                label: None,
                email: None,
                plan_label: None,
                active,
                authentication: AgentAuthenticationState::Unknown,
                usage_windows: vec![],
                display_name: Some("Codex native (provider-managed)".into()),
                organization: None,
                auth_kind: None,
                switchable: !active,
                saved_at: None,
            });
            warnings.push(AgentAccountWarning {
                harness: HarnessId::Codex,
                message: "Codex may keep the native login in its provider-managed OS credential store. LUNA does not inspect that store; native runs remain available, but identity and usage are unavailable until Codex exposes them without credential access."
                    .into(),
            });
        }

        for record in state
            .codex_accounts
            .iter()
            .filter(|_| includes(HarnessId::Codex))
        {
            let home = self.inner.config.codex_shadow_home(&record.id);
            let detected = read_json(&home.join("auth.json")).and_then(parse_codex_auth);
            let active = state.default_codex_account_id == record.id;
            let slot = match &detected {
                Some(detected) => slot_from_detected(&record.id, detected),
                None => Slot {
                    id: record.id.clone(),
                    account_key: record.account_key.clone(),
                    profile: record.profile.clone(),
                    credentials: serde_json::json!({}),
                    claude_config: None,
                },
            };
            let usage = if detected.is_some() {
                self.usage_for(HarnessId::Codex, &slot, active, force_usage)
                    .await
            } else {
                None
            };
            accounts.push(account_from_slot(
                HarnessId::Codex,
                slot,
                usage,
                active,
                !active && detected.is_some(),
                detected
                    .is_none()
                    .then_some(AgentAuthenticationState::Unavailable),
                Some(record.created_at),
                Some(if record.label.is_empty() {
                    "Codex account".into()
                } else {
                    record.label.clone()
                }),
            ));
        }

        for harness in [HarnessId::ClaudeCode, HarnessId::Codex]
            .into_iter()
            .filter(|harness| includes(*harness))
        {
            if self.legacy_slots_present(harness) {
                warnings.push(AgentAccountWarning {
                    harness,
                    message: "Legacy LUNA account copies are present but disabled. LUNA will not read, activate, migrate, or delete them automatically. Provider-native login remains authoritative."
                        .into(),
                });
            }
        }
        if self.legacy_login_dirs_present() {
            warnings.push(AgentAccountWarning {
                harness: HarnessId::Codex,
                message: "An unfinished legacy isolated sign-in directory is present. LUNA left it untouched because it may contain credential material."
                    .into(),
            });
        }
        Ok(AgentAccountsSnapshot { accounts, warnings })
    }

    // ── swap ────────────────────────────────────────────────────────────────

    pub async fn activate(
        &self,
        harness: HarnessId,
        account_id: &str,
    ) -> Result<AgentAccountsSnapshot, EngineError> {
        match (harness).kind() {
            luna_proto::BuiltinHarnessId::ClaudeCode if account_id == CLAUDE_NATIVE_ACCOUNT_ID => {
                if self.detect_claude().await.0.is_none() {
                    return Err(EngineError::Other(
                        "Claude Code has no provider-native login to reconnect; sign in first."
                            .into(),
                    ));
                }
                self.update_state(|state| state.claude_enabled = true)?;
            }
            luna_proto::BuiltinHarnessId::Codex => {
                self.codex_home_for_account(account_id)?;
                self.update_state(|state| {
                    state.default_codex_account_id = account_id.to_string();
                })?;
            }
            _ => {
                return Err(EngineError::Other(
                    "This provider does not support a LUNA account selection action.".into(),
                ));
            }
        }
        self.invalidate_caches(harness);
        self.list(false).await
    }

    async fn activate_claude(&self, slot: &Slot) -> Result<(), EngineError> {
        // Merge the identity back into ~/.claude.json — everything else (caches,
        // project history, onboarding flags, mcpServers) is left untouched, which
        // is all Claude Code needs to treat this as a fresh login.
        //
        // GUARD the merge: a parse failure on an EXISTING file means "don't touch
        // it", not "start fresh" — writing only our identity fields would destroy
        // the user's entire Claude config. Only a missing file may start from {}.
        let file = &self.inner.config.claude_config_file;
        let cfg = read_json(file);
        if cfg.is_none() && file.exists() {
            return Err(EngineError::Other(
                "~/.claude.json exists but could not be parsed — not switching to avoid wiping \
                 it. Fix or remove the file and try again."
                    .into(),
            ));
        }
        let mut merged = cfg.unwrap_or_else(|| serde_json::json!({}));
        let map = merged.as_object_mut().ok_or_else(|| {
            EngineError::Other("~/.claude.json is not a JSON object — not switching.".into())
        })?;
        let (oauth_account, user_id) = match &slot.claude_config {
            Some(cc) => (cc.get("oauthAccount").cloned(), cc.get("userID").cloned()),
            None => (None, None),
        };
        map.insert(
            "oauthAccount".into(),
            oauth_account.unwrap_or_else(|| {
                serde_json::json!({
                    "accountUuid": slot.account_key,
                    "emailAddress": slot.profile.email,
                    "organizationName": slot.profile.organization,
                    "displayName": slot.profile.display_name,
                })
            }),
        );
        match user_id.filter(|v| v.is_string()) {
            Some(user_id) => {
                map.insert("userID".into(), user_id);
            }
            None => {
                map.remove("userID");
            }
        }

        // Validate and prepare the config merge before mutating the provider's
        // credential store. Slot owns the account login; live owns MCP/plugin
        // OAuth that lives in the same blob. A wholesale replace would restore
        // stale (or empty) mcpOAuth and force every MCP to re-auth.
        let (live, read_warning) = self.read_claude_credentials().await;
        if let Some(read_warning) = read_warning {
            return Err(EngineError::Other(format!(
                "Claude Code's existing credentials could not be read without user interaction, so LUNA did not replace them or risk losing shared MCP authentication. {read_warning}"
            )));
        }
        let credentials = compose_claude_credentials(&slot.credentials, live.as_ref());
        self.write_claude_credentials(&credentials).await?;

        // Atomic: Claude Code rewrites this file frequently — a torn write from
        // our side must never be readable as "empty config".
        write_file_atomic(file, merged.to_string().as_bytes(), false)
    }

    // ── forget ──────────────────────────────────────────────────────────────

    pub async fn forget(
        &self,
        harness: HarnessId,
        account_id: &str,
    ) -> Result<AgentAccountsSnapshot, EngineError> {
        if harness == HarnessId::Codex {
            let selected = lock(&self.inner.state).default_codex_account_id.clone();
            if selected != account_id {
                return Err(EngineError::Other(
                    "This Codex account is not connected in LUNA.".into(),
                ));
            }
            // Disconnect only LUNA's selection. Provider-owned credentials and
            // standalone CLI sessions are deliberately left untouched.
            self.update_state(|state| state.default_codex_account_id.clear())?;
            self.invalidate_caches(HarnessId::Codex);
            return self.list(false).await;
        }
        if harness != HarnessId::ClaudeCode || account_id != CLAUDE_NATIVE_ACCOUNT_ID {
            return Err(EngineError::Other(
                "LUNA will not delete provider credentials or Codex account overlays through this action."
                    .into(),
            ));
        }
        self.update_state(|state| state.claude_enabled = false)?;
        self.invalidate_caches(HarnessId::ClaudeCode);
        self.list(false).await
    }

    // ── add-account OAuth flows ─────────────────────────────────────────────

    pub async fn start_login(&self, harness: HarnessId) -> Result<AgentLoginStart, EngineError> {
        self.sweep_flows();
        match (harness).kind() {
            luna_proto::BuiltinHarnessId::ClaudeCode => Ok(self.start_claude_login()),
            luna_proto::BuiltinHarnessId::Codex => self.start_codex_login().await,
            other => Err(EngineError::Other(format!(
                "agent logins are not supported for {other:?}"
            ))),
        }
    }

    fn start_claude_login(&self) -> AgentLoginStart {
        let login_id = new_id();
        // PKCE: 32 random bytes (two v4 uuids) as the verifier, S256 challenge.
        let raw: Vec<u8> = uuid::Uuid::new_v4()
            .as_bytes()
            .iter()
            .chain(uuid::Uuid::new_v4().as_bytes())
            .copied()
            .collect();
        let verifier = BASE64_URL.encode(&raw);
        let challenge = BASE64_URL.encode(Sha256::digest(verifier.as_bytes()));
        let url = format!(
            "https://claude.ai/oauth/authorize?code=true&client_id={CLAUDE_CLIENT_ID}\
             &response_type=code&redirect_uri={}&scope={}&code_challenge={challenge}\
             &code_challenge_method=S256&state={verifier}",
            urlencode(CLAUDE_REDIRECT),
            urlencode(CLAUDE_SCOPES),
        );
        lock(&self.inner.flows).insert(
            login_id.clone(),
            LoginFlow::Claude {
                verifier,
                started_at: Instant::now(),
            },
        );
        AgentLoginStart {
            login_id,
            url,
            mode: AgentLoginMode::PasteCode,
        }
    }

    /// Supersede — and reap — any pending spawned flow for `harness` (codex:
    /// `codex login` binds a fixed loopback OAuth port, so a lingering flow
    /// makes every retry exit on EADDRINUSE; one flow is simply the
    /// sane state).
    fn reap_spawned_flows(&self, harness: HarnessId) {
        let stale: Vec<String> = lock(&self.inner.flows)
            .iter()
            .filter(|(_, f)| {
                matches!(
                    f,
                    LoginFlow::Spawned { harness: h, .. }
                        if *h == harness
                )
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in stale {
            self.cancel_login(&id);
        }
    }

    async fn start_codex_login(&self) -> Result<AgentLoginStart, EngineError> {
        self.reap_spawned_flows(HarnessId::Codex);
        if let Some(source) = incompatible_managed_codex_auth_store() {
            return Err(EngineError::Other(format!(
                "Named Codex accounts require cli_auth_credentials_store=\"file\", but {source} enforces a different credential store. LUNA did not start Codex or touch the provider login. The provider-native Codex account remains available."
            )));
        }
        let login_id = new_id();
        let account_id = new_id();
        // A named login gets a private auth overlay over the canonical Codex
        // session/config home. The provider CLI writes its own auth.json; LUNA
        // never sees or copies the token value into metadata.
        let home = self.inner.config.codex_shadow_home(&account_id);
        materialize_codex_shadow_home(&self.inner.config.codex_home, &home)?;
        let baseline_fingerprint = file_fingerprint(&home.join("auth.json"));
        // Resolve through the harness itself (`CODEX_EXECUTABLE`, PATH, the
        // login-shell snapshot, install dirs — the Windows npm payload
        // included) and compose the same child PATH a chat run gets, so
        // account login never diverges from what the harness can launch.
        let mut command = match luna_harness::codex::login_command(&home) {
            Ok(command) => command,
            Err(err) => {
                return Err(EngineError::Other(match err {
                    luna_harness::HarnessError::NotInstalled(hint) => {
                        format!(
                            "The `codex` CLI was not found on this device — install it first. ({hint})"
                        )
                    }
                    other => format!("Could not resolve the codex CLI for login: {other}"),
                }));
            }
        };
        command
            .stdin(luna_harness::process::Stdio::null())
            .stdout(luna_harness::process::Stdio::piped())
            .stderr(luna_harness::process::Stdio::piped());
        // The CLI opens the authorization tab itself (via the `webbrowser`
        // crate) AND the app opens the page when this start reply lands —
        // users got TWO identical auth.openai.com tabs. `webbrowser` prefers
        // $BROWSER over xdg-open, so a no-op script there keeps the CLI's
        // open quiet; a failed open is advisory to `codex login` (it prints
        // the URL and keeps serving the loopback callback either way).
        #[cfg(unix)]
        if let Some(noop_browser) = ensure_noop_browser(&self.inner.config.root_dir()) {
            command.env("BROWSER", noop_browser);
        }
        let child = match command.spawn() {
            Ok(child) => child,
            Err(err) => {
                return Err(EngineError::Other(
                    if err.kind() == std::io::ErrorKind::NotFound {
                        "The `codex` CLI was not found on this device — install it first.".into()
                    } else {
                        format!("Could not start codex login: {err}")
                    },
                ));
            }
        };

        // codex prints the authorize URL (to stderr as of 0.142 — scan both
        // streams); grab it so the app can open the single authorization tab
        // (the CLI's own browser-open is suppressed via BROWSER above).
        let (child, output, exit) = wire_login_child(child);
        lock(&self.inner.flows).insert(
            login_id.clone(),
            LoginFlow::Spawned {
                harness: HarnessId::Codex,
                account_id: Some(account_id),
                child,
                home,
                baseline_fingerprint,
                started_at: Instant::now(),
                output: output.clone(),
                exit: exit.clone(),
            },
        );
        let url = await_login_url(&output, &exit, scan_openai_url).await;
        Ok(AgentLoginStart {
            login_id,
            url,
            mode: AgentLoginMode::Browser,
        })
    }

    /// Exchange the pasted `code#state` and finish the explicit sign-in in
    /// Claude's provider-native credential store. LUNA retains no slot copy.
    pub async fn complete_login(
        &self,
        login_id: &str,
        code: &str,
    ) -> Result<AgentAccountsSnapshot, EngineError> {
        let verifier = match lock(&self.inner.flows).get(login_id) {
            Some(LoginFlow::Claude { verifier, .. }) => verifier.clone(),
            _ => {
                return Err(EngineError::Other(
                    "This sign-in attempt expired — start again.".into(),
                ));
            }
        };
        let (auth_code, state) = match code.trim().split_once('#') {
            Some((c, s)) => (c.to_string(), s.to_string()),
            None => (code.trim().to_string(), verifier.clone()),
        };
        if auth_code.is_empty() {
            return Err(EngineError::Other(
                "That code looks empty — paste the whole code.".into(),
            ));
        }
        let token = self
            .inner
            .http
            .post(CLAUDE_TOKEN_URL)
            .json(&serde_json::json!({
                "grant_type": "authorization_code",
                "code": auth_code,
                "state": state,
                "client_id": CLAUDE_CLIENT_ID,
                "redirect_uri": CLAUDE_REDIRECT,
                "code_verifier": verifier,
            }))
            .timeout(Duration::from_secs(15))
            .send()
            .await
            .map_err(|e| EngineError::Other(format!("token exchange failed: {e}")))?;
        if !token.status().is_success() {
            let status = token.status();
            return Err(EngineError::Other(format!(
                "Anthropic rejected the sign-in code ({status}); start the provider login again."
            )));
        }
        let token: serde_json::Value = token
            .json()
            .await
            .map_err(|e| EngineError::Other(format!("token exchange returned junk: {e}")))?;

        let access_token = str_field(&token, "access_token");
        let refresh_token = str_field(&token, "refresh_token");
        let expires_in = token
            .get("expires_in")
            .and_then(|v| v.as_i64())
            .unwrap_or(3600);
        let (Some(access_token), Some(refresh_token)) = (access_token, refresh_token) else {
            return Err(EngineError::Other(
                "Anthropic returned no usable tokens — try signing in again.".into(),
            ));
        };

        // Best-effort profile fetch — fills in the plan/org the way Claude Code does.
        let profile: Option<serde_json::Value> = match self
            .inner
            .http
            .get(CLAUDE_PROFILE_URL)
            .bearer_auth(&access_token)
            .header("anthropic-beta", "oauth-2025-04-20")
            .send()
            .await
        {
            Ok(res) if res.status().is_success() => res.json().await.ok(),
            _ => None,
        };
        let empty = serde_json::json!({});
        let p_account = profile
            .as_ref()
            .and_then(|p| p.get("account"))
            .unwrap_or(&empty);
        let p_org = profile
            .as_ref()
            .and_then(|p| p.get("organization"))
            .unwrap_or(&empty);
        let t_account = token.get("account").unwrap_or(&empty);
        let t_org = token.get("organization").unwrap_or(&empty);

        let email = str_field(p_account, "email_address")
            .or_else(|| str_field(t_account, "email_address"))
            .ok_or_else(|| {
                EngineError::Other("Could not identify the signed-in account.".into())
            })?;
        let account_uuid = str_field(p_account, "uuid")
            .or_else(|| str_field(t_account, "uuid"))
            .unwrap_or_else(|| email.clone());
        let org_name = str_field(p_org, "name").or_else(|| str_field(t_org, "name"));
        let org_type = str_field(p_org, "organization_type");
        let rate_tier = str_field(p_org, "rate_limit_tier");
        let display_name =
            str_field(p_account, "display_name").or_else(|| str_field(p_account, "full_name"));
        let subscription_type = match org_type.as_deref() {
            Some("claude_max") => Some("max"),
            Some("claude_pro") => Some("pro"),
            Some("claude_team") => Some("team"),
            Some("claude_enterprise") => Some("enterprise"),
            _ => None,
        };

        let scopes: Vec<String> = str_field(&token, "scope")
            .unwrap_or_else(|| CLAUDE_SCOPES.to_string())
            .split(' ')
            .map(str::to_string)
            .collect();
        let mut oauth = serde_json::json!({
            "accessToken": access_token,
            "refreshToken": refresh_token,
            "expiresAt": now_ms() + expires_in * 1000,
            "scopes": scopes,
        });
        if let (Some(sub), Some(map)) = (subscription_type, oauth.as_object_mut()) {
            map.insert("subscriptionType".into(), serde_json::json!(sub));
        }
        let mut oauth_account = serde_json::json!({
            "accountUuid": account_uuid,
            "emailAddress": email,
            "organizationUuid": str_field(p_org, "uuid").or_else(|| str_field(t_org, "uuid")),
            "organizationName": org_name,
            "displayName": display_name,
        });
        if let Some(map) = oauth_account.as_object_mut() {
            if let Some(t) = &org_type {
                map.insert("organizationType".into(), serde_json::json!(t));
            }
            if let Some(t) = &rate_tier {
                map.insert("organizationRateLimitTier".into(), serde_json::json!(t));
            }
        }

        let provider_login = Slot {
            id: slot_id_for(HarnessId::ClaudeCode, &account_uuid),
            account_key: account_uuid.clone(),
            profile: SlotProfile {
                email,
                display_name,
                organization: org_name,
                plan: claude_plan(org_type.as_deref(), rate_tier.as_deref()),
                auth_kind: AgentAuthKind::Oauth,
            },
            credentials: serde_json::json!({ "claudeAiOauth": oauth }),
            claude_config: Some(serde_json::json!({ "oauthAccount": oauth_account })),
        };
        self.activate_claude(&provider_login).await?;
        self.update_state(|state| state.claude_enabled = true)?;
        self.invalidate_caches(HarnessId::ClaudeCode);
        lock(&self.inner.flows).remove(login_id);
        self.list(false).await
    }

    pub fn login_harness(&self, login_id: &str) -> Option<HarnessId> {
        match lock(&self.inner.flows).get(login_id) {
            Some(LoginFlow::Claude { .. }) => Some(HarnessId::ClaudeCode),
            Some(LoginFlow::Spawned { harness, .. }) => Some(*harness),
            None => None,
        }
    }

    pub async fn poll_login(&self, login_id: &str) -> Result<AgentLoginPoll, EngineError> {
        self.sweep_flows();
        let (harness, account_id, home, baseline_fingerprint, exit, output) =
            match lock(&self.inner.flows).get(login_id) {
                None => {
                    return Err(EngineError::Other(
                        "This sign-in attempt expired — start again.".into(),
                    ));
                }
                Some(LoginFlow::Claude { .. }) => {
                    return Ok(AgentLoginPoll {
                        status: AgentLoginStatus::Pending,
                        message: None,
                        url: None,
                    });
                }
                Some(LoginFlow::Spawned {
                    harness,
                    account_id,
                    home,
                    baseline_fingerprint,
                    exit,
                    output,
                    ..
                }) => (
                    *harness,
                    account_id.clone(),
                    home.clone(),
                    *baseline_fingerprint,
                    exit.clone(),
                    output.clone(),
                ),
            };
        let auth_file = home.join("auth.json");
        let detected = read_json(&auth_file).and_then(|auth| match (harness).kind() {
            luna_proto::BuiltinHarnessId::Codex => parse_codex_auth(auth),
            _ => None,
        });
        if detected.is_some() && file_fingerprint(&auth_file) != baseline_fingerprint {
            if harness == HarnessId::Codex {
                self.finish_codex_login(
                    account_id.as_deref(),
                    detected.as_ref().expect("checked above"),
                )?;
            }
            self.cancel_login(login_id);
            return Ok(AgentLoginPoll {
                status: AgentLoginStatus::Done,
                message: None,
                url: None,
            });
        }
        let exited = *lock(&exit);
        if let Some(code) = exited {
            self.cancel_login(login_id);
            let message = if code == Some(0) && detected.is_some() {
                if harness == HarnessId::Codex {
                    self.finish_codex_login(
                        account_id.as_deref(),
                        detected.as_ref().expect("checked above"),
                    )?;
                }
                return Ok(AgentLoginPoll {
                    status: AgentLoginStatus::Done,
                    message: None,
                    url: None,
                });
            } else if code == Some(0) {
                "The sign-in finished without a usable provider-native login.".to_string()
            } else {
                let _ = output;
                format!(
                    "The provider sign-in exited before a usable native login was detected{}.",
                    code.map(|code| format!(" (exit {code})"))
                        .unwrap_or_default()
                )
            };
            return Ok(AgentLoginPoll {
                status: AgentLoginStatus::Error,
                message: Some(message),
                url: None,
            });
        }
        Ok(AgentLoginPoll {
            status: AgentLoginStatus::Pending,
            message: None,
            url: None,
        })
    }

    /// Drop a flow and kill its child. Provider-native credential homes are
    /// never removed by cancellation.
    pub fn cancel_login(&self, login_id: &str) {
        let flow = lock(&self.inner.flows).remove(login_id);
        match flow {
            Some(LoginFlow::Spawned { child, .. }) => {
                if let Some(c) = lock(&child).as_mut() {
                    let _ = c.start_kill();
                }
            }
            _ => {}
        }
    }

    /// Engine shutdown: kill any in-flight login child so an orphan `codex login`
    /// can't survive the restart and brick the next attempt.
    pub fn shutdown(&self) {
        let ids: Vec<String> = lock(&self.inner.flows).keys().cloned().collect();
        for id in ids {
            self.cancel_login(&id);
        }
    }

    /// Lazy TTL sweep (luna uses a background fiber; native reaps on the next
    /// accounts call — same bound, no standing task).
    fn sweep_flows(&self) {
        let stale: Vec<String> = lock(&self.inner.flows)
            .iter()
            .filter(|(_, f)| f.started_at().elapsed() > FLOW_TTL)
            .map(|(id, _)| id.clone())
            .collect();
        for id in stale {
            self.cancel_login(&id);
        }
    }

    // ── detection ───────────────────────────────────────────────────────────

    async fn detect_claude(&self) -> (Option<Detected>, Option<String>) {
        let cfg = read_json(&self.inner.config.claude_config_file);
        let Some(oauth) = cfg.as_ref().and_then(|c| c.get("oauthAccount")).cloned() else {
            return (None, None);
        };
        let Some(email) = str_field(&oauth, "emailAddress") else {
            return (None, None);
        };
        let (credentials, warning) = self.read_claude_credentials().await;
        (
            Some(Detected {
                account_key: str_field(&oauth, "accountUuid").unwrap_or_else(|| email.clone()),
                profile: SlotProfile {
                    email,
                    display_name: str_field(&oauth, "displayName"),
                    organization: str_field(&oauth, "organizationName"),
                    plan: claude_plan(
                        str_field(&oauth, "organizationType").as_deref(),
                        str_field(&oauth, "organizationRateLimitTier").as_deref(),
                    ),
                    auth_kind: AgentAuthKind::Oauth,
                },
                credentials,
            }),
            warning,
        )
    }

    fn detect_codex(&self) -> Option<Detected> {
        read_json(&self.inner.config.codex_auth_file()).and_then(parse_codex_auth)
    }

    /// Filename-only legacy detection. Never deserialize historical slot files:
    /// their JSON contains provider-native access and refresh tokens.
    fn legacy_slots_present(&self, harness: HarnessId) -> bool {
        std::fs::read_dir(self.inner.config.root_dir().join(harness_slug(harness)))
            .ok()
            .into_iter()
            .flatten()
            .flatten()
            .any(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
    }

    fn legacy_login_dirs_present(&self) -> bool {
        std::fs::read_dir(self.inner.config.root_dir())
            .ok()
            .into_iter()
            .flatten()
            .flatten()
            .any(|entry| {
                entry.file_type().is_ok_and(|kind| kind.is_dir())
                    && entry.file_name().to_string_lossy().starts_with(".login-")
            })
    }

    // ── Claude provider-owned credentials file ───────────────────────────────

    /// An absent file is normal for provider-managed authentication. Never
    /// inspect Keychain or verify credentials merely to discover an account.
    async fn read_claude_credentials(&self) -> (Option<serde_json::Value>, Option<String>) {
        (read_json(&self.inner.config.claude_creds_file()), None)
    }

    async fn write_claude_credentials(
        &self,
        credentials: &serde_json::Value,
    ) -> Result<(), EngineError> {
        // Only the explicit sign-in flow writes this provider-owned file.
        let mut json = credentials.to_string();
        std::fs::create_dir_all(&self.inner.config.claude_config_dir)?;
        let result = write_file_atomic(
            &self.inner.config.claude_creds_file(),
            json.as_bytes(),
            true,
        );
        json.zeroize();
        result
    }

    // ── remaining usage ─────────────────────────────────────────────────────

    /// Ask Claude's native control channel, including native credential renewal.
    /// LUNA never extracts Keychain secrets or starts a model turn for usage.
    pub async fn read_claude_usage_limit(&self) -> luna_proto::UsageLimitSnapshot {
        use luna_proto::{LimitAvailability, UsageLimitSnapshot, UsageProvider};
        let account = read_json(&self.inner.config.claude_config_file);
        let oauth = account.as_ref().and_then(|v| v.get("oauthAccount"));
        let account_key = oauth
            .and_then(|v| str_field(v, "accountUuid").or_else(|| str_field(v, "emailAddress")));
        let verified_id = account_key
            .as_deref()
            .map(|key| format!("claude:{:x}", Sha256::digest(key.as_bytes())));
        let account_label = oauth.and_then(|v| str_field(v, "emailAddress"));
        let plan = oauth.and_then(|v| {
            claude_plan(
                str_field(v, "organizationType").as_deref(),
                str_field(v, "organizationRateLimitTier").as_deref(),
            )
        });
        let unavailable = |availability, detail: &str| UsageLimitSnapshot {
            provider: UsageProvider::Claude,
            verified_account_id: verified_id.clone(),
            account_label: account_label.clone(),
            plan: plan.clone(),
            checked_at_ms: None,
            availability,
            windows: Vec::new(),
            reset_credits: None,
            detail: Some(detail.into()),
        };
        if !lock(&self.inner.state).claude_enabled
            || account_key.is_none()
            || self
                .inner
                .config
                .claude_config_dir
                .join(DISABLE_CLAUDE_KEYCHAIN_FALLBACK)
                .is_file()
        {
            return unavailable(
                LimitAvailability::Unsupported,
                "No selected native Claude account; LUNA does not access Keychain directly",
            );
        }
        let mut harness = luna_harness::claude::ClaudeHarness::new();
        if self.inner.config.claude_config_dir != home_dir().join(".claude")
            || self.inner.config.claude_config_file != home_dir().join(".claude.json")
        {
            harness = harness.with_config_dir(&self.inner.config.claude_config_dir);
        }
        let response = match harness.subscription_usage().await {
            Ok(value) => value,
            Err(_) => {
                return unavailable(
                    LimitAvailability::ProbeFailed,
                    "Claude's native usage read did not complete",
                );
            }
        };
        let current = read_json(&self.inner.config.claude_config_file);
        let current_key = current
            .as_ref()
            .and_then(|v| v.get("oauthAccount"))
            .and_then(|v| str_field(v, "accountUuid").or_else(|| str_field(v, "emailAddress")));
        let native_email = response
            .get("account")
            .and_then(|v| v.get("email"))
            .and_then(serde_json::Value::as_str);
        if current_key != account_key
            || native_email
                .zip(account_label.as_deref())
                .is_some_and(|(a, b)| !a.eq_ignore_ascii_case(b))
        {
            return unavailable(
                LimitAvailability::Unverified,
                "Claude account changed during the native usage read",
            );
        }
        let usage = &response["usage"];
        if usage["rate_limits_available"] != true {
            return unavailable(
                if plan.is_some() {
                    LimitAvailability::ProbeFailed
                } else {
                    LimitAvailability::Unsupported
                },
                "Claude has not reported subscription windows",
            );
        }
        crate::usage::live_claude_limit(&usage["rate_limits"], verified_id.clone(), plan.clone())
            .map(|mut limit| {
                limit.account_label = account_label.clone();
                limit.detail = Some("Claude native get_usage".into());
                limit
            })
            .unwrap_or_else(|| {
                unavailable(
                    LimitAvailability::ProbeFailed,
                    "Claude's native usage response had no valid windows",
                )
            })
    }

    async fn usage_for(
        &self,
        harness: HarnessId,
        slot: &Slot,
        _is_active: bool,
        force: bool,
    ) -> Option<UsageSnapshot> {
        let key = format!("{}:{}", harness_slug(harness), slot.account_key);
        if let Some((usage, _at)) = lock(&self.inner.usage_cache).get(&key)
            && !force
        {
            return usage.clone();
        }
        if !force {
            // Non-forced lists never hit the network (see module docs).
            return None;
        }
        let usage = match (harness).kind() {
            luna_proto::BuiltinHarnessId::ClaudeCode => self.claude_usage(slot).await,
            luna_proto::BuiltinHarnessId::Codex => self.codex_usage(slot).await,
            _ => None,
        };
        let mut cache = lock(&self.inner.usage_cache);
        let retained = usage.or_else(|| cache.get(&key).and_then(|(value, _)| value.clone()));
        cache.insert(key, (retained.clone(), Instant::now()));
        retained
    }

    async fn claude_usage(&self, slot: &Slot) -> Option<UsageSnapshot> {
        let oauth = slot.credentials.get("claudeAiOauth")?;
        let access_token = str_field(oauth, "accessToken")?;
        self.claude_usage_request(&access_token)
            .await
            .ok()
            .flatten()
    }

    /// One usage probe: GET the endpoint and parse windows. `Err` means the
    /// token was rejected (401/403) — the only case worth a refresh.
    async fn claude_usage_request(&self, access_token: &str) -> Result<Option<UsageSnapshot>, ()> {
        self.claude_usage_response(access_token)
            .await
            .map(|body| claude_usage_windows(&body))
    }

    async fn claude_usage_response(&self, access_token: &str) -> Result<serde_json::Value, ()> {
        let response = self
            .inner
            .http
            .get(CLAUDE_USAGE_URL)
            .bearer_auth(access_token)
            .header("anthropic-beta", "oauth-2025-04-20")
            .send()
            .await
            .map_err(|_| ())?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED
            || response.status() == reqwest::StatusCode::FORBIDDEN
        {
            return Err(());
        }
        let body: serde_json::Value = response
            .error_for_status()
            .map_err(|_| ())?
            .json()
            .await
            .map_err(|_| ())?;
        Ok(body)
    }

    async fn codex_usage(&self, slot: &Slot) -> Option<UsageSnapshot> {
        let tokens = slot.credentials.get("tokens")?;
        // api-key mode has no ChatGPT rate windows.
        let access_token = str_field(tokens, "access_token")?;
        let body: serde_json::Value = self
            .inner
            .http
            .get(CODEX_USAGE_URL)
            .bearer_auth(&access_token)
            .header(
                "chatgpt-account-id",
                str_field(tokens, "account_id").unwrap_or_default(),
            )
            .send()
            .await
            .ok()?
            .error_for_status()
            .ok()?
            .json()
            .await
            .ok()?;
        let rl = body.get("rate_limit")?;
        let mut windows = Vec::new();
        for key in ["primary_window", "secondary_window"] {
            if let Some(w) = rl.get(key)
                && let Some(used) = w.get("used_percent").and_then(|v| v.as_f64())
            {
                let span = w
                    .get("limit_window_seconds")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);
                windows.push(AgentUsageWindow {
                    label: codex_window_label(span).to_string(),
                    used_fraction: (used / 100.0) as f32,
                    resets_at: parse_when(w.get("reset_at")),
                });
            }
        }
        if windows.is_empty() {
            return None;
        }
        // Live plan ("free"/"plus"/"pro"…) — beats the login-time JWT claim,
        // so a plan change shows up on the next forced refresh without a
        // re-login.
        let plan_label = codex_plan(str_field(&body, "plan_type").as_deref());
        Some(UsageSnapshot {
            windows,
            plan_label,
        })
    }
}

fn configured_auth_store(requirements: &str) -> Option<String> {
    requirements.lines().find_map(|line| {
        let line = line.split('#').next()?.trim();
        let (key, value) = line.split_once('=')?;
        (key.trim() == "cli_auth_credentials_store").then(|| {
            value
                .trim()
                .trim_matches(|character| character == '"' || character == '\'')
                .to_ascii_lowercase()
        })
    })
}

fn auth_store_from_file(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|contents| configured_auth_store(&contents))
}

#[cfg(target_os = "macos")]
fn auth_store_from_macos_preference(key: &str) -> Option<String> {
    // These are the two documented Codex MDM TOML payloads. `defaults read`
    // only reads managed preferences; it never loads provider credentials.
    let output = std::process::Command::new("/usr/bin/defaults")
        .args(["read", "com.openai.codex", key])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let encoded = std::str::from_utf8(&output.stdout).ok()?.trim();
    let decoded = BASE64.decode(encoded).ok()?;
    configured_auth_store(std::str::from_utf8(&decoded).ok()?)
}

fn incompatible_managed_codex_auth_store() -> Option<String> {
    let system_requirements = Path::new("/etc/codex/requirements.toml");
    let system_managed = Path::new("/etc/codex/managed_config.toml");
    let mut requirement = auth_store_from_file(system_requirements)
        .map(|store| (store, system_requirements.display().to_string()));
    let mut managed_default = auth_store_from_file(system_managed)
        .map(|store| (store, system_managed.display().to_string()));

    #[cfg(target_os = "macos")]
    {
        if let Some(store) = auth_store_from_macos_preference("requirements_toml_base64") {
            requirement = Some((store, "the managed macOS Codex requirements".into()));
        }
        if let Some(store) = auth_store_from_macos_preference("config_toml_base64") {
            managed_default = Some((store, "the managed macOS Codex defaults".into()));
        }
    }

    // An explicit requirement wins over managed defaults. With no requirement,
    // managed_config/MDM defaults outrank the per-process `-c` override.
    incompatible_auth_store_source(requirement, managed_default)
}

fn incompatible_auth_store_source(
    requirement: Option<(String, String)>,
    managed_default: Option<(String, String)>,
) -> Option<String> {
    match requirement.or(managed_default) {
        Some((store, source)) if store != "file" => Some(source),
        _ => None,
    }
}

// ── helpers ─────────────────────────────────────────────────────────────────

fn harness_slug(harness: HarnessId) -> String {
    match harness.kind() {
        luna_proto::BuiltinHarnessId::Other => format!(
            "provider-{}",
            &crate::repos::hex(&Sha256::digest(harness.canonical().as_str().as_bytes()))[..24]
        ),
        _ => harness
            .canonical()
            .as_str()
            .split_once('/')
            .expect("canonical provider")
            .1
            .to_string(),
    }
}

fn slot_from_detected(id: &str, detected: &Detected) -> Slot {
    Slot {
        id: id.to_string(),
        account_key: detected.account_key.clone(),
        profile: detected.profile.clone(),
        credentials: detected
            .credentials
            .clone()
            .unwrap_or_else(|| serde_json::json!({})),
        claude_config: None,
    }
}

fn account_from_slot(
    harness: HarnessId,
    slot: Slot,
    usage: Option<UsageSnapshot>,
    active: bool,
    switchable: bool,
    authentication: Option<AgentAuthenticationState>,
    saved_at: Option<i64>,
    label: Option<String>,
) -> AgentAccount {
    let credentials_available = slot
        .credentials
        .as_object()
        .is_some_and(|credentials| !credentials.is_empty());
    AgentAccount {
        id: slot.id,
        harness,
        label,
        email: Some(slot.profile.email.clone()),
        authentication: authentication.unwrap_or(if credentials_available {
            AgentAuthenticationState::Authenticated
        } else {
            AgentAuthenticationState::Unavailable
        }),
        plan_label: usage
            .as_ref()
            .and_then(|usage| usage.plan_label.clone())
            .or(slot.profile.plan),
        active,
        usage_windows: usage.map(|usage| usage.windows).unwrap_or_default(),
        display_name: slot.profile.display_name,
        organization: slot.profile.organization,
        auth_kind: Some(slot.profile.auth_kind),
        switchable,
        saved_at,
    }
}

fn load_account_state(config: &AgentAccountsConfig) -> ProviderAccountState {
    let file = config.state_file();
    let Ok(bytes) = std::fs::read(&file) else {
        return ProviderAccountState::default();
    };
    match serde_json::from_slice::<ProviderAccountState>(&bytes) {
        Ok(mut state) if state.schema_version == PROVIDER_ACCOUNT_STATE_VERSION => {
            state
                .codex_accounts
                .retain(|record| safe_account_id(&record.id));
            if !state.default_codex_account_id.is_empty()
                && state.default_codex_account_id != CODEX_NATIVE_ACCOUNT_ID
                && !state
                    .codex_accounts
                    .iter()
                    .any(|record| record.id == state.default_codex_account_id)
            {
                state.default_codex_account_id = native_codex_account();
            }
            state
        }
        Ok(state) => {
            tracing::warn!(
                version = state.schema_version,
                path = %file.display(),
                "ignoring unsupported provider-account metadata"
            );
            ProviderAccountState::default()
        }
        Err(error) => {
            tracing::warn!(%error, path = %file.display(), "provider-account metadata is unreadable");
            ProviderAccountState::default()
        }
    }
}

fn persist_account_state(
    config: &AgentAccountsConfig,
    state: &ProviderAccountState,
) -> Result<(), EngineError> {
    std::fs::create_dir_all(&config.data_dir)?;
    let bytes = serde_json::to_vec_pretty(state)
        .map_err(|error| EngineError::Other(format!("account metadata encode failed: {error}")))?;
    write_file_atomic(&config.state_file(), &bytes, true)
}

fn materialize_codex_shadow_home(shared: &Path, shadow: &Path) -> Result<(), EngineError> {
    if shared == shadow {
        return Err(EngineError::Other(
            "Codex auth overlay must differ from the canonical session home.".into(),
        ));
    }
    std::fs::create_dir_all(shared)?;
    std::fs::create_dir_all(shadow)?;
    for directory in CODEX_SHARED_DIRECTORIES {
        std::fs::create_dir_all(shared.join(directory))?;
    }

    for private in CODEX_PRIVATE_ENTRIES {
        let path = shadow.join(private);
        if path
            .symlink_metadata()
            .is_ok_and(|meta| meta.file_type().is_symlink())
        {
            if *private == "auth.json" {
                return Err(EngineError::Other(format!(
                    "Codex shadow auth must be a private file, not a symlink: {}",
                    path.display()
                )));
            }
            std::fs::remove_file(&path)?;
        }
    }

    let mut entries = CODEX_SHARED_DIRECTORIES
        .iter()
        .map(|entry| (*entry).to_string())
        .collect::<std::collections::BTreeSet<_>>();
    for entry in std::fs::read_dir(shared)? {
        let name = entry?.file_name().to_string_lossy().to_string();
        if !CODEX_PRIVATE_ENTRIES.contains(&name.as_str())
            && !CODEX_SHADOW_LOCAL_ENTRIES.contains(&name.as_str())
        {
            entries.insert(name);
        }
    }
    for entry in entries {
        ensure_codex_shared_link(shared, shadow, &entry)?;
    }
    Ok(())
}

fn ensure_codex_shared_link(shared: &Path, shadow: &Path, entry: &str) -> Result<(), EngineError> {
    let target = shared.join(entry);
    let link = shadow.join(entry);
    match link.symlink_metadata() {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            let current = std::fs::read_link(&link)?;
            let current = if current.is_absolute() {
                current
            } else {
                shadow.join(current)
            };
            if current == target {
                return Ok(());
            }
            std::fs::remove_file(&link)?;
        }
        Ok(_) if entry == "mcp-oauth-locks" => {
            if link.is_dir() {
                std::fs::remove_dir_all(&link)?;
            } else {
                std::fs::remove_file(&link)?;
            }
        }
        Ok(_) => {
            return Err(EngineError::Other(format!(
                "Cannot share Codex home entry `{entry}` because {} already exists and is not a symlink.",
                link.display()
            )));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    create_symlink(&target, &link)
}

#[cfg(unix)]
fn create_symlink(target: &Path, link: &Path) -> Result<(), EngineError> {
    std::os::unix::fs::symlink(target, link)?;
    Ok(())
}

#[cfg(windows)]
fn create_symlink(target: &Path, link: &Path) -> Result<(), EngineError> {
    if target.is_dir() {
        std::os::windows::fs::symlink_dir(target, link)?;
    } else {
        std::os::windows::fs::symlink_file(target, link)?;
    }
    Ok(())
}

fn read_json(file: &Path) -> Option<serde_json::Value> {
    let mut raw = std::fs::read(file).ok()?;
    let parsed = serde_json::from_slice(&raw)
        .ok()
        .filter(serde_json::Value::is_object);
    raw.zeroize();
    parsed
}

fn file_fingerprint(file: &Path) -> Option<[u8; 32]> {
    let mut bytes = std::fs::read(file).ok()?;
    let fingerprint = Sha256::digest(&bytes).into();
    bytes.zeroize();
    Some(fingerprint)
}

fn str_field(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Decode a JWT payload without verifying — we only mine identity claims from a
/// token the user's own CLI already trusts.
fn jwt_claims(jwt: &str) -> Option<serde_json::Value> {
    let payload = jwt.split('.').nth(1)?;
    let bytes = BASE64_URL
        .decode(payload)
        .or_else(|_| BASE64.decode(payload))
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn slot_id_for(harness: HarnessId, account_key: &str) -> String {
    let digest = Sha256::digest(format!("{}:{account_key}", harness_slug(harness)).as_bytes());
    crate::repos::hex(&digest)[..16].to_string()
}

/// Pretty plan label from Claude's org type + rate-limit tier ("Max 20×").
fn claude_plan(org_type: Option<&str>, tier: Option<&str>) -> Option<String> {
    let base = match org_type {
        Some("claude_max") => "Max",
        Some("claude_pro") => "Pro",
        Some("claude_team") => "Team",
        Some("claude_enterprise") => "Enterprise",
        _ => return None,
    };
    // "…_20x" style tiers carry a multiplier suffix.
    let mult = tier.and_then(|t| {
        let stem = t.strip_suffix('x')?;
        let digits: String = stem
            .chars()
            .rev()
            .take_while(char::is_ascii_digit)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let preceded = stem.len() > digits.len()
            && stem.as_bytes().get(stem.len() - digits.len() - 1) == Some(&b'_');
        (!digits.is_empty() && preceded).then_some(digits)
    });
    Some(match mult {
        Some(mult) => format!("{base} {mult}×"),
        None => base.to_string(),
    })
}

fn codex_plan(plan: Option<&str>) -> Option<String> {
    let plan = plan?;
    let mut chars = plan.chars();
    let first = chars.next()?;
    Some(format!(
        "ChatGPT {}{}",
        first.to_uppercase(),
        chars.as_str()
    ))
}

/// Meter label for a Codex rate-limit window from its `limit_window_seconds`:
/// the free tier's window is a 30-day month (2_592_000s), Plus runs a 5-hour
/// primary (~18_000s) with a weekly secondary (604_800s). A bare "> 1 day =
/// week" rule mislabeled the monthly window "Week"; thresholds in seconds
/// leave the middle gaps to the nearest label rather than guessing a plan.
fn codex_window_label(span_seconds: i64) -> &'static str {
    const DAY: i64 = 86_400;
    if span_seconds >= 28 * DAY {
        "Month"
    } else if span_seconds >= 5 * DAY {
        "Week"
    } else {
        "Session"
    }
}

/// Compose an explicitly completed Claude login with the machine's current
/// provider-owned shared fields.
///
/// `claudeAiOauth` (and any other slot-owned sibling, including
/// `trustedDeviceToken`) come from the newly authenticated target. Allowlisted shared keys come
/// from `live`, presence and absence alike — a key the live blob no longer
/// holds is not resurrected from historical state. When there is no live JSON object
/// (or the target is not a Claude OAuth blob), `target` is returned unchanged.
fn compose_claude_credentials(
    target: &serde_json::Value,
    live: Option<&serde_json::Value>,
) -> serde_json::Value {
    let Some(live_obj) = live.and_then(|v| v.as_object()) else {
        return target.clone();
    };
    let Some(target_obj) = target.as_object() else {
        return target.clone();
    };
    if !target_obj.contains_key("claudeAiOauth") {
        return target.clone();
    }
    let unrecognized: Vec<&str> = live_obj
        .keys()
        .map(String::as_str)
        .filter(|key| {
            *key != "claudeAiOauth"
                && *key != "trustedDeviceToken"
                && !CLAUDE_SHARED_CREDENTIAL_KEYS.contains(key)
        })
        .collect();
    if !unrecognized.is_empty() {
        tracing::debug!(
            keys = ?unrecognized,
            "live Claude credentials have sibling keys we don't recognize; treating them as slot-owned"
        );
    }
    let mut composed = serde_json::Map::new();
    for (key, value) in target_obj {
        if !CLAUDE_SHARED_CREDENTIAL_KEYS.contains(&key.as_str()) {
            composed.insert(key.clone(), value.clone());
        }
    }
    for key in CLAUDE_SHARED_CREDENTIAL_KEYS {
        if let Some(value) = live_obj.get(*key) {
            composed.insert((*key).to_string(), value.clone());
        }
    }
    serde_json::Value::Object(composed)
}

/// Parse a codex `auth.json` (the live one or a fresh login's).
fn parse_codex_auth(auth: serde_json::Value) -> Option<Detected> {
    if let Some(id_token) = auth
        .get("tokens")
        .and_then(|t| t.get("id_token"))
        .and_then(|v| v.as_str())
    {
        let claims = jwt_claims(id_token).unwrap_or_else(|| serde_json::json!({}));
        let oa = claims
            .get("https://api.openai.com/auth")
            .cloned()
            .unwrap_or_default();
        let email = str_field(&claims, "email")?;
        return Some(Detected {
            account_key: str_field(&oa, "chatgpt_account_id").unwrap_or_else(|| email.clone()),
            profile: SlotProfile {
                email,
                display_name: str_field(&claims, "name"),
                organization: None,
                plan: codex_plan(str_field(&oa, "chatgpt_plan_type").as_deref()),
                auth_kind: AgentAuthKind::Oauth,
            },
            credentials: Some(auth),
        });
    }
    let api_key = str_field(&auth, "OPENAI_API_KEY")?;
    let digest = Sha256::digest(api_key.as_bytes());
    let tail: String = api_key
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    Some(Detected {
        account_key: format!("api-key:{}", &crate::repos::hex(&digest)[..12]),
        profile: SlotProfile {
            email: format!("API key ·…{tail}"),
            display_name: None,
            organization: None,
            plan: Some("API key".into()),
            auth_kind: AgentAuthKind::ApiKey,
        },
        credentials: Some(auth),
    })
}

/// ISO string (Claude) or unix seconds (Codex) → timestamp.
fn parse_when(value: Option<&serde_json::Value>) -> Option<DateTime<Utc>> {
    match value? {
        serde_json::Value::Number(n) => DateTime::<Utc>::from_timestamp(n.as_i64()?, 0),
        serde_json::Value::String(s) => DateTime::parse_from_rfc3339(s)
            .ok()
            .map(|t| t.with_timezone(&Utc)),
        _ => None,
    }
}

/// Windows from Claude's `/api/oauth/usage`: the 5-hour session and weekly
/// buckets, each a 0-100 `utilization` with an RFC3339 `resets_at`.
fn claude_usage_windows(body: &serde_json::Value) -> Option<UsageSnapshot> {
    let mut windows = Vec::new();
    for (key, label) in [("five_hour", "Session"), ("seven_day", "Week")] {
        if let Some(w) = body.get(key)
            && let Some(utilization) = w.get("utilization").and_then(|v| v.as_f64())
        {
            windows.push(AgentUsageWindow {
                label: label.to_string(),
                used_fraction: (utilization / 100.0) as f32,
                resets_at: parse_when(w.get("resets_at")),
            });
        }
    }
    (!windows.is_empty()).then_some(UsageSnapshot {
        windows,
        plan_label: None,
    })
}

fn scan_openai_url(output: &str) -> Option<String> {
    let start = output.find("https://auth.openai.com/")?;
    let rest = &output[start..];
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    Some(rest[..end].to_string())
}

/// Path of the no-op "browser" script `start_codex_login` hands the CLI via
/// `BROWSER` so `codex login` doesn't open a second authorization tab (the
/// app opens the one tab). Unix only — `webbrowser` only consults `BROWSER`
/// on unix; elsewhere the CLI's own open is left as-is.
#[cfg(unix)]
fn ensure_noop_browser(root: &Path) -> Option<PathBuf> {
    const SCRIPT: &str = "#!/bin/sh\nexit 0\n";
    let path = root.join(".noop-browser");
    if std::fs::read_to_string(&path).ok().as_deref() != Some(SCRIPT) {
        std::fs::write(&path, SCRIPT).ok()?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).ok()?;
    }
    Some(path)
}

type LoginChildHandles = (
    Arc<Mutex<Option<luna_harness::process::Child>>>,
    Arc<Mutex<String>>,
    Arc<Mutex<Option<Option<i32>>>>,
);

/// Wire a spawned login child: both pipes accumulate into one output buffer
/// (the URL can land on either stream), and a monitor polls `try_wait` so the
/// child is reaped without owning it — the cancel path needs concurrent kill
/// access.
fn wire_login_child(mut child: luna_harness::process::Child) -> LoginChildHandles {
    let output = Arc::new(Mutex::new(String::new()));
    for pipe in [
        child
            .stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Send + Unpin>),
        child
            .stderr
            .take()
            .map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Send + Unpin>),
    ]
    .into_iter()
    .flatten()
    {
        let sink = output.clone();
        tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            let mut pipe = pipe;
            let mut buf = [0u8; 4096];
            while let Ok(n) = pipe.read(&mut buf).await {
                if n == 0 {
                    break;
                }
                lock(&sink).push_str(&String::from_utf8_lossy(&buf[..n]));
            }
        });
    }
    let child = Arc::new(Mutex::new(Some(child)));
    let exit: Arc<Mutex<Option<Option<i32>>>> = Arc::new(Mutex::new(None));
    {
        let child = child.clone();
        let exit = exit.clone();
        tokio::spawn(async move {
            loop {
                {
                    let mut slot = lock(&child);
                    match slot.as_mut().map(|c| c.try_wait()) {
                        None => break,
                        Some(Ok(Some(status))) => {
                            *lock(&exit) = Some(status.code());
                            *slot = None;
                            break;
                        }
                        Some(Ok(None)) => {}
                        Some(Err(_)) => {
                            *lock(&exit) = Some(None);
                            *slot = None;
                            break;
                        }
                    }
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        });
    }
    (child, output, exit)
}

/// Wait briefly for the login child to print its authorize URL (empty when it
/// exits or stays silent past the deadline — the flow still completes via
/// poll; the UI just can't offer an open-browser button).
async fn await_login_url(
    output: &Arc<Mutex<String>>,
    exit: &Arc<Mutex<Option<Option<i32>>>>,
    scan: fn(&str) -> Option<String>,
) -> String {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(url) = scan(&lock(output)) {
            break url;
        }
        if lock(exit).is_some() || Instant::now() > deadline {
            break String::new();
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Minimal percent-encoding for OAuth query params (matches `encodeURIComponent`
/// for the constant inputs used here).
fn urlencode(input: &str) -> String {
    let mut out = String::with_capacity(input.len() * 3);
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'!'
            | b'~'
            | b'*'
            | b'\''
            | b'('
            | b')' => out.push(byte as char),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Atomic write via a same-dir temp file + rename; `secret` = 0600 from birth.
fn write_file_atomic(file: &Path, bytes: &[u8], secret: bool) -> Result<(), EngineError> {
    let tmp = file.with_extension(format!("tmp-{}", std::process::id()));
    {
        use std::io::Write;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        if secret {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        #[cfg(not(unix))]
        let _ = secret;
        let mut handle = options.open(&tmp)?;
        handle.write_all(bytes)?;
    }
    std::fs::rename(&tmp, file)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config(root: &Path) -> AgentAccountsConfig {
        AgentAccountsConfig {
            data_dir: root.join("luna"),
            claude_config_dir: root.join("claude"),
            claude_config_file: root.join("claude.json"),
            codex_home: root.join("codex"),
        }
    }

    #[test]
    fn inventory_home_resolution_does_not_materialize_named_account() {
        let root = tempfile::tempdir().unwrap();
        let config = test_config(root.path());
        let accounts = AgentAccounts::new(config.clone());
        assert_eq!(
            accounts.codex_home_for_inventory().unwrap(),
            config.codex_home
        );
        assert_eq!(
            accounts.codex_usage_target().unwrap(),
            (config.codex_home.clone(), false)
        );
        {
            let mut state = lock(&accounts.inner.state);
            state.default_codex_account_id = "named".into();
            state.codex_accounts.push(CodexAccountRecord {
                id: "named".into(),
                label: "Named".into(),
                account_key: "synthetic".into(),
                profile: SlotProfile {
                    email: "synthetic@example.test".into(),
                    display_name: None,
                    organization: None,
                    plan: None,
                    auth_kind: AgentAuthKind::Oauth,
                },
                created_at: 0,
            });
        }
        let home = accounts.codex_home_for_inventory().unwrap();
        assert_eq!(home, config.codex_shadow_home("named"));
        assert!(!home.exists());
        assert_eq!(accounts.codex_usage_target().unwrap(), (home, true));
    }

    #[tokio::test]
    async fn claude_disconnect_persists_without_touching_native_login() {
        let root = tempfile::tempdir().unwrap();
        let config = test_config(root.path());
        std::fs::create_dir_all(&config.claude_config_dir).unwrap();
        std::fs::write(
            &config.claude_config_file,
            serde_json::json!({
                "oauthAccount": {
                    "accountUuid": "synthetic-account",
                    "emailAddress": "claude@example.test"
                }
            })
            .to_string(),
        )
        .unwrap();
        let credentials = serde_json::json!({
            "claudeAiOauth": {
                "accessToken": "synthetic-access-token",
                "refreshToken": "synthetic-refresh-token"
            }
        });
        std::fs::write(config.claude_creds_file(), credentials.to_string()).unwrap();

        let accounts = AgentAccounts::new(config.clone());
        accounts
            .forget(HarnessId::ClaudeCode, CLAUDE_NATIVE_ACCOUNT_ID)
            .await
            .unwrap();
        assert_eq!(
            read_json(&config.claude_creds_file()).unwrap(),
            credentials,
            "disconnect must not log out, delete, or rewrite provider credentials"
        );
        assert!(
            accounts
                .resolve_execution(HarnessId::ClaudeCode, None)
                .unwrap_err()
                .to_string()
                .contains("disconnected")
        );

        let reloaded = AgentAccounts::new(config.clone());
        assert!(
            reloaded
                .resolve_execution(HarnessId::ClaudeCode, None)
                .is_err(),
            "native rediscovery must not reconnect a LUNA-disabled account"
        );
        reloaded
            .activate(HarnessId::ClaudeCode, CLAUDE_NATIVE_ACCOUNT_ID)
            .await
            .unwrap();
        assert_eq!(
            reloaded
                .resolve_execution(HarnessId::ClaudeCode, None)
                .unwrap()
                .account_id,
            CLAUDE_NATIVE_ACCOUNT_ID
        );
        let metadata = std::fs::read_to_string(config.state_file()).unwrap();
        assert!(!metadata.contains("synthetic-access-token"));
        assert!(!metadata.contains("synthetic-refresh-token"));
    }

    #[tokio::test]
    async fn codex_scoped_listing_excludes_other_provider_discovery() {
        let root = tempfile::tempdir().unwrap();
        let config = test_config(root.path());
        std::fs::create_dir_all(&config.codex_home).unwrap();
        std::fs::write(
            config.codex_auth_file(),
            serde_json::json!({"OPENAI_API_KEY": "synthetic-codex-key"}).to_string(),
        )
        .unwrap();
        std::fs::create_dir_all(&config.claude_config_dir).unwrap();
        std::fs::write(
            &config.claude_config_file,
            serde_json::json!({
                "oauthAccount": {
                    "accountUuid": "synthetic-claude",
                    "emailAddress": "claude@example.test"
                }
            })
            .to_string(),
        )
        .unwrap();
        // Keep the fixture hermetic even if this contract regresses: the file
        // wins over macOS Keychain in Claude discovery.
        std::fs::write(
            config.claude_creds_file(),
            serde_json::json!({"claudeAiOauth": {"accessToken": "fixture-only"}}).to_string(),
        )
        .unwrap();

        let snapshot = AgentAccounts::new(config)
            .list_harness(HarnessId::Codex, false)
            .await
            .unwrap();
        assert!(!snapshot.accounts.is_empty());
        assert!(
            snapshot
                .accounts
                .iter()
                .all(|account| account.harness == HarnessId::Codex)
        );
        assert!(
            snapshot
                .warnings
                .iter()
                .all(|warning| warning.harness == HarnessId::Codex)
        );
    }

    #[test]
    fn native_identity_changes_rotate_execution_auth_identity() {
        let root = tempfile::tempdir().unwrap();
        let config = test_config(root.path());
        std::fs::create_dir_all(&config.codex_home).unwrap();
        std::fs::write(
            config.codex_auth_file(),
            serde_json::json!({"OPENAI_API_KEY": "synthetic-codex-a"}).to_string(),
        )
        .unwrap();
        std::fs::write(
            &config.claude_config_file,
            serde_json::json!({
                "oauthAccount": {
                    "accountUuid": "synthetic-claude-a",
                    "emailAddress": "a@example.test"
                }
            })
            .to_string(),
        )
        .unwrap();
        let accounts = AgentAccounts::new(config.clone());
        let codex_a = accounts
            .resolve_execution(HarnessId::Codex, Some(CODEX_NATIVE_ACCOUNT_ID))
            .unwrap();
        let claude_a = accounts
            .resolve_execution(HarnessId::ClaudeCode, None)
            .unwrap();

        std::fs::write(
            config.codex_auth_file(),
            serde_json::json!({"OPENAI_API_KEY": "synthetic-codex-b"}).to_string(),
        )
        .unwrap();
        std::fs::write(
            &config.claude_config_file,
            serde_json::json!({
                "oauthAccount": {
                    "accountUuid": "synthetic-claude-b",
                    "emailAddress": "b@example.test"
                }
            })
            .to_string(),
        )
        .unwrap();
        let codex_b = accounts
            .resolve_execution(HarnessId::Codex, Some(CODEX_NATIVE_ACCOUNT_ID))
            .unwrap();
        let claude_b = accounts
            .resolve_execution(HarnessId::ClaudeCode, None)
            .unwrap();
        assert_eq!(codex_a.account_id, codex_b.account_id);
        assert_ne!(codex_a.auth_instance_id, codex_b.auth_instance_id);
        assert_eq!(claude_a.account_id, claude_b.account_id);
        assert_ne!(claude_a.auth_instance_id, claude_b.auth_instance_id);
    }

    #[tokio::test]
    async fn codex_label_rename_roundtrips_through_persisted_metadata() {
        let root = tempfile::tempdir().unwrap();
        let config = test_config(root.path());
        std::fs::create_dir_all(&config.codex_home).unwrap();
        let account_id = "named-label";
        let shadow = config.codex_shadow_home(account_id);
        materialize_codex_shadow_home(&config.codex_home, &shadow).unwrap();
        std::fs::write(
            shadow.join("auth.json"),
            serde_json::json!({"OPENAI_API_KEY": "synthetic-only"}).to_string(),
        )
        .unwrap();
        let accounts = AgentAccounts::new(config.clone());
        accounts
            .update_state(|state| {
                state.codex_accounts.push(CodexAccountRecord {
                    id: account_id.into(),
                    label: "Work".into(),
                    account_key: "synthetic-key-id".into(),
                    profile: SlotProfile {
                        email: "provider@example.test".into(),
                        display_name: Some("Provider Name".into()),
                        organization: None,
                        plan: None,
                        auth_kind: AgentAuthKind::ApiKey,
                    },
                    created_at: 1,
                });
            })
            .unwrap();
        accounts
            .rename(HarnessId::Codex, account_id, "Personal")
            .await
            .unwrap();

        let reloaded = AgentAccounts::new(config);
        let snapshot = reloaded
            .list_harness(HarnessId::Codex, false)
            .await
            .unwrap();
        let account = snapshot
            .accounts
            .iter()
            .find(|account| account.id == account_id)
            .unwrap();
        assert_eq!(account.label.as_deref(), Some("Personal"));
        assert_ne!(account.email.as_deref(), account.label.as_deref());
    }

    #[test]
    fn managed_codex_auth_store_parser_only_accepts_file() {
        assert_eq!(
            configured_auth_store("cli_auth_credentials_store = \"keyring\""),
            Some("keyring".into())
        );
        assert_eq!(
            configured_auth_store("# managed\ncli_auth_credentials_store='file'"),
            Some("file".into())
        );
        assert_eq!(
            configured_auth_store("allowed_login_methods=['chatgpt']"),
            None
        );
        assert_eq!(
            incompatible_auth_store_source(
                None,
                Some(("keyring".into(), "managed defaults".into()))
            )
            .as_deref(),
            Some("managed defaults")
        );
        assert_eq!(
            incompatible_auth_store_source(
                Some(("file".into(), "requirements".into())),
                Some(("keyring".into(), "managed defaults".into()))
            ),
            None,
            "a file requirement makes a conflicting default compatible"
        );
    }

    #[tokio::test]
    async fn provider_managed_native_codex_remains_available_without_auth_json() {
        let root = tempfile::tempdir().unwrap();
        let config = test_config(root.path());
        std::fs::create_dir_all(&config.codex_home).unwrap();
        let accounts = AgentAccounts::new(config.clone());

        let execution = accounts
            .resolve_execution(HarnessId::Codex, Some(CODEX_NATIVE_ACCOUNT_ID))
            .unwrap();
        assert_eq!(
            execution.codex_home.as_deref(),
            Some(config.codex_home.as_path())
        );
        assert!(!execution.codex_file_auth);

        let snapshot = accounts
            .list_harness(HarnessId::Codex, false)
            .await
            .unwrap();
        let native = snapshot
            .accounts
            .iter()
            .find(|account| account.id == CODEX_NATIVE_ACCOUNT_ID)
            .unwrap();
        assert_eq!(native.authentication, AgentAuthenticationState::Unknown);
        assert!(snapshot.warnings.iter().any(|warning| {
            warning.harness == HarnessId::Codex
                && warning
                    .message
                    .contains("provider-managed OS credential store")
        }));
    }

    #[cfg(unix)]
    #[test]
    fn codex_named_accounts_share_sessions_but_keep_auth_private() {
        let root = tempfile::tempdir().unwrap();
        let config = test_config(root.path());
        std::fs::create_dir_all(&config.codex_home).unwrap();
        std::fs::write(
            config.codex_auth_file(),
            serde_json::json!({"OPENAI_API_KEY": "synthetic-native-key"}).to_string(),
        )
        .unwrap();
        let shared_config =
            "model = 'test'\n[features.context_management]\nexperimental_mode = true\n";
        std::fs::write(config.codex_home.join("config.toml"), shared_config).unwrap();
        std::fs::write(config.codex_home.join("models_cache.json"), "{}").unwrap();

        let account_id = "named-synthetic";
        let shadow = config.codex_shadow_home(account_id);
        materialize_codex_shadow_home(&config.codex_home, &shadow).unwrap();
        assert!(
            shadow
                .join("sessions")
                .symlink_metadata()
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(
            shadow
                .join("config.toml")
                .symlink_metadata()
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            std::fs::read_to_string(shadow.join("config.toml")).unwrap(),
            shared_config,
            "named-account execution must inherit non-private Codex configuration"
        );
        assert!(!shadow.join("models_cache.json").exists());
        assert!(!shadow.join("auth.json").exists());
        std::fs::write(
            shadow.join("auth.json"),
            serde_json::json!({"OPENAI_API_KEY": "synthetic-named-key"}).to_string(),
        )
        .unwrap();
        assert!(
            !shadow
                .join("auth.json")
                .symlink_metadata()
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            read_json(&config.codex_auth_file()).unwrap()["OPENAI_API_KEY"],
            "synthetic-native-key"
        );

        let accounts = AgentAccounts::new(config.clone());
        accounts
            .update_state(|state| {
                state.codex_accounts.push(CodexAccountRecord {
                    id: account_id.into(),
                    label: "Work".into(),
                    account_key: "synthetic-key-id".into(),
                    profile: SlotProfile {
                        email: "Named test account".into(),
                        display_name: None,
                        organization: None,
                        plan: Some("API key".into()),
                        auth_kind: AgentAuthKind::ApiKey,
                    },
                    created_at: 1,
                });
            })
            .unwrap();
        let native = accounts
            .resolve_execution(HarnessId::Codex, Some(CODEX_NATIVE_ACCOUNT_ID))
            .unwrap();
        let named = accounts
            .resolve_execution(HarnessId::Codex, Some(account_id))
            .unwrap();
        assert_eq!(
            native.codex_home.as_deref(),
            Some(config.codex_home.as_path())
        );
        assert_eq!(named.codex_home.as_deref(), Some(shadow.as_path()));
        assert!(!native.codex_file_auth);
        assert!(named.codex_file_auth);
        assert_ne!(native.auth_instance_id, named.auth_instance_id);
        let metadata = std::fs::read_to_string(config.state_file()).unwrap();
        assert!(!metadata.contains("synthetic-native-key"));
        assert!(!metadata.contains("synthetic-named-key"));
    }

    #[cfg(unix)]
    #[test]
    fn codex_shadow_rejects_symlinked_private_auth() {
        let root = tempfile::tempdir().unwrap();
        let config = test_config(root.path());
        std::fs::create_dir_all(&config.codex_home).unwrap();
        std::fs::write(config.codex_auth_file(), "{}").unwrap();
        let shadow = config.codex_shadow_home("bad-auth");
        std::fs::create_dir_all(&shadow).unwrap();
        std::os::unix::fs::symlink(config.codex_auth_file(), shadow.join("auth.json")).unwrap();
        let error = materialize_codex_shadow_home(&config.codex_home, &shadow).unwrap_err();
        assert!(error.to_string().contains("private file"));
    }

    #[test]
    fn plan_labels() {
        assert_eq!(
            claude_plan(Some("claude_max"), Some("default_claude_max_20x")).as_deref(),
            Some("Max 20×")
        );
        assert_eq!(
            claude_plan(Some("claude_pro"), None).as_deref(),
            Some("Pro")
        );
        assert_eq!(
            claude_plan(Some("claude_team"), Some("weird")).as_deref(),
            Some("Team")
        );
        assert_eq!(claude_plan(Some("free"), None), None);
        assert_eq!(codex_plan(Some("plus")).as_deref(), Some("ChatGPT Plus"));
        assert_eq!(codex_plan(Some("free")).as_deref(), Some("ChatGPT Free"));
        assert_eq!(codex_plan(None), None);
    }

    #[test]
    fn codex_window_labels_track_the_window_span() {
        // Codex free tier: one 30-day window (observed live:
        // limit_window_seconds = 2_592_000) — NOT a week.
        assert_eq!(codex_window_label(2_592_000), "Month");
        // Plus: 5-hour primary + weekly secondary.
        assert_eq!(codex_window_label(18_000), "Session");
        assert_eq!(codex_window_label(604_800), "Week");
        // Unknown/absent span falls back to the shortest label.
        assert_eq!(codex_window_label(0), "Session");
    }

    #[test]
    fn claude_usage_windows_map_buckets_and_percent() {
        let body = serde_json::json!({
            "five_hour": { "utilization": 6.0, "resets_at": "2026-04-08T18:59:59Z" },
            "seven_day": { "utilization": 35.0, "resets_at": "2026-04-14T16:59:59Z" },
            "extra_usage": { "is_enabled": true },
        });
        let snapshot = claude_usage_windows(&body).expect("windows");
        assert_eq!(snapshot.plan_label, None);
        let labels: Vec<_> = snapshot.windows.iter().map(|w| w.label.as_str()).collect();
        assert_eq!(labels, ["Session", "Week"]);
        assert!((snapshot.windows[0].used_fraction - 0.06).abs() < 1e-6);
        assert!((snapshot.windows[1].used_fraction - 0.35).abs() < 1e-6);
        assert_eq!(
            snapshot.windows[1].resets_at,
            Some(
                "2026-04-14T16:59:59Z"
                    .parse::<chrono::DateTime<Utc>>()
                    .unwrap()
            )
        );
    }

    #[test]
    fn claude_usage_windows_none_without_any_bucket() {
        // A 200 with only `extra_usage` (no rate windows) is not a snapshot —
        // the UI then says "Usage unavailable" instead of showing nothing.
        assert!(claude_usage_windows(&serde_json::json!({ "extra_usage": {} })).is_none());
        assert!(claude_usage_windows(&serde_json::json!({ "five_hour": {} })).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn noop_browser_script_is_stable_and_executable() {
        let root = tempfile::tempdir().expect("tempdir");
        let path = ensure_noop_browser(root.path()).expect("script");
        assert!(path.ends_with(".noop-browser"));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "#!/bin/sh\nexit 0\n"
        );
        {
            use std::os::unix::fs::PermissionsExt;
            assert!(path.metadata().unwrap().permissions().mode() & 0o111 != 0);
        }
        // A second ensure is idempotent (same path, same content).
        assert_eq!(ensure_noop_browser(root.path()), Some(path));
    }

    #[test]
    fn urlencode_matches_encode_uri_component() {
        assert_eq!(
            urlencode("org:create_api_key user:profile"),
            "org%3Acreate_api_key%20user%3Aprofile"
        );
        assert_eq!(urlencode("https://a/b"), "https%3A%2F%2Fa%2Fb");
    }

    #[test]
    fn compose_claude_credentials_live_mcp_wins_over_stale_slot() {
        let target = serde_json::json!({
            "claudeAiOauth": { "accessToken": "alice" },
            "trustedDeviceToken": "alice-device",
            "mcpOAuth": { "github": { "accessToken": "stale" } },
            "pluginSecrets": { "old": true },
        });
        let live = serde_json::json!({
            "claudeAiOauth": { "accessToken": "bob" },
            "trustedDeviceToken": "bob-device",
            "mcpOAuth": { "github": { "accessToken": "live" } },
            "pluginSecrets": { "live": true },
        });
        let composed = compose_claude_credentials(&target, Some(&live));
        assert_eq!(composed["claudeAiOauth"]["accessToken"], "alice");
        assert_eq!(composed["trustedDeviceToken"], "alice-device");
        assert_eq!(composed["mcpOAuth"]["github"]["accessToken"], "live");
        assert_eq!(composed["pluginSecrets"]["live"], true);
        assert!(composed["pluginSecrets"].get("old").is_none());
    }

    #[test]
    fn compose_claude_credentials_does_not_resurrect_absent_live_mcp() {
        let target = serde_json::json!({
            "claudeAiOauth": { "accessToken": "alice" },
            "mcpOAuth": { "github": { "accessToken": "stale" } },
        });
        let live = serde_json::json!({
            "claudeAiOauth": { "accessToken": "bob" },
        });
        let composed = compose_claude_credentials(&target, Some(&live));
        assert_eq!(composed["claudeAiOauth"]["accessToken"], "alice");
        assert!(composed.get("mcpOAuth").is_none());
    }

    #[test]
    fn compose_claude_credentials_passthrough_without_live_or_oauth() {
        let target = serde_json::json!({
            "claudeAiOauth": { "accessToken": "alice" },
            "mcpOAuth": { "github": { "accessToken": "slot" } },
        });
        assert_eq!(compose_claude_credentials(&target, None), target);

        let api_key = serde_json::json!({ "apiKey": "sk-x" });
        let live = serde_json::json!({
            "claudeAiOauth": { "accessToken": "bob" },
            "mcpOAuth": { "github": { "accessToken": "live" } },
        });
        assert_eq!(
            compose_claude_credentials(&api_key, Some(&live)),
            api_key,
            "opaque/API-key shapes activate verbatim"
        );
    }

    #[tokio::test]
    async fn claude_usage_without_credentials_file_never_falls_back_to_keychain() {
        let root = tempfile::tempdir().unwrap();
        let config = test_config(root.path());
        assert!(!config.claude_creds_file().exists());
        assert!(
            !config
                .claude_config_dir
                .join(DISABLE_CLAUDE_KEYCHAIN_FALLBACK)
                .exists()
        );
        let observed = AgentAccounts::new(config).read_claude_usage_limit().await;
        assert_eq!(observed.provider, luna_proto::UsageProvider::Claude);
        assert_eq!(
            observed.availability,
            luna_proto::LimitAvailability::Unsupported
        );
        assert!(observed.windows.is_empty());
        assert!(
            observed
                .detail
                .as_deref()
                .unwrap()
                .contains("does not access Keychain")
        );
    }
}
