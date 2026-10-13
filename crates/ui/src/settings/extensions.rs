//! Settings → Extensions (CORE-06): the local packages installed on this
//! device and this profile's enablement.
//!
//! Installing verifies and stores a package as a disabled candidate; it never
//! runs package code. Enabling opens a review of the exact package: its hash,
//! trust class and every requested capability. Required capabilities stay
//! on, optional ones are the user's choice, and the engine receives exactly
//! that hash and that set. The page reads and writes only through the
//! engine's local-only extension methods.
//!
//! CORE-19: a disabled package whose extension has another version enabled
//! offers **Update to** or **Roll back to** instead of enabling. Its review
//! shows both versions, what happens to the data, and every capability the
//! enabled version was not granted, marked so and off unless it is required:
//! nothing the old version was not granted is granted without a decision
//! here. Running work is waited for unless the owner chooses to stop it.
//! **Remove…** and **Erase data…** are separate confirmations, and **Safe
//! mode** keeps every extension from running while all of this stays
//! available.

use std::collections::BTreeSet;

use gpui::{AnyElement, Context, Entity, SharedString, Subscription, Window, div, prelude::*};
use luna_proto::{
    ExtensionDataPlan, ExtensionDataView, ExtensionIntegrityStatus, ExtensionPackageView,
    ExtensionSafeModeSource, ExtensionSafeModeView, ExtensionState, ExtensionSwitchKind,
    ExtensionSwitchView, ExtensionTrust, ExtensionsSnapshot,
};
use luna_rpc::methods;

use crate::button::{ButtonKind, ControlSize};
use crate::popover;
use crate::settings::widgets;
use crate::state::AppState;
use crate::theme::Theme;
use crate::typography::ui_rems;

/// The enable or switch review: one exact package and the capabilities left
/// granted. `switch` is set when another version is enabled (CORE-19).
struct Review {
    package: ExtensionPackageView,
    granted: BTreeSet<String>,
    switch: Option<ExtensionSwitchView>,
    /// Stop running work instead of waiting for it (switches only).
    stop_running: bool,
}

/// A destructive action waiting for its confirmation.
enum Confirm {
    Remove(Box<ExtensionPackageView>),
    Erase(Box<ExtensionDataView>),
}

/// What a package row offers, in order. The render and the lifecycle tests
/// read the same list.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RowAction {
    Disable,
    Review,
    Switch {
        kind: ExtensionSwitchKind,
        version: String,
        blocked: Option<String>,
    },
    Release,
    Remove,
}

impl RowAction {
    fn label(&self) -> String {
        match self {
            Self::Disable => "Disable".into(),
            Self::Review => "Review and enable".into(),
            Self::Switch {
                kind: ExtensionSwitchKind::Update,
                version,
                ..
            } => format!("Update to v{version}"),
            Self::Switch {
                kind: ExtensionSwitchKind::Rollback,
                version,
                ..
            } => format!("Roll back to v{version}"),
            Self::Release => "Release".into(),
            Self::Remove => "Remove…".into(),
        }
    }
}

fn row_actions(package: &ExtensionPackageView) -> Vec<RowAction> {
    let mut actions = Vec::new();
    if package.state == ExtensionState::Enabled {
        actions.push(RowAction::Disable);
    } else if let Some(switch) = &package.switch {
        actions.push(RowAction::Switch {
            kind: switch.kind,
            version: package.version.clone(),
            blocked: switch.blocked.clone(),
        });
    } else {
        actions.push(RowAction::Review);
    }
    if package.quarantine_reason.is_some() {
        actions.push(RowAction::Release);
    }
    actions.push(RowAction::Remove);
    actions
}

fn row_badges(package: &ExtensionPackageView) -> Vec<&'static str> {
    let mut badges = vec![if package.state == ExtensionState::Enabled {
        "Enabled"
    } else {
        "Off"
    }];
    if package.quarantine_reason.is_some() {
        badges.push("Quarantined");
    }
    badges
}

