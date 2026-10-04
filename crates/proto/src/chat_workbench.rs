//! Presentation-only conversation tabs and split trees. This shared reducer
//! never touches a chat, transcript, draft, provider process or permission.
use std::collections::HashSet;

use serde::{Deserialize, Serialize};

pub const CHAT_WORKBENCH_VERSION: u32 = 1;
pub const WORKBENCH_MAX_TABS: usize = 128;
pub const WORKBENCH_MAX_PANES: usize = 16;
pub const WORKBENCH_MAX_DEPTH: usize = 16;
pub const WORKBENCH_CLOSED_LIMIT: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChatWorkbenchTab {
    pub chat_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side_parent_id: Option<String>,
}

impl ChatWorkbenchTab {
    pub fn chat(id: impl Into<String>) -> Self {
        Self {
            chat_id: id.into(),
            side_parent_id: None,
        }
    }

    fn validate(&self) -> Result<(), ChatWorkbenchError> {
        valid_id(&self.chat_id)?;
        if let Some(parent) = &self.side_parent_id {
            valid_id(parent)?;
            if parent == &self.chat_id {
                return Err(invalid("A side chat cannot be its own source"));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkbenchAxis {
    Horizontal,
    Vertical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkbenchEdge {
    Left,
    Right,
    Top,
    Bottom,
}

impl WorkbenchEdge {
    pub fn axis(self) -> WorkbenchAxis {
        match self {
            Self::Left | Self::Right => WorkbenchAxis::Horizontal,
            Self::Top | Self::Bottom => WorkbenchAxis::Vertical,
        }
    }

    pub fn before(self) -> bool {
        matches!(self, Self::Left | Self::Top)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum ChatWorkbenchNode {
    #[serde(rename_all = "camelCase")]
    Pane {
        id: String,
        tabs: Vec<ChatWorkbenchTab>,
        active_chat_id: Option<String>,
    },
    Split {
        id: String,
        axis: WorkbenchAxis,
        ratio: f64,
        first: Box<Self>,
        second: Box<Self>,
    },
}

impl ChatWorkbenchNode {
    pub fn id(&self) -> &str {
        match self {
            Self::Pane { id, .. } | Self::Split { id, .. } => id,
        }
    }

    pub fn node(&self, id: &str) -> Option<&Self> {
        if self.id() == id {
            return Some(self);
        }
        match self {
            Self::Split { first, second, .. } => first.node(id).or_else(|| second.node(id)),
            Self::Pane { .. } => None,
        }
    }

    fn node_mut(&mut self, id: &str) -> Option<&mut Self> {
        if self.id() == id {
            return Some(self);
        }
        match self {
            Self::Split { first, second, .. } => first.node_mut(id).or_else(|| second.node_mut(id)),
            Self::Pane { .. } => None,
        }
    }

    pub fn pane_for_chat(&self, chat_id: &str) -> Option<&Self> {
        match self {
            Self::Pane { tabs, .. } => tabs
                .iter()
                .any(|tab| tab.chat_id == chat_id)
                .then_some(self),
            Self::Split { first, second, .. } => first
                .pane_for_chat(chat_id)
                .or_else(|| second.pane_for_chat(chat_id)),
        }
    }

    pub fn first_pane(&self) -> &Self {
        match self {
            Self::Pane { .. } => self,
            Self::Split { first, .. } => first.first_pane(),
        }
    }

    pub fn tabs(&self) -> Vec<&ChatWorkbenchTab> {
        match self {
            Self::Pane { tabs, .. } => tabs.iter().collect(),
            Self::Split { first, second, .. } => {
                let mut tabs = first.tabs();
                tabs.extend(second.tabs());
                tabs
            }
        }
    }

    fn neighbor(&self, pane_id: &str) -> Option<&str> {
        match self {
            Self::Pane { .. } => None,
            Self::Split { first, second, .. } if first.node(pane_id).is_some() => first
                .neighbor(pane_id)
                .or_else(|| Some(second.first_pane().id())),
            Self::Split { first, second, .. } if second.node(pane_id).is_some() => second
                .neighbor(pane_id)
                .or_else(|| Some(first.first_pane().id())),
            Self::Split { .. } => None,
        }
    }

    fn without_pane(self, pane_id: &str) -> Option<Self> {
        match self {
            Self::Pane { ref id, .. } if id == pane_id => None,
            Self::Pane { .. } => Some(self),
            Self::Split {
                id,
                axis,
                ratio,
                first,
                second,
            } => match (first.without_pane(pane_id), second.without_pane(pane_id)) {
                (Some(first), Some(second)) => Some(Self::Split {
                    id,
                    axis,
                    ratio,
                    first: Box::new(first),
                    second: Box::new(second),
                }),
                (Some(node), None) | (None, Some(node)) => Some(node),
                (None, None) => None,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClosedWorkbenchTab {
    pub tab: ChatWorkbenchTab,
    pub pane_id: String,
    pub index: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChatWorkbenchSnapshot {
    pub version: u32,
    pub main_chat_id: String,
    pub revision: u64,
    pub root: ChatWorkbenchNode,
    pub focused_pane_id: String,
    pub closed_tabs: Vec<ClosedWorkbenchTab>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum WorkbenchDestination {
    #[serde(rename_all = "camelCase")]
    Pane {
        pane_id: String,
        #[serde(default)]
        index: Option<usize>,
    },
    #[serde(rename_all = "camelCase")]
    Split {
        node_id: String,
        edge: WorkbenchEdge,
        new_pane_id: String,
        split_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase", deny_unknown_fields)]
pub enum ChatWorkbenchCommand {
    OpenTab {
        tab: ChatWorkbenchTab,
        #[serde(default)]
        destination: Option<WorkbenchDestination>,
    },
    #[serde(rename_all = "camelCase")]
    FocusTab { chat_id: String },
    /// Focus a group without changing its selected tab, including an empty group.
    #[serde(rename_all = "camelCase")]
    FocusPane { pane_id: String },
    #[serde(rename_all = "camelCase")]
    MoveTab {
        chat_id: String,
        destination: WorkbenchDestination,
    },
    #[serde(rename_all = "camelCase")]
    CloseTab { chat_id: String },
    #[serde(rename_all = "camelCase")]
    CloseOtherTabs {
        pane_id: String,
        keep_chat_id: String,
    },
    #[serde(rename_all = "camelCase")]
    ClosePane { pane_id: String },
    #[serde(rename_all = "camelCase")]
    SplitPane {
        node_id: String,
        edge: WorkbenchEdge,
        new_pane_id: String,
        split_id: String,
    },
    #[serde(rename_all = "camelCase")]
    ResizeSplit { split_id: String, ratio: f64 },
    /// Give each leaf pane equal area while preserving the split tree.
    /// Empty panes count once, regardless of the number of tabs in a pane.
    /// Existing ratio bounds apply to extreme, unevenly nested trees.
    BalancePanes,
    #[serde(rename_all = "camelCase")]
    ReopenClosed {
        #[serde(default)]
        chat_id: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatWorkbenchError {
    pub code: &'static str,
    pub message: String,
}
impl std::fmt::Display for ChatWorkbenchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for ChatWorkbenchError {}
fn invalid(message: impl Into<String>) -> ChatWorkbenchError {
    ChatWorkbenchError {
        code: "invalidWorkbench",
        message: message.into(),
    }
}
fn missing(message: impl Into<String>) -> ChatWorkbenchError {
    ChatWorkbenchError {
        code: "missingWorkbenchTarget",
        message: message.into(),
    }
}
fn protected() -> ChatWorkbenchError {
    ChatWorkbenchError {
        code: "protectedMainChat",
        message: "The Main chat cannot be closed".into(),
    }
}

/// IDs are opaque tokens, never storage paths or unbounded user-facing titles.
pub fn valid_workbench_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b':' | b'.'))
}
fn valid_id(id: &str) -> Result<(), ChatWorkbenchError> {
    valid_workbench_id(id)
        .then_some(())
        .ok_or_else(|| invalid("Invalid workbench identity"))
}

impl ChatWorkbenchSnapshot {
    pub fn new(main_chat_id: impl Into<String>) -> Self {
        let main_chat_id = main_chat_id.into();
        Self {
            version: CHAT_WORKBENCH_VERSION,
            main_chat_id: main_chat_id.clone(),
            revision: 0,
            root: ChatWorkbenchNode::Pane {
                id: "main-pane".into(),
                tabs: vec![ChatWorkbenchTab::chat(main_chat_id.clone())],
                active_chat_id: Some(main_chat_id),
            },
            focused_pane_id: "main-pane".into(),
            closed_tabs: Vec::new(),
        }
    }

    pub fn pane(&self, id: &str) -> Option<&ChatWorkbenchNode> {
        self.root
            .node(id)
            .filter(|node| matches!(node, ChatWorkbenchNode::Pane { .. }))
    }
    pub fn pane_for_chat(&self, id: &str) -> Option<&ChatWorkbenchNode> {
        self.root.pane_for_chat(id)
    }
    pub fn contains_chat(&self, id: &str) -> bool {
        self.pane_for_chat(id).is_some()
    }
    pub fn focused_chat_id(&self) -> Option<&str> {
        match self.pane(&self.focused_pane_id)? {
            ChatWorkbenchNode::Pane { active_chat_id, .. } => active_chat_id.as_deref(),
            ChatWorkbenchNode::Split { .. } => None,
        }
    }

    pub fn validate(&self) -> Result<(), ChatWorkbenchError> {
        if self.version != CHAT_WORKBENCH_VERSION {
            return Err(invalid("Unsupported layout version"));
        }
        valid_id(&self.main_chat_id)?;
        let mut nodes = HashSet::new();
        let mut chats = HashSet::new();
        let mut panes = 0;
        fn visit<'a>(
            node: &'a ChatWorkbenchNode,
            depth: usize,
            nodes: &mut HashSet<&'a str>,
            chats: &mut HashSet<&'a str>,
            panes: &mut usize,
        ) -> Result<(), ChatWorkbenchError> {
            if depth > WORKBENCH_MAX_DEPTH {
                return Err(invalid("Split tree is too deep"));
            }
            valid_id(node.id())?;
            if !nodes.insert(node.id()) {
                return Err(invalid("Duplicate split/pane ID"));
            }
            match node {
                ChatWorkbenchNode::Pane {
                    tabs,
                    active_chat_id,
                    ..
                } => {
                    *panes += 1;
                    if *panes > WORKBENCH_MAX_PANES {
                        return Err(invalid("Too many split panes"));
                    }
                    for tab in tabs {
                        tab.validate()?;
                        if !chats.insert(&tab.chat_id) {
                            return Err(invalid("A chat is open more than once"));
                        }
                        if chats.len() > WORKBENCH_MAX_TABS {
                            return Err(invalid("Too many open tabs"));
                        }
                    }
                    if tabs.is_empty() != active_chat_id.is_none()
                        || active_chat_id
                            .as_ref()
                            .is_some_and(|id| !tabs.iter().any(|tab| &tab.chat_id == id))
                    {
                        return Err(invalid("Active chat does not belong to its pane"));
                    }
                }
                ChatWorkbenchNode::Split {
                    ratio,
                    first,
                    second,
                    ..
                } => {
                    if !ratio.is_finite() || !(0.1..=0.9).contains(ratio) {
                        return Err(invalid("Invalid split ratio"));
                    }
                    visit(first, depth + 1, nodes, chats, panes)?;
                    visit(second, depth + 1, nodes, chats, panes)?;
                }
            }
            Ok(())
        }
        visit(&self.root, 0, &mut nodes, &mut chats, &mut panes)?;
        if !chats.contains(self.main_chat_id.as_str()) {
            return Err(protected());
        }
        if self
            .root
            .tabs()
            .iter()
            .any(|tab| tab.chat_id == self.main_chat_id && tab.side_parent_id.is_some())
        {
            return Err(invalid(
                "The Main tab stores its provenance on the chat row",
            ));
        }
        if self.pane(&self.focused_pane_id).is_none() {
            return Err(invalid("Focused pane is missing"));
        }
        if self.closed_tabs.len() > WORKBENCH_CLOSED_LIMIT {
            return Err(invalid("Closed-tab history is too large"));
        }
        for closed in &self.closed_tabs {
            closed.tab.validate()?;
            valid_id(&closed.pane_id)?;
            if closed.index > WORKBENCH_MAX_TABS || !chats.insert(&closed.tab.chat_id) {
                return Err(invalid(
                    "Closed-tab history duplicates an open or closed chat",
                ));
            }
        }
        Ok(())
    }

    /// Apply to a clone, validate, then commit: a failed drag cannot lose a tab.
    /// This does not advance revision; persistence owns the revision fence.
    pub fn apply(&mut self, command: &ChatWorkbenchCommand) -> Result<bool, ChatWorkbenchError> {
        self.validate()?;
        let mut next = self.clone();
        next.apply_inner(command)?;
        next.validate()?;
        let changed = next != *self;
        if changed {
            *self = next;
        }
        Ok(changed)
    }

    fn pane_mut(&mut self, id: &str) -> Result<&mut ChatWorkbenchNode, ChatWorkbenchError> {
        self.root
            .node_mut(id)
            .filter(|node| matches!(node, ChatWorkbenchNode::Pane { .. }))
            .ok_or_else(|| missing("Pane is no longer available"))
    }

    fn focus(&mut self, chat_id: &str) -> Result<(), ChatWorkbenchError> {
        let pane_id = self
            .pane_for_chat(chat_id)
            .ok_or_else(|| missing("Chat tab is no longer open"))?
            .id()
            .to_string();
        if let ChatWorkbenchNode::Pane { active_chat_id, .. } = self.pane_mut(&pane_id)? {
            *active_chat_id = Some(chat_id.into());
        }
        self.focused_pane_id = pane_id;
        Ok(())
    }

    fn take_tab(
        &mut self,
        pane_id: &str,
        chat_id: &str,
    ) -> Result<ClosedWorkbenchTab, ChatWorkbenchError> {
        let ChatWorkbenchNode::Pane {
            tabs,
            active_chat_id,
            ..
        } = self.pane_mut(pane_id)?
        else {
            unreachable!()
        };
        let index = tabs
            .iter()
            .position(|tab| tab.chat_id == chat_id)
            .ok_or_else(|| missing("Chat tab is no longer open"))?;
        let tab = tabs.remove(index);
        if active_chat_id.as_deref() == Some(chat_id) {
            *active_chat_id = tabs
                .get(index.min(tabs.len().saturating_sub(1)))
                .map(|tab| tab.chat_id.clone());
        }
        Ok(ClosedWorkbenchTab {
            tab,
            pane_id: pane_id.into(),
            index,
        })
    }

    fn collapse_empty(&mut self, pane_id: &str) {
        if self.pane(pane_id).is_some_and(
            |pane| matches!(pane, ChatWorkbenchNode::Pane { tabs, .. } if tabs.is_empty()),
        ) {
            let neighbor = self.root.neighbor(pane_id).map(str::to_string);
            if let Some(root) = self.root.clone().without_pane(pane_id) {
                self.root = root;
                if self.focused_pane_id == pane_id {
                    self.focused_pane_id = neighbor
                        .filter(|id| self.pane(id).is_some())
                        .unwrap_or_else(|| self.root.first_pane().id().to_string());
                }
            }
        }
    }

    fn remember(&mut self, tab: ClosedWorkbenchTab) {
        self.closed_tabs
            .retain(|entry| entry.tab.chat_id != tab.tab.chat_id);
        self.closed_tabs.insert(0, tab);
        self.closed_tabs.truncate(WORKBENCH_CLOSED_LIMIT);
    }

    fn close(&mut self, chat_id: &str) -> Result<(), ChatWorkbenchError> {
        if chat_id == self.main_chat_id {
            return Err(protected());
        }
        let pane_id = self
            .pane_for_chat(chat_id)
            .ok_or_else(|| missing("Chat tab is no longer open"))?
            .id()
            .to_string();
        let closed = self.take_tab(&pane_id, chat_id)?;
        self.remember(closed);
        self.collapse_empty(&pane_id);
        Ok(())
    }

    fn split(
        &mut self,
        node_id: &str,
        edge: WorkbenchEdge,
        new_pane_id: &str,
        split_id: &str,
        tab: Option<ChatWorkbenchTab>,
    ) -> Result<(), ChatWorkbenchError> {
        valid_id(new_pane_id)?;
        valid_id(split_id)?;
        if new_pane_id == split_id
            || self.root.node(new_pane_id).is_some()
            || self.root.node(split_id).is_some()
        {
            return Err(invalid("New split and pane IDs must be unique"));
        }
        let node = self
            .root
            .node_mut(node_id)
            .ok_or_else(|| missing("Split destination is no longer available"))?;
        let new = ChatWorkbenchNode::Pane {
            id: new_pane_id.into(),
            active_chat_id: tab.as_ref().map(|tab| tab.chat_id.clone()),
            tabs: tab.into_iter().collect(),
        };
        let old = node.clone();
        let (first, second) = if edge.before() {
            (new, old)
        } else {
            (old, new)
        };
        *node = ChatWorkbenchNode::Split {
            id: split_id.into(),
            axis: edge.axis(),
            ratio: 0.5,
            first: Box::new(first),
            second: Box::new(second),
        };
        self.focused_pane_id = new_pane_id.into();
        Ok(())
    }

    fn insert(
        &mut self,
        tab: ChatWorkbenchTab,
        destination: &WorkbenchDestination,
    ) -> Result<(), ChatWorkbenchError> {
        match destination {
            WorkbenchDestination::Pane { pane_id, index } => {
                let ChatWorkbenchNode::Pane {
                    tabs,
                    active_chat_id,
                    ..
                } = self.pane_mut(pane_id)?
                else {
                    unreachable!()
                };
                let index = index.unwrap_or(tabs.len());
                if index > tabs.len() {
                    return Err(invalid("Tab insertion index is outside the group"));
                }
                *active_chat_id = Some(tab.chat_id.clone());
                tabs.insert(index, tab);
                self.focused_pane_id = pane_id.clone();
                Ok(())
            }
            WorkbenchDestination::Split {
                node_id,
                edge,
                new_pane_id,
                split_id,
            } => self.split(node_id, *edge, new_pane_id, split_id, Some(tab)),
        }
    }

    fn apply_inner(&mut self, command: &ChatWorkbenchCommand) -> Result<(), ChatWorkbenchError> {
        match command {
            ChatWorkbenchCommand::OpenTab { tab, destination } => {
                tab.validate()?;
                if self.contains_chat(&tab.chat_id) {
                    let existing = self
                        .root
                        .tabs()
                        .into_iter()
                        .find(|item| item.chat_id == tab.chat_id)
                        .expect("found tab");
                    if existing != tab {
                        return Err(invalid("A tab's source identity cannot change"));
                    }
                    return self.focus(&tab.chat_id);
                }
                let destination =
                    destination
                        .clone()
                        .unwrap_or_else(|| WorkbenchDestination::Pane {
                            pane_id: self.focused_pane_id.clone(),
                            index: None,
                        });
                self.insert(tab.clone(), &destination)?;
                self.closed_tabs
                    .retain(|closed| closed.tab.chat_id != tab.chat_id);
                Ok(())
            }
            ChatWorkbenchCommand::FocusTab { chat_id } => self.focus(chat_id),
            ChatWorkbenchCommand::FocusPane { pane_id } => {
                valid_id(pane_id)?;
                if self.pane(pane_id).is_none() {
                    return Err(missing("Focused pane is no longer available"));
                }
                self.focused_pane_id = pane_id.clone();
                Ok(())
            }
            ChatWorkbenchCommand::MoveTab {
                chat_id,
                destination,
            } => {
                let source = self
                    .pane_for_chat(chat_id)
                    .ok_or_else(|| missing("Dragged chat is no longer open"))?;
                let ChatWorkbenchNode::Pane {
                    id: source_id,
                    tabs,
                    ..
                } = source
                else {
                    unreachable!()
                };
                if matches!(destination, WorkbenchDestination::Split { node_id, .. } if node_id == source_id && tabs.len() == 1)
                {
                    return Ok(());
                }
                let source_id = source_id.clone();
                let tab = tabs
                    .iter()
                    .find(|tab| &tab.chat_id == chat_id)
                    .expect("found tab")
                    .clone();
                if matches!(destination, WorkbenchDestination::Split { .. }) {
                    // Create the destination before collapsing the source, so an
                    // ancestor destination remains addressable throughout the move.
                    self.insert(tab, destination)?;
                    self.take_tab(&source_id, chat_id)?;
                } else {
                    self.take_tab(&source_id, chat_id)?;
                    self.insert(tab, destination)?;
                }
                self.collapse_empty(&source_id);
                Ok(())
            }
            ChatWorkbenchCommand::CloseTab { chat_id } => self.close(chat_id),
            ChatWorkbenchCommand::CloseOtherTabs {
                pane_id,
                keep_chat_id,
            } => {
                let Some(ChatWorkbenchNode::Pane { tabs, .. }) = self.pane(pane_id) else {
                    return Err(missing("Pane is no longer available"));
                };
                if !tabs.iter().any(|tab| &tab.chat_id == keep_chat_id) {
                    return Err(missing("Kept tab is not in this pane"));
                }
                let close = tabs
                    .iter()
                    .filter(|tab| &tab.chat_id != keep_chat_id && tab.chat_id != self.main_chat_id)
                    .map(|tab| tab.chat_id.clone())
                    .collect::<Vec<_>>();
                for id in close {
                    self.close(&id)?;
                }
                self.focus(keep_chat_id)
            }
            ChatWorkbenchCommand::ClosePane { pane_id } => {
                let Some(ChatWorkbenchNode::Pane { tabs, .. }) = self.pane(pane_id) else {
                    return Err(missing("Pane is no longer available"));
                };
                if tabs.iter().any(|tab| tab.chat_id == self.main_chat_id) {
                    return Err(protected());
                }
                let ids = tabs
                    .iter()
                    .map(|tab| tab.chat_id.clone())
                    .collect::<Vec<_>>();
                for id in ids {
                    self.close(&id)?;
                }
                self.collapse_empty(pane_id);
                Ok(())
            }
            ChatWorkbenchCommand::SplitPane {
                node_id,
                edge,
                new_pane_id,
                split_id,
            } => self.split(node_id, *edge, new_pane_id, split_id, None),
            ChatWorkbenchCommand::ResizeSplit { split_id, ratio } => {
                if !ratio.is_finite() || !(0.1..=0.9).contains(ratio) {
                    return Err(invalid(
                        "Split ratio must be finite and between 0.1 and 0.9",
                    ));
                }
                let Some(ChatWorkbenchNode::Split { ratio: saved, .. }) =
                    self.root.node_mut(split_id)
                else {
                    return Err(missing("Split divider is no longer available"));
                };
                *saved = *ratio;
                Ok(())
            }
            ChatWorkbenchCommand::BalancePanes => {
                fn balance(node: &mut ChatWorkbenchNode) -> usize {
                    match node {
                        ChatWorkbenchNode::Pane { .. } => 1,
                        ChatWorkbenchNode::Split {
                            ratio,
                            first,
                            second,
                            ..
                        } => {
                            let first_count = balance(first);
                            let total = first_count + balance(second);
                            *ratio = (first_count as f64 / total as f64).clamp(0.1, 0.9);
                            total
                        }
                    }
                }
                balance(&mut self.root);
                Ok(())
            }
            ChatWorkbenchCommand::ReopenClosed { chat_id } => {
                let index = match chat_id {
                    Some(id) => self
                        .closed_tabs
                        .iter()
                        .position(|entry| &entry.tab.chat_id == id)
                        .ok_or_else(|| missing("Closed tab is no longer in recent history"))?,
                    None if self.closed_tabs.is_empty() => return Ok(()),
                    None => 0,
                };
                let closed = self.closed_tabs.remove(index);
                let pane_id = if self.pane(&closed.pane_id).is_some() {
                    closed.pane_id
                } else {
                    self.focused_pane_id.clone()
                };
                let Some(ChatWorkbenchNode::Pane { tabs, .. }) = self.pane(&pane_id) else {
                    return Err(missing("Reopen destination is no longer available"));
                };
                let index = closed.index.min(tabs.len());
                self.insert(
                    closed.tab,
                    &WorkbenchDestination::Pane {
                        pane_id,
                        index: Some(index),
                    },
                )
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GetChatWorkbenchRequest {
    pub main_chat_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MutateChatWorkbenchRequest {
    pub main_chat_id: String,
    pub profile_id: String,
    pub expected_revision: u64,
    pub command_id: String,
    pub command: ChatWorkbenchCommand,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatWorkbenchMutationResult {
    pub snapshot: ChatWorkbenchSnapshot,
    pub applied: bool,
    pub conflict: bool,
}

/// User-created blank child in the parent's current checkout. This intentionally
/// has no prompt, worktree, account override, access grant or outbox fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateChildTabRequest {
    pub parent_task_id: String,
    pub idempotency_key: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub provider: Option<crate::HarnessId>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub reasoning: Option<crate::ReasoningLevel>,
    #[serde(default)]
    pub model_options: serde_json::Map<String, serde_json::Value>,
}
