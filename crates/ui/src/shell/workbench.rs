//! The central chat workbench: Main and its child, side and alongside chats
//! as tabs in groups, arranged in resizable nested splits inside the inset
//! canvas. Related chats (right pane) is the library they reopen from.
//!
//! The selected left-navigation chat is the workbench's **Main** and owner;
//! the focused pane is a separate value. The layout belongs to the engine
//! (`WatchChatWorkbench` / `MutateChatWorkbench`, canonical
//! `luna_proto::chat_workbench` types): a gesture runs the shared reducer for
//! an optimistic preview, then commits one serialized mutation guarded by
//! engine generation, profile and main chat. An engine without
//! `chat-workbench-v1` keeps the single Main view and says so for layout
//! actions.
//!
//! Views are keyed by chat ID and survive moves, focus and ratio changes:
//! each non-main tab mounts one [`PaneChatView`] bound to the per-chat
//! [`PaneComposers`] composer (drafts, sends and receipt watches belong to
//! the chat) and its owner-token watch. Closing a tab drops the view only.
//! Main renders the shell's own transcript and composer.

mod chooser;
mod model;
pub(super) mod render;
pub(super) use chooser::CreationKind;

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;

use gpui::Bounds;
use serde_json::json;

use super::pane_chat::{PaneChatView, PaneIdentity};
use super::*;
use model::{Command, Destination, command};
pub(super) use model::{DropTarget, Edge, Layout, TabRef};

/// Smallest pane a split or a divider drag may produce.
pub(super) const MIN_PANE_WIDTH: f32 = 320.0;
pub(super) const MIN_PANE_HEIGHT: f32 = 240.0;
/// Pointer travel before a tab press becomes a drag.
const DRAG_START_DISTANCE: f32 = 5.0;
/// Edge zones are about a quarter of the pane, never more than this.
const EDGE_ZONE_CAP: f32 = 140.0;
/// The outer edge band of the whole workbench (root-level splits).
const OUTER_EDGE_BAND: f32 = 14.0;
/// Transport retries of one ambiguous mutation, with the same command ID.
const MUTATION_RETRIES: u8 = 2;

/// A tab's role in this workbench. Main is a view role: a promoted child
/// selected as its own workspace is Main here and still a child elsewhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TabRole {
    Main,
    Child,
    Side,
    Alongside,
}

impl TabRole {
    /// What the UI calls this role: every chat a main chat started is a
    /// Side chat, fresh (a child) or with its context (a fork).
    pub(super) fn display(self) -> &'static str {
        match self {
            Self::Main => "Main",
            Self::Child => "Side chat",
            Self::Side => "Side chat",
            Self::Alongside => "Alongside",
        }
    }

    /// The stable identifier QA readback and layout logs use (unchanged
    /// by renames of the user-facing name).
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Main => "Main",
            Self::Child => "Child",
            Self::Side => "Side",
            Self::Alongside => "Alongside",
        }
    }

    pub(super) fn icon(self) -> &'static str {
        match self {
            Self::Main => icons::HOME,
            Self::Child => icons::BOT,
            Self::Side => icons::FORK,
            Self::Alongside => icons::SPLIT_COLUMNS,
        }
    }

    fn pane_identity(self) -> PaneIdentity {
        match self {
            Self::Child => PaneIdentity::Child,
            Self::Side => PaneIdentity::Side,
            Self::Main | Self::Alongside => PaneIdentity::Alongside,
        }
    }
}

/// The dragged tab.
#[derive(Clone)]
pub(super) struct WorkbenchTabDrag {
    pub(super) tab: TabRef,
    pub(super) role: TabRole,
    pub(super) title: SharedString,
    pub(super) harness: Option<luna_proto::HarnessId>,
    /// The workbench this drag started in; a drag outliving a main switch
    /// or an engine replacement drops nothing.
    pub(super) main: String,
    pub(super) generation: u64,
    /// Set once the pointer travels [`DRAG_START_DISTANCE`] from the press;
    /// the ghost stays invisible until then and a shorter drag is a click.
    pub(super) armed: Rc<Cell<bool>>,
}

/// The dragged split divider.
pub(super) struct WorkbenchDividerDrag {
    pub(super) split_id: String,
}

/// A live drop preview: where the drop lands and the command it commits.
#[derive(Clone)]
pub(super) struct DropPreview {
    pub(super) target: DropTarget,
    pub(super) command: Command,
    pub(super) label: SharedString,
}

/// Bounds measured at paint, in window coordinates. Entries are replaced
/// every frame and pruned to the current layout, so pointer handlers between
/// frames (and the next render) read the last complete frame.
#[derive(Default)]
pub(super) struct Geometry {
    /// The scroll viewport: where workbench content is shown (inset inside
    /// the canvas's rounded hairline).
    pub(super) root: Option<Bounds<Pixels>>,
    /// The whole rounded canvas, gutter included.
    pub(super) canvas: Option<Bounds<Pixels>>,
    /// Each group's body by pane id (its tab strip is in the bottom row).
    pub(super) panes: HashMap<String, Bounds<Pixels>>,
    /// Each group's tab strip in the titlebar's tab row, by pane id.
    pub(super) strips: HashMap<String, Bounds<Pixels>>,
    /// Pane id -> `(chat id, tab bounds)` in strip order.
    pub(super) tabs: HashMap<String, Vec<(String, Bounds<Pixels>)>>,
    pub(super) splits: HashMap<String, Bounds<Pixels>>,
    /// Each strip's New child chat button (QA readback).
    pub(super) plus: HashMap<String, Bounds<Pixels>>,
    /// Each compact strip's Group actions button (QA readback); a strip has
    /// either this or `plus`, never both.
    pub(super) group_actions: HashMap<String, Bounds<Pixels>>,
    /// The open tab / Group actions menu's rows by id, as last painted
    /// (QA readback; empty while it is closed).
    pub(super) menu_rows: Vec<(&'static str, Bounds<Pixels>)>,
    /// The titlebar's tab row (the groups' tab strips).
    pub(super) tab_row: Option<Bounds<Pixels>>,
    /// The workbench notice's Dismiss control, while a notice shows.
    pub(super) notice_dismiss: Option<Bounds<Pixels>>,
    /// Each strip's visible tab viewport: tabs scrolled outside it are
    /// clipped for hit-testing and readback.
    pub(super) tab_view: HashMap<String, Bounds<Pixels>>,
    /// Each strip's room for its tabs and `+` (the viewport hugs the tabs,
    /// so this, not the viewport, bounds how wide one tab may get).
    pub(super) tab_slot: HashMap<String, Bounds<Pixels>>,
    /// Width of the pane showing Main, for the main composer's layout, and
    /// the center column width it was measured at (a different column width
    /// makes it stale).
    pub(super) main_width: Option<f32>,
    pub(super) main_width_for: Option<f32>,
}

impl Geometry {
    fn retain(&mut self, layout: &Layout) {
        let panes: HashSet<&str> = model::panes(layout).iter().map(|pane| pane.id).collect();
        self.panes.retain(|id, _| panes.contains(id.as_str()));
        self.strips.retain(|id, _| panes.contains(id.as_str()));
        self.tabs.retain(|id, _| panes.contains(id.as_str()));
        self.plus.retain(|id, _| panes.contains(id.as_str()));
        self.group_actions
            .retain(|id, _| panes.contains(id.as_str()));
        self.tab_view.retain(|id, _| panes.contains(id.as_str()));
        self.tab_slot.retain(|id, _| panes.contains(id.as_str()));
        self.splits.retain(|id, _| layout.root.node(id).is_some());
        if !has_main_visible(layout) {
            self.main_width = None;
            self.main_width_for = None;
        }
    }

    /// `pane_id`'s tabs clipped to what can be seen: the strip's viewport,
    /// within the titlebar's tab row. A tab scrolled out of view keeps its
    /// slot as a zero-width span at the edge it left by, so it can never be
    /// hit or mis-targeted.
    pub(super) fn visible_tabs(&self, pane_id: &str) -> Vec<(String, Bounds<Pixels>)> {
        let Some(tabs) = self.tabs.get(pane_id) else {
            return Vec::new();
        };
        let Some(view) = self.tab_view.get(pane_id) else {
            return tabs.clone();
        };
        let (mut view_left, mut view_right) = (f32::from(view.left()), f32::from(view.right()));
        if let Some(row) = self.tab_row {
            view_left = view_left.max(f32::from(row.left()));
            view_right = view_right.min(f32::from(row.right())).max(view_left);
        }
        tabs.iter()
            .map(|(chat, bounds)| {
                let left = f32::from(bounds.left()).clamp(view_left, view_right);
                let right = f32::from(bounds.right()).clamp(view_left, view_right);
                (
                    chat.clone(),
                    Bounds {
                        origin: gpui::point(px(left), bounds.origin.y),
                        size: gpui::size(px(right - left), bounds.size.height),
                    },
                )
            })
            .collect()
    }

    fn pane_at(&self, point: Point<Pixels>) -> Option<(String, Bounds<Pixels>)> {
        self.panes
            .iter()
            .find(|(_, bounds)| bounds.contains(&point))
            .map(|(id, bounds)| (id.clone(), *bounds))
    }

    /// The group whose strip (in the bottom row) holds `point`.
    fn strip_at(&self, point: Point<Pixels>) -> Option<String> {
        self.strips
            .iter()
            .find(|(_, bounds)| bounds.contains(&point))
            .map(|(id, _)| id.clone())
    }

    /// Whether `point` is somewhere a dragged tab can land: a group's body
    /// (the canvas viewport) or a group's strip.
    fn accepts_drop_at(&self, point: Point<Pixels>) -> bool {
        self.root.is_some_and(|root| root.contains(&point)) || self.strip_at(point).is_some()
    }
}

fn has_main_visible(layout: &Layout) -> bool {
    model::visible_chat_ids(layout).contains(&layout.main_chat_id)
}

/// A closed tab's exit: its chip narrows and fades where it was.
#[derive(Clone)]
pub(super) struct LeavingTab {
    pub(super) pane_id: String,
    pub(super) index: usize,
    pub(super) chat_id: String,
    pub(super) width: f32,
    pub(super) started: std::time::Instant,
}

/// A collapsed branch's exit: a ghost of the closed side shrinks toward its
/// outer edge while the surviving side already holds the full space.
#[derive(Clone)]
pub(super) struct Collapse {
    /// The closed side's last painted bounds (window coordinates).
    pub(super) bounds: Bounds<Pixels>,
    pub(super) toward: Edge,
    pub(super) started: std::time::Instant,
}

/// Exit durations (ease-out); reduced motion records none.
pub(super) const TAB_EXIT_MS: u64 = 150;
pub(super) const COLLAPSE_MS: u64 = 170;

struct Mutation {
    command_id: String,
    command: Command,
    main: String,
    generation: u64,
    profile: String,
    /// The exact request once sent: a transport retry resends it unchanged.
    request: Option<serde_json::Value>,
    attempts: u8,
    /// Focus this chat's composer once the engine accepts the change.
    then_focus: Option<String>,
}

struct TabView {
    view: Entity<PaneChatView>,
    _transcript_events: Subscription,
}

/// `+` (New child chat) in flight or failed: one gesture, one request key.
#[derive(Clone)]
pub(super) struct ChildRequest {
    pub(super) key: String,
    pub(super) main: String,
    pub(super) generation: u64,
    pub(super) pane_id: String,
    /// Open the child in a new group to the right of `pane_id` (the group's
    /// split control) rather than as a tab in it.
    pub(super) split: bool,
    /// The New thread's source: the active chat of the group it was asked
    /// from (Main only hosts the layout).
    pub(super) parent: String,
    /// The provider chosen in the creation chooser (`None`: the engine's
    /// default). Never an account: the provider's own default applies.
    pub(super) provider: Option<luna_proto::HarnessId>,
}

#[derive(Clone)]
pub(super) struct ChildFailure {
    pub(super) request: ChildRequest,
    pub(super) message: SharedString,
    /// The engine may have created it: Retry resends the same key.
    pub(super) ambiguous: bool,
}

#[derive(Clone)]
pub(super) struct TabMenu {
    pub(super) chat_id: String,
    pub(super) pane_id: String,
    pub(super) position: Point<Pixels>,
    /// Opened from a compact strip's Group actions: the group's own actions
    /// (`+`, split, close) lead the menu. `chat_id` may be empty (an empty
    /// group), in which case no tab actions are listed.
    pub(super) group: bool,
}

pub(super) struct WorkbenchState {
    /// The owner: the selected left-navigation chat ("" on the canvas).
    main: String,
    /// The execution focus this window last published to the shared
    /// `AppState` (`None`: not yet published).
    execution_focus_sent: Option<Option<String>>,
    generation: u64,
    profile: Option<String>,
    capable: bool,
    /// Why this engine cannot hold a layout for `main` (it said so).
    pub(super) unavailable: Option<SharedString>,
    /// The last transient load failure while retrying (Main still renders).
    pub(super) load_error: Option<SharedString>,
    /// The engine's committed layout for `main`.
    authoritative: Option<Layout>,
    /// `authoritative` plus every unacknowledged mutation, for display.
    optimistic: Option<Layout>,
    /// Last committed layout of other mains on this engine/profile.
    cache: HashMap<String, Layout>,
    pending: VecDeque<Mutation>,
    in_flight: Option<Mutation>,
    send_task: Option<Task<()>>,
    watch_task: Option<Task<()>>,
    watching: Option<(String, u64)>,
    views: HashMap<String, TabView>,
    /// Divider being dragged and its local ratio (not yet committed).
    pub(super) resize: Option<(String, f32)>,
    pub(super) preview: Option<DropPreview>,
    /// The chat whose tab is being dragged (dims the source tab).
    pub(super) dragging: Option<String>,
    /// Where the last tab press began: `(chat id, window point)`.
    press: Option<(String, Point<Pixels>)>,
    /// Where a divider press began (a release that no drag move reached
    /// resolves from here; a click without travel changes nothing).
    divider_press: Option<(String, Point<Pixels>)>,
    /// Escape since the last mouse press: the gesture in progress (armed or
    /// not, previewed or not) is cancelled and its release does nothing.
    pub(super) gesture_cancelled: bool,
    pub(super) geometry: Rc<RefCell<Geometry>>,
    pub(super) creating_child: Option<ChildRequest>,
    pub(super) child_failure: Option<ChildFailure>,
    /// A new New thread was refused while an earlier outcome is unknown
    /// (the failure card says why until its status is checked).
    pub(super) child_blocked: bool,
    child_task: Option<Task<()>>,
    pub(super) menu: popover::Popup<TabMenu>,
    pub(super) notice: Option<SharedString>,
    /// Tracks keyboard focus inside the canvas (Cmd-W scope).
    pub(super) focus: FocusHandle,
    /// Tracks keyboard focus on the tab row in the titlebar (the rest of the
    /// workbench's scope: see `workbench_holds_focus`).
    pub(super) title_focus: FocusHandle,
    /// This frame's tab row, built with the canvas
    /// (`render_workbench_tab_row`) and taken by the titlebar, which places
    /// it over the canvas's columns. `None` off the workbench.
    pub(super) title_tabs: Option<AnyElement>,
    /// A chat whose composer takes focus once its view mounts.
    focus_composer: Option<String>,
    /// A group (without a composer to focus) whose own focus scope takes
    /// keyboard focus next frame, so keys never stay in another group.
    pub(super) focus_pane: Option<String>,
    /// The `+` creation chooser, while open (see `chooser`).
    pub(super) chooser: Option<chooser::CreationChooser>,
    /// Keyboard focus for each tab (arrow-key navigation within a group).
    tab_focus: HashMap<String, FocusHandle>,
    /// The group and chat keyboard focus was last in, inside the canvas
    /// (kept so focus can stay in that tab when the element holding it
    /// disappears, e.g. a chat's composer when the chat is archived).
    keyboard_home: Option<(String, String)>,
    /// The composer-less tab a follow-up frame was already requested for
    /// (once per tab, not every frame).
    vanish_watch: Option<String>,
    /// Per pane: the tab width cap last rendered (the strip viewport's
    /// width, between the minimum and maximum tab widths).
    tab_caps: HashMap<String, f32>,
    /// The canvas scroll position when the saved layout is larger than the
    /// canvas, and what the focused group was last revealed for.
    canvas_scroll: gpui::ScrollHandle,
    canvas_reveal: Option<CanvasReveal>,
    /// Each pane's tab-strip scroll position (user scrolling is kept).
    tab_scroll: HashMap<String, gpui::ScrollHandle>,
    /// Per pane: what the active tab was last revealed for, and how many
    /// frames may still adjust the scroll to bring it into view.
    tab_reveal: HashMap<String, TabReveal>,
    /// Each pane's focus scope and its focus-in listener.
    pub(super) pane_focus: HashMap<String, (FocusHandle, gpui::Subscription)>,
    /// Each pane's tab strip focus scope (in the frame's bottom row, outside
    /// the pane) and its focus-in listener: a focused tab or strip control
    /// belongs to its group as much as anything in the pane does.
    pub(super) strip_focus: HashMap<String, (FocusHandle, gpui::Subscription)>,
    pub(super) leaving: Vec<LeavingTab>,
    pub(super) collapse: Vec<Collapse>,
    /// QA: park mutation completions to deliver them late.
    #[cfg(feature = "appshots-fixture")]
    hold_responses: bool,
    /// QA: the next New thread's response is lost after the engine
    /// handled it (an unknown outcome, as a dropped connection leaves).
    #[cfg(feature = "appshots-fixture")]
    lose_child_response: bool,
    /// QA: every shell Escape that reached the stop path, as
    /// (target chat, outcome), in order.
    #[cfg(feature = "appshots-fixture")]
    pub(super) escape_log: Vec<(Option<String>, &'static str)>,
    #[cfg(feature = "appshots-fixture")]
    held: Vec<(
        serde_json::Value,
        Result<model::MutationResult, luna_rpc::RpcError>,
    )>,
}

/// What the focused group was last scrolled into the canvas viewport for:
/// the focused group, and the groups with their active chats. Only an
/// explicit change (focus moves, a group opens or closes, a tab opens or is
/// reopened) reveals; a viewport or minimum-size change (a sidebar opening,
/// a window resize) keeps the current offset, so the layout stays anchored
/// at its left and the rightmost groups are the ones covered.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct CanvasReveal {
    key: (String, Vec<(String, Option<String>)>),
    pending: u8,
}

/// When a group's active tab was last brought into view: the tab order, the
/// active chat, the strip's width, the tab width cap and whether a closed
/// tab is still narrowing out of it. Any change re-reveals; the user's own
/// scrolling in between is left alone.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct TabReveal {
    key: (Vec<String>, String, i32, i32, bool),
    /// Frames left to settle the reveal (bounds are measured a frame late).
    pending: u8,
}

/// Re-render the Shell once this draw is over. A reveal or cap step settles
/// against bounds measured a frame late, but a `notify` while drawing only
/// marks the view dirty and requests no frame (GPUI ignores invalidation
/// mid-draw), so the next step would wait for an unrelated event and a strip
/// could stay scrolled past its active tab. The deferred notify runs after
/// the draw and schedules the frame.
fn notify_after_draw(cx: &mut Context<Shell>) {
    let shell = cx.entity_id();
    cx.defer(move |cx| cx.notify(shell));
}

