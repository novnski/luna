//! Mounting the automations panel (slice 5): the right pane's surface for
//! the active conversation and the Automations page for the sidebar's
//! project (or every project on this host).
use super::*;
use crate::automations::{AutomationCards, AutomationScope, AutomationsEvent, AutomationsPanel};

impl Shell {
    /// The right pane's scope: this window's focused conversation (a split,
    /// child or alongside tab, in whatever project), else Main. Focusing the
    /// right pane itself never moves it; a new focus starts over (its
    /// editor closes, like a project change on the page).
    pub(super) fn automations_conversation_scope(&self, cx: &App) -> AutomationScope {
        AutomationScope::Conversation(
            self.workbench_execution_focus(cx)
                .unwrap_or_else(|| self.active_chat.clone()),
        )
    }

    /// The Automations page's scope: the sidebar's project filter.
    pub(super) fn automations_page_scope(&self) -> AutomationScope {
        AutomationScope::Project(self.settings.space_filter.clone())
    }

    /// An inline card's Open (or Edit), or an automation alert: the
    /// Automations page with that automation shown (its history open). A
    /// filter on another project moves to the automation's own (`project`
    /// when the caller knows it, else the card's record), so the page never
    /// shows it under the wrong one.
    pub(crate) fn open_automation(
        &mut self,
        id: String,
        edit: bool,
        project: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let project = project
            .or_else(|| {
                AutomationCards::global(cx)
                    .read(cx)
                    .record(&id)
                    .map(|record| record.authority.project_id.clone())
            })
            .filter(|project| !project.is_empty());
        if let (Some(filter), Some(project)) = (self.settings.space_filter.as_ref(), project)
            && *filter != project
        {
            self.set_space_filter(Some(project), cx);
        }
        let scope = self.automations_page_scope();
        let panel = self.automations_panel(scope, true, cx);
        self.open_sidebar_page(SidebarPage::Automations, cx);
        panel.update(cx, |panel, cx| panel.focus_automation(id, edit, cx));
    }

    /// The panel for `scope`, created on first use. The right pane's panel
    /// follows the active conversation; `settings` selects the app-wide one.
    pub(super) fn automations_panel(
        &mut self,
        scope: AutomationScope,
        settings: bool,
        cx: &mut Context<Self>,
    ) -> Entity<AutomationsPanel> {
        let slot = if settings {
            &mut self.automations_settings
        } else {
            &mut self.automations_conversation
        };
        if let Some((panel, _)) = slot.as_ref() {
            let panel = panel.clone();
            panel.update(cx, |panel, cx| panel.set_scope(scope, cx));
            return panel;
        }
        let state = self.state.clone();
        let panel = cx.new(|cx| AutomationsPanel::new(state, scope, cx));
        let events = cx.subscribe(&panel, |this, _, event, cx| match event {
            AutomationsEvent::OpenChat(chat) => this.open_chat(chat.clone(), cx),
        });
        let slot = if settings {
            &mut self.automations_settings
        } else {
            &mut self.automations_conversation
        };
        *slot = Some((panel.clone(), events));
        panel
    }

    /// QA: the mounted panel (`settings` selects the app-wide one).
    #[cfg(feature = "appshots-fixture")]
    pub fn fixture_automations(&self, settings: bool) -> Option<Entity<AutomationsPanel>> {
        let slot = if settings {
            &self.automations_settings
        } else {
            &self.automations_conversation
        };
        slot.as_ref().map(|(panel, _)| panel.clone())
    }

    /// QA: open the right pane's Automations surface for the active chat.
    #[cfg(feature = "appshots-fixture")]
    pub fn fixture_open_automations(&mut self, cx: &mut Context<Self>) {
        self.add_work_surface(RightSurface::Automations, cx);
        let scope = self.automations_conversation_scope(cx);
        self.automations_panel(scope, false, cx);
    }

    /// QA: close the right pane's Automations surface (its panel goes too).
    #[cfg(feature = "appshots-fixture")]
    pub fn fixture_close_automations(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_right_surface(RightSurface::Automations, window, cx);
    }

    /// QA: open Settings → Automations.
    #[cfg(feature = "appshots-fixture")]
    pub fn fixture_open_automation_settings(&mut self, cx: &mut Context<Self>) {
        self.open_settings(SettingsSection::Automations, cx);
        let scope = self.automations_page_scope();
        self.automations_panel(scope, true, cx);
    }

    /// QA: an inline automation card's Open (`edit: false`) or Edit.
    #[cfg(feature = "appshots-fixture")]
    pub fn fixture_open_automation(&mut self, id: &str, edit: bool, cx: &mut Context<Self>) {
        self.open_automation(id.to_string(), edit, None, cx);
    }
}
