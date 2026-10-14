//! CORE-12 shell side of native UI contributions.
//!
//! The shell follows the engine's `WatchUiContributions` (one subscription
//! per engine) into the shared [`ContributionRegistry`], which the command
//! palette, Settings' navigation, the sidebar's navigation and the right
//! rail read. The host owns every element: an extension supplies only data,
//! rendered here with the host's own components, focus ring, keyboard
//! activation and accessible names (each extension entry names its
//! extension).
//!
//! Views are fetched off the render thread (`GetUiContributionView`) and
//! cached by their exact (contribution, package, task): a reply can only
//! land in the slot it was asked for, so a late answer for one task never
//! appears beside another, and a stale package's reply is dropped. Actions
//! (`InvokeUiAction`) carry the task the control was shown for, run once
//! per click while one is in flight, and their outcome applies to that task
//! only. A route, saved layout or rail place whose contribution is gone
//! falls back to the host's known-good default: tasks for a page, Settings
//! → Extensions (the protected recovery route) for a section, the Overview
//! for a panel. Nothing here touches the selected task or the composer.

use std::collections::{HashMap, HashSet};

use luna_proto::{
    ExtensionTrust, UiActionOutcome, UiButtonStyle, UiContext, UiContributionEntry,
    UiContributionsSnapshot, UiHostAction, UiNode, UiOutcomeStatus, UiPlacement, UiTone, UiView,
    UiViewKind, UiViewOutcome,
};

use super::*;
use crate::button::{ButtonKind, ControlSize};
use crate::contributions::{ContributionKey, ContributionRegistry, saved_contribution_id};
use crate::settings::widgets;
use crate::typography::ui_rems;

/// One fetched view's exact identity.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct ViewKey {
    pub(super) contribution: ContributionKey,
    pub(super) package_hash: String,
    pub(super) task_id: Option<String>,
}

#[derive(Debug, Clone)]
pub(super) enum ViewState {
    Loading { request: u64 },
    Ready { view: UiView },
    Failed { message: SharedString },
}

/// An action in flight: (contribution, action id, task).
type Acting = (ContributionKey, String, Option<String>);

#[derive(Default)]
pub(super) struct ShellContributions {
    pub(super) registry: ContributionRegistry,
    watch: Option<Task<()>>,
    /// The engine generation the watch follows.
    watch_generation: Option<u64>,
    /// A saved `contribution:<id>` page waiting for the first snapshot.
    restore: Option<String>,
    pub(super) views: HashMap<ViewKey, ViewState>,
    requests: u64,
    pub(super) acting: HashSet<Acting>,
}

impl ShellContributions {
    pub(super) fn restoring(saved: Option<String>) -> Self {
        Self {
            restore: saved,
            ..Self::default()
        }
    }

    pub(super) fn restore_pending(&self) -> bool {
        self.restore.is_some()
    }

    /// Explicit navigation wins over a page still waiting to be restored.
    pub(super) fn drop_restore(&mut self) {
        self.restore = None;
    }

    /// How a contributed page is saved: by its id, never its handle.
    pub(super) fn saved_page(&self, key: ContributionKey) -> Option<String> {
        self.registry
            .extension(key, UiPlacement::SidebarSection)
            .map(|contribution| {
                format!(
                    "{}{}",
                    crate::contributions::SAVED_CONTRIBUTION_PREFIX,
                    contribution.id
                )
            })
    }

    fn next_request(&mut self) -> u64 {
        self.requests += 1;
        self.requests
    }
}

fn tone_color(theme: &Theme, tone: UiTone) -> gpui::Hsla {
    match tone {
        UiTone::Default => theme.text,
        UiTone::Muted => theme.text_muted,
        UiTone::Success => theme.readable_tone(theme.success, 1.0),
        UiTone::Warning => theme.readable_tone(theme.warning, 1.0),
        UiTone::Danger => theme.danger,
    }
}

/// The host-owned attribution every contributed surface carries.
fn attribution(entry: &UiContributionEntry) -> String {
    let trust = match entry.trust {
        ExtensionTrust::Data => "data only",
        ExtensionTrust::Portable => "portable code",
        ExtensionTrust::Native => "native code, not sandboxed",
    };
    format!("From {} v{} · {trust}", entry.extension_name, entry.version)
}

