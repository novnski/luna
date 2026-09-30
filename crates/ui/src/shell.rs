//! The app shell (luna `__root.tsx`): sidebar column + main panel + optional
//! right "Changes" pane, plus the boot splash and the connection gate.
//!
//! Layout is luna's: collapsible drag-resizable sidebar (224–400px, default
//! 256) with a 200ms ease-out width transition; main panel with an h-11 header,
//! content outlet, and a reserved h-6 status strip so later content never
//! shifts; right pane scaffold (360px floor, default 520), hidden by default.
//! Widths/collapsed state persist to `ui-settings.json` (debounced).
//!
//! Resize handles use gpui's drag-and-drop pattern (an `on_drag` with an empty
//! ghost view + `on_drag_move::<Marker>` on the root), the same idiom as Zed's
//! dock. Double-clicking a handle resets that pane to its default width.

use std::path::PathBuf;
use std::time::Duration;

use chrono::Utc;
use gpui::{
    Action, AnyElement, App, ClipboardItem, Context, Empty, Entity, FocusHandle, Focusable as _,
    IntoElement, KeyBinding, Keystroke, ModifiersChangedEvent, MouseButton, MouseDownEvent,
    MouseUpEvent, Pixels, Point, Render, SharedString, Subscription, Task, Window,
    WindowControlArea, actions, div, prelude::*, px,
};

use gpui_tokio::Tokio;
use luna_engine::InstanceLock;
use luna_proto::{
    ActivityEnvelope, ActivityOutcome, AuthState, ChatConfig, ConversationForkBoundaryKind,
    HarnessId, HistoryRecoveryView, HistorySearchFilters, HistorySearchPage, HistorySearchRequest,
    HistorySearchScope, PermissionMode, ProviderUpdateStatus, TaskCommandSnapshot, UsageProvider,
    UsageSnapshot, WorkspaceScope,
};
use luna_rpc::methods;

use crate::changes::{Changes, ChangesEvent};
use crate::composer::{Composer, ComposerEvent, ComposerInput, ComposerInputEvent};
use crate::files::{FilesCloseDisposition, FilesEvent, FilesSurface, WorkspacePathDrag};
use crate::icons::{self, icon};
use crate::loaders;
use crate::motion::{self, AnimationExt as _, MotionSpec, RESIZE, SPLASH_OUT, TAB_SLIDE};
use crate::popover::{self, Loadable};
use crate::rail;
use crate::settings::accounts::AccountsPage;
use crate::settings::appearance::{AppearancePage, AppearanceSettingsEvent};
use crate::settings::archived::ArchivedPage;
use crate::settings::backup::BackupsPage;
use crate::settings::devices::DevicesPage;
use crate::settings::files::{FilesSettingsEvent, FilesSettingsPage};
use crate::settings::general::{GeneralEvent, GeneralPage};
use crate::settings::notifications::{
    NotificationPreferences, NotificationTask, NotificationsEvent, NotificationsPage,
};
use crate::settings::project_defaults::ProjectsPage;
use crate::settings::shortcuts::{ShortcutsEvent, ShortcutsPage};
use crate::settings::widgets;
use crate::settings::{
    self, CHAT_PANEL_MIN, ComposerSendBehavior, JUMP_SLOTS, KeymapConfig, RIGHT_PANE_DEFAULT,
    RIGHT_PANE_MIN, SIDEBAR_DEFAULT, SIDEBAR_MAX, SIDEBAR_MIN, SavePolicy, ShortcutId,
    SidebarOrganization, SidebarSort, TERMINAL_DEFAULT_HEIGHT, TERMINAL_MAX_VH,
    TERMINAL_MIN_HEIGHT, TaskConfirmation, UiSettings, badge_combo, jump_hints_visible,
    platform_combo,
};
use crate::state::{
    AppState, ConnectionStatus, EngineBootConfig, EngineMode, GatePhase, Indicator, OrgRow,
    format_time_ago, org_name_valid, parse_orgs, sort_memberships,
};
use crate::terminal::panel::{
    TerminalPanel, TerminalPanelEvent, ToggleTerminal, clamp_terminal_height,
};
use crate::theme::Theme;
use crate::transcript::{self, Transcript, TranscriptEvent};
use crate::workspace_links::resolve_workspace_file_link;

mod activity_center;
mod agents;
mod command_palette;
mod side_chats;
mod spaces;
mod tabs;
mod usage;

use spaces::{AddSpaceFlow, RenameSpaceDialog};

actions!(
    shell,
    [
        SaveFile,
        ToggleSidebar,
        ToggleChanges,
        AddSpacePalette,
        ToggleCommandPalette,
        ToggleActivity,
        OpenModelPicker,
        NewSession,
        OpenSettings,
        NextSession,
        PrevSession,
        ArchiveSession
    ]
);

/// Restore a default focus only after an in-flight handoff has had a frame to
/// claim the window. A synchronous focus-lost fallback can otherwise steal
/// focus from controls that are mounting in response to the same input event.
pub(crate) fn restore_focus_if_empty_on_next_frame<T: 'static>(
    focus: FocusHandle,
    window: &mut Window,
    cx: &mut Context<T>,
) {
    window.on_next_frame(move |window, cx| {
        if window.focused(cx).is_none() {
            window.focus(&focus, cx);
        }
    });
    cx.notify();
}

/// Check the completed dispatch tree, not just the lifetime of the focused
/// handle: a hidden editor can stay alive after its element has unmounted.
fn restore_mounted_focus(
    root: &FocusHandle,
    preferred: &FocusHandle,
    unfocused: &FocusHandle,
    window: &mut Window,
    cx: &mut App,
) {
    let preferred_mounted = root.contains(preferred, window);
    if !root.contains_focused(window, cx) || (root.is_focused(window) && preferred_mounted) {
        // Explicit blur keeps shortcuts active without returning the caret to
        // an input. The root remains the temporary fallback for stale handles.
        let target = if window.focused(cx).is_none() {
            unfocused
        } else if preferred_mounted {
            preferred
        } else {
            root
        };
        window.focus(target, cx);
    }
}

#[derive(Clone, Copy)]
enum ChatMenuPage {
    Root,
    Copy,
}

#[derive(Clone)]
struct ChatMenuState {
    chat_id: String,
    position: Point<Pixels>,
    page: ChatMenuPage,
}

/// Interruptible height tween for the sidebar's device/archive disclosures.
/// The rendered element owns the frame clock; this state preserves the current
/// interpolated height when a second click reverses an in-flight transition.
#[derive(Clone, Copy)]
pub(super) struct SidebarDisclosureMotion {
    pub(super) epoch: u64,
    pub(super) from: f32,
    pub(super) to: f32,
    started: std::time::Instant,
}

impl SidebarDisclosureMotion {
    fn new(epoch: u64, from: f32, to: f32) -> Self {
        Self {
            epoch,
            from,
            to,
            started: std::time::Instant::now(),
        }
    }

    fn current(self) -> f32 {
        let total = motion::COLLAPSE.total().as_secs_f32();
        let raw = if total > 0.0 {
            self.started.elapsed().as_secs_f32() / total
        } else {
            1.0
        };
        motion::lerp(self.from, self.to, motion::COLLAPSE.progress(raw))
    }

    fn animating(self) -> bool {
        self.started.elapsed() < motion::COLLAPSE.total() + spaces::SIDEBAR_DISCLOSURE_TWEEN_GRACE
    }
}

/// Vertical pane resize hitboxes yield the global titlebar. Keeping this in
/// the shared constructor makes left/right seams mirror each other and avoids
/// relying on paint order when chrome crosses an animated pane boundary.
const PANE_RESIZE_HITBOX_HALF_WIDTH: f32 = 10.0;
const PANE_RESIZE_HITBOX_TOP: f32 = Theme::TITLEBAR_HEIGHT;
/// Corner radius of the floating Linux CSD window (macOS gets its native
/// curve from the platform; maximized/tiled Linux windows go square).
/// Chrome layers that paint full-bleed at a window edge round their own
/// backgrounds with it — see [`Shell::window_corner_radius`].
pub(crate) const LINUX_WINDOW_CORNER_RADIUS: f32 = 10.0;
const TERMINAL_RESIZE_HITBOX_HEIGHT: f32 = 10.0;

fn stable_panel_content_width(target: f32, transition: Option<(f32, f32)>) -> f32 {
    transition.map(|(from, to)| from.max(to)).unwrap_or(target)
}

fn right_panel_content_width(
    target: f32,
    transition: Option<(f32, f32)>,
    takeover_width: Option<f32>,
) -> f32 {
    takeover_width.unwrap_or_else(|| stable_panel_content_width(target, transition))
}

fn conversation_width(viewport: f32, sidebar: f32, right: f32) -> f32 {
    (viewport - sidebar - right).max(0.0)
}

fn titlebar_new_session_alpha(is_chat_route: bool, has_selected_chat: bool) -> f32 {
    if is_chat_route && has_selected_chat {
        1.0
    } else {
        0.0
    }
}

/// Open the session at `slot` (zero-based) of the sidebar's active list. One
/// action carrying the slot, rather than nine near-identical action types.
#[derive(Clone, PartialEq, Action)]
#[action(namespace = shell, no_json)]
pub struct JumpSession(pub usize);

// ---------------------------------------------------------------------------
// Traffic-light-aware titlebar layout (feature-inventory §1.1)
// ---------------------------------------------------------------------------

/// Where the top-left window-control cluster starts, in px from the window's
/// left edge (luna window-controls.tsx: `left: fullscreen ? 12 : 88`). The
/// frameless hiddenInset chrome puts the macOS traffic lights at {14,15};
/// fullscreen hides them and the cluster reclaims the inset.
pub fn titlebar_cluster_start(fullscreen: bool) -> f32 {
    if fullscreen { 12.0 } else { 88.0 }
}

/// Width of the spacer ahead of the control cluster for a strip that already
/// carries `container_pad` px of its own left padding. macOS only — on
/// Linux/Windows there are no traffic lights and the cluster hugs the edge.
pub fn titlebar_spacer_width(is_macos: bool, fullscreen: bool, container_pad: f32) -> f32 {
    if !is_macos {
        return 0.0;
    }
    (titlebar_cluster_start(fullscreen) - container_pad).max(0.0)
}

/// Within-group rhythm for Back/Forward.
pub const TITLEBAR_CONTROL_GAP: f32 = 2.0;
/// Structural separation between titlebar groups: sidebar, navigation,
/// transcript identity, and trailing actions.
pub const TITLEBAR_GROUP_GAP: f32 = Theme::SPACE_SM;
/// Breathing room between the navigation cluster and transcript identity.
pub const TITLEBAR_IDENTITY_GAP: f32 = Theme::SPACE_MD;
/// A 28px action centered in the 38px titlebar with its 2px downward optical
/// shift lands 6px from the top; use the same inset at the trailing edge.
pub const TITLEBAR_ACTION_EDGE_INSET: f32 = 6.0;
/// Width of the persistent top-left button cluster itself: a 24px sidebar
/// trigger, an 8px group gap, then two 24px history buttons on a 2px rhythm.
pub const CLUSTER_BUTTONS_WIDTH: f32 = 24.0 * 3.0 + TITLEBAR_GROUP_GAP + TITLEBAR_CONTROL_GAP;
/// Extra width consumed when the collapsed-sidebar New Session action joins
/// the left controls as its own group.
pub const TITLEBAR_ACTION_SLOT_WIDTH: f32 = TITLEBAR_GROUP_GAP + 24.0;
/// Horizontal inset owned by the titlebar control row itself. Keep this value
/// paired with [`Self::titlebar_spacer`]: using a different number for the
/// spacer shifts every control while leaving the declared cluster geometry
/// unchanged.
const TITLEBAR_CLUSTER_PAD: f32 = 10.0;

/// Width of a row of `count` Linux caption buttons, drawn at the cluster's
/// own 24px-button / 2px-gap rhythm.
pub fn caption_buttons_width(count: usize) -> f32 {
    if count == 0 {
        return 0.0;
    }
    count as f32 * 24.0 + (count as f32 - 1.0) * 2.0
}

/// Where the cluster's first button starts, from the window's left edge.
/// `linux_left_captions` is the number of caption buttons luna draws at the
/// top-left on Linux (GNOME `close:…` layouts) — the app cluster follows them
/// at the shared 2px rhythm.
pub fn cluster_buttons_start(is_macos: bool, fullscreen: bool, linux_left_captions: usize) -> f32 {
    if is_macos {
        titlebar_cluster_start(fullscreen)
    } else if linux_left_captions > 0 {
        10.0 + caption_buttons_width(linux_left_captions) + 2.0
    } else {
        10.0
    }
}

/// Left clearance a full-bleed header (collapsed sidebar) needs so its content
/// starts past the overlay cluster, given the header's own `container_pad`.
pub fn cluster_clearance(
    is_macos: bool,
    fullscreen: bool,
    linux_left_captions: usize,
    container_pad: f32,
) -> f32 {
    (cluster_buttons_start(is_macos, fullscreen, linux_left_captions) + CLUSTER_BUTTONS_WIDTH + 8.0
        - container_pad)
        .max(0.0)
}

/// (Re-)apply the whole app keymap: clears every binding, restores the composer
/// map, then binds the customizable shortcuts from `keymap` (feature-inventory
/// §1.4). Invalid persisted combos fall back to that shortcut's default.
pub fn apply_keymap(
    cx: &mut App,
    keymap: &KeymapConfig,
    composer_send_behavior: ComposerSendBehavior,
) {
    fn valid_or_default(combo: &str, fallback: &str) -> String {
        let candidate = platform_combo(combo);
        if Keystroke::parse(&candidate).is_ok() {
            candidate
        } else {
            tracing::warn!(%combo, "unparseable shortcut combo; using default");
            platform_combo(fallback)
        }
    }
    crate::appshots::set_shortcut(&keymap.capture_appshot);
    cx.clear_key_bindings();
    // `clear_key_bindings` also removes the contextual editing actions that
    // gpui-base installed at startup. Reinitialize the component layer before
    // rebuilding Luna's bindings so the file editor keymap remains active.
    gpui_base::init(cx);
    crate::composer::init(cx, composer_send_behavior);
    // Fixed app-level shortcuts (Settings on every platform; ⌘Q quit, ⌘W
    // close, ⌘M minimize, ⌘H hide on macOS) — these back the native menu
    // key equivalents and must survive keymap re-application.
    crate::app_menus::bind_keys(cx);
    cx.bind_keys([
        KeyBinding::new(
            &valid_or_default(&keymap.save_file, "mod-s"),
            SaveFile,
            None,
        ),
        KeyBinding::new(
            &valid_or_default(&keymap.toggle_sidebar, "mod-b"),
            ToggleSidebar,
            None,
        ),
        KeyBinding::new(
            &valid_or_default(&keymap.toggle_changes, "mod-r"),
            ToggleChanges,
            None,
        ),
        KeyBinding::new(
            &valid_or_default(&keymap.toggle_terminal, "mod-j"),
            ToggleTerminal,
            None,
        ),
        KeyBinding::new(
            &valid_or_default(&keymap.new_session, "mod-n"),
            NewSession,
            None,
        ),
        KeyBinding::new(
            &valid_or_default(
                &keymap.next_session,
                crate::settings::ShortcutId::NextSession.default_combo(),
            ),
            NextSession,
            None,
        ),
        KeyBinding::new(
            &valid_or_default(
                &keymap.prev_session,
                crate::settings::ShortcutId::PrevSession.default_combo(),
            ),
            PrevSession,
            None,
        ),
        KeyBinding::new(
            &valid_or_default(&keymap.archive_session, "mod-shift-a"),
            ArchiveSession,
            None,
        ),
        KeyBinding::new(
            &valid_or_default(&keymap.new_project, ShortcutId::NewProject.default_combo()),
            AddSpacePalette,
            None,
        ),
        // Fixed: ⌘K summons the command palette.
        // Pressing it again dismisses.
        KeyBinding::new(&platform_combo("mod-k"), ToggleCommandPalette, None),
        KeyBinding::new(
            &valid_or_default(&keymap.open_model_picker, "mod-/"),
            OpenModelPicker,
            None,
        ),
    ]);
    if !keymap.open_activity.is_empty() {
        let combo = platform_combo(&keymap.open_activity);
        if !crate::settings::activity_combo_is_reserved(&keymap.open_activity)
            && Keystroke::parse(&combo).is_ok()
        {
            cx.bind_keys([KeyBinding::new(&combo, ToggleActivity, None)]);
        } else {
            tracing::warn!(combo = %keymap.open_activity, "unparseable Activity shortcut; leaving it unbound");
        }
    }
    crate::browser::bind_keys(cx, keymap);
    // ⌘1..⌘9 open the sidebar's first nine rows. A slot left unbound (an empty
    // combo in a hand-edited file) binds nothing rather than falling back —
    // the user cleared it on purpose.
    cx.bind_keys((0..JUMP_SLOTS).filter_map(|slot| {
        let id = ShortcutId::JumpSession(slot);
        let combo = keymap.get(id);
        if combo.is_empty() {
            return None;
        }
        Some(KeyBinding::new(
            &valid_or_default(combo, id.default_combo()),
            JumpSession(slot),
            None,
        ))
    }));
}

/// The settings sections (feature-inventory §1.5 routes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsSection {
    General,
    Projects,
    Devices,
    /// Per-device providers, CLI enablement, accounts, and usage.
    Providers,
    Appearance,
    Files,
    Backups,
    Notifications,
    Shortcuts,
    Appshots,
    Archived,
    Usage,
    Updates,
}

impl SettingsSection {
    pub const ALL: [SettingsSection; 13] = [
        SettingsSection::General,
        SettingsSection::Projects,
        SettingsSection::Providers,
        SettingsSection::Appearance,
        SettingsSection::Notifications,
        SettingsSection::Shortcuts,
        SettingsSection::Files,
        SettingsSection::Appshots,
        SettingsSection::Usage,
        SettingsSection::Updates,
        SettingsSection::Devices,
        SettingsSection::Backups,
        SettingsSection::Archived,
    ];

    pub const DEFAULT: Self = Self::General;

    /// Fixture-only boot selector, not an OS or URL deep-link route.
    fn from_fixture_route(route: &str) -> Option<Self> {
        Some(match route {
            "settings" | "settings/general" => Self::General,
            "settings/projects" => Self::Projects,
            "settings/devices" => Self::Devices,
            "settings/harnesses" | "settings/agents" | "settings/providers" => Self::Providers,
            "settings/appearance" => Self::Appearance,
            "settings/notifications" => Self::Notifications,
            "settings/shortcuts" => Self::Shortcuts,
            "settings/appshots" => Self::Appshots,
            "settings/archived" => Self::Archived,
            "settings/usage" => Self::Usage,
            "settings/updates" => Self::Updates,
            _ => return None,
        })
    }

    /// Sidebar + header label (luna settings-sidebar.tsx SECTIONS / __root.tsx
    /// `settingsTitle` — the same strings in both places).
    pub fn label(self) -> &'static str {
        match self {
            SettingsSection::General => "General",
            SettingsSection::Projects => "Projects",
            SettingsSection::Devices => "Devices",
            SettingsSection::Providers => "Providers",
            SettingsSection::Appearance => "Appearance",
            SettingsSection::Files => "Files",
            SettingsSection::Backups => "Backups",
            SettingsSection::Notifications => "Notifications",
            SettingsSection::Shortcuts => "Shortcuts",
            SettingsSection::Appshots => "Appshots",
            SettingsSection::Archived => "Archived sessions",
            SettingsSection::Usage => "Usage",
            SettingsSection::Updates => "Updates",
        }
    }
}

/// What the main outlet shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    Chat,
    Settings(SettingsSection),
}

fn is_settings_nav_activation(key: &str, is_held: bool) -> bool {
    !is_held && matches!(key, "enter" | "space")
}

/// Maximum width the right pane may occupy while retaining the conversation
/// floor. On unusually small windows this deliberately falls below the right
/// pane's preferred minimum: the chat remains usable and the side surface
/// yields the scarce space.
fn right_pane_max_width(viewport: f32, sidebar: f32) -> f32 {
    (viewport - sidebar - CHAT_PANEL_MIN).max(0.0)
}

/// Width used by right-pane takeover. Unlike manual resizing, takeover is
/// intentionally allowed to consume the conversation column completely.
fn right_pane_takeover_width(viewport: f32, sidebar: f32) -> f32 {
    (viewport - sidebar).max(0.0)
}

/// One right-pane surface tab: a workspace browser, an individual workspace
/// file editor, a Git diff or history page, an embedded terminal, or a
/// subagent transcript. `Picker` is the empty surface chooser.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RightSurface {
    #[default]
    Picker,
    Files,
    File(u64),
    Browser(u64),
    Diff(u64),
    Terminal(u64),
    /// A subagent's transcript, read-only (per-subagent viz) — the handle
    /// keys [`Shell::subagent_tabs`].
    Subagent(u64),
    Agents,
    SideChats,
}

fn push_unique_right_surface(tabs: &mut Vec<RightSurface>, surface: RightSurface) -> bool {
    if tabs.contains(&surface) {
        false
    } else {
        tabs.push(surface);
        true
    }
}

fn workspace_file_title(path: &str) -> SharedString {
    path.rsplit('/').next().unwrap_or(path).to_string().into()
}

/// Per-chat panel open flags (luna parity: `sessionPanels` — the terminal and
/// changes panels open *per session*, in memory only; heights and every other
/// persisted setting stay global).
///
/// Everything defaults CLOSED — the right pane included (user request,
/// revising the earlier default-open: it popped open on every session you
/// visited). Opening is an explicit act, remembered per chat for the rest of
/// the app run; a fresh open with no surface tabs lands on the picker.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ChatPanels {
    pub terminal_open: bool,
    /// Right pane visible (the surface host — historically the Changes pane).
    pub changes_open: bool,
    /// Which surface tab renders; validated against the live tab list each
    /// frame (a closed tab falls back gracefully).
    pub right_active: RightSurface,
}

/// The session-scoped panel map. Keys are chat ids; the new-chat canvas uses
/// the empty key. Not persisted — a fresh app starts with everything closed.
#[derive(Debug, Default)]
pub struct SessionPanels {
    map: std::collections::HashMap<String, ChatPanels>,
}

impl SessionPanels {
    pub fn get(&self, key: &str) -> ChatPanels {
        self.map.get(key).copied().unwrap_or_default()
    }

    /// Flip the terminal flag for `key`; returns the new value.
    pub fn toggle_terminal(&mut self, key: &str) -> bool {
        let entry = self.map.entry(key.to_string()).or_default();
        entry.terminal_open = !entry.terminal_open;
        entry.terminal_open
    }

    /// Flip the changes flag for `key`; returns the new value.
    pub fn toggle_changes(&mut self, key: &str) -> bool {
        let entry = self.map.entry(key.to_string()).or_default();
        entry.changes_open = !entry.changes_open;
        entry.changes_open
    }

    /// Mutate `key`'s flags in place (right-pane surface bookkeeping).
    pub fn update(&mut self, key: &str, f: impl FnOnce(&mut ChatPanels)) {
        f(self.map.entry(key.to_string()).or_default());
    }
}

/// One route-history entry (luna parity: the renderer's TanStack memory
/// history — every route the user visited, browser-style).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NavEntry {
    /// A chat route; the id of the selected chat ("" = the new-chat canvas).
    Chat(String),
    Settings(SettingsSection),
}

/// Browser-style navigation history for the titlebar back/forward buttons
/// (luna window-controls.tsx semantics): every route change pushes an entry;
/// Back/Forward walk the stack without changing it; pushing while behind the
/// tip truncates the entries ahead (a new branch, exactly like a browser).
#[derive(Debug)]
pub struct NavHistory {
    entries: Vec<NavEntry>,
    index: usize,
}

impl NavHistory {
    pub fn new(initial: NavEntry) -> Self {
        Self {
            entries: vec![initial],
            index: 0,
        }
    }

    pub fn current(&self) -> &NavEntry {
        &self.entries[self.index]
    }

    /// Record a route change. Re-navigating to the current route is a no-op
    /// (selecting the already-selected chat never happened as a navigation);
    /// otherwise any forward branch is truncated and the entry appended.
    pub fn push(&mut self, entry: NavEntry) {
        if *self.current() == entry {
            return;
        }
        self.entries.truncate(self.index + 1);
        self.entries.push(entry);
        self.index += 1;
    }

    /// Swap the current entry in place without growing the stack — the native
    /// equivalent of a `replace: true` navigation (luna's boot redirect from
    /// `/` into the last-used chat leaves no dead Back target behind).
    pub fn replace(&mut self, entry: NavEntry) {
        self.entries[self.index] = entry;
    }

    pub fn can_back(&self) -> bool {
        self.index > 0
    }

    /// Memory history keeps every entry, so "behind the last entry" is exactly
    /// "can go forward" (luna window-controls.tsx).
    pub fn can_forward(&self) -> bool {
        self.index + 1 < self.entries.len()
    }

    pub fn back(&mut self) -> Option<NavEntry> {
        if !self.can_back() {
            return None;
        }
        self.index -= 1;
        Some(self.current().clone())
    }

    pub fn forward(&mut self) -> Option<NavEntry> {
        if !self.can_forward() {
            return None;
        }
        self.index += 1;
        Some(self.current().clone())
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

/// Sidebar resort glide (feature-inventory §1.6): 260ms
/// `cubic-bezier(0.22,1,0.36,1)` per-row translate, the View Transitions
/// equivalent.
pub const RESORT: MotionSpec = MotionSpec::new(260, motion::EASE_RESORT);

/// FLIP diff for a keyed list: given the previously rendered order and the new
/// order (key + row height), return each surviving key's paint-only start
/// offset `old_y - new_y` (only keys whose position actually moved). `gap` is
/// the flex gap between rows. Pure — drives the sidebar resort glide.
pub fn resort_offsets(
    old: &[(String, f32)],
    new: &[(String, f32)],
    gap: f32,
) -> std::collections::HashMap<String, f32> {
    let mut old_y = std::collections::HashMap::new();
    let mut y = 0.0_f32;
    for (key, height) in old {
        old_y.insert(key.as_str(), y);
        y += height + gap;
    }
    let mut offsets = std::collections::HashMap::new();
    let mut y = 0.0_f32;
    for (key, height) in new {
        if let Some(prev) = old_y.get(key.as_str()) {
            let dy = prev - y;
            if dy.abs() > 0.5 {
                offsets.insert(key.clone(), dy);
            }
        }
        y += height + gap;
    }
    offsets
}

/// Height changes do not constitute a list reorder. In particular, sidebar
/// disclosures animate their own height and must not also trigger FLIP offsets
/// on every following keyed section.
fn sidebar_key_order_changed(old: &[(String, f32)], new: &[(String, f32)]) -> bool {
    old.len() != new.len()
        || old
            .iter()
            .zip(new)
            .any(|((old_key, _), (new_key, _))| old_key != new_key)
}

/// All active task rows reserve the same height, including tasks without a
/// branch or pull request. The tallest metadata line fits within 63 pixels.
/// Keep this value shared by the renderer and sidebar reorder geometry.
pub(super) fn chat_row_height(_shows_branch: bool, _shows_pull_request: bool) -> f32 {
    63.0
}
/// Keep adjacent task hitboxes contiguous so vertical pointer movement never
/// crosses an arrow-cursor strip between clickable rows.
const SIDEBAR_LIST_GAP: f32 = 0.0;
/// Harness/title geometry follows the row hierarchy: active multi-line cards
/// keep identity close on the standard 8px rhythm, while the one-line archived
/// shelf gives its larger mark a little more separation.
const SIDEBAR_ACTIVE_HARNESS_ICON_SIZE: f32 = 13.0;
const SIDEBAR_ACTIVE_HARNESS_TITLE_GAP: f32 = Theme::SPACE_SM;
const SIDEBAR_ARCHIVED_HARNESS_ICON_SIZE: f32 = 14.0;
const SIDEBAR_ARCHIVED_HARNESS_TITLE_GAP: f32 = 10.0;

/// Keep the fade short so only the last few glyphs recede. Tracking clipped
/// content lets the shared paint-time overflow gate leave fitting labels intact.
fn sidebar_faded_label(id: SharedString, fill: bool, label: impl IntoElement) -> impl IntoElement {
    let overflow = gpui::ScrollHandle::new();
    crate::edge_fade::edge_faded(
        20.0,
        false,
        false,
        div()
            .id(id)
            .when(fill, |el| el.flex_1())
            .min_w_0()
            .overflow_hidden()
            .track_scroll(&overflow)
            .flex()
            .child(div().flex_none().whitespace_nowrap().child(label)),
    )
    .fade_right(true)
    .fade_label_overflow(&overflow)
}

/// Ramp height of the sidebar's scroll-edge fade (the gpui
/// [`gpui::EdgeFade`] scope — per-primitive, so text fades per glyph).
const SIDEBAR_GLASS_FADE_BAND: f32 = 24.0;

/// New-thread controls float over the tail of a top-anchored image hero. The
/// hero reaches below the composer, giving its lower mask room to dissolve
/// gradually into the otherwise empty lower canvas.
const NEW_THREAD_BACKGROUND_FROSTED_OPACITY: f32 = 0.84;
const NEW_THREAD_BACKGROUND_VIEWPORT_RATIO: f32 = 0.72;
const NEW_THREAD_BACKGROUND_MAX_HEIGHT: f32 = 760.0;

/// Drag marker for the sidebar resize handle.
struct SidebarResize;
/// Drag marker for the right-pane resize handle.
struct RightPaneResize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PaneResizeKind {
    Sidebar,
    Right,
    Terminal,
}

/// Resolve one pointer sample while keeping the persisted width legal. The
/// edge is latched by the caller, so a held pointer produces one nudge rather
/// than restarting the animation for every drag event.
fn sidebar_drag_sample(
    pointer_x: f32,
    latched_edge: Option<motion::ResizeEdge>,
    reduced_motion: bool,
) -> motion::ResizeDragSample {
    motion::resize_drag_sample(
        pointer_x,
        SIDEBAR_MIN,
        SIDEBAR_MAX,
        latched_edge,
        reduced_motion,
    )
}

/// The dragged surface-tab payload (strip reorder).
struct RightTabDrag {
    panel_key: String,
    from: usize,
    title: SharedString,
    workspace_path: Option<WorkspacePathDrag>,
}

/// Live drag-over state for the surface-tab strip — the terminal drawer's
/// [`crate::terminal::panel`] DragState, ported: `epoch` keys the 150ms
/// slide-animation restarts as the hovered slot changes.
struct RightTabDragState {
    from: usize,
    over: usize,
    epoch: usize,
    prev_over: usize,
}

/// Ghost chip following the pointer while a surface tab drags.
struct SurfaceTabGhost {
    title: SharedString,
}

impl Render for SurfaceTabGhost {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx);
        div()
            .h(px(24.0))
            .w(px(112.0))
            .px(px(8.0))
            .flex()
            .items_center()
            .rounded(px(6.0))
            .bg(theme.surface_raised)
            .border_1()
            .border_color(theme.border_strong)
            .text_size(crate::typography::ui_rems(11.5))
            .text_color(theme.text)
            .opacity(0.85)
            .child(div().truncate().child(self.title.clone()))
    }
}

struct SurfaceTabTooltip {
    text: SharedString,
}

#[derive(Clone)]
struct SidebarThreadTooltip {
    title: SharedString,
    project: SharedString,
    device: SharedString,
    model: Option<SharedString>,
    branch: Option<SharedString>,
    status: Option<SharedString>,
}

impl SidebarThreadTooltip {
    fn estimated_height(&self) -> f32 {
        let rows = 2
            + usize::from(self.model.is_some())
            + usize::from(self.branch.is_some())
            + usize::from(self.status.is_some());
        56.0 + rows as f32 * 25.0
    }

    fn detail(theme: &Theme, glyph: &'static str, label: SharedString) -> gpui::Div {
        div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .min_w_0()
            .child(
                icon(glyph)
                    .size(px(13.0))
                    .flex_none()
                    .text_color(theme.text_muted),
            )
            .child(div().min_w_0().truncate().child(label))
    }
}

impl Render for SidebarThreadTooltip {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx);
        let card = div()
            .w(px(244.0))
            .px(px(12.0))
            .py(px(11.0))
            .flex()
            .flex_col()
            .gap(px(7.0))
            .rounded(px(9.0))
            .border_1()
            .border_color(theme.border_strong)
            .bg(crate::popover::surface_bg(theme))
            .shadow_md()
            .text_size(crate::typography::ui_rems(11.0))
            .text_color(theme.text_muted)
            .child(
                div()
                    .truncate()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_size(crate::typography::ui_rems(12.0))
                    .text_color(theme.text)
                    .child(self.title.clone()),
            )
            .child(Self::detail(&theme, icons::FOLDER, self.project.clone()))
            .child(Self::detail(&theme, icons::LAPTOP, self.device.clone()))
            .when_some(self.model.clone(), |el, model| {
                el.child(Self::detail(&theme, icons::WIDGET, model))
            })
            .when_some(self.branch.clone(), |el, branch| {
                el.child(Self::detail(&theme, icons::GIT_BRANCH, branch))
            })
            .when_some(self.status.clone(), |el, status| {
                el.child(Self::detail(&theme, icons::CHECK, status))
            });
        crate::frost::frosted(7.0, crate::frost::MENU_BLUR, card)
    }
}

fn sidebar_thread_tooltip_anchor(
    bounds: gpui::Bounds<Pixels>,
    pointer: Point<Pixels>,
    window: &Window,
    action_width: f32,
    tooltip_height: f32,
) -> Option<Point<Pixels>> {
    // The action strip owns its own tooltips. Excluding it also prevents the
    // row tooltip from flashing between adjacent action buttons.
    if !bounds.contains(&pointer) || pointer.x >= bounds.right() - px(action_width) {
        return None;
    }
    let last_top = (window.viewport_size().height - px(tooltip_height + 8.0)).max(px(8.0));
    let top = bounds.top().min(last_top).max(px(8.0));
    // GPUI adds one pixel to an anchor. The card therefore begins at the
    // row's top and just beyond the sidebar edge, independent of the cursor.
    Some(gpui::point(bounds.right() + px(13.0), top - px(1.0)))
}

impl Render for SurfaceTabTooltip {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx);
        let card = div()
            .max_w(px(380.0))
            .px(px(9.0))
            .py(px(6.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(theme.border)
            .bg(crate::popover::surface_bg(theme))
            .text_size(crate::typography::ui_rems(10.5))
            .text_color(theme.text_muted)
            .child(self.text.clone());
        crate::frost::frosted(6.0, crate::frost::MENU_BLUR, card)
    }
}
/// Drag marker for the terminal-panel height handle.
struct TerminalResize;

/// Invisible drag ghost — resize drags render nothing at the cursor.
struct DragGhost;

impl Render for DragGhost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

/// A oneshot width tween (200ms ease-out), driven MANUALLY from render via
/// [`Shell::eval_tween`] — never through a `with_animation` wrapper. gpui keys
/// an animation element's start time by its full global element-id path, so a
/// wrapper that mounts/remounts (route swap, or an ancestor animation keyed by
/// a fresh epoch) silently REPLAYS the tween from t=0. Manual evaluation keeps
/// the element tree's shape constant: a finished or stale tween is exactly the
/// steady state, no matter how the tree around it remounts (round-6 §1–3).
#[derive(Debug, Clone, Copy)]
struct WidthTween {
    from: f32,
    to: f32,
    started: std::time::Instant,
}

impl WidthTween {
    fn new(from: f32, to: f32) -> Self {
        Self {
            from,
            to,
            started: std::time::Instant::now(),
        }
    }
}

fn titlebar_island_vertical_geometry(progress: f32) -> (f32, f32) {
    // Match the padded flex row's center, not the raw titlebar center.
    // Keep the native 24px controls untouched and give them 4px of air.
    let height = 28.0 + 4.0 * progress.clamp(0.0, 1.0);
    let center = (Theme::TITLEBAR_HEIGHT + Theme::TITLEBAR_TOP_PAD) * 0.5;
    (center - height * 0.5, height)
}

fn bottom_stack_measurement_matches(
    measured_has_composer: bool,
    expected_has_composer: bool,
) -> bool {
    measured_has_composer == expected_has_composer
}

fn new_thread_background_opacity(is_frost: bool) -> f32 {
    if is_frost {
        NEW_THREAD_BACKGROUND_FROSTED_OPACITY
    } else {
        1.0
    }
}

fn new_thread_background_height(viewport_height: f32) -> f32 {
    (viewport_height.max(0.0) * NEW_THREAD_BACKGROUND_VIEWPORT_RATIO)
        .min(NEW_THREAD_BACKGROUND_MAX_HEIGHT)
}

fn new_thread_background(
    artwork: Option<std::sync::Arc<gpui::RenderImage>>,
    viewport_height: f32,
    hero_width: f32,
    composer_bounds: crate::new_thread_background_mask::SurfaceBounds,
    dissolve: f32,
    opacity: f32,
) -> AnyElement {
    let Some(artwork) = artwork else {
        return Empty.into_any_element();
    };
    let hero_height = new_thread_background_height(viewport_height);
    let dissolve = dissolve.clamp(0.0, 1.0);
    // Image and treatment share a fixed crop and fade together in place.
    // The hero uses the full conversation canvas even while the destination
    // right pane clips it. Navigation must never rescale the artwork.
    div()
        .absolute()
        .top_0()
        .left_0()
        .w(px(hero_width))
        .h(px(hero_height))
        .overflow_hidden()
        .opacity((1.0 - dissolve) * opacity)
        // Alpha resolves into the real canvas, including translucent themes;
        // no theme-colored overlay bleaches or darkens the source pixels.
        .children([false, true].into_iter().map(|cutout| {
            let artwork = artwork.clone();
            let composer_bounds = composer_bounds.clone();
            div()
                .absolute()
                .inset_0()
                .opacity(if cutout {
                    1.0
                } else {
                    crate::new_thread_background_mask::CUTOUT_REVEAL_OPACITY
                })
                .child(
                    gpui::canvas(
                        |_, _, _| {},
                        move |bounds, _, window, _cx| {
                            if let Some(composer) = composer_bounds.get() {
                                crate::new_thread_background_mask::paint(
                                    artwork.clone(),
                                    bounds,
                                    composer,
                                    cutout,
                                    window,
                                );
                            }
                        },
                    )
                    .absolute()
                    .inset_0(),
                )
        }))
        .into_any_element()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SplashPhase {
    Visible,
    FadingOut,
    Gone,
}

/// The chat-row Rename dialog.
struct RenameChatDialog {
    chat_id: String,
    input: Entity<ComposerInput>,
    /// Focus the input on the dialog's first paint (opened without window access).
    focus_pending: bool,
    _events: Subscription,
}

/// Confirmation surface for a provider-native independent task fork.
struct HistoryForkDialog {
    source_task_id: String,
    boundary: ConversationForkBoundaryKind,
    message_id: Option<String>,
    title: Entity<ComposerInput>,
    prompt: Entity<ComposerInput>,
    snapshot: TaskCommandSnapshot,
    review: serde_json::Value,
    origin: HistoryEngineOrigin,
    config: ChatConfig,
    /// Immutable authority captured from the host-reviewed source config.
    /// Picker changes update only the requested mode, never this ceiling.
    permission_ceiling: PermissionMode,
    cwd: String,
    focus_pending: bool,
    _events: Subscription,
}

fn history_fork_initial_permission(config: &ChatConfig) -> PermissionMode {
    match config.effective_permission_mode {
        PermissionMode::Full if config.permission_mode == PermissionMode::Full => {
            PermissionMode::Full
        }
        PermissionMode::Full | PermissionMode::Auto => PermissionMode::Auto,
        PermissionMode::Ask => PermissionMode::Ask,
    }
}

const HISTORY_FORK_AUTO_ONLY: &[PermissionMode] = &[PermissionMode::Auto];
const HISTORY_FORK_AUTO_FULL: &[PermissionMode] = &[PermissionMode::Auto, PermissionMode::Full];

fn history_fork_permission_choices(ceiling: PermissionMode) -> &'static [PermissionMode] {
    match ceiling {
        PermissionMode::Full => HISTORY_FORK_AUTO_FULL,
        PermissionMode::Auto => HISTORY_FORK_AUTO_ONLY,
        PermissionMode::Ask => &[],
    }
}

fn next_history_fork_permission(
    current: PermissionMode,
    ceiling: PermissionMode,
) -> Option<PermissionMode> {
    let choices = history_fork_permission_choices(ceiling);
    let current = choices.iter().position(|choice| *choice == current)?;
    Some(choices[(current + 1) % choices.len()])
}

/// Staged earlier-message rewrite. The engine creates a native fork and a
/// durable recovery snapshot before this text replaces conversation history.
struct EditEarlierMessageDialog {
    task_id: String,
    message_id: String,
    input: Entity<ComposerInput>,
    snapshot: TaskCommandSnapshot,
    review: serde_json::Value,
    origin: HistoryEngineOrigin,
    focus_pending: bool,
    _events: Subscription,
}

struct ConversationRewindDialog {
    task_id: String,
    message_id: String,
    text: String,
    snapshot: TaskCommandSnapshot,
    review: serde_json::Value,
    origin: HistoryEngineOrigin,
    pending_rewind: Option<String>,
}

struct HistoryRecoveryDialog {
    task_id: String,
    recoveries: Vec<HistoryRecoveryView>,
    selected: usize,
    snapshot: TaskCommandSnapshot,
    review: serde_json::Value,
    origin: HistoryEngineOrigin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HistoryEngineOrigin {
    engine_generation: u64,
}

impl HistoryEngineOrigin {
    fn capture(state: &AppState) -> Self {
        Self {
            engine_generation: state.engine_generation(),
        }
    }

    fn is_current(self, state: &AppState) -> bool {
        self.engine_generation == state.engine_generation()
    }

    fn run_if_current<T>(self, current_generation: u64, run: impl FnOnce() -> T) -> Option<T> {
        (self.engine_generation == current_generation).then(run)
    }
}

fn history_flow_response_is_live(current_generation: u64, request_generation: u64) -> bool {
    current_generation == request_generation
}

fn history_engine_generation_changed(observed: &mut u64, current: u64) -> bool {
    if *observed == current {
        return false;
    }
    *observed = current;
    true
}

fn history_search_response_is_live(
    requested_generation: u64,
    current_generation: u64,
    requested_engine_generation: u64,
    current_engine_generation: u64,
) -> bool {
    requested_generation == current_generation
        && requested_engine_generation == current_engine_generation
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HistorySearchScopeKey {
    scope: HistorySearchScope,
    project_id: Option<String>,
}

impl From<&HistorySearchRequest> for HistorySearchScopeKey {
    fn from(request: &HistorySearchRequest) -> Self {
        Self {
            scope: request.scope,
            project_id: (request.scope == HistorySearchScope::SelectedProject)
                .then(|| request.project_id.clone())
                .flatten(),
        }
    }
}

fn history_search_request(
    query: &str,
    global: bool,
    selected_project: Option<String>,
    spaces: &[luna_proto::Space],
    devices: &[luna_proto::Device],
) -> HistorySearchRequest {
    let mut text = Vec::new();
    let mut filters = HistorySearchFilters::default();
    let mut project_id = selected_project.filter(|selected| {
        spaces
            .iter()
            .any(|space| space.id.eq_ignore_ascii_case(selected))
    });
    let mut explicit_project = false;
    for token in query.split_whitespace() {
        let Some((prefix, value)) = token.split_once(':') else {
            text.push(token);
            continue;
        };
        if value.is_empty() {
            text.push(token);
            continue;
        }
        match prefix.to_ascii_lowercase().as_str() {
            "machine" | "device" => {
                let resolved = devices
                    .iter()
                    .find(|device| {
                        device.id.eq_ignore_ascii_case(value)
                            || device.name.eq_ignore_ascii_case(value)
                    })
                    .map(|device| device.id.clone())
                    .unwrap_or_else(|| value.to_string());
                filters.device_ids.push(resolved);
            }
            "project" => {
                project_id = spaces
                    .iter()
                    .find(|space| {
                        space.id.eq_ignore_ascii_case(value)
                            || space.display_name().eq_ignore_ascii_case(value)
                    })
                    .map(|space| space.id.clone())
                    .or_else(|| Some(value.to_string()));
                explicit_project = true;
            }
            "harness" => {
                let value = value.to_ascii_lowercase();
                if let Ok(harness) =
                    serde_json::from_value::<HarnessId>(serde_json::Value::String(value))
                {
                    filters.harnesses.push(harness);
                }
            }
            "model" => filters.models.push(value.to_string()),
            "status" | "outcome" => {
                let outcome = match value.to_ascii_lowercase().as_str() {
                    "pending" | "running" => Some(ActivityOutcome::Pending),
                    "done" | "success" | "succeeded" | "complete" => {
                        Some(ActivityOutcome::Succeeded)
                    }
                    "failed" | "error" => Some(ActivityOutcome::Failed),
                    "interrupted" | "cancelled" | "canceled" => Some(ActivityOutcome::Interrupted),
                    "unknown" => Some(ActivityOutcome::Unknown),
                    _ => None,
                };
                if let Some(outcome) = outcome {
                    filters.outcomes.push(outcome);
                }
            }
            "tool" => filters.tools.push(value.to_string()),
            "app" => filters.apps.push(value.to_string()),
            "after" => {
                filters.after_ms = parse_history_date(value, false);
            }
            "before" => {
                filters.before_ms = parse_history_date(value, true);
            }
            _ => text.push(token),
        }
    }
    HistorySearchRequest {
        query: text.join(" "),
        scope: if global && !explicit_project {
            HistorySearchScope::Global
        } else {
            HistorySearchScope::SelectedProject
        },
        project_id,
        filters,
        cursor: None,
        limit: 50,
        target_device_id: None,
    }
}

fn parse_history_date(value: &str, end_of_day: bool) -> Option<i64> {
    let date = chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d").ok()?;
    let time = if end_of_day {
        date.and_hms_milli_opt(23, 59, 59, 999)?
    } else {
        date.and_hms_opt(0, 0, 0)?
    };
    Some(time.and_utc().timestamp_millis())
}

fn history_review_profile_unchanged(
    reviewed_session: &Option<String>,
    reviewed_cwd: &Option<String>,
    reviewed_config: &Option<ChatConfig>,
    current: Option<&luna_proto::Chat>,
) -> bool {
    current.is_some_and(|current| {
        current.harness_session_id == *reviewed_session
            && current.cwd == *reviewed_cwd
            && current.config == *reviewed_config
    })
}

/// Searchable same-workspace project reassignment dialog. The UI previews
/// known topology; the authoritative engine repeats canonical validation in
/// the fenced command before committing any metadata.
struct MoveProjectDialog {
    chat_id: String,
    search: Entity<ComposerInput>,
    selected_space_id: Option<String>,
    focus_pending: bool,
    _events: Subscription,
}

/// In-app update lifecycle (desktop installs; see `render_update_strip`).
enum UpdateFlow {
    Idle,
    Downloading,
    /// Staged bundle ready to swap in — one click restarts into it.
    Ready(PathBuf),
    Failed(SharedString),
}

/// Account lifecycle owned by this process. Sign-in on a local workspace
/// flows through the in-place switch wizard (offer → switch → import → done);
/// `RestartPending` survives only as the fallback when the in-place swap
/// fails and a full quit is the safe way out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SyncFlow {
    Idle,
    Enabling,
    Canceling,
    /// Signed in on a local runtime: the wizard's choice step (bring local
    /// work / start fresh / later). `notice_open: false` = postponed, with a
    /// recovery action in Settings.
    SwitchOffer {
        notice_open: bool,
    },
    /// Stopping the local runtime and bootstrapping the synced one in-place.
    Switching {
        import: bool,
    },
    /// The one-time import stream is running on the new synced runtime.
    Importing {
        done: usize,
        total: usize,
    },
    /// Import finished; the success step stays until dismissed.
    ImportDone {
        imported: usize,
        skipped: usize,
    },
    /// The import stream reported errors or died early. Explicit retry step —
    /// structural idempotence makes re-running safe (only missing rows copy).
    /// Details ride `runtime_change_error`. `notice_open: false` = postponed:
    /// the dialog is hidden but the failure stays pending, reachable through
    /// Settings — dismissal must never discard the only retry entry point
    /// (under Synced scope Settings otherwise offers Sign out, and the local
    /// rows would be unreachable).
    ImportFailed {
        notice_open: bool,
    },
    RestartPending {
        notice_open: bool,
    },
    SignOutConfirm,
    SigningOut,
    SignedOutRestartRequired,
}

impl SyncFlow {
    /// States the in-place switch driver owns end-to-end — auth/scope edges
    /// must not reset them while the runtime is being replaced under the UI.
    fn is_switch_lifecycle(self) -> bool {
        matches!(
            self,
            SyncFlow::Switching { .. }
                | SyncFlow::Importing { .. }
                | SyncFlow::ImportDone { .. }
                | SyncFlow::ImportFailed { .. }
        )
    }

    fn has_visible_overlay(self) -> bool {
        match self {
            SyncFlow::Idle
            | SyncFlow::SwitchOffer { notice_open: false }
            | SyncFlow::ImportFailed { notice_open: false }
            | SyncFlow::RestartPending { notice_open: false }
            | SyncFlow::SignedOutRestartRequired => false,
            SyncFlow::Enabling
            | SyncFlow::Canceling
            | SyncFlow::SwitchOffer { notice_open: true }
            | SyncFlow::Switching { .. }
            | SyncFlow::Importing { .. }
            | SyncFlow::ImportDone { .. }
            | SyncFlow::ImportFailed { notice_open: true }
            | SyncFlow::RestartPending { notice_open: true }
            | SyncFlow::SignOutConfirm
            | SyncFlow::SigningOut => true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ShellEscapeOutcome {
    OtherKey,
    Blocked,
    InterruptChat(String),
    Ignored,
}

fn resolve_shell_escape(
    key: &str,
    blocking_overlay: bool,
    escape_stops_active_agent: bool,
    route: Route,
    selected_chat: Option<&str>,
    indicator: Indicator,
    interrupting: bool,
) -> ShellEscapeOutcome {
    if key != "escape" {
        ShellEscapeOutcome::OtherKey
    } else if blocking_overlay {
        ShellEscapeOutcome::Blocked
    } else if !escape_stops_active_agent || !matches!(route, Route::Chat) || interrupting {
        ShellEscapeOutcome::Ignored
    } else if matches!(
        indicator,
        Indicator::Working | Indicator::AwaitingApproval | Indicator::AwaitingInput
    ) {
        selected_chat
            .map(|chat_id| ShellEscapeOutcome::InterruptChat(chat_id.to_owned()))
            .unwrap_or(ShellEscapeOutcome::Ignored)
    } else {
        ShellEscapeOutcome::Ignored
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AccountAction {
    EnableSync,
    SyncInProgress,
    /// Postponed switch wizard (or legacy restart fallback) — reopen it.
    RestartPending,
    SignOut,
}

const RUNTIME_CHANGE_TIMEOUT: Duration = Duration::from_secs(10);
const RUNTIME_CHANGE_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Wait until a stopped daemon can no longer win the next bootstrap probe and
/// has released the data directory for the replacement runtime.
async fn wait_for_remote_engine_shutdown(
    ipc_port: u16,
    data_dir: &std::path::Path,
    timeout: Duration,
) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let port_closed = !matches!(
            tokio::time::timeout(
                Duration::from_millis(200),
                tokio::net::TcpStream::connect(("127.0.0.1", ipc_port)),
            )
            .await,
            Ok(Ok(_))
        );
        if port_closed && InstanceLock::holder(data_dir).is_none() {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "the daemon did not finish stopping within {} seconds",
                timeout.as_secs()
            ));
        }
        tokio::time::sleep(RUNTIME_CHANGE_POLL_INTERVAL).await;
    }
}

/// Stop the engine that owns the synced profile and wait until a local runtime
/// can safely acquire both its IPC port and data-directory lock.
async fn stop_synced_runtime(
    engine: crate::state::EngineHandle,
    ipc_port: u16,
    data_dir: &std::path::Path,
) -> Result<(), String> {
    let stop_error = if matches!(engine.mode(), EngineMode::Remote { .. }) {
        engine
            .client()
            .call(methods::STOP_ENGINE, serde_json::json!({}))
            .await
            .err()
            .map(|error| error.to_string())
    } else {
        None
    };
    engine.shutdown().await;
    match wait_for_remote_engine_shutdown(ipc_port, data_dir, RUNTIME_CHANGE_TIMEOUT).await {
        Ok(()) => Ok(()),
        Err(error) => match stop_error {
            Some(stop_error) => Err(format!("{stop_error}; {error}")),
            None => Err(error),
        },
    }
}

/// What an import-summary stream item means for the wizard: `Ok((imported,
/// skipped))` only when the engine reported zero errors; otherwise the
/// user-facing failure message. Pure so the partial-failure path is testable.
fn import_summary_outcome(item: &serde_json::Value) -> Result<(usize, usize), String> {
    let count = |key: &str| item.get(key).and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    let errors: Vec<&str> = item
        .get("errors")
        .and_then(|e| e.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    if errors.is_empty() {
        return Ok((count("importedChats"), count("skippedChats")));
    }
    let first = errors.first().copied().unwrap_or("unknown error");
    Err(if errors.len() == 1 {
        format!("{} imported, 1 failure: {first}", count("importedChats"))
    } else {
        format!(
            "{} imported, {} failures — first: {first}",
            count("importedChats"),
            errors.len()
        )
    })
}

/// The offer step's description of what a switch would bring along, or `None`
/// when the local profile holds nothing importable. Spaces count as work:
/// a projects-only profile must get the import choice too.
fn local_work_phrase(chats: usize, spaces: usize) -> Option<String> {
    let plural = |n: usize, word: &str| format!("{n} {word}{}", if n == 1 { "" } else { "s" });
    match (chats, spaces) {
        (0, 0) => None,
        (c, 0) => Some(format!("the {}", plural(c, "session"))),
        (0, s) => Some(format!("the {}", plural(s, "project"))),
        (c, s) => Some(format!(
            "the {} and {}",
            plural(c, "session"),
            plural(s, "project")
        )),
    }
}

fn account_action_for_scope(
    scope: Option<WorkspaceScope>,
    flow: SyncFlow,
) -> Option<AccountAction> {
    match scope {
        Some(WorkspaceScope::Local) => match flow {
            SyncFlow::Idle => Some(AccountAction::EnableSync),
            SyncFlow::Enabling | SyncFlow::Canceling => Some(AccountAction::SyncInProgress),
            SyncFlow::SwitchOffer { .. } | SyncFlow::RestartPending { .. } => {
                Some(AccountAction::RestartPending)
            }
            SyncFlow::ImportFailed { .. } => Some(AccountAction::RestartPending),
            SyncFlow::Switching { .. }
            | SyncFlow::Importing { .. }
            | SyncFlow::ImportDone { .. } => Some(AccountAction::SyncInProgress),
            SyncFlow::SignOutConfirm
            | SyncFlow::SigningOut
            | SyncFlow::SignedOutRestartRequired => None,
        },
        Some(WorkspaceScope::Synced) => match flow {
            SyncFlow::SignedOutRestartRequired => None,
            // A pending import failure must stay reachable: this is the only
            // surface that can reopen the retry dialog on a synced runtime.
            SyncFlow::ImportFailed { .. } => Some(AccountAction::RestartPending),
            _ if flow.is_switch_lifecycle() => Some(AccountAction::SyncInProgress),
            _ => Some(AccountAction::SignOut),
        },
        Some(WorkspaceScope::Development) | None => None,
    }
}

fn sync_flow_after_auth(
    flow: SyncFlow,
    scope: Option<WorkspaceScope>,
    auth: Option<&AuthState>,
) -> SyncFlow {
    match scope {
        Some(WorkspaceScope::Local) => match (flow, auth) {
            // The in-place switch owns its own lifecycle once started.
            (flow, _) if flow.is_switch_lifecycle() => flow,
            // AuthStatus belongs to the runtime, not to the Shell that opened
            // the browser. Every attached viewport must advertise the pending
            // profile switch once any of them completes sign-in.
            (SyncFlow::SwitchOffer { .. }, Some(AuthState::SignedOut)) => SyncFlow::Idle,
            (SyncFlow::RestartPending { .. }, Some(AuthState::SignedOut)) => SyncFlow::Idle,
            (SyncFlow::Canceling, Some(AuthState::SignedIn { .. })) => flow,
            (SyncFlow::SwitchOffer { .. }, Some(AuthState::SignedIn { .. })) => flow,
            (SyncFlow::RestartPending { .. }, Some(AuthState::SignedIn { .. })) => flow,
            (_, Some(AuthState::SignedIn { .. })) => SyncFlow::SwitchOffer { notice_open: true },
            _ => flow,
        },
        Some(WorkspaceScope::Synced) => match auth {
            // AuthStatus is shared by every viewport attached to the runtime.
            // Once a synced store loses its credentials, every Shell must stop:
            // letting another viewport sign in would authenticate a new account
            // while the engine still serves the previous account's fixed store.
            Some(AuthState::SignedOut) => SyncFlow::SignedOutRestartRequired,
            _ => match flow {
                SyncFlow::SignOutConfirm
                | SyncFlow::SigningOut
                | SyncFlow::SignedOutRestartRequired => flow,
                flow if flow.is_switch_lifecycle() => flow,
                _ => SyncFlow::Idle,
            },
        },
        Some(WorkspaceScope::Development) => SyncFlow::Idle,
        None => flow,
    }
}

/// The "Create your workspace" gate (feature-inventory §1.2 OrgGate).
struct OrgGateUi {
    name_input: Entity<ComposerInput>,
    orgs: Loadable<Vec<OrgRow>>,
    submitting: bool,
    error: Option<SharedString>,
    task: Option<Task<()>>,
    _events: Subscription,
}

/// One right-pane subagent tab: the doc it shows, its strip title, and the
/// read-only transcript entity whose drop tears the view down.
struct SubagentTab {
    doc_id: String,
    chat_id: String,
    frozen: bool,
    paused: bool,
    title: SharedString,
    transcript: Entity<Transcript>,
    /// Keeps a frozen-blob fetch alive (it falls back to a live doc watch).
    _fetch: Option<Task<()>>,
    /// Spawn chips INSIDE the subagent transcript open their own tabs.
    _events: Subscription,
}

/// Sidebar render identity lets transcript/caret frames reuse its GPUI scene.
/// State and event handlers stay on Shell. Explicit Shell notifications still
/// invalidate the sidebar, including selection, menus, theme and navigation.
struct SidebarPane {
    shell: gpui::WeakEntity<Shell>,
    _observation: Subscription,
}

impl Render for SidebarPane {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::transcript::record_view_frame("sidebar");
        let Some(shell) = self.shell.upgrade() else {
            return div().into_any_element();
        };
        let inner = shell.update(cx, |shell, cx| {
            let theme = Theme::of(cx).clone();
            match shell.route {
                Route::Settings(section) => shell.render_settings_nav(section, &theme, cx),
                Route::Chat => shell.render_chat_sidebar(&theme, cx),
            }
        });
        div().size_full().child(inner).into_any_element()
    }
}

#[derive(Debug, Clone)]
enum PendingExit {
    CloseWindow,
    Quit,
    RuntimeChange,
    InstallUpdate(PathBuf),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SidebarNoticeTone {
    Status,
    Error,
}

/// Clones retain ownership; every newly published notice has its own identity,
/// including consecutive progress messages with identical text.
#[derive(Clone, Debug)]
struct SidebarNotice {
    id: uuid::Uuid,
    tone: SidebarNoticeTone,
    message: SharedString,
    expires_at: Option<std::time::Instant>,
}

impl SidebarNotice {
    fn new(tone: SidebarNoticeTone, message: impl Into<SharedString>) -> Self {
        Self {
            id: uuid::Uuid::new_v4(),
            tone,
            message: message.into(),
            expires_at: None,
        }
    }

    fn status(message: impl Into<SharedString>) -> Self {
        Self::new(SidebarNoticeTone::Status, message)
    }

    fn transient_status(message: impl Into<SharedString>) -> Self {
        let mut notice = Self::status(message);
        notice.expires_at = Some(std::time::Instant::now() + Duration::from_secs(4));
        notice
    }

    fn transient_error(message: impl Into<SharedString>) -> Self {
        let mut notice = Self::new(SidebarNoticeTone::Error, message);
        notice.expires_at = Some(std::time::Instant::now() + Duration::from_secs(10));
        notice
    }
}

// Existing error-strip callers keep their current presentation. Status is
// explicit at the side-chat progress and confirmation call sites.
impl From<SharedString> for SidebarNotice {
    fn from(message: SharedString) -> Self {
        Self::new(SidebarNoticeTone::Error, message)
    }
}

impl From<String> for SidebarNotice {
    fn from(message: String) -> Self {
        Self::new(SidebarNoticeTone::Error, message)
    }
}

impl From<&'static str> for SidebarNotice {
    fn from(message: &'static str) -> Self {
        Self::new(SidebarNoticeTone::Error, message)
    }
}

impl std::fmt::Display for SidebarNotice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.message, f)
    }
}

impl std::ops::Deref for SidebarNotice {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        self.message.as_ref()
    }
}

pub struct Shell {
    state: Entity<AppState>,
    sidebar_pane: Entity<SidebarPane>,
    transcript: Entity<Transcript>,
    composer: Entity<Composer>,
    onboarding_moon: Option<Entity<crate::empty_project_moon::AsciiMoon>>,
    /// Measured height of the bottom chrome stack (status strip + composer +
    /// terminal dock) the full-height transcript scrolls under. Paint-time
    /// measurement schedules another frame whenever this value changes.
    bottom_stack: std::rc::Rc<std::cell::Cell<f32>>,
    /// Whether `bottom_stack` was measured with the session composer present.
    /// A newly selected transcript stays hidden until this matches its route,
    /// preventing one frame at the blank canvas's stale bottom clearance.
    bottom_stack_has_composer: std::rc::Rc<std::cell::Cell<bool>>,
    /// Shared route clock and measured prepaint geometry for the persistent composer.
    composer_dock: crate::composer_dock::SharedDock,
    new_thread_artwork_ready: crate::new_thread_background_effects::Readiness,
    /// The sidebar's archived accordion (t3code Sidebar): OPEN by default
    /// (user request), session-transient. `archived_shown` pages the
    /// expanded list ("Show more" reveals another page).
    pub(super) archived_open: bool,
    pub(super) archived_shown: usize,
    /// Inactive threads are shelved by the sidebar view, without archiving
    /// their provider task. Keep this disclosure collapsed by default.
    pub(super) settled_open: bool,
    pub(super) settled_shown: usize,
    pub(super) active_shown: usize,
    pub(super) sidebar_hidden_active_count: usize,
    /// Archived slim row under the pointer — swaps its time label for the
    /// Unarchive affordance and restores the dimmed harness mark (t3code's
    /// settled-row hover).
    pub(super) archived_hover: Option<String>,
    /// Ephemeral collapsed project/device sections, keyed by organization + id.
    pub(super) sidebar_collapsed_groups: std::collections::HashSet<String>,
    /// In-flight disclosure tweens, shared by device groups and Archived.
    pub(super) sidebar_disclosure_motion:
        std::collections::HashMap<String, SidebarDisclosureMotion>,
    /// The jump-hint overlay: true while the held modifiers exactly match a
    /// jump shortcut, which swaps the first nine rows' time-ago for their
    /// key-cap chip (t3code's `showJumpHints`). Frame-transient — window
    /// deactivation clears it, so a chip cannot stick after an app switch
    /// swallows the key-up.
    pub(super) jump_hints: bool,
    /// Lazy panes: no entity (and no RPC) until first opened.
    terminal: Option<Entity<TerminalPanel>>,
    /// Embedded terminal host for right-pane Terminal surfaces — a SEPARATE
    /// entity from the bottom drawer's (own PTYs, own grid geometry; one
    /// panel can only size one visible grid at a time).
    right_terminal: Option<Entity<TerminalPanel>>,
    right_terminal_sub: Option<Subscription>,
    terminal_picker_notice: Option<(String, SharedString)>,
    /// The surface-tab strip's `+` menu (Files / Terminal / Diffs / History rows).
    right_plus: popover::Popup<()>,
    /// Diff surfaces by id — each tab its own [`Changes`] viewer with its own
    /// scope/base pick and diff watch (multiple diff panels, user request).
    diffs: std::collections::HashMap<u64, Entity<Changes>>,
    /// One workspace browser per chat/panel key. Dropping the entity closes
    /// its file watcher and every in-flight workspace request.
    files: std::collections::HashMap<String, Entity<FilesSurface>>,
    files_subs: std::collections::HashMap<String, Subscription>,
    /// One independent editor/tree per opened workspace file. IDs are global
    /// while the lookup key keeps a file tab scoped to its chat panel.
    file_surfaces: std::collections::HashMap<u64, Entity<FilesSurface>>,
    file_surface_paths: std::collections::HashMap<u64, String>,
    file_surface_keys: std::collections::HashMap<(String, String), u64>,
    file_surface_subs: std::collections::HashMap<u64, Subscription>,
    file_surface_seq: u64,
    pending_file_closes: std::collections::HashSet<RightSurface>,
    pending_exit: Option<PendingExit>,
    /// Event hookups for [`Self::diffs`] (History rows opening commit tabs).
    diff_subs: std::collections::HashMap<u64, Subscription>,
    diff_seq: u64,
    /// Subagent transcript surfaces by id — each tab a read-only
    /// [`Transcript`] pinned to its subagent doc.
    subagent_tabs: std::collections::HashMap<u64, SubagentTab>,
    subagent_seq: u64,
    side_chat_panel: Option<Entity<side_chats::SideChatPanel>>,
    side_chat_events: Option<Subscription>,
    browsers: std::collections::HashMap<u64, Entity<crate::browser::BrowserSurface>>,
    browser_subs: std::collections::HashMap<u64, Subscription>,
    browser_seq: u64,
    browser_context: crate::browser::BrowserContext,
    browser_profile: Option<String>,
    /// Ordered surface tabs per panel key (drag-reorderable; stale entries —
    /// closed terminals/diffs — are skipped at read time).
    right_tabs: std::collections::HashMap<String, Vec<RightSurface>>,
    /// In-flight surface-tab drag (slide animation state).
    right_tab_drag: Option<RightTabDragState>,
    /// Surface-tab strip scroll (the strip overflows horizontally, t3
    /// ScrollArea-style; drag drop-math reads the offset back out).
    right_tab_scroll: gpui::ScrollHandle,
    /// Chat outlet vs settings pages.
    route: Route,
    /// One normalized service response shared by the full page and sidebar.
    usage_snapshot: Option<UsageSnapshot>,
    usage_environment_snapshots: Vec<(String, UsageSnapshot)>,
    usage_environment_filter: Option<String>,
    usage_page_environment_ids: Option<std::collections::HashSet<String>>,
    usage_environment_menu_open: bool,
    usage_error: Option<String>,
    usage_refreshing: bool,
    usage_card_open: bool,
    usage_alerted: Vec<(String, UsageProvider)>,
    usage_provider_filter: Option<UsageProvider>,
    /// False = model, true = day/hour. T3 keeps this choice for the page visit.
    usage_breakdown_by_time: bool,
    usage_page_price_editor_open: bool,
    usage_hover_period_start: Option<i64>,
    usage_request_epoch: u64,
    usage_last_refresh_attempt_ms: Option<i64>,
    usage_task: Option<Task<()>>,
    provider_updates: Vec<ProviderUpdateStatus>,
    _provider_update_poll: Task<()>,
    usage_action_focus: FocusHandle,
    usage_price_inputs: [Entity<ComposerInput>; 5],
    usage_price_error: Option<String>,
    /// Route history behind the titlebar back/forward buttons (§ nav history).
    nav: NavHistory,
    general_page: Option<Entity<GeneralPage>>,
    projects_page: Option<Entity<ProjectsPage>>,
    devices_page: Option<Entity<DevicesPage>>,
    archived_page: Option<Entity<ArchivedPage>>,
    appearance_page: Option<Entity<AppearancePage>>,
    files_settings_page: Option<Entity<FilesSettingsPage>>,
    backups_page: Option<Entity<BackupsPage>>,
    notifications_page: Option<Entity<NotificationsPage>>,
    shortcuts_page: Option<Entity<ShortcutsPage>>,
    accounts_page: Option<Entity<AccountsPage>>,
    shortcuts_sub: Option<Subscription>,
    general_sub: Option<Subscription>,
    notifications_sub: Option<Subscription>,
    files_settings_sub: Option<Subscription>,
    appearance_settings_sub: Option<Subscription>,
    /// Session-row context menu, including the Copy submenu.
    chat_menu: popover::Popup<ChatMenuState>,
    rename_dialog: Option<RenameChatDialog>,
    history_fork_dialog: Option<HistoryForkDialog>,
    edit_earlier_message_dialog: Option<EditEarlierMessageDialog>,
    conversation_rewind_dialog: Option<ConversationRewindDialog>,
    history_recovery_dialog: Option<HistoryRecoveryDialog>,
    history_flow_generation: u64,
    /// Last AppState runtime identity observed by this shell. Any change
    /// invalidates profile-owned history reviews and delayed open responses.
    history_engine_generation: u64,
    move_project_dialog: Option<MoveProjectDialog>,
    /// Task action waiting for the person to confirm it.
    task_action_confirm: Option<(String, TaskConfirmation)>,
    task_action_dont_show_again: bool,
    /// Local sidebar projection while the durable command catches up.
    pending_task_actions: std::collections::HashMap<String, TaskConfirmation>,
    /// Space-row context menu (dropdown rows): (space id, window position).
    space_menu: popover::Popup<(String, Point<Pixels>)>,
    rename_space_dialog: Option<RenameSpaceDialog>,
    /// Space id awaiting delete confirmation (hard delete + session cascade).
    delete_space_confirm: Option<String>,
    /// The add-space palette (device tabs + folder search), `Some`
    /// while open.
    add_space: Option<AddSpaceFlow>,
    /// Monotonic identity for one Add Project flow. Async completions may only
    /// affect the still-open flow that created them.
    add_space_flow_seq: u64,
    command_palette: Option<command_palette::CommandPalette>,
    /// The sidebar's space-filter dropdown.
    spaces_menu: popover::Popup<spaces::SpacesMenu>,
    /// Hover/drag + scroll-linger state of the dropdown's floating rail.
    spaces_menu_bar: popover::MenuScrollbarState,
    /// Persisted organization/sort/metadata controls beside the project filter.
    sidebar_view_menu: popover::Popup<spaces::SidebarViewMenu>,
    /// Natural-tab-order focus target for the icon-only view-options button.
    sidebar_view_trigger_focus: gpui::FocusHandle,
    /// Chat id whose STATUS CORNER is under the pointer — just that corner
    /// swaps to the archive button (t3code's settle-on-hover); hovering the
    /// row body leaves the status readable.
    chat_status_hover: Option<String>,
    /// Scroll position of the sidebar lists region (drives its edge fades).
    sidebar_scroll: gpui::ScrollHandle,
    activity_sidebar_scroll: gpui::ScrollHandle,
    settings_nav_scroll: gpui::ScrollHandle,
    /// L14 session-history query state. Cmd+K owns the only query input;
    /// results are derived/index-only rows and navigate back to the
    /// authoritative transcript.
    history_search_global: bool,
    history_search_results: Vec<ActivityEnvelope>,
    history_search_loading: bool,
    history_search_error: Option<SharedString>,
    history_search_generation: u64,
    history_search_task: Option<Task<()>>,
    history_search_next_cursor: Option<luna_proto::HistoryCursor>,
    history_search_scope_key: Option<HistorySearchScopeKey>,
    activity: activity_center::ActivityUi,
    keep_awake: crate::power::KeepAwake<crate::power::SystemPower>,
    /// `settings.last_space_id` applied once after the first spaces frame.
    space_boot_applied: bool,
    /// Inline sidebar status/error notice; dismissal retires its ownership.
    sidebar_notice: Option<SidebarNotice>,
    cleanup_summary_generation: Option<u64>,
    cleanup_summary_last: Option<String>,
    /// Local lifecycle of an in-app update (macOS bundle swap) — the engine's
    /// UpdateStatus stream says WHETHER one exists; this says how far the
    /// download/stage of it has come in this process.
    update_flow: UpdateFlow,
    update_task: Option<Task<()>>,
    update_auto_apply: bool,
    /// The manual result survives an older engine stream frame during attach.
    update_checked: Option<luna_update::UpdateStatus>,
    update_check_attempted: bool,
    update_checking: bool,
    /// Manual failure time lets a later successful engine check clear stale UI.
    update_check_error: Option<(SharedString, i64)>,
    update_check_task: Option<Task<()>>,
    /// Version whose update strip the user dismissed (advisory installs only —
    /// a newer release shows the strip again).
    update_dismissed: Option<String>,
    /// How this binary was installed — decides the strip's click behavior.
    /// Cached: `detect_install` stats `current_exe` and this renders per frame.
    install: luna_update::InstallKind,
    org: Option<OrgGateUi>,
    sync_flow: SyncFlow,
    mutate_task: Option<Task<()>>,
    auth_task: Option<Task<()>>,
    runtime_change_task: Option<Task<()>>,
    runtime_change_error: Option<SharedString>,
    /// The one-time local→synced import stream (switch wizard progress step).
    import_task: Option<Task<()>>,
    /// Title of the chat the import stream is copying right now.
    import_current: Option<SharedString>,
    /// Kept for the failed-gate "Retry" action.
    boot: EngineBootConfig,
    data_dir: PathBuf,
    settings: UiSettings,
    /// Session-scoped panel open flags (terminal / changes per chat; §1.10-1.11
    /// parity — heights stay in [`UiSettings`]).
    panels: SessionPanels,
    /// The panel key of the chat currently shown ("" = new-chat canvas).
    active_chat: String,
    /// Last selected session survives opening the blank Appshot destination.
    last_appshot_chat: Option<String>,
    /// Last rendered sidebar order (key + estimated height) — the FLIP baseline
    /// for the §1.6 resort glide.
    sidebar_prev_order: Vec<(String, f32)>,
    /// Per-key paint offsets of the resort in flight, keyed elements restart on
    /// `resort_epoch` bumps.
    sidebar_resort: std::collections::HashMap<String, f32>,
    /// Keys that just appeared in a live list (fade in, no glide).
    sidebar_new_keys: std::collections::HashSet<String>,
    resort_epoch: usize,
    /// Last observed `window.is_window_active()` — rising edge fires a
    /// ProbeSync so a broadcast-deaf room heals as the user looks at the app.
    was_window_active: bool,
    /// Dev/testing knobs (`LUNA_OPEN_DIALOG`, `LUNA_FORCE_GATE`,
    /// `LUNA_DEMO_UPLOAD`) — see [`Shell::new`].
    debug_dialog: Option<String>,
    debug_gate: Option<GatePhase>,
    debug_upload: Option<String>,
    sidebar_tween: Option<WidthTween>,
    sidebar_edge_bounce: Option<motion::ResizeEdgeBounce>,
    /// Boundary currently held during a sidebar drag. Cleared on re-entry or
    /// release so the next genuine edge crossing can acknowledge the limit.
    sidebar_resize_edge: Option<motion::ResizeEdge>,
    /// Gesture-owned resize feedback. Unlike hover, this stays active while
    /// the seam moves away from the pointer and clears only on release or when
    /// a constrained edge takes over with its bounce cue.
    pane_resize_active: Option<PaneResizeKind>,
    pane_resize_dragging: Option<PaneResizeKind>,
    right_tween: Option<WidthTween>,
    right_edge_bounce: Option<motion::ResizeEdgeBounce>,
    right_resize_edge: Option<motion::ResizeEdge>,
    /// Mirrors `right_tween` only for takeover entry/exit, allowing the visible
    /// right-panel contents to resize with their outer frame in that mode.
    right_takeover_content_tween: Option<WidthTween>,
    /// Conversation-width tween used only while entering/leaving right-pane
    /// takeover. Normal right-pane open/close keeps the upstream flex behavior.
    main_takeover_tween: Option<WidthTween>,
    /// Changes-panel takeover (the header's expand button): the panel fills
    /// everything right of the sidebar and the conversation column collapses
    /// to zero. Session-local view state — never persisted, reset on close.
    right_pane_expanded: bool,
    /// Viewport width stamped each frame at render — the expanded panel's
    /// width target and the physical ceiling for free-form resizing
    /// ([`Self::right_target`] has no `Window`).
    viewport_width: f32,
    viewport_height: f32,
    terminal_tween: Option<WidthTween>,
    /// Last observed `window.is_fullscreen()` (`None` before first paint) —
    /// flips key the traffic-light inset tween.
    fullscreen: Option<bool>,
    /// 200ms ease-out tween of the cluster start on fullscreen toggles.
    titlebar_tween: Option<WidthTween>,
    titlebar_island: Option<WidthTween>,
    /// Armed by mouse-down on a titlebar strip; the next mouse-move hands the
    /// drag to the compositor (zed's platform-titlebar pattern).
    titlebar_should_move: bool,
    /// The caption buttons luna itself draws on Linux under client-side
    /// decorations, per side, already filtered to what the compositor
    /// supports — `None` off Linux or under server decorations (where the WM
    /// draws real buttons). Re-resolved every frame at the top of `render`.
    linux_captions: Option<gpui::WindowButtonLayout>,
    /// Re-renders when the desktop's button layout changes (GNOME
    /// `button-layout` gsetting). Registered on first paint — [`Shell::new`]
    /// has no window.
    button_layout_sub: Option<Subscription>,
    /// Clears the height tween once it completes (so a closed panel unmounts).
    terminal_tween_task: Option<Task<()>>,
    /// Height-drag anchor: (pointer y, height) at mouse-down on the handle.
    terminal_drag_anchor: Option<(f32, f32)>,
    /// `motion::reduced_motion` snapshot, refreshed at the top of each render
    /// pass so [`Shell::eval_tween`] (called from `&self` render helpers) can
    /// snap without a `cx`.
    reduced_motion: bool,
    /// Set by [`Shell::eval_tween`] when any tween is mid-flight this frame;
    /// render schedules the next animation frame off it.
    motion_active: std::cell::Cell<bool>,
    /// All pane masks and chrome evaluate animation at the same frame time.
    /// A slow render must not give the native page and its titlebar different widths.
    render_time: Option<std::time::Instant>,
    splash: SplashPhase,
    splash_task: Option<Task<()>>,
    /// Focus fallback (registered on first paint — [`Shell::new`] has no
    /// window): keyboard shortcuts dispatch through the window focus chain, so
    /// with missing or unmounted focus they go dead. Recover after handoffs
    /// settle, preserving focus on mounted controls.
    focus_sub: Option<Subscription>,
    shortcut_focus: FocusHandle,
    /// Neutral shortcut target after clicking away from an input.
    unfocused: FocusHandle,
    /// Clears the jump hints when the window deactivates: a Cmd+Tab away
    /// swallows the key-up, so without this the chips stay on screen for good.
    activation_sub: Option<Subscription>,
    /// 1s heartbeat re-rendering the working indicator (elapsed + flavour word).
    _ticker: Task<()>,
    _state_observation: Subscription,
    _composer_events: Subscription,
    /// The primary transcript's spawn-chip events (subagent tabs).
    _transcript_events: Subscription,
    _transcript_invalidation: Subscription,
}

impl Shell {
    #[cfg(feature = "appshots-fixture")]
    pub fn fixture_welcome_moon_state(&self, cx: &App) -> Option<crate::MoonFixtureState> {
        self.onboarding_moon
            .as_ref()
            .map(|moon| moon.read(cx).fixture_state())
    }

    #[cfg(feature = "appshots-fixture")]
    pub fn fixture_welcome_picker_open(&self) -> bool {
        self.add_space.is_some()
    }

    pub fn new(state: Entity<AppState>, boot: EngineBootConfig, cx: &mut Context<Self>) -> Self {
        let observation = cx.observe(&state, |this: &mut Shell, state, cx| {
            this.on_state_changed(&state, cx);
            cx.notify();
        });
        let transcript = cx.new(|cx| Transcript::new(state.clone(), cx));
        transcript.update(cx, |transcript, _| transcript.retain_for_route_exit());
        let composer = cx.new(|cx| Composer::new(state.clone(), cx));
        let links = Self::session_links(None, cx);
        transcript.update(cx, |transcript, _| {
            transcript.set_workspace_link_handler(links)
        });
        // Every send glides the prompt to the viewport top and reserves the
        // reply's space below it (notes-app parity).
        let composer_events = cx.subscribe(&composer, {
            let transcript = transcript.clone();
            move |_this: &mut Shell, _, event: &ComposerEvent, cx| match event {
                ComposerEvent::NewThreadTransitionStarted => {
                    // Route observation drives the dock once selection commits.
                    cx.notify();
                }
                ComposerEvent::Sent {
                    chat_id,
                    message_id,
                } => {
                    transcript.update(cx, |t, cx| {
                        t.on_own_send(chat_id.clone(), message_id.clone(), cx)
                    });
                }
                ComposerEvent::Queued {
                    chat_id,
                    message_id,
                } => {
                    transcript.update(cx, |t, cx| {
                        t.on_own_queued_send(chat_id.clone(), message_id.clone(), cx)
                    });
                }
                ComposerEvent::DraftFlushCompleted => cx.notify(),
            }
        });
        // Spawn chips open their subagent's transcript as a right-pane tab.
        let transcript_events = cx.subscribe(&transcript, Self::on_transcript_event);
        // Working-indicator heartbeat: notify once a second while a session is
        // live so elapsed time and the flavour word stay fresh.
        let ticker = cx.spawn(async move |this, cx| {
            let mut displayed_minute = Utc::now().timestamp().div_euclid(60);
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let minute = Utc::now().timestamp().div_euclid(60);
                let minute_changed = minute != displayed_minute;
                displayed_minute = minute;
                let alive = this.update(cx, |shell: &mut Shell, cx| {
                    let live = {
                        let s = shell.state.read(cx);
                        s.selected_chat
                            .as_deref()
                            .is_some_and(|id| s.indicator_for(id, Utc::now()) != Indicator::None)
                            || s.selected_chat.as_deref().is_some_and(|id| {
                                s.session_for(id)
                                    .and_then(|session| session.recovery.as_ref())
                                    .is_some_and(|recovery| recovery.status.active())
                            })
                            // The connection pill's retry countdown needs the
                            // same per-second refresh while degraded.
                            || matches!(
                                s.connectivity.state,
                                luna_proto::ConnectivityState::Offline
                                    | luna_proto::ConnectivityState::Reconnecting
                            )
                    };
                    // Relative sidebar times still advance when unchanged
                    // presence heartbeats no longer invalidate the whole UI.
                    if live || minute_changed {
                        cx.notify();
                    }
                    if minute_changed {
                        shell.refresh_deletion_cleanup_summary(cx);
                    }
                    shell.maybe_auto_refresh_usage(cx);
                });
                if alive.is_err() {
                    break;
                }
            }
        });
        let provider_update_poll = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(8)).await;
            loop {
                let engine = this
                    .update(cx, |shell: &mut Shell, cx| {
                        shell.state.read(cx).engine().cloned()
                    })
                    .ok()
                    .flatten();
                let Some(engine) = engine else {
                    cx.background_executor().timer(Duration::from_secs(5)).await;
                    continue;
                };
                let result = engine
                    .client()
                    .call(methods::GET_PROVIDER_UPDATES, serde_json::json!({}))
                    .await;
                if let Ok(value) = result
                    && let Ok(statuses) = serde_json::from_value::<Vec<ProviderUpdateStatus>>(value)
                {
                    this.update(cx, |shell, cx| {
                        shell.provider_updates = statuses;
                        cx.notify();
                    })
                    .ok();
                }
                cx.background_executor()
                    .timer(Duration::from_secs(2 * 60 * 60))
                    .await;
            }
        });
        let data_dir = boot.data_dir.clone();
        let settings = settings::current(cx);
        state.update(cx, |state, cx| {
            state.set_change_requests_visible(settings.sidebar_show_pull_request, cx)
        });
        crate::appshots::set_enabled(settings.appshots_enabled);
        crate::appshots::set_capture_sound_enabled(settings.appshot_sound_enabled);
        // Bind the customizable shortcuts from the persisted keymap.
        apply_keymap(cx, &settings.keymap, settings.composer_send_behavior);
        // Dev/testing knob: `LUNA_OPEN_ROUTE=settings[/<section>]` boots
        // straight into a settings section — these pages have no deep link and
        // synthetic input can't reach them on headless compositors.
        let route = match std::env::var("LUNA_OPEN_ROUTE").ok().as_deref() {
            // `new` pins the new-chat canvas (suppresses boot auto-select).
            Some("new") => {
                state.update(cx, |s, _| s.auto_selected = true);
                Route::Chat
            }
            Some(route) => SettingsSection::from_fixture_route(route)
                .map(Route::Settings)
                .unwrap_or(Route::Chat),
            None => Route::Chat,
        };
        // More capture knobs of the same kind: `LUNA_OPEN_DIALOG=rename|delete`
        // opens that dialog for the first chat once chats land; `=model` pops
        // the combined harness/model menu once the shell is Ready;
        // `LUNA_FORCE_GATE=signin|org|failed` renders that gate regardless of
        // real auth state (display-only — for styling passes).
        let debug_dialog = std::env::var("LUNA_OPEN_DIALOG").ok();
        // `LUNA_DEMO_UPLOAD=<pct>:<image path>` fabricates an in-flight image
        // send on the selected chat (echo bubble + frozen thumbnail progress
        // ring) — display-only; a real upload can't be paused for a capture.
        let debug_upload = std::env::var("LUNA_DEMO_UPLOAD").ok();
        let debug_gate = match std::env::var("LUNA_FORCE_GATE").ok().as_deref() {
            Some("signin") => Some(GatePhase::SignIn),
            Some("org") => Some(GatePhase::OrgGate),
            Some("failed") => Some(GatePhase::Failed(
                "Could not reach the luna engine on port 27901".into(),
            )),
            _ => None,
        };
        let nav = NavHistory::new(match route {
            Route::Chat => NavEntry::Chat(String::new()),
            Route::Settings(section) => NavEntry::Settings(section),
        });
        // Parent notifications carry presentation changes (session status,
        // elapsed labels, menus); sibling animation/caret ticks do not.
        let transcript_invalidation = cx.observe_self(|shell, cx| {
            shell.transcript.update(cx, |_, cx| cx.notify());
        });
        let shell = cx.entity();
        let sidebar_pane = cx.new(|cx| SidebarPane {
            shell: shell.downgrade(),
            _observation: cx.observe(&shell, |_, _, cx| cx.notify()),
        });
        let history_engine_generation = state.read(cx).engine_generation();
        Self {
            state,
            sidebar_pane,
            transcript,
            composer,
            onboarding_moon: None,
            // Seed with the compact composer stack's rough height so the
            // first frame's clearance isn't zero (the measure corrects it).
            bottom_stack: std::rc::Rc::new(std::cell::Cell::new(120.0)),
            bottom_stack_has_composer: std::rc::Rc::new(std::cell::Cell::new(false)),
            composer_dock: Default::default(),
            new_thread_artwork_ready: Default::default(),
            archived_open: true,
            archived_shown: 0,
            settled_open: false,
            settled_shown: 0,
            active_shown: 20,
            sidebar_hidden_active_count: 0,
            archived_hover: None,
            sidebar_collapsed_groups: std::collections::HashSet::new(),
            sidebar_disclosure_motion: std::collections::HashMap::new(),
            jump_hints: false,
            terminal: None,
            right_terminal: None,
            right_terminal_sub: None,
            terminal_picker_notice: None,
            right_plus: popover::Popup::default(),
            diffs: std::collections::HashMap::new(),
            files: std::collections::HashMap::new(),
            files_subs: std::collections::HashMap::new(),
            file_surfaces: std::collections::HashMap::new(),
            file_surface_paths: std::collections::HashMap::new(),
            file_surface_keys: std::collections::HashMap::new(),
            file_surface_subs: std::collections::HashMap::new(),
            file_surface_seq: 0,
            pending_file_closes: std::collections::HashSet::new(),
            pending_exit: None,
            diff_subs: std::collections::HashMap::new(),
            diff_seq: 0,
            subagent_tabs: std::collections::HashMap::new(),
            subagent_seq: 0,
            side_chat_panel: None,
            side_chat_events: None,
            browsers: std::collections::HashMap::new(),
            browser_subs: std::collections::HashMap::new(),
            browser_seq: 0,
            browser_context: crate::browser::BrowserContext::default(),
            browser_profile: None,
            right_tabs: std::collections::HashMap::new(),
            right_tab_drag: None,
            right_tab_scroll: gpui::ScrollHandle::new(),
            route,
            usage_snapshot: None,
            usage_environment_snapshots: Vec::new(),
            usage_environment_filter: None,
            usage_page_environment_ids: None,
            usage_environment_menu_open: false,
            usage_error: None,
            usage_refreshing: false,
            usage_card_open: false,
            usage_alerted: Vec::new(),
            usage_provider_filter: None,
            usage_breakdown_by_time: false,
            usage_page_price_editor_open: false,
            usage_hover_period_start: None,
            usage_request_epoch: 0,
            usage_last_refresh_attempt_ms: None,
            usage_task: None,
            provider_updates: Vec::new(),
            _provider_update_poll: provider_update_poll,
            usage_action_focus: cx.focus_handle().tab_stop(true),
            usage_price_inputs: [
                "Exact model name",
                "Input $/1M",
                "Cached $/1M",
                "Creation $/1M",
                "Output $/1M",
            ]
            .map(|placeholder| cx.new(|cx| ComposerInput::new(placeholder, cx))),
            usage_price_error: None,
            nav,
            general_page: None,
            projects_page: None,
            devices_page: None,
            archived_page: None,
            appearance_page: None,
            files_settings_page: None,
            backups_page: None,
            notifications_page: None,
            shortcuts_page: None,
            accounts_page: None,
            shortcuts_sub: None,
            general_sub: None,
            notifications_sub: None,
            files_settings_sub: None,
            appearance_settings_sub: None,
            chat_menu: popover::Popup::default(),
            rename_dialog: None,
            history_fork_dialog: None,
            edit_earlier_message_dialog: None,
            conversation_rewind_dialog: None,
            history_recovery_dialog: None,
            history_flow_generation: 0,
            history_engine_generation,
            move_project_dialog: None,
            task_action_confirm: None,
            task_action_dont_show_again: false,
            pending_task_actions: std::collections::HashMap::new(),
            space_menu: popover::Popup::default(),
            rename_space_dialog: None,
            delete_space_confirm: None,
            add_space: None,
            add_space_flow_seq: 0,
            command_palette: None,
            spaces_menu: popover::Popup::default(),
            spaces_menu_bar: popover::MenuScrollbarState::default(),
            sidebar_view_menu: popover::Popup::default(),
            sidebar_view_trigger_focus: cx.focus_handle().tab_stop(true),
            chat_status_hover: None,
            sidebar_scroll: gpui::ScrollHandle::new(),
            activity_sidebar_scroll: gpui::ScrollHandle::new(),
            settings_nav_scroll: gpui::ScrollHandle::new(),
            history_search_global: false,
            history_search_results: Vec::new(),
            history_search_loading: false,
            history_search_error: None,
            history_search_generation: 0,
            history_search_task: None,
            history_search_next_cursor: None,
            history_search_scope_key: None,
            activity: activity_center::ActivityUi::default(),
            keep_awake: crate::power::KeepAwake::new(crate::power::SystemPower),
            space_boot_applied: false,
            sidebar_notice: None,
            cleanup_summary_generation: None,
            cleanup_summary_last: None,
            update_flow: UpdateFlow::Idle,
            update_task: None,
            update_auto_apply: false,
            update_checked: None,
            update_check_attempted: false,
            update_checking: false,
            update_check_error: None,
            update_check_task: None,
            update_dismissed: None,
            install: luna_update::detect_install(),
            org: None,
            sync_flow: SyncFlow::Idle,
            mutate_task: None,
            auth_task: None,
            runtime_change_task: None,
            runtime_change_error: None,
            import_task: None,
            import_current: None,
            boot,
            data_dir,
            settings,
            panels: SessionPanels::default(),
            active_chat: String::new(),
            last_appshot_chat: None,
            sidebar_prev_order: Vec::new(),
            sidebar_resort: std::collections::HashMap::new(),
            sidebar_new_keys: std::collections::HashSet::new(),
            resort_epoch: 0,
            was_window_active: false,
            debug_dialog,
            debug_gate,
            debug_upload,
            sidebar_tween: None,
            sidebar_edge_bounce: None,
            sidebar_resize_edge: None,
            pane_resize_active: None,
            pane_resize_dragging: None,
            right_tween: None,
            right_edge_bounce: None,
            right_resize_edge: None,
            right_takeover_content_tween: None,
            main_takeover_tween: None,
            right_pane_expanded: false,
            viewport_width: 1280.0,
            viewport_height: 880.0,
            terminal_tween: None,
            fullscreen: None,
            titlebar_tween: None,
            titlebar_island: None,
            titlebar_should_move: false,
            linux_captions: None,
            button_layout_sub: None,
            terminal_tween_task: None,
            terminal_drag_anchor: None,
            reduced_motion: false,
            motion_active: std::cell::Cell::new(false),
            render_time: None,
            splash: SplashPhase::Visible,
            splash_task: None,
            focus_sub: None,
            shortcut_focus: cx.focus_handle(),
            unfocused: cx.focus_handle(),
            activation_sub: None,
            _ticker: ticker,
            _state_observation: observation,
            _composer_events: composer_events,
            _transcript_events: transcript_events,
            _transcript_invalidation: transcript_invalidation,
        }
    }

    /// Route a completed viewer-side capture only after its source window is
    /// safely captured. The explicit target key avoids relying on the
    /// state-observation/draft-swap effect ordering when opening the canvas.
    pub fn receive_appshot(
        &mut self,
        appshot: crate::appshots::CapturedAppshot,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::appshots::AppshotDestination;

        let selected = self.state.read(cx).selected_chat.clone();
        let target = match self.settings.appshot_destination {
            AppshotDestination::Automatic if selected.is_some() => selected,
            AppshotDestination::LastSession if selected.is_some() => selected,
            AppshotDestination::LastSession => self
                .last_appshot_chat
                .clone()
                .filter(|id| self.state.read(cx).chats.iter().any(|chat| &chat.id == id)),
            AppshotDestination::Automatic | AppshotDestination::NewSession => None,
        };
        if let Some(chat_id) = &target {
            self.open_chat(chat_id.clone(), cx);
        } else if self.settings.appshot_destination == AppshotDestination::NewSession
            || self.state.read(cx).selected_chat.is_some()
        {
            // Reuse upstream's project-filter and device defaults for a new
            // canvas. Automatic capture on an existing canvas keeps its pick.
            self.open_new_session(cx);
        } else {
            self.route = Route::Chat;
        }
        let key = target.unwrap_or_default();
        self.composer.update(cx, |composer, cx| {
            composer.stage_appshot_for(key, appshot, cx)
        });
        window.focus(&self.composer.focus_handle(cx), cx);
        cx.notify();
    }

    pub fn show_appshot_error(
        &mut self,
        message: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.route = Route::Chat;
        self.composer
            .update(cx, |composer, cx| composer.show_appshot_error(message, cx));
        window.focus(&self.composer.focus_handle(cx), cx);
        cx.notify();
    }

    // ---- splash ----

    fn reconcile_session_history_scope(&mut self, engine_changed: bool, cx: &mut Context<Self>) {
        // Session history follows the effective search scope rather than the
        // open task's immutable project. The generation fence owns late pages
        // whenever that scope changes.
        let current_scope = self
            .current_session_history_request(None, cx)
            .map(|request| HistorySearchScopeKey::from(&request));
        let scope_changed = self.history_search_scope_key.is_some()
            && self.history_search_scope_key != current_scope;
        if engine_changed || scope_changed {
            self.cancel_history_flow(cx);
            self.history_search_generation = self.history_search_generation.wrapping_add(1);
            if engine_changed {
                self.history_search_task = None;
            } else if let Some(task) = self.history_search_task.take() {
                // Let an already-issued same-profile request finish so the
                // generation fence, not cancellation timing, owns stale scope.
                task.detach();
            }
            self.history_search_results.clear();
            self.history_search_loading = false;
            self.history_search_error = None;
            self.history_search_next_cursor = None;
            self.history_search_scope_key = current_scope;
            if self.command_palette_query(cx).is_some() {
                self.begin_session_history_search(cx);
            }
        }
    }

    fn on_state_changed(&mut self, state: &Entity<AppState>, cx: &mut Context<Self>) {
        self.pending_task_actions.retain(|task_id, action| {
            let chat = state.read(cx).chats.iter().find(|chat| chat.id == *task_id);
            match action {
                TaskConfirmation::Archive => chat.is_some_and(|chat| !chat.archived),
                TaskConfirmation::Settle => chat.is_some_and(|chat| {
                    !chat
                        .settlement_override
                        .as_ref()
                        .is_some_and(|override_state| {
                            override_state.disposition == luna_proto::SettlementDisposition::Settled
                                && override_state.at
                                    >= chat.last_message_at.unwrap_or(chat.created_at)
                        })
                }),
                TaskConfirmation::Delete => chat.is_some(),
            }
        });
        let engine_generation = state.read(cx).engine_generation();
        let engine_changed = history_engine_generation_changed(
            &mut self.history_engine_generation,
            engine_generation,
        );
        if engine_changed {
            if self.sidebar_notice.as_ref().map(ToString::to_string) == self.cleanup_summary_last {
                self.sidebar_notice = None;
            }
            self.cleanup_summary_generation = None;
            self.cleanup_summary_last = None;
            self.usage_request_epoch = self.usage_request_epoch.wrapping_add(1);
            self.usage_last_refresh_attempt_ms = None;
            self.usage_task = None;
            self.usage_refreshing = false;
            self.usage_snapshot = None;
            self.usage_environment_snapshots.clear();
            self.usage_alerted.clear();
            self.usage_environment_filter = None;
            self.usage_error = Some("Engine changed · refresh usage for this environment".into());
        }
        self.reconcile_session_history_scope(engine_changed, cx);
        if matches!(state.read(cx).connection, ConnectionStatus::Ready)
            && self.cleanup_summary_generation != Some(engine_generation)
        {
            self.cleanup_summary_generation = Some(engine_generation);
            self.refresh_deletion_cleanup_summary(cx);
        }
        self.reconcile_keep_awake(cx);
        self.ensure_activity_polling(cx);
        self.refresh_activity(cx);
        if let Some(notice) = state.update(cx, |state, _| state.take_deep_link_notice()) {
            self.sidebar_notice = Some(notice.into());
        }
        let next_sync_flow = {
            let state = state.read(cx);
            sync_flow_after_auth(self.sync_flow, state.workspace_scope, state.auth.as_ref())
        };
        if next_sync_flow != self.sync_flow {
            self.sync_flow = next_sync_flow;
            if matches!(
                self.sync_flow,
                SyncFlow::RestartPending { .. } | SyncFlow::SwitchOffer { .. }
            ) {
                self.org = None;
            }
        }
        // The in-place local→synced switch: once the replacement runtime is
        // attached and Ready, kick the import (or finish) from here.
        self.drive_sync_switch(cx);
        let signed_out_synced = {
            let state = state.read(cx);
            state.workspace_scope == Some(WorkspaceScope::Synced)
                && matches!(state.auth, Some(AuthState::SignedOut))
        };
        // AuthStatus is shared by every viewport. Whichever viewport owns the
        // embedded runtime drains it; remote viewports request daemon shutdown
        // and all of them independently reattach to the new local runtime.
        if signed_out_synced && self.runtime_change_task.is_none() {
            self.start_local_runtime_transition(false, cx);
        }
        // Capture knob: the add-space palette needs only the device registry.
        if self.debug_dialog.as_deref() == Some("add-space") && !state.read(cx).devices.is_empty() {
            self.debug_dialog = None;
            self.open_add_space(cx);
        }
        // Capture knob: pop the requested dialog once chats have landed.
        if let Some(which) = self.debug_dialog.clone()
            && let Some(first) = state.read(cx).chats.first().map(|c| c.id.clone())
        {
            self.debug_dialog = None;
            match which.as_str() {
                "rename" => self.open_rename_chat(first, cx),
                "delete" => {
                    self.request_task_action(first, TaskConfirmation::Delete, cx);
                }
                _ => {}
            }
        }
        // Capture knob: `LUNA_DEMO_UPLOAD=<pct>:<image path>` — once a chat
        // is selected, push a fake sending echo carrying that image as a
        // pending attachment and freeze upload progress at <pct>, so the
        // thumbnail progress ring can be styled/screenshotted (a real upload
        // is too fast to pause).
        if let Some(spec) = self.debug_upload.clone()
            && let Some(chat_id) = state.read(cx).selected_chat.clone()
        {
            self.debug_upload = None;
            if let Some((pct, img_path)) = spec.split_once(':')
                && let Ok(pct) = pct.parse::<u64>()
                && let Ok(att) = crate::attachments::stage_file(std::path::Path::new(img_path))
            {
                let pending_path = format!("pending/{}/{}", att.id, att.name);
                let device_ids: Vec<String> = {
                    let s = state.read(cx);
                    s.selected_chat_row()
                        .map(|c| c.device_id.clone())
                        .into_iter()
                        .chain(s.local_device_id.clone())
                        .chain(Some("local".to_string()))
                        .collect()
                };
                for device_id in &device_ids {
                    crate::attachments::seed_attachment(
                        device_id,
                        &pending_path,
                        &att.name,
                        att.image.clone(),
                    );
                }
                let text = crate::attachments::with_attachments(
                    "Here is the screenshot of the bug.",
                    std::slice::from_ref(&pending_path),
                );
                let echo = luna_doc::SessionMessageEntry {
                    id: "demo-upload-echo".into(),
                    role: luna_doc::MessageRole::User,
                    parts: vec![luna_doc::MessagePart::Text {
                        id: "t0".into(),
                        text,
                    }],
                    created_at: chrono::Utc::now().timestamp_millis(),
                    device_id: "local".into(),
                    status: None,
                    continuation_of: None,
                };
                state.update(cx, |s, cx| {
                    s.push_echo(&chat_id, echo);
                    s.begin_upload_progress(
                        100,
                        std::sync::Arc::new(std::sync::atomic::AtomicU64::new(pct)),
                    );
                    cx.notify();
                });
            }
        }
        // Boot: restore the last selected space once the first spaces frame
        // lands (a still-existing row wins over the auto-selected first one;
        // the boot-auto-selected chat's own space wins over both — selecting a
        // chat implies its space, which `select_chat` already applied).
        if !self.space_boot_applied && state.read(cx).spaces_synced {
            self.space_boot_applied = true;
            if state.read(cx).selected_chat.is_none() {
                // A set sidebar filter is an explicit standing choice. The
                // canvas defaults to that project, then the last selection.
                let exists = |id: &String| state.read(cx).space_row(id).is_some();
                let filter = self.settings.space_filter.clone().filter(&exists);
                let target = filter.or_else(|| self.settings.last_space_id.clone().filter(&exists));
                if target.is_some() {
                    state.update(cx, |s, cx| s.select_space(target, cx));
                }
            }
        }
        // Persist the selected space (the new-tab fallback under "All").
        {
            let selected_space = state.read(cx).selected_space.clone();
            if selected_space != self.settings.last_space_id && selected_space.is_some() {
                self.settings.last_space_id = selected_space;
                self.schedule_save(cx);
            }
        }
        // Boot landing: the most recent session once the first chats frame
        // syncs (manual selection wins).
        self.boot_select_chat(cx);
        // Heal a dangling sidebar filter (space deleted, possibly elsewhere):
        // fall back to "All" rather than filtering everything out.
        if state.read(cx).spaces_synced
            && let Some(filter) = self.settings.space_filter.clone()
            && state.read(cx).space_row(&filter).is_none()
        {
            self.settings.space_filter = None;
            self.schedule_save(cx);
        }
        // Healing a persisted, deleted filter above changes the effective
        // Project scope without changing AppState again.
        self.reconcile_session_history_scope(false, cx);
        if self
            .side_chat_panel
            .as_ref()
            .is_some_and(|panel| !panel.read(cx).is_current(cx))
        {
            self.release_side_chat(cx);
        }
        // Chat switch: restore THAT chat's panel state (per-session open flags;
        // snap, no tween — the panels belong to the destination chat).
        let selected = state.read(cx).selected_chat.clone().unwrap_or_default();
        if !selected.is_empty() {
            self.last_appshot_chat = Some(selected.clone());
        }
        if selected != self.active_chat {
            self.release_side_chat(cx);
            self.suspend_subagent_watches(None, cx);
            self.suspend_file_images(cx);
            self.active_chat = selected;
            // Route history: a chat switch is a navigation. The very first
            // selection off the untouched boot canvas REPLACES that entry —
            // luna's `/` route redirected into the last-used chat, leaving no
            // dead Back target. Walking history lands here too, but the
            // destination already equals `current()`, so the push dedups.
            if matches!(self.route, Route::Chat) {
                let entry = NavEntry::Chat(self.active_chat.clone());
                if self.nav.len() == 1 && *self.nav.current() == NavEntry::Chat(String::new()) {
                    self.nav.replace(entry);
                } else {
                    self.nav.push(entry);
                }
            }
            self.right_tween = None;
            self.right_takeover_content_tween = None;
            self.main_takeover_tween = None;
            self.terminal_tween = None;
            let panels = self.panels.get(&self.panel_key(cx));
            if let Some(panel) = self.terminal.clone() {
                panel.update(cx, |panel, cx| panel.set_open(panels.terminal_open, cx));
            }
            if panels.changes_open
                && let RightSurface::Diff(id) = self.resolved_right_active(cx)
                && let Some(changes) = self.diffs.get(&id).cloned()
            {
                changes.update(cx, |changes, cx| changes.ensure_content(cx));
            }
        }
        match state.read(cx).connection {
            ConnectionStatus::Ready => {
                if self.splash == SplashPhase::Visible {
                    self.splash = SplashPhase::FadingOut;
                    self.splash_task = Some(cx.spawn(async move |this, cx| {
                        cx.background_executor()
                            .timer(SPLASH_OUT.total() + Duration::from_millis(30))
                            .await;
                        this.update(cx, |shell, cx| {
                            shell.splash = SplashPhase::Gone;
                            cx.notify();
                        })
                        .ok();
                    }));
                }
            }
            // Reveal the gate card immediately; the splash never returns mid-session.
            ConnectionStatus::Failed(_) => self.splash = SplashPhase::Gone,
            ConnectionStatus::Connecting => {}
        }
    }

    // ---- layout state ----

    fn sidebar_target(&self) -> f32 {
        if self.settings.sidebar_collapsed {
            0.0
        } else {
            self.settings.sidebar_width
        }
    }

    /// Does the selected space's folder have git? Owner-stamped and synced —
    /// gates the Changes pane, its toggle, and Cmd-B with zero RPCs.
    fn space_git_detected(&self, cx: &App) -> bool {
        self.state.read(cx).selected_space_git()
    }

    /// The current chat's changes-pane flag (per-session, in-memory), gated on
    /// the space having git at all: a stale per-chat open flag must not reopen
    /// the pane after switching into a non-git space.
    /// The per-session panel key. The new-chat canvas (no selection) keys per
    /// SPACE — one shared "" key made a canvas toggle read as global state
    /// (user report).
    fn panel_key(&self, cx: &App) -> String {
        if self.active_chat.is_empty() {
            let space = self
                .state
                .read(cx)
                .selected_space
                .clone()
                .unwrap_or_default();
            format!("space-canvas:{space}")
        } else {
            self.active_chat.clone()
        }
    }

    /// Whether the right pane shows. NOT gated on git any more: the pane is
    /// a surface HOST now (terminals work in any space), so only the Git
    /// surface rows check `space_git_detected`. Still hidden on the
    /// new-session canvas, where the titlebar carries no toggle to close it
    /// again (an earlier user request).
    fn right_pane_open(&self, cx: &App) -> bool {
        !self.active_chat.is_empty() && self.panels.get(&self.panel_key(cx)).changes_open
    }

    /// The current chat's terminal flag (per-session, in-memory).
    fn terminal_open(&self, cx: &App) -> bool {
        self.panels.get(&self.panel_key(cx)).terminal_open
    }

    fn right_target(&self, cx: &App) -> f32 {
        if !self.right_pane_open(cx) {
            0.0
        } else {
            // Manual sizing preserves a usable conversation column. Takeover
            // intentionally consumes it completely. Both ride the sidebar
            // tween so toggling it remains seamless.
            let sidebar_now = self.sidebar_now();
            if self.right_pane_expanded {
                right_pane_takeover_width(self.viewport_width, sidebar_now)
            } else {
                self.settings
                    .right_pane_width
                    .min(right_pane_max_width(self.viewport_width, sidebar_now))
            }
        }
    }

    fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        let from = self.sidebar_now();
        self.sidebar_edge_bounce = None;
        self.sidebar_resize_edge = None;
        self.pane_resize_active = None;
        self.pane_resize_dragging = None;
        self.settings.sidebar_collapsed = !self.settings.sidebar_collapsed;
        if self.settings.sidebar_collapsed {
            self.usage_card_open = false;
        }
        self.sidebar_tween = Some(WidthTween::new(from, self.sidebar_target()));
        self.schedule_save(cx);
        cx.notify();
    }

    fn toggle_right_pane(&mut self, cx: &mut Context<Self>) {
        // Reverse from the visible width when toggled during an animation.
        let from = self.eval_tween(self.right_tween, self.right_target(cx));
        self.right_edge_bounce = None;
        self.right_resize_edge = None;
        self.finish_pane_resize(PaneResizeKind::Right);
        let sidebar_now = self.sidebar_now();
        let from_main = conversation_width(self.viewport_width, sidebar_now, from);
        let was_expanded = self.right_pane_expanded;
        let key = self.panel_key(cx);
        let open = self.panels.toggle_changes(&key);
        if !open {
            self.release_side_chat(cx);
            self.suspend_subagent_watches(None, cx);
            self.suspend_file_images(cx);
            // Closing always leaves takeover mode — reopening at full bleed
            // with the conversation gone read as a broken chat.
            self.right_pane_expanded = false;
        }
        let to = self.right_target(cx);
        self.right_tween = Some(WidthTween::new(from, to));
        self.right_takeover_content_tween = None;
        self.main_takeover_tween = was_expanded.then(|| {
            WidthTween::new(
                from_main,
                conversation_width(self.viewport_width, sidebar_now, to),
            )
        });
        if open
            && let RightSurface::Diff(id) = self.resolved_right_active(cx)
            && let Some(changes) = self.diffs.get(&id).cloned()
        {
            // Reopening onto a diff tab revalidates its watch.
            changes.update(cx, |changes, cx| changes.ensure_content(cx));
        }
        cx.notify();
    }

    fn right_terminal_panel(&mut self, cx: &mut Context<Self>) -> Entity<TerminalPanel> {
        if let Some(terminal) = &self.right_terminal {
            return terminal.clone();
        }
        let terminal = cx.new(|cx| TerminalPanel::new_embedded(self.state.clone(), cx));
        let sub = cx.subscribe(&terminal, |this: &mut Self, _, event, cx| match event {
            TerminalPanelEvent::SavedTabsLoaded { chat_id, tab_keys } => {
                this.register_saved_terminal_surfaces(chat_id, tab_keys, cx);
            }
            TerminalPanelEvent::SavedTabsUnavailable { chat_id } => {
                if this.active_chat == *chat_id {
                    this.terminal_picker_notice = Some((
                        chat_id.clone(),
                        "Saved terminals could not be loaded. Try Terminal again.".into(),
                    ));
                    cx.notify();
                }
            }
        });
        self.right_terminal_sub = Some(sub);
        self.right_terminal = Some(terminal.clone());
        terminal
    }

    fn register_saved_terminal_surfaces(
        &mut self,
        chat_id: &str,
        tab_keys: &[u64],
        cx: &mut Context<Self>,
    ) {
        let activate = self.active_chat == chat_id
            && self.panels.get(chat_id).right_active == RightSurface::Picker;
        if !tab_keys.is_empty() {
            let tabs = self.right_tabs.entry(chat_id.to_string()).or_default();
            for tab in tab_keys {
                let surface = RightSurface::Terminal(*tab);
                if !tabs.contains(&surface) {
                    tabs.push(surface);
                }
            }
        }
        if !tab_keys.is_empty()
            && self
                .terminal_picker_notice
                .as_ref()
                .is_some_and(|(chat, _)| chat == chat_id)
        {
            self.terminal_picker_notice = None;
        }
        if self.active_chat == chat_id {
            if let Some(first) = tab_keys.first() {
                if activate {
                    // This event may arrive while the terminal entity is in
                    // its own update. Store the selection without reading or
                    // re-entering that entity; the next shell paint mounts it.
                    self.panels.update(chat_id, |panel| {
                        panel.right_active = RightSurface::Terminal(*first);
                    });
                }
            } else {
                self.terminal_picker_notice = Some((
                    chat_id.to_string(),
                    "No saved terminals. Use + → Terminal to start a new shell.".into(),
                ));
            }
            cx.notify();
        }
    }

    /// The right pane's surface tabs in the STORED (drag-reorderable) order —
    /// `(surface, title)`; entries whose backing tab/entity is gone are
    /// skipped.
    fn right_surface_rows(
        &self,
        cx: &App,
    ) -> Vec<(RightSurface, SharedString, bool, Option<SharedString>)> {
        let key = self.panel_key(cx);
        let stored: &[RightSurface] = self
            .right_tabs
            .get(&key)
            .map(|v| v.as_slice())
            .unwrap_or(&[]);
        let terminals: Vec<(u64, SharedString, bool)> = self
            .right_terminal
            .as_ref()
            .map(|t| t.read(cx).tab_summaries(cx))
            .unwrap_or_default();
        stored
            .iter()
            .filter_map(|surface| match surface {
                RightSurface::Files => self.files.get(&key).map(|files| {
                    let files = files.read(cx);
                    (
                        *surface,
                        files.tab_title(),
                        files.has_unsaved_changes(),
                        None,
                    )
                }),
                RightSurface::File(id) => self.file_surfaces.get(id).map(|file| {
                    let path = self.file_surface_paths.get(id);
                    let title = path
                        .map(|path| workspace_file_title(path))
                        .unwrap_or_else(|| SharedString::from("File"));
                    (
                        *surface,
                        title,
                        file.read(cx).has_unsaved_changes(),
                        path.cloned().map(Into::into),
                    )
                }),
                RightSurface::Diff(id) => self
                    .diffs
                    .get(id)
                    // Contextual title (user request): the pane's scope
                    // label, or the pinned commit's subject.
                    .map(|changes| (*surface, changes.read(cx).tab_title(), false, None)),
                RightSurface::Terminal(tab) => terminals
                    .iter()
                    .find(|(k, _, _)| k == tab)
                    .map(|(_, title, _)| (*surface, title.clone(), false, None)),
                RightSurface::Subagent(id) => self
                    .subagent_tabs
                    .get(id)
                    .map(|tab| (*surface, tab.title.clone(), false, None)),
                RightSurface::Browser(id) => self.browsers.get(id).map(|browser| {
                    let browser = browser.read(cx);
                    (
                        *surface,
                        browser.title(),
                        false,
                        browser.page.url.clone().map(Into::into),
                    )
                }),
                RightSurface::Agents => Some((*surface, "Agents".into(), false, None)),
                RightSurface::SideChats => Some((*surface, "Side chats".into(), false, None)),
                RightSurface::Picker => None,
            })
            .collect()
    }

    fn workspace_path_for_surface(
        &self,
        surface: RightSurface,
        cx: &App,
    ) -> Option<WorkspacePathDrag> {
        let path = match surface {
            RightSurface::Files => self
                .files
                .get(&self.panel_key(cx))
                .and_then(|files| files.read(cx).attachment_path().map(str::to_string))?,
            RightSurface::File(id) => self.file_surface_paths.get(&id)?.clone(),
            RightSurface::Picker
            | RightSurface::Diff(_)
            | RightSurface::Terminal(_)
            | RightSurface::Agents
            | RightSurface::SideChats
            | RightSurface::Subagent(_)
            | RightSurface::Browser(_) => {
                return None;
            }
        };
        Some(WorkspacePathDrag::new(path, false))
    }

    /// Drag-reorder a surface tab within this chat's strip.
    fn reorder_right_tabs(&mut self, from: usize, to: usize, cx: &mut Context<Self>) {
        let key = self.panel_key(cx);
        if let Some(tabs) = self.right_tabs.get_mut(&key)
            && from < tabs.len()
            && to < tabs.len()
            && from != to
        {
            let surface = tabs.remove(from);
            tabs.insert(to, surface);
            cx.notify();
        }
    }

    /// Track the hovered drop slot mid-drag (the terminal drawer's
    /// `update_drag_over`, ported: epoch bumps restart the slide tween).
    fn update_right_tab_drag_over(&mut self, from: usize, over: usize, cx: &mut Context<Self>) {
        match &mut self.right_tab_drag {
            Some(drag) if drag.over != over => {
                drag.prev_over = drag.over;
                drag.over = over;
                drag.epoch += 1;
                cx.notify();
            }
            Some(_) => {}
            None => {
                self.right_tab_drag = Some(RightTabDragState {
                    from,
                    over,
                    epoch: 0,
                    prev_over: from,
                });
                cx.notify();
            }
        }
    }

    /// The surface that actually renders: the stored pick when it still
    /// exists, else the first remaining tab, else the picker. Terminal keys
    /// go stale when their tab closes/exits — never render a dead surface.
    fn resolved_right_active(&self, cx: &App) -> RightSurface {
        let picked = self.panels.get(&self.panel_key(cx)).right_active;
        let rows = self.right_surface_rows(cx);
        let exists = match picked {
            RightSurface::Picker => false,
            surface => rows.iter().any(|(s, _, _, _)| *s == surface),
        };
        if exists {
            picked
        } else {
            rows.first()
                .map(|(s, _, _, _)| *s)
                .unwrap_or(RightSurface::Picker)
        }
    }

    fn suspend_file_images(&mut self, cx: &mut Context<Self>) {
        for files in self.files.values().chain(self.file_surfaces.values()) {
            files.update(cx, |files, cx| files.suspend_images(cx));
        }
    }

    fn set_right_active(&mut self, surface: RightSurface, cx: &mut Context<Self>) {
        if self.resolved_right_active(cx) != surface {
            self.release_side_chat(cx);
            self.suspend_subagent_watches(Some(surface), cx);
            self.suspend_file_images(cx);
        }
        let key = self.panel_key(cx);
        self.panels.update(&key, |p| p.right_active = surface);
        match surface {
            RightSurface::Files => {
                if let Some(files) = self.files.get(&key).cloned() {
                    files.update(cx, |files, cx| files.ensure_loaded(cx));
                }
            }
            RightSurface::File(id) => {
                if let Some(file) = self.file_surfaces.get(&id).cloned() {
                    file.update(cx, |file, cx| file.ensure_loaded(cx));
                }
            }
            RightSurface::Terminal(tab) => {
                let panel = self.right_terminal_panel(cx);
                self.composer
                    .update(cx, |composer, _| composer.focus_pending = false);
                panel.update(cx, |panel, cx| {
                    panel.select_tab_by_key(tab, cx);
                    panel.request_focus(cx);
                });
            }
            RightSurface::Diff(id) => {
                if let Some(changes) = self.diffs.get(&id).cloned() {
                    changes.update(cx, |changes, cx| changes.ensure_content(cx));
                }
            }
            // The tab's feed (watch or snapshot) runs from open to close —
            // activation needs no revalidation.
            RightSurface::Subagent(_)
            | RightSurface::Browser(_)
            | RightSurface::Agents
            | RightSurface::SideChats => {}
            RightSurface::Picker => {}
        }
        cx.notify();
    }

    fn focus_right_file_editor(
        &mut self,
        surface: RightSurface,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let RightSurface::Browser(id) = surface {
            if let Some(browser) = self.browsers.get(&id).cloned() {
                browser.update(cx, |browser, cx| browser.focus_address(window, cx));
            }
            return;
        }
        let key = self.panel_key(cx);
        let files = match surface {
            RightSurface::Files => self.files.get(&key).cloned(),
            RightSurface::File(id) => self.file_surfaces.get(&id).cloned(),
            _ => None,
        };
        if let Some(files) = files {
            files.update(cx, |files, cx| {
                files.focus_editor(window, cx);
            });
        }
    }

    fn set_files_word_wrap(
        &mut self,
        word_wrap: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings.files_word_wrap = word_wrap;
        if let Some(page) = self.files_settings_page.clone() {
            page.update(cx, |page, cx| page.set_word_wrap(word_wrap, cx));
        }
        let surfaces = self
            .files
            .values()
            .chain(self.file_surfaces.values())
            .cloned()
            .collect::<Vec<_>>();
        for surface in surfaces {
            surface.update(cx, |surface, cx| {
                surface.set_word_wrap(word_wrap, window, cx)
            });
        }
        settings::update(SavePolicy::Immediate, cx, |settings| {
            settings.files_word_wrap = word_wrap;
        });
        cx.notify();
    }

    /// Push a new code size into every open file surface. Called by the
    /// Appearance settings page, which owns the control. The typography
    /// global is the canonical store and persists on its own; this only
    /// propagates the change to already-open surfaces.
    pub(crate) fn set_code_font_size(&mut self, code_font_size: f32, cx: &mut Context<Self>) {
        let surfaces = self
            .files
            .values()
            .chain(self.file_surfaces.values())
            .cloned()
            .collect::<Vec<_>>();
        for surface in surfaces {
            surface.update(cx, |surface, cx| {
                surface.set_editor_font_size(code_font_size, cx)
            });
        }
        cx.notify();
    }

    fn set_files_show_all(&mut self, show_all_files: bool, cx: &mut Context<Self>) {
        self.settings.files_show_all = show_all_files;
        if let Some(page) = self.files_settings_page.clone() {
            page.update(cx, |page, cx| page.set_show_all_files(show_all_files, cx));
        }
        let surfaces = self
            .files
            .values()
            .chain(self.file_surfaces.values())
            .cloned()
            .collect::<Vec<_>>();
        for surface in surfaces {
            surface.update(cx, |surface, cx| {
                surface.set_show_all_files(show_all_files, cx)
            });
        }
        self.schedule_save(cx);
        cx.notify();
    }

    fn session_links(
        source_session: Option<String>,
        cx: &Context<Self>,
    ) -> crate::markdown::render::LinkUi {
        let shell = cx.weak_entity();
        crate::markdown::render::LinkUi {
            source_session,
            handler: std::rc::Rc::new(move |activation, window, cx| {
                shell
                    .update(cx, |shell, cx| {
                        shell.activate_session_link(activation, window, cx)
                    })
                    .unwrap_or(crate::markdown::render::LinkOutcome::Rejected)
            }),
        }
    }

    fn activate_session_link(
        &mut self,
        activation: &crate::markdown::render::LinkActivation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> crate::markdown::render::LinkOutcome {
        use crate::markdown::render::{LinkAction, LinkOutcome};
        if self.active_chat.is_empty()
            || activation.source_session.as_deref() != Some(self.active_chat.as_str())
            || self.state.read(cx).selected_chat.as_deref() != Some(self.active_chat.as_str())
        {
            return LinkOutcome::Rejected;
        }
        if activation.target.navigation.is_err() {
            return if matches!(
                activation.action,
                LinkAction::Primary | LinkAction::Internal
            ) && self.open_workspace_file_link(&activation.target.original, window, cx)
            {
                LinkOutcome::Internal
            } else {
                LinkOutcome::Rejected
            };
        }
        let mut resolved = activation.clone();
        if resolved.action == LinkAction::Primary {
            resolved.action = if crate::settings::current(cx).open_web_links_in_luna {
                LinkAction::Internal
            } else {
                LinkAction::External
            };
        }
        let outcome = resolved.web_outcome(cfg!(any(target_os = "macos", target_os = "linux")));
        if outcome == LinkOutcome::Internal {
            if !self.right_pane_open(cx) {
                self.toggle_right_pane(cx);
            }
            self.add_browser_surface(activation.target.navigation.clone().ok(), window, cx);
        }
        outcome
    }

    /// Browser tabs are independent instances owned by the current session.
    fn add_browser_surface(
        &mut self,
        url: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.active_chat.is_empty() {
            return;
        }
        let key = self.panel_key(cx);
        let remote = {
            let state = self.state.read(cx);
            state.selected_chat_row().is_some_and(|chat| {
                Some(chat.device_id.as_str()) != state.local_device_id.as_deref()
            })
        };
        self.browser_seq += 1;
        let id = self.browser_seq;
        let browser = cx.new(|cx| {
            crate::browser::BrowserSurface::new(self.browser_context.clone(), remote, window, cx)
        });
        if let Some(handle) = self.state.read(cx).engine().cloned() {
            let chat_id = self.active_chat.clone();
            browser.update(cx, |browser, cx| {
                browser.watch_previews(handle, chat_id, cx)
            });
        }
        let owner = key.clone();
        let sub = cx.subscribe_in(&browser, window, move |this, _, event, window, cx| {
            match event {
                crate::browser::BrowserEvent::Changed => cx.notify(),
                crate::browser::BrowserEvent::NewTab(url) => {
                    // A background page cannot open a tab in the wrong session.
                    if this.panel_key(cx) == owner
                        && this.resolved_right_active(cx) == RightSurface::Browser(id)
                    {
                        this.add_browser_surface(url.clone(), window, cx);
                    }
                }
                crate::browser::BrowserEvent::Close => {
                    this.close_right_surface(RightSurface::Browser(id), window, cx)
                }
            }
        });
        self.browsers.insert(id, browser.clone());
        self.browser_subs.insert(id, sub);
        self.right_tabs
            .entry(key)
            .or_default()
            .push(RightSurface::Browser(id));
        self.set_right_active(RightSurface::Browser(id), cx);
        browser.update(cx, |browser, cx| {
            if let Some(url) = url {
                browser.navigate(&url, window, cx);
            } else {
                browser.focus_address(window, cx);
            }
        });
    }

    /// The picker's Diffs card / the `+` menu's Diff row: every click opens a
    /// FRESH diff tab with its own scope/base selection (multiple diff
    /// panels, user request).
    fn add_diff_surface(&mut self, cx: &mut Context<Self>) {
        let changes = cx.new(|cx| Changes::new(self.state.clone(), cx));
        self.register_diff_surface(changes, cx);
    }

    /// Files is single-instance per chat: both the picker and the `+` menu
    /// focus the existing surface instead of creating duplicate trees and
    /// duplicate workspace subscriptions.
    fn add_files_surface(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_chat.is_empty() {
            return;
        }
        let key = self.panel_key(cx);
        if !self.files.contains_key(&key) {
            let autosave_enabled = self.settings.files_autosave_enabled;
            let delay = self.settings.files_autosave_delay_ms;
            let editor_font_size = crate::typography::code_font_size(cx);
            let word_wrap = self.settings.files_word_wrap;
            let show_all_files = self.settings.files_show_all;
            let files = cx.new(|cx| {
                FilesSurface::new(
                    self.state.clone(),
                    self.active_chat.clone(),
                    autosave_enabled,
                    delay,
                    editor_font_size,
                    word_wrap,
                    show_all_files,
                    cx,
                )
            });
            let event_key = key.clone();
            let sub = cx.subscribe_in(
                &files,
                window,
                move |this: &mut Self, _, event, window, cx| match event {
                    FilesEvent::OpenFile(path) => this.add_file_surface(path.clone(), window, cx),
                    FilesEvent::OpenWebLink(activation) => {
                        if let crate::markdown::render::LinkOutcome::External(url) =
                            this.activate_session_link(activation, window, cx)
                        {
                            cx.open_url(&url);
                        }
                    }
                    FilesEvent::TitleChanged => cx.notify(),
                    FilesEvent::FileRenamed { .. } => cx.notify(),
                    FilesEvent::WordWrapChanged(word_wrap) => {
                        this.set_files_word_wrap(*word_wrap, window, cx)
                    }
                    FilesEvent::ShowAllFilesChanged(show_all_files) => {
                        this.set_files_show_all(*show_all_files, cx)
                    }
                    FilesEvent::CloseReady => {
                        this.on_file_close_ready(RightSurface::Files, &event_key, cx)
                    }
                    FilesEvent::CloseCancelled => this.cancel_file_close(RightSurface::Files, cx),
                },
            );
            self.files.insert(key.clone(), files);
            self.files_subs.insert(key.clone(), sub);
        }
        let tabs = self.right_tabs.entry(key).or_default();
        push_unique_right_surface(tabs, RightSurface::Files);
        self.set_right_active(RightSurface::Files, cx);
        self.focus_right_file_editor(RightSurface::Files, window, cx);
    }

    /// Open a workspace file as a first-class right-pane tab. Every editor is
    /// a separate FilesSurface so its tree, search, watcher and split layout
    /// stay stable while users move among open files.
    fn add_file_surface(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_chat.is_empty() {
            return;
        }
        let panel_key = self.panel_key(cx);
        let lookup = (panel_key.clone(), path.clone());
        if let Some(id) = self.file_surface_keys.get(&lookup).copied() {
            let surface = RightSurface::File(id);
            self.set_right_active(surface, cx);
            self.focus_right_file_editor(surface, window, cx);
            return;
        }

        self.file_surface_seq += 1;
        let id = self.file_surface_seq;
        let file = cx.new(|cx| {
            FilesSurface::new_editor(
                self.state.clone(),
                self.active_chat.clone(),
                path.clone(),
                self.settings.files_autosave_enabled,
                self.settings.files_autosave_delay_ms,
                crate::typography::code_font_size(cx),
                self.settings.files_word_wrap,
                self.settings.files_show_all,
                cx,
            )
        });
        let event_panel_key = panel_key.clone();
        let sub = cx.subscribe_in(
            &file,
            window,
            move |this: &mut Self, _, event, window, cx| match event {
                FilesEvent::OpenFile(path) => this.add_file_surface(path.clone(), window, cx),
                FilesEvent::OpenWebLink(activation) => {
                    if let crate::markdown::render::LinkOutcome::External(url) =
                        this.activate_session_link(activation, window, cx)
                    {
                        cx.open_url(&url);
                    }
                }
                FilesEvent::TitleChanged => cx.notify(),
                FilesEvent::FileRenamed { old_path, new_path } => {
                    this.rename_file_surface(id, &event_panel_key, old_path, new_path, cx)
                }
                FilesEvent::WordWrapChanged(word_wrap) => {
                    this.set_files_word_wrap(*word_wrap, window, cx)
                }
                FilesEvent::ShowAllFilesChanged(show_all_files) => {
                    this.set_files_show_all(*show_all_files, cx)
                }
                FilesEvent::CloseReady => {
                    this.on_file_close_ready(RightSurface::File(id), &event_panel_key, cx)
                }
                FilesEvent::CloseCancelled => this.cancel_file_close(RightSurface::File(id), cx),
            },
        );
        self.file_surfaces.insert(id, file);
        self.file_surface_paths.insert(id, path);
        self.file_surface_keys.insert(lookup, id);
        self.file_surface_subs.insert(id, sub);
        self.right_tabs
            .entry(panel_key)
            .or_default()
            .push(RightSurface::File(id));
        self.set_right_active(RightSurface::File(id), cx);
    }

    fn open_workspace_file_link(
        &mut self,
        target: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(chat) = self
            .state
            .read(cx)
            .chats
            .iter()
            .find(|chat| chat.id == self.active_chat)
        else {
            return false;
        };
        let Some(root) = chat.cwd.as_deref() else {
            return false;
        };
        let Some(link) = resolve_workspace_file_link(target, root) else {
            return false;
        };

        let key = self.panel_key(cx);
        let was_open = self.panels.get(&key).changes_open;
        let from = self.right_target(cx);
        self.panels.update(&key, |panel| panel.changes_open = true);
        if !was_open {
            self.right_tween = Some(WidthTween::new(from, self.right_target(cx)));
        }
        self.add_file_surface(link.path, window, cx);
        true
    }

    fn rename_file_surface(
        &mut self,
        id: u64,
        panel_key: &str,
        old_path: &str,
        new_path: &str,
        cx: &mut Context<Self>,
    ) {
        if self.file_surface_paths.get(&id).map(String::as_str) != Some(old_path) {
            return;
        }
        self.file_surface_paths.insert(id, new_path.to_string());
        self.file_surface_keys
            .remove(&(panel_key.to_string(), old_path.to_string()));
        self.file_surface_keys
            .entry((panel_key.to_string(), new_path.to_string()))
            .or_insert(id);
        cx.notify();
    }

    /// The dedicated History surface. Keeping it as its own tab preserves its
    /// graph/search state while Diff tabs retain their ordinary scope picker.
    fn add_history_surface(&mut self, cx: &mut Context<Self>) {
        let history = cx.new(|cx| Changes::for_history(self.state.clone(), cx));
        self.register_diff_surface(history, cx);
    }

    /// A History row click: the commit opens as its own pinned diff tab
    /// (user request).
    fn add_commit_diff_surface(
        &mut self,
        commit: luna_proto::GitHistoryCommit,
        cx: &mut Context<Self>,
    ) {
        let changes = cx.new(|cx| Changes::for_commit(self.state.clone(), commit, cx));
        self.register_diff_surface(changes, cx);
    }

    fn register_diff_surface(&mut self, changes: Entity<Changes>, cx: &mut Context<Self>) {
        self.diff_seq += 1;
        let id = self.diff_seq;
        let sub = cx.subscribe(&changes, |this: &mut Self, _, event, cx| match event {
            ChangesEvent::OpenCommit(commit) => {
                this.add_commit_diff_surface(commit.clone(), cx);
            }
        });
        self.diffs.insert(id, changes);
        self.diff_subs.insert(id, sub);
        let key = self.panel_key(cx);
        self.right_tabs
            .entry(key)
            .or_default()
            .push(RightSurface::Diff(id));
        self.set_right_active(RightSurface::Diff(id), cx);
    }

    /// The picker's Terminal card restores saved/live tabs for this task.
    fn add_terminal_surface(&mut self, cx: &mut Context<Self>) {
        self.terminal_picker_notice = None;
        let key = self.panel_key(cx);
        self.panels
            .update(&key, |panel| panel.right_active = RightSurface::Picker);
        let panel = self.right_terminal_panel(cx);
        panel.update(cx, |panel, cx| {
            panel.set_open(true, cx);
            panel.restore_tabs_for_selected(cx);
        });
    }

    /// The right-pane `+` menu explicitly starts a fresh managed shell.
    fn add_new_terminal_surface(&mut self, cx: &mut Context<Self>) {
        let panel = self.right_terminal_panel(cx);
        let opened = panel.update(cx, |panel, cx| {
            panel.set_open(true, cx);
            panel.open_tab_for_selected(cx)
        });
        if let Some(tab) = opened {
            self.terminal_picker_notice = None;
            let key = self.panel_key(cx);
            self.right_tabs
                .entry(key)
                .or_default()
                .push(RightSurface::Terminal(tab));
            self.set_right_active(RightSurface::Terminal(tab), cx);
        }
    }

    /// Spawn-chip events from the primary transcript AND from subagent-tab
    /// transcripts (nested spawns open their own tabs).
    fn on_transcript_event(
        &mut self,
        _: Entity<Transcript>,
        event: &TranscriptEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            TranscriptEvent::StartSideChat {
                task_id,
                message_id,
                text,
            } => {
                if self.active_chat == *task_id {
                    self.release_side_chat(cx);
                    self.add_work_surface(RightSurface::SideChats, cx);
                    self.create_side_chat_panel(Some((message_id.clone(), text.clone())), cx);
                }
            }
            TranscriptEvent::SideChatResult { .. } => {}
            TranscriptEvent::OpenSubagent {
                chat_id,
                doc_id,
                title,
                frozen,
            } => {
                self.add_subagent_surface(
                    chat_id.clone(),
                    doc_id.clone(),
                    title.clone(),
                    *frozen,
                    cx,
                );
            }
            TranscriptEvent::OpenAccounts => {
                self.open_settings(SettingsSection::Providers, cx);
            }
            TranscriptEvent::ForkMessage {
                task_id,
                message_id,
                boundary,
            } => self.open_history_fork(task_id.clone(), *boundary, Some(message_id.clone()), cx),
            TranscriptEvent::RewindConversation {
                task_id,
                message_id,
                text,
            } => {
                self.open_conversation_rewind(
                    task_id.clone(),
                    message_id.clone(),
                    text.clone(),
                    cx,
                );
            }
            TranscriptEvent::EditEarlierMessage {
                task_id,
                message_id,
                text,
            } => self.open_edit_earlier_message(
                task_id.clone(),
                message_id.clone(),
                text.clone(),
                cx,
            ),
        }
    }

    /// A spawn chip's "Open subagent": focus the existing tab for that doc,
    /// or open one. `frozen` (subagent done/failed) tries the uploaded
    /// transcript blob first and falls back to the live doc watch; running
    /// subagents watch the doc directly.
    fn add_subagent_surface(
        &mut self,
        chat_id: String,
        doc_id: String,
        title: String,
        frozen: bool,
        cx: &mut Context<Self>,
    ) {
        // The chip lives in the conversation column — the pane it opens into
        // may still be closed.
        if !self.right_pane_open(cx) {
            self.toggle_right_pane(cx);
        }
        if let Some((&id, _)) = self
            .subagent_tabs
            .iter()
            .find(|(_, tab)| tab.doc_id == doc_id)
        {
            self.set_right_active(RightSurface::Subagent(id), cx);
            return;
        }
        self.subagent_seq += 1;
        let id = self.subagent_seq;
        // A live subagent follows its streaming end (main-transcript feel);
        // a frozen one reads top-down.
        let transcript =
            cx.new(|cx| Transcript::for_doc(self.state.clone(), doc_id.clone(), !frozen, cx));
        let links = Self::session_links(Some(self.active_chat.clone()), cx);
        transcript.update(cx, |transcript, _| {
            transcript.set_workspace_link_handler(links)
        });
        let events = cx.subscribe(&transcript, Self::on_transcript_event);
        let fetch = if frozen {
            self.spawn_subagent_snapshot_fetch(&chat_id, &doc_id, cx)
        } else {
            self.state
                .update(cx, |s, cx| s.watch_subagent_doc(doc_id.clone(), cx));
            None
        };
        self.subagent_tabs.insert(
            id,
            SubagentTab {
                doc_id,
                chat_id,
                frozen,
                paused: false,
                title: title.into(),
                transcript,
                _fetch: fetch,
                _events: events,
            },
        );
        let key = self.panel_key(cx);
        self.right_tabs
            .entry(key)
            .or_default()
            .push(RightSurface::Subagent(id));
        self.set_right_active(RightSurface::Subagent(id), cx);
    }

    fn suspend_subagent_watches(&mut self, except: Option<RightSurface>, cx: &mut Context<Self>) {
        for (id, tab) in &mut self.subagent_tabs {
            if except == Some(RightSurface::Subagent(*id)) || tab.paused {
                continue;
            }
            tab._fetch = None;
            tab.paused = true;
            self.state.update(cx, |state, cx| {
                state.unwatch_subagent_doc(&tab.doc_id);
                cx.notify();
            });
        }
    }

    fn resume_subagent_watch(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(tab) = self.subagent_tabs.get(&id).filter(|tab| tab.paused) else {
            return;
        };
        let (chat, doc, frozen) = (tab.chat_id.clone(), tab.doc_id.clone(), tab.frozen);
        let fetch = if frozen {
            self.spawn_subagent_snapshot_fetch(&chat, &doc, cx)
        } else {
            self.state
                .update(cx, |state, cx| state.watch_subagent_doc(doc, cx));
            None
        };
        if let Some(tab) = self.subagent_tabs.get_mut(&id) {
            tab._fetch = fetch;
            tab.paused = false;
        }
    }

    /// Fetch a finished subagent's frozen transcript blob
    /// (`{chat_id}/{doc_id}`); on ANY failure fall back to watching the doc
    /// — the blob upload is best-effort engine-side.
    fn spawn_subagent_snapshot_fetch(
        &self,
        chat_id: &str,
        doc_id: &str,
        cx: &mut Context<Self>,
    ) -> Option<Task<()>> {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.state
                .update(cx, |s, cx| s.watch_subagent_doc(doc_id.to_string(), cx));
            return None;
        };
        let blob_ref = format!("{chat_id}/{doc_id}");
        let state = self.state.clone();
        let doc_id = doc_id.to_string();
        Some(cx.spawn(async move |_, cx| {
            let reply = crate::attachments::call_with_timeout(
                &engine,
                cx.background_executor(),
                methods::FETCH_TOOL_BLOB,
                serde_json::json!({ "blobRef": blob_ref }),
                Duration::from_secs(20),
            )
            .await;
            let entries: Option<Vec<luna_doc::SessionMessageEntry>> = reply.ok().and_then(|v| {
                let text = v.get("text")?.as_str()?.to_owned();
                serde_json::from_str(&text).ok()
            });
            state.update(cx, |s, cx| {
                match entries {
                    Some(entries) => s.set_subagent_snapshot(doc_id, entries),
                    None => s.watch_subagent_doc(doc_id, cx),
                }
                cx.notify();
            });
        }))
    }

    /// A surface tab's ✕. The active fallback happens naturally through
    /// [`Self::resolved_right_active`] on the next frame.
    fn close_right_surface(
        &mut self,
        surface: RightSurface,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let was_active = self.resolved_right_active(cx) == surface;
        let key = self.panel_key(cx);
        let files = match surface {
            RightSurface::Files => self.files.get(&key).cloned(),
            RightSurface::File(id) => self.file_surfaces.get(&id).cloned(),
            _ => None,
        };
        if let Some(files) = files {
            match files.update(cx, |files, cx| files.prepare_close(cx)) {
                FilesCloseDisposition::Allow => {
                    self.complete_file_close(surface, &key, cx);
                }
                FilesCloseDisposition::Pending | FilesCloseDisposition::Blocked => {
                    self.pending_file_closes.insert(surface);
                    self.set_right_active(surface, cx);
                }
            }
            return;
        }
        if let Some(tabs) = self.right_tabs.get_mut(&key) {
            tabs.retain(|s| *s != surface);
        }
        match surface {
            RightSurface::Files | RightSurface::File(_) => {}
            RightSurface::Browser(id) => {
                if let Some(browser) = self.browsers.remove(&id) {
                    browser.update(cx, |browser, cx| browser.close(cx));
                }
                self.browser_subs.remove(&id);
                if was_active {
                    window.focus(&self.composer.focus_handle(cx), cx);
                }
            }
            RightSurface::Diff(id) => {
                // Dropping the entity tears down its diff watch.
                self.diffs.remove(&id);
                self.diff_subs.remove(&id);
            }
            RightSurface::Terminal(tab) => {
                let panel = self.right_terminal_panel(cx);
                panel.update(cx, |panel, cx| panel.close_tab_by_key(tab, window, cx));
            }
            RightSurface::Subagent(id) => {
                // Unwatch drops the watch task — that cancels the engine-side
                // watch and unpins the subagent doc from the engine LRU.
                if let Some(tab) = self.subagent_tabs.remove(&id) {
                    self.state
                        .update(cx, |s, _| s.unwatch_subagent_doc(&tab.doc_id));
                }
            }
            RightSurface::SideChats => self.release_side_chat(cx),
            RightSurface::Agents | RightSurface::Picker => {}
        }
        self.panels.update(&key, |p| {
            if p.right_active == surface {
                p.right_active = RightSurface::Picker;
            }
        });
        cx.notify();
    }

    fn on_file_close_ready(
        &mut self,
        surface: RightSurface,
        panel_key: &str,
        cx: &mut Context<Self>,
    ) {
        if self.pending_file_closes.contains(&surface) {
            self.complete_file_close(surface, panel_key, cx);
        } else {
            if self.pending_exit.is_some() {
                self.reveal_unsaved_file(cx);
            }
            cx.notify();
        }
    }

    fn cancel_file_close(&mut self, surface: RightSurface, cx: &mut Context<Self>) {
        self.pending_file_closes.remove(&surface);
        self.pending_exit = None;
        cx.notify();
    }

    pub fn prepare_window_close(&mut self, cx: &mut Context<Self>) -> bool {
        self.prepare_exit(PendingExit::CloseWindow, cx)
    }

    /// The first rung of `⌘W` / Window > Close Window: when the right pane is
    /// open on a real surface (the file / diff / terminal / browser tab the
    /// user just opened), close THAT and leave the window alone. Returns true
    /// when the close was consumed by the pane.
    ///
    /// The pane's empty picker state and a closed pane both yield false, so the
    /// caller falls through to [`Self::prepare_window_close`] and the window
    /// closes — the same cascade browsers use. The native traffic-light close
    /// deliberately skips this rung: it always closes the window.
    pub fn close_active_surface(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(surface) = self.closable_right_surface(cx) else {
            return false;
        };
        self.close_right_surface(surface, window, cx);
        true
    }

    /// The right pane's closable surface for the `⌘W` cascade: the resolved
    /// active surface while the pane is open, or `None` on the picker empty
    /// state / a closed pane / the new-session canvas.
    fn closable_right_surface(&self, cx: &App) -> Option<RightSurface> {
        if !self.right_pane_open(cx) {
            return None;
        }
        match self.resolved_right_active(cx) {
            RightSurface::Picker => None,
            surface => Some(surface),
        }
    }

    pub fn prepare_quit(&mut self, cx: &mut Context<Self>) -> bool {
        self.prepare_exit(PendingExit::Quit, cx)
    }

    fn prepare_exit(&mut self, action: PendingExit, cx: &mut Context<Self>) -> bool {
        let _ = self.keep_awake.reconcile(None);
        let composer_ready = self
            .composer
            .update(cx, |composer, cx| composer.prepare_close(cx));
        let surfaces = self
            .files
            .values()
            .chain(self.file_surfaces.values())
            .cloned()
            .collect::<Vec<_>>();
        if composer_ready
            && surfaces
                .iter()
                .all(|surface| !surface.read(cx).has_unsaved_changes())
        {
            self.pending_exit = None;
            return true;
        }
        self.pending_exit = Some(action);
        let mut all_ready = composer_ready;
        for surface in surfaces {
            let disposition = surface.update(cx, |surface, cx| surface.prepare_close(cx));
            all_ready &= disposition == FilesCloseDisposition::Allow;
        }
        if all_ready {
            self.pending_exit = None;
        } else {
            self.reveal_unsaved_file(cx);
        }
        cx.notify();
        all_ready
    }

    fn reveal_unsaved_file(&mut self, cx: &mut Context<Self>) {
        let browser = self.files.iter().filter_map(|(key, files)| {
            files
                .read(cx)
                .has_unsaved_changes()
                .then(|| (key.clone(), RightSurface::Files))
        });
        let editors = self.file_surface_keys.iter().filter_map(|((key, _), id)| {
            self.file_surfaces
                .get(id)
                .filter(|files| files.read(cx).has_unsaved_changes())
                .map(|_| (key.clone(), RightSurface::File(*id)))
        });
        let current = self.panel_key(cx);
        let mut dirty = browser.chain(editors).collect::<Vec<_>>();
        dirty.sort_by_key(|(key, _)| (key != &current, key.clone()));
        if let Some((key, surface)) = dirty.into_iter().next() {
            self.panels.update(&key, |panel| {
                panel.changes_open = true;
                panel.right_active = surface;
            });
            self.apply_nav(NavEntry::Chat(key), cx);
        }
    }

    fn all_file_edits_flushed(&self, cx: &App) -> bool {
        self.files
            .values()
            .chain(self.file_surfaces.values())
            .all(|surface| !surface.read(cx).has_unsaved_changes())
    }

    fn complete_file_close(
        &mut self,
        surface: RightSurface,
        panel_key: &str,
        cx: &mut Context<Self>,
    ) {
        if let Some(tabs) = self.right_tabs.get_mut(panel_key) {
            tabs.retain(|candidate| *candidate != surface);
        }
        match surface {
            RightSurface::Files => {
                self.files.remove(panel_key);
                self.files_subs.remove(panel_key);
            }
            RightSurface::File(id) => {
                self.file_surfaces.remove(&id);
                self.file_surface_paths.remove(&id);
                self.file_surface_subs.remove(&id);
                self.file_surface_keys.retain(|_, value| *value != id);
            }
            _ => return,
        }
        self.pending_file_closes.remove(&surface);
        self.panels.update(panel_key, |panel| {
            if panel.right_active == surface {
                panel.right_active = RightSurface::Picker;
            }
        });
        cx.notify();
    }

    fn terminal_panel(&mut self, cx: &mut Context<Self>) -> Entity<TerminalPanel> {
        if let Some(terminal) = &self.terminal {
            return terminal.clone();
        }
        let terminal = cx.new(|cx| TerminalPanel::new(self.state.clone(), cx));
        self.terminal = Some(terminal.clone());
        terminal
    }

    fn terminal_target(&self, cx: &App) -> f32 {
        if self.terminal_open(cx) {
            self.settings.terminal_height
        } else {
            0.0
        }
    }

    /// Cmd/Ctrl+J and the header button (feature-inventory §1.10). Height
    /// animates 200 ms; closing detaches (PTYs stay alive), opening restores.
    /// The flag is per chat (luna `sessionPanels`).
    fn toggle_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let from = self.terminal_target(cx);
        let key = self.panel_key(cx);
        let open = self.panels.toggle_terminal(&key);
        self.terminal_tween = Some(WidthTween::new(from, self.terminal_target(cx)));
        let panel = self.terminal_panel(cx);
        panel.update(cx, |panel, cx| panel.set_open(open, cx));
        if open {
            self.composer
                .update(cx, |composer, _| composer.focus_pending = false);
            panel.update(cx, |panel, cx| panel.request_focus(cx));
            // Opening lands keyboard focus IN the shell — typing goes straight
            // to the prompt, no click needed (luna terminal-panel.tsx: the
            // visible+active effect calls `terminal.focus()` on every open).
            // The handle is focusable before the panel's first paint; once the
            // terminal body mounts with `track_focus` it receives the keys.
            window.focus(&panel.read(cx).focus_handle(), cx);
        } else {
            // Hiding the panel removes the (likely focused) terminal view;
            // with nothing focused, window key bindings stop dispatching, so
            // hand focus to the composer. (Cmd+J is a pure toggle — a second
            // press closes even while the terminal is focused, as in luna's
            // `useHotkey(toggleShortcut, ... setOpenScoped(!open))`.)
            window.focus(&self.composer.focus_handle(cx), cx);
        }
        self.terminal_tween_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(RESIZE.total().mul_f32(motion::speed_scale()) + Duration::from_millis(30))
                .await;
            this.update(cx, |shell, cx| {
                shell.terminal_tween = None;
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn on_terminal_drag(
        &mut self,
        event: &gpui::DragMoveEvent<TerminalResize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((anchor_y, anchor_h)) = self.terminal_drag_anchor else {
            return;
        };
        let dy = anchor_y - f32::from(event.event.position.y);
        let viewport_h = f32::from(window.viewport_size().height);
        let requested = anchor_h + dy;
        let max = (viewport_h * TERMINAL_MAX_VH).max(TERMINAL_MIN_HEIGHT);
        self.settings.terminal_height = clamp_terminal_height(requested, viewport_h);
        self.pane_resize_dragging = Some(PaneResizeKind::Terminal);
        self.pane_resize_active = (requested > TERMINAL_MIN_HEIGHT && requested < max)
            .then_some(PaneResizeKind::Terminal);
        self.terminal_tween = None; // live drag tracks the pointer
        self.schedule_save(cx);
        cx.notify();
    }

    fn on_sidebar_drag(
        &mut self,
        event: &gpui::DragMoveEvent<SidebarResize>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let x = f32::from(event.event.position.x);
        let sample = sidebar_drag_sample(x, self.sidebar_resize_edge, self.reduced_motion);
        self.settings.sidebar_width = sample.width;
        self.settings.sidebar_collapsed = false;
        self.pane_resize_dragging = Some(PaneResizeKind::Sidebar);
        self.sidebar_tween = None; // live drag tracks the pointer directly
        if sample.starts_bounce {
            self.sidebar_edge_bounce = sample.edge.map(motion::ResizeEdgeBounce::new);
        } else if sample.edge.is_none() {
            self.sidebar_edge_bounce = None;
        }
        self.pane_resize_active = sample.edge.is_none().then_some(PaneResizeKind::Sidebar);
        self.sidebar_resize_edge = sample.edge;
        self.schedule_save(cx);
        cx.notify();
    }

    fn finish_pane_resize(&mut self, kind: PaneResizeKind) {
        if self.pane_resize_active == Some(kind) {
            self.pane_resize_active = None;
        }
        if self.pane_resize_dragging == Some(kind) {
            self.pane_resize_dragging = None;
        }
        match kind {
            PaneResizeKind::Sidebar => self.sidebar_resize_edge = None,
            PaneResizeKind::Terminal => self.terminal_drag_anchor = None,
            PaneResizeKind::Right => self.right_resize_edge = None,
        }
    }

    fn on_right_pane_drag(
        &mut self,
        event: &gpui::DragMoveEvent<RightPaneResize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let viewport = f32::from(window.viewport_size().width);
        let width = viewport - f32::from(event.event.position.x);
        // No arbitrary percentage ceiling, but retain the chat's usable 300px
        // floor instead of allowing the conversation to collapse to zero.
        let max = right_pane_max_width(viewport, self.sidebar_target());
        let sample = if max >= RIGHT_PANE_MIN {
            motion::resize_drag_sample(
                width,
                RIGHT_PANE_MIN,
                max,
                self.right_resize_edge,
                self.reduced_motion,
            )
        } else {
            motion::ResizeDragSample {
                width: max,
                edge: None,
                starts_bounce: false,
            }
        };
        self.settings.right_pane_width = sample.width;
        self.pane_resize_dragging = Some(PaneResizeKind::Right);
        if sample.starts_bounce {
            self.right_edge_bounce = sample.edge.map(motion::ResizeEdgeBounce::new);
        } else if sample.edge.is_none() {
            self.right_edge_bounce = None;
        }
        self.pane_resize_active = sample.edge.is_none().then_some(PaneResizeKind::Right);
        self.right_resize_edge = sample.edge;
        self.right_tween = None;
        self.right_takeover_content_tween = None;
        self.main_takeover_tween = None;
        self.schedule_save(cx);
        cx.notify();
    }

    /// Publish this view's working copy to the central settings store. The
    /// store owns the single debounce task and the only production writer.
    fn schedule_save(&mut self, cx: &mut Context<Self>) {
        self.settings.appearance = crate::appearance::mode(cx);
        self.settings.git_history_columns = crate::history::configured_columns(cx);
        self.settings.git_history_column_widths = crate::history::configured_column_widths(cx);
        self.settings.git_history_column_order = crate::history::configured_column_order(cx);
        self.settings.git_history_author_display = crate::history::configured_author_display(cx);
        self.settings.theme_selection = crate::appearance::themes(cx);
        self.settings.accent = crate::appearance::accent(cx);
        self.settings.surface = crate::appearance::surface(cx);
        self.sync_independent_settings(cx);
        settings::replace(self.settings.clone(), SavePolicy::Debounced, cx);
    }

    /// Controls outside the Shell mutate these choices directly. A geometry
    /// save must never publish the Shell's older values over those selections.
    /// The typography globals own the font choices but persist every change
    /// immediately, so the central store is an equally canonical read and
    /// keeps this block on a single source.
    fn sync_independent_settings(&mut self, cx: &App) {
        let current = settings::current(cx);
        self.settings.window_geometry = current.window_geometry;
        self.settings.new_thread_composer_background = current.new_thread_composer_background;
        self.settings.new_thread_background_effect = current.new_thread_background_effect;
        self.settings.open_web_links_in_luna = current.open_web_links_in_luna;
        self.settings.ui_font_family = current.ui_font_family;
        self.settings.ui_font_size = current.ui_font_size;
        self.settings.terminal_font_family = current.terminal_font_family;
        self.settings.terminal_font_size = current.terminal_font_size;
        self.settings.code_font_family = current.code_font_family;
        self.settings.code_font_size = current.code_font_size;
        self.settings.transcript_width = current.transcript_width;
        self.settings.code_fences_fit_content = current.code_fences_fit_content;
        self.settings.diff_split = current.diff_split;
        self.settings.diff_wrap = current.diff_wrap;
        self.settings.diff_color_scheme = current.diff_color_scheme;
    }

    fn retry_engine(&mut self, cx: &mut Context<Self>) {
        AppState::bootstrap(self.state.clone(), self.boot.clone(), cx);
    }

    // ---- routes / settings ----

    /// Close the session-row context menu through the exit animation.
    fn close_chat_menu(&mut self, cx: &mut Context<Self>) {
        if self.chat_menu.begin_close() {
            popover::reap_popup(cx, |shell: &mut Self| &mut shell.chat_menu);
            cx.notify();
        }
    }

    fn open_chat_copy_menu(&mut self, cx: &mut Context<Self>) {
        if let Some(menu) = self.chat_menu.open_mut() {
            menu.page = ChatMenuPage::Copy;
            cx.notify();
        }
    }

    fn copy_luna_conversation_link(&mut self, chat_id: &str, cx: &mut Context<Self>) {
        let link = {
            let state = self.state.read(cx);
            crate::links::workspace_locator(
                state.workspace_scope,
                state.auth.as_ref(),
                state.local_device_id.as_deref(),
            )
            .map(|workspace| crate::links::luna_conversation_link(chat_id, &workspace))
        };
        if let Some(link) = link {
            cx.write_to_clipboard(ClipboardItem::new_string(link));
            self.sidebar_notice = Some(SidebarNotice::transient_status("LUNA task link copied"));
        } else {
            self.sidebar_notice = Some("Conversation link is not ready yet".into());
        }
        self.close_chat_menu(cx);
        cx.notify();
    }

    fn copy_harness_conversation_link(&mut self, chat_id: &str, cx: &mut Context<Self>) {
        let link = self
            .state
            .read(cx)
            .chats
            .iter()
            .find(|chat| chat.id == chat_id)
            .and_then(crate::links::harness_conversation_link);
        if let Some(link) = link {
            cx.write_to_clipboard(ClipboardItem::new_string(link.url));
            self.sidebar_notice = Some(SidebarNotice::transient_status(format!(
                "{} copied",
                link.label
            )));
        }
        self.close_chat_menu(cx);
        cx.notify();
    }

    fn copy_harness_session_id(&mut self, chat_id: &str, cx: &mut Context<Self>) {
        let id = self
            .state
            .read(cx)
            .chats
            .iter()
            .find(|chat| chat.id == chat_id)
            .and_then(|chat| chat.harness_session_id.clone());
        if let Some(id) = id.filter(|id| !id.trim().is_empty()) {
            cx.write_to_clipboard(ClipboardItem::new_string(id));
            self.sidebar_notice = Some(SidebarNotice::transient_status(
                "Provider session ID copied",
            ));
        }
        self.close_chat_menu(cx);
        cx.notify();
    }

    fn open_settings(&mut self, section: SettingsSection, cx: &mut Context<Self>) {
        self.dismiss_command_palette(cx);
        if section == SettingsSection::Providers
            && let Some(page) = &self.accounts_page
        {
            page.update(cx, |page, cx| page.refresh_on_visit(cx));
        }
        if section == SettingsSection::Backups {
            self.backups_page = None;
        }
        self.route = Route::Settings(section);
        if section == SettingsSection::Usage {
            self.usage_card_open = false;
            self.refresh_usage(cx);
        }
        self.nav.push(NavEntry::Settings(section));
        self.close_chat_menu(cx);
        cx.notify();
    }

    fn close_settings(&mut self, cx: &mut Context<Self>) {
        self.route = Route::Chat;
        self.focus_composer(cx);
        self.nav.push(NavEntry::Chat(self.active_chat.clone()));
        cx.notify();
    }

    // ---- back/forward (route history) ----

    fn navigate_back(&mut self, cx: &mut Context<Self>) {
        if let Some(entry) = self.nav.back() {
            self.apply_nav(entry, cx);
        }
    }

    fn navigate_forward(&mut self, cx: &mut Context<Self>) {
        if let Some(entry) = self.nav.forward() {
            self.apply_nav(entry, cx);
        }
    }

    /// Land on a history entry WITHOUT recording a new one: the stack already
    /// points at `entry` (back/forward moved the index); the selection change
    /// this triggers dedups against `current()` in [`Self::on_state_changed`].
    fn apply_nav(&mut self, entry: NavEntry, cx: &mut Context<Self>) {
        self.suspend_file_images(cx);
        match entry {
            NavEntry::Chat(chat_id) => {
                self.route = Route::Chat;
                self.focus_composer(cx);
                let target = (!chat_id.is_empty()).then_some(chat_id);
                if self.state.read(cx).selected_chat != target {
                    self.state.update(cx, |s, cx| s.select_chat(target, cx));
                }
            }
            NavEntry::Settings(section) => {
                self.route = Route::Settings(section);
            }
        }
        self.close_chat_menu(cx);
        cx.notify();
    }

    /// Lazily create the entity for a settings section and return it renderable.
    fn settings_outlet(
        &mut self,
        section: SettingsSection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match section {
            SettingsSection::General => {
                if self.general_page.is_none() {
                    let preferences = self.settings.clone();
                    let page = cx.new(|cx| GeneralPage::new(&preferences, cx));
                    self.general_sub = Some(cx.subscribe(
                        &page,
                        |this: &mut Shell, _, event: &GeneralEvent, cx| {
                            match event {
                                GeneralEvent::ComposerSendBehaviorChanged(behavior) => {
                                    this.settings.composer_send_behavior = *behavior;
                                    apply_keymap(cx, &this.settings.keymap, *behavior);
                                    if let Some(page) = &this.shortcuts_page {
                                        page.update(cx, |page, cx| {
                                            page.sync_composer_send_behavior(*behavior, cx)
                                        });
                                    }
                                }
                                GeneralEvent::FollowUpBehaviorChanged(behavior) => {
                                    this.settings.follow_up_behavior = *behavior;
                                }
                                GeneralEvent::AutomaticRetriesChanged(enabled) => {
                                    this.settings.automatic_retries_enabled = *enabled;
                                }
                                GeneralEvent::ShowContextUsageChanged(enabled) => {
                                    this.settings.show_context_usage = *enabled;
                                    cx.refresh_windows();
                                }
                                GeneralEvent::OpenWebLinksInLunaChanged(enabled) => {
                                    // Transcript menus write the global store directly; keep
                                    // that source current before Shell republishes its copy.
                                    settings::update(SavePolicy::Debounced, cx, |settings| {
                                        settings.open_web_links_in_luna = *enabled;
                                    });
                                    this.settings.open_web_links_in_luna = *enabled;
                                }
                                GeneralEvent::KeepAwakeWhileRunningChanged(enabled) => {
                                    this.settings.keep_awake_while_running = *enabled;
                                    this.reconcile_keep_awake(cx);
                                }
                                GeneralEvent::TaskConfirmationChanged(action, enabled) => {
                                    this.settings.set_confirmation(*action, *enabled);
                                }
                            }
                            this.schedule_save(cx);
                            if matches!(event, GeneralEvent::AutomaticRetriesChanged(_)) {
                                // The engine reads this flag from the settings file when a
                                // failure arrives, so the change must not wait for debounce.
                                settings::flush(cx);
                            }
                            cx.notify();
                        },
                    ));
                    self.general_page = Some(page);
                }
                self.settings.open_web_links_in_luna = settings::current(cx).open_web_links_in_luna;
                if let Some(page) = &self.general_page {
                    page.update(cx, |page, cx| page.sync(&self.settings, cx));
                }
                self.general_page
                    .as_ref()
                    .unwrap()
                    .clone()
                    .into_any_element()
            }
            SettingsSection::Devices => {
                if self.devices_page.is_none() {
                    let state = self.state.clone();
                    self.devices_page = Some(cx.new(|cx| DevicesPage::new(state, cx)));
                }
                match &self.devices_page {
                    Some(page) => page.clone().into_any_element(),
                    None => Empty.into_any_element(),
                }
            }
            SettingsSection::Projects => {
                if self.projects_page.is_none() {
                    let state = self.state.clone();
                    self.projects_page = Some(cx.new(|cx| ProjectsPage::new(state, cx)));
                }
                match &self.projects_page {
                    Some(page) => page.clone().into_any_element(),
                    None => Empty.into_any_element(),
                }
            }
            SettingsSection::Providers => {
                if self.accounts_page.is_none() {
                    let state = self.state.clone();
                    self.accounts_page = Some(cx.new(|cx| AccountsPage::new(state, cx)));
                }
                match &self.accounts_page {
                    Some(page) => page.clone().into_any_element(),
                    None => Empty.into_any_element(),
                }
            }
            SettingsSection::Appearance => {
                if self.appearance_page.is_none() {
                    let page = cx.new(AppearancePage::new);
                    self.appearance_settings_sub = Some(cx.subscribe_in(
                        &page,
                        window,
                        |this: &mut Shell, _, event: &AppearanceSettingsEvent, window, cx| {
                            match *event {
                                AppearanceSettingsEvent::CodeFontSizeChanged(size) => {
                                    this.set_code_font_size(size, cx);
                                }
                                AppearanceSettingsEvent::FileWordWrapChanged(wrap) => {
                                    this.set_files_word_wrap(wrap, window, cx);
                                }
                                AppearanceSettingsEvent::DiffPreferencesChanged { split, wrap } => {
                                    for diff in this.diffs.values() {
                                        diff.update(cx, |diff, cx| {
                                            diff.apply_appearance_preferences(split, wrap, cx)
                                        });
                                    }
                                    cx.notify();
                                }
                            }
                        },
                    ));
                    self.appearance_page = Some(page);
                }
                match &self.appearance_page {
                    Some(page) => page.clone().into_any_element(),
                    None => Empty.into_any_element(),
                }
            }
            SettingsSection::Files => {
                if self.files_settings_page.is_none() {
                    let page = cx.new(|cx| {
                        FilesSettingsPage::new(
                            self.settings.files_autosave_enabled,
                            self.settings.files_autosave_delay_ms,
                            self.settings.files_word_wrap,
                            self.settings.files_show_all,
                            cx,
                        )
                    });
                    self.files_settings_sub = Some(cx.subscribe_in(
                        &page,
                        window,
                        |this: &mut Shell, _, event: &FilesSettingsEvent, window, cx| match *event {
                            FilesSettingsEvent::AutosaveChanged(autosave_enabled) => {
                                this.settings.files_autosave_enabled = autosave_enabled;
                                for surface in
                                    this.files.values().chain(this.file_surfaces.values())
                                {
                                    surface.update(cx, |surface, cx| {
                                        surface.set_autosave_enabled(autosave_enabled, cx)
                                    });
                                }
                                this.schedule_save(cx);
                                cx.notify();
                            }
                            FilesSettingsEvent::AutosaveDelayChanged(autosave_delay_ms) => {
                                this.settings.files_autosave_delay_ms = autosave_delay_ms;
                                for surface in
                                    this.files.values().chain(this.file_surfaces.values())
                                {
                                    surface.update(cx, |surface, cx| {
                                        surface.set_autosave_delay_ms(autosave_delay_ms, cx)
                                    });
                                }
                                this.schedule_save(cx);
                                cx.notify();
                            }
                            FilesSettingsEvent::WordWrapChanged(word_wrap) => {
                                this.set_files_word_wrap(word_wrap, window, cx);
                            }
                            FilesSettingsEvent::ShowAllFilesChanged(show_all_files) => {
                                this.set_files_show_all(show_all_files, cx);
                            }
                        },
                    ));
                    self.files_settings_page = Some(page);
                }
                match &self.files_settings_page {
                    Some(page) => page.clone().into_any_element(),
                    None => Empty.into_any_element(),
                }
            }
            SettingsSection::Backups => {
                if self.backups_page.is_none() {
                    let state = self.state.clone();
                    self.backups_page = Some(cx.new(|cx| BackupsPage::new(state, cx)));
                }
                match &self.backups_page {
                    Some(page) => page.clone().into_any_element(),
                    None => Empty.into_any_element(),
                }
            }
            SettingsSection::Notifications => {
                let notification_tasks = self.notification_tasks(cx);
                if self.notifications_page.is_none() {
                    let task_rows = notification_tasks.clone();
                    let preferences = NotificationPreferences::from(&self.settings);
                    let page = cx.new(|cx| NotificationsPage::new(preferences, task_rows, cx));
                    // Persist the flags whenever the page flips one.
                    self.notifications_sub = Some(cx.subscribe(
                        &page,
                        |this: &mut Shell, _, event: &NotificationsEvent, cx| {
                            match event {
                                NotificationsEvent::Changed {
                                    sound,
                                    completion_sound,
                                    input_sound,
                                    attention_sound,
                                    desktop,
                                    background_only,
                                } => {
                                    this.settings.sound_enabled = *sound;
                                    this.settings.sound_completion_enabled = *completion_sound;
                                    this.settings.sound_input_enabled = *input_sound;
                                    this.settings.sound_attention_enabled = *attention_sound;
                                    this.settings.notifications_enabled = *desktop;
                                    this.settings.notifications_background_only = *background_only;
                                }
                                NotificationsEvent::TaskPolicyChanged { task_id, policy } => {
                                    this.settings
                                        .task_notification_policies
                                        .insert(task_id.clone(), *policy);
                                }
                                NotificationsEvent::KeepAwakeChanged { task_id, enabled } => {
                                    if *enabled {
                                        this.settings.keep_awake_task_ids.insert(task_id.clone());
                                    } else {
                                        this.settings.keep_awake_task_ids.remove(task_id);
                                    }
                                    this.reconcile_keep_awake(cx);
                                }
                            }
                            this.schedule_save(cx);
                            cx.notify();
                        },
                    ));
                    self.notifications_page = Some(page);
                }
                match &self.notifications_page {
                    Some(page) => {
                        page.update(cx, |page, cx| page.set_tasks(notification_tasks, cx));
                        page.clone().into_any_element()
                    }
                    None => Empty.into_any_element(),
                }
            }
            SettingsSection::Shortcuts | SettingsSection::Appshots => {
                if self.shortcuts_page.is_none() {
                    let state = self.state.clone();
                    let keymap = self.settings.keymap.clone();
                    let escape_stops_active_agent = self.settings.escape_stops_active_agent;
                    let composer_send_behavior = self.settings.composer_send_behavior;
                    let appshots_enabled = self.settings.appshots_enabled;
                    let appshot_sound_enabled = self.settings.appshot_sound_enabled;
                    let appshot_destination = self.settings.appshot_destination;
                    let page = cx.new(|cx| {
                        ShortcutsPage::new(
                            state,
                            keymap,
                            escape_stops_active_agent,
                            composer_send_behavior,
                            appshots_enabled,
                            appshot_sound_enabled,
                            appshot_destination,
                            cx,
                        )
                    });
                    // Persist + re-apply shortcut preferences whenever the page changes them.
                    self.shortcuts_sub = Some(cx.subscribe(
                        &page,
                        |this: &mut Shell, _, event: &ShortcutsEvent, cx| {
                            match event {
                                ShortcutsEvent::KeymapChanged(keymap) => {
                                    this.settings.keymap = keymap.clone();
                                }
                                ShortcutsEvent::EscapeStopsActiveAgentChanged(enabled) => {
                                    this.settings.escape_stops_active_agent = *enabled;
                                }
                                ShortcutsEvent::ComposerSendBehaviorChanged(behavior) => {
                                    this.settings.composer_send_behavior = *behavior;
                                    if let Some(page) = &this.general_page {
                                        page.update(cx, |page, cx| page.sync(&this.settings, cx));
                                    }
                                }
                                ShortcutsEvent::AppshotsChanged {
                                    enabled,
                                    sound_enabled,
                                    destination,
                                } => {
                                    this.settings.appshots_enabled = *enabled;
                                    this.settings.appshot_sound_enabled = *sound_enabled;
                                    crate::appshots::set_capture_sound_enabled(*sound_enabled);
                                    this.settings.appshot_destination = *destination;
                                    crate::appshots::set_enabled(*enabled);
                                }
                            }
                            apply_keymap(
                                cx,
                                &this.settings.keymap,
                                this.settings.composer_send_behavior,
                            );
                            this.schedule_save(cx);
                            cx.notify();
                        },
                    ));
                    self.shortcuts_page = Some(page);
                }
                match &self.shortcuts_page {
                    Some(page) => {
                        page.update(cx, |page, _| {
                            page.show_appshots(section == SettingsSection::Appshots)
                        });
                        page.clone().into_any_element()
                    }
                    None => Empty.into_any_element(),
                }
            }
            SettingsSection::Archived => {
                if self.archived_page.is_none() {
                    let state = self.state.clone();
                    self.archived_page = Some(cx.new(|cx| ArchivedPage::new(state, cx)));
                }
                match &self.archived_page {
                    Some(page) => page.clone().into_any_element(),
                    None => Empty.into_any_element(),
                }
            }
            SettingsSection::Usage => self.render_usage_page(cx),
            SettingsSection::Updates => {
                if !self.update_check_attempted
                    && self
                        .update_status(cx)
                        .as_ref()
                        .and_then(|s| s.checked_at)
                        .is_none()
                {
                    self.begin_update_check(cx);
                }
                self.render_updates_page(cx)
            }
        }
    }

    // ---- sidebar mutations ----

    fn refresh_deletion_cleanup_summary(&mut self, cx: &mut Context<Self>) {
        let (generation, engine) = {
            let state = self.state.read(cx);
            if !matches!(state.connection, ConnectionStatus::Ready) {
                return;
            }
            (state.engine_generation(), state.engine().cloned())
        };
        let Some(engine) = engine else { return };
        cx.spawn(async move |this, cx| {
            let Ok(summary) = engine.client().call(
                methods::GET_DELETION_CLEANUP_SUMMARY, serde_json::json!({}),
            ).await else { return };
            let pending = summary.get("pending").and_then(|value| value.as_u64()).unwrap_or(0);
            let failed = summary.get("failed").and_then(|value| value.as_u64()).unwrap_or(0);
            let notice = if failed > 0 {
                let detail = summary.get("firstFailure").and_then(|value| value.as_str())
                    .unwrap_or("Review the local cleanup status.");
                Some(format!("Deletion cleanup: {failed} failed, {pending} pending. {detail}"))
            } else if pending > 0 {
                Some(format!("Deletion cleanup: {pending} pending. LUNA will retry after the registry is ready."))
            } else {
                None
            };
            this.update(cx, |shell, cx| {
                if shell.state.read(cx).engine_generation() != generation {
                    return;
                }
                if notice == shell.cleanup_summary_last {
                    return;
                }
                let old_notice = shell.cleanup_summary_last.take();
                if let Some(message) = notice.clone() {
                    shell.sidebar_notice = Some(message.into());
                    cx.notify();
                } else if shell.sidebar_notice.as_ref().map(ToString::to_string) == old_notice {
                    shell.sidebar_notice = None;
                    cx.notify();
                }
                shell.cleanup_summary_last = notice;
            }).ok();
        }).detach();
    }

    /// Execute a durable task command; failures surface in the sidebar notice strip.
    fn mutate_task(&mut self, task_id: String, intent: serde_json::Value, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.sidebar_notice = Some("Engine not connected".into());
            cx.notify();
            return;
        };
        let deleting = intent.get("kind").and_then(|kind| kind.as_str()) == Some("delete");
        let pending_action = match intent.get("kind").and_then(|kind| kind.as_str()) {
            Some("setArchived")
                if intent.get("archived").and_then(|value| value.as_bool()) == Some(true) =>
            {
                Some(TaskConfirmation::Archive)
            }
            Some("setSettled")
                if intent.get("settled").and_then(|value| value.as_bool()) == Some(true) =>
            {
                Some(TaskConfirmation::Settle)
            }
            Some("delete") => Some(TaskConfirmation::Delete),
            _ => None,
        };
        if let Some(action) = pending_action {
            self.pending_task_actions.insert(task_id.clone(), action);
            cx.notify();
        }
        let prior_notice = self.sidebar_notice.as_ref().map(|notice| notice.id);
        let target_device = self
            .state
            .read(cx)
            .chats
            .iter()
            .find(|chat| chat.id == task_id)
            .map(|chat| chat.device_id.clone());
        self.mutate_task = Some(cx.spawn(async move |this, cx| {
            let mut result = crate::task_commands::execute(&engine, &task_id, intent).await;
            if pending_action.is_some()
                && let Ok(initial) = &result
                && initial.status == luna_proto::CommandReceiptStatus::Pending
            {
                let command_id = initial.command_id.clone();
                for _ in 0..40 {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    let mut params = serde_json::json!({ "commandId": command_id });
                    if let Some(device) = target_device.as_deref() {
                        params["targetDeviceId"] = device.into();
                    }
                    match engine
                        .client()
                        .call_as::<Option<luna_proto::CommandReceipt>>(
                            methods::GET_TASK_COMMAND_RECEIPT,
                            params,
                        )
                        .await
                    {
                        Ok(Some(receipt)) if receipt.status == luna_proto::CommandReceiptStatus::Applied => {
                            result = Ok(receipt);
                            break;
                        }
                        Ok(Some(receipt)) if receipt.status.is_terminal() => {
                            result = Err(luna_rpc::RpcError::Failed(
                                crate::task_commands::failure_message(
                                    &receipt,
                                    "Could not update this task.",
                                ),
                            ));
                            break;
                        }
                        Ok(_) => {}
                        Err(error) => {
                            result = Err(error);
                            break;
                        }
                    }
                }
                if result.as_ref().is_ok_and(|receipt| {
                    receipt.status == luna_proto::CommandReceiptStatus::Pending
                }) {
                    result = Err(luna_rpc::RpcError::Failed(
                        "LUNA could not confirm this action yet. Check the task before trying again."
                            .into(),
                    ));
                }
            }
            if let Err(err) = &result {
                let message = match err {
                    luna_rpc::RpcError::Closed | luna_rpc::RpcError::Transport(_) => {
                        "Could not reach this task's device. Try again when it reconnects."
                            .to_string()
                    }
                    _ => err.to_string(),
                };
                tracing::warn!(task_id, %err, "task action failed");
                this.update(cx, |shell, cx| {
                    if pending_action.is_some() {
                        shell.pending_task_actions.remove(&task_id);
                    }
                    shell.sidebar_notice = Some(SidebarNotice::transient_error(message));
                    cx.notify();
                })
                .ok();
            } else {
                this.update(cx, |shell, cx| {
                    if shell
                        .sidebar_notice
                        .as_ref()
                        .is_some_and(|notice| Some(notice.id) == prior_notice)
                    {
                        shell.sidebar_notice = None;
                        cx.notify();
                    }
                })
                .ok();
            }
            if deleting
                && result.as_ref().is_ok_and(|receipt| {
                    receipt.status == luna_proto::CommandReceiptStatus::Applied
                })
            {
                this.update(cx, |shell, cx| {
                    if let Some(tabs) = shell.right_tabs.get(&task_id) {
                        for surface in tabs {
                            if let RightSurface::Browser(id) = surface {
                                if let Some(browser) = shell.browsers.remove(id) {
                                    browser.update(cx, |browser, cx| browser.close(cx));
                                }
                                shell.browser_subs.remove(id);
                            }
                        }
                    }
                    if shell.state.read(cx).selected_chat.as_deref() == Some(task_id.as_str()) {
                        shell
                            .state
                            .update(cx, |state, cx| state.select_chat(None, cx));
                    }
                    shell
                        .composer
                        .update(cx, |composer, cx| composer.purge_chat(&task_id, cx));
                    cx.notify();
                })
                .ok();
                let mut params = serde_json::json!({ "taskId": task_id });
                if let Some(device) = target_device {
                    params["targetDeviceId"] = device.into();
                }
                let status = engine
                    .client()
                    .call(methods::GET_TASK_CLEANUP, params)
                    .await;
                let notice = match status {
                    Ok(value) if value.is_null() => {
                        Some("Deletion committed; cleanup status is missing.".to_string())
                    }
                    Ok(value) => value
                        .get("detail")
                        .and_then(|detail| detail.as_str())
                        .filter(|_| {
                            value.get("state").and_then(|state| state.as_str()) != Some("completed")
                        })
                        .map(str::to_owned),
                    Err(error) => Some(format!(
                        "Deletion committed; cleanup status is unavailable: {error}"
                    )),
                };
                if let Some(notice) = notice {
                    this.update(cx, |shell, cx| {
                        shell.sidebar_notice = Some(notice.into());
                        cx.notify();
                    })
                    .ok();
                }
            }
        }));
    }

    fn mutate_task_reviewed(
        &mut self,
        task_id: String,
        snapshot: TaskCommandSnapshot,
        intent: serde_json::Value,
        origin: HistoryEngineOrigin,
        cx: &mut Context<Self>,
    ) {
        let (origin_current, engine) = {
            let state = self.state.read(cx);
            let current_generation = state.engine_generation();
            (
                origin.is_current(&state),
                origin
                    .run_if_current(current_generation, || state.engine().cloned())
                    .flatten(),
            )
        };
        if !origin_current {
            self.sidebar_notice = Some(
                "History review belongs to the previous profile; reopen it in this profile.".into(),
            );
            self.cancel_history_flow(cx);
            return;
        }
        let Some(engine) = engine else {
            self.sidebar_notice = Some("Engine not connected".into());
            cx.notify();
            return;
        };
        self.mutate_task = Some(cx.spawn(async move |this, cx| {
            let current = this
                .update(cx, |shell, cx| {
                    if origin.is_current(&shell.state.read(cx)) {
                        true
                    } else {
                        shell.sidebar_notice = Some(
                            "History review belongs to the previous profile; reopen it in this profile."
                                .into(),
                        );
                        shell.cancel_history_flow(cx);
                        false
                    }
                })
                .unwrap_or(false);
            if !current {
                return;
            }
            if let Err(error) =
                crate::task_commands::execute_reviewed(&engine, &task_id, snapshot, intent).await
            {
                this.update(cx, |shell, cx| {
                    if !origin.is_current(&shell.state.read(cx)) {
                        return;
                    }
                    shell.sidebar_notice = Some(error.to_string().into());
                    cx.notify();
                })
                .ok();
            }
        }));
    }

    fn cancel_history_flow(&mut self, cx: &mut Context<Self>) {
        self.history_flow_generation = self.history_flow_generation.wrapping_add(1);
        self.history_fork_dialog = None;
        self.edit_earlier_message_dialog = None;
        self.conversation_rewind_dialog = None;
        self.history_recovery_dialog = None;
        cx.notify();
    }

    fn require_history_origin(
        &mut self,
        origin: HistoryEngineOrigin,
        cx: &mut Context<Self>,
    ) -> bool {
        if origin.is_current(&self.state.read(cx)) {
            return true;
        }
        self.sidebar_notice = Some(
            "History review belongs to the previous profile; reopen it in this profile.".into(),
        );
        self.cancel_history_flow(cx);
        false
    }

    /// Non-task workspace mutations retain their existing owner. L07 does not
    /// widen task receipts into project/device lifecycle behavior.
    fn mutate(&mut self, params: serde_json::Value, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.sidebar_notice = Some("Engine not connected".into());
            cx.notify();
            return;
        };
        let deleted_project = (params.get("op").and_then(|op| op.as_str()) == Some("deleteSpace"))
            .then(|| {
                params
                    .get("spaceId")
                    .and_then(|id| id.as_str())
                    .map(str::to_owned)
            })
            .flatten();
        self.mutate_task = Some(cx.spawn(async move |this, cx| {
            if let Err(err) = engine.client().call(methods::MUTATE, params).await {
                this.update(cx, |shell, cx| {
                    shell.sidebar_notice = Some(format!("{err}").into());
                    cx.notify();
                })
                .ok();
            } else if let Some(project_id) = deleted_project {
                let status = engine
                    .client()
                    .call(
                        methods::GET_PROJECT_CLEANUP,
                        serde_json::json!({ "projectId": project_id }),
                    )
                    .await;
                let notice = match status {
                    Ok(value) if value.is_null() => {
                        Some("Project deletion committed; cleanup status is missing.".to_string())
                    }
                    Ok(value) => value
                        .get("detail")
                        .and_then(|detail| detail.as_str())
                        .filter(|_| {
                            value.get("state").and_then(|state| state.as_str()) != Some("completed")
                        })
                        .map(str::to_owned),
                    Err(error) => Some(format!(
                        "Project deleted; cleanup status is unavailable: {error}"
                    )),
                };
                if let Some(notice) = notice {
                    this.update(cx, |shell, cx| {
                        shell.sidebar_notice = Some(notice.into());
                        cx.notify();
                    })
                    .ok();
                }
            }
        }));
    }

    fn open_rename_chat(&mut self, chat_id: String, cx: &mut Context<Self>) {
        self.close_chat_menu(cx);
        let current = self
            .state
            .read(cx)
            .chats
            .iter()
            .find(|c| c.id == chat_id)
            .and_then(|c| c.title.clone())
            .unwrap_or_default();
        let input = cx.new(|cx| {
            ComposerInput::new("Session title", cx).with_accessibility_role(gpui::Role::TextInput)
        });
        input.update(cx, |input, cx| input.set_text(current, cx));
        let events = cx.subscribe(&input, |this: &mut Shell, _, event, cx| {
            if matches!(event, ComposerInputEvent::Submitted) {
                this.submit_rename_chat(cx);
            }
        });
        self.rename_dialog = Some(RenameChatDialog {
            chat_id,
            input,
            focus_pending: true,
            _events: events,
        });
        cx.notify();
    }

    fn submit_rename_chat(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.rename_dialog.take() else {
            return;
        };
        let title = dialog.input.read(cx).text().trim().to_string();
        if !title.is_empty() {
            let chat_id = dialog.chat_id;
            self.mutate_task(
                chat_id,
                serde_json::json!({ "kind": "rename", "title": title }),
                cx,
            );
        }
        cx.notify();
    }

    fn open_history_fork(
        &mut self,
        source_task_id: String,
        boundary: ConversationForkBoundaryKind,
        message_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.close_chat_menu(cx);
        self.cancel_history_flow(cx);
        let generation = self.history_flow_generation;
        let (engine, origin, chat, message_text) = {
            let state = self.state.read(cx);
            let chat = state
                .chats
                .iter()
                .find(|chat| chat.id == source_task_id)
                .cloned();
            let message_text = message_id.as_ref().and_then(|id| {
                state
                    .transcript
                    .iter()
                    .find(|entry| &entry.id == id)
                    .map(|entry| {
                        entry
                            .parts
                            .iter()
                            .filter_map(|part| match part {
                                luna_doc::MessagePart::Text { text, .. } => Some(text.trim()),
                                _ => None,
                            })
                            .filter(|text| !text.is_empty())
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
            });
            (
                state.engine().cloned(),
                HistoryEngineOrigin::capture(&state),
                chat,
                message_text,
            )
        };
        let Some(engine) = engine else {
            self.sidebar_notice = Some("Engine not connected".into());
            return;
        };
        let Some(chat) = chat else {
            self.sidebar_notice = Some("Task no longer exists".into());
            return;
        };
        self.mutate_task = Some(cx.spawn(async move |this, cx| {
            let read = engine
                .client()
                .call_as::<luna_proto::TaskCommandRead>(
                    methods::GET_TASK_COMMAND_SNAPSHOT,
                    serde_json::json!({ "taskId": source_task_id }),
                )
                .await;
            this.update(cx, |shell, cx| {
                if !history_flow_response_is_live(shell.history_flow_generation, generation) {
                    return;
                }
                if !origin.is_current(&shell.state.read(cx)) {
                    return;
                }
                let current = shell
                    .state
                    .read(cx)
                    .chats
                    .iter()
                    .find(|candidate| candidate.id == chat.id);
                let unchanged = history_review_profile_unchanged(
                    &chat.harness_session_id,
                    &chat.cwd,
                    &chat.config,
                    current,
                );
                let Ok(read) = read else {
                    shell.sidebar_notice = Some("Could not review the current task state.".into());
                    return;
                };
                let Some(mut config) = chat.config.clone() else {
                    shell.sidebar_notice = Some("Task has no provider configuration.".into());
                    return;
                };
                let Some(provider_session_id) = chat.harness_session_id.clone() else {
                    shell.sidebar_notice = Some("Task has no provider history yet.".into());
                    return;
                };
                let Some(binding_generation) = read.snapshot.binding_generation else {
                    shell.sidebar_notice = Some("Task binding is not materialized.".into());
                    return;
                };
                if !unchanged {
                    shell.sidebar_notice =
                        Some("Task changed while opening the fork review.".into());
                    return;
                }
                let permission_ceiling = config.effective_permission_mode;
                config.permission_mode = history_fork_initial_permission(&config);
                let source_title = chat
                    .title
                    .as_deref()
                    .map(|title| format!("{title} (fork)"))
                    .unwrap_or_else(|| "Forked task".into());
                let title = cx.new(|cx| ComposerInput::new("Task title (optional)", cx));
                title.update(cx, |input, cx| input.set_text(source_title, cx));
                let prompt = cx.new(|cx| ComposerInput::new("First prompt (optional)", cx));
                let events = cx.subscribe(&prompt, |this: &mut Shell, _, event, cx| {
                    if matches!(event, ComposerInputEvent::Submitted) {
                        this.submit_history_fork(cx);
                    }
                });
                let cwd = chat.cwd.clone().unwrap_or_else(|| ".".into());
                let review = serde_json::json!({
                    "taskRevision": read.snapshot.task_revision,
                    "bindingGeneration": binding_generation,
                    "providerSessionId": provider_session_id,
                    "providerAccountId": config.provider_account_id,
                    "cwd": cwd,
                    "messageId": message_id,
                    "messageText": message_text,
                });
                shell.history_fork_dialog = Some(HistoryForkDialog {
                    source_task_id: chat.id,
                    boundary,
                    message_id,
                    title,
                    prompt,
                    snapshot: read.snapshot,
                    review,
                    origin,
                    config,
                    permission_ceiling,
                    cwd,
                    focus_pending: true,
                    _events: events,
                });
                cx.notify();
            })
            .ok();
        }));
    }

    fn submit_history_fork(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.history_fork_dialog.take() else {
            return;
        };
        if !self.require_history_origin(dialog.origin, cx) {
            return;
        }
        let title = dialog.title.read(cx).text().trim().to_string();
        let initial_prompt = dialog.prompt.read(cx).text().trim().to_string();
        let state = self.state.read(cx);
        let source_link = crate::links::workspace_locator(
            state
                .engine()
                .map(|engine| engine.engine_info().workspace_scope),
            state.auth.as_ref(),
            state.local_device_id.as_deref(),
        )
        .map(|workspace| crate::links::luna_conversation_link(&dialog.source_task_id, &workspace));
        self.history_flow_generation = self.history_flow_generation.wrapping_add(1);
        self.mutate_task_reviewed(
            dialog.source_task_id,
            dialog.snapshot,
            serde_json::json!({
                "kind": "forkTask",
                "newTaskId": uuid::Uuid::new_v4().to_string(),
                "boundary": dialog.boundary,
                "messageId": dialog.message_id,
                "title": (!title.is_empty()).then_some(title),
                "initialPrompt": (!initial_prompt.is_empty()).then_some(initial_prompt),
                "config": dialog.config,
                "review": dialog.review,
                "sourceLink": source_link,
            }),
            dialog.origin,
            cx,
        );
        cx.notify();
    }

    fn open_conversation_rewind(
        &mut self,
        task_id: String,
        message_id: String,
        text: String,
        cx: &mut Context<Self>,
    ) {
        self.cancel_history_flow(cx);
        let generation = self.history_flow_generation;
        let (engine, origin, chat, reviewed_text) = {
            let state = self.state.read(cx);
            (
                state.engine().cloned(),
                HistoryEngineOrigin::capture(state),
                state.chats.iter().find(|chat| chat.id == task_id).cloned(),
                state
                    .transcript
                    .iter()
                    .find(|entry| entry.id == message_id)
                    .map(|entry| {
                        entry
                            .parts
                            .iter()
                            .filter_map(|part| match part {
                                luna_doc::MessagePart::Text { text, .. } => Some(text.trim()),
                                _ => None,
                            })
                            .filter(|text| !text.is_empty())
                            .collect::<Vec<_>>()
                            .join("\n")
                    }),
            )
        };
        let Some(engine) = engine else {
            self.sidebar_notice = Some("Engine not connected".into());
            return;
        };
        let Some(chat) = chat else {
            self.sidebar_notice = Some("Task no longer exists".into());
            return;
        };
        let Some(reviewed_text) = reviewed_text else {
            self.sidebar_notice =
                Some("The selected message is no longer in this conversation.".into());
            return;
        };
        self.mutate_task = Some(cx.spawn(async move |this, cx| {
            let read = engine
                .client()
                .call_as::<luna_proto::TaskCommandRead>(
                    methods::GET_TASK_COMMAND_SNAPSHOT,
                    serde_json::json!({ "taskId": task_id }),
                )
                .await;
            this.update(cx, |shell, cx| {
                if !history_flow_response_is_live(shell.history_flow_generation, generation) {
                    return;
                }
                if !origin.is_current(shell.state.read(cx)) {
                    return;
                }
                let current = shell
                    .state
                    .read(cx)
                    .chats
                    .iter()
                    .find(|candidate| candidate.id == chat.id);
                let unchanged = history_review_profile_unchanged(
                    &chat.harness_session_id,
                    &chat.cwd,
                    &chat.config,
                    current,
                );
                let Ok(read) = read else {
                    shell.sidebar_notice = Some("Could not review the current task state.".into());
                    return;
                };
                let Some(config) = chat.config.as_ref() else {
                    shell.sidebar_notice = Some("Task has no provider configuration.".into());
                    return;
                };
                let Some(provider_session_id) = chat.harness_session_id.clone() else {
                    shell.sidebar_notice = Some("Task has no provider history yet.".into());
                    return;
                };
                let Some(binding_generation) = read.snapshot.binding_generation else {
                    shell.sidebar_notice = Some("Task binding is not materialized.".into());
                    return;
                };
                if !unchanged {
                    shell.sidebar_notice =
                        Some("Task changed while opening the rewind review.".into());
                    return;
                }
                let review = serde_json::json!({
                    "taskRevision": read.snapshot.task_revision,
                    "bindingGeneration": binding_generation,
                    "providerSessionId": provider_session_id,
                    "providerAccountId": config.provider_account_id,
                    "cwd": chat.cwd.clone().unwrap_or_else(|| ".".into()),
                    "messageId": message_id,
                    "messageText": reviewed_text,
                });
                let pending_rewind = shell.state.read(cx).context_control.pending_rewind.clone();
                shell.conversation_rewind_dialog = Some(ConversationRewindDialog {
                    task_id: chat.id,
                    message_id,
                    text,
                    snapshot: read.snapshot,
                    review,
                    origin,
                    pending_rewind,
                });
                cx.notify();
            })
            .ok();
        }));
    }

    fn submit_conversation_rewind(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.conversation_rewind_dialog.take() else {
            return;
        };
        if !self.require_history_origin(dialog.origin, cx) {
            return;
        }
        let intent = if let Some(command_id) = dialog.pending_rewind {
            serde_json::json!({ "kind": "reconcileConversationRewind", "rewindCommandId": command_id })
        } else {
            serde_json::json!({ "kind": "rewindConversation", "messageId": dialog.message_id,
                "keepFiles": true, "review": dialog.review })
        };
        self.history_flow_generation = self.history_flow_generation.wrapping_add(1);
        self.mutate_task_reviewed(dialog.task_id, dialog.snapshot, intent, dialog.origin, cx);
        cx.notify();
    }

    fn open_edit_earlier_message(
        &mut self,
        task_id: String,
        message_id: String,
        text: String,
        cx: &mut Context<Self>,
    ) {
        self.cancel_history_flow(cx);
        let generation = self.history_flow_generation;
        let (engine, origin, chat) = {
            let state = self.state.read(cx);
            (
                state.engine().cloned(),
                HistoryEngineOrigin::capture(&state),
                state.chats.iter().find(|chat| chat.id == task_id).cloned(),
            )
        };
        let Some(engine) = engine else {
            self.sidebar_notice = Some("Engine not connected".into());
            return;
        };
        let Some(chat) = chat else {
            self.sidebar_notice = Some("Task no longer exists".into());
            return;
        };
        let reviewed_text = text.clone();
        self.mutate_task = Some(cx.spawn(async move |this, cx| {
            let read = engine
                .client()
                .call_as::<luna_proto::TaskCommandRead>(
                    methods::GET_TASK_COMMAND_SNAPSHOT,
                    serde_json::json!({ "taskId": task_id }),
                )
                .await;
            this.update(cx, |shell, cx| {
                if !history_flow_response_is_live(shell.history_flow_generation, generation) {
                    return;
                }
                if !origin.is_current(&shell.state.read(cx)) {
                    return;
                }
                let current = shell
                    .state
                    .read(cx)
                    .chats
                    .iter()
                    .find(|candidate| candidate.id == chat.id);
                let unchanged = history_review_profile_unchanged(
                    &chat.harness_session_id,
                    &chat.cwd,
                    &chat.config,
                    current,
                );
                let Ok(read) = read else {
                    shell.sidebar_notice = Some("Could not review the current task state.".into());
                    return;
                };
                let Some(config) = chat.config.as_ref() else {
                    shell.sidebar_notice = Some("Task has no provider configuration.".into());
                    return;
                };
                let Some(provider_session_id) = chat.harness_session_id.clone() else {
                    shell.sidebar_notice = Some("Task has no provider history yet.".into());
                    return;
                };
                let Some(binding_generation) = read.snapshot.binding_generation else {
                    shell.sidebar_notice = Some("Task binding is not materialized.".into());
                    return;
                };
                if !unchanged {
                    shell.sidebar_notice =
                        Some("Task changed while opening the edit review.".into());
                    return;
                }
                let input = cx.new(|cx| ComposerInput::new("Replacement message", cx));
                input.update(cx, |input, cx| input.set_text(text, cx));
                let events = cx.subscribe(&input, |this: &mut Shell, _, event, cx| {
                    if matches!(event, ComposerInputEvent::Submitted) {
                        this.submit_edit_earlier_message(cx);
                    }
                });
                let review = serde_json::json!({
                    "taskRevision": read.snapshot.task_revision,
                    "bindingGeneration": binding_generation,
                    "providerSessionId": provider_session_id,
                    "providerAccountId": config.provider_account_id,
                    "cwd": chat.cwd.clone().unwrap_or_else(|| ".".into()),
                    "messageId": message_id,
                    "messageText": reviewed_text,
                });
                shell.edit_earlier_message_dialog = Some(EditEarlierMessageDialog {
                    task_id: chat.id,
                    message_id,
                    input,
                    snapshot: read.snapshot,
                    review,
                    origin,
                    focus_pending: true,
                    _events: events,
                });
                cx.notify();
            })
            .ok();
        }));
    }

    fn submit_edit_earlier_message(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.edit_earlier_message_dialog.take() else {
            return;
        };
        if !self.require_history_origin(dialog.origin, cx) {
            return;
        }
        let text = dialog.input.read(cx).text().trim().to_string();
        if !text.is_empty() {
            self.history_flow_generation = self.history_flow_generation.wrapping_add(1);
            self.mutate_task_reviewed(
                dialog.task_id,
                dialog.snapshot,
                serde_json::json!({
                    "kind": "editEarlierMessage",
                    "messageId": dialog.message_id,
                    "text": text,
                    "review": dialog.review,
                }),
                dialog.origin,
                cx,
            );
        }
        cx.notify();
    }

    fn open_history_recovery(&mut self, task_id: String, cx: &mut Context<Self>) {
        self.close_chat_menu(cx);
        self.cancel_history_flow(cx);
        let generation = self.history_flow_generation;
        let (engine, origin, chat) = {
            let state = self.state.read(cx);
            (
                state.engine().cloned(),
                HistoryEngineOrigin::capture(&state),
                state.chats.iter().find(|chat| chat.id == task_id).cloned(),
            )
        };
        let Some(engine) = engine else {
            self.sidebar_notice = Some("Engine not connected".into());
            cx.notify();
            return;
        };
        let Some(chat) = chat else {
            self.sidebar_notice = Some("Task no longer exists".into());
            return;
        };
        let requested_task = task_id.clone();
        self.mutate_task = Some(cx.spawn(async move |this, cx| {
            let recoveries = engine
                .client()
                .call_as::<Vec<HistoryRecoveryView>>(
                    methods::LIST_HISTORY_RECOVERIES,
                    serde_json::json!({ "taskId": requested_task }),
                )
                .await;
            let snapshot = engine
                .client()
                .call_as::<luna_proto::TaskCommandRead>(
                    methods::GET_TASK_COMMAND_SNAPSHOT,
                    serde_json::json!({ "taskId": requested_task }),
                )
                .await;
            this.update(cx, |shell, cx| {
                if !history_flow_response_is_live(shell.history_flow_generation, generation) {
                    return;
                }
                if !origin.is_current(&shell.state.read(cx)) {
                    return;
                }
                let current = shell
                    .state
                    .read(cx)
                    .chats
                    .iter()
                    .find(|candidate| candidate.id == chat.id);
                let unchanged = history_review_profile_unchanged(
                    &chat.harness_session_id,
                    &chat.cwd,
                    &chat.config,
                    current,
                );
                match (recoveries, snapshot) {
                    (Ok(recoveries), Ok(read)) if !recoveries.is_empty() && unchanged => {
                        let Some(config) = chat.config.as_ref() else {
                            shell.sidebar_notice =
                                Some("Task has no provider configuration.".into());
                            return;
                        };
                        let Some(provider_session_id) = chat.harness_session_id.clone() else {
                            shell.sidebar_notice = Some("Task has no provider history yet.".into());
                            return;
                        };
                        let Some(binding_generation) = read.snapshot.binding_generation else {
                            shell.sidebar_notice = Some("Task binding is not materialized.".into());
                            return;
                        };
                        let review = serde_json::json!({
                            "taskRevision": read.snapshot.task_revision,
                            "bindingGeneration": binding_generation,
                            "providerSessionId": provider_session_id,
                            "providerAccountId": config.provider_account_id,
                            "cwd": chat.cwd.clone().unwrap_or_else(|| ".".into()),
                            "messageId": null,
                            "messageText": null,
                        });
                        shell.history_recovery_dialog = Some(HistoryRecoveryDialog {
                            task_id,
                            selected: recoveries.len() - 1,
                            recoveries,
                            snapshot: read.snapshot,
                            review,
                            origin,
                        });
                    }
                    (Ok(recoveries), Ok(_)) if recoveries.is_empty() => {
                        shell.sidebar_notice =
                            Some("No earlier-message recovery is available.".into())
                    }
                    (Ok(_), Ok(_)) => {
                        shell.sidebar_notice = Some("Task changed while opening recovery.".into())
                    }
                    (Err(error), _) | (_, Err(error)) => {
                        shell.sidebar_notice = Some(error.to_string().into())
                    }
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn submit_history_recovery(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.history_recovery_dialog.take() else {
            return;
        };
        if !self.require_history_origin(dialog.origin, cx) {
            return;
        }
        let Some(recovery) = dialog.recoveries.get(dialog.selected) else {
            cx.notify();
            return;
        };
        let mut review = dialog.review;
        review["messageText"] = recovery.source_provider_session_id.clone().into();
        self.history_flow_generation = self.history_flow_generation.wrapping_add(1);
        self.mutate_task_reviewed(
            dialog.task_id,
            dialog.snapshot,
            serde_json::json!({
                "kind": "restoreHistoryRecovery",
                "recoveryId": recovery.recovery_id,
                "review": review,
            }),
            dialog.origin,
            cx,
        );
        cx.notify();
    }

    fn start_new_task_with_context(&mut self, task_id: &str, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        if state.selected_chat.as_deref() != Some(task_id) {
            self.sidebar_notice =
                Some("Open the source task before starting a new task with its context.".into());
            cx.notify();
            return;
        }
        let Some(handoff) = crate::pickers::recent_visible_handoff(&state.transcript) else {
            self.sidebar_notice = Some("This task has no visible context to hand off.".into());
            cx.notify();
            return;
        };
        let engine_generation = state.engine_generation();
        let draft = format!(
            "[LUNA user-selected context handoff]\n{}\n[End handoff]\n\n",
            handoff.text
        );
        let staged = self.composer.update(cx, |composer, cx| {
            composer.stage_new_task_context_handoff(task_id, draft, engine_generation, cx)
        });
        if !staged {
            self.sidebar_notice = Some(
                "The draft owner changed while preparing the context handoff; try again.".into(),
            );
            cx.notify();
            return;
        }
        self.open_new_session(cx);
        self.sidebar_notice = Some(SidebarNotice::transient_status(
            "Started a new task draft with the selected visible context. Review it before sending.",
        ));
        cx.notify();
    }

    fn archive_chat(&mut self, chat_id: String, cx: &mut Context<Self>) {
        self.set_chat_archived(chat_id, true, cx);
    }

    fn set_chat_pinned(&mut self, chat_id: String, pinned: bool, cx: &mut Context<Self>) {
        self.close_chat_menu(cx);
        self.mutate_task(
            chat_id,
            serde_json::json!({ "kind": "setPinned", "pinned": pinned }),
            cx,
        );
    }

    fn reorder_pinned_chat(&mut self, chat_id: String, delta: isize, cx: &mut Context<Self>) {
        let mut pinned: Vec<_> = self
            .state
            .read(cx)
            .chats
            .iter()
            .filter(|chat| !chat.archived && chat.pinned_order.is_some())
            .cloned()
            .collect();
        pinned.sort_by(|left, right| {
            luna_proto::compare_pin_order(
                left.pinned_order.as_deref().expect("filtered pin key"),
                right.pinned_order.as_deref().expect("filtered pin key"),
            )
            .then_with(|| left.id.cmp(&right.id))
        });
        let Some(index) = pinned.iter().position(|chat| chat.id == chat_id) else {
            return;
        };
        let target = index
            .saturating_add_signed(delta)
            .min(pinned.len().saturating_sub(1));
        if target == index {
            return;
        }
        let moved = pinned.remove(index);
        pinned.insert(target, moved);
        let after = target
            .checked_sub(1)
            .and_then(|ix| pinned.get(ix))
            .map(|chat| chat.id.clone());
        let before = pinned.get(target + 1).map(|chat| chat.id.clone());
        self.close_chat_menu(cx);
        self.mutate_task(
            chat_id,
            serde_json::json!({
                "kind": "setPinned",
                "pinned": true,
                "afterTaskId": after,
                "beforeTaskId": before,
            }),
            cx,
        );
    }

    fn set_chat_read(&mut self, chat_id: String, read: bool, cx: &mut Context<Self>) {
        self.close_chat_menu(cx);
        self.mutate_task(
            chat_id,
            serde_json::json!({ "kind": "setRead", "read": read }),
            cx,
        );
    }

    fn open_move_project(&mut self, chat_id: String, cx: &mut Context<Self>) {
        self.close_chat_menu(cx);
        let search =
            cx.new(|cx| ComposerInput::with_context("Search projects…", "PaletteSearch", cx));
        let events = cx.subscribe(&search, |_: &mut Shell, _, event, cx| {
            if matches!(event, ComposerInputEvent::Edited) {
                cx.notify();
            }
        });
        self.move_project_dialog = Some(MoveProjectDialog {
            chat_id,
            search,
            selected_space_id: None,
            focus_pending: true,
            _events: events,
        });
        cx.notify();
    }

    fn open_move_project_to(&mut self, chat_id: String, space_id: String, cx: &mut Context<Self>) {
        self.open_move_project(chat_id, cx);
        if let Some(dialog) = self.move_project_dialog.as_mut() {
            dialog.selected_space_id = Some(space_id);
        }
    }

    fn submit_move_project(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.move_project_dialog.take() else {
            return;
        };
        let Some(destination) = dialog.selected_space_id else {
            return;
        };
        self.mutate_task(
            dialog.chat_id,
            serde_json::json!({
                "kind": "moveProject",
                "destinationSpaceId": destination,
            }),
            cx,
        );
        cx.notify();
    }

    /// The Archive session shortcut. With no chat open, or with an already
    /// archived one, it does nothing — the shortcut archives, it never
    /// unarchives.
    fn archive_selected_chat(&mut self, cx: &mut Context<Self>) {
        let Some(chat_id) = self
            .state
            .read(cx)
            .archivable_selected_chat()
            .map(str::to_string)
        else {
            return;
        };
        self.archive_chat(chat_id, cx);
    }

    pub(super) fn set_chat_archived(
        &mut self,
        chat_id: String,
        archived: bool,
        cx: &mut Context<Self>,
    ) {
        self.close_chat_menu(cx);
        if archived {
            self.request_task_action(chat_id, TaskConfirmation::Archive, cx);
        } else {
            self.mutate_task(
                chat_id,
                serde_json::json!({ "kind": "setArchived", "archived": false }),
                cx,
            );
        }
    }

    pub(super) fn set_chat_settled(
        &mut self,
        chat_id: String,
        settled: bool,
        cx: &mut Context<Self>,
    ) {
        self.close_chat_menu(cx);
        if settled {
            self.request_task_action(chat_id, TaskConfirmation::Settle, cx);
        } else {
            self.mutate_task(
                chat_id,
                serde_json::json!({ "kind": "setSettled", "settled": false }),
                cx,
            );
        }
    }

    /// A jump shortcut: open the sidebar row at `slot`. A slot past the end of
    /// a short list does nothing. Reads the DISPLAYED order — sort and
    /// grouping view options permute the list, and the chip on a row must
    /// name the key that opens it.
    fn jump_to_session(&mut self, slot: usize, cx: &mut Context<Self>) {
        let Some(chat_id) = self.sidebar_visible_order(cx).into_iter().nth(slot) else {
            return;
        };
        // Same path a click on that row takes.
        self.open_chat(chat_id, cx);
    }

    /// Whether an overlay that owns the keyboard is up — the add-space
    /// palette or a composer picker popover (model selector, traits, repo,
    /// branch…). Session-nav shortcuts (cycle/jump/archive) go quiet
    /// underneath one: gpui runs a matched binding before any `on_key_down`,
    /// so an unguarded jump would switch sessions UNDER the open popover,
    /// stranding it over a session the user never picked.
    pub(super) fn overlay_owns_keyboard(&self, cx: &App) -> bool {
        self.command_palette.is_some()
            || self.add_space.is_some()
            || self.composer.read(cx).pickers().read(cx).is_open()
    }

    /// Track held modifiers for sidebar jump hints and the queue's submit hint.
    /// Only a visibility change repaints; modifier traffic is otherwise constant.
    fn on_modifiers_changed(&mut self, event: &ModifiersChangedEvent, cx: &mut Context<Self>) {
        self.update_jump_hints(&event.modifiers, cx);
    }

    fn update_jump_hints(&mut self, mods: &gpui::Modifiers, cx: &mut Context<Self>) {
        let primary = if cfg!(target_os = "macos") {
            mods.platform
        } else {
            mods.control
        };
        // No hints while an overlay owns the keyboard — the jumps they
        // advertise are suppressed there.
        let visible = matches!(self.route, Route::Chat)
            && !self.overlay_owns_keyboard(cx)
            && jump_hints_visible(&self.settings.keymap, primary, mods.alt, mods.shift);
        self.set_jump_hints(visible, cx);
    }

    pub(super) fn set_jump_hints(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.jump_hints != visible {
            self.jump_hints = visible;
            cx.notify();
        }
    }

    fn request_task_action(
        &mut self,
        chat_id: String,
        action: TaskConfirmation,
        cx: &mut Context<Self>,
    ) {
        if self.settings.confirms(action) {
            self.task_action_confirm = Some((chat_id, action));
            self.task_action_dont_show_again = false;
        } else {
            self.execute_task_action(chat_id, action, cx);
        }
        cx.notify();
    }

    fn execute_task_action(
        &mut self,
        chat_id: String,
        action: TaskConfirmation,
        cx: &mut Context<Self>,
    ) {
        self.task_action_confirm = None;
        let intent = match action {
            TaskConfirmation::Archive => {
                serde_json::json!({ "kind": "setArchived", "archived": true })
            }
            TaskConfirmation::Settle => {
                serde_json::json!({ "kind": "setSettled", "settled": true })
            }
            TaskConfirmation::Delete => serde_json::json!({ "kind": "delete" }),
        };
        self.mutate_task(chat_id, intent, cx);
        cx.notify();
    }

    fn confirm_task_action(&mut self, cx: &mut Context<Self>) {
        let Some((chat_id, action)) = self.task_action_confirm.take() else {
            return;
        };
        if self.task_action_dont_show_again {
            self.settings.set_confirmation(action, false);
            settings::update(SavePolicy::Immediate, cx, |settings| {
                settings.set_confirmation(action, false);
            });
            if let Some(page) = &self.general_page {
                page.update(cx, |page, cx| page.sync(&self.settings, cx));
            }
        }
        self.execute_task_action(chat_id, action, cx);
    }

    fn request_sign_out(&mut self, cx: &mut Context<Self>) {
        if self.state.read(cx).workspace_scope != Some(WorkspaceScope::Synced) {
            return;
        }
        self.sync_flow = SyncFlow::SignOutConfirm;
        cx.notify();
    }

    fn confirm_sign_out(&mut self, cx: &mut Context<Self>) {
        self.start_local_runtime_transition(true, cx);
    }

    fn start_local_runtime_transition(&mut self, sign_out: bool, cx: &mut Context<Self>) {
        if self.runtime_change_task.is_some() {
            return;
        }
        self.cancel_history_flow(cx);
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.runtime_change_error = Some("Engine not connected".into());
            self.sync_flow = SyncFlow::SignedOutRestartRequired;
            cx.notify();
            return;
        };
        self.sync_flow = SyncFlow::SigningOut;
        self.runtime_change_error = None;
        let ipc_port = self.boot.ipc_port;
        let data_dir = self.data_dir.clone();
        let shutdown_dir = data_dir.clone();
        let transition = Tokio::spawn(cx, async move {
            if sign_out {
                engine
                    .client()
                    .call(methods::SIGN_OUT, serde_json::json!({}))
                    .await
                    .map_err(|error| format!("Sign out failed: {error}"))?;
            }
            stop_synced_runtime(engine, ipc_port, &shutdown_dir).await
        });
        let state = self.state.clone();
        let boot = self.boot.clone();
        self.runtime_change_task = Some(cx.spawn(async move |this, cx| {
            let result = match transition.await {
                Ok(result) => result,
                Err(error) => Err(error.to_string()),
            };
            this.update(cx, |shell, cx| {
                shell.runtime_change_task = None;
                match result {
                    Ok(()) => {
                        shell.sync_flow = SyncFlow::Idle;
                        shell.runtime_change_error = None;
                        shell.org = None;
                        shell.route = Route::Chat;
                        shell.space_boot_applied = false;
                        state.update(cx, |state, cx| state.prepare_runtime_replacement(cx));
                        AppState::bootstrap(state.clone(), boot, cx);
                    }
                    Err(error) => {
                        shell.sync_flow = SyncFlow::SignedOutRestartRequired;
                        shell.runtime_change_error = Some(error.into());
                        cx.notify();
                    }
                }
            })
            .ok();
        }));
        cx.notify();
    }

    fn cancel_auth_setup(&mut self, cx: &mut Context<Self>) {
        let local = self.state.read(cx).workspace_scope == Some(WorkspaceScope::Local);
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let pending_auth = self.auth_task.take();
        let pending_org = self.org.as_mut().and_then(|org| org.task.take());
        if local {
            self.sync_flow = SyncFlow::Canceling;
        }
        self.auth_task = Some(cx.spawn(async move |this, cx| {
            // Do not race SignOut against an exchange or organization write
            // that can still persist a session after credentials were cleared.
            if let Some(task) = pending_auth {
                task.await;
            }
            if let Some(task) = pending_org {
                task.await;
            }
            let result = engine
                .client()
                .call(methods::SIGN_OUT, serde_json::json!({}))
                .await;
            this.update(cx, |shell, cx| {
                match result {
                    Ok(_) => {
                        shell.org = None;
                        if local {
                            shell.sync_flow = SyncFlow::Idle;
                        }
                    }
                    Err(err) => {
                        if local {
                            shell.sync_flow = SyncFlow::Enabling;
                        }
                        shell.sidebar_notice =
                            Some(format!("Could not cancel sign-in: {err}").into());
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn postpone_sync_restart(&mut self, cx: &mut Context<Self>) {
        match self.sync_flow {
            SyncFlow::RestartPending { .. } => {
                self.sync_flow = SyncFlow::RestartPending { notice_open: false };
            }
            SyncFlow::SwitchOffer { .. } => {
                self.sync_flow = SyncFlow::SwitchOffer { notice_open: false };
            }
            SyncFlow::ImportFailed { .. } => {
                self.sync_flow = SyncFlow::ImportFailed { notice_open: false };
            }
            _ => return,
        }
        cx.notify();
    }

    fn reopen_sync_notice(&mut self, cx: &mut Context<Self>) {
        match self.sync_flow {
            SyncFlow::RestartPending { .. } => {
                self.sync_flow = SyncFlow::RestartPending { notice_open: true };
            }
            SyncFlow::SwitchOffer { .. } => {
                self.sync_flow = SyncFlow::SwitchOffer { notice_open: true };
            }
            SyncFlow::ImportFailed { .. } => {
                self.sync_flow = SyncFlow::ImportFailed { notice_open: true };
            }
            _ => return,
        }
        cx.notify();
    }

    /// The wizard's choice step chose a path: stop the local runtime, boot the
    /// synced one in-place (mirror of the sign-out transition), then let
    /// [`Self::drive_sync_switch`] run the import once the runtime is ready.
    /// Failure falls back to the quit-and-reopen dialog — the local profile is
    /// untouched, so the old path is always a safe exit.
    fn start_synced_switch(&mut self, import: bool, cx: &mut Context<Self>) {
        if self.runtime_change_task.is_some() {
            return;
        }
        self.cancel_history_flow(cx);
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.runtime_change_error = Some("Engine not connected".into());
            self.sync_flow = SyncFlow::RestartPending { notice_open: true };
            cx.notify();
            return;
        };
        self.sync_flow = SyncFlow::Switching { import };
        self.runtime_change_error = None;
        self.import_current = None;
        let ipc_port = self.boot.ipc_port;
        let data_dir = self.data_dir.clone();
        let transition = Tokio::spawn(cx, async move {
            stop_synced_runtime(engine, ipc_port, &data_dir).await
        });
        let state = self.state.clone();
        let boot = self.boot.clone();
        self.runtime_change_task = Some(cx.spawn(async move |this, cx| {
            let result = match transition.await {
                Ok(result) => result,
                Err(error) => Err(error.to_string()),
            };
            this.update(cx, |shell, cx| {
                shell.runtime_change_task = None;
                match result {
                    Ok(()) => {
                        // Keep `Switching { import }`: the state observer sees
                        // the replacement runtime reach Ready and advances the
                        // wizard from there.
                        shell.org = None;
                        shell.route = Route::Chat;
                        shell.space_boot_applied = false;
                        state.update(cx, |state, cx| state.prepare_runtime_replacement(cx));
                        AppState::bootstrap(state.clone(), boot, cx);
                    }
                    Err(error) => {
                        shell.sync_flow = SyncFlow::RestartPending { notice_open: true };
                        shell.runtime_change_error = Some(error.into());
                        cx.notify();
                    }
                }
            })
            .ok();
        }));
        cx.notify();
    }

    /// Advance the in-place switch when the replacement runtime lands: Ready +
    /// Synced starts the import stream (or finishes immediately when the user
    /// chose a fresh start); a runtime that comes back non-synced fell out of
    /// the swap — surface the quit fallback rather than pretend.
    fn drive_sync_switch(&mut self, cx: &mut Context<Self>) {
        let SyncFlow::Switching { import } = self.sync_flow else {
            return;
        };
        if self.runtime_change_task.is_some() {
            return; // still stopping the local runtime
        }
        let (ready, scope) = {
            let state = self.state.read(cx);
            (
                matches!(state.connection, ConnectionStatus::Ready),
                state.workspace_scope,
            )
        };
        if !ready {
            if let ConnectionStatus::Failed(error) = &self.state.read(cx).connection {
                self.sync_flow = SyncFlow::RestartPending { notice_open: true };
                self.runtime_change_error = Some(error.clone().into());
                cx.notify();
            }
            return;
        }
        match scope {
            Some(WorkspaceScope::Synced) => {
                if import {
                    self.spawn_local_import(cx);
                } else {
                    self.sync_flow = SyncFlow::Idle;
                    cx.notify();
                }
            }
            Some(_) => {
                self.sync_flow = SyncFlow::RestartPending { notice_open: true };
                self.runtime_change_error =
                    Some("The synced workspace did not come up — restart to finish.".into());
                cx.notify();
            }
            None => {}
        }
    }

    /// Subscribe to the engine's one-time import stream and mirror its
    /// progress into the wizard.
    fn spawn_local_import(&mut self, cx: &mut Context<Self>) {
        if self.import_task.is_some() {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.sync_flow = SyncFlow::RestartPending { notice_open: true };
            self.runtime_change_error = Some("Engine not connected".into());
            cx.notify();
            return;
        };
        self.sync_flow = SyncFlow::Importing { done: 0, total: 0 };
        self.runtime_change_error = None;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<serde_json::Value>();
        let stream = Tokio::spawn(cx, async move {
            let mut items = engine
                .client()
                .subscribe(methods::IMPORT_LOCAL_WORKSPACE, serde_json::json!({}))
                .await
                .map_err(|error| error.to_string())?;
            while let Some(item) = items.recv().await {
                let _ = tx.send(item);
            }
            Ok::<(), String>(())
        });
        self.import_task = Some(cx.spawn(async move |this, cx| {
            loop {
                let item = rx.recv().await;
                let ended = item.is_none();
                this.update(cx, |shell, cx| {
                    if let Some(item) = &item {
                        shell.apply_import_event(item, cx);
                    }
                    if ended {
                        shell.import_task = None;
                        shell.import_current = None;
                        // A stream that died before its summary is a failure —
                        // offer the in-place retry (idempotent).
                        if matches!(shell.sync_flow, SyncFlow::Importing { .. }) {
                            shell.sync_flow = SyncFlow::ImportFailed { notice_open: true };
                            shell.runtime_change_error =
                                Some("The import stream ended before it finished.".into());
                        }
                        cx.notify();
                    }
                })
                .ok();
                if ended {
                    break;
                }
            }
            if let Ok(Err(error)) = stream.await {
                this.update(cx, |shell, cx| {
                    shell.import_task = None;
                    if matches!(shell.sync_flow, SyncFlow::Importing { .. }) {
                        shell.sync_flow = SyncFlow::ImportFailed { notice_open: true };
                        shell.runtime_change_error = Some(error.into());
                        cx.notify();
                    }
                })
                .ok();
            }
        }));
        cx.notify();
    }

    fn apply_import_event(&mut self, item: &serde_json::Value, cx: &mut Context<Self>) {
        match item.get("kind").and_then(|k| k.as_str()) {
            Some("start") => {
                let total = item.get("chats").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                self.sync_flow = SyncFlow::Importing { done: 0, total };
            }
            Some("chat") => {
                let index = item.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                let total = item.get("total").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                self.import_current = item
                    .get("title")
                    .and_then(|v| v.as_str())
                    .map(|t| SharedString::from(t.to_string()));
                self.sync_flow = SyncFlow::Importing { done: index, total };
            }
            Some("summary") => {
                self.import_current = None;
                // A summary with errors is a FAILED import, however normally
                // the stream ended — never present a partial migration as
                // complete (the engine keeps collecting per-item failures
                // precisely so this can be surfaced).
                match import_summary_outcome(item) {
                    Ok((imported, skipped)) => {
                        self.sync_flow = SyncFlow::ImportDone { imported, skipped };
                    }
                    Err(message) => {
                        self.sync_flow = SyncFlow::ImportFailed { notice_open: true };
                        self.runtime_change_error = Some(message.into());
                    }
                }
            }
            _ => return,
        }
        cx.notify();
    }

    fn quit_for_runtime_change(&mut self, cx: &mut Context<Self>) {
        if !self.prepare_exit(PendingExit::RuntimeChange, cx) {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.runtime_change_error = Some("Engine not connected".into());
            cx.notify();
            return;
        };
        if engine.mode() == EngineMode::InProcess {
            crate::app_menus::quit_after_save(cx);
            return;
        }
        if self.runtime_change_task.is_some() {
            return;
        }

        self.runtime_change_error = None;
        let ipc_port = self.boot.ipc_port;
        let data_dir = self.data_dir.clone();
        let shutdown = Tokio::spawn(cx, async move {
            engine
                .client()
                .call(methods::STOP_ENGINE, serde_json::json!({}))
                .await
                .map_err(|err| err.to_string())?;
            wait_for_remote_engine_shutdown(ipc_port, &data_dir, RUNTIME_CHANGE_TIMEOUT).await
        });
        self.runtime_change_task = Some(cx.spawn(async move |this, cx| {
            let result = match shutdown.await {
                Ok(result) => result,
                Err(err) => Err(err.to_string()),
            };
            this.update(cx, |shell, cx| {
                shell.runtime_change_task = None;
                match result {
                    Ok(_) => {
                        if shell.prepare_quit(cx) {
                            crate::app_menus::quit_after_save(cx);
                        }
                    },
                    Err(err) => {
                        shell.runtime_change_error = Some(format!(
                            "Could not stop the remote engine: {err}. Run `luna daemon stop`, then quit and reopen LUNA."
                        ).into());
                        cx.notify();
                    }
                }
            })
            .ok();
        }));
        cx.notify();
    }

    fn start_sign_in(&mut self, cx: &mut Context<Self>) {
        let scope = self.state.read(cx).workspace_scope;
        if scope == Some(WorkspaceScope::Development) {
            return;
        }
        if scope == Some(WorkspaceScope::Local) {
            self.sync_flow = SyncFlow::Enabling;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.auth_task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(methods::SIGN_IN, serde_json::json!({}))
                .await;
            this.update(cx, |shell, cx| match result {
                Ok(value) => {
                    if let Some(url) = value.get("url").and_then(|u| u.as_str()) {
                        cx.open_url(url);
                    }
                    cx.notify();
                }
                Err(err) => {
                    if scope == Some(WorkspaceScope::Local) && shell.sync_flow == SyncFlow::Enabling
                    {
                        shell.sync_flow = SyncFlow::Idle;
                    }
                    shell.sidebar_notice = Some(format!("Sign in failed: {err}").into());
                    cx.notify();
                }
            })
            .ok();
        }));
        cx.notify();
    }

    // ---- org gate ----

    fn ensure_org_ui(&mut self, cx: &mut Context<Self>) {
        if self.org.is_some() {
            return;
        }
        let name_input = cx.new(|cx| {
            ComposerInput::new("Workspace name", cx).with_accessibility_role(gpui::Role::TextInput)
        });
        let events = cx.subscribe(&name_input, |this: &mut Shell, _, event, cx| {
            if matches!(event, ComposerInputEvent::Submitted) {
                this.create_org(cx);
            }
        });
        self.org = Some(OrgGateUi {
            name_input,
            orgs: Loadable::Idle,
            submitting: false,
            error: None,
            task: None,
            _events: events,
        });
        self.load_orgs(cx);
    }

    fn load_orgs(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let Some(org) = self.org.as_mut() else { return };
        org.orgs = Loadable::Loading;
        org.task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(methods::LIST_ORGS, serde_json::json!({}))
                .await;
            this.update(cx, |shell, cx| {
                if let Some(org) = shell.org.as_mut() {
                    org.orgs = match result {
                        Ok(value) => Loadable::Ready(sort_memberships(parse_orgs(&value))),
                        Err(err) => Loadable::Error(err.to_string()),
                    };
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn create_org(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let Some(org) = self.org.as_mut() else { return };
        if org.submitting {
            return;
        }
        let name = org.name_input.read(cx).text().trim().to_string();
        if !org_name_valid(&name) {
            org.error = Some("Enter a workspace name".into());
            cx.notify();
            return;
        }
        org.submitting = true;
        org.error = None;
        org.task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(methods::CREATE_ORG, serde_json::json!({ "name": name }))
                .await;
            this.update(cx, |shell, cx| {
                if let Some(org) = shell.org.as_mut() {
                    org.submitting = false;
                    if let Err(err) = result {
                        org.error = Some(format!("{err}").into());
                    }
                    // Success: the AuthStatus stream flips to SignedIn and the
                    // gate falls away on its own.
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn select_org(&mut self, organization_id: String, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let Some(org) = self.org.as_mut() else { return };
        org.submitting = true;
        org.error = None;
        org.task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(
                    methods::SELECT_ORG,
                    serde_json::json!({ "organizationId": organization_id }),
                )
                .await;
            this.update(cx, |shell, cx| {
                if let Some(org) = shell.org.as_mut() {
                    org.submitting = false;
                    if let Err(err) = result {
                        org.error = Some(format!("{err}").into());
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    // ---- render pieces ----

    fn tween_elapsed(&self, started: std::time::Instant) -> Duration {
        self.render_time
            .unwrap_or_else(std::time::Instant::now)
            .saturating_duration_since(started)
    }

    /// Evaluate a width tween at the frame time (see [`WidthTween`]).
    /// Mid-flight: eased 200ms lerp, and `motion_active` is flagged so render
    /// schedules the next animation frame. Finished, stale, absent, or under
    /// reduced motion: exactly `target`. Honors `LUNA_MOTION_SCALE`.
    fn eval_tween(&self, tween: Option<WidthTween>, target: f32) -> f32 {
        let Some(WidthTween { from, to, started }) = tween else {
            return target;
        };
        if self.reduced_motion {
            return target;
        }
        let total = RESIZE.total().mul_f32(motion::speed_scale());
        let raw = self.tween_elapsed(started).as_secs_f32() / total.as_secs_f32();
        if raw >= 1.0 {
            return target;
        }
        self.motion_active.set(true);
        motion::lerp(from, to, RESIZE.progress(raw))
    }

    fn eval_resize_edge_bounce(
        &self,
        bounce: Option<motion::ResizeEdgeBounce>,
        enabled: bool,
    ) -> f32 {
        let Some(bounce) = bounce else {
            return 0.0;
        };
        if self.reduced_motion || !enabled {
            return 0.0;
        }
        let total =
            Duration::from_millis(motion::RESIZE_EDGE_BOUNCE_MS).mul_f32(motion::speed_scale());
        let raw = self.tween_elapsed(bounce.started).as_secs_f32() / total.as_secs_f32();
        if raw >= 1.0 {
            return 0.0;
        }
        self.motion_active.set(true);
        motion::resize_bounce_offset(bounce.edge, raw)
    }

    pub(super) fn sidebar_now(&self) -> f32 {
        self.eval_tween(self.sidebar_tween, self.sidebar_target())
            + self
                .eval_resize_edge_bounce(self.sidebar_edge_bounce, !self.settings.sidebar_collapsed)
    }

    fn right_now(&self, cx: &App) -> f32 {
        self.eval_tween(self.right_tween, self.right_target(cx))
            + self.eval_resize_edge_bounce(
                self.right_edge_bounce,
                self.right_pane_open(cx) && !self.right_pane_expanded,
            )
    }

    fn tween_active(&self, tween: Option<WidthTween>) -> bool {
        tween.is_some_and(|tween| {
            !self.reduced_motion
                && self.tween_elapsed(tween.started) < RESIZE.total().mul_f32(motion::speed_scale())
        })
    }

    fn active_tween_endpoints(&self, tween: Option<WidthTween>) -> Option<(f32, f32)> {
        tween
            .filter(|transition| {
                !self.reduced_motion
                    && self.tween_elapsed(transition.started)
                        < RESIZE.total().mul_f32(motion::speed_scale())
            })
            .map(|transition| (transition.from, transition.to))
    }

    /// Right-anchored variant for the changes pane. The outer width follows the
    /// existing shell tween, while descendants retain the larger endpoint's
    /// geometry for that 200ms transition. This mirrors the sidebar's stable
    /// inner/clipped outer behavior without changing the center column's
    /// upstream flex layout.
    fn right_pane_container(
        &self,
        tween: Option<WidthTween>,
        target: f32,
        edge_offset: f32,
        inner: AnyElement,
    ) -> AnyElement {
        let takeover_width = self
            .active_tween_endpoints(self.right_takeover_content_tween)
            .map(|_| self.eval_tween(self.right_takeover_content_tween, target));
        let content_width =
            right_panel_content_width(target, self.active_tween_endpoints(tween), takeover_width)
                + edge_offset;
        div()
            .h_full()
            .flex_none()
            .relative()
            .overflow_hidden()
            .w(px(self.eval_tween(tween, target) + edge_offset))
            .child(
                div()
                    .absolute()
                    .top_0()
                    .right_0()
                    .h_full()
                    .w(px(content_width))
                    .child(inner),
            )
            .into_any_element()
    }

    /// The animated spacer clearing the macOS traffic lights ahead of a
    /// titlebar control cluster. Fullscreen toggles tween the cluster start
    /// over 200ms ease-out ([`RESIZE`]; reduced motion snaps).
    /// `None` off macOS — no phantom flex child.
    fn titlebar_spacer(&self, container_pad: f32) -> Option<AnyElement> {
        if !cfg!(target_os = "macos") {
            return None;
        }
        let fullscreen = self.fullscreen.unwrap_or(false);
        // The tween runs in cluster-start coordinates; the spacer is that
        // minus the container's own padding.
        let start = self.eval_tween(self.titlebar_tween, titlebar_cluster_start(fullscreen));
        let width = (start - container_pad).max(0.0);
        Some(div().flex_none().h_full().w(px(width)).into_any_element())
    }

    /// The header's content row with the animated left inset — the native port
    /// of luna __root.tsx `transition-[padding-left] duration-200 ease-out` +
    /// `style={{ paddingLeft: headerInset }}`: on sidebar toggles (and macOS
    /// fullscreen flips) the SAME element's padding tweens, so the title
    /// glides to its new x-position. Route changes SNAP: the tween is killed
    /// by every route transition (luna remounts the keyed header variants —
    /// instant swap, zero horizontal motion).
    /// Where unified-titlebar content (tabs / the settings label) starts: past
    /// the traffic lights + control cluster, riding the fullscreen inset tween.
    pub(super) fn title_bar_content_start(&self) -> f32 {
        let fullscreen = self.fullscreen.unwrap_or(false);
        let is_macos = cfg!(target_os = "macos");
        let cluster = self.eval_tween(
            self.titlebar_tween,
            cluster_buttons_start(is_macos, fullscreen, self.linux_left_caption_count()),
        );
        cluster + CLUSTER_BUTTONS_WIDTH + TITLEBAR_IDENTITY_GAP
    }

    /// The unified window titlebar: chat → the session tab strip; settings →
    /// the section label. Full-width on the glass shell; the traffic lights
    /// and control cluster overlay its left end.
    fn render_title_bar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        match self.route {
            Route::Chat => self.render_session_title_bar(cx),
            Route::Settings(_) => {
                let inner = div()
                    .size_full()
                    .flex()
                    .items_center()
                    .pt(px(Theme::TITLEBAR_TOP_PAD))
                    .pl(px(self.title_bar_content_start()))
                    .pr(px(self.titlebar_right_pad(TITLEBAR_ACTION_EDGE_INSET)));
                let bar = div().h(px(Theme::TITLEBAR_HEIGHT)).flex_none().child(inner);
                self.titlebar_drag_region("settings-header-titlebar", bar, cx)
                    .into_any_element()
            }
        }
    }

    /// Make a titlebar strip drag the window — zed's platform-titlebar
    /// pattern (luna's `.drag` region): mark it a [`WindowControlArea::Drag`]
    /// (macOS app-owned titlebar), hand the drag to the compositor once the
    /// pointer moves with the button down, and double-click zooms.
    fn titlebar_drag_region(
        &self,
        id: &'static str,
        el: gpui::Div,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        el.id(id)
            .window_control_area(WindowControlArea::Drag)
            .on_mouse_down_out(cx.listener(|this, _, _, _| this.titlebar_should_move = false))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.titlebar_should_move = false),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.titlebar_should_move = true),
            )
            // Hand the drag to the compositor only while the button is
            // actually held (`pressed_button` guard): on macOS
            // `start_window_move` runs AppKit's NATIVE drag session
            // (`performWindowDragWithEvent:`), and AppKit resolves a quick
            // second click inside that session as a titlebar double-click —
            // system zoom — natively, beyond gpui's reach. Without the guard a
            // stale `titlebar_should_move` (armed by a down whose bubble was
            // later stopped) would start that session from a mere hover move
            // between the two clicks of a double-click.
            .on_mouse_move(
                cx.listener(|this, event: &gpui::MouseMoveEvent, window, _| {
                    if this.titlebar_should_move && event.pressed_button == Some(MouseButton::Left)
                    {
                        this.titlebar_should_move = false;
                        window.start_window_move();
                    }
                }),
            )
            .on_click(|event, window, _| {
                if event.click_count() == 2 {
                    if cfg!(target_os = "macos") {
                        // Native titlebar double-click action (zoom/minimize
                        // per system preference).
                        window.titlebar_double_click();
                    } else {
                        window.zoom_window();
                    }
                }
            })
    }

    /// The ONE top-left window-control cluster (sidebar toggle + back/forward —
    /// luna window-controls.tsx): rendered once, in a paint-only overlay layer
    /// pinned at the window's top-left, ABOVE the sidebar and headers. The
    /// sidebar width animates *beneath* it, so the buttons keep their element
    /// identity and never move or remount on collapse/expand; only the
    /// fullscreen traffic-light inset tweens (the animated spacer). The
    /// container has no id/listeners — everything between the buttons falls
    /// through to the titlebar drag strips below.
    fn render_titlebar_cluster(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let can_back = self.nav.can_back();
        let can_forward = self.nav.can_forward();
        // The titlebar is the single owner of the new-session action in both
        // sidebar states. Hide it on the new-session canvas: opening another
        // blank canvas from an already blank canvas has no effect and used to
        // leave two competing + placements across the responsive variants.
        let plus_alpha = self.titlebar_plus_alpha(cx);
        let show_plus = plus_alpha > 0.01;
        let island_target = if matches!(self.route, Route::Chat)
            && self.state.read(cx).selected_chat.is_none()
            && self.settings.sidebar_collapsed
            && settings::current(cx)
                .new_thread_composer_background
                .as_ref()
                .is_some_and(|background| std::path::Path::new(&background.path).is_file())
        {
            1.0
        } else {
            0.0
        };
        // Persistent manual tween: reversals start from the painted value,
        // initial presentation is settled, and reduced motion snaps.
        match self.titlebar_island {
            None => self.titlebar_island = Some(WidthTween::new(island_target, island_target)),
            Some(previous) if previous.to != island_target => {
                let from = self.eval_tween(Some(previous), previous.to);
                self.titlebar_island = Some(WidthTween::new(from, island_target));
            }
            _ => {}
        }
        let island = self.eval_tween(self.titlebar_island, island_target);
        let (island_top, island_height) = titlebar_island_vertical_geometry(island);
        div()
            .absolute()
            .top_0()
            .left_0()
            .h(px(Theme::TITLEBAR_HEIGHT))
            .flex()
            .flex_row()
            .items_center()
            .pt(px(Theme::TITLEBAR_TOP_PAD))
            .px(px(TITLEBAR_CLUSTER_PAD))
            .child(
                div()
                    .absolute()
                    .left(px(6.0))
                    .right_0()
                    .top(px(island_top))
                    .h(px(island_height))
                    .opacity(island)
                    .children((island > 0.001).then(|| {
                        crate::frost::frosted(
                            12.0,
                            20.0,
                            div()
                                .size_full()
                                .rounded(px(12.0))
                                .bg(theme.glass_overlay())
                                .shadow_sm(),
                        )
                    })),
            )
            .children(self.titlebar_spacer(TITLEBAR_CLUSTER_PAD))
            // Left-side Linux captions (GNOME `close:…` layouts): the
            // root-level caption overlay owns the buttons; the cluster row
            // just starts past them, at the shared 2px rhythm.
            .children((self.linux_left_caption_count() > 0).then(|| {
                div()
                    .flex_none()
                    .h_full()
                    .w(px(caption_buttons_width(self.linux_left_caption_count())))
            }))
            .child(window_control_button(
                "toggle-sidebar",
                icons::SIDEBAR_MINIMALISTIC_LEFT,
                &theme,
                cx.listener(|this, _, _, cx| this.toggle_sidebar(cx)),
            ))
            .child(
                div()
                    .ml(px(TITLEBAR_GROUP_GAP))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(TITLEBAR_CONTROL_GAP))
                    .child(nav_history_button(
                        "nav-back",
                        icons::ARROW_LEFT,
                        can_back,
                        &theme,
                        cx.listener(|this, _, _, cx| this.navigate_back(cx)),
                    ))
                    .child(nav_history_button(
                        "nav-forward",
                        icons::ARROW_RIGHT,
                        can_forward,
                        &theme,
                        cx.listener(|this, _, _, cx| this.navigate_forward(cx)),
                    )),
            )
            .children(show_plus.then(|| {
                div()
                    .flex_none()
                    .ml(px(TITLEBAR_GROUP_GAP))
                    .opacity(plus_alpha)
                    .child(window_control_button(
                        "titlebar-new-session",
                        icons::PLUS,
                        &theme,
                        cx.listener(|this, _, _, cx| this.open_new_session(cx)),
                    ))
            }))
            .into_any_element()
    }

    /// The titlebar owns new-session creation regardless of sidebar state. It
    /// is useful only while an existing session is selected.
    pub(super) fn titlebar_plus_alpha(&self, cx: &App) -> f32 {
        titlebar_new_session_alpha(
            matches!(self.route, Route::Chat),
            self.state.read(cx).selected_chat.is_some(),
        )
    }

    /// Native Windows caption controls integrated into Luna's unified
    /// titlebar. `WindowControlArea` maps these hit targets to HTMINBUTTON,
    /// HTMAXBUTTON, and HTCLOSE, so Windows owns their behavior (including
    /// Snap Layouts) while GPUI renders the system Segoe caption glyphs.
    fn render_windows_caption_controls(&self, window: &Window, cx: &App) -> Option<AnyElement> {
        if !cfg!(target_os = "windows") {
            return None;
        }

        let theme = Theme::of(cx);
        let (maximize_id, maximize_glyph) = if window.is_maximized() {
            ("window-restore", "\u{e923}")
        } else {
            ("window-maximize", "\u{e922}")
        };
        Some(
            div()
                .id("windows-window-controls")
                .absolute()
                .top_0()
                .right_0()
                .h(px(Theme::TITLEBAR_HEIGHT))
                .flex()
                .flex_row()
                .font_family("Segoe Fluent Icons")
                .child(windows_caption_button(
                    "window-minimize",
                    "\u{e921}",
                    WindowControlArea::Min,
                    theme,
                    false,
                ))
                .child(windows_caption_button(
                    maximize_id,
                    maximize_glyph,
                    WindowControlArea::Max,
                    theme,
                    false,
                ))
                .child(windows_caption_button(
                    "window-close",
                    "\u{e8bb}",
                    WindowControlArea::Close,
                    theme,
                    true,
                ))
                .into_any_element(),
        )
    }

    /// Which caption buttons luna itself must draw on Linux: under
    /// client-side decorations (the Wayland default) nobody else will —
    /// without these the window has NO minimize/maximize/close at all.
    /// Server-side decorations (X11 WMs, KDE with SSD) already draw real
    /// buttons, so `None` there. The desktop's layout (GNOME's
    /// `button-layout` gsetting via `cx.button_layout()`) decides side and
    /// order — min/max/close on the right by default; controls the
    /// compositor can't do (e.g. minimize on some Wayland compositors) drop
    /// out, close always stays.
    #[cfg(target_os = "linux")]
    fn resolve_linux_captions(window: &Window, cx: &App) -> Option<gpui::WindowButtonLayout> {
        use gpui::{MAX_BUTTONS_PER_SIDE, WindowButton, WindowButtonLayout};
        if !matches!(
            window.window_decorations(),
            gpui::Decorations::Client { .. }
        ) {
            return None;
        }
        let layout = cx
            .button_layout()
            .unwrap_or_else(WindowButtonLayout::linux_default);
        let supported = window.window_controls();
        let filter_side = |side: [Option<WindowButton>; MAX_BUTTONS_PER_SIDE]| {
            let mut out = [None; MAX_BUTTONS_PER_SIDE];
            let mut i = 0;
            for button in side.into_iter().flatten() {
                let keep = match button {
                    WindowButton::Minimize => supported.minimize,
                    WindowButton::Maximize => supported.maximize,
                    WindowButton::Close => true,
                };
                if keep {
                    out[i] = Some(button);
                    i += 1;
                }
            }
            out
        };
        let layout = WindowButtonLayout {
            left: filter_side(layout.left),
            right: filter_side(layout.right),
        };
        (layout.left[0].is_some() || layout.right[0].is_some()).then_some(layout)
    }

    #[cfg(not(target_os = "linux"))]
    fn resolve_linux_captions(_window: &Window, _cx: &App) -> Option<gpui::WindowButtonLayout> {
        None
    }

    pub(super) fn linux_left_caption_count(&self) -> usize {
        self.linux_captions
            .map_or(0, |l| l.left.iter().flatten().count())
    }

    pub(super) fn linux_right_caption_count(&self) -> usize {
        self.linux_captions
            .map_or(0, |l| l.right.iter().flatten().count())
    }

    /// Right padding titlebar content needs to clear the platform's caption
    /// controls (native Windows cluster / luna-drawn Linux buttons).
    pub(super) fn titlebar_right_pad(&self, base: f32) -> f32 {
        titlebar_right_padding(
            cfg!(target_os = "windows"),
            self.linux_right_caption_count(),
            base,
        )
    }

    /// Luna-drawn Linux caption controls, one overlay per populated side.
    /// Shell-level chrome like the Windows cluster: mounted at the root so
    /// they stay above the splash and every auth/org/error gate.
    fn render_linux_caption_controls(&self, window: &Window, cx: &App) -> Vec<AnyElement> {
        let Some(layout) = self.linux_captions else {
            return Vec::new();
        };
        let theme = Theme::of(cx);
        let is_maximized = window.is_maximized();
        // Ids can be per-button (not per-side): the layout parser dedups, so
        // a button never appears on both sides at once.
        let strip = |buttons: &[Option<gpui::WindowButton>]| {
            div()
                .absolute()
                .top_0()
                .h(px(Theme::TITLEBAR_HEIGHT))
                .flex()
                .flex_row()
                .items_center()
                .pt(px(Theme::TITLEBAR_TOP_PAD))
                .gap(px(2.0))
                .px(px(10.0))
                .children(buttons.iter().flatten().map(|button| {
                    match button {
                        gpui::WindowButton::Minimize => linux_caption_button(
                            "window-minimize",
                            icons::WINDOW_MINIMIZE,
                            false,
                            theme,
                            |_, window, _| window.minimize_window(),
                        )
                        .into_any_element(),
                        gpui::WindowButton::Maximize => {
                            let (id, icon_path) = if is_maximized {
                                ("window-restore", icons::WINDOW_RESTORE)
                            } else {
                                ("window-maximize", icons::WINDOW_MAXIMIZE)
                            };
                            linux_caption_button(id, icon_path, false, theme, |_, window, _| {
                                window.zoom_window()
                            })
                            .into_any_element()
                        }
                        gpui::WindowButton::Close => linux_caption_button(
                            "window-close",
                            icons::CLOSE,
                            true,
                            theme,
                            |_, window, _| window.remove_window(),
                        )
                        .into_any_element(),
                    }
                }))
        };
        let mut out = Vec::new();
        if layout.left[0].is_some() {
            out.push(strip(&layout.left).left_0().into_any_element());
        }
        if layout.right[0].is_some() {
            out.push(strip(&layout.right).right_0().into_any_element());
        }
        out
    }

    /// Linux CSD chrome state: which edges the compositor has taken (tiled or
    /// maximized). A free edge can carry a resize strip; a fully floating
    /// window (nothing tiled) also gets rounded corners. Server decorations
    /// (e.g. KDE SSD) hand the frame back to the compositor — no CSD chrome.
    #[cfg(target_os = "linux")]
    fn linux_csd_edges(window: &Window) -> (bool, bool, bool, bool) {
        match window.window_decorations() {
            gpui::Decorations::Client { tiling } => {
                (!tiling.top, !tiling.bottom, !tiling.left, !tiling.right)
            }
            gpui::Decorations::Server => (false, false, false, false),
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn linux_csd_edges(_window: &Window) -> (bool, bool, bool, bool) {
        (false, false, false, false)
    }

    /// True when the Linux window floats (CSD, nothing tiled or maximized) —
    /// the state that gets macOS-style rounded corners.
    fn linux_window_floating(window: &Window) -> bool {
        let (top, bottom, left, right) = Self::linux_csd_edges(window);
        top && bottom && left && right && cfg!(target_os = "linux")
    }

    /// The corner radius chrome layers must PAINT for this frame. gpui's
    /// content masks are rectangles — `.rounded()` never clips children, so
    /// every full-bleed layer that can occupy a window corner rounds its own
    /// background with this radius (the layer beneath shows through the
    /// curve, down to the transparent window corners). Zero everywhere the
    /// window must be square: macOS rounds via the window server, and a
    /// tiled/maximized Linux window sits flush with the screen.
    pub(crate) fn window_corner_radius(window: &Window) -> f32 {
        if Self::linux_window_floating(window) {
            LINUX_WINDOW_CORNER_RADIUS
        } else {
            0.0
        }
    }

    /// Invisible edge strips that turn pointer presses into compositor
    /// resizes — the CSD contract on X11 and Wayland, where the platform
    /// draws no frame for us. Each strip mounts only along a free edge (a
    /// maximized or snapped window exposes just its untiled edges, like
    /// GTK). The strips paint last so they sit above all content chrome.
    fn render_linux_resize_borders(window: &Window) -> Vec<AnyElement> {
        let (top, bottom, left, right) = Self::linux_csd_edges(window);
        const EDGE: f32 = 6.0;
        const CORNER: f32 = 14.0;
        let mut out = Vec::new();
        macro_rules! strip {
            ($position:expr, $cursor:ident, $edge:expr) => {
                out.push(
                    $position
                        .$cursor()
                        .on_mouse_down(MouseButton::Left, |_, window, cx| {
                            cx.stop_propagation();
                            window.start_window_resize($edge);
                        })
                        .into_any_element(),
                );
            };
        }
        // Cardinal edges: full-length strips inset by the corner squares.
        if top {
            strip!(
                div()
                    .absolute()
                    .top_0()
                    .left(px(CORNER))
                    .right(px(CORNER))
                    .h(px(EDGE)),
                cursor_ns_resize,
                gpui::ResizeEdge::Top
            );
        }
        if bottom {
            strip!(
                div()
                    .absolute()
                    .bottom_0()
                    .left(px(CORNER))
                    .right(px(CORNER))
                    .h(px(EDGE)),
                cursor_ns_resize,
                gpui::ResizeEdge::Bottom
            );
        }
        if left {
            strip!(
                div()
                    .absolute()
                    .left_0()
                    .top(px(CORNER))
                    .bottom(px(CORNER))
                    .w(px(EDGE)),
                cursor_ew_resize,
                gpui::ResizeEdge::Left
            );
        }
        if right {
            strip!(
                div()
                    .absolute()
                    .right_0()
                    .top(px(CORNER))
                    .bottom(px(CORNER))
                    .w(px(EDGE)),
                cursor_ew_resize,
                gpui::ResizeEdge::Right
            );
        }
        // Corners: squares over both adjacent edges, diagonal cursors.
        if top && left {
            strip!(
                div().absolute().top_0().left_0().size(px(CORNER)),
                cursor_nwse_resize,
                gpui::ResizeEdge::TopLeft
            );
        }
        if top && right {
            strip!(
                div().absolute().top_0().right_0().size(px(CORNER)),
                cursor_nesw_resize,
                gpui::ResizeEdge::TopRight
            );
        }
        if bottom && left {
            strip!(
                div().absolute().bottom_0().left_0().size(px(CORNER)),
                cursor_nesw_resize,
                gpui::ResizeEdge::BottomLeft
            );
        }
        if bottom && right {
            strip!(
                div().absolute().bottom_0().right_0().size(px(CORNER)),
                cursor_nwse_resize,
                gpui::ResizeEdge::BottomRight
            );
        }
        out
    }

    fn render_sidebar(&mut self, _cx: &mut Context<Self>) -> AnyElement {
        // The sidebar is part of the resolved theme. A second fixed-Luna
        // palette here made imported families look split in half and froze
        // activity/glyph personality independently of the selected variant.
        let inner = self.sidebar_pane.clone().cached(
            gpui::StyleRefinement::default()
                .w(px(self.settings.sidebar_width))
                .h_full()
                .flex_none(),
        );
        // Transparent — the sidebar sits directly on the frost shell; the main
        // card's own border provides the separation. The content row spans the
        // full window height (the titlebar overlays it), so the column pads
        // itself below the chrome.
        div()
            .h_full()
            .flex_none()
            .overflow_hidden()
            .w(px(self.sidebar_now()))
            .child(div().h_full().pt(px(Theme::TITLEBAR_HEIGHT)).child(inner))
            .into_any_element()
    }

    /// Settings-mode sidebar (luna settings-sidebar.tsx): window-control
    /// strip, "Settings" heading, icon section rows styled like session rows,
    /// and a Back row pinned to the bottom.
    fn render_settings_nav(
        &mut self,
        section: SettingsSection,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let focus_border = theme.border_strong;
        let account_row = account_action_for_scope(
            self.state.read(cx).workspace_scope,
            self.sync_flow,
        )
        .and_then(|action| {
            let (label, mark) = match action {
                AccountAction::EnableSync => ("Enable sync", icons::GLOBAL),
                AccountAction::RestartPending => ("Finish sync setup", icons::RESTART),
                AccountAction::SignOut => ("Sign out", icons::LOGOUT_2),
                AccountAction::SyncInProgress => return None,
            };
            Some(
                div()
                    .id("settings-account-action")
                    .role(gpui::Role::Button)
                    .aria_label(label)
                    .tab_index(0)
                    .focus_visible(move |style| style.border_2().border_color(focus_border))
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .rounded(px(8.0))
                    .px(px(Theme::SPACE_SM))
                    .py(px(6.0))
                    .text_size(crate::typography::ui_rems(13.0))
                    .text_color(theme.text_muted)
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.glass_hover()).text_color(theme.text))
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.activate_account_action(action, cx)),
                    )
                    .on_key_down(cx.listener(move |this, event: &gpui::KeyDownEvent, _, cx| {
                        if is_settings_nav_activation(&event.keystroke.key, event.is_held) {
                            cx.stop_propagation();
                            this.activate_account_action(action, cx);
                        }
                    }))
                    .child(icon(mark).size(px(16.0)).text_color(theme.text_muted))
                    .child(SharedString::from(label))
                    .into_any_element(),
            )
        });
        let section_icon = |item: SettingsSection| match item {
            SettingsSection::General => icons::SETTINGS_MINIMALISTIC,
            SettingsSection::Projects => icons::FOLDER_WITH_FILES,
            SettingsSection::Devices => icons::MONITOR,
            SettingsSection::Providers => icons::WIDGET,
            SettingsSection::Appearance => icons::TUNING,
            SettingsSection::Files => icons::FOLDER,
            SettingsSection::Backups => icons::ARCHIVE_UP_MINIMALISTIC,
            SettingsSection::Notifications => icons::BELL,
            SettingsSection::Shortcuts => icons::KEYBOARD,
            SettingsSection::Appshots => icons::MONITOR,
            SettingsSection::Archived => icons::ARCHIVE_MINIMALISTIC,
            SettingsSection::Usage => icons::CHART,
            SettingsSection::Updates => icons::RESTART,
        };
        // Match the user's dragged sidebar width — the pane container clips to
        // it, so a hardcoded default here left hover washes stopping short of
        // the sidebar's right edge (user-reported). Device identity lives on
        // the Accounts page now — the one surface where the device matters.
        div()
            .w(px(self.settings.sidebar_width))
            .h_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .px(px(Theme::SPACE_SM))
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .px(px(Theme::SPACE_SM))
                            .pt(px(12.0))
                            .pb(px(4.0))
                            .text_size(crate::typography::ui_rems(11.0))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme.readable_muted(0.6))
                            .child(SharedString::from("Settings")),
                    )
                    .child(
                        div()
                            .id("settings-sections-scroll")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .track_scroll(&self.settings_nav_scroll)
                            .child(
                                div().flex().flex_col().gap(px(2.0)).children(
                                    SettingsSection::ALL
                                        .into_iter()
                                        .filter(|item| {
                                            *item != SettingsSection::Appshots
                                                || crate::appshots::is_desktop()
                                        })
                                        .map(|item| {
                                            let selected = item == section;
                                            div()
                                            .id(SharedString::from(format!(
                                                "settings-nav-{}",
                                                item.label()
                                            )))
                                            .role(gpui::Role::Button)
                                            .aria_label(format!("{} settings", item.label()))
                                            .aria_selected(selected)
                                            .tab_index(0)
                                            .focus_visible(move |style| {
                                                style.border_2().border_color(focus_border)
                                            })
                                            .when(
                                                matches!(
                                                    item,
                                                    SettingsSection::Files
                                                        | SettingsSection::Archived
                                                ),
                                                |el| el.mt(px(Theme::SPACE_LG)),
                                            )
                                            .flex()
                                            .flex_row()
                                            .items_center()
                                            .gap(px(8.0))
                                            .rounded(px(8.0))
                                            .px(px(Theme::SPACE_SM))
                                            .py(px(6.0))
                                            .text_size(crate::typography::ui_rems(13.0))
                                            .when(selected, |el| {
                                                // Same tokens as the main sidebar's session
                                                // rows — the two sidebars must feel alike.
                                                el.bg(crate::theme::glass_selected_bg())
                                                    .font_weight(gpui::FontWeight::MEDIUM)
                                            })
                                            .text_color(if selected {
                                                theme.text
                                            } else {
                                                theme.text_muted
                                            })
                                            .cursor_pointer()
                                            .hover(|s| {
                                                s.bg(theme.glass_hover()).text_color(theme.text)
                                            })
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.open_settings(item, cx)
                                            }))
                                            .on_key_down(cx.listener(
                                                move |this, event: &gpui::KeyDownEvent, _, cx| {
                                                    if is_settings_nav_activation(
                                                        &event.keystroke.key,
                                                        event.is_held,
                                                    ) {
                                                        cx.stop_propagation();
                                                        this.open_settings(item, cx);
                                                    }
                                                },
                                            ))
                                            .child(
                                                icon(section_icon(item))
                                                    .size(px(16.0))
                                                    .text_color(theme.text_muted),
                                            )
                                            .child(SharedString::from(item.label()))
                                        }),
                                ),
                            ),
                    ),
            )
            .when_some(account_row, |el, row| {
                el.child(
                    div()
                        .flex_none()
                        .px(px(Theme::SPACE_SM))
                        .pb(px(4.0))
                        .child(row),
                )
            })
            // Back pinned to the bottom (luna settings-sidebar.tsx).
            .child(
                div()
                    .flex_none()
                    .px(px(Theme::SPACE_SM))
                    .pb(px(12.0))
                    .child(
                        div()
                            .id("settings-back")
                            .role(gpui::Role::Button)
                            .aria_label("Back to chat")
                            .tab_index(0)
                            .focus_visible(move |style| style.border_2().border_color(focus_border))
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(6.0))
                            .rounded(px(8.0))
                            .px(px(Theme::SPACE_SM))
                            .py(px(6.0))
                            .text_size(crate::typography::ui_rems(13.0))
                            .text_color(theme.text_muted)
                            .cursor_pointer()
                            .hover(|s| s.bg(theme.glass_hover()).text_color(theme.text))
                            .on_click(cx.listener(|this, _, _, cx| this.close_settings(cx)))
                            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                                if is_settings_nav_activation(&event.keystroke.key, event.is_held) {
                                    cx.stop_propagation();
                                    this.close_settings(cx);
                                }
                            }))
                            .child(
                                // AltArrowLeft chevron (luna settings-sidebar.tsx),
                                // not the straight history arrow.
                                icon(icons::ALT_ARROW_LEFT)
                                    .size(px(16.0))
                                    .text_color(theme.text_muted),
                            )
                            .child(SharedString::from("Back")),
                    ),
            )
            .into_any_element()
    }

    fn activate_account_action(&mut self, action: AccountAction, cx: &mut Context<Self>) {
        match action {
            AccountAction::EnableSync => self.start_sign_in(cx),
            AccountAction::RestartPending => self.reopen_sync_notice(cx),
            AccountAction::SignOut => self.request_sign_out(cx),
            AccountAction::SyncInProgress => {}
        }
    }

    /// One session row: context + status on line one, harness + title on line
    /// two, and source metadata below. Working uses the live thread glyph in
    /// the status corner. Click selects; right-click opens the context menu.
    #[allow(clippy::too_many_arguments)]
    fn render_chat_row(
        &self,
        id: String,
        title: SharedString,
        time_ago: SharedString,
        space_name: SharedString,
        branch: Option<SharedString>,
        change_request: Option<luna_proto::ChangeRequestSummary>,
        harness: Option<luna_proto::HarnessId>,
        status: luna_proto::ChatIndicator,
        selected: bool,
        archived: bool,
        pinned: bool,
        activity_row: bool,
        can_settle: bool,
        // This row's jump combo while the hint overlay is up. It takes the
        // corner outright — above hover and above the status word — so all
        // nine chips appear together instead of leaving a hole on whichever
        // row is busy or under the pointer.
        jump_label: Option<SharedString>,
        search_query: Option<&str>,
        details: Option<SidebarThreadTooltip>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // The status and hover actions share a fixed-size corner. Changing its
        // width used to squeeze the project label and make neighboring rows
        // appear to jump when the pointer crossed them.
        // A chat can appear on both surfaces at once. Namespace every hover
        // key and child id so the palette never animates the sidebar copy.
        let row_id = if search_query.is_some() {
            format!("palette-chat-{id}")
        } else {
            format!("chat-{id}")
        };
        let corner_hovered = self.chat_status_hover.as_deref() == Some(row_id.as_str());
        let pin_control = (!archived && (pinned || (corner_hovered && search_query.is_none())))
            .then(|| {
                let pin_id = id.clone();
                div()
                    .id(SharedString::from(format!("{row_id}-pin")))
                    .role(gpui::Role::Button)
                    .aria_label(if pinned { "Unpin task" } else { "Pin task" })
                    .size(px(20.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(4.0))
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.glass_hover()))
                    .tooltip(move |_, cx| {
                        cx.new(|_| SurfaceTabTooltip {
                            text: (if pinned { "Unpin task" } else { "Pin task" }).into(),
                        })
                        .into()
                    })
                    .tooltip_show_delay(Duration::from_millis(350))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.set_chat_pinned(pin_id.clone(), !pinned, cx);
                    }))
                    .child(icon(icons::PIN).size(px(12.0)).text_color(theme.text_muted))
                    .into_any_element()
            });
        // Send-truth overrides: a send unadopted past the grace window is
        // FAILED (explicit, with the transcript's retry affordance); a send
        // whose delivery path is degraded is QUEUED, not Working — the
        // pending pill tells the truth instead of faking a spinner.
        let (queued, undelivered) = {
            let now = Utc::now();
            let state = self.state.read(cx);
            (
                state.send_queued(&id, now),
                state.send_undelivered(&id, now),
            )
        };
        let status_color = if undelivered {
            theme.danger
        } else if queued {
            theme.warning
        } else {
            spaces::status_dot_color(status, theme)
        };
        let status_label: Option<&'static str> = if undelivered {
            Some("Failed")
        } else if queued {
            Some("Queued")
        } else {
            match status {
                luna_proto::ChatIndicator::Working => Some("Working"),
                luna_proto::ChatIndicator::AwaitingApproval => Some("Approval"),
                luna_proto::ChatIndicator::AwaitingInput => Some("Input"),
                luna_proto::ChatIndicator::Errored => Some("Failed"),
                luna_proto::ChatIndicator::Completed => Some("Done"),
                luna_proto::ChatIndicator::Idle => None,
            }
        };
        let shows_metadata = branch.is_some() || change_request.is_some();
        let queued = queued && !undelivered;
        let working = status == luna_proto::ChatIndicator::Working && !queued && !undelivered;
        let corner_body: AnyElement = if let Some(label) = jump_label {
            // The jump hint replaces the status/time corner while the modifier
            // is held, cut to the sidebar PR badge's exact cloth
            // (`pull_request_badge`, Sidebar surface): pinned 16px, px 4,
            // rounded 4, borderless 0.08-fill with 0.85 text of one tone —
            // neutral here — and the label in the badge's mono at 10 MEDIUM.
            // Any other geometry reads as a second badge system on the row.
            {
                let tone = theme.text_muted;
                div()
                    .h(px(16.0))
                    .flex_none()
                    .flex()
                    .flex_row()
                    .items_center()
                    .px(px(4.0))
                    .rounded(px(4.0))
                    .bg(tone.opacity(0.08))
                    .text_size(crate::typography::ui_rems(10.0))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme.readable_tone(tone, 0.85))
                    .font_family(theme.font_mono.clone())
                    .child(label)
                    .into_any_element()
            }
        } else if corner_hovered && search_query.is_none() {
            let archive_id = id.clone();
            let settle_id = id.clone();
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_end()
                .gap(px(2.0))
                .h(px(18.0))
                .when_some(pin_control, |el, control| el.child(control))
                .when(can_settle, |el| {
                    el.child(
                        div()
                            .id(SharedString::from(format!("{row_id}-settle")))
                            .role(gpui::Role::Button)
                            .aria_label("Settle task")
                            .size(px(20.0))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(4.0))
                            .cursor_pointer()
                            .hover(|s| s.bg(theme.glass_hover()))
                            .tooltip(|_, cx| {
                                cx.new(|_| SurfaceTabTooltip {
                                    text: "Settle task".into(),
                                })
                                .into()
                            })
                            .tooltip_show_delay(Duration::from_millis(350))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.set_chat_settled(settle_id.clone(), true, cx);
                            }))
                            .child(
                                icon(icons::CHECK)
                                    .size(px(13.0))
                                    .text_color(theme.text_muted),
                            ),
                    )
                })
                .child(
                    div()
                        .id(SharedString::from(format!("{row_id}-archive")))
                        .role(gpui::Role::Button)
                        .aria_label(if archived {
                            "Unarchive task"
                        } else {
                            "Archive task"
                        })
                        .size(px(20.0))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(4.0))
                        .cursor_pointer()
                        .hover(|s| s.bg(theme.glass_hover()))
                        .tooltip(move |_, cx| {
                            cx.new(|_| SurfaceTabTooltip {
                                text: (if archived {
                                    "Unarchive task"
                                } else {
                                    "Archive task"
                                })
                                .into(),
                            })
                            .into()
                        })
                        .tooltip_show_delay(Duration::from_millis(350))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.set_chat_archived(archive_id.clone(), !archived, cx);
                        }))
                        .child(
                            icon(if archived {
                                icons::ARCHIVE_UP_MINIMALISTIC
                            } else {
                                icons::ARCHIVE_MINIMALISTIC
                            })
                            .size(px(13.0))
                            .text_color(theme.text_muted),
                        ),
                )
                .into_any_element()
        } else {
            let status_body: AnyElement = match status_label {
                Some(label) => {
                    // Glyph slot: Working wears the preset's animated pixel
                    // glyph beside its label, Done wears the check, and the
                    // remaining statuses use a compact dot.
                    let glyph: AnyElement = if working {
                        loaders::mini_glyph_spinner(
                            format!("{row_id}-working"),
                            2.0,
                            theme.glyph,
                            self.sidebar_pane.entity_id(),
                            cx,
                        )
                        .into_any_element()
                    } else {
                        div()
                            .size(px(6.0))
                            .flex_none()
                            .rounded_full()
                            .bg(status_color)
                            .into_any_element()
                    };
                    let status = div()
                        .h(px(18.0))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(4.0))
                        .child(glyph);
                    if label == "Done" {
                        status.into_any_element()
                    } else {
                        status
                            .child(
                                div()
                                    .text_size(crate::typography::ui_rems(10.0))
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(status_color)
                                    .child(SharedString::from(label)),
                            )
                            .into_any_element()
                    }
                }
                None => div()
                    .h(px(18.0))
                    .flex()
                    .items_center()
                    .text_size(crate::typography::ui_rems(10.0))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .child(time_ago.clone())
                    .into_any_element(),
            };
            div()
                .h(px(18.0))
                .flex()
                .flex_row()
                .items_center()
                .justify_end()
                .gap(px(2.0))
                .when_some(pin_control, |el, control| el.child(control))
                .child(status_body)
                .into_any_element()
        };
        // One stable wrapper across both states (identity keeps the hover
        // from flickering as the content swaps); the swap is driven by the
        // ROW's hover (user request — corner-only felt undiscoverable), but
        // archiving only clicks on the corner itself, so the row's own click
        // stays the selector.
        let corner: AnyElement = {
            div()
                .id(SharedString::from(format!("{row_id}-corner")))
                .flex_none()
                .w(px(96.0))
                .h(px(14.0))
                .flex()
                .items_center()
                .justify_end()
                .child(corner_body)
                .into_any_element()
        };
        let (hover, text) = (theme.glass_hover(), theme.text);
        let selected_wash = crate::theme::glass_selected_bg();
        let subline = if search_query.is_some() {
            theme.text_muted
        } else if theme.appearance.is_light() {
            theme.text_muted
        } else {
            theme.text_muted.opacity(0.5)
        };
        let select_id = id.clone();
        let menu_id = id.clone();
        // Hover fades over transition-colors (luna session-row.tsx) — both
        // the wash and the title brighten ride the same 150ms blend.
        let fade_key = format!("{row_id}-hover");
        let rest_bg = if selected {
            selected_wash
        } else {
            crate::theme::wash(0.0)
        };
        // A selected row must NOT drift toward the hover wash: in dark the two
        // fills are identical so the blend is a no-op, but light's hover sits
        // below its near-opaque selected fill, and blending toward it visibly
        // dimmed the active row under the pointer (user report).
        let hover_bg = if selected { selected_wash } else { hover };
        let rest_text = if selected || search_query.is_some() {
            text
        } else if status == luna_proto::ChatIndicator::Completed {
            text.opacity(0.95)
        } else {
            text.opacity(0.8)
        };
        let hover_progress = motion::hover_t_or(&fade_key, if corner_hovered { 1.0 } else { 0.0 });
        div()
            .id(SharedString::from(row_id.clone()))
            .h(px(chat_row_height(
                branch.is_some(),
                change_request.is_some(),
            )))
            .flex()
            .flex_col()
            .justify_center()
            .gap(px(2.0))
            .rounded(px(if search_query.is_some() {
                popover::PALETTE_ITEM_RADIUS
            } else {
                8.0
            }))
            .px(px(Theme::SPACE_SM))
            .py(px(6.0))
            .text_color(motion::mix(rest_text, text, hover_progress))
            .bg(motion::mix(rest_bg, hover_bg, hover_progress))
            // No selection ring (user request) — the wash alone marks the
            // active row.
            // Row hover drives BOTH the wash blend and the corner's
            // status→Archive swap (one listener — gpui allows a single
            // hover listener per element).
            .on_hover({
                let fade_hover = motion::hover_listener(fade_key.clone());
                let hover_id = row_id.clone();
                cx.listener(move |this, hovered: &bool, window, cx| {
                    fade_hover(hovered, window, cx);
                    if *hovered {
                        if this.chat_status_hover.as_deref() != Some(hover_id.as_str()) {
                            this.chat_status_hover = Some(hover_id.clone());
                            cx.notify();
                        }
                    } else if this.chat_status_hover.as_deref() == Some(hover_id.as_str()) {
                        this.chat_status_hover = None;
                        cx.notify();
                    }
                })
            })
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| {
                if activity_row {
                    this.open_activity_chat(select_id.clone(), cx);
                } else {
                    this.open_chat(select_id.clone(), cx);
                }
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.chat_menu.open(ChatMenuState {
                        chat_id: menu_id.clone(),
                        position: event.position,
                        page: ChatMenuPage::Root,
                    });
                    cx.notify();
                }),
            )
            .when_some(details, |row, details| {
                let height = details.estimated_height();
                // Exclude the actual control cluster, not the full status
                // reservation: an empty gap before Pin still belongs to the
                // thread's detail-hover region.
                let action_width = if pinned {
                    96.0
                } else if can_settle {
                    72.0
                } else {
                    50.0
                };
                row.tooltip_at(
                    move |_, cx| cx.new(|_| details.clone()).into(),
                    move |bounds, pointer, window| {
                        sidebar_thread_tooltip_anchor(bounds, pointer, window, action_width, height)
                    },
                )
                .tooltip_show_delay(Duration::from_millis(300))
            })
            // Line 1: project and one stable right-side status/action group.
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(Theme::SPACE_SM))
                    .child(sidebar_faded_label(
                        format!("chat-device-{id}").into(),
                        true,
                        div()
                            .text_size(crate::typography::ui_rems(11.0))
                            .line_height(crate::typography::ui_rems(14.0))
                            .text_color(subline)
                            .child(popover::search_highlight(space_name, search_query, theme)),
                    ))
                    .child(div().text_color(subline).child(corner)),
            )
            // Line 2: harness identity belongs directly with the title,
            // instead of floating as unrelated metadata below it.
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(SIDEBAR_ACTIVE_HARNESS_TITLE_GAP))
                    .when_some(
                        harness.map(crate::pickers::harness_brand_icon),
                        |el, (path, tint)| {
                            el.child(
                                icon(path)
                                    .size(px(SIDEBAR_ACTIVE_HARNESS_ICON_SIZE))
                                    .flex_none()
                                    .text_color(if theme.appearance.is_light() {
                                        theme.text_muted
                                    } else {
                                        tint.unwrap_or(subline).opacity(0.8)
                                    }),
                            )
                        },
                    )
                    .child(sidebar_faded_label(
                        format!("chat-title-{id}").into(),
                        true,
                        div()
                            .text_size(crate::typography::ui_rems(13.0))
                            .line_height(crate::typography::ui_rems(17.0))
                            .child(popover::search_highlight(title, search_query, theme)),
                    )),
            )
            // Omit the third line when both Branch and Pull request are hidden;
            // the two-line group stays centered in the uniform row height.
            .when(shows_metadata, |row| {
                row.child(
                    div()
                        .w_full()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(4.0))
                        .when_some(branch, |el, branch| {
                            el.child(
                                icon(icons::GIT_BRANCH)
                                    .size(px(11.0))
                                    .flex_none()
                                    .text_color(subline),
                            )
                            .child(sidebar_faded_label(
                                format!("chat-branch-{id}").into(),
                                false,
                                div()
                                    .text_size(crate::typography::ui_rems(11.0))
                                    .line_height(crate::typography::ui_rems(14.0))
                                    .text_color(subline)
                                    .child(popover::search_highlight(branch, search_query, theme)),
                            ))
                        })
                        // Stable invisible spring keeps the optional PR badge
                        // pinned right without changing no-PR paint.
                        .child(div().flex_1().min_w_0())
                        .when_some(change_request, |el, summary| {
                            el.child(crate::change_requests::pull_request_badge_with_query(
                                format!("{row_id}-pr").into(),
                                summary,
                                crate::change_requests::ChangeRequestBadgeSurface::Sidebar,
                                search_query,
                                theme,
                            ))
                        }),
                )
            })
            .into_any_element()
    }

    /// Chat-mode sidebar (spaces overhaul): window-control strip, the Spaces
    /// section (folder + device rows, add-space), the global Active sessions
    /// list, the notice strip, and the footer actions (§1.6).
    /// The global connection line. `None` while healthy (`Connected`) or on
    /// local profiles (`Disabled`) — and the engine's degrade grace means it
    /// only exists during REAL outages, never join/wake blips. No surface,
    /// no border (v0.2.12 feedback): a bare spinner + faint caption while
    /// reconnecting; an amber dot only when the OS says offline. The
    /// transport error belongs in logs, not the sidebar.
    fn render_connection_pill(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        use luna_proto::ConnectivityState as S;
        let conn = self.state.read(cx).connectivity.clone();
        let (label, glyph): (SharedString, AnyElement) = match conn.state {
            S::Disabled | S::Connected => return None,
            S::Offline => (
                "Offline — sends are saved".into(),
                div()
                    .size(px(5.0))
                    .rounded_full()
                    .bg(theme.warning)
                    .into_any_element(),
            ),
            S::Reconnecting => (
                "Reconnecting…".into(),
                loaders::mini_mono_spinner(
                    "connection-spinner",
                    2.0,
                    theme.text_muted,
                    self.sidebar_pane.entity_id(),
                    cx,
                )
                .into_any_element(),
            ),
        };
        Some(
            crate::motion::fade_in(
                "connection-pill",
                div()
                    .id("connection-pill")
                    .mx(px(Theme::SPACE_SM + 4.0))
                    .mb(px(Theme::SPACE_SM))
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(glyph)
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(crate::typography::ui_rems(11.0))
                            .text_color(theme.text_faint)
                            .child(label),
                    ),
            )
            .into_any_element(),
        )
    }

    fn begin_session_history_search(&mut self, cx: &mut Context<Self>) {
        self.history_search_generation = self.history_search_generation.wrapping_add(1);
        if let Some(task) = self.history_search_task.take() {
            task.detach();
        }
        self.history_search_error = None;
        self.history_search_next_cursor = None;
        let Some(request) = self.current_session_history_request(None, cx) else {
            self.history_search_results.clear();
            self.history_search_loading = false;
            self.history_search_scope_key = None;
            cx.notify();
            return;
        };
        self.history_search_scope_key = Some(HistorySearchScopeKey::from(&request));
        self.request_session_history_page(request, true, true, cx);
    }

    fn reset_session_history_search(&mut self, cx: &mut Context<Self>) {
        self.history_search_generation = self.history_search_generation.wrapping_add(1);
        if let Some(task) = self.history_search_task.take() {
            task.detach();
        }
        self.history_search_global = false;
        self.history_search_results.clear();
        self.history_search_loading = false;
        self.history_search_error = None;
        self.history_search_next_cursor = None;
        self.history_search_scope_key = None;
        cx.notify();
    }

    fn current_session_history_request(
        &self,
        cursor: Option<luna_proto::HistoryCursor>,
        cx: &Context<Self>,
    ) -> Option<HistorySearchRequest> {
        let query = self.command_palette_query(cx)?;
        let (selected_project, spaces, devices) = {
            let state = self.state.read(cx);
            // An explicit sidebar project is the user's search selection even
            // while an open task keeps its own project context. "All projects"
            // falls back to that context. Global
            // history remains a separate explicit toggle.
            let selected_project = match self.settings.space_filter.as_deref() {
                Some(space_id) => Some(space_id.to_string()),
                None => state.selected_space.clone(),
            };
            (
                selected_project,
                state.spaces.clone(),
                state.devices.clone(),
            )
        };
        let mut request = history_search_request(
            &query,
            self.history_search_global,
            selected_project,
            &spaces,
            &devices,
        );
        request.cursor = cursor;
        Some(request)
    }

    fn load_more_session_history(&mut self, cx: &mut Context<Self>) {
        if self.history_search_loading {
            return;
        }
        let Some(cursor) = self.history_search_next_cursor.clone() else {
            return;
        };
        let Some(request) = self.current_session_history_request(Some(cursor), cx) else {
            return;
        };
        self.request_session_history_page(request, false, false, cx);
    }

    fn request_session_history_page(
        &mut self,
        request: HistorySearchRequest,
        reset: bool,
        debounce: bool,
        cx: &mut Context<Self>,
    ) {
        if reset {
            // Replacement requests must stop exposing the previous query or
            // scope before debounce, engine lookup, or RPC completion. Append
            // requests deliberately retain their current page while loading.
            self.history_search_results.clear();
            self.history_search_next_cursor = None;
        }
        let generation = self.history_search_generation;
        let (handle, engine_generation) = {
            let state = self.state.read(cx);
            (state.engine().cloned(), state.engine_generation())
        };
        let Some(handle) = handle else {
            self.history_search_loading = false;
            self.history_search_error =
                Some("History search is unavailable while reconnecting".into());
            cx.notify();
            return;
        };
        self.history_search_loading = true;
        self.history_search_task = Some(cx.spawn(async move |this, cx| {
            if debounce {
                cx.background_executor()
                    .timer(Duration::from_millis(120))
                    .await;
            }
            let outcome = handle
                .client()
                .call(
                    methods::SEARCH_SESSION_HISTORY,
                    serde_json::to_value(&request).unwrap_or_default(),
                )
                .await
                .and_then(|value| {
                    serde_json::from_value::<HistorySearchPage>(value)
                        .map_err(|error| luna_rpc::RpcError::Failed(error.to_string()))
                });
            this.update(cx, |shell, cx| {
                if !history_search_response_is_live(
                    generation,
                    shell.history_search_generation,
                    engine_generation,
                    shell.state.read(cx).engine_generation(),
                ) {
                    return;
                }
                shell.history_search_loading = false;
                match outcome {
                    Ok(page) => {
                        if reset {
                            shell.history_search_results = page.events;
                        } else {
                            let mut seen = shell
                                .history_search_results
                                .iter()
                                .map(|event| event.event_id.clone())
                                .collect::<std::collections::HashSet<_>>();
                            shell.history_search_results.extend(
                                page.events
                                    .into_iter()
                                    .filter(|event| seen.insert(event.event_id.clone())),
                            );
                        }
                        shell.history_search_next_cursor = page.next_cursor;
                        shell.history_search_error = None;
                    }
                    Err(error) => {
                        if reset {
                            shell.history_search_results.clear();
                            shell.history_search_next_cursor = None;
                        }
                        shell.history_search_error = Some(error.to_string().into());
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn open_session_history_result(&mut self, event: ActivityEnvelope, cx: &mut Context<Self>) {
        if !event.source.available_here {
            self.sidebar_notice = Some(
                event
                    .source
                    .unavailable_reason
                    .unwrap_or_else(|| "This result belongs to another environment.".into())
                    .into(),
            );
            cx.notify();
            return;
        }
        if !self
            .state
            .read(cx)
            .chats
            .iter()
            .any(|chat| chat.id == event.task_id)
        {
            self.sidebar_notice = Some("The owning task is no longer available.".into());
            cx.notify();
            return;
        }
        self.transcript.update(cx, |transcript, cx| {
            transcript.navigate_to_message(
                event.task_id.clone(),
                event.source.message_id.clone(),
                cx,
            )
        });
        self.open_chat(event.task_id, cx);
    }

    fn render_chat_sidebar(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let activity_view = self.activity.view_active;
        let activity_unavailable = self.activity_is_unavailable();

        // Keyed rows: (stable key, estimated height, element) — the key + height
        // list drives the §1.6 resort FLIP diff below (attention-bucket
        // promotions glide; cleared rows just go).
        let keyed: Vec<(String, f32, AnyElement)> = self.render_active_rows(theme, cx);

        // Resort glide (§1.6 View Transitions parity): when the ORDER of a live
        // list changes (new activity resort, grouping flip), surviving rows
        // glide from their old y to the new one — layout is already at the new
        // position; the offset is a paint-only relative inset animated to 0
        // over 260ms cubic-bezier(0.22,1,0.36,1). New rows fade in; removals
        // just go (matching the original). First fill and chat switches (which
        // don't reorder) never animate.
        let order: Vec<(String, f32)> = keyed.iter().map(|(k, h, _)| (k.clone(), *h)).collect();
        if self.sidebar_prev_order != order {
            let key_order_changed = sidebar_key_order_changed(&self.sidebar_prev_order, &order);
            if !self.sidebar_prev_order.is_empty() {
                // A disclosure already animates its own body height. Applying
                // FLIP offsets when only keyed heights change double-counts
                // that movement, leaving gaps and momentary overlaps between
                // the first group, following groups, and Archived.
                let offsets = if key_order_changed {
                    resort_offsets(&self.sidebar_prev_order, &order, SIDEBAR_LIST_GAP)
                } else {
                    std::collections::HashMap::new()
                };
                let prev_keys: std::collections::HashSet<&str> = self
                    .sidebar_prev_order
                    .iter()
                    .map(|(k, _)| k.as_str())
                    .collect();
                let new_keys: std::collections::HashSet<String> = order
                    .iter()
                    .filter(|(k, _)| !prev_keys.contains(k.as_str()))
                    .map(|(k, _)| k.clone())
                    .collect();
                if key_order_changed && (!offsets.is_empty() || !new_keys.is_empty()) {
                    self.resort_epoch += 1;
                    self.sidebar_resort = offsets;
                    self.sidebar_new_keys = new_keys;
                }
            }
            self.sidebar_prev_order = order;
        }
        let epoch = self.resort_epoch;
        let list_items: Vec<AnyElement> = keyed
            .into_iter()
            .map(|(key, _, element)| {
                if let Some(dy) = self.sidebar_resort.get(&key).copied() {
                    let id = SharedString::from(format!("resort-{epoch}-{key}"));
                    div()
                        .child(element)
                        .with_animation(id, RESORT.animation(), move |el, t| {
                            el.relative().top(px(dy * (1.0 - t)))
                        })
                        .into_any_element()
                } else if self.sidebar_new_keys.contains(&key) {
                    let id = SharedString::from(format!("row-in-{epoch}-{key}"));
                    motion::fade_quick(id, div().child(element)).into_any_element()
                } else {
                    element
                }
            })
            .collect();

        // t3code's archived accordion, below the active list.
        let older_section = (!activity_view)
            .then(|| self.render_settled_section(theme, cx))
            .flatten();
        let archived_section = (!activity_view)
            .then(|| self.render_archived_section(theme, cx))
            .flatten();

        // The normal project filter and Activity toggle share the existing
        // fixed header row above the sidebar's scrolling task list.
        let filter_row = self.render_spaces_filter(theme, cx);
        let sidebar_scroll = if activity_view {
            self.activity_sidebar_scroll.clone()
        } else {
            self.sidebar_scroll.clone()
        };

        // The (filtered) Sessions list scrolls inside an EdgeFade scope —
        // a true per-glyph gradient at active overflow edges. Glass-safe
        // (no painted overlay can fade content over see-through blur) and
        // equivalent on opaque themes: alpha→0 reveals the surface tone
        // underneath, same as the gradient overlays it replaced. Overflow
        // is read at PAINT time via the scroll handle — render-time gating
        // rode the previous frame's offset, so the last frame of a content
        // shrink (row archived while scrolled) left a phantom fade stuck
        // over an unscrollable list (user report).
        let terminal_inventory = self.render_terminal_inventory(theme, cx);
        let activity_load_state = self.render_activity_load_state(theme);
        let sidebar_lists = crate::edge_fade::edge_faded(
            SIDEBAR_GLASS_FADE_BAND,
            true,
            true,
            div().relative().flex_1().min_h_0().child(
                div()
                    .id("sidebar-lists")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&sidebar_scroll)
                    .px(px(Theme::SPACE_SM))
                    .flex()
                    .flex_col()
                    // No "Sessions" header (user request) — the list
                    // is the whole column; a little air stands in.
                    .pt(px(4.0))
                    .when_some(activity_load_state, |el, state| el.child(state))
                    .child(if !list_items.is_empty() {
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(SIDEBAR_LIST_GAP))
                            .children(list_items)
                            .into_any_element()
                    } else {
                        div()
                            .px(px(Theme::SPACE_SM))
                            .pb(px(Theme::SPACE_SM))
                            .text_size(crate::typography::ui_rems(12.0))
                            .text_color(theme.text_faint)
                            .child(SharedString::from(if activity_unavailable {
                                "Activity is temporarily unavailable"
                            } else if activity_view {
                                "No tasks need attention or are running"
                            } else {
                                "No sessions yet"
                            }))
                            .into_any_element()
                    })
                    .children(terminal_inventory)
                    .children(older_section)
                    .children(archived_section),
            ),
        )
        .fade_overflow_y(&sidebar_scroll);

        div()
            .w(px(self.settings.sidebar_width))
            .h_full()
            .flex()
            .flex_col()
            // (No titlebar strip: the unified window titlebar spans the whole
            // window above this column.)
            .child(filter_row)
            .child(sidebar_lists)
            // Global connection pill (durable-by-design UI truth): appears
            // whenever the edge posture is degraded; hidden while healthy —
            // appearing IS the signal.
            .when_some(self.render_connection_pill(theme, cx), |el, pill| {
                el.child(pill)
            })
            // Update strip (above the user menu; below the lists).
            .when_some(self.render_update_strip(theme, cx), |el, strip| {
                el.child(strip)
            })
            // Sidebar feedback is brief and readable; the action text carries
            // the error, while the tint distinguishes it from a status note.
            .when_some(self.sidebar_notice.clone(), |el, notice| {
                let notice_id = notice.id;
                let is_status = notice.tone == SidebarNoticeTone::Status;
                el.child(
                    div()
                        .id("sidebar-notice")
                        .mx(px(Theme::SPACE_SM))
                        .mb(px(Theme::SPACE_SM))
                        .px(px(Theme::SPACE_SM))
                        .py(px(4.0))
                        .rounded(px(Theme::CONTROL_RADIUS))
                        .border_1()
                        .border_color(if is_status {
                            theme.border
                        } else {
                            theme.danger.opacity(0.25)
                        })
                        .bg(if is_status {
                            theme.accent_wash
                        } else {
                            theme.danger.opacity(0.05)
                        })
                        .text_size(crate::typography::ui_rems(11.0))
                        .text_color(theme.text)
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .child(div().flex_1().min_w_0().child(notice.message))
                        .child(
                            div()
                                .id("sidebar-notice-dismiss")
                                .role(gpui::Role::Button)
                                .aria_label("Dismiss notice")
                                .tab_index(0)
                                .cursor_pointer()
                                .flex_none()
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .child("Dismiss")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if this
                                        .sidebar_notice
                                        .as_ref()
                                        .is_some_and(|current| current.id == notice_id)
                                    {
                                        this.sidebar_notice = None;
                                        cx.notify();
                                    }
                                }))
                                .on_key_down(cx.listener(
                                    move |this, event: &gpui::KeyDownEvent, _, cx| {
                                        if matches!(event.keystroke.key.as_str(), "enter" | "space")
                                            && !event.is_held
                                        {
                                            cx.stop_propagation();
                                            if this
                                                .sidebar_notice
                                                .as_ref()
                                                .is_some_and(|current| current.id == notice_id)
                                            {
                                                this.sidebar_notice = None;
                                                cx.notify();
                                            }
                                        }
                                    },
                                )),
                        ),
                )
            })
            .when(!self.settings.usage_sidebar_hidden, |el| {
                el.child(self.render_usage_card(theme, cx))
            })
            .child(
                div()
                    .id("sidebar-footer-actions")
                    .mx(px(Theme::SPACE_SM))
                    .mt(px(4.0))
                    .mb(px(Theme::SPACE_SM))
                    .pl(px(3.0))
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .id("sidebar-settings-action")
                            .role(gpui::Role::Button)
                            .aria_label("Open settings")
                            .size(px(36.0))
                            .rounded(px(Theme::CONTROL_RADIUS))
                            .cursor_pointer()
                            .hover(|el| el.bg(theme.glass_hover()))
                            .tooltip(|_, cx| {
                                cx.new(|_| SurfaceTabTooltip {
                                    text: "Open settings".into(),
                                })
                                .into()
                            })
                            .tooltip_show_delay(Duration::from_millis(350))
                            .flex()
                            .items_center()
                            .justify_center()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.open_settings(SettingsSection::DEFAULT, cx)
                            }))
                            .child(
                                icon(icons::SETTINGS_GEAR)
                                    .size(px(19.0))
                                    .text_color(theme.text_muted),
                            ),
                    )
                    .child(
                        div()
                            .id("sidebar-devices-action")
                            .role(gpui::Role::Button)
                            .aria_label("Open devices")
                            .size(px(36.0))
                            .rounded(px(Theme::CONTROL_RADIUS))
                            .cursor_pointer()
                            .hover(|el| el.bg(theme.glass_hover()))
                            .tooltip(|_, cx| {
                                cx.new(|_| SurfaceTabTooltip {
                                    text: "Open devices".into(),
                                })
                                .into()
                            })
                            .tooltip_show_delay(Duration::from_millis(350))
                            .flex()
                            .items_center()
                            .justify_center()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.open_settings(SettingsSection::Devices, cx)
                            }))
                            .child(
                                icon(icons::SMARTPHONE)
                                    .size(px(18.0))
                                    .text_color(theme.text_muted),
                            ),
                    )
                    .when(!self.settings.usage_sidebar_hidden, |el| {
                        el.child(self.render_usage_action(theme, cx))
                    })
                    .child(div().flex_1())
                    .when_some(self.render_provider_update_badge(theme, cx), |el, badge| {
                        el.child(badge)
                    }),
            )
            .into_any_element()
    }

    fn render_provider_update_badge(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let statuses = self
            .accounts_page
            .as_ref()
            .and_then(|page| page.read(cx).provider_update_statuses())
            .unwrap_or_else(|| self.provider_updates.clone());
        let pending: Vec<_> = statuses
            .iter()
            .filter(|status| status.update_available)
            .collect();
        let first = pending.first()?;
        let mark = match first.harness {
            HarnessId::Codex => icons::OPENAI_MARK,
            HarnessId::ClaudeCode => icons::CLAUDE_MARK,
            HarnessId::Opencode => icons::OPENCODE_MARK,
            _ => icons::ARROW_DOWN,
        };
        Some(
            div()
                .id("sidebar-provider-update")
                .role(gpui::Role::Button)
                .aria_label(format!(
                    "{} provider update{} available. Open Providers.",
                    pending.len(),
                    if pending.len() == 1 { "" } else { "s" }
                ))
                .w(px(48.0))
                .h(px(24.0))
                .rounded(px(12.0))
                .border_1()
                .border_color(theme.border_strong)
                .bg(theme.card_glass_bg())
                .cursor_pointer()
                .hover(|el| el.bg(theme.glass_hover()))
                .flex()
                .items_center()
                .justify_center()
                .gap(px(4.0))
                .on_click(
                    cx.listener(|this, _, _, cx| {
                        this.open_settings(SettingsSection::Providers, cx)
                    }),
                )
                .child(
                    icon(icons::ARROW_DOWN)
                        .size(px(12.0))
                        .text_color(theme.accent),
                )
                .child(icon(mark).size(px(13.0)).text_color(theme.text))
                .into_any_element(),
        )
    }

    fn update_status(&self, cx: &Context<Self>) -> Option<luna_update::UpdateStatus> {
        let engine = self.state.read(cx).update.clone();
        match (&self.update_checked, engine) {
            (Some(manual), Some(engine))
                if engine.checked_at.unwrap_or_default()
                    >= manual.checked_at.unwrap_or_default() =>
            {
                Some(engine)
            }
            (Some(manual), _) => Some(manual.clone()),
            (None, engine) => engine,
        }
    }

    /// Check the signed GitHub release feed on demand. Publishing the result
    /// through AppState keeps the settings page and sidebar on the same fact.
    fn begin_update_check(&mut self, cx: &mut Context<Self>) {
        if self.update_checking {
            return;
        }
        self.update_check_attempted = true;
        self.update_checking = true;
        self.update_check_error = None;
        let check_started_at = Utc::now().timestamp_millis();
        let edge_url = self.boot.edge_url.clone();
        let check = Tokio::spawn(
            cx,
            async move { luna_update::fetch_latest(&edge_url).await },
        );
        self.update_check_task = Some(cx.spawn(async move |this, cx| {
            let result = match check.await {
                Ok(result) => result.map_err(|err| format!("{err:#}")),
                Err(err) => Err(err.to_string()),
            };
            this.update(cx, |shell, cx| {
                shell.update_checking = false;
                match result {
                    Ok(manifest) => {
                        let current = luna_update::current_version();
                        let status = luna_update::UpdateStatus {
                            current_version: current.to_owned(),
                            latest_version: Some(manifest.version.clone()),
                            update_available: luna_update::version_newer(
                                &manifest.version,
                                current,
                            ),
                            checked_at: Some(Utc::now().timestamp_millis()),
                            error: None,
                        };
                        let staged_for_other_version = shell
                            .update_status(cx)
                            .as_ref()
                            .and_then(|old| old.latest_version.as_deref())
                            .is_some_and(|old| old != manifest.version);
                        if staged_for_other_version
                            && matches!(shell.update_flow, UpdateFlow::Ready(_))
                        {
                            shell.update_flow = UpdateFlow::Idle;
                        }
                        shell.update_check_error = None;
                        shell.update_checked = Some(status.clone());
                        shell.state.update(cx, |state, cx| {
                            state.apply_update(status);
                            cx.notify();
                        });
                    }
                    Err(message) => {
                        tracing::warn!(%message, "manual update check failed");
                        shell.update_check_error = Some((message.into(), check_started_at));
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn render_updates_page(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let focus_border = theme.border_strong;
        let focus_accent = theme.accent;
        let status = self.update_status(cx);
        let current = status
            .as_ref()
            .map(|status| status.current_version.clone())
            .unwrap_or_else(|| luna_update::current_version().to_owned());
        let latest = status
            .as_ref()
            .and_then(|status| status.latest_version.as_deref());
        let available = status
            .as_ref()
            .is_some_and(|status| status.update_available && latest.is_some());
        let desktop_update = self.install.supports_desktop_update();
        let check_error = self
            .update_check_error
            .as_ref()
            .filter(|(_, failed_at)| {
                status
                    .as_ref()
                    .and_then(|status| status.checked_at)
                    .is_none_or(|checked_at| checked_at < *failed_at)
            })
            .map(|(error, _)| error.to_string())
            .or_else(|| status.as_ref().and_then(|status| status.error.clone()));
        let status_text = if self.update_checking {
            "Checking GitHub Releases…".to_owned()
        } else if check_error.is_some() && !available {
            "Couldn't check GitHub Releases.".to_owned()
        } else if available {
            format!("Version v{} is available.", latest.unwrap())
        } else if status
            .as_ref()
            .is_some_and(|status| status.checked_at.is_some())
        {
            "You're up to date.".to_owned()
        } else {
            "Check for the latest GitHub release.".to_owned()
        };
        let checked_at = status
            .as_ref()
            .and_then(|status| status.checked_at)
            .and_then(chrono::DateTime::<Utc>::from_timestamp_millis)
            .map(|checked| format!("Last checked {} UTC", checked.format("%b %-d, %Y at %H:%M")));
        let mut release_row = widgets::card_row(&theme, false)
            .min_h(px(88.0))
            .child(widgets::row_tile(&theme, icons::RESTART))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(widgets::row_title(&theme, "Release status"))
                    .child(widgets::meta_line(
                        &theme,
                        vec![div().child(status_text).into_any_element()],
                    ))
                    .when_some(checked_at, |row, checked_at| {
                        row.child(
                            div()
                                .mt(px(4.0))
                                .text_size(crate::typography::ui_rems(11.0))
                                .text_color(theme.text_muted)
                                .child(checked_at),
                        )
                    }),
            );
        if available {
            release_row = release_row.child(widgets::badge_active(&theme, "Available"));
        }

        let mut actions = div()
            .mt(px(20.0))
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .gap(px(10.0));
        if !self.update_checking && !matches!(self.update_flow, UpdateFlow::Downloading) {
            actions = actions.child(
                popover::btn_ghost(&theme, "Check for updates", "updates-check")
                    .id("updates-check")
                    .role(gpui::Role::Button)
                    .aria_label("Check GitHub Releases for updates")
                    .tab_index(0)
                    .focus_visible(move |style| style.border_2().border_color(focus_border))
                    .on_click(cx.listener(|this, _, _, cx| this.begin_update_check(cx)))
                    .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                        if is_settings_nav_activation(&event.keystroke.key, event.is_held) {
                            cx.stop_propagation();
                            this.begin_update_check(cx);
                        }
                    })),
            );
        }
        if available && !self.update_checking {
            if desktop_update {
                let action = match &self.update_flow {
                    UpdateFlow::Idle => Some("Update and restart"),
                    UpdateFlow::Downloading => None,
                    UpdateFlow::Ready(_) => Some("Restart to update"),
                    UpdateFlow::Failed(_) => Some("Retry download"),
                };
                if let Some(label) = action {
                    actions = actions.child(
                        popover::btn_primary(&theme, label)
                            .id("updates-apply")
                            .role(gpui::Role::Button)
                            .aria_label(label)
                            .tab_index(0)
                            .focus_visible(move |style| style.border_2().border_color(focus_accent))
                            .on_click(cx.listener(|this, _, _, cx| this.on_update_page_action(cx)))
                            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                                if is_settings_nav_activation(&event.keystroke.key, event.is_held) {
                                    cx.stop_propagation();
                                    this.on_update_page_action(cx);
                                }
                            })),
                    );
                }
            } else {
                actions = actions.child(
                    popover::btn_primary(&theme, "View release on GitHub")
                        .id("updates-open-github")
                        .role(gpui::Role::Button)
                        .aria_label("View latest LUNA release on GitHub")
                        .tab_index(0)
                        .focus_visible(move |style| style.border_2().border_color(focus_accent))
                        .on_click(cx.listener(|_, _, _, cx| {
                            cx.open_url("https://github.com/novnski/luna/releases/latest");
                        })),
                );
            }
        }

        let flow_message = match &self.update_flow {
            UpdateFlow::Downloading => Some("Downloading and verifying the release…".to_owned()),
            UpdateFlow::Ready(_) if available => {
                Some("The update is ready. Restart LUNA to finish installing it.".to_owned())
            }
            UpdateFlow::Failed(error) if available => Some(format!("Update failed: {error}")),
            _ => None,
        };
        let install_note = if desktop_update {
            "When an update is available, press Update once. LUNA downloads and verifies it, then restarts after open work is saved."
        } else {
            "This installation cannot be replaced automatically. Install the GitHub release to enable in-app updates."
        };
        div()
            .id("settings-updates-scroll")
            .size_full()
            .overflow_y_scroll()
            .child(
                widgets::page_column()
                    .child(widgets::page_header(&theme, "Updates", None))
                    .child(widgets::page_subtitle(
                        &theme,
                        "Get new versions directly from this project's GitHub Releases.",
                    ))
                    .child(
                        widgets::section_card(&theme)
                            .child(
                                widgets::card_row(&theme, true)
                                    .min_h(px(76.0))
                                    .child(widgets::row_tile(&theme, icons::MONITOR))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .child(widgets::row_title(&theme, "Installed version"))
                                            .child(widgets::meta_line(
                                                &theme,
                                                vec![
                                                    div()
                                                        .child(format!("v{current}"))
                                                        .into_any_element(),
                                                ],
                                            )),
                                    ),
                            )
                            .child(release_row),
                    )
                    .child(actions)
                    .when_some(flow_message, |page, message| {
                        page.child(
                            div()
                                .mt(px(12.0))
                                .text_size(crate::typography::ui_rems(12.0))
                                .text_color(theme.text_muted)
                                .child(message),
                        )
                    })
                    .when_some(check_error, |page, error| {
                        page.child(widgets::error_strip(
                            &theme,
                            format!("Update check failed: {error}"),
                        ))
                    })
                    .child(
                        div()
                            .mt(px(24.0))
                            .text_size(crate::typography::ui_rems(12.0))
                            .text_color(theme.text_muted)
                            .child(install_note),
                    ),
            )
            .into_any_element()
    }

    /// Update strip: shown above the user menu whenever the engine's
    /// UpdateStatus stream reports a newer release. On a macOS bundle install
    /// it drives the whole flow — click to download, then click to restart into
    /// the staged bundle. Elsewhere (managed/source installs) it is advisory
    /// (`luna update`); click dismisses it for that version.
    fn render_update_strip(&mut self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let status = self.update_status(cx)?;
        if !status.update_available {
            return None;
        }
        let latest = status.latest_version.clone()?;
        if self.update_dismissed.as_deref() == Some(latest.as_str()) {
            return None;
        }
        let desktop_update = self.install.supports_desktop_update();

        let (label, clickable): (SharedString, bool) = if desktop_update {
            match &self.update_flow {
                UpdateFlow::Idle => (format!("Update available — v{latest}").into(), true),
                UpdateFlow::Downloading => (format!("Downloading v{latest}…").into(), false),
                UpdateFlow::Ready(_) => ("Update ready — restart to apply".into(), true),
                UpdateFlow::Failed(message) => (format!("Update failed: {message}").into(), true),
            }
        } else {
            (
                format!("Update available — v{latest} · run `luna update`").into(),
                true,
            )
        };
        let failed = matches!(self.update_flow, UpdateFlow::Failed(_));
        let tone = if failed { theme.danger } else { theme.accent };
        // Follow the selected spectrum with a low-emphasis glass tint rather
        // than painting the bright text accent as a solid slab.
        let (chip_bg, chip_bg_hover) = if failed {
            (theme.danger.opacity(0.14), theme.danger.opacity(0.22))
        } else {
            (theme.accent_wash, theme.accent.opacity(0.16))
        };

        let mut strip = div()
            .id("update-strip")
            .mx(px(Theme::SPACE_SM))
            // No bottom margin: the footer actions below carry their own
            // SPACE_SM padding — doubling it read as a hole (user report).
            .px(px(Theme::SPACE_SM))
            .py(px(6.0))
            .rounded(px(Theme::CONTROL_RADIUS))
            .bg(chip_bg)
            .flex()
            .flex_row()
            .items_center()
            .text_size(crate::typography::ui_rems(11.0))
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(tone)
            .child(div().flex_1().min_w_0().child(label));
        if clickable {
            strip = strip
                .cursor_pointer()
                .hover(move |s| s.bg(chip_bg_hover))
                .on_click(cx.listener(move |this, _, _, cx| this.on_update_strip_click(cx)));
        }
        Some(strip.into_any_element())
    }

    /// Idle → download; Ready → swap + relaunch; Failed → retry; advisory
    /// installs → dismiss for this version.
    fn on_update_strip_click(&mut self, cx: &mut Context<Self>) {
        if !self.install.supports_desktop_update() {
            self.update_dismissed = self
                .update_status(cx)
                .as_ref()
                .and_then(|s| s.latest_version.clone());
            cx.notify();
            return;
        }
        match std::mem::replace(&mut self.update_flow, UpdateFlow::Idle) {
            UpdateFlow::Idle | UpdateFlow::Failed(_) => self.begin_update_download(false, cx),
            UpdateFlow::Downloading => self.update_flow = UpdateFlow::Downloading,
            UpdateFlow::Ready(staged) => {
                self.update_flow = UpdateFlow::Ready(staged.clone());
                self.apply_staged_update(staged, cx);
            }
        }
    }

    /// The Updates page has a one-action flow: stage, then gracefully apply
    /// and restart. PendingExit waits for unsaved work when needed.
    fn on_update_page_action(&mut self, cx: &mut Context<Self>) {
        match std::mem::replace(&mut self.update_flow, UpdateFlow::Idle) {
            UpdateFlow::Idle | UpdateFlow::Failed(_) => self.begin_update_download(true, cx),
            UpdateFlow::Downloading => self.update_flow = UpdateFlow::Downloading,
            UpdateFlow::Ready(staged) => {
                self.update_flow = UpdateFlow::Ready(staged.clone());
                self.apply_staged_update(staged, cx);
            }
        }
    }

    /// Fetch the manifest and stage the new desktop bundle under the data dir.
    /// The Updates page applies it after staging; the sidebar waits for a
    /// separate restart click.
    fn begin_update_download(&mut self, auto_apply: bool, cx: &mut Context<Self>) {
        let edge_url = self.boot.edge_url.clone();
        let data_dir = self.data_dir.clone();
        let install = self.install.clone();
        self.update_auto_apply = auto_apply;
        self.update_flow = UpdateFlow::Downloading;
        let download = Tokio::spawn(cx, async move {
            let manifest = luna_update::fetch_latest(&edge_url).await?;
            install.stage_desktop(&edge_url, &manifest, &data_dir).await
        });
        self.update_task = Some(cx.spawn(async move |this, cx| {
            let outcome = match download.await {
                Ok(Ok(staged)) => Ok(staged),
                Ok(Err(err)) => Err(format!("{err:#}")),
                Err(join_err) => Err(join_err.to_string()),
            };
            this.update(cx, |shell, cx| {
                shell.update_flow = match outcome {
                    Ok(staged) => UpdateFlow::Ready(staged),
                    Err(message) => {
                        tracing::warn!(%message, "update download failed");
                        shell.update_auto_apply = false;
                        UpdateFlow::Failed(message.into())
                    }
                };
                if shell.update_auto_apply {
                    shell.update_auto_apply = false;
                    let staged = match &shell.update_flow {
                        UpdateFlow::Ready(staged) => Some(staged.clone()),
                        _ => None,
                    };
                    if let Some(staged) = staged {
                        shell.apply_staged_update(staged, cx);
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// Swap the staged bundle over the installed one, arm the detached
    /// relauncher, and quit — the relauncher `open`s the new bundle once this
    /// process (and its engine lock / IPC port) is gone.
    fn apply_staged_update(&mut self, staged: PathBuf, cx: &mut Context<Self>) {
        if !self.prepare_exit(PendingExit::InstallUpdate(staged.clone()), cx) {
            return;
        }
        match self.install.apply_desktop(&staged) {
            Ok(()) => {
                crate::app_menus::quit_after_save(cx);
            }
            Err(err) => {
                tracing::error!(error = %err, "update apply failed");
                self.update_flow = UpdateFlow::Failed(format!("{err:#}").into());
                cx.notify();
            }
        }
    }

    fn render_sync_overlay(
        &mut self,
        viewport: gpui::Size<Pixels>,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let theme = Theme::of(cx).for_popup();
        let needs_org = matches!(
            self.state.read(cx).auth.as_ref(),
            Some(AuthState::NeedsOrganization { .. })
        );
        let remote_engine = self
            .state
            .read(cx)
            .engine()
            .is_some_and(|engine| matches!(engine.mode(), EngineMode::Remote { .. }));
        let runtime_change_label = if self.runtime_change_task.is_some() {
            "Stopping engine…"
        } else if remote_engine {
            "Stop daemon and quit"
        } else {
            "Quit LUNA"
        };

        if self.sync_flow == SyncFlow::Enabling && needs_org {
            return Some(self.render_org_gate(cx));
        }

        let signed_in_email: Option<SharedString> = match self.state.read(cx).auth.as_ref() {
            Some(AuthState::SignedIn { user, .. }) => Some(SharedString::from(user.email.clone())),
            _ => None,
        };
        // Spaces count as local work too: a projects-only profile must get
        // the import choice, not a bare "Switch now".
        let (local_chats, local_spaces) = {
            let state = self.state.read(cx);
            (state.chats.len(), state.spaces.len())
        };
        let work_phrase = local_work_phrase(local_chats, local_spaces);

        let card = match self.sync_flow {
            SyncFlow::Enabling => popover::dialog_card(&theme)
                .child(popover::dialog_title(&theme, "Enable sync"))
                .child(
                    div().mt(px(6.0)).child(popover::dialog_body(
                        &theme,
                        "Finish signing in in your browser. LUNA will keep using this local workspace until you quit and reopen.",
                    )),
                )
                .child(
                    div()
                        .mt(px(16.0))
                        .flex()
                        .flex_row()
                        .justify_end()
                        .gap(px(8.0))
                        .child(
                            popover::btn_ghost(&theme, "Cancel", "sync-enable-cancel")
                                .id("sync-enable-cancel")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.cancel_auth_setup(cx)
                                })),
                        )
                        .child(
                            popover::btn_primary(&theme, "Open browser again")
                                .id("sync-enable-open-browser")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.start_sign_in(cx)
                                })),
                        ),
                )
                .into_any_element(),
            SyncFlow::Canceling => popover::dialog_card(&theme)
                .child(popover::dialog_title(&theme, "Canceling sync setup…"))
                .child(
                    div().mt(px(6.0)).child(popover::dialog_body(
                        &theme,
                        "Removing the partial sign-in before returning to your local workspace.",
                    )),
                )
                .into_any_element(),
            // ── in-place switch wizard ────────────────────────────────────
            SyncFlow::SwitchOffer { notice_open: true } => {
                let has_local_work = work_phrase.is_some();
                let body: SharedString = match (&signed_in_email, &work_phrase) {
                    (Some(email), Some(phrase)) => format!(
                        "You're signed in as {email}. Bring {phrase} from this device into your synced workspace, or start it fresh."
                    )
                    .into(),
                    (Some(email), None) => format!(
                        "You're signed in as {email}. LUNA can switch to your synced workspace now."
                    )
                    .into(),
                    (None, Some(phrase)) => format!(
                        "Bring {phrase} from this device into your synced workspace, or start it fresh."
                    )
                    .into(),
                    (None, None) => "LUNA can switch to your synced workspace now.".into(),
                };
                let mut actions = div()
                    .mt(px(16.0))
                    .flex()
                    .flex_row()
                    .justify_end()
                    .gap(px(8.0))
                    .child(
                        popover::btn_ghost(&theme, "Later", "sync-switch-later")
                            .id("sync-switch-later")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.postpone_sync_restart(cx)
                            })),
                    );
                if has_local_work {
                    actions = actions
                        .child(
                            popover::btn_ghost(&theme, "Start fresh", "sync-switch-fresh")
                                .id("sync-switch-fresh")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.start_synced_switch(false, cx)
                                })),
                        )
                        .child(
                            popover::btn_primary(&theme, "Bring my work")
                                .id("sync-switch-import")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.start_synced_switch(true, cx)
                                })),
                        );
                } else {
                    actions = actions.child(
                        popover::btn_primary(&theme, "Switch now")
                            .id("sync-switch-now")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.start_synced_switch(false, cx)
                            })),
                    );
                }
                popover::dialog_card(&theme)
                    .child(popover::dialog_title(&theme, "Sync is ready"))
                    .child(div().mt(px(6.0)).child(popover::dialog_body(&theme, body)))
                    .child(actions)
                    .into_any_element()
            }
            SyncFlow::Switching { import } => popover::dialog_card(&theme)
                .child(popover::dialog_title(
                    &theme,
                    "Switching to your synced workspace…",
                ))
                .child(div().mt(px(6.0)).child(popover::dialog_body(
                    &theme,
                    if import {
                        "Handing the engine over to your account. Your local sessions come along next."
                    } else {
                        "Handing the engine over to your account."
                    },
                )))
                .into_any_element(),
            SyncFlow::Importing { done, total } => {
                let fraction = if total == 0 {
                    0.0
                } else {
                    (done as f32 / total as f32).clamp(0.0, 1.0)
                };
                let label: SharedString = if total == 0 {
                    "Looking for local sessions…".into()
                } else {
                    format!("Importing session {} of {total}", (done + 1).min(total)).into()
                };
                let mut card = popover::dialog_card(&theme)
                    .child(popover::dialog_title(&theme, "Bringing your work over"))
                    .child(
                        div()
                            .mt(px(6.0))
                            .child(popover::dialog_body(&theme, label)),
                    );
                if let Some(current) = self.import_current.clone() {
                    card = card.child(
                        div()
                            .mt(px(4.0))
                            .text_size(crate::typography::ui_rems(12.0))
                            .line_height(crate::typography::ui_rems(17.0))
                            .text_color(theme.text_muted)
                            .overflow_hidden()
                            .child(current),
                    );
                }
                card.child(
                    // Determinate progress: a hairline track with an accent fill.
                    div()
                        .mt(px(14.0))
                        .h(px(4.0))
                        .w_full()
                        .rounded(px(2.0))
                        .bg(theme.border)
                        .child(
                            div()
                                .h_full()
                                .rounded(px(2.0))
                                .bg(theme.accent_strong)
                                .w(gpui::relative(fraction.max(0.04))),
                        ),
                )
                .into_any_element()
            }
            SyncFlow::ImportDone { imported, skipped } => {
                let body: SharedString = match (imported, skipped) {
                    (0, 0) => "Your synced workspace is ready.".into(),
                    (n, 0) => format!(
                        "{n} session{} moved into your synced workspace.",
                        if n == 1 { "" } else { "s" },
                    )
                    .into(),
                    (n, s) => format!(
                        "{n} session{} imported, {s} already present.",
                        if n == 1 { "" } else { "s" },
                    )
                    .into(),
                };
                popover::dialog_card(&theme)
                    .child(popover::dialog_title(&theme, "You're all set"))
                    .child(div().mt(px(6.0)).child(popover::dialog_body(&theme, body)))
                    .child(
                        div()
                            .mt(px(16.0))
                            .flex()
                            .flex_row()
                            .justify_end()
                            .child(
                                popover::btn_primary(&theme, "Continue")
                                    .id("sync-switch-done")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.sync_flow = SyncFlow::Idle;
                                        cx.notify();
                                    })),
                            ),
                    )
                    .into_any_element()
            }
            SyncFlow::ImportFailed { notice_open: true } => popover::dialog_card(&theme)
                .child(popover::dialog_title(&theme, "Import didn't finish"))
                .child(div().mt(px(6.0)).child(popover::dialog_body(
                    &theme,
                    "Anything already imported is kept; retrying only copies what's missing.",
                )))
                .when_some(self.runtime_change_error.clone(), |card, error| {
                    card.child(
                        div()
                            .mt(px(10.0))
                            .text_size(crate::typography::ui_rems(12.0))
                            .line_height(crate::typography::ui_rems(17.0))
                            .text_color(theme.danger)
                            .child(error),
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
                            popover::btn_ghost(&theme, "Later", "import-failed-dismiss")
                                .id("import-failed-dismiss")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.postpone_sync_restart(cx)
                                })),
                        )
                        .child(
                            popover::btn_primary(&theme, "Retry import")
                                .id("import-failed-retry")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.spawn_local_import(cx)
                                })),
                        ),
                )
                .into_any_element(),
            SyncFlow::RestartPending { notice_open: true } => popover::dialog_card(&theme)
                .child(popover::dialog_title(
                    &theme,
                    "Sync needs a restart",
                ))
                .child(
                    div().mt(px(6.0)).child(popover::dialog_body(
                        &theme,
                        if remote_engine {
                            "LUNA is using a background daemon. Stop it and quit LUNA, then reopen to start the synced workspace. Existing local sessions stay on this device and will not be uploaded."
                        } else {
                            "Quit and reopen LUNA to start the synced workspace. Existing local sessions stay on this device and will not be uploaded."
                        },
                    )),
                )
                .when_some(self.runtime_change_error.clone(), |card, error| {
                    card.child(
                        div()
                            .mt(px(10.0))
                            .text_size(crate::typography::ui_rems(12.0))
                            .line_height(crate::typography::ui_rems(17.0))
                            .text_color(theme.danger)
                            .child(error),
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
                            popover::btn_ghost(&theme, "Later", "sync-restart-later")
                                .id("sync-restart-later")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.postpone_sync_restart(cx)
                                })),
                        )
                        .child(
                            popover::btn_primary(&theme, runtime_change_label)
                                .id("sync-restart-quit")
                                .when(self.runtime_change_task.is_some(), |button| {
                                    button.opacity(0.6)
                                })
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.quit_for_runtime_change(cx)
                                })),
                        ),
                )
                .into_any_element(),
            SyncFlow::SignOutConfirm => popover::dialog_card(&theme)
                .child(popover::dialog_title(&theme, "Sign out?"))
                .child(
                    div().mt(px(6.0)).child(popover::dialog_body(
                        &theme,
                        "LUNA will remove your credentials, close the synced workspace, and continue in local mode.",
                    )),
                )
                .child(
                    div()
                        .mt(px(16.0))
                        .flex()
                        .flex_row()
                        .justify_end()
                        .gap(px(8.0))
                        .child(
                            popover::btn_ghost(&theme, "Cancel", "signout-cancel")
                                .id("signout-cancel")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.sync_flow = SyncFlow::Idle;
                                    cx.notify();
                                })),
                        )
                        .child(
                            popover::btn_danger(&theme, "Sign out")
                                .id("signout-confirm")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.confirm_sign_out(cx)
                                })),
                        ),
                )
                .into_any_element(),
            SyncFlow::SigningOut => popover::dialog_card(&theme)
                .child(popover::dialog_title(&theme, "Signing out…"))
                .child(
                    div().mt(px(6.0)).child(popover::dialog_body(
                        &theme,
                        "Removing account credentials and closing the synced workspace.",
                    )),
                )
                .into_any_element(),
            SyncFlow::Idle
            | SyncFlow::SwitchOffer { notice_open: false }
            | SyncFlow::ImportFailed { notice_open: false }
            | SyncFlow::RestartPending { notice_open: false }
            | SyncFlow::SignedOutRestartRequired => return None,
        };

        Some(popover::modal("sync-lifecycle-dialog", viewport, card))
    }

    fn active_changes(&self, cx: &App) -> Option<Entity<Changes>> {
        if !self.right_pane_open(cx) {
            return None;
        }
        let RightSurface::Diff(id) = self.resolved_right_active(cx) else {
            return None;
        };
        self.diffs.get(&id).cloned()
    }

    /// Resolve shell-owned Escape surfaces in capture phase, before focused
    /// descendants such as an integrated terminal can consume the key.
    fn capture_escape_surface(&mut self, cx: &mut Context<Self>) -> bool {
        // Modals and context menus sit above the rest of the shell. Preserve
        // their existing behavior: only surfaces that already have a Cancel
        // path close here; the others remain explicit blockers.
        if self.sync_flow.has_visible_overlay()
            || self.task_action_confirm.is_some()
            || self.delete_space_confirm.is_some()
            || self.chat_menu.get().is_some()
            || self.space_menu.get().is_some()
        {
            return true;
        }
        if self.rename_dialog.is_some() {
            self.rename_dialog = None;
            cx.notify();
            return true;
        }
        if self.history_fork_dialog.is_some() {
            self.cancel_history_flow(cx);
            return true;
        }
        if self.conversation_rewind_dialog.is_some() {
            self.cancel_history_flow(cx);
            return true;
        }
        if self.edit_earlier_message_dialog.is_some() {
            self.cancel_history_flow(cx);
            return true;
        }
        if self.history_recovery_dialog.is_some() {
            self.cancel_history_flow(cx);
            return true;
        }
        if self.rename_space_dialog.is_some() {
            self.rename_space_dialog = None;
            cx.notify();
            return true;
        }
        if self.add_space.is_some() {
            self.dismiss_add_space(cx);
            return true;
        }
        if self.spaces_menu.is_open() {
            self.close_spaces_menu(cx);
            return true;
        }
        if self.spaces_menu.get().is_some() {
            return true;
        }

        if self.right_plus.is_open() {
            self.close_right_plus(cx);
            return true;
        }
        if self.right_plus.get().is_some() {
            return true;
        }
        self.active_changes(cx)
            .is_some_and(|changes| changes.update(cx, |changes, cx| changes.handle_escape(cx)))
    }

    fn on_key_down_capture(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.keystroke.key == "escape" && self.command_palette.is_some() {
            self.close_command_palette(window, cx);
            cx.stop_propagation();
            return;
        }
        if event.keystroke.key == "escape" && self.capture_escape_surface(cx) {
            cx.stop_propagation();
            return;
        }
        if event.keystroke.key == "escape" && self.usage_card_open {
            self.usage_card_open = false;
            window.focus(&self.usage_action_focus, cx);
            cx.stop_propagation();
            cx.notify();
            return;
        }
    }

    fn on_key_down(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Inputs and completion menus consume Tab first. Unhandled Tab walks
        // accessible controls, including individual transcript link ranges.
        let modifiers = event.keystroke.modifiers;
        if event.keystroke.key == "tab"
            && !modifiers.control
            && !modifiers.alt
            && !modifiers.platform
        {
            if modifiers.shift {
                window.focus_prev(cx);
            } else {
                window.focus_next(cx);
            }
            cx.stop_propagation();
            return;
        }
        let selected_chat = self.state.read(cx).selected_chat.clone();
        let indicator = selected_chat
            .as_deref()
            .map(|chat_id| self.state.read(cx).indicator_for(chat_id, Utc::now()))
            .unwrap_or(Indicator::None);
        let interrupting = selected_chat
            .as_deref()
            .is_some_and(|chat_id| self.composer.read(cx).is_interrupting(chat_id));
        let escape_stops_active_agent = self.settings.escape_stops_active_agent;

        match resolve_shell_escape(
            &event.keystroke.key,
            false,
            escape_stops_active_agent,
            self.route,
            selected_chat.as_deref(),
            indicator,
            interrupting,
        ) {
            ShellEscapeOutcome::Blocked => cx.stop_propagation(),
            ShellEscapeOutcome::InterruptChat(chat_id) => {
                cx.stop_propagation();
                self.composer
                    .update(cx, |composer, cx| composer.interrupt_chat(chat_id, cx));
            }
            ShellEscapeOutcome::OtherKey | ShellEscapeOutcome::Ignored => {}
        }
    }

    fn render_overlays(
        &mut self,
        viewport: gpui::Size<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let theme = Theme::of(cx).for_popup();
        let mut overlays: Vec<AnyElement> = Vec::new();

        if let Some(menu_state) = self.chat_menu.get().cloned() {
            let chat_id = menu_state.chat_id;
            let position = menu_state.position;
            let chat_menu_closing = self.chat_menu.closing_since();
            let rename_id = chat_id.clone();
            let archive_id = chat_id.clone();
            let delete_id = chat_id.clone();
            let mut attention_task_keys = self.activity_task_keys(cx);
            attention_task_keys.extend(self.settlement_blocking_task_keys());
            let now = Utc::now();
            let (
                archived,
                pinned,
                unread,
                settled,
                can_settle,
                can_move_up,
                can_move_down,
                native_history_fork,
            ) = {
                let state = self.state.read(cx);
                let chat = state.chats.iter().find(|chat| chat.id == chat_id);
                let has_attention = chat.is_some_and(|chat| {
                    attention_task_keys.contains(&(chat.device_id.clone(), chat.id.clone()))
                });
                let has_queued_message =
                    state.selected_chat.as_deref() == Some(&chat_id) && !state.queue.is_empty();
                let settled = chat.is_some_and(|chat| {
                    spaces::is_settled_chat(
                        chat,
                        state.display_status_for(chat, now),
                        now,
                        self.settings.sidebar_auto_settle_after_days,
                        has_attention,
                    )
                });
                let can_settle = chat.is_some_and(|chat| {
                    spaces::can_manually_settle_chat(
                        chat,
                        state.display_status_for(chat, now),
                        has_attention,
                        has_queued_message,
                    )
                });
                let mut pins: Vec<_> = state
                    .chats
                    .iter()
                    .filter(|chat| !chat.archived && chat.pinned_order.is_some())
                    .collect();
                pins.sort_by(|left, right| {
                    luna_proto::compare_pin_order(
                        left.pinned_order.as_deref().expect("filtered pin key"),
                        right.pinned_order.as_deref().expect("filtered pin key"),
                    )
                    .then_with(|| left.id.cmp(&right.id))
                });
                let pin_index = pins.iter().position(|chat| chat.id == chat_id);
                (
                    chat.is_some_and(|chat| chat.archived),
                    chat.is_some_and(|chat| chat.pinned_order.is_some()),
                    chat.is_some_and(luna_proto::Chat::unseen),
                    settled,
                    can_settle,
                    pin_index.is_some_and(|index| index > 0),
                    pin_index.is_some_and(|index| index + 1 < pins.len()),
                    chat.and_then(|chat| chat.config.as_ref())
                        .is_some_and(|config| {
                            matches!(config.harness, HarnessId::Codex | HarnessId::Opencode)
                        }),
                )
            };
            let pin_id = chat_id.clone();
            let move_up_id = chat_id.clone();
            let move_down_id = chat_id.clone();
            let read_id = chat_id.clone();
            let settle_id = chat_id.clone();
            let move_project_id = chat_id.clone();
            let fork_task_id = chat_id.clone();
            let context_task_id = chat_id.clone();
            let recover_task_id = chat_id.clone();
            let menu = popover::popover_card(&theme)
                .w(px(216.0))
                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                    this.close_chat_menu(cx);
                }))
                .flex()
                .flex_col();
            let menu = match menu_state.page {
                ChatMenuPage::Root => menu
                    .when(!archived, |menu| {
                        menu.child(
                            popover::menu_row(&theme, false, format!("chat-menu-pin-{chat_id}"))
                                .id("chat-menu-pin")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.set_chat_pinned(pin_id.clone(), !pinned, cx)
                                }))
                                .child(icon(icons::TAG).size(px(16.0)).text_color(theme.text_muted))
                                .child(SharedString::from(if pinned { "Unpin" } else { "Pin" })),
                        )
                    })
                    .when(can_move_up, |menu| {
                        menu.child(
                            popover::menu_row(&theme, false, format!("chat-menu-pin-up-{chat_id}"))
                                .id("chat-menu-pin-up")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.reorder_pinned_chat(move_up_id.clone(), -1, cx)
                                }))
                                .child(
                                    icon(icons::ARROW_UP)
                                        .size(px(16.0))
                                        .text_color(theme.text_muted),
                                )
                                .child(SharedString::from("Move pinned up")),
                        )
                    })
                    .when(can_move_down, |menu| {
                        menu.child(
                            popover::menu_row(
                                &theme,
                                false,
                                format!("chat-menu-pin-down-{chat_id}"),
                            )
                            .id("chat-menu-pin-down")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.reorder_pinned_chat(move_down_id.clone(), 1, cx)
                            }))
                            .child(
                                icon(icons::ARROW_DOWN)
                                    .size(px(16.0))
                                    .text_color(theme.text_muted),
                            )
                            .child(SharedString::from("Move pinned down")),
                        )
                    })
                    .child(
                        popover::menu_row(&theme, false, format!("chat-menu-rename-{chat_id}"))
                            .id("chat-menu-rename")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.open_rename_chat(rename_id.clone(), cx)
                            }))
                            .child(icon(icons::PEN).size(px(16.0)).text_color(theme.text_muted))
                            .child(SharedString::from("Rename…")),
                    )
                    .child(
                        popover::menu_row(&theme, false, format!("chat-menu-read-{chat_id}"))
                            .id("chat-menu-read")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.set_chat_read(read_id.clone(), unread, cx)
                            }))
                            .child(
                                icon(if unread {
                                    icons::EYE
                                } else {
                                    icons::EYE_CLOSED
                                })
                                .size(px(16.0))
                                .text_color(theme.text_muted),
                            )
                            .child(SharedString::from(if unread {
                                "Mark read"
                            } else {
                                "Mark unread"
                            })),
                    )
                    .when(!archived && (settled || can_settle), |menu| {
                        menu.child(
                            popover::menu_row(&theme, false, format!("chat-menu-settle-{chat_id}"))
                                .id("chat-menu-settle")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.set_chat_settled(settle_id.clone(), !settled, cx)
                                }))
                                .child(
                                    icon(icons::CHECK)
                                        .size(px(16.0))
                                        .text_color(theme.text_muted),
                                )
                                .child(SharedString::from(if settled {
                                    "Return to Active"
                                } else {
                                    "Settle"
                                })),
                        )
                    })
                    .child(
                        popover::menu_row(&theme, false, format!("chat-menu-move-{chat_id}"))
                            .id("chat-menu-move")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.open_move_project(move_project_id.clone(), cx)
                            }))
                            .child(
                                icon(icons::FOLDER)
                                    .size(px(16.0))
                                    .text_color(theme.text_muted),
                            )
                            .child(SharedString::from("Move to project…")),
                    )
                    .child(
                        popover::menu_row(&theme, false, format!("chat-menu-fork-{chat_id}"))
                            .id("chat-menu-fork")
                            .when(native_history_fork, |row| {
                                row.on_click(cx.listener(move |this, _, _, cx| {
                                    this.open_history_fork(
                                        fork_task_id.clone(),
                                        ConversationForkBoundaryKind::Tip,
                                        None,
                                        cx,
                                    )
                                }))
                            })
                            .when(!native_history_fork, |row| {
                                row.on_click(cx.listener(move |this, _, _, cx| {
                                    this.close_chat_menu(cx);
                                    this.start_new_task_with_context(&context_task_id, cx);
                                }))
                            })
                            .child(
                                icon(icons::GIT_BRANCH)
                                    .size(px(16.0))
                                    .text_color(theme.text_muted),
                            )
                            .child(SharedString::from(if native_history_fork {
                                "Fork into new task…"
                            } else {
                                "Start new task with context…"
                            })),
                    )
                    .child(
                        popover::menu_row(&theme, false, format!("chat-menu-recover-{chat_id}"))
                            .id("chat-menu-recover")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.open_history_recovery(recover_task_id.clone(), cx)
                            }))
                            .child(
                                icon(icons::REFRESH)
                                    .size(px(16.0))
                                    .text_color(theme.text_muted),
                            )
                            .child(SharedString::from("Restore earlier history…")),
                    )
                    .child(
                        popover::menu_row(&theme, false, format!("chat-menu-archive-{chat_id}"))
                            .id("chat-menu-archive")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.set_chat_archived(archive_id.clone(), !archived, cx)
                            }))
                            .child(
                                icon(icons::ARCHIVE_MINIMALISTIC)
                                    .size(px(16.0))
                                    .text_color(theme.text_muted),
                            )
                            .child(SharedString::from(if archived {
                                "Unarchive"
                            } else {
                                "Archive"
                            })),
                    )
                    .child(
                        popover::menu_row(&theme, false, format!("chat-menu-copy-{chat_id}"))
                            .id("chat-menu-copy")
                            .on_click(cx.listener(|this, _, _, cx| this.open_chat_copy_menu(cx)))
                            .child(
                                icon(icons::COPY)
                                    .size(px(16.0))
                                    .text_color(theme.text_muted),
                            )
                            .child(div().flex_1().child(SharedString::from("Copy")))
                            .child(
                                icon(icons::ALT_ARROW_RIGHT)
                                    .size(px(14.0))
                                    .text_color(theme.text_muted),
                            ),
                    )
                    .child(popover::menu_separator())
                    .child(
                        popover::menu_row(&theme, false, format!("chat-menu-delete-{chat_id}"))
                            .id("chat-menu-delete")
                            .text_color(theme.danger)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.close_chat_menu(cx);
                                this.request_task_action(
                                    delete_id.clone(),
                                    TaskConfirmation::Delete,
                                    cx,
                                );
                            }))
                            .child(
                                icon(icons::TRASH_BIN_MINIMALISTIC)
                                    .size(px(16.0))
                                    .text_color(theme.danger),
                            )
                            .child(SharedString::from("Delete…")),
                    ),
                ChatMenuPage::Copy => {
                    let chat = self
                        .state
                        .read(cx)
                        .chats
                        .iter()
                        .find(|chat| chat.id == chat_id)
                        .cloned();
                    let harness_link = chat
                        .as_ref()
                        .and_then(crate::links::harness_conversation_link);
                    let session_id = chat
                        .as_ref()
                        .and_then(|chat| chat.harness_session_id.as_deref())
                        .is_some_and(|id| !id.trim().is_empty());
                    let luna_id = chat_id.clone();
                    let harness_id = chat_id.clone();
                    let session_chat_id = chat_id.clone();
                    menu.child(
                        popover::menu_row(&theme, false, format!("chat-copy-back-{chat_id}"))
                            .id("chat-copy-back")
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(menu) = this.chat_menu.open_mut() {
                                    menu.page = ChatMenuPage::Root;
                                    cx.notify();
                                }
                            }))
                            .child(
                                icon(icons::ALT_ARROW_LEFT)
                                    .size(px(16.0))
                                    .text_color(theme.text_muted),
                            )
                            .child(SharedString::from("Back")),
                    )
                    .child(popover::menu_separator())
                    .child(
                        popover::menu_row(&theme, false, format!("chat-copy-luna-{chat_id}"))
                            .id("chat-copy-luna")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.copy_luna_conversation_link(&luna_id, cx)
                            }))
                            .child(
                                icon(icons::COPY)
                                    .size(px(16.0))
                                    .text_color(theme.text_muted),
                            )
                            .child(SharedString::from("LUNA task link")),
                    )
                    .when_some(harness_link, |menu, link| {
                        menu.child(
                            popover::menu_row(
                                &theme,
                                false,
                                format!("chat-copy-harness-{chat_id}"),
                            )
                            .id("chat-copy-harness")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.copy_harness_conversation_link(&harness_id, cx)
                            }))
                            .child(
                                icon(icons::COPY)
                                    .size(px(16.0))
                                    .text_color(theme.text_muted),
                            )
                            .child(SharedString::from(link.label)),
                        )
                    })
                    .when(session_id, |menu| {
                        menu.child(
                            popover::menu_row(
                                &theme,
                                false,
                                format!("chat-copy-session-{chat_id}"),
                            )
                            .id("chat-copy-session")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.copy_harness_session_id(&session_chat_id, cx)
                            }))
                            .child(
                                icon(icons::COPY)
                                    .size(px(16.0))
                                    .text_color(theme.text_muted),
                            )
                            .child(SharedString::from("Provider session ID")),
                        )
                    })
                }
            }
            .into_any_element();
            overlays.push(popover::menu_at(
                "chat-context-menu",
                position,
                menu,
                chat_menu_closing,
            ));
        }

        if let Some(dialog) = &mut self.rename_dialog {
            if std::mem::take(&mut dialog.focus_pending) {
                window.focus(&dialog.input.focus_handle(cx), cx);
            }
            let input = dialog.input.clone();
            let card = popover::dialog_card(&theme)
                .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, _, cx| {
                    if ev.keystroke.key == "escape" {
                        this.rename_dialog = None;
                        cx.notify();
                        cx.stop_propagation();
                    }
                }))
                .child(popover::dialog_title(&theme, "Rename session"))
                .child(
                    div()
                        .mt(px(12.0))
                        .child(popover::dialog_field(input.into_any_element())),
                )
                .child(
                    div()
                        .mt(px(16.0))
                        .flex()
                        .flex_row()
                        .justify_end()
                        .gap(px(8.0))
                        .child(
                            popover::btn_ghost(&theme, "Cancel", "rename-chat-cancel")
                                .id("rename-chat-cancel")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.rename_dialog = None;
                                    cx.notify();
                                })),
                        )
                        .child(
                            popover::btn_primary(&theme, "Rename")
                                .id("rename-chat-save")
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.submit_rename_chat(cx)),
                                ),
                        ),
                )
                .into_any_element();
            overlays.push(popover::modal("rename-chat-dialog", viewport, card));
        }

        if let Some(dialog) = &mut self.history_fork_dialog {
            if std::mem::take(&mut dialog.focus_pending) {
                window.focus(&dialog.title.focus_handle(cx), cx);
            }
            let title = dialog.title.clone();
            let prompt = dialog.prompt.clone();
            let permission = dialog.config.permission_mode;
            let permission_ceiling = dialog.permission_ceiling;
            let permission_label = match (permission, permission_ceiling) {
                (PermissionMode::Ask, _) => "Ask (legacy; unchanged)",
                (PermissionMode::Auto, PermissionMode::Auto) => "Auto (Full unavailable)",
                (PermissionMode::Auto, _) => "Auto",
                (PermissionMode::Full, _) => "Full",
            };
            let permission_selectable = permission_ceiling >= PermissionMode::Auto;
            let cwd = dialog.cwd.clone();
            let card = popover::dialog_card(&theme)
                .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, _, cx| {
                    if ev.keystroke.key == "escape" {
                        this.cancel_history_flow(cx);
                        cx.stop_propagation();
                    }
                }))
                .child(popover::dialog_title(&theme, "Fork into new task"))
                .child(
                    div()
                        .mt(px(8.0))
                        .text_size(crate::typography::ui_rems(12.0))
                        .text_color(theme.text_muted)
                        .child("Creates an independent provider conversation. Files and external effects are not copied or rewound."),
                )
                .child(
                    div()
                        .mt(px(12.0))
                        .child(popover::dialog_field(title.into_any_element())),
                )
                .child(
                    div()
                        .mt(px(8.0))
                        .child(popover::dialog_field(prompt.into_any_element())),
                )
                .child(
                    div()
                        .mt(px(12.0))
                        .flex()
                        .flex_col()
                        .gap(px(6.0))
                        .child(
                            div()
                                .text_size(crate::typography::ui_rems(12.0))
                                .text_color(theme.text_muted)
                                .child(format!("Environment: current checkout · {cwd}")),
                        )
                        .child(
                            div()
                                .text_size(crate::typography::ui_rems(12.0))
                                .text_color(theme.readable_muted(0.55))
                                .child("New worktree: unavailable for conversation-only forks"),
                        )
                        .child(
                            popover::btn_ghost(
                                &theme,
                                &format!("Permission: {permission_label}"),
                                "history-fork-permission",
                            )
                            .id("history-fork-permission")
                            .opacity(if permission_selectable { 1.0 } else { 0.58 })
                            .when(!permission_selectable, |button| button.cursor_default())
                            .when(permission_selectable, |button| {
                                button.on_click(cx.listener(move |this, _, _, cx| {
                                    if let Some(dialog) = this.history_fork_dialog.as_mut()
                                        && let Some(next) = next_history_fork_permission(
                                            dialog.config.permission_mode,
                                            dialog.permission_ceiling,
                                        )
                                    {
                                        dialog.config.permission_mode = next;
                                    }
                                    cx.notify();
                                }))
                            }),
                        ),
                )
                .child(
                    div()
                        .mt(px(16.0))
                        .flex()
                        .justify_end()
                        .gap(px(8.0))
                        .child(
                            popover::btn_ghost(&theme, "Cancel", "history-fork-cancel")
                                .id("history-fork-cancel")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.cancel_history_flow(cx);
                                })),
                        )
                        .child(
                            popover::btn_primary(&theme, "Create fork")
                                .id("history-fork-confirm")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.submit_history_fork(cx)
                                })),
                        ),
                )
                .into_any_element();
            overlays.push(popover::modal("history-fork-dialog", viewport, card));
        }

        if let Some(dialog) = &self.conversation_rewind_dialog {
            let pending = dialog.pending_rewind.is_some();
            let text = dialog.text.chars().take(500).collect::<String>();
            let card = popover::dialog_card(&theme)
                .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, _, cx| {
                    if ev.keystroke.key == "escape" { this.cancel_history_flow(cx); cx.stop_propagation(); }
                }))
                .child(popover::dialog_title(&theme, if pending { "Reconcile conversation rewind" } else { "Rewind this conversation" }))
                .child(div().mt(px(8.0)).text_size(crate::typography::ui_rems(12.0)).text_color(theme.text_muted)
                    .child(if pending {
                        "Read the saved provider history to finish or dismiss the unresolved rewind. Revert will not be sent again. Later LUNA rows stay visible until the retained prefix is verified."
                    } else {
                        "Remove this message and every later turn from this same Codex conversation. The provider's removed history cannot be restored by LUNA; an audit snapshot is kept. Your draft, files and external effects are kept."
                    }))
                .when(!pending, |card| card.child(div().id("history-rewind-message-preview").mt(px(10.0)).max_h(px(100.0)).overflow_y_scroll()
                    .text_size(crate::typography::ui_rems(11.0)).child(SharedString::from(text))))
                .child(div().mt(px(12.0)).text_size(crate::typography::ui_rems(11.0)).text_color(theme.text_muted)
                    .child("Files: keep current files. To restore files, review a Git checkpoint separately in Changes. Unsaved files must be saved before restoring."))
                .child(div().mt(px(16.0)).flex().justify_end().gap(px(8.0))
                    .child(popover::btn_ghost(&theme, "Cancel", "history-rewind-cancel")
                        .id("history-rewind-cancel").on_click(cx.listener(|this, _, _, cx| this.cancel_history_flow(cx))))
                    .when(!pending, |buttons| buttons.child(popover::btn_ghost(&theme, "Preview file checkpoints", "history-rewind-files")
                        .id("history-rewind-files").on_click(cx.listener(|this, _, _, cx| {
                            this.cancel_history_flow(cx);
                            let changes = cx.new(|cx| Changes::new(this.state.clone(), cx));
                            this.register_diff_surface(changes.clone(), cx);
                            changes.update(cx, |changes, cx| changes.open_file_checkpoints(cx));
                        }))))
                    .child(popover::btn_primary(&theme, if pending { "Reconcile rewind" } else { "Rewind conversation · keep files" })
                        .id("history-rewind-confirm").on_click(cx.listener(|this, _, _, cx| this.submit_conversation_rewind(cx)))))
                .into_any_element();
            overlays.push(popover::modal("history-rewind-dialog", viewport, card));
        }

        if let Some(dialog) = &mut self.edit_earlier_message_dialog {
            if std::mem::take(&mut dialog.focus_pending) {
                window.focus(&dialog.input.focus_handle(cx), cx);
            }
            let input = dialog.input.clone();
            let card = popover::dialog_card(&theme)
                .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, _, cx| {
                    if ev.keystroke.key == "escape" {
                        this.cancel_history_flow(cx);
                        cx.stop_propagation();
                    }
                }))
                .child(popover::dialog_title(&theme, "Edit earlier message"))
                .child(
                    div()
                        .mt(px(8.0))
                        .text_size(crate::typography::ui_rems(12.0))
                        .text_color(theme.text_muted)
                        .child("LUNA first stages a native conversation fork and recovery snapshot. Files, tool calls, and external effects are not undone."),
                )
                .child(
                    div()
                        .mt(px(12.0))
                        .child(popover::dialog_field(input.into_any_element())),
                )
                .child(
                    div()
                        .mt(px(16.0))
                        .flex()
                        .justify_end()
                        .gap(px(8.0))
                        .child(
                            popover::btn_ghost(&theme, "Cancel", "history-edit-cancel")
                                .id("history-edit-cancel")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.cancel_history_flow(cx);
                                })),
                        )
                        .child(
                            popover::btn_primary(&theme, "Rewrite conversation")
                                .id("history-edit-confirm")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.submit_edit_earlier_message(cx)
                                })),
                        ),
                )
                .into_any_element();
            overlays.push(popover::modal("history-edit-dialog", viewport, card));
        }

        if let Some(dialog) = &self.history_recovery_dialog {
            let recoveries = dialog.recoveries.clone();
            let selected = dialog.selected;
            let rows = recoveries
                .iter()
                .enumerate()
                .map(|(index, recovery)| {
                    let label = format!(
                        "{} · {}",
                        recovery.summary,
                        chrono::DateTime::from_timestamp_millis(recovery.created_at_ms)
                            .map(|value| value
                                .with_timezone(&chrono::Local)
                                .format("%b %-d, %-I:%M %p")
                                .to_string())
                            .unwrap_or_else(|| "unknown time".into())
                    );
                    popover::menu_row(
                        &theme,
                        index == selected,
                        format!("history-recovery-{index}"),
                    )
                    .id(("history-recovery-row", index))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(dialog) = this.history_recovery_dialog.as_mut() {
                            dialog.selected = index;
                        }
                        cx.notify();
                    }))
                    .child(SharedString::from(label))
                })
                .collect::<Vec<_>>();
            let card = popover::dialog_card(&theme)
                .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, _, cx| {
                    if ev.keystroke.key == "escape" {
                        this.cancel_history_flow(cx);
                        cx.stop_propagation();
                    }
                }))
                .child(popover::dialog_title(&theme, "Restore earlier history"))
                .child(
                    div()
                        .mt(px(8.0))
                        .text_size(crate::typography::ui_rems(12.0))
                        .text_color(theme.text_muted)
                        .child("Restoring creates another independent provider fork and keeps the current conversation as an undo recovery. Files and external effects are unchanged."),
                )
                .child(div().mt(px(12.0)).flex().flex_col().children(rows))
                .child(
                    div()
                        .mt(px(16.0))
                        .flex()
                        .justify_end()
                        .gap(px(8.0))
                        .child(
                            popover::btn_ghost(&theme, "Cancel", "history-recovery-cancel")
                                .id("history-recovery-cancel")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.cancel_history_flow(cx);
                                })),
                        )
                        .child(
                            popover::btn_primary(&theme, "Restore")
                                .id("history-recovery-confirm")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.submit_history_recovery(cx)
                                })),
                        ),
                )
                .into_any_element();
            overlays.push(popover::modal("history-recovery-dialog", viewport, card));
        }

        if self.move_project_dialog.is_some() {
            let (chat_id, search, selected_space_id) = {
                let dialog = self.move_project_dialog.as_mut().expect("checked above");
                if std::mem::take(&mut dialog.focus_pending) {
                    window.focus(&dialog.search.focus_handle(cx), cx);
                }
                (
                    dialog.chat_id.clone(),
                    dialog.search.clone(),
                    dialog.selected_space_id.clone(),
                )
            };
            let query = search.read(cx).text().to_string();
            let (chat, spaces, source_space) = {
                let state = self.state.read(cx);
                let chat = state.chats.iter().find(|chat| chat.id == chat_id).cloned();
                let spaces = state
                    .spaces_sorted()
                    .into_iter()
                    .cloned()
                    .collect::<Vec<_>>();
                let source_space = chat
                    .as_ref()
                    .and_then(|chat| state.space_for_chat(chat))
                    .cloned();
                (chat, spaces, source_space)
            };
            let names: Vec<_> = spaces
                .iter()
                .map(|space| format!("{} @ {}", space.display_name(), space.device_id))
                .collect();
            let rows = popover::filter_indices(&query, &names)
                .into_iter()
                .map(|index| {
                    let space = spaces[index].clone();
                    let label = names[index].clone();
                    let selected = selected_space_id.as_deref() == Some(space.id.as_str());
                    popover::menu_row(&theme, selected, format!("move-project-{}", space.id))
                        .id(("move-project-row", index))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(dialog) = this.move_project_dialog.as_mut() {
                                dialog.selected_space_id = Some(space.id.clone());
                            }
                            cx.notify();
                        }))
                        .child(
                            icon(icons::FOLDER)
                                .size(px(14.0))
                                .text_color(theme.text_muted),
                        )
                        .child(div().flex_1().min_w_0().truncate().child(label))
                })
                .collect::<Vec<_>>();
            let destination = selected_space_id
                .as_deref()
                .and_then(|id| spaces.iter().find(|space| space.id == id));
            let (available, availability): (bool, SharedString) = match (chat.as_ref(), destination)
            {
                (Some(chat), Some(destination))
                    if chat.space_id.as_deref() == Some(destination.id.as_str()) =>
                {
                    (false, "Already in this project".into())
                }
                (Some(chat), Some(destination)) => {
                    let cwd = chat.cwd.as_deref().unwrap_or("Unavailable");
                    let source_name = source_space
                        .as_ref()
                        .map(|space| space.display_name())
                        .unwrap_or("Unknown project");
                    let source_root = source_space
                        .as_ref()
                        .map(|space| space.path.as_str())
                        .or(chat.cwd.as_deref())
                        .unwrap_or("Unavailable");
                    let source_checkout = source_space
                        .as_ref()
                        .and_then(|space| space.checkout_id.as_deref())
                        .or(chat.checkout_id.as_deref())
                        .unwrap_or("Unavailable / non-Git");
                    let destination_checkout = destination
                        .checkout_id
                        .as_deref()
                        .unwrap_or("Unavailable / non-Git");
                    let provider = if chat
                        .harness_session_id
                        .as_deref()
                        .is_some_and(|id| !id.is_empty())
                    {
                        "Provider session: retained only after engine validation succeeds"
                    } else {
                        "Provider session: unavailable; task identity/history remain unchanged"
                    };
                    let known_incompatible_machine = chat.device_id != destination.device_id;
                    let known_incompatible_checkout = chat.checkout_id.is_some()
                        && destination.checkout_id.is_some()
                        && chat.checkout_id != destination.checkout_id;
                    let (available, validation) = if known_incompatible_machine {
                        (false, "Unavailable: start a new task in that environment")
                    } else if known_incompatible_checkout {
                        (false, "Unavailable: start a new task in that checkout")
                    } else {
                        (
                            true,
                            "Pending: the engine will canonicalize both roots and verify the live workspace/checkout at confirmation",
                        )
                    };
                    (
                        available,
                        format!(
                            "Source: {source_name}\nSource environment/machine: owning engine @ {}\nConfigured source root (not yet canonicalized): {source_root}\nCurrent cwd: {cwd}\nCurrent branch: {}\nSource checkout/worktree: {source_checkout}\nDestination: {}\nDestination environment/machine: owning engine @ {}\nConfigured destination root (not yet canonicalized): {}\nDestination checkout/worktree: {destination_checkout}\nValidation: {validation}\n{}\nUnchanged effects: task ID/history, read/pin state, permissions, running terminals, browser tabs, scheduled work, and remote scopes. Terminals and browser tabs remain in their current cwd/scope.",
                            chat.device_id,
                            chat.branch.as_deref().unwrap_or("Unavailable"),
                            destination.display_name(),
                            destination.device_id,
                            destination.path,
                            provider,
                        )
                        .into(),
                    )
                }
                _ => (false, "Choose a destination project".into()),
            };
            let new_project_chat = chat_id.clone();
            let card = popover::dialog_card(&theme)
                .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, _, cx| {
                    if ev.keystroke.key == "escape" {
                        this.move_project_dialog = None;
                        cx.notify();
                        cx.stop_propagation();
                    }
                }))
                .child(popover::dialog_title(&theme, "Move to project"))
                .child(div().mt(px(6.0)).child(popover::dialog_body(
                    &theme,
                    format!(
                        "Source: {}",
                        source_space
                            .as_ref()
                            .map(|space| space.display_name())
                            .unwrap_or("Unknown project")
                    ),
                )))
                .child(
                    div()
                        .mt(px(12.0))
                        .child(popover::dialog_field(search.into_any_element())),
                )
                .child(
                    div()
                        .id("move-project-list")
                        .mt(px(6.0))
                        .max_h(px(190.0))
                        .overflow_y_scroll()
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .children(rows),
                )
                .child(
                    popover::menu_row(&theme, false, "move-project-new")
                        .id("move-project-new")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.move_project_dialog = None;
                            this.open_add_space_for_move(new_project_chat.clone(), cx);
                        }))
                        .child(
                            icon(icons::PLUS)
                                .size(px(14.0))
                                .text_color(theme.text_muted),
                        )
                        .child(SharedString::from("New project…")),
                )
                .child(
                    div()
                        .mt(px(10.0))
                        .child(popover::dialog_body(&theme, availability)),
                )
                .child(
                    div()
                        .mt(px(16.0))
                        .flex()
                        .flex_row()
                        .justify_end()
                        .gap(px(8.0))
                        .child(
                            popover::btn_ghost(&theme, "Cancel", "move-project-cancel")
                                .id("move-project-cancel")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.move_project_dialog = None;
                                    cx.notify();
                                })),
                        )
                        .when(available, |actions| {
                            actions.child(
                                popover::btn_primary(&theme, "Move task")
                                    .id("move-project-confirm")
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.submit_move_project(cx)),
                                    ),
                            )
                        }),
                )
                .into_any_element();
            overlays.push(popover::modal("move-project-dialog", viewport, card));
        }

        overlays.extend(self.render_space_overlays(viewport, window, cx));
        if let Some(overlay) = self.render_command_palette(viewport, window, cx) {
            overlays.push(overlay);
        }
        if let Some(overlay) = self.render_add_space_overlay(viewport, window, cx) {
            overlays.push(overlay);
        }

        if let Some((chat_id, action)) = self.task_action_confirm.clone() {
            let title = transcript::single_line(
                &self
                    .state
                    .read(cx)
                    .chats
                    .iter()
                    .find(|c| c.id == chat_id)
                    .and_then(|c| c.title.clone())
                    .unwrap_or_else(|| "New session".into()),
            );
            let (heading, body, button_label) = match action {
                TaskConfirmation::Archive => (
                    "Archive task?",
                    format!(
                        "Move \u{201C}{title}\u{201D} to Archived? You can unarchive it later."
                    ),
                    "Archive",
                ),
                TaskConfirmation::Settle => (
                    "Settle task?",
                    format!(
                        "Move \u{201C}{title}\u{201D} to Settled? You can return it to Active later."
                    ),
                    "Settle",
                ),
                TaskConfirmation::Delete => (
                    "Delete session?",
                    format!(
                        "\u{201C}{title}\u{201D} and its LUNA-owned local records will be permanently deleted. This can\u{2019}t be undone."
                    ),
                    "Delete",
                ),
            };
            let checked = self.task_action_dont_show_again;
            let confirm_button = if action == TaskConfirmation::Delete {
                popover::btn_danger(&theme, button_label)
            } else {
                popover::btn_primary(&theme, button_label)
            };
            let card = popover::dialog_card(&theme)
                .id("task-action-card")
                .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                    if event.keystroke.key == "escape" {
                        this.task_action_confirm = None;
                        cx.notify();
                    }
                }))
                .child(popover::dialog_title(&theme, heading))
                .child(div().mt(px(6.0)).child(popover::dialog_body(&theme, body)))
                .when(action == TaskConfirmation::Delete, |card| {
                    card.child(div().mt(px(6.0)).child(popover::dialog_body(
                        &theme,
                        "LUNA-managed execution will stop. LUNA removes its local transcript snapshot, unsent draft, terminal history and verified unshared draft uploads. Independent forks, project files, shared attachments, synced copies and provider-private data remain. Provider cleanup is unsupported.",
                    )))
                })
                .child(
                    div()
                        .id("task-action-dont-show-again")
                        .mt(px(16.0))
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .role(gpui::Role::CheckBox)
                        .aria_label("Don't show this confirmation again")
                        .aria_toggled(if checked { gpui::Toggled::True } else { gpui::Toggled::False })
                        .tab_index(0)
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.task_action_dont_show_again = !this.task_action_dont_show_again;
                            cx.notify();
                        }))
                        .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                            if !event.is_held && matches!(event.keystroke.key.as_str(), "enter" | "space") {
                                cx.stop_propagation();
                                this.task_action_dont_show_again = !this.task_action_dont_show_again;
                                cx.notify();
                            }
                        }))
                        .child(
                            div()
                                .size(px(16.0))
                                .rounded(px(4.0))
                                .border_1()
                                .border_color(if checked { theme.accent } else { theme.border })
                                .bg(if checked { theme.accent } else { theme.glass_hover() })
                                .flex()
                                .items_center()
                                .justify_center()
                                .when(checked, |box_| box_.child(icon(icons::CHECK).size(px(12.0)).text_color(theme.on_solid))),
                        )
                        .child(popover::dialog_body(&theme, "Don't show this again")),
                )
                .child(
                    div()
                        .mt(px(16.0))
                        .flex()
                        .flex_row()
                        .justify_end()
                        .gap(px(8.0))
                        .child(
                            popover::btn_ghost(&theme, "Cancel", "task-action-cancel")
                                .id("task-action-cancel")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.task_action_confirm = None;
                                    cx.notify();
                                })),
                        )
                        .child(
                            confirm_button
                                .id("task-action-confirm")
                                .on_click(cx.listener(|this, _, _, cx| this.confirm_task_action(cx))),
                        ),
                )
                .into_any_element();
            overlays.push(popover::modal("task-action-dialog", viewport, card));
        }

        if let Some(sync) = self.render_sync_overlay(viewport, cx) {
            overlays.push(sync);
        }

        overlays
    }

    fn resize_handle<T>(
        &self,
        id: &'static str,
        kind: PaneResizeKind,
        marker: fn() -> T,
        reset: fn(&mut Shell, &mut Context<Shell>),
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div>
    where
        T: 'static,
    {
        let theme = Theme::of(cx);
        let fade_key = format!("pane-resize-{id}");
        let hover_highlight = motion::hover_blend(
            &fade_key,
            theme.border_strong.opacity(0.0),
            theme.border_strong,
        );
        let active = self.pane_resize_active == Some(kind);
        let constrained = self.pane_resize_dragging == Some(kind) && !active;
        let highlight = if constrained {
            theme.border_strong.opacity(0.0)
        } else if active {
            theme.border_strong
        } else {
            hover_highlight
        };
        let clear = highlight.opacity(0.0);
        let release_key = fade_key.clone();
        let release_out_key = fade_key.clone();
        div()
            .id(id)
            .absolute()
            .top(px(PANE_RESIZE_HITBOX_TOP))
            .bottom_0()
            .w(px(PANE_RESIZE_HITBOX_HALF_WIDTH * 2.0))
            .flex_none()
            .occlude()
            .cursor_col_resize()
            .on_hover(motion::hover_listener(fade_key))
            // Codex-style seam feedback: the existing 1px panel border stays
            // visible at rest; hover adds a stronger center highlight that
            // fades back into that border toward both ends.
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(px(PANE_RESIZE_HITBOX_HALF_WIDTH))
                    .w(px(1.0))
                    .flex()
                    .flex_col()
                    .child(div().flex_1().bg(gpui::linear_gradient(
                        180.0,
                        gpui::linear_color_stop(clear, 0.0),
                        gpui::linear_color_stop(highlight, 1.0),
                    )))
                    .child(div().flex_1().bg(gpui::linear_gradient(
                        180.0,
                        gpui::linear_color_stop(highlight, 0.0),
                        gpui::linear_color_stop(clear, 1.0),
                    ))),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    this.pane_resize_dragging = Some(kind);
                    this.pane_resize_active = Some(kind);
                    cx.notify();
                }),
            )
            .on_drag(marker(), |_, _point: Point<gpui::Pixels>, _, cx| {
                cx.stop_propagation();
                cx.new(|_| DragGhost)
            })
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseUpEvent, window, cx| {
                    if event.click_count == 2 {
                        reset(this, cx);
                        this.schedule_save(cx);
                        cx.notify();
                    }
                    this.finish_pane_resize(kind);
                    motion::set_hover(&release_key, false, this.reduced_motion);
                    window.refresh();
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(move |this, _, window, _| {
                    this.finish_pane_resize(kind);
                    motion::set_hover(&release_out_key, false, this.reduced_motion);
                    window.refresh();
                }),
            )
    }

    fn render_main(
        &mut self,
        window: &mut Window,
        main_content_width: f32,
        transcript_width: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme_owned = Theme::of(cx).clone();
        let theme = &theme_owned;
        let (border, text, faint) = (theme.border, theme.text, theme.text_faint);

        // Settings route: just the section outlet — the section label lives in
        // the unified window titlebar now (render_title_bar). Settings never
        // underlaps: pad below the overlaid titlebar.
        if let Route::Settings(section) = self.route {
            let outlet = self.settings_outlet(section, window, cx);
            return div()
                .flex_1()
                .min_w_0()
                .h_full()
                .pt(px(Theme::TITLEBAR_HEIGHT))
                .flex()
                .flex_col()
                .child(div().flex_1().min_h_0().child(outlet))
                .into_any_element();
        }

        let _ = (text, border);
        let has_selection = self.state.read(cx).selected_chat.is_some();
        let has_spaces = !self.state.read(cx).spaces.is_empty();
        let has_appshots = !self.composer.read(cx).staged_appshots().is_empty();
        let transcript_geometry_ready = bottom_stack_measurement_matches(
            self.bottom_stack_has_composer.get(),
            (has_spaces || has_appshots) && has_selection,
        );
        let ui_settings = settings::current(cx);
        let new_thread_background_setting = ui_settings.new_thread_composer_background;
        let new_thread_background_effect = ui_settings.new_thread_background_effect;
        let frame_time = self.render_time.unwrap_or_else(std::time::Instant::now);
        // Prewarm even in an established thread. Decode/effect work is not
        // contingent on a hero measurement or a navigation gesture.
        let artwork = new_thread_background_setting
            .as_ref()
            .and_then(|background| {
                crate::new_thread_background_effects::prepare(
                    new_thread_background_effect,
                    theme,
                    std::path::Path::new(&background.path),
                    cx,
                )
            });
        let artwork_opacity = self.new_thread_artwork_ready.opacity(
            artwork.as_ref().map(|image| image.id),
            self.reduced_motion,
            frame_time,
        );
        let dock_frame =
            self.composer_dock
                .borrow_mut()
                .tick(has_selection, self.reduced_motion, frame_time);
        if dock_frame.active {
            self.motion_active.set(true);
        }
        self.composer
            .update(cx, |composer, cx| composer.set_dock_frame(dock_frame, cx));
        let composer_width = self.composer_dock.borrow_mut().layout_width(
            main_content_width.min(crate::composer::COMPOSER_MAX_WIDTH),
            self.reduced_motion,
            frame_time,
        );
        self.composer.update(cx, |composer, cx| {
            composer.set_available_width(composer_width, cx)
        });
        let term_h = self.eval_tween(self.terminal_tween, self.terminal_target(cx));
        let new_thread_background_layer = (!has_selection || dock_frame.active).then(|| {
            if artwork.is_some() && artwork_opacity < 1.0 {
                window.request_animation_frame();
            }
            new_thread_background(
                artwork,
                self.viewport_height,
                (self.viewport_width - self.sidebar_now()).max(0.0),
                self.composer.read(cx).surface_bounds(),
                dock_frame.dissolve(),
                artwork_opacity * new_thread_background_opacity(theme.is_frost()),
            )
        });

        // Content outlet: selected chat → transcript; nothing selected → the
        // centered new-thread composition; no spaces at all → the onboarding
        // card. New-chat mode mints the chat id on first send.
        let departing_transcript = !has_selection && dock_frame.transcript() > 0.0;
        if has_spaces || has_selection || departing_transcript {
            self.onboarding_moon = None;
        }
        if !has_selection && !departing_transcript {
            self.transcript
                .update(cx, |transcript, cx| transcript.finish_route_exit(cx));
        }
        let outlet: AnyElement = if has_selection || departing_transcript {
            div()
                .relative()
                .size_full()
                .overflow_hidden()
                .child(
                    div()
                        .relative()
                        .top(px(8.0 * (1.0 - dock_frame.transcript())))
                        .size_full()
                        .when(departing_transcript, |el| el.w(px(transcript_width)))
                        .opacity(if transcript_geometry_ready || departing_transcript {
                            dock_frame.transcript()
                        } else {
                            0.0
                        })
                        .child(self.transcript.clone()),
                )
                // A departing transcript is visual history, not an active
                // interaction surface bound to the newly blank route.
                .when(departing_transcript, |el| {
                    el.child(div().absolute().inset_0().occlude())
                })
                .into_any_element()
        } else if !has_spaces {
            // Onboarding (first boot / after the destructive wipe): no folders
            // to work in yet — one clear affordance.
            let moon_scale = (self.viewport_height / 900.0)
                .min(main_content_width / 440.0)
                .clamp(0.58, 1.0);
            let diameter = 320.0 * moon_scale;
            let moon = self.onboarding_moon.get_or_insert_with(|| {
                cx.new(|_| crate::empty_project_moon::AsciiMoon::new(diameter))
            });
            moon.update(cx, |moon, cx| moon.set_diameter(diameter, cx));
            div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .child(motion::fade_in(
                    "no-spaces-canvas",
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .child(moon.clone())
                        .child(
                            div()
                                .mt(px(22.0))
                                .text_size(crate::typography::ui_rems(20.0))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(theme.text)
                                .child(SharedString::from("What will you build?")),
                        )
                        .child(
                            popover::btn_primary(&theme_owned, "Add project")
                                .id("onboarding-add-space")
                                .role(gpui::accesskit::Role::Button)
                                .aria_label("Add project")
                                .mt(px(24.0))
                                .px(px(20.0))
                                .py(px(10.0))
                                .text_size(crate::typography::ui_rems(14.0))
                                .on_click(cx.listener(|this, _, _, cx| this.open_add_space(cx))),
                        ),
                ))
                .into_any_element()
        } else {
            Empty.into_any_element()
        };

        let status = self.render_status_strip(cx);
        // Attachment dropzone over the ENTIRE conversation column (transcript
        // + composer, not just the pill). OS images keep using the upload
        // pipeline; workspace files/directories and file tabs become the same
        // projected file-mention chips the composer already understands.
        // The veil itself uses typed `drag_over` styles below. Do not cache
        // drag presence in shell state: the platform's `FileDrop::Exited`
        // clears GPUI's external payload without sending one last mouse-move,
        // so a cached bit can survive and reappear during an unrelated drag
        // such as a pane resize.
        div()
            .id("chat-dropzone")
            .relative()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .on_drop(cx.listener(|this, paths: &gpui::ExternalPaths, _, cx| {
                let paths = paths.paths().to_vec();
                this.composer
                    .update(cx, |composer, cx| composer.add_paths(paths, cx));
                cx.notify();
            }))
            .on_drop::<WorkspacePathDrag>(cx.listener(
                |this, payload: &WorkspacePathDrag, window, cx| {
                    this.composer.update(cx, |composer, cx| {
                        composer.add_workspace_path(&payload.path, payload.is_directory, window, cx)
                    });
                    cx.notify();
                },
            ))
            .on_drop::<RightTabDrag>(cx.listener(|this, payload: &RightTabDrag, window, cx| {
                if let Some(path) = &payload.workspace_path {
                    this.composer.update(cx, |composer, cx| {
                        composer.add_workspace_path(&path.path, path.is_directory, window, cx)
                    });
                }
                cx.notify();
            }))
            // The hero is deliberately outside the transcript EdgeFade below:
            // it must paint under the overlaid titlebar instead of becoming
            // fully transparent across the titlebar's inset band.
            .children(new_thread_background_layer)
            .child(
                // Full-height underlay: the transcript viewport spans the
                // whole column, scrolling UNDER the titlebar above and the
                // composer stack below. The per-glyph EdgeFade (glass-safe,
                // same as the sidebar's) spans the full column with
                // ASYMMETRIC bands sized to the chrome: content is opaque at
                // the chrome's inner edge and fades to zero at the window
                // edge — visible mid-fade through the glass chrome it slides
                // under. Always on (the resting paddings keep pinned content
                // out of the bands, and gating on measured scroll state left
                // the top unfaded for one frame on session switch — user
                // report). The jump pill floats outside the fade scope,
                // anchored above the measured stack.
                {
                    // The terminal dock is NOT glass the transcript may slide
                    // under: with the dock's translucent fill, transcript text
                    // ghosted through the grid (user report). The underlay
                    // ends at the dock's top instead, riding the same height
                    // tween the dock animates with; `stack_h` below is only
                    // the chrome that still overlaps the transcript (status
                    // strip + composer).
                    let stack_h = (self.bottom_stack.get() - term_h).max(0.0);
                    // Opaque from the composer PILL's top (the reserved
                    // status strip above it is empty air), zero at the
                    // underlay's bottom edge.
                    let bottom_band = (stack_h - Theme::STATUS_STRIP_HEIGHT).max(1.0);
                    div().absolute().inset_0().bottom(px(term_h)).child(
                        crate::edge_fade::edge_faded(
                            Theme::TRANSCRIPT_FADE_BAND,
                            true,
                            true,
                            div().size_full().child(outlet),
                        )
                        // Fully faded BY the titlebar's bottom edge (the
                        // title text is opaque — overlap read as collision),
                        // ramping in the band just below it.
                        .inset_top(Theme::TITLEBAR_HEIGHT)
                        .band_top(Theme::TRANSCRIPT_FADE_BAND)
                        .band_bottom(bottom_band),
                    )
                },
            )
            // The glass chrome stack, floating over the transcript's bottom:
            // reserved status strip (h-6, the WorkingIndicator — the composer
            // below never shifts), composer, terminal dock. A paint-time
            // canvas measures the stack for next frame's fade inset and
            // transcript clearance. The flex_1 spacer has no id/listeners, so
            // pointer + wheel events over it fall through to the list below.
            .child(div().flex_1().min_h_0())
            .child({
                let measured = self.bottom_stack.clone();
                let measured_has_composer = self.bottom_stack_has_composer.clone();
                let contains_composer = (has_spaces || has_appshots) && has_selection;
                let composer = self.composer.clone();
                div()
                    .flex_none()
                    .relative()
                    .flex()
                    .flex_col()
                    .child(
                        gpui::canvas(
                            move |bounds, window, cx| {
                                // Reserve the destination footprint, never the animated height.
                                let next_height = f32::from(bounds.size.height)
                                    + composer.read(cx).dock_clearance_correction();
                                let changed = (measured.get() - next_height).abs() > 0.5
                                    || measured_has_composer.get() != contains_composer;
                                measured.set(next_height);
                                measured_has_composer.set(contains_composer);
                                if changed {
                                    window.request_animation_frame();
                                }
                            },
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .inset_0(),
                    )
                    .child(status)
                    .when(has_spaces || has_appshots, |el| {
                        let composer_opacity = self.composer_dock.borrow().opacity();
                        el.child(crate::composer_dock::docked_composer(
                            div()
                                .id("persistent-composer")
                                .relative()
                                .w(px(composer_width))
                                .opacity(composer_opacity)
                                .mx_auto()
                                .child(self.composer.clone())
                                .children(if has_selection {
                                    self.render_jump_to_bottom(cx)
                                } else {
                                    None
                                }),
                            self.composer_dock.clone(),
                            self.viewport_height,
                            self.reduced_motion,
                            frame_time,
                        ))
                    })
                    .child(self.render_terminal_container(window, cx))
            })
            .child(
                div()
                    .id("attachment-drop-overlay")
                    .absolute()
                    .inset_0()
                    .opacity(0.0)
                    .bg(theme.scrim().opacity(0.4 / 0.6))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(crate::typography::ui_rems(13.0))
                    .text_color(theme.text)
                    // GPUI matches these styles against the active payload's
                    // concrete TypeId. Resize markers therefore cannot reveal
                    // this overlay, even after an external drag exits without
                    // another move event.
                    .drag_over::<gpui::ExternalPaths>(|style, _, _, _| style.opacity(1.0))
                    .drag_over::<WorkspacePathDrag>(|style, _, _, _| style.opacity(1.0))
                    .drag_over::<RightTabDrag>(|style, tab, _, _| {
                        if tab.workspace_path.is_some() {
                            style.opacity(1.0)
                        } else {
                            style
                        }
                    })
                    .child("Drop to attach"),
            )
            .into_any_element()
    }

    /// The "↓ Scroll to bottom" pill (round-9 §3): a LABELED rounded-full
    /// chip — down-arrow glyph + 13px label on a near-opaque raised surface
    /// with a hairline — horizontally centered over the transcript column and
    /// floating six pixels above the composer. It shares the composer's
    /// measured dock transform and paints after it, outside the transcript fade.
    fn render_jump_to_bottom(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.transcript.read(cx).jump_button_shown() {
            return None;
        }
        Some(
            div()
                .absolute()
                // Share the composer's measured translation, not its final
                // bottom-stack target. Paint after the composer so it cannot
                // pass over this control during docking.
                .top(px(-36.0))
                .left_0()
                .right(px(10.0))
                .flex()
                .justify_center()
                .child(self.jump_pill("jump-to-bottom", "jump-pill", self.transcript.clone(), cx))
                .into_any_element(),
        )
    }

    /// The jump pill itself — shared between the conversation overlay and
    /// the subagent pane so both read as one control. `anim_key`/`hover_key`
    /// must be distinct per instance (they key global animation state).
    ///
    /// Use the shared popover tint and blur, with a separate hover wash so
    /// the floating control retains the same glass surface in either theme.
    fn jump_pill(
        &self,
        anim_key: &'static str,
        hover_key: &'static str,
        transcript: Entity<Transcript>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = Theme::of(cx).for_popup();
        let glass = theme.is_frost();
        let base = if glass {
            popover::surface_bg(&theme)
        } else {
            motion::hover_blend(hover_key, theme.surface_raised, theme.surface_raised_hover)
        };
        let wash = if glass {
            motion::hover_blend(hover_key, gpui::transparent_black(), theme.glass_hover())
        } else {
            gpui::transparent_black()
        };
        let pill = div()
            .id(anim_key)
            .h(px(30.0))
            .rounded_full()
            .border_1()
            .border_color(theme.border)
            .when(!glass, |el| el.shadow_md())
            .cursor_pointer()
            .bg(base)
            .on_hover(motion::hover_listener(hover_key))
            .on_click(cx.listener(move |_, _, _, cx| {
                transcript.update(cx, |transcript, cx| transcript.jump_to_bottom(cx));
            }))
            .child(
                // The hover wash rides an inner full-height layer so it
                // composites over the tint (a div has one bg).
                div()
                    .h_full()
                    .rounded_full()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .pl(px(11.0))
                    .pr(px(13.0))
                    .bg(wash)
                    .child(
                        div()
                            .text_size(crate::typography::ui_rems(13.0))
                            .text_color(theme.text_muted)
                            .child(SharedString::from("↓")),
                    )
                    .child(
                        div()
                            .text_size(crate::typography::ui_rems(13.0))
                            .text_color(theme.text)
                            .child(SharedString::from("Scroll to bottom")),
                    ),
            );
        // Frost OUTSIDE the entry animation (the composer pill's exact
        // composition): one scene layer — blur, then the pill's quads, then
        // glyphs — so the pill always composes over the transcript content
        // scrolling under it, and never loses its washes to the kind-sorted
        // draw order (frost.rs module docs).
        crate::frost::frosted(
            15.0,
            crate::frost::MENU_BLUR,
            motion::dialog_in(anim_key, pill),
        )
        .into_any_element()
    }

    /// Terminal panel dock at the main-column bottom: a 5px height-drag handle
    /// over the panel, the whole container height-animated 200 ms on toggle.
    fn render_terminal_container(&mut self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let target = self.terminal_target(cx);
        let tween = self.terminal_tween;
        if target <= 0.0 && tween.is_none() {
            return gpui::Empty.into_any_element();
        }
        // Defensive: an open flag needs its entity (and set_open) even if
        // toggle_terminal never created one.
        if self.terminal_open(cx) && self.terminal.is_none() {
            let panel = self.terminal_panel(cx);
            panel.update(cx, |panel, cx| panel.set_open(true, cx));
        }
        let Some(panel) = self.terminal.clone() else {
            return gpui::Empty.into_any_element();
        };
        // The dock spans the main column's bottom: when the sidebar is fully
        // closed the column IS the window's left edge (and likewise the right
        // edge when the right pane is closed) — the panel's fill then carries
        // the CSD window's bottom corners.
        let window_corner = Self::window_corner_radius(window) > 0.0;
        {
            let bl = window_corner && self.sidebar_now() < 0.5;
            let br = window_corner && !self.right_pane_open(cx);
            panel.update(cx, |panel, cx| panel.set_window_corners(bl, br, cx));
        }
        let border = Theme::of(cx).border;
        let handle_key = "pane-resize-terminal-resize";
        let handle_hover = motion::hover_blend(
            handle_key,
            Theme::of(cx).border_strong.opacity(0.0),
            Theme::of(cx).border_strong,
        );
        let terminal_active = self.pane_resize_active == Some(PaneResizeKind::Terminal);
        let terminal_constrained =
            self.pane_resize_dragging == Some(PaneResizeKind::Terminal) && !terminal_active;
        let handle_highlight = if terminal_constrained {
            Theme::of(cx).border_strong.opacity(0.0)
        } else if terminal_active {
            Theme::of(cx).border_strong
        } else {
            handle_hover
        };
        let height = self.settings.terminal_height;

        let handle = div()
            .id("terminal-resize")
            .h(px(TERMINAL_RESIZE_HITBOX_HEIGHT))
            .w_full()
            .flex_none()
            .cursor_row_resize()
            .on_hover(motion::hover_listener(handle_key))
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .h(px(1.0))
                    .bg(handle_highlight),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &gpui::MouseDownEvent, _, cx| {
                    this.terminal_drag_anchor =
                        Some((f32::from(event.position.y), this.settings.terminal_height));
                    this.pane_resize_dragging = Some(PaneResizeKind::Terminal);
                    this.pane_resize_active = Some(PaneResizeKind::Terminal);
                    cx.notify();
                }),
            )
            .on_drag(TerminalResize, |_, _point: Point<gpui::Pixels>, _, cx| {
                cx.stop_propagation();
                cx.new(|_| DragGhost)
            })
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, event: &MouseUpEvent, window, cx| {
                    if event.click_count == 2 {
                        this.settings.terminal_height = TERMINAL_DEFAULT_HEIGHT;
                        this.schedule_save(cx);
                        cx.notify();
                    }
                    this.finish_pane_resize(PaneResizeKind::Terminal);
                    motion::set_hover(handle_key, false, this.reduced_motion);
                    window.refresh();
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, window, _| {
                    this.finish_pane_resize(PaneResizeKind::Terminal);
                    motion::set_hover(handle_key, false, this.reduced_motion);
                    window.refresh();
                }),
            );

        // Fixed-height inner clipped by the animated container: content never
        // reflows mid-transition (same trick as the side panes). The handle
        // FLOATS over the panel's top edge (painted after, so it wins hit
        // testing) instead of stacking above it — stacked, its hitbox would read as
        // dead air between the seam and the tab bar (user report).
        let inner = div()
            .h(px(height))
            .w_full()
            .relative()
            .flex()
            .flex_col()
            .child(div().flex_1().min_h_0().child(panel))
            .child(handle.absolute().top_0().left_0().right_0());

        div()
            .w_full()
            .flex_none()
            .overflow_hidden()
            .border_t_1()
            .border_color(border)
            .h(px(self.eval_tween(tween, target)))
            .child(inner)
            .into_any_element()
    }

    /// Working indicator strip: gradient spinner + rotating flavour word (7s,
    /// seeded per chat) + elapsed, staleness-gated via [`Indicator`]; falls back
    /// to a "Sending…" bridge and then the engine mode line.
    fn render_status_strip(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let now = Utc::now();
        let state = self.state.read(cx);

        // Aligned with the composer column: centered, same max width, small
        // inner gutter (luna's `mx-auto h-6 max-w-3xl px-2`).
        let strip = div()
            .h(px(Theme::STATUS_STRIP_HEIGHT))
            .flex_none()
            .w_full()
            .max_w(px(768.0))
            .mx_auto()
            .flex()
            .items_center()
            .gap(px(Theme::SPACE_SM))
            .px(px(Theme::SPACE_LG + 8.0))
            .text_size(crate::typography::ui_rems(11.0));

        let Some(chat_id) = state.selected_chat.clone() else {
            return strip.into_any_element();
        };
        let indicator = state.indicator_for(&chat_id, now);
        // Timer base: the freshest of the session row's turn start and the
        // in-flight send. During the send→ack window the row (if any) still
        // carries the PREVIOUS turn's start, and using it opened the timer at
        // the old turn's elapsed instead of 0:00.
        let started = state
            .session_for(&chat_id)
            .and_then(|s| s.started_at)
            .into_iter()
            .chain(state.pending_send_started(&chat_id, now))
            .max();
        let elapsed_secs = started
            .map(|t| now.signed_duration_since(t).num_seconds().max(0))
            .unwrap_or(0);
        let sending = self.composer.read(cx).is_sending();

        // Unused here since the Working loader moved into the transcript
        // (its trailer computes its own elapsed).
        let _ = elapsed_secs;
        match indicator {
            // The working loader lives in the TRANSCRIPT now, under the
            // streaming reply (user request) — the strip stays empty (its
            // reserved height still steadies the composer).
            Indicator::Working => strip.into_any_element(),
            Indicator::AwaitingApproval => strip.into_any_element(),
            // No label: the QuestionPanel right below IS the awaiting-input
            // surface — a strip caption above it was redundant (user request).
            Indicator::AwaitingInput => strip.into_any_element(),
            Indicator::Errored => strip
                .text_color(theme.danger)
                .child(SharedString::from("Run failed"))
                .into_any_element(),
            Indicator::None if sending => strip
                .child(loaders::gradient_spinner(
                    "sending-indicator",
                    &theme,
                    2.5,
                    cx.entity_id(),
                    cx,
                ))
                .child(
                    div()
                        .text_size(crate::typography::ui_rems(12.0))
                        .text_color(theme.text_muted)
                        .child(SharedString::from("Sending…")),
                )
                .into_any_element(),
            Indicator::None => strip.into_any_element(),
        }
    }

    /// Right pane — the surface host (t3code RightPanelTabs): hidden by
    /// default, drag-resizable. Content is the ACTIVE surface — the Diff
    /// page (its options row + the lazy [`Changes`] viewer), workspace Files,
    /// an embedded terminal, or the surface picker when no tabs exist.
    fn render_right_pane(&mut self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let bg = theme.bg;
        let content: AnyElement = if self.right_pane_open(cx) || self.tween_active(self.right_tween)
        {
            match self.resolved_right_active(cx) {
                RightSurface::Agents if self.right_pane_open(cx) => self.render_agents(cx),
                RightSurface::SideChats if self.right_pane_open(cx) => self.render_side_chats(cx),
                // Rendering a Files surface activates its image. Keep it unmounted
                // throughout the closing animation after suspending its resources.
                RightSurface::Files | RightSurface::File(_) if !self.right_pane_open(cx) => {
                    gpui::Empty.into_any_element()
                }
                RightSurface::Files => {
                    let key = self.panel_key(cx);
                    if let Some(files) = self.files.get(&key).cloned() {
                        files.update(cx, |files, cx| files.ensure_loaded(cx));
                        files.into_any_element()
                    } else {
                        self.render_surface_picker(cx)
                    }
                }
                RightSurface::File(id) => {
                    if let Some(file) = self.file_surfaces.get(&id).cloned() {
                        file.update(cx, |file, cx| file.ensure_loaded(cx));
                        file.into_any_element()
                    } else {
                        self.render_surface_picker(cx)
                    }
                }
                RightSurface::Diff(id) if self.diffs.contains_key(&id) => {
                    let changes = self.diffs.get(&id).cloned().expect("checked");
                    // Idempotent — also covers a persisted-open pane on boot.
                    changes.update(cx, |changes, cx| changes.ensure_content(cx));
                    // The diff options (scope dropdown, ref selector,
                    // fold-all) moved DOWN from the titlebar band — the
                    // surface tabs own that row now; the expand/close
                    // buttons stayed up there (user request).
                    let controls =
                        changes.update(cx, |changes, cx| changes.render_header_controls(cx));
                    div()
                        .size_full()
                        .flex()
                        .flex_col()
                        .child(crate::surface_chrome::toolbar(&theme).child(controls))
                        .child(div().flex_1().min_h_0().child(changes))
                        .into_any_element()
                }
                RightSurface::Browser(id) => self
                    .browsers
                    .get(&id)
                    .cloned()
                    .map(|browser| browser.into_any_element())
                    .unwrap_or_else(|| self.render_surface_picker(cx)),
                RightSurface::Terminal(tab) => {
                    let panel = self.right_terminal_panel(cx);
                    // Keep the embedded panel's own active tab aligned with
                    // the resolved surface (fallbacks can move it).
                    let resize_suspended = self.tween_active(self.right_tween);
                    panel.update(cx, |panel, cx| {
                        panel.set_resize_suspended(resize_suspended);
                        panel.select_tab_by_key(tab, cx);
                    });
                    panel.into_any_element()
                }
                RightSurface::Subagent(id)
                    if self.right_pane_open(cx) && self.subagent_tabs.contains_key(&id) =>
                {
                    self.resume_subagent_watch(id, cx);
                    let transcript = self
                        .subagent_tabs
                        .get(&id)
                        .expect("checked")
                        .transcript
                        .clone();
                    // The pane hosts its own jump pill: the conversation
                    // overlay's is bound to the PRIMARY transcript, and this
                    // one anchors to the pane (no composer stack to clear).
                    let pill = transcript.read(cx).jump_button_shown().then(|| {
                        div()
                            .absolute()
                            .bottom(px(16.0))
                            .left_0()
                            .right_0()
                            .flex()
                            .justify_center()
                            .child(self.jump_pill(
                                "subagent-jump-to-bottom",
                                "subagent-jump-pill",
                                transcript.clone(),
                                cx,
                            ))
                    });
                    // Read-only surface: the transcript fills the pane — no
                    // composer, no status strip.
                    div()
                        .size_full()
                        .relative()
                        .flex()
                        .flex_col()
                        .child(div().flex_1().min_h_0().child(transcript))
                        .children(pill)
                        .into_any_element()
                }
                _ => self.render_surface_picker(cx),
            }
        } else {
            gpui::Empty.into_any_element()
        };
        // Flush panel (user request — the inset card is gone): full window
        // height with a left hairline, glass-friendly like the terminal dock
        // (translucent over the frost; solid otherwise). The resize grabber
        // lives outside this clipped container, on the root layout's seam.
        let panel_bg = if theme.is_glass() {
            bg.opacity(0.4)
        } else {
            bg
        };
        let panel = div()
            .size_full()
            .flex()
            .flex_col()
            // In takeover the panel's left edge IS the sidebar seam, which
            // already carries the sidebar tone's right hairline — a second
            // border there doubled up (user report).
            .when(!self.right_pane_expanded, |el| {
                el.border_l_1().border_color(theme.border)
            })
            // The panel's right edge IS the window's right edge: it carries
            // the CSD window's rounded corners directly (gpui cannot clip
            // children rounded — each full-bleed layer rounds itself; see
            // [`Self::window_corner_radius`]).
            .when(Self::window_corner_radius(window) > 0.0, |el| {
                let corner = Self::window_corner_radius(window);
                el.rounded_tr(px(corner)).rounded_br(px(corner))
            })
            .bg(panel_bg)
            .overflow_hidden()
            // The titlebar is a glass overlay over the full-height content
            // row; the panel's own chrome starts below it.
            .pt(px(Theme::TITLEBAR_HEIGHT))
            .child(content);
        let target = self.right_target(cx);
        let edge_offset = self.eval_resize_edge_bounce(
            self.right_edge_bounce,
            self.right_pane_open(cx) && !self.right_pane_expanded,
        );
        self.right_pane_container(
            self.right_tween,
            target,
            edge_offset,
            div().h_full().relative().child(panel).into_any_element(),
        )
    }

    /// The right pane's empty state: a compact vertical list of surface rows
    /// (icon + label). The old two-card grid clipped in narrow panes and
    /// wasted short ones.
    fn render_surface_picker(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let terminal_notice = self
            .terminal_picker_notice
            .as_ref()
            .filter(|(chat, _)| chat == &self.active_chat)
            .map(|(_, notice)| notice.clone());
        let text = theme.text;
        let muted = theme.text_muted;
        let border = theme.border;
        let border_strong = theme.border_strong;
        let row = |id: &'static str, icon_path: &'static str, title: &'static str| {
            div()
                .id(id)
                .role(gpui::Role::Button)
                .aria_label(title)
                .tab_index(0)
                .focus_visible(move |style| style.border_color(border_strong))
                .w_full()
                .h(px(44.0))
                .px(px(14.0))
                .rounded(px(10.0))
                .border_1()
                .border_color(border)
                .bg(crate::theme::ink(0.02))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(10.0))
                .cursor_pointer()
                .hover(move |s| s.bg(crate::theme::ink(0.05)).border_color(border_strong))
                .child(icon(icon_path).size(px(15.0)).flex_none().text_color(muted))
                .child(
                    div()
                        .text_size(crate::typography::ui_rems(13.0))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(text)
                        .child(SharedString::from(title)),
                )
        };
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .p(px(16.0))
            .child(
                div()
                    .w_full()
                    .max_w(px(280.0))
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .when(!self.active_chat.is_empty(), |el| {
                        el.child(
                            row("surface-card-agents", icons::BOT, "Agents")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.add_work_surface(RightSurface::Agents, cx)
                                }))
                                .on_key_down(cx.listener(
                                    |this, event: &gpui::KeyDownEvent, _, cx| {
                                        if is_settings_nav_activation(
                                            &event.keystroke.key,
                                            event.is_held,
                                        ) {
                                            this.add_work_surface(RightSurface::Agents, cx);
                                            cx.stop_propagation();
                                        }
                                    },
                                )),
                        )
                        .child(
                            row("surface-card-side-chats", icons::PLUS, "Side chats")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.add_work_surface(RightSurface::SideChats, cx)
                                }))
                                .on_key_down(cx.listener(
                                    |this, event: &gpui::KeyDownEvent, _, cx| {
                                        if is_settings_nav_activation(
                                            &event.keystroke.key,
                                            event.is_held,
                                        ) {
                                            this.add_work_surface(RightSurface::SideChats, cx);
                                            cx.stop_propagation();
                                        }
                                    },
                                )),
                        )
                    })
                    .child(
                        row("surface-card-files", icons::FOLDER_WITH_FILES, "Files").on_click(
                            cx.listener(|this, _, window, cx| {
                                this.add_files_surface(window, cx);
                            }),
                        ),
                    )
                    .child(
                        row("surface-card-browser", icons::GLOBE, "Browser").on_click(cx.listener(
                            |this, _, window, cx| this.add_browser_surface(None, window, cx),
                        )),
                    )
                    .child(
                        row("surface-card-terminal", icons::TERMINAL, "Terminal").on_click(
                            cx.listener(|this, _, _, cx| {
                                this.add_terminal_surface(cx);
                            }),
                        ),
                    )
                    .when_some(terminal_notice, |el, notice| {
                        el.child(
                            div()
                                .px(px(14.0))
                                .text_size(crate::typography::ui_rems(11.0))
                                .text_color(muted)
                                .child(notice),
                        )
                    })
                    // Git surfaces only where there IS git — the pane itself
                    // no longer gates on it (terminals work anywhere).
                    .when(self.space_git_detected(cx), |el| {
                        el.child(row("surface-card-diffs", icons::LIST, "Diffs").on_click(
                            cx.listener(|this, _, _, cx| {
                                this.add_diff_surface(cx);
                            }),
                        ))
                        .child(
                            row("surface-card-history", icons::GIT_BRANCH, "History").on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.add_history_surface(cx);
                                }),
                            ),
                        )
                    }),
            )
            .into_any_element()
    }

    fn render_signed_out_restart(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let runtime_change_label = if self.runtime_change_task.is_some() {
            "Stopping engine…"
        } else {
            "Retry local mode"
        };
        let card = div()
            .w(px(380.0))
            .px(px(32.0))
            .py(px(40.0))
            .rounded(px(12.0))
            .border_1()
            .border_color(theme.border)
            .bg(theme.surface_card)
            .shadow_lg()
            .flex()
            .flex_col()
            .items_center()
            .text_center()
            .child(
                icon(icons::LUNA_LOGO)
                    .w(px(31.4))
                    .h(px(36.0))
                    .text_color(theme.text),
            )
            .child(
                div()
                    .mt(px(24.0))
                    .text_size(crate::typography::ui_rems(18.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme.text)
                    .child(SharedString::from("Signed out")),
            )
            .child(
                div()
                    .mt(px(6.0))
                    .mb(px(24.0))
                    .text_size(crate::typography::ui_rems(13.0))
                    .line_height(crate::typography::ui_rems(19.0))
                    .text_color(theme.text_muted)
                    .child(SharedString::from(
                        "LUNA removed your credentials but could not finish closing the previous synced workspace. Retry before continuing in local mode.",
                    )),
            )
            .when_some(self.runtime_change_error.clone(), |card, error| {
                card.child(
                    div()
                        .mb(px(16.0))
                        .text_size(crate::typography::ui_rems(12.0))
                        .line_height(crate::typography::ui_rems(17.0))
                        .text_color(theme.danger)
                        .child(error),
                )
            })
            .child(
                popover::btn_primary(&theme, runtime_change_label)
                    .id("signed-out-quit")
                    .when(self.runtime_change_task.is_some(), |button| {
                        button.opacity(0.6)
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.start_local_runtime_transition(false, cx)
                    })),
            );

        div()
            .absolute()
            .inset_0()
            .occlude()
            .bg(theme.bg)
            .child(grid_backdrop(&theme))
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(motion::fade_in("signed-out-restart", card)),
            )
            .into_any_element()
    }

    fn close_right_plus(&mut self, cx: &mut Context<Self>) {
        if self.right_plus.begin_close() {
            popover::reap_popup(cx, |shell: &mut Self| &mut shell.right_plus);
        }
        cx.notify();
    }

    /// The titlebar strip over the right pane: one chip per surface tab
    /// (icon · title · ✕) plus the `+` menu — the t3code RightPanelTabs bar,
    /// living in the top row; the diff options moved into the pane below.
    pub(crate) fn render_right_tab_strip(&mut self, cx: &mut Context<Self>) -> AnyElement {
        /// Fixed chip slot — the terminal drawer's drag mechanics (drop-index
        /// quantisation + slide offsets) assume uniform widths.
        const CHIP_W: f32 = 112.0;
        const CHIP_SLOT: f32 = CHIP_W + 4.0; // + the strip's own gap

        let theme = Theme::of(cx).clone();
        // Heal drag state if the pointer was released outside the strip.
        if self.right_tab_drag.is_some() && !cx.has_active_drag() {
            self.right_tab_drag = None;
        }
        let rows = self.right_surface_rows(cx);
        let count = rows.len();
        let active = self.resolved_right_active(cx);
        let drag = self
            .right_tab_drag
            .as_ref()
            .map(|d| (d.from, d.over, d.epoch, d.prev_over));

        // Fade flags from the LAST frame's scroll state (invisible lag).
        // The EdgeFade scope below fades per-pixel on x for glyphs AND
        // quads/images (fork 5d1f83d) — washes dissolve across the band.
        const FADE_WIDTH: f32 = 36.0;
        let scrolled = -f32::from(self.right_tab_scroll.offset().x);
        let max_scroll = f32::from(self.right_tab_scroll.max_offset().x);
        let fade_left = scrolled > 1.0;
        let fade_right = scrolled < max_scroll - 1.0;
        // The old session-tab strip's proven scroll shape: the flex row IS
        // the scroller (id + overflow_x_scroll + track_scroll), wrapped in a
        // relative min_w_0 region below; drop math runs in CONTENT
        // coordinates (viewport-relative x plus the scrolled-off width).
        let scroll_for_drag = self.right_tab_scroll.clone();
        let mut strip = div()
            .id("right-surface-strip")
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.0))
            .min_w_0()
            .overflow_x_scroll()
            .track_scroll(&self.right_tab_scroll)
            // Windows caption hit-testing includes the scroll-only hitboxes
            // behind each chip. Stop at the scroller so the titlebar cannot
            // claim tab clicks, while wheel events still reach this scroller.
            .when(cfg!(target_os = "windows"), |strip| strip.occlude())
            .on_drag_move::<RightTabDrag>(cx.listener(
                move |this, event: &gpui::DragMoveEvent<RightTabDrag>, _, cx| {
                    let payload = event.drag(cx);
                    if payload.panel_key != this.panel_key(cx) {
                        return;
                    }
                    let from = payload.from;
                    let rel_x = f32::from(event.event.position.x)
                        - f32::from(event.bounds.left())
                        - f32::from(scroll_for_drag.offset().x);
                    let over = crate::terminal::panel::drop_index(rel_x, CHIP_SLOT, count);
                    this.update_right_tab_drag_over(from, over, cx);
                },
            ))
            .on_drop::<RightTabDrag>(cx.listener(move |this, payload: &RightTabDrag, _, cx| {
                if payload.panel_key != this.panel_key(cx) {
                    this.right_tab_drag = None;
                    cx.notify();
                    return;
                }
                let to = this
                    .right_tab_drag
                    .as_ref()
                    .map(|d| d.over)
                    .unwrap_or(payload.from);
                this.right_tab_drag = None;
                this.reorder_right_tabs(payload.from, to, cx);
            }));
        for (ix, (surface, title, dirty, detail)) in rows.into_iter().enumerate() {
            let is_active = surface == active;
            let file_identity_path = detail.as_ref().cloned().unwrap_or_else(|| title.clone());
            let icon_path = match surface {
                RightSurface::Files => icons::FOLDER_WITH_FILES,
                RightSurface::File(_) => icons::DOCUMENT,
                RightSurface::Diff(id) => self
                    .diffs
                    .get(&id)
                    .map(|changes| {
                        if changes.read(cx).is_history() {
                            icons::GIT_BRANCH
                        } else {
                            icons::LIST
                        }
                    })
                    .unwrap_or(icons::LIST),
                RightSurface::Subagent(_) | RightSurface::Agents => icons::BOT,
                RightSurface::SideChats => icons::PLUS,
                RightSurface::Terminal(_) => icons::TERMINAL,
                RightSurface::Browser(_) => icons::GLOBE,
                RightSurface::Picker => icons::PLUS,
            };
            // A live subagent tab swaps its icon for the mini working
            // spinner (the history fetch button's in-flight recipe) — the
            // doc's streaming tail entry IS the run's liveness, so the swap
            // settles by itself when the subagent finishes.
            let browser_favicon = match surface {
                RightSurface::Browser(id) => self
                    .browsers
                    .get(&id)
                    .and_then(|b| b.read(cx).favicon.clone()),
                _ => None,
            };
            let subagent_running = match surface {
                RightSurface::Browser(id) => self
                    .browsers
                    .get(&id)
                    .is_some_and(|b| b.read(cx).page.loading),
                RightSurface::Subagent(id) => self.subagent_tabs.get(&id).is_some_and(|tab| {
                    self.state
                        .read(cx)
                        .sub_transcript(&tab.doc_id)
                        .last()
                        .is_some_and(|e| e.status == Some(luna_doc::MessageStatus::Streaming))
                }),
                _ => false,
            };
            // t3 tab hover: the surface icon swaps IN PLACE for the close ✕
            // (same slot, no width jump) — the ✕ only shows while the tab is
            // hovered (user request).
            let group: SharedString = format!("right-surface-tab-{ix}").into();
            let ghost_title = title.clone();
            let workspace_path = self.workspace_path_for_surface(surface, cx);
            let accessible_name = detail.as_ref().unwrap_or(&title);
            let accessible_label = if dirty {
                format!("{accessible_name}, unsaved changes")
            } else {
                accessible_name.to_string()
            };
            let chip = div()
                .id(("right-surface-tab", ix))
                .debug_selector(|| format!("right-surface-tab-{ix}"))
                .group(group.clone())
                .h(px(24.0))
                .w(px(CHIP_W))
                .flex_none()
                .pl(px(4.0))
                .pr(px(8.0))
                .rounded(px(6.0))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(3.0))
                .cursor_pointer()
                .role(gpui::Role::Button)
                .aria_label(accessible_label)
                .when_some(detail, |chip, detail| {
                    chip.tooltip(move |_, cx| {
                        cx.new(|_| SurfaceTabTooltip {
                            text: detail.clone(),
                        })
                        .into()
                    })
                    .tooltip_show_delay(Duration::from_millis(350))
                })
                // The old session-tab strip's solved carve-out: NOT
                // `.occlude()` — a BlockMouse hitbox ends the hit test,
                // so the scroll container behind the tabs never saw
                // wheel events and an overflowing strip could not be
                // scrolled (tabs tile the whole region). ExceptScroll
                // keeps the titlebar drag-region carve-out and lets the
                // strip scroll.
                .block_mouse_except_scroll()
                .on_mouse_down(gpui::MouseButton::Left, |_, window, _| {
                    window.prevent_default()
                })
                .when(is_active, |el| el.bg(crate::theme::wash(0.10)))
                .when(!is_active, |el| {
                    el.hover(|s| s.bg(crate::theme::wash(0.06)))
                })
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.set_right_active(surface, cx);
                    this.focus_right_file_editor(surface, window, cx);
                }))
                // Middle-click closes, like every tab strip.
                .on_mouse_down(
                    gpui::MouseButton::Middle,
                    cx.listener(move |this, _, window, cx| {
                        this.close_right_surface(surface, window, cx);
                    }),
                )
                .when(crate::click_activation_drag_enabled(), |el| {
                    el.on_drag(
                        RightTabDrag {
                            panel_key: self.panel_key(cx),
                            from: ix,
                            title: ghost_title,
                            workspace_path,
                        },
                        |payload, _point, _, cx| {
                            let title = payload.title.clone();
                            cx.stop_propagation();
                            cx.new(|_| SurfaceTabGhost { title })
                        },
                    )
                })
                .child(
                    // Leading slot: icon normally, ✕ on tab hover — two
                    // stacked layers opacity-swapped by the group hover.
                    div()
                        .id(("right-surface-close", ix))
                        .debug_selector(|| format!("right-surface-close-{ix}"))
                        .flex_none()
                        .size(px(18.0))
                        .rounded(px(4.0))
                        .relative()
                        .hover(|s| s.bg(crate::theme::wash(0.12)))
                        // The tab owns a drag payload. Claim the close press
                        // before it reaches that parent or GPUI starts a tab
                        // drag instead of delivering the close click.
                        .on_mouse_down(gpui::MouseButton::Left, |_, window, cx| {
                            window.prevent_default();
                            cx.stop_propagation();
                        })
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.close_right_surface(surface, window, cx);
                        }))
                        .child(
                            div()
                                .absolute()
                                .inset_0()
                                .flex()
                                .items_center()
                                .justify_center()
                                .group_hover(group.clone(), |s| s.opacity(0.0))
                                .child(if subagent_running {
                                    loaders::mini_glyph_spinner(
                                        format!("subagent-tab-{ix}"),
                                        2.0,
                                        theme.glyph,
                                        cx.entity_id(),
                                        cx,
                                    )
                                    .into_any_element()
                                } else if let Some(favicon) = browser_favicon {
                                    gpui::img(favicon).size(px(12.0)).into_any_element()
                                } else if matches!(surface, RightSurface::File(_)) {
                                    crate::file_icons::icon(
                                        crate::file_icons::FileIconIdentity::file(
                                            file_identity_path.as_ref(),
                                        ),
                                        theme.appearance,
                                    )
                                    .size(px(14.0))
                                    .when(!is_active, |icon| icon.opacity(0.78))
                                    .into_any_element()
                                } else {
                                    icon(icon_path)
                                        .size(px(12.0))
                                        .text_color(if is_active {
                                            theme.text_muted
                                        } else {
                                            theme.readable_muted(0.7)
                                        })
                                        .into_any_element()
                                }),
                        )
                        .child(
                            div()
                                .absolute()
                                .inset_0()
                                .flex()
                                .items_center()
                                .justify_center()
                                .opacity(0.0)
                                .group_hover(group.clone(), |s| s.opacity(1.0))
                                .child(
                                    icon(icons::CLOSE)
                                        .size(px(12.0))
                                        .text_color(theme.text_muted),
                                ),
                        ),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(crate::typography::ui_rems(11.5))
                        .text_color(if is_active {
                            theme.text
                        } else {
                            theme.text_muted
                        })
                        .child(title),
                )
                .when(dirty, |chip| {
                    chip.child(
                        div()
                            .flex_none()
                            .size(px(6.0))
                            .rounded_full()
                            .bg(theme.text_muted),
                    )
                });
            // Sliding transform while a sibling drags over (the terminal
            // drawer's exact recipe): animate 150ms between committed
            // offsets; the dragged tab leaves an invisible spacer — the
            // ghost carries it.
            let wrapped: AnyElement = match drag {
                Some((from, over, epoch, prev_over)) if ix != from => {
                    let target = crate::terminal::panel::slide_offset(ix, from, over) * CHIP_SLOT;
                    let start =
                        crate::terminal::panel::slide_offset(ix, from, prev_over) * CHIP_SLOT;
                    div()
                        .relative()
                        .child(chip.with_animation(
                            ("right-tab-slide", (ix as u64) | ((epoch as u64) << 32)),
                            TAB_SLIDE.animation(),
                            move |el, t| el.left(px(motion::lerp(start, target, t))),
                        ))
                        .into_any_element()
                }
                Some((from, ..)) if ix == from => div()
                    .w(px(CHIP_W))
                    .h(px(24.0))
                    .flex_none()
                    .into_any_element(),
                _ => chip.into_any_element(),
            };
            strip = strip.child(wrapped);
        }
        // The `+` — a small menu offering the available surfaces (t3 "Add panel
        // surface"); mirrors the picker cards.
        let plus_open = self.right_plus.get().is_some();
        let plus_fade = "right-surface-add-fade";
        let mut plus = div()
            .id("right-surface-add")
            .size(px(24.0))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(6.0))
            .cursor_pointer()
            .bg(motion::hover_blend(
                plus_fade,
                crate::theme::wash(0.0),
                crate::theme::wash(0.11),
            ))
            .on_hover(motion::hover_listener(plus_fade))
            .block_mouse_except_scroll()
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _, window, _| {
                    window.prevent_default();
                    this.right_plus.note_trigger_press();
                }),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                cx.stop_propagation();
                if this.right_plus.take_press_was_open() {
                    this.close_right_plus(cx);
                } else {
                    this.right_plus.open(());
                    cx.notify();
                }
            }))
            .child(
                icon(icons::PLUS)
                    .size(px(13.0))
                    .text_color(theme.text_muted),
            );
        if plus_open {
            let closing = self.right_plus.closing_since();
            let menu = popover::popover_card(&theme)
                .w(px(168.0))
                .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_right_plus(cx)))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .child(
                            popover::menu_row(&theme, false, "right-plus-files")
                                .id("right-plus-files-row")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.add_files_surface(window, cx);
                                    this.close_right_plus(cx);
                                }))
                                .child(
                                    icon(icons::FOLDER_WITH_FILES)
                                        .size(px(13.0))
                                        .text_color(theme.text_muted),
                                )
                                .child(SharedString::from("Files")),
                        )
                        .child(
                            popover::menu_row(&theme, false, "right-plus-browser")
                                .id("right-plus-browser-row")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.add_browser_surface(None, window, cx);
                                    this.close_right_plus(cx);
                                }))
                                .child(
                                    icon(icons::GLOBE)
                                        .size(px(13.0))
                                        .text_color(theme.text_muted),
                                )
                                .child(SharedString::from("Browser")),
                        )
                        .when(!self.active_chat.is_empty(), |menu| {
                            menu.child(
                                popover::menu_row(&theme, false, "right-plus-agents")
                                    .id("right-plus-agents-row")
                                    .role(gpui::Role::Button)
                                    .aria_label("Agents")
                                    .tab_index(0)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.add_work_surface(RightSurface::Agents, cx);
                                        this.close_right_plus(cx);
                                    }))
                                    .on_key_down(cx.listener(
                                        |this, event: &gpui::KeyDownEvent, _, cx| {
                                            if is_settings_nav_activation(
                                                &event.keystroke.key,
                                                event.is_held,
                                            ) {
                                                this.add_work_surface(RightSurface::Agents, cx);
                                                this.close_right_plus(cx);
                                                cx.stop_propagation();
                                            }
                                        },
                                    ))
                                    .child(
                                        icon(icons::BOT)
                                            .size(px(13.0))
                                            .text_color(theme.text_muted),
                                    )
                                    .child("Agents"),
                            )
                            .child(
                                popover::menu_row(&theme, false, "right-plus-side-chats")
                                    .id("right-plus-side-chats-row")
                                    .role(gpui::Role::Button)
                                    .aria_label("Side chats")
                                    .tab_index(0)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.add_work_surface(RightSurface::SideChats, cx);
                                        this.close_right_plus(cx);
                                    }))
                                    .on_key_down(cx.listener(
                                        |this, event: &gpui::KeyDownEvent, _, cx| {
                                            if is_settings_nav_activation(
                                                &event.keystroke.key,
                                                event.is_held,
                                            ) {
                                                this.add_work_surface(RightSurface::SideChats, cx);
                                                this.close_right_plus(cx);
                                                cx.stop_propagation();
                                            }
                                        },
                                    ))
                                    .child(
                                        icon(icons::PLUS)
                                            .size(px(13.0))
                                            .text_color(theme.text_muted),
                                    )
                                    .child("Side chats"),
                            )
                        })
                        .child(
                            popover::menu_row(&theme, false, "right-plus-terminal")
                                .id("right-plus-terminal-row")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.add_new_terminal_surface(cx);
                                    this.close_right_plus(cx);
                                }))
                                .child(
                                    icon(icons::TERMINAL)
                                        .size(px(13.0))
                                        .text_color(theme.text_muted),
                                )
                                .child(SharedString::from("Terminal")),
                        )
                        .when(self.space_git_detected(cx), |menu| {
                            menu.child(
                                popover::menu_row(&theme, false, "right-plus-diff")
                                    .id("right-plus-diff-row")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.add_diff_surface(cx);
                                        this.close_right_plus(cx);
                                    }))
                                    .child(
                                        icon(icons::LIST)
                                            .size(px(13.0))
                                            .text_color(theme.text_muted),
                                    )
                                    .child(SharedString::from("Diffs")),
                            )
                            .child(
                                popover::menu_row(&theme, false, "right-plus-history")
                                    .id("right-plus-history-row")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.add_history_surface(cx);
                                        this.close_right_plus(cx);
                                    }))
                                    .child(
                                        icon(icons::GIT_BRANCH)
                                            .size(px(13.0))
                                            .text_color(theme.text_muted),
                                    )
                                    .child(SharedString::from("History")),
                            )
                        }),
                )
                .into_any_element();
            plus = plus.relative().child(popover::anchored_menu_below_gap(
                "right-plus-menu",
                menu,
                closing,
                10.0,
            ));
        }
        // The empty-state picker already offers every surface. Show a single
        // Chrome-style add-tab affordance only after at least one tab exists.
        strip = strip.when(count > 0, |strip| strip.child(plus));
        // Edge fades on whichever side hides tabs (flags computed above).
        // Glass: per-glyph EdgeFade scope over the chips' own opacity ramps;
        // opaque: painted gradients in the shell surface tone.
        let glass = theme.is_glass();
        let bar_bg = theme.surface;
        let region = div()
            .relative()
            .min_w_0()
            .size_full()
            .flex()
            .items_center()
            .child(strip)
            .when(fade_left && !glass, |el| {
                el.child(
                    div()
                        .absolute()
                        .left_0()
                        .top_0()
                        .bottom_0()
                        .w(px(FADE_WIDTH))
                        .bg(gpui::linear_gradient(
                            90.0,
                            gpui::linear_color_stop(bar_bg, 0.0),
                            gpui::linear_color_stop(bar_bg.opacity(0.0), 1.0),
                        )),
                )
            })
            .when(fade_right && !glass, |el| {
                el.child(
                    div()
                        .absolute()
                        .right_0()
                        .top_0()
                        .bottom_0()
                        .w(px(FADE_WIDTH))
                        .bg(gpui::linear_gradient(
                            270.0,
                            gpui::linear_color_stop(bar_bg, 0.0),
                            gpui::linear_color_stop(bar_bg.opacity(0.0), 1.0),
                        )),
                )
            });
        if glass {
            crate::edge_fade::edge_faded(FADE_WIDTH, false, false, region)
                .fade_left(fade_left)
                .fade_right(fade_right)
                .into_any_element()
        } else {
            region.into_any_element()
        }
    }

    /// Toggle the changes-panel takeover (the header's expand button, t3code
    /// parity): the panel grows to fill everything right of the sidebar,
    /// hiding the conversation column; toggling back restores the saved
    /// width. Rides the same width tween as open/close so the jump glides.
    fn toggle_right_pane_expand(&mut self, cx: &mut Context<Self>) {
        let from = self.right_target(cx);
        self.right_edge_bounce = None;
        self.right_resize_edge = None;
        self.finish_pane_resize(PaneResizeKind::Right);
        let sidebar_now = self.sidebar_now();
        let from_main = conversation_width(self.viewport_width, sidebar_now, from);
        self.right_pane_expanded = !self.right_pane_expanded;
        let to = self.right_target(cx);
        let right_transition = WidthTween::new(from, to);
        self.right_tween = Some(right_transition);
        self.right_takeover_content_tween = Some(right_transition);
        self.main_takeover_tween = Some(WidthTween::new(
            from_main,
            conversation_width(self.viewport_width, sidebar_now, to),
        ));
        cx.notify();
    }

    fn render_gate_card(&mut self, phase: &GatePhase, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let content: AnyElement = match phase {
            // Backend unreachable: quiet centered copy (luna Gate `Failed`),
            // plus a Retry affordance (the native engine doesn't self-redial).
            GatePhase::Failed(error) => div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(Theme::SPACE_MD))
                .child(
                    div()
                        .text_size(crate::typography::ui_rems(14.0))
                        .text_color(theme.text_muted)
                        .child(SharedString::from(error.clone())),
                )
                .child(
                    div()
                        .id("retry-engine")
                        .px(px(12.0))
                        .py(px(6.0))
                        .rounded(px(8.0))
                        .border_1()
                        .border_color(theme.border)
                        .text_size(crate::typography::ui_rems(13.0))
                        .text_color(theme.text)
                        .cursor_pointer()
                        .hover(|s| s.bg(theme.glass_hover()))
                        .on_click(cx.listener(|this, _, _, cx| this.retry_engine(cx)))
                        .child(SharedString::from("Retry")),
                )
                .into_any_element(),
            // Login card (luna App.tsx Gate): centered card on the grid —
            // logo, "Log in to LUNA", copy, full-width white Log in button.
            _ => div()
                .w(px(360.0))
                .px(px(32.0))
                .py(px(40.0))
                .rounded(px(12.0))
                .border_1()
                .border_color(theme.border)
                .bg(theme.surface_card)
                .shadow_lg()
                .flex()
                .flex_col()
                .items_center()
                .text_center()
                .child(
                    icon(icons::LUNA_LOGO)
                        .w(px(31.4))
                        .h(px(36.0))
                        .text_color(theme.text),
                )
                .child(
                    div()
                        .mt(px(24.0))
                        .text_size(crate::typography::ui_rems(18.0))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme.text)
                        .child(SharedString::from("Log in to LUNA")),
                )
                .child(
                    div()
                        .mt(px(6.0))
                        .mb(px(24.0))
                        .text_size(crate::typography::ui_rems(13.0))
                        .line_height(crate::typography::ui_rems(19.0))
                        .text_color(theme.text_muted)
                        .child(SharedString::from(
                            "This opens your browser to finish logging in — you'll come right back.",
                        )),
                )
                .child(
                    div()
                        .id("sign-in")
                        .w_full()
                        .h(px(36.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(6.0))
                        .bg(theme.text)
                        .text_size(crate::typography::ui_rems(14.0))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(theme.on_solid)
                        .cursor_pointer()
                        .hover(|s| s.opacity(0.9))
                        .on_click(cx.listener(|this, _, _, cx| this.start_sign_in(cx)))
                        .child(SharedString::from("Log in")),
                )
                .into_any_element(),
        };
        div()
            .size_full()
            .relative()
            .bg(theme.bg)
            .child(grid_backdrop(&theme))
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    // Keyed per phase (luna App.tsx `<div key={phase}
                    // className="animate-in">`): every gate swap replays the
                    // 0.5s entrance instead of mutating one animated element.
                    .child(motion::fade_in(
                        match phase {
                            GatePhase::SignIn => "gate-card-signin",
                            _ => "gate-card-failed",
                        },
                        div().child(content),
                    )),
            )
            .into_any_element()
    }

    /// Organization onboarding used by the synced gate and, for a local
    /// runtime, only after the user explicitly starts the sync opt-in.
    fn render_org_gate(&mut self, cx: &mut Context<Self>) -> AnyElement {
        self.ensure_org_ui(cx);
        let theme = Theme::of(cx).clone();
        let local_setup = self.state.read(cx).workspace_scope == Some(WorkspaceScope::Local);
        let Some(org) = self.org.as_ref() else {
            return Empty.into_any_element();
        };
        let submitting = org.submitting;
        let error = org.error.clone();
        let name_input = org.name_input.clone();
        let orgs = org.orgs.clone();

        let email: Option<SharedString> = self
            .state
            .read(cx)
            .auth_user()
            .map(|u| u.email.clone().into());

        let memberships: AnyElement =
            match &orgs {
                Loadable::Idle | Loadable::Loading => div()
                    .mt(px(24.0))
                    .child(popover::skeleton_rows(
                        "org-skeleton",
                        &theme,
                        2,
                        cx.entity_id(),
                        cx,
                    ))
                    .into_any_element(),
                Loadable::Error(message) => div()
                    .mt(px(24.0))
                    .child(
                        popover::error_row(&theme, message).child(
                            div()
                                .id("orgs-retry")
                                .px(px(Theme::SPACE_SM))
                                .py(px(3.0))
                                .rounded(px(Theme::CONTROL_RADIUS))
                                .border_1()
                                .border_color(theme.border)
                                .text_color(theme.text)
                                .cursor_pointer()
                                .hover(|s| s.bg(theme.glass_hover()))
                                .on_click(cx.listener(|this, _, _, cx| this.load_orgs(cx)))
                                .child(SharedString::from("Retry")),
                        ),
                    )
                    .into_any_element(),
                Loadable::Ready(rows) if rows.is_empty() => Empty.into_any_element(),
                Loadable::Ready(rows) => div()
                    .mt(px(24.0))
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .pb(px(8.0))
                            .text_size(crate::typography::ui_rems(11.0))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme.readable_muted(0.6))
                            .child(SharedString::from(
                                "Or continue in a workspace you belong to",
                            )),
                    )
                    .child(div().flex().flex_col().gap(px(4.0)).children(
                        rows.iter().enumerate().map(|(ix, row)| {
                            let org_id = row.organization_id.clone();
                            div()
                                .id(("org-row", ix))
                                .px(px(12.0))
                                .py(px(8.0))
                                .rounded(px(8.0))
                                .border_1()
                                .border_color(theme.border)
                                .bg(theme.bg)
                                .text_size(crate::typography::ui_rems(13.0))
                                .text_color(theme.text)
                                .when(submitting, |el| el.opacity(0.5))
                                .cursor_pointer()
                                .hover(|s| s.bg(crate::theme::wash(0.11)))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.select_org(org_id.clone(), cx);
                                }))
                                .child(SharedString::from(row.name.clone()))
                        }),
                    ))
                    .into_any_element(),
            };

        // luna App.tsx OrgGate: w-400 card on the grid — logo, headline,
        // explainer (+ signed-in email), name form with a white Create button,
        // then existing memberships and the account escape hatch.
        let blurb: SharedString = match email {
            Some(email) => format!(
                "LUNA is organized around workspaces — create one for yourself or your team. Signed in as {email}."
            )
            .into(),
            None => {
                "LUNA is organized around workspaces — create one for yourself or your team."
                    .into()
            }
        };
        let card = div()
            .w(px(400.0))
            .px(px(32.0))
            .py(px(36.0))
            .rounded(px(12.0))
            .border_1()
            .border_color(theme.border)
            .bg(theme.surface_card)
            .shadow_lg()
            .flex()
            .flex_col()
            .child(
                icon(icons::LUNA_LOGO)
                    .w(px(24.4))
                    .h(px(28.0))
                    .text_color(theme.text),
            )
            .child(
                div()
                    .mt(px(20.0))
                    .text_size(crate::typography::ui_rems(18.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme.text)
                    .child(SharedString::from("Create your workspace")),
            )
            .child(
                div()
                    .mt(px(6.0))
                    .mb(px(24.0))
                    .text_size(crate::typography::ui_rems(13.0))
                    .line_height(crate::typography::ui_rems(19.0))
                    .text_color(theme.text_muted)
                    .child(blurb),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(8.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h(px(36.0))
                            .flex()
                            .items_center()
                            .px(px(12.0))
                            .rounded(px(8.0))
                            .border_1()
                            .border_color(theme.border)
                            .bg(theme.bg)
                            .text_size(crate::typography::ui_rems(13.0))
                            .child(name_input),
                    )
                    .child(
                        div()
                            .id("create-org")
                            .h(px(36.0))
                            .px(px(16.0))
                            .flex()
                            .items_center()
                            .rounded(px(6.0))
                            .bg(theme.text)
                            .text_size(crate::typography::ui_rems(14.0))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme.on_solid)
                            .when(submitting, |el| el.opacity(0.5))
                            .cursor_pointer()
                            .hover(|s| s.opacity(0.9))
                            .on_click(cx.listener(|this, _, _, cx| this.create_org(cx)))
                            .child(SharedString::from(if submitting {
                                "Creating…"
                            } else {
                                "Create"
                            })),
                    ),
            )
            .child(memberships)
            .when_some(error, |el, message| {
                el.child(
                    div()
                        .mt(px(16.0))
                        .text_size(crate::typography::ui_rems(12.0))
                        .line_height(crate::typography::ui_rems(17.0))
                        .text_color(theme.readable_tone(theme.danger_muted, 0.9))
                        .child(message),
                )
            })
            .child(
                div().mt(px(24.0)).flex().flex_row().child(
                    div()
                        .id("org-signout")
                        .text_size(crate::typography::ui_rems(12.0))
                        .text_color(theme.readable_muted(0.6))
                        .cursor_pointer()
                        .hover(|s| s.text_color(theme.text))
                        .on_click(cx.listener(|this, _, _, cx| this.cancel_auth_setup(cx)))
                        .child(SharedString::from(if local_setup {
                            "Cancel sync setup"
                        } else {
                            "Use a different account"
                        })),
                ),
            );

        div()
            .absolute()
            .inset_0()
            .occlude()
            .bg(theme.bg)
            .child(grid_backdrop(&theme))
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(motion::fade_in("org-gate-card", card)),
            )
            .into_any_element()
    }
}

/// The sign-in gate's faint grid backdrop (luna styles.css `.bg-grid`):
/// 44px hairlines at white 3.5%, with the radial mask approximated by edge
/// gradients back into the page background (gpui has no mask-image).
fn grid_backdrop(theme: &Theme) -> AnyElement {
    let line = crate::theme::hairline(0.035);
    let bg = theme.bg;
    const STEP: f32 = 44.0;
    const SPAN: f32 = 2640.0;
    let verticals = (1..(SPAN / STEP) as usize).map(|i| {
        div()
            .absolute()
            .left(px(i as f32 * STEP))
            .top_0()
            .bottom_0()
            .w(px(1.0))
            .bg(line)
    });
    let horizontals = (1..((SPAN * 0.75) / STEP) as usize).map(|i| {
        div()
            .absolute()
            .top(px(i as f32 * STEP))
            .left_0()
            .right_0()
            .h(px(1.0))
            .bg(line)
    });
    div()
        .absolute()
        .inset_0()
        .overflow_hidden()
        .children(verticals)
        .children(horizontals)
        // Mask approximation: fade the grid back into the background toward
        // the window edges (the original masks to an ellipse at 50% / 40%).
        .child(
            div()
                .absolute()
                .top_0()
                .left_0()
                .right_0()
                .h(px(120.0))
                .bg(gpui::linear_gradient(
                    180.0,
                    gpui::linear_color_stop(bg, 0.0),
                    gpui::linear_color_stop(bg.opacity(0.0), 1.0),
                )),
        )
        .child(
            div()
                .absolute()
                .bottom_0()
                .left_0()
                .right_0()
                .h(px(260.0))
                .bg(gpui::linear_gradient(
                    0.0,
                    gpui::linear_color_stop(bg, 0.0),
                    gpui::linear_color_stop(bg.opacity(0.0), 1.0),
                )),
        )
        .child(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left_0()
                .w(px(200.0))
                .bg(gpui::linear_gradient(
                    90.0,
                    gpui::linear_color_stop(bg, 0.0),
                    gpui::linear_color_stop(bg.opacity(0.0), 1.0),
                )),
        )
        .child(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .right_0()
                .w(px(200.0))
                .bg(gpui::linear_gradient(
                    270.0,
                    gpui::linear_color_stop(bg, 0.0),
                    gpui::linear_color_stop(bg.opacity(0.0), 1.0),
                )),
        )
        .into_any_element()
}

/// A size-6 icon button for the titlebar strip (luna window-controls.tsx:
/// `grid size-6 place-items-center rounded-md text-muted-foreground`).
fn window_control_button(
    id: &'static str,
    icon_path: &'static str,
    theme: &Theme,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let muted = theme.text_muted;
    let fade_key = format!("window-control-{id}");
    div()
        .id(id)
        .size(px(24.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.0))
        .cursor_pointer()
        // luna window-controls.tsx: `transition-colors` — the wash fades.
        .bg(motion::hover_blend(
            &fade_key,
            theme.glass_hover().opacity(0.0),
            theme.glass_hover(),
        ))
        .on_hover(motion::hover_listener(fade_key))
        // Buttons in/over a titlebar drag strip must be EXCLUDED from the
        // strip's event surface entirely. `.occlude()` (gpui
        // `HitboxBehavior::BlockMouse`) makes the window hit-test STOP at the
        // button, so every `is_hovered`-guarded strip listener — the
        // mouse-down that arms the drag, the mouse-move that hands AppKit a
        // native drag session (`performWindowDragWithEvent:`, whose second
        // quick click zooms NATIVELY on macOS), and the `click_count == 2`
        // zoom handler — never fires with the pointer over a button. It also
        // removes the button's rect from the native Drag control-area
        // hit-test on Windows/Linux. The click-level stop_propagation is
        // zed's ButtonLike belt on top. Double-click on EMPTY strip space
        // still zooms — nothing occludes it there.
        .occlude()
        .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
        .on_click(move |event, window, cx| {
            cx.stop_propagation();
            on_click(event, window, cx)
        })
        .child(icon(icon_path).size(px(16.0)).text_color(muted))
}

const WINDOWS_CAPTION_BUTTON_WIDTH: f32 = 36.0;
const WINDOWS_CAPTION_WIDTH: f32 = WINDOWS_CAPTION_BUTTON_WIDTH * 3.0;

/// Right padding for titlebar content: past the native Windows caption
/// cluster, or past luna's own Linux caption buttons (10px edge inset +
/// the button row) when the layout puts any on the right.
fn titlebar_right_padding(is_windows: bool, linux_right_captions: usize, base: f32) -> f32 {
    base + if is_windows {
        WINDOWS_CAPTION_WIDTH
    } else if linux_right_captions > 0 {
        10.0 + caption_buttons_width(linux_right_captions)
    } else {
        0.0
    }
}

/// A Windows-owned caption target using the same system glyphs and native
/// non-client hit-test areas as GPUI/Zed's platform titlebar.
fn windows_caption_button(
    id: &'static str,
    glyph: &'static str,
    area: WindowControlArea,
    theme: &Theme,
    close: bool,
) -> impl IntoElement {
    let (hover_bg, hover_fg, active_bg, active_fg) = if close {
        let red: gpui::Hsla = gpui::rgb(0xe81123).into();
        (
            red,
            gpui::white(),
            red.opacity(0.8),
            gpui::white().opacity(0.8),
        )
    } else {
        (
            theme.glass_hover(),
            theme.text,
            theme.glass_hover().opacity(0.7),
            theme.text,
        )
    };
    div()
        .id(id)
        .w(px(WINDOWS_CAPTION_BUTTON_WIDTH))
        .h_full()
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .text_size(crate::typography::ui_rems(10.0))
        .text_color(theme.text)
        .hover(move |style| style.bg(hover_bg).text_color(hover_fg))
        .active(move |style| style.bg(active_bg).text_color(active_fg))
        .occlude()
        .window_control_area(area)
        .child(glyph)
}

/// A Linux caption button in luna's own cluster style (24px, rounded-6,
/// 16px linear icon). gpui's `WindowControlArea` hit-testing is inert on
/// Linux, so unlike the Windows cluster these carry explicit click handlers
/// (`minimize_window` / `zoom_window` / `remove_window`), the same calls
/// zed's Linux titlebar makes. `occlude` + `prevent_default` keep them out
/// of the drag strip's event surface (see [`window_control_button`]).
fn linux_caption_button(
    id: &'static str,
    icon_path: &'static str,
    close: bool,
    theme: &Theme,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let (muted, hover_bg, hover_fg) = if close {
        let red: gpui::Hsla = gpui::rgb(0xe81123).into();
        (theme.text_muted, red, gpui::white())
    } else {
        (theme.text_muted, theme.glass_hover(), theme.text)
    };
    div()
        .id(id)
        // gpui svgs don't inherit the div's text color — recolor the glyph
        // on hover through the group instead (zed's WindowControl idiom).
        .group("linux-caption-button")
        .size(px(24.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.0))
        .cursor_pointer()
        .hover(move |style| style.bg(hover_bg))
        .occlude()
        .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
        .on_click(move |event, window, cx| {
            cx.stop_propagation();
            on_click(event, window, cx)
        })
        .child(
            icon(icon_path)
                .size(px(16.0))
                .text_color(muted)
                .group_hover("linux-caption-button", move |style| {
                    style.text_color(hover_fg)
                }),
        )
}

/// A titlebar history button (luna window-controls.tsx): enabled it is a
/// normal window-control button; disabled it dims to 35% opacity and ignores
/// the pointer (`disabled:pointer-events-none disabled:opacity-35`).
fn nav_history_button(
    id: &'static str,
    icon_path: &'static str,
    enabled: bool,
    theme: &Theme,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    if !enabled {
        return div()
            .size(px(24.0))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            // Even disabled it reads as a control — occlude so double-clicks
            // on it don't fall through to the titlebar strip's zoom handler.
            .occlude()
            .child(
                icon(icon_path)
                    .size(px(16.0))
                    .text_color(theme.readable_muted(0.35)),
            )
            .into_any_element();
    }
    window_control_button(id, icon_path, theme, on_click).into_any_element()
}

/// A size-7 icon button for the main-panel header (luna __root.tsx:
/// `grid size-7 place-items-center rounded-md text-muted-foreground`).
fn header_icon_button(
    id: &'static str,
    icon_path: &'static str,
    theme: &Theme,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let muted = theme.text_muted;
    let fade_key = format!("header-icon-{id}");
    div()
        .id(id)
        .size(px(28.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.0))
        .cursor_pointer()
        // luna __root.tsx header buttons: `transition-colors`.
        .bg(motion::hover_blend(
            &fade_key,
            crate::theme::wash(0.0),
            crate::theme::wash(0.11),
        ))
        .on_hover(motion::hover_listener(fade_key))
        // Same occlusion + click-swallowing as [`window_control_button`]: this
        // button sits inside the chat header's titlebar drag region, so its
        // rect must be carved out of the strip's drag/double-click surface.
        .occlude()
        .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
        .on_click(move |event, window, cx| {
            cx.stop_propagation();
            on_click(event, window, cx)
        })
        .child(icon(icon_path).size(px(16.0)).text_color(muted))
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.render_time = Some(std::time::Instant::now());
        if self.all_file_edits_flushed(cx)
            && self.composer.read(cx).draft_flush_complete()
            && let Some(action) = self.pending_exit.take()
        {
            let shell = cx.weak_entity();
            window.defer(cx, move |window, cx| {
                if matches!(action, PendingExit::Quit) {
                    crate::app_menus::request_quit(cx);
                } else {
                    shell
                        .update(cx, |shell, cx| match action {
                            PendingExit::CloseWindow => {
                                if shell.prepare_window_close(cx) {
                                    window.remove_window();
                                }
                            }
                            PendingExit::RuntimeChange => shell.quit_for_runtime_change(cx),
                            PendingExit::InstallUpdate(staged) => {
                                shell.apply_staged_update(staged, cx)
                            }
                            PendingExit::Quit => unreachable!(),
                        })
                        .ok();
                }
            });
        }
        crate::transcript::record_view_frame("shell");
        self.viewport_width = f32::from(window.viewport_size().width);
        // Appearance actions persist independently of the shell. Mirror the
        // globals before any later debounced settings save can overwrite them.
        self.settings.appearance = crate::appearance::mode(cx);
        self.settings.theme_selection = crate::appearance::themes(cx);
        self.settings.accent = crate::appearance::accent(cx);
        self.settings.surface = crate::appearance::surface(cx);
        self.sync_independent_settings(cx);
        let theme = Theme::of(cx);
        // The shell frost sits over native desktop blur on macOS and Windows.
        // Content surfaces add their own backgrounds over this shared tint.
        let (frost, text, font) = (theme.glass(), theme.text, theme.font_sans.clone());
        let (workspace_scope, auth) = {
            let state = self.state.read(cx);
            (state.workspace_scope, state.auth.clone())
        };
        self.sync_flow = sync_flow_after_auth(self.sync_flow, workspace_scope, auth.as_ref());
        let restart_required = self.sync_flow == SyncFlow::SignedOutRestartRequired;
        let gate = self
            .debug_gate
            .clone()
            .unwrap_or_else(|| self.state.read(cx).gate());

        let browser_profile = {
            let state = self.state.read(cx);
            crate::links::workspace_locator(
                state.workspace_scope,
                state.auth.as_ref(),
                state.local_device_id.as_deref(),
            )
        };
        if browser_profile.is_some() && browser_profile != self.browser_profile {
            if self.browser_profile.is_some() {
                for browser in self.browsers.values() {
                    browser.update(cx, |browser, cx| browser.close(cx));
                }
                self.browsers.clear();
                self.browser_subs.clear();
                self.browser_context = crate::browser::BrowserContext::default();
            }
            self.browser_profile = browser_profile;
        }
        let browser_active = matches!(gate, GatePhase::Ready)
            && !restart_required
            && matches!(self.route, Route::Chat)
            && (self.right_pane_open(cx) || self.tween_active(self.right_tween));
        // Native clipping follows the animated GPUI mask. Drags only transfer
        // pointer ownership; the browser continues rendering and reflowing.
        let browser_dragging = cx.has_active_drag();
        #[cfg(target_os = "macos")]
        let browser_resize_inset = if self.right_pane_open(cx)
            && !self.right_pane_expanded
            && !self.tween_active(self.right_tween)
        {
            // The browser starts inside the panel's one-point left border.
            px(PANE_RESIZE_HITBOX_HALF_WIDTH - 1.0)
        } else {
            px(0.0)
        };
        let selected_surface = self.resolved_right_active(cx);
        for (id, browser) in &self.browsers {
            let presentation = crate::browser::model::presentation(
                browser_active && selected_surface == RightSurface::Browser(*id),
                browser_dragging,
            );
            browser.update(cx, |browser, cx| {
                #[cfg(target_os = "macos")]
                browser.set_resize_inset(browser_resize_inset, cx);
                browser.set_shortcuts(&self.settings.keymap);
                browser.set_presentation(presentation, cx);
            });
        }

        // Fullscreen hides the macOS traffic lights — reflow the control
        // cluster with a 200ms ease-out tween (§1.1). A fullscreen transition
        // resizes the window, which re-renders us, so polling here is exact.
        let fullscreen = window.is_fullscreen();
        if self.fullscreen != Some(fullscreen) {
            if self.fullscreen.is_some() && cfg!(target_os = "macos") {
                self.titlebar_tween = Some(WidthTween::new(
                    titlebar_cluster_start(!fullscreen),
                    titlebar_cluster_start(fullscreen),
                ));
            }
            self.fullscreen = Some(fullscreen);
        }
        // Linux CSD: (re-)resolve which caption buttons we draw and on which
        // side — decorations can flip server↔client at runtime and the
        // desktop's button layout is user configuration.
        self.linux_captions = Self::resolve_linux_captions(window, cx);
        if cfg!(target_os = "linux") && self.button_layout_sub.is_none() {
            self.button_layout_sub =
                Some(cx.observe_button_layout_changed(window, |_, _, cx| cx.notify()));
        }
        // Manual tween drive bookkeeping for this pass (see [`WidthTween`]).
        self.reduced_motion = motion::reduced_motion(cx);
        self.motion_active.set(false);

        if self.activation_sub.is_none() {
            self.activation_sub = Some(cx.observe_window_activation(
                window,
                |this: &mut Shell, window, cx| {
                    if !window.is_window_active() {
                        this.reset_command_palette_key_state();
                        this.set_jump_hints(false, cx);
                    }
                },
            ));
        }

        // A live handle can refer to an unmounted element. Recover against
        // the completed frame so newly mounted dialogs can claim focus first.
        if self.focus_sub.is_none() {
            self.focus_sub = Some(cx.on_focus_lost(window, |this: &mut Shell, window, cx| {
                let root = this.shortcut_focus.clone();
                let unfocused = this.unfocused.clone();
                let preferred = this.composer.focus_handle(cx);
                window.on_next_frame(move |window, cx| {
                    restore_mounted_focus(&root, &preferred, &unfocused, window, cx);
                });
                cx.notify();
            }));
        }
        let shortcut_focus = self.shortcut_focus.clone();
        let unfocused = self.unfocused.clone();
        let preferred_focus = self.composer.focus_handle(cx);
        window.defer(cx, move |window, cx| {
            restore_mounted_focus(&shortcut_focus, &preferred_focus, &unfocused, window, cx);
        });

        // Modifier events follow focus too. Reconcile from the window's input
        // snapshot so a missed release (or pointer event after it) heals hints.
        if !window.is_window_active() {
            self.set_jump_hints(false, cx);
        } else if self.jump_hints {
            // Only a fresh modifier event may turn hints on: activation can
            // retain the snapshot from before Cmd+Tab.
            self.update_jump_hints(&window.modifiers(), cx);
        }

        let root = div()
            .id("shell-root")
            .track_focus(&self.shortcut_focus)
            .child(div().track_focus(&self.unfocused))
            .relative()
            .flex()
            .flex_row()
            .size_full()
            .bg(frost)
            // Linux CSD: a floating window rounds its corners against the
            // desktop (the window composites with alpha — see
            // `Theme::window_background_appearance`); tiled/maximized keeps
            // square edges flush with the screen. Everything, including the
            // splash and caption chrome, clips to the curve.
            .when(Self::linux_window_floating(window), |el| {
                el.rounded(px(LINUX_WINDOW_CORNER_RADIUS)).overflow_hidden()
            })
            .text_color(text)
            .font_family(font)
            .text_size(crate::typography::ui_rems(14.0))
            .capture_key_down(cx.listener(Self::on_key_down_capture))
            .on_key_down(cx.listener(Self::on_key_down))
            .on_drag_move(cx.listener(Self::on_sidebar_drag))
            .on_drag_move(cx.listener(Self::on_right_pane_drag))
            .on_drag_move(cx.listener(Self::on_terminal_drag))
            // The panel shortcuts are chat-scoped chrome: in Settings they are
            // no-ops (luna __root.tsx gates the hotkey on `!isSettings`, and
            // the terminal panel is only mounted on session routes). The
            // sidebar toggle stays live everywhere, as in the original.
            .on_action(cx.listener(|this, _: &ToggleTerminal, window, cx| {
                if matches!(this.route, Route::Chat) {
                    this.toggle_terminal(window, cx)
                }
            }))
            .on_action(cx.listener(|this, _: &SaveFile, window, cx| {
                if matches!(this.route, Route::Chat)
                    && this.composer.update(cx, |composer, cx| {
                        composer.stash_if_message_input_focused(window, cx)
                    })
                {
                    return;
                }
                if matches!(this.route, Route::Chat) && this.right_pane_open(cx) {
                    let file = match this.resolved_right_active(cx) {
                        RightSurface::Files => this.files.get(&this.panel_key(cx)).cloned(),
                        RightSurface::File(id) => this.file_surfaces.get(&id).cloned(),
                        _ => None,
                    };
                    if let Some(file) = file {
                        file.update(cx, |file, cx| file.save_active_document(cx));
                    }
                }
            }))
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| this.toggle_sidebar(cx)))
            // New session works from anywhere — `open_new_session` routes back
            // to chat itself, so Settings is not a dead spot.
            .on_action(cx.listener(|this, _: &NewSession, _, cx| this.open_new_session(cx)))
            // Native Settings menu item and the platform convention (Cmd+, on
            // macOS, Ctrl+, elsewhere) always land on the default section.
            .on_action(cx.listener(
                |this, action: &crate::project_commands::OpenCommandPreview, window, cx| {
                    this.add_browser_surface(Some(action.0.clone()), window, cx);
                },
            ))
            .on_action(cx.listener(|this, _: &OpenSettings, _, cx| {
                this.open_settings(SettingsSection::DEFAULT, cx)
            }))
            // Chat-scoped, unlike new-session — `cycle_session` holds the guard
            // and says why.
            .on_action(cx.listener(|this, _: &NextSession, _, cx| this.cycle_session(true, cx)))
            .on_action(cx.listener(|this, _: &PrevSession, _, cx| this.cycle_session(false, cx)))
            .on_action(cx.listener(|this, _: &ToggleChanges, window, cx| {
                if matches!(this.route, Route::Chat) {
                    this.toggle_right_pane(cx);
                    if !this.right_pane_open(cx) {
                        // The hidden editor can retain a focus handle after unmounting.
                        // Restore a mounted target so the next shortcut can reopen it.
                        window.focus(&this.composer.focus_handle(cx), cx);
                    }
                }
            }))
            // Chat-scoped like the panel toggles: Settings has no current
            // session to archive. Quiet under an open popover, like the other
            // session-nav shortcuts.
            .on_action(cx.listener(|this, _: &ArchiveSession, _, cx| {
                if matches!(this.route, Route::Chat) && !this.overlay_owns_keyboard(cx) {
                    this.archive_selected_chat(cx)
                }
            }))
            // A jump routes back to chat itself, so Settings is not a dead
            // spot — the same call a click on that sidebar row makes. But an
            // open picker/palette owns the keyboard: no jumping underneath
            // it. The MODEL menu advertises these same slots on its rows and
            // this matched binding beats its key handler to the dispatch —
            // forward the slot instead of eating it.
            .on_action(cx.listener(|this, jump: &JumpSession, _, cx| {
                let pickers = this.composer.read(cx).pickers().clone();
                let handled = pickers.update(cx, |pickers, cx| pickers.jump_model_slot(jump.0, cx));
                if !handled && !this.overlay_owns_keyboard(cx) {
                    this.jump_to_session(jump.0, cx)
                }
            }))
            .on_modifiers_changed(
                cx.listener(|this, event, _, cx| this.on_modifiers_changed(event, cx)),
            )
            .on_action(cx.listener(|this, _: &ToggleCommandPalette, window, cx| {
                this.toggle_command_palette(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleActivity, window, cx| {
                this.toggle_activity(window, cx);
            }))
            .on_action(cx.listener(|this, _: &OpenModelPicker, window, cx| {
                if matches!(this.route, Route::Chat) && !this.overlay_owns_keyboard(cx) {
                    let pickers = this.composer.read(cx).pickers().clone();
                    pickers.update(cx, |pickers, cx| pickers.open_model_menu(window, cx));
                }
            }))
            .on_action(cx.listener(|this, _: &AddSpacePalette, _, cx| {
                if this.add_space.is_some() {
                    this.dismiss_add_space(cx);
                } else {
                    this.open_add_space(cx);
                }
            }));

        let render_gate = if restart_required {
            GatePhase::Loading
        } else {
            gate.clone()
        };
        let root = match &render_gate {
            GatePhase::Ready => {
                // Focus is a sync signal: on the rising edge of window
                // activation, nudge every open room to verify liveness — a
                // broadcast-deaf socket (accepted writes, runtime pongs,
                // nothing delivered; 2026-08-04 incident) then heals within
                // seconds of the user looking at the app rather than waiting
                // out the background probe cadence.
                let window_active = window.is_window_active();
                if window_active && !self.was_window_active {
                    self.state.update(cx, |s, cx| s.probe_sync(cx));
                }
                self.was_window_active = window_active;
                // A run finishing while you're LOOKING at the session must not
                // badge "completed" until you leave and return — mark it seen
                // live while the window is active (idempotent guard inside;
                // one extra frame settles it).
                if window_active {
                    let unseen_selected = {
                        let s = self.state.read(cx);
                        s.selected_chat_row()
                            .filter(|c| c.unseen())
                            .map(|c| c.id.clone())
                    };
                    if let Some(chat_id) = unseen_selected {
                        self.state
                            .update(cx, |s, cx| s.mark_chat_seen(&chat_id, cx));
                    }
                }
                // Capture knob: `LUNA_OPEN_DIALOG=model` pops the combined
                // harness/model menu (needs `window`, so it fires here rather
                // than in `on_state_changed`).
                if self.debug_dialog.as_deref() == Some("model") {
                    self.debug_dialog = None;
                    self.composer
                        .update(cx, |c, cx| c.debug_open_model_menu(window, cx));
                }
                // MessageRail width gate: hide below 48rem of main-panel width.
                let viewport = f32::from(window.viewport_size().width);
                self.viewport_height = f32::from(window.viewport_size().height);
                // Stamped for `right_target` — the expanded changes panel
                // sizes itself to the viewport.
                self.viewport_width = viewport;
                let on_chat = matches!(self.route, Route::Chat);
                let right_target_width = if on_chat { self.right_now(cx) } else { 0.0 };
                let panel_handoff = self.composer_dock.borrow_mut().observe_pane(
                    self.state.read(cx).selected_chat.is_some(),
                    right_target_width,
                    on_chat && !self.reduced_motion,
                    self.render_time.unwrap_or_else(std::time::Instant::now),
                );
                if panel_handoff {
                    self.motion_active.set(true);
                }
                let main_target_width =
                    conversation_width(viewport, self.sidebar_target(), right_target_width);
                let main_transition = self.active_tween_endpoints(self.main_takeover_tween);
                let main_content_width =
                    stable_panel_content_width(main_target_width, main_transition);
                let transcript_width = self.composer_dock.borrow_mut().transcript_width(
                    main_content_width,
                    self.state.read(cx).selected_chat.is_some(),
                    panel_handoff,
                );
                let main_width = (transcript_width - 10.0).max(0.0);
                // Clearance excludes the terminal dock: the transcript
                // viewport ends at the dock's top (see the underlay in
                // `render_main`), so only the chrome above it overlaps.
                let term_h = self.eval_tween(self.terminal_tween, self.terminal_target(cx));
                let stack_h = (self.bottom_stack.get() - term_h).max(0.0);
                let expected_has_composer = {
                    let state = self.state.read(cx);
                    !state.spaces.is_empty() && state.selected_chat.is_some()
                };
                let bottom_stack_ready = bottom_stack_measurement_matches(
                    self.bottom_stack_has_composer.get(),
                    expected_has_composer,
                );
                self.transcript.update(cx, |t, cx| {
                    t.set_rail_enabled(rail::rail_visible(main_width), cx);
                    if bottom_stack_ready && expected_has_composer {
                        t.set_bottom_clearance(stack_h, cx);
                    }
                });

                let sidebar = self.render_sidebar(cx);
                let sidebar_handle = self.resize_handle(
                    "sidebar-resize",
                    PaneResizeKind::Sidebar,
                    || SidebarResize,
                    |shell, _| {
                        shell.settings.sidebar_width = SIDEBAR_DEFAULT;
                        shell.sidebar_edge_bounce = None;
                    },
                    cx,
                );
                let main = self.render_main(window, main_content_width, transcript_width, cx);
                // The Changes pane is chat-scoped chrome: the Settings route
                // never renders it (luna __root.tsx `!isSettings && activeChat`
                // around the diff column) — the per-session open flags stay
                // intact for the return trip.
                let right_open = on_chat && self.right_pane_open(cx);
                // Takeover mode derives its width from the viewport, so a
                // manual drag handle would fight the expanded target.
                let right_handle = (right_open
                    && !panel_handoff
                    && !self.right_pane_expanded
                    && !self.tween_active(self.right_tween))
                .then(|| {
                    self.resize_handle(
                        "right-pane-resize",
                        PaneResizeKind::Right,
                        || RightPaneResize,
                        |shell, _| {
                            shell.settings.right_pane_width = RIGHT_PANE_DEFAULT;
                            shell.right_edge_bounce = None;
                        },
                        cx,
                    )
                    // A forgiving transparent hit target centered on the
                    // seam; the panel's 1px border remains the visual divider.
                    .left(px(-PANE_RESIZE_HITBOX_HALF_WIDTH))
                });
                let right: AnyElement = if on_chat {
                    self.render_right_pane(window, cx)
                } else {
                    Empty.into_any_element()
                };
                let overlays = self.render_overlays(window.viewport_size(), window, cx);
                // Copied out (not held) — `render_title_bar` needs `cx` mutable.
                let border_color = Theme::of(cx).border;
                // No inset cards (user request): the conversation column sits
                // flush and unbordered, the transcript directly on the frost
                // glass; the changes pane is a flush left-bordered glass panel
                // (built inside `render_right_pane`).
                let main = if main_transition.is_some() {
                    div()
                        .h_full()
                        .w(px(main_content_width))
                        .flex_none()
                        .flex()
                        .child(main)
                        .into_any_element()
                } else {
                    main
                };
                let card: AnyElement = div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_row()
                    .overflow_hidden()
                    .child(main)
                    .into_any_element();
                // The whole app page is one keyed `animate-in` entrance (luna
                // App.tsx `<div key={phase} className="animate-in h-full">`):
                // arriving from the splash or any gate fades the page in; the
                // splash-out crossfades over it on boot.
                // The sidebar resize handle FLOATS over the sidebar/card seam
                // (zero layout width, same idiom as the changes-pane grabber)
                // so the sidebar's right gutter stays exactly as wide as its
                // left one — a 5px flex child here read as lopsided spacing.
                let sidebar_seam = div()
                    .w(px(0.0))
                    .h_full()
                    .flex_none()
                    .relative()
                    .child(sidebar_handle.left(px(-PANE_RESIZE_HITBOX_HALF_WIDTH)));
                // Keep the right resize target outside the pane's
                // overflow-hidden width container. This mirrors the sidebar
                // seam and lets the target straddle both adjacent panes.
                // Paint it after the page so page input cannot occlude the
                // inner half. A deferred draw would capture all native input.
                let right_seam: AnyElement = if let Some(handle) = right_handle {
                    div()
                        .w(px(0.0))
                        .h_full()
                        .flex_none()
                        .absolute()
                        .left_0()
                        .top_0()
                        .child(handle)
                        .into_any_element()
                } else {
                    Empty.into_any_element()
                };
                let title_bar = self.render_title_bar(cx);
                // Sidebar tone: a slightly lighter column behind the sidebar,
                // spanning the FULL window height (under the traffic lights,
                // through the titlebar, down to the bottom edge). Its width
                // rides the same tween as the sidebar, so the tone melts away
                // with the collapse instead of vanishing in a frame.
                let sidebar_now = self.sidebar_now();
                // Hairline on its right edge — full height like the tone,
                // so the sidebar column reads as its own surface.
                // The tone carries the window's left corners when the CSD
                // window floats — with one caveat: a corner radius is
                // clamped to the element's own size, and the COLLAPSED
                // sidebar is a ~1px border sliver (the grab affordance).
                // macOS trims that hairline with the window server's native
                // corner clip; we reproduce the same trim by insetting the
                // sliver vertically to where the curve begins, so its tips
                // never float over the transparent corner cutouts.
                let window_corner = Self::window_corner_radius(window);
                let sidebar_tone = div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left_0()
                    .w(px(sidebar_now))
                    .when(window_corner > 0.0, |el| {
                        if sidebar_now >= 2.0 * window_corner {
                            el.rounded_tl(px(window_corner))
                                .rounded_bl(px(window_corner))
                        } else {
                            el.top(px(window_corner)).bottom(px(window_corner))
                        }
                    })
                    .bg(crate::theme::wash(0.05))
                    .border_r_1()
                    .border_color(border_color);
                // The content row spans the FULL window height — the titlebar
                // overlays it (glass, no fill), so the transcript can scroll
                // under the header and fade out at its edge. Columns that
                // must NOT underlap (sidebar content, the changes panel,
                // settings) pad themselves down by the titlebar height.
                let page = div()
                    .size_full()
                    .relative()
                    .child(
                        div()
                            .size_full()
                            .flex()
                            .flex_row()
                            .child(sidebar)
                            .child(sidebar_seam)
                            .child(card)
                            .child(
                                div()
                                    .h_full()
                                    .flex_none()
                                    .relative()
                                    .child(right)
                                    .child(right_seam),
                            ),
                    )
                    .child(div().absolute().top_0().left_0().right_0().child(title_bar))
                    .child(self.render_titlebar_cluster(cx))
                    .children(overlays);
                root.child(sidebar_tone)
                    .child(motion::fade_in("phase-app", page))
            }
            GatePhase::Loading => root, // splash overlay covers boot
            GatePhase::OrgGate => {
                let card = self.render_org_gate(cx);
                root.child(card)
            }
            phase @ (GatePhase::Failed(_) | GatePhase::SignIn) => {
                let card = self.render_gate_card(phase, cx);
                root.child(card)
            }
        };
        let root = if restart_required {
            let restart = self.render_signed_out_restart(cx);
            root.child(restart)
        } else {
            root
        };

        // A manually-driven tween is mid-flight: keep frames coming (the same
        // scheduling `with_animation` would have requested). Hover color fades
        // ride the same clock; their once-per-frame tick lives here (this is
        // the window's root render — it runs exactly once per frame).
        if self.motion_active.get() | motion::hover_fades_active() {
            window.request_animation_frame();
        }

        // Boot splash overlay: visible → crossfades out on Ready → removed.
        let root = match self.splash {
            SplashPhase::Visible => {
                let theme = Theme::of(cx).clone();
                root.child(loaders::splash_overlay(&theme, false, cx.entity_id(), cx))
            }
            SplashPhase::FadingOut => {
                let theme = Theme::of(cx).clone();
                root.child(loaders::splash_overlay(&theme, true, cx.entity_id(), cx))
            }
            SplashPhase::Gone => root,
        };

        // Caption controls are shell-level chrome, not Ready-page content:
        // keep them above the splash and every auth/org/error gate as well as
        // the full application. Gate pages also need a drag surface because
        // they do not render the unified tabs/settings titlebar — on Windows
        // the native `Drag` control area, on Linux the explicit
        // `start_window_move` strip (the control-area hit-test is inert
        // there); macOS drags gate windows natively.
        let root = if (!restart_required && matches!(gate, GatePhase::Ready))
            || cfg!(target_os = "macos")
        {
            root
        } else {
            root.child(
                self.titlebar_drag_region(
                    "gate-titlebar-drag",
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .right_0()
                        .h(px(Theme::TITLEBAR_HEIGHT)),
                    cx,
                ),
            )
        };
        let root = root
            .children(self.render_windows_caption_controls(window, cx))
            .children(self.render_linux_caption_controls(window, cx))
            // Last so the invisible CSD resize strips sit above every other
            // element at the window edges.
            .children(Self::render_linux_resize_borders(window));
        self.render_time = None;
        root
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_sidebar_keyboard_activation_ignores_repeats_and_other_keys() {
        assert!(is_settings_nav_activation("enter", false));
        assert!(is_settings_nav_activation("space", false));
        assert!(!is_settings_nav_activation("enter", true));
        assert!(!is_settings_nav_activation("space", true));
        assert!(!is_settings_nav_activation("escape", false));
    }

    #[test]
    fn settings_entry_and_fixture_routes_keep_their_targets() {
        assert_eq!(SettingsSection::ALL[0], SettingsSection::DEFAULT);
        assert_eq!(SettingsSection::DEFAULT, SettingsSection::General);
        assert_eq!(
            SettingsSection::from_fixture_route("settings"),
            Some(SettingsSection::General)
        );
        assert_eq!(
            SettingsSection::from_fixture_route("settings/general"),
            Some(SettingsSection::General)
        );
        assert_eq!(
            SettingsSection::from_fixture_route("settings/devices"),
            Some(SettingsSection::Devices)
        );
        assert_eq!(
            SettingsSection::from_fixture_route("settings/agents"),
            Some(SettingsSection::Providers)
        );
        assert_eq!(
            SettingsSection::from_fixture_route("settings/harnesses"),
            Some(SettingsSection::Providers)
        );
    }

    #[test]
    fn history_search_parses_all_supported_filters_and_requires_current_profile() {
        let spaces = vec![luna_proto::Space {
            execution_defaults: Default::default(),
            id: "project-id".into(),
            device_id: "device-id".into(),
            path: "/synthetic/project".into(),
            name: Some("Project".into()),
            git_detected: false,
            git_checked_at: None,
            checkout_id: None,
            created_at: chrono::DateTime::UNIX_EPOCH,
        }];
        let devices = vec![luna_proto::Device {
            id: "device-id".into(),
            name: "Machine".into(),
            platform: "synthetic".into(),
            last_seen_at: None,
            created_at: None,
            version: None,
            capabilities: Vec::new(),
        }];
        let request = history_search_request(
            "needle machine:Machine project:Project harness:codex model:gpt status:failed tool:exec app:browser after:2026-01-02 before:2026-01-03",
            true,
            None,
            &spaces,
            &devices,
        );
        assert_eq!(request.scope, HistorySearchScope::SelectedProject);
        assert_eq!(request.project_id.as_deref(), Some("project-id"));
        assert_eq!(request.query, "needle");
        assert_eq!(request.filters.device_ids, ["device-id"]);
        assert_eq!(request.filters.harnesses, [HarnessId::Codex]);
        assert_eq!(request.filters.models, ["gpt"]);
        assert_eq!(request.filters.outcomes, [ActivityOutcome::Failed]);
        assert_eq!(request.filters.tools, ["exec"]);
        assert_eq!(request.filters.apps, ["browser"]);
        assert!(request.filters.after_ms.is_some());
        assert!(request.filters.before_ms.is_some());
        assert!(history_search_response_is_live(4, 4, 8, 8));
        assert!(!history_search_response_is_live(4, 5, 8, 8));
        assert!(!history_search_response_is_live(4, 4, 8, 9));
        let stale_profile_project = history_search_request(
            "needle",
            false,
            Some("project-from-old-profile".into()),
            &spaces,
            &devices,
        );
        assert_eq!(
            stale_profile_project.scope,
            HistorySearchScope::SelectedProject
        );
        assert_eq!(stale_profile_project.project_id, None);
        let global_a =
            history_search_request("needle", true, Some("project-id".into()), &spaces, &devices);
        let global_b = history_search_request("needle", true, None, &spaces, &devices);
        assert_eq!(
            HistorySearchScopeKey::from(&global_a),
            HistorySearchScopeKey::from(&global_b),
            "the explicit global scope does not follow selected-project changes"
        );
    }

    fn history_chat(session: &str, cwd: &str, permission_mode: PermissionMode) -> luna_proto::Chat {
        luna_proto::Chat {
            id: "history-task".into(),
            device_id: "device".into(),
            title: None,
            archived: false,
            settlement_override: None,
            pinned_order: None,
            cwd: Some(cwd.into()),
            branch: None,
            checkout_id: None,
            source_context: None,
            config: Some(ChatConfig {
                harness: HarnessId::Codex,
                provider_account_id: Some("account".into()),
                model: Some("model".into()),
                reasoning: None,
                model_options: Default::default(),
                sandbox: luna_proto::SandboxLevel::WorkspaceWrite,
                permission_mode,
                effective_permission_mode: permission_mode,
                permission_ceiling_source: None,
                permission_degradation_reason: None,
                permission_audit: Vec::new(),
            }),
            last_message_preview: None,
            last_message_at: None,
            created_at: chrono::DateTime::UNIX_EPOCH,
            harness_session_id: Some(session.into()),
            harness_session_cwd: Some(cwd.into()),
            space_id: None,
            last_seen_at: None,
            manually_unread: false,
            room_gen: None,
        }
    }

    #[test]
    fn history_review_drops_late_cancelled_and_profile_changed_responses() {
        assert!(history_flow_response_is_live(7, 7));
        assert!(!history_flow_response_is_live(8, 7));

        let reviewed = history_chat("session-a", "/workspace", PermissionMode::Ask);
        let same = reviewed.clone();
        assert!(history_review_profile_unchanged(
            &reviewed.harness_session_id,
            &reviewed.cwd,
            &reviewed.config,
            Some(&same),
        ));
        let mut changed_session = same.clone();
        changed_session.harness_session_id = Some("session-b".into());
        assert!(!history_review_profile_unchanged(
            &reviewed.harness_session_id,
            &reviewed.cwd,
            &reviewed.config,
            Some(&changed_session),
        ));
        let mut changed_permissions = same;
        changed_permissions.config.as_mut().unwrap().permission_mode = PermissionMode::Auto;
        assert!(!history_review_profile_unchanged(
            &reviewed.harness_session_id,
            &reviewed.cwd,
            &reviewed.config,
            Some(&changed_permissions),
        ));
    }

    #[test]
    fn history_origin_rejects_delayed_response_with_identical_chat_metadata() {
        let reviewed = history_chat("session-a", "/workspace", PermissionMode::Auto);
        let replacement_profile_chat = reviewed.clone();
        assert!(history_review_profile_unchanged(
            &reviewed.harness_session_id,
            &reviewed.cwd,
            &reviewed.config,
            Some(&replacement_profile_chat),
        ));

        let origin = HistoryEngineOrigin {
            engine_generation: 23,
        };
        assert!(origin.run_if_current(24, || "opened").is_none());
        assert_eq!(origin.run_if_current(23, || "opened"), Some("opened"));

        let mut observed_generation = 23;
        assert!(history_engine_generation_changed(
            &mut observed_generation,
            24
        ));
        assert_eq!(observed_generation, 24);
        assert!(!history_engine_generation_changed(
            &mut observed_generation,
            24
        ));
        assert!(history_flow_response_is_live(9, 9));
        assert!(!history_flow_response_is_live(10, 9));
    }

    #[test]
    fn history_origin_blocks_fork_edit_and_restore_submission_after_replacement() {
        let origin = HistoryEngineOrigin {
            engine_generation: 31,
        };
        let mut old_engine_rpc_calls = 0;
        let mut replacement_engine_rpc_calls = 0;

        for operation in ["fork", "edit", "restore"] {
            let dispatched = origin.run_if_current(32, || {
                old_engine_rpc_calls += 1;
                replacement_engine_rpc_calls += 1;
                operation
            });
            assert_eq!(dispatched, None, "{operation} must fail closed");
        }
        assert_eq!(old_engine_rpc_calls, 0);
        assert_eq!(replacement_engine_rpc_calls, 0);

        let mut same_origin_rpc_calls = 0;
        for operation in ["fork", "edit", "restore"] {
            assert_eq!(
                origin.run_if_current(31, || {
                    same_origin_rpc_calls += 1;
                    operation
                }),
                Some(operation)
            );
        }
        assert_eq!(same_origin_rpc_calls, 3);
    }

    #[test]
    fn history_fork_permission_choice_preserves_immutable_l04_ceiling() {
        let mut full = history_chat("session", "/workspace", PermissionMode::Full)
            .config
            .unwrap();
        assert_eq!(history_fork_initial_permission(&full), PermissionMode::Full);
        let auto = next_history_fork_permission(PermissionMode::Full, PermissionMode::Full)
            .expect("Full ceiling permits Auto");
        assert_eq!(auto, PermissionMode::Auto);
        assert_eq!(
            next_history_fork_permission(auto, PermissionMode::Full),
            Some(PermissionMode::Full),
            "lowering the selection must not lower the immutable ceiling"
        );

        full.effective_permission_mode = PermissionMode::Auto;
        assert_eq!(history_fork_initial_permission(&full), PermissionMode::Auto);
        assert_eq!(
            next_history_fork_permission(PermissionMode::Auto, PermissionMode::Auto),
            Some(PermissionMode::Auto),
            "an Auto ceiling must never offer Full"
        );

        let legacy = history_chat("session", "/workspace", PermissionMode::Ask)
            .config
            .unwrap();
        assert_eq!(
            history_fork_initial_permission(&legacy),
            PermissionMode::Ask
        );
        assert_eq!(
            next_history_fork_permission(PermissionMode::Ask, PermissionMode::Ask),
            None,
            "legacy Ask is explanatory state, not a selectable choice"
        );
        assert!(
            [
                PermissionMode::Ask,
                PermissionMode::Auto,
                PermissionMode::Full
            ]
            .into_iter()
            .all(|ceiling| history_fork_permission_choices(ceiling)
                .iter()
                .all(|choice| *choice != PermissionMode::Ask)),
            "Ask must never be a selectable history-fork mode"
        );
    }

    #[test]
    fn sidebar_drag_nudges_each_edge_once_until_rearmed() {
        let min = sidebar_drag_sample(SIDEBAR_MIN, None, false);
        assert_eq!(min.width, SIDEBAR_MIN);
        assert_eq!(min.edge, Some(motion::ResizeEdge::Min));
        assert!(min.starts_bounce);

        let held_min = sidebar_drag_sample(SIDEBAR_MIN - 80.0, min.edge, false);
        assert_eq!(held_min.width, SIDEBAR_MIN);
        assert_eq!(held_min.edge, min.edge);
        assert!(!held_min.starts_bounce);

        let inside = sidebar_drag_sample(SIDEBAR_MIN + 1.0, held_min.edge, false);
        assert_eq!(inside.edge, None);
        assert!(!inside.starts_bounce);

        let rearmed_min = sidebar_drag_sample(SIDEBAR_MIN - 1.0, inside.edge, false);
        assert!(rearmed_min.starts_bounce);

        let max = sidebar_drag_sample(SIDEBAR_MAX, rearmed_min.edge, false);
        assert_eq!(max.width, SIDEBAR_MAX);
        assert_eq!(max.edge, Some(motion::ResizeEdge::Max));
        assert!(max.starts_bounce);

        let held_max = sidebar_drag_sample(SIDEBAR_MAX + 80.0, max.edge, false);
        assert_eq!(held_max.width, SIDEBAR_MAX);
        assert!(!held_max.starts_bounce);
    }

    #[test]
    fn sidebar_drag_stays_exact_in_range_and_reduced_motion_never_nudges() {
        let middle = sidebar_drag_sample(312.0, None, false);
        assert_eq!(middle.width, 312.0);
        assert_eq!(middle.edge, None);
        assert!(!middle.starts_bounce);

        for pointer_x in [
            SIDEBAR_MIN - 100.0,
            SIDEBAR_MIN,
            SIDEBAR_MAX,
            SIDEBAR_MAX + 100.0,
        ] {
            let sample = sidebar_drag_sample(pointer_x, None, true);
            assert!((SIDEBAR_MIN..=SIDEBAR_MAX).contains(&sample.width));
            assert!(!sample.starts_bounce);
        }
    }

    #[test]
    fn right_pane_uses_the_shared_clamp_and_edge_latch() {
        let min =
            motion::resize_drag_sample(RIGHT_PANE_MIN - 40.0, RIGHT_PANE_MIN, 820.0, None, false);
        assert_eq!(min.width, RIGHT_PANE_MIN);
        assert_eq!(min.edge, Some(motion::ResizeEdge::Min));
        assert!(min.starts_bounce);

        let held = motion::resize_drag_sample(
            RIGHT_PANE_MIN - 80.0,
            RIGHT_PANE_MIN,
            820.0,
            min.edge,
            false,
        );
        assert!(!held.starts_bounce);

        let max = motion::resize_drag_sample(900.0, RIGHT_PANE_MIN, 820.0, None, false);
        assert_eq!(max.width, 820.0);
        assert_eq!(max.edge, Some(motion::ResizeEdge::Max));
        assert!(max.starts_bounce);
    }

    #[test]
    fn sidebar_bounce_has_rounded_out_and_return_phases() {
        assert_eq!(
            motion::resize_bounce_offset(motion::ResizeEdge::Max, 0.0),
            0.0
        );
        assert_eq!(
            motion::resize_bounce_offset(
                motion::ResizeEdge::Max,
                motion::RESIZE_EDGE_BOUNCE_OUT_FRACTION
            ),
            motion::RESIZE_EDGE_NUDGE
        );
        assert_eq!(
            motion::resize_bounce_offset(motion::ResizeEdge::Max, 1.0),
            0.0
        );

        let gentle_start = motion::resize_bounce_offset(motion::ResizeEdge::Max, 0.01);
        let outbound = motion::resize_bounce_offset(motion::ResizeEdge::Max, 0.2);
        let returning = motion::resize_bounce_offset(motion::ResizeEdge::Max, 0.7);
        assert!(gentle_start > 0.0 && gentle_start < 0.1);
        assert!(outbound > gentle_start && outbound < motion::RESIZE_EDGE_NUDGE);
        assert!(returning > 0.0 && returning < motion::RESIZE_EDGE_NUDGE);
        assert_eq!(
            motion::resize_bounce_offset(motion::ResizeEdge::Min, 0.2),
            -outbound
        );
    }

    #[test]
    fn every_default_shortcut_binds_on_this_platform() {
        // `apply_keymap` silently falls back on an unparseable combo, so a
        // default gpui cannot parse would ship as a dead shortcut.
        for id in crate::settings::ShortcutId::ALL {
            if id.default_combo().is_empty() {
                continue;
            }
            let combo = platform_combo(id.default_combo());
            assert!(
                Keystroke::parse(&combo).is_ok(),
                "{} default {combo:?} does not parse",
                id.label()
            );
        }
    }

    #[test]
    fn island_stays_centered_on_controls_while_expanding() {
        let center = (Theme::TITLEBAR_HEIGHT + Theme::TITLEBAR_TOP_PAD) * 0.5;
        for step in 0..=20 {
            let (top, height) = titlebar_island_vertical_geometry(step as f32 / 20.0);
            assert_eq!(top + height * 0.5, center);
            assert!((28.0..=32.0).contains(&height));
        }
        let (top, height) = titlebar_island_vertical_geometry(1.0);
        assert_eq!(center, 21.0);
        assert_eq!(center - 12.0 - top, 4.0);
        assert_eq!(top + height - (center + 12.0), 4.0);
    }

    #[test]
    fn new_thread_handoff_is_continuous_and_staged() {
        assert!(bottom_stack_measurement_matches(false, false));
        assert!(bottom_stack_measurement_matches(true, true));
        assert!(!bottom_stack_measurement_matches(false, true));
        assert!(!bottom_stack_measurement_matches(true, false));
        assert_eq!(new_thread_background_opacity(false), 1.0);
        assert_eq!(
            new_thread_background_opacity(true),
            NEW_THREAD_BACKGROUND_FROSTED_OPACITY
        );
        assert_eq!(new_thread_background_height(400.0), 288.0);
        assert!((new_thread_background_height(600.0) - 432.0).abs() < 0.001);
        assert_eq!(new_thread_background_height(1_000.0), 720.0);
        assert_eq!(new_thread_background_height(1_200.0), 760.0);
        assert!(new_thread_background_height(848.0) > 848.0 / 2.0);
    }

    #[test]
    fn right_pane_ceiling_preserves_the_chat_floor() {
        assert_eq!(right_pane_max_width(1200.0, 256.0), 644.0);
        assert_eq!(1200.0 - 256.0 - 644.0, CHAT_PANEL_MIN);
        // The chat floor wins over the right pane's preferred 360px minimum
        // when the whole window is unusually narrow.
        assert_eq!(right_pane_max_width(800.0, 256.0), 244.0);
        assert_eq!(800.0 - 256.0 - 244.0, CHAT_PANEL_MIN);
    }

    #[test]
    fn right_pane_takeover_consumes_the_chat_column() {
        assert_eq!(right_pane_takeover_width(1200.0, 256.0), 944.0);
        assert_eq!(1200.0 - 256.0 - 944.0, 0.0);
    }

    #[test]
    fn escape_interrupts_only_the_active_live_chat() {
        assert_eq!(
            resolve_shell_escape(
                "escape",
                false,
                true,
                Route::Chat,
                Some("chat-a"),
                Indicator::Working,
                false,
            ),
            ShellEscapeOutcome::InterruptChat("chat-a".to_owned())
        );
        assert_eq!(
            resolve_shell_escape(
                "escape",
                false,
                true,
                Route::Chat,
                Some("chat-b"),
                Indicator::AwaitingInput,
                false,
            ),
            ShellEscapeOutcome::InterruptChat("chat-b".to_owned())
        );
    }

    #[test]
    fn escape_ignores_non_live_or_ineligible_views() {
        assert_eq!(
            resolve_shell_escape(
                "escape",
                true,
                true,
                Route::Chat,
                Some("chat-a"),
                Indicator::Working,
                false,
            ),
            ShellEscapeOutcome::Blocked
        );
        for (route, selected, indicator, interrupting) in [
            (Route::Chat, Some("chat-a"), Indicator::None, false),
            (Route::Chat, None, Indicator::Working, false),
            (Route::Chat, Some("chat-a"), Indicator::Working, true),
            (
                Route::Settings(SettingsSection::Devices),
                Some("chat-a"),
                Indicator::Working,
                false,
            ),
        ] {
            assert_eq!(
                resolve_shell_escape(
                    "escape",
                    false,
                    true,
                    route,
                    selected,
                    indicator,
                    interrupting,
                ),
                ShellEscapeOutcome::Ignored
            );
        }
        assert_eq!(
            resolve_shell_escape(
                "enter",
                true,
                true,
                Route::Chat,
                Some("chat-a"),
                Indicator::Working,
                false,
            ),
            ShellEscapeOutcome::OtherKey
        );
    }

    #[test]
    fn escape_interrupt_is_opt_in() {
        assert_eq!(
            resolve_shell_escape(
                "escape",
                false,
                false,
                Route::Chat,
                Some("chat-a"),
                Indicator::Working,
                false,
            ),
            ShellEscapeOutcome::Ignored
        );
    }

    #[test]
    fn only_visible_sync_steps_block_escape() {
        assert!(!SyncFlow::Idle.has_visible_overlay());
        assert!(!SyncFlow::SwitchOffer { notice_open: false }.has_visible_overlay());
        assert!(SyncFlow::SwitchOffer { notice_open: true }.has_visible_overlay());
        assert!(SyncFlow::Importing { done: 1, total: 3 }.has_visible_overlay());
        assert!(SyncFlow::SignOutConfirm.has_visible_overlay());
    }

    #[test]
    fn right_pane_takeover_control_reverses_direction() {
        assert_eq!(tabs::right_pane_expand_icon(false), icons::EXPAND_ARROWS);
        assert_eq!(tabs::right_pane_expand_icon(true), icons::COLLAPSE_ARROWS);
    }

    #[test]
    fn pane_resize_hitboxes_yield_the_titlebar_chrome() {
        assert_eq!(PANE_RESIZE_HITBOX_TOP, Theme::TITLEBAR_HEIGHT);
        assert_eq!(PANE_RESIZE_HITBOX_HALF_WIDTH * 2.0, 20.0);
        assert_eq!(TERMINAL_RESIZE_HITBOX_HEIGHT, 10.0);
    }

    #[test]
    fn new_session_action_lives_in_the_titlebar_only_when_useful() {
        assert_eq!(titlebar_new_session_alpha(true, true), 1.0);
        assert_eq!(titlebar_new_session_alpha(true, false), 0.0);
        assert_eq!(titlebar_new_session_alpha(false, true), 0.0);
        assert_eq!(titlebar_new_session_alpha(false, false), 0.0);
    }

    #[test]
    fn right_panel_content_keeps_the_larger_width_only_during_transition() {
        assert_eq!(right_panel_content_width(520.0, None, None), 520.0);
        assert_eq!(
            right_panel_content_width(0.0, Some((520.0, 0.0)), None),
            520.0
        );
        assert_eq!(
            right_panel_content_width(760.0, Some((520.0, 760.0)), None),
            760.0
        );
        assert_eq!(
            right_panel_content_width(1064.0, Some((520.0, 1064.0)), Some(760.0)),
            760.0
        );

        let conversation = conversation_width(1320.0, 256.0, 520.0);
        let takeover = conversation_width(1320.0, 256.0, 1064.0);
        assert_eq!(conversation, 544.0);
        assert_eq!(takeover, 0.0);
        assert_eq!(
            stable_panel_content_width(takeover, Some((conversation, takeover))),
            conversation
        );
        assert_eq!(
            stable_panel_content_width(conversation, Some((takeover, conversation))),
            conversation
        );
    }

    #[tokio::test]
    async fn remote_shutdown_waits_for_ipc_release() {
        let dir = tempfile::tempdir().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let release = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            drop(listener);
        });

        wait_for_remote_engine_shutdown(port, dir.path(), Duration::from_secs(2))
            .await
            .unwrap();
        release.await.unwrap();
    }

    #[tokio::test]
    async fn rejected_legacy_session_stops_and_reboots_local() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("session.json"),
            r#"{"refreshToken":"still-valid","user":{"id":"user_1","email":"u@example.com"},"orgId":"org_1"}"#,
        )
        .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let boot = EngineBootConfig {
            data_dir: dir.path().to_path_buf(),
            ipc_port: port,
            edge_url: "http://127.0.0.1:1".into(),
            edge_token: None,
            org_id: None,
            workos_client_id: Some("client_test".into()),
            default_harness: luna_proto::HarnessId::Mock,
        };
        let local = crate::state::EngineHandle::bootstrap(boot.clone())
            .await
            .expect("legacy plaintext session is rejected and local profile opens");
        assert_eq!(local.engine_info().workspace_scope, WorkspaceScope::Local);

        local
            .client()
            .call(methods::SIGN_OUT, serde_json::json!({}))
            .await
            .expect("sign out clears credentials");
        stop_synced_runtime(local, port, dir.path())
            .await
            .expect("local runtime drains and releases ownership");

        assert!(!dir.path().join("session.json").exists());
        let local = crate::state::EngineHandle::bootstrap(boot)
            .await
            .expect("same process can continue locally");
        assert_eq!(local.engine_info().workspace_scope, WorkspaceScope::Local);
        local.shutdown().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn remote_shutdown_waits_for_engine_lock_release() {
        let dir = tempfile::tempdir().unwrap();
        let lock = InstanceLock::acquire(dir.path()).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let lock_released = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let released_by_task = lock_released.clone();
        let release = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            drop(lock);
            released_by_task.store(true, std::sync::atomic::Ordering::SeqCst);
        });

        wait_for_remote_engine_shutdown(port, dir.path(), Duration::from_secs(2))
            .await
            .unwrap();
        assert!(lock_released.load(std::sync::atomic::Ordering::SeqCst));
        release.await.unwrap();
    }

    #[tokio::test]
    async fn remote_shutdown_times_out_while_ipc_remains_open() {
        let dir = tempfile::tempdir().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let error = wait_for_remote_engine_shutdown(port, dir.path(), Duration::from_millis(100))
            .await
            .unwrap_err();

        assert!(error.contains("did not finish stopping"));
        drop(listener);
    }

    #[test]
    fn account_actions_follow_the_attached_workspace_scope() {
        assert_eq!(
            account_action_for_scope(Some(WorkspaceScope::Local), SyncFlow::Idle),
            Some(AccountAction::EnableSync)
        );
        assert_eq!(
            account_action_for_scope(Some(WorkspaceScope::Synced), SyncFlow::Idle),
            Some(AccountAction::SignOut)
        );
        assert_eq!(
            account_action_for_scope(Some(WorkspaceScope::Development), SyncFlow::Idle),
            None
        );
    }

    #[test]
    fn local_sign_in_offers_the_in_place_switch() {
        let signed_in = AuthState::SignedIn {
            user: luna_proto::UserProfile {
                id: "user-1".into(),
                email: "user@example.com".into(),
                name: None,
            },
            org_id: Some("org-1".into()),
        };

        assert_eq!(
            sync_flow_after_auth(
                SyncFlow::Enabling,
                Some(WorkspaceScope::Local),
                Some(&signed_in),
            ),
            SyncFlow::SwitchOffer { notice_open: true }
        );
        assert_eq!(
            sync_flow_after_auth(
                SyncFlow::Idle,
                Some(WorkspaceScope::Local),
                Some(&signed_in),
            ),
            SyncFlow::SwitchOffer { notice_open: true },
            "another viewport derives the pending switch from AuthStatus"
        );
        assert_eq!(
            sync_flow_after_auth(
                SyncFlow::SwitchOffer { notice_open: false },
                Some(WorkspaceScope::Local),
                Some(&signed_in),
            ),
            SyncFlow::SwitchOffer { notice_open: false },
            "shared auth updates do not reopen a postponed wizard"
        );
        assert_eq!(
            sync_flow_after_auth(
                SyncFlow::RestartPending { notice_open: false },
                Some(WorkspaceScope::Local),
                Some(&signed_in),
            ),
            SyncFlow::RestartPending { notice_open: false },
            "the quit fallback survives shared auth updates too"
        );
        assert_eq!(
            account_action_for_scope(
                Some(WorkspaceScope::Local),
                SyncFlow::SwitchOffer { notice_open: false },
            ),
            Some(AccountAction::RestartPending)
        );
        for notice_open in [true, false] {
            assert_eq!(
                sync_flow_after_auth(
                    SyncFlow::SwitchOffer { notice_open },
                    Some(WorkspaceScope::Local),
                    Some(&AuthState::SignedOut),
                ),
                SyncFlow::Idle,
                "revoked credentials cancel the pending switch"
            );
        }
    }

    #[test]
    fn import_summary_errors_are_a_failure_not_a_success() {
        // Clean summary → done with counts.
        let clean = serde_json::json!({
            "kind": "summary", "importedChats": 2, "skippedChats": 1, "errors": []
        });
        assert_eq!(import_summary_outcome(&clean), Ok((2, 1)));

        // Any error means the wizard must NOT say "all set" — partial
        // migrations surface as an explicit failure with the first cause.
        let partial = serde_json::json!({
            "kind": "summary", "importedChats": 1, "skippedChats": 0,
            "errors": ["chat c2: journal copy failed"]
        });
        let message = import_summary_outcome(&partial).expect_err("errors must fail");
        assert!(message.contains("journal copy failed"), "{message}");
        assert!(message.contains("1 imported"), "{message}");

        let many = serde_json::json!({
            "kind": "summary", "importedChats": 0, "skippedChats": 0,
            "errors": ["a", "b", "c"]
        });
        let message = import_summary_outcome(&many).expect_err("errors must fail");
        assert!(message.contains("3 failures"), "{message}");

        // A summary missing the errors field entirely (older engine) is
        // treated as clean rather than failing every import.
        let legacy = serde_json::json!({ "kind": "summary", "importedChats": 4 });
        assert_eq!(import_summary_outcome(&legacy), Ok((4, 0)));
    }

    #[test]
    fn spaces_only_local_work_still_gets_the_import_offer() {
        assert_eq!(local_work_phrase(0, 0), None, "nothing to bring");
        assert_eq!(local_work_phrase(2, 0).as_deref(), Some("the 2 sessions"));
        assert_eq!(
            local_work_phrase(0, 1).as_deref(),
            Some("the 1 project"),
            "a projects-only profile must be offered the import, not a bare switch"
        );
        assert_eq!(
            local_work_phrase(1, 2).as_deref(),
            Some("the 1 session and 2 projects")
        );
    }

    #[test]
    fn dismissed_import_failure_stays_reachable_on_a_synced_runtime() {
        let signed_in = AuthState::SignedIn {
            user: luna_proto::UserProfile {
                id: "user-1".into(),
                email: "user@example.com".into(),
                name: None,
            },
            org_id: Some("org-1".into()),
        };

        // "Later" postpones the failure notice; it must not evaporate.
        let dismissed = SyncFlow::ImportFailed { notice_open: false };
        assert_eq!(
            sync_flow_after_auth(dismissed, Some(WorkspaceScope::Synced), Some(&signed_in)),
            dismissed,
            "a postponed import failure survives auth/scope updates"
        );

        // …and Settings on the SYNCED runtime still exposes the re-entry
        // point. This is the whole point: after the switch there is no local
        // runtime left to re-derive an offer from, so this action
        // is the only path back to the retry dialog.
        assert_eq!(
            account_action_for_scope(Some(WorkspaceScope::Synced), dismissed),
            Some(AccountAction::RestartPending),
            "retry must remain reachable after dismissal"
        );
        assert_eq!(
            account_action_for_scope(
                Some(WorkspaceScope::Synced),
                SyncFlow::ImportFailed { notice_open: true },
            ),
            Some(AccountAction::RestartPending)
        );

        // Resolving the failure restores the normal synced menu.
        assert_eq!(
            account_action_for_scope(Some(WorkspaceScope::Synced), SyncFlow::Idle),
            Some(AccountAction::SignOut)
        );
    }

    #[test]
    fn switch_lifecycle_survives_the_runtime_replacement_window() {
        let signed_in = AuthState::SignedIn {
            user: luna_proto::UserProfile {
                id: "user-1".into(),
                email: "user@example.com".into(),
                name: None,
            },
            org_id: Some("org-1".into()),
        };
        for flow in [
            SyncFlow::Switching { import: true },
            SyncFlow::Importing { done: 1, total: 3 },
            SyncFlow::ImportDone {
                imported: 3,
                skipped: 0,
            },
            SyncFlow::ImportFailed { notice_open: true },
            SyncFlow::ImportFailed { notice_open: false },
        ] {
            // Local (before the stop), detached (mid-replacement), and synced
            // (replacement runtime up): the driver owns these states — auth
            // and scope edges must never reset them.
            assert_eq!(
                sync_flow_after_auth(flow, Some(WorkspaceScope::Local), Some(&signed_in)),
                flow
            );
            assert_eq!(sync_flow_after_auth(flow, None, None), flow);
            assert_eq!(
                sync_flow_after_auth(flow, Some(WorkspaceScope::Synced), Some(&signed_in)),
                flow
            );
        }
    }

    #[test]
    fn synced_sign_out_blocks_every_viewport_and_cannot_switch_accounts() {
        let signed_in_as_another_user = AuthState::SignedIn {
            user: luna_proto::UserProfile {
                id: "user-2".into(),
                email: "other@example.com".into(),
                name: None,
            },
            org_id: Some("org-2".into()),
        };

        assert_eq!(
            sync_flow_after_auth(
                SyncFlow::SigningOut,
                Some(WorkspaceScope::Synced),
                Some(&AuthState::SignedOut),
            ),
            SyncFlow::SignedOutRestartRequired,
            "the viewport that requested sign-out is blocked by AuthStatus"
        );
        assert_eq!(
            sync_flow_after_auth(
                SyncFlow::Idle,
                Some(WorkspaceScope::Synced),
                Some(&AuthState::SignedOut),
            ),
            SyncFlow::SignedOutRestartRequired,
            "another viewport observing the same runtime is also blocked"
        );
        assert_eq!(
            sync_flow_after_auth(
                SyncFlow::SignedOutRestartRequired,
                Some(WorkspaceScope::Synced),
                Some(&signed_in_as_another_user),
            ),
            SyncFlow::SignedOutRestartRequired,
            "new credentials cannot reopen the previous account's store"
        );
    }

    #[test]
    fn titlebar_cluster_matches_luna_window_controls() {
        // luna window-controls.tsx: `left: fullscreen ? 12 : 88` — the
        // cluster clears the {14,15} traffic lights, and reclaims the inset
        // when fullscreen hides them.
        assert_eq!(titlebar_cluster_start(false), 88.0);
        assert_eq!(titlebar_cluster_start(true), 12.0);
        assert_eq!(TITLEBAR_CONTROL_GAP, 2.0);
        assert_eq!(TITLEBAR_GROUP_GAP, Theme::SPACE_SM);
        assert_eq!(TITLEBAR_IDENTITY_GAP, Theme::SPACE_MD);
        assert_eq!(CLUSTER_BUTTONS_WIDTH, 82.0);
        assert_eq!(TITLEBAR_ACTION_SLOT_WIDTH, 32.0);
        assert_eq!(TITLEBAR_ACTION_EDGE_INSET, 6.0);
    }

    #[test]
    fn titlebar_spacer_selects_per_platform_and_fullscreen() {
        // macOS, lights visible: spacer fills up to the 88px cluster start.
        assert_eq!(titlebar_spacer_width(true, false, 10.0), 78.0);
        assert_eq!(titlebar_spacer_width(true, false, 12.0), 76.0);
        assert_eq!(titlebar_spacer_width(true, false, 26.0), 62.0);
        // macOS fullscreen: the inset animates away (clamped at zero when the
        // strip's own padding already exceeds the 12px cluster start).
        assert_eq!(titlebar_spacer_width(true, true, 10.0), 2.0);
        assert_eq!(titlebar_spacer_width(true, true, 26.0), 0.0);
        // Linux / Windows: never any inset.
        assert_eq!(titlebar_spacer_width(false, false, 10.0), 0.0);
        assert_eq!(titlebar_spacer_width(false, true, 10.0), 0.0);
        assert_eq!(
            TITLEBAR_CLUSTER_PAD + titlebar_spacer_width(true, false, TITLEBAR_CLUSTER_PAD),
            titlebar_cluster_start(false),
            "the rendered row padding and spacer must land on the declared cluster start"
        );
    }

    #[test]
    fn windows_caption_controls_reserve_titlebar_space() {
        assert_eq!(titlebar_right_padding(true, 0, 16.0), 124.0);
        assert_eq!(titlebar_right_padding(false, 0, 16.0), 16.0);
    }

    #[test]
    fn linux_caption_controls_reserve_titlebar_space() {
        // 24px buttons on the cluster's 2px rhythm.
        assert_eq!(caption_buttons_width(0), 0.0);
        assert_eq!(caption_buttons_width(1), 24.0);
        assert_eq!(caption_buttons_width(3), 76.0);
        // Right-side captions (the Linux default: minimize,maximize,close):
        // content pads past the 10px edge inset + the button row.
        assert_eq!(titlebar_right_padding(false, 3, 16.0), 16.0 + 10.0 + 76.0);
        // GNOME-vanilla ":close" — a single right button.
        assert_eq!(titlebar_right_padding(false, 1, 16.0), 16.0 + 10.0 + 24.0);
        // Left-side captions ("close:…" layouts) shift the app cluster right
        // by the button row + one 2px gap.
        assert_eq!(cluster_buttons_start(false, false, 0), 10.0);
        assert_eq!(cluster_buttons_start(false, false, 1), 10.0 + 24.0 + 2.0);
        assert_eq!(cluster_buttons_start(false, false, 3), 10.0 + 76.0 + 2.0);
        // macOS ignores the Linux caption count entirely.
        assert_eq!(cluster_buttons_start(true, false, 3), 88.0);
    }

    #[test]
    fn cluster_clearance_clears_the_overlay_buttons() {
        // Linux: buttons at 10..92; a 16px-padded header needs 84 more px to
        // put content at 92 + 8 breathing room.
        assert_eq!(cluster_clearance(false, false, 0, 16.0), 84.0);
        assert_eq!(cluster_clearance(false, false, 0, 10.0), 90.0);
        // Linux with a left-side close caption: everything shifts one slot.
        assert_eq!(cluster_clearance(false, false, 1, 16.0), 84.0 + 26.0);
        // macOS: buttons start at the 88px traffic-light cluster start.
        assert_eq!(
            cluster_clearance(true, false, 0, 16.0),
            88.0 + CLUSTER_BUTTONS_WIDTH + 8.0 - 16.0
        );
        // macOS fullscreen: cluster reclaims the inset (starts at 12).
        assert_eq!(
            cluster_clearance(true, true, 0, 16.0),
            12.0 + CLUSTER_BUTTONS_WIDTH + 8.0 - 16.0
        );
    }

    // ---- per-session panel flags (§1.10/1.11 parity: luna sessionPanels) ----

    #[test]
    fn session_panels_default_closed_per_chat() {
        let panels = SessionPanels::default();
        assert_eq!(panels.get("a"), ChatPanels::default());
        // Everything closed until explicitly opened (user request — the
        // brief default-open popped the pane on every visited session).
        assert!(!panels.get("a").terminal_open);
        assert!(!panels.get("a").changes_open);
        assert_eq!(panels.get("a").right_active, RightSurface::Picker);
        // The new-chat canvas ("" key) is its own session, also closed.
        assert!(!panels.get("").terminal_open);
    }

    #[test]
    fn session_panels_flags_are_chat_scoped() {
        let mut panels = SessionPanels::default();
        // Opening the terminal in chat A opens it ONLY in chat A.
        assert!(panels.toggle_terminal("a"));
        assert!(panels.get("a").terminal_open);
        assert!(!panels.get("b").terminal_open);
        assert!(!panels.get("").terminal_open);
        // Changes pane in B is independent of A's terminal.
        assert!(panels.toggle_changes("b"));
        assert!(panels.get("b").changes_open);
        assert!(!panels.get("b").terminal_open);
        assert!(!panels.get("a").changes_open);
        // Switching back to A restores A's state untouched.
        assert!(panels.get("a").terminal_open);
        // Toggling off round-trips.
        assert!(!panels.toggle_terminal("a"));
        assert!(!panels.get("a").terminal_open);
    }

    #[test]
    fn session_panels_both_flags_coexist_per_chat() {
        let mut panels = SessionPanels::default();
        panels.toggle_terminal("a");
        panels.toggle_changes("a");
        assert_eq!(
            panels.get("a"),
            ChatPanels {
                terminal_open: true,
                changes_open: true,
                ..Default::default()
            }
        );
        assert_eq!(panels.get("b"), ChatPanels::default());
        // The right pane round-trips back closed.
        assert!(!panels.toggle_changes("a"));
        assert!(!panels.get("a").changes_open);
    }

    #[test]
    fn session_panels_update_tracks_right_surfaces() {
        let mut panels = SessionPanels::default();
        panels.update("a", |p| p.right_active = RightSurface::Diff(3));
        assert_eq!(panels.get("a").right_active, RightSurface::Diff(3));
        // Other chats keep the picker default.
        assert_eq!(panels.get("b").right_active, RightSurface::Picker);
        panels.update("a", |p| p.right_active = RightSurface::Terminal(7));
        assert_eq!(panels.get("a").right_active, RightSurface::Terminal(7));
        panels.update("a", |p| p.right_active = RightSurface::Files);
        assert_eq!(panels.get("a").right_active, RightSurface::Files);
    }

    #[test]
    fn files_surface_is_single_instance_per_tab_list() {
        let mut tabs = vec![RightSurface::Terminal(1)];
        assert!(push_unique_right_surface(&mut tabs, RightSurface::Files));
        assert!(!push_unique_right_surface(&mut tabs, RightSurface::Files));
        assert_eq!(tabs, vec![RightSurface::Terminal(1), RightSurface::Files]);
    }

    #[test]
    fn file_editors_are_distinct_surface_tabs_with_stable_titles() {
        let mut tabs = vec![RightSurface::Files];
        assert!(push_unique_right_surface(&mut tabs, RightSurface::File(1)));
        assert!(push_unique_right_surface(&mut tabs, RightSurface::File(2)));
        assert!(!push_unique_right_surface(&mut tabs, RightSurface::File(1)));
        assert_eq!(workspace_file_title("src/árbol.rs"), "árbol.rs");
    }

    // ---- sidebar resort FLIP diff (§1.6) ----

    fn keys(list: &[(&str, f32)]) -> Vec<(String, f32)> {
        list.iter().map(|(k, h)| (k.to_string(), *h)).collect()
    }

    #[test]
    fn sidebar_chat_height_is_uniform() {
        assert_eq!(chat_row_height(false, false), 63.0);
        assert_eq!(chat_row_height(true, false), 63.0);
        assert_eq!(chat_row_height(false, true), 63.0);
        assert_eq!(chat_row_height(true, true), 63.0);
    }

    #[test]
    fn sidebar_harness_geometry_reflects_row_hierarchy() {
        assert_eq!(SIDEBAR_ACTIVE_HARNESS_TITLE_GAP, Theme::SPACE_SM);
        assert!(SIDEBAR_ACTIVE_HARNESS_TITLE_GAP < SIDEBAR_ARCHIVED_HARNESS_TITLE_GAP);
        assert!(SIDEBAR_ACTIVE_HARNESS_ICON_SIZE < SIDEBAR_ARCHIVED_HARNESS_ICON_SIZE);
    }

    #[test]
    fn sidebar_height_change_is_not_a_reorder() {
        let open = keys(&[("first-group", 105.0), ("second-group", 240.0)]);
        let collapsed = keys(&[("first-group", 40.0), ("second-group", 240.0)]);
        assert!(!sidebar_key_order_changed(&open, &collapsed));

        let reordered = keys(&[("second-group", 240.0), ("first-group", 40.0)]);
        assert!(sidebar_key_order_changed(&collapsed, &reordered));
    }

    #[test]
    fn resort_offsets_empty_when_order_unchanged() {
        let order = keys(&[("a", 29.0), ("b", 29.0), ("c", 45.0)]);
        assert!(resort_offsets(&order, &order, 2.0).is_empty());
    }

    #[test]
    fn resort_offsets_activity_moves_row_to_top() {
        // c (bottom, y=62) jumps to top: c glides down-from-above? No — c's
        // old y is 62, new y is 0 → starts +62 below… offset = old - new = +62,
        // painted at +62 decaying to 0 (a glide UP into place). a and b shift
        // down by c's height + gap (31).
        let old = keys(&[("a", 29.0), ("b", 29.0), ("c", 29.0)]);
        let new = keys(&[("c", 29.0), ("a", 29.0), ("b", 29.0)]);
        let offsets = resort_offsets(&old, &new, 2.0);
        assert_eq!(offsets.get("c"), Some(&62.0));
        assert_eq!(offsets.get("a"), Some(&-31.0));
        assert_eq!(offsets.get("b"), Some(&-31.0));
    }

    #[test]
    fn resort_offsets_respect_heights_and_gap() {
        // Tall row (45px) swaps with a short one (29px).
        let old = keys(&[("tall", 45.0), ("short", 29.0)]);
        let new = keys(&[("short", 29.0), ("tall", 45.0)]);
        let offsets = resort_offsets(&old, &new, 2.0);
        // short: old y 47 → new y 0; tall: old y 0 → new y 31.
        assert_eq!(offsets.get("short"), Some(&47.0));
        assert_eq!(offsets.get("tall"), Some(&-31.0));
    }

    #[test]
    fn resort_offsets_ignore_added_and_removed_keys() {
        let old = keys(&[("a", 29.0), ("gone", 29.0), ("b", 29.0)]);
        let new = keys(&[("new", 29.0), ("a", 29.0), ("b", 29.0)]);
        let offsets = resort_offsets(&old, &new, 2.0);
        // "new" has no old position (fades in instead); "gone" just goes.
        assert!(!offsets.contains_key("new"));
        assert!(!offsets.contains_key("gone"));
        // a: old 0 → new 31 (pushed down by the insert); b: 62 → 62 (gone's
        // slot replaced by "new" of equal height — no move, no entry).
        assert_eq!(offsets.get("a"), Some(&-31.0));
        assert_eq!(offsets.get("b"), None);
    }

    #[test]
    fn resort_glide_spec_matches_original() {
        // §1.6: 260ms cubic-bezier(0.22, 1, 0.36, 1).
        assert_eq!(RESORT.duration_ms, 260);
        assert_eq!(RESORT.curve, motion::EASE_RESORT);
    }

    // ---- navigation history (titlebar back/forward) ----

    fn chat(id: &str) -> NavEntry {
        NavEntry::Chat(id.to_string())
    }

    #[test]
    fn nav_history_starts_with_nothing_to_walk() {
        let nav = NavHistory::new(chat(""));
        assert!(!nav.can_back());
        assert!(!nav.can_forward());
        assert_eq!(*nav.current(), chat(""));
    }

    #[test]
    fn nav_push_then_back_and_forward() {
        let mut nav = NavHistory::new(chat("a"));
        nav.push(chat("b"));
        nav.push(NavEntry::Settings(SettingsSection::Devices));
        assert!(nav.can_back());
        assert!(!nav.can_forward());

        // Back walks toward the oldest entry without dropping anything.
        assert_eq!(
            nav.back(),
            Some(chat("b")),
            "back lands on the previous route"
        );
        assert_eq!(nav.back(), Some(chat("a")));
        assert!(!nav.can_back());
        assert!(nav.can_forward());
        assert_eq!(nav.back(), None, "past the oldest entry is a no-op");

        // Forward retraces the same path.
        assert_eq!(nav.forward(), Some(chat("b")));
        assert_eq!(
            nav.forward(),
            Some(NavEntry::Settings(SettingsSection::Devices))
        );
        assert!(!nav.can_forward());
        assert_eq!(nav.forward(), None);
    }

    #[test]
    fn nav_push_dedups_the_current_route() {
        let mut nav = NavHistory::new(chat("a"));
        nav.push(chat("a"));
        nav.push(chat("a"));
        assert_eq!(nav.len(), 1, "re-selecting the current route never stacks");
        nav.push(NavEntry::Settings(SettingsSection::Providers));
        nav.push(NavEntry::Settings(SettingsSection::Providers));
        assert_eq!(nav.len(), 2);
    }

    #[test]
    fn nav_push_truncates_the_forward_branch() {
        // a → b → c, back to a, then push d: the b/c branch is gone (browser
        // semantics — luna's memory history PUSH truncates entries ahead).
        let mut nav = NavHistory::new(chat("a"));
        nav.push(chat("b"));
        nav.push(chat("c"));
        nav.back();
        nav.back();
        assert_eq!(*nav.current(), chat("a"));
        assert!(nav.can_forward());
        nav.push(chat("d"));
        assert!(!nav.can_forward(), "the old branch is unreachable");
        assert_eq!(nav.len(), 2);
        assert_eq!(nav.back(), Some(chat("a")));
        assert_eq!(nav.forward(), Some(chat("d")));
    }

    #[test]
    fn nav_replace_swaps_in_place() {
        // The boot auto-select replaces the untouched canvas entry, so Back
        // stays disabled after landing in the last-used chat.
        let mut nav = NavHistory::new(chat(""));
        nav.replace(chat("boot"));
        assert_eq!(nav.len(), 1);
        assert_eq!(*nav.current(), chat("boot"));
        assert!(!nav.can_back());
    }

    #[test]
    fn nav_settings_sections_are_distinct_entries() {
        let mut nav = NavHistory::new(chat("a"));
        nav.push(NavEntry::Settings(SettingsSection::Devices));
        nav.push(NavEntry::Settings(SettingsSection::Shortcuts));
        assert_eq!(nav.len(), 3, "section changes are navigations");
        assert_eq!(
            nav.back(),
            Some(NavEntry::Settings(SettingsSection::Devices))
        );
        assert_eq!(nav.back(), Some(chat("a")));
    }

    #[test]
    fn sidebar_disclosure_motion_lands_exactly_on_its_target() {
        let mut tween = SidebarDisclosureMotion::new(1, 240.0, 0.0);
        tween.started = std::time::Instant::now() - motion::COLLAPSE.total().mul_f32(2.0);
        assert_eq!(tween.current(), 0.0);
        assert!(!tween.animating());
    }
}

#[cfg(test)]
mod exit_regressions {
    use super::*;
    use async_trait::async_trait;
    use base64::Engine as _;
    use futures::StreamExt as _;
    use gpui::{AppContext, TestAppContext};
    use luna_proto::{ComposerDraft, ComposerDraftRead, ComposerDraftSaveOutcome, DraftContext};
    use luna_rpc::{RpcError, RpcReply, RpcService};
    use std::collections::HashMap;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    const HANDOFF_SOURCE: &str = "handoff-source";
    const HANDOFF_MARKER: &str = "[LUNA user-selected context handoff]";

    struct ShellDraftRpc {
        drafts: Mutex<HashMap<String, ComposerDraft>>,
    }

    struct UsageRefreshRpc {
        reads: AtomicUsize,
    }

    #[async_trait]
    impl RpcService for UsageRefreshRpc {
        async fn handle(
            &self,
            method: &str,
            _params: serde_json::Value,
        ) -> Result<RpcReply, RpcError> {
            if method != methods::GET_USAGE_SNAPSHOT {
                return Err(RpcError::UnknownMethod(method.into()));
            }
            self.reads.fetch_add(1, Ordering::SeqCst);
            RpcReply::value(&serde_json::json!({
                "summary": {
                    "contractVersion": luna_proto::USAGE_CONTRACT_VERSION,
                    "readAtMs": 1,
                    "timeZone": "UTC",
                    "days": 30,
                    "resolution": "day",
                    "buckets": [],
                    "sources": [],
                    "scanDurationMs": 0,
                    "partial": false,
                    "warnings": []
                },
                "limits": []
            }))
        }
    }

    struct HistorySearchRpc {
        calls: Mutex<Vec<HistorySearchRequest>>,
        release_project_a: tokio::sync::Notify,
    }

    struct PaletteFenceRpc {
        label: &'static str,
        block_stale: bool,
        calls: Mutex<Vec<HistorySearchRequest>>,
        release: tokio::sync::Notify,
    }

    struct PaletteReplacementRpc {
        calls: Mutex<Vec<HistorySearchRequest>>,
        release: tokio::sync::Notify,
    }

    struct PaletteSelectionRpc {
        calls: Mutex<Vec<HistorySearchRequest>>,
        release: tokio::sync::Notify,
    }

    impl PaletteFenceRpc {
        fn new(label: &'static str, block_stale: bool) -> Self {
            Self {
                label,
                block_stale,
                calls: Mutex::new(Vec::new()),
                release: tokio::sync::Notify::new(),
            }
        }
    }

    impl PaletteReplacementRpc {
        fn new() -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                release: tokio::sync::Notify::new(),
            }
        }
    }

    impl PaletteSelectionRpc {
        fn new() -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                release: tokio::sync::Notify::new(),
            }
        }
    }

    impl HistorySearchRpc {
        fn new() -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                release_project_a: tokio::sync::Notify::new(),
            }
        }

        fn event(project_id: &str, index: usize) -> ActivityEnvelope {
            ActivityEnvelope {
                schema_version: luna_proto::ACTIVITY_ENVELOPE_VERSION,
                event_id: format!("{project_id}-{index}"),
                environment_id: "local".into(),
                device_id: "local".into(),
                task_id: format!("{project_id}-task"),
                turn_id: format!("turn-{index}"),
                sequence: index as u64,
                timestamp_ms: 10_000 - index as i64,
                kind: luna_proto::ActivityKind::Answer,
                outcome: ActivityOutcome::Succeeded,
                harness: None,
                provider: None,
                model: None,
                reasoning: None,
                project_id: Some(project_id.into()),
                workspace_id: None,
                tool: None,
                app: None,
                file: None,
                command: None,
                summary: format!("{project_id} result {index}"),
                redaction: luna_proto::ActivityRedaction::None,
                source: luna_proto::ActivitySourceRef {
                    task_id: format!("{project_id}-task"),
                    message_id: format!("message-{index}"),
                    part_id: None,
                    available_here: true,
                    unavailable_reason: None,
                },
            }
        }
    }

    #[async_trait]
    impl RpcService for HistorySearchRpc {
        async fn handle(
            &self,
            method: &str,
            params: serde_json::Value,
        ) -> Result<RpcReply, RpcError> {
            if method != methods::SEARCH_SESSION_HISTORY {
                return Err(RpcError::UnknownMethod(method.into()));
            }
            let request: HistorySearchRequest = serde_json::from_value(params)
                .map_err(|error| RpcError::BadParams(error.to_string()))?;
            self.calls.lock().unwrap().push(request.clone());
            let project_id = if request.scope == HistorySearchScope::Global {
                "global"
            } else {
                request.project_id.as_deref().unwrap_or("personal")
            };
            if project_id == "project-a" {
                self.release_project_a.notified().await;
            }
            let (start, end) = if project_id == "project-b" {
                if request.cursor.is_some() {
                    (50, 60)
                } else {
                    (0, 50)
                }
            } else {
                (0, 1)
            };
            let events = (start..end)
                .map(|index| Self::event(project_id, index))
                .collect::<Vec<_>>();
            let next_cursor = (project_id == "project-b" && end == 50).then(|| {
                let event = events.last().unwrap();
                luna_proto::HistoryCursor {
                    timestamp_ms: event.timestamp_ms,
                    sequence: event.sequence,
                    event_id: event.event_id.clone(),
                }
            });
            RpcReply::value(&HistorySearchPage {
                events,
                next_cursor,
                indexed_through_ms: 10_000,
                heuristic_redaction: true,
            })
        }
    }

    #[async_trait]
    impl RpcService for PaletteFenceRpc {
        async fn handle(
            &self,
            method: &str,
            params: serde_json::Value,
        ) -> Result<RpcReply, RpcError> {
            if method != methods::SEARCH_SESSION_HISTORY {
                return Err(RpcError::UnknownMethod(method.into()));
            }
            let request: HistorySearchRequest = serde_json::from_value(params)
                .map_err(|error| RpcError::BadParams(error.to_string()))?;
            self.calls.lock().unwrap().push(request.clone());
            let project = request.project_id.as_deref().unwrap_or("personal");
            if self.block_stale
                && ((request.query.starts_with("old") && project == "project-a")
                    || request.query == "profile")
            {
                self.release.notified().await;
            }
            let mut event = HistorySearchRpc::event(project, 0);
            event.event_id = format!("{}-{}-{project}", self.label, request.query);
            event.summary = format!("{}:{}:{project}", self.label, request.query);
            RpcReply::value(&HistorySearchPage {
                events: vec![event],
                next_cursor: None,
                indexed_through_ms: 10_000,
                heuristic_redaction: true,
            })
        }
    }

    #[async_trait]
    impl RpcService for PaletteReplacementRpc {
        async fn handle(
            &self,
            method: &str,
            params: serde_json::Value,
        ) -> Result<RpcReply, RpcError> {
            if method != methods::SEARCH_SESSION_HISTORY {
                return Err(RpcError::UnknownMethod(method.into()));
            }
            let request: HistorySearchRequest = serde_json::from_value(params)
                .map_err(|error| RpcError::BadParams(error.to_string()))?;
            self.calls.lock().unwrap().push(request.clone());
            if request.query == "held"
                || (request.query == "scope-switch"
                    && request.scope == HistorySearchScope::SelectedProject)
                || request.cursor.is_some()
            {
                self.release.notified().await;
            }
            if request.query == "error" {
                return Err(RpcError::Failed("synthetic history failure".into()));
            }
            if request.query == "empty" {
                return RpcReply::value(&HistorySearchPage {
                    events: Vec::new(),
                    next_cursor: None,
                    indexed_through_ms: 10_000,
                    heuristic_redaction: true,
                });
            }
            let project = if request.scope == HistorySearchScope::Global {
                "global"
            } else {
                request.project_id.as_deref().unwrap_or("personal")
            };
            let mut event = HistorySearchRpc::event(project, 0);
            event.event_id = format!("{}-{project}", request.query);
            event.summary = format!("{} {project}", request.query);
            RpcReply::value(&HistorySearchPage {
                events: vec![event],
                next_cursor: None,
                indexed_through_ms: 10_000,
                heuristic_redaction: true,
            })
        }
    }

    #[async_trait]
    impl RpcService for PaletteSelectionRpc {
        async fn handle(
            &self,
            method: &str,
            params: serde_json::Value,
        ) -> Result<RpcReply, RpcError> {
            if method != methods::SEARCH_SESSION_HISTORY {
                return Err(RpcError::UnknownMethod(method.into()));
            }
            let request: HistorySearchRequest = serde_json::from_value(params)
                .map_err(|error| RpcError::BadParams(error.to_string()))?;
            self.calls.lock().unwrap().push(request);
            self.release.notified().await;
            let mut event = HistorySearchRpc::event("project-a", 0);
            event.task_id = "stable-a".into();
            event.source.task_id = "stable-a".into();
            event.event_id = "stable-a-event".into();
            event.summary = "stable async content".into();
            RpcReply::value(&HistorySearchPage {
                events: vec![event],
                next_cursor: None,
                indexed_through_ms: 10_000,
                heuristic_redaction: true,
            })
        }
    }

    impl ShellDraftRpc {
        fn new(drafts: Vec<ComposerDraft>) -> Self {
            Self {
                drafts: Mutex::new(
                    drafts
                        .into_iter()
                        .map(|draft| (draft.draft_id.clone(), draft))
                        .collect(),
                ),
            }
        }

        fn text_for_task(&self, task_id: Option<&str>) -> Option<String> {
            self.drafts
                .lock()
                .unwrap()
                .values()
                .filter(|draft| draft.task_id.as_deref() == task_id)
                .max_by_key(|draft| draft.revision)
                .map(|draft| draft.text.clone())
        }
    }

    #[async_trait]
    impl RpcService for ShellDraftRpc {
        async fn handle(
            &self,
            method: &str,
            params: serde_json::Value,
        ) -> Result<RpcReply, RpcError> {
            match method {
                methods::GET_COMPOSER_DRAFT => {
                    let task_id = params.get("taskId").and_then(|value| value.as_str());
                    let draft_id = params.get("draftId").and_then(|value| value.as_str());
                    let draft = self
                        .drafts
                        .lock()
                        .unwrap()
                        .values()
                        .filter(|draft| {
                            draft_id.is_some_and(|id| draft.draft_id == id)
                                || (draft_id.is_none() && draft.task_id.as_deref() == task_id)
                        })
                        .max_by_key(|draft| draft.revision)
                        .cloned();
                    RpcReply::value(&ComposerDraftRead {
                        draft,
                        submissions: Vec::new(),
                    })
                }
                methods::SAVE_COMPOSER_DRAFT => {
                    let draft: ComposerDraft = serde_json::from_value(
                        params
                            .get("draft")
                            .cloned()
                            .ok_or_else(|| RpcError::BadParams("missing draft".into()))?,
                    )
                    .map_err(|error| RpcError::BadParams(error.to_string()))?;
                    let outcome = {
                        let mut drafts = self.drafts.lock().unwrap();
                        match drafts.get(&draft.draft_id) {
                            Some(current) if current.revision > draft.revision => {
                                ComposerDraftSaveOutcome::Stale {
                                    current_revision: current.revision,
                                }
                            }
                            Some(current) if current == &draft => {
                                ComposerDraftSaveOutcome::Unchanged
                            }
                            _ => {
                                drafts.insert(draft.draft_id.clone(), draft);
                                ComposerDraftSaveOutcome::Saved
                            }
                        }
                    };
                    RpcReply::value(&outcome)
                }
                methods::GET_TASK_COMMAND_RECEIPT => {
                    RpcReply::value(&Option::<luna_proto::CommandReceipt>::None)
                }
                other => Err(RpcError::UnknownMethod(other.into())),
            }
        }
    }

    fn shell_draft(
        draft_id: &str,
        task_id: Option<&str>,
        revision: u64,
        text: &str,
    ) -> ComposerDraft {
        ComposerDraft {
            version: luna_proto::COMPOSER_DRAFT_VERSION,
            draft_id: draft_id.into(),
            task_id: task_id.map(str::to_string),
            revision,
            text: text.into(),
            attachments: Vec::new(),
            appshots: Vec::new(),
            context: DraftContext::default(),
            execution: serde_json::Value::Null,
            updated_at_ms: revision as i64,
        }
    }

    fn handoff_chat(id: &str) -> luna_proto::Chat {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "deviceId": "local",
            "title": "Unsupported synthetic task",
            "archived": false,
            "cwd": "/tmp",
            "config": {
                "harness": "claude-code",
                "model": "synthetic-l13",
                "sandbox": "workspace-write",
                "permissionMode": "ask",
                "effectivePermissionMode": "ask"
            },
            "createdAt": Utc::now(),
        }))
        .unwrap()
    }

    fn palette_chat(id: &str, project_id: Option<&str>, title: &str) -> luna_proto::Chat {
        let mut chat = handoff_chat(id);
        chat.space_id = project_id.map(str::to_string);
        chat.title = Some(title.into());
        chat
    }

    fn command_palette_test_window(
        cx: &mut TestAppContext,
        data_dir: &Path,
        service: Option<Arc<dyn RpcService>>,
        chats: Vec<luna_proto::Chat>,
        selected_chat: Option<&str>,
    ) -> gpui::WindowHandle<Shell> {
        let data_dir = data_dir.to_path_buf();
        let selected_chat = selected_chat.map(str::to_string);
        cx.add_window(move |_, cx| {
            let state = cx.new(move |_| {
                let mut state = AppState::new();
                if let Some(service) = service {
                    state.set_test_engine(crate::state::EngineHandle::from_test_client(
                        luna_rpc::memory_client(service),
                    ));
                }
                state.spaces = vec![luna_proto::Space {
                    execution_defaults: Default::default(),
                    id: "project-a".into(),
                    device_id: "local".into(),
                    path: "/synthetic/project-a".into(),
                    name: Some("Alpha".into()),
                    git_detected: false,
                    git_checked_at: None,
                    checkout_id: None,
                    created_at: Utc::now(),
                }];
                state.chats = chats;
                state.selected_chat = selected_chat;
                state.selected_space = Some("project-a".into());
                state
            });
            Shell::new(
                state,
                EngineBootConfig {
                    data_dir,
                    ipc_port: 0,
                    edge_url: "http://127.0.0.1:1".into(),
                    edge_token: None,
                    org_id: None,
                    workos_client_id: None,
                    default_harness: luna_proto::HarnessId::Mock,
                },
                cx,
            )
        })
    }

    fn handoff_transcript() -> Vec<luna_doc::SessionMessageEntry> {
        [
            (
                "handoff-user",
                luna_doc::MessageRole::User,
                "Visible user context",
            ),
            (
                "handoff-assistant",
                luna_doc::MessageRole::Assistant,
                "Visible assistant context",
            ),
        ]
        .into_iter()
        .map(|(id, role, text)| luna_doc::SessionMessageEntry {
            id: id.into(),
            role,
            parts: vec![luna_doc::MessagePart::Text {
                id: "text".into(),
                text: text.into(),
            }],
            created_at: 1,
            device_id: "local".into(),
            status: Some(luna_doc::MessageStatus::Complete),
            continuation_of: None,
        })
        .collect()
    }

    fn init_handoff_shell_test(cx: &mut TestAppContext, data_dir: &Path) {
        cx.update(|cx| {
            settings::init(settings::UiSettings::default(), data_dir, cx);
            crate::history::init(
                Default::default(),
                Default::default(),
                Default::default(),
                Default::default(),
                cx,
            );
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
        });
    }

    #[gpui::test]
    fn sidebar_usage_refresh_bridges_to_tokio_without_a_gpui_reactor(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        init_handoff_shell_test(cx, dir.path());
        cx.update(|cx| gpui_tokio::init_from_handle(cx, runtime.handle().clone()));
        let service = Arc::new(UsageRefreshRpc {
            reads: AtomicUsize::new(0),
        });
        // memory_client sets up Tokio channels. Only that construction needs
        // an enter guard; the sidebar action below deliberately has none.
        let window = {
            let _entered = runtime.enter();
            command_palette_test_window(cx, dir.path(), Some(service.clone()), vec![], None)
        };
        window
            .update(cx, |shell, _, _| shell.usage_card_open = false)
            .unwrap();
        window
            .update(cx, |shell, _, cx| shell.toggle_usage_card(cx))
            .unwrap();
        for _ in 0..150 {
            cx.run_until_parked();
            runtime.block_on(tokio::task::yield_now());
            if window
                .read_with(cx, |shell, _| {
                    shell.usage_card_open
                        && !shell.usage_refreshing
                        && shell.usage_snapshot.is_some()
                })
                .unwrap()
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        window
            .read_with(cx, |shell, _| {
                assert!(shell.usage_card_open);
                assert!(!shell.usage_refreshing);
                assert!(shell.usage_snapshot.is_some());
                assert!(shell.usage_error.is_none());
            })
            .unwrap();
        assert_eq!(service.reads.load(Ordering::SeqCst), 1);
    }

    fn handoff_shell(
        cx: &mut TestAppContext,
        data_dir: &Path,
        selected: Option<&str>,
        service: Arc<ShellDraftRpc>,
    ) -> gpui::WindowHandle<Shell> {
        cx.add_window(|_, cx| {
            let state = cx.new(|_| {
                let mut state = AppState::new();
                state.set_test_engine(crate::state::EngineHandle::from_test_client(
                    luna_rpc::memory_client(service),
                ));
                state.chats = vec![handoff_chat(HANDOFF_SOURCE), handoff_chat("other")];
                state.selected_chat = selected.map(str::to_string);
                state.transcript = selected
                    .is_some_and(|id| id == HANDOFF_SOURCE)
                    .then(handoff_transcript)
                    .unwrap_or_default();
                state
            });
            Shell::new(
                state,
                EngineBootConfig {
                    data_dir: data_dir.into(),
                    ipc_port: 0,
                    edge_url: "http://127.0.0.1:1".into(),
                    edge_token: None,
                    org_id: None,
                    workos_client_id: None,
                    default_harness: luna_proto::HarnessId::Mock,
                },
                cx,
            )
        })
    }

    fn pump_shell_until(
        cx: &mut TestAppContext,
        runtime: &tokio::runtime::Runtime,
        mut done: impl FnMut(&mut TestAppContext) -> bool,
    ) {
        for _ in 0..100 {
            cx.run_until_parked();
            runtime.block_on(tokio::task::yield_now());
            if done(cx) {
                return;
            }
        }
        assert!(done(cx), "shell draft fixture did not settle");
    }

    fn pump_history_until(
        cx: &mut TestAppContext,
        runtime: &tokio::runtime::Runtime,
        mut done: impl FnMut(&mut TestAppContext) -> bool,
    ) {
        for _ in 0..250 {
            cx.executor().advance_clock(Duration::from_millis(10));
            cx.run_until_parked();
            runtime.block_on(tokio::task::yield_now());
            if done(cx) {
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(done(cx), "session history fixture did not settle");
    }

    // Failure-first retained-Shell coverage; see the matrix in
    // docs/qa/sidebar-notice-ownership-2026-09-26.md. This is a memory RPC
    // boundary fixture, not a second implementation of the release callback.
    mod sidebar_notice_regressions {
        use super::*;
        use crate::shell::side_chats::SideChatEvent;
        use serde_json::{Value, json};

        const DRAFT: &str = "Existing parent draft — keep this.";
        const RESULT: &str = "SIDEBAR_NOTICE_HANDOFF_RESULT";
        const CONFIRMATION: &str = "Added to the parent draft. Review it and press Send.";
        const PROGRESS: &str = "Stopping side chat and saving its transcript…";
        const NEW_ERROR: &str = "A newer operation failed; keep this error visible.";

        struct HeldUnloadRpc {
            drafts: ShellDraftRpc,
            calls: Mutex<Vec<(String, Value)>>,
            lifecycle: Mutex<Vec<Value>>,
            gates: Mutex<HashMap<String, tokio::sync::oneshot::Sender<Result<(), RpcError>>>>,
            created: AtomicUsize,
            returned: AtomicUsize,
        }

        impl HeldUnloadRpc {
            fn new() -> Self {
                Self {
                    drafts: ShellDraftRpc::new(vec![shell_draft(
                        HANDOFF_SOURCE,
                        Some(HANDOFF_SOURCE),
                        7,
                        DRAFT,
                    )]),
                    calls: Mutex::new(Vec::new()),
                    lifecycle: Mutex::new(Vec::new()),
                    gates: Mutex::new(HashMap::new()),
                    created: AtomicUsize::new(0),
                    returned: AtomicUsize::new(0),
                }
            }

            fn meta(id: &str) -> Value {
                json!({
                    "id": id, "parentId": HANDOFF_SOURCE, "title": "Held unload fixture",
                    "createdAtMs": 1, "expiresAtMs": i64::MAX, "state": "active",
                    "detail": null, "messageCount": 0,
                    "config": {"harness": "mock", "model": "synthetic-notice", "sandbox": "read-only"},
                    "contextMode": "snapshot", "originMessageId": "handoff-assistant",
                    "anchorText": "Visible assistant context"
                })
            }

            fn release(&self, id: &str, fail: bool) {
                let sender = self.gates.lock().unwrap().remove(id).expect("held unload");
                sender
                    .send(if fail {
                        Err(RpcError::Failed("fixture unload failed".into()))
                    } else {
                        Ok(())
                    })
                    .expect("production release is still awaiting its RPC reply");
            }

            fn record_lifecycle(&self, method: &str, phase: &str, detail: Value) {
                let mut lifecycle = self.lifecycle.lock().unwrap();
                if lifecycle.len() < 128 {
                    let sequence = lifecycle.len();
                    lifecycle.push(json!({
                        "sequence": sequence, "method": method, "phase": phase, "detail": detail
                    }));
                }
            }
        }

        #[async_trait]
        impl RpcService for HeldUnloadRpc {
            async fn handle(&self, method: &str, params: Value) -> Result<RpcReply, RpcError> {
                self.calls
                    .lock()
                    .unwrap()
                    .push((method.into(), params.clone()));
                self.record_lifecycle(
                    method,
                    "received",
                    json!({
                        "params_preview": params.to_string().chars().take(1024).collect::<String>()
                    }),
                );
                // Preserve dispatch and reply semantics, including error
                // returns, while recording the fixture's RPC lifecycle.
                let result = async {
                    match method {
                        methods::CREATE_SIDE_CHAT => {
                            assert_eq!(params["parentId"], HANDOFF_SOURCE);
                            let n = self.created.fetch_add(1, Ordering::SeqCst) + 1;
                            RpcReply::value(&json!({
                                "meta": Self::meta(&format!("held-side-{n}")),
                                "lease": format!("held-lease-{n}")
                            }))
                        }
                        methods::READ_SIDE_CHAT => RpcReply::value(&json!({
                            "meta": Self::meta(params["id"].as_str().expect("side id")),
                            "entries": [], "before": null, "working": false, "status": null
                        })),
                        methods::LIST_SIDE_CHATS => {
                            RpcReply::value(&json!({"items": [], "before": null}))
                        }
                        methods::UNLOAD_SIDE_CHAT => {
                            let id = params["id"].as_str().expect("unload id").to_owned();
                            let n = id.strip_prefix("held-side-").expect("created side id");
                            assert_eq!(params["lease"], format!("held-lease-{n}"));
                            let (sender, receiver) = tokio::sync::oneshot::channel();
                            assert!(self.gates.lock().unwrap().insert(id, sender).is_none());
                            let result =
                                receiver.await.expect("test must release every held reply");
                            self.returned.fetch_add(1, Ordering::SeqCst);
                            result?;
                            RpcReply::value(&json!({"unloaded": true}))
                        }
                        // Only these two fixture writes are allowed. Any accidental
                        // command/queue/provider call remains recorded and fails.
                        methods::GET_COMPOSER_DRAFT
                        | methods::SAVE_COMPOSER_DRAFT
                        | methods::GET_TASK_COMMAND_RECEIPT => {
                            self.drafts.handle(method, params).await
                        }
                        _ => Err(RpcError::UnknownMethod(method.into())),
                    }
                }
                .await;
                let summary = match &result {
                    Ok(RpcReply::Value(value)) => json!({
                        "kind": "value", "preview": value.to_string().chars().take(1024).collect::<String>()
                    }),
                    Ok(RpcReply::Stream(_)) => json!({"kind": "stream"}),
                    Err(error) => {
                        json!({"kind": "error", "message": error.to_string().chars().take(1024).collect::<String>()})
                    }
                };
                self.record_lifecycle(method, "returned", summary);
                result
            }
        }

        #[derive(Clone, Copy, Debug)]
        enum Newer {
            None,
            Handoff,
            Error,
            Dismiss,
            IdenticalProgress,
        }

        fn notice(window: &gpui::WindowHandle<Shell>, cx: &mut TestAppContext) -> Option<String> {
            window
                .read_with(cx, |shell, _| {
                    shell.sidebar_notice.as_ref().map(ToString::to_string)
                })
                .unwrap()
        }

        fn setup_failure_readback(
            case_stage: (&str, &str),
            polls: usize,
            window: &gpui::WindowHandle<Shell>,
            cx: &mut TestAppContext,
            service: &HeldUnloadRpc,
        ) {
            let (case, stage) = case_stage;
            let ui = window.read_with(cx, |shell, cx| {
                let state = shell.state.read(cx);
                let composer = shell.composer.read(cx);
                json!({
                    "active_chat": shell.active_chat, "selected_chat": state.selected_chat,
                    "engine_present": state.engine().is_some(), "engine_generation": state.engine_generation(),
                    "composer_owner": composer.current_key,
                    "composer_text": composer.input.read(cx).text().chars().take(1024).collect::<String>(),
                    "composer_failure": composer.failure.as_ref().map(|text| text.chars().take(1024).collect::<String>()),
                    "notice": shell.sidebar_notice.as_ref().map(|text| text.chars().take(1024).collect::<String>()),
                    "panel_present": shell.side_chat_panel.is_some(),
                    "release_request": shell.side_chat_panel.as_ref().and_then(|panel|
                        panel.read(cx).release_request().map(|(_, id, lease)| json!({"id":id,"lease":lease}))),
                    "parent_transcript_entries": state.transcript.len()
                })
            }).unwrap();
            let calls = service.calls.lock().unwrap();
            let mut gates = service
                .gates
                .lock()
                .unwrap()
                .keys()
                .cloned()
                .collect::<Vec<_>>();
            gates.sort();
            let evidence = json!({
                "case": case, "stage": stage, "classification": "fixture_wait_did_not_settle",
                "predicate_checks": polls, "ui": ui,
                "persisted_parent_draft": service.drafts.text_for_task(Some(HANDOFF_SOURCE)).map(|text| text.chars().take(1024).collect::<String>()),
                "rpc_call_count": calls.len(),
                "rpc_calls_truncated": calls.len() > 64,
                "rpc_calls": calls.iter().take(64).map(|(method,params)| json!({
                    "method":method,"params_preview":params.to_string().chars().take(1024).collect::<String>()
                })).collect::<Vec<_>>(),
                "rpc_lifecycle": service.lifecycle.lock().unwrap().clone(),
                "held_gate_ids": gates.into_iter().take(32).collect::<Vec<_>>(),
                "created_count": service.created.load(Ordering::SeqCst),
                "unload_returned_count": service.returned.load(Ordering::SeqCst),
                "ownership_assertions_reached": false
            });
            if let Some(path) = std::env::var_os("LUNA_SIDEBAR_NOTICE_EVIDENCE_DIR") {
                let path = PathBuf::from(path);
                std::fs::create_dir_all(&path).unwrap();
                let safe_stage: String = stage
                    .chars()
                    .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
                    .collect();
                std::fs::write(
                    path.join(format!("{case}.{safe_stage}.setup-failure.json")),
                    serde_json::to_vec_pretty(&evidence).unwrap(),
                )
                .unwrap();
            }
            eprintln!("SIDEBAR_NOTICE_SETUP_FAILURE case={case} stage={stage} checks={polls}");
        }

        fn wait_notice_stage(
            case_stage: (&str, &str),
            window: &gpui::WindowHandle<Shell>,
            cx: &mut TestAppContext,
            runtime: &tokio::runtime::Runtime,
            service: &HeldUnloadRpc,
            history_pump: bool,
            mut done: impl FnMut(&mut TestAppContext) -> bool,
        ) {
            // Both existing helpers recheck their unchanged predicate once in
            // the final assert (100+1 or 250+1). Capture only that failure, so
            // diagnostics add no delay, sleep, or scheduling change to a wait.
            let final_check = if history_pump { 251 } else { 101 };
            let mut polls = 0;
            let predicate = |cx: &mut TestAppContext| {
                polls += 1;
                let settled = done(cx);
                if !settled && polls == final_check {
                    setup_failure_readback(case_stage, polls, window, cx, service);
                }
                settled
            };
            if history_pump {
                pump_history_until(cx, runtime, predicate);
            } else {
                pump_shell_until(cx, runtime, predicate);
            }
        }

        fn begin_release(
            case: &str,
            window: &gpui::WindowHandle<Shell>,
            cx: &mut TestAppContext,
            runtime: &tokio::runtime::Runtime,
            service: &HeldUnloadRpc,
            id: &str,
        ) {
            window
                .update(cx, |shell, _, cx| {
                    shell.create_side_chat_panel(
                        Some((
                            "handoff-assistant".into(),
                            "Visible assistant context".into(),
                        )),
                        cx,
                    );
                })
                .unwrap();
            wait_notice_stage(
                (case, &format!("create-read-lease:{id}")),
                window,
                cx,
                runtime,
                service,
                false,
                |cx| {
                    window
                        .read_with(cx, |shell, cx| {
                            shell.side_chat_panel.as_ref().is_some_and(|panel| {
                                panel
                                    .read(cx)
                                    .release_request()
                                    .is_some_and(|(_, actual, _)| actual == id)
                            })
                        })
                        .unwrap()
                },
            );
            window
                .update(cx, |shell, _, cx| shell.release_side_chat(cx))
                .unwrap();
            wait_notice_stage(
                (case, &format!("unload-request-held:{id}")),
                window,
                cx,
                runtime,
                service,
                false,
                |_| service.gates.lock().unwrap().contains_key(id),
            );
            assert_eq!(notice(window, cx).as_deref(), Some(PROGRESS));
            assert!(
                window
                    .read_with(cx, |shell, _| shell.side_chat_panel.is_none())
                    .unwrap()
            );
        }

        fn finish_release(
            case_window: (&str, &gpui::WindowHandle<Shell>),
            cx: &mut TestAppContext,
            runtime: &tokio::runtime::Runtime,
            service: &HeldUnloadRpc,
            id: &str,
            fail: bool,
            returned: usize,
        ) {
            service.release(id, fail);
            wait_notice_stage(
                (case_window.0, &format!("unload-server-return:{id}")),
                case_window.1,
                cx,
                runtime,
                service,
                false,
                |_| service.returned.load(Ordering::SeqCst) == returned,
            );
            // RPC return precedes transport delivery and the GPUI callback.
            // Drain both executors; do not assert merely on the server return
            // counter or advance the 20-second production timeout as a proxy.
            for _ in 0..100 {
                runtime.block_on(tokio::task::yield_now());
                cx.run_until_parked();
            }
        }

        fn run_case(name: &str, newer: Newer, fail: bool, cx: &mut TestAppContext) {
            let dir = tempfile::tempdir().unwrap();
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            init_handoff_shell_test(cx, dir.path());
            cx.update(|cx| gpui_tokio::init_from_handle(cx, runtime.handle().clone()));
            let service = Arc::new(HeldUnloadRpc::new());
            let window = {
                let _entered = runtime.enter();
                command_palette_test_window(
                    cx,
                    dir.path(),
                    Some(service.clone()),
                    vec![palette_chat(
                        HANDOFF_SOURCE,
                        Some("project-a"),
                        "Notice fixture",
                    )],
                    Some(HANDOFF_SOURCE),
                )
            };
            window
                .update(cx, |shell, _, cx| {
                    shell.active_chat = HANDOFF_SOURCE.into();
                    shell.state.update(cx, |state, cx| {
                        state.transcript = handoff_transcript();
                        cx.notify();
                    });
                })
                .unwrap();
            wait_notice_stage(
                (name, "initial-draft-restore"),
                &window,
                cx,
                &runtime,
                &service,
                false,
                |cx| shell_composer_snapshot(cx, &window).1 == DRAFT,
            );
            begin_release(name, &window, cx, &runtime, &service, "held-side-1");
            let mut events =
                vec![json!({"event": "release_a_held", "notice": notice(&window, cx)})];

            match newer {
                Newer::None => {}
                Newer::Handoff => {
                    window
                        .update(cx, |shell, _, cx| {
                            shell.create_side_chat_panel(None, cx);
                            let generation = shell.state.read(cx).engine_generation();
                            shell.side_chat_panel.as_ref().unwrap().update(cx, |_, cx| {
                                cx.emit(SideChatEvent::ResultToParent {
                                    parent: HANDOFF_SOURCE.into(),
                                    engine_generation: generation,
                                    text: RESULT.into(),
                                });
                            });
                        })
                        .unwrap();
                    wait_notice_stage(
                        (name, "handoff-durable-draft"),
                        &window,
                        cx,
                        &runtime,
                        &service,
                        true,
                        |cx| {
                            notice(&window, cx).as_deref() == Some(CONFIRMATION)
                                && service
                                    .drafts
                                    .text_for_task(Some(HANDOFF_SOURCE))
                                    .as_deref()
                                    == Some(format!("{DRAFT}\n\n{RESULT}").as_str())
                        },
                    );
                }
                Newer::Error | Newer::Dismiss => {
                    window
                        .update(cx, |shell, _, cx| {
                            shell.sidebar_notice =
                                matches!(newer, Newer::Error).then(|| NEW_ERROR.into());
                            cx.notify();
                        })
                        .unwrap();
                }
                Newer::IdenticalProgress => {
                    begin_release(name, &window, cx, &runtime, &service, "held-side-2");
                }
            }
            let replacement = notice(&window, cx);
            events.push(json!({"event": "intervening_action", "action": format!("{newer:?}"), "notice": replacement}));
            finish_release(
                (name, &window),
                cx,
                &runtime,
                &service,
                "held-side-1",
                fail,
                1,
            );
            let after_a = notice(&window, cx);
            let ownership_passed = match newer {
                Newer::None if fail => after_a.as_deref().is_some_and(|text| {
                    text.starts_with("Side chat unload incomplete:")
                        && text.contains("fixture unload failed")
                }),
                Newer::None => after_a.is_none(),
                _ => after_a == replacement,
            };
            events.push(
                json!({"event": "release_a_callback_drained", "failed": fail, "notice": after_a}),
            );
            let current_b_cleared = if matches!(newer, Newer::IdenticalProgress) {
                finish_release(
                    (name, &window),
                    cx,
                    &runtime,
                    &service,
                    "held-side-2",
                    false,
                    2,
                );
                events.push(
                    json!({"event": "release_b_callback_drained", "notice": notice(&window, cx)}),
                );
                notice(&window, cx).is_none()
            } else {
                true
            };
            let expected_draft = if matches!(newer, Newer::Handoff) {
                format!("{DRAFT}\n\n{RESULT}")
            } else {
                DRAFT.into()
            };
            let draft = shell_composer_snapshot(cx, &window).1;
            let persisted = service.drafts.text_for_task(Some(HANDOFF_SOURCE));
            let parent_unchanged = window
                .read_with(cx, |shell, cx| {
                    shell.state.read(cx).transcript == handoff_transcript()
                })
                .unwrap();
            let calls = service.calls.lock().unwrap().clone();
            let forbidden = calls
                .iter()
                .filter(|(method, _)| {
                    !matches!(
                        method.as_str(),
                        methods::CREATE_SIDE_CHAT
                            | methods::READ_SIDE_CHAT
                            | methods::LIST_SIDE_CHATS
                            | methods::UNLOAD_SIDE_CHAT
                            | methods::SAVE_COMPOSER_DRAFT
                    ) && (method.starts_with("Send")
                        || method.starts_with("Submit")
                        || method.starts_with("Queue")
                        || method.starts_with("Run")
                        || method.starts_with("Execute")
                        || method.starts_with("Enqueue")
                        || method.contains("QueuedMessage")
                        || method == methods::RELAY_COMMAND
                        || method == methods::RETRY_DELIVERY
                        || method == methods::MUTATE)
                })
                .collect::<Vec<_>>();
            let passed = ownership_passed
                && current_b_cleared
                && draft == expected_draft
                && persisted.as_deref() == Some(expected_draft.as_str())
                && parent_unchanged
                && forbidden.is_empty()
                && service.gates.lock().unwrap().is_empty();
            let evidence = json!({
                "case": name, "passed": passed, "events": events,
                "ownership_passed": ownership_passed, "current_b_cleared": current_b_cleared,
                "draft": draft, "persisted_draft": persisted, "expected_draft": expected_draft,
                "parent_transcript_unchanged": parent_unchanged,
                "forbidden_mutations": forbidden, "rpc_calls": calls,
                "real_provider_calls": 0, "native_theme_verification": "pending separate grant"
            });
            if let Some(path) = std::env::var_os("LUNA_SIDEBAR_NOTICE_EVIDENCE_DIR") {
                let path = PathBuf::from(path);
                std::fs::create_dir_all(&path).unwrap();
                std::fs::write(
                    path.join(format!("{name}.json")),
                    serde_json::to_vec_pretty(&evidence).unwrap(),
                )
                .unwrap();
            }
            assert!(passed, "held-unload retained-Shell case: {evidence}");
        }

        macro_rules! notice_case {
            ($name:ident, $newer:ident, $fail:expr) => {
                #[gpui::test]
                fn $name(cx: &mut TestAppContext) {
                    run_case(stringify!($name), Newer::$newer, $fail, cx);
                }
            };
        }

        notice_case!(newer_handoff_survives_old_success, Handoff, false);
        notice_case!(newer_handoff_survives_old_failure, Handoff, true);
        notice_case!(newer_error_survives_old_success, Error, false);
        notice_case!(newer_error_survives_old_failure, Error, true);
        notice_case!(dismissal_survives_old_success, Dismiss, false);
        notice_case!(dismissal_survives_old_failure, Dismiss, true);
        notice_case!(
            identical_new_progress_survives_old_success,
            IdenticalProgress,
            false
        );
        notice_case!(
            identical_new_progress_survives_old_failure,
            IdenticalProgress,
            true
        );
        notice_case!(current_release_success_clears_own_progress, None, false);
        notice_case!(current_release_failure_remains_visible, None, true);
    }

    fn set_palette_query(window: &gpui::WindowHandle<Shell>, query: &str, cx: &mut TestAppContext) {
        let search = window
            .read_with(cx, |shell, _| {
                shell.command_palette.as_ref().unwrap().search.clone()
            })
            .unwrap();
        search.update(cx, |input, cx| input.set_text(query, cx));
    }

    fn palette_key(window: &gpui::WindowHandle<Shell>, key: &str, cx: &mut TestAppContext) {
        let event = gpui::KeyDownEvent {
            keystroke: gpui::Keystroke::parse(key).unwrap(),
            is_held: false,
            prefer_character_input: false,
        };
        window
            .update(cx, |shell, window, cx| {
                assert!(shell.command_palette_key_down(&event, window, cx));
            })
            .unwrap();
    }

    #[gpui::test]
    fn command_palette_groups_actions_chats_and_content_and_restores_focus(
        cx: &mut TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _guard = runtime.enter();
        init_handoff_shell_test(cx, dir.path());
        cx.update(|cx| {
            cx.bind_keys([KeyBinding::new(
                &platform_combo("mod-k"),
                ToggleCommandPalette,
                None,
            )]);
        });
        let service = Arc::new(HistorySearchRpc::new());
        let window = cx.add_window(|_, cx| {
            let state = cx.new(|_| {
                let mut state = AppState::new();
                state.set_test_engine(crate::state::EngineHandle::from_test_client(
                    luna_rpc::memory_client(service),
                ));
                state.spaces = ["project-a", "project-b"]
                    .into_iter()
                    .map(|id| luna_proto::Space {
                        execution_defaults: Default::default(),
                        id: id.into(),
                        device_id: "local".into(),
                        path: format!("/synthetic/{id}"),
                        name: Some(if id == "project-a" { "Alpha" } else { "Beta" }.into()),
                        git_detected: false,
                        git_checked_at: None,
                        checkout_id: None,
                        created_at: Utc::now(),
                    })
                    .collect();
                state.chats = vec![
                    palette_chat(
                        "project-a-task",
                        Some("project-a"),
                        "New cache investigation",
                    ),
                    palette_chat(
                        "project-b-task",
                        Some("project-b"),
                        "New cache investigation",
                    ),
                    palette_chat("personal-task", None, "New personal notes"),
                ];
                state.selected_chat = Some("project-b-task".into());
                state.selected_space = Some("project-a".into());
                state
            });
            let mut shell = Shell::new(
                state,
                EngineBootConfig {
                    data_dir: dir.path().into(),
                    ipc_port: 0,
                    edge_url: "http://127.0.0.1:1".into(),
                    edge_token: None,
                    org_id: None,
                    workos_client_id: None,
                    default_harness: luna_proto::HarnessId::Mock,
                },
                cx,
            );
            shell.debug_gate = Some(GatePhase::Ready);
            shell.splash = SplashPhase::Gone;
            shell
        });
        window
            .update(cx, |shell, window, cx| {
                window.focus(&shell.composer.focus_handle(cx), cx);
            })
            .unwrap();
        cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear())
            .unwrap();

        cx.simulate_keystrokes(window.into(), &platform_combo("mod-k"));
        set_palette_query(&window, "new", cx);
        window
            .update(cx, |shell, _, cx| {
                shell.history_search_task = None;
                shell.history_search_loading = false;
                shell.history_search_error = None;
                shell.history_search_results = vec![HistorySearchRpc::event("project-a", 0)];
                shell.history_search_next_cursor = Some(luna_proto::HistoryCursor {
                    timestamp_ms: 1,
                    sequence: 0,
                    event_id: "cursor".into(),
                });
                let entries = shell.command_entries(cx);
                assert!(matches!(entries[0], command_palette::Entry::NewChat));
                assert!(matches!(entries[1], command_palette::Entry::NewProject));
                assert!(matches!(
                    entries[2],
                    command_palette::Entry::SearchAllContent
                ));
                assert!(entries.iter().any(|entry| matches!(
                    entry,
                    command_palette::Entry::History {
                        group: command_palette::ResultGroup::CurrentContent,
                        ..
                    }
                )));
                assert!(!entries.iter().any(|entry| matches!(
                    entry,
                    command_palette::Entry::Chat { id, .. } if id == "project-a-task"
                )));
                assert!(entries.iter().any(|entry| matches!(
                    entry,
                    command_palette::Entry::Chat {
                        id,
                        group: command_palette::ResultGroup::OtherChats,
                    } if id == "project-b-task"
                )));

                shell.settings.space_filter = Some("project-a".into());
                shell.history_search_results = vec![HistorySearchRpc::event("project-a", 0)];
                shell.command_palette.as_mut().unwrap().active = 0;
            })
            .unwrap();
        cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear())
            .unwrap();
        window
            .update(cx, |shell, window, cx| {
                let search = shell.command_palette.as_ref().unwrap().search.clone();
                window.focus(&search.focus_handle(cx), cx);
            })
            .unwrap();

        palette_key(&window, "down", cx);
        window
            .read_with(cx, |shell, _| {
                assert_eq!(shell.command_palette.as_ref().unwrap().active, 1)
            })
            .unwrap();
        palette_key(&window, "down", cx);
        palette_key(&window, "enter", cx);
        window
            .read_with(cx, |shell, _| {
                assert!(shell.history_search_global);
                assert!(shell.command_palette.is_some());
            })
            .unwrap();
        palette_key(&window, "escape", cx);
        window
            .update(cx, |shell, window, cx| {
                assert!(shell.command_palette.is_none());
                assert!(shell.composer.focus_handle(cx).is_focused(window));
            })
            .unwrap();

        cx.simulate_keystrokes(window.into(), &platform_combo("mod-k"));
        set_palette_query(&window, "new", cx);
        let history_index = window
            .update(cx, |shell, _, cx| {
                shell.history_search_task = None;
                shell.history_search_loading = false;
                shell.history_search_results = vec![HistorySearchRpc::event("project-a", 0)];
                let entries = shell.command_entries(cx);
                entries
                    .iter()
                    .position(|entry| matches!(entry, command_palette::Entry::History { .. }))
                    .unwrap()
            })
            .unwrap();
        cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear())
            .unwrap();
        for _ in 0..history_index {
            palette_key(&window, "down", cx);
        }
        palette_key(&window, "enter", cx);
        window
            .read_with(cx, |shell, cx| {
                assert!(shell.command_palette.is_none());
                assert_eq!(
                    shell.state.read(cx).selected_chat.as_deref(),
                    Some("project-a-task")
                );
            })
            .unwrap();
    }

    #[gpui::test]
    fn command_palette_default_enter_opens_current_content_without_widening(
        cx: &mut TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _guard = runtime.enter();
        init_handoff_shell_test(cx, dir.path());
        let service = Arc::new(PaletteReplacementRpc::new());
        let window = command_palette_test_window(
            cx,
            dir.path(),
            Some(service.clone()),
            vec![palette_chat(
                "project-a-task",
                Some("project-a"),
                "Alpha task",
            )],
            None,
        );
        window
            .update(cx, |shell, window, cx| {
                shell.toggle_command_palette(window, cx)
            })
            .unwrap();
        set_palette_query(&window, "lunar cache", cx);
        pump_history_until(cx, &runtime, |cx| {
            cx.update(|cx| {
                window.read(cx).is_ok_and(|shell| {
                    shell
                        .history_search_results
                        .first()
                        .is_some_and(|event| event.summary == "lunar cache project-a")
                })
            })
        });

        palette_key(&window, "enter", cx);
        window
            .read_with(cx, |shell, cx| {
                assert!(!shell.history_search_global);
                assert!(shell.command_palette.is_none());
                assert_eq!(
                    shell.state.read(cx).selected_chat.as_deref(),
                    Some("project-a-task")
                );
            })
            .unwrap();
    }

    #[gpui::test]
    fn command_palette_default_enter_is_safe_and_scope_requires_explicit_navigation(
        cx: &mut TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _guard = runtime.enter();
        init_handoff_shell_test(cx, dir.path());

        let pending_service = Arc::new(PaletteReplacementRpc::new());
        let pending = command_palette_test_window(
            cx,
            dir.path(),
            Some(pending_service.clone()),
            Vec::new(),
            None,
        );
        pending
            .update(cx, |shell, window, cx| {
                shell.toggle_command_palette(window, cx)
            })
            .unwrap();
        set_palette_query(&pending, "held", cx);
        pump_history_until(cx, &runtime, |_| {
            pending_service
                .calls
                .lock()
                .unwrap()
                .iter()
                .any(|request| request.query == "held")
        });
        palette_key(&pending, "enter", cx);
        pending
            .read_with(cx, |shell, _| {
                assert!(!shell.history_search_global);
                assert!(shell.command_palette.is_some());
                assert!(shell.history_search_results.is_empty());
                assert!(shell.history_search_loading);
            })
            .unwrap();

        let empty_service = Arc::new(PaletteReplacementRpc::new());
        let empty = command_palette_test_window(
            cx,
            dir.path(),
            Some(empty_service.clone()),
            Vec::new(),
            None,
        );
        empty
            .update(cx, |shell, window, cx| {
                shell.toggle_command_palette(window, cx)
            })
            .unwrap();
        set_palette_query(&empty, "empty", cx);
        pump_history_until(cx, &runtime, |cx| {
            cx.update(|cx| {
                empty.read(cx).is_ok_and(|shell| {
                    !shell.history_search_loading
                        && empty_service
                            .calls
                            .lock()
                            .unwrap()
                            .iter()
                            .any(|request| request.query == "empty")
                })
            })
        });
        palette_key(&empty, "enter", cx);
        empty
            .read_with(cx, |shell, _| {
                assert!(!shell.history_search_global);
                assert!(shell.command_palette.is_some());
                assert!(shell.history_search_results.is_empty());
            })
            .unwrap();

        let error_service = Arc::new(PaletteReplacementRpc::new());
        let error =
            command_palette_test_window(cx, dir.path(), Some(error_service), Vec::new(), None);
        error
            .update(cx, |shell, window, cx| {
                shell.toggle_command_palette(window, cx)
            })
            .unwrap();
        set_palette_query(&error, "error", cx);
        pump_history_until(cx, &runtime, |cx| {
            cx.update(|cx| {
                error
                    .read(cx)
                    .is_ok_and(|shell| shell.history_search_error.is_some())
            })
        });
        palette_key(&error, "enter", cx);
        error
            .read_with(cx, |shell, _| {
                assert!(!shell.history_search_global);
                assert!(shell.command_palette.is_some());
                assert!(shell.history_search_results.is_empty());
            })
            .unwrap();

        let offline = command_palette_test_window(cx, dir.path(), None, Vec::new(), None);
        offline
            .update(cx, |shell, window, cx| {
                shell.toggle_command_palette(window, cx)
            })
            .unwrap();
        set_palette_query(&offline, "offline", cx);
        cx.run_until_parked();
        palette_key(&offline, "enter", cx);
        offline
            .read_with(cx, |shell, _| {
                assert!(!shell.history_search_global);
                assert!(shell.history_search_error.is_some());
                assert!(shell.command_palette.is_some());
            })
            .unwrap();

        let action = command_palette_test_window(cx, dir.path(), None, Vec::new(), None);
        action
            .update(cx, |shell, window, cx| {
                shell.toggle_command_palette(window, cx)
            })
            .unwrap();
        set_palette_query(&action, "project", cx);
        cx.run_until_parked();
        palette_key(&action, "enter", cx);
        action
            .read_with(cx, |shell, _| {
                assert!(!shell.history_search_global);
                assert!(shell.command_palette.is_none());
                assert!(shell.add_space.is_some());
            })
            .unwrap();

        let explicit_service = Arc::new(PaletteReplacementRpc::new());
        let explicit = command_palette_test_window(
            cx,
            dir.path(),
            Some(explicit_service.clone()),
            Vec::new(),
            None,
        );
        explicit
            .update(cx, |shell, window, cx| {
                shell.toggle_command_palette(window, cx)
            })
            .unwrap();
        set_palette_query(&explicit, "held", cx);
        pump_history_until(cx, &runtime, |_| {
            explicit_service
                .calls
                .lock()
                .unwrap()
                .iter()
                .any(|request| request.query == "held")
        });
        palette_key(&explicit, "down", cx);
        explicit_service.release.notify_one();
        pump_history_until(cx, &runtime, |cx| {
            cx.update(|cx| {
                explicit
                    .read(cx)
                    .is_ok_and(|shell| shell.history_search_results.len() == 1)
            })
        });
        palette_key(&explicit, "enter", cx);
        explicit
            .read_with(cx, |shell, _| {
                assert!(shell.history_search_global);
                assert!(shell.command_palette.is_some());
            })
            .unwrap();

        pending_service.release.notify_one();
    }

    #[gpui::test]
    fn command_palette_replacement_hides_stale_content_and_keeps_append_rows(
        cx: &mut TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _guard = runtime.enter();
        init_handoff_shell_test(cx, dir.path());
        let service = Arc::new(PaletteReplacementRpc::new());
        let window = cx.add_window(|_, cx| {
            let state = cx.new(|_| {
                let mut state = AppState::new();
                state.set_test_engine(crate::state::EngineHandle::from_test_client(
                    luna_rpc::memory_client(service.clone()),
                ));
                state.spaces = vec![luna_proto::Space {
                    execution_defaults: Default::default(),
                    id: "project-a".into(),
                    device_id: "local".into(),
                    path: "/synthetic/project-a".into(),
                    name: Some("Alpha".into()),
                    git_detected: false,
                    git_checked_at: None,
                    checkout_id: None,
                    created_at: Utc::now(),
                }];
                state.selected_space = Some("project-a".into());
                state
            });
            Shell::new(
                state,
                EngineBootConfig {
                    data_dir: dir.path().into(),
                    ipc_port: 0,
                    edge_url: "http://127.0.0.1:1".into(),
                    edge_token: None,
                    org_id: None,
                    workos_client_id: None,
                    default_harness: luna_proto::HarnessId::Mock,
                },
                cx,
            )
        });
        window
            .update(cx, |shell, window, cx| {
                shell.toggle_command_palette(window, cx)
            })
            .unwrap();

        set_palette_query(&window, "ready", cx);
        pump_history_until(cx, &runtime, |cx| {
            cx.update(|cx| {
                window.read(cx).is_ok_and(|shell| {
                    shell
                        .history_search_results
                        .first()
                        .is_some_and(|event| event.summary == "ready project-a")
                })
            })
        });

        set_palette_query(&window, "held", cx);
        pump_history_until(cx, &runtime, |_| {
            service
                .calls
                .lock()
                .unwrap()
                .iter()
                .any(|request| request.query == "held")
        });
        window
            .read_with(cx, |shell, cx| {
                assert!(shell.history_search_results.is_empty());
                assert!(
                    !shell
                        .command_entries(cx)
                        .iter()
                        .any(|entry| matches!(entry, command_palette::Entry::History { .. }))
                );
            })
            .unwrap();
        service.release.notify_one();
        pump_history_until(cx, &runtime, |cx| {
            cx.update(|cx| {
                window.read(cx).is_ok_and(|shell| {
                    shell
                        .history_search_results
                        .first()
                        .is_some_and(|event| event.summary == "held project-a")
                })
            })
        });

        window
            .update(cx, |shell, _, cx| {
                shell.history_search_next_cursor = Some(luna_proto::HistoryCursor {
                    timestamp_ms: 1,
                    sequence: 0,
                    event_id: "append-cursor".into(),
                });
                shell.load_more_session_history(cx);
                assert_eq!(shell.history_search_results.len(), 1);
                assert_eq!(shell.history_search_results[0].summary, "held project-a");
            })
            .unwrap();
        pump_history_until(cx, &runtime, |_| {
            service
                .calls
                .lock()
                .unwrap()
                .iter()
                .any(|request| request.cursor.is_some())
        });
        window
            .read_with(cx, |shell, _| {
                assert_eq!(shell.history_search_results.len(), 1);
                assert_eq!(shell.history_search_results[0].summary, "held project-a");
            })
            .unwrap();
        service.release.notify_one();
        pump_history_until(cx, &runtime, |cx| {
            cx.update(|cx| {
                window
                    .read(cx)
                    .is_ok_and(|shell| !shell.history_search_loading)
            })
        });

        set_palette_query(&window, "error", cx);
        pump_history_until(cx, &runtime, |cx| {
            cx.update(|cx| {
                window
                    .read(cx)
                    .is_ok_and(|shell| shell.history_search_error.is_some())
            })
        });
        window
            .read_with(cx, |shell, cx| {
                assert!(shell.history_search_results.is_empty());
                assert!(
                    !shell
                        .command_entries(cx)
                        .iter()
                        .any(|entry| matches!(entry, command_palette::Entry::History { .. }))
                );
            })
            .unwrap();

        window
            .update(cx, |shell, _, cx| {
                shell.history_search_global = true;
                let search = shell.command_palette.as_ref().unwrap().search.clone();
                search.update(cx, |input, cx| input.set_text("scope-switch", cx));
            })
            .unwrap();
        pump_history_until(cx, &runtime, |cx| {
            cx.update(|cx| {
                window.read(cx).is_ok_and(|shell| {
                    shell
                        .history_search_results
                        .first()
                        .is_some_and(|event| event.project_id.as_deref() == Some("global"))
                })
            })
        });
        palette_key(&window, "up", cx);
        palette_key(&window, "enter", cx);
        pump_history_until(cx, &runtime, |_| {
            service.calls.lock().unwrap().iter().any(|request| {
                request.query == "scope-switch"
                    && request.scope == HistorySearchScope::SelectedProject
            })
        });
        window
            .read_with(cx, |shell, cx| {
                assert!(shell.history_search_results.is_empty());
                assert!(
                    !shell
                        .command_entries(cx)
                        .iter()
                        .any(|entry| matches!(entry, command_palette::Entry::History { .. }))
                );
            })
            .unwrap();
        service.release.notify_one();

        let offline_dir = tempfile::tempdir().unwrap();
        let offline = cx.add_window(|_, cx| {
            let state = cx.new(|_| {
                let mut state = AppState::new();
                state.selected_space = Some("project-a".into());
                state
            });
            Shell::new(
                state,
                EngineBootConfig {
                    data_dir: offline_dir.path().into(),
                    ipc_port: 0,
                    edge_url: "http://127.0.0.1:1".into(),
                    edge_token: None,
                    org_id: None,
                    workos_client_id: None,
                    default_harness: luna_proto::HarnessId::Mock,
                },
                cx,
            )
        });
        offline
            .update(cx, |shell, window, cx| {
                shell.toggle_command_palette(window, cx);
                shell.history_search_results = vec![HistorySearchRpc::event("project-a", 9)];
            })
            .unwrap();
        set_palette_query(&offline, "offline", cx);
        cx.run_until_parked();
        offline
            .read_with(cx, |shell, cx| {
                assert!(shell.history_search_error.is_some());
                assert!(shell.history_search_results.is_empty());
                assert!(
                    !shell
                        .command_entries(cx)
                        .iter()
                        .any(|entry| matches!(entry, command_palette::Entry::History { .. }))
                );
            })
            .unwrap();
    }

    #[gpui::test]
    fn command_palette_keeps_keyboard_target_stable_across_async_dedup(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _guard = runtime.enter();
        init_handoff_shell_test(cx, dir.path());
        let service = Arc::new(PaletteSelectionRpc::new());
        let window = cx.add_window(|_, cx| {
            let state = cx.new(|_| {
                let mut state = AppState::new();
                state.set_test_engine(crate::state::EngineHandle::from_test_client(
                    luna_rpc::memory_client(service.clone()),
                ));
                state.spaces = vec![luna_proto::Space {
                    execution_defaults: Default::default(),
                    id: "project-a".into(),
                    device_id: "local".into(),
                    path: "/synthetic/project-a".into(),
                    name: Some("Alpha".into()),
                    git_detected: false,
                    git_checked_at: None,
                    checkout_id: None,
                    created_at: Utc::now(),
                }];
                let mut alpha = palette_chat("stable-a", Some("project-a"), "Stable alpha");
                let mut beta = palette_chat("stable-b", Some("project-a"), "Stable beta");
                alpha.created_at = Utc::now();
                beta.created_at = alpha.created_at - chrono::Duration::seconds(1);
                state.chats = vec![alpha, beta];
                state.selected_chat = Some("stable-a".into());
                state.selected_space = Some("project-a".into());
                state
            });
            Shell::new(
                state,
                EngineBootConfig {
                    data_dir: dir.path().into(),
                    ipc_port: 0,
                    edge_url: "http://127.0.0.1:1".into(),
                    edge_token: None,
                    org_id: None,
                    workos_client_id: None,
                    default_harness: luna_proto::HarnessId::Mock,
                },
                cx,
            )
        });
        window
            .update(cx, |shell, window, cx| {
                shell.toggle_command_palette(window, cx)
            })
            .unwrap();
        set_palette_query(&window, "stable", cx);
        pump_history_until(cx, &runtime, |_| !service.calls.lock().unwrap().is_empty());
        let (default_index, target_index, entry_count) = window
            .read_with(cx, |shell, cx| {
                let entries = shell.command_entries(cx);
                let default_index = entries
                    .iter()
                    .position(|entry| {
                        matches!(
                            entry,
                            command_palette::Entry::Chat { .. }
                                | command_palette::Entry::History { .. }
                        )
                    })
                    .unwrap();
                let target_index = entries
                    .iter()
                    .position(|entry| {
                        matches!(entry, command_palette::Entry::Chat { id, .. } if id == "stable-b")
                    })
                    .unwrap();
                (default_index, target_index, entries.len())
            })
            .unwrap();
        for _ in 0..((target_index + entry_count - default_index) % entry_count) {
            palette_key(&window, "down", cx);
        }
        service.release.notify_one();
        pump_history_until(cx, &runtime, |cx| {
            cx.update(|cx| {
                window.read(cx).is_ok_and(|shell| {
                    shell
                        .history_search_results
                        .first()
                        .is_some_and(|event| event.event_id == "stable-a-event")
                })
            })
        });
        window
            .read_with(cx, |shell, cx| {
                let entries = shell.command_entries(cx);
                let new_target = entries
                    .iter()
                    .position(|entry| {
                        matches!(entry, command_palette::Entry::Chat { id, .. } if id == "stable-b")
                    })
                    .unwrap();
                assert_ne!(new_target, target_index);
                assert!(!matches!(
                    entries.get(target_index),
                    Some(command_palette::Entry::Chat { id, .. }) if id == "stable-b"
                ));
            })
            .unwrap();
        palette_key(&window, "enter", cx);
        window
            .read_with(cx, |shell, cx| {
                assert_eq!(
                    shell.state.read(cx).selected_chat.as_deref(),
                    Some("stable-b")
                );
            })
            .unwrap();
    }

    #[gpui::test]
    fn sidebar_renders_chat_navigation_without_l14_search_chrome(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        init_handoff_shell_test(cx, dir.path());
        let (shell, cx) = cx.add_window_view(|_, cx| {
            let state = cx.new(|_| {
                let mut state = AppState::new();
                state.connection = ConnectionStatus::Ready;
                state.workspace_scope = Some(WorkspaceScope::Local);
                state.auth = Some(luna_proto::AuthState::SignedOut);
                state.chats = vec![palette_chat(
                    "sidebar-chat",
                    None,
                    "Sidebar navigation remains",
                )];
                state.selected_chat = Some("sidebar-chat".into());
                state
            });
            let mut shell = Shell::new(
                state,
                EngineBootConfig {
                    data_dir: dir.path().into(),
                    ipc_port: 0,
                    edge_url: "http://127.0.0.1:1".into(),
                    edge_token: None,
                    org_id: None,
                    workos_client_id: None,
                    default_harness: luna_proto::HarnessId::Mock,
                },
                cx,
            );
            shell.debug_gate = Some(GatePhase::Ready);
            shell.splash = SplashPhase::Gone;
            shell
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.draw(cx).clear();
            window.draw(cx).clear();
        });
        shell.update(cx, |shell, cx| {
            assert!(matches!(shell.route, Route::Chat));
            assert!(shell.sidebar_now() > 0.0);
            let theme = Theme::of(cx).clone();
            let _ = shell.render_chat_sidebar(&theme, cx);
        });
        assert!(cx.debug_bounds("history-search-scope").is_none());
    }

    #[gpui::test]
    fn command_palette_rejects_delayed_query_project_and_profile_results(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _guard = runtime.enter();
        init_handoff_shell_test(cx, dir.path());
        let old_service = Arc::new(PaletteFenceRpc::new("old-engine", true));
        let new_service = Arc::new(PaletteFenceRpc::new("new-engine", false));
        let close_service = Arc::new(PaletteFenceRpc::new("close-engine", true));
        let window = cx.add_window(|_, cx| {
            let state = cx.new(|_| {
                let mut state = AppState::new();
                state.set_test_engine(crate::state::EngineHandle::from_test_client(
                    luna_rpc::memory_client(old_service.clone()),
                ));
                state.spaces = ["project-a", "project-b"]
                    .into_iter()
                    .map(|id| luna_proto::Space {
                        execution_defaults: Default::default(),
                        id: id.into(),
                        device_id: "local".into(),
                        path: format!("/synthetic/{id}"),
                        name: None,
                        git_detected: false,
                        git_checked_at: None,
                        checkout_id: None,
                        created_at: Utc::now(),
                    })
                    .collect();
                state.selected_space = Some("project-a".into());
                state
            });
            Shell::new(
                state,
                EngineBootConfig {
                    data_dir: dir.path().into(),
                    ipc_port: 0,
                    edge_url: "http://127.0.0.1:1".into(),
                    edge_token: None,
                    org_id: None,
                    workos_client_id: None,
                    default_harness: luna_proto::HarnessId::Mock,
                },
                cx,
            )
        });
        window
            .update(cx, |shell, window, cx| {
                shell.toggle_command_palette(window, cx)
            })
            .unwrap();

        set_palette_query(&window, "old-query", cx);
        pump_history_until(cx, &runtime, |_| {
            old_service
                .calls
                .lock()
                .unwrap()
                .iter()
                .any(|request| request.query == "old-query")
        });
        set_palette_query(&window, "new-query", cx);
        pump_history_until(cx, &runtime, |cx| {
            cx.update(|cx| {
                window.read(cx).is_ok_and(|shell| {
                    shell
                        .history_search_results
                        .first()
                        .is_some_and(|event| event.summary == "old-engine:new-query:project-a")
                })
            })
        });
        old_service.release.notify_one();
        runtime.block_on(tokio::task::yield_now());
        cx.run_until_parked();
        window
            .read_with(cx, |shell, _| {
                assert_eq!(
                    shell.history_search_results[0].summary,
                    "old-engine:new-query:project-a"
                )
            })
            .unwrap();

        set_palette_query(&window, "old-project", cx);
        pump_history_until(cx, &runtime, |_| {
            old_service.calls.lock().unwrap().iter().any(|request| {
                request.query == "old-project" && request.project_id.as_deref() == Some("project-a")
            })
        });
        window
            .update(cx, |shell, _, cx| {
                shell.set_space_filter(Some("project-b".into()), cx)
            })
            .unwrap();
        pump_history_until(cx, &runtime, |cx| {
            cx.update(|cx| {
                window.read(cx).is_ok_and(|shell| {
                    shell
                        .history_search_results
                        .first()
                        .is_some_and(|event| event.summary == "old-engine:old-project:project-b")
                })
            })
        });
        old_service.release.notify_one();
        runtime.block_on(tokio::task::yield_now());
        cx.run_until_parked();
        window
            .read_with(cx, |shell, _| {
                assert_eq!(
                    shell.history_search_results[0].summary,
                    "old-engine:old-project:project-b"
                )
            })
            .unwrap();

        set_palette_query(&window, "profile", cx);
        pump_history_until(cx, &runtime, |_| {
            old_service
                .calls
                .lock()
                .unwrap()
                .iter()
                .any(|request| request.query == "profile")
        });
        window
            .update(cx, |shell, _, cx| {
                shell.state.update(cx, |state, cx| {
                    state.set_test_engine(crate::state::EngineHandle::from_test_client(
                        luna_rpc::memory_client(new_service.clone()),
                    ));
                    cx.notify();
                });
            })
            .unwrap();
        pump_history_until(cx, &runtime, |cx| {
            cx.update(|cx| {
                window.read(cx).is_ok_and(|shell| {
                    shell
                        .history_search_results
                        .first()
                        .is_some_and(|event| event.summary == "new-engine:profile:project-b")
                })
            })
        });
        old_service.release.notify_one();
        runtime.block_on(tokio::task::yield_now());
        cx.run_until_parked();
        window
            .read_with(cx, |shell, _| {
                assert_eq!(
                    shell.history_search_results[0].summary,
                    "new-engine:profile:project-b"
                )
            })
            .unwrap();

        window
            .update(cx, |shell, _, cx| {
                shell.state.update(cx, |state, cx| {
                    state.set_test_engine(crate::state::EngineHandle::from_test_client(
                        luna_rpc::memory_client(close_service.clone()),
                    ));
                    cx.notify();
                });
                shell.set_space_filter(Some("project-a".into()), cx);
            })
            .unwrap();
        set_palette_query(&window, "old-close", cx);
        pump_history_until(cx, &runtime, |_| {
            close_service
                .calls
                .lock()
                .unwrap()
                .iter()
                .any(|request| request.query == "old-close")
        });
        window
            .update(cx, |shell, window, cx| {
                shell.close_command_palette(window, cx)
            })
            .unwrap();
        close_service.release.notify_waiters();
        runtime.block_on(tokio::task::yield_now());
        cx.run_until_parked();
        window
            .read_with(cx, |shell, _| {
                assert!(shell.command_palette.is_none());
                assert!(shell.history_search_results.is_empty());
                assert!(shell.history_search_next_cursor.is_none());
                assert!(!shell.history_search_loading);
            })
            .unwrap();
    }

    #[gpui::test]
    fn session_history_scope_fences_out_of_order_results_and_loads_older_pages(
        cx: &mut TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _guard = runtime.enter();
        init_handoff_shell_test(cx, dir.path());
        let service = Arc::new(HistorySearchRpc::new());
        let window = cx.add_window(|_, cx| {
            let state = cx.new(|_| {
                let mut state = AppState::new();
                state.set_test_engine(crate::state::EngineHandle::from_test_client(
                    luna_rpc::memory_client(service.clone()),
                ));
                state.spaces = ["project-a", "project-b"]
                    .into_iter()
                    .map(|id| luna_proto::Space {
                        execution_defaults: Default::default(),
                        id: id.into(),
                        device_id: "local".into(),
                        path: format!("/synthetic/{id}"),
                        name: None,
                        git_detected: false,
                        git_checked_at: None,
                        checkout_id: None,
                        created_at: Utc::now(),
                    })
                    .collect();
                let mut alpha_chat = handoff_chat("alpha-chat");
                alpha_chat.space_id = Some("project-a".into());
                let mut beta_chat = handoff_chat("beta-chat");
                beta_chat.space_id = Some("project-b".into());
                state.chats = vec![alpha_chat, beta_chat];
                state.selected_chat = Some("alpha-chat".into());
                state.selected_space = Some("project-a".into());
                state
            });
            Shell::new(
                state,
                EngineBootConfig {
                    data_dir: dir.path().into(),
                    ipc_port: 0,
                    edge_url: "http://127.0.0.1:1".into(),
                    edge_token: None,
                    org_id: None,
                    workos_client_id: None,
                    default_harness: luna_proto::HarnessId::Mock,
                },
                cx,
            )
        });

        window
            .update(cx, |shell, window, cx| {
                shell.toggle_command_palette(window, cx);
            })
            .unwrap();
        set_palette_query(&window, "needle", cx);
        pump_history_until(cx, &runtime, |_| {
            service
                .calls
                .lock()
                .unwrap()
                .iter()
                .any(|request| request.project_id.as_deref() == Some("project-a"))
        });

        window
            .update(cx, |shell, _, cx| {
                shell.set_space_filter(Some("project-b".into()), cx);
                assert_eq!(
                    shell.state.read(cx).selected_chat.as_deref(),
                    Some("alpha-chat"),
                    "the project picker must not switch or close the open task"
                );
                assert_eq!(
                    shell.state.read(cx).selected_space.as_deref(),
                    Some("project-a"),
                    "the open alpha task keeps its own project context"
                );
                assert_eq!(shell.settings.space_filter.as_deref(), Some("project-b"));
                assert!(shell.history_search_results.is_empty());
                assert!(shell.history_search_next_cursor.is_none());
            })
            .unwrap();
        pump_history_until(cx, &runtime, |cx| {
            cx.update(|cx| {
                window
                    .read(cx)
                    .is_ok_and(|shell| shell.history_search_results.len() == 50)
            })
        });
        cx.update(|cx| {
            let shell = window.read(cx).unwrap();
            assert!(
                shell
                    .history_search_results
                    .iter()
                    .all(|event| event.project_id.as_deref() == Some("project-b"))
            );
            assert!(shell.history_search_next_cursor.is_some());
        });

        service.release_project_a.notify_one();
        pump_history_until(cx, &runtime, |_| service.calls.lock().unwrap().len() >= 2);
        cx.update(|cx| {
            let shell = window.read(cx).unwrap();
            assert_eq!(shell.history_search_results.len(), 50);
            assert!(
                shell
                    .history_search_results
                    .iter()
                    .all(|event| event.project_id.as_deref() == Some("project-b"))
            );
        });

        window
            .update(cx, |shell, _, cx| {
                shell.set_space_filter(Some("project-a".into()), cx);
                assert!(shell.history_search_results.is_empty());
                assert!(
                    shell.history_search_next_cursor.is_none(),
                    "a picker scope change resets the previous project's paging cursor"
                );
            })
            .unwrap();
        pump_history_until(cx, &runtime, |_| service.calls.lock().unwrap().len() >= 3);
        service.release_project_a.notify_one();
        pump_history_until(cx, &runtime, |cx| {
            cx.update(|cx| {
                window.read(cx).is_ok_and(|shell| {
                    shell.history_search_results.len() == 1
                        && shell.history_search_results[0].project_id.as_deref()
                            == Some("project-a")
                })
            })
        });

        window
            .update(cx, |shell, _, cx| {
                shell.history_search_global = true;
                shell.begin_session_history_search(cx);
                shell.history_search_task = None;
                let request = shell.current_session_history_request(None, cx).unwrap();
                shell.request_session_history_page(request, true, false, cx);
            })
            .unwrap();
        pump_history_until(cx, &runtime, |_| {
            service
                .calls
                .lock()
                .unwrap()
                .last()
                .is_some_and(|request| request.scope == HistorySearchScope::Global)
        });
        pump_history_until(cx, &runtime, |cx| {
            cx.update(|cx| {
                window
                    .read(cx)
                    .is_ok_and(|shell| !shell.history_search_loading)
            })
        });
        cx.update(|cx| {
            let shell = window.read(cx).unwrap();
            assert_eq!(shell.history_search_results.len(), 1);
            assert_eq!(
                shell.history_search_results[0].project_id.as_deref(),
                Some("global")
            );
        });
        let global_call_count = service.calls.lock().unwrap().len();
        window
            .update(cx, |shell, _, cx| {
                shell.set_space_filter(Some("project-b".into()), cx);
                let request = shell.current_session_history_request(None, cx).unwrap();
                assert_eq!(request.scope, HistorySearchScope::Global);
                assert_eq!(shell.history_search_results.len(), 1);
            })
            .unwrap();
        runtime.block_on(async {
            tokio::time::sleep(Duration::from_millis(150)).await;
        });
        cx.run_until_parked();
        assert_eq!(
            service.calls.lock().unwrap().len(),
            global_call_count,
            "changing the project picker must not silently narrow explicit global history"
        );

        window
            .update(cx, |shell, _, cx| {
                shell.history_search_global = false;
                shell.begin_session_history_search(cx);
                shell.history_search_task = None;
                let request = shell.current_session_history_request(None, cx).unwrap();
                shell.request_session_history_page(request, true, false, cx);
            })
            .unwrap();
        pump_history_until(cx, &runtime, |cx| {
            cx.update(|cx| {
                window
                    .read(cx)
                    .is_ok_and(|shell| shell.history_search_results.len() == 50)
            })
        });
        window
            .update(cx, |shell, _, cx| shell.load_more_session_history(cx))
            .unwrap();
        pump_history_until(cx, &runtime, |cx| {
            cx.update(|cx| {
                window
                    .read(cx)
                    .is_ok_and(|shell| shell.history_search_results.len() == 60)
            })
        });
        cx.update(|cx| {
            let shell = window.read(cx).unwrap();
            assert!(shell.history_search_next_cursor.is_none());
            assert_eq!(
                shell
                    .history_search_results
                    .iter()
                    .map(|event| &event.event_id)
                    .collect::<std::collections::HashSet<_>>()
                    .len(),
                60
            );
        });

        window
            .update(cx, |shell, _, cx| {
                shell.set_space_filter(None, cx);
                let request = shell.current_session_history_request(None, cx).unwrap();
                assert_eq!(request.scope, HistorySearchScope::SelectedProject);
                assert_eq!(
                    request.project_id.as_deref(),
                    Some("project-a"),
                    "All projects in the picker keeps Project search tied to the open task"
                );
            })
            .unwrap();
        pump_history_until(cx, &runtime, |_| service.calls.lock().unwrap().len() >= 7);
        service.release_project_a.notify_one();
        pump_history_until(cx, &runtime, |cx| {
            cx.update(|cx| {
                window.read(cx).is_ok_and(|shell| {
                    shell.history_search_results.len() == 1
                        && shell.history_search_results[0].project_id.as_deref()
                            == Some("project-a")
                })
            })
        });

        set_palette_query(&window, "needle", cx);
        window
            .update(cx, |shell, _, cx| {
                shell.history_search_global = true;
            })
            .unwrap();
        set_palette_query(&window, "needle project:project-b", cx);
        window
            .update(cx, |shell, _, cx| {
                let request = shell.current_session_history_request(None, cx).unwrap();
                assert_eq!(request.scope, HistorySearchScope::SelectedProject);
                assert_eq!(request.project_id.as_deref(), Some("project-b"));
            })
            .unwrap();
    }

    fn shell_composer_snapshot(
        cx: &mut TestAppContext,
        window: &gpui::WindowHandle<Shell>,
    ) -> (String, String) {
        cx.update(|cx| {
            let shell = window.read(cx).unwrap();
            let composer = shell.composer.read(cx);
            (
                composer.current_key.clone(),
                composer.input.read(cx).text().to_string(),
            )
        })
    }

    #[gpui::test]
    fn context_handoff_follows_new_task_draft_owner_through_navigation_and_restart(
        cx: &mut TestAppContext,
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _guard = runtime.enter();
        let dir = tempfile::tempdir().unwrap();
        init_handoff_shell_test(cx, dir.path());
        let service = Arc::new(ShellDraftRpc::new(vec![
            shell_draft("source-draft", Some(HANDOFF_SOURCE), 7, "source sentinel"),
            shell_draft("canvas-draft", None, 4, "existing new-task draft"),
        ]));
        let window = handoff_shell(cx, dir.path(), Some(HANDOFF_SOURCE), service.clone());
        window
            .update(cx, |shell, _, cx| {
                shell.state.update(cx, |_, cx| cx.notify());
            })
            .unwrap();
        pump_shell_until(cx, &runtime, |cx| {
            shell_composer_snapshot(cx, &window).1 == "source sentinel"
        });

        window
            .update(cx, |shell, _, cx| {
                shell.start_new_task_with_context(HANDOFF_SOURCE, cx)
            })
            .unwrap();
        pump_shell_until(cx, &runtime, |cx| {
            let (key, text) = shell_composer_snapshot(cx, &window);
            key.is_empty()
                && text.contains("existing new-task draft")
                && text.matches(HANDOFF_MARKER).count() == 1
                && service
                    .text_for_task(None)
                    .is_some_and(|saved| saved.matches(HANDOFF_MARKER).count() == 1)
        });
        let target_text = shell_composer_snapshot(cx, &window).1;
        assert_eq!(target_text.matches(HANDOFF_MARKER).count(), 1);
        assert!(target_text.contains("Visible user context"));
        assert!(target_text.contains("Visible assistant context"));
        assert!(target_text.contains("existing new-task draft"));
        assert_eq!(
            service.text_for_task(Some(HANDOFF_SOURCE)).as_deref(),
            Some("source sentinel")
        );

        window
            .update(cx, |shell, _, cx| {
                shell.open_chat(HANDOFF_SOURCE.into(), cx)
            })
            .unwrap();
        pump_shell_until(cx, &runtime, |cx| {
            shell_composer_snapshot(cx, &window).1 == "source sentinel"
        });
        window
            .update(cx, |shell, _, cx| shell.open_new_session(cx))
            .unwrap();
        pump_shell_until(cx, &runtime, |cx| {
            shell_composer_snapshot(cx, &window).1 == target_text
        });

        let restarted = handoff_shell(cx, dir.path(), None, service.clone());
        restarted
            .update(cx, |shell, _, cx| {
                shell.state.update(cx, |_, cx| cx.notify());
            })
            .unwrap();
        pump_shell_until(cx, &runtime, |cx| {
            shell_composer_snapshot(cx, &restarted).1 == target_text
        });
        assert_eq!(
            service.text_for_task(Some(HANDOFF_SOURCE)).as_deref(),
            Some("source sentinel")
        );

        let blank_service = Arc::new(ShellDraftRpc::new(vec![shell_draft(
            "blank-source-draft",
            Some(HANDOFF_SOURCE),
            3,
            "blank source sentinel",
        )]));
        let blank = handoff_shell(cx, dir.path(), Some(HANDOFF_SOURCE), blank_service.clone());
        blank
            .update(cx, |shell, _, cx| {
                shell.state.update(cx, |_, cx| cx.notify());
            })
            .unwrap();
        pump_shell_until(cx, &runtime, |cx| {
            shell_composer_snapshot(cx, &blank).1 == "blank source sentinel"
        });
        blank
            .update(cx, |shell, _, cx| {
                shell.start_new_task_with_context(HANDOFF_SOURCE, cx)
            })
            .unwrap();
        pump_shell_until(cx, &runtime, |cx| {
            let (key, text) = shell_composer_snapshot(cx, &blank);
            key.is_empty()
                && text.matches(HANDOFF_MARKER).count() == 1
                && blank_service
                    .text_for_task(None)
                    .is_some_and(|saved| saved.matches(HANDOFF_MARKER).count() == 1)
        });
        assert_eq!(
            blank_service.text_for_task(Some(HANDOFF_SOURCE)).as_deref(),
            Some("blank source sentinel")
        );
    }

    #[gpui::test]
    fn cancelled_or_profile_replaced_handoff_never_lands_on_an_unowned_draft(
        cx: &mut TestAppContext,
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _guard = runtime.enter();
        let dir = tempfile::tempdir().unwrap();
        init_handoff_shell_test(cx, dir.path());
        let service = Arc::new(ShellDraftRpc::new(vec![
            shell_draft("source-draft", Some(HANDOFF_SOURCE), 7, "source sentinel"),
            shell_draft("canvas-draft", None, 4, "existing new-task draft"),
        ]));
        let window = handoff_shell(cx, dir.path(), Some(HANDOFF_SOURCE), service.clone());
        window
            .update(cx, |shell, _, cx| {
                shell.state.update(cx, |_, cx| cx.notify());
            })
            .unwrap();
        pump_shell_until(cx, &runtime, |cx| {
            shell_composer_snapshot(cx, &window).1 == "source sentinel"
        });

        window
            .update(cx, |shell, _, cx| {
                shell.start_new_task_with_context(HANDOFF_SOURCE, cx);
                shell.open_chat("other".into(), cx);
            })
            .unwrap();
        pump_shell_until(cx, &runtime, |cx| {
            shell_composer_snapshot(cx, &window).0 == "other"
        });
        assert_eq!(
            service.text_for_task(None).as_deref(),
            Some("existing new-task draft")
        );
        assert_eq!(
            service.text_for_task(Some(HANDOFF_SOURCE)).as_deref(),
            Some("source sentinel")
        );

        window
            .update(cx, |shell, _, cx| {
                shell.state.update(cx, |state, cx| {
                    state.selected_chat = Some(HANDOFF_SOURCE.into());
                    state.transcript = handoff_transcript();
                    cx.notify();
                });
            })
            .unwrap();
        pump_shell_until(cx, &runtime, |cx| {
            shell_composer_snapshot(cx, &window).1 == "source sentinel"
        });
        let replacement = Arc::new(ShellDraftRpc::new(vec![shell_draft(
            "replacement-canvas",
            None,
            2,
            "replacement profile canvas",
        )]));
        window
            .update(cx, |shell, _, cx| {
                shell.start_new_task_with_context(HANDOFF_SOURCE, cx);
                shell.state.update(cx, |state, cx| {
                    state.set_test_engine(crate::state::EngineHandle::from_test_client(
                        luna_rpc::memory_client(replacement.clone()),
                    ));
                    cx.notify();
                });
            })
            .unwrap();
        pump_shell_until(cx, &runtime, |cx| {
            shell_composer_snapshot(cx, &window).1 == "replacement profile canvas"
        });
        assert!(
            !shell_composer_snapshot(cx, &window)
                .1
                .contains(HANDOFF_MARKER)
        );
        assert_eq!(
            service.text_for_task(None).as_deref(),
            Some("existing new-task draft")
        );
    }

    #[gpui::test]
    fn appshot_destinations_retain_last_session_and_use_new_canvas_defaults(
        cx: &mut TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            settings::init(settings::UiSettings::default(), dir.path(), cx);
            crate::history::init(
                Default::default(),
                Default::default(),
                Default::default(),
                Default::default(),
                cx,
            );
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
        });
        let window = cx.add_window(|_, cx| {
            let state = cx.new(|_| AppState::new());
            Shell::new(
                state,
                EngineBootConfig {
                    data_dir: dir.path().into(),
                    ipc_port: 0,
                    edge_url: "http://127.0.0.1:1".into(),
                    edge_token: None,
                    org_id: None,
                    workos_client_id: None,
                    default_harness: luna_proto::HarnessId::Mock,
                },
                cx,
            )
        });
        window
            .update(cx, |shell, window, cx| {
                shell.state.update(cx, |state, cx| {
                    state.spaces = ["a", "b"]
                        .into_iter()
                        .map(|id| {
                            serde_json::from_value(serde_json::json!({
                                "id": id, "deviceId": "local", "path": "/tmp", "gitDetected": false,
                                "createdAt": Utc::now(),
                            }))
                            .unwrap()
                        })
                        .collect();
                    state.chats =
                        vec![serde_json::from_value(serde_json::json!({
                    "id": "last", "deviceId": "local", "spaceId": "a", "archived": false,
                    "createdAt": Utc::now(),
                })).unwrap()];
                    state.select_chat(Some("last".into()), cx);
                });
                shell.on_state_changed(&shell.state.clone(), cx);
                shell.settings.space_filter = Some("b".into());
                shell.open_new_session(cx);
                shell.on_state_changed(&shell.state.clone(), cx);
                assert!(shell.active_chat.is_empty());
                shell.settings.appshot_destination =
                    crate::appshots::AppshotDestination::LastSession;
                shell.receive_appshot(crate::appshots::tests::shot(), window, cx);
                assert_eq!(shell.state.read(cx).selected_chat.as_deref(), Some("last"));
                assert_eq!(shell.composer.read(cx).appshots["last"].len(), 1);
                shell.settings.appshot_destination =
                    crate::appshots::AppshotDestination::NewSession;
                shell.receive_appshot(crate::appshots::tests::shot(), window, cx);
                assert!(shell.state.read(cx).selected_chat.is_none());
                assert_eq!(shell.state.read(cx).selected_space.as_deref(), Some("b"));
                assert_eq!(shell.composer.read(cx).appshots[""].len(), 1);
            })
            .unwrap();
    }

    #[gpui::test]
    fn pane_geometry_uses_one_animation_time_per_frame(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            settings::init(settings::UiSettings::default(), dir.path(), cx);
            crate::history::init(
                Default::default(),
                Default::default(),
                Default::default(),
                Default::default(),
                cx,
            );
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
        });
        let window = cx.add_window(|_, cx| {
            let state = cx.new(|_| AppState::new());
            Shell::new(
                state,
                EngineBootConfig {
                    data_dir: dir.path().into(),
                    ipc_port: 0,
                    edge_url: "http://127.0.0.1:1".into(),
                    edge_token: None,
                    org_id: None,
                    workos_client_id: None,
                    default_harness: luna_proto::HarnessId::Mock,
                },
                cx,
            )
        });
        window
            .update(cx, |shell, window, cx| {
                let duration = RESIZE.total().mul_f32(motion::speed_scale());
                let started = std::time::Instant::now() - duration.mul_f32(2.);
                let tween = Some(WidthTween {
                    from: 520.,
                    to: 0.,
                    started,
                });
                shell.reduced_motion = false;
                // The frame started halfway through the transition but rendering
                // crossed its deadline. Every region must still use that frame.
                shell.render_time = Some(started + duration.mul_f32(0.5));
                assert!(shell.tween_active(tween));
                assert_eq!(shell.active_tween_endpoints(tween), Some((520., 0.)));
                let width = shell.eval_tween(tween, 0.);
                assert!(width > 0. && width < 520.);
                assert_eq!(shell.eval_tween(tween, 0.), width);
                shell.active_chat = "preview".into();
                shell.viewport_width = 1000.;
                shell.right_tween = tween;
                shell.toggle_right_pane(cx);
                assert_eq!(
                    shell.right_tween.unwrap().from,
                    width,
                    "reversing must not restart from zero"
                );
                let files = cx.new(|cx| {
                    FilesSurface::new(
                        shell.state.clone(),
                        "preview".into(),
                        false,
                        1000,
                        13.0,
                        false,
                        false,
                        cx,
                    )
                });
                let key = shell.panel_key(cx);
                shell.files.insert(key.clone(), files.clone());
                shell
                    .panels
                    .update(&key, |panel| panel.right_active = RightSurface::Files);
                assert!(files.read(cx).test_images_visible());
                shell.toggle_right_pane(cx);
                assert!(!shell.right_pane_open(cx));
                assert!(shell.tween_active(shell.right_tween));
                assert!(
                    !files.read(cx).test_images_visible(),
                    "closing suspends image resources immediately"
                );
                let _ = shell.render_right_pane(window, cx);
                assert!(
                    !files.read(cx).test_images_visible(),
                    "closing animation must not reactivate images"
                );
                shell.settings.sidebar_collapsed = true;
                shell.sidebar_tween = tween;
                shell.toggle_sidebar(cx);
                assert_eq!(shell.sidebar_tween.unwrap().from, width);
                shell.render_time = None;
                assert!(!shell.tween_active(tween));
                assert_eq!(shell.active_tween_endpoints(tween), None);
                assert_eq!(shell.eval_tween(tween, 0.), 0.);
            })
            .unwrap();
    }

    #[gpui::test]
    fn panel_saves_preserve_settings_selected_outside_the_shell(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
            crate::history::init(
                Default::default(),
                Default::default(),
                Default::default(),
                Default::default(),
                cx,
            );
            settings::init(settings::UiSettings::default(), dir.path(), cx);
        });
        let window = cx.add_window(|_, cx| {
            let state = cx.new(|_| AppState::new());
            Shell::new(
                state,
                EngineBootConfig {
                    data_dir: dir.path().into(),
                    ipc_port: 0,
                    edge_url: "http://127.0.0.1:1".into(),
                    edge_token: None,
                    org_id: None,
                    workos_client_id: None,
                    default_harness: luna_proto::HarnessId::Mock,
                },
                cx,
            )
        });
        for (index, effect) in settings::NewThreadBackgroundEffect::ALL
            .into_iter()
            .enumerate()
        {
            let open_links_in_luna = index % 2 == 0;
            let terminal_family = if open_links_in_luna {
                crate::typography::UiFontFamily::System
            } else {
                crate::typography::UiFontFamily::Geist
            };
            let code_family = if open_links_in_luna {
                crate::typography::UiFontFamily::Geist
            } else {
                crate::typography::UiFontFamily::System
            };
            let terminal_size = 15.0 + index as f32;
            let code_size = 11.0 + index as f32;
            let transcript_width = 736.0 + 16.0 * index as f32;
            let geometry = Some(settings::WindowGeometry {
                display_uuid: Some(uuid::Uuid::from_u128(7)),
                x: 80.0 + index as f32,
                y: 60.0,
                width: 1100.0,
                height: 750.0,
            });
            window
                .update(cx, |shell, _, cx| {
                    // Selection changes in Appearance, independently of the shell's
                    // cached snapshot. Include a previously queued geometry save.
                    shell.settings.sidebar_width = 280.0;
                    shell.schedule_save(cx);
                    settings::set_new_thread_background_effect(effect, cx);
                    settings::update(settings::SavePolicy::Immediate, cx, |settings| {
                        settings.window_geometry = geometry;
                        settings.open_web_links_in_luna = open_links_in_luna;
                        settings.terminal_font_family = terminal_family.clone();
                        settings.terminal_font_size = terminal_size;
                        settings.code_font_family = code_family.clone();
                        settings.code_font_size = code_size;
                        settings.transcript_width = transcript_width;
                    });
                    for step in 0..3 {
                        shell.settings.sidebar_width = 290.0 + step as f32;
                        shell.settings.right_pane_width = 540.0 + step as f32;
                        shell.settings.terminal_height = 300.0 + step as f32;
                        shell.schedule_save(cx);
                        let current = settings::current(cx);
                        assert_eq!(current.window_geometry, geometry);
                        assert_eq!(current.new_thread_background_effect, effect);
                        assert_eq!(current.open_web_links_in_luna, open_links_in_luna);
                        assert_eq!(current.terminal_font_family, terminal_family);
                        assert_eq!(current.terminal_font_size, terminal_size);
                        assert_eq!(current.code_font_family, code_family);
                        assert_eq!(current.code_font_size, code_size);
                        assert_eq!(current.transcript_width, transcript_width);
                    }
                    settings::flush(cx);
                    let loaded = settings::UiSettings::load(dir.path());
                    assert_eq!(loaded.window_geometry, geometry);
                    assert_eq!(loaded.new_thread_background_effect, effect);
                    assert_eq!(loaded.open_web_links_in_luna, open_links_in_luna);
                    assert_eq!(loaded.terminal_font_family, terminal_family);
                    assert_eq!(loaded.terminal_font_size, terminal_size);
                    assert_eq!(loaded.code_font_family, code_family);
                    assert_eq!(loaded.code_font_size, code_size);
                    assert_eq!(loaded.transcript_width, transcript_width);
                    assert_eq!(loaded.sidebar_width, 292.0);
                    assert_eq!(loaded.right_pane_width, 542.0);
                    assert_eq!(loaded.terminal_height, 302.0);
                })
                .unwrap();
        }
    }

    struct SavedTerminalRpc {
        saved: bool,
        listings: AtomicUsize,
        opens: AtomicUsize,
    }

    #[async_trait]
    impl RpcService for SavedTerminalRpc {
        async fn handle(
            &self,
            method: &str,
            params: serde_json::Value,
        ) -> Result<RpcReply, RpcError> {
            match method {
                methods::LIST_TERMINALS => {
                    let selected = params.get("chatId").and_then(|value| value.as_str())
                        == Some("terminal-task");
                    if selected {
                        self.listings.fetch_add(1, Ordering::SeqCst);
                    }
                    let records = if self.saved && selected {
                        vec![luna_proto::TerminalInventoryEntry {
                            id: "saved-terminal".into(),
                            chat_id: "terminal-task".into(),
                            device_id: "local".into(),
                            cwd: "/synthetic/project-a".into(),
                            shell: "sh".into(),
                            state: luna_proto::TerminalProcessState::Stopped,
                            exit_code: None,
                            last_seq: 2,
                            updated_at_ms: 1,
                        }]
                    } else {
                        Vec::new()
                    };
                    RpcReply::value(&records)
                }
                methods::SUBSCRIBE_TERMINAL => {
                    let frames = if params["terminalId"] == "saved-terminal" {
                        vec![
                            serde_json::to_value(luna_proto::TerminalEvent::Data {
                                seq: 1,
                                data: base64::engine::general_purpose::STANDARD
                                    .encode(b"L21_QA\r\n"),
                            })
                            .unwrap(),
                            serde_json::to_value(luna_proto::TerminalEvent::Exit {
                                seq: 2,
                                exit_code: -1,
                                signal: Some("closed".into()),
                            })
                            .unwrap(),
                        ]
                    } else {
                        Vec::new()
                    };
                    Ok(RpcReply::Stream(futures::stream::iter(frames).boxed()))
                }
                methods::OPEN_TERMINAL => {
                    self.opens.fetch_add(1, Ordering::SeqCst);
                    RpcReply::value(&luna_proto::TerminalSession {
                        id: "fresh-terminal".into(),
                        cwd: "/synthetic/project-a".into(),
                        shell: "sh".into(),
                    })
                }
                _ => Err(RpcError::UnknownMethod(method.into())),
            }
        }
    }

    #[gpui::test]
    fn right_terminal_picker_restores_saved_output_without_spawning(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        init_handoff_shell_test(cx, dir.path());
        cx.update(|cx| gpui_tokio::init_from_handle(cx, runtime.handle().clone()));
        let service = Arc::new(SavedTerminalRpc {
            saved: true,
            listings: AtomicUsize::new(0),
            opens: AtomicUsize::new(0),
        });
        let window = {
            let _entered = runtime.enter();
            command_palette_test_window(
                cx,
                dir.path(),
                Some(service.clone()),
                vec![palette_chat(
                    "terminal-task",
                    Some("project-a"),
                    "Terminal QA",
                )],
                Some("terminal-task"),
            )
        };
        window
            .update(cx, |shell, _, cx| {
                shell.active_chat = "terminal-task".into();
                shell.add_terminal_surface(cx);
                assert_eq!(service.opens.load(Ordering::SeqCst), 0);
            })
            .unwrap();
        for _ in 0..150 {
            cx.run_until_parked();
            runtime.block_on(tokio::task::yield_now());
            let restored = window
                .read_with(cx, |shell, cx| {
                    let tabs = shell.right_tabs.get("terminal-task");
                    let panel = shell.right_terminal.as_ref();
                    tabs.is_some_and(|tabs| tabs.len() == 1)
                        && panel.is_some_and(|panel| {
                            panel
                                .read(cx)
                                .tab_summaries(cx)
                                .first()
                                .is_some_and(|(_, _, exited)| *exited)
                        })
                })
                .unwrap();
            if restored {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        window
            .read_with(cx, |shell, cx| {
                let tabs = shell
                    .right_tabs
                    .get("terminal-task")
                    .expect("saved surface tab");
                assert_eq!(tabs.len(), 1);
                assert_eq!(shell.panels.get("terminal-task").right_active, tabs[0]);
                let panel = shell.right_terminal.as_ref().unwrap().read(cx);
                assert_eq!(panel.tab_summaries(cx).len(), 1);
                let text: String = panel
                    .active_grid_snapshot(cx)
                    .unwrap()
                    .lines
                    .iter()
                    .flat_map(|line| line.iter().map(|cell| cell.ch))
                    .collect();
                assert!(text.contains("L21_QA"));
                assert!(text.contains("[terminal stopped]"));
                assert!(shell.terminal_picker_notice.is_none());
            })
            .unwrap();
        assert!(service.listings.load(Ordering::SeqCst) >= 1);
        assert_eq!(service.opens.load(Ordering::SeqCst), 0);
        window
            .update(cx, |shell, _, cx| shell.add_new_terminal_surface(cx))
            .unwrap();
        for _ in 0..100 {
            cx.run_until_parked();
            runtime.block_on(tokio::task::yield_now());
            if service.opens.load(Ordering::SeqCst) == 1 {
                break;
            }
        }
        assert_eq!(service.opens.load(Ordering::SeqCst), 1);
    }

    #[gpui::test]
    fn right_terminal_picker_with_no_history_waits_for_new(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        init_handoff_shell_test(cx, dir.path());
        cx.update(|cx| gpui_tokio::init_from_handle(cx, runtime.handle().clone()));
        let service = Arc::new(SavedTerminalRpc {
            saved: false,
            listings: AtomicUsize::new(0),
            opens: AtomicUsize::new(0),
        });
        let window = {
            let _entered = runtime.enter();
            command_palette_test_window(
                cx,
                dir.path(),
                Some(service.clone()),
                vec![palette_chat(
                    "terminal-task",
                    Some("project-a"),
                    "Terminal QA",
                )],
                Some("terminal-task"),
            )
        };
        window
            .update(cx, |shell, _, cx| {
                shell.active_chat = "terminal-task".into();
                shell.add_terminal_surface(cx);
            })
            .unwrap();
        for _ in 0..150 {
            cx.run_until_parked();
            runtime.block_on(tokio::task::yield_now());
            if window
                .read_with(cx, |shell, _| shell.terminal_picker_notice.is_some())
                .unwrap()
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        window
            .read_with(cx, |shell, _| {
                assert!(shell.right_tabs.get("terminal-task").is_none());
                assert_eq!(
                    shell.terminal_picker_notice.as_ref().unwrap().0,
                    "terminal-task"
                );
            })
            .unwrap();
        assert_eq!(service.opens.load(Ordering::SeqCst), 0);
    }

    #[gpui::test]
    fn opening_terminals_focuses_the_terminal_once(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
            crate::history::init(
                Default::default(),
                Default::default(),
                Default::default(),
                Default::default(),
                cx,
            );
            settings::init(settings::UiSettings::default(), dir.path(), cx);
        });
        let window = cx.add_window(|_, cx| {
            let state = cx.new(|_| AppState::new());
            Shell::new(
                state,
                EngineBootConfig {
                    data_dir: dir.path().into(),
                    ipc_port: 0,
                    edge_url: "http://127.0.0.1:1".into(),
                    edge_token: None,
                    org_id: None,
                    workos_client_id: None,
                    default_harness: luna_proto::HarnessId::Mock,
                },
                cx,
            )
        });
        let mut drawer_window = None;
        for embedded in [false, false, true] {
            let panel = window
                .update(cx, |shell, window, cx| {
                    shell.open_chat("terminal-session".into(), cx);
                    shell.active_chat = "terminal-session".into();
                    let panel = if embedded {
                        shell.add_new_terminal_surface(cx);
                        shell.right_terminal.clone().unwrap()
                    } else {
                        shell.toggle_terminal(window, cx);
                        shell.terminal.clone().unwrap()
                    };
                    assert!(!shell.composer.read(cx).focus_pending);
                    panel
                })
                .unwrap();
            let terminal_window = if !embedded && drawer_window.is_some() {
                drawer_window.unwrap()
            } else {
                let handle = cx.update(|cx| {
                    cx.open_window(gpui::WindowOptions::default(), |_, _| panel.clone())
                        .unwrap()
                });
                if !embedded {
                    drawer_window = Some(handle);
                }
                handle
            };
            cx.update_window(terminal_window.into(), |_, window, cx| {
                window.draw(cx).clear();
                assert!(
                    panel.read(cx).focus_handle().is_focused(window),
                    "embedded: {embedded}"
                );
                // Ordinary redraws must not steal focus from another control.
                let other_input = cx.focus_handle();
                window.focus(&other_input, cx);
                window.draw(cx).clear();
                assert!(other_input.is_focused(window));
            })
            .unwrap();
            if !embedded {
                window
                    .update(cx, |shell, window, cx| {
                        shell.toggle_terminal(window, cx);
                        assert!(shell.composer.focus_handle(cx).is_focused(window));
                    })
                    .unwrap();
            }
        }
    }

    #[gpui::test]
    fn session_navigation_focuses_composer_once(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
            crate::history::init(
                Default::default(),
                Default::default(),
                Default::default(),
                Default::default(),
                cx,
            );
            settings::init(settings::UiSettings::default(), dir.path(), cx);
        });
        let window = cx.add_window(|_, cx| {
            let state = cx.new(|_| AppState::new());
            Shell::new(
                state,
                EngineBootConfig {
                    data_dir: dir.path().into(),
                    ipc_port: 0,
                    edge_url: "http://127.0.0.1:1".into(),
                    edge_token: None,
                    org_id: None,
                    workos_client_id: None,
                    default_harness: luna_proto::HarnessId::Mock,
                },
                cx,
            )
        });
        // Render the real composer in its own window to exercise mounting
        // without the shell's boot/connection gate hiding the destination.
        let composer_window = cx.update(|cx| {
            let composer = window.read(cx).unwrap().composer.clone();
            cx.open_window(gpui::WindowOptions::default(), |_, _| composer)
                .unwrap()
        });
        for destination in ["initial", "chat", "chat", "new", "new", "back", "settings"] {
            window
                .update(cx, |shell, _, cx| match destination {
                    "chat" => shell.open_chat("existing-session".into(), cx),
                    "new" => shell.open_new_session(cx),
                    "back" => shell.apply_nav(NavEntry::Chat("existing-session".into()), cx),
                    "settings" => {
                        shell.open_settings(SettingsSection::Devices, cx);
                        shell.close_settings(cx);
                    }
                    _ => {}
                })
                .unwrap();
            cx.update_window(composer_window.into(), |composer, window, cx| {
                window.draw(cx).clear();
                assert!(
                    composer
                        .downcast::<Composer>()
                        .unwrap()
                        .focus_handle(cx)
                        .is_focused(window),
                    "{destination}"
                );
                // Subsequent renders after a click-away must not reclaim it.
                window.blur();
                window.draw(cx).clear();
                assert!(window.focused(cx).is_none(), "{destination}");
            })
            .unwrap();
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[gpui::test]
    fn transcript_links_open_new_tabs_and_reject_stale_sessions(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
            settings::init(settings::UiSettings::default(), dir.path(), cx);
        });
        let window = cx.add_window(|_, cx| {
            let state = cx.new(|_| AppState::new());
            Shell::new(
                state,
                EngineBootConfig {
                    data_dir: dir.path().into(),
                    ipc_port: 0,
                    edge_url: "http://127.0.0.1:1".into(),
                    edge_token: None,
                    org_id: None,
                    workos_client_id: None,
                    default_harness: luna_proto::HarnessId::Mock,
                },
                cx,
            )
        });
        let weak = window
            .update(cx, |shell, window, cx| {
                use crate::markdown::render::{
                    LinkAction, LinkActivation, LinkOutcome, LinkTarget,
                };
                shell.active_chat = "first-session".into();
                shell.state.update(cx, |state, _| {
                    state.selected_chat = Some("first-session".into())
                });
                let mut activation = LinkActivation {
                    target: LinkTarget::new("Docs", "https://example.com/docs"),
                    action: LinkAction::Primary,
                    source_session: Some("first-session".into()),
                };
                assert!(!shell.right_pane_open(cx));
                assert_eq!(
                    shell.activate_session_link(&activation, window, cx),
                    LinkOutcome::Internal
                );
                assert!(shell.right_pane_open(cx));
                let first = shell.browser_seq;
                assert_eq!(
                    shell.browsers[&first].read(cx).page.url.as_deref(),
                    Some("https://example.com/docs")
                );
                assert_eq!(
                    shell.resolved_right_active(cx),
                    RightSurface::Browser(first)
                );
                shell.activate_session_link(&activation, window, cx);
                assert_eq!(shell.browsers.len(), 2);
                settings::update(settings::SavePolicy::Immediate, cx, |settings| {
                    settings.open_web_links_in_luna = false;
                });
                assert_eq!(
                    shell.activate_session_link(&activation, window, cx),
                    LinkOutcome::External("https://example.com/docs".into())
                );
                assert_eq!(shell.browsers.len(), 2);
                activation.action = LinkAction::Internal;
                assert_eq!(
                    shell.activate_session_link(&activation, window, cx),
                    LinkOutcome::Internal,
                    "the explicit internal action ignores the default preference"
                );
                assert_eq!(shell.browsers.len(), 3);
                activation.action = LinkAction::External;
                assert_eq!(
                    shell.activate_session_link(&activation, window, cx),
                    LinkOutcome::External("https://example.com/docs".into())
                );
                assert_eq!(shell.browsers.len(), 3);
                activation.source_session = Some("other-session".into());
                assert_eq!(
                    shell.activate_session_link(&activation, window, cx),
                    LinkOutcome::Rejected
                );
                activation.source_session = Some("first-session".into());
                shell.state.update(cx, |state, _| {
                    state.selected_chat = Some("switch-in-progress".into())
                });
                assert_eq!(
                    shell.activate_session_link(&activation, window, cx),
                    LinkOutcome::Rejected
                );
                assert_eq!(shell.browsers.len(), 3);
                shell.state.update(cx, |state, _| {
                    state.selected_chat = Some("first-session".into())
                });
                shell.add_subagent_surface(
                    "first-session".into(),
                    "child-doc".into(),
                    "Child".into(),
                    true,
                    cx,
                );
                let child = shell
                    .subagent_tabs
                    .values()
                    .next()
                    .unwrap()
                    .transcript
                    .clone();
                let ui = child.read(cx).link_ui().unwrap();
                assert_eq!(ui.source_session.as_deref(), Some("first-session"));
                activation.action = LinkAction::Internal;
                activation.source_session = ui.source_session;
                assert_eq!(
                    shell.activate_session_link(&activation, window, cx),
                    LinkOutcome::Internal
                );
                assert_eq!(shell.browsers.len(), 4);
                let weak = shell.browsers[&first].downgrade();
                shell.close_right_surface(RightSurface::Browser(first), window, cx);
                weak
            })
            .unwrap();
        cx.run_until_parked();
        assert!(weak.upgrade().is_none());
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[gpui::test]
    fn markdown_preview_events_open_browser_from_tree_and_file_tabs(cx: &mut TestAppContext) {
        use crate::markdown::render::{LinkAction, LinkActivation, LinkTarget};
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
            settings::init(settings::UiSettings::default(), dir.path(), cx);
        });
        let window = cx.add_window(|_, cx| {
            let state = cx.new(|_| AppState::new());
            Shell::new(
                state,
                EngineBootConfig {
                    data_dir: dir.path().into(),
                    ipc_port: 0,
                    edge_url: "http://127.0.0.1:1".into(),
                    edge_token: None,
                    org_id: None,
                    workos_client_id: None,
                    default_harness: luna_proto::HarnessId::Mock,
                },
                cx,
            )
        });
        let surfaces = window
            .update(cx, |shell, window, cx| {
                shell.active_chat = "owner".into();
                shell
                    .state
                    .update(cx, |state, _| state.selected_chat = Some("owner".into()));
                shell.add_files_surface(window, cx);
                shell.add_file_surface("README.md".into(), window, cx);
                [
                    shell.files[&shell.panel_key(cx)].clone(),
                    shell.file_surfaces[&shell.file_surface_seq].clone(),
                ]
            })
            .unwrap();
        let mut last_external_url = None;
        for (index, surface) in surfaces.iter().enumerate() {
            let mut activation = LinkActivation {
                target: LinkTarget::new("Docs", &format!("https://example.com/preview/{index}")),
                action: LinkAction::Primary,
                source_session: Some("owner".into()),
            };
            surface.update(cx, |_, cx| {
                cx.emit(FilesEvent::OpenWebLink(activation.clone()))
            });
            cx.run_until_parked();
            assert_eq!(cx.opened_url(), last_external_url);
            window
                .update(cx, |shell, _, cx| {
                    assert!(shell.right_pane_open(cx));
                    assert_eq!(shell.browsers.len(), index + 1);
                    assert_eq!(
                        shell.resolved_right_active(cx),
                        RightSurface::Browser(shell.browser_seq)
                    );
                    assert_eq!(
                        shell.browsers[&shell.browser_seq]
                            .read(cx)
                            .page
                            .url
                            .as_deref(),
                        Some(activation.target.original.as_str())
                    );
                })
                .unwrap();
            activation.action = LinkAction::External;
            surface.update(cx, |_, cx| {
                cx.emit(FilesEvent::OpenWebLink(activation.clone()))
            });
            cx.run_until_parked();
            assert_eq!(
                cx.opened_url().as_deref(),
                Some(activation.target.original.as_str())
            );
            last_external_url = Some(activation.target.original.clone());
            activation.target = LinkTarget::new("Stale", "https://example.com/stale");
            activation.source_session = Some("stale-owner".into());
            for action in [LinkAction::Internal, LinkAction::External] {
                activation.action = action;
                surface.update(cx, |_, cx| {
                    cx.emit(FilesEvent::OpenWebLink(activation.clone()))
                });
                cx.run_until_parked();
                assert_eq!(cx.opened_url(), last_external_url);
            }
            window
                .update(cx, |shell, _, _| {
                    assert_eq!(shell.browsers.len(), index + 1)
                })
                .unwrap();
        }
    }

    #[gpui::test]
    fn browser_tabs_keep_session_ownership_and_release_on_close(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
        });
        let window = cx.add_window(|_, cx| {
            let state = cx.new(|_| AppState::new());
            Shell::new(
                state,
                EngineBootConfig {
                    data_dir: dir.path().into(),
                    ipc_port: 0,
                    edge_url: "http://127.0.0.1:1".into(),
                    edge_token: None,
                    org_id: None,
                    workos_client_id: None,
                    default_harness: luna_proto::HarnessId::Mock,
                },
                cx,
            )
        });
        let weak = window
            .update(cx, |shell, window, cx| {
                // A fresh-session canvas never accumulates ownerless tabs.
                shell.add_browser_surface(None, window, cx);
                assert!(shell.browsers.is_empty());
                shell.active_chat = "first-session".into();
                shell.add_browser_surface(None, window, cx);
                let first = shell.browser_seq;
                let weak = shell.browsers[&first].downgrade();
                shell.add_browser_surface(None, window, cx);
                let second = shell.browser_seq;
                assert_ne!(first, second);
                assert_eq!(
                    shell.resolved_right_active(cx),
                    RightSurface::Browser(second)
                );
                shell.active_chat = "second-session".into();
                assert!(shell.right_surface_rows(cx).is_empty());
                shell.add_browser_surface(None, window, cx);
                let other = shell.browser_seq;
                shell.active_chat = "first-session".into();
                assert_eq!(shell.right_surface_rows(cx).len(), 2);
                assert_eq!(
                    shell.resolved_right_active(cx),
                    RightSurface::Browser(second)
                );
                // Closing a background tab preserves the selected address input.
                let focus = window.focused(cx);
                shell.close_right_surface(RightSurface::Browser(first), window, cx);
                assert_eq!(window.focused(cx), focus);
                assert_eq!(
                    shell.resolved_right_active(cx),
                    RightSurface::Browser(second)
                );
                shell.close_right_surface(RightSurface::Browser(second), window, cx);
                assert_eq!(shell.resolved_right_active(cx), RightSurface::Picker);
                shell.active_chat = "second-session".into();
                assert_eq!(
                    shell.resolved_right_active(cx),
                    RightSurface::Browser(other)
                );
                shell.close_right_surface(RightSurface::Browser(other), window, cx);
                assert!(shell.browsers.is_empty());
                assert!(shell.browser_subs.is_empty());
                weak
            })
            .unwrap();
        cx.run_until_parked();
        assert!(
            weak.upgrade().is_none(),
            "closed browser retained by callbacks"
        );
    }

    #[gpui::test]
    fn cmd_w_closes_the_active_right_pane_surface_before_the_window(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
        });
        let window = cx.add_window(|_, cx| {
            let state = cx.new(|_| AppState::new());
            Shell::new(
                state,
                EngineBootConfig {
                    data_dir: dir.path().into(),
                    ipc_port: 0,
                    edge_url: "http://127.0.0.1:1".into(),
                    edge_token: None,
                    org_id: None,
                    workos_client_id: None,
                    default_harness: luna_proto::HarnessId::Mock,
                },
                cx,
            )
        });
        window
            .update(cx, |shell, window, cx| {
                shell.active_chat = "session".into();
                shell.toggle_right_pane(cx);
                assert!(shell.right_pane_open(cx));

                shell.add_browser_surface(None, window, cx);
                let first = shell.browser_seq;
                shell.add_browser_surface(None, window, cx);
                let second = shell.browser_seq;
                assert_eq!(
                    shell.resolved_right_active(cx),
                    RightSurface::Browser(second)
                );

                // ⌘W closes the active tab, not the window.
                assert!(shell.close_active_surface(window, cx));
                assert_eq!(
                    shell.resolved_right_active(cx),
                    RightSurface::Browser(first)
                );

                // …and again for the last tab.
                assert!(shell.close_active_surface(window, cx));
                assert_eq!(shell.resolved_right_active(cx), RightSurface::Picker);

                // An open pane with nothing left to close falls through to the
                // window-close rung instead of being consumed.
                assert!(!shell.close_active_surface(window, cx));

                // So does an already-closed pane.
                shell.toggle_right_pane(cx);
                assert!(!shell.right_pane_open(cx));
                assert!(!shell.close_active_surface(window, cx));
            })
            .unwrap();
    }

    #[gpui::test]
    fn lifecycle_actions_keep_pending_and_failed_file_saves_alive(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
        });
        let window = cx.add_window(|_, cx| {
            let state = cx.new(|_| AppState::new());
            Shell::new(
                state,
                EngineBootConfig {
                    data_dir: dir.path().into(),
                    ipc_port: 0,
                    edge_url: "http://127.0.0.1:1".into(),
                    edge_token: None,
                    org_id: None,
                    workos_client_id: None,
                    default_harness: luna_proto::HarnessId::Mock,
                },
                cx,
            )
        });
        for failed in [false, true] {
            window
                .update(cx, |shell, window, cx| {
                    window.activate_window();
                    let state = shell.state.clone();
                    let files = cx.new(|cx| {
                        let mut files = FilesSurface::new(
                            state,
                            "test".into(),
                            false,
                            1000,
                            13.0,
                            false,
                            false,
                            cx,
                        );
                        files.seed_pending_exit_test_document(failed);
                        files
                    });
                    shell.files.insert("test".into(), files);
                })
                .unwrap();
            cx.update(|cx| cx.dispatch_action(&crate::app_menus::Quit));
            cx.run_until_parked();
            window
                .update(cx, |shell, _, cx| {
                    assert!(matches!(shell.pending_exit, Some(PendingExit::Quit)));
                    assert!(!shell.all_file_edits_flushed(cx));
                    shell.cancel_file_close(RightSurface::Files, cx);
                    assert!(shell.pending_exit.is_none());
                })
                .unwrap();
            cx.update(|cx| cx.dispatch_action(&crate::app_menus::CloseWindow));
            cx.run_until_parked();
            window
                .update(cx, |shell, _, cx| {
                    assert!(matches!(shell.pending_exit, Some(PendingExit::CloseWindow)));
                    shell.quit_for_runtime_change(cx);
                    assert!(matches!(
                        shell.pending_exit,
                        Some(PendingExit::RuntimeChange)
                    ));
                    assert!(shell.runtime_change_task.is_none());
                    shell.apply_staged_update(PathBuf::from("must-not-install"), cx);
                    assert!(matches!(
                        shell.pending_exit,
                        Some(PendingExit::InstallUpdate(_))
                    ));
                    assert!(matches!(shell.update_flow, UpdateFlow::Idle));
                    shell.cancel_file_close(RightSurface::Files, cx);
                    assert!(shell.pending_exit.is_none());
                })
                .unwrap();
        }
    }
}

/// Native browser regression fixture hooks are excluded from shipped builds.
#[cfg(feature = "browser-fixture")]
impl Shell {
    pub fn fixture_focus_mounted(&self, window: &Window, cx: &App) -> bool {
        self.shortcut_focus.contains_focused(window, cx)
    }
    pub fn fixture_active_browser(
        &self,
        cx: &App,
    ) -> Option<(u64, Entity<crate::browser::BrowserSurface>)> {
        let RightSurface::Browser(id) = self.resolved_right_active(cx) else {
            return None;
        };
        self.browsers.get(&id).cloned().map(|browser| (id, browser))
    }
    pub fn fixture_open_browser(
        &mut self,
        url: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (u64, Entity<crate::browser::BrowserSurface>) {
        if !self.right_pane_open(cx) {
            self.toggle_right_pane(cx);
        }
        // Hosted Macs can expose only a 1024px desktop. Use the app's
        // normal collapsed-sidebar layout to keep both conversation and
        // preview readable in that real window.
        if f32::from(window.viewport_size().width) < 1200.0 {
            self.settings.sidebar_collapsed = true;
        }
        self.add_browser_surface(url, window, cx);
        (self.browser_seq, self.browsers[&self.browser_seq].clone())
    }
    pub fn fixture_select_browser(&mut self, id: u64, cx: &mut Context<Self>) {
        self.set_right_active(RightSurface::Browser(id), cx);
    }
    pub fn fixture_close_browser(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        self.close_right_surface(RightSurface::Browser(id), window, cx);
    }
    pub fn fixture_browser_menu(&mut self, open: bool, cx: &mut Context<Self>) {
        if open {
            self.right_plus.open(());
            cx.notify();
        } else {
            self.close_right_plus(cx);
        }
    }
    pub fn fixture_browser_menu_mounted(&self) -> bool {
        self.right_plus.get().is_some()
    }
    pub fn fixture_expand_browser(&mut self, cx: &mut Context<Self>) {
        self.toggle_right_pane_expand(cx);
    }
    pub fn fixture_blur_browser(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.route = Route::Settings(SettingsSection::Devices);
        window.blur();
        cx.notify();
    }
    pub fn fixture_toggle_sidebar(&mut self, right: bool, cx: &mut Context<Self>) {
        if right {
            self.toggle_right_pane(cx);
        } else {
            self.toggle_sidebar(cx);
        }
    }
    pub fn fixture_resize_browser(&mut self, width: f32, cx: &mut Context<Self>) {
        self.settings.right_pane_width = width;
        cx.notify();
    }
}

#[cfg(test)]
mod right_tab_mouse_regressions {
    use super::*;
    use gpui::{AppContext, TestAppContext, VisualTestContext};

    // Render the production strip and use its real Shell callbacks, without
    // starting an engine or rendering the rest of the desktop application.
    struct TabHost {
        shell: Entity<Shell>,
        _data_dir: tempfile::TempDir,
    }

    impl Render for TabHost {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            self.shell.update(cx, |shell, cx| {
                let tabs = shell.render_right_tab_strip(cx);
                shell.titlebar_drag_region(
                    "right-tab-test-titlebar",
                    div().w(px(400.)).h(px(40.)).child(tabs),
                    cx,
                )
            })
        }
    }

    fn setup(cx: &mut TestAppContext) -> (Entity<Shell>, &mut VisualTestContext) {
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
        });
        let (host, cx) = cx.add_window_view(|_, cx| {
            let shell = cx.new(|cx| {
                let state = cx.new(|_| AppState::new());
                let mut shell = Shell::new(
                    state,
                    EngineBootConfig {
                        data_dir: dir.path().into(),
                        ipc_port: 0,
                        edge_url: "http://127.0.0.1:1".into(),
                        edge_token: None,
                        org_id: None,
                        workos_client_id: None,
                        default_harness: luna_proto::HarnessId::Mock,
                    },
                    cx,
                );
                shell.active_chat = "parent".into();
                for id in ["first", "second"] {
                    shell.add_subagent_surface("parent".into(), id.into(), id.into(), false, cx);
                }
                shell
            });
            TabHost {
                shell,
                _data_dir: dir,
            }
        });
        let shell = host.read_with(cx, |host, _| host.shell.clone());
        cx.update(|window, cx| window.draw(cx).clear());
        (shell, cx)
    }

    #[gpui::test]
    fn subagent_close_press_does_not_start_parent_drag(cx: &mut TestAppContext) {
        let (shell, cx) = setup(cx);
        let start = cx.debug_bounds("right-surface-close-0").unwrap().center();
        let end = start + gpui::point(px(6.), px(0.));
        cx.simulate_mouse_down(start, MouseButton::Left, gpui::Modifiers::default());
        cx.simulate_mouse_move(end, Some(MouseButton::Left), gpui::Modifiers::default());
        cx.update(|_, cx| assert!(!cx.has_active_drag(), "close press started a tab drag"));
        cx.simulate_mouse_up(end, MouseButton::Left, gpui::Modifiers::default());
        shell.read_with(cx, |shell, cx| {
            assert!(!shell.subagent_tabs.contains_key(&1));
            assert!(shell.subagent_tabs.contains_key(&2));
            assert_eq!(shell.resolved_right_active(cx), RightSurface::Subagent(2));
        });
    }

    #[gpui::test]
    fn tab_strip_scrolls_over_chips_inside_titlebar(cx: &mut TestAppContext) {
        let (shell, cx) = setup(cx);
        shell.update(cx, |shell, cx| {
            for id in ["third", "fourth", "fifth", "sixth"] {
                shell.add_subagent_surface("parent".into(), id.into(), id.into(), false, cx);
            }
        });
        cx.update(|window, cx| window.draw(cx).clear());
        let start = cx.debug_bounds("right-surface-tab-0").unwrap().center();
        cx.update(|window, cx| {
            window.dispatch_event(
                gpui::PlatformInput::ScrollWheel(gpui::ScrollWheelEvent {
                    position: start,
                    delta: gpui::ScrollDelta::Pixels(gpui::point(px(-100.), px(0.))),
                    modifiers: gpui::Modifiers::default(),
                    touch_phase: gpui::TouchPhase::Moved,
                }),
                cx,
            );
        });
        shell.read_with(cx, |shell, _| {
            assert!(
                shell.right_tab_scroll.offset().x < px(0.),
                "tab strip did not scroll"
            );
        });
    }

    #[gpui::test]
    fn subagent_tab_body_still_selects_drags_and_middle_closes(cx: &mut TestAppContext) {
        let (shell, cx) = setup(cx);
        let start = cx.debug_bounds("right-surface-tab-0").unwrap().center();
        cx.simulate_click(start, gpui::Modifiers::default());
        shell.read_with(cx, |shell, cx| {
            assert_eq!(shell.resolved_right_active(cx), RightSurface::Subagent(1));
        });
        cx.simulate_mouse_down(start, MouseButton::Left, gpui::Modifiers::default());
        cx.simulate_mouse_move(
            start + gpui::point(px(8.), px(0.)),
            Some(MouseButton::Left),
            gpui::Modifiers::default(),
        );
        cx.update(|_, cx| {
            assert_eq!(
                cx.has_active_drag(),
                crate::click_activation_drag_enabled(),
                "tab drag policy does not match the current platform"
            )
        });
        cx.simulate_mouse_up(start, MouseButton::Left, gpui::Modifiers::default());
        cx.simulate_mouse_down(start, MouseButton::Middle, gpui::Modifiers::default());
        cx.simulate_mouse_up(start, MouseButton::Middle, gpui::Modifiers::default());
        shell.read_with(cx, |shell, _| {
            assert!(!shell.subagent_tabs.contains_key(&1));
            assert!(shell.subagent_tabs.contains_key(&2));
        });
    }

    #[cfg(target_os = "windows")]
    #[gpui::test]
    fn subagent_tab_click_jitter_selects_without_starting_a_drag(cx: &mut TestAppContext) {
        let (shell, cx) = setup(cx);
        let start = cx.debug_bounds("right-surface-tab-0").unwrap().center();
        let end = start + gpui::point(px(8.), px(0.));

        cx.simulate_mouse_down(start, MouseButton::Left, gpui::Modifiers::default());
        cx.simulate_mouse_move(end, Some(MouseButton::Left), gpui::Modifiers::default());
        cx.update(|_, cx| {
            assert!(
                !cx.has_active_drag(),
                "ordinary Windows click jitter started a tab drag"
            )
        });
        cx.simulate_mouse_up(end, MouseButton::Left, gpui::Modifiers::default());

        shell.read_with(cx, |shell, cx| {
            assert_eq!(shell.resolved_right_active(cx), RightSurface::Subagent(1));
        });
    }
}

#[cfg(test)]
mod shortcut_focus_regressions {
    use super::*;
    use gpui::{AppContext, TestAppContext};

    struct ShortcutHost {
        root: FocusHandle,
        unfocused: FocusHandle,
        editor: FocusHandle,
        show_editor: bool,
        jumps: usize,
    }

    impl Render for ShortcutHost {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let root = self.root.clone();
            let unfocused = self.unfocused.clone();
            let preferred = self.editor.clone();
            window.defer(cx, move |window, cx| {
                restore_mounted_focus(&root, &preferred, &unfocused, window, cx);
            });
            div()
                .size_full()
                .track_focus(&self.root)
                .child(div().track_focus(&self.unfocused))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, event: &MouseDownEvent, window, cx| {
                        // Exercise mouse focus handoffs, hiding a focused pane,
                        // and clicking a control that explicitly clears focus.
                        this.show_editor = event.position.x < px(100.0);
                        if event.position.x < px(200.0) {
                            window.focus(&this.editor, cx);
                        } else {
                            window.blur();
                        }
                        cx.notify();
                    }),
                )
                .on_action(cx.listener(|this, _: &JumpSession, _, _| this.jumps += 1))
                .when(self.show_editor, |el| {
                    el.child(div().track_focus(&self.editor))
                })
        }
    }

    #[gpui::test]
    fn explicit_blur_does_not_refocus_a_mounted_input(cx: &mut TestAppContext) {
        let host = cx.add_window(|_, cx| ShortcutHost {
            root: cx.focus_handle(),
            unfocused: cx.focus_handle(),
            editor: cx.focus_handle(),
            show_editor: true,
            jumps: 0,
        });
        cx.run_until_parked();
        cx.update_window(host.into(), |_, window, cx| window.draw(cx).clear())
            .unwrap();
        host.update(cx, |host, window, cx| {
            window.focus(&host.editor, cx);
            window.blur();
            restore_mounted_focus(&host.root, &host.editor, &host.unfocused, window, cx);
            assert!(host.unfocused.is_focused(window));
            // Subsequent renders must keep the neutral shortcut focus too.
            restore_mounted_focus(&host.root, &host.editor, &host.unfocused, window, cx);
            assert!(host.unfocused.is_focused(window));
        })
        .unwrap();
    }

    #[gpui::test]
    fn shortcuts_recover_from_retained_editor_focus(cx: &mut TestAppContext) {
        cx.update(|cx| {
            cx.bind_keys([KeyBinding::new(
                &platform_combo("mod-2"),
                JumpSession(1),
                None,
            )]);
        });
        let host = cx.add_window(|_, cx| ShortcutHost {
            root: cx.focus_handle(),
            unfocused: cx.focus_handle(),
            editor: cx.focus_handle(),
            show_editor: true,
            jumps: 0,
        });
        for show_editor in [true, false, true, false] {
            host.update(cx, |host, window, cx| {
                host.show_editor = show_editor;
                // Keep the editor handle alive and focused even when hidden.
                window.focus(&host.editor, cx);
                cx.notify();
            })
            .unwrap();
            cx.run_until_parked();
            cx.update_window(host.into(), |_, window, cx| window.draw(cx).clear())
                .unwrap();
            host.update(cx, |host, window, cx| {
                restore_mounted_focus(&host.root, &host.editor, &host.unfocused, window, cx);
                assert!(host.root.contains_focused(window, cx));
                assert_eq!(host.editor.is_focused(window), show_editor);
            })
            .unwrap();
            cx.simulate_keystrokes(host.into(), &platform_combo("mod-2"));
        }
        host.update(cx, |host, window, cx| {
            assert_eq!(host.jumps, 4);
            window.blur();
            restore_mounted_focus(&host.root, &host.editor, &host.unfocused, window, cx);
            assert!(host.unfocused.is_focused(window));
        })
        .unwrap();
    }

    #[gpui::test]
    fn shortcuts_work_after_mouse_focus_changes(cx: &mut TestAppContext) {
        cx.update(|cx| {
            cx.bind_keys([KeyBinding::new(
                &platform_combo("mod-2"),
                JumpSession(1),
                None,
            )]);
        });
        let host = cx.add_window(|_, cx| ShortcutHost {
            root: cx.focus_handle(),
            unfocused: cx.focus_handle(),
            editor: cx.focus_handle(),
            show_editor: true,
            jumps: 0,
        });
        for (index, x) in [50.0, 150.0, 250.0, 50.0, 150.0, 250.0]
            .into_iter()
            .enumerate()
        {
            cx.update_window(host.into(), |_, window, cx| {
                window.draw(cx).clear();
                window.dispatch_event(
                    gpui::PlatformInput::MouseDown(MouseDownEvent {
                        position: gpui::point(px(x), px(20.0)),
                        button: MouseButton::Left,
                        modifiers: gpui::Modifiers::default(),
                        click_count: 1,
                        first_mouse: false,
                    }),
                    cx,
                );
            })
            .unwrap();
            // Dispatch immediately after the mouse event; no manual recovery.
            cx.simulate_keystrokes(host.into(), &platform_combo("mod-2"));
            host.update(cx, |host, window, cx| {
                assert_eq!(
                    host.jumps,
                    index + 1,
                    "shortcut failed after mouse click at {x}"
                );
                assert!(host.root.contains_focused(window, cx));
                assert_eq!(host.editor.is_focused(window), x < 100.0);
            })
            .unwrap();
        }
    }
}

/// Native visual QA uses the production shell with isolated fixture data.
#[cfg(feature = "appshots-fixture")]
impl Shell {
    pub fn fixture_usage_card_is_open(&self) -> bool {
        self.usage_card_open
    }

    pub fn fixture_poll_usage(&mut self, cx: &mut Context<Self>) {
        self.maybe_auto_refresh_usage(cx);
    }

    pub fn fixture_provider_usage(&mut self, cx: &mut Context<Self>) {
        let now = chrono::Utc::now().timestamp_millis();
        let environment_id = self
            .state
            .read(cx)
            .local_device_id
            .clone()
            .unwrap_or_else(|| "fixture-mac".into());
        let snapshot = luna_proto::UsageSnapshot {
            summary: luna_proto::UsageSummary {
                contract_version: luna_proto::USAGE_CONTRACT_VERSION,
                read_at_ms: now,
                time_zone: "UTC".into(),
                days: 7,
                resolution: "daily".into(),
                period_starts_ms: vec![],
                until_ms: None,
                buckets: vec![],
                sources: vec![luna_proto::UsageSource {
                    provider: luna_proto::UsageProvider::Codex,
                    environment_id: environment_id.clone(),
                    fingerprint: "fixture-only".into(),
                    status: luna_proto::UsageSourceStatus::Ready,
                    distinct_sessions: 0,
                    files_scanned: 0,
                    files_cached: 0,
                    failed_files: 0,
                    detail: None,
                }],
                scan_duration_ms: 0,
                partial: false,
                warnings: vec![],
            },
            limits: vec![luna_proto::UsageLimitSnapshot {
                provider: luna_proto::UsageProvider::Codex,
                verified_account_id: Some("fixture-codex".into()),
                account_label: Some("iulian@example.test".into()),
                plan: Some("pro".into()),
                checked_at_ms: Some(now),
                availability: luna_proto::LimitAvailability::Ready,
                windows: vec![luna_proto::UsageLimitWindow {
                    id: "weekly".into(),
                    label: "Weekly".into(),
                    used_percent: 100.0,
                    resets_at_ms: Some(now + (4 * 24 + 23) * 60 * 60_000 + 45 * 60_000),
                    duration_minutes: Some(7 * 24 * 60),
                }],
                reset_credits: None,
                detail: None,
            }],
        };
        self.usage_snapshot = Some(snapshot.clone());
        self.usage_environment_snapshots = vec![(environment_id.clone(), snapshot)];
        self.usage_environment_filter = Some(environment_id);
        self.usage_provider_filter = Some(luna_proto::UsageProvider::Codex);
        self.provider_updates = vec![ProviderUpdateStatus {
            harness: HarnessId::Codex,
            installed_version: None,
            latest_version: None,
            update_available: true,
            update_command: None,
            detail: None,
        }];
        cx.notify();
    }

    pub fn fixture_add_compact_usage_providers(&mut self, cx: &mut Context<Self>) {
        let now = chrono::Utc::now().timestamp_millis();
        let Some(environment_id) = self.usage_environment_filter.clone() else {
            return;
        };
        let source = luna_proto::UsageSource {
            provider: luna_proto::UsageProvider::OpenCodeGo,
            environment_id,
            fingerprint: "fixture-opencode".into(),
            status: luna_proto::UsageSourceStatus::Ready,
            distinct_sessions: 0,
            files_scanned: 0,
            files_cached: 0,
            failed_files: 0,
            detail: None,
        };
        let limit = luna_proto::UsageLimitSnapshot {
            provider: luna_proto::UsageProvider::OpenCodeGo,
            verified_account_id: Some("fixture-opencode".into()),
            account_label: None,
            plan: Some("OpenCode Go".into()),
            checked_at_ms: Some(now),
            availability: luna_proto::LimitAvailability::Ready,
            windows: vec![
                luna_proto::UsageLimitWindow {
                    id: "five-hour".into(),
                    label: "5h".into(),
                    used_percent: 0.0,
                    resets_at_ms: Some(now + 22 * 60_000),
                    duration_minutes: Some(5 * 60),
                },
                luna_proto::UsageLimitWindow {
                    id: "weekly".into(),
                    label: "7d".into(),
                    used_percent: 0.0,
                    resets_at_ms: Some(now + (3 * 24 + 2) * 60 * 60_000 + 60_000),
                    duration_minutes: Some(7 * 24 * 60),
                },
                luna_proto::UsageLimitWindow {
                    id: "monthly".into(),
                    label: "Monthly".into(),
                    used_percent: 37.0,
                    resets_at_ms: Some(now + (17 * 24 + 11) * 60 * 60_000 + 60_000),
                    duration_minutes: Some(30 * 24 * 60),
                },
            ],
            reset_credits: None,
            detail: None,
        };
        let claude_source = luna_proto::UsageSource {
            provider: luna_proto::UsageProvider::Claude,
            environment_id: source.environment_id.clone(),
            fingerprint: "fixture-claude".into(),
            status: luna_proto::UsageSourceStatus::Ready,
            distinct_sessions: 0,
            files_scanned: 0,
            files_cached: 0,
            failed_files: 0,
            detail: None,
        };
        let claude_limit = luna_proto::UsageLimitSnapshot {
            provider: luna_proto::UsageProvider::Claude,
            verified_account_id: Some("fixture-claude".into()),
            account_label: None,
            plan: Some("Max".into()),
            checked_at_ms: Some(now),
            availability: luna_proto::LimitAvailability::Ready,
            windows: vec![luna_proto::UsageLimitWindow {
                id: "seven_day".into(),
                label: "Weekly".into(),
                used_percent: 10.0,
                resets_at_ms: Some(now + 2 * 24 * 60 * 60_000),
                duration_minutes: Some(7 * 24 * 60),
            }],
            reset_credits: None,
            detail: None,
        };
        if let Some(snapshot) = &mut self.usage_snapshot {
            snapshot.summary.sources.push(source.clone());
            snapshot.summary.sources.push(claude_source.clone());
            snapshot.limits.push(limit.clone());
            snapshot.limits.push(claude_limit.clone());
        }
        if let Some((_, snapshot)) = self.usage_environment_snapshots.first_mut() {
            snapshot.summary.sources.push(source);
            snapshot.summary.sources.push(claude_source);
            snapshot.limits.push(limit);
            snapshot.limits.push(claude_limit);
        }
        cx.notify();
    }

    pub fn fixture_accounts_settings(
        &mut self,
        snapshot: luna_proto::AgentAccountsSnapshot,
        cx: &mut Context<Self>,
    ) {
        self.open_settings(SettingsSection::Providers, cx);
        let state = self.state.clone();
        self.accounts_page = Some(cx.new(|cx| AccountsPage::new_fixture(state, snapshot, cx)));
        cx.notify();
    }

    pub fn fixture_appshots_settings(&mut self, open: bool, cx: &mut Context<Self>) {
        if open {
            self.open_settings(SettingsSection::Appshots, cx);
        } else {
            self.close_settings(cx);
        }
    }
    pub fn fixture_appshots_composer(&self) -> Entity<Composer> {
        self.composer.clone()
    }
    pub fn fixture_appshots_sidebar(&mut self, collapsed: bool, cx: &mut Context<Self>) {
        self.settings.sidebar_collapsed = collapsed;
        cx.notify();
    }
    pub fn fixture_appshots_transcript_start(&self, cx: &mut Context<Self>) {
        self.transcript
            .update(cx, |t, cx| t.fixture_appshots_start(cx));
    }
}
