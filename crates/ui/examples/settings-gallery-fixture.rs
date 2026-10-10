//! Settings gallery: every Settings page of the production `Shell`, captured
//! at the verification matrix (Interface size × window width × theme) against
//! an in-process synthetic engine. The E2E evidence for Settings polish
//! (`docs/design/settings-polish-2026-10-06/spec.md` §6).
//!
//! Build: scripts/cargo.sh build --locked -p luna-ui --features appshots-fixture \
//!          --example settings-gallery-fixture
//! Run:   target/debug/examples/settings-gallery-fixture ABSOLUTE_OUT_DIR \
//!          [--pages providers,general,…] [--sizes 12,16,20] [--widths 900,1400] \
//!          [--themes dark,light] [--height 4000]
//! Writes `{page}-{theme}-{size}px-{width}w.png`, `manifest.json` (every
//! capture, its parameters and sha256), an `index.html` contact sheet and the
//! `settings-gallery.pass` receipt. A failure writes `failure.txt` and exits
//! nonzero; a panic inside a page records the capture it happened in.
//!
//! Synthetic and offline: a temporary profile, UI and engine data dir; the
//! Claude Keychain fallback disabled; no provider CLI registered; an in-memory
//! RPC transport whose provider surface (accounts, CLI versions, models,
//! usage) is answered here and whose logins, CLI updates and org calls are
//! refused; a pre-seeded update status so Updates never fetches the release
//! feed; hidden, unfocused windows only.
//!
//! How a capture could lie, and what this fixture does about it:
//! - Captured before data loads: every RPC is counted; a capture waits until
//!   none is in flight for 250 ms and the page's own readiness facts hold
//!   (accounts, CLI versions, usage, automations, update status…). A page
//!   that never gets there fails the run.
//! - An empty or loading page: per-page facts assert the seeded rows reached
//!   the page; the content column must not be blank in the PNG.
//! - Interface size not applied: the window rem and the theme's interface
//!   size are read back and must equal the requested size.
//! - Mid-animation: a frame is kept only when two renders 120 ms apart are
//!   pixel-identical; otherwise it is flagged `unsettled` after 8 tries.
//! - Stale entities across sizes: every (width, size) gets a fresh window and
//!   Shell, so page entities are first built at the size being shown.
//! - Hover or focus artefacts: the pointer is parked on empty content space.
//! - A page cut off by the window: content reaching the bottom edge is
//!   flagged `touchesBottom` (rerun with a larger `--height`).
//! - A network check from Updates: its "check attempted" flag must stay off.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{TimeDelta, Utc};
use gpui::{
    AppContext, AsyncApp, Bounds, PlatformInput, WindowBounds, WindowHandle, WindowOptions, point,
    px, size,
};
use luna_engine::{
    AgentAccountsConfig, Auth, AuthConfig, EngineCore, EngineProfile, HarnessDescriptor,
    HarnessRegistry,
};
use luna_proto::{
    AgentAccount, AgentAccountsSnapshot, AgentAuthKind, AgentAuthenticationState, AgentUsageWindow,
    HarnessId, LimitAvailability, Model, ModelOption, ModelOptionChoice, PermissionMode,
    ProjectAction, ProjectCheckout, ProjectProviderDefault, ProviderUpdateStatus, ReasoningLevel,
    UsageBucket, UsageLimitSnapshot, UsageLimitWindow, UsageProvider, UsageSnapshot, UsageSource,
    UsageSourceStatus, UsageSummary, UsageTokens,
};
use luna_rpc::{RpcError, RpcReply, methods};
use luna_ui::shell::{SettingsSection, Shell};
use luna_ui::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// Page slug → section, in `SettingsSection::ALL` order.
const PAGES: [(&str, SettingsSection); 14] = [
    ("general", SettingsSection::General),
    ("projects", SettingsSection::Projects),
    ("providers", SettingsSection::Providers),
    ("appearance", SettingsSection::Appearance),
    ("notifications", SettingsSection::Notifications),
    ("shortcuts", SettingsSection::Shortcuts),
    ("files", SettingsSection::Files),
    ("appshots", SettingsSection::Appshots),
    ("automations", SettingsSection::Automations),
    ("usage", SettingsSection::Usage),
    ("updates", SettingsSection::Updates),
    ("devices", SettingsSection::Devices),
    ("backups", SettingsSection::Backups),
    ("archived", SettingsSection::Archived),
];

const PROJECT: &str = "gallery-luna";
const DOCS_PROJECT: &str = "gallery-docs";
const OFFICE: &str = "5b1f0c3e-7a2d-4e8b-9c41-0f6d2a8e7b13";
const PHONE: &str = "c8e4a2d0-1b9f-4c3a-8e57-2d6f9a0b4c71";
/// Logical margin of the inset content panel's frame, excluded from the
/// content scan (its border is not page content).
const PANEL_FRAME: f32 = 20.0;
const SETTLE_TRIES: usize = 8;
const READY_TIMEOUT: Duration = Duration::from_secs(20);
const QUIET: Duration = Duration::from_millis(250);

struct ChatSpec {
    id: &'static str,
    project: &'static str,
    title: &'static str,
    hours_ago: i64,
    archived: bool,
    /// Owned by the offline Office Mini instead of this device.
    remote: bool,
}

const CHATS: [ChatSpec; 7] = [
    ChatSpec {
        id: "a0a0a0a0-0000-4000-8000-000000000001",
        project: PROJECT,
        title: "Release plan for the v0.6 cut",
        hours_ago: 1,
        archived: false,
        remote: false,
    },
    ChatSpec {
        id: "a0a0a0a0-0000-4000-8000-000000000002",
        project: PROJECT,
        title: "Fix the flaky workbench drag test",
        hours_ago: 5,
        archived: false,
        remote: false,
    },
    ChatSpec {
        id: "a0a0a0a0-0000-4000-8000-000000000003",
        project: DOCS_PROJECT,
        title: "Rewrite the keyboard shortcuts guide",
        hours_ago: 26,
        archived: false,
        remote: false,
    },
    ChatSpec {
        id: "a0a0a0a0-0000-4000-8000-000000000004",
        project: PROJECT,
        title: "Investigate usage meter rounding at 99.5%",
        hours_ago: 72,
        archived: true,
        remote: false,
    },
    ChatSpec {
        id: "a0a0a0a0-0000-4000-8000-000000000005",
        project: PROJECT,
        title: "Migrate the model cache to the v3 layout",
        hours_ago: 240,
        archived: true,
        remote: false,
    },
    ChatSpec {
        id: "a0a0a0a0-0000-4000-8000-000000000006",
        project: DOCS_PROJECT,
        title: "Review the onboarding copy with a much longer title that has to wrap politely",
        hours_ago: 500,
        archived: true,
        remote: true,
    },
    ChatSpec {
        id: "a0a0a0a0-0000-4000-8000-000000000007",
        project: PROJECT,
        title: "Nightly dependency audit",
        hours_ago: 9,
        archived: false,
        remote: false,
    },
];

type Win = WindowHandle<Shell>;

// ---------- matrix ----------

