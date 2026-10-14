//! CORE-12 native UI contributions: the declarative contract a package uses
//! to place commands, settings sections, sidebar pages and task panels in
//! the native shell, and the bounded view-model vocabulary the host renders.
//!
//! A package never supplies GPUI entities, callbacks or code that runs on
//! the render thread. It supplies data:
//! - a hash-pinned `luna-ui.json` declaration (read without running code),
//!   naming each contribution's stable id, placement, title, context and
//!   actions;
//! - views, as a bounded list of [`UiNode`]s, either static in that file or
//!   returned by the package's worker when the host asks for them, off the
//!   render thread, with a deadline.
//!
//! The host owns rendering, focus, accessibility, selection, drafts,
//! navigation and recovery. A contribution can only be *added* to its
//! placement, after the host's own entries; it can never replace or hide the
//! protected host UI (Settings → Extensions and its safe-mode controls,
//! permission and approval controls, the composer's Send and Stop). Its ids,
//! stored in saved layouts, are data: an id that is no longer contributed
//! falls back to the host's known-good default.
//!
//! The same descriptor shape ([`UiPlacement`], [`UiContext`], stable
//! namespaced ids) describes LUNA's own built-in shell entries, which use the
//! reserved `luna.shell/` namespace.

use serde::{Deserialize, Serialize};

use crate::contract::{ContributionId, ExtensionId};
use crate::extension_packages::ExtensionTrust;

/// The declaration's file name at the package root; it must be listed (and
/// so hash-pinned) in the manifest's `files`.
pub const UI_DECLARATION_FILE: &str = "luna-ui.json";
pub const UI_DECLARATION_VERSION: u32 = 1;
/// Namespace of LUNA's own built-in shell contributions.
pub const UI_BUILTIN_NAMESPACE: &str = "luna.shell";

/// Bounds the host enforces on declarations, views and results.
pub const UI_MAX_DECLARATION_BYTES: u64 = 128 * 1024;
pub const UI_MAX_CONTRIBUTIONS: usize = 32;
pub const UI_MAX_ACTIONS: usize = 16;
pub const UI_MAX_NODES: usize = 128;
/// Titles, labels, values, badges and details: one line each.
pub const UI_MAX_LABEL_CHARS: usize = 80;
pub const UI_MAX_VALUE_CHARS: usize = 200;
/// Body text of a `text` node.
pub const UI_MAX_TEXT_CHARS: usize = 2_000;
/// An action's result message.
pub const UI_MAX_MESSAGE_CHARS: usize = 500;
/// A whole view, serialized.
pub const UI_MAX_VIEW_BYTES: usize = 32 * 1024;
/// Action, item and worker method ids.
pub const UI_MAX_ID_BYTES: usize = 64;
/// The worker's deadline to return a view, and to run an action.
pub const UI_VIEW_TIMEOUT_MS: u64 = 5_000;
pub const UI_ACTION_TIMEOUT_MS: u64 = 15_000;

/// The icon vocabulary a contribution may name; the host maps each to its
/// own glyph and uses `widget` for none.
pub const UI_ICONS: &[&str] = &[
    "widget",
    "list",
    "checklist",
    "notebook",
    "info",
    "star",
    "bolt",
    "clipboard",
];

/// Titles a contribution may not use (compared trimmed, ignoring case): the
/// names of the host's protected recovery, permission and composer controls.
pub const UI_PROTECTED_TITLES: &[&str] = &[
    "extensions",
    "safe mode",
    "turn off safe mode",
    "permissions",
    "permission mode",
    "allow",
    "deny",
    "approve",
    "send",
    "stop",
];

/// Built-in settings sections an `openSettings` host action may open.
pub const UI_SETTINGS_SECTIONS: &[&str] = &[
    "general",
    "projects",
    "providers",
    "extensions",
    "appearance",
    "notifications",
    "shortcuts",
    "files",
    "usage",
    "updates",
    "connections",
    "devices",
    "backups",
    "diagnostics",
    "archived",
];

/// Where a contribution is placed. The spellings are the manifest's
/// contribution kinds (`luna-plugin.json`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UiPlacement {
    /// A command palette entry that runs one action.
    Command,
    /// A section in Settings' navigation, after the built-in sections.
    SettingsSection,
    /// A page in the sidebar's navigation, after the built-in pages.
    SidebarSection,
    /// A place on the right sidebar's rail, beside one task.
    Panel,
}

