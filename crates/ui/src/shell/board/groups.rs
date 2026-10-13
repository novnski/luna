//! PB-4A feature groups on the Board page
//! (docs/pending-tasks/devin-project-board/pb-4a/README.md).
//!
//! The panel lists the feature groups that concern the page's scope (homed
//! in the project, or containing one of its tickets) above the columns. Each
//! group shows its members as their own owners report them (project, column,
//! phase, branch, source, review, blockers), its integration gates and why it
//! cannot be accepted yet. Accept, Reopen, gate results and Remove are one
//! `BoardFeatureGroupMutate` each; the card menu creates a group from a ticket
//! or links a ticket (see `BoardPage::card_menu_entries`). Nothing here moves,
//! starts or reviews a ticket: the engine derives every state shown.
use std::time::Duration;

use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, IntoElement, ParentElement, Render,
    SharedString, Styled, Subscription, Task, Window, div, prelude::*,
};
use luna_proto::{
    BoardFeatureGroupMutateRequest, BoardFeatureGroupsRequest, BoardScope, BoardSourceKind,
    FeatureGroupAcceptanceState, FeatureGroupAction, FeatureGroupGateState, FeatureGroupGateStatus,
    FeatureGroupGateView, FeatureGroupMemberAvailability, FeatureGroupMemberView,
    FeatureGroupMutation, FeatureGroupView,
};

use super::api::BoardCallError;
use super::model::{BoardModel, BoardModelEvent};
use crate::button::{ButtonKind, ControlSize};
use crate::settings::widgets;
use crate::theme::Theme;
use crate::typography::ui_rems;

/// While a group is shown it is re-read this often, so a related
/// repository's change (outside the page's watch scope) still appears.
const POLL: Duration = Duration::from_secs(4);
/// The gate a group created from a ticket starts with.
pub(super) const DEFAULT_GATE: &str = "Integration check";

pub struct FeatureGroupsPanel {
    model: Entity<BoardModel>,
    /// Every group on this host (the card menu links into any of them); the
    /// panel shows those of the page's scope.
    groups: Vec<FeatureGroupView>,
    /// The last refused or lost mutation (kept until the next one).
    error: Option<String>,
    /// The last failed read (cleared by the next successful one).
    load_error: Option<String>,
    /// The host serves feature groups (an older engine does not).
    supported: bool,
    /// Mutations in flight.
    pending: usize,
    /// Mutation kinds this panel issued, in order (QA readback).
    issued: Vec<String>,
    /// A change arrived while a read was in flight: read again after it.
    dirty: bool,
    load_task: Option<Task<()>>,
    _poll: Task<()>,
    #[cfg(feature = "appshots-fixture")]
    probe: std::rc::Rc<
        std::cell::RefCell<std::collections::HashMap<String, gpui::Bounds<gpui::Pixels>>>,
    >,
    _model: Subscription,
}

fn source_label(source: BoardSourceKind) -> &'static str {
    match source {
        BoardSourceKind::Local => "Local",
        BoardSourceKind::Chat => "From a chat",
        BoardSourceKind::Authoring => "Discussed with AI",
        BoardSourceKind::GitHub => "GitHub",
    }
}

/// One member's line, from the engine's view only.
fn member_line(member: &FeatureGroupMemberView) -> String {
    let mut parts = vec![
        member
            .project_name
            .clone()
            .unwrap_or_else(|| member.project_id.clone()),
    ];
    match member.availability {
        FeatureGroupMemberAvailability::Deleted => parts.push("Deleted".into()),
        FeatureGroupMemberAvailability::Missing => parts.push("Missing".into()),
        FeatureGroupMemberAvailability::OtherHost => parts.push("On another host".into()),
        FeatureGroupMemberAvailability::Archived => parts.push("Archived".into()),
        FeatureGroupMemberAvailability::Available => {}
    }
    if let (Some(column), Some(phase)) = (member.column, member.phase) {
        parts.push(format!("{} · {}", column.label(), phase.label()));
    }
    if let Some(checkout) = &member.checkout
        && let Some(branch) = checkout
            .live_branch
            .as_ref()
            .or(checkout.recorded_branch.as_ref())
    {
        parts.push(format!("branch {branch}"));
    }
    if let Some(source) = member.source {
        parts.push(match &member.issue {
            Some(issue) => format!("{} {}", source_label(source), issue.display),
            None => source_label(source).to_owned(),
        });
    }
    if let Some(review) = &member.review {
        let verdict = review
            .verdict
            .map(|verdict| format!(" ({verdict:?})"))
            .unwrap_or_default();
        let current = if review.current { "" } else { ", earlier run" };
        parts.push(format!("review {:?}{verdict}{current}", review.state));
    }
    if !member.blocked_by.is_empty() {
        let keys: Vec<&str> = member
            .blocked_by
            .iter()
            .map(|blocker| blocker.key.as_str())
            .collect();
        parts.push(format!("blocked by {}", keys.join(", ")));
    }
    parts.join(" · ")
}

