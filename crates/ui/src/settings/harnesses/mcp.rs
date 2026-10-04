//! CC-11: Claude MCP rows in the shared "Skill library and MCP" card.
//!
//! Renders the provider-reported details the engine attaches to an MCP
//! inventory item (status, transport, per-launch-mode load, the selected
//! chat's session report, MCP App tools) plus two owner-local actions:
//! provider-owned per-project enable/disable, and inert inspection of a
//! declared MCP Apps `ui://` resource. App views are never rendered here.

use gpui::{AnyElement, Context, IntoElement, SharedString, Task, div, prelude::*, px};
use luna_proto::{
    HarnessId, IntegrationActionAvailability, IntegrationInventoryItem, IntegrationMcpDetails,
    ProviderMcpActionOutcome, ProviderMcpActionReceipt, ProviderMcpAppResourceView,
    ProviderMcpAppSupport, ProviderMcpStatus,
};
use luna_rpc::methods;

use super::{HarnessesPage, meta_text};
use crate::settings::widgets;
use crate::theme::Theme;

const BUSY_REASON: &str = "Unavailable while Claude handles the previous MCP request.";

/// Per-page MCP action state. Results are bound to the provider/project they
/// were requested for and dropped when either changes.
#[derive(Default)]
pub(super) struct McpPanel {
    busy: bool,
    scope: Option<(HarnessId, Option<String>)>,
    notice: Option<(bool, String)>,
    resource: Option<Result<ProviderMcpAppResourceView, String>>,
    task: Option<Task<()>>,
}

fn status_badge(theme: &Theme, status: ProviderMcpStatus) -> gpui::Div {
    let label = format!("Claude: {}", status.label());
    match status {
        ProviderMcpStatus::Connected => widgets::badge_active(theme, label),
        ProviderMcpStatus::Failed => widgets::badge(theme, label)
            .border_color(theme.danger.opacity(0.5))
            .text_color(theme.readable_tone(theme.danger, 1.0)),
        ProviderMcpStatus::NeedsAuth | ProviderMcpStatus::Pending => {
            widgets::badge(theme, label).text_color(theme.readable_tone(theme.warning, 1.0))
        }
        ProviderMcpStatus::Disabled | ProviderMcpStatus::Unknown => widgets::badge(theme, label),
    }
}

fn mcp_button(
    theme: &Theme,
    id: SharedString,
    label: SharedString,
    text: &'static str,
    enabled: bool,
) -> gpui_base::Button {
    let accent = theme.accent;
    let wash = theme.ink(0.08);
    let text_color = theme.text;
    gpui_base::Button::new(id.clone())
        .role(gpui::Role::Button)
        .accessibility_id(id)
        .accessibility_label(label)
        .disabled(!enabled)
        .tab_index(0)
        .tab_stop(enabled)
        .flex_none()
        .px(px(10.0))
        .h(px(24.0))
        .flex()
        .items_center()
        .rounded(px(6.0))
        .border_1()
        .border_color(theme.border)
        .text_size(crate::typography::ui_rems(11.5))
        .text_color(theme.readable_muted(0.9))
        .when(enabled, |button| {
            button
                .cursor_pointer()
                .hover(move |style| style.bg(wash).text_color(text_color))
                .focus_visible(move |style| crate::theme::focus_ring(style, accent))
        })
        .when(!enabled, |button| {
            button.opacity(0.45).aria_description(BUSY_REASON)
        })
        .child(text)
}

impl HarnessesPage {
    fn mcp_scope(&self, cx: &Context<Self>) -> (HarnessId, Option<String>) {
        (
            self.integration_provider,
            self.state
                .read(cx)
                .selected_space_row()
                .map(|space| space.id.clone()),
        )
    }

    /// Drop results that belong to another provider or project.
    pub(super) fn mcp_sync_scope(&mut self, cx: &Context<Self>) {
        let scope = self.mcp_scope(cx);
        if self.mcp.scope.as_ref() != Some(&scope) {
            self.mcp = McpPanel {
                scope: Some(scope),
                ..McpPanel::default()
            };
        }
    }

