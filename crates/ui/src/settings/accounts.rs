//! Settings → Providers: per-device CLI enablement and provider-owned accounts.
//! Claude Code and Codex have account rows — email, plan badge, Active, usage
//! meters (indigo → amber ≥80% → red ≥95%, reset time), lifecycle controls — plus
//! the add-account dialogs (paste-code and browser-poll flows) and
//! account-shaped loading skeletons. Luna retargets devices from the settings
//! sidebar (`targetDeviceId` passthrough kept plumbed, unused single-device).
//!
//! The accounts RPC surface is being implemented engine-side in parallel —
//! every call here surfaces failures as inline UI states.

use chrono::{DateTime, Utc};
use gpui::{
    AnyElement, Context, Entity, Hsla, SharedString, Subscription, Task, Window, div, prelude::*,
    px,
};
use std::time::Duration;

use luna_engine::registry::{HarnessDescriptor, descriptor_enabled};
use luna_proto::{
    AgentAccount, AgentAccountsSnapshot, AgentAuthenticationState, AgentLoginMode, AgentLoginPoll,
    AgentLoginStart, AgentLoginStatus, HarnessId, LimitAvailability, ProviderUpdateResult,
    ProviderUpdateStatus, UsageProvider, UsageSnapshot,
};
use luna_rpc::methods;

use crate::composer::{ComposerInput, ComposerInputEvent};
use crate::popover::{self, Loadable};
use crate::settings::harnesses::HarnessesPage;
use crate::settings::widgets;
use crate::state::AppState;
use crate::theme::Theme;

// ---------------------------------------------------------------------------
// Pure: usage meters + labels
// ---------------------------------------------------------------------------

pub const USAGE_WARN_FRACTION: f32 = 0.80;
pub const USAGE_CRITICAL_FRACTION: f32 = 0.95;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageLevel {
    /// < 80% — indigo.
    Normal,
    /// ≥ 80% — amber.
    Warn,
    /// ≥ 95% — red.
    Critical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AccountLifecycleAction {
    DisconnectClaude,
    ReconnectClaude,
    DisconnectCodex,
    SelectCodexDefault,
}

fn account_lifecycle_action(account: &AgentAccount) -> Option<AccountLifecycleAction> {
    match (account.harness, account.active, account.switchable) {
        (HarnessId::ClaudeCode, true, _) => Some(AccountLifecycleAction::DisconnectClaude),
        (HarnessId::ClaudeCode, false, true) => Some(AccountLifecycleAction::ReconnectClaude),
        (HarnessId::Codex, true, _) => Some(AccountLifecycleAction::DisconnectCodex),
        (HarnessId::Codex, false, true) => Some(AccountLifecycleAction::SelectCodexDefault),
        _ => None,
    }
}

/// Threshold classification of a usage fraction. Pure.
pub fn usage_level(fraction: f32) -> UsageLevel {
    if fraction >= USAGE_CRITICAL_FRACTION {
        UsageLevel::Critical
    } else if fraction >= USAGE_WARN_FRACTION {
        UsageLevel::Warn
    } else {
        UsageLevel::Normal
    }
}

pub fn usage_color(level: UsageLevel, theme: &Theme) -> Hsla {
    match level {
        UsageLevel::Normal => theme.accent,
        UsageLevel::Warn => theme.warning,
        UsageLevel::Critical => theme.danger,
    }
}

/// Why a `ListAgentAccounts` load is happening. Pure input to
/// [`force_usage_for`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadTrigger {
    /// Page construction — the visit's first list.
    Mount,
    /// "Click to retry" after a failed load — still the visit's first
    /// successful list.
    Retry,
    /// The explicit Refresh button.
    Refresh,
    /// After a completed add-account login flow.
    PostLogin,
    /// After Switch/Forget succeeds.
    PostAction,
}

/// Whether a load should ask the engine to probe usage (`forceUsage`). The
/// engine only hits the provider when forced; non-forced lists serve the 60s
/// usage cache or nothing (engine/src/agent_accounts.rs module docs — the
/// design expects the UI to force "on page mount/refresh"). The visit's first
/// list (mount, or retry after a failure) must force, or every first open
/// renders "Usage unavailable" until a manual Refresh — the old app fetched
/// usage on every list. Post-Switch/Forget lists ride the still-warm cache.
pub fn force_usage_for(trigger: LoadTrigger) -> bool {
    match trigger {
        LoadTrigger::Mount | LoadTrigger::Retry | LoadTrigger::Refresh | LoadTrigger::PostLogin => {
            true
        }
        LoadTrigger::PostAction => false,
    }
}

/// Compact absolute reset moment (luna settings.agents.tsx `formatReset`):
/// a local clock time ("3:45 PM") when it lands within ~22h, a short weekday
/// ("Mon") within a week, else month + day ("Sep 14") — a weekday is noise
/// when the window is a Codex free-tier MONTHLY reset weeks out. The caller
/// prefixes "resets ". Pure given `now`.
pub fn format_reset(resets_at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> Option<String> {
    use chrono::Local;
    let at = resets_at?;
    let local = at.with_timezone(&Local);
    Some(if at.signed_duration_since(now).num_hours() < 22 {
        format!("resets {}", local.format("%-I:%M %p"))
    } else if at.signed_duration_since(now).num_hours() < 24 * 7 {
        format!("resets {}", local.format("%a"))
    } else {
        format!("resets {}", local.format("%b %-d"))
    })
}

/// The provider cards, in display order: harness, name, and CLI command.
pub const PROVIDERS: [(HarnessId, &str, &str); 3] = [
    (HarnessId::Codex, "Codex", "codex"),
    (HarnessId::ClaudeCode, "Claude Code", "claude"),
    (HarnessId::Opencode, "OpenCode", "opencode"),
];

/// Accounts of one provider, in the engine's order (slot creation). No
/// active-first re-sort: switching accounts must not move the switched-to
/// card — the Active badge already says which one is live, and a list that
/// reshuffles under the click reads as broken. Pure.
pub fn provider_accounts(
    snapshot: &AgentAccountsSnapshot,
    harness: HarnessId,
) -> Vec<&AgentAccount> {
    snapshot
        .accounts
        .iter()
        .filter(|a| a.harness == harness)
        .collect()
}

// ---------------------------------------------------------------------------
// Entity
// ---------------------------------------------------------------------------

enum LoginFlow {
    /// Claude's native login is shared with other local clients. Replacing it
    /// requires an explicit disclosure before any provider flow starts.
    Disclosure { harness: HarnessId },
    /// StartAgentLogin in flight.
    Starting { harness: HarnessId },
    /// Claude-style: open the URL, paste the code back.
    PasteCode {
        harness: HarnessId,
        start: AgentLoginStart,
        submitting: bool,
        error: Option<SharedString>,
    },
    /// Codex-style: open the URL, poll until the browser flow lands.
    Browser {
        harness: HarnessId,
        start: AgentLoginStart,
        message: Option<SharedString>,
        error: Option<SharedString>,
    },
}

struct RenameFlow {
    account: AgentAccount,
    submitting: bool,
    error: Option<SharedString>,
}

impl LoginFlow {
    /// Provider-owned sign-in dialog title.
    fn title(&self) -> &'static str {
        let harness = match self {
            LoginFlow::Disclosure { harness }
            | LoginFlow::Starting { harness }
            | LoginFlow::PasteCode { harness, .. }
            | LoginFlow::Browser { harness, .. } => *harness,
        };
        match harness {
            HarnessId::Codex => "Sign in to Codex",
            _ => "Sign in to Claude Code",
        }
    }
}

pub struct AccountsPage {
    state: Entity<AppState>,
    scroll: widgets::PageScroll,
    /// Which device's logins are shown; `None` = this device (no passthrough).
    /// Retargeted by the page-header device switcher (luna parity: the
    /// accounts RPCs are relay-forwardable, CLI logins are per-device).
    target_device: Option<String>,
    device_menu: popover::Popup<()>,
    snapshot: Loadable<AgentAccountsSnapshot>,
    harnesses: Loadable<Vec<HarnessDescriptor>>,
    opencode_usage: Loadable<UsageSnapshot>,
    provider_updates: Loadable<Vec<ProviderUpdateStatus>>,
    harness_error: Option<String>,
    update_message: Option<String>,
    updating_provider: Option<HarnessId>,
    /// Account id with an in-flight Switch/Forget.
    busy_account: Option<String>,
    login: Option<LoginFlow>,
    rename: Option<RenameFlow>,
    error: Option<SharedString>,
    code_input: Entity<ComposerInput>,
    label_input: Entity<ComposerInput>,
    load_task: Option<Task<()>>,
    harness_task: Option<Task<()>>,
    usage_task: Option<Task<()>>,
    toggle_task: Option<Task<()>>,
    provider_update_check_task: Option<Task<()>>,
    provider_update_task: Option<Task<()>>,
    advanced_page: Option<Entity<HarnessesPage>>,
    advanced_open: bool,
    action_task: Option<Task<()>>,
    poll_task: Option<Task<()>>,
    _observe: Subscription,
    _code_events: Subscription,
    _label_events: Subscription,
}