struct Matrix {
    pages: Vec<(&'static str, SettingsSection)>,
    sizes: Vec<typography::UiFontSize>,
    widths: Vec<f32>,
    themes: Vec<(&'static str, appearance::AppearanceMode)>,
    height: f32,
}

fn usage() -> String {
    "usage: settings-gallery-fixture ABSOLUTE_OUT_DIR [--pages providers,general,…] \
     [--sizes 12,16,20] [--widths 900,1400] [--themes dark,light] [--height 4000]"
        .into()
}

fn parse_args() -> anyhow::Result<(PathBuf, Matrix)> {
    let mut args = std::env::args().skip(1);
    let output = PathBuf::from(args.next().ok_or_else(|| anyhow::anyhow!(usage()))?);
    anyhow::ensure!(
        output.is_absolute(),
        "the output directory must be absolute\n{}",
        usage()
    );
    let mut matrix = Matrix {
        pages: PAGES.to_vec(),
        sizes: [12.0, 16.0, 20.0]
            .into_iter()
            .map(|pixels| font_size(pixels).expect("matrix size"))
            .collect(),
        widths: vec![900.0, 1400.0],
        themes: vec![
            ("dark", appearance::AppearanceMode::Dark),
            ("light", appearance::AppearanceMode::Light),
        ],
        height: 4000.0,
    };
    while let Some(flag) = args.next() {
        let value = args
            .next()
            .ok_or_else(|| anyhow::anyhow!("{flag} needs a value\n{}", usage()))?;
        let list = || {
            value
                .split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
        };
        match flag.as_str() {
            "--pages" => {
                matrix.pages = list()
                    .map(|slug| {
                        PAGES
                            .into_iter()
                            .find(|(known, _)| *known == slug)
                            .ok_or_else(|| anyhow::anyhow!("unknown page {slug:?}"))
                    })
                    .collect::<anyhow::Result<_>>()?
            }
            "--sizes" => {
                matrix.sizes = list()
                    .map(|item| {
                        let pixels: f32 = item.parse()?;
                        font_size(pixels).ok_or_else(|| {
                            anyhow::anyhow!(
                                "Interface size {item} is not one of 12,13,14,15,16,18,20"
                            )
                        })
                    })
                    .collect::<anyhow::Result<_>>()?
            }
            "--widths" => {
                matrix.widths = list()
                    .map(|item| item.parse::<f32>().map_err(anyhow::Error::from))
                    .collect::<anyhow::Result<_>>()?
            }
            "--themes" => {
                matrix.themes = list()
                    .map(|item| match item {
                        "dark" => Ok(("dark", appearance::AppearanceMode::Dark)),
                        "light" => Ok(("light", appearance::AppearanceMode::Light)),
                        other => Err(anyhow::anyhow!("unknown theme {other:?} (dark, light)")),
                    })
                    .collect::<anyhow::Result<_>>()?
            }
            "--height" => matrix.height = value.parse()?,
            other => anyhow::bail!("unknown flag {other}\n{}", usage()),
        }
    }
    anyhow::ensure!(
        !matrix.pages.is_empty()
            && !matrix.sizes.is_empty()
            && !matrix.widths.is_empty()
            && !matrix.themes.is_empty(),
        "the matrix is empty\n{}",
        usage()
    );
    Ok((output, matrix))
}

/// The installed version with its last component bumped (`0.2.73` -> `0.2.74`).
fn next_version(current: &str) -> String {
    let mut parts: Vec<String> = current.split('.').map(str::to_owned).collect();
    if let Some(last) = parts.last_mut()
        && let Ok(number) = last.parse::<u64>()
    {
        *last = (number + 1).to_string();
    }
    parts.join(".")
}

fn font_size(pixels: f32) -> Option<typography::UiFontSize> {
    typography::UiFontSize::ALL
        .into_iter()
        .find(|size| size.pixels() == pixels)
}

// ---------- synthetic provider transport ----------

/// Every RPC the UI makes, counted, so a capture can wait for quiet.
#[derive(Default)]
struct RpcLog {
    in_flight: AtomicUsize,
    last_activity: Mutex<Option<Instant>>,
    served: Mutex<BTreeMap<String, u64>>,
    refused: Mutex<BTreeMap<String, u64>>,
    failed: Mutex<BTreeMap<String, String>>,
}

impl RpcLog {
    fn touch(&self) {
        *self.last_activity.lock().unwrap() = Some(Instant::now());
    }

    /// How long no RPC has been in flight (`None`: one is in flight now).
    fn quiet_for(&self) -> Option<Duration> {
        if self.in_flight.load(Ordering::SeqCst) > 0 {
            return None;
        }
        Some(
            self.last_activity
                .lock()
                .unwrap()
                .map_or(Duration::MAX, |at| at.elapsed()),
        )
    }

    fn count(&self, method: &str) -> u64 {
        self.served
            .lock()
            .unwrap()
            .get(method)
            .copied()
            .unwrap_or(0)
    }
}

/// Decrements the in-flight count even when the caller drops the request.
struct InFlight<'a>(&'a RpcLog);

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        self.0.in_flight.fetch_sub(1, Ordering::SeqCst);
        self.0.touch();
    }
}

/// Methods that would sign in, update a CLI, spawn a provider runtime or
/// reach an org service. Answered with an error and listed in the manifest.
const REFUSED: &[&str] = &[
    methods::UPDATE_PROVIDER,
    methods::SET_HARNESS_ENABLED,
    methods::ACTIVATE_AGENT_ACCOUNT,
    methods::RENAME_AGENT_ACCOUNT,
    methods::FORGET_AGENT_ACCOUNT,
    methods::START_AGENT_LOGIN,
    methods::COMPLETE_AGENT_LOGIN,
    methods::POLL_AGENT_LOGIN,
    methods::CANCEL_AGENT_LOGIN,
    methods::LIST_COMPOSER_CATALOG,
    methods::GET_INTEGRATION_INVENTORY,
    methods::SIGN_IN,
    methods::SIGN_OUT,
    methods::LIST_ORGS,
    methods::SELECT_ORG,
    methods::CREATE_ORG,
    methods::STOP_ENGINE,
    methods::EXPORT_BACKUP,
    methods::STAGE_BACKUP_RESTORE,
];

struct GalleryRpc {
    inner: Arc<dyn luna_rpc::RpcService>,
    local_device: String,
    log: Arc<RpcLog>,
}

#[async_trait::async_trait]
impl luna_rpc::RpcService for GalleryRpc {
    async fn handle(&self, method: &str, params: Value) -> Result<RpcReply, RpcError> {
        self.log.in_flight.fetch_add(1, Ordering::SeqCst);
        self.log.touch();
        let _guard = InFlight(&self.log);
        if REFUSED.contains(&method) {
            *self
                .log
                .refused
                .lock()
                .unwrap()
                .entry(method.to_owned())
                .or_default() += 1;
            return Err(RpcError::Failed(format!(
                "settings gallery: {method} is unavailable in the synthetic provider transport"
            )));
        }
        let result = match method {
            methods::LIST_AGENT_ACCOUNTS => RpcReply::value(&synthetic_accounts()),
            methods::LIST_HARNESSES => RpcReply::value(&synthetic_harnesses()),
            methods::GET_PROVIDER_UPDATES => RpcReply::value(&synthetic_provider_updates()),
            methods::LIST_MODELS => {
                let harness: HarnessId = serde_json::from_value(params["harness"].clone())
                    .map_err(|error| RpcError::BadParams(error.to_string()))?;
                RpcReply::value(&synthetic_models(harness))
            }
            methods::GET_USAGE_SNAPSHOT => {
                RpcReply::value(&synthetic_usage(&params, &self.local_device))
            }
            _ => self.inner.handle(method, params).await,
        };
        *self
            .log
            .served
            .lock()
            .unwrap()
            .entry(method.to_owned())
            .or_default() += 1;
        if let Err(error) = &result {
            self.log
                .failed
                .lock()
                .unwrap()
                .insert(method.to_owned(), error.to_string());
        }
        result
    }
}