fn safe_mode_copy(safe_mode: &ExtensionSafeModeView) -> String {
    match safe_mode.source {
        ExtensionSafeModeSource::Profile => "Safe mode is on. No extension runs; you can still disable, roll back, remove or erase them, and LUNA's built-in agents work as usual.".into(),
        ExtensionSafeModeSource::Launch => "Safe mode is on for this launch (LUNA_EXTENSIONS_SAFE_MODE). No extension runs until LUNA is started without it; you can still disable, roll back, remove or erase them.".into(),
    }
}

fn data_plan_copy(plan: &ExtensionDataPlan) -> String {
    match plan {
        ExtensionDataPlan::None => {
            "This version keeps no data; existing data stays untouched.".into()
        }
        ExtensionDataPlan::Keep { schema_version } => {
            format!("Its data (schema {schema_version}) carries over unchanged.")
        }
        ExtensionDataPlan::Fresh { schema_version } => {
            format!("It starts with empty data (schema {schema_version}).")
        }
        ExtensionDataPlan::Migrate {
            from_schema_version,
            to_schema_version,
        } => format!(
            "It migrates a copy of the data from schema {from_schema_version} to {to_schema_version}. The current data is kept for a rollback, and nothing changes if the migration fails."
        ),
        ExtensionDataPlan::Restore {
            schema_version,
            written_by_version,
            ..
        } => format!(
            "It goes back to the schema-{schema_version} copy last written by v{written_by_version}. Changes made since are kept aside, not merged."
        ),
    }
}

fn data_summary(data: &ExtensionDataView) -> String {
    let mut parts = vec![
        format!("Schema {}", data.current.schema_version),
        format!("{} bytes", data.current.size_bytes),
        format!("written by v{}", data.current.version),
    ];
    match data.kept.len() {
        0 => {}
        1 => parts.push("1 earlier copy kept".into()),
        kept => parts.push(format!("{kept} earlier copies kept")),
    }
    if !data.installed {
        parts.push("package removed".into());
    }
    parts.join(" · ")
}

pub struct ExtensionsPage {
    state: Entity<AppState>,
    scroll: widgets::PageScroll,
    snapshot: Option<ExtensionsSnapshot>,
    loading: bool,
    /// An install or enablement change is in flight; actions wait for it.
    busy: bool,
    /// Why the last listing failed; cleared by the next successful one.
    load_error: Option<SharedString>,
    /// Why the last install or enablement change failed; dismissible.
    error: Option<SharedString>,
    review: Option<Review>,
    confirm: Option<Confirm>,
    _observe: Subscription,
}