impl UiPlacement {
    pub const ALL: [UiPlacement; 4] = [
        Self::Command,
        Self::SettingsSection,
        Self::SidebarSection,
        Self::Panel,
    ];

    /// The manifest contribution kind this placement corresponds to.
    pub fn manifest_kind(self) -> &'static str {
        match self {
            Self::Command => "command",
            Self::SettingsSection => "settingsSection",
            Self::SidebarSection => "sidebarSection",
            Self::Panel => "panel",
        }
    }

    pub fn from_manifest_kind(kind: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|placement| placement.manifest_kind() == kind)
    }

    /// Placements that show a view (every one except a command).
    pub fn has_view(self) -> bool {
        !matches!(self, Self::Command)
    }
}

/// What a contribution acts on. A `task` contribution is shown only beside
/// (or with) one task, and every view request and action carries that exact
/// task id; the host never substitutes the task that is current later.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UiContext {
    #[default]
    None,
    Task,
}

// ---- views ----

/// A bounded, declarative view: a flat list of nodes the host lays out in
/// its own components. See [`UiView::validate`] for the bounds.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UiView {
    pub nodes: Vec<UiNode>,
}

/// One element of a view. Unknown kinds are refused, never ignored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UiNode {
    Heading(UiTextNode),
    Text(UiTextNode),
    /// A label and its value on one row.
    Field(UiFieldNode),
    /// A list row, optionally running an action.
    Item(UiItemNode),
    /// A push button running one of the contribution's actions.
    Button(UiButtonNode),
    Divider,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UiTextNode {
    pub text: String,
    #[serde(default, skip_serializing_if = "UiTone::is_default")]
    pub tone: UiTone,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UiFieldNode {
    pub label: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UiItemNode {
    /// Unique within the view.
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub badge: Option<String>,
    /// One of the contribution's action ids.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UiButtonNode {
    /// One of the contribution's action ids.
    pub action: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "UiButtonStyle::is_default")]
    pub style: UiButtonStyle,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UiTone {
    #[default]
    Default,
    Muted,
    Success,
    Warning,
    Danger,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UiButtonStyle {
    #[default]
    Secondary,
    Primary,
}

impl UiTone {
    pub fn is_default(&self) -> bool {
        *self == Self::Default
    }
}

impl UiButtonStyle {
    pub fn is_default(&self) -> bool {
        *self == Self::Secondary
    }
}

fn check_line(what: &str, value: &str, max: usize) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!("{what} is empty"));
    }
    if value.chars().count() > max {
        return Err(format!("{what} is longer than {max} characters"));
    }
    if value.chars().any(char::is_control) {
        return Err(format!("{what} contains a control character or line break"));
    }
    Ok(())
}

fn check_text(what: &str, value: &str, max: usize) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!("{what} is empty"));
    }
    if value.chars().count() > max {
        return Err(format!("{what} is longer than {max} characters"));
    }
    if value.chars().any(|c| c.is_control() && c != '\n') {
        return Err(format!("{what} contains a control character"));
    }
    Ok(())
}

/// An action, item or method id: 1 to [`UI_MAX_ID_BYTES`] bytes of ASCII
/// letters, digits, `.`, `_` and `-`.
pub fn check_ui_id(what: &str, value: &str) -> Result<(), String> {
    if value.is_empty() || value.len() > UI_MAX_ID_BYTES {
        return Err(format!("{what} must be 1 to {UI_MAX_ID_BYTES} bytes"));
    }
    if !value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return Err(format!(
            "{what} `{value}` may only use letters, digits, `.`, `_` and `-`"
        ));
    }
    Ok(())
}

/// A title the host would show beside its own controls: one line, bounded,
/// and not the name of a protected host control.
pub fn check_ui_title(what: &str, value: &str) -> Result<(), String> {
    check_line(what, value, UI_MAX_LABEL_CHARS)?;
    let folded = value.trim().to_lowercase();
    if UI_PROTECTED_TITLES.contains(&folded.as_str()) {
        return Err(format!(
            "{what} `{}` is the name of a protected LUNA control",
            value.trim()
        ));
    }
    Ok(())
}

/// A result message: bounded text, no control characters but line breaks.
pub fn check_ui_message(value: &str) -> Result<(), String> {
    check_text("the message", value, UI_MAX_MESSAGE_CHARS)
}