fn synthetic_accounts() -> AgentAccountsSnapshot {
    let now = Utc::now();
    let windows = |five_hour: f32, weekly: f32| {
        vec![
            AgentUsageWindow {
                label: "5h".into(),
                used_fraction: five_hour,
                resets_at: Some(now + TimeDelta::minutes(134)),
            },
            AgentUsageWindow {
                label: "Weekly".into(),
                used_fraction: weekly,
                resets_at: Some(now + TimeDelta::hours(3 * 24 + 7)),
            },
        ]
    };
    let account = |id: &str,
                   harness: HarnessId,
                   label: Option<&str>,
                   email: &str,
                   plan: &str,
                   active: bool,
                   usage_windows: Vec<AgentUsageWindow>| AgentAccount {
        id: id.into(),
        harness,
        label: label.map(Into::into),
        email: Some(email.into()),
        plan_label: Some(plan.into()),
        active,
        authentication: AgentAuthenticationState::Authenticated,
        usage_windows,
        display_name: None,
        organization: None,
        auth_kind: Some(AgentAuthKind::Oauth),
        switchable: !active,
        saved_at: Some(now.timestamp_millis() - 40 * 24 * 60 * 60_000),
    };
    AgentAccountsSnapshot {
        accounts: vec![
            account(
                "gallery-codex-work",
                HarnessId::Codex,
                Some("Work"),
                "iulian@studio.example.test",
                "ChatGPT Pro",
                true,
                windows(0.42, 0.68),
            ),
            account(
                "gallery-codex-personal",
                HarnessId::Codex,
                Some("Personal"),
                "iulian.personal@example.test",
                "ChatGPT Plus",
                false,
                windows(0.03, 0.12),
            ),
            account(
                "gallery-claude",
                HarnessId::ClaudeCode,
                None,
                "iulian@claude.example.test",
                "Max",
                true,
                windows(0.23, 0.51),
            ),
        ],
        warnings: vec![],
    }
}

/// Codex and Claude Code enabled; OpenCode installed but switched off.
fn synthetic_harnesses() -> Vec<HarnessDescriptor> {
    [
        (HarnessId::Codex, "Codex", true),
        (HarnessId::ClaudeCode, "Claude Code", true),
        (HarnessId::Opencode, "OpenCode", false),
    ]
    .into_iter()
    .map(|(id, name, enabled)| HarnessDescriptor {
        id,
        name: name.into(),
        supports_steering: true,
        steering_mode: luna_proto::SteeringMode::TurnBoundary,
        reasoning_levels: synthetic_models(id)
            .first()
            .map(|model| model.reasoning_levels.clone())
            .unwrap_or_default(),
        installed: true,
        enabled: Some(enabled),
    })
    .collect()
}

/// Codex has a newer CLI available; the others are current.
fn synthetic_provider_updates() -> Vec<ProviderUpdateStatus> {
    vec![
        ProviderUpdateStatus {
            harness: HarnessId::Codex,
            installed_version: Some("0.160.0".into()),
            latest_version: Some("0.162.1".into()),
            update_available: true,
            update_command: Some("brew upgrade --cask codex".into()),
            detail: None,
        },
        ProviderUpdateStatus {
            harness: HarnessId::ClaudeCode,
            installed_version: Some("2.1.291".into()),
            latest_version: Some("2.1.291".into()),
            update_available: false,
            update_command: Some("claude update".into()),
            detail: None,
        },
        ProviderUpdateStatus {
            harness: HarnessId::Opencode,
            installed_version: Some("2.0.14".into()),
            latest_version: Some("2.0.14".into()),
            update_available: false,
            update_command: Some("opencode upgrade".into()),
            detail: None,
        },
    ]
}

fn synthetic_models(harness: HarnessId) -> Vec<Model> {
    use ReasoningLevel::*;
    let model = |id: &str, label: &str, description: &str, levels: &[ReasoningLevel]| Model {
        id: id.into(),
        label: label.into(),
        description: Some(description.into()),
        reasoning_levels: levels.to_vec(),
        options: vec![],
        first_seen_at_ms: None,
    };
    match harness {
        HarnessId::Codex => {
            let speed = ModelOption {
                id: "serviceTier".into(),
                label: "Speed".into(),
                choices: vec![
                    ModelOptionChoice {
                        id: "default".into(),
                        label: "Standard".into(),
                    },
                    ModelOptionChoice {
                        id: "fast".into(),
                        label: "Fast".into(),
                    },
                ],
                default_choice: "default".into(),
            };
            let mut models = vec![
                model(
                    "gpt-5.5-codex",
                    "GPT-5.5 Codex",
                    "Tuned for long agentic coding sessions",
                    &[Low, Medium, High, XHigh],
                ),
                model(
                    "gpt-5.5",
                    "GPT-5.5",
                    "General reasoning model",
                    &[Minimal, Low, Medium, High, XHigh],
                ),
                model(
                    "gpt-5.5-mini",
                    "GPT-5.5 mini",
                    "Fast and inexpensive",
                    &[Low, Medium, High],
                ),
            ];
            for model in &mut models {
                model.options.push(speed.clone());
            }
            models
        }
        HarnessId::ClaudeCode => vec![
            model(
                "opus",
                "Opus",
                "Most capable for complex work",
                &[Low, Medium, High, XHigh, Max],
            ),
            model(
                "sonnet",
                "Sonnet",
                "Smart, efficient everyday model",
                &[Low, Medium, High, Max],
            ),
            model("haiku", "Haiku", "Fastest for quick answers", &[]),
        ],
        HarnessId::Opencode => vec![
            model("opencode/big-pickle", "Big Pickle", "OpenCode Zen", &[]),
            model("opencode/grok-code", "Grok Code Fast", "OpenCode Zen", &[]),
        ],
        _ => vec![],
    }
}

/// Daily (or hourly) buckets for the periods the UI asked for, per device:
/// this Mac carries most of the work, the Office Mini some, the phone none.
fn synthetic_usage(params: &Value, local_device: &str) -> UsageSnapshot {
    let now = Utc::now().timestamp_millis();
    let days = params["days"].as_u64().unwrap_or(7).clamp(1, 90) as u32;
    let device = params["targetDeviceId"]
        .as_str()
        .unwrap_or(local_device)
        .to_owned();
    let include_limits = params["includeLimits"].as_bool().unwrap_or(true);
    let mut starts: Vec<i64> = params["periodStartsMs"]
        .as_array()
        .map(|starts| starts.iter().filter_map(Value::as_i64).collect())
        .unwrap_or_default();
    if starts.is_empty() {
        let day = 24 * 60 * 60_000;
        starts = (0..days as i64).rev().map(|ago| now - ago * day).collect();
    }
    let factor = if device == local_device {
        1.0
    } else if device == OFFICE {
        0.35
    } else {
        0.0
    };
    let mut buckets = Vec::new();
    if factor > 0.0 {
        for (ix, start) in starts.iter().enumerate() {
            for (provider, model, base, rate) in [
                (UsageProvider::Codex, "gpt-5.5-codex", 2_400_000.0, 1.6),
                (UsageProvider::Claude, "claude-opus-4-6", 1_100_000.0, 6.2),
                (UsageProvider::Claude, "claude-sonnet-4-5", 600_000.0, 2.4),
            ] {
                // A deterministic, uneven week: weekends quieter.
                let wave = [0.62, 1.0, 0.86, 1.18, 0.74, 0.31, 0.22][ix % 7];
                let total = (base * wave * factor) as u64;
                let tokens = UsageTokens {
                    uncached_input: total * 22 / 100,
                    cached_input: total * 64 / 100,
                    cache_creation: total * 4 / 100,
                    output: total * 10 / 100,
                    reasoning: total * 3 / 100,
                };
                let cost = total as f64 / 1_000_000.0 * rate;
                buckets.push(UsageBucket {
                    start_ms: *start,
                    provider,
                    model: model.into(),
                    tokens,
                    records: (40.0 * wave * factor) as u64 + 1,
                    sessions: (6.0 * wave * factor) as u64 + 1,
                    api_equivalent_cost_usd: Some(cost),
                    known_cost_usd: Some(cost),
                    unpriced_records: Some(0),
                    cache_savings_usd: Some(cost * 0.4),
                    cost_quality: luna_proto::CostQuality::ModelPriced,
                });
            }
        }
    }
    let source = |provider: UsageProvider| UsageSource {
        provider,
        environment_id: device.clone(),
        fingerprint: format!("gallery-{device}-{}", provider.label()),
        status: if factor > 0.0 {
            UsageSourceStatus::Ready
        } else {
            UsageSourceStatus::Missing
        },
        distinct_sessions: (48.0 * factor) as u64,
        files_scanned: (120.0 * factor) as u64,
        files_cached: (110.0 * factor) as u64,
        failed_files: 0,
        detail: None,
    };
    let limit = |provider: UsageProvider, account: &str, plan: &str, five: f32, weekly: f32| {
        UsageLimitSnapshot {
            provider,
            verified_account_id: Some(format!("gallery-{account}")),
            account_label: Some(account.into()),
            plan: Some(plan.into()),
            checked_at_ms: Some(now - 60_000),
            availability: LimitAvailability::Ready,
            windows: vec![
                UsageLimitWindow {
                    id: "five-hour".into(),
                    label: "5h".into(),
                    used_percent: five,
                    resets_at_ms: Some(now + 134 * 60_000),
                    duration_minutes: Some(5 * 60),
                },
                UsageLimitWindow {
                    id: "weekly".into(),
                    label: "Weekly".into(),
                    used_percent: weekly,
                    resets_at_ms: Some(now + (3 * 24 + 7) * 60 * 60_000),
                    duration_minutes: Some(7 * 24 * 60),
                },
            ],
            reset_credits: None,
            detail: None,
        }
    };
    let limits = if include_limits && device == local_device {
        vec![
            limit(
                UsageProvider::Codex,
                "iulian@studio.example.test",
                "pro",
                42.0,
                68.0,
            ),
            limit(
                UsageProvider::Claude,
                "iulian@claude.example.test",
                "Max",
                23.0,
                51.0,
            ),
        ]
    } else {
        vec![]
    };
    UsageSnapshot {
        summary: UsageSummary {
            contract_version: luna_proto::USAGE_CONTRACT_VERSION,
            read_at_ms: now,
            time_zone: "UTC".into(),
            days,
            resolution: if days == 1 { "hourly" } else { "daily" }.into(),
            period_starts_ms: starts,
            until_ms: params["untilMs"].as_i64(),
            buckets,
            sources: vec![source(UsageProvider::Codex), source(UsageProvider::Claude)],
            scan_duration_ms: 84,
            partial: false,
            warnings: vec![],
        },
        limits,
        live_sessions: vec![],
    }
}

