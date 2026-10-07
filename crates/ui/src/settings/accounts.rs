//! Settings → Providers: per-device CLI enablement, provider-owned accounts
//! and each provider's defaults for new chats.
//!
//! Layout (design spec `docs/design/settings-polish-2026-10-06/spec.md` §8):
//! a title bar with the device picker and Refresh; a "New chats" section
//! (which provider a new chat starts with); then one card per provider whose
//! header carries the brand mark, name, CLI version, Update, Sign in and the
//! enable switch, and whose rows are its accounts (email, plan badge, Active,
//! usage meters — indigo → amber ≥80% → red ≥95%, reset time — and lifecycle
//! controls) followed by its defaults ([`ProviderDefaultsPanel`]). A turned-off
//! provider collapses to its header. "Advanced provider options" is a
//! disclosure row below. The add-account dialogs (paste-code and browser-poll
//! flows) open over the page.
//!
//! The device picker retargets every accounts RPC at another device
//! (`targetDeviceId` passthrough); defaults stay with this device, so they
//! show only while it is selected. Every call surfaces failures as inline
//! strips inside the section they concern.

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

use crate::button::{self, ButtonKind, ControlSize, Segment};
use crate::composer::{ComposerInput, ComposerInputEvent};
use crate::interaction::HoverFill as _;
use crate::motion::{self, AnimationExt as _};
use crate::popover::{self, Loadable};
use crate::settings::harnesses::HarnessesPage;
use crate::settings::provider_defaults::ProviderDefaultsPanel;
use crate::settings::widgets;
use crate::state::AppState;
use crate::theme::{FillTier, Theme};
use crate::typography::{TextStyle, TypeStyleExt as _, ui_rems};

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

/// A stable id fragment for a provider (its CLI name).
fn provider_key(harness: HarnessId) -> &'static str {
    PROVIDERS
        .iter()
        .find(|(id, _, _)| *id == harness)
        .map_or("provider", |(_, _, cli)| *cli)
}

