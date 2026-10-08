//! Claude Code advanced coordination and hosted-integration surfaces:
//!
//! - provider-native workflow/teammate runs for the selected chat (Agents pane),
//! - Claude's own sessions outside LUNA (`claude agents --json`, read-only),
//! - the certification ledger for workflows/teams/channels and hosted/device
//!   integrations, so every lane shows its real LUNA status.
//!
//! Pure render functions: the shell and settings own state and listeners, and
//! the headless fixture renders the same functions.

use gpui::{AnyElement, IntoElement, SharedString, div, prelude::*, px};

use luna_proto::{
    ClaudeExternalSession, ClaudeExternalSessionActivity, ClaudeExternalSessionKind,
    ClaudeExternalSessionState, ClaudeExternalSessionsView, NativeTaskEndReason, NativeTaskKind,
    NativeTaskLedger, NativeTaskSnapshot, NativeTaskStatus, NativeWorkflowAgentState,
};

use crate::theme::Theme;

/// Agents shown per phase before collapsing to a count.
const AGENTS_PER_PHASE: usize = 8;

fn small(theme: &Theme, text: impl Into<SharedString>) -> gpui::Div {
    div()
        .text_size(crate::typography::ui_rems(11.0))
        .text_color(theme.text_muted)
        .child(text.into())
}

fn compact_tokens(tokens: u64) -> String {
    match tokens {
        0..=999 => format!("{tokens} tokens"),
        1_000..=999_999 => format!("{:.1}k tokens", tokens as f64 / 1_000.0),
        _ => format!("{:.2}M tokens", tokens as f64 / 1_000_000.0),
    }
}

fn duration(ms: u64) -> String {
    let secs = ms / 1_000;
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3_599 => format!("{}m {}s", secs / 60, secs % 60),
        _ => format!("{}h {}m", secs / 3_600, (secs % 3_600) / 60),
    }
}

/// Status words always ride as text; colour only reinforces them.
fn status_text(task: &NativeTaskSnapshot) -> String {
    match (task.status, task.end_reason) {
        (NativeTaskStatus::Stopped, Some(NativeTaskEndReason::SessionEnded)) => {
            "Stopped · Claude process ended".into()
        }
        (NativeTaskStatus::Stopped, Some(NativeTaskEndReason::WorkerRestart)) => {
            "Stopped · worker restarted".into()
        }
        (status, _) => status.label().into(),
    }
}

fn status_color(theme: &Theme, status: NativeTaskStatus) -> gpui::Hsla {
    match status {
        NativeTaskStatus::Running | NativeTaskStatus::Pending => theme.accent,
        NativeTaskStatus::Paused => theme.warning,
        NativeTaskStatus::Completed => theme.success,
        NativeTaskStatus::Failed => theme.danger,
        NativeTaskStatus::Stopped => theme.text_muted,
    }
}

fn agent_state_color(theme: &Theme, state: NativeWorkflowAgentState) -> gpui::Hsla {
    match state {
        NativeWorkflowAgentState::Running => theme.accent,
        NativeWorkflowAgentState::Done => theme.success,
        NativeWorkflowAgentState::Failed => theme.danger,
        NativeWorkflowAgentState::Started | NativeWorkflowAgentState::Unknown => theme.text_muted,
    }
}

fn metrics_line(task: &NativeTaskSnapshot) -> String {
    let mut parts = Vec::new();
    if task.graph_observed {
        let done = task
            .agents
            .iter()
            .filter(|a| a.state == NativeWorkflowAgentState::Done)
            .count();
        let failed = task
            .agents
            .iter()
            .filter(|a| a.state == NativeWorkflowAgentState::Failed)
            .count();
        let mut agents = format!(
            "{} agent{} · {done} done",
            task.agents.len(),
            if task.agents.len() == 1 { "" } else { "s" }
        );
        if failed > 0 {
            agents.push_str(&format!(" · {failed} failed"));
        }
        if !task.phases.is_empty() {
            parts.push(format!(
                "{} phase{}",
                task.phases.len(),
                if task.phases.len() == 1 { "" } else { "s" }
            ));
        }
        parts.push(agents);
    }
    if let Some(tokens) = task.total_tokens {
        parts.push(compact_tokens(tokens));
    }
    if let Some(tools) = task.tool_uses {
        parts.push(format!("{tools} tool calls"));
    }
    if let Some(ms) = task.duration_ms {
        parts.push(duration(ms));
    }
    if parts.is_empty() {
        "Usage not reported yet".into()
    } else {
        parts.join(" · ")
    }
}