impl ExtensionsPage {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let observe = cx.observe(&state, |_, _, cx| cx.notify());
        let mut page = Self {
            state,
            scroll: widgets::PageScroll::default(),
            snapshot: None,
            loading: false,
            busy: false,
            load_error: None,
            error: None,
            review: None,
            confirm: None,
            _observe: observe,
        };
        page.refresh(cx);
        page
    }

    /// Re-read the installed packages (each visit, and after every change).
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.load_error = Some("Extensions can't load: the engine is unavailable.".into());
            cx.notify();
            return;
        };
        self.loading = true;
        cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(methods::LIST_EXTENSIONS, serde_json::json!({}))
                .await
                .map_err(|error| error.to_string())
                .and_then(|value| {
                    serde_json::from_value::<ExtensionsSnapshot>(value)
                        .map_err(|error| error.to_string())
                });
            this.update(cx, |page, cx| {
                page.loading = false;
                match result {
                    Ok(snapshot) => {
                        page.snapshot = Some(snapshot);
                        page.load_error = None;
                    }
                    Err(error) => {
                        page.load_error = Some(format!("Extensions can't load: {error}").into());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn choose_package(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let receiver = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: true,
            multiple: false,
            prompt: Some("Install Extension".into()),
        });
        cx.spawn(async move |this, cx| {
            let path = match receiver.await {
                Ok(Ok(Some(mut paths))) => paths.pop(),
                _ => None,
            };
            let Some(path) = path else {
                return;
            };
            this.update(cx, |page, cx| page.install(path, cx)).ok();
        })
        .detach();
    }

    fn install(&mut self, path: std::path::PathBuf, cx: &mut Context<Self>) {
        let params = serde_json::json!({ "path": path.display().to_string() });
        self.change(methods::INSTALL_EXTENSION, params, "Install failed", cx);
    }

    fn open_review(&mut self, package: ExtensionPackageView, cx: &mut Context<Self>) {
        // Start from the required set: optional capabilities are opt-in.
        let granted = package
            .capabilities
            .iter()
            .filter(|capability| capability.required)
            .map(|capability| capability.id.to_string())
            .collect();
        self.review = Some(Review {
            package,
            granted,
            switch: None,
            stop_running: false,
        });
        cx.notify();
    }

    /// Review a switch to `package`. Required capabilities start on; others
    /// start on only if the enabled version already had them. A capability
    /// the new version adds is never on unless it is required.
    fn open_switch(&mut self, package: ExtensionPackageView, cx: &mut Context<Self>) {
        let Some(switch) = package.switch.clone() else {
            return;
        };
        if switch.blocked.is_some() {
            return;
        }
        let granted = package
            .capabilities
            .iter()
            .filter(|capability| {
                capability.required || !switch.new_capabilities.contains(&capability.id)
            })
            .map(|capability| capability.id.to_string())
            .collect();
        self.review = Some(Review {
            package,
            granted,
            switch: Some(switch),
            stop_running: false,
        });
        cx.notify();
    }

    fn toggle_stop_running(&mut self, cx: &mut Context<Self>) {
        if let Some(review) = self.review.as_mut() {
            review.stop_running = !review.stop_running;
            cx.notify();
        }
    }

    fn toggle_grant(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(review) = self.review.as_mut() else {
            return;
        };
        let required = review
            .package
            .capabilities
            .iter()
            .any(|capability| capability.required && capability.id.as_str() == id);
        if required {
            return;
        }
        if !review.granted.remove(id) {
            review.granted.insert(id.to_owned());
        }
        cx.notify();
    }

    fn confirm_review(&mut self, cx: &mut Context<Self>) {
        let Some(review) = self.review.take() else {
            return;
        };
        let grants = review.granted.into_iter().collect::<Vec<_>>();
        match review.switch {
            None => {
                let params = serde_json::json!({
                    "extensionId": review.package.id,
                    "packageHash": review.package.package_hash,
                    "enabled": true,
                    "grants": grants,
                });
                self.change(methods::SET_EXTENSION_ENABLED, params, "Enable failed", cx);
            }
            Some(switch) => {
                let params = serde_json::json!({
                    "extensionId": review.package.id,
                    "fromPackageHash": switch.from_package_hash,
                    "toPackageHash": review.package.package_hash,
                    "grants": grants,
                    "active": if review.stop_running { "stop" } else { "drain" },
                });
                let failure = match switch.kind {
                    ExtensionSwitchKind::Update => "Update failed",
                    ExtensionSwitchKind::Rollback => "Rollback failed",
                };
                self.change(methods::UPDATE_EXTENSION, params, failure, cx);
            }
        }
    }

    fn release(&mut self, package: &ExtensionPackageView, cx: &mut Context<Self>) {
        let params = serde_json::json!({
            "extensionId": package.id,
            "packageHash": package.package_hash,
        });
        self.change(
            methods::RELEASE_EXTENSION_QUARANTINE,
            params,
            "Release failed",
            cx,
        );
    }

    fn ask(&mut self, confirm: Confirm, cx: &mut Context<Self>) {
        if !self.busy {
            self.confirm = Some(confirm);
            cx.notify();
        }
    }

    fn confirm_destructive(&mut self, cx: &mut Context<Self>) {
        match self.confirm.take() {
            Some(Confirm::Remove(package)) => {
                let params = serde_json::json!({
                    "extensionId": package.id,
                    "packageHash": package.package_hash,
                });
                self.change(methods::REMOVE_EXTENSION, params, "Remove failed", cx);
            }
            Some(Confirm::Erase(data)) => {
                let params = serde_json::json!({ "extensionId": data.extension_id });
                self.change(methods::ERASE_EXTENSION_DATA, params, "Erase failed", cx);
            }
            None => {}
        }
    }

    fn set_safe_mode(&mut self, enabled: bool, cx: &mut Context<Self>) {
        let params = serde_json::json!({ "enabled": enabled });
        self.change(
            methods::SET_EXTENSION_SAFE_MODE,
            params,
            "Safe mode change failed",
            cx,
        );
    }

    fn disable(&mut self, package: &ExtensionPackageView, cx: &mut Context<Self>) {
        let params = serde_json::json!({
            "extensionId": package.id,
            "packageHash": package.package_hash,
            "enabled": false,
        });
        self.change(methods::SET_EXTENSION_ENABLED, params, "Disable failed", cx);
    }

    /// One install or enablement call, then a fresh listing either way.
    fn change(
        &mut self,
        method: &'static str,
        params: serde_json::Value,
        failure: &'static str,
        cx: &mut Context<Self>,
    ) {
        if self.busy {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.error = Some(format!("{failure}: the engine is unavailable.").into());
            cx.notify();
            return;
        };
        self.busy = true;
        self.error = None;
        cx.spawn(async move |this, cx| {
            let result = engine.client().call(method, params).await;
            this.update(cx, |page, cx| {
                page.busy = false;
                if let Err(error) = result {
                    page.error = Some(format!("{failure}: {error}").into());
                }
                page.refresh(cx);
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn dismiss_error(&mut self, cx: &mut Context<Self>) {
        self.error = None;
        cx.notify();
    }

    fn on_scroll_hovered(&mut self, hovered: &bool, _: &mut Window, cx: &mut Context<Self>) {
        if self.scroll.set_list_hovered(*hovered) {
            cx.notify();
        }
    }

    fn package_row(
        &self,
        theme: &Theme,
        package: &ExtensionPackageView,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let changed = package.integrity.status == ExtensionIntegrityStatus::Failed;
        let tile = widgets::row_tile(
            theme,
            match package.trust {
                ExtensionTrust::Native => crate::icons::SHIELD_WARNING,
                ExtensionTrust::Portable | ExtensionTrust::Data => crate::icons::WIDGET,
            },
        );
        let hash = package.package_hash.clone();
        let meta = vec![
            div()
                .child(SharedString::from(format!("v{}", package.version)))
                .into_any_element(),
            div()
                .child(SharedString::from(package.publisher.clone()))
                .into_any_element(),
            div()
                .child(SharedString::from(trust_label(package.trust)))
                .into_any_element(),
            div()
                .id(SharedString::from(format!("extension-hash-{hash}")))
                .font_family(theme.font_mono.clone())
                .text_size(ui_rems(11.0))
                .tooltip(popover::text_tooltip(hash.clone()))
                .child(SharedString::from(short_hash(&hash)))
                .into_any_element(),
        ];
        let mut controls = widgets::row_controls();
        for badge in row_badges(package) {
            controls = controls.child(if badge == "Enabled" {
                widgets::badge_active(theme, badge)
            } else {
                widgets::badge(theme, badge)
            });
        }
        let mut blocked = None;
        for action in row_actions(package) {
            let (kind, key, enabled) = match &action {
                RowAction::Disable => (ButtonKind::Ghost, "disable", !self.busy),
                RowAction::Review => (ButtonKind::Secondary, "review", !self.busy && !changed),
                RowAction::Switch {
                    blocked: reason, ..
                } => {
                    blocked = reason.clone();
                    (
                        ButtonKind::Secondary,
                        "switch",
                        !self.busy && !changed && reason.is_none(),
                    )
                }
                RowAction::Release => (ButtonKind::Ghost, "release", !self.busy),
                RowAction::Remove => (ButtonKind::Ghost, "remove", !self.busy),
            };
            let label = action.label();
            let target = package.clone();
            let button = crate::button::button(
                format!("extension-{key}-{hash}"),
                kind,
                ControlSize::Sm,
                enabled,
                theme,
            )
            .aria_label(SharedString::from(format!(
                "{label} {}",
                package.display_name
            )))
            .on_click(cx.listener(move |this, _, _, cx| {
                if this.busy {
                    return;
                }
                match &action {
                    RowAction::Disable => this.disable(&target, cx),
                    RowAction::Review => {
                        if target.integrity.status != ExtensionIntegrityStatus::Failed {
                            this.open_review(target.clone(), cx);
                        }
                    }
                    RowAction::Switch { .. } => {
                        if target.integrity.status != ExtensionIntegrityStatus::Failed {
                            this.open_switch(target.clone(), cx);
                        }
                    }
                    RowAction::Release => this.release(&target, cx),
                    RowAction::Remove => this.ask(Confirm::Remove(Box::new(target.clone())), cx),
                }
            }))
            .child(SharedString::from(label));
            controls = controls.child(button);
        }
        let mut row = widgets::card_row(theme, false)
            .child(tile)
            .child(
                widgets::row_text(
                    theme,
                    package.display_name.clone(),
                    Some(package.id.as_str().to_owned().into()),
                )
                .child(widgets::meta_line(theme, meta)),
            )
            .child(controls);
        if let Some(reason) = &package.quarantine_reason {
            row = row.child(
                widgets::row_block().child(
                    widgets::warning_strip(
                        theme,
                        format!(
                            "This exact package is quarantined in this profile after repeated crashes ({reason}). Release it to run it again, or remove it."
                        ),
                    )
                    .mt(ui_rems(0.0))
                    .w_full(),
                ),
            );
        }
        if let Some(reason) = blocked {
            row = row.child(
                widgets::row_block().child(
                    widgets::warning_strip(
                        theme,
                        format!("Can't switch to this version: {reason}"),
                    )
                    .mt(ui_rems(0.0))
                    .w_full(),
                ),
            );
        }
        if changed {
            let detail = package
                .integrity
                .detail
                .clone()
                .unwrap_or_else(|| "its files changed".into());
            row = row.child(
                widgets::row_block().child(
                    widgets::warning_strip(
                        theme,
                        format!(
                            "This package changed after it was installed ({detail}). It can't be enabled; reinstall it."
                        ),
                    )
                    .mt(ui_rems(0.0))
                    .w_full(),
                ),
            );
        }
        row.into_any_element()
    }

    fn render_review(
        &mut self,
        viewport: gpui::Size<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let theme = Theme::of(cx).for_popup();
        let review = self.review.as_ref()?;
        let package = &review.package;
        let mut capabilities = div().mt(ui_rems(12.0)).flex().flex_col().gap(ui_rems(10.0));
        if package.capabilities.is_empty() {
            capabilities = capabilities.child(popover::dialog_body(
                &theme,
                "This package asks for no capabilities.",
            ));
        }
        for capability in &package.capabilities {
            let id = capability.id.to_string();
            let on = review.granted.contains(&id);
            let switch = if capability.required {
                widgets::switch_control_disabled(
                    SharedString::from(format!("extension-grant-{id}")),
                    capability.label.clone(),
                    true,
                    "Required to enable this package",
                    &theme,
                )
            } else {
                let toggled = id.clone();
                widgets::switch_control(
                    SharedString::from(format!("extension-grant-{id}")),
                    capability.label.clone(),
                    on,
                    &theme,
                    cx,
                    move |this: &mut Self, cx| this.toggle_grant(&toggled, cx),
                )
            };
            let mut description = capability.id.to_string();
            if capability.required {
                description.push_str(" · required");
            }
            if let Some(switch) = review
                .switch
                .as_ref()
                .filter(|switch| switch.new_capabilities.contains(&capability.id))
            {
                description.push_str(&format!(" · not granted to v{}", switch.from_version));
            }
            if let Some(reason) = &capability.reason {
                description.push_str(" · ");
                description.push_str(reason);
            }
            capabilities = capabilities.child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(ui_rems(12.0))
                    .child(widgets::row_text(
                        &theme,
                        capability.label.clone(),
                        Some(description.into()),
                    ))
                    .child(switch),
            );
        }
        let (title, body, confirm) = match &review.switch {
            None => (
                format!("Enable {}?", package.display_name),
                format!(
                    "{} {} from {}. Enabling applies to this profile only.",
                    package.id.as_str(),
                    package.version,
                    package.origin.source
                ),
                "Enable",
            ),
            Some(switch) => {
                let (verb, confirm) = match switch.kind {
                    ExtensionSwitchKind::Update => ("Update", "Update"),
                    ExtensionSwitchKind::Rollback => ("Roll back", "Roll back"),
                };
                (
                    format!("{verb} {} to v{}?", package.display_name, package.version),
                    format!(
                        "{}: v{} → v{}, from {}. This applies to this profile only; the grants below replace the current ones.",
                        package.id.as_str(),
                        switch.from_version,
                        package.version,
                        package.origin.source
                    ),
                    confirm,
                )
            }
        };
        let mut lifecycle = div().flex().flex_col().gap(ui_rems(8.0));
        if let Some(switch) = &review.switch {
            lifecycle = lifecycle.child(popover::dialog_body(&theme, data_plan_copy(&switch.data)));
            if !switch.dropped_grants.is_empty() {
                let dropped = switch
                    .dropped_grants
                    .iter()
                    .map(|grant| grant.to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                lifecycle = lifecycle.child(popover::dialog_body(
                    &theme,
                    format!("No longer requested, so no longer granted: {dropped}."),
                ));
            }
            if switch.running_instances > 0 {
                let copy = if review.stop_running {
                    format!(
                        "{} running instance(s) stop now. A call in progress ends without an answer; LUNA records it as unknown and never sends it again.",
                        switch.running_instances
                    )
                } else {
                    format!(
                        "{} running instance(s) take no new calls and stop once their calls finish (up to 30 seconds); otherwise nothing changes.",
                        switch.running_instances
                    )
                };
                lifecycle = lifecycle.child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(ui_rems(12.0))
                        .child(widgets::row_text(
                            &theme,
                            "Stop running work instead of waiting",
                            Some(copy.into()),
                        ))
                        .child(widgets::switch_control(
                            SharedString::from("extension-review-stop-running"),
                            "Stop running work instead of waiting",
                            review.stop_running,
                            &theme,
                            cx,
                            |this: &mut Self, cx| this.toggle_stop_running(cx),
                        )),
                );
            }
        }
        let card = popover::dialog_card(&theme)
            .w(ui_rems(460.0))
            .child(popover::dialog_title(&theme, &title))
            .child(div().mt(ui_rems(8.0)).child(popover::dialog_body(&theme, body)))
            .child(
                div()
                    .mt(ui_rems(6.0))
                    .font_family(theme.font_mono.clone())
                    .text_size(ui_rems(11.0))
                    .text_color(theme.readable_muted(0.8))
                    .child(SharedString::from(package.package_hash.clone())),
            )
            .when(package.trust == ExtensionTrust::Native, |card| {
                card.child(widgets::warning_strip(
                    &theme,
                    "This package runs native code with your account's access. LUNA does not sandbox it; enable it only if you trust its publisher.",
                ))
            })
            .child(div().mt(ui_rems(8.0)).child(lifecycle))
            .child(capabilities)
            .child(
                div()
                    .mt(ui_rems(16.0))
                    .flex()
                    .flex_row()
                    .justify_end()
                    .gap(ui_rems(8.0))
                    .child(
                        popover::btn_ghost(&theme, "Cancel", "extension-review-cancel")
                            .id("extension-review-cancel")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.review = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        popover::btn_primary(&theme, confirm)
                            .id("extension-review-enable")
                            .on_click(cx.listener(|this, _, _, cx| this.confirm_review(cx))),
                    ),
            )
            .into_any_element();
        Some(popover::modal("extension-review-dialog", viewport, card))
    }

    /// The Remove… or Erase data… confirmation.
    fn render_confirm(
        &mut self,
        viewport: gpui::Size<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let theme = Theme::of(cx).for_popup();
        let (title, body, action) = match self.confirm.as_ref()? {
            Confirm::Remove(package) => (
                format!("Remove {} v{}?", package.display_name, package.version),
                "LUNA stops it, deletes the package from this device and releases its quarantine. If it is enabled here, it is disabled first. Its data in this profile is kept until you erase it.".to_string(),
                "Remove",
            ),
            Confirm::Erase(data) => (
                format!("Erase {}'s data?", data.extension_id.as_str()),
                format!(
                    "This deletes its data in this profile{}. Installed packages and permissions don't change. This can't be undone.",
                    match data.kept.len() {
                        0 => String::new(),
                        1 => ", including 1 copy kept for rollback".into(),
                        kept => format!(", including {kept} copies kept for rollback"),
                    }
                ),
                "Erase",
            ),
        };
        let card = popover::dialog_card(&theme)
            .w(ui_rems(420.0))
            .child(popover::dialog_title(&theme, &title))
            .child(
                div()
                    .mt(ui_rems(8.0))
                    .child(popover::dialog_body(&theme, body)),
            )
            .child(
                div()
                    .mt(ui_rems(16.0))
                    .flex()
                    .flex_row()
                    .justify_end()
                    .gap(ui_rems(8.0))
                    .child(
                        popover::btn_ghost(&theme, "Cancel", "extension-confirm-cancel")
                            .id("extension-confirm-cancel")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.confirm = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        popover::btn_danger(&theme, action)
                            .id("extension-confirm-action")
                            .on_click(cx.listener(|this, _, _, cx| this.confirm_destructive(cx))),
                    ),
            )
            .into_any_element();
        Some(popover::modal("extension-confirm-dialog", viewport, card))
    }

    fn data_card(
        &self,
        theme: &Theme,
        data: &[ExtensionDataView],
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let mut card = widgets::section_card(theme).child(widgets::section_header(
            theme,
            "Extension data in this profile",
            Some(
                "Removing an extension keeps its data. Erase it here when you no longer need it."
                    .into(),
            ),
            Vec::new(),
        ));
        for entry in data {
            let target = entry.clone();
            let key = entry.extension_id.as_str().replace(['/', '.'], "-");
            card = card.child(
                widgets::card_row(theme, false)
                    .child(widgets::row_tile(theme, crate::icons::WIDGET))
                    .child(widgets::row_text(
                        theme,
                        entry.extension_id.as_str().to_owned(),
                        Some(data_summary(entry).into()),
                    ))
                    .child(
                        widgets::row_controls().child(
                            crate::button::button(
                                format!("extension-erase-{key}"),
                                ButtonKind::Ghost,
                                ControlSize::Sm,
                                !self.busy,
                                theme,
                            )
                            .aria_label(SharedString::from(format!(
                                "Erase the data of {}",
                                entry.extension_id.as_str()
                            )))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.ask(Confirm::Erase(Box::new(target.clone())), cx)
                            }))
                            .child(SharedString::from("Erase data…")),
                        ),
                    ),
            );
        }
        card
    }
}

impl popover::ScrollRailHost for ExtensionsPage {
    fn rail_bar(&mut self) -> &mut popover::MenuScrollbarState {
        self.scroll.rail_bar()
    }

    fn rail_scroll(&self) -> Option<gpui::ScrollHandle> {
        self.scroll.rail_scroll()
    }
}

fn trust_label(trust: ExtensionTrust) -> &'static str {
    match trust {
        ExtensionTrust::Data => "Data only",
        ExtensionTrust::Portable => "Sandboxed code",
        ExtensionTrust::Native => "Native code",
    }
}

/// `abcd1234…wxyz`, the full hash in the tooltip.
fn short_hash(hash: &str) -> String {
    if hash.len() > 12 && hash.is_ascii() {
        format!("{}…{}", &hash[..8], &hash[hash.len() - 4..])
    } else {
        hash.to_string()
    }
}

impl Render for ExtensionsPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let dialog = self.render_review(window.viewport_size(), cx);
        let confirm = self.render_confirm(window.viewport_size(), cx);
        let packages = self
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.packages.clone())
            .unwrap_or_default();
        let grants_error = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.grants_error.clone());
        let safe_mode = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.safe_mode.clone());
        let data = self
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.data.clone())
            .unwrap_or_default();

        let install = widgets::ghost_action("extensions-install", !self.busy, &theme)
            .aria_label("Install an extension package")
            .on_click(cx.listener(|this, _, _, cx| this.choose_package(cx)))
            .child(crate::button::glyph(
                crate::icons::ADD_CIRCLE,
                ControlSize::Md,
            ))
            .child(SharedString::from("Install…"))
            .into_any_element();
        let mut actions = vec![install];
        if safe_mode.is_none() && self.snapshot.is_some() {
            actions.push(
                widgets::ghost_action("extensions-safe-mode", !self.busy, &theme)
                    .aria_label("Turn on safe mode: no extension runs")
                    .on_click(cx.listener(|this, _, _, cx| this.set_safe_mode(true, cx)))
                    .child(SharedString::from("Safe mode"))
                    .into_any_element(),
            );
        }

        let mut card = widgets::section_card(&theme).child(widgets::section_header(
            &theme,
            "Installed on this device",
            Some(
                "Extensions stay off until you review and enable them. Enabling applies to this profile only."
                    .into(),
            ),
            Vec::new(),
        ));
        if let Some(safe_mode) = &safe_mode {
            let mut strip = widgets::card_row(&theme, false).child(
                widgets::warning_strip(&theme, safe_mode_copy(safe_mode))
                    .mt(ui_rems(0.0))
                    .w_full()
                    .id("extensions-safe-mode-banner"),
            );
            if safe_mode.profile_enabled {
                strip = strip.child(
                    widgets::row_controls().child(
                        crate::button::button(
                            "extensions-safe-mode-off",
                            ButtonKind::Secondary,
                            ControlSize::Sm,
                            !self.busy,
                            &theme,
                        )
                        .aria_label("Turn off safe mode")
                        .on_click(cx.listener(|this, _, _, cx| this.set_safe_mode(false, cx)))
                        .child(SharedString::from("Turn off safe mode")),
                    ),
                );
            }
            card = card.child(strip);
        }
        if let Some(message) = self.error.clone() {
            card = card.child(
                widgets::card_row(&theme, false).child(
                    widgets::error_strip(&theme, message)
                        .mt(ui_rems(0.0))
                        .w_full()
                        .id("extensions-error")
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| this.dismiss_error(cx))),
                ),
            );
        }
        if let Some(message) = self.load_error.clone() {
            card = card.child(
                widgets::card_row(&theme, false).child(
                    widgets::error_strip(&theme, message)
                        .mt(ui_rems(0.0))
                        .w_full(),
                ),
            );
        }
        if let Some(message) = grants_error {
            card = card.child(
                widgets::card_row(&theme, false).child(
                    widgets::warning_strip(
                        &theme,
                        format!("This profile's extension permissions can't be read, so every extension is off: {message}"),
                    )
                    .mt(ui_rems(0.0))
                    .w_full(),
                ),
            );
        }
        if packages.is_empty() {
            let copy = if self.loading && self.snapshot.is_none() {
                "Loading…"
            } else {
                "No extensions installed"
            };
            card = card.child(
                widgets::card_row(&theme, false)
                    .child(widgets::row_tile(&theme, crate::icons::WIDGET))
                    .child(widgets::row_text(
                        &theme,
                        copy,
                        Some(
                            "Choose Install… and pick a package folder or a .tar/.tar.gz archive. Installing never runs its code."
                                .into(),
                        ),
                    )),
            );
        } else {
            for package in &packages {
                card = card.child(self.package_row(&theme, package, cx));
            }
        }

        let count = packages.len();
        let data_card = (!data.is_empty()).then(|| self.data_card(&theme, &data, cx));
        let scrollbar = popover::rail(self, "extensions-page-scrollbar", &theme, cx);
        div()
            .id("extensions-page-host")
            .relative()
            .size_full()
            .on_hover(cx.listener(Self::on_scroll_hovered))
            .child(
                div()
                    .id("extensions-page")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll.scroll)
                    .child(
                        widgets::page_column()
                            .child(widgets::page_title_bar(
                                &theme,
                                "Extensions",
                                (count > 0).then_some(count),
                                actions,
                            ))
                            .child(widgets::page_subtitle(
                                &theme,
                                "Local extension packages for this device. LUNA verifies every file when it installs a package and runs nothing until you enable it.",
                            ))
                            .child(card)
                            .children(data_card),
                    ),
            )
            .children(scrollbar)
            .when_some(dialog, |el, dialog| el.child(dialog))
            .when_some(confirm, |el, confirm| el.child(confirm))
    }
}

#[cfg(test)]
#[path = "extensions_lifecycle_tests.rs"]
mod lifecycle_tests;