impl Shell {
    /// Whether the extension entry `key` of `placement` is placed now.
    pub(super) fn contribution_placed(&self, key: ContributionKey, placement: UiPlacement) -> bool {
        self.contributions
            .registry
            .extension(key, placement)
            .is_some()
    }

    /// The rail places of the enabled extensions' task panels (none on the
    /// new-task canvas: a panel is always beside one task).
    pub(super) fn contributed_panels(&self) -> Vec<RightPlace> {
        if self.active_chat.is_empty() {
            return Vec::new();
        }
        self.contributions
            .registry
            .placed(UiPlacement::Panel)
            .filter(|contribution| contribution.entry().is_some())
            .map(|contribution| RightPlace::Contributed(contribution.key))
            .collect()
    }

    pub(super) fn place_key(&self, place: RightPlace) -> String {
        match place {
            RightPlace::Contributed(key) => format!("ext-{}", key.slug()),
            place => place.key().to_owned(),
        }
    }

    pub(super) fn place_label(&self, place: RightPlace) -> SharedString {
        match place {
            RightPlace::Contributed(key) => self
                .contributions
                .registry
                .extension(key, UiPlacement::Panel)
                .map(|contribution| contribution.title.clone())
                .unwrap_or_else(|| RightPlace::Overview.label().into()),
            place => place.label().into(),
        }
    }