fn task_title(task: &NativeTaskSnapshot) -> String {
    match &task.name {
        Some(name) => format!("{} · {name}", task.kind.label()),
        None => task.kind.label().to_owned(),
    }
}

fn task_card(theme: &Theme, task: &NativeTaskSnapshot) -> AnyElement {
    let title = task_title(task);
    let status = status_text(task);
    let metrics = metrics_line(task);
    let accessible = format!("{title}, {status}. {metrics}");
    let mut card = div()
        .id(SharedString::from(format!("native-task-{}", task.task_id)))
        .flex_none()
        .px(px(12.0))
        .py(px(10.0))
        .rounded(px(8.0))
        .border_1()
        .border_color(theme.border)
        .flex()
        .flex_col()
        .gap(px(4.0))
        .role(gpui::Role::Group)
        .aria_label(accessible)
        .child(
            div()
                .flex()
                .justify_between()
                .gap(px(8.0))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_color(theme.text)
                        .child(title),
                )
                .child(
                    div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .child(
                            div()
                                .size(px(7.0))
                                .rounded_full()
                                .bg(status_color(theme, task.status)),
                        )
                        .child(div().text_color(theme.text_muted).child(status)),
                ),
        );
    if !task.description.is_empty() {
        card = card.child(
            div()
                .text_color(theme.text_muted)
                .line_clamp(2)
                .child(task.description.clone()),
        );
    }
    card = card.child(small(theme, metrics));
    if task.graph_observed {
        card = card.child(phase_list(theme, task));
    } else if task.kind == NativeTaskKind::Workflow {
        card = card.child(small(
            theme,
            "This Claude version has not reported a per-agent graph for this run.",
        ));
    }
    if let Some(activity) = task
        .last_activity
        .as_ref()
        .filter(|_| !task.status.is_terminal())
    {
        card = card.child(small(theme, format!("Latest: {activity}")));
    }
    if let Some(summary) = &task.summary {
        card = card.child(
            div()
                .mt(px(2.0))
                .text_color(theme.text)
                .line_clamp(4)
                .child(summary.clone()),
        );
    }
    if let Some(error) = &task.error {
        card = card.child(
            div()
                .text_color(theme.readable_tone(theme.danger, 1.0))
                .line_clamp(3)
                .child(error.clone()),
        );
    }
    if let Some(path) = &task.output_file {
        card = card.child(small(theme, format!("Result file: {path}")).truncate());
    }
    let notes: &[&str] = match (task.kind, task.status.is_terminal()) {
        (NativeTaskKind::Workflow, false) => &[
            "LUNA can't stop or pause native workflows yet. Claude exposes no pause control to LUNA.",
            "Agent approvals that arrive after the launching turn ends are declined. Allow the tools the workflow needs first.",
        ],
        (NativeTaskKind::Teammate, _) => &[
            "Experimental agent teams are not enabled by LUNA. External Claude configuration started this teammate.",
        ],
        (NativeTaskKind::RemoteAgent, _) => {
            &["Runs on Anthropic's cloud under your Claude account. LUNA only observes it."]
        }
        _ => &[],
    };
    for note in notes {
        card = card.child(
            div()
                .text_size(crate::typography::ui_rems(11.0))
                .text_color(theme.text_faint)
                .child(*note),
        );
    }
    card.into_any_element()
}

