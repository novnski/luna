//! UI helpers over the canonical `luna_proto::chat_workbench` types. The
//! layout, tabs, commands and destinations are the engine's own wire types;
//! this module only adds read helpers, ID minting, labels and the pointer
//! geometry (zones, strip insertion) that is purely the UI's.

pub(in crate::shell) use luna_proto::{
    ChatWorkbenchCommand as Command, ChatWorkbenchMutationResult as MutationResult,
    ChatWorkbenchNode as Node, ChatWorkbenchSnapshot as Layout, ChatWorkbenchTab as TabRef,
    ClosedWorkbenchTab as ClosedTab, WorkbenchAxis as Axis, WorkbenchDestination as Destination,
    WorkbenchEdge as Edge,
};

/// Engine capability for the workbench RPCs.
pub(in crate::shell) const CAPABILITY: &str = luna_proto::capabilities::CHAT_WORKBENCH_V1;
/// Ratio bounds the engine accepts for `resizeSplit`.
pub(in crate::shell) const RATIO_MIN: f32 = 0.1;
pub(in crate::shell) const RATIO_MAX: f32 = 0.9;

/// One pane's fields, borrowed from a [`Node::Pane`].
#[derive(Debug, Clone, Copy)]
pub(in crate::shell) struct PaneRef<'a> {
    pub id: &'a str,
    pub tabs: &'a [TabRef],
    pub active_chat_id: Option<&'a str>,
}

impl PaneRef<'_> {
    pub fn holds(&self, chat_id: &str) -> bool {
        self.tabs.iter().any(|tab| tab.chat_id == chat_id)
    }
}

pub(in crate::shell) fn as_pane(node: &Node) -> Option<PaneRef<'_>> {
    match node {
        Node::Pane {
            id,
            tabs,
            active_chat_id,
        } => Some(PaneRef {
            id,
            tabs,
            active_chat_id: active_chat_id.as_deref(),
        }),
        Node::Split { .. } => None,
    }
}

pub(in crate::shell) fn depth(node: &Node) -> usize {
    match node {
        Node::Pane { .. } => 1,
        Node::Split { first, second, .. } => 1 + depth(first).max(depth(second)),
    }
}

/// `(axis, ratio)` of a split node.
pub(in crate::shell) fn split_of(layout: &Layout, split_id: &str) -> Option<(Axis, f64)> {
    match layout.root.node(split_id)? {
        Node::Split { axis, ratio, .. } => Some((*axis, *ratio)),
        Node::Pane { .. } => None,
    }
}

/// Every pane in reading order (first before second).
pub(in crate::shell) fn panes(layout: &Layout) -> Vec<PaneRef<'_>> {
    fn walk<'a>(node: &'a Node, out: &mut Vec<PaneRef<'a>>) {
        match node {
            Node::Pane { .. } => out.extend(as_pane(node)),
            Node::Split { first, second, .. } => {
                walk(first, out);
                walk(second, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(&layout.root, &mut out);
    out
}

pub(in crate::shell) fn pane<'a>(layout: &'a Layout, id: &str) -> Option<PaneRef<'a>> {
    layout.pane(id).and_then(as_pane)
}

pub(in crate::shell) fn pane_for_chat<'a>(
    layout: &'a Layout,
    chat_id: &str,
) -> Option<PaneRef<'a>> {
    layout.pane_for_chat(chat_id).and_then(as_pane)
}

pub(in crate::shell) fn tab<'a>(layout: &'a Layout, chat_id: &str) -> Option<&'a TabRef> {
    pane_for_chat(layout, chat_id)
        .and_then(|pane| pane.tabs.iter().find(|tab| tab.chat_id == chat_id))
}

/// Every open chat ID, each once.
pub(in crate::shell) fn open_chat_ids(layout: &Layout) -> Vec<String> {
    layout
        .root
        .tabs()
        .into_iter()
        .map(|tab| tab.chat_id.clone())
        .collect()
}

/// Chats visible right now: each pane's selected tab.
pub(in crate::shell) fn visible_chat_ids(layout: &Layout) -> Vec<String> {
    panes(layout)
        .into_iter()
        .filter_map(|pane| pane.active_chat_id.map(str::to_string))
        .collect()
}

/// The newest closed tab not open again (what Cmd-Shift-T restores).
pub(in crate::shell) fn newest_reopenable(layout: &Layout) -> Option<&ClosedTab> {
    layout
        .closed_tabs
        .iter()
        .find(|closed| !layout.contains_chat(&closed.tab.chat_id))
}

pub(in crate::shell) fn has_split(layout: &Layout) -> bool {
    matches!(layout.root, Node::Split { .. })
}

/// The shared reducer, for an optimistic preview: `Some((next, changed))`
/// when the engine would accept `command`; `None` when it would refuse.
/// Never advances the revision (persistence does).
pub(in crate::shell) fn reduce(layout: &Layout, command: &Command) -> Option<(Layout, bool)> {
    let mut next = layout.clone();
    let changed = next.apply(command).ok()?;
    Some((next, changed))
}