impl WorkbenchState {
    pub(super) fn new(cx: &mut App) -> Self {
        Self {
            main: String::new(),
            execution_focus_sent: None,
            generation: 0,
            profile: None,
            capable: false,
            unavailable: None,
            load_error: None,
            authoritative: None,
            optimistic: None,
            cache: HashMap::new(),
            pending: VecDeque::new(),
            in_flight: None,
            send_task: None,
            watch_task: None,
            watching: None,
            views: HashMap::new(),
            resize: None,
            preview: None,
            dragging: None,
            press: None,
            divider_press: None,
            gesture_cancelled: false,
            geometry: Rc::new(RefCell::new(Geometry::default())),
            creating_child: None,
            child_failure: None,
            child_blocked: false,
            child_task: None,
            menu: popover::Popup::default(),
            notice: None,
            focus: cx.focus_handle(),
            title_focus: cx.focus_handle(),
            title_tabs: None,
            focus_composer: None,
            focus_pane: None,
            chooser: None,
            tab_focus: HashMap::new(),
            pane_focus: HashMap::new(),
            strip_focus: HashMap::new(),
            tab_scroll: HashMap::new(),
            canvas_scroll: gpui::ScrollHandle::new(),
            canvas_reveal: None,
            keyboard_home: None,
            vanish_watch: None,
            tab_caps: HashMap::new(),
            tab_reveal: HashMap::new(),
            leaving: Vec::new(),
            collapse: Vec::new(),
            #[cfg(feature = "appshots-fixture")]
            hold_responses: false,
            #[cfg(feature = "appshots-fixture")]
            lose_child_response: false,
            #[cfg(feature = "appshots-fixture")]
            escape_log: Vec::new(),
            #[cfg(feature = "appshots-fixture")]
            held: Vec::new(),
        }
    }

    pub(super) fn capable(&self) -> bool {
        self.capable && self.unavailable.is_none()
    }

    pub(super) fn main(&self) -> &str {
        &self.main
    }

    /// The layout to draw: optimistic, else committed, else Main alone.
    pub(super) fn layout(&self) -> Layout {
        self.optimistic
            .as_ref()
            .or(self.authoritative.as_ref())
            .cloned()
            .unwrap_or_else(|| Layout::new(self.main.clone()))
    }

    fn display(&self) -> Option<&Layout> {
        self.optimistic.as_ref().or(self.authoritative.as_ref())
    }

    /// Whether `chat_id` has a mounted tab view in this workbench.
    pub(super) fn has_view(&self, chat_id: &str) -> bool {
        self.views.contains_key(chat_id)
    }

    /// Whether the engine's layout for this main has arrived.
    pub(super) fn loaded(&self) -> bool {
        self.authoritative.is_some()
    }

    /// The engine's committed layout (never the optimistic one).
    pub(super) fn committed(&self) -> Option<&Layout> {
        self.authoritative.as_ref()
    }

    /// No layout mutation is queued or in flight: what shows is committed.
    pub(super) fn settled(&self) -> bool {
        self.pending.is_empty() && self.in_flight.is_none()
    }

    fn revision(&self) -> u64 {
        self.authoritative
            .as_ref()
            .map(|layout| layout.revision)
            .unwrap_or(0)
    }

    /// Re-derive the optimistic layout from the committed one plus the
    /// in-flight and queued commands. A queued command the reducer now
    /// refuses (its target changed underneath) is dropped, never forced.
    fn recompute_optimistic(&mut self) {
        let Some(base) = &self.authoritative else {
            self.optimistic = None;
            return;
        };
        let mut layout = base.clone();
        let mut changed = false;
        if let Some(mutation) = &self.in_flight
            && let Some((next, did)) = model::reduce(&layout, &mutation.command)
        {
            changed |= did;
            layout = next;
        }
        let mut kept = VecDeque::new();
        for mutation in self.pending.drain(..) {
            if let Some((next, did)) = model::reduce(&layout, &mutation.command) {
                changed |= did;
                layout = next;
                kept.push_back(mutation);
            }
        }
        self.pending = kept;
        self.optimistic = changed.then_some(layout);
    }

    fn reset_engine(&mut self) {
        self.unavailable = None;
        self.load_error = None;
        self.authoritative = None;
        self.optimistic = None;
        self.cache.clear();
        self.pending.clear();
        self.in_flight = None;
        self.send_task = None;
        self.watch_task = None;
        self.watching = None;
        self.views.clear();
        self.resize = None;
        self.preview = None;
        self.dragging = None;
        self.press = None;
        self.creating_child = None;
        self.child_failure = None;
        self.child_blocked = false;
        self.child_task = None;
        self.focus_composer = None;
        self.chooser = None;
    }
}

/// A chat's role in `main`'s workbench, and the chat its view names as the
/// parent (a child's actual parent; a side chat's source; the host for an
/// alongside chat). Never derived from tab position.
pub(super) fn tab_role(state: &AppState, main: &str, tab: &TabRef) -> (TabRole, String) {
    if tab.chat_id == main {
        return (TabRole::Main, String::new());
    }
    tab_origin(state, tab).unwrap_or_else(|| (TabRole::Alongside, main.to_string()))
}

/// Where a chat came from, by lineage alone (never placement, and whatever
/// workbench it is Main of): a Side chat of its source, a New thread
/// created from its parent, or nothing for a parentless chat.
fn tab_origin(state: &AppState, tab: &TabRef) -> Option<(TabRole, String)> {
    if let Some(parent) = &tab.side_parent_id {
        return Some((TabRole::Side, parent.clone()));
    }
    if let Some(row) = state.side_chat_rows.get(&tab.chat_id) {
        return Some((TabRole::Side, row.parent_id.clone()));
    }
    // A generic history fork carries its source's context too: it reads as
    // a Side chat of that source (lineage, never placement), while its tab
    // stays an ordinary chat tab with no side-parent hint.
    if let Some(source) = state.fork_source_rows.get(&tab.chat_id)
        && *source != tab.chat_id
    {
        return Some((TabRole::Side, source.clone()));
    }
    let chat = state.chats.iter().find(|chat| chat.id == tab.chat_id);
    chat.and_then(super::placement::lineage_parent_id)
        .map(|parent| (TabRole::Child, parent.to_string()))
}

/// A tab's help (its tooltip): the relationship to its source, from
/// lineage, and what context it carries. A side chat that copied recent
/// visible messages says so; absent metadata (older engines, generic forks)
/// adds nothing and never implies full native context.
///
/// The origin comes from lineage for every tab, Main included: a Side chat
/// or New thread opened as its own workbench's Main keeps its explanation
/// (and says it is Main); only a parentless Main reads as plain Main.
pub(super) fn tab_help(state: &AppState, main: &str, tab: &TabRef) -> (SharedString, SharedString) {
    let (role, _) = tab_role(state, main, tab);
    let is_main = role == TabRole::Main;
    let (role, parent) = match tab_origin(state, tab) {
        Some(origin) => origin,
        None => (role, main.to_string()),
    };
    let main_note = if is_main {
        " It is this workbench's Main and stays open."
    } else {
        ""
    };
    let title = tab_title(state, tab, role);
    let parent_name = match state.chats.iter().find(|chat| chat.id == parent) {
        Some(row) => row
            .title
            .as_deref()
            .map(transcript::single_line)
            .filter(|title| !title.trim().is_empty())
            .unwrap_or_else(|| "Untitled chat".into()),
        None => "a deleted chat".to_string(),
    };
    match role {
        TabRole::Main => (
            title,
            "Main · this workbench's own chat. It stays open.".into(),
        ),
        TabRole::Side => {
            let mut details = "Side chats carry the context of the parent thread. Context is captured when created; later parent messages are not copied.".to_string();
            if state
                .side_chat_context_modes
                .get(&tab.chat_id)
                .is_some_and(|mode| mode == "contextSnapshot")
            {
                details.push_str(" This side chat copied recent visible messages only.");
            }
            details.push_str(main_note);
            (format!("Side chat of {parent_name}").into(), details.into())
        }
        TabRole::Child => (
            format!("Side chat of {parent_name}").into(),
            format!("Started fresh: it does not carry the main chat's conversation.{main_note}")
                .into(),
        ),
        TabRole::Alongside => (
            title,
            "Alongside · your own chat, listed beside Main".into(),
        ),
    }
}

/// A tab's title: the chat's own title from the live row (a fork's too);
/// an untitled fork falls back to its first question.
pub(super) fn tab_title(state: &AppState, tab: &TabRef, role: TabRole) -> SharedString {
    let titled = state
        .chats
        .iter()
        .find(|chat| chat.id == tab.chat_id)
        .and_then(|chat| chat.title.as_deref())
        .map(transcript::single_line)
        .filter(|title| !title.trim().is_empty());
    if let Some(title) = titled {
        return title.into();
    }
    let asked = (role == TabRole::Side)
        .then(|| {
            crate::chat_binding::ChatBinding::Chat(tab.chat_id.clone())
                .transcript(state)
                .iter()
                .find(|entry| entry.role == luna_doc::MessageRole::User)
                .and_then(|entry| {
                    entry.parts.iter().find_map(|part| match part {
                        luna_doc::MessagePart::Text { text, .. } => {
                            Some(transcript::single_line(text))
                        }
                        _ => None,
                    })
                })
        })
        .flatten()
        .filter(|title| !title.trim().is_empty());
    asked
        .unwrap_or_else(|| match role {
            TabRole::Child => "Side chat".to_string(),
            TabRole::Side => "Side chat".to_string(),
            _ => "Untitled chat".to_string(),
        })
        .into()
}

impl Shell {
    /// The chord a configurable shortcut is bound to now, as a badge
    /// (`None` when unbound): tooltips and the palette show the user's own
    /// keys, never a static default.
    pub(super) fn shortcut_badge(&self, id: crate::settings::ShortcutId) -> Option<String> {
        let combo = self.settings.keymap.get(id);
        (!combo.is_empty()).then(|| crate::settings::badge_combo(combo))
    }

    /// Whether a workbench shortcut (tabs, groups, chooser, reopen) acts
    /// now: the chat route, a workbench, and no overlay owning the keyboard
    /// (palette, picker, chooser, dialogs keep their own keys).
    pub(super) fn workbench_keys_apply(&self, cx: &App) -> bool {
        matches!(self.route, Route::Chat)
            && !self.overlay_owns_keyboard(cx)
            && self.workbench_available()
    }

    /// Whether layout actions can reach the engine.
    pub(super) fn workbench_available(&self) -> bool {
        self.workbench.capable() && !self.workbench.main().is_empty()
    }

    /// The chat execution diagnostics describe: the focused group's active
    /// chat while this workbench is loaded for the selected chat, else `None`
    /// (the selected chat applies).
    pub(super) fn workbench_execution_focus(&self, cx: &App) -> Option<String> {
        if !self.workbench_ready()
            || self.workbench.main.is_empty()
            || self.state.read(cx).selected_chat.as_deref() != Some(self.workbench.main.as_str())
        {
            return None;
        }
        self.workbench
            .layout()
            .focused_chat_id()
            .map(str::to_string)
    }

    /// Keep `AppState::execution_focus` on the focused tab (Settings'
    /// Execution card and other diagnostics follow it, never Main).
    pub(super) fn sync_execution_focus(&mut self, cx: &mut Context<Self>) {
        let focus = self.workbench_execution_focus(cx);
        // Publish this window's focus when it changes, not on every render.
        // Every window's Shell shares one AppState: re-asserting a differing
        // value each render made two windows overwrite each other forever
        // (each write notifies the state, which re-renders the other window
        // — 100% CPU, and a synchronous loop in test flushes).
        if self.workbench.execution_focus_sent.as_ref() != Some(&focus) {
            self.workbench.execution_focus_sent = Some(focus.clone());
            if self.state.read(cx).execution_focus != focus {
                self.state.update(cx, |state, cx| {
                    state.execution_focus = focus;
                    cx.notify();
                });
            }
        }
        // The focused chat's work watch follows the same focus (and the
        // engine connection); unchanged, this is a comparison only.
        let stale = {
            let state = self.state.read(cx);
            let focus = state.execution_focus_chat();
            match (&state.task_work, focus) {
                (None, None) => false,
                (Some(view), Some(focus)) => {
                    view.task_id != focus || view.engine_generation != state.engine_generation()
                }
                _ => true,
            }
        };
        if stale {
            self.state.update(cx, |state, cx| state.sync_task_work(cx));
        }
    }

    fn workbench_ready(&self) -> bool {
        self.workbench_available() && self.workbench.loaded() && self.workbench.profile.is_some()
    }

    /// Retry loading the layout now (the notice's Retry; the automatic
    /// retry keeps running meanwhile).
    pub(super) fn retry_workbench_load(&mut self, cx: &mut Context<Self>) {
        self.workbench.unavailable = None;
        self.workbench.load_error = None;
        self.workbench.watch_task = None;
        self.workbench.watching = None;
        if self.workbench.capable && !self.workbench.main.is_empty() {
            self.start_workbench_watch(cx);
        }
        cx.notify();
    }

    fn workbench_unavailable_notice(&mut self, cx: &mut Context<Self>) {
        let message = if let Some(reason) = self.workbench.unavailable.clone() {
            reason
        } else if !self.workbench.capable() {
            "Tabs and splits need a newer engine. Update LUNA's engine, then try again.".into()
        } else if self.workbench.profile.is_none() {
            "Tabs and splits need an engine that identifies its profile.".into()
        } else {
            "The chat layout is still loading. Try again in a moment.".into()
        };
        self.sidebar_notice = Some(SidebarNotice::transient_status(message));
        cx.notify();
    }

    /// Follow the owner (selected chat), the engine and its profile. Called
    /// from `on_state_changed` after `active_chat` is updated.
    pub(super) fn reconcile_workbench(&mut self, cx: &mut Context<Self>) {
        let (generation, capable, profile) = {
            let state = self.state.read(cx);
            let engine = state.engine();
            (
                state.engine_generation(),
                engine.is_some_and(|engine| engine.engine_info().supports(model::CAPABILITY)),
                engine
                    .and_then(|engine| engine.engine_info().profile_id.clone())
                    .filter(|profile| !profile.is_empty()),
            )
        };
        let wb = &mut self.workbench;
        if wb.generation != generation || wb.profile != profile {
            wb.reset_engine();
            wb.generation = generation;
            wb.profile = profile;
        }
        wb.capable = capable;
        if wb.main != self.active_chat {
            let previous = std::mem::take(&mut wb.main);
            if let Some(layout) = wb.authoritative.take()
                && !previous.is_empty()
            {
                wb.cache.insert(previous, layout);
            }
            wb.main = self.active_chat.clone();
            wb.unavailable = None;
            wb.load_error = None;
            // The previous main's queue and in-flight request are its own:
            // drop them (and cancel the wait) so no late completion can
            // settle or pump this main's queue.
            wb.pending.clear();
            wb.in_flight = None;
            wb.send_task = None;
            wb.optimistic = None;
            wb.resize = None;
            wb.preview = None;
            wb.dragging = None;
            wb.child_failure = None;
            wb.child_blocked = false;
            wb.authoritative = wb.cache.remove(&wb.main);
            wb.watch_task = None;
            wb.watching = None;
            if wb.menu.begin_close() {
                popover::reap_popup(cx, |shell: &mut Self| &mut shell.workbench.menu);
            }
        }
        if self.workbench.capable
            && !self.workbench.main.is_empty()
            && self.workbench.watching.as_ref() != Some(&(self.workbench.main.clone(), generation))
        {
            self.start_workbench_watch(cx);
        }
        self.sync_workbench_views(cx);
    }

    fn start_workbench_watch(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let main = self.workbench.main.clone();
        let generation = self.workbench.generation;
        self.workbench.watching = Some((main.clone(), generation));
        self.workbench.watch_task = Some(cx.spawn(async move |this, cx| {
            const RETRY: Duration = Duration::from_secs(2);
            // A Main the engine does not host *yet* (a chat created a moment
            // ago, before its registry has it) is refused until it does:
            // keep asking, quietly at first, never latching a permanent
            // refusal. Only a refusal that outlasts the grace says so (with
            // Retry), and the next delivered layout clears it.
            const REFUSAL_GRACE: Duration = Duration::from_secs(6);
            const REFUSAL_BACKOFF: Duration = Duration::from_secs(1);
            const REFUSAL_BACKOFF_MAX: Duration = Duration::from_secs(10);
            let mut refusals: u32 = 0;
            let mut refused_since: Option<std::time::Instant> = None;
            let params = serde_json::to_value(luna_proto::GetChatWorkbenchRequest {
                main_chat_id: main.clone(),
            })
            .unwrap_or_else(|_| json!({ "mainChatId": main }));
            // Whether a frame still belongs to the current main and engine.
            let deliver = |this: &gpui::WeakEntity<Shell>,
                           cx: &mut gpui::AsyncApp,
                           layout: Layout| {
                matches!(
                    this.update(cx, |shell, cx| {
                        if shell.workbench.main != main || shell.workbench.generation != generation
                        {
                            return false;
                        }
                        shell.apply_workbench_snapshot(layout, cx);
                        true
                    }),
                    Ok(true)
                )
            };
            // A definitive refusal (an engine without the layout store, or
            // a main it cannot host) leaves Main alone, with the reason.
            let refuse = |this: &gpui::WeakEntity<Shell>,
                          cx: &mut gpui::AsyncApp,
                          reason: String| {
                this.update(cx, |shell, cx| {
                    if shell.workbench.main == main && shell.workbench.generation == generation {
                        shell.workbench.unavailable = Some(reason.into());
                        cx.notify();
                    }
                })
                .ok();
            };
            loop {
                // The current layout first: a watch need not repeat it.
                match engine
                    .client()
                    .call_as::<Layout>(methods::GET_CHAT_WORKBENCH, params.clone())
                    .await
                {
                    Ok(layout) => {
                        refusals = 0;
                        refused_since = None;
                        if !deliver(&this, cx, layout) {
                            return;
                        }
                    }
                    Err(luna_rpc::RpcError::UnknownMethod(_)) => {
                        refuse(&this, cx, "Tabs and splits need a newer engine.".into());
                        return;
                    }
                    Err(luna_rpc::RpcError::BadParams(reason)) => {
                        refuse(&this, cx, reason);
                        return;
                    }
                    Err(luna_rpc::RpcError::Failed(reason)) => {
                        refusals = refusals.saturating_add(1);
                        tracing::debug!(%reason, refusals, "chat workbench main not hosted yet; retrying");
                        let since = *refused_since.get_or_insert_with(std::time::Instant::now);
                        let show = since.elapsed() >= REFUSAL_GRACE;
                        let alive = this.update(cx, |shell, cx| {
                            if show
                                && shell.workbench.main == main
                                && shell.workbench.generation == generation
                            {
                                shell.workbench.load_error =
                                    Some(format!("{reason}. Retrying…").into());
                                cx.notify();
                            }
                        });
                        if alive.is_err() {
                            return;
                        }
                        let backoff = REFUSAL_BACKOFF
                            .saturating_mul(refusals)
                            .min(REFUSAL_BACKOFF_MAX);
                        cx.background_executor().timer(backoff).await;
                        continue;
                    }
                    Err(error) => {
                        tracing::debug!(%error, "chat workbench unavailable; retrying");
                        let message: SharedString =
                            format!("Couldn't load this chat's layout ({error}). Retrying…").into();
                        let alive = this.update(cx, |shell, cx| {
                            if shell.workbench.main == main
                                && shell.workbench.generation == generation
                            {
                                shell.workbench.load_error = Some(message);
                                cx.notify();
                            }
                        });
                        if alive.is_err() {
                            return;
                        }
                        cx.background_executor().timer(RETRY).await;
                        continue;
                    }
                }
                let mut subscription = match engine
                    .client()
                    .subscribe_checked(methods::WATCH_CHAT_WORKBENCH, params.clone())
                    .await
                {
                    Ok(subscription) => subscription,
                    Err(error) => {
                        tracing::debug!(%error, "chat workbench watch unavailable; retrying");
                        if this.update(cx, |_, _| {}).is_err() {
                            return;
                        }
                        cx.background_executor().timer(RETRY).await;
                        continue;
                    }
                };
                while let Some(value) = subscription.recv().await {
                    match serde_json::from_value::<Layout>(value) {
                        Ok(layout) => {
                            if !deliver(&this, cx, layout) {
                                return;
                            }
                        }
                        Err(error) => {
                            tracing::warn!(%error, "dropping an unreadable chat workbench frame");
                        }
                    }
                }
                cx.background_executor().timer(RETRY).await;
            }
        }));
    }