fn phase_list(theme: &Theme, task: &NativeTaskSnapshot) -> AnyElement {
    let mut list = div().mt(px(4.0)).flex().flex_col().gap(px(4.0));
    let mut groups: Vec<(Option<u32>, String)> = task
        .phases
        .iter()
        .map(|phase| (Some(phase.index), phase.title.clone()))
        .collect();
    if task.agents.iter().any(|a| {
        a.phase_index
            .is_none_or(|i| !task.phases.iter().any(|p| p.index == i))
    }) {
        groups.push((None, "Other agents".into()));
    }
    for (index, title) in groups {
        let agents: Vec<_> = task
            .agents
            .iter()
            .filter(|agent| match index {
                Some(index) => agent.phase_index == Some(index),
                None => agent
                    .phase_index
                    .is_none_or(|i| !task.phases.iter().any(|p| p.index == i)),
            })
            .collect();
        let done = agents
            .iter()
            .filter(|a| a.state == NativeWorkflowAgentState::Done)
            .count();
        let heading = match index {
            Some(index) => format!("Phase {index} · {title} — {done}/{} done", agents.len()),
            None => format!("{title} — {done}/{} done", agents.len()),
        };
        list = list.child(
            div()
                .text_size(crate::typography::ui_rems(11.5))
                .text_color(theme.text)
                .child(heading),
        );
        for agent in agents.iter().take(AGENTS_PER_PHASE) {
            let mut detail = vec![agent.state.label().to_owned()];
            if agent.cached {
                detail.push("cached".into());
            }
            if agent.blocked {
                detail.push("blocked".into());
            }
            if let Some(model) = &agent.model {
                detail.push(model.clone());
            }
            if let Some(tokens) = agent.tokens {
                detail.push(compact_tokens(tokens));
            }
            list = list.child(
                div()
                    .pl(px(10.0))
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .text_size(crate::typography::ui_rems(11.0))
                    .child(
                        div()
                            .flex_none()
                            .size(px(5.0))
                            .rounded_full()
                            .bg(agent_state_color(theme, agent.state)),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_color(theme.text_muted)
                            .child(format!("{} — {}", agent.label, detail.join(" · "))),
                    ),
            );
            if let Some(error) = &agent.error {
                list = list.child(
                    div()
                        .pl(px(21.0))
                        .text_size(crate::typography::ui_rems(11.0))
                        .text_color(theme.readable_tone(theme.danger, 1.0))
                        .truncate()
                        .child(error.clone()),
                );
            }
        }
        if agents.len() > AGENTS_PER_PHASE {
            list = list.child(small(
                theme,
                format!("+{} more agents", agents.len() - AGENTS_PER_PHASE),
            ));
        }
    }
    list.into_any_element()
}

/// The selected chat's native workflow/teammate runs, newest first. `None`
/// when the provider reported none.
pub fn native_task_section(theme: &Theme, ledger: &NativeTaskLedger) -> Option<AnyElement> {
    if ledger.tasks.is_empty() {
        return None;
    }
    let active = ledger
        .tasks
        .iter()
        .filter(|t| !t.status.is_terminal())
        .count();
    let mut section = div()
        .id("native-tasks")
        .flex()
        .flex_col()
        .gap(px(8.0))
        .role(gpui::Role::Region)
        .aria_label(format!(
            "Native workflows and teammates: {} total, {active} active",
            ledger.tasks.len()
        ))
        .child(
            div()
                .flex()
                .justify_between()
                .child(div().text_color(theme.text).child("Workflows & teammates"))
                .child(small(
                    theme,
                    if active > 0 {
                        format!("{active} active")
                    } else {
                        "Last observed".into()
                    },
                )),
        );
    for task in ledger.tasks.iter().rev() {
        section = section.child(task_card(theme, task));
    }
    Some(section.into_any_element())
}

/// Read state of the external-session list.
#[derive(Debug, Clone, Default)]
pub enum ExternalSessionsState {
    #[default]
    Idle,
    Loading,
    Ready(ClaudeExternalSessionsView),
    Error(String),
}