pub(in crate::shell) fn fresh_id(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4().simple())
}

pub(in crate::shell) fn split_destination(node_id: impl Into<String>, edge: Edge) -> Destination {
    Destination::Split {
        node_id: node_id.into(),
        edge,
        new_pane_id: fresh_id("pane"),
        split_id: fresh_id("split"),
    }
}

pub(in crate::shell) fn edge_label(edge: Edge) -> &'static str {
    match edge {
        Edge::Left => "Split left",
        Edge::Right => "Split right",
        Edge::Top => "Split up",
        Edge::Bottom => "Split down",
    }
}

/// Command constructors (pane/split IDs are minted once, here).
pub(in crate::shell) mod command {
    use super::*;

    pub fn open_tab(tab: &TabRef, destination: Option<Destination>) -> Command {
        Command::OpenTab {
            tab: tab.clone(),
            destination,
        }
    }

    pub fn focus_tab(chat_id: &str) -> Command {
        Command::FocusTab {
            chat_id: chat_id.to_string(),
        }
    }

    /// Make `pane_id` the focused group, keeping its selected tab (an
    /// empty group can be focused too).
    pub fn focus_pane(pane_id: &str) -> Command {
        Command::FocusPane {
            pane_id: pane_id.to_string(),
        }
    }

    pub fn move_tab(chat_id: &str, destination: Destination) -> Command {
        Command::MoveTab {
            chat_id: chat_id.to_string(),
            destination,
        }
    }

    pub fn close_tab(chat_id: &str) -> Command {
        Command::CloseTab {
            chat_id: chat_id.to_string(),
        }
    }

    pub fn close_other_tabs(pane_id: &str, keep_chat_id: &str) -> Command {
        Command::CloseOtherTabs {
            pane_id: pane_id.to_string(),
            keep_chat_id: keep_chat_id.to_string(),
        }
    }

    pub fn close_pane(pane_id: &str) -> Command {
        Command::ClosePane {
            pane_id: pane_id.to_string(),
        }
    }

    pub fn split_pane(node_id: &str, edge: Edge) -> Command {
        Command::SplitPane {
            node_id: node_id.to_string(),
            edge,
            new_pane_id: fresh_id("pane"),
            split_id: fresh_id("split"),
        }
    }

    pub fn resize_split(split_id: &str, ratio: f32) -> Command {
        Command::ResizeSplit {
            split_id: split_id.to_string(),
            ratio: f64::from(ratio.clamp(RATIO_MIN, RATIO_MAX)),
        }
    }

    /// Every split's ratio set from the groups (panes, not tabs) on each
    /// side, so all groups get equal area; axes, order and IDs unchanged.
    pub fn balance_panes() -> Command {
        Command::BalancePanes
    }

    pub fn reopen_closed(chat_id: Option<&str>) -> Command {
        Command::ReopenClosed {
            chat_id: chat_id.map(str::to_string),
        }
    }
}

/// Where a drop lands, resolved from the pointer before release.
#[derive(Debug, Clone, PartialEq)]
pub(in crate::shell) enum DropTarget {
    /// Insert into a tab strip at this index of the RESULTING order.
    Strip { pane_id: String, index: usize },
    /// The pane body's center: join that group as its last tab.
    Join { pane_id: String },
    /// One of a pane's four edges, or (`node_id` = root) the outer edge.
    Split {
        node_id: String,
        edge: Edge,
        outer: bool,
    },
}

/// Classify a point inside a pane body into center (`None`) or one of four
/// edge zones. Zones are about a quarter of the pane, capped so a small pane
/// keeps a usable center; a corner goes to the relatively nearer edge.
pub(in crate::shell) fn body_zone(
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    edge_cap: f32,
) -> Option<Edge> {
    if width <= 0.0 || height <= 0.0 {
        return None;
    }
    let zone_x = (width * 0.24).min(edge_cap);
    let zone_y = (height * 0.24).min(edge_cap);
    let mut best: Option<(Edge, f32)> = None;
    for (edge, distance, zone) in [
        (Edge::Left, x, zone_x),
        (Edge::Right, width - x, zone_x),
        (Edge::Top, y, zone_y),
        (Edge::Bottom, height - y, zone_y),
    ] {
        if distance <= zone {
            let weight = distance / zone.max(1.0);
            if best.is_none_or(|(_, best_weight)| weight < best_weight) {
                best = Some((edge, weight));
            }
        }
    }
    best.map(|(edge, _)| edge)
}

/// Insertion index in the resulting order for a pointer at `x` over a strip
/// whose tabs span `spans` (left, right) in order. `from` is the dragged
/// tab's index when it already sits in this strip: the index is then counted
/// after its removal, so dropping it in place is a no-op.
pub(in crate::shell) fn strip_index(x: f32, spans: &[(f32, f32)], from: Option<usize>) -> usize {
    let mut slot = spans.len();
    for (index, (left, right)) in spans.iter().enumerate() {
        if x < (left + right) / 2.0 {
            slot = index;
            break;
        }
    }
    match from {
        Some(from) if slot > from => slot - 1,
        _ => slot,
    }
}
