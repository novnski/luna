//! Chat placement across the two sidebars: where a chat is *listed*, never
//! what it is.
//!
//! - A **promoted** child chat lists in the left sidebar as its own row (in
//!   its pin and sort scope) instead of in its parent's Related chats. It is
//!   still that parent's child: same lineage, owner and agent control, and
//!   every left-sidebar child wears a Child badge.
//! - A **docked** ordinary chat lists in another chat's Related chats as
//!   "Alongside" instead of the left sidebar. It gains no parent.
//!
//! Placements persist per engine profile ([`ChatPlacements`] in settings),
//! keyed by the engine's opaque `profileId`. Account profiles share one
//! device and scope, so nothing else separates them: without a profile id
//! (older engines, pre-profile bootstrap) placement is off, never guessed.
//! Entries that no longer apply (a deleted chat, a missing or archived host,
//! a chat that is not a child) are ignored, and pruned on the next change.
//! A side chat (a fork) is an ordinary root row placed like any chat; its
//! source is only a link.

use super::*;
use crate::settings::ChatPlacements;

/// The placements in force for the current engine profile.
#[derive(Debug, Default, Clone)]
pub(super) struct EffectivePlacements {
    pub(super) promoted: std::collections::HashSet<String>,
    /// Chat id → host chat id.
    pub(super) docked: std::collections::HashMap<String, String>,
}

impl EffectivePlacements {
    pub(super) fn is_promoted(&self, id: &str) -> bool {
        self.promoted.contains(id)
    }

    pub(super) fn host(&self, id: &str) -> Option<&str> {
        self.docked.get(id).map(String::as_str)
    }
}

/// The child's actual parent, from its lineage alone.
pub(super) fn lineage_parent_id(chat: &luna_proto::Chat) -> Option<&str> {
    chat.child_thread
        .as_ref()
        .map(|child| child.parent_task_id.as_str())
        .filter(|parent| *parent != chat.id)
}

/// Chats by id: built once per pass so lookups stay proportional to chats
/// plus placements, never a scan per row or per step.
pub(super) type ChatIndex<'a> = std::collections::HashMap<&'a str, &'a luna_proto::Chat>;

pub(super) fn chat_index(chats: &[luna_proto::Chat]) -> ChatIndex<'_> {
    chats.iter().map(|chat| (chat.id.as_str(), chat)).collect()
}

/// The display title of `id`'s actual parent (record lineage), when `id` is
/// a child: "an unavailable chat" once that parent is gone.
pub(super) fn parent_title(index: &ChatIndex<'_>, id: &str) -> Option<String> {
    let parent = lineage_parent_id(index.get(id)?)?;
    Some(
        index
            .get(parent)
            .and_then(|chat| chat.title.as_deref())
            .map(transcript::single_line)
            .unwrap_or_else(|| "an unavailable chat".into()),
    )
}

/// Where a chat is listed when not in the left sidebar's roots: its dock
/// host, or the parent its sidebar row folds into (a child that is neither
/// listed in the sidebar nor pinned). `None` lists it as a sidebar root.
fn listed_under<'a>(
    placed: &'a EffectivePlacements,
    chat: &'a luna_proto::Chat,
) -> Option<&'a str> {
    if let Some(host) = placed.host(&chat.id) {
        return Some(host);
    }
    let parent = lineage_parent_id(chat)?;
    (!placed.is_promoted(&chat.id) && chat.pinned_order.is_none()).then_some(parent)
}

/// Whether following "listed under" from `start` (docks and lineage alike,
/// in any mix, however long) reaches `target`, or loops. Listing `target`
/// under `start` would then close a cycle, or hang it off one: every chat in
/// it would leave the sidebar for good. The visited set bounds the walk.
fn listing_reaches(
    placed: &EffectivePlacements,
    index: &ChatIndex<'_>,
    start: &str,
    target: &str,
) -> bool {
    let mut cursor = start.to_string();
    let mut seen = std::collections::HashSet::new();
    loop {
        if cursor == target {
            return true;
        }
        if !seen.insert(cursor.clone()) {
            // The walk loops (malformed lineage) without ever reaching a
            // sidebar root: listing `target` there could not be undone from
            // the sidebar either. Fail closed, as for a cycle.
            return true;
        }
        let Some(chat) = index.get(cursor.as_str()) else {
            return false;
        };
        match listed_under(placed, chat) {
            Some(next) => cursor = next.to_string(),
            None => return false,
        }
    }
}