    /// A committed snapshot (watch frame or mutation result) for the
    /// current main. Older revisions never replace newer ones.
    fn apply_workbench_snapshot(&mut self, layout: Layout, cx: &mut Context<Self>) {
        let wb = &mut self.workbench;
        if layout.main_chat_id != wb.main {
            return;
        }
        wb.load_error = None;
        if wb
            .authoritative
            .as_ref()
            .is_some_and(|current| current.revision > layout.revision)
        {
            return;
        }
        wb.authoritative = Some(layout);
        wb.recompute_optimistic();
        self.sync_workbench_views(cx);
        cx.notify();
    }

    /// Validate `command` with the shared reducer, show it optimistically and
    /// queue it. `false` when it is refused or changes nothing (no request).
    pub(super) fn workbench_mutate(&mut self, command: Command, cx: &mut Context<Self>) -> bool {
        self.workbench_mutate_then(command, None, cx)
    }

    fn workbench_mutate_then(
        &mut self,
        command: Command,
        then_focus: Option<String>,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.workbench_ready() {
            self.workbench_unavailable_notice(cx);
            return false;
        }
        let Some(base) = self.workbench.display() else {
            return false;
        };
        let Some((next, changed)) = model::reduce(base, &command) else {
            return false;
        };
        if !changed {
            // Valid but a no-op (same position, already focused).
            if let Some(chat) = then_focus {
                self.workbench_focus_composer(chat, cx);
            }
            return false;
        }
        if !self.reduced_motion {
            let before = base.clone();
            self.record_workbench_exits(&before, &next);
        }
        let wb = &mut self.workbench;
        let Some(profile) = wb.profile.clone() else {
            return false;
        };
        wb.pending.push_back(Mutation {
            command_id: uuid::Uuid::new_v4().to_string(),
            command,
            main: wb.main.clone(),
            generation: wb.generation,
            profile,
            request: None,
            attempts: 0,
            then_focus,
        });
        wb.recompute_optimistic();
        self.sync_workbench_views(cx);
        self.pump_workbench(cx);
        cx.notify();
        true
    }