/// A message shown in a strip inside the provider card it concerns, or under
/// the page title when it concerns no single provider (`harness: None`).
#[derive(Clone)]
struct Notice {
    harness: Option<HarnessId>,
    message: SharedString,
}

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
    /// One defaults panel per provider card, in [`PROVIDERS`] order.
    defaults_panels: Vec<Entity<ProviderDefaultsPanel>>,
    /// The provider new chats start with (the composer's remembered pick),
    /// reread on every visit.
    new_chat_provider: Option<HarnessId>,
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
    /// A failed enable/disable, shown in that provider's card.
    harness_error: Option<(HarnessId, String)>,
    /// The last CLI update's outcome: provider, message, and whether it
    /// fully succeeded (a quiet note) or needs attention (a warning strip).
    update_message: Option<(HarnessId, String, bool)>,
    updating_provider: Option<HarnessId>,
    /// Account id with an in-flight Switch/Forget.
    busy_account: Option<String>,
    login: Option<LoginFlow>,
    rename: Option<RenameFlow>,
    error: Option<Notice>,
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
        self.reload_new_chat_provider(cx);
        // `load` marks every provider's defaults stale; they reread once the
        // harness list says which cards show them.
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
                period_starts_ms: vec![],
                until_ms: None,
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
            live_sessions: Vec::new(),
        });
        // The defaults rows load as on a real visit once the harness list
        // says which cards show them.
        page.sync_defaults_panels(cx);
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
        let defaults_panels = PROVIDERS
            .into_iter()
            .map(|(harness, name, cli)| {
                let state = state.clone();
                cx.new(|cx| ProviderDefaultsPanel::new(state, harness, name, cli, cx))
            })
            .collect();
        let new_chat_provider = state
            .read(cx)
            .data_dir
            .as_deref()
            .and_then(super::provider_defaults::new_chat_provider);
        Self {
            defaults_panels,
            new_chat_provider,
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
        self.harness_error = None;
        self.update_message = None;
        self.load(force_usage_for(LoadTrigger::Mount), cx);
    }

    /// Whether the page shows this device (defaults and the new-chat
    /// provider belong to it; another device's are not editable here).
    fn targets_local_device(&self, cx: &Context<Self>) -> bool {
        self.target_device
            .as_deref()
            .is_none_or(|id| self.state.read(cx).local_device_id.as_deref() == Some(id))
    }

    fn reload_new_chat_provider(&mut self, cx: &mut Context<Self>) {
        self.new_chat_provider = self
            .state
            .read(cx)
            .data_dir
            .as_deref()
            .and_then(super::provider_defaults::new_chat_provider);
    }

    fn set_new_chat_provider(&mut self, harness: HarnessId, cx: &mut Context<Self>) {
        let Some(dir) = self.state.read(cx).data_dir.clone() else {
            return;
        };
        match super::provider_defaults::save_new_chat_provider(&dir, harness, cx) {
            Ok(()) => self.new_chat_provider = Some(harness),
            Err(error) => {
                self.error = Some(Notice {
                    harness: None,
                    message: format!("The default provider could not be saved: {error}").into(),
                });
            }
        }
        cx.notify();
    }

    /// Reread every provider's catalog and saved defaults when its card next
    /// shows them.
    fn mark_defaults_stale(&mut self, cx: &mut Context<Self>) {
        for panel in &self.defaults_panels {
            panel.update(cx, |panel, _| panel.mark_stale());
        }
    }

    /// Load the defaults of the providers whose cards show them: this device
    /// only, and providers that are on (or whose state is unknown because the
    /// harness list failed, so the card can still show the failure).
    fn sync_defaults_panels(&mut self, cx: &mut Context<Self>) {
        if !self.targets_local_device(cx) {
            return;
        }
        let shown: Vec<HarnessId> = match &self.harnesses {
            Loadable::Ready(list) => PROVIDERS
                .iter()
                .map(|(harness, _, _)| *harness)
                .filter(|harness| {
                    list.iter()
                        .any(|d| d.id == *harness && d.installed && descriptor_enabled(d))
                })
                .collect(),
            Loadable::Error(_) => PROVIDERS.iter().map(|(harness, _, _)| *harness).collect(),
            Loadable::Idle | Loadable::Loading => return,
        };
        for panel in &self.defaults_panels {
            if shown.contains(&panel.read(cx).provider()) {
                panel.update(cx, |panel, cx| panel.ensure_loaded(cx));
            }
        }
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

    /// The title bar's device picker (luna device-switcher.tsx), on the kit's
    /// pop-up button: platform glyph · name · this-device dot · chevron,
    /// opening a menu of every registered device. Selecting one retargets
    /// the page.
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
        let idle_dot = theme.readable_muted(0.35);
        let open = self.device_menu.is_open();
        let dot = move |on: bool| {
            div()
                .flex_none()
                .size(ui_rems(6.0))
                .rounded_full()
                .bg(if on { emerald } else { idle_dot })
        };

        let mut trigger = button::select_frame(
            "accounts-device-switcher",
            ControlSize::Md,
            open,
            true,
            theme,
        )
        .relative()
        .max_w(ui_rems(240.0))
        .aria_label(format!("Device: {trigger_label}"))
        .on_mouse_down(
            gpui::MouseButton::Left,
            cx.listener(|this, _, _, _| this.device_menu.note_trigger_press()),
        )
        .on_click(cx.listener(|this, event: &gpui::ClickEvent, _, cx| {
            // A press that found the menu open closes it (the card's
            // mouse-down-out already began the close) — never reopen. A
            // keyboard click has no press note, so it toggles.
            if event.is_keyboard() {
                if this.device_menu.is_open() {
                    this.close_device_menu(cx);
                } else {
                    this.device_menu.open(());
                }
            } else if this.device_menu.take_press_was_open() {
                this.close_device_menu(cx);
            } else {
                this.device_menu.open(());
            }
            cx.notify();
        }))
        .on_key_down(crate::swallow_activation_key)
        .child(button::glyph(trigger_glyph, ControlSize::Md).text_color(theme.readable_muted(0.9)))
        .child(div().min_w_0().truncate().child(trigger_label))
        .child(dot(effective == local_id))
        .child(button::select_chevron(theme));

        if self.device_menu.get().is_some() {
            let closing = self.device_menu.closing_since();
            let menu = popover::popover_card(theme)
                .w(ui_rems(240.0))
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
                        .role(gpui::Role::Button)
                        .aria_label(name.clone())
                        .aria_toggled(if is_active {
                            gpui::Toggled::True
                        } else {
                            gpui::Toggled::False
                        })
                        .tab_index(0)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            // Local device = no passthrough (calls stay direct).
                            let target = (!pick_local).then(|| pick_id.clone());
                            this.set_target_device(target, cx);
                        }))
                        .on_key_down(crate::swallow_activation_key)
                        .child(
                            icon(glyph)
                                .size(ui_rems(16.0))
                                .flex_none()
                                .text_color(theme.text_muted),
                        )
                        .child(div().flex_1().min_w_0().truncate().child(name))
                        .when(is_local, |el| {
                            el.child(
                                div()
                                    .flex_none()
                                    .type_style(TextStyle::Caption)
                                    .text_color(theme.readable_muted(0.6))
                                    .child(SharedString::from("You")),
                            )
                        })
                        .child(dot(is_local))
                }))
                .into_any_element();
            trigger = trigger.child(popover::anchored_menu_below_end(
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
        // Catalogs can change with the device, a login or an account switch.
        self.mark_defaults_stale(cx);
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
                page.sync_defaults_panels(cx);
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
                            page.sync_defaults_panels(cx);
                        }
                        Err(error) => page.harness_error = Some((harness, error.to_string())),
                    },
                    Err(error) => page.harness_error = Some((harness, error.to_string())),
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
                let (message, ok) = match result {
                    Ok(value) => match serde_json::from_value::<ProviderUpdateResult>(value) {
                        Ok(updated) => {
                            crate::pickers::bump_harness_catalog(cx);
                            // The new CLI may offer different models.
                            page.mark_defaults_stale(cx);
                            page.load_harnesses(cx);
                            page.load_provider_updates(true, cx);
                            let ok = updated.runtime_reload_error.is_none()
                                && updated.model_refresh_error.is_none();
                            let runtime = updated.runtime_reload_error.map_or_else(
                                || "Idle provider runtimes reloaded.".to_owned(),
                                |error| format!("Runtime reload deferred: {error}"),
                            );
                            let message = match updated.model_refresh_error {
                                Some(error) => format!(
                                    "CLI updated to {}. {runtime} Model refresh failed: {error}",
                                    updated.installed_version
                                ),
                                None => format!(
                                    "CLI updated to {}. {runtime} Models refreshed.",
                                    updated.installed_version
                                ),
                            };
                            (message, ok)
                        }
                        Err(error) => (error.to_string(), false),
                    },
                    Err(error) => (error.to_string(), false),
                };
                page.update_message = Some((harness, message, ok));
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
        let harness = account.harness;
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
                    Err(err) => {
                        page.error = Some(Notice {
                            harness: Some(harness),
                            message: format!("{err}").into(),
                        })
                    }
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
                        page.error = Some(Notice {
                            harness: Some(harness),
                            message: format!("Login failed to start: {err}").into(),
                        });
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

    /// One usage window (luna settings.agents.tsx `UsageMeter`): label · bar
    /// (indigo → amber ≥80% → red ≥95%) · "NN% used" · quiet reset time. The
    /// label, percentage and reset columns hold fixed widths so stacked
    /// meters line up.
    fn usage_meter(
        label: &str,
        fraction: f32,
        resets_at: Option<DateTime<Utc>>,
        theme: &Theme,
        now: DateTime<Utc>,
    ) -> AnyElement {
        let fraction = fraction.clamp(0.0, 1.0);
        let level = usage_level(fraction);
        let fill = usage_color(level, theme).opacity(match level {
            UsageLevel::Normal => 0.8,
            _ => 0.85,
        });
        let reset = format_reset(resets_at, now);
        div()
            .w_full()
            .flex()
            .flex_row()
            .items_center()
            .gap(ui_rems(8.0))
            .type_style(TextStyle::Caption)
            .text_color(theme.readable_muted(0.85))
            .child(
                div()
                    .w(ui_rems(48.0))
                    .flex_none()
                    .truncate()
                    .child(SharedString::from(label.to_string())),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(ui_rems(40.0))
                    .max_w(ui_rems(240.0))
                    .h(ui_rems(5.0))
                    .rounded_full()
                    .overflow_hidden()
                    .bg(theme.fill(FillTier::Secondary))
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
                    .w(ui_rems(64.0))
                    .flex_none()
                    .text_right()
                    .child(SharedString::from(format!(
                        "{}% used",
                        (fraction * 100.0).round() as u32
                    ))),
            )
            // The reset column always holds its width, even when a window has
            // no reset time, so the bars of stacked meters end together.
            .child(
                div()
                    .w(ui_rems(96.0))
                    .flex_none()
                    .truncate()
                    .text_color(theme.readable_muted(0.65))
                    .child(SharedString::from(reset.unwrap_or_default())),
            )
            .into_any_element()
    }

    /// A row's identity text — an email, a label — truncated to its column
    /// with the full value in a tooltip: in the row-title style, or the
    /// secondary description style.
    fn identity_text(id: String, text: SharedString, title: bool, theme: &Theme) -> AnyElement {
        div()
            .id(SharedString::from(id))
            .map(|el| {
                if title {
                    el.text_size(ui_rems(widgets::ROW_TITLE_SIZE))
                        .line_height(ui_rems(18.0))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(theme.text)
                } else {
                    el.type_style(TextStyle::Footnote)
                        .text_color(theme.readable_muted(0.85))
                }
            })
            .min_w_0()
            .truncate()
            .tooltip(popover::text_tooltip(text.clone()))
            .tooltip_show_delay(popover::TOOLTIP_DELAY)
            .child(text)
            .into_any_element()
    }

    /// One account row (luna settings.agents.tsx `AccountRow`): initial
    /// avatar · email with its badges · usage meters, and the trailing
    /// lifecycle actions.
    fn render_account_row(
        &self,
        account: &AgentAccount,
        ix: usize,
        theme: &Theme,
        now: DateTime<Utc>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = provider_key(account.harness);
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
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(ui_rems(6.0))
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
        // Disconnect only clears LUNA's use of the login (Reconnect or
        // Connect restores it; provider credentials are untouched), so it is
        // a quiet Secondary button, never the loudest thing in the row.
        let lifecycle_action = action.map(|(method, label, id)| {
            button::button(
                format!("{id}-{key}-{ix}"),
                ButtonKind::Secondary,
                ControlSize::Sm,
                self.busy_account.is_none(),
                theme,
            )
            .aria_label(format!("{label} {title}"))
            .child(label)
            .on_click(cx.listener(move |this, _, _, cx| {
                if this.busy_account.is_some() {
                    return;
                }
                this.account_action(method, &action_account, cx);
            }))
        });
        let rename_action = (account.harness == HarnessId::Codex && account.id != "codex-native")
            .then(|| {
                button::button(
                    format!("account-rename-{key}-{ix}"),
                    ButtonKind::Ghost,
                    ControlSize::Sm,
                    true,
                    theme,
                )
                .aria_label(format!("Rename {title}"))
                .child("Rename")
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.begin_rename(rename_account.clone(), cx);
                }))
            });

        let avatar = div()
            .flex_none()
            .size(ui_rems(32.0))
            .rounded_full()
            .border_1()
            .border_color(theme.border)
            .bg(theme.fill(FillTier::Quaternary))
            .flex()
            .items_center()
            .justify_center()
            .type_style(TextStyle::Footnote)
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(theme.readable_muted(0.9))
            .child(initial);

        let usage: AnyElement = if account.usage_windows.is_empty() {
            // Meters XOR the quiet fallback line — never both (luna:
            // `usage ? meters : "Usage unavailable"…`).
            widgets::row_description(theme, "Usage unavailable").into_any_element()
        } else {
            div()
                .mt(ui_rems(4.0))
                .w_full()
                .flex()
                .flex_col()
                .gap(ui_rems(4.0))
                .children(account.usage_windows.iter().map(|window| {
                    Self::usage_meter(
                        &window.label,
                        window.used_fraction,
                        window.resets_at,
                        theme,
                        now,
                    )
                }))
                .into_any_element()
        };

        widgets::card_row(theme, false)
            .child(avatar)
            .child(
                div()
                    .flex_1()
                    .min_w(ui_rems(widgets::ROW_TEXT_MIN))
                    .flex()
                    .flex_col()
                    .gap(ui_rems(2.0))
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .flex_row()
                            .flex_wrap()
                            .items_center()
                            .gap_x(ui_rems(8.0))
                            .gap_y(ui_rems(4.0))
                            .child(Self::identity_text(
                                format!("account-title-{key}-{ix}"),
                                title.clone(),
                                true,
                                theme,
                            ))
                            .child(badges),
                    )
                    .when(account.label.is_some(), |el| {
                        el.child(Self::identity_text(
                            format!("account-identity-{key}-{ix}"),
                            identity.clone(),
                            false,
                            theme,
                        ))
                    })
                    .child(usage),
            )
            .when(
                rename_action.is_some() || lifecycle_action.is_some(),
                |row| {
                    row.child(
                        widgets::row_controls()
                            .children(rename_action)
                            .children(lifecycle_action),
                    )
                },
            )
            .into_any_element()
    }

    /// OpenCode has no account rows: its Go subscription usage, when the CLI
    /// exposes one, in the same row anatomy.
    fn render_opencode_usage(
        &self,
        theme: &Theme,
        now: DateTime<Utc>,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        match &self.opencode_usage {
            Loadable::Ready(snapshot) => {
                let limits: Vec<_> = snapshot
                    .limits
                    .iter()
                    .filter(|limit| limit.provider == UsageProvider::OpenCodeGo)
                    .collect();
                if limits.is_empty() {
                    return vec![
                        widgets::card_row(theme, false)
                            .child(widgets::row_text(
                                theme,
                                "Usage",
                                Some(
                                    "OpenCode uses its CLI connection. No subscription usage is \
                                     available."
                                        .into(),
                                ),
                            ))
                            .into_any_element(),
                    ];
                }
                limits
                    .into_iter()
                    .map(|limit| {
                        let name = limit.provider.limits_label();
                        let title: SharedString = match &limit.plan {
                            Some(plan) if plan != name => format!("{name} · {plan}").into(),
                            _ => name.into(),
                        };
                        let detail = (limit.availability != LimitAvailability::Ready).then(|| {
                            SharedString::from(
                                limit
                                    .detail
                                    .clone()
                                    .unwrap_or_else(|| "Usage unavailable".into()),
                            )
                        });
                        widgets::card_row(theme, false)
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(ui_rems(widgets::ROW_TEXT_MIN))
                                    .flex()
                                    .flex_col()
                                    .gap(ui_rems(2.0))
                                    .child(widgets::row_title(theme, title))
                                    .when_some(detail, |el, detail| {
                                        el.child(widgets::row_description(theme, detail))
                                    })
                                    .when(!limit.windows.is_empty(), |el| {
                                        el.child(
                                            div()
                                                .mt(ui_rems(4.0))
                                                .w_full()
                                                .flex()
                                                .flex_col()
                                                .gap(ui_rems(4.0))
                                                .children(limit.windows.iter().map(|window| {
                                                    Self::usage_meter(
                                                        &window.label,
                                                        window.used_percent / 100.0,
                                                        window.resets_at_ms.and_then(
                                                            DateTime::from_timestamp_millis,
                                                        ),
                                                        theme,
                                                        now,
                                                    )
                                                })),
                                        )
                                    }),
                            )
                            .into_any_element()
                    })
                    .collect()
            }
            Loadable::Loading | Loadable::Idle => {
                vec![self.render_skeleton_row(false, theme, cx)]
            }
            Loadable::Error(message) => vec![
                widgets::card_row(theme, false)
                    .child(widgets::row_text(
                        theme,
                        "Usage",
                        Some(format!("OpenCode usage unavailable: {message}").into()),
                    ))
                    .into_any_element(),
            ],
        }
    }

    /// A strip inside a provider card, under its header.
    fn card_strip(strip: gpui::Div) -> AnyElement {
        div()
            .w_full()
            .px(ui_rems(widgets::ROW_PAD_X))
            .pb(ui_rems(12.0))
            .child(strip.mt(ui_rems(0.0)))
            .into_any_element()
    }

    /// An error strip with a trailing kit Dismiss button.
    fn dismissible_error(
        &self,
        id: &'static str,
        message: SharedString,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        widgets::error_strip(theme, message)
            .child(div().flex_1())
            .child(
                button::icon_button(id, crate::icons::CLOSE, ControlSize::Sm, true, theme)
                    .mt(ui_rems(-4.0))
                    .aria_label("Dismiss")
                    .tooltip(popover::text_tooltip("Dismiss"))
                    .tooltip_show_delay(popover::TOOLTIP_DELAY)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.error = None;
                        cx.notify();
                    })),
            )
    }

    /// One provider's section card: the header (brand mark, name, CLI
    /// version, Update, Sign in, the enable switch), its strips, then its
    /// accounts and defaults. A provider that is off collapses to its header.
    #[allow(clippy::too_many_arguments)]
    fn render_provider_card(
        &self,
        harness: HarnessId,
        name: &'static str,
        cli: &'static str,
        panel: Option<Entity<ProviderDefaultsPanel>>,
        theme: &Theme,
        now: DateTime<Utc>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let descriptor = self
            .harnesses
            .ready()
            .and_then(|list| list.iter().find(|d| d.id == harness))
            .cloned();
        let catalog_known = self.harnesses.ready().is_some();
        let installed = descriptor.as_ref().is_some_and(|d| d.installed);
        let enabled = descriptor.as_ref().is_some_and(descriptor_enabled);
        // Collapse only on a known "off": while the harness list loads (or
        // failed) the card keeps its rows so nothing jumps.
        let collapsed = catalog_known && !(installed && enabled);
        let update = self
            .provider_updates
            .ready()
            .and_then(|updates| updates.iter().find(|status| status.harness == harness))
            .cloned();
        let is_updating = self.updating_provider == Some(harness);
        let update_available = update
            .as_ref()
            .is_some_and(|status| status.update_available);
        let update_command = update
            .as_ref()
            .and_then(|status| status.update_command.clone());
        let installed_version = update
            .as_ref()
            .and_then(|status| status.installed_version.clone());
        let latest_version = update
            .as_ref()
            .and_then(|status| status.latest_version.clone());
        let accounts: Vec<AgentAccount> = self
            .snapshot
            .ready()
            .map(|snapshot| {
                provider_accounts(snapshot, harness)
                    .into_iter()
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        let replacing_existing = !accounts.is_empty();
        let sign_in_label = match (harness, replacing_existing) {
            (HarnessId::ClaudeCode, true) => "Sign in another",
            (HarnessId::Codex, true) => "Add account",
            _ => "Sign in",
        };

        // ---- header ----
        let (mark, tint) = match harness {
            HarnessId::Codex => (crate::icons::OPENAI_MARK, None),
            HarnessId::Opencode => (crate::icons::OPENCODE_MARK, None),
            _ => (
                crate::icons::CLAUDE_MARK,
                Some(crate::icons::claude_brand()),
            ),
        };
        let caption = |text: String, color: gpui::Hsla| {
            div()
                .flex_none()
                .type_style(TextStyle::Caption)
                .text_color(color)
                .child(SharedString::from(text))
        };
        let title_line = div()
            .w_full()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .gap_x(ui_rems(8.0))
            .gap_y(ui_rems(2.0))
            .child(
                div()
                    .flex_none()
                    .size(ui_rems(20.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .when(collapsed, |el| el.opacity(0.55))
                    .child(
                        crate::icons::icon(mark)
                            .size(ui_rems(16.0))
                            .text_color(tint.unwrap_or(theme.text_muted)),
                    ),
            )
            .child(
                div()
                    .type_style(TextStyle::Title3)
                    .text_color(if collapsed {
                        theme.readable_muted(0.85)
                    } else {
                        theme.text
                    })
                    .child(SharedString::from(name)),
            )
            .when_some(installed_version.clone(), |el, version| {
                el.child(caption(format!("CLI {version}"), theme.readable_muted(0.8)))
            })
            .when(update_available, |el| {
                let latest = latest_version
                    .clone()
                    .unwrap_or_else(|| "A newer version".into());
                el.child(caption(
                    if update_command.is_none() {
                        format!("{latest} available · update manually")
                    } else {
                        format!("{latest} available")
                    },
                    theme.readable_tone(theme.accent, 1.0),
                ))
            });
        let description: Option<SharedString> = if catalog_known && !installed {
            Some(format!("Not installed. Install the {cli} CLI to enable {name}.").into())
        } else if collapsed {
            Some("Turned off on this device.".into())
        } else {
            None
        };

        let mut actions: Vec<AnyElement> = Vec::new();
        if update_available && update_command.is_some() {
            actions.push(
                button::button(
                    format!("provider-update-{name}"),
                    ButtonKind::Secondary,
                    ControlSize::Sm,
                    self.updating_provider.is_none(),
                    theme,
                )
                .aria_label(format!("Update {name}"))
                .child(button::glyph(crate::icons::ARROW_DOWN, ControlSize::Sm))
                .child(if is_updating { "Updating…" } else { "Update" })
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.update_provider(harness, cx);
                }))
                .into_any_element(),
            );
        }
        if harness != HarnessId::Opencode && !collapsed {
            // Whether a sign-in replaces an existing login (Claude's shared
            // native login needs the disclosure first) is only known once the
            // accounts list has loaded, so the button waits for it.
            let accounts_known = self.snapshot.ready().is_some();
            actions.push(
                button::button(
                    format!("add-account-{name}"),
                    ButtonKind::Secondary,
                    ControlSize::Sm,
                    self.login.is_none() && accounts_known,
                    theme,
                )
                .aria_label(format!("{name}: {sign_in_label}"))
                .child(button::glyph(crate::icons::ADD_CIRCLE, ControlSize::Sm))
                .child(sign_in_label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    if this.login.is_some() || this.snapshot.ready().is_none() {
                        return;
                    }
                    this.request_login(harness, replacing_existing, cx);
                }))
                .into_any_element(),
            );
        }
        actions.push(if installed {
            widgets::switch_control(
                SharedString::from(format!("provider-toggle-{name}")),
                format!("Enable {name}"),
                enabled,
                theme,
                cx,
                move |this, cx| {
                    this.toggle_harness(harness, !enabled, cx);
                },
            )
            .into_any_element()
        } else {
            // Not installed (or not known yet): shown, but not a control.
            widgets::toggle_switch(theme, enabled, &format!("provider-toggle-{name}"))
                .opacity(0.35)
                .into_any_element()
        });

        let header = div()
            .w_full()
            .px(ui_rems(widgets::ROW_PAD_X))
            .pt(ui_rems(14.0))
            .pb(ui_rems(12.0))
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .gap_x(ui_rems(12.0))
            .gap_y(ui_rems(8.0))
            .child(
                div()
                    .flex_1()
                    .min_w(ui_rems(widgets::ROW_TEXT_MIN))
                    .flex()
                    .flex_col()
                    .gap(ui_rems(3.0))
                    .child(title_line)
                    .when_some(description, |column, description| {
                        column.child(widgets::row_description(theme, description))
                    }),
            )
            .child(widgets::row_controls().children(actions));

        // ---- strips: this provider's errors and notes ----
        let mut strips: Vec<AnyElement> = Vec::new();
        if let Some(notice) = self
            .error
            .clone()
            .filter(|notice| notice.harness == Some(harness))
        {
            strips.push(Self::card_strip(self.dismissible_error(
                "accounts-action-error-dismiss",
                notice.message,
                theme,
                cx,
            )));
        }
        if let Some((_, message)) = self
            .harness_error
            .clone()
            .filter(|(owner, _)| *owner == harness)
        {
            strips.push(Self::card_strip(widgets::error_strip(theme, message)));
        }
        if let Some((_, message, ok)) = self
            .update_message
            .clone()
            .filter(|(owner, _, _)| *owner == harness)
        {
            strips.push(if ok {
                Self::card_strip(
                    div()
                        .flex()
                        .flex_row()
                        .items_start()
                        .gap(ui_rems(6.0))
                        .child(
                            crate::icons::icon(crate::icons::CHECK)
                                .mt(ui_rems(1.0))
                                .size(ui_rems(14.0))
                                .flex_none()
                                .text_color(theme.readable_tone(theme.success, 1.0)),
                        )
                        .child(widgets::row_description(theme, message)),
                )
            } else {
                Self::card_strip(widgets::warning_strip(theme, message))
            });
        }
        if let Some(snapshot) = self.snapshot.ready() {
            // EVERY warning renders its own strip (luna maps them).
            for warning in snapshot.warnings.iter().filter(|w| w.harness == harness) {
                strips.push(Self::card_strip(widgets::warning_strip(
                    theme,
                    warning.message.clone(),
                )));
            }
        }

        let card = widgets::section_card(theme).child(header).children(strips);
        if collapsed {
            return card.into_any_element();
        }

        // ---- rows: accounts (or usage), then defaults ----
        let rows: Vec<AnyElement> = if harness == HarnessId::Opencode {
            self.render_opencode_usage(theme, now, cx)
        } else {
            match &self.snapshot {
                // Ghost rows shaped like real ones (row two dimmed) so the
                // card keeps its size while data develops.
                Loadable::Idle | Loadable::Loading => vec![
                    self.render_skeleton_row(false, theme, cx),
                    self.render_skeleton_row(true, theme, cx),
                ],
                // The page-level strip under the title owns the retry.
                Loadable::Error(_) => Vec::new(),
                Loadable::Ready(_) if accounts.is_empty() => vec![
                    widgets::card_row(theme, false)
                        .child(widgets::row_description(
                            theme,
                            format!(
                                "No {name} login detected on this device. Sign in with \
                                 \u{201C}{cli}\u{201D}, or use {sign_in_label} above."
                            ),
                        ))
                        .into_any_element(),
                ],
                Loadable::Ready(_) => accounts
                    .iter()
                    .enumerate()
                    .map(|(ix, account)| self.render_account_row(account, ix, theme, now, cx))
                    .collect(),
            }
        };
        card.children(rows)
            .children(panel.map(|panel| panel.into_any_element()))
            .into_any_element()
    }

    /// "New chats": which provider a new chat starts with. That is the
    /// composer's remembered provider on this device, so another device's
    /// page explains where to change it instead.
    fn render_new_chats(&self, theme: &Theme, local: bool, cx: &mut Context<Self>) -> AnyElement {
        let card = widgets::section_card(theme).child(widgets::section_header(
            theme,
            "New chats",
            None,
            Vec::new(),
        ));
        if !local {
            let local_name = {
                let state = self.state.read(cx);
                state
                    .local_device_id
                    .as_deref()
                    .and_then(|id| state.devices.iter().find(|d| d.id == id))
                    .map(|d| d.name.clone())
                    .unwrap_or_else(|| "this device".into())
            };
            return card
                .child(
                    widgets::card_row(theme, false).child(widgets::row_text(
                        theme,
                        "Default provider",
                        Some(
                            format!(
                                "The default provider and each provider's defaults belong to the \
                             device you start chats from. Switch to {local_name} to change them."
                            )
                            .into(),
                        ),
                    )),
                )
                .into_any_element();
        }
        let description: SharedString = "New chats start with this provider unless their project \
                                          sets one. Picking a provider in the composer changes it \
                                          too."
            .into();
        let control: Option<AnyElement> = match &self.harnesses {
            Loadable::Ready(list) => {
                let offered = crate::pickers::offered_harnesses(list);
                // The cards' order first, then anything else offered.
                let mut ordered: Vec<(HarnessId, SharedString)> = PROVIDERS
                    .iter()
                    .filter(|(harness, _, _)| offered.iter().any(|d| d.id == *harness))
                    .map(|(harness, name, _)| (*harness, SharedString::from(*name)))
                    .collect();
                ordered.extend(
                    offered
                        .iter()
                        .filter(|d| !PROVIDERS.iter().any(|(harness, _, _)| *harness == d.id))
                        .map(|d| (d.id, SharedString::from(d.name.clone()))),
                );
                if ordered.is_empty() {
                    None
                } else {
                    // What a new chat would really use: the remembered
                    // provider while it is offered, else the first offered
                    // (the composer's own fallback).
                    let effective = self
                        .new_chat_provider
                        .filter(|harness| offered.iter().any(|d| d.id == *harness))
                        .or_else(|| offered.first().map(|d| d.id));
                    let selected = ordered
                        .iter()
                        .position(|(harness, _)| Some(*harness) == effective)
                        .unwrap_or(0);
                    let labels: Vec<SharedString> =
                        ordered.iter().map(|(_, label)| label.clone()).collect();
                    let width = super::provider_defaults::segmented_width(&labels);
                    let ids: Vec<HarnessId> = ordered.iter().map(|(harness, _)| *harness).collect();
                    Some(
                        button::segmented(
                            "new-chat-provider",
                            ordered
                                .iter()
                                .map(|(harness, label)| {
                                    Segment::new(provider_key(*harness), label.clone())
                                })
                                .collect(),
                            selected,
                            width,
                            ControlSize::Md,
                            theme,
                            cx.listener(move |this, ix: &usize, _, cx| {
                                if let Some(harness) = ids.get(*ix) {
                                    this.set_new_chat_provider(*harness, cx);
                                }
                            }),
                        )
                        .into_any_element(),
                    )
                }
            }
            Loadable::Idle | Loadable::Loading => {
                Some(popover::skeleton_bar(200.0, cx.entity_id(), cx))
            }
            Loadable::Error(_) => None,
        };
        let description = match (&self.harnesses, &control) {
            (Loadable::Ready(_), None) => "Turn on a provider below to start new chats.".into(),
            (Loadable::Error(message), _) => {
                SharedString::from(format!("Providers could not load: {message}"))
            }
            _ => description,
        };
        card.child(
            widgets::card_row(theme, false)
                .child(widgets::row_text(
                    theme,
                    "Default provider",
                    Some(description),
                ))
                .children(control.map(|control| widgets::row_controls().child(control))),
        )
        .into_any_element()
    }

    /// "Advanced provider options": a disclosure row in its own section — a
    /// chevron that turns with the open state, a title and one line of
    /// description. The options render below the card when open.
    fn render_advanced_disclosure(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let open = self.advanced_open;
        let ring = theme.focus_ring();
        let turn = motion::follow(
            "providers-advanced-chevron",
            if open { 1.0 } else { 0.0 },
            motion::DISCLOSURE,
        );
        widgets::section_card(theme)
            .child(
                widgets::card_row(theme, true)
                    .id("providers-advanced-disclosure")
                    .role(gpui::Role::Button)
                    .aria_label("Advanced provider options")
                    .aria_expanded(open)
                    .tab_index(0)
                    .cursor_pointer()
                    .flex_nowrap()
                    .hover_fill(
                        "providers-advanced-disclosure",
                        gpui::transparent_black(),
                        theme.fill(FillTier::Quaternary),
                    )
                    .focus_visible(move |style| crate::theme::focus_ring(style, ring))
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_advanced(cx)))
                    .on_key_down(crate::swallow_activation_key)
                    .child(
                        div()
                            .flex_none()
                            .size(ui_rems(16.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                crate::icons::icon(crate::icons::ALT_ARROW_RIGHT)
                                    .size(ui_rems(14.0))
                                    .text_color(theme.readable_muted(0.85))
                                    .with_transformation(gpui::Transformation::rotate(
                                        gpui::percentage(turn * 0.25),
                                    )),
                            ),
                    )
                    .child(
                        widgets::row_text(
                            theme,
                            "Advanced provider options",
                            Some(
                                "Model catalogs, instructions, integrations, execution and chat \
                                 titles."
                                    .into(),
                            ),
                        )
                        .min_w_0(),
                    ),
            )
            .into_any_element()
    }

    /// The title bar's Refresh: an icon button, or while a refresh runs the
    /// same-sized glyph turning in place (not a control, so it can't be
    /// pressed twice).
    fn render_refresh(
        &self,
        refreshing: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        const LABEL: &str = "Refresh accounts and usage";
        if refreshing {
            let glyph = button::glyph(crate::icons::REFRESH, ControlSize::Md)
                .text_color(theme.readable_muted(0.95));
            let glyph: AnyElement = if motion::reduced_motion(cx) {
                glyph.into_any_element()
            } else {
                glyph
                    .with_animation(
                        "accounts-refresh-spin",
                        gpui::Animation::new(Duration::from_millis(900)).repeat(),
                        |glyph, t| {
                            glyph.with_transformation(gpui::Transformation::rotate(
                                gpui::percentage(t),
                            ))
                        },
                    )
                    .into_any_element()
            };
            return div()
                .id("accounts-refresh")
                .role(gpui::Role::Status)
                .aria_label("Refreshing accounts and usage")
                .flex_none()
                .size(ui_rems(ControlSize::Md.height()))
                .flex()
                .items_center()
                .justify_center()
                .opacity(0.7)
                .child(glyph)
                .into_any_element();
        }
        button::icon_button(
            "accounts-refresh",
            crate::icons::REFRESH,
            ControlSize::Md,
            true,
            theme,
        )
        .aria_label(LABEL)
        .tooltip(popover::text_tooltip(LABEL))
        .tooltip_show_delay(popover::TOOLTIP_DELAY)
        .on_click(cx.listener(|this, _, _, cx| {
            this.reload_new_chat_provider(cx);
            this.load(force_usage_for(LoadTrigger::Refresh), cx)
        }))
        .into_any_element()
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
                    .truncate()
                    .cursor_pointer()
                    .hover_fg(id, theme.text_muted, theme.text)
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
                        .text_color(theme.readable_tone(theme.danger_muted, 1.0))
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
    /// title line, two usage-meter ghosts, a badge — the real row's geometry,
    /// so loaded data lands without a layout jump. `dim` fades row two.
    fn render_skeleton_row(&self, dim: bool, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let delta = motion::pulse_delta(&motion::LUNA_PULSE, cx.entity_id(), cx);
        let wash = theme.fill(FillTier::Tertiary);
        let ghost = move |w: gpui::Length, h: f32, round_full: bool| {
            div()
                .w(w)
                .h(ui_rems(h))
                .flex_none()
                .map(|el| {
                    if round_full {
                        el.rounded_full()
                    } else {
                        el.rounded(px(4.0))
                    }
                })
                .bg(wash)
        };
        let meters = div()
            .mt(ui_rems(6.0))
            .w_full()
            .flex()
            .flex_col()
            .gap(ui_rems(7.0))
            .children((0..2).map(|_| {
                div()
                    .w_full()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(ui_rems(8.0))
                    .child(ghost(ui_rems(48.0).into(), 9.0, false))
                    .child(
                        div()
                            .flex_1()
                            .min_w(ui_rems(40.0))
                            .max_w(ui_rems(240.0))
                            .h(ui_rems(5.0))
                            .rounded_full()
                            .bg(theme.fill(FillTier::Quaternary)),
                    )
                    .child(ghost(ui_rems(64.0).into(), 9.0, false))
            }));
        widgets::card_row(theme, false)
            .when(dim, |el| el.opacity(0.6))
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(ui_rems(12.0))
                    .opacity(0.55 + 0.35 * motion::pulse_wave(delta))
                    .child(ghost(ui_rems(32.0).into(), 32.0, true))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(
                                ghost(ui_rems(176.0).into(), 13.0, false)
                                    .max_w(gpui::relative(0.6)),
                            )
                            .child(meters),
                    )
                    .child(ghost(ui_rems(64.0).into(), 20.0, true)),
            )
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
        let local = self.targets_local_device(cx);
        let advanced_sections = self
            .advanced_open
            .then(|| {
                self.advanced_page
                    .as_ref()
                    .map(|page| page.clone().into_any_element())
            })
            .flatten();

        let device_switcher = self.render_device_switcher(&theme, cx);
        let refresh = self.render_refresh(refreshing, &theme, cx);

        // Strips that concern no single provider sit under the title.
        let mut page_strips: Vec<AnyElement> = Vec::new();
        if let Some(notice) = self.error.clone().filter(|notice| notice.harness.is_none()) {
            page_strips.push(
                self.dismissible_error("accounts-page-error-dismiss", notice.message, &theme, cx)
                    .id("accounts-action-error")
                    .into_any_element(),
            );
        }
        if let Loadable::Error(message) = &self.snapshot {
            page_strips.push(
                widgets::error_strip(&theme, message.clone())
                    .id("accounts-load-error")
                    .child(div().flex_1())
                    .child(
                        button::button(
                            "accounts-load-retry",
                            ButtonKind::Secondary,
                            ControlSize::Sm,
                            true,
                            &theme,
                        )
                        .mt(ui_rems(-2.0))
                        .aria_label("Retry loading accounts")
                        .child("Retry")
                        .on_click(cx.listener(|this, _, _, cx| {
                            // Retry IS the visit's first successful list —
                            // force usage.
                            this.load(force_usage_for(LoadTrigger::Retry), cx)
                        })),
                    )
                    .into_any_element(),
            );
        }

        let new_chats = self.render_new_chats(&theme, local, cx);
        let panels = self.defaults_panels.clone();
        let cards: Vec<AnyElement> = PROVIDERS
            .into_iter()
            .map(|(harness, name, cli)| {
                // Defaults belong to this device; another device's page
                // shows its accounts only.
                let panel = local
                    .then(|| {
                        panels
                            .iter()
                            .find(|panel| panel.read(cx).provider() == harness)
                            .cloned()
                    })
                    .flatten();
                self.render_provider_card(harness, name, cli, panel, &theme, now, cx)
            })
            .collect();
        let advanced = self.render_advanced_disclosure(&theme, cx);

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
                            .child(widgets::page_title_bar(
                                &theme,
                                "Providers",
                                None,
                                vec![device_switcher, refresh],
                            ))
                            .child(widgets::page_subtitle(
                                &theme,
                                "Enable the CLIs you use on this device. Claude Code and Codex \
                                 accounts are managed by their own CLIs; OpenCode connects through \
                                 its CLI and shows available usage here. Each provider keeps its \
                                 own defaults for new chats.",
                            ))
                            .children(page_strips)
                            .child(new_chats)
                            .children(cards)
                            // Fine print under the provider cards (luna:
                            // `mt-6 text-[12px] text-muted-foreground/60`).
                            .child(widgets::section_footer(
                                &theme,
                                "Provider sign-in updates the CLI's native login on this \
                                 device. Existing CLI sessions stay with their provider.",
                            ))
                            .child(advanced)
                            .children(advanced_sections),
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
            Some(AccountLifecycleAction::DisconnectCodex)
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

