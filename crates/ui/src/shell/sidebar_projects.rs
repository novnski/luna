//! Project identity and live counts for the sidebar: the coloured project
//! tile shared by the project pill, the picker and the All projects groups,
//! and each project's active / needs-you / working tallies.
//!
//! Child module of `shell` so it reads straight off `Shell`'s private state.

use super::*;
use luna_proto::ChatIndicator;

/// What a project has going on right now, from the same rows the sidebar
/// draws (placement-hidden children excluded, settled and archived tasks
/// not counted as active).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct ProjectActivity {
    /// Tasks still in the active list (pins included).
    pub active: usize,
    /// Tasks waiting on the user: an approval, a question, an unseen failure
    /// or an unresolved Activity item.
    pub need: usize,
    /// Tasks whose session is running (and not also waiting on the user).
    pub working: usize,
    /// The most recent message or creation time of any task, settled ones
    /// included, so a quiet project still says when it was last used.
    pub last: Option<chrono::DateTime<Utc>>,
}

impl ProjectActivity {
    /// The pill's second line and the picker's summary: what needs the user
    /// first, then the size of the active list.
    pub(super) fn summary(&self) -> String {
        match (self.active, self.need) {
            (0, _) => "No active tasks".into(),
            (active, 0) => format!("{active} active"),
            (active, need) => format!(
                "{active} active · {need} need{} you",
                if need == 1 { "s" } else { "" }
            ),
        }
    }
}

/// Six hues spread around the wheel (violet, green, amber, blue, pink,
/// coral), far enough apart to tell side by side.
const PROJECT_HUES: [f32; 6] = [250.0, 150.0, 40.0, 205.0, 325.0, 12.0];

/// A project's tint: projects take the hues in the order they were added,
/// so the first six never share one and adding a project never recolours
/// the others. Synced creation times keep it the same on every device.
pub(super) fn project_tint(state: &AppState, space_id: &str) -> gpui::Hsla {
    let mut spaces: Vec<(chrono::DateTime<Utc>, &str)> = state
        .spaces
        .iter()
        .map(|space| (space.created_at, space.id.as_str()))
        .collect();
    spaces.sort();
    let ix = spaces
        .iter()
        .position(|(_, id)| *id == space_id)
        // An unknown id (a project removed mid-render): a stable fallback.
        .unwrap_or_else(|| space_id.bytes().map(usize::from).sum());
    let hue = PROJECT_HUES[ix % PROJECT_HUES.len()];
    gpui::hsla(hue / 360.0, 0.62, 0.68, 1.0)
}

/// The first letter or digit of the name, upper-cased ("luna" → "L").
pub(super) fn project_monogram(name: &str) -> SharedString {
    name.chars()
        .find(|ch| ch.is_alphanumeric())
        .map(|ch| ch.to_uppercase().collect::<String>())
        .unwrap_or_else(|| "·".into())
        .into()
}

/// A rounded square with the project's monogram on its tint. `size` is in
/// interface rems so the tile follows Interface size with the text beside it.
pub(super) fn project_tile(tint: gpui::Hsla, name: &str, size: f32, theme: &Theme) -> gpui::Div {
    div()
        .size(crate::typography::ui_rems(size))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(crate::typography::ui_rems((size * 0.28).round()))
        .bg(tint.opacity(if theme.appearance.is_light() {
            0.22
        } else {
            0.2
        }))
        .text_size(crate::typography::ui_rems((size * 0.5).round()))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(theme.readable_tone(tint, 1.0))
        .child(project_monogram(name))
}

/// The neutral tile that stands for every project at once.
pub(super) fn all_projects_tile(size: f32, theme: &Theme) -> gpui::Div {
    div()
        .size(crate::typography::ui_rems(size))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(crate::typography::ui_rems((size * 0.28).round()))
        .bg(theme.text.opacity(0.07))
        .child(
            icon(icons::FOLDER_WITH_FILES)
                .size(crate::typography::ui_rems((size * 0.58).round()))
                .text_color(theme.text_muted),
        )
}

/// Collapse the home directory to `~` so a project's path reads at a glance.
pub(super) fn display_project_path(path: &str) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    let home = home.trim_end_matches('/');
    if !home.is_empty() && super::spaces::path_under(path, home) {
        format!("~{}", &path[home.len()..])
    } else {
        path.to_string()
    }
}

impl Shell {
    /// Live counts per project id, for the pill, the picker and the All
    /// projects group headers. One pass over the rows the sidebar draws.
    pub(super) fn project_activity(
        &self,
        cx: &App,
    ) -> std::collections::HashMap<String, ProjectActivity> {
        let now = Utc::now();
        let attention = self.activity_task_keys(cx);
        let mut blocking = attention.clone();
        blocking.extend(self.settlement_blocking_task_keys());
        let placed = self.effective_placements(cx);
        let auto_settle = self.settings.sidebar_auto_settle_after_days;
        let state = self.state.read(cx);
        let mut out: std::collections::HashMap<String, ProjectActivity> = Default::default();
        for (status, chat) in spaces::sidebar_rows(state.sidebar_chats(now, None), state, &placed) {
            let Some(space_id) = chat.space_id.clone() else {
                continue;
            };
            let key = (chat.device_id.clone(), chat.id.clone());
            let entry = out.entry(space_id).or_default();
            let at = chat.last_message_at.unwrap_or(chat.created_at);
            entry.last = Some(entry.last.map_or(at, |last| last.max(at)));
            let settled = match self.pending_task_actions.get(&chat.id) {
                Some(_) => true,
                None => {
                    spaces::is_settled_chat(chat, status, now, auto_settle, blocking.contains(&key))
                }
            };
            if chat.archived || settled {
                continue;
            }
            entry.active += 1;
            if attention.contains(&key)
                || matches!(
                    status,
                    ChatIndicator::AwaitingApproval
                        | ChatIndicator::AwaitingInput
                        | ChatIndicator::Errored
                )
            {
                entry.need += 1;
            } else if status == ChatIndicator::Working {
                entry.working += 1;
            }
        }
        out
    }
}