impl UiView {
    /// Every bound the host renders within: at most [`UI_MAX_NODES`] nodes
    /// and [`UI_MAX_VIEW_BYTES`] serialized; one-line, bounded labels; unique
    /// item ids; and buttons or items that name one of `actions`.
    pub fn validate(&self, actions: &[&str]) -> Result<(), String> {
        if self.nodes.len() > UI_MAX_NODES {
            return Err(format!(
                "the view has {} nodes; at most {UI_MAX_NODES} are shown",
                self.nodes.len()
            ));
        }
        let size = serde_json::to_vec(self).map_or(usize::MAX, |bytes| bytes.len());
        if size > UI_MAX_VIEW_BYTES {
            return Err(format!(
                "the view is {size} bytes; at most {UI_MAX_VIEW_BYTES} are shown"
            ));
        }
        let action = |what: &str, id: &str| -> Result<(), String> {
            if actions.contains(&id) {
                Ok(())
            } else {
                Err(format!("{what} names the undeclared action `{id}`"))
            }
        };
        let mut items = std::collections::HashSet::new();
        for (index, node) in self.nodes.iter().enumerate() {
            let at = |what: &str| format!("node {index}: {what}");
            match node {
                UiNode::Heading(text) => {
                    check_line(&at("the heading"), &text.text, UI_MAX_LABEL_CHARS)?
                }
                UiNode::Text(text) => check_text(&at("the text"), &text.text, UI_MAX_TEXT_CHARS)?,
                UiNode::Field(field) => {
                    check_line(&at("the field label"), &field.label, UI_MAX_LABEL_CHARS)?;
                    check_line(&at("the field value"), &field.value, UI_MAX_VALUE_CHARS)?;
                }
                UiNode::Item(item) => {
                    check_ui_id(&at("the item id"), &item.id)?;
                    if !items.insert(item.id.as_str()) {
                        return Err(at(&format!("the item id `{}` is repeated", item.id)));
                    }
                    check_line(&at("the item title"), &item.title, UI_MAX_LABEL_CHARS)?;
                    if let Some(detail) = &item.detail {
                        check_line(&at("the item detail"), detail, UI_MAX_VALUE_CHARS)?;
                    }
                    if let Some(badge) = &item.badge {
                        check_line(&at("the item badge"), badge, UI_MAX_LABEL_CHARS)?;
                    }
                    if let Some(id) = &item.action {
                        action(&at("the item"), id)?;
                    }
                }
                UiNode::Button(button) => {
                    check_ui_title(&at("the button label"), &button.label)?;
                    action(&at("the button"), &button.action)?;
                }
                UiNode::Divider => {}
            }
        }
        Ok(())
    }

    /// The action ids this view's buttons and items name, in order.
    pub fn named_actions(&self) -> Vec<&str> {
        self.nodes
            .iter()
            .filter_map(|node| match node {
                UiNode::Button(button) => Some(button.action.as_str()),
                UiNode::Item(item) => item.action.as_deref(),
                _ => None,
            })
            .collect()
    }
}

// ---- the declaration (`luna-ui.json`) ----

/// `luna-ui.json`, version 1. Unknown fields are refused everywhere.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UiDeclaration {
    pub declaration_version: u32,
    pub contributions: Vec<UiContributionDeclaration>,
}

/// One contribution. Its id must be one of the manifest's contributions of
/// the matching kind. A `command` has exactly one action (run when chosen)
/// and no view; every other placement has a view and up to
/// [`UI_MAX_ACTIONS`] actions its buttons and items name. A `panel` is
/// always `task` context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UiContributionDeclaration {
    pub id: ContributionId,
    pub placement: UiPlacement,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// Order among this placement's extension entries (0 to 1000, default
    /// 500); always after the host's own entries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<i32>,
    #[serde(default)]
    pub context: UiContext,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view: Option<UiViewSource>,
    #[serde(default)]
    pub actions: Vec<UiActionDeclaration>,
}

/// Where a view comes from: the declaration itself, or the package's worker
/// (`method` is called off the render thread, bounded by
/// [`UI_VIEW_TIMEOUT_MS`], and must return a [`UiView`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UiViewSource {
    Static(UiView),
    Worker(UiWorkerCall),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UiWorkerCall {
    /// A worker operation, required by name at the worker's start. The
    /// `luna.` and `provider.` prefixes are reserved.
    pub method: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UiActionDeclaration {
    pub id: String,
    pub title: String,
    pub run: UiActionRun,
}

/// What an action does: one of the host's own actions, or a worker call
/// (bounded by [`UI_ACTION_TIMEOUT_MS`]; its result is a
/// [`UiWorkerActionResult`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UiActionRun {
    Host(UiHostAction),
    Worker(UiWorkerCall),
}