fn external_row(theme: &Theme, session: &ClaudeExternalSession, index: usize) -> AnyElement {
    use crate::settings::widgets;
    let kind = match session.kind {
        ClaudeExternalSessionKind::Background => "Background session",
        ClaudeExternalSessionKind::Interactive if session.owned_by_luna => "LUNA chat process",
        ClaudeExternalSessionKind::Interactive => "Terminal / IDE session",
        ClaudeExternalSessionKind::Unknown => "Claude session",
    };
    let title = match (&session.name, &session.short_id) {
        (Some(name), Some(id)) => format!("{name} · {id}"),
        (Some(name), None) => name.clone(),
        (None, Some(id)) => id.clone(),
        (None, None) => kind.to_owned(),
    };
    let state = match session.state {
        Some(ClaudeExternalSessionState::Working) => Some("working"),
        Some(ClaudeExternalSessionState::Blocked) => Some("blocked"),
        Some(ClaudeExternalSessionState::Done) => Some("done"),
        Some(ClaudeExternalSessionState::Failed) => Some("failed"),
        Some(ClaudeExternalSessionState::Stopped) => Some("stopped"),
        Some(ClaudeExternalSessionState::Unknown) => Some("unknown state"),
        None => None,
    };
    let activity = match session.activity {
        Some(ClaudeExternalSessionActivity::Busy) => Some("busy".to_owned()),
        Some(ClaudeExternalSessionActivity::Waiting) => Some(match &session.waiting_for {
            Some(reason) => format!("waiting for {reason}"),
            None => "waiting".into(),
        }),
        Some(ClaudeExternalSessionActivity::Idle) => Some("idle".to_owned()),
        Some(ClaudeExternalSessionActivity::Unknown) => Some("unknown activity".to_owned()),
        None => None,
    };
    let mut meta = vec![kind.to_owned()];
    meta.extend(state.map(str::to_owned));
    meta.extend(activity);
    if !session.process_alive {
        meta.push("not running".into());
    }
    let meta = meta.join(" · ");
    // The working directory wraps rather than truncating: it is the one
    // fact that tells two sessions apart.
    widgets::card_row(theme, false)
        .id(SharedString::from(format!("claude-session-{index}")))
        .role(gpui::Role::Group)
        .aria_label(format!("{title}. {meta}. {}", session.cwd))
        .child(
            widgets::row_text(theme, title, Some(meta.into()))
                .child(widgets::row_description(theme, session.cwd.clone())),
        )
        .into_any_element()
}

/// Claude's own sessions outside LUNA. `refresh` is the caller's labelled,
/// keyboard-reachable button.
pub fn external_sessions_card(
    theme: &Theme,
    state: &ExternalSessionsState,
    refresh: AnyElement,
) -> AnyElement {
    external_sessions_section(theme, state, refresh, None)
}

/// [`external_sessions_card`] with the caller's loading placeholder
/// (Settings passes skeleton rows; without one the card says "Checking…").
pub fn external_sessions_section(
    theme: &Theme,
    state: &ExternalSessionsState,
    refresh: AnyElement,
    loading: Option<AnyElement>,
) -> AnyElement {
    use crate::settings::widgets;
    let note = |copy: &'static str| {
        widgets::card_row(theme, false).child(widgets::row_description(theme, copy).flex_1())
    };
    let strip = |strip: gpui::Div| {
        widgets::card_row(theme, false).child(div().w_full().min_w_0().child(strip.mt_0()))
    };
    let mut card = widgets::section_card(theme).child(widgets::section_header(
        theme,
        "Claude sessions outside LUNA",
        Some(
            "Read from `claude agents --json` on this device. LUNA never attaches to, stops or \
             removes these sessions. It refuses to resume a conversation that another live \
             Claude process is writing."
                .into(),
        ),
        vec![refresh],
    ));
    let mut footer = None;
    match state {
        ExternalSessionsState::Idle => card = card.child(note("Not checked yet.")),
        ExternalSessionsState::Loading => {
            card = card.child(match loading {
                Some(placeholder) => widgets::card_row(theme, false)
                    .child(div().w_full().min_w_0().child(placeholder)),
                None => note("Checking…"),
            })
        }
        ExternalSessionsState::Error(error) => {
            card = card.child(strip(widgets::error_strip(
                theme,
                format!("Couldn't check Claude sessions: {error}"),
            )))
        }
        ExternalSessionsState::Ready(view) => {
            if let Some(reason) = &view.unavailable_reason {
                card = card.child(strip(widgets::warning_strip(
                    theme,
                    format!("Unavailable: {reason}"),
                )));
            } else if view.sessions.is_empty() {
                card = card.child(note("Claude reports no live sessions on this device."));
            } else {
                for (index, session) in view.sessions.iter().enumerate() {
                    card = card.child(external_row(theme, session, index));
                }
                if view.truncated {
                    footer = Some("Showing the first 200 sessions.");
                }
            }
        }
    }
    div()
        .flex()
        .flex_col()
        .child(card)
        .when_some(footer, |el, copy| {
            el.child(widgets::section_footer(theme, copy))
        })
        .into_any_element()
}

