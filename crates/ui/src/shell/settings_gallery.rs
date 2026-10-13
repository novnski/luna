//! Settings gallery fixture hooks (`examples/settings-gallery-fixture.rs`):
//! route the production Shell to a settings section and read back what the
//! routed page has loaded, so captures wait for real data rather than a delay.
#![cfg(feature = "appshots-fixture")]

use super::*;

impl Shell {
    /// Settings → `section` through the production route: lazy page creation,
    /// refresh-on-visit and the usage refresh all run as they do for a click.
    pub fn fixture_gallery_open(&mut self, section: SettingsSection, cx: &mut Context<Self>) {
        self.open_settings(section, cx);
    }

    /// The routed section, whether its page entity exists, whether the boot
    /// splash is gone, and the loads and seeded rows each page depends on.
    pub fn fixture_gallery_readback(&self, cx: &Context<Self>) -> serde_json::Value {
        let state = self.state.read(cx);
        let section = match self.route {
            Route::Settings(section) => Some(section),
            Route::Chat | Route::Page(_) => None,
        };
        let mounted = section.is_some_and(|section| match section {
            SettingsSection::General => self.general_page.is_some(),
            SettingsSection::Projects => self.projects_page.is_some(),
            SettingsSection::Providers => self.accounts_page.is_some(),
            SettingsSection::Extensions => self.extensions_page.is_some(),
            SettingsSection::Connections => self.connections_page.is_some(),
            SettingsSection::Appearance => self.appearance_page.is_some(),
            SettingsSection::Files => self.files_settings_page.is_some(),
            SettingsSection::Backups => self.backups_page.is_some(),
            SettingsSection::Diagnostics => self.diagnostics_page.is_some(),
            SettingsSection::Notifications => self.notifications_page.is_some(),
            SettingsSection::Shortcuts | SettingsSection::Appshots => self.shortcuts_page.is_some(),
            SettingsSection::Archived => self.archived_page.is_some(),
            SettingsSection::Devices => self.devices_page.is_some(),
            SettingsSection::Automations => self.automations_settings.is_some(),
            // Rendered by the Shell itself; there is no page entity.
            SettingsSection::Usage | SettingsSection::Updates => true,
        });
        let automations = self
            .automations_settings
            .as_ref()
            .map(|(panel, _)| panel.read(cx).fixture_readback(cx));
        let update = self.update_status(cx);
        serde_json::json!({
            "section": section.map(|section| format!("{section:?}")),
            "mounted": mounted,
            "gate": format!("{:?}", state.gate()),
            "splashGone": self.splash == SplashPhase::Gone,
            "devices": state.devices.len(),
            "projects": state.spaces.len(),
            "archived": state.chats.iter().filter(|chat| chat.archived).count(),
            "notificationTasks": self.notification_tasks(cx).len(),
            "selectedProject": self
                .projects_page
                .as_ref()
                .and_then(|page| page.read(cx).selected_project_id().map(str::to_owned)),
            "providerUpdates": self
                .accounts_page
                .as_ref()
                .and_then(|page| page.read(cx).provider_update_statuses())
                .map(|statuses| statuses.len()),
            "usageSnapshot": self.usage_snapshot.is_some(),
            "usageRefreshing": self.usage_refreshing,
            "usageError": self.usage_error,
            "updateLatest": update.as_ref().and_then(|status| status.latest_version.clone()),
            "updateChecking": self.update_checking,
            "updateCheckAttempted": self.update_check_attempted,
            "automationsLoading": automations.as_ref().map(|panel| panel["loading"].clone()),
            "automationsError": automations.as_ref().map(|panel| panel["error"].clone()),
            "automationRows": automations
                .as_ref()
                .and_then(|panel| panel["rows"].as_array().map(Vec::len)),
        })
    }
}
