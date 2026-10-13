//! Settings → Extensions (CORE-06): the local packages installed on this
//! device and this profile's enablement.
//!
//! Installing verifies and stores a package as a disabled candidate; it never
//! runs package code. Enabling opens a review of the exact package: its hash,
//! trust class and every requested capability. Required capabilities stay
//! on, optional ones are the user's choice, and the engine receives exactly
//! that hash and that set. The page reads and writes only through the
//! engine's local-only extension methods.

use std::collections::BTreeSet;

use gpui::{AnyElement, Context, Entity, SharedString, Subscription, Window, div, prelude::*};
use luna_proto::{
    ExtensionIntegrityStatus, ExtensionPackageView, ExtensionState, ExtensionTrust,
    ExtensionsSnapshot,
};
use luna_rpc::methods;

use crate::button::{ButtonKind, ControlSize};
use crate::popover;
use crate::settings::widgets;
use crate::state::AppState;
use crate::theme::Theme;
use crate::typography::ui_rems;

/// The enable review: one exact package and the capabilities left granted.
struct Review {
    package: ExtensionPackageView,
    granted: BTreeSet<String>,
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
        self.review = Some(Review { package, granted });
        cx.notify();
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

    fn confirm_enable(&mut self, cx: &mut Context<Self>) {
        let Some(review) = self.review.take() else {
            return;
        };
        let params = serde_json::json!({
            "extensionId": review.package.id,
            "packageHash": review.package.package_hash,
            "enabled": true,
            "grants": review.granted.into_iter().collect::<Vec<_>>(),
        });
        self.change(methods::SET_EXTENSION_ENABLED, params, "Enable failed", cx);
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
        let enabled = package.state == ExtensionState::Enabled;
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
        let action = if enabled {
            let package = package.clone();
            crate::button::button(
                format!("extension-disable-{hash}"),
                ButtonKind::Ghost,
                ControlSize::Sm,
                !self.busy,
                theme,
            )
            .aria_label(SharedString::from(format!(
                "Disable {}",
                package.display_name
            )))
            .on_click(cx.listener(move |this, _, _, cx| this.disable(&package, cx)))
            .child(SharedString::from("Disable"))
        } else {
            let package = package.clone();
            crate::button::button(
                format!("extension-review-{hash}"),
                ButtonKind::Secondary,
                ControlSize::Sm,
                !self.busy && !changed,
                theme,
            )
            .aria_label(SharedString::from(format!(
                "Review and enable {}",
                package.display_name
            )))
            .on_click(cx.listener(move |this, _, _, cx| {
                if !this.busy && package.integrity.status != ExtensionIntegrityStatus::Failed {
                    this.open_review(package.clone(), cx);
                }
            }))
            .child(SharedString::from("Review and enable"))
        };
        let controls = widgets::row_controls()
            .child(if enabled {
                widgets::badge_active(theme, "Enabled")
            } else {
                widgets::badge(theme, "Off")
            })
            .child(action);
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
        let card = popover::dialog_card(&theme)
            .w(ui_rems(460.0))
            .child(popover::dialog_title(
                &theme,
                &format!("Enable {}?", package.display_name),
            ))
            .child(
                div().mt(ui_rems(8.0)).child(popover::dialog_body(
                    &theme,
                    format!(
                        "{} {} from {}. Enabling applies to this profile only.",
                        package.id.as_str(),
                        package.version,
                        package.origin.source
                    ),
                )),
            )
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
                        popover::btn_primary(&theme, "Enable")
                            .id("extension-review-enable")
                            .on_click(cx.listener(|this, _, _, cx| this.confirm_enable(cx))),
                    ),
            )
            .into_any_element();
        Some(popover::modal("extension-review-dialog", viewport, card))
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
        let packages = self
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.packages.clone())
            .unwrap_or_default();
        let grants_error = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.grants_error.clone());

        let install = widgets::ghost_action("extensions-install", !self.busy, &theme)
            .aria_label("Install an extension package")
            .on_click(cx.listener(|this, _, _, cx| this.choose_package(cx)))
            .child(crate::button::glyph(
                crate::icons::ADD_CIRCLE,
                ControlSize::Md,
            ))
            .child(SharedString::from("Install…"))
            .into_any_element();

        let mut card = widgets::section_card(&theme).child(widgets::section_header(
            &theme,
            "Installed on this device",
            Some(
                "Extensions stay off until you review and enable them. Enabling applies to this profile only."
                    .into(),
            ),
            Vec::new(),
        ));
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
                                vec![install],
                            ))
                            .child(widgets::page_subtitle(
                                &theme,
                                "Local extension packages for this device. LUNA verifies every file when it installs a package and runs nothing until you enable it.",
                            ))
                            .child(card),
                    ),
            )
            .children(scrollbar)
            .when_some(dialog, |el, dialog| el.child(dialog))
    }
}