impl AccountsPage {
    pub fn refresh_on_visit(&mut self, cx: &mut Context<Self>) {
        self.load(true, cx);
    }

    pub fn provider_update_statuses(&self) -> Option<Vec<ProviderUpdateStatus>> {
        self.provider_updates.ready().cloned()
    }

    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let mut page = Self::build(state, Loadable::Idle, cx);
        // Force the usage probe on the visit's first list — a plain list
        // returns no usage windows on a cold engine cache, which rendered
        // every account as "Usage unavailable" until a manual Refresh. The
        // Loading skeleton (meter ghosts) covers the probe latency, so
        // "Usage unavailable" is reserved for a probe that genuinely failed.
        page.load(force_usage_for(LoadTrigger::Mount), cx);
        page
    }

    #[cfg(feature = "appshots-fixture")]
    pub fn new_fixture(
        state: Entity<AppState>,
        snapshot: AgentAccountsSnapshot,
        cx: &mut Context<Self>,
    ) -> Self {
        // Deliberately bypasses `load`: native visual QA cannot probe a real
        // provider, usage endpoint, provider home, or Keychain.
        let mut page = Self::build(state, Loadable::Ready(snapshot), cx);
        page.harnesses = Loadable::Ready(
            PROVIDERS
                .into_iter()
                .map(|(id, name, _)| HarnessDescriptor {
                    id,
                    name: name.into(),
                    supports_steering: true,
                    steering_mode: luna_proto::SteeringMode::TurnBoundary,
                    reasoning_levels: vec![],
                    installed: true,
                    enabled: Some(true),
                })
                .collect(),
        );
        page.provider_updates = Loadable::Ready(vec![
            ProviderUpdateStatus {
                harness: HarnessId::Codex,
                installed_version: Some("0.154.0".into()),
                latest_version: Some("0.156.1".into()),
                update_available: true,
                update_command: Some("brew upgrade --cask codex".into()),
                detail: None,
            },
            ProviderUpdateStatus {
                harness: HarnessId::ClaudeCode,
                installed_version: Some("2.1.251".into()),
                latest_version: Some("2.1.251".into()),
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
        ]);
        page.opencode_usage = Loadable::Ready(UsageSnapshot {
            summary: luna_proto::UsageSummary {
                contract_version: luna_proto::USAGE_CONTRACT_VERSION,
                read_at_ms: Utc::now().timestamp_millis(),
                time_zone: "UTC".into(),
                days: 7,
                resolution: "daily".into(),
                buckets: vec![],
                sources: vec![],
                scan_duration_ms: 0,
                partial: false,
                warnings: vec![],
            },
            limits: vec![luna_proto::UsageLimitSnapshot {
                provider: UsageProvider::OpenCodeGo,
                verified_account_id: None,
                account_label: None,
                plan: Some("Go".into()),
                checked_at_ms: Some(Utc::now().timestamp_millis()),
                availability: LimitAvailability::Ready,
                windows: vec![luna_proto::UsageLimitWindow {
                    id: "week".into(),
                    label: "Week".into(),
                    used_percent: 44.0,
                    resets_at_ms: None,
                    duration_minutes: Some(7 * 24 * 60),
                }],
                reset_credits: None,
                detail: None,
            }],
        });
        page
    }

    fn build(
        state: Entity<AppState>,
        snapshot: Loadable<AgentAccountsSnapshot>,
        cx: &mut Context<Self>,
    ) -> Self {
        let observe = cx.observe(&state, |_, _, cx| cx.notify());
        let code_input = cx.new(|cx| ComposerInput::new("Paste the authorization code", cx));
        let code_events = cx.subscribe(&code_input, |this: &mut Self, _, event, cx| {
            if matches!(event, ComposerInputEvent::Submitted) {
                this.submit_code(cx);
            }
        });
        let label_input = cx.new(|cx| ComposerInput::new("Account label", cx));
        let label_events = cx.subscribe(&label_input, |this: &mut Self, _, event, cx| {
            if matches!(event, ComposerInputEvent::Submitted) {
                this.submit_rename(cx);
            }
        });
        Self {
            state,
            scroll: widgets::PageScroll::default(),
            target_device: None,
            device_menu: popover::Popup::default(),
            snapshot,
            harnesses: Loadable::Idle,
            opencode_usage: Loadable::Idle,
            provider_updates: Loadable::Idle,
            harness_error: None,
            update_message: None,
            updating_provider: None,
            busy_account: None,
            login: None,
            rename: None,
            error: None,
            code_input,
            label_input,
            load_task: None,
            harness_task: None,
            usage_task: None,
            toggle_task: None,
            provider_update_check_task: None,
            provider_update_task: None,
            advanced_page: None,
            advanced_open: false,
            action_task: None,
            poll_task: None,
            _observe: observe,
            _code_events: code_events,
            _label_events: label_events,
        }
    }

    /// Retarget the page at another device's logins: every accounts RPC is
    /// relay-forwardable, so the whole page — list, usage probes, switch,
    /// forget, login flows — follows the passthrough.
    fn close_device_menu(&mut self, cx: &mut Context<Self>) {
        if self.device_menu.begin_close() {
            popover::reap_popup(cx, |page: &mut Self| &mut page.device_menu);
            cx.notify();
        }
    }

    fn set_target_device(&mut self, target: Option<String>, cx: &mut Context<Self>) {
        self.close_device_menu(cx);
        if self.target_device == target {
            cx.notify();
            return;
        }
        self.target_device = target;
        if let Some(page) = &self.advanced_page {
            let target = self.target_device.clone();
            page.update(cx, |page, cx| page.set_target_device(target, cx));
        }
        // A different device = a different accounts world: drop in-flight
        // login/action state and reload with a forced usage probe (the new
        // device's cache is cold).
        self.login = None;
        self.rename = None;
        self.busy_account = None;
        self.error = None;
        self.update_message = None;
        self.load(force_usage_for(LoadTrigger::Mount), cx);
    }

    /// Params with the `targetDeviceId` passthrough merged in.
    fn params(&self, value: serde_json::Value) -> serde_json::Value {
        let mut value = value;
        if let (Some(target), Some(object)) = (&self.target_device, value.as_object_mut()) {
            object.insert("targetDeviceId".into(), serde_json::json!(target));
        }
        value
    }

    fn on_scroll_hovered(&mut self, hovered: &bool, _: &mut Window, cx: &mut Context<Self>) {
        if self.scroll.set_list_hovered(*hovered) {
            cx.notify();
        }
    }

    /// The page-header device switcher (luna device-switcher.tsx): a quiet
    /// trigger — platform glyph · name · presence dot · sort glyph — opening a
    /// dropdown of every registered device. Selecting one retargets the page.
    fn render_device_switcher(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        use crate::icons::{self, icon};
        let (mut devices, local_id) = {
            let s = self.state.read(cx);
            (s.devices.clone(), s.local_device_id.clone())
        };
        // Stable row order (registration time, then id) — luna's switcher
        // sorts the same way so rows never reshuffle on heartbeats.
        devices.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        let effective = self.target_device.clone().or_else(|| local_id.clone());
        let selected = devices
            .iter()
            .find(|d| Some(d.id.as_str()) == effective.as_deref())
            .cloned();
        let platform_glyph = |platform: &str| match platform {
            "macos" | "darwin" => icons::LAPTOP,
            "ios" | "android" => icons::SMARTPHONE,
            _ => icons::MONITOR,
        };
        let trigger_glyph = platform_glyph(
            selected
                .as_ref()
                .map(|d| d.platform.as_str())
                .unwrap_or("macos"),
        );
        let trigger_label: SharedString = selected
            .as_ref()
            .map(|d| d.name.clone().into())
            .unwrap_or_else(|| SharedString::from("This device"));
        let emerald = theme.success;
        let open = self.device_menu.is_open();

        let mut trigger =
            div()
                .id("accounts-device-switcher")
                .flex_none()
                .h(px(28.0))
                .px(px(8.0))
                .rounded(px(6.0))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(6.0))
                .cursor_pointer()
                .bg(if open {
                    crate::theme::ink(0.06)
                } else {
                    gpui::transparent_black()
                })
                .when(!open, |el| el.hover(|s| s.bg(crate::theme::ink(0.04))))
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(|this, _, _, _| this.device_menu.note_trigger_press()),
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    // A press that found the menu open closes it (the card's
                    // mouse-down-out already began the close) — never reopen.
                    if this.device_menu.take_press_was_open() {
                        this.close_device_menu(cx);
                    } else {
                        this.device_menu.open(());
                    }
                    cx.notify();
                }))
                .child(
                    icon(trigger_glyph)
                        .size(px(16.0))
                        .flex_none()
                        .text_color(theme.text_muted),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(crate::typography::ui_rems(12.5))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(theme.text)
                        .child(trigger_label),
                )
                .child(div().size(px(6.0)).rounded_full().flex_none().bg(
                    if effective == local_id {
                        emerald
                    } else {
                        crate::theme::ink(0.2)
                    },
                ))
                .child(
                    icon(icons::SORT_VERTICAL)
                        .size(px(14.0))
                        .flex_none()
                        .text_color(theme.text_muted.opacity(if open { 0.9 } else { 0.4 })),
                );

        if self.device_menu.get().is_some() {
            let closing = self.device_menu.closing_since();
            let menu = popover::popover_card(theme)
                .w(px(220.0))
                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                    this.close_device_menu(cx);
                }))
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(popover::menu_heading(theme, "Devices"))
                .children(devices.into_iter().enumerate().map(|(ix, d)| {
                    let is_active = Some(d.id.as_str()) == effective.as_deref();
                    let is_local = local_id.as_deref() == Some(d.id.as_str());
                    let glyph = platform_glyph(&d.platform);
                    let name: SharedString = d.name.clone().into();
                    let pick_local = is_local;
                    let pick_id = d.id.clone();
                    popover::menu_row(theme, is_active, format!("accounts-device-row-{ix}"))
                        .id(("accounts-device-row", ix))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            // Local device = no passthrough (calls stay direct).
                            let target = (!pick_local).then(|| pick_id.clone());
                            this.set_target_device(target, cx);
                        }))
                        .child(
                            icon(glyph)
                                .size(px(16.0))
                                .flex_none()
                                .text_color(theme.text_muted),
                        )
                        .child(div().flex_1().min_w_0().truncate().child(name))
                        .when(is_local, |el| {
                            el.child(
                                div()
                                    .flex_none()
                                    .text_size(crate::typography::ui_rems(10.5))
                                    .text_color(theme.text_muted.opacity(0.35))
                                    .child(SharedString::from("You")),
                            )
                        })
                        .child(
                            div()
                                .size(px(6.0))
                                .rounded_full()
                                .flex_none()
                                .bg(if is_local {
                                    emerald
                                } else {
                                    crate::theme::ink(0.2)
                                }),
                        )
                }))
                .into_any_element();
            trigger = trigger.child(popover::anchored_menu(
                "accounts-device-menu",
                menu,
                closing,
            ));
        }
        trigger.into_any_element()
    }

    fn load(&mut self, force_usage: bool, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.snapshot = Loadable::Error("Engine not connected".into());
            return;
        };
        self.snapshot = Loadable::Loading;
        self.load_harnesses(cx);
        self.load_opencode_usage(cx);
        self.load_provider_updates(force_usage, cx);
        let params = self.params(serde_json::json!({ "forceUsage": force_usage }));
        self.load_task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(methods::LIST_AGENT_ACCOUNTS, params)
                .await;
            this.update(cx, |page, cx| {
                page.snapshot = match result {
                    Ok(value) => match serde_json::from_value::<AgentAccountsSnapshot>(value) {
                        Ok(snapshot) => Loadable::Ready(snapshot),
                        Err(err) => Loadable::Error(err.to_string()),
                    },
                    Err(err) => Loadable::Error(err.to_string()),
                };
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn load_harnesses(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.harnesses = Loadable::Error("Engine not connected".into());
            return;
        };
        self.harnesses = Loadable::Loading;
        let params = self.params(serde_json::json!({}));
        self.harness_task = Some(cx.spawn(async move |this, cx| {
            let result = engine.client().call(methods::LIST_HARNESSES, params).await;
            this.update(cx, |page, cx| {
                page.harnesses = match result {
                    Ok(value) => serde_json::from_value::<Vec<HarnessDescriptor>>(value)
                        .map(Loadable::Ready)
                        .unwrap_or_else(|error| Loadable::Error(error.to_string())),
                    Err(error) => Loadable::Error(error.to_string()),
                };
                cx.notify();
            })
            .ok();
        }));
    }

    fn load_opencode_usage(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.opencode_usage = Loadable::Error("Engine not connected".into());
            return;
        };
        self.opencode_usage = Loadable::Loading;
        let params = self.params(serde_json::json!({"days": 7, "priceOverrides": {}}));
        self.usage_task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(methods::GET_USAGE_SNAPSHOT, params)
                .await;
            this.update(cx, |page, cx| {
                page.opencode_usage = match result {
                    Ok(value) => serde_json::from_value::<UsageSnapshot>(value)
                        .map(Loadable::Ready)
                        .unwrap_or_else(|error| Loadable::Error(error.to_string())),
                    Err(error) => Loadable::Error(error.to_string()),
                };
                cx.notify();
            })
            .ok();
        }));
    }

    fn toggle_harness(&mut self, harness: HarnessId, enabled: bool, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let params = self.params(serde_json::json!({
            "harness": harness,
            "enabled": enabled,
        }));
        self.harness_error = None;
        self.toggle_task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(methods::SET_HARNESS_ENABLED, params)
                .await;
            this.update(cx, |page, cx| {
                match result {
                    Ok(value) => match serde_json::from_value::<Vec<HarnessDescriptor>>(value) {
                        Ok(list) => {
                            page.harnesses = Loadable::Ready(list);
                            crate::pickers::bump_harness_catalog(cx);
                            page.load_opencode_usage(cx);
                        }
                        Err(error) => page.harness_error = Some(error.to_string()),
                    },
                    Err(error) => page.harness_error = Some(error.to_string()),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn load_provider_updates(&mut self, force: bool, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.provider_updates = Loadable::Error("Engine not connected".into());
            return;
        };
        self.provider_updates = Loadable::Loading;
        let params = self.params(serde_json::json!({"force": force}));
        self.provider_update_check_task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(methods::GET_PROVIDER_UPDATES, params)
                .await;
            this.update(cx, |page, cx| {
                page.provider_updates = match result {
                    Ok(value) => serde_json::from_value::<Vec<ProviderUpdateStatus>>(value)
                        .map(Loadable::Ready)
                        .unwrap_or_else(|error| Loadable::Error(error.to_string())),
                    Err(error) => Loadable::Error(error.to_string()),
                };
                cx.notify();
            })
            .ok();
        }));
    }

    fn update_provider(&mut self, harness: HarnessId, cx: &mut Context<Self>) {
        if self.updating_provider.is_some() {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.updating_provider = Some(harness);
        self.update_message = None;
        let params = self.params(serde_json::json!({"harness": harness}));
        self.provider_update_task = Some(cx.spawn(async move |this, cx| {
            let result = engine.client().call(methods::UPDATE_PROVIDER, params).await;
            this.update(cx, |page, cx| {
                page.updating_provider = None;
                page.update_message = Some(match result {
                    Ok(value) => match serde_json::from_value::<ProviderUpdateResult>(value) {
                        Ok(updated) => {
                            crate::pickers::bump_harness_catalog(cx);
                            page.load_harnesses(cx);
                            page.load_provider_updates(true, cx);
                            let runtime = updated.runtime_reload_error.map_or_else(
                                || "Idle provider runtimes reloaded.".to_owned(),
                                |error| format!("Runtime reload deferred: {error}"),
                            );
                            match updated.model_refresh_error {
                                Some(error) => format!(
                                    "CLI updated to {}. {runtime} Model refresh failed: {error}",
                                    updated.installed_version
                                ),
                                None => format!(
                                    "CLI updated to {}. {runtime} Models refreshed.",
                                    updated.installed_version
                                ),
                            }
                        }
                        Err(error) => error.to_string(),
                    },
                    Err(error) => error.to_string(),
                });
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn toggle_advanced(&mut self, cx: &mut Context<Self>) {
        self.advanced_open = !self.advanced_open;
        if self.advanced_open && self.advanced_page.is_none() {
            let state = self.state.clone();
            let page = cx.new(|cx| HarnessesPage::new(state, cx));
            if self.target_device.is_some() {
                let target = self.target_device.clone();
                page.update(cx, |page, cx| page.set_target_device(target, cx));
            }
            self.advanced_page = Some(page);
        }
        cx.notify();
    }

    /// Switch / Forget an account.
    fn account_action(
        &mut self,
        method: &'static str,
        account: &AgentAccount,
        cx: &mut Context<Self>,
    ) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.busy_account = Some(account.id.clone());
        self.error = None;
        // Tolerant param shape: both `id` and `accountId` plus the harness.
        let params = self.params(serde_json::json!({
            "id": account.id,
            "accountId": account.id,
            "harness": account.harness,
        }));
        self.action_task = Some(cx.spawn(async move |this, cx| {
            let result = engine.client().call(method, params).await;
            this.update(cx, |page, cx| {
                page.busy_account = None;
                match result {
                    Ok(_) => page.load(force_usage_for(LoadTrigger::PostAction), cx),
                    Err(err) => page.error = Some(format!("{err}").into()),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn begin_rename(&mut self, account: AgentAccount, cx: &mut Context<Self>) {
        let label = account
            .label
            .clone()
            .unwrap_or_else(|| "Codex account".into());
        self.label_input
            .update(cx, |input, cx| input.set_text(&label, cx));
        self.rename = Some(RenameFlow {
            account,
            submitting: false,
            error: None,
        });
        cx.notify();
    }

    fn cancel_rename(&mut self, cx: &mut Context<Self>) {
        self.rename = None;
        cx.notify();
    }

    fn submit_rename(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let Some(rename) = self.rename.as_mut() else {
            return;
        };
        if rename.submitting {
            return;
        }
        let label = self.label_input.read(cx).text().trim().to_string();
        if label.is_empty() || label.chars().count() > 64 {
            rename.error = Some("Use a label between 1 and 64 characters.".into());
            cx.notify();
            return;
        }
        rename.submitting = true;
        rename.error = None;
        let account_id = rename.account.id.clone();
        let harness = rename.account.harness;
        let params = self.params(serde_json::json!({
            "accountId": account_id,
            "harness": harness,
            "label": label,
        }));
        self.action_task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(methods::RENAME_AGENT_ACCOUNT, params)
                .await;
            this.update(cx, |page, cx| {
                match result {
                    Ok(value) => match serde_json::from_value::<AgentAccountsSnapshot>(value) {
                        Ok(snapshot) => {
                            page.snapshot = Loadable::Ready(snapshot);
                            page.rename = None;
                        }
                        Err(error) => {
                            if let Some(rename) = page.rename.as_mut() {
                                rename.submitting = false;
                                rename.error = Some(error.to_string().into());
                            }
                        }
                    },
                    Err(error) => {
                        if let Some(rename) = page.rename.as_mut() {
                            rename.submitting = false;
                            rename.error = Some(error.to_string().into());
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    // ---- add-account flows ----

    fn request_login(
        &mut self,
        harness: HarnessId,
        replacing_existing: bool,
        cx: &mut Context<Self>,
    ) {
        if harness == HarnessId::ClaudeCode && replacing_existing {
            self.login = Some(LoginFlow::Disclosure { harness });
            self.error = None;
            cx.notify();
        } else {
            self.start_login(harness, cx);
        }
    }

    fn start_login(&mut self, harness: HarnessId, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.login = Some(LoginFlow::Starting { harness });
        self.error = None;
        let params = self.params(serde_json::json!({ "harness": harness }));
        self.action_task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(methods::START_AGENT_LOGIN, params)
                .await;
            this.update(cx, |page, cx| {
                match result.and_then(|value| {
                    serde_json::from_value::<AgentLoginStart>(value)
                        .map_err(|e| luna_rpc::RpcError::Failed(e.to_string()))
                }) {
                    Ok(start) => {
                        cx.open_url(&start.url);
                        match start.mode {
                            AgentLoginMode::PasteCode => {
                                page.code_input
                                    .update(cx, |input, cx| input.set_text("", cx));
                                page.login = Some(LoginFlow::PasteCode {
                                    harness,
                                    start,
                                    submitting: false,
                                    error: None,
                                });
                            }
                            AgentLoginMode::Browser => {
                                page.login = Some(LoginFlow::Browser {
                                    harness,
                                    start,
                                    message: None,
                                    error: None,
                                });
                                page.spawn_poll(cx);
                            }
                        }
                    }
                    Err(err) => {
                        page.login = None;
                        page.error = Some(format!("Login failed to start: {err}").into());
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn submit_code(&mut self, cx: &mut Context<Self>) {
        let Some(LoginFlow::PasteCode {
            start, submitting, ..
        }) = &mut self.login
        else {
            return;
        };
        if *submitting {
            return;
        }
        let code = self.code_input.read(cx).text().trim().to_string();
        if code.is_empty() {
            return;
        }
        let login_id = start.login_id.clone();
        *submitting = true;
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let params = self.params(serde_json::json!({ "loginId": login_id, "code": code }));
        self.action_task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(methods::COMPLETE_AGENT_LOGIN, params)
                .await;
            this.update(cx, |page, cx| {
                match result {
                    Ok(_) => {
                        page.login = None;
                        page.load(force_usage_for(LoadTrigger::PostLogin), cx);
                    }
                    Err(err) => {
                        if let Some(LoginFlow::PasteCode {
                            submitting, error, ..
                        }) = &mut page.login
                        {
                            *submitting = false;
                            *error = Some(format!("{err}").into());
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// The browser-wait poll loop: PollAgentLogin every 1.5s until Done/Error.
    fn spawn_poll(&mut self, cx: &mut Context<Self>) {
        let Some(LoginFlow::Browser { start, .. }) = &self.login else {
            return;
        };
        let login_id = start.login_id.clone();
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let params = self.params(serde_json::json!({ "loginId": login_id }));
        self.poll_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(1500))
                    .await;
                let result = engine
                    .client()
                    .call(methods::POLL_AGENT_LOGIN, params.clone())
                    .await;
                let outcome = this.update(cx, |page, cx| {
                    let Some(LoginFlow::Browser { message, error, .. }) = &mut page.login else {
                        return true; // dialog dismissed — stop polling
                    };
                    match result.as_ref().ok().and_then(|value| {
                        serde_json::from_value::<AgentLoginPoll>(value.clone()).ok()
                    }) {
                        Some(poll) => match poll.status {
                            AgentLoginStatus::Done => {
                                page.login = None;
                                page.load(force_usage_for(LoadTrigger::PostLogin), cx);
                                cx.notify();
                                true
                            }
                            AgentLoginStatus::Error => {
                                *error = Some(
                                    poll.message
                                        .unwrap_or_else(|| "Login failed".to_string())
                                        .into(),
                                );
                                cx.notify();
                                true
                            }
                            AgentLoginStatus::Pending => {
                                if let Some(text) = poll.message {
                                    *message = Some(text.into());
                                }
                                cx.notify();
                                false
                            }
                        },
                        None => {
                            let text = match &result {
                                Err(err) => format!("Poll failed: {err}"),
                                Ok(_) => "Poll failed: malformed reply".to_string(),
                            };
                            *error = Some(text.into());
                            cx.notify();
                            true
                        }
                    }
                });
                match outcome {
                    Ok(true) | Err(_) => break,
                    Ok(false) => {}
                }
            }
        }));
    }

    fn cancel_login(&mut self, cx: &mut Context<Self>) {
        let login_id = match &self.login {
            Some(LoginFlow::PasteCode { start, .. }) | Some(LoginFlow::Browser { start, .. }) => {
                Some(start.login_id.clone())
            }
            _ => None,
        };
        self.login = None;
        self.poll_task = None;
        if let (Some(login_id), Some(engine)) = (login_id, self.state.read(cx).engine().cloned()) {
            let params = self.params(serde_json::json!({ "loginId": login_id }));
            self.action_task = Some(cx.spawn(async move |_, _| {
                if let Err(err) = engine
                    .client()
                    .call(methods::CANCEL_AGENT_LOGIN, params)
                    .await
                {
                    tracing::debug!(error = %err, "CancelAgentLogin failed (best-effort)");
                }
            }));
        }
        cx.notify();
    }

    // ---- render pieces ----

    /// One usage window (luna settings.agents.tsx `UsageMeter`): label ·
    /// 5px rounded-full bar (indigo → amber ≥80% → red ≥95%) · "NN% used" ·
    /// quiet reset time.
    fn render_usage_meter(
        &self,
        window: &luna_proto::AgentUsageWindow,
        theme: &Theme,
        now: DateTime<Utc>,
    ) -> AnyElement {
        let fraction = window.used_fraction.clamp(0.0, 1.0);
        let level = usage_level(fraction);
        let fill = usage_color(level, theme).opacity(match level {
            UsageLevel::Normal => 0.8,
            _ => 0.85,
        });
        let reset = format_reset(window.resets_at, now);
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .text_size(crate::typography::ui_rems(11.5))
            .text_color(theme.text_muted.opacity(0.7))
            .child(
                div()
                    .w(px(48.0))
                    .flex_none()
                    .truncate()
                    .child(SharedString::from(window.label.clone())),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(56.0))
                    .max_w(px(230.0))
                    .h(px(5.0))
                    .rounded_full()
                    .overflow_hidden()
                    .bg(crate::theme::ink(0.07))
                    .when(fraction > 0.0, |el| {
                        el.child(
                            div()
                                .h_full()
                                // A 1.5% floor keeps tiny non-zero usage
                                // visible (luna `max(used, 1.5)%`).
                                .w(gpui::relative(fraction.max(0.015)))
                                .rounded_full()
                                .bg(fill),
                        )
                    }),
            )
            .child(
                div()
                    .w(px(64.0))
                    .flex_none()
                    .text_right()
                    .child(SharedString::from(format!(
                        "{}% used",
                        (fraction * 100.0).round() as u32
                    ))),
            )
            .when_some(reset, |el, reset| {
                el.child(
                    div()
                        .flex_none()
                        .truncate()
                        .text_color(theme.text_muted.opacity(0.45))
                        .child(SharedString::from(reset)),
                )
            })
            .into_any_element()
    }

    /// One account row (luna settings.agents.tsx `AccountRow`): initial
    /// avatar, email + usage meters left; badges over the Switch/Forget
    /// actions right-anchored.
    fn render_account_row(
        &self,
        account: &AgentAccount,
        ix: usize,
        first: bool,
        theme: &Theme,
        now: DateTime<Utc>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let is_busy = self.busy_account.as_deref() == Some(account.id.as_str());
        let identity: SharedString = account
            .email
            .clone()
            .or_else(|| account.display_name.clone())
            .unwrap_or_else(|| "Unknown account".into())
            .into();
        let title: SharedString = account
            .label
            .clone()
            .unwrap_or_else(|| identity.to_string())
            .into();
        let initial: SharedString = title
            .chars()
            .next()
            .map(|c| c.to_uppercase().to_string())
            .unwrap_or_else(|| "?".into())
            .into();
        let action_account = account.clone();
        let rename_account = account.clone();

        let badges = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .when(account.active, |el| {
                el.child(widgets::badge_active(theme, "Active"))
            })
            .when(
                account.harness == HarnessId::ClaudeCode && !account.active,
                |el| el.child(widgets::badge(theme, "Disconnected")),
            )
            .when(
                account.harness == HarnessId::Codex && !account.active,
                |el| el.child(widgets::badge(theme, "Not connected")),
            )
            .when(
                account.authentication == AgentAuthenticationState::Expired,
                |el| el.child(widgets::badge(theme, "Expired")),
            )
            .when(
                account.authentication == AgentAuthenticationState::Unavailable
                    && !(account.harness == HarnessId::ClaudeCode && !account.active),
                |el| el.child(widgets::badge(theme, "Unavailable")),
            )
            .when_some(account.plan_label.clone(), |el, plan| {
                el.child(widgets::badge(theme, plan))
            });

        let action = match account_lifecycle_action(account) {
            Some(AccountLifecycleAction::DisconnectClaude) => Some((
                methods::FORGET_AGENT_ACCOUNT,
                if is_busy {
                    "Disconnecting…"
                } else {
                    "Disconnect"
                },
                "account-disconnect",
            )),
            Some(AccountLifecycleAction::ReconnectClaude) => Some((
                methods::ACTIVATE_AGENT_ACCOUNT,
                if is_busy {
                    "Reconnecting…"
                } else {
                    "Reconnect"
                },
                "account-reconnect",
            )),
            Some(AccountLifecycleAction::DisconnectCodex) => Some((
                methods::FORGET_AGENT_ACCOUNT,
                if is_busy {
                    "Disconnecting…"
                } else {
                    "Disconnect"
                },
                "account-disconnect-codex",
            )),
            Some(AccountLifecycleAction::SelectCodexDefault) => Some((
                methods::ACTIVATE_AGENT_ACCOUNT,
                if is_busy { "Connecting…" } else { "Connect" },
                "account-select-default",
            )),
            None => None,
        };
        let lifecycle_action = action.map(|(method, label, id)| {
            div().child(
                crate::popover::btn_primary(theme, label)
                    .id((id, ix))
                    .px(px(8.0))
                    .py(px(4.0))
                    .rounded(px(6.0))
                    .text_size(crate::typography::ui_rems(11.5))
                    .when(is_busy, |el| el.opacity(0.5))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.account_action(method, &action_account, cx);
                    })),
            )
        });
        let rename_action = (account.harness == HarnessId::Codex && account.id != "codex-native")
            .then(|| {
                crate::popover::btn_ghost(theme, "Rename", "account-rename")
                    .id(("account-rename", ix))
                    .px(px(8.0))
                    .py(px(4.0))
                    .rounded(px(6.0))
                    .text_size(crate::typography::ui_rems(11.5))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.begin_rename(rename_account.clone(), cx);
                    }))
            });

        div()
            .px(px(20.0))
            .py(px(14.0))
            .when(!first, |el| el.border_t_1().border_color(theme.border))
            .flex()
            .flex_row()
            .items_stretch()
            .gap(px(12.0))
            .child(
                // Initial avatar: size-8 rounded-full border bg-white/[0.03].
                div()
                    .flex_none()
                    .self_center()
                    .size(px(32.0))
                    .rounded_full()
                    .border_1()
                    .border_color(theme.border)
                    .bg(crate::theme::ink(0.03))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(crate::typography::ui_rems(12.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme.text_muted)
                    .child(initial),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(widgets::row_title(theme, title))
                    .when(account.label.is_some(), |el| {
                        el.child(
                            div()
                                .mt(px(2.0))
                                .truncate()
                                .text_size(crate::typography::ui_rems(11.5))
                                .text_color(theme.text_muted.opacity(0.7))
                                .child(identity),
                        )
                    })
                    .map(|el| {
                        // Meters XOR the quiet fallback line — never both
                        // (luna: `usage ? meters : "Usage unavailable"…`).
                        if account.usage_windows.is_empty() {
                            el.child(
                                div()
                                    .mt(px(6.0))
                                    .truncate()
                                    .text_size(crate::typography::ui_rems(11.5))
                                    .text_color(theme.text_muted.opacity(0.6))
                                    .child(SharedString::from("Usage unavailable")),
                            )
                        } else {
                            el.child(
                                div().mt(px(6.0)).flex().flex_col().gap(px(4.0)).children(
                                    account
                                        .usage_windows
                                        .iter()
                                        .map(|w| self.render_usage_meter(w, theme, now)),
                                ),
                            )
                        }
                    }),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .items_end()
                    .justify_between()
                    .gap(px(8.0))
                    .child(badges)
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(6.0))
                            .children(rename_action)
                            .children(lifecycle_action),
                    ),
            )
            .into_any_element()
    }

    fn render_opencode_usage(&self, theme: &Theme) -> AnyElement {
        let mut body = div()
            .px(px(20.0))
            .py(px(16.0))
            .flex()
            .flex_col()
            .gap(px(8.0))
            .text_size(crate::typography::ui_rems(12.0))
            .text_color(theme.text_muted);
        match &self.opencode_usage {
            Loadable::Ready(snapshot) => {
                let limits: Vec<_> = snapshot
                    .limits
                    .iter()
                    .filter(|limit| limit.provider == UsageProvider::OpenCodeGo)
                    .collect();
                if limits.is_empty() {
                    body = body.child(
                        "OpenCode uses its CLI connection. No subscription usage is available.",
                    );
                }
                for limit in limits {
                    if let Some(plan) = &limit.plan {
                        body = body.child(format!("OpenCode Go · {plan}"));
                    }
                    if limit.availability != LimitAvailability::Ready {
                        body = body.child(
                            limit
                                .detail
                                .clone()
                                .unwrap_or_else(|| "Usage unavailable".into()),
                        );
                    }
                    for window in &limit.windows {
                        let fraction = (window.used_percent / 100.0).clamp(0.0, 1.0);
                        let color = usage_color(usage_level(fraction), theme);
                        body = body.child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(5.0))
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .child(window.label.clone())
                                        .child(format!(
                                            "{}% used",
                                            window.used_percent.round() as u32
                                        )),
                                )
                                .child(
                                    div()
                                        .h(px(5.0))
                                        .rounded_full()
                                        .overflow_hidden()
                                        .bg(crate::theme::ink(0.07))
                                        .child(
                                            div()
                                                .h_full()
                                                .w(gpui::relative(fraction))
                                                .rounded_full()
                                                .bg(color),
                                        ),
                                ),
                        );
                    }
                }
            }
            Loadable::Loading | Loadable::Idle => {
                body = body.child("Loading OpenCode usage…");
            }
            Loadable::Error(message) => {
                body = body.child(format!("OpenCode usage unavailable: {message}"));
            }
        }
        body.into_any_element()
    }

    fn render_login_dialog(
        &mut self,
        viewport: gpui::Size<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let theme = Theme::of(cx).for_popup();
        let red_text = theme.danger_muted.opacity(0.9); // red-300
        let login = self.login.as_ref()?;
        let title = login.title();
        let url_link =
            |id: &'static str, label: &'static str, url: &str, cx: &mut Context<Self>| {
                let open_url = url.to_string();
                // "Reopen the …" text link (luna: `text-[12px]
                // text-muted-foreground/60 hover:underline`).
                div()
                    .id(id)
                    .mt(px(6.0))
                    .text_size(crate::typography::ui_rems(12.0))
                    .text_color(theme.text_muted)
                    .truncate()
                    .cursor_pointer()
                    .hover(|s| s.text_color(theme.text))
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.open_url(&open_url);
                    }))
                    .child(SharedString::from(label))
            };
        let body: AnyElement = match login {
            LoginFlow::Disclosure { harness } => {
                let harness = *harness;
                div()
                    .flex()
                    .flex_col()
                    .child(div().mt(px(8.0)).child(popover::dialog_body(
                        &theme,
                        "Claude Code has one native login on this device. Signing in to another account replaces that shared provider login and can affect other Claude Code clients. LUNA will keep local chats and session ids, but it does not isolate or copy Claude credentials.",
                    )))
                    .child(
                        div()
                            .mt(px(16.0))
                            .flex()
                            .flex_row()
                            .justify_end()
                            .gap(px(8.0))
                            .child(
                                popover::btn_ghost(&theme, "Cancel", "login-disclosure-cancel")
                                    .id("login-disclosure-cancel")
                                    .on_click(cx.listener(|this, _, _, cx| this.cancel_login(cx))),
                            )
                            .child(
                                popover::btn_primary(&theme, "Continue",)
                                    .id("login-disclosure-continue")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.start_login(harness, cx);
                                    })),
                            ),
                    )
                    .into_any_element()
            }
            LoginFlow::Starting { .. } => div()
                .mt(px(8.0))
                .child(popover::skeleton_rows(
                    "login-starting",
                    &theme,
                    2,
                    cx.entity_id(),
                    cx,
                ))
                .into_any_element(),
            LoginFlow::PasteCode {
                start,
                submitting,
                error,
                ..
            } => {
                let submitting = *submitting;
                div()
                    .flex()
                    .flex_col()
                    .child(div().mt(px(8.0)).child(popover::dialog_body(
                        &theme,
                        "A browser window opened. Sign in to Claude Code, approve access, then \
                         paste the code Anthropic shows you below. Claude Code saves the login \
                         in its own shared credential store; LUNA keeps no copy. Completing this \
                         replaces the native login for other local Claude Code clients too.",
                    )))
                    .child(url_link(
                        "login-open-url",
                        "Reopen the authorization page",
                        &start.url,
                        cx,
                    ))
                    .child(
                        div().mt(px(12.0)).child(
                            popover::dialog_field(self.code_input.clone().into_any_element())
                                .font_family(theme.font_mono.clone())
                                .text_size(crate::typography::ui_rems(13.0)),
                        ),
                    )
                    .when_some(error.clone(), |el, message| {
                        el.child(
                            div()
                                .mt(px(8.0))
                                .text_size(crate::typography::ui_rems(12.0))
                                .text_color(red_text)
                                .child(message),
                        )
                    })
                    .child(
                        div()
                            .mt(px(16.0))
                            .flex()
                            .flex_row()
                            .justify_end()
                            .gap(px(8.0))
                            .child(
                                popover::btn_ghost(&theme, "Cancel", "login-cancel")
                                    .id("login-cancel")
                                    .on_click(cx.listener(|this, _, _, cx| this.cancel_login(cx))),
                            )
                            .child(
                                popover::btn_primary(
                                    &theme,
                                    if submitting {
                                        "Verifying…"
                                    } else {
                                        "Sign in"
                                    },
                                )
                                .id("login-submit-code")
                                .when(submitting, |el| el.opacity(0.5))
                                .on_click(cx.listener(|this, _, _, cx| this.submit_code(cx))),
                            ),
                    )
                    .into_any_element()
            }
            LoginFlow::Browser {
                harness: _,
                start,
                message,
                error,
            } => {
                let has_error = error.is_some();
                let body = "Finish signing in to OpenAI in your browser. The Codex CLI saves the \
                    login in a private auth overlay; the canonical Codex sessions and config \
                    remain shared, and LUNA keeps no token copy.";
                div()
                    .flex()
                    .flex_col()
                    .child(div().mt(px(8.0)).child(popover::dialog_body(&theme, body)))
                    .child(url_link(
                        "login-open-url-browser",
                        "Reopen the sign-in page",
                        &start.url,
                        cx,
                    ))
                    .when(!has_error, |el| {
                        el.child(
                            div()
                                .mt(px(16.0))
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(8.0))
                                .child(crate::loaders::gradient_spinner(
                                    "login-poll",
                                    &theme,
                                    3.0,
                                    cx.entity_id(),
                                    cx,
                                ))
                                .child(
                                    div()
                                        .text_size(crate::typography::ui_rems(12.5))
                                        .text_color(theme.text_muted)
                                        .child(message.clone().unwrap_or_else(|| {
                                            SharedString::from("Waiting for the browser…")
                                        })),
                                ),
                        )
                    })
                    .when_some(error.clone(), |el, message| {
                        el.child(
                            div()
                                .mt(px(12.0))
                                .text_size(crate::typography::ui_rems(12.0))
                                .text_color(red_text)
                                .child(message),
                        )
                    })
                    .child(
                        div().mt(px(16.0)).flex().flex_row().justify_end().child(
                            popover::btn_ghost(
                                &theme,
                                if has_error { "Close" } else { "Cancel" },
                                "login-cancel",
                            )
                            .id("login-cancel")
                            .on_click(cx.listener(|this, _, _, cx| this.cancel_login(cx))),
                        ),
                    )
                    .into_any_element()
            }
        };
        let card = popover::dialog_card(&theme)
            .child(popover::dialog_title(&theme, title))
            .child(body)
            .into_any_element();
        Some(popover::modal("add-account-dialog", viewport, card))
    }

    fn render_rename_dialog(
        &mut self,
        viewport: gpui::Size<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let theme = Theme::of(cx).for_popup();
        let rename = self.rename.as_ref()?;
        let submitting = rename.submitting;
        let error = rename.error.clone();
        let identity: SharedString = rename
            .account
            .email
            .clone()
            .or_else(|| rename.account.display_name.clone())
            .unwrap_or_else(|| "Provider identity unavailable".into())
            .into();
        let card = popover::dialog_card(&theme)
            .child(popover::dialog_title(&theme, "Rename Codex account"))
            .child(
                div().mt(px(8.0)).child(popover::dialog_body(
                    &theme,
                    "This label is stored by LUNA for your account picker. It does not change the OpenAI account name or email.",
                )),
            )
            .child(
                div()
                    .mt(px(6.0))
                    .text_size(crate::typography::ui_rems(11.5))
                    .text_color(theme.text_muted)
                    .child(identity),
            )
            .child(
                div().mt(px(12.0)).child(
                    popover::dialog_field(self.label_input.clone().into_any_element())
                        .text_size(crate::typography::ui_rems(13.0)),
                ),
            )
            .when_some(error, |el, message| {
                el.child(
                    div()
                        .mt(px(8.0))
                        .text_size(crate::typography::ui_rems(12.0))
                        .text_color(theme.danger_muted)
                        .child(message),
                )
            })
            .child(
                div()
                    .mt(px(16.0))
                    .flex()
                    .justify_end()
                    .gap(px(8.0))
                    .child(
                        popover::btn_ghost(&theme, "Cancel", "rename-account-cancel")
                            .id("rename-account-cancel")
                            .on_click(cx.listener(|this, _, _, cx| this.cancel_rename(cx))),
                    )
                    .child(
                        popover::btn_primary(
                            &theme,
                            if submitting { "Saving…" } else { "Save" },
                        )
                        .id("rename-account-save")
                        .when(submitting, |el| el.opacity(0.5))
                        .on_click(cx.listener(|this, _, _, cx| this.submit_rename(cx))),
                    ),
            )
            .into_any_element();
        Some(popover::modal("rename-account-dialog", viewport, card))
    }

    /// A ghost account row (luna settings.agents.tsx `SkeletonRow`): avatar,
    /// email line, two usage-meter ghosts, a badge — same geometry as the real
    /// row so loaded data lands without a layout jump. `dim` fades row two.
    fn render_skeleton_row(
        &self,
        _id: (&'static str, usize),
        dim: bool,
        first: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        use crate::motion;
        let delta = motion::pulse_delta(&motion::LUNA_PULSE, cx.entity_id(), cx);
        let ghost = |w: gpui::Length, h: f32, round_full: bool| {
            div()
                .w(w)
                .h(px(h))
                .flex_none()
                .map(|el| {
                    if round_full {
                        el.rounded_full()
                    } else {
                        el.rounded(px(4.0))
                    }
                })
                .bg(crate::theme::ink(0.05))
        };
        let meters = div()
            .mt(px(8.0))
            .flex()
            .flex_col()
            .gap(px(7.0))
            .children((0..2).map(|_| {
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .child(ghost(px(48.0).into(), 9.0, false))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(56.0))
                            .max_w(px(230.0))
                            .h(px(5.0))
                            .rounded_full()
                            .bg(crate::theme::ink(0.04)),
                    )
                    .child(ghost(px(64.0).into(), 9.0, false))
            }));
        let inner = div()
            .flex()
            .flex_row()
            .items_stretch()
            .gap(px(12.0))
            .child(
                div()
                    .flex_none()
                    .self_center()
                    .size(px(32.0))
                    .rounded_full()
                    .bg(crate::theme::ink(0.05)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(ghost(px(176.0).into(), 13.0, false).max_w(gpui::relative(0.6)))
                    .child(meters),
            )
            .child(div().flex_none().flex().flex_col().items_end().child(ghost(
                px(64.0).into(),
                21.0,
                true,
            )));
        div()
            .px(px(20.0))
            .py(px(14.0))
            .when(!first, |el| el.border_t_1().border_color(theme.border))
            .when(dim, |el| el.opacity(0.6))
            .child(inner.opacity(0.55 + 0.35 * motion::pulse_wave(delta)))
            .into_any_element()
    }
}

impl popover::ScrollRailHost for AccountsPage {
    fn rail_bar(&mut self) -> &mut popover::MenuScrollbarState {
        self.scroll.rail_bar()
    }

    fn rail_scroll(&self) -> Option<gpui::ScrollHandle> {
        self.scroll.rail_scroll()
    }
}

impl Render for AccountsPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let now = Utc::now();
        let dialog = if self.login.is_some() {
            self.render_login_dialog(window.viewport_size(), cx)
        } else {
            self.render_rename_dialog(window.viewport_size(), cx)
        };
        let refreshing = matches!(self.snapshot, Loadable::Loading);
        let account_count = None;
        let advanced_sections = self
            .advanced_open
            .then(|| {
                self.advanced_page
                    .as_ref()
                    .map(|page| page.clone().into_any_element())
            })
            .flatten();

        let provider_icon = |harness: HarnessId| match harness {
            HarnessId::Codex => (crate::icons::OPENAI_MARK, None),
            HarnessId::Opencode => (crate::icons::OPENCODE_MARK, None),
            _ => (
                crate::icons::CLAUDE_MARK,
                Some(crate::icons::claude_brand()),
            ),
        };
        // Brand mark inside a 24px centered box (luna: `grid size-6
        // place-items-center [&_svg]:size-4`).
        let provider_mark = |harness: HarnessId, theme: &Theme| {
            let (mark, tint) = provider_icon(harness);
            div()
                .flex_none()
                .size(px(24.0))
                .flex()
                .items_center()
                .justify_center()
                .child(
                    crate::icons::icon(mark)
                        .size(px(16.0))
                        .text_color(tint.unwrap_or(theme.text_muted)),
                )
        };

        // One section per provider: brand header, controls, then account or
        // usage rows.
        let sections: Vec<AnyElement> = match &self.snapshot {
            Loadable::Idle | Loadable::Loading => PROVIDERS
                .into_iter()
                .map(|(harness, name, _cli)| {
                    let skeleton_id = match harness {
                        HarnessId::Codex => "accounts-skeleton-codex",
                        HarnessId::Opencode => "accounts-skeleton-opencode",
                        _ => "accounts-skeleton-claude",
                    };
                    div()
                        .mt(px(24.0))
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(8.0))
                                .child(provider_mark(harness, &theme))
                                .child(
                                    div()
                                        .text_size(crate::typography::ui_rems(14.0))
                                        .font_weight(gpui::FontWeight::MEDIUM)
                                        .text_color(theme.text)
                                        .child(SharedString::from(name)),
                                ),
                        )
                        .child(
                            // Ghost rows shaped like real ones (row two dimmed)
                            // so the card keeps its size while data develops.
                            widgets::section_card(&theme)
                                .mt(px(8.0))
                                .child(self.render_skeleton_row(
                                    (skeleton_id, 0),
                                    false,
                                    true,
                                    &theme,
                                    cx,
                                ))
                                .child(self.render_skeleton_row(
                                    (skeleton_id, 1),
                                    true,
                                    false,
                                    &theme,
                                    cx,
                                )),
                        )
                        .into_any_element()
                })
                .collect(),
            Loadable::Error(message) => {
                let message = message.clone();
                vec![
                    widgets::error_strip(&theme, message)
                        .id("accounts-load-error")
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| {
                            // Retry IS the visit's first successful list — force usage.
                            this.load(force_usage_for(LoadTrigger::Retry), cx)
                        }))
                        .child(
                            div()
                                .mt(px(4.0))
                                .text_size(crate::typography::ui_rems(11.5))
                                .text_color(theme.text_muted)
                                .child(SharedString::from("Click to retry")),
                        )
                        .into_any_element(),
                ]
            }
            Loadable::Ready(snapshot) => {
                let snapshot = snapshot.clone();
                let descriptors = self.harnesses.ready().cloned().unwrap_or_default();
                let updates = self.provider_updates.ready().cloned().unwrap_or_default();
                PROVIDERS
                    .into_iter()
                    .map(|(harness, name, cli)| {
                        let descriptor = descriptors.iter().find(|d| d.id == harness);
                        let update = updates.iter().find(|status| status.harness == harness);
                        let is_updating = self.updating_provider == Some(harness);
                        let update_available = update.is_some_and(|status| status.update_available);
                        let update_command =
                            update.and_then(|status| status.update_command.as_deref());
                        let version_label = update
                            .and_then(|status| status.installed_version.as_deref())
                            .map_or_else(
                                || "Version unavailable".to_owned(),
                                |version| format!("CLI {version}"),
                            );
                        let installed = descriptor.is_some_and(|d| d.installed);
                        let enabled = descriptor.is_some_and(descriptor_enabled);
                        let accounts = provider_accounts(&snapshot, harness);
                        // EVERY warning renders its own strip (luna maps them).
                        let warnings: Vec<String> = snapshot
                            .warnings
                            .iter()
                            .filter(|w| w.harness == harness)
                            .map(|w| w.message.clone())
                            .collect();
                        let rows: Vec<AnyElement> = accounts
                            .iter()
                            .enumerate()
                            .map(|(ix, account)| {
                                self.render_account_row(account, ix, ix == 0, &theme, now, cx)
                            })
                            .collect();
                        let add_id: SharedString = format!("add-account-{name}").into();
                        let replacing_existing = !accounts.is_empty();
                        let sign_in_label = match (harness, replacing_existing) {
                            (HarnessId::ClaudeCode, true) => "Sign in another",
                            (HarnessId::Codex, true) => "Add account",
                            _ => "Sign in",
                        };
                        let card = widgets::section_card(&theme).mt(px(8.0));
                        let empty_copy = format!(
                            "No {name} login detected on this device — sign in with \
                             \u{201C}{cli}\u{201D} or use Sign in."
                        );
                        let card = if harness == HarnessId::Opencode {
                            card.child(self.render_opencode_usage(&theme))
                        } else if rows.is_empty() {
                            card.child(
                                div()
                                    .px(px(20.0))
                                    .py(px(32.0))
                                    .text_center()
                                    .text_size(crate::typography::ui_rems(14.0))
                                    .text_color(theme.text_muted.opacity(0.6))
                                    .child(SharedString::from(empty_copy)),
                            )
                        } else {
                            card.children(rows)
                        };
                        div()
                            .mt(px(24.0))
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap(px(8.0))
                                    .child(provider_mark(harness, &theme))
                                    .child(
                                        div()
                                            .text_size(crate::typography::ui_rems(14.0))
                                            .font_weight(gpui::FontWeight::MEDIUM)
                                            .text_color(theme.text)
                                            .child(SharedString::from(name)),
                                    )
                                    .child(div().flex_1())
                                    .when(update_available && update_command.is_some(), |el| {
                                        el.child(
                                            widgets::ghost_action(&theme)
                                                .id(SharedString::from(format!(
                                                    "provider-update-{name}"
                                                )))
                                                .hover(|s| widgets::ghost_hover(&theme, s))
                                                .when(is_updating, |button| button.opacity(0.5))
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.update_provider(harness, cx);
                                                }))
                                                .child(
                                                    crate::icons::icon(crate::icons::ARROW_DOWN)
                                                        .size(px(15.0))
                                                        .text_color(theme.accent),
                                                )
                                                .child(if is_updating {
                                                    "Updating…"
                                                } else {
                                                    "Update"
                                                }),
                                        )
                                    })
                                    .when(harness != HarnessId::Opencode, |el| {
                                        el.child(
                                            widgets::ghost_action(&theme)
                                                .id(add_id)
                                                .hover(|s| widgets::ghost_hover(&theme, s))
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.request_login(
                                                        harness,
                                                        replacing_existing,
                                                        cx,
                                                    );
                                                }))
                                                .child(
                                                    crate::icons::icon(crate::icons::ADD_CIRCLE)
                                                        .size(px(16.0))
                                                        .text_color(theme.text_muted),
                                                )
                                                .child(SharedString::from(sign_in_label)),
                                        )
                                    })
                                    .child(
                                        widgets::toggle_switch(&theme, enabled)
                                            .id(SharedString::from(format!(
                                                "provider-toggle-{name}"
                                            )))
                                            .when(!installed || descriptor.is_none(), |el| {
                                                el.opacity(0.35)
                                            })
                                            .when(installed, |el| {
                                                el.cursor_pointer().on_click(cx.listener(
                                                    move |this, _, _, cx| {
                                                        this.toggle_harness(harness, !enabled, cx);
                                                    },
                                                ))
                                            }),
                                    ),
                            )
                            .when(!installed, |el| {
                                el.child(widgets::warning_strip(
                                    &theme,
                                    format!("Install the {cli} CLI to enable {name}"),
                                ))
                            })
                            .child(
                                div()
                                    .mt(px(5.0))
                                    .ml(px(28.0))
                                    .text_size(crate::typography::ui_rems(11.0))
                                    .text_color(if update_available {
                                        theme.accent
                                    } else {
                                        theme.text_muted
                                    })
                                    .child(if update_available {
                                        format!(
                                            "{version_label} · {} available{}",
                                            update
                                                .and_then(|status| status.latest_version.as_deref())
                                                .unwrap_or("newer version"),
                                            if update_command.is_none() {
                                                " · update manually"
                                            } else {
                                                ""
                                            }
                                        )
                                    } else {
                                        version_label
                                    }),
                            )
                            .children(
                                warnings
                                    .into_iter()
                                    .map(|warning| widgets::warning_strip(&theme, warning)),
                            )
                            .child(card)
                            .into_any_element()
                    })
                    .collect()
            }
        };

        let scrollbar = popover::rail(self, "accounts-page-scrollbar", &theme, cx);
        div()
            .id("accounts-page-host")
            .relative()
            .size_full()
            .on_hover(cx.listener(Self::on_scroll_hovered))
            .child(
                div()
                    .id("accounts-page")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll.scroll)
                    .child(
                        widgets::page_column()
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap(px(10.0))
                                    .child(widgets::page_header(&theme, "Providers", account_count))
                                    .child(div().flex_1())
                                    .child(
                                        // `text-[12.5px]` + leading 16px Refresh icon,
                                        // dimmed while a refresh is in flight (luna
                                        // `disabled:opacity-50`).
                                        widgets::ghost_action(&theme)
                                            .id("accounts-refresh")
                                            .flex_none()
                                            .text_size(crate::typography::ui_rems(12.5))
                                            .hover(|s| widgets::ghost_hover(&theme, s))
                                            .when(refreshing, |el| el.opacity(0.5))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.load(force_usage_for(LoadTrigger::Refresh), cx)
                                            }))
                                            .child(
                                                crate::icons::icon(crate::icons::REFRESH)
                                                    .size(px(16.0))
                                                    .text_color(theme.text_muted),
                                            )
                                            .child(SharedString::from("Refresh")),
                                    )
                                    .child(self.render_device_switcher(&theme, cx)),
                            )
                            .child(widgets::page_subtitle(
                                &theme,
                                "Enable the CLIs you use on this device. Claude Code and Codex \
                                 accounts are managed by their own CLIs; OpenCode connects through \
                                 its CLI and shows available usage here.",
                            ))
                            .when_some(self.error.clone(), |el, message| {
                                el.child(
                                    widgets::error_strip(&theme, message)
                                        .id("accounts-action-error")
                                        .cursor_pointer()
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.error = None;
                                            cx.notify();
                                        })),
                                )
                            })
                            .when_some(self.harness_error.clone(), |el, message| {
                                el.child(widgets::error_strip(&theme, message))
                            })
                            .when_some(self.update_message.clone(), |el, message| {
                                el.child(widgets::warning_strip(&theme, message))
                            })
                            .children(sections)
                            .child(
                                widgets::ghost_action(&theme)
                                    .id("providers-advanced-disclosure")
                                    .mt(px(24.0))
                                    .hover(|s| widgets::ghost_hover(&theme, s))
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.toggle_advanced(cx)),
                                    )
                                    .child(if self.advanced_open {
                                        "Hide provider options"
                                    } else {
                                        "Advanced provider options"
                                    }),
                            )
                            .children(advanced_sections)
                            // Footer note (luna: `mt-6 text-[12px] leading-relaxed
                            // text-muted-foreground/60`).
                            .child(
                                div()
                                    .mt(px(24.0))
                                    .text_size(crate::typography::ui_rems(12.0))
                                    .line_height(px(19.0))
                                    .text_color(theme.text_muted.opacity(0.6))
                                    .child(SharedString::from(
                                        "Provider sign-in updates the CLI's native login on this \
                                         device. Existing CLI sessions stay with their provider.",
                                    )),
                            ),
                    ),
            )
            .children(scrollbar)
            .when_some(dialog, |el, dialog| el.child(dialog))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeDelta;

    fn lifecycle_account(harness: HarnessId, active: bool, switchable: bool) -> AgentAccount {
        AgentAccount {
            id: "fixture".into(),
            harness,
            label: None,
            email: None,
            plan_label: None,
            active,
            authentication: AgentAuthenticationState::Authenticated,
            usage_windows: vec![],
            display_name: None,
            organization: None,
            auth_kind: None,
            switchable,
            saved_at: None,
        }
    }

    #[test]
    fn lifecycle_controls_match_provider_contracts() {
        assert_eq!(
            account_lifecycle_action(&lifecycle_account(HarnessId::ClaudeCode, true, false)),
            Some(AccountLifecycleAction::DisconnectClaude)
        );
        assert_eq!(
            account_lifecycle_action(&lifecycle_account(HarnessId::ClaudeCode, false, true)),
            Some(AccountLifecycleAction::ReconnectClaude)
        );
        assert_eq!(
            account_lifecycle_action(&lifecycle_account(HarnessId::Codex, false, true)),
            Some(AccountLifecycleAction::SelectCodexDefault)
        );
        assert_eq!(
            account_lifecycle_action(&lifecycle_account(HarnessId::Codex, true, false)),
            None
        );
    }

    #[test]
    fn first_load_of_a_visit_forces_the_usage_probe() {
        // The engine only probes usage when forced (M5c); without forcing on
        // mount, the first Accounts open always rendered "Usage unavailable".
        assert!(force_usage_for(LoadTrigger::Mount));
        // A retry after a failed load is still the visit's first successful
        // list — same requirement.
        assert!(force_usage_for(LoadTrigger::Retry));
        // Explicit refresh and a just-completed login always re-probe.
        assert!(force_usage_for(LoadTrigger::Refresh));
        assert!(force_usage_for(LoadTrigger::PostLogin));
        // Switch/Forget re-lists ride the still-warm 60s cache.
        assert!(!force_usage_for(LoadTrigger::PostAction));
    }

    #[test]
    fn usage_thresholds_match_luna() {
        assert_eq!(usage_level(0.0), UsageLevel::Normal);
        assert_eq!(usage_level(0.79), UsageLevel::Normal);
        assert_eq!(usage_level(0.80), UsageLevel::Warn);
        assert_eq!(usage_level(0.94), UsageLevel::Warn);
        assert_eq!(usage_level(0.95), UsageLevel::Critical);
        assert_eq!(usage_level(1.0), UsageLevel::Critical);
    }

    #[test]
    fn usage_colors_map_to_theme_accents() {
        let theme = Theme::dark();
        assert_eq!(usage_color(UsageLevel::Normal, &theme), theme.accent);
        assert_eq!(usage_color(UsageLevel::Warn, &theme), theme.warning);
        assert_eq!(usage_color(UsageLevel::Critical, &theme), theme.danger);
    }

    #[test]
    fn reset_formatting_is_absolute() {
        use chrono::Local;
        let now = Utc::now();
        assert_eq!(format_reset(None, now), None);
        // Within ~22h: a local clock time ("resets 3:45 PM").
        let soon = now + TimeDelta::minutes(125);
        assert_eq!(
            format_reset(Some(soon), now),
            Some(format!(
                "resets {}",
                soon.with_timezone(&Local).format("%-I:%M %p")
            ))
        );
        // Within a week: a short weekday ("resets Mon").
        let later = now + TimeDelta::days(3);
        assert_eq!(
            format_reset(Some(later), now),
            Some(format!(
                "resets {}",
                later.with_timezone(&Local).format("%a")
            ))
        );
        // Beyond a week (Codex free tier resets ~monthly): month + day
        // ("resets Sep 14") — a weekday 4 weeks out carries no information.
        let monthly = now + TimeDelta::days(26);
        assert_eq!(
            format_reset(Some(monthly), now),
            Some(format!(
                "resets {}",
                monthly.with_timezone(&Local).format("%b %-d")
            ))
        );
    }

    #[test]
    fn provider_grouping_keeps_engine_order_even_when_active_is_later() {
        let account = |id: &str, harness: HarnessId, active: bool| AgentAccount {
            id: id.into(),
            harness,
            label: None,
            email: None,
            plan_label: None,
            active,
            authentication: luna_proto::AgentAuthenticationState::Authenticated,
            usage_windows: vec![],
            display_name: None,
            organization: None,
            auth_kind: None,
            switchable: true,
            saved_at: None,
        };
        let snapshot = AgentAccountsSnapshot {
            accounts: vec![
                account("c1", HarnessId::ClaudeCode, false),
                account("x1", HarnessId::Codex, false),
                account("c2", HarnessId::ClaudeCode, true),
            ],
            warnings: vec![],
        };
        let claude = provider_accounts(&snapshot, HarnessId::ClaudeCode);
        let ids: Vec<&str> = claude.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(
            ids,
            ["c1", "c2"],
            "engine (creation) order holds — switching must not move a card"
        );
        assert_eq!(provider_accounts(&snapshot, HarnessId::Codex).len(), 1);
        assert!(provider_accounts(&snapshot, HarnessId::Cursor).is_empty());
    }
}