// ---------- seeding ----------

/// What the synthetic world contains; degraded seeds are noted, not fatal.
#[derive(Default)]
struct Seeded {
    devices: usize,
    projects: usize,
    archived: usize,
    automations: usize,
    project_commands: usize,
    backup: bool,
    notes: Vec<String>,
}

/// Chats run Codex with a model (automations require one).
fn chat_config() -> luna_proto::ChatConfig {
    luna_proto::ChatConfig {
        harness: HarnessId::Codex,
        provider_account_id: None,
        model: Some("gpt-5.5-codex".into()),
        reasoning: None,
        model_options: Default::default(),
        sandbox: luna_proto::SandboxLevel::WorkspaceWrite,
        permission_mode: PermissionMode::Auto,
        effective_permission_mode: PermissionMode::Auto,
        permission_ceiling_source: None,
        permission_degradation_reason: None,
        permission_audit: Vec::new(),
    }
}

fn device(
    id: &str,
    name: &str,
    platform: &str,
    seen_hours_ago: Option<i64>,
    version: &str,
) -> Value {
    let now = Utc::now();
    json!({
        "id": id,
        "name": name,
        "platform": platform,
        "lastSeenAt": seen_hours_ago.map(|hours| now - TimeDelta::hours(hours)),
        "createdAt": now - TimeDelta::days(120),
        "version": version,
    })
}

async fn seed_rpc(
    reader: &luna_rpc::RpcClient,
    device_id: &str,
    data_root: &Path,
    backup_source: &Path,
    seeded: &mut Seeded,
) {
    // Project commands through the engine's own store.
    let actions = vec![
        ProjectAction {
            id: "gallery-build".into(),
            name: "Build".into(),
            command: "cargo build --locked".into(),
            setup_on_worktree_create: true,
            wait_for_setup: true,
            preview_url: None,
        },
        ProjectAction {
            id: "gallery-test".into(),
            name: "Test".into(),
            command: "cargo test --locked -p luna-ui".into(),
            setup_on_worktree_create: false,
            wait_for_setup: true,
            preview_url: None,
        },
        ProjectAction {
            id: "gallery-docs".into(),
            name: "Docs preview".into(),
            command: "npm run dev --prefix docs".into(),
            setup_on_worktree_create: false,
            wait_for_setup: false,
            preview_url: Some("http://localhost:5173".into()),
        },
    ];
    match reader
        .call_as::<Vec<ProjectAction>>(
            methods::SAVE_PROJECT_COMMANDS,
            json!({"projectId": PROJECT, "actions": actions, "expectedActions": []}),
        )
        .await
    {
        Ok(saved) => seeded.project_commands = saved.len(),
        Err(error) => seeded
            .notes
            .push(format!("project commands not seeded: {error}")),
    }

    // Automations: preview, then apply, as the editor does.
    let automations = [
        (
            "Weekday release check",
            "Summarize what changed since yesterday and flag anything blocking the release.",
            luna_proto::AutomationTrigger::Calendar {
                hour: 9,
                minute: 0,
                weekdays: vec![1, 2, 3, 4, 5],
            },
            CHATS[0].id,
        ),
        (
            "Dependency audit",
            "Run cargo audit and open a task for anything new.",
            luna_proto::AutomationTrigger::Interval {
                anchor_ms: Utc::now().timestamp_millis() + 3 * 60 * 60_000,
                every_minutes: 360,
            },
            CHATS[6].id,
        ),
    ];
    for (name, prompt, trigger, task) in automations {
        let mut request = luna_proto::AutomationMutationRequest {
            request_id: format!("req-{}", uuid::Uuid::new_v4()),
            automation_id: format!("auto-{}", uuid::Uuid::new_v4()),
            owner_host_id: device_id.into(),
            expected_revision: 0,
            operation: luna_proto::AutomationOperation::Save {
                definition: luna_proto::AutomationDefinition {
                    name: name.into(),
                    prompt: prompt.into(),
                    timezone: "Europe/Bucharest".into(),
                    target: luna_proto::AutomationTarget::Existing {
                        task_id: task.into(),
                    },
                    trigger,
                    max_lateness_ms: 30 * 60_000,
                    ..luna_proto::AutomationDefinition::default()
                },
            },
            preview_id: String::new(),
            permission_ceiling: None,
        };
        let applied = async {
            let preview: luna_proto::AutomationPreview = reader
                .call_as(methods::PREVIEW_AUTOMATION, serde_json::to_value(&request)?)
                .await?;
            request.preview_id = preview.preview_id;
            reader
                .call(methods::MUTATE_AUTOMATION, serde_json::to_value(&request)?)
                .await?;
            anyhow::Ok(())
        }
        .await;
        match applied {
            Ok(()) => seeded.automations += 1,
            Err(error) => seeded
                .notes
                .push(format!("automation {name:?} not seeded: {error}")),
        }
    }

    // A configured backup destination with one real archive.
    let backup = async {
        let source = backup_source.to_string_lossy();
        let destination = data_root.join("Backups");
        std::fs::create_dir_all(&destination)?;
        std::fs::write(
            destination.join(".luna-l17-isolated-backups"),
            "L17 synthetic or copy-isolated backup destination\n",
        )?;
        reader
            .call(
                methods::CONFIGURE_BACKUP,
                json!({
                    "sourceRoot": source,
                    "destinationDir": destination.to_string_lossy(),
                    "automaticEnabled": true,
                }),
            )
            .await?;
        reader.call(methods::BACKUP_NOW, json!({})).await?;
        anyhow::Ok(())
    }
    .await;
    match backup {
        Ok(()) => seeded.backup = true,
        Err(error) => seeded.notes.push(format!("backup not seeded: {error}")),
    }
}