/// The closed set of host actions a declaration may name. The host performs
/// them with its own navigation; none can reach protected controls' state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UiHostAction {
    /// Open the new-task canvas (the composer stays the user's).
    NewTask,
    /// Open a built-in settings section ([`UI_SETTINGS_SECTIONS`]).
    OpenSettings(UiOpenSettings),
    /// Open another view contribution of the same package.
    OpenContribution(UiOpenContribution),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UiOpenSettings {
    pub section: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UiOpenContribution {
    pub contribution_id: ContributionId,
}

/// What a worker action returns: an optional message for the user and an
/// optional replacement view for the contribution it ran from (bounded by
/// the same rules as every view).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UiWorkerActionResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view: Option<UiView>,
}

// ---- what the engine publishes ----

/// `ListUiContributions`, and each `WatchUiContributions` item: the UI
/// contributions of this profile's enabled packages. In safe mode the list
/// is empty and every declared contribution is refused with the reason.
/// `contributions` is ordered by placement ([`UiPlacement`] order), then
/// `order`, then id; `refused` by contribution id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiContributionsSnapshot {
    /// Changes whenever the list, a refusal or safe mode changes.
    pub revision: String,
    pub safe_mode: bool,
    pub contributions: Vec<UiContributionEntry>,
    pub refused: Vec<UiContributionRefusal>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiContributionEntry {
    pub id: ContributionId,
    pub placement: UiPlacement,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    pub order: i32,
    pub context: UiContext,
    pub extension_id: ExtensionId,
    /// The package's `displayName`, shown as the contribution's attribution.
    pub extension_name: String,
    pub version: String,
    /// The exact package; every view request and action names it, and is
    /// refused as stale once another package (or none) provides the id.
    pub package_hash: String,
    pub trust: ExtensionTrust,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view: Option<UiViewKind>,
    #[serde(default)]
    pub actions: Vec<UiActionEntry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UiViewKind {
    Static,
    Worker,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiActionEntry {
    pub id: String,
    pub title: String,
    pub runs: UiActionKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UiActionKind {
    Host,
    Worker,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiContributionRefusal {
    /// As declared (it may not be a valid id).
    pub contribution_id: String,
    pub extension_id: ExtensionId,
    pub version: String,
    pub package_hash: String,
    pub reason: String,
}

/// `GetUiContributionView`. Unknown fields are refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GetUiContributionViewRequest {
    pub contribution_id: ContributionId,
    /// The package the caller rendered the contribution from.
    pub package_hash: String,
    /// Required for a `task` contribution; refused for any other.
    #[serde(default)]
    pub task_id: Option<String>,
}

/// `InvokeUiAction`. Unknown fields are refused, so a forged
/// `confirmed: true` or principal is `bad params`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InvokeUiActionRequest {
    pub contribution_id: ContributionId,
    pub package_hash: String,
    pub action_id: String,
    /// The task the action was shown for (required for `task` context).
    #[serde(default)]
    pub task_id: Option<String>,
    /// The caller's id for this request; passed to a worker call so an
    /// unknown outcome is reconciled, never replayed (CORE-19).
    #[serde(default)]
    pub request_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UiOutcomeStatus {
    Ok,
    /// The contribution is no longer provided by `packageHash` (disabled,
    /// updated, rolled back or removed): nothing ran; refresh and retry.
    Stale,
    /// Not allowed as asked (safe mode, an unknown action, a missing or
    /// unknown task): nothing ran.
    Refused,
    /// The worker failed, timed out, exited or returned something invalid.
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiViewOutcome {
    pub contribution_id: ContributionId,
    pub package_hash: String,
    /// Exactly the request's task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    pub status: UiOutcomeStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view: Option<UiView>,
    /// Why, for every status but `ok`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiActionOutcome {
    pub contribution_id: ContributionId,
    pub package_hash: String,
    pub action_id: String,
    /// Exactly the request's task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    pub status: UiOutcomeStatus,
    /// For a host action: what the host should now do.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<UiHostAction>,
    /// For a worker action: the contribution's replacement view.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view: Option<UiView>,
    /// The worker's message, or why for every status but `ok`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}