/// Headless QA: the Advanced section (and its Execution card).
#[cfg(feature = "appshots-fixture")]
impl AccountsPage {
    pub fn fixture_open_advanced(&mut self, cx: &mut Context<Self>) {
        if !self.advanced_open {
            self.toggle_advanced(cx);
        }
    }
    pub fn fixture_advanced_page(&self) -> Option<Entity<HarnessesPage>> {
        self.advanced_page.clone()
    }
    /// The page's real scroll container: its viewport and offset.
    pub fn fixture_scroll_state(&self) -> (gpui::Bounds<gpui::Pixels>, f32, f32) {
        let handle = &self.scroll.scroll;
        (
            handle.bounds(),
            f32::from(handle.offset().y),
            f32::from(handle.max_offset().y),
        )
    }
    /// Scroll the page's container down by `delta` (clamped to its range),
    /// as a wheel would.
    pub fn fixture_scroll_by(&mut self, delta: f32, cx: &mut Context<Self>) {
        let handle = &self.scroll.scroll;
        let offset = handle.offset();
        let max = f32::from(handle.max_offset().y).max(0.0);
        let y = (f32::from(offset.y) - delta).clamp(-max, 0.0);
        handle.set_offset(gpui::point(offset.x, gpui::px(y)));
        cx.notify();
    }
}