// ---------- capture helpers ----------

fn draw(window: Win, cx: &mut AsyncApp) -> anyhow::Result<()> {
    gpui::AnyWindowHandle::from(window).update(cx, |_, window, cx| {
        window.draw(cx).clear();
    })?;
    Ok(())
}

fn render(window: Win, cx: &mut AsyncApp) -> anyhow::Result<image::RgbaImage> {
    gpui::AnyWindowHandle::from(window).update(cx, |_, window, cx| {
        window.draw(cx).clear();
        window.render_to_image()
    })?
}

async fn settle(cx: &mut AsyncApp, ms: u64) {
    cx.background_executor()
        .timer(Duration::from_millis(ms))
        .await;
}

/// Park the pointer on empty content space so nothing shows a hover state.
fn park_pointer(window: Win, cx: &mut AsyncApp, width: f32, height: f32) -> anyhow::Result<()> {
    gpui::AnyWindowHandle::from(window).update(cx, |_, window, cx| {
        window.dispatch_event(
            PlatformInput::MouseMove(gpui::MouseMoveEvent {
                position: point(px(width - 6.0), px(height - 6.0)),
                pressed_button: None,
                modifiers: Default::default(),
            }),
            cx,
        );
    })?;
    Ok(())
}

/// Keep a frame only when two renders 120 ms apart match pixel for pixel.
async fn stable_capture(
    window: Win,
    cx: &mut AsyncApp,
) -> anyhow::Result<(image::RgbaImage, bool, usize)> {
    let mut last = render(window, cx)?;
    for attempt in 1..=SETTLE_TRIES {
        settle(cx, 120).await;
        let next = render(window, cx)?;
        if next.as_raw() == last.as_raw() {
            return Ok((next, true, attempt));
        }
        last = next;
    }
    Ok((last, false, SETTLE_TRIES))
}

/// Why `section`'s page isn't ready yet (`Ok`: it is).
fn page_ready(
    section: SettingsSection,
    readback: &Value,
    log: &RpcLog,
    seeded: &Seeded,
) -> Result<(), String> {
    let want = format!("{section:?}");
    if readback["section"] != want {
        return Err(format!(
            "routed to {} instead of {want}",
            readback["section"]
        ));
    }
    if readback["gate"] != "Ready" {
        return Err(format!("the shell shows the {} gate", readback["gate"]));
    }
    if readback["splashGone"] != true {
        return Err("the boot splash is still up".into());
    }
    if readback["mounted"] != true {
        return Err("the page entity does not exist yet".into());
    }
    let at_least = |key: &str, min: usize| {
        let have = readback[key].as_u64().unwrap_or(0) as usize;
        if have >= min {
            Ok(())
        } else {
            Err(format!("{key} is {have}, expected at least {min}"))
        }
    };
    let served = |method: &str| {
        if log.count(method) > 0 {
            Ok(())
        } else {
            Err(format!("{method} was never served"))
        }
    };
    match section {
        SettingsSection::Providers => {
            served(methods::LIST_AGENT_ACCOUNTS)?;
            served(methods::LIST_HARNESSES)?;
            served(methods::GET_PROVIDER_UPDATES)?;
            at_least("providerUpdates", 1)
        }
        SettingsSection::Projects => {
            if readback["selectedProject"].is_string() {
                Ok(())
            } else {
                Err("no project is selected".into())
            }
        }
        SettingsSection::Devices => at_least("devices", seeded.devices),
        SettingsSection::Archived => at_least("archived", seeded.archived),
        SettingsSection::Notifications => at_least("notificationTasks", 1),
        SettingsSection::Usage => {
            if readback["usageSnapshot"] != true {
                Err("no usage snapshot yet".into())
            } else if readback["usageRefreshing"] == true {
                Err("usage is refreshing".into())
            } else {
                Ok(())
            }
        }
        SettingsSection::Updates => {
            if readback["updateLatest"].is_string() && readback["updateChecking"] != true {
                Ok(())
            } else {
                Err("no update status yet".into())
            }
        }
        SettingsSection::Automations => {
            if readback["automationsLoading"] != false {
                Err("automations are loading".into())
            } else {
                at_least("automationRows", seeded.automations)
            }
        }
        SettingsSection::Backups => served(methods::GET_BACKUP_STATUS),
        _ => Ok(()),
    }
}

async fn wait_ready(
    window: Win,
    cx: &mut AsyncApp,
    section: SettingsSection,
    log: &RpcLog,
    seeded: &Seeded,
) -> anyhow::Result<Value> {
    let deadline = Instant::now() + READY_TIMEOUT;
    // Let the route's own loads reach the transport before judging quiet.
    settle(cx, 100).await;
    loop {
        draw(window, cx)?;
        let readback = window.update(cx, |shell, _, cx| shell.fixture_gallery_readback(cx))?;
        anyhow::ensure!(
            readback["updateCheckAttempted"] != true,
            "the Updates page tried to reach the release feed: {readback}"
        );
        let quiet = log.quiet_for().is_some_and(|quiet| quiet >= QUIET);
        let ready = page_ready(section, &readback, log, seeded);
        if quiet && ready.is_ok() {
            return Ok(readback);
        }
        if Instant::now() > deadline {
            anyhow::bail!(
                "{section:?} never became ready ({}; transport quiet: {quiet}); readback {readback}",
                ready.err().unwrap_or_else(|| "ready".into())
            );
        }
        settle(cx, 50).await;
    }
}

/// Bottom of the content column (image rows), whether it reaches the window
/// edge, and whether the column is effectively blank.
struct Pixels {
    content_bottom: u32,
    touches_bottom: bool,
    blank: bool,
}

fn analyze(image: &image::RgbaImage, content_left: u32, frame: u32) -> Pixels {
    let (width, height) = image.dimensions();
    let left = content_left.min(width.saturating_sub(1));
    let right = width.saturating_sub(frame).max(left + 1);
    let floor = height.saturating_sub(frame + 1);
    let edge: Vec<[u8; 4]> = (left..right).map(|x| image.get_pixel(x, floor).0).collect();
    let mut counts: BTreeMap<[u8; 4], usize> = BTreeMap::new();
    for pixel in &edge {
        *counts.entry(*pixel).or_default() += 1;
    }
    let (background, uniform) = counts
        .iter()
        .max_by_key(|(_, count)| **count)
        .map(|(pixel, count)| (*pixel, *count))
        .unwrap_or(([0; 4], 0));
    let differs = |pixel: [u8; 4]| {
        pixel
            .iter()
            .zip(background)
            .take(3)
            .any(|(a, b)| a.abs_diff(b) > 6)
    };
    let mut bottom = None;
    for y in (0..=floor).rev() {
        if (left..right)
            .step_by(2)
            .any(|x| differs(image.get_pixel(x, y).0))
        {
            bottom = Some(y);
            break;
        }
    }
    let content_bottom = bottom.unwrap_or(0);
    Pixels {
        content_bottom,
        touches_bottom: (uniform as f32) < edge.len() as f32 * 0.98
            || content_bottom + frame / 2 >= floor,
        // A settings page is at least a title and one section tall.
        blank: bottom.is_none_or(|y| y < height / 40),
    }
}