/// Certification status of one Claude Code lane in LUNA.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaneStatus {
    /// Verified against the installed CLI with retained evidence.
    Certified,
    /// Implemented and verified with frame shapes from the installed binary.
    /// A live provider run is still pending.
    LiveQaPending,
    /// The native feature exists, but LUNA never turns it on.
    NotEnabled,
    /// LUNA cannot perform it today.
    Unsupported,
    /// Needs an account, plan, OS permission or hosted service LUNA does not embed.
    ExternallyGated,
    /// Only usable in Claude Code itself; LUNA offers no embedded control.
    NativeHandoff,
}

impl LaneStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Certified => "Certified",
            Self::LiveQaPending => "Supported · live QA pending",
            Self::NotEnabled => "Not enabled by LUNA",
            Self::Unsupported => "Unsupported in LUNA",
            Self::ExternallyGated => "Externally gated",
            Self::NativeHandoff => "Native Claude Code only",
        }
    }
}

pub struct LedgerRow {
    pub lane: &'static str,
    pub status: LaneStatus,
    pub luna: &'static str,
}

/// Mirrors `docs/reference/claude-code-advanced-certification.md`
/// (Claude Code 2.1.286). Keep both in step.
pub const CLAUDE_ADVANCED_LEDGER: &[LedgerRow] = &[
    LedgerRow {
        lane: "Dynamic workflow runs",
        status: LaneStatus::LiveQaPending,
        luna: "Phases, agents, usage and the final state appear in the Agents pane and persist with the chat.",
    },
    LedgerRow {
        lane: "Workflow availability",
        status: LaneStatus::Certified,
        luna: "Follows Claude's own gate. Disabling workflows removes /deep-research and /workflow-authoring.",
    },
    LedgerRow {
        lane: "Stop a workflow",
        status: LaneStatus::Unsupported,
        luna: "LUNA sends Claude's stop_task only for native agents, not workflow runs.",
    },
    LedgerRow {
        lane: "Pause, resume or restart workflow agents",
        status: LaneStatus::NativeHandoff,
        luna: "Only in Claude Code's /workflows view. Claude exposes no SDK control for these.",
    },
    LedgerRow {
        lane: "Workflow agent approvals after the turn",
        status: LaneStatus::Unsupported,
        luna: "Declined once the launching turn has finished. Allow the needed tools first.",
    },
    LedgerRow {
        lane: "Agent teams",
        status: LaneStatus::NotEnabled,
        luna: "Experimental opt-in that LUNA never sets. Teammates started elsewhere show as experimental.",
    },
    LedgerRow {
        lane: "Channels",
        status: LaneStatus::NotEnabled,
        luna: "LUNA never passes --channels, and channel events do not reach LUNA's stream.",
    },
    LedgerRow {
        lane: "Cross-session peer messages",
        status: LaneStatus::Unsupported,
        luna: "LUNA chats never join another Claude session's message bridge.",
    },
    LedgerRow {
        lane: "Claude sessions outside LUNA",
        status: LaneStatus::Certified,
        luna: "Listed read-only below.",
    },
    LedgerRow {
        lane: "Two-writer protection on resume",
        status: LaneStatus::Certified,
        luna: "Refuses a resume when Claude reports another live writer for that conversation.",
    },
    LedgerRow {
        lane: "Remote agents",
        status: LaneStatus::ExternallyGated,
        luna: "Cloud runs under your Claude account. LUNA observes them only.",
    },
    LedgerRow {
        lane: "Claude in Chrome",
        status: LaneStatus::NativeHandoff,
        luna: "Needs the Chrome extension and claude --chrome. LUNA never enables it.",
    },
    LedgerRow {
        lane: "Computer use",
        status: LaneStatus::ExternallyGated,
        luna: "Needs macOS permissions and a supported plan. LUNA never grants OS access.",
    },
    LedgerRow {
        lane: "Remote Control",
        status: LaneStatus::NativeHandoff,
        luna: "Interactive Claude Code only. This is separate from LUNA's own remote access.",
    },
    LedgerRow {
        lane: "Cloud sessions and teleport",
        status: LaneStatus::NativeHandoff,
        luna: "claude --cloud and --teleport need a claude.ai account. They are not embedded.",
    },
    LedgerRow {
        lane: "Cloud Projects",
        status: LaneStatus::ExternallyGated,
        luna: "No embeddable interface is verified.",
    },
    LedgerRow {
        lane: "Artifacts",
        status: LaneStatus::ExternallyGated,
        luna: "Hosted publishing. LUNA does not publish on Claude's behalf.",
    },
    LedgerRow {
        lane: "Routines and cloud schedules",
        status: LaneStatus::ExternallyGated,
        luna: "Account-gated hosted schedules. These are separate from session cron.",
    },
    LedgerRow {
        lane: "Ultrareview",
        status: LaneStatus::NativeHandoff,
        luna: "Billed cloud review via claude ultrareview. LUNA does not start it.",
    },
    LedgerRow {
        lane: "Push notifications",
        status: LaneStatus::ExternallyGated,
        luna: "Needs Claude's mobile app and account.",
    },
];