    /// Closed tabs and collapsed branches between two layouts, captured from
    /// the last painted geometry for their exit motion.
    fn record_workbench_exits(&mut self, before: &Layout, after: &Layout) {
        let now = std::time::Instant::now();
        let geometry = self.workbench.geometry.borrow();
        for pane in model::panes(before) {
            for (index, tab) in pane.tabs.iter().enumerate() {
                if after.contains_chat(&tab.chat_id) {
                    continue;
                }
                // Only a tab whose group survives narrows in place.
                if model::pane(after, pane.id).is_none() {
                    continue;
                }
                let width = geometry
                    .tabs
                    .get(pane.id)
                    .and_then(|tabs| tabs.iter().find(|(chat, _)| chat == &tab.chat_id))
                    .map(|(_, bounds)| f32::from(bounds.size.width))
                    .unwrap_or(0.0);
                if width <= 0.0 {
                    continue;
                }
                self.workbench.leaving.push(LeavingTab {
                    pane_id: pane.id.to_string(),
                    index,
                    chat_id: tab.chat_id.clone(),
                    width,
                    started: now,
                });
            }
        }
        // A split that disappeared: the side that left is the one whose
        // node is gone from the new tree.
        fn splits<'a>(node: &'a model::Node, out: &mut Vec<&'a model::Node>) {
            if let model::Node::Split { first, second, .. } = node {
                out.push(node);
                splits(first, out);
                splits(second, out);
            }
        }
        let mut gone = Vec::new();
        splits(&before.root, &mut gone);
        for node in gone {
            let model::Node::Split {
                id,
                axis,
                first,
                second,
                ..
            } = node
            else {
                continue;
            };
            if after.root.node(id).is_some() {
                continue;
            }
            let (closed, closed_first) = if after.root.node(first.id()).is_none() {
                (first, true)
            } else if after.root.node(second.id()).is_none() {
                (second, false)
            } else {
                continue;
            };
            let bounds = geometry
                .panes
                .get(closed.id())
                .or_else(|| geometry.splits.get(closed.id()))
                .copied();
            let Some(bounds) = bounds else {
                continue;
            };
            let toward = match (axis, closed_first) {
                (model::Axis::Horizontal, true) => Edge::Left,
                (model::Axis::Horizontal, false) => Edge::Right,
                (model::Axis::Vertical, true) => Edge::Top,
                (model::Axis::Vertical, false) => Edge::Bottom,
            };
            self.workbench.collapse.push(Collapse {
                bounds,
                toward,
                started: now,
            });
        }
    }

    /// Drop finished exits; whether any is still playing.
    pub(super) fn prune_workbench_exits(&mut self) -> bool {
        let tab = Duration::from_millis(TAB_EXIT_MS);
        let collapse = Duration::from_millis(COLLAPSE_MS);
        self.workbench
            .leaving
            .retain(|leaving| leaving.started.elapsed() < tab);
        self.workbench
            .collapse
            .retain(|exit| exit.started.elapsed() < collapse);
        !self.workbench.leaving.is_empty() || !self.workbench.collapse.is_empty()
    }

    /// Send the next queued mutation once the previous one settled.
    fn pump_workbench(&mut self, cx: &mut Context<Self>) {
        if self.workbench.in_flight.is_some() {
            return;
        }
        let Some(mut mutation) = self.workbench.pending.pop_front() else {
            return;
        };
        let request = luna_proto::MutateChatWorkbenchRequest {
            main_chat_id: mutation.main.clone(),
            profile_id: mutation.profile.clone(),
            expected_revision: self.workbench.revision(),
            command_id: mutation.command_id.clone(),
            command: mutation.command.clone(),
        };
        let Ok(request) = serde_json::to_value(&request) else {
            self.workbench.recompute_optimistic();
            return;
        };
        mutation.request = Some(request.clone());
        self.workbench.in_flight = Some(mutation);
        self.send_workbench_request(request, Duration::ZERO, cx);
    }

    fn send_workbench_request(
        &mut self,
        request: serde_json::Value,
        delay: Duration,
        cx: &mut Context<Self>,
    ) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.workbench.in_flight = None;
            self.workbench.recompute_optimistic();
            return;
        };
        self.workbench.send_task = Some(cx.spawn(async move |this, cx| {
            if !delay.is_zero() {
                cx.background_executor().timer(delay).await;
            }
            let result = engine
                .client()
                .call_as::<model::MutationResult>(methods::MUTATE_CHAT_WORKBENCH, request.clone())
                .await;
            this.update(cx, |shell, cx| {
                #[cfg(feature = "appshots-fixture")]
                if shell.workbench.hold_responses {
                    shell.workbench.held.push((request, result));
                    cx.notify();
                    return;
                }
                shell.finish_workbench_mutation(request, result, cx)
            })
            .ok();
        }));
    }

    fn finish_workbench_mutation(
        &mut self,
        request: serde_json::Value,
        result: Result<model::MutationResult, luna_rpc::RpcError>,
        cx: &mut Context<Self>,
    ) {
        let wb = &mut self.workbench;
        // Only the completion of the request in flight settles it. A late
        // callback (an earlier main, profile or engine, or a request already
        // replaced) leaves the current queue, task and layout untouched.
        if wb
            .in_flight
            .as_ref()
            .and_then(|mutation| mutation.request.as_ref())
            != Some(&request)
        {
            return;
        }
        let Some(mutation) = wb.in_flight.take() else {
            return;
        };
        let current = mutation.generation == wb.generation
            && wb.profile.as_deref() == Some(mutation.profile.as_str())
            && mutation.main == wb.main;
        if !current {
            // Its own request, but the owner changed meanwhile: it belongs to
            // that other workbench (the engine applied it there, if at all).
            wb.send_task = None;
            wb.recompute_optimistic();
            self.pump_workbench(cx);
            cx.notify();
            return;
        }
        match result {
            Ok(result) => {
                if result.conflict {
                    // Another window changed the layout: show the engine's
                    // and drop gestures computed against the old one.
                    wb.pending.clear();
                    wb.notice = Some("The layout changed in another window.".into());
                }
                self.apply_workbench_snapshot(result.snapshot, cx);
                if !result.conflict
                    && let Some(chat) = mutation.then_focus
                {
                    self.workbench_focus_composer(chat, cx);
                }
            }
            Err(luna_rpc::RpcError::Transport(_) | luna_rpc::RpcError::Closed)
                if mutation.attempts < MUTATION_RETRIES =>
            {
                // Unknown outcome: resend the exact request; the engine's
                // command ledger applies it at most once.
                let attempts = mutation.attempts + 1;
                wb.in_flight = Some(Mutation {
                    attempts,
                    ..mutation
                });
                self.send_workbench_request(
                    request,
                    Duration::from_millis(400 * u64::from(attempts)),
                    cx,
                );
                return;
            }
            Err(error) => {
                wb.pending.clear();
                wb.notice = Some(format!("Couldn't change the layout: {error}").into());
                wb.recompute_optimistic();
            }
        }
        self.workbench.send_task = None;
        self.sync_workbench_views(cx);
        self.pump_workbench(cx);
        cx.notify();
    }

    // ---- views ----

    /// Keep one view per open non-main chat and drop views of closed ones
    /// (their composers stay cached in `PaneComposers`).
    pub(super) fn sync_workbench_views(&mut self, cx: &mut Context<Self>) {
        let layout = self.workbench.layout();
        let open: HashSet<String> = model::open_chat_ids(&layout)
            .into_iter()
            .filter(|id| *id != layout.main_chat_id)
            .collect();
        self.workbench.views.retain(|id, _| open.contains(id));
        let main = layout.main_chat_id.clone();
        self.workbench
            .tab_focus
            .retain(|id, _| open.contains(id) || *id == main);
        let panes: HashSet<String> = model::panes(&layout)
            .into_iter()
            .map(|pane| pane.id.to_string())
            .collect();
        self.workbench.pane_focus.retain(|id, _| panes.contains(id));
        self.workbench
            .strip_focus
            .retain(|id, _| panes.contains(id));
        self.workbench.tab_scroll.retain(|id, _| panes.contains(id));
        self.workbench.tab_caps.retain(|id, _| panes.contains(id));
        self.workbench.tab_reveal.retain(|id, _| panes.contains(id));
        // A tab that stopped rendering its composer (its chat was archived)
        // must not keep an invisible popover open that would own the
        // keyboard.
        let app: &App = cx;
        let hidden: Vec<Entity<Composer>> = self
            .workbench
            .views
            .keys()
            .filter(|id| self.workbench_tab_composer(id, app).is_none())
            .filter_map(|id| self.pane_composers.read(app).get(id))
            .collect();
        for composer in hidden {
            let pickers = composer.read(cx).pickers().clone();
            pickers.update(cx, |pickers, cx| {
                pickers.dismiss_open(cx);
            });
        }
        if let Some(chat) = self.workbench.focus_composer.clone()
            && model::visible_chat_ids(&layout).contains(&chat)
        {
            self.workbench_focus_composer(chat, cx);
        }
    }

    /// The view for a non-main tab, created on first display.
    pub(super) fn workbench_view(
        &mut self,
        tab: &TabRef,
        cx: &mut Context<Self>,
    ) -> Entity<PaneChatView> {
        let (role, parent) = tab_role(self.state.read(cx), &self.workbench.main, tab);
        if let Some(existing) = self.workbench.views.get(&tab.chat_id) {
            let view = existing.view.clone();
            view.update(cx, |view, cx| {
                view.set_identity(role.pane_identity(), parent, cx)
            });
            return view;
        }
        let composer = self
            .pane_composers
            .update(cx, |composers, cx| composers.composer(&tab.chat_id, cx));
        let links = Self::session_links(Some(tab.chat_id.clone()), cx);
        let state = self.state.clone();
        let chat_id = tab.chat_id.clone();
        let view = cx.new(|cx| {
            PaneChatView::new_embedded(
                state,
                chat_id,
                role.pane_identity(),
                parent,
                composer,
                links,
                cx,
            )
        });
        let transcript = view.read(cx).transcript().clone();
        let transcript_events = cx.subscribe(&transcript, Self::on_transcript_event);
        self.workbench.views.insert(
            tab.chat_id.clone(),
            TabView {
                view: view.clone(),
                _transcript_events: transcript_events,
            },
        );
        view
    }

    /// Width of the pane showing Main while the workbench is split.
    pub(super) fn workbench_main_pane_width(&self, column_width: f32) -> Option<f32> {
        if !model::has_split(&self.workbench.layout()) {
            return None;
        }
        self.workbench_main_width_now(column_width)
    }

    /// Main's pane width for a center column `column_width` wide: the last
    /// measurement while it was taken at this column width, else Main's
    /// saved share of the current viewport (a resize never keeps the
    /// previous width; the next frame's measurement then refines it).
    pub(super) fn workbench_main_width_now(&self, column_width: f32) -> Option<f32> {
        {
            let geometry = self.workbench.geometry.borrow();
            if let (Some(width), Some(at)) = (geometry.main_width, geometry.main_width_for)
                && (at - column_width).abs() < 0.5
            {
                return Some(width);
            }
        }
        let layout = self.workbench.layout();
        let pane = model::panes(&layout)
            .into_iter()
            .find(|pane| pane.active_chat_id == Some(layout.main_chat_id.as_str()))?;
        let share = render::pane_width_share(&layout.root, pane.id)?;
        Some(render::viewport_width_in(column_width) * share)
    }

    /// The composer that sends for `chat_id` in this workbench.
    pub(super) fn workbench_composer(&self, chat_id: &str, cx: &App) -> Option<Entity<Composer>> {
        if chat_id == self.workbench.main {
            return Some(self.composer.clone());
        }
        self.pane_composers.read(cx).get(chat_id)
    }

    /// The composer `chat_id`'s tab actually renders, if any: Main's own, or
    /// a tab's live view composer. An archived chat (read-only strip) or a
    /// tab without a mounted view has none, so no key, picker or Stop may
    /// reach a hidden composer.
    pub(super) fn workbench_tab_composer(
        &self,
        chat_id: &str,
        cx: &App,
    ) -> Option<Entity<Composer>> {
        if chat_id == self.workbench.main {
            return Some(self.composer.clone());
        }
        if !self.workbench.views.contains_key(chat_id) {
            return None;
        }
        let archived = self
            .state
            .read(cx)
            .chats
            .iter()
            .find(|chat| chat.id == chat_id)
            .is_some_and(|chat| chat.archived);
        if archived {
            return None;
        }
        self.pane_composers.read(cx).get(chat_id)
    }

    /// The chat and composer that own composer-scoped keys (the model
    /// picker, its slot keys, Escape stop).
    ///
    /// - Focus outside the canvas (sidebars, right surfaces, terminal,
    ///   browser), or no workbench: Main's, as on the single-chat route
    ///   (`Some((selected chat, Main's composer))`; the chat is `None` on a
    ///   new-chat draft).
    /// - Focus inside the canvas: the composer holding focus, else the pane
    ///   holding focus, else the focused group's tab — and only if that tab
    ///   renders a composer. A focused tab without one (archived, empty
    ///   group) gives `None`: the keys do nothing there and
    ///   never fall back to Main.
    pub(super) fn keyboard_composer(
        &self,
        window: &Window,
        cx: &App,
    ) -> Option<(Option<String>, Entity<Composer>)> {
        if !matches!(self.route, Route::Chat)
            || self.workbench.main.is_empty()
            || !self.workbench_keyboard_in_canvas(window, cx)
        {
            return Some((
                self.state.read(cx).selected_chat.clone(),
                self.composer.clone(),
            ));
        }
        if !self.workbench_holds_focus(window, cx) {
            // Focus vanished from a tab that no longer renders a composer:
            // the keys belong to that tab, which has none.
            return None;
        }
        let layout = self.workbench.layout();
        let visible = model::visible_chat_ids(&layout);
        let holding = visible.iter().find(|chat| {
            self.workbench_tab_composer(chat, cx)
                .is_some_and(|composer| {
                    gpui::Focusable::focus_handle(composer.read(cx), cx)
                        .contains_focused(window, cx)
                })
        });
        let in_pane = || {
            model::panes(&layout).into_iter().find_map(|pane| {
                self.workbench_group_holds_focus(pane.id, window, cx)
                    .then(|| pane.active_chat_id.map(str::to_string))
            })
        };
        let chat = match holding {
            Some(chat) => Some(chat.clone()),
            None => match in_pane() {
                Some(active) => active,
                None => layout.focused_chat_id().map(str::to_string),
            },
        }?;
        if !visible.contains(&chat) {
            return None;
        }
        let composer = self.workbench_tab_composer(&chat, cx)?;
        Some((Some(chat), composer))
    }

    /// Main's pickers and every visible workbench tab's that renders its
    /// composer: each can hold an open popover that owns the keyboard.
    /// Main's composer and every visible tab's composer.
    pub(super) fn visible_composers(&self, cx: &App) -> Vec<Entity<Composer>> {
        let mut composers = vec![self.composer.clone()];
        if self.workbench.main.is_empty() {
            return composers;
        }
        for chat in model::visible_chat_ids(&self.workbench.layout()) {
            if chat == self.workbench.main {
                continue;
            }
            if let Some(composer) = self.workbench_tab_composer(&chat, cx) {
                composers.push(composer);
            }
        }
        composers
    }

    pub(super) fn visible_pickers(&self, cx: &App) -> Vec<Entity<crate::pickers::Pickers>> {
        let mut pickers = vec![self.composer.read(cx).pickers().clone()];
        if self.workbench.main.is_empty() {
            return pickers;
        }
        for chat in model::visible_chat_ids(&self.workbench.layout()) {
            if chat == self.workbench.main {
                continue;
            }
            if let Some(composer) = self.workbench_tab_composer(&chat, cx) {
                pickers.push(composer.read(cx).pickers().clone());
            }
        }
        pickers
    }

    /// A pane's focus scope (its whole body), created with a focus-in
    /// listener: keyboard focus entering a pane by any route (Tab, a click,
    /// a programmatic focus) makes it the focused group, so Cmd-W, the
    /// frame's Git context and New child follow the pane the user is in.
    pub(super) fn workbench_pane_focus(
        &mut self,
        pane_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> FocusHandle {
        if let Some((handle, _)) = self.workbench.pane_focus.get(pane_id) {
            return handle.clone();
        }
        let handle = cx.focus_handle();
        let id = pane_id.to_string();
        let subscription = cx.on_focus_in(&handle, window, move |shell, _, cx| {
            shell.workbench_pane_focused(&id, cx);
        });
        self.workbench
            .pane_focus
            .insert(pane_id.to_string(), (handle.clone(), subscription));
        handle
    }

    /// A pane's tab strip focus scope (see [`WorkbenchState::strip_focus`]),
    /// created with the same focus-in listener as the pane's own.
    pub(super) fn workbench_strip_focus(
        &mut self,
        pane_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> FocusHandle {
        if let Some((handle, _)) = self.workbench.strip_focus.get(pane_id) {
            return handle.clone();
        }
        let handle = cx.focus_handle();
        let id = pane_id.to_string();
        let subscription = cx.on_focus_in(&handle, window, move |shell, _, cx| {
            shell.workbench_pane_focused(&id, cx);
        });
        self.workbench
            .strip_focus
            .insert(pane_id.to_string(), (handle.clone(), subscription));
        handle
    }

    /// Whether keyboard focus is in the workbench: its canvas, or its tab
    /// row in the titlebar.
    pub(super) fn workbench_holds_focus(&self, window: &Window, cx: &App) -> bool {
        self.workbench.focus.contains_focused(window, cx)
            || self.workbench.title_focus.contains_focused(window, cx)
    }

    /// Whether keyboard focus is in `pane_id`'s group: in its pane, or on
    /// its tab strip in the titlebar.
    pub(super) fn workbench_group_holds_focus(
        &self,
        pane_id: &str,
        window: &Window,
        cx: &App,
    ) -> bool {
        [&self.workbench.pane_focus, &self.workbench.strip_focus]
            .iter()
            .any(|scopes| {
                scopes
                    .get(pane_id)
                    .is_some_and(|(handle, _)| handle.contains_focused(window, cx))
            })
    }

    /// The canvas's scroll handle, first scrolling it so the focused group is
    /// in the viewport whenever focus moves to another group, a group opens
    /// or closes, or a group's active chat changes (open, reopen). A
    /// viewport or minimum-size change alone never scrolls: the offset (left
    /// anchored by default, or where the user put it) stands, clamped only
    /// when the content shrank. Nothing moves while something is dragged or a
    /// divider is resized.
    pub(super) fn workbench_canvas_scroll(
        &mut self,
        layout: &Layout,
        cx: &mut Context<Self>,
    ) -> gpui::ScrollHandle {
        let handle = self.workbench.canvas_scroll.clone();
        // Content that shrank (a wider window, a group closed) cannot leave
        // the offset past its end; otherwise the offset stands.
        let (max, offset) = (handle.max_offset(), handle.offset());
        let clamped = gpui::point(
            px(f32::from(offset.x).clamp(-f32::from(max.x).max(0.0), 0.0)),
            px(f32::from(offset.y).clamp(-f32::from(max.y).max(0.0), 0.0)),
        );
        if clamped != offset {
            handle.set_offset(clamped);
        }
        let geometry = self.workbench.geometry.clone();
        let geometry = geometry.borrow();
        let Some(view) = geometry.root else {
            return handle;
        };
        let groups = model::panes(layout)
            .into_iter()
            .map(|pane| (pane.id.to_string(), pane.active_chat_id.map(str::to_string)))
            .collect();
        let key = (layout.focused_pane_id.clone(), groups);
        // A fresh view starts anchored at the left: nothing to reveal yet.
        let reveal = self
            .workbench
            .canvas_reveal
            .get_or_insert_with(|| CanvasReveal {
                key: key.clone(),
                pending: 0,
            });
        if reveal.key != key {
            reveal.key = key;
            reveal.pending = 4;
        }
        if reveal.pending == 0
            || self.workbench.dragging.is_some()
            || self.workbench.resize.is_some()
            || cx.has_active_drag()
        {
            return handle;
        }
        let Some(pane) = geometry.panes.get(&layout.focused_pane_id).copied() else {
            reveal.pending -= 1;
            notify_after_draw(cx);
            return handle;
        };
        // The shift that brings `[start, end)` into `[view_start, view_end)`,
        // showing its start when it is larger than the viewport.
        let shift = |start: f32, end: f32, view_start: f32, view_end: f32| {
            if start < view_start {
                view_start - start
            } else if end > view_end {
                (view_end - end).max(view_start - start)
            } else {
                0.0
            }
        };
        let dx = shift(
            f32::from(pane.left()),
            f32::from(pane.right()),
            f32::from(view.left()),
            f32::from(view.right()),
        );
        let dy = shift(
            f32::from(pane.top()),
            f32::from(pane.bottom()),
            f32::from(view.top()),
            f32::from(view.bottom()),
        );
        if dx.abs() < 0.5 && dy.abs() < 0.5 {
            reveal.pending = 0;
            return handle;
        }
        let max = handle.max_offset();
        let mut offset = handle.offset();
        offset.x = px((f32::from(offset.x) + dx).clamp(-f32::from(max.x).max(0.0), 0.0));
        offset.y = px((f32::from(offset.y) + dy).clamp(-f32::from(max.y).max(0.0), 0.0));
        handle.set_offset(offset);
        reveal.pending -= 1;
        notify_after_draw(cx);
        handle
    }

    /// The tab strip's scroll handle for `pane`, first scrolling it so the
    /// active tab is wholly inside the strip's viewport whenever the active
    /// tab, the tab order or the strip width changed (a new, reopened or
    /// keyboard-selected tab past the edge). Between those changes the
    /// user's own scroll position stands, and nothing moves while a tab is
    /// dragged (the reveal waits for the drop).
    pub(super) fn workbench_tab_scroll(
        &mut self,
        pane: model::PaneRef<'_>,
        cx: &mut Context<Self>,
    ) -> gpui::ScrollHandle {
        let id = pane.id.to_string();
        let handle = self
            .workbench
            .tab_scroll
            .entry(id.clone())
            .or_default()
            .clone();
        let Some(active) = pane.active_chat_id.map(str::to_string) else {
            return handle;
        };
        let geometry = self.workbench.geometry.clone();
        let geometry = geometry.borrow();
        let view = geometry.tab_view.get(&id).copied();
        let width = view.map_or(0, |view| f32::from(view.size.width).round() as i32);
        // The tab width cap too: a narrower cap (a compact strip, a resize)
        // moves every tab even when the viewport kept its width.
        let cap = self
            .workbench
            .tab_caps
            .get(&id)
            .map_or(0, |cap| cap.round() as i32);
        // A closed tab's ghost still narrowing in this strip moves every tab
        // after it on each frame: the reveal stays live until it is gone
        // instead of settling on a position the next frame changes, and its
        // end (part of the key) re-reveals against the final layout.
        let exiting = self
            .workbench
            .leaving
            .iter()
            .any(|leaving| leaving.pane_id == id);
        let key = (
            pane.tabs.iter().map(|tab| tab.chat_id.clone()).collect(),
            active.clone(),
            width,
            cap,
            exiting,
        );
        let reveal = self
            .workbench
            .tab_reveal
            .entry(id.clone())
            .or_insert_with(|| TabReveal {
                key: key.clone(),
                pending: 4,
            });
        if reveal.key != key {
            // The bounds on hand were measured for the old tabs, order or
            // width (a closed tab's neighbour still where it was): act on
            // next frame's measurement, not on these.
            reveal.key = key;
            reveal.pending = 4;
            notify_after_draw(cx);
            return handle;
        }
        // A tab or Related row in flight: the reveal waits for its drop.
        if reveal.pending == 0 || self.workbench.dragging.is_some() || cx.has_active_drag() {
            return handle;
        }
        let tab = geometry
            .tabs
            .get(&id)
            .and_then(|tabs| tabs.iter().find(|(chat, _)| *chat == active))
            .map(|(_, bounds)| *bounds)
            .filter(|bounds| bounds.size.width > px(0.0));
        let (Some(view), Some(tab)) = (view, tab) else {
            // Not measured yet (a tab that just mounted): look next frame.
            if !exiting {
                reveal.pending -= 1;
            }
            notify_after_draw(cx);
            return handle;
        };
        let mut offset = handle.offset();
        let (left, right) = (f32::from(tab.left()), f32::from(tab.right()));
        let (view_left, view_right) = (f32::from(view.left()), f32::from(view.right()));
        let shift = if left < view_left {
            view_left - left
        } else if right > view_right {
            // A tab wider than the viewport shows its start (title, status).
            (view_right - right).max(view_left - left)
        } else {
            0.0
        };
        if shift.abs() < 0.5 {
            // Settled only once nothing in the strip is still moving (the
            // exit animation requests its own frames until then).
            if !exiting {
                reveal.pending = 0;
            }
            return handle;
        }
        let max = f32::from(handle.max_offset().x).max(0.0);
        offset.x = px((f32::from(offset.x) + shift).clamp(-max, 0.0));
        handle.set_offset(offset);
        if !exiting {
            reveal.pending -= 1;
        }
        notify_after_draw(cx);
        handle
    }

    /// The widest a tab in `pane_id` may be: its strip viewport (as measured
    /// last frame), within the minimum and maximum tab widths, so the active
    /// tab's label and close control always fit inside the strip instead of
    /// running under its trailing controls. A changed cap re-renders once so
    /// the new width settles in the same strip.
    pub(super) fn workbench_tab_cap(
        &mut self,
        pane_id: &str,
        min: f32,
        max: f32,
        reserved: f32,
        cx: &mut Context<Self>,
    ) -> f32 {
        // The strip's room for tabs (less `reserved`, what follows them: `+`
        // and its gap), never the viewport itself, which hugs the tabs.
        let room = self
            .workbench
            .geometry
            .borrow()
            .tab_slot
            .get(pane_id)
            .map(|slot| f32::from(slot.size.width) - reserved);
        // A hair under the room: tab gaps and rounding never overflow it.
        let cap = room
            .map_or(max, |width| (width - 4.0).clamp(min, max))
            .floor();
        if self.workbench.tab_caps.insert(pane_id.to_string(), cap) != Some(cap) {
            notify_after_draw(cx);
        }
        cap
    }

    /// Keep keyboard focus in the canvas when the element holding it
    /// disappears from under it: archiving a chat swaps the tab's composer
    /// for read-only content.
    /// Focus would otherwise land nowhere, and composer-scoped keys would
    /// fall back to Main's composer. It moves to that tab's own tab chip, so
    /// the group stays focused and those keys do nothing there. A real move
    /// elsewhere (sidebar, right pane, terminal) holds live focus and is
    /// left alone.
    pub(super) fn workbench_keep_keyboard_home(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.workbench.main.is_empty() {
            return;
        }
        let layout = self.workbench.layout();
        let app: &App = cx;
        if self.workbench_holds_focus(window, app) {
            let home = model::panes(&layout).into_iter().find_map(|pane| {
                let chat = pane.active_chat_id?;
                self.workbench_group_holds_focus(pane.id, window, app)
                    .then(|| (pane.id.to_string(), chat.to_string()))
            });
            if let Some((_, chat)) = &home {
                // This frame may be the one that drops the focused element
                // (its composer just stopped rendering): look once more next
                // frame, when a vanished focus is visible.
                let on_chip = self
                    .workbench
                    .tab_focus
                    .get(chat)
                    .is_some_and(|chip| chip.is_focused(window));
                let composerless = !on_chip && self.workbench_tab_composer(chat, app).is_none();
                if !composerless {
                    self.workbench.vanish_watch = None;
                } else if self.workbench.vanish_watch.as_deref() != Some(chat.as_str()) {
                    self.workbench.vanish_watch = Some(chat.clone());
                    cx.on_next_frame(window, |shell, window, cx| {
                        shell.workbench_keep_keyboard_home(window, cx);
                    });
                }
            }
            if home.is_some() {
                self.workbench.keyboard_home = home;
            }
            return;
        }
        if !self.workbench_focus_vanished(window, app) {
            return;
        }
        let Some((_, chat)) = self.workbench.keyboard_home.clone() else {
            return;
        };
        let handle = self.workbench_tab_focus(&chat, cx);
        window.focus(&handle, cx);
        cx.notify();
    }

    /// Keyboard focus fell out of the canvas only because the element that
    /// held it stopped rendering (not because the user moved it): the
    /// group it was in still shows the same chat, that chat no longer
    /// renders a composer, and focus is either gone or still names that
    /// chat's hidden composer. Key routing treats this as still being in
    /// that group (never Main's), and the next frame moves focus to the
    /// tab's chip.
    pub(super) fn workbench_focus_vanished(&self, window: &Window, cx: &App) -> bool {
        let Some((pane_id, chat)) = &self.workbench.keyboard_home else {
            return false;
        };
        let layout = self.workbench.layout();
        let still_there = model::pane(&layout, pane_id)
            .is_some_and(|pane| pane.active_chat_id == Some(chat.as_str()));
        if !still_there || self.workbench_tab_composer(chat, cx).is_some() {
            return false;
        }
        match window.focused(cx) {
            None => true,
            // The Shell's own fallback for a focus whose element unmounted
            // (`restore_mounted_focus`: its blur handle, or the shell root).
            // A press outside the canvas clears the home first, so a click on
            // empty chrome is never mistaken for this.
            Some(focused) if focused == self.unfocused || focused == self.shortcut_focus => true,
            Some(focused) => self
                .pane_composers
                .read(cx)
                .get(chat)
                .is_some_and(|composer| {
                    gpui::Focusable::focus_handle(composer.read(cx), cx) == focused
                }),
        }
    }

    /// Any press: a new pointer gesture (an earlier Escape no longer
    /// applies); a press outside the workbench (its canvas and its tab row
    /// in the titlebar) is the user leaving it, so the keyboard's home in the
    /// workbench is forgotten.
    pub(super) fn workbench_pressed_at(&mut self, position: Point<Pixels>) {
        self.workbench.gesture_cancelled = false;
        let inside = {
            let geometry = self.workbench.geometry.borrow();
            [geometry.canvas, geometry.tab_row]
                .iter()
                .flatten()
                .any(|area| area.contains(&position))
        };
        if !inside {
            self.workbench.keyboard_home = None;
            self.workbench.vanish_watch = None;
        }
    }

    /// Whether composer-scoped keys and Cmd-W act in the workbench canvas:
    /// focus is inside it, or it just vanished from a tab there.
    pub(super) fn workbench_keyboard_in_canvas(&self, window: &Window, cx: &App) -> bool {
        self.workbench_holds_focus(window, cx) || self.workbench_focus_vanished(window, cx)
    }

    /// Keyboard focus just moved by a key (Tab / Shift-Tab): make the group
    /// it landed in the focused one. Focus-in listeners cover the other
    /// routes, but GPUI only reports focus changes for an active window, so
    /// the key path syncs directly from where focus actually is.
    pub(super) fn sync_workbench_pane_focus(&mut self, window: &Window, cx: &mut Context<Self>) {
        if !matches!(self.route, Route::Chat) || self.workbench.main.is_empty() {
            return;
        }
        let app: &App = cx;
        let pane = self
            .workbench
            .pane_focus
            .keys()
            .find(|id| self.workbench_group_holds_focus(id, window, app))
            .cloned();
        if let Some(pane) = pane {
            self.workbench_pane_focused(&pane, cx);
        }
    }

    pub(super) fn workbench_pane_focused(&mut self, pane_id: &str, cx: &mut Context<Self>) {
        if !self.workbench_ready() {
            return;
        }
        let layout = self.workbench.layout();
        if layout.focused_pane_id == pane_id || model::pane(&layout, pane_id).is_none() {
            return;
        }
        // `focusPane` keeps the group's selected tab and works for an empty
        // group, so Cmd-W, `+` and the palette's destination follow focus.
        self.workbench_mutate(command::focus_pane(pane_id), cx);
        cx.notify();
    }

    fn workbench_focus_composer(&mut self, chat_id: String, cx: &mut Context<Self>) {
        let layout = self.workbench.layout();
        if !model::visible_chat_ids(&layout).contains(&chat_id) {
            self.workbench.focus_composer = Some(chat_id);
            return;
        }
        self.workbench.focus_composer = None;
        if chat_id == self.workbench.main {
            self.focus_composer(cx);
        } else if self.workbench.views.contains_key(&chat_id)
            && let Some(composer) = self.pane_composers.read(cx).get(&chat_id)
        {
            composer.update(cx, |composer, cx| composer.request_focus(cx));
        } else {
            // Its view mounts this frame; focus it once it exists.
            self.workbench.focus_composer = Some(chat_id);
        }
        cx.notify();
    }

    /// After render created a view for a chat waiting for focus.
    pub(super) fn workbench_take_focus_request(&mut self, chat_id: &str, cx: &mut Context<Self>) {
        if self.workbench.focus_composer.as_deref() == Some(chat_id)
            && let Some(composer) = self.pane_composers.read(cx).get(chat_id)
        {
            self.workbench.focus_composer = None;
            composer.update(cx, |composer, cx| composer.request_focus(cx));
        }
    }

    /// The focus handle of `chat_id`'s tab.
    pub(super) fn workbench_tab_focus(
        &mut self,
        chat_id: &str,
        cx: &mut Context<Self>,
    ) -> FocusHandle {
        // A Tab-key stop: with `track_focus`, GPUI registers the stop from
        // the handle's own flag (the element's `tab_index` applies only to
        // handles it creates itself).
        self.workbench
            .tab_focus
            .entry(chat_id.to_string())
            .or_insert_with(|| cx.focus_handle().tab_stop(true))
            .clone()
    }

    /// Arrow keys on a focused tab: Left/Right step through its group
    /// (wrapping), Home/End jump to its ends. Selection follows focus.
    pub(super) fn workbench_step_tab(
        &mut self,
        chat_id: &str,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let layout = self.workbench.layout();
        let Some(pane) = model::pane_for_chat(&layout, chat_id) else {
            return false;
        };
        let ids: Vec<String> = pane.tabs.iter().map(|tab| tab.chat_id.clone()).collect();
        let Some(at) = ids.iter().position(|id| id == chat_id) else {
            return false;
        };
        let last = ids.len() - 1;
        let next = match key {
            "left" => at.checked_sub(1).unwrap_or(last),
            "right" => (at + 1) % ids.len(),
            "home" => 0,
            "end" => last,
            _ => return false,
        };
        let target = ids[next].clone();
        let focus = self.workbench_tab_focus(&target, cx);
        window.focus(&focus, cx);
        if target != chat_id && self.workbench_ready() {
            self.workbench_mutate(command::focus_tab(&target), cx);
        }
        cx.notify();
        true
    }

    /// Palette Next/Previous tab: the focused group's neighbour, composer
    /// focused.
    /// Focus the next (or previous) group in reading order, with its active
    /// chat's composer focused (an empty group: the group itself).
    pub(super) fn workbench_cycle_group(&mut self, forward: bool, cx: &mut Context<Self>) {
        let layout = self.workbench.layout();
        let panes = model::panes(&layout);
        if panes.len() < 2 {
            return;
        }
        let at = panes
            .iter()
            .position(|pane| pane.id == layout.focused_pane_id)
            .unwrap_or(0);
        let next = if forward {
            (at + 1) % panes.len()
        } else {
            at.checked_sub(1).unwrap_or(panes.len() - 1)
        };
        let pane_id = panes[next].id.to_string();
        self.workbench_focus_group(&pane_id, None, cx);
    }

    /// Give `pane_id` the keyboard: `preferred` when it is still that
    /// group's active chat, else the group's active chat, else (an empty
    /// group) the group's own focus scope. A group that no longer exists
    /// falls back to the focused group's chat, then Main.
    pub(super) fn workbench_focus_group(
        &mut self,
        pane_id: &str,
        preferred: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        let layout = self.workbench.layout();
        let pane =
            model::pane(&layout, pane_id).or_else(|| model::pane(&layout, &layout.focused_pane_id));
        let Some(pane) = pane else {
            self.focus_composer(cx);
            return;
        };
        let pane_id = pane.id.to_string();
        let active = pane.active_chat_id.map(str::to_string);
        self.workbench_pane_focused(&pane_id, cx);
        match active {
            Some(active) => {
                let chat = preferred
                    .filter(|preferred| *preferred == active)
                    .map(str::to_string)
                    .unwrap_or(active);
                self.workbench.focus_pane = None;
                self.workbench_focus_composer(chat, cx);
            }
            None => {
                self.workbench.focus_composer = None;
                self.workbench.focus_pane = Some(pane_id);
                cx.notify();
            }
        }
    }

    pub(super) fn workbench_cycle_tab(&mut self, forward: bool, cx: &mut Context<Self>) {
        let layout = self.workbench.layout();
        let Some(pane) = model::pane(&layout, &layout.focused_pane_id) else {
            return;
        };
        let ids: Vec<String> = pane.tabs.iter().map(|tab| tab.chat_id.clone()).collect();
        if ids.len() < 2 {
            return;
        }
        let at = pane
            .active_chat_id
            .and_then(|active| ids.iter().position(|id| id == active))
            .unwrap_or(0);
        let next = if forward {
            (at + 1) % ids.len()
        } else {
            at.checked_sub(1).unwrap_or(ids.len() - 1)
        };
        let target = ids[next].clone();
        self.workbench_focus_tab(&target, cx);
    }

    // ---- gestures ----

    /// Open `tab`, or focus it where it already is. Related row click,
    /// Enter, Open tab and the palette land here; it never duplicates.
    pub(super) fn workbench_open(&mut self, tab: TabRef, cx: &mut Context<Self>) {
        let layout = self.workbench.layout();
        let chat_id = tab.chat_id.clone();
        if layout.contains_chat(&tab.chat_id) {
            self.workbench_mutate_then(command::focus_tab(&tab.chat_id), Some(chat_id), cx);
            return;
        }
        // A recently closed chat returns to its old group and position.
        let command = if layout
            .closed_tabs
            .iter()
            .any(|closed| closed.tab.chat_id == tab.chat_id)
        {
            command::reopen_closed(Some(&tab.chat_id))
        } else {
            command::open_tab(&tab, None)
        };
        self.workbench_mutate_then(command, Some(chat_id), cx);
    }

    pub(super) fn workbench_focus_tab(&mut self, chat_id: &str, cx: &mut Context<Self>) {
        if !self.workbench_ready() {
            // The single Main view of an older engine (or a loading layout):
            // a click on its tab only focuses its composer.
            self.workbench_focus_composer(chat_id.to_string(), cx);
            return;
        }
        self.workbench_mutate_then(command::focus_tab(chat_id), Some(chat_id.to_string()), cx);
    }

    /// Hide a non-main tab. Never Stop, End, Archive or Delete.
    pub(super) fn workbench_close_tab(&mut self, chat_id: &str, cx: &mut Context<Self>) -> bool {
        if chat_id == self.workbench.main {
            return false;
        }
        self.workbench_mutate(command::close_tab(chat_id), cx)
    }

    pub(super) fn workbench_close_others(
        &mut self,
        pane_id: &str,
        keep: &str,
        cx: &mut Context<Self>,
    ) {
        self.workbench_mutate(command::close_other_tabs(pane_id, keep), cx);
    }

    pub(super) fn workbench_close_pane(&mut self, pane_id: &str, cx: &mut Context<Self>) {
        let layout = self.workbench.layout();
        if model::pane(&layout, pane_id).is_some_and(|pane| pane.holds(&layout.main_chat_id)) {
            return;
        }
        self.workbench_mutate(command::close_pane(pane_id), cx);
    }

    fn no_room_notice(&mut self, cx: &mut Context<Self>) {
        self.sidebar_notice = Some(SidebarNotice::transient_status(
            "There isn't room for another pane here.",
        ));
        cx.notify();
    }

    /// An empty neighbouring pane (strip button, palette).
    pub(super) fn workbench_split_pane(
        &mut self,
        pane_id: &str,
        edge: Edge,
        cx: &mut Context<Self>,
    ) {
        if !self.workbench_split_fits(Some(pane_id), edge) {
            self.no_room_notice(cx);
            return;
        }
        self.workbench_mutate(command::split_pane(pane_id, edge), cx);
    }

    /// Move a tab into a new pane beside its own (context menu, palette).
    pub(super) fn workbench_move_to_split(
        &mut self,
        chat_id: &str,
        edge: Edge,
        cx: &mut Context<Self>,
    ) {
        let layout = self.workbench.layout();
        let Some(pane_id) = model::pane_for_chat(&layout, chat_id).map(|pane| pane.id.to_string())
        else {
            return;
        };
        if !self.workbench_split_fits(Some(&pane_id), edge) {
            self.no_room_notice(cx);
            return;
        }
        let destination = model::split_destination(pane_id, edge);
        self.workbench_mutate_then(
            command::move_tab(chat_id, destination),
            Some(chat_id.to_string()),
            cx,
        );
    }

    /// Cmd-Shift-T / palette: the newest closed tab, or a named one.
    pub(super) fn workbench_reopen(&mut self, chat_id: Option<String>, cx: &mut Context<Self>) {
        let layout = self.workbench.layout();
        let target = match &chat_id {
            Some(id) => layout
                .closed_tabs
                .iter()
                .find(|closed| &closed.tab.chat_id == id)
                .map(|closed| closed.tab.chat_id.clone()),
            None => model::newest_reopenable(&layout).map(|closed| closed.tab.chat_id.clone()),
        };
        let Some(target) = target else {
            self.sidebar_notice = Some(SidebarNotice::transient_status(
                "No closed tab to reopen. Related chats lists every chat.",
            ));
            cx.notify();
            return;
        };
        self.workbench_mutate_then(command::reopen_closed(Some(&target)), Some(target), cx);
    }

    /// Commit a divider ratio (drag release or double-click equalize).
    pub(super) fn workbench_commit_resize(
        &mut self,
        split_id: &str,
        ratio: f32,
        cx: &mut Context<Self>,
    ) {
        self.workbench.resize = None;
        self.workbench_mutate(command::resize_split(split_id, ratio), cx);
        cx.notify();
    }

    /// Whether splitting `pane_id` (`None`: the whole root) on `edge` leaves
    /// both halves at least the minimum pane size.
    fn workbench_split_fits(&self, pane_id: Option<&str>, edge: Edge) -> bool {
        let layout = self.workbench.layout();
        if model::depth(&layout.root) >= luna_proto::WORKBENCH_MAX_DEPTH
            || model::panes(&layout).len() >= luna_proto::WORKBENCH_MAX_PANES
        {
            return false;
        }
        let geometry = self.workbench.geometry.borrow();
        let bounds = match pane_id {
            Some(id) => geometry.panes.get(id).copied(),
            None => geometry.root,
        };
        // Not measured yet (first frame): the engine's validation decides.
        bounds.is_none_or(|bounds| match edge {
            Edge::Left | Edge::Right => f32::from(bounds.size.width) >= 2.0 * MIN_PANE_WIDTH,
            Edge::Top | Edge::Bottom => f32::from(bounds.size.height) >= 2.0 * MIN_PANE_HEIGHT,
        })
    }

    fn workbench_show_parent(&mut self, parent: String, cx: &mut Context<Self>) {
        // The parent is open here (often Main): focus it in place.
        if self.workbench_ready() && self.workbench.layout().contains_chat(&parent) {
            self.workbench_focus_tab(&parent, cx);
        } else {
            self.show_pane_parent(parent, cx);
        }
    }

    // ---- New child chat (`+`) ----

    /// `+`: one blank child of the immutable Main, opened in the given (or
    /// focused) group. Same key across an ambiguous retry; one request per
    /// gesture, never a starter message, run, fork or worktree.
    pub(super) fn workbench_new_child(&mut self, pane_id: Option<String>, cx: &mut Context<Self>) {
        self.request_child(pane_id, false, None, cx);
    }

    /// The New thread source for a group: its active chat, or Main for an
    /// empty group (Main hosts the layout).
    pub(super) fn workbench_group_source(&self, pane_id: &str) -> String {
        let layout = self.workbench.layout();
        model::pane(&layout, pane_id)
            .and_then(|pane| pane.active_chat_id)
            .map(str::to_string)
            .unwrap_or_else(|| self.workbench.main.clone())
    }

    /// A group's split control: one blank child of Main, opened in a new
    /// group to the right of `pane_id`. Nothing happens without room for
    /// another group at its minimum size.
    pub(super) fn workbench_new_child_in_split(&mut self, pane_id: String, cx: &mut Context<Self>) {
        if self.workbench_ready() && !self.workbench_split_fits(Some(&pane_id), Edge::Right) {
            self.no_room_notice(cx);
            return;
        }
        self.request_child(Some(pane_id), true, None, cx);
    }

    /// Retry (or, for an unknown outcome, Check status of) the failed New
    /// thread exactly as it was asked: same source, provider, group and
    /// placement. An unknown outcome resends the same idempotency key, so
    /// the engine returns the thread it may already have created.
    pub(super) fn workbench_retry_child(&mut self, cx: &mut Context<Self>) {
        if !self.workbench_ready() {
            self.workbench_unavailable_notice(cx);
            return;
        }
        if self.workbench.creating_child.is_some() {
            return;
        }
        let Some(failure) = self.workbench.child_failure.as_ref() else {
            return;
        };
        if failure.request.main != self.workbench.main
            || failure.request.generation != self.workbench.generation
        {
            self.workbench.child_failure = None;
            cx.notify();
            return;
        }
        let request = if failure.ambiguous {
            failure.request.clone()
        } else {
            // Known not created: a fresh attempt of the same intent.
            if failure.request.split
                && !self.workbench_split_fits(Some(&failure.request.pane_id), Edge::Right)
            {
                self.no_room_notice(cx);
                return;
            }
            ChildRequest {
                key: uuid::Uuid::new_v4().to_string(),
                ..failure.request.clone()
            }
        };
        self.workbench.child_failure = None;
        self.send_child_request(request, cx);
    }

    /// Whether an earlier New thread's outcome is unknown: no new New
    /// thread may start until its status is checked (it may exist).
    pub(super) fn workbench_child_unresolved(&self) -> bool {
        self.workbench
            .child_failure
            .as_ref()
            .is_some_and(|failure| {
                failure.ambiguous
                    && failure.request.main == self.workbench.main
                    && failure.request.generation == self.workbench.generation
            })
    }

    pub(super) fn request_child(
        &mut self,
        pane_id: Option<String>,
        split: bool,
        provider: Option<luna_proto::HarnessId>,
        cx: &mut Context<Self>,
    ) {
        if !self.workbench_ready() {
            self.workbench_unavailable_notice(cx);
            return;
        }
        if self.workbench.creating_child.is_some() {
            return;
        }
        // An unknown earlier outcome is never reused for, or replaced by, a
        // different intent: it stays until its own Check status resolves it.
        if self.workbench_child_unresolved() {
            self.workbench.child_blocked = true;
            cx.notify();
            return;
        }
        let pane_id = pane_id.unwrap_or_else(|| self.workbench.layout().focused_pane_id.clone());
        // One level of side chats: a side chat's group starts a fresh
        // sibling under the same main chat, never a nested child.
        let source = self.workbench_group_source(&pane_id);
        if self.board_chat_role(&source, cx).role == luna_proto::BoardChatRole::Side {
            self.board_create_sibling(source, cx);
            return;
        }
        self.workbench.child_failure = None;
        let request = ChildRequest {
            key: uuid::Uuid::new_v4().to_string(),
            main: self.workbench.main.clone(),
            generation: self.workbench.generation,
            parent: self.workbench_group_source(&pane_id),
            pane_id,
            split,
            provider,
        };
        self.send_child_request(request, cx);
    }

    fn send_child_request(&mut self, request: ChildRequest, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let params = luna_proto::CreateChildTabRequest {
            parent_task_id: request.parent.clone(),
            idempotency_key: request.key.clone(),
            title: None,
            provider: request.provider,
            model: None,
            reasoning: None,
            model_options: Default::default(),
        };
        let Ok(params) = serde_json::to_value(&params) else {
            return;
        };
        self.workbench.creating_child = Some(request.clone());
        self.workbench.child_task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call_as::<luna_proto::ChildThreadReceipt>(methods::CREATE_CHILD_TAB, params)
                .await;
            this.update(cx, |shell, cx| {
                shell.finish_child_request(request, result, cx)
            })
            .ok();
        }));
        cx.notify();
    }

    fn finish_child_request(
        &mut self,
        request: ChildRequest,
        result: Result<luna_proto::ChildThreadReceipt, luna_rpc::RpcError>,
        cx: &mut Context<Self>,
    ) {
        if self
            .workbench
            .creating_child
            .as_ref()
            .is_none_or(|pending| pending.key != request.key)
        {
            return;
        }
        self.workbench.creating_child = None;
        self.workbench.child_task = None;
        #[cfg(feature = "appshots-fixture")]
        let result = if std::mem::take(&mut self.workbench.lose_child_response) && result.is_ok() {
            Err(luna_rpc::RpcError::Transport(
                "fixture: response lost".into(),
            ))
        } else {
            result
        };
        if request.generation != self.workbench.generation {
            return;
        }
        match result {
            Ok(receipt) => {
                // The child exists whatever happens next: Related chats lists
                // it even if this tab never opens.
                if request.main != self.workbench.main {
                    cx.notify();
                    return;
                }
                let tab = TabRef::chat(receipt.task_id.clone());
                let layout = self.workbench.layout();
                let destination = model::pane(&layout, &request.pane_id).map(|pane| {
                    if request.split {
                        model::split_destination(pane.id.to_string(), Edge::Right)
                    } else {
                        Destination::Pane {
                            pane_id: pane.id.to_string(),
                            index: None,
                        }
                    }
                });
                let opened = self.workbench_mutate_then(
                    command::open_tab(&tab, destination),
                    Some(receipt.task_id.clone()),
                    cx,
                );
                if !opened && !self.workbench.layout().contains_chat(&receipt.task_id) {
                    self.sidebar_notice = Some(SidebarNotice::status(
                        "The new side chat was created. Open it from Related chats.",
                    ));
                }
            }
            Err(error) => {
                let ambiguous = matches!(
                    error,
                    luna_rpc::RpcError::Transport(_) | luna_rpc::RpcError::Closed
                );
                let message = if ambiguous {
                    format!(
                        "The new side chat may have been created ({error}). Check its status before creating another."
                    )
                } else {
                    format!("Couldn't create the new side chat: {error}")
                };
                self.workbench.child_blocked = false;
                self.workbench.child_failure = Some(ChildFailure {
                    request,
                    message: message.into(),
                    ambiguous,
                });
            }
        }
        cx.notify();
    }

    /// Dismiss a failure. An unknown outcome can't be dismissed (that
    /// would let a new intent replace it); only its Check status settles it.
    pub(super) fn workbench_dismiss_child_failure(&mut self, cx: &mut Context<Self>) {
        if self.workbench_child_unresolved() {
            return;
        }
        self.workbench.child_failure = None;
        cx.notify();
    }

    // ---- Related chats as the library ----

    /// A Related row's Open: a center tab when the engine has the
    /// workbench (`false` leaves the legacy right-pane view to the caller).
    pub(super) fn show_agent_chat(&mut self, id: String, split: bool, cx: &mut Context<Self>) {
        if !self.workbench_ready() {
            self.open_pane_chat_as_main(id, cx);
            return;
        }
        if !split {
            self.workbench_open(TabRef::chat(id), cx);
            return;
        }
        let layout = self.workbench.layout();
        if !self.workbench_split_fits(Some(&layout.focused_pane_id), model::Edge::Right) {
            self.no_room_notice(cx);
            return;
        }
        let destination =
            model::split_destination(layout.focused_pane_id.clone(), model::Edge::Right);
        self.workbench_mutate(
            model::Command::OpenBackgroundTab {
                tab: TabRef::chat(id),
                destination: Some(destination),
            },
            cx,
        );
    }

    pub(super) fn open_related_in_workbench(
        &mut self,
        id: String,
        side: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.workbench_ready() {
            return false;
        }
        let tab = if side {
            let parent = self.side_source_parent(&id, cx);
            TabRef {
                chat_id: id,
                side_parent_id: Some(parent),
            }
        } else {
            TabRef::chat(id)
        };
        self.workbench_open(tab, cx);
        true
    }

    /// A side chat's actual source chat: the layout's own record of it (open
    /// or recently closed), then its published row; the selected chat only
    /// when neither knows it.
    pub(super) fn side_source_parent(&self, id: &str, cx: &App) -> String {
        let layout = self.workbench.layout();
        let recorded = model::tab(&layout, id)
            .and_then(|tab| tab.side_parent_id.clone())
            .or_else(|| {
                layout
                    .closed_tabs
                    .iter()
                    .find(|closed| closed.tab.chat_id == id)
                    .and_then(|closed| closed.tab.side_parent_id.clone())
            });
        recorded
            .or_else(|| {
                self.state
                    .read(cx)
                    .side_chat_rows
                    .get(id)
                    .map(|row| row.parent_id.clone())
            })
            .unwrap_or_else(|| self.active_chat.clone())
    }

    /// Each group's active (on-screen) tab chat that has unseen activity.
    pub(super) fn workbench_visible_unseen(&self, cx: &App) -> Vec<String> {
        if !self.workbench_available() {
            return Vec::new();
        }
        let state = self.state.read(cx);
        model::visible_chat_ids(&self.workbench.layout())
            .into_iter()
            .filter(|id| {
                state
                    .chats
                    .iter()
                    .any(|chat| chat.id == *id && chat.unseen())
            })
            .collect()
    }

    /// Every chat in the workbench: open tabs, then recently closed ones.
    pub(super) fn workbench_chat_ids(&self) -> Vec<String> {
        let layout = self.workbench.layout();
        let mut ids = model::open_chat_ids(&layout);
        for closed in &layout.closed_tabs {
            if !ids.contains(&closed.tab.chat_id) {
                ids.push(closed.tab.chat_id.clone());
            }
        }
        ids
    }

    /// Whether a Related row's chat has an open view here (`None` when the
    /// workbench is unavailable and rows keep their legacy behavior).
    pub(super) fn workbench_is_open(&self, chat_id: &str) -> Option<bool> {
        self.workbench_ready()
            .then(|| self.workbench.layout().contains_chat(chat_id))
    }

    // ---- keyboard ----

    /// Cmd-W while keyboard focus is inside the canvas. Always handled
    /// there: a closable tab closes, a focused empty secondary group closes,
    /// and Main is a protected no-op (never the right pane or the window).
    /// Outside the canvas the cascade continues as before.
    pub(super) fn workbench_close_focused(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !matches!(self.route, Route::Chat)
            || self.workbench.main.is_empty()
            || !self.workbench_keyboard_in_canvas(window, cx)
        {
            return false;
        }
        let layout = self.workbench.layout();
        match layout.focused_chat_id().map(str::to_string) {
            Some(chat) if chat == layout.main_chat_id => {}
            Some(chat) => {
                self.workbench_close_tab(&chat, cx);
            }
            None => {
                let pane = layout.focused_pane_id.clone();
                if model::has_split(&layout) {
                    self.workbench_close_pane(&pane, cx);
                }
            }
        }
        true
    }

    /// Escape during a tab, row or divider drag: cancel it, layout unchanged.
    pub(super) fn workbench_cancel_drag(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        // Whatever happens below, the gesture in progress (if any) is over:
        // its release must neither click nor commit, even before it has a
        // preview or a live ratio.
        self.workbench.gesture_cancelled = true;
        self.workbench.press = None;
        self.workbench.divider_press = None;
        let dragging = self.workbench.preview.is_some()
            || self.workbench.resize.is_some()
            || self.workbench.dragging.is_some();
        if !dragging {
            return false;
        }
        self.workbench.preview = None;
        self.workbench.resize = None;
        self.workbench.dragging = None;
        cx.stop_active_drag(window);
        cx.notify();
        true
    }

    /// A new mouse press starts a new gesture: an earlier Escape no longer
    /// applies.
    pub(super) fn workbench_gesture_pressed(&mut self) {
        self.workbench.gesture_cancelled = false;
    }

    // ---- drag and drop ----

    /// Remember where a tab press began (drag arming distance).
    pub(super) fn workbench_note_press(&mut self, chat_id: String, point: Point<Pixels>) {
        self.workbench.gesture_cancelled = false;
        self.workbench.press = Some((chat_id, point));
    }

    /// A double-click equalized the divider: its press is spent, so no
    /// later release can resolve a ratio from it.
    pub(super) fn workbench_equalize_divider(&mut self, split_id: &str, cx: &mut Context<Self>) {
        if self
            .workbench
            .divider_press
            .as_ref()
            .is_some_and(|(id, _)| id == split_id)
        {
            self.workbench.divider_press = None;
        }
        self.workbench_commit_resize(split_id, 0.5, cx);
    }

    /// Remember where a divider press began.
    pub(super) fn workbench_note_divider_press(&mut self, split_id: String, point: Point<Pixels>) {
        self.workbench.gesture_cancelled = false;
        self.workbench.divider_press = Some((split_id, point));
    }

    /// Resolve where a drop at `point` lands, for `dragged`: a group's
    /// strip in the bottom row (an insertion slot), else a group's body.
    fn workbench_drop_target(&self, point: Point<Pixels>, dragged: &str) -> Option<DropTarget> {
        let geometry = self.workbench.geometry.borrow();
        if let Some(pane_id) = geometry.strip_at(point) {
            let tabs = geometry.visible_tabs(&pane_id);
            let spans: Vec<(f32, f32)> = tabs
                .iter()
                .map(|(_, bounds)| (f32::from(bounds.left()), f32::from(bounds.right())))
                .collect();
            let from = tabs.iter().position(|(chat, _)| chat == dragged);
            let index = model::strip_index(f32::from(point.x), &spans, from);
            return Some(DropTarget::Strip { pane_id, index });
        }
        let root = geometry.root?;
        if !root.contains(&point) {
            return None;
        }
        let layout = self.workbench.layout();
        let local = point - root.origin;
        let (x, y) = (f32::from(local.x), f32::from(local.y));
        let (width, height) = (f32::from(root.size.width), f32::from(root.size.height));
        // The outer band: a full-height/width split around the whole tree.
        if model::has_split(&layout) {
            let outer = if x <= OUTER_EDGE_BAND {
                Some(Edge::Left)
            } else if width - x <= OUTER_EDGE_BAND {
                Some(Edge::Right)
            } else if y <= OUTER_EDGE_BAND {
                Some(Edge::Top)
            } else if height - y <= OUTER_EDGE_BAND {
                Some(Edge::Bottom)
            } else {
                None
            };
            if let Some(edge) = outer {
                return Some(DropTarget::Split {
                    node_id: layout.root.id().to_string(),
                    edge,
                    outer: true,
                });
            }
        }
        let (pane_id, body) = geometry.pane_at(point)?;
        let local = point - body.origin;
        match model::body_zone(
            f32::from(local.x),
            f32::from(local.y),
            f32::from(body.size.width),
            f32::from(body.size.height),
            EDGE_ZONE_CAP,
        ) {
            Some(edge) => Some(DropTarget::Split {
                node_id: pane_id,
                edge,
                outer: false,
            }),
            None => Some(DropTarget::Join { pane_id }),
        }
    }

    /// The command a drop on `target` commits, if it is valid and changes
    /// something. Pane/split IDs are minted once per accepted target.
    fn workbench_drop_command(
        &self,
        target: &DropTarget,
        tab: &TabRef,
        previous: Option<&DropPreview>,
    ) -> Option<(Command, SharedString)> {
        // Keep the IDs minted for this same target while the pointer stays.
        if let Some(previous) = previous
            && &previous.target == target
        {
            return Some((previous.command.clone(), previous.label.clone()));
        }
        let layout = self.workbench.layout();
        let open = layout.contains_chat(&tab.chat_id);
        let (destination, label): (Destination, SharedString) = match target {
            DropTarget::Strip { pane_id, index } => (
                Destination::Pane {
                    pane_id: pane_id.clone(),
                    index: Some(*index),
                },
                (if open { "Move tab here" } else { "Add tab" }).into(),
            ),
            DropTarget::Join { pane_id } => (
                Destination::Pane {
                    pane_id: pane_id.clone(),
                    index: None,
                },
                (if open {
                    "Move to this group"
                } else {
                    "Add tab"
                })
                .into(),
            ),
            DropTarget::Split {
                node_id,
                edge,
                outer,
            } => {
                let pane = (!outer).then_some(node_id.as_str());
                if !self.workbench_split_fits(pane, *edge) {
                    return None;
                }
                (
                    model::split_destination(node_id.clone(), *edge),
                    if *outer {
                        format!("{} (whole workbench)", model::edge_label(*edge)).into()
                    } else {
                        model::edge_label(*edge).into()
                    },
                )
            }
        };
        let command = if open {
            command::move_tab(&tab.chat_id, destination)
        } else {
            command::open_tab(tab, Some(destination))
        };
        // Only an accepted, changing drop gets a preview.
        let base = self.workbench.display()?;
        let (_, changed) = model::reduce(base, &command)?;
        changed.then_some((command, label))
    }

    fn update_workbench_preview(
        &mut self,
        point: Point<Pixels>,
        tab: &TabRef,
        cx: &mut Context<Self>,
    ) {
        let preview = self
            .workbench_drop_target(point, &tab.chat_id)
            .and_then(|target| {
                self.workbench_drop_command(&target, tab, self.workbench.preview.as_ref())
                    .map(|(command, label)| DropPreview {
                        target,
                        command,
                        label,
                    })
            });
        let changed = match (&preview, &self.workbench.preview) {
            (Some(next), Some(current)) => next.target != current.target,
            (None, None) => false,
            _ => true,
        };
        if changed {
            self.workbench.preview = preview;
            cx.notify();
        }
    }

    pub(super) fn on_workbench_tab_drag(
        &mut self,
        event: &gpui::DragMoveEvent<WorkbenchTabDrag>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let drag = event.drag(cx).clone();
        if drag.main != self.workbench.main || drag.generation != self.workbench.generation {
            return;
        }
        if self.workbench.gesture_cancelled {
            return;
        }
        let point = event.event.position;
        if !drag.armed.get() {
            let origin = self
                .workbench
                .press
                .as_ref()
                .filter(|(chat, _)| chat == &drag.tab.chat_id)
                .map(|(_, origin)| *origin)
                .unwrap_or(point);
            let travel = point - origin;
            let distance = (f32::from(travel.x).powi(2) + f32::from(travel.y).powi(2)).sqrt();
            if distance < DRAG_START_DISTANCE {
                return;
            }
            drag.armed.set(true);
            self.workbench.dragging = Some(drag.tab.chat_id.clone());
            cx.notify();
        }
        self.update_workbench_preview(point, &drag.tab, cx);
    }

    /// A Related chats row dragged over the canvas: the same destinations.
    pub(super) fn on_related_row_drag(
        &mut self,
        event: &gpui::DragMoveEvent<SidebarChatDrag>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let drag = event.drag(cx).clone();
        if self.workbench.gesture_cancelled {
            return;
        }
        let Some(tab) = self.related_drag_tab(&drag, cx) else {
            return;
        };
        let point = event.event.position;
        let inside = self.workbench.geometry.borrow().accepts_drop_at(point);
        if !inside {
            if self.workbench.preview.take().is_some() {
                cx.notify();
            }
            return;
        }
        self.update_workbench_preview(point, &tab, cx);
    }

    /// The tab a Related row drag opens; left-sidebar drags open nothing here.
    fn related_drag_tab(&self, drag: &SidebarChatDrag, cx: &App) -> Option<TabRef> {
        if !self.workbench_ready() {
            return None;
        }
        match drag.origin {
            DragOrigin::Related(super::related_chats::RelatedKind::Side) => Some(TabRef {
                chat_id: drag.chat_id.clone(),
                side_parent_id: Some(self.side_source_parent(&drag.chat_id, cx)),
            }),
            DragOrigin::Related(_) => Some(TabRef::chat(drag.chat_id.clone())),
            DragOrigin::Sidebar => None,
        }
    }

    pub(super) fn drop_workbench_tab(
        &mut self,
        drag: &WorkbenchTabDrag,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let preview = self.workbench.preview.take();
        self.workbench.dragging = None;
        let press = self.workbench.press.take();
        if drag.main != self.workbench.main || drag.generation != self.workbench.generation {
            cx.notify();
            return;
        }
        if self.workbench.gesture_cancelled {
            // Escaped: neither a click nor a drop.
            cx.notify();
            return;
        }
        if !drag.armed.get() {
            // No drag move reached the workbench (GPUI starts a drag on the
            // move that crosses its threshold, after the canvas's drag-move
            // listeners ran; a fast flick or a synthetic drag can be one
            // move): resolve the drop where it was released, exactly as the
            // preview would have. Less travel than the drag distance is a
            // click.
            let release = window.mouse_position();
            let travelled = press
                .filter(|(chat, _)| chat == &drag.tab.chat_id)
                .is_some_and(|(_, origin)| travel(origin, release) >= DRAG_START_DISTANCE);
            if !travelled {
                self.workbench_focus_tab(&drag.tab.chat_id, cx);
                return;
            }
            if let Some((command, _)) = self
                .workbench_drop_target(release, &drag.tab.chat_id)
                .and_then(|target| self.workbench_drop_command(&target, &drag.tab, None))
            {
                self.workbench_mutate_then(command, Some(drag.tab.chat_id.clone()), cx);
            }
            cx.notify();
            return;
        }
        if let Some(preview) = preview {
            self.workbench_mutate_then(preview.command, Some(drag.tab.chat_id.clone()), cx);
        }
        cx.notify();
    }

    pub(super) fn drop_related_row(
        &mut self,
        drag: &SidebarChatDrag,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let preview = self.workbench.preview.take();
        if self.workbench.gesture_cancelled {
            cx.notify();
            return;
        }
        if let Some(tab) = self.related_drag_tab(drag, cx) {
            // A preview from the drag moves, or (no move reached the canvas)
            // the same target resolved at the release point.
            let command = match preview {
                Some(preview) => Some(preview.command),
                None => {
                    let release = window.mouse_position();
                    let inside = self.workbench.geometry.borrow().accepts_drop_at(release);
                    inside
                        .then(|| self.workbench_drop_target(release, &tab.chat_id))
                        .flatten()
                        .and_then(|target| self.workbench_drop_command(&target, &tab, None))
                        .map(|(command, _)| command)
                }
            };
            if let Some(command) = command {
                self.workbench_mutate_then(command, Some(drag.chat_id.clone()), cx);
            }
        }
        cx.notify();
    }

    pub(super) fn on_workbench_divider_drag(
        &mut self,
        event: &gpui::DragMoveEvent<WorkbenchDividerDrag>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let split_id = event.drag(cx).split_id.clone();
        if self.workbench.gesture_cancelled {
            return;
        }
        let Some(ratio) = self.workbench_divider_ratio(&split_id, event.event.position) else {
            return;
        };
        if self
            .workbench
            .resize
            .as_ref()
            .is_none_or(|(id, current)| id != &split_id || (current - ratio).abs() > 0.0005)
        {
            self.workbench.resize = Some((split_id, ratio));
            cx.notify();
        }
    }

    /// The ratio a divider pointer at `point` asks for, within the minimum
    /// pane sizes.
    fn workbench_divider_ratio(&self, split_id: &str, point: Point<Pixels>) -> Option<f32> {
        let (axis, _) = model::split_of(&self.workbench.layout(), split_id)?;
        let bounds = *self.workbench.geometry.borrow().splits.get(split_id)?;
        let (offset, extent, minimum) = match axis {
            model::Axis::Horizontal => (
                f32::from(point.x - bounds.origin.x),
                f32::from(bounds.size.width),
                MIN_PANE_WIDTH,
            ),
            model::Axis::Vertical => (
                f32::from(point.y - bounds.origin.y),
                f32::from(bounds.size.height),
                MIN_PANE_HEIGHT,
            ),
        };
        (extent > 0.0).then(|| clamp_ratio(offset / extent, extent, minimum))
    }

    /// Divider release: commit the local ratio once. A release that no drag
    /// move reached (a fast or synthetic one-move drag) resolves the ratio at
    /// the release point, if the pointer travelled the drag distance from
    /// the press; a click (no travel) and an Escaped gesture change nothing.
    pub(super) fn finish_workbench_divider(
        &mut self,
        split_id: &str,
        release: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        // Every divider hears a release outside itself: only the pressed
        // divider's own press is consumed here.
        let press = match &self.workbench.divider_press {
            Some((id, _)) if id == split_id => self.workbench.divider_press.take(),
            _ => None,
        };
        if self.workbench.gesture_cancelled {
            self.workbench.resize = None;
            cx.notify();
            return;
        }
        if let Some((id, ratio)) = self.workbench.resize.clone()
            && id == split_id
        {
            self.workbench_commit_resize(&id, ratio, cx);
            return;
        }
        if let Some((_, origin)) = press
            && travel(origin, release) >= DRAG_START_DISTANCE
            && let Some(ratio) = self.workbench_divider_ratio(split_id, release)
        {
            self.workbench_commit_resize(split_id, ratio, cx);
        }
    }

    pub(super) fn close_workbench_menu(&mut self, cx: &mut Context<Self>) {
        if self.workbench.menu.begin_close() {
            popover::reap_popup(cx, |shell: &mut Self| &mut shell.workbench.menu);
        }
        cx.notify();
    }

    pub(super) fn open_workbench_menu(
        &mut self,
        chat_id: String,
        pane_id: String,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        // Its board rows follow the chat's canonical role.
        self.board_chat_ensure_identity(&chat_id, cx);
        self.workbench.menu.open(TabMenu {
            chat_id,
            pane_id,
            position,
            group: false,
        });
        cx.notify();
    }

    /// A compact strip's Group actions menu (the tab menu with the group's
    /// actions first).
    pub(super) fn open_workbench_group_menu(
        &mut self,
        chat_id: String,
        pane_id: String,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.workbench.menu.open(TabMenu {
            chat_id,
            pane_id,
            position,
            group: true,
        });
        cx.notify();
    }

    /// Render-time hygiene: a drag that ended anywhere (released outside the
    /// canvas, cancelled) leaves no preview, dimmed tab or live ratio.
    pub(super) fn settle_workbench_drag(&mut self, cx: &App) {
        if !cx.has_active_drag() {
            self.workbench.preview = None;
            self.workbench.resize = None;
            self.workbench.dragging = None;
        }
    }

    // ---- palette ----

    /// Workbench commands the palette offers right now.
    /// Balance panes: one shared command makes every group (pane, not tab)
    /// the same area, keeping the arrangement. Only meaningful with a split.
    pub(super) fn workbench_balance_panes(&mut self, cx: &mut Context<Self>) {
        if self.workbench_ready() && model::has_split(&self.workbench.layout()) {
            self.workbench_mutate(command::balance_panes(), cx);
        }
    }

    pub(super) fn workbench_palette_actions(&self) -> Vec<WorkbenchAction> {
        if !self.workbench_ready() {
            return Vec::new();
        }
        let layout = self.workbench.layout();
        let mut actions = vec![
            WorkbenchAction::Chooser,
            WorkbenchAction::SideChat,
            WorkbenchAction::NewChild,
            WorkbenchAction::NewThreadSplit,
        ];
        if model::has_split(&layout) {
            actions.push(WorkbenchAction::NextGroup);
            actions.push(WorkbenchAction::PreviousGroup);
            if model::pane(&layout, &layout.focused_pane_id)
                .is_some_and(|pane| !pane.holds(&layout.main_chat_id))
            {
                actions.push(WorkbenchAction::CloseGroup);
            }
        }
        if model::newest_reopenable(&layout).is_some() {
            actions.push(WorkbenchAction::Reopen);
        }
        if let Some(focused) = layout.focused_chat_id() {
            if focused != layout.main_chat_id {
                actions.push(WorkbenchAction::Close);
            }
            actions.extend([
                WorkbenchAction::Move(Edge::Right),
                WorkbenchAction::Move(Edge::Bottom),
                WorkbenchAction::Move(Edge::Left),
                WorkbenchAction::Move(Edge::Top),
            ]);
        }
        if model::has_split(&layout) {
            actions.push(WorkbenchAction::BalancePanes);
        }
        if model::pane(&layout, &layout.focused_pane_id).is_some_and(|pane| pane.tabs.len() > 1) {
            actions.push(WorkbenchAction::CloseOthers);
            actions.push(WorkbenchAction::NextTab);
            actions.push(WorkbenchAction::PreviousTab);
        }
        actions
    }

    pub(super) fn run_workbench_action(&mut self, action: WorkbenchAction, cx: &mut Context<Self>) {
        let layout = self.workbench.layout();
        let focused = layout.focused_chat_id().map(str::to_string);
        match action {
            WorkbenchAction::Chooser => {
                self.workbench_open_chooser(None, CreationKind::NewThread, cx)
            }
            WorkbenchAction::SideChat => {
                self.workbench_open_chooser(None, CreationKind::SideChat, cx)
            }
            WorkbenchAction::NewChild => self.workbench_new_child(None, cx),
            WorkbenchAction::NewThreadSplit => {
                let pane = layout.focused_pane_id.clone();
                self.workbench_new_child_in_split(pane, cx);
            }
            WorkbenchAction::NextGroup => self.workbench_cycle_group(true, cx),
            WorkbenchAction::PreviousGroup => self.workbench_cycle_group(false, cx),
            WorkbenchAction::CloseGroup => {
                let pane = layout.focused_pane_id.clone();
                self.workbench_close_pane(&pane, cx);
            }
            WorkbenchAction::Reopen => self.workbench_reopen(None, cx),
            WorkbenchAction::Close => {
                if let Some(chat) = focused {
                    self.workbench_close_tab(&chat, cx);
                }
            }
            WorkbenchAction::CloseOthers => {
                if let Some(chat) = focused {
                    self.workbench_close_others(&layout.focused_pane_id, &chat, cx);
                }
            }
            WorkbenchAction::Move(edge) => {
                if let Some(chat) = focused {
                    self.workbench_move_to_split(&chat, edge, cx);
                }
            }
            WorkbenchAction::NextTab => self.workbench_cycle_tab(true, cx),
            WorkbenchAction::PreviousTab => self.workbench_cycle_tab(false, cx),
            WorkbenchAction::BalancePanes => self.workbench_balance_panes(cx),
        }
    }
}