fn sha256(path: &Path) -> anyhow::Result<String> {
    let bytes = std::fs::read(path)?;
    Ok(Sha256::digest(&bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

// ---------- the run ----------

struct Run {
    output: PathBuf,
    matrix: Matrix,
    state: gpui::Entity<state::AppState>,
    boot: EngineBootConfig,
    log: Arc<RpcLog>,
    seeded: Seeded,
    sidebar_width: f32,
}

static CURRENT: Mutex<String> = Mutex::new(String::new());

async fn run(cx: &mut AsyncApp, job: &Run) -> anyhow::Result<(Vec<Value>, Vec<String>)> {
    let mut captures = Vec::new();
    let mut warnings = Vec::new();
    for &width in &job.matrix.widths {
        for &interface in &job.matrix.sizes {
            *CURRENT.lock().unwrap() = format!("opening {width}w at {}px", interface.pixels());
            let height = job.matrix.height;
            let state = job.state.clone();
            let boot = job.boot.clone();
            let background = cx.update(|cx| theme::Theme::of(cx).window_background_appearance());
            let window: Win = cx.open_window(
                WindowOptions {
                    window_background: background,
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        point(px(20.), px(20.)),
                        size(px(width), px(height)),
                    ))),
                    show: false,
                    focus: false,
                    ..Default::default()
                },
                move |window, cx| {
                    // As the app's own window builder does.
                    window.set_rem_size(px(typography::font_size(cx).pixels()));
                    cx.new(|cx| Shell::new(state, boot, cx))
                },
            )?;
            // The production Interface size path (Appearance → Interface size).
            let rem = window.update(cx, |_, window, cx| {
                typography::set_font_size(interface, window, cx);
                (
                    f32::from(window.rem_size()),
                    theme::Theme::of(cx).ui_font_size,
                    typography::font_size(cx).pixels(),
                )
            })?;
            anyhow::ensure!(
                rem.0 == interface.pixels()
                    && rem.1 == interface.pixels()
                    && rem.2 == interface.pixels(),
                "Interface size {}px not applied: window rem {}, theme {}, typography {}",
                interface.pixels(),
                rem.0,
                rem.1,
                rem.2
            );
            // The first state frame (connection Ready) dismisses the splash.
            cx.update(|cx| job.state.update(cx, |_, cx| cx.notify()));
            for &(theme_name, mode) in &job.matrix.themes {
                cx.update(|cx| appearance::set_mode(mode, cx));
                for &(slug, section) in &job.matrix.pages {
                    let name = format!("{slug}-{theme_name}-{}px-{width}w", interface.pixels());
                    *CURRENT.lock().unwrap() = name.clone();
                    window.update(cx, |shell, _, cx| shell.fixture_gallery_open(section, cx))?;
                    park_pointer(window, cx, width, height)?;
                    let readback = wait_ready(window, cx, section, &job.log, &job.seeded).await?;
                    let rem_now = window.update(cx, |_, window, _| f32::from(window.rem_size()))?;
                    anyhow::ensure!(
                        rem_now == interface.pixels(),
                        "{name}: window rem drifted to {rem_now}"
                    );
                    let (image, settled, attempts) = stable_capture(window, cx).await?;
                    let scale = image.width() as f32 / width;
                    let pixels = analyze(
                        &image,
                        ((job.sidebar_width + PANEL_FRAME) * scale) as u32,
                        (PANEL_FRAME * scale) as u32,
                    );
                    anyhow::ensure!(!pixels.blank, "{name}: the content column rendered blank");
                    let file = format!("{name}.png");
                    let path = job.output.join(&file);
                    image.save(&path)?;
                    if !settled {
                        warnings.push(format!(
                            "{name}: frames never settled (animation or live content)"
                        ));
                    }
                    if pixels.touches_bottom {
                        warnings.push(format!(
                            "{name}: content reaches the window bottom; rerun with a larger --height"
                        ));
                    }
                    captures.push(json!({
                        "page": slug,
                        "section": format!("{section:?}"),
                        "theme": theme_name,
                        "sizePx": interface.pixels(),
                        "widthPx": width,
                        "heightPx": height,
                        "file": file,
                        "sha256": sha256(&path)?,
                        "imageWidth": image.width(),
                        "imageHeight": image.height(),
                        "scale": scale,
                        "contentHeightPx": ((pixels.content_bottom as f32 / scale).ceil() as u32 + 24).min(height as u32),
                        "settled": settled,
                        "settleAttempts": attempts,
                        "touchesBottom": pixels.touches_bottom,
                        "remPx": rem_now,
                        "readback": readback,
                    }));
                    println!("captured {name}");
                }
            }
            window.update(cx, |_, window, _| window.remove_window())?;
            settle(cx, 50).await;
        }
    }
    Ok((captures, warnings))
}