/// The saved placements that apply: promotions of actual children; docks of
/// ordinary chats under an existing, unarchived host. Docks are taken in a
/// fixed (id) order and one that would close a cycle with those before it is
/// skipped, so a saved cycle (from an older build or another device's edits)
/// never hides its chats; [`Shell::prune_placements`] then drops it.
fn effective(saved: &ChatPlacements, index: &ChatIndex<'_>) -> EffectivePlacements {
    let find = |id: &str| index.get(id).copied();
    let mut placed = EffectivePlacements {
        promoted: saved
            .promoted
            .iter()
            .filter(|id| find(id.as_str()).is_some_and(|chat| lineage_parent_id(chat).is_some()))
            .cloned()
            .collect(),
        docked: Default::default(),
    };
    for (id, host) in &saved.docked {
        let qualifies = id != host
            && find(id.as_str()).is_some_and(|chat| lineage_parent_id(chat).is_none())
            && find(host.as_str()).is_some_and(|host| !host.archived);
        if qualifies && !listing_reaches(&placed, index, host, id) {
            placed.docked.insert(id.clone(), host.clone());
        }
    }
    placed
}

impl Shell {
    /// This engine profile's placement key: placements made against one
    /// profile's chats never apply to (or get pruned by) another's. `None`
    /// turns placement off: no saved placement applies, persists or prunes.
    pub(super) fn placement_scope(&self, cx: &App) -> Option<String> {
        self.state
            .read(cx)
            .engine()?
            .engine_info()
            .profile_id
            .clone()
            .filter(|id| !id.is_empty())
    }

    /// Moves are refused, not lost, while the engine names no profile.
    fn placement_unavailable(&mut self, cx: &mut Context<Self>) -> bool {
        if self.placement_scope(cx).is_some() {
            return false;
        }
        self.sidebar_notice = Some(SidebarNotice::transient_status(
            "Moving chats between the sidebars needs an engine that identifies its profile. Update the engine, then try again.",
        ));
        cx.notify();
        true
    }

    pub(super) fn effective_placements(&self, cx: &App) -> EffectivePlacements {
        let Some(scope) = self.placement_scope(cx) else {
            return EffectivePlacements::default();
        };
        let Some(saved) = self.settings.chat_placements.get(&scope) else {
            return EffectivePlacements::default();
        };
        effective(saved, &chat_index(&self.state.read(cx).chats))
    }