fn gate_line(gate: &FeatureGroupGateView) -> String {
    let mut line = gate.status.label().to_owned();
    if let Some(note) = gate
        .gate
        .result
        .as_ref()
        .and_then(|result| result.note.as_ref())
    {
        line.push_str(" · ");
        line.push_str(note);
    }
    line
}

impl FeatureGroupsPanel {
    pub fn new(model: Entity<BoardModel>, cx: &mut Context<Self>) -> Self {
        // Every Board change (journal, observation, scope) may change a
        // member's state: re-read the groups after the cards.
        let subscription = cx.subscribe(
            &model,
            |panel: &mut Self, _, event: &BoardModelEvent, cx| {
                if matches!(event, BoardModelEvent::Changed) {
                    panel.refresh(cx);
                }
            },
        );
        let poll = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(POLL).await;
                let alive = this
                    .update(cx, |panel, cx| {
                        if !panel.groups.is_empty() {
                            panel.refresh(cx);
                        }
                    })
                    .is_ok();
                if !alive {
                    break;
                }
            }
        });
        let mut panel = Self {
            model,
            groups: Vec::new(),
            error: None,
            load_error: None,
            supported: true,
            pending: 0,
            issued: Vec::new(),
            dirty: false,
            load_task: None,
            _poll: poll,
            #[cfg(feature = "appshots-fixture")]
            probe: Default::default(),
            _model: subscription,
        };
        panel.refresh(cx);
        panel
    }

    /// Every group on this host.
    pub fn groups(&self) -> &[FeatureGroupView] {
        &self.groups
    }

    pub fn supported(&self) -> bool {
        self.supported
    }

    fn scope_project(&self, cx: &App) -> Option<String> {
        self.model.read(cx).scope().project_id().map(str::to_owned)
    }

    /// The groups that concern a scope: homed there or containing one of
    /// its tickets (every group under All projects).
    fn visible(&self, scope_project: Option<&str>) -> Vec<FeatureGroupView> {
        self.groups
            .iter()
            .filter(|view| match scope_project {
                None => true,
                Some(project) => {
                    view.group.project_id == project
                        || view
                            .members
                            .iter()
                            .any(|member| member.project_id == project)
                }
            })
            .cloned()
            .collect()
    }

    /// Whether the page should mount the panel at all (nothing to show
    /// adds no gap to the page).
    pub fn shows_anything(&self, cx: &App) -> bool {
        self.error.is_some()
            || self.load_error.is_some()
            || !self.visible(self.scope_project(cx).as_deref()).is_empty()
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if !self.supported {
            return;
        }
        if self.load_task.is_some() {
            self.dirty = true;
            return;
        }
        let Some(api) = self.model.read(cx).api(cx) else {
            return;
        };
        let request = BoardFeatureGroupsRequest {
            scope: BoardScope::AllProjects,
            group_id: None,
            work_item_id: None,
            include_archived: false,
        };
        self.load_task = Some(cx.spawn(async move |this, cx| {
            let result = api.feature_groups(request).await;
            this.update(cx, |panel, cx| {
                panel.load_task = None;
                let before = (
                    panel.groups.len(),
                    panel.load_error.clone(),
                    panel.supported,
                );
                let mut changed = false;
                match result {
                    Ok(page) => {
                        changed = panel.groups != page.groups;
                        panel.groups = page.groups;
                        panel.load_error = None;
                    }
                    Err(BoardCallError::Unsupported) => {
                        panel.supported = false;
                        panel.groups.clear();
                    }
                    Err(error) => panel.load_error = Some(error.message()),
                }
                if std::mem::take(&mut panel.dirty) {
                    panel.refresh(cx);
                }
                // An unchanged read does not redraw the page.
                if changed
                    || before
                        != (
                            panel.groups.len(),
                            panel.load_error.clone(),
                            panel.supported,
                        )
                {
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    fn group(&self, group_id: &str) -> Option<&FeatureGroupView> {
        self.groups.iter().find(|view| view.group.id == group_id)
    }

    fn apply(&mut self, mutation: FeatureGroupMutation) {
        let id = mutation.receipt.group_id.clone();
        match mutation.group {
            Some(view) if !mutation.receipt.deleted => {
                match self
                    .groups
                    .iter_mut()
                    .find(|existing| existing.group.id == id)
                {
                    Some(existing) => *existing = view,
                    None => self.groups.insert(0, view),
                }
            }
            _ => self.groups.retain(|existing| existing.group.id != id),
        }
    }

    /// One engine mutation; the panel re-reads afterwards either way.
    fn mutate(&mut self, action: FeatureGroupAction, cx: &mut Context<Self>) {
        let Some(api) = self.model.read(cx).api(cx) else {
            return;
        };
        self.issued
            .push(serde_json::to_value(action.kind()).map_or_else(
                |_| "?".into(),
                |kind| kind.as_str().unwrap_or("?").to_owned(),
            ));
        let request = BoardFeatureGroupMutateRequest {
            request_id: super::page::request_id("group"),
            action,
        };
        self.pending += 1;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = api.feature_group_mutate(request).await;
            this.update(cx, |panel, cx| {
                panel.pending = panel.pending.saturating_sub(1);
                match result {
                    Ok(mutation) => {
                        panel.apply(mutation);
                        panel.error = None;
                    }
                    Err(BoardCallError::Lost) => {
                        panel.error =
                            Some("The answer was lost. Reloading the feature groups…".into())
                    }
                    Err(error) => panel.error = Some(error.message()),
                }
                panel.refresh(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Card menu: a new group in the ticket's project, named after it, with
    /// the ticket as its first member and one integration gate.
    pub fn create_with_ticket(
        &mut self,
        project_id: String,
        title: String,
        work_item_id: String,
        cx: &mut Context<Self>,
    ) {
        self.mutate(
            FeatureGroupAction::Create {
                project_id,
                title,
                acceptance: String::new(),
                gates: vec![DEFAULT_GATE.into()],
                members: vec![work_item_id],
            },
            cx,
        );
    }

    /// Card menu: link a ticket (of any project on this host) explicitly.
    pub fn add_member(&mut self, group_id: &str, work_item_id: &str, cx: &mut Context<Self>) {
        let Some(revision) = self.group(group_id).map(|view| view.group.revision) else {
            return;
        };
        self.mutate(
            FeatureGroupAction::AddMember {
                group_id: group_id.into(),
                expected_revision: revision,
                work_item_id: work_item_id.into(),
            },
            cx,
        );
    }

    pub fn remove_member(&mut self, group_id: &str, work_item_id: &str, cx: &mut Context<Self>) {
        let Some(revision) = self.group(group_id).map(|view| view.group.revision) else {
            return;
        };
        self.mutate(
            FeatureGroupAction::RemoveMember {
                group_id: group_id.into(),
                expected_revision: revision,
                work_item_id: work_item_id.into(),
            },
            cx,
        );
    }

    fn record_gate(
        &mut self,
        group_id: &str,
        gate_id: &str,
        state: Option<FeatureGroupGateState>,
        cx: &mut Context<Self>,
    ) {
        let Some(revision) = self.group(group_id).map(|view| view.group.revision) else {
            return;
        };
        self.mutate(
            FeatureGroupAction::RecordGate {
                group_id: group_id.into(),
                expected_revision: revision,
                gate_id: gate_id.into(),
                state,
                note: None,
                evidence: Vec::new(),
            },
            cx,
        );
    }

    fn accept(&mut self, group_id: &str, cx: &mut Context<Self>) {
        let Some(revision) = self.group(group_id).map(|view| view.group.revision) else {
            return;
        };
        self.mutate(
            FeatureGroupAction::Accept {
                group_id: group_id.into(),
                expected_revision: revision,
            },
            cx,
        );
    }

    fn reopen(&mut self, group_id: &str, cx: &mut Context<Self>) {
        let Some(revision) = self.group(group_id).map(|view| view.group.revision) else {
            return;
        };
        self.mutate(
            FeatureGroupAction::Reopen {
                group_id: group_id.into(),
                expected_revision: revision,
            },
            cx,
        );
    }

    #[cfg(feature = "appshots-fixture")]
    fn probed(&self, label: String, element: impl IntoElement) -> AnyElement {
        let probe = self.probe.clone();
        div()
            .child(element)
            .on_children_prepainted(move |children, _, _| {
                if let Some(bounds) = children.first() {
                    probe.borrow_mut().insert(label.clone(), *bounds);
                }
            })
            .into_any_element()
    }

    #[cfg(not(feature = "appshots-fixture"))]
    fn probed(&self, _label: String, element: impl IntoElement) -> AnyElement {
        element.into_any_element()
    }

    fn render_group(
        &self,
        index: usize,
        view: &FeatureGroupView,
        scope_project: Option<&str>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let gid = view.group.id.clone();
        let idle = self.pending == 0;
        let home = scope_project.is_none_or(|project| project == view.group.project_id);
        let home_name = view
            .project_name
            .clone()
            .unwrap_or_else(|| view.group.project_id.clone());
        let repositories = view.summary.repositories;
        let status = format!(
            "{} · {} of {} tickets Done · {} {}",
            view.acceptance.label(),
            view.summary.done,
            view.summary.members,
            repositories,
            if repositories == 1 {
                "repository"
            } else {
                "repositories"
            }
        );
        let description = if home {
            format!("Feature group in {home_name} · {status}")
        } else {
            format!("Feature group in {home_name} (related) · {status}")
        };
        let mut actions = Vec::new();
        if view.acceptance != FeatureGroupAcceptanceState::NotAccepted {
            let id = gid.clone();
            actions.push(
                self.probed(
                    format!("reopen-{gid}"),
                    crate::button::button(
                        SharedString::from(format!("board-fg-reopen-{index}")),
                        ButtonKind::Ghost,
                        ControlSize::Sm,
                        idle,
                        theme,
                    )
                    .aria_label("Reopen feature group")
                    .child("Reopen")
                    .on_click(cx.listener(move |panel, _, _, cx| panel.reopen(&id, cx))),
                ),
            );
        }
        let id = gid.clone();
        actions.push(
            self.probed(
                format!("accept-{gid}"),
                crate::button::button(
                    SharedString::from(format!("board-fg-accept-{index}")),
                    ButtonKind::Primary,
                    ControlSize::Sm,
                    idle && view.can_accept,
                    theme,
                )
                .aria_label("Accept feature group")
                .child("Accept")
                .when(idle && view.can_accept, |button| {
                    button.on_click(cx.listener(move |panel, _, _, cx| panel.accept(&id, cx)))
                }),
            ),
        );
        let mut card = widgets::section_card(theme)
            .id(SharedString::from(format!("board-feature-group-{index}")))
            .child(widgets::section_header(
                theme,
                view.group.title.clone(),
                Some(SharedString::from(description)),
                actions,
            ));
        let mut first = true;
        if !view.group.acceptance.is_empty() {
            card = card.child(widgets::card_row(theme, first).child(widgets::row_text(
                theme,
                "Shared acceptance",
                Some(SharedString::from(view.group.acceptance.clone())),
            )));
            first = false;
        }
        for (member_index, member) in view.members.iter().enumerate() {
            let title = format!(
                "{} · {}",
                member.key,
                member.title.as_deref().unwrap_or("Ticket unavailable")
            );
            let (group_id, work_item_id) = (gid.clone(), member.work_item_id.clone());
            let remove =
                crate::button::button(
                    SharedString::from(format!("board-fg-remove-{index}-{member_index}")),
                    ButtonKind::Ghost,
                    ControlSize::Sm,
                    idle,
                    theme,
                )
                .aria_label(format!("Remove {} from the feature group", member.key))
                .child("Remove")
                .on_click(cx.listener(move |panel, _, _, cx| {
                    panel.remove_member(&group_id, &work_item_id, cx)
                }));
            card = card.child(
                widgets::card_row(theme, first)
                    .child(widgets::row_text(
                        theme,
                        title,
                        Some(SharedString::from(member_line(member))),
                    ))
                    .child(widgets::row_controls().child(
                        self.probed(format!("remove-{gid}-{}", member.work_item_id), remove),
                    )),
            );
            first = false;
        }
        for (gate_index, gate) in view.gates.iter().enumerate() {
            let mut controls = widgets::row_controls().gap(ui_rems(6.0));
            for (verb, state) in [
                ("pass", Some(FeatureGroupGateState::Passed)),
                ("fail", Some(FeatureGroupGateState::Failed)),
                ("clear", None),
            ] {
                if state.is_none() && gate.status == FeatureGroupGateStatus::Pending {
                    continue;
                }
                let (group_id, gate_id) = (gid.clone(), gate.gate.id.clone());
                let label = match verb {
                    "pass" => "Passed",
                    "fail" => "Failed",
                    _ => "Clear",
                };
                let button = crate::button::button(
                    SharedString::from(format!("board-fg-gate-{verb}-{index}-{gate_index}")),
                    if verb == "pass" {
                        ButtonKind::Secondary
                    } else {
                        ButtonKind::Ghost
                    },
                    ControlSize::Sm,
                    idle,
                    theme,
                )
                .aria_label(format!(
                    "Record {} as {}",
                    gate.gate.label,
                    label.to_lowercase()
                ))
                .child(label)
                .on_click(cx.listener(move |panel, _, _, cx| {
                    panel.record_gate(&group_id, &gate_id, state, cx)
                }));
                controls = controls
                    .child(self.probed(format!("gate-{verb}-{gid}-{}", gate.gate.id), button));
            }
            card = card.child(
                widgets::card_row(theme, first)
                    .child(widgets::row_text(
                        theme,
                        format!("Gate · {}", gate.gate.label),
                        Some(SharedString::from(gate_line(gate))),
                    ))
                    .child(controls),
            );
            first = false;
        }
        let verdict = if view.unmet.is_empty() {
            match view.acceptance {
                FeatureGroupAcceptanceState::Accepted => {
                    "Accepted for the tickets as they are now.".to_owned()
                }
                _ => "Ready to accept. Finishing the tickets never accepts the group by itself."
                    .to_owned(),
            }
        } else {
            let reasons: Vec<String> = view.unmet.iter().map(|reason| reason.label()).collect();
            format!("Not ready to accept: {}.", reasons.join("; "))
        };
        card.child(widgets::card_row(theme, first).child(widgets::row_text(
            theme,
            "Acceptance",
            Some(SharedString::from(verdict)),
        )))
        .into_any_element()
    }

    #[cfg(feature = "appshots-fixture")]
    pub fn fixture_readback(&self, cx: &App) -> serde_json::Value {
        let scope_project = self.scope_project(cx);
        let bounds: serde_json::Map<String, serde_json::Value> = self
            .probe
            .borrow()
            .iter()
            .map(|(label, bounds)| {
                (
                    label.clone(),
                    serde_json::json!({
                        "x": f32::from(bounds.origin.x),
                        "y": f32::from(bounds.origin.y),
                        "w": f32::from(bounds.size.width),
                        "h": f32::from(bounds.size.height),
                    }),
                )
            })
            .collect();
        let shown: Vec<serde_json::Value> = self
            .visible(scope_project.as_deref())
            .iter()
            .map(|view| {
                serde_json::json!({
                    "id": view.group.id,
                    "title": view.group.title,
                    "projectId": view.group.project_id,
                    "projectName": view.project_name,
                    "home": scope_project.as_deref().is_none_or(|project| project == view.group.project_id),
                    "revision": view.group.revision,
                    "acceptance": view.acceptance.label(),
                    "canAccept": view.can_accept,
                    "unmet": view.unmet.iter().map(|reason| reason.label()).collect::<Vec<_>>(),
                    "members": view.members.iter().map(|member| serde_json::json!({
                        "id": member.work_item_id,
                        "key": member.key,
                        "projectId": member.project_id,
                        "projectName": member.project_name,
                        "column": member.column,
                        "branch": member.checkout.as_ref().and_then(|checkout| checkout.live_branch.clone()),
                        "line": member_line(member),
                    })).collect::<Vec<_>>(),
                    "gates": view.gates.iter().map(|gate| serde_json::json!({
                        "id": gate.gate.id,
                        "label": gate.gate.label,
                        "status": gate.status.label(),
                    })).collect::<Vec<_>>(),
                })
            })
            .collect();
        serde_json::json!({
            "supported": self.supported,
            "pending": self.pending,
            "loading": self.load_task.is_some(),
            "error": self.error,
            "loadError": self.load_error,
            "issued": self.issued,
            "all": self.groups.len(),
            "groups": shown,
            "bounds": bounds,
        })
    }
}

impl Render for FeatureGroupsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let scope_project = self.scope_project(cx);
        let visible = self.visible(scope_project.as_deref());
        let mut root = div()
            .id("board-feature-groups")
            .w_full()
            .flex()
            .flex_col()
            .gap(ui_rems(10.0));
        for (index, view) in visible.iter().enumerate() {
            root = root.child(self.render_group(index, view, scope_project.as_deref(), &theme, cx));
        }
        for error in [&self.error, &self.load_error].into_iter().flatten() {
            root = root.child(widgets::error_strip(&theme, error.clone()));
        }
        root
    }
}