impl Shell {
    /// All projects, outside Activity, lists tasks as a tree: one group per
    /// project (Design D). A single project keeps the flat list (Design A).
    pub(super) fn project_tree_view(&self) -> bool {
        self.settings.space_filter.is_none() && !self.activity.view_active
    }

    /// An All projects group's header: the disclosure chevron, the project's
    /// tile and name, what needs the user and what is running, the active
    /// count while folded, and (under the pointer) a New task button for
    /// that project. Same height as every sidebar section header, so the
    /// list's height bookkeeping is unchanged.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn project_group_header(
        &self,
        id: SharedString,
        space_id: &str,
        name: SharedString,
        collapsed: bool,
        activity: ProjectActivity,
        chevron: AnyElement,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let count = |tone: gpui::Hsla, glyph: AnyElement, n: usize| {
            div()
                .flex_none()
                .flex()
                .flex_row()
                .items_center()
                .gap(crate::typography::ui_rems(3.0))
                .text_size(crate::typography::ui_rems(11.0))
                .font_weight(gpui::FontWeight::MEDIUM)
                .font_features(crate::typography::tabular_figures())
                .text_color(theme.readable_tone(tone, 1.0))
                .child(glyph)
                .child(SharedString::from(n.to_string()))
        };
        #[cfg(feature = "appshots-fixture")]
        self.sidebar_mark(format!("project-header:{space_id}"), || {
            vec![
                name.to_string(),
                format!("need={}", activity.need),
                format!("working={}", activity.working),
                format!("active={}", activity.active),
                format!("collapsed={collapsed}"),
            ]
        });
        let new_task_space = space_id.to_owned();
        let new_task_id = format!("{id}-new-task");
        let new_task_label: SharedString = format!("New task in {name}").into();
        div()
            .id(id.clone())
            .group("sidebar-project-header")
            .role(gpui::Role::Button)
            .aria_label(SharedString::from(format!(
                "{name}, {} active",
                activity.active
            )))
            .aria_expanded(!collapsed)
            .flex()
            .flex_row()
            .items_center()
            .gap(crate::typography::ui_rems(7.0))
            .h(px(
                super::spaces::SIDEBAR_DISCLOSURE_HEADER_HEIGHT * theme.ui_scale()
            ))
            .pl(crate::typography::ui_rems(4.0))
            .pr(px(Theme::SPACE_SM))
            .rounded(px(crate::theme::radius::SM))
            .cursor_pointer()
            .hover_wash(id)
            .child(chevron)
            .child(project_tile(
                project_tint(self.state.read(cx), space_id),
                &name,
                18.0,
                theme,
            ))
            .child(super::sidebar_faded_label(
                "sidebar-project-label".into(),
                false,
                div()
                    .text_size(crate::typography::ui_rems(12.5))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme.text)
                    .child(name),
            ))
            .when(activity.need > 0, |el| {
                el.child(count(
                    theme.warning,
                    icon(icons::SHIELD_WARNING)
                        .size(crate::typography::ui_rems(11.0))
                        .text_color(theme.readable_icon(theme.warning))
                        .into_any_element(),
                    activity.need,
                ))
            })
            .when(activity.working > 0, |el| {
                el.child(count(
                    theme.busy,
                    div()
                        .size(crate::typography::ui_rems(6.0))
                        .rounded(px(1.5))
                        .bg(theme.busy)
                        .into_any_element(),
                    activity.working,
                ))
            })
            .child(div().flex_1())
            .when(collapsed, |el| {
                el.child(
                    div()
                        .flex_none()
                        .text_size(crate::typography::ui_rems(11.0))
                        .text_color(theme.readable_faint(0.9))
                        .child(SharedString::from(format!("{} active", activity.active))),
                )
            })
            .child(
                div()
                    .id(SharedString::from(new_task_id.clone()))
                    .role(gpui::Role::Button)
                    .aria_label(new_task_label.clone())
                    .size(px(crate::surface_chrome::CONTROL_S))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(crate::surface_chrome::RADIUS_S))
                    .opacity(0.0)
                    .group_hover("sidebar-project-header", |style| style.opacity(1.0))
                    .hover_wash(new_task_id)
                    .tooltip(move |_, cx| {
                        cx.new(|_| super::SurfaceTabTooltip {
                            text: new_task_label.clone(),
                        })
                        .into()
                    })
                    .tooltip_show_delay(crate::popover::TOOLTIP_DELAY)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.land_in_space(new_task_space.clone(), cx);
                    }))
                    .child(
                        icon(icons::PLUS)
                            .size(px(crate::surface_chrome::ICON_S))
                            .text_color(theme.text_muted),
                    ),
            )
    }
}