fn contact_sheet(matrix: &Matrix, captures: &[Value]) -> String {
    let escape = |text: &str| {
        text.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    };
    let mut html = String::from(
        r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Settings gallery</title>
<style>
:root { --bg:#f6f6f7; --fg:#1b1b1f; --muted:#6b6b76; --line:#dcdce2; --warn:#b4530a; --card:#ffffff; }
@media (prefers-color-scheme: dark) { :root { --bg:#111114; --fg:#ececf1; --muted:#9a9aa6; --line:#2a2a31; --warn:#f0a35c; --card:#18181c; } }
body { margin:0; padding:24px 16px 64px; background:var(--bg); color:var(--fg); font:14px/1.45 -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; }
h1 { font-size:22px; margin:0 0 4px; } h2 { font-size:18px; margin:40px 0 12px; scroll-margin-top:16px; }
nav { display:flex; flex-wrap:wrap; gap:6px 12px; margin:12px 0 0; } nav a { color:var(--fg); }
p.meta { color:var(--muted); margin:0; }
table { border-collapse:separate; border-spacing:10px; width:100%; table-layout:fixed; margin-left:-10px; }
th { font-weight:600; text-align:left; color:var(--muted); font-size:12px; }
th.size { width:52px; vertical-align:top; padding-top:4px; }
td { vertical-align:top; background:var(--card); border:1px solid var(--line); border-radius:8px; padding:6px; }
.thumb { overflow:hidden; border-radius:4px; } .thumb img { width:100%; display:block; }
.cap { font-size:11px; color:var(--muted); margin-top:4px; } .warn { color:var(--warn); font-weight:600; }
</style></head><body>
<h1>Settings gallery</h1>
"#,
    );
    html.push_str(&format!(
        "<p class=\"meta\">{} captures · sizes {} · widths {} · themes {} · window height {} pt · thumbnails cropped to the content; click for the full capture</p>\n<nav>",
        captures.len(),
        matrix.sizes.iter().map(|size| format!("{}px", size.pixels())).collect::<Vec<_>>().join(", "),
        matrix.widths.iter().map(|width| format!("{width}")).collect::<Vec<_>>().join(", "),
        matrix.themes.iter().map(|(name, _)| *name).collect::<Vec<_>>().join(", "),
        matrix.height,
    ));
    for (slug, _) in &matrix.pages {
        html.push_str(&format!("<a href=\"#{slug}\">{slug}</a>"));
    }
    html.push_str("</nav>\n");
    let columns: Vec<(f32, &str)> = matrix
        .widths
        .iter()
        .flat_map(|width| matrix.themes.iter().map(move |(theme, _)| (*width, *theme)))
        .collect();
    for (slug, section) in &matrix.pages {
        html.push_str(&format!(
            "<h2 id=\"{slug}\">{}</h2>\n<table><tr><th class=\"size\"></th>",
            escape(section.label())
        ));
        for (width, theme) in &columns {
            html.push_str(&format!("<th>{width}w · {theme}</th>"));
        }
        html.push_str("</tr>\n");
        for size in &matrix.sizes {
            html.push_str(&format!("<tr><th class=\"size\">{}px</th>", size.pixels()));
            for (width, theme) in &columns {
                let capture = captures.iter().find(|capture| {
                    capture["page"] == *slug
                        && capture["theme"] == *theme
                        && capture["sizePx"].as_f64() == Some(size.pixels() as f64)
                        && capture["widthPx"].as_f64() == Some(*width as f64)
                });
                let Some(capture) = capture else {
                    html.push_str("<td>missing</td>");
                    continue;
                };
                let file = capture["file"].as_str().unwrap_or_default();
                let content = capture["contentHeightPx"].as_u64().unwrap_or(1).max(1);
                let mut flags = Vec::new();
                if capture["settled"] != true {
                    flags.push("unsettled");
                }
                if capture["touchesBottom"] == true {
                    flags.push("reaches bottom");
                }
                html.push_str(&format!(
                    "<td><a href=\"{file}\"><div class=\"thumb\" style=\"aspect-ratio:{width}/{content}\"><img src=\"{file}\" loading=\"lazy\" alt=\"{}\"></div></a><div class=\"cap\">{file}{}</div></td>",
                    escape(file),
                    if flags.is_empty() {
                        String::new()
                    } else {
                        format!(" · <span class=\"warn\">{}</span>", flags.join(", "))
                    }
                ));
            }
            html.push_str("</tr>\n");
        }
        html.push_str("</table>\n");
    }
    html.push_str("</body></html>\n");
    html
}

fn fail(output: &Path, message: &str) {
    let context = CURRENT
        .lock()
        .map(|current| current.clone())
        .unwrap_or_default();
    let _ = std::fs::write(
        output.join("failure.txt"),
        format!("FAIL during {context}\n{message}\n"),
    );
    eprintln!("settings gallery failed during {context}: {message}");
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter("warn").init();
    let (output, matrix) = parse_args()?;
    // Every section the Shell knows must be in the gallery (and only once).
    anyhow::ensure!(
        SettingsSection::ALL.len() == PAGES.len()
            && SettingsSection::ALL.iter().all(|section| PAGES
                .iter()
                .filter(|(_, known)| known == section)
                .count()
                == 1),
        "SettingsSection::ALL changed; update PAGES in settings-gallery-fixture.rs"
    );
    std::fs::create_dir_all(&output)?;
    for entry in std::fs::read_dir(&output)? {
        let path = entry?.path();
        let stale = path.extension().is_some_and(|ext| ext == "png")
            || [
                "manifest.json",
                "index.html",
                "settings-gallery.pass",
                "failure.txt",
            ]
            .iter()
            .any(|name| path.file_name().is_some_and(|file| file == *name));
        if stale {
            std::fs::remove_file(&path)?;
        }
    }
    let panic_output = output.clone();
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        fail(&panic_output, &format!("panic: {info}"));
        default_hook(info);
    }));

    // ---- synthetic engine ----
    let root = tempfile::Builder::new()
        .prefix("luna-settings-gallery.")
        .tempdir()?;
    let ui_data = root.path().join("ui");
    let engine_data = root.path().join("engine");
    let providers = root.path().join("providers");
    let luna_dir = root.path().join("projects/luna");
    let docs_dir = root.path().join("projects/docs-site");
    for dir in [&ui_data, &engine_data, &luna_dir, &docs_dir] {
        std::fs::create_dir_all(dir)?;
    }
    let accounts = AgentAccountsConfig {
        data_dir: engine_data.join("provider-state"),
        claude_config_dir: providers.join("claude"),
        claude_config_file: providers.join("claude.json"),
        codex_home: providers.join("codex"),
    };
    accounts.disable_claude_keychain_fallback()?;
    let runtime = tokio::runtime::Runtime::new()?;
    let profile = EngineProfile::development(&engine_data, "gallery-org", "gallery-user");
    // Mark the synthetic device root (and, below, its backup destination) as
    // copy-isolated so the engine offers its real backup controls.
    std::fs::create_dir_all(profile.device_root())?;
    std::fs::write(
        profile.device_root().join(".luna-l17-isolated-root"),
        "L17 synthetic or copy-isolated root\n",
    )?;
    let core = runtime.block_on(async {
        EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
            profile,
            Arc::new(HarnessRegistry::new()),
            HarnessId::Mock,
            None,
            accounts,
        )
    })?;
    // Backups copy a separate copy-isolated LUNA root: assemble one, seed it
    // with a project, and shut it down before the gallery engine backs it up.
    let backup_source = {
        let profile = EngineProfile::development(
            &root.path().join("backup-source"),
            "gallery-org",
            "gallery-user",
        );
        std::fs::create_dir_all(profile.device_root())?;
        std::fs::write(
            profile.device_root().join(".luna-l17-isolated-root"),
            "L17 synthetic or copy-isolated root\n",
        )?;
        let device_root = profile.device_root().to_path_buf();
        let source_accounts = AgentAccountsConfig {
            data_dir: root.path().join("backup-source-providers"),
            claude_config_dir: root.path().join("backup-source-providers/claude"),
            claude_config_file: root.path().join("backup-source-providers/claude.json"),
            codex_home: root.path().join("backup-source-providers/codex"),
        };
        source_accounts.disable_claude_keychain_fallback()?;
        let source = runtime.block_on(async {
            EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
                profile,
                Arc::new(HarnessRegistry::new()),
                HarnessId::Mock,
                None,
                source_accounts,
            )
        })?;
        source.workspace.create_space(
            PROJECT,
            &source.device_id,
            luna_dir.to_str().unwrap(),
            Some("LUNA".into()),
            true,
        )?;
        runtime.block_on(source.shutdown());
        drop(source);
        device_root
    };
    let mut auth = AuthConfig::new("", root.path().join("auth"));
    auth.dev_user_id = "gallery-user".into();
    auth.use_in_memory_credentials_for_tests();
    core.set_auth(Auth::new(auth));
    let local = core.device_id.clone();
    core.workspace.create_space(
        PROJECT,
        &local,
        luna_dir.to_str().unwrap(),
        Some("LUNA".into()),
        true,
    )?;
    core.workspace.create_space(
        DOCS_PROJECT,
        &local,
        docs_dir.to_str().unwrap(),
        Some("Docs site".into()),
        false,
    )?;
    let now = Utc::now();
    let mut chats = Vec::new();
    for spec in &CHATS {
        let cwd = if spec.project == PROJECT {
            &luna_dir
        } else {
            &docs_dir
        };
        core.workspace.create_chat(
            spec.id,
            Some(spec.project),
            Some(&local),
            Some(chat_config()),
            Some(cwd.to_string_lossy().into_owned()),
        )?;
        core.workspace.rename_chat(spec.id, spec.title)?;
        let mut chat = core
            .workspace
            .chat(spec.id)?
            .ok_or_else(|| anyhow::anyhow!("seeded chat {} is missing", spec.id))?;
        let at = now - TimeDelta::hours(spec.hours_ago);
        chat.created_at = at - TimeDelta::minutes(20);
        chat.last_message_at = Some(at);
        chat.last_seen_at = Some(at);
        chat.archived = spec.archived;
        if spec.remote {
            chat.device_id = OFFICE.into();
        }
        core.workspace.import_chat_row(&chat)?;
        chats.push(chat);
    }
    let mut spaces = core.workspace.read_spaces()?;
    if let Some(space) = spaces.iter_mut().find(|space| space.id == PROJECT) {
        space.execution_defaults.provider = Some(ProjectProviderDefault {
            harness: HarnessId::Codex,
            model: Some("gpt-5.5-codex".into()),
        });
        space.execution_defaults.permission_mode = Some(PermissionMode::Auto);
        space.execution_defaults.checkout = Some(ProjectCheckout::NewWorktree);
    }
    // The UI shows each project's folder; show a plausible one instead of
    // the temporary directory (only the engine's copy is ever used on disk).
    for space in &mut spaces {
        space.path = match space.id.as_str() {
            PROJECT => "/Users/studio/Code/luna".into(),
            _ => "/Users/studio/Code/docs-site".into(),
        };
    }
    let devices: Vec<luna_proto::Device> = vec![
        serde_json::from_value(device(&local, "Studio Mac", "macos", Some(0), "0.6.0"))?,
        serde_json::from_value(device(OFFICE, "Office Mini", "macos", Some(3), "0.5.8"))?,
        serde_json::from_value(device(PHONE, "iPhone", "ios", Some(50), "0.6.0"))?,
    ];
    let reader = {
        let _entered = runtime.enter();
        luna_rpc::memory_client(core.rpc_service())
    };
    let mut seeded = Seeded {
        devices: devices.len(),
        projects: spaces.len(),
        archived: CHATS.iter().filter(|spec| spec.archived).count(),
        ..Default::default()
    };
    runtime.block_on(seed_rpc(
        &reader,
        &local,
        root.path(),
        &backup_source,
        &mut seeded,
    ));
    for note in &seeded.notes {
        eprintln!("settings gallery: degraded seed: {note}");
    }
    let log = Arc::new(RpcLog::default());
    let service: Arc<dyn luna_rpc::RpcService> = Arc::new(GalleryRpc {
        inner: core.rpc_service(),
        local_device: local.clone(),
        log: log.clone(),
    });
    let handle = runtime.block_on(state::EngineHandle::fixture_from_core_with_service(
        &core, service,
    ))?;
    let boot = EngineBootConfig {
        data_dir: ui_data.clone(),
        ipc_port: 59_993,
        edge_url: String::new(),
        edge_token: None,
        org_id: None,
        workos_client_id: None,
        default_harness: HarnessId::Codex,
    };
    let update = luna_update::UpdateStatus {
        current_version: luna_update::current_version().to_owned(),
        latest_version: Some(next_version(luna_update::current_version())),
        update_available: true,
        checked_at: Some(now.timestamp_millis() - 12 * 60_000),
        error: None,
    };

    let mut ui_settings = settings::UiSettings::default();
    // Opaque surfaces: a frosted window composites over the desktop, which a
    // headless capture cannot show.
    ui_settings.surface = luna_theme::SurfacePreference::Opaque;
    ui_settings.sidebar_auto_settle_after_days = None;
    ui_settings.task_notification_policies.insert(
        CHATS[1].id.into(),
        settings::TaskNotificationPolicy::ImportantOnly,
    );
    ui_settings
        .task_notification_policies
        .insert(CHATS[2].id.into(), settings::TaskNotificationPolicy::Muted);
    ui_settings.keep_awake_task_ids.insert(CHATS[0].id.into());
    let sidebar_width = ui_settings.sidebar_width;

    let temp_root = root.path().to_path_buf();
    let ui_output = output.clone();
    let first_mode = matrix.themes[0].1;
    gpui_platform::headless()
        .with_assets(icons::Assets)
        .run(move |cx| {
            gpui_tokio::init(cx);
            gpui_base::init(cx);
            settings::init(ui_settings.clone(), ui_data.clone(), cx);
            let fonts = typography::register_fonts(cx);
            typography::init(
                ui_settings.ui_font_family.clone(),
                ui_settings.ui_font_size,
                ui_settings.terminal_font_family.clone(),
                ui_settings.terminal_font_size,
                ui_settings.code_font_family.clone(),
                ui_settings.code_font_size,
                fonts,
                cx,
            );
            theme_library::init(ui_data.clone(), cx);
            appearance::init(
                first_mode,
                ui_settings.theme_selection,
                ui_settings.accent,
                ui_settings.surface,
                cx,
            );
            history::init(
                ui_settings.git_history_columns,
                ui_settings.git_history_column_widths,
                ui_settings.git_history_column_order,
                ui_settings.git_history_author_display,
                cx,
            );
            composer::init(cx, ui_settings.composer_send_behavior);
            terminal::panel::init(cx);
            let state = cx.new(|_| {
                let mut state = state::AppState::new();
                state.local_device_id = Some(local.clone());
                state.devices = devices;
                state.spaces = spaces;
                state.selected_space = Some(PROJECT.into());
                state.chats = chats;
                state.chats_synced = true;
                state.spaces_synced = true;
                state.auto_selected = true;
                state.connection = state::ConnectionStatus::Ready;
                state.workspace_scope = Some(luna_proto::WorkspaceScope::Synced);
                state.auth = Some(luna_proto::AuthState::SignedIn {
                    user: luna_proto::UserProfile {
                        id: "gallery-user".into(),
                        email: "iulian@studio.example.test".into(),
                        name: Some("Iulian".into()),
                    },
                    org_id: Some("gallery-org".into()),
                });
                state.update = Some(update);
                state.fixture_attachment_engine(handle);
                state
            });
            let job = Run {
                output: ui_output.clone(),
                matrix,
                state,
                boot,
                log,
                seeded,
                sidebar_width,
            };
            cx.spawn(async move |cx| {
                let result = run(cx, &job).await.and_then(|(captures, warnings)| {
                    let expected = job.matrix.pages.len()
                        * job.matrix.sizes.len()
                        * job.matrix.widths.len()
                        * job.matrix.themes.len();
                    anyhow::ensure!(
                        captures.len() == expected,
                        "captured {} of {expected}",
                        captures.len()
                    );
                    let manifest = json!({
                        "generatedAt": Utc::now(),
                        "fixture": "crates/ui/examples/settings-gallery-fixture.rs",
                        "matrix": {
                            "pages": job.matrix.pages.iter().map(|(slug, _)| *slug).collect::<Vec<_>>(),
                            "sizesPx": job.matrix.sizes.iter().map(|size| size.pixels()).collect::<Vec<_>>(),
                            "widthsPx": job.matrix.widths,
                            "themes": job.matrix.themes.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
                            "heightPx": job.matrix.height,
                        },
                        "environment": {
                            "shell": "production Shell, one hidden window per (width, size)",
                            "engine": "in-process EngineCore in a temporary data dir",
                            "providerTransport": "synthetic accounts, harnesses, CLI versions, models and usage",
                            "surface": "opaque",
                            "sidebarWidthPt": job.sidebar_width,
                            "workspaceScope": "synced",
                        },
                        "seeded": {
                            "devices": job.seeded.devices,
                            "projects": job.seeded.projects,
                            "archivedChats": job.seeded.archived,
                            "automations": job.seeded.automations,
                            "projectCommands": job.seeded.project_commands,
                            "backup": job.seeded.backup,
                            "degraded": job.seeded.notes,
                        },
                        "rpc": {
                            "served": job.log.served.lock().unwrap().clone(),
                            "refused": job.log.refused.lock().unwrap().clone(),
                            "failed": job.log.failed.lock().unwrap().clone(),
                        },
                        "warnings": warnings,
                        "captures": captures,
                    });
                    std::fs::write(
                        job.output.join("manifest.json"),
                        serde_json::to_vec_pretty(&manifest)?,
                    )?;
                    std::fs::write(
                        job.output.join("index.html"),
                        contact_sheet(&job.matrix, &captures),
                    )?;
                    let mut receipt = format!(
                        "PASS: {} Settings captures of the production Shell ({} pages × {} sizes × {} widths × {} themes); every page loaded its data, rendered non-blank, at the requested Interface size.\nmanifest.json sha256 {}\n",
                        captures.len(),
                        job.matrix.pages.len(),
                        job.matrix.sizes.len(),
                        job.matrix.widths.len(),
                        job.matrix.themes.len(),
                        sha256(&job.output.join("manifest.json"))?,
                    );
                    for note in &job.seeded.notes {
                        receipt.push_str(&format!("degraded seed: {note}\n"));
                    }
                    for warning in &warnings {
                        receipt.push_str(&format!("warning: {warning}\n"));
                    }
                    std::fs::write(job.output.join("settings-gallery.pass"), receipt)?;
                    Ok(())
                });
                // Quitting ends the process here (the platform run loop does
                // not return), so the verdict and cleanup happen now.
                let _ = std::fs::remove_dir_all(&temp_root);
                if let Err(error) = result {
                    fail(&job.output, &format!("{error:#}"));
                    std::process::exit(1);
                }
                println!("settings gallery: {}", job.output.join("index.html").display());
                cx.update(|cx| cx.quit());
            })
            .detach();
        });
    // Reached only if the platform run loop returns instead of exiting.
    runtime.block_on(core.shutdown());
    Ok(())
}