    pub(super) fn place_icon(&self, place: RightPlace) -> &'static str {
        match place {
            RightPlace::Contributed(key) => self
                .contributions
                .registry
                .extension(key, UiPlacement::Panel)
                .map_or(icons::WIDGET, |contribution| contribution.icon),
            place => place.icon(),
        }
    }

    pub(super) fn place_accessible_label(&self, place: RightPlace) -> SharedString {
        match place {
            RightPlace::Contributed(key) => self
                .contributions
                .registry
                .extension(key, UiPlacement::Panel)
                .map(|contribution| contribution.accessible_label())
                .unwrap_or_else(|| RightPlace::Overview.label().into()),
            place => place.label().into(),
        }
    }

    /// Follow the attached engine's contributions: one subscription per
    /// engine generation, retried while the engine lives. An engine without
    /// the method (an older one) leaves the host's own entries alone.
    pub(super) fn ensure_contribution_watch(&mut self, cx: &mut Context<Self>) {
        let (engine, generation) = {
            let state = self.state.read(cx);
            (state.engine().cloned(), state.engine_generation())
        };
        let Some(engine) = engine else {
            if self.contributions.watch_generation.take().is_some() {
                self.contributions.watch = None;
                self.contributions.views.clear();
                self.contributions.registry.expire();
            }
            return;
        };
        if self.contributions.watch_generation == Some(generation) {
            return;
        }
        // Another engine: its first snapshot replaces every extension entry.
        self.contributions.registry.expire();
        self.contributions.views.clear();
        self.contributions.watch_generation = Some(generation);
        self.contributions.watch = Some(cx.spawn(async move |this, cx| {
            loop {
                let stream = engine
                    .client()
                    .subscribe_checked(methods::WATCH_UI_CONTRIBUTIONS, serde_json::json!({}))
                    .await;
                if matches!(stream, Err(luna_rpc::RpcError::UnknownMethod(_))) {
                    this.update(cx, |shell, _| shell.contributions.drop_restore())
                        .ok();
                    return;
                }
                if let Ok(mut events) = stream {
                    while let Some(value) = events.recv().await {
                        let Ok(snapshot) = serde_json::from_value::<UiContributionsSnapshot>(value)
                        else {
                            continue;
                        };
                        let keep = this
                            .update(cx, |shell, cx| {
                                if shell.contributions.watch_generation != Some(generation) {
                                    return false;
                                }
                                shell.apply_contributions(&snapshot, cx);
                                true
                            })
                            .unwrap_or(false);
                        if !keep {
                            return;
                        }
                    }
                }
                let live = this
                    .update(cx, |shell, _| {
                        shell.contributions.watch_generation == Some(generation)
                    })
                    .unwrap_or(false);
                if !live {
                    return;
                }
                cx.background_executor().timer(Duration::from_secs(3)).await;
            }
        }));
    }

    fn apply_contributions(&mut self, snapshot: &UiContributionsSnapshot, cx: &mut Context<Self>) {
        let changed = self.contributions.registry.apply(snapshot);
        let ShellContributions {
            registry, views, ..
        } = &mut self.contributions;
        // A view of a package that no longer provides its contribution is
        // never shown again; its replacement is fetched fresh.
        views.retain(|key, _| {
            registry
                .get(key.contribution)
                .and_then(|contribution| contribution.entry())
                .is_some_and(|entry| entry.package_hash == key.package_hash)
        });
        self.restore_contribution_page(cx);
        self.reconcile_contribution_routes(cx);
        if changed {
            cx.notify();
            // The sidebar is a cached view (`render_sidebar`): it repaints
            // only when it is itself invalidated, and it lists the sidebar
            // pages (and, on the Settings route, the sections). A snapshot
            // that only reaches the shell left its rows stale until another
            // change re-rendered it (native QA, add and remove). Snapshots
            // change only on enable, disable, update, removal and safe
            // mode, so invalidate the sidebar and refresh every window, as
            // appearance changes do, and each surface that lists entries
            // repaints from the registry on the next frame.
            self.sidebar_pane.update(cx, |_, cx| cx.notify());
            cx.refresh_windows();
        }
    }

    /// The saved contributed page, once the first snapshot says whether it
    /// still exists. Only while the shell is still on its boot route.
    fn restore_contribution_page(&mut self, cx: &mut Context<Self>) {
        let Some(saved) = self.contributions.restore.take() else {
            return;
        };
        if self.route != Route::Chat {
            return;
        }
        let key = saved_contribution_id(&saved)
            .and_then(|id| self.contributions.registry.find_id(id))
            .filter(|contribution| {
                contribution.entry().is_some()
                    && contribution.placement == UiPlacement::SidebarSection
            })
            .map(|contribution| contribution.key);
        if let Some(key) = key {
            self.open_sidebar_page(SidebarPage::Contributed(key), cx);
        }
    }

    /// Leave a route whose contribution is gone for the host's known-good
    /// default. Runs as snapshots arrive and before each frame (Back and
    /// Forward can land on an older entry).
    pub(super) fn reconcile_contribution_routes(&mut self, cx: &mut Context<Self>) {
        if !self.contributions.registry.loaded() {
            return;
        }
        let route = self.route;
        match route {
            Route::Page(SidebarPage::Contributed(key))
                if !self.contribution_placed(key, UiPlacement::SidebarSection) =>
            {
                self.route = Route::Chat;
                self.nav.push(NavEntry::Chat(self.active_chat.clone()));
                self.sidebar_notice = Some(SidebarNotice::transient_status(
                    "That extension page is no longer available",
                ));
            }
            Route::Settings(SettingsSection::Contributed(key))
                if !self.contribution_placed(key, UiPlacement::SettingsSection) =>
            {
                // Where the extension that provided it is managed.
                self.route = Route::Settings(SettingsSection::Extensions);
                self.nav
                    .push(NavEntry::Settings(SettingsSection::Extensions));
                if let Some(page) = &self.extensions_page {
                    page.update(cx, |page, cx| page.refresh(cx));
                }
                self.sidebar_notice = Some(SidebarNotice::transient_status(
                    "That extension section is no longer available",
                ));
            }
            _ => {}
        }
    }

    /// The view for `key` as placed now (fetching it on first use), with
    /// the entry it belongs to and its exact identity.
    fn contribution_view(
        &mut self,
        key: ContributionKey,
        placement: UiPlacement,
        task_id: Option<String>,
        cx: &mut Context<Self>,
    ) -> Option<(UiContributionEntry, ViewKey, ViewState)> {
        let entry = self
            .contributions
            .registry
            .extension(key, placement)?
            .entry()?
            .clone();
        let view_key = ViewKey {
            contribution: key,
            package_hash: entry.package_hash.clone(),
            task_id,
        };
        if !self.contributions.views.contains_key(&view_key) {
            self.request_contribution_view(&entry, view_key.clone(), cx);
        }
        let state = self.contributions.views.get(&view_key).cloned()?;
        Some((entry, view_key, state))
    }

    /// Ask the engine for one view. Its reply lands only in `view_key`'s
    /// slot, and only if that slot still waits for this request.
    fn request_contribution_view(
        &mut self,
        entry: &UiContributionEntry,
        view_key: ViewKey,
        cx: &mut Context<Self>,
    ) {
        let request = self.contributions.next_request();
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.contributions.views.insert(
                view_key,
                ViewState::Failed {
                    message: "This view can't load: the engine is unavailable.".into(),
                },
            );
            return;
        };
        self.contributions
            .views
            .insert(view_key.clone(), ViewState::Loading { request });
        let mut params = serde_json::json!({
            "contributionId": entry.id,
            "packageHash": entry.package_hash,
        });
        if let Some(task) = &view_key.task_id {
            params["taskId"] = task.clone().into();
        }
        cx.spawn(async move |this, cx| {
            let outcome = engine
                .client()
                .call(methods::GET_UI_CONTRIBUTION_VIEW, params)
                .await
                .map_err(|error| error.to_string())
                .and_then(|value| {
                    serde_json::from_value::<UiViewOutcome>(value)
                        .map_err(|error| error.to_string())
                });
            this.update(cx, |shell, cx| {
                let waiting = matches!(
                    shell.contributions.views.get(&view_key),
                    Some(ViewState::Loading { request: pending }) if *pending == request
                );
                if !waiting {
                    return;
                }
                let state = match outcome {
                    Ok(outcome)
                        if outcome.status == UiOutcomeStatus::Ok
                            && outcome.task_id == view_key.task_id =>
                    {
                        match outcome.view {
                            Some(view) => ViewState::Ready { view },
                            None => ViewState::Failed {
                                message: "The extension returned no view.".into(),
                            },
                        }
                    }
                    Ok(outcome) => ViewState::Failed {
                        message: outcome
                            .message
                            .unwrap_or_else(|| "This view is unavailable.".into())
                            .into(),
                    },
                    Err(error) => ViewState::Failed {
                        message: format!("This view can't load: {error}").into(),
                    },
                };
                shell.contributions.views.insert(view_key, state);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Run one of an extension entry's actions for exactly `task_id` (the
    /// task the control was shown for). A second press of the same action
    /// for the same task waits for the first.
    pub(super) fn invoke_contribution_action(
        &mut self,
        key: ContributionKey,
        action_id: String,
        task_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(entry) = self
            .contributions
            .registry
            .get(key)
            .and_then(|contribution| contribution.entry())
            .cloned()
        else {
            return;
        };
        let acting: Acting = (key, action_id.clone(), task_id.clone());
        if !self.contributions.acting.insert(acting.clone()) {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.contributions.acting.remove(&acting);
            self.sidebar_notice = Some(SidebarNotice::transient_error(format!(
                "{}: the engine is unavailable",
                entry.extension_name
            )));
            cx.notify();
            return;
        };
        let mut params = serde_json::json!({
            "contributionId": entry.id,
            "packageHash": entry.package_hash,
            "actionId": action_id,
            "requestId": uuid::Uuid::new_v4().to_string(),
        });
        if let Some(task) = &task_id {
            params["taskId"] = task.clone().into();
        }
        let view_key = ViewKey {
            contribution: key,
            package_hash: entry.package_hash.clone(),
            task_id,
        };
        cx.notify();
        cx.spawn(async move |this, cx| {
            let outcome = engine
                .client()
                .call(methods::INVOKE_UI_ACTION, params)
                .await
                .map_err(|error| error.to_string())
                .and_then(|value| {
                    serde_json::from_value::<UiActionOutcome>(value)
                        .map_err(|error| error.to_string())
                });
            this.update(cx, |shell, cx| {
                shell.contributions.acting.remove(&acting);
                shell.apply_action_outcome(&entry, view_key, outcome, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn apply_action_outcome(
        &mut self,
        entry: &UiContributionEntry,
        view_key: ViewKey,
        outcome: Result<UiActionOutcome, String>,
        cx: &mut Context<Self>,
    ) {
        let name = entry.extension_name.clone();
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(error) => {
                self.sidebar_notice =
                    Some(SidebarNotice::transient_error(format!("{name}: {error}")));
                return;
            }
        };
        // The answer belongs to exactly the task the action was shown for.
        if outcome.task_id != view_key.task_id {
            self.sidebar_notice = Some(SidebarNotice::transient_error(format!(
                "{name} answered for another task; nothing was applied"
            )));
            return;
        }
        match outcome.status {
            UiOutcomeStatus::Ok => {}
            UiOutcomeStatus::Stale => {
                self.contributions.views.remove(&view_key);
                self.sidebar_notice = Some(SidebarNotice::transient_status(format!(
                    "{name} changed; nothing ran. Showing its current version."
                )));
                return;
            }
            UiOutcomeStatus::Refused | UiOutcomeStatus::Failed => {
                let reason = outcome
                    .message
                    .unwrap_or_else(|| "the action did not run".into());
                self.sidebar_notice =
                    Some(SidebarNotice::transient_error(format!("{name}: {reason}")));
                return;
            }
        }
        if let Some(view) = outcome.view {
            self.contributions
                .views
                .insert(view_key.clone(), ViewState::Ready { view });
        }
        if let Some(message) = outcome.message {
            // Name the task when it is not the one on screen any more.
            let task = match view_key.task_id.as_deref() {
                Some(task) if task != self.active_chat => {
                    format!(" ({})", self.contribution_task_title(task, cx))
                }
                _ => String::new(),
            };
            self.sidebar_notice = Some(SidebarNotice::transient_status(format!(
                "{name}{task}: {}",
                crate::transcript::single_line(&message)
            )));
        }
        if let Some(host) = outcome.host {
            self.run_contribution_host_action(entry, host, view_key.task_id.as_deref(), cx);
        }
    }

    fn contribution_task_title(&self, task: &str, cx: &App) -> String {
        self.state
            .read(cx)
            .chats
            .iter()
            .find(|chat| chat.id == task)
            .and_then(|chat| chat.title.clone())
            .unwrap_or_else(|| "New session".into())
    }

    /// The closed set of host actions, performed by the host's own owners.
    fn run_contribution_host_action(
        &mut self,
        entry: &UiContributionEntry,
        action: UiHostAction,
        task_id: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        match action {
            UiHostAction::NewTask => self.open_new_session(cx),
            UiHostAction::OpenSettings(open) => {
                if let Some(section) = SettingsSection::from_key(&open.section)
                    .filter(|section| *section != SettingsSection::Automations)
                {
                    self.open_settings(section, cx);
                }
            }
            UiHostAction::OpenContribution(open) => {
                // Only another view of the same package.
                let Some((key, placement)) = self
                    .contributions
                    .registry
                    .find_id(open.contribution_id.as_str())
                    .filter(|target| {
                        target
                            .entry()
                            .is_some_and(|other| other.extension_id == entry.extension_id)
                    })
                    .map(|target| (target.key, target.placement))
                else {
                    return;
                };
                match placement {
                    UiPlacement::SettingsSection => {
                        self.open_settings(SettingsSection::Contributed(key), cx)
                    }
                    UiPlacement::SidebarSection => {
                        self.open_sidebar_page(SidebarPage::Contributed(key), cx)
                    }
                    // A panel opens beside exactly the task the action ran for.
                    UiPlacement::Panel => {
                        if task_id.is_some_and(|task| task == self.active_chat) {
                            self.show_right_place(RightPlace::Contributed(key), cx);
                        }
                    }
                    UiPlacement::Command => {}
                }
            }
        }
    }

    /// The extension commands the palette lists for `query`. A task
    /// command is listed only while a task is shown.
    pub(super) fn contributed_commands(&self, query: &str) -> Vec<ContributionKey> {
        self.contributions
            .registry
            .placed(UiPlacement::Command)
            .filter_map(|contribution| {
                let entry = contribution.entry()?;
                if contribution.context == UiContext::Task && self.active_chat.is_empty() {
                    return None;
                }
                let haystack =
                    format!("{} {}", contribution.title, entry.extension_name).to_lowercase();
                query
                    .split_whitespace()
                    .all(|word| haystack.contains(word))
                    .then_some(contribution.key)
            })
            .collect()
    }

    /// Run a chosen extension command: its one action, for the shown task
    /// when it is a task command.
    pub(super) fn run_contribution_command(
        &mut self,
        key: ContributionKey,
        cx: &mut Context<Self>,
    ) {
        let Some(contribution) = self
            .contributions
            .registry
            .extension(key, UiPlacement::Command)
        else {
            return;
        };
        let Some(action) = contribution
            .entry()
            .and_then(|entry| entry.actions.first())
            .map(|action| action.id.clone())
        else {
            return;
        };
        let task = match contribution.context {
            UiContext::None => None,
            UiContext::Task if self.active_chat.is_empty() => return,
            UiContext::Task => Some(self.active_chat.clone()),
        };
        self.invoke_contribution_action(key, action, task, cx);
    }

    /// A contributed settings section or sidebar page: the host's page frame
    /// (title, attribution, Refresh), then the extension's view.
    pub(super) fn render_contribution_page(
        &mut self,
        key: ContributionKey,
        placement: UiPlacement,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let Some((entry, view_key, state)) = self.contribution_view(key, placement, None, cx)
        else {
            return div().into_any_element();
        };
        let title = crate::transcript::single_line(&entry.title);
        let actions = self
            .contribution_refresh(&entry, &view_key, &theme, cx)
            .into_iter()
            .collect();
        let body = self.render_contribution_state(&view_key, &state, &theme, cx);
        div()
            .id(SharedString::from(format!(
                "contribution-page-{}",
                key.slug()
            )))
            .debug_selector(|| "contribution-page".to_string())
            .size_full()
            .overflow_y_scroll()
            .child(
                widgets::page_column()
                    .child(widgets::page_title_bar(&theme, &title, None, actions))
                    .child(widgets::page_subtitle(&theme, attribution(&entry)))
                    .child(widgets::section_card(&theme).child(body)),
            )
            .into_any_element()
    }

    /// A contributed task panel on the right sidebar, for the shown task.
    pub(super) fn render_contribution_panel(
        &mut self,
        key: ContributionKey,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if self.active_chat.is_empty() {
            return div().into_any_element();
        }
        let theme = Theme::of(cx).clone();
        let task = self.active_chat.clone();
        let Some((entry, view_key, state)) =
            self.contribution_view(key, UiPlacement::Panel, Some(task), cx)
        else {
            return self.render_place_home(RightPlace::Overview, cx);
        };
        let refresh = self.contribution_refresh(&entry, &view_key, &theme, cx);
        let body = self.render_contribution_state(&view_key, &state, &theme, cx);
        div()
            .id(SharedString::from(format!(
                "contribution-panel-{}",
                key.slug()
            )))
            .debug_selector(|| "contribution-panel".to_string())
            .size_full()
            .overflow_y_scroll()
            .px(px(Theme::SPACE_SM))
            .pt(ui_rems(2.0))
            .pb(ui_rems(16.0))
            .flex()
            .flex_col()
            .gap(ui_rems(6.0))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(ui_rems(6.0))
                    .px(ui_rems(8.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(ui_rems(11.0))
                            .text_color(theme.readable_faint(0.9))
                            .child(SharedString::from(attribution(&entry))),
                    )
                    .children(refresh),
            )
            .child(body)
            .into_any_element()
    }

    /// Refresh for a worker view (a static one cannot change).
    fn contribution_refresh(
        &self,
        entry: &UiContributionEntry,
        view_key: &ViewKey,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if entry.view != Some(UiViewKind::Worker) {
            return None;
        }
        let loading = matches!(
            self.contributions.views.get(view_key),
            Some(ViewState::Loading { .. })
        );
        let target = view_key.clone();
        Some(
            crate::button::button(
                format!("contribution-refresh-{}", view_key.contribution.slug()),
                ButtonKind::Ghost,
                ControlSize::Sm,
                !loading,
                theme,
            )
            .aria_label(SharedString::from(format!(
                "Refresh {}",
                crate::transcript::single_line(&entry.title)
            )))
            .child("Refresh")
            .on_click(cx.listener(move |this, _, _, cx| {
                this.contributions.views.remove(&target);
                cx.notify();
            }))
            .into_any_element(),
        )
    }

    fn render_contribution_state(
        &mut self,
        view_key: &ViewKey,
        state: &ViewState,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match state {
            ViewState::Loading { .. } => div()
                .id("contribution-loading")
                .debug_selector(|| "contribution-loading".to_string())
                .px(ui_rems(16.0))
                .py(ui_rems(12.0))
                .text_size(ui_rems(12.0))
                .text_color(theme.text_muted)
                .child("Loading…")
                .into_any_element(),
            ViewState::Failed { message } => div()
                .id("contribution-failed")
                .debug_selector(|| "contribution-failed".to_string())
                .p(ui_rems(12.0))
                .child(widgets::warning_strip(theme, message.clone()))
                .into_any_element(),
            ViewState::Ready { view } => self.render_ui_nodes(view_key, view, theme, cx),
        }
    }

    /// The extension's nodes in the host's components. Every control is a
    /// host control: role, focus ring, keyboard activation and name.
    fn render_ui_nodes(
        &mut self,
        view_key: &ViewKey,
        view: &UiView,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = view_key.contribution;
        let slug = key.slug();
        let mut column = div()
            .w_full()
            .flex()
            .flex_col()
            .gap(ui_rems(8.0))
            .px(ui_rems(16.0))
            .py(ui_rems(12.0));
        for (index, node) in view.nodes.iter().take(luna_proto::UI_MAX_NODES).enumerate() {
            let element: AnyElement = match node {
                UiNode::Heading(text) => div()
                    .text_size(ui_rems(14.0))
                    .line_height(ui_rems(20.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme.text)
                    .child(SharedString::from(crate::transcript::single_line(
                        &text.text,
                    )))
                    .into_any_element(),
                UiNode::Text(text) => div()
                    .text_size(ui_rems(13.0))
                    .line_height(ui_rems(19.0))
                    .text_color(tone_color(theme, text.tone))
                    .child(SharedString::from(text.text.clone()))
                    .into_any_element(),
                UiNode::Field(field) => div()
                    .w_full()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .gap_x(ui_rems(12.0))
                    .text_size(ui_rems(13.0))
                    .child(
                        div()
                            .flex_none()
                            .min_w(ui_rems(96.0))
                            .text_color(theme.text_muted)
                            .child(SharedString::from(crate::transcript::single_line(
                                &field.label,
                            ))),
                    )
                    .child(div().flex_1().min_w_0().text_color(theme.text).child(
                        SharedString::from(crate::transcript::single_line(&field.value)),
                    ))
                    .into_any_element(),
                UiNode::Item(item) => {
                    let id = format!("contribution-{slug}-item-{index}");
                    let selector = format!("contribution-item-{}", item.id);
                    let title = crate::transcript::single_line(&item.title);
                    let detail = item.detail.as_deref().map(crate::transcript::single_line);
                    let label = match &detail {
                        Some(detail) => format!("{title}, {detail}"),
                        None => title.clone(),
                    };
                    let row = div()
                        .id(SharedString::from(id.clone()))
                        .debug_selector(move || selector)
                        .w_full()
                        .min_h(ui_rems(36.0))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(ui_rems(10.0))
                        .px(ui_rems(8.0))
                        .py(ui_rems(6.0))
                        .rounded(px(8.0))
                        .aria_label(SharedString::from(label))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .truncate()
                                        .text_size(ui_rems(13.0))
                                        .text_color(theme.text)
                                        .child(SharedString::from(title)),
                                )
                                .when_some(detail, |column, detail| {
                                    column.child(
                                        div()
                                            .truncate()
                                            .text_size(ui_rems(11.5))
                                            .text_color(theme.text_muted)
                                            .child(SharedString::from(detail)),
                                    )
                                }),
                        )
                        .when_some(item.badge.clone(), |row, badge| {
                            row.child(widgets::badge(
                                theme,
                                crate::transcript::single_line(&badge),
                            ))
                        });
                    match item.action.clone() {
                        Some(action) => {
                            let task = view_key.task_id.clone();
                            let ring = theme.focus_ring();
                            row.role(gpui::Role::Button)
                                .tab_index(0)
                                .cursor_pointer()
                                .focus_visible(move |style| crate::theme::focus_ring(style, ring))
                                .hover_wash(SharedString::from(id))
                                .on_key_down(crate::swallow_activation_key)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.invoke_contribution_action(
                                        key,
                                        action.clone(),
                                        task.clone(),
                                        cx,
                                    )
                                }))
                                .into_any_element()
                        }
                        None => row.into_any_element(),
                    }
                }
                UiNode::Button(button) => {
                    let busy = self.contributions.acting.contains(&(
                        key,
                        button.action.clone(),
                        view_key.task_id.clone(),
                    ));
                    let selector = format!("contribution-button-{}", button.action);
                    let action = button.action.clone();
                    let task = view_key.task_id.clone();
                    let label = crate::transcript::single_line(&button.label);
                    crate::button::button(
                        format!("contribution-{slug}-button-{index}"),
                        match button.style {
                            UiButtonStyle::Primary => ButtonKind::Primary,
                            UiButtonStyle::Secondary => ButtonKind::Secondary,
                        },
                        ControlSize::Sm,
                        !busy,
                        theme,
                    )
                    .debug_selector(move || selector)
                    .aria_label(SharedString::from(label.clone()))
                    .child(SharedString::from(label))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.invoke_contribution_action(key, action.clone(), task.clone(), cx)
                    }))
                    .into_any_element()
                }
                UiNode::Divider => div()
                    .w_full()
                    .h(px(1.0))
                    .bg(theme.separator(false))
                    .into_any_element(),
            };
            column = column.child(element);
        }
        column.into_any_element()
    }
}
