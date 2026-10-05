//! Mounting the automations panel (slice 5): the right pane's surface for
//! the active conversation and Settings → Automations for the whole host.
use super::*;
use crate::automations::{AutomationScope, AutomationsEvent, AutomationsPanel};

impl Shell {
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
        let scope = AutomationScope::Conversation(self.active_chat.clone());
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
        self.automations_panel(AutomationScope::All, true, cx);
    }
}