/// Palette entries for the workbench (dragging is never the only way).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WorkbenchAction {
    /// The focused group's `+` creation chooser.
    Chooser,
    /// The chooser with Side chat preselected.
    SideChat,
    NewChild,
    /// A fresh New thread in a new group to the right (the split shortcut).
    NewThreadSplit,
    NextGroup,
    PreviousGroup,
    CloseGroup,
    Reopen,
    Close,
    CloseOthers,
    Move(Edge),
    NextTab,
    PreviousTab,
    BalancePanes,
}

impl WorkbenchAction {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Chooser => "Side chat…",
            Self::SideChat => "Side chat with context…",
            Self::NewChild => "New side chat (fresh)",
            Self::NewThreadSplit => "Side chat in split",
            Self::NextGroup => "Focus next group",
            Self::PreviousGroup => "Focus previous group",
            Self::CloseGroup => "Close group",
            Self::Reopen => "Reopen closed tab",
            Self::Close => "Close tab",
            Self::CloseOthers => "Close other tabs (keeps Main)",
            Self::Move(Edge::Right) => "Move tab to split right",
            Self::Move(Edge::Left) => "Move tab to split left",
            Self::Move(Edge::Bottom) => "Move tab to split below",
            Self::Move(Edge::Top) => "Move tab to split above",
            Self::NextTab => "Next tab in group",
            Self::PreviousTab => "Previous tab in group",
            Self::BalancePanes => "Balance panes",
        }
    }

    /// The configurable shortcut that runs this action, if any.
    pub(super) fn shortcut(self) -> Option<crate::settings::ShortcutId> {
        use crate::settings::ShortcutId;
        Some(match self {
            Self::Chooser => ShortcutId::NewThreadChooser,
            Self::NewThreadSplit => ShortcutId::NewThreadSplit,
            Self::NextTab => ShortcutId::NextTab,
            Self::PreviousTab => ShortcutId::PreviousTab,
            Self::NextGroup => ShortcutId::NextGroup,
            Self::PreviousGroup => ShortcutId::PreviousGroup,
            Self::Reopen => ShortcutId::ReopenTab,
            _ => return None,
        })
    }

    pub(super) fn icon(self) -> &'static str {
        match self {
            Self::Chooser | Self::NewChild => icons::PLUS,
            Self::SideChat => icons::FORK,
            Self::NewThreadSplit => icons::SPLIT_COLUMNS,
            Self::NextGroup => icons::ALT_ARROW_RIGHT,
            Self::PreviousGroup => icons::ALT_ARROW_LEFT,
            Self::CloseGroup => icons::CLOSE,
            Self::Reopen => icons::RESTART,
            Self::Close | Self::CloseOthers => icons::CLOSE,
            Self::Move(_) => icons::SPLIT_COLUMNS,
            Self::NextTab => icons::ALT_ARROW_RIGHT,
            Self::PreviousTab => icons::ALT_ARROW_LEFT,
            Self::BalancePanes => icons::WIDGET,
        }
    }
}