fn lane_color(theme: &Theme, status: LaneStatus) -> gpui::Hsla {
    match status {
        LaneStatus::Certified => theme.success,
        LaneStatus::LiveQaPending => theme.accent,
        LaneStatus::NotEnabled | LaneStatus::NativeHandoff => theme.text_muted,
        LaneStatus::Unsupported => theme.warning,
        LaneStatus::ExternallyGated => theme.text_faint,
    }
}

/// Every advanced Claude lane with its truthful LUNA status.
pub fn ledger_card(theme: &Theme) -> AnyElement {
    use crate::settings::widgets;
    use crate::typography::ui_rems;
    let mut card = widgets::section_card(theme)
        .id("claude-advanced-ledger")
        .role(gpui::Role::List)
        .aria_label("Claude Code advanced features in LUNA")
        .child(widgets::section_header(
            theme,
            "Claude Code advanced features",
            Some(
                "What LUNA does with each native lane in Claude Code 2.1.286. Unavailable lanes \
                 say why."
                    .into(),
            ),
            Vec::new(),
        ));
    for (index, row) in CLAUDE_ADVANCED_LEDGER.iter().enumerate() {
        // The status rides as text; the dot only reinforces it.
        let status = div()
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(ui_rems(6.0))
            .text_size(ui_rems(11.0))
            .line_height(ui_rems(14.0))
            .text_color(theme.readable_muted(0.9))
            .child(
                div()
                    .flex_none()
                    .size(ui_rems(7.0))
                    .rounded_full()
                    .bg(lane_color(theme, row.status)),
            )
            .child(row.status.label());
        card = card.child(
            widgets::card_row(theme, false)
                .id(SharedString::from(format!("claude-lane-{index}")))
                .role(gpui::Role::ListItem)
                .aria_label(format!(
                    "{}: {}. {}",
                    row.lane,
                    row.status.label(),
                    row.luna
                ))
                .child(widgets::row_text(theme, row.lane, Some(row.luna.into())))
                .child(widgets::row_controls().child(status)),
        );
    }
    card.into_any_element()
}