    pub(super) fn mcp_set_enabled(
        &mut self,
        identity: String,
        server: String,
        enabled: bool,
        cx: &mut Context<Self>,
    ) {
        if self.mcp.busy || self.target_device.is_some() {
            return;
        }
        self.mcp_sync_scope(cx);
        let scope = self.mcp_scope(cx);
        let params = serde_json::json!({
            "provider": scope.0,
            "projectId": scope.1,
            "identity": identity,
            "server": server,
            "enabled": enabled,
        });
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.mcp.busy = true;
        self.mcp.notice = None;
        self.mcp.task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(methods::SET_PROVIDER_MCP_SERVER_ENABLED, params)
                .await
                .map_err(|error| error.to_string())
                .and_then(|value| {
                    serde_json::from_value::<ProviderMcpActionReceipt>(value)
                        .map_err(|error| error.to_string())
                });
            this.update(cx, |page, cx| {
                if page.mcp_scope(cx) != scope {
                    return;
                }
                page.mcp.busy = false;
                page.mcp.notice = Some(match result {
                    Ok(receipt) => (
                        receipt.outcome == ProviderMcpActionOutcome::Verified,
                        receipt.message,
                    ),
                    Err(error) => (false, error),
                });
                // Re-read Claude's own state rather than trusting the receipt.
                page.load_inventory(true, cx);
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub(super) fn mcp_inspect(
        &mut self,
        identity: String,
        server: String,
        uri: String,
        cx: &mut Context<Self>,
    ) {
        if self.mcp.busy || self.target_device.is_some() {
            return;
        }
        self.mcp_sync_scope(cx);
        let scope = self.mcp_scope(cx);
        let params = serde_json::json!({
            "provider": scope.0,
            "projectId": scope.1,
            "identity": identity,
            "server": server,
            "uri": uri,
        });
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.mcp.busy = true;
        self.mcp.resource = None;
        self.mcp.task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(methods::READ_PROVIDER_MCP_APP_RESOURCE, params)
                .await
                .map_err(|error| error.to_string())
                .and_then(|value| {
                    serde_json::from_value::<ProviderMcpAppResourceView>(value)
                        .map_err(|error| error.to_string())
                });
            this.update(cx, |page, cx| {
                if page.mcp_scope(cx) != scope {
                    return;
                }
                page.mcp.busy = false;
                page.mcp.resource = Some(result);
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// Provider-reported details and actions for one MCP inventory row.
    pub(super) fn render_mcp_details(
        &self,
        theme: &Theme,
        item: &IntegrationInventoryItem,
        details: &IntegrationMcpDetails,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let identity = item.identity.clone().unwrap_or_default();
        let enabled = !self.mcp.busy && !identity.is_empty();
        let mut facts = vec![status_badge(theme, details.status).into_any_element()];
        if let Some(transport) = &details.transport {
            facts.push(widgets::badge(theme, transport.clone()).into_any_element());
        }
        if let Some(version) = &details.server_version {
            facts.push(widgets::badge(theme, version.clone()).into_any_element());
        }
        facts.push(
            widgets::badge(
                theme,
                match details.tool_count {
                    1 => "1 tool".to_owned(),
                    count => format!("{count} tools"),
                },
            )
            .into_any_element(),
        );
        if let Some(session) = &details.session {
            facts.push(
                widgets::badge(theme, format!("This chat: {}", session.status.label()))
                    .into_any_element(),
            );
        }
        let mut block = div().mt(px(6.0)).flex().flex_col().child(
            div()
                .flex()
                .flex_row()
                .flex_wrap()
                .gap(px(6.0))
                .children(facts),
        );
        if let Some(error) = &details.error {
            block = block.child(
                div()
                    .mt(px(4.0))
                    .text_size(crate::typography::ui_rems(11.5))
                    .text_color(theme.readable_tone(theme.danger, 1.0))
                    .child(format!(
                        "Claude error{}: {error}",
                        details
                            .error_code
                            .as_deref()
                            .map(|code| format!(" ({code})"))
                            .unwrap_or_default()
                    )),
            );
        }
        let mut actions: Vec<AnyElement> = Vec::new();
        for tool in &details.app_tools {
            let Some(uri) = tool.ui_resource_uri.clone() else {
                continue;
            };
            block = block.child(meta_text(
                theme,
                format!("MCP App view: {} → {uri} (not rendered by LUNA)", tool.name),
            ));
            match &details.app_support {
                ProviderMcpAppSupport::ReadSupported => {
                    let label: SharedString =
                        format!("Inspect MCP App resource {uri} from {}", item.name).into();
                    let id: SharedString = format!("mcp-inspect-{identity}-{}", tool.name).into();
                    let (identity, server) = (identity.clone(), item.name.clone());
                    actions.push(
                        mcp_button(theme, id, label, "Inspect app view", enabled)
                            .on_click(cx.listener(move |page, _, _, cx| {
                                page.mcp_inspect(identity.clone(), server.clone(), uri.clone(), cx)
                            }))
                            .into_any_element(),
                    );
                }
                ProviderMcpAppSupport::Unavailable { reason } => {
                    block = block.child(meta_text(theme, reason.clone()));
                }
                ProviderMcpAppSupport::Unknown => {
                    block = block.child(meta_text(
                        theme,
                        "Check provider to verify this Claude CLI can read MCP App resources.",
                    ));
                }
            }
        }
        let toggle = if item.actions.disable == IntegrationActionAvailability::Supported {
            Some(false)
        } else if item.actions.enable == IntegrationActionAvailability::Supported {
            Some(true)
        } else {
            None
        };
        if let Some(target) = toggle {
            let verb = if target { "Enable" } else { "Disable" };
            let label: SharedString =
                format!("{verb} MCP server {} for this project in Claude", item.name).into();
            let id: SharedString = format!("mcp-toggle-{identity}").into();
            let (identity, server) = (identity.clone(), item.name.clone());
            actions.push(
                mcp_button(
                    theme,
                    id,
                    label,
                    if target {
                        "Enable for this project"
                    } else {
                        "Disable for this project"
                    },
                    enabled,
                )
                .on_click(cx.listener(move |page, _, _, cx| {
                    page.mcp_set_enabled(identity.clone(), server.clone(), target, cx)
                }))
                .into_any_element(),
            );
        }
        if !actions.is_empty() {
            block = block.child(
                div()
                    .mt(px(6.0))
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .gap(px(6.0))
                    .children(actions),
            );
        }
        block.into_any_element()
    }

    /// Action receipt and inspected resource, below the inventory rows.
    pub(super) fn render_mcp_panel(&self, theme: &Theme) -> Option<AnyElement> {
        if self.mcp.notice.is_none() && self.mcp.resource.is_none() && !self.mcp.busy {
            return None;
        }
        let mut panel = div()
            .id("mcp-result-panel")
            .role(gpui::Role::Status)
            .mt(px(12.0))
            .p(px(12.0))
            .rounded(px(8.0))
            .border_1()
            .border_color(theme.border)
            .flex()
            .flex_col();
        if self.mcp.busy {
            panel = panel.child(meta_text(theme, "Waiting for Claude…"));
        }
        if let Some((ok, message)) = &self.mcp.notice {
            panel = panel.child(
                div()
                    .text_size(crate::typography::ui_rems(12.0))
                    .text_color(if *ok { theme.text } else { theme.warning })
                    .child(message.clone()),
            );
        }
        match &self.mcp.resource {
            Some(Ok(view)) => {
                panel = panel
                    .child(widgets::row_title(
                        theme,
                        format!("MCP App resource · {} · {}", view.server, view.uri),
                    ))
                    .child(meta_text(theme, view.render_unavailable_reason.clone()))
                    .child(meta_text(
                        theme,
                        format!(
                            "{} · {} bytes · sha256 {}{}",
                            view.mime_type.as_deref().unwrap_or("unknown type"),
                            view.byte_len,
                            view.sha256,
                            if view.content_count > 1 {
                                format!(" · {} content items (first shown)", view.content_count)
                            } else {
                                String::new()
                            }
                        ),
                    ));
                let csp = [
                    ("connect", &view.csp_connect_domains),
                    ("resources", &view.csp_resource_domains),
                    ("frames", &view.csp_frame_domains),
                    ("base URI", &view.csp_base_uri_domains),
                ]
                .iter()
                .filter(|(_, domains)| !domains.is_empty())
                .map(|(label, domains)| format!("{label}: {}", domains.join(", ")))
                .collect::<Vec<_>>();
                panel = panel.child(meta_text(
                    theme,
                    if csp.is_empty() {
                        "Declared CSP: no external domains".to_owned()
                    } else {
                        format!("Declared CSP · {}", csp.join(" · "))
                    },
                ));
                if !view.permissions.is_empty() {
                    panel = panel.child(meta_text(
                        theme,
                        format!("Requested permissions: {}", view.permissions.join(", ")),
                    ));
                }
                if let Some(source) = &view.text_preview {
                    panel = panel.child(
                        div()
                            .id("mcp-app-source")
                            .role(gpui::Role::Document)
                            .aria_label("MCP App source, inert text")
                            .mt(px(8.0))
                            .p(px(8.0))
                            .rounded(px(6.0))
                            .bg(theme.ink(0.04))
                            .font_family(theme.font_mono.clone())
                            .text_size(crate::typography::ui_rems(11.0))
                            .text_color(theme.text_muted)
                            .child(if view.truncated {
                                format!("{source}\n… (preview truncated)")
                            } else {
                                source.clone()
                            }),
                    );
                } else if view.binary {
                    panel = panel.child(meta_text(theme, "Binary content: size and hash only."));
                }
            }
            Some(Err(error)) => {
                panel = panel.child(widgets::error_strip(theme, error.clone()));
            }
            None => {}
        }
        Some(panel.into_any_element())
    }
}

/// Headless E2E for the production page against a real in-process engine and
/// the deterministic fake Claude CLI. Each step uses the same page methods
/// the rendered buttons call; accessibility is read from GPUI's AccessKit tree.
#[cfg(feature = "appshots-fixture")]
pub mod fixture {
    use super::*;
    use gpui::{AnyWindowHandle, AsyncApp, Entity, Window};
    use luna_proto::IntegrationInventoryView;
    use serde_json::{Value, json};
    use std::path::Path;
    use std::time::Duration;

    fn capture(window: AnyWindowHandle, cx: &mut AsyncApp, path: &Path) -> anyhow::Result<()> {
        window.update(cx, |_, window, cx| -> anyhow::Result<()> {
            window.draw(cx).clear();
            window.render_to_image()?.save(path)?;
            Ok(())
        })?
    }

    #[cfg(target_os = "macos")]
    fn activate_accessibility(window: &Window) -> anyhow::Result<()> {
        use objc2::runtime::AnyObject;
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let handle = HasWindowHandle::window_handle(window)
            .map_err(|error| anyhow::anyhow!("window handle unavailable: {error}"))?;
        let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
            anyhow::bail!("fixture window is not an AppKit window");
        };
        let view = appkit.ns_view.as_ptr().cast::<AnyObject>();
        // SAFETY: the view belongs to this live window on the main thread; each
        // returned pointer is checked before it is messaged.
        let ns_window: *mut AnyObject = unsafe { objc2::msg_send![view, window] };
        anyhow::ensure!(!ns_window.is_null(), "fixture view has no NSWindow");
        let content: *mut AnyObject = unsafe { objc2::msg_send![ns_window, contentView] };
        anyhow::ensure!(!content.is_null(), "fixture NSWindow has no content view");
        let _children: *mut AnyObject = unsafe { objc2::msg_send![content, accessibilityChildren] };
        Ok(())
    }

    #[cfg(not(target_os = "macos"))]
    fn activate_accessibility(_: &Window) -> anyhow::Result<()> {
        anyhow::bail!("AccessKit activation readback is implemented for macOS only")
    }

    fn a11y_buttons(window: AnyWindowHandle, cx: &mut AsyncApp) -> anyhow::Result<Vec<Value>> {
        for _ in 0..2 {
            window.update(cx, |_, window, cx| {
                window.draw(cx).clear();
            })?;
        }
        let json = window
            .update(cx, |_, window, _| window.debug_a11y_tree_json())?
            .ok_or_else(|| anyhow::anyhow!("AccessKit tree was not built"))?;
        let tree: Value = serde_json::from_str(&json)?;
        Ok(tree["nodes"]
            .as_object()
            .into_iter()
            .flatten()
            .filter(|(_, node)| node["aria"]["role"] == "Button")
            .map(|(_, node)| node["aria"].clone())
            .collect())
    }

    fn button<'a>(buttons: &'a [Value], label: &str) -> anyhow::Result<&'a Value> {
        buttons
            .iter()
            .find(|aria| aria["label"] == label)
            .ok_or_else(|| anyhow::anyhow!("no AccessKit button labelled {label:?}"))
    }

    fn clickable(aria: &Value) -> bool {
        aria["on_action"]
            .as_array()
            .is_some_and(|actions| actions.iter().any(|action| action == "Click"))
    }

    async fn wait_until(
        page: &Entity<HarnessesPage>,
        cx: &mut AsyncApp,
        what: &str,
        done: impl Fn(&HarnessesPage) -> bool,
    ) -> anyhow::Result<()> {
        for _ in 0..600 {
            if page.read_with(cx, |page, _| done(page)) {
                return Ok(());
            }
            cx.background_executor()
                .timer(Duration::from_millis(100))
                .await;
        }
        anyhow::bail!("timed out waiting for {what}")
    }

    fn view(page: &HarnessesPage) -> Option<&IntegrationInventoryView> {
        match &page.inventory {
            crate::popover::Loadable::Ready(view) => Some(view),
            _ => None,
        }
    }

    fn status_of(page: &HarnessesPage, name: &str) -> Option<ProviderMcpStatus> {
        view(page)?
            .items
            .iter()
            .find(|item| item.name == name && item.mcp.is_some())?
            .mcp
            .as_ref()
            .map(|details| details.status)
    }

    pub async fn run_mcp_inventory_e2e(
        page: Entity<HarnessesPage>,
        window: AnyWindowHandle,
        output: &Path,
        cx: &mut AsyncApp,
    ) -> anyhow::Result<Value> {
        page.update(cx, |page, cx| {
            page.integration_provider = HarnessId::ClaudeCode;
            page.load_inventory(true, cx);
        });
        wait_until(&page, cx, "Claude MCP readback", |page| {
            status_of(page, "userdemo").is_some() && status_of(page, "projdemo").is_some()
        })
        .await?;
        let first = page
            .read_with(cx, |page, _| view(page).cloned())
            .ok_or_else(|| anyhow::anyhow!("inventory not ready"))?;
        let rows: Vec<Value> = first
            .items
            .iter()
            .filter_map(|item| {
                let details = item.mcp.as_ref()?;
                Some(json!({
                    "name": item.name,
                    "scope": item.scope,
                    "status": details.status,
                    "attached": item.attached,
                    "launches": details.launches,
                    "appTools": details.app_tools.len(),
                }))
            })
            .collect();
        let demo = first
            .items
            .iter()
            .find(|item| item.name == "userdemo" && item.mcp.is_some())
            .unwrap();
        anyhow::ensure!(
            demo.attached == Some(true),
            "selected chat session evidence missing: {demo:?}"
        );
        let identity = demo.identity.clone().unwrap_or_default();
        capture(window, cx, &output.join("01-claude-mcp-inventory.png"))?;

        let activation = window.update(cx, |_, window, _| {
            activate_accessibility(window).map_err(|error| error.to_string())
        });
        if let Err(error) = activation {
            anyhow::bail!("AccessKit activation failed: {error}");
        }
        let buttons = a11y_buttons(window, cx)?;
        let disable_label = "Disable MCP server userdemo for this project in Claude";
        let inspect_label = "Inspect MCP App resource ui://demo/widget.html from userdemo";
        for label in [
            disable_label,
            inspect_label,
            "Check provider skill and MCP catalog",
        ] {
            let aria = button(&buttons, label)?;
            anyhow::ensure!(clickable(aria), "{label} is not clickable: {aria}");
        }
        anyhow::ensure!(
            buttons.iter().all(
                |aria| aria["label"] != "Disable MCP server dyndemo for this project in Claude"
            ),
            "explicit --mcp-config servers must not offer a per-project toggle"
        );

        // Disable through the button's own handler, then observe busy state.
        page.update(cx, |page, cx| {
            page.mcp_set_enabled(identity.clone(), "userdemo".into(), false, cx)
        });
        let busy_buttons = a11y_buttons(window, cx)?;
        let busy_disable = button(&busy_buttons, disable_label)?.clone();
        wait_until(&page, cx, "verified disable", |page| {
            !page.mcp.busy
                && page.mcp.notice.as_ref().is_some_and(|(ok, _)| *ok)
                && status_of(page, "userdemo") == Some(ProviderMcpStatus::Disabled)
        })
        .await?;
        let disable_notice = page.read_with(cx, |page, _| page.mcp.notice.clone());
        capture(window, cx, &output.join("02-after-disable.png"))?;
        let disabled_buttons = a11y_buttons(window, cx)?;
        button(
            &disabled_buttons,
            "Enable MCP server userdemo for this project in Claude",
        )?;

        // A disabled server's app view cannot be read: truthful error.
        page.update(cx, |page, cx| {
            page.mcp_inspect(
                identity.clone(),
                "userdemo".into(),
                "ui://demo/widget.html".into(),
                cx,
            )
        });
        wait_until(&page, cx, "inspect refusal", |page| {
            matches!(page.mcp.resource, Some(Err(_)))
        })
        .await?;
        let refusal = page.read_with(cx, |page, _| match &page.mcp.resource {
            Some(Err(error)) => error.clone(),
            _ => String::new(),
        });
        capture(window, cx, &output.join("03-inspect-disabled-refused.png"))?;

        page.update(cx, |page, cx| {
            page.mcp_set_enabled(identity.clone(), "userdemo".into(), true, cx)
        });
        wait_until(&page, cx, "verified enable", |page| {
            !page.mcp.busy && status_of(page, "userdemo") == Some(ProviderMcpStatus::Connected)
        })
        .await?;
        page.update(cx, |page, cx| {
            page.mcp_inspect(
                identity.clone(),
                "userdemo".into(),
                "ui://demo/widget.html".into(),
                cx,
            )
        });
        wait_until(&page, cx, "inspected app resource", |page| {
            matches!(page.mcp.resource, Some(Ok(_)))
        })
        .await?;
        let resource = page.read_with(cx, |page, _| match &page.mcp.resource {
            Some(Ok(view)) => Some(view.clone()),
            _ => None,
        });
        let resource = resource.unwrap();
        anyhow::ensure!(!resource.rendered && resource.text_preview.is_some());
        capture(window, cx, &output.join("04-app-resource-inspected.png"))?;
        cx.update(|cx| crate::appearance::set_mode(crate::appearance::AppearanceMode::Light, cx));
        capture(window, cx, &output.join("05-app-resource-light.png"))?;
        Ok(json!({
            "rows": rows,
            "help": first.help,
            "buttons": {
                "disable": button(&buttons, disable_label)?,
                "inspect": button(&buttons, inspect_label)?,
                "disableWhileBusy": busy_disable,
            },
            "disableNotice": disable_notice,
            "inspectRefusal": refusal,
            "resource": {
                "uri": resource.uri,
                "sha256": resource.sha256,
                "byteLen": resource.byte_len,
                "rendered": resource.rendered,
                "cspResourceDomains": resource.csp_resource_domains,
            },
        }))
    }
}