    /// Change this profile's placements, pruning entries for chats that no
    /// longer exist, then persist.
    fn update_placements(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut ChatPlacements)) {
        let Some(scope) = self.placement_scope(cx) else {
            return;
        };
        let known: std::collections::HashSet<String> = self
            .state
            .read(cx)
            .chats
            .iter()
            .map(|chat| chat.id.clone())
            .collect();
        let placements = self
            .settings
            .chat_placements
            .entry(scope.clone())
            .or_default();
        f(placements);
        placements.promoted.retain(|id| known.contains(id));
        placements.promoted.dedup();
        placements.docked.retain(|id, host| {
            known.contains(id) && known.contains(host.as_str()) && id.as_str() != host.as_str()
        });
        if placements.is_empty() {
            self.settings.chat_placements.remove(&scope);
        }
        self.schedule_save(cx);
        cx.notify();
    }

    /// Drop saved placements naming chats this profile no longer has, once
    /// its chat list has synced (a deleted chat never comes back by id).
    pub(super) fn prune_placements(&mut self, cx: &mut Context<Self>) {
        let Some(scope) = self.placement_scope(cx) else {
            return;
        };
        let stale = {
            let state = self.state.read(cx);
            if !state.chats_synced {
                return;
            }
            let Some(saved) = self.settings.chat_placements.get(&scope) else {
                return;
            };
            let index = chat_index(&state.chats);
            let known = |id: &str| index.contains_key(id);
            let applied = effective(saved, &index);
            // A dock that would close a cycle is skipped on load; drop it.
            let cyclic: Vec<String> = saved
                .docked
                .iter()
                .filter(|(id, host)| {
                    applied.host(id.as_str()).is_none()
                        && index.get(host.as_str()).is_some_and(|host| !host.archived)
                        && index
                            .get(id.as_str())
                            .is_some_and(|chat| lineage_parent_id(chat).is_none())
                })
                .map(|(id, _)| id.clone())
                .collect();
            let unknown = saved.promoted.iter().any(|id| !known(id.as_str()))
                || saved
                    .docked
                    .iter()
                    .any(|(id, host)| !known(id.as_str()) || !known(host.as_str()));
            (unknown || !cyclic.is_empty()).then_some(cyclic)
        };
        if let Some(cyclic) = stale {
            self.update_placements(cx, |placements| {
                for id in &cyclic {
                    placements.docked.remove(id);
                }
            });
        }
    }

    fn chat_row(&self, id: &str, cx: &App) -> Option<luna_proto::Chat> {
        self.state
            .read(cx)
            .chats
            .iter()
            .find(|chat| chat.id == id)
            .cloned()
    }

    fn chat_title(&self, id: &str, cx: &App) -> String {
        self.chat_row(id, cx)
            .and_then(|chat| chat.title)
            .map(|title| transcript::single_line(&title))
            .filter(|title| !title.trim().is_empty())
            .unwrap_or_else(|| "this chat".into())
    }

    /// A related row dropped on the left sidebar: a child is listed there as
    /// its own row (still its parent's child); an alongside chat returns. A
    /// fork is an ordinary sidebar row already: the drop only places it
    /// (its source link stays). A drop slot places it under Manual sort,
    /// exactly like dragging a sidebar row there.
    pub(super) fn place_in_sidebar(
        &mut self,
        drag: &SidebarChatDrag,
        kind: related_chats::RelatedKind,
        target: Option<(String, SidebarDropPlace)>,
        cx: &mut Context<Self>,
    ) {
        let id = drag.chat_id.clone();
        match kind {
            _ if self.placement_unavailable(cx) => return,
            related_chats::RelatedKind::Child => {
                let Some(parent) = self
                    .chat_row(&id, cx)
                    .and_then(|chat| lineage_parent_id(&chat).map(str::to_string))
                else {
                    return;
                };
                self.update_placements(cx, |placements| {
                    placements.docked.remove(&id);
                    if !placements.promoted.contains(&id) {
                        placements.promoted.push(id.clone());
                    }
                });
                let parent_title = self.chat_title(&parent, cx);
                self.sidebar_notice = Some(SidebarNotice::transient_status(format!(
                    "Listed in the sidebar. It is still a new thread from {parent_title}."
                )));
            }
            related_chats::RelatedKind::Alongside | related_chats::RelatedKind::Side => {
                self.update_placements(cx, |placements| {
                    placements.docked.remove(&id);
                });
            }
        }
        // It left the pane's list: close it there if it was open.
        if let Some(panel) = self.child_chat_panel.clone()
            && panel.read(cx).open_chat_id() == Some(id.as_str())
        {
            panel.update(cx, |panel, cx| panel.back(cx));
        }
        if let Some((anchor, place)) = target.filter(|(anchor, _)| *anchor != id) {
            match drag.scope {
                SidebarDragScope::Pinned => self.move_pinned_chat(id, &anchor, place, cx),
                SidebarDragScope::Unpinned(_) => self.move_unpinned_chat(id, &anchor, place, cx),
            }
        }
        cx.notify();
    }

    /// A sidebar row dropped on the selected chat's Related chats: an
    /// ordinary chat is listed there alongside it; its own child that was
    /// listed in the sidebar returns. Lineage never changes, so a chat cannot
    /// sit beside itself, beside its own descendant, or (as another parent's
    /// child) beside a chat that is not its parent.
    pub(super) fn dock_beside_active(&mut self, drag: &SidebarChatDrag, cx: &mut Context<Self>) {
        let host = self.active_chat.clone();
        let id = drag.chat_id.clone();
        if host.is_empty() {
            return;
        }
        let refuse = |shell: &mut Self, message: String, cx: &mut Context<Self>| {
            shell.sidebar_notice = Some(SidebarNotice::transient_status(message));
            cx.notify();
        };
        if id == host {
            return refuse(self, "That is this chat.".into(), cx);
        }
        if self.placement_unavailable(cx) {
            return;
        }
        match self.chat_row(&host, cx) {
            None => {
                return refuse(
                    self,
                    "This chat is no longer available, so nothing can be placed beside it.".into(),
                    cx,
                );
            }
            Some(host_chat) if host_chat.archived => {
                return refuse(
                    self,
                    "This chat is archived. Restore it to place chats beside it.".into(),
                    cx,
                );
            }
            Some(_) => {}
        }
        let Some(chat) = self.chat_row(&id, cx) else {
            return;
        };
        if let Some(parent) = lineage_parent_id(&chat).map(str::to_string) {
            if parent == host {
                self.update_placements(cx, |placements| {
                    placements.promoted.retain(|promoted| *promoted != id);
                });
                // Pins always list in the sidebar's Pinned group as well.
                if chat.pinned_order.is_some() {
                    return refuse(
                        self,
                        "It is pinned, so it also stays in the sidebar's Pinned list. Unpin it to list it only here."
                            .into(),
                        cx,
                    );
                }
                return;
            }
            let parent_title = self.chat_title(&parent, cx);
            return refuse(
                self,
                format!(
                    "New threads stay with their parent, {parent_title}. Drop it on that chat's Related chats, or keep it in the sidebar."
                ),
                cx,
            );
        }
        // Placement is local presentation; a chat hosted on another device
        // stays in the sidebar rather than opening in this chat's pane.
        if self
            .chat_row(&host, cx)
            .is_some_and(|host| host.device_id != chat.device_id)
        {
            return refuse(
                self,
                "Chats from another device stay in the sidebar.".into(),
                cx,
            );
        }
        // Listing it under this chat must not close a cycle through docks
        // and parents (A beside B beside A, or longer and mixed chains): the
        // whole cycle would leave the sidebar with no way back.
        let placed = self.effective_placements(cx);
        let (cycle, host_lineage) = {
            let index = chat_index(&self.state.read(cx).chats);
            let cycle = listing_reaches(&placed, &index, &host, &id);
            // Name a real ancestor as such; otherwise it is a placement loop.
            let mut ancestor = false;
            let mut cursor = index.get(host.as_str()).copied();
            let mut depth = 0;
            while cycle && let Some(parent) = cursor.and_then(lineage_parent_id) {
                if parent == id {
                    ancestor = true;
                    break;
                }
                depth += 1;
                if depth > 32 {
                    break;
                }
                cursor = index.get(parent).copied();
            }
            (cycle, ancestor)
        };
        if cycle {
            let title = self.chat_title(&id, cx);
            return refuse(
                self,
                if host_lineage {
                    format!("{title} is this chat's parent; it stays in the sidebar.")
                } else {
                    format!(
                        "This chat is already listed inside {title}. Placing {title} here would hide both."
                    )
                },
                cx,
            );
        }
        self.update_placements(cx, |placements| {
            placements.promoted.retain(|promoted| *promoted != id);
            placements.docked.insert(id.clone(), host.clone());
        });
        let host_title = self.chat_title(&host, cx);
        self.sidebar_notice = Some(SidebarNotice::transient_status(format!(
            "Listed alongside {host_title}. Drag it back to the sidebar anytime."
        )));
        cx.notify();
    }
}