/// A divider ratio that keeps both sides at least `minimum` along `extent`;
/// a split too small for both keeps the midpoint.
/// Pointer travel between two points, in px.
fn travel(from: Point<Pixels>, to: Point<Pixels>) -> f32 {
    let delta = to - from;
    (f32::from(delta.x).powi(2) + f32::from(delta.y).powi(2)).sqrt()
}

pub(super) fn clamp_ratio(ratio: f32, extent: f32, minimum: f32) -> f32 {
    let floor = (minimum / extent.max(1.0)).max(model::RATIO_MIN);
    let ceiling = (1.0 - floor).min(model::RATIO_MAX);
    if floor > ceiling {
        0.5
    } else {
        ratio.clamp(floor, ceiling)
    }
}

/// Native visual QA drives the production workbench through these hooks.
#[cfg(feature = "appshots-fixture")]
impl Shell {
    /// The workbench as the user sees it: layout, roles, views, geometry.
    pub fn fixture_workbench_readback(&self, cx: &App) -> serde_json::Value {
        let layout = self.workbench.layout();
        let state = self.state.read(cx);
        let geometry = self.workbench.geometry.borrow();
        fn node_json(node: &model::Node, main: &str, state: &AppState) -> serde_json::Value {
            match node {
                model::Node::Pane {
                    id,
                    tabs,
                    active_chat_id,
                } => json!({
                    "kind": "pane",
                    "id": id,
                    "active": active_chat_id,
                    "tabs": tabs.iter().map(|tab| {
                        let (role, parent) = tab_role(state, main, tab);
                        let (heading, details) = tab_help(state, main, tab);
                        json!({
                            "chatId": tab.chat_id,
                            "sideParentId": tab.side_parent_id,
                            "role": role.label(),
                            "parent": parent,
                            "title": tab_title(state, tab, role).to_string(),
                            // The tab's tooltip, as hovering shows it.
                            "help": {"title": heading.to_string(), "details": details.to_string()},
                            // The tab's live state, as its glyph shows it
                            // (Completed: the accent unread dot).
                            "status": state
                                .chats
                                .iter()
                                .find(|chat| chat.id == tab.chat_id)
                                .map(|chat| format!("{:?}", state.display_status_for(chat, chrono::Utc::now()))),
                        })
                    }).collect::<Vec<_>>(),
                }),
                model::Node::Split {
                    id,
                    axis,
                    ratio,
                    first,
                    second,
                } => json!({
                    "kind": "split",
                    "id": id,
                    "axis": axis,
                    "ratio": ratio,
                    "first": node_json(first, main, state),
                    "second": node_json(second, main, state),
                }),
            }
        }
        let bounds = |bounds: &Bounds<Pixels>| {
            json!({
                "x": f32::from(bounds.origin.x),
                "y": f32::from(bounds.origin.y),
                "w": f32::from(bounds.size.width),
                "h": f32::from(bounds.size.height),
            })
        };
        let mut views: Vec<String> = self.workbench.views.keys().cloned().collect();
        views.sort();
        // The saved tree's minimum size, and the canvas scroll (content larger
        // than the viewport scrolls; nothing is resized or rewritten).
        let (min_w, min_h) = render::min_tree_size(&layout.root);
        let canvas_minimum = json!({"w": min_w, "h": min_h});
        let offset = self.workbench.canvas_scroll.offset();
        let max = self.workbench.canvas_scroll.max_offset();
        let canvas_scroll = json!({
            "x": -f32::from(offset.x),
            "y": -f32::from(offset.y),
            "maxX": f32::from(max.x),
            "maxY": f32::from(max.y),
        });
        // The titlebar's tab row and each group's strip in it.
        let row_bounds = geometry.tab_row.as_ref().map(bounds);
        let strip_bounds: Vec<serde_json::Value> = geometry
            .strips
            .iter()
            .map(|(id, strip)| json!({"pane": id, "bounds": bounds(strip)}))
            .collect();
        // The checkout row above each shown chat's prompt: what it names
        // (its own checkout and branch) and where it sits against the pill.
        let context_rows: serde_json::Map<String, serde_json::Value> =
            model::visible_chat_ids(&layout)
                .iter()
                .filter_map(|chat| {
                    let composer = self.workbench_composer(chat, cx)?;
                    Some((chat.clone(), composer.read(cx).fixture_context_row(cx)))
                })
                .collect();
        let mut readback = json!({
                    "available": self.workbench_available(),
                    "loaded": self.workbench.loaded(),
                    // The layout notice (a latched refusal, or a load that is
                    // still being retried) and whether it is latched.
                    "loadNotice": self.workbench.unavailable.clone().or_else(|| self.workbench.load_error.clone()).map(|notice| notice.to_string()),
                    "unavailableLatched": self.workbench.unavailable.is_some(),
                    // The boot splash still covers the window (fading or visible).
                    "bootOverlay": self.splash != SplashPhase::Gone,
                    "main": layout.main_chat_id,
                    "revision": layout.revision,
                    "focusedPane": layout.focused_pane_id,
                    "focusedChat": layout.focused_chat_id(),
                    "canvasMinimum": canvas_minimum,
        // Only rendered (active) tabs: a hidden tab's view keeps the
                    // width it last painted at.
                    "composerWidths": self.workbench.views.iter()
                        .filter(|(chat, _)| model::visible_chat_ids(&layout).contains(chat))
                        .map(|(chat, tab)| (chat.clone(), json!(tab.view.read(cx).composer_slot_width())))
                        .collect::<serde_json::Map<String, serde_json::Value>>(),
                    "canvasScroll": canvas_scroll,
                    "root": node_json(&layout.root, &layout.main_chat_id, state),
                    "closed": layout.closed_tabs.iter().map(|closed| closed.tab.chat_id.clone()).collect::<Vec<_>>(),
                    "views": views,
                    "pendingMutations": self.workbench.pending.len() + usize::from(self.workbench.in_flight.is_some()),
                    "creatingChild": self.workbench.creating_child.is_some(),
                    "childFailure": self.workbench.child_failure.as_ref().map(|failure| failure.message.to_string()),
                    "childFailureRequest": self.workbench.child_failure.as_ref().map(|failure| serde_json::json!({
                        "key": failure.request.key,
                        "parent": failure.request.parent,
                        "provider": failure.request.provider.map(|provider| format!("{provider:?}")),
                        "pane": failure.request.pane_id,
                        "split": failure.request.split,
                        "ambiguous": failure.ambiguous,
                    })),
                    "childBlocked": self.workbench.child_blocked,
                    "creatingChildRequest": self.workbench.creating_child.as_ref().map(|request| serde_json::json!({
                        "key": request.key,
                        "parent": request.parent,
                        "provider": request.provider.map(|provider| format!("{provider:?}")),
                        "pane": request.pane_id,
                        "split": request.split,
                    })),
                    "notice": self.workbench.notice.as_ref().map(|notice| notice.to_string()),
                    "preview": self.workbench.preview.as_ref().map(|preview| preview.label.to_string()),
                    "resize": self.workbench.resize.as_ref().map(|(id, ratio)| json!({"split": id, "ratio": ratio})),
                    "rootBounds": geometry.root.as_ref().map(bounds),
                    // The rounded canvas (as measured inside it) and how far
                    // the viewport's rectangular clip sits inside it on each
                    // side: [left, right, top, bottom].
                    "canvasBounds": geometry.canvas.as_ref().map(bounds),
                    "viewportInsets": geometry.root.zip(geometry.canvas).map(|(root, canvas)| json!([
                        f32::from(root.left() - canvas.left()),
                        f32::from(canvas.right() - root.right()),
                        f32::from(root.top() - canvas.top()),
                        f32::from(canvas.bottom() - root.bottom()),
                    ])),
                    // Overflow fades shown: [left, right, top, bottom].
                    "overflowFades": render::canvas_overflow(&self.workbench.canvas_scroll),
                    // No group or tab has an outline or underline: each
                    // group's active tab is marked by its own rounded fill
                    // (the focused group's: the accent tint), color as HSLA.
                    "paneOutline": false,
                    "tabSelection": model::panes(&layout).iter().map(|pane| {
                        let focused = model::has_split(&layout) && layout.focused_pane_id == pane.id;
                        let theme = Theme::of(cx);
                        let fill = render::tab_selected_fill(theme, focused);
                        let title = theme.tab_title(true, focused);
                        json!({
                            "pane": pane.id,
                            "active": pane.active_chat_id,
                            "focused": focused,
                            "fill": [fill.h, fill.s, fill.l, fill.a],
                            "accentTint": fill == theme.tab_focused_fill(),
                            "title": [title.h, title.s, title.l, title.a],
                            "underline": false,
                            "outline": false,
                        })
                    }).collect::<Vec<_>>(),
                    "paneBounds": geometry.panes.iter().map(|(id, pane)| json!({"id": id, "bounds": bounds(pane)})).collect::<Vec<_>>(),
                    "tabBounds": geometry.tabs.iter().map(|(pane, tabs)| {
                        let visible = geometry.visible_tabs(pane);
                        json!({
                            "pane": pane,
                            "view": geometry.tab_view.get(pane).map(bounds),
                            "tabs": tabs.iter().zip(visible.iter()).map(|((chat, tab), (_, shown))| json!({
                                "chatId": chat,
                                "bounds": bounds(tab),
                                // What can be seen and hit: clipped to the strip.
                                "visible": bounds(shown),
                            })).collect::<Vec<_>>(),
                        })
                    }).collect::<Vec<_>>(),
                    "splitBounds": geometry.splits.iter().map(|(id, split)| json!({"id": id, "bounds": bounds(split)})).collect::<Vec<_>>(),
                    "plusBounds": geometry.plus.iter().map(|(id, plus)| json!({"pane": id, "bounds": bounds(plus)})).collect::<Vec<_>>(),
                    "groupActionsBounds": geometry.group_actions.iter().map(|(id, more)| json!({"pane": id, "bounds": bounds(more)})).collect::<Vec<_>>(),
                    "menuRows": if self.workbench.menu.is_open() {
                        geometry.menu_rows.iter().map(|(id, row)| json!({"id": id, "bounds": bounds(row)})).collect::<Vec<_>>()
                    } else {
                        Vec::new()
                    },
                    "noticeDismiss": self.workbench.notice.as_ref().and(geometry.notice_dismiss.as_ref()).map(bounds),
                });
        // (Past `json!`'s recursion limit, so set after it.)
        readback["rowBounds"] = json!(row_bounds);
        readback["stripBounds"] = json!(strip_bounds);
        readback["contextRows"] = serde_json::Value::Object(context_rows);
        readback
    }

    /// The composer bound to `chat_id` in this workbench, if mounted.
    pub fn fixture_workbench_composer(&self, chat_id: &str, cx: &App) -> Option<Entity<Composer>> {
        self.workbench_composer(chat_id, cx)
    }

    /// Whether keyboard focus is inside the canvas.
    /// Focus `chat_id`'s composer input the way a click into it does.
    pub fn fixture_workbench_focus_input(
        &mut self,
        chat_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(composer) = self.workbench_composer(chat_id, cx) else {
            return false;
        };
        let handle = gpui::Focusable::focus_handle(composer.read(cx), cx);
        window.focus(&handle, cx);
        // As the click into the input it stands for would (the pane's press
        // handler focuses its group).
        self.sync_workbench_pane_focus(window, cx);
        cx.notify();
        true
    }
    /// The model `chat_id`'s composer would send with, and whether its
    /// picker popover is open.
    pub fn fixture_workbench_model(
        &self,
        chat_id: &str,
        cx: &App,
    ) -> Option<(Option<String>, bool)> {
        let composer = self.workbench_composer(chat_id, cx)?;
        let pickers = composer.read(cx).pickers().read(cx);
        Some((pickers.current_model_id(cx), pickers.is_open()))
    }
    /// QA capture: forget every layout probe, then re-render every view that
    /// paints one (Shell strips and menus, visible composers and their
    /// pickers, Main's and each tab's transcript) and refresh the window, so
    /// the next drawn frame records only controls painted after this call.
    /// Cached views would otherwise replay an earlier paint without
    /// re-recording, and an old rectangle must never pass for a fresh one.
    pub fn fixture_repaint_probes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        crate::perf_counters::clear_bounds("");
        for composer in self.visible_composers(cx) {
            composer.update(cx, |composer, cx| {
                composer.pickers().update(cx, |_, cx| cx.notify());
                cx.notify();
            });
        }
        self.transcript.update(cx, |_, cx| cx.notify());
        let tabs: Vec<_> = self
            .workbench
            .views
            .values()
            .map(|tab| tab.view.clone())
            .collect();
        for view in tabs {
            view.update(cx, |view, cx| {
                view.transcript().update(cx, |_, cx| cx.notify());
                cx.notify();
            });
        }
        cx.notify();
        window.refresh();
    }
    /// Whether `chat_id`'s compact composer controls tray is open.
    pub fn fixture_workbench_controls_tray(&self, chat_id: &str, cx: &App) -> Option<bool> {
        self.workbench_composer(chat_id, cx)
            .map(|composer| composer.read(cx).controls_tray_open())
    }
    /// The tab / Group actions menu: `{open, closing, group, paneId,
    /// chatId}` (its painted rows are `wb-menu:` probes).
    pub fn fixture_workbench_menu(&self) -> serde_json::Value {
        let menu = self.workbench.menu.get();
        json!({
            "open": self.workbench.menu.is_open(),
            "closing": self.workbench.menu.is_closing(),
            "group": menu.is_some_and(|menu| menu.group),
            "paneId": menu.map(|menu| menu.pane_id.clone()),
            "chatId": menu.map(|menu| menu.chat_id.clone()),
        })
    }
    pub fn fixture_overlay_owns_keyboard(&self, cx: &App) -> bool {
        self.overlay_owns_keyboard(cx)
    }
    /// The model ids `chat_id`'s model menu lists, in slot order.
    pub fn fixture_workbench_model_rows(&self, chat_id: &str, cx: &App) -> Vec<String> {
        self.workbench_composer(chat_id, cx)
            .map(|composer| composer.read(cx).pickers().read(cx).model_row_ids(cx))
            .unwrap_or_default()
    }
    /// The sidebar indicator for `chat_id` (Working, Idle…).
    pub fn fixture_workbench_indicator(&self, chat_id: &str, cx: &App) -> String {
        format!(
            "{:?}",
            self.state
                .read(cx)
                .indicator_for(chat_id, chrono::Utc::now())
        )
    }
    /// The outcome of `chat_id`'s latest Stop from its own composer: the
    /// engine's receipt, or `{"error": …}` with the message the user saw.
    pub fn fixture_workbench_last_stop(&self, chat_id: &str, cx: &App) -> serde_json::Value {
        let Some(composer) = self.workbench_composer(chat_id, cx) else {
            return serde_json::Value::Null;
        };
        match composer.read(cx).last_stop.get(chat_id) {
            Some(Ok(receipt)) => serde_json::to_value(receipt).unwrap_or_default(),
            Some(Err(error)) => json!({ "error": error }),
            None => serde_json::Value::Null,
        }
    }
    /// The transcript entries after `chat_id`'s latest user message
    /// containing `prompt` (that turn's output), as `{role, status, text}`.
    pub fn fixture_workbench_turn_after(
        &self,
        chat_id: &str,
        prompt: &str,
        cx: &App,
    ) -> Vec<serde_json::Value> {
        let entries = crate::chat_binding::ChatBinding::Chat(chat_id.to_string())
            .transcript(self.state.read(cx));
        let text = |entry: &luna_doc::SessionMessageEntry| {
            entry
                .parts
                .iter()
                .filter_map(|part| match part {
                    luna_doc::MessagePart::Text { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("")
        };
        let Some(start) = entries.iter().rposition(|entry| {
            entry.role == luna_doc::MessageRole::User && text(entry).contains(prompt)
        }) else {
            return Vec::new();
        };
        entries[start + 1..]
            .iter()
            .map(|entry| {
                json!({
                    "role": format!("{:?}", entry.role),
                    "status": entry.status.map(|status| format!("{status:?}")),
                    "text": text(entry),
                })
            })
            .collect()
    }
    /// Whether `chat_id`'s own composer is stopping its turn.
    pub fn fixture_workbench_interrupting(&self, chat_id: &str, cx: &App) -> bool {
        self.workbench_composer(chat_id, cx)
            .is_some_and(|composer| composer.read(cx).is_interrupting(chat_id))
    }
    /// Open `chat_id` as a tab in the focused group (as a Related row does).
    pub fn fixture_workbench_open(&mut self, chat_id: &str, cx: &mut Context<Self>) {
        self.workbench_open(TabRef::chat(chat_id), cx);
    }
    /// Open `chat_id` in a new group split right of `pane_id` (as dropping
    /// its Related row on that pane's right edge does).
    pub fn fixture_workbench_open_beside(
        &mut self,
        chat_id: &str,
        pane_id: &str,
        cx: &mut Context<Self>,
    ) -> bool {
        let destination = model::split_destination(pane_id.to_string(), Edge::Right);
        self.workbench_mutate(
            command::open_tab(&TabRef::chat(chat_id), Some(destination)),
            cx,
        )
    }
    /// Open a Side tab under its exact source parent in a new group split
    /// right of the focused one (as dropping its Related row there does).
    pub fn fixture_workbench_open_side_split(
        &mut self,
        side_id: &str,
        parent: &str,
        cx: &mut Context<Self>,
    ) -> bool {
        let layout = self.workbench.layout();
        let tab = TabRef {
            chat_id: side_id.to_string(),
            side_parent_id: Some(parent.to_string()),
        };
        let destination = model::split_destination(layout.focused_pane_id.clone(), Edge::Right);
        self.workbench_mutate(command::open_tab(&tab, Some(destination)), cx)
    }
    /// A tab's view header, as `PaneChatView::fixture_strip` reads it.
    pub fn fixture_workbench_strip(&self, chat_id: &str, cx: &App) -> serde_json::Value {
        self.workbench
            .views
            .get(chat_id)
            .map(|tab| tab.view.read(cx).fixture_strip(cx))
            .unwrap_or(serde_json::Value::Null)
    }
    /// A Related/sidebar row's ordinary task action (`archive`, `unarchive`,
    /// `pin`, `unpin`, `settle`, `unsettle`, `delete`), past any
    /// confirmation dialog.
    pub fn fixture_chat_action(&mut self, chat_id: &str, action: &str, cx: &mut Context<Self>) {
        let id = chat_id.to_string();
        match action {
            "archive" => self.execute_task_action(id, TaskConfirmation::Archive, cx),
            "unarchive" => self.set_chat_archived(id, false, cx),
            "pin" => self.set_chat_pinned(id, true, cx),
            "unpin" => self.set_chat_pinned(id, false, cx),
            "settle" => self.execute_task_action(id, TaskConfirmation::Settle, cx),
            "unsettle" => self.set_chat_settled(id, false, cx),
            "delete" => self.execute_task_action(id, TaskConfirmation::Delete, cx),
            // The chat menu's context fork ("… with context from here…").
            "fork" => self.open_history_fork(id, ConversationForkBoundaryKind::Tip, None, cx),
            _ => {}
        }
    }
    /// The tab menu's "Open as its own workspace" for `chat_id`.
    pub fn fixture_workbench_open_as_main(&mut self, chat_id: &str, cx: &mut Context<Self>) {
        self.open_pane_chat_as_main(chat_id.to_string(), cx);
    }
    /// A Related row of `kind` (`side`, `child`, `alongside`) dropped on
    /// the left sidebar with no slot, as a real drag drops it.
    pub fn fixture_place_in_sidebar(&mut self, chat_id: &str, kind: &str, cx: &mut Context<Self>) {
        let kind = match kind {
            "side" => super::related_chats::RelatedKind::Side,
            "child" => super::related_chats::RelatedKind::Child,
            _ => super::related_chats::RelatedKind::Alongside,
        };
        let drag = SidebarChatDrag {
            chat_id: chat_id.to_string(),
            origin: DragOrigin::Related(kind),
            scope: SidebarDragScope::Unpinned(None),
            title: SharedString::default(),
            project: SharedString::default(),
            harness: None,
        };
        self.place_in_sidebar(&drag, kind, None, cx);
    }
    /// Seed a legacy device-local side chat mark (as an older build saved
    /// it), then let the migration run as on any state change.
    pub fn fixture_seed_side_marks(
        &mut self,
        chat_id: &str,
        marks: crate::settings::SideChatMarks,
        cx: &mut Context<Self>,
    ) {
        self.settings
            .side_chat_marks
            .insert(chat_id.to_string(), marks);
        self.migrate_side_chat_marks(cx);
    }
    /// Whether `chat_id`'s tab renders a composer keys can reach.
    pub fn fixture_workbench_tab_has_composer(&self, chat_id: &str, cx: &App) -> bool {
        self.workbench_tab_composer(chat_id, cx).is_some()
    }
    /// Close `chat_id`'s tab (as its close glyph does).
    pub fn fixture_workbench_close(&mut self, chat_id: &str, cx: &mut Context<Self>) -> bool {
        self.workbench_close_tab(chat_id, cx)
    }
    /// The chat whose tab (not its composer) holds keyboard focus.
    pub fn fixture_workbench_focused_tab(&self, window: &Window) -> Option<String> {
        self.workbench
            .tab_focus
            .iter()
            .find(|(_, handle)| handle.is_focused(window))
            .map(|(chat, _)| chat.clone())
    }
    /// The routed chat of every shell Escape so far, with its outcome.
    pub fn fixture_workbench_escape_log(&self) -> Vec<(Option<String>, &'static str)> {
        self.workbench.escape_log.clone()
    }
    /// The chat that composer-scoped keys would reach right now.
    pub fn fixture_workbench_keyboard_chat(&self, window: &Window, cx: &App) -> Option<String> {
        self.keyboard_composer(window, cx)
            .and_then(|(chat, _)| chat)
    }
    pub fn fixture_workbench_has_focus(&self, window: &Window, cx: &App) -> bool {
        self.workbench_holds_focus(window, cx)
    }
    /// Main transcript's retained outgoing-chat caches (slice 6).
    pub fn fixture_main_retained(&self, cx: &App) -> serde_json::Value {
        self.transcript.read(cx).fixture_retained()
    }

    /// Lower (or restore) Main transcript's retained-cache ceilings.
    pub fn fixture_set_main_retained_limits(
        &mut self,
        limits: Option<(usize, usize, usize)>,
        cx: &mut Context<Self>,
    ) {
        self.transcript.update(cx, |transcript, _| {
            transcript.fixture_set_retained_limits(limits)
        });
    }

    /// Cmd+N: the New session action's own path (a new thread in the
    /// current project).
    pub fn fixture_new_session(&mut self, cx: &mut Context<Self>) {
        self.open_new_session(cx);
        cx.notify();
    }

    /// The main composer's surroundings as last painted, on either route:
    /// the frame's bottom row (tab strips) and canvas, the checkout row
    /// above the pill (the new thread's target chips and open target menu,
    /// or a chat's read-only checkout and branch), the pill, and the route
    /// clock's two chrome opacities (window coordinates).
    pub fn fixture_frame_footer(&self, cx: &App) -> serde_json::Value {
        let rect = |b: &Bounds<Pixels>| {
            json!([
                f32::from(b.origin.x),
                f32::from(b.origin.y),
                f32::from(b.size.width),
                f32::from(b.size.height),
            ])
        };
        let geometry = self.workbench.geometry.borrow();
        let dock = self.composer_dock.borrow().frame;
        let composer = self.composer.read(cx);
        json!({
            "framed": self.workbench_framed(cx),
            "row": geometry.tab_row.as_ref().map(rect),
            "canvas": geometry.canvas.as_ref().map(rect),
            "strips": geometry
                .strips
                .iter()
                .map(|(pane, bounds)| json!({"pane": pane, "bounds": rect(bounds)}))
                .collect::<Vec<_>>(),
            "contextRow": composer.fixture_context_row(cx),
            "targets": composer.pickers().read(cx).fixture_target_readback(cx),
            "pill": composer.surface_bounds().get().as_ref().map(rect),
            "dock": {
                "targetOpacity": dock.selectors(),
                "threadOpacity": dock.footer(),
                "active": dock.active,
                "docked": dock.docked,
            },
        })
    }

    /// The painted composer surface (pill) of `chat_id`: [x, y, w, h].
    pub fn fixture_composer_surface(&self, chat_id: &str, cx: &App) -> Option<[f32; 4]> {
        let composer = self.workbench_composer(chat_id, cx)?;
        let bounds = composer.read(cx).surface_bounds().get()?;
        Some([
            f32::from(bounds.origin.x),
            f32::from(bounds.origin.y),
            f32::from(bounds.size.width),
            f32::from(bounds.size.height),
        ])
    }

    /// SIMULATION for QA: age `chat_id`'s tab transcript paint stamp.
    pub fn fixture_age_tab_paint(&self, chat_id: &str, ms: u64, cx: &App) {
        if let Some(tab) = self.workbench.views.get(chat_id) {
            tab.view
                .read(cx)
                .transcript()
                .read(cx)
                .fixture_age_paint(ms);
        }
    }

    /// How long ago `chat_id`'s workbench tab transcript was painted.
    pub fn fixture_tab_painted_age_ms(&self, chat_id: &str, cx: &App) -> Option<f64> {
        self.workbench.views.get(chat_id).and_then(|tab| {
            tab.view
                .read(cx)
                .transcript()
                .read(cx)
                .fixture_painted_age_ms()
        })
    }

    /// Scroll Main's transcript to its first rows or back to its tail.
    pub fn fixture_main_transcript_edge(&mut self, top: bool, cx: &mut Context<Self>) {
        self.transcript
            .update(cx, |transcript, cx| transcript.fixture_scroll_edge(top, cx));
    }

    /// `chat_id`'s transcript row projection: Main's view when it is the
    /// selected chat, else its workbench tab's view (slice 6 baseline).
    pub fn fixture_transcript_rows(&self, chat_id: &str, cx: &App) -> serde_json::Value {
        if self.state.read(cx).selected_chat.as_deref() == Some(chat_id) {
            return self.transcript.read(cx).fixture_rows(cx);
        }
        self.workbench
            .views
            .get(chat_id)
            .map(|tab| tab.view.read(cx).transcript().read(cx).fixture_rows(cx))
            .unwrap_or(serde_json::Value::Null)
    }

    /// Type into the composer bound to `chat_id` (Main's or its tab's) and
    /// optionally submit it through the composer's own Submitted path.
    pub fn fixture_workbench_type(
        &mut self,
        chat_id: &str,
        text: &str,
        send: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(composer) = self.workbench_composer(chat_id, cx) else {
            return false;
        };
        let input = composer.read(cx).input.clone();
        let text = text.to_string();
        input.update(cx, |input, cx| {
            input.set_text(text, cx);
            if send {
                cx.emit(crate::composer::ComposerInputEvent::Submitted);
            }
        });
        true
    }

    /// User message texts of `chat_id` as its view shows them (Main: the
    /// selected transcript; a tab: its pane watch).
    pub fn fixture_workbench_user_texts(&self, chat_id: &str, cx: &App) -> Vec<String> {
        crate::chat_binding::ChatBinding::Chat(chat_id.to_string())
            .transcript(self.state.read(cx))
            .iter()
            .filter(|entry| entry.role == luna_doc::MessageRole::User)
            .flat_map(|entry| {
                entry.parts.iter().filter_map(|part| match part {
                    luna_doc::MessagePart::Text { text, .. } => Some(text.clone()),
                    _ => None,
                })
            })
            .collect()
    }

    /// Run a palette workbench command by its label (the palette's own path).
    pub fn fixture_workbench_action(&mut self, label: &str, cx: &mut Context<Self>) -> bool {
        let Some(action) = self
            .workbench_palette_actions()
            .into_iter()
            .find(|action| action.label() == label)
        else {
            return false;
        };
        self.run_workbench_action(action, cx);
        true
    }

    /// Park (`true`) or deliver (`false`) mutation completions: a late
    /// response after a profile or main switch must change nothing.
    pub fn fixture_workbench_hold_responses(
        &mut self,
        hold: bool,
        cx: &mut Context<Self>,
    ) -> usize {
        self.workbench.hold_responses = hold;
        if hold {
            return 0;
        }
        let held = std::mem::take(&mut self.workbench.held);
        let delivered = held.len();
        for (request, result) in held {
            self.finish_workbench_mutation(request, result, cx);
        }
        delivered
    }

    /// Install another engine handle on the same state (profile switch QA).
    pub fn fixture_workbench_engine_switch(
        &mut self,
        engine: crate::state::EngineHandle,
        cx: &mut Context<Self>,
    ) {
        self.state.update(cx, |state, cx| {
            state.fixture_attachment_engine(engine);
            cx.notify();
        });
    }

    /// An empty group below `pane_id` (the strip split control's path).
    /// Lose the next New thread's response after the engine handled it.
    pub fn fixture_workbench_lose_child_response(&mut self) {
        self.workbench.lose_child_response = true;
    }
    /// A New thread from `pane_id`'s group with `provider`, as the chooser's
    /// Create asks for it.
    pub fn fixture_workbench_request_child(
        &mut self,
        pane_id: &str,
        provider: Option<luna_proto::HarnessId>,
        cx: &mut Context<Self>,
    ) {
        self.request_child(Some(pane_id.to_string()), false, provider, cx);
    }
    /// The failure card's Retry / Check status.
    pub fn fixture_workbench_retry_child(&mut self, cx: &mut Context<Self>) {
        self.workbench_retry_child(cx);
    }

    pub fn fixture_workbench_split_empty(&mut self, pane_id: &str, cx: &mut Context<Self>) {
        self.workbench_split_pane(pane_id, Edge::Bottom, cx);
    }

    /// The group's split control: a new blank child of Main in a new group
    /// to the right of `pane_id` (nothing without room).
    pub fn fixture_workbench_split_child(&mut self, pane_id: &str, cx: &mut Context<Self>) {
        self.workbench_new_child_in_split(pane_id.to_string(), cx);
    }

    /// An empty group on `edge` ("right" or "bottom") beside `pane_id`, if
    /// there is room (the layout's split command, as drag-to-edge and the
    /// move-to-split paths use; the strip control now makes a child).
    pub fn fixture_workbench_split(&mut self, pane_id: &str, edge: &str, cx: &mut Context<Self>) {
        let edge = if edge == "right" {
            Edge::Right
        } else {
            Edge::Bottom
        };
        self.workbench_split_pane(pane_id, edge, cx);
    }

    /// A group's own Close group control.
    pub fn fixture_workbench_close_pane(&mut self, pane_id: &str, cx: &mut Context<Self>) {
        self.workbench_close_pane(pane_id, cx);
    }

    /// Open a right-sidebar surface by name (`related`, `changes`, `files`)
    /// through the same paths as the picker and frame controls.
    pub fn fixture_open_surface(
        &mut self,
        kind: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match kind {
            "related" => self.show_right_place(RightPlace::Related, cx),
            "changes" => {
                let main = self.active_chat.clone();
                self.add_diff_surface_for(main, cx);
            }
            "files" => {
                if !self.right_pane_open(cx) {
                    self.toggle_right_pane(cx);
                }
                self.add_files_surface(window, cx);
            }
            _ => {}
        }
        cx.notify();
    }

    /// Open Settings on its General page (as the app menu does).
    pub fn fixture_open_settings(&mut self, cx: &mut Context<Self>) {
        self.open_settings(SettingsSection::General, cx);
    }
    /// Leave Settings (its Back to the chat) or step back in route history.
    pub fn fixture_leave_settings(&mut self, back: bool, cx: &mut Context<Self>) {
        if back {
            self.navigate_back(cx);
        } else {
            self.close_settings(cx);
        }
    }

    /// The surrounding frame as painted: sidebar and right widths (live,
    /// mid-tween included), their targets, and the canvas bounds.
    pub fn fixture_frame_readback(&self, cx: &App) -> serde_json::Value {
        let geometry = self.workbench.geometry.borrow();
        json!({
            "viewportWidth": self.viewport_width,
            "sidebarNow": self.sidebar_now(),
            "sidebarTarget": self.sidebar_target(),
            "sidebarCollapsed": self.settings.sidebar_collapsed,
            // The route's own visibility (Settings keeps a per-visit one) and
            // the width actually painted.
            "sidebarShown": !self.sidebar_collapsed(),
            "settingsRoute": matches!(self.route, Route::Settings(_)),
            "sidebarWidthNow": self.sidebar_now(),
            "rightNow": self.right_now(cx),
            "rightOpen": self.right_pane_open(cx),
            "rightActive": format!("{:?}", self.resolved_right_active(cx)),
            "canvas": geometry.root.map(|root| json!({
                "x": f32::from(root.origin.x),
                "y": f32::from(root.origin.y),
                "w": f32::from(root.size.width),
                "h": f32::from(root.size.height),
            })),
            "rightTarget": self.right_target(cx),
            // The chat the right pane's active Changes view is bound to (None:
            // not a Changes view, or the selected chat's own).
            "rightDiffTarget": self
                .active_diff_entity(cx)
                .and_then(|changes| changes.read(cx).target_chat_id().map(str::to_string)),
            "tabRow": geometry.tab_row.map(|row| json!({
                "x": f32::from(row.origin.x),
                "y": f32::from(row.origin.y),
                "w": f32::from(row.size.width),
                "h": f32::from(row.size.height),
            })),
            // Each group's tab strip in the titlebar's tab row, by pane id.
            "strips": geometry.strips.iter().map(|(pane, strip)| json!({
                "pane": pane,
                "x": f32::from(strip.origin.x),
                "y": f32::from(strip.origin.y),
                "w": f32::from(strip.size.width),
                "h": f32::from(strip.size.height),
            })).collect::<Vec<_>>(),
            // The app-wide reduced-motion flag (from the system setting):
            // under it, width changes land without a transition.
            "reducedMotion": crate::motion::reduced_motion(cx),
            "motionScale": crate::motion::speed_scale(),
            "titlebarHeight": Theme::TITLEBAR_HEIGHT,
            "toolbarHeight": crate::surface_chrome::HEADER_HEIGHT,
            "stripHeight": render::STRIP_HEIGHT,
        })
    }

    /// The live draft text of the composer bound to `chat_id`.
    pub fn fixture_workbench_draft(&self, chat_id: &str, cx: &App) -> Option<String> {
        self.workbench_composer(chat_id, cx)
            .map(|composer| composer.read(cx).input.read(cx).text().to_string())
    }

    /// Whether `chat_id`'s composer has a send or receipt check in flight.
    pub fn fixture_workbench_sending(&self, chat_id: &str, cx: &App) -> bool {
        self.workbench_composer(chat_id, cx)
            .is_some_and(|composer| composer.read(cx).has_work_in_flight())
    }
}
