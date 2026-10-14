//! CORE-12 shell contributions: the one registry the native shell reads to
//! place commands, settings sections, sidebar pages and task panels.
//!
//! LUNA's own entries and an enabled extension's entries share the same
//! descriptor ([`Contribution`]: a stable namespaced id, a placement, a
//! context, a title and icon). The host's own come first in every placement,
//! in their original order; extension entries follow, in the engine's order,
//! and can never replace, reorder or hide them. Protected entries (Settings
//! → Extensions, the recovery route) are host entries and always present.
//!
//! An extension entry is data from the engine (`WatchUiContributions`): no
//! GPUI entity, callback or code of the extension runs in this process. The
//! shell renders its view with its own components and asks the engine for
//! views and actions off the render thread.

use gpui::SharedString;
use luna_proto::{UiContext, UiContributionEntry, UiContributionsSnapshot, UiPlacement};

use crate::icons;
use crate::shell::{SettingsSection, SidebarNavItem, SidebarPage};

/// The prefix a saved layout uses for a contributed sidebar page.
pub const SAVED_CONTRIBUTION_PREFIX: &str = "contribution:";

/// A copyable handle for one contribution id (FNV-1a of the id), so routes
/// and rail places can name a contribution without owning its string. The
/// id itself, not this handle, is what a saved layout records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ContributionKey(u64);

impl ContributionKey {
    pub fn of(id: &str) -> Self {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in id.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
        Self(hash)
    }

    /// A stable element-id fragment.
    pub fn slug(self) -> String {
        format!("{:016x}", self.0)
    }
}

/// The host's own palette commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinCommand {
    NewTask,
    NewProject,
    OpenBoard,
    OpenSettings,
    ToggleActivity,
    ToggleTheme,
}

/// What a host entry runs: the shell's existing owner for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Builtin {
    Command(BuiltinCommand),
    Settings(SettingsSection),
    Sidebar(SidebarNavItem),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Source {
    Builtin(Builtin),
    Extension(Box<UiContributionEntry>),
}

/// One placed entry, host or extension.
#[derive(Debug, Clone, PartialEq)]
pub struct Contribution {
    pub key: ContributionKey,
    pub id: SharedString,
    pub placement: UiPlacement,
    pub title: SharedString,
    pub icon: &'static str,
    pub context: UiContext,
    /// Host recovery or permission UI: always present, never displaced.
    pub protected: bool,
    pub source: Source,
}

impl Contribution {
    fn builtin(
        id: &str,
        placement: UiPlacement,
        title: &'static str,
        icon: &'static str,
        run: Builtin,
    ) -> Self {
        let id = format!("{}/{id}", luna_proto::UI_BUILTIN_NAMESPACE);
        Self {
            key: ContributionKey::of(&id),
            protected: matches!(run, Builtin::Settings(SettingsSection::Extensions)),
            id: id.into(),
            placement,
            title: title.into(),
            icon,
            context: UiContext::None,
            source: Source::Builtin(run),
        }
    }

    fn extension(entry: &UiContributionEntry) -> Self {
        Self {
            key: ContributionKey::of(entry.id.as_str()),
            id: entry.id.as_str().to_owned().into(),
            placement: entry.placement,
            title: crate::transcript::single_line(&entry.title).into(),
            icon: icon_for(entry.icon.as_deref()),
            context: entry.context,
            protected: false,
            source: Source::Extension(Box::new(entry.clone())),
        }
    }

    pub fn host(&self) -> Option<Builtin> {
        match &self.source {
            Source::Builtin(run) => Some(*run),
            Source::Extension(_) => None,
        }
    }

    pub fn entry(&self) -> Option<&UiContributionEntry> {
        match &self.source {
            Source::Builtin(_) => None,
            Source::Extension(entry) => Some(entry),
        }
    }

    /// The accessible name. An extension entry always names its extension,
    /// so it can never be mistaken for one of the host's own controls.
    pub fn accessible_label(&self) -> SharedString {
        match &self.source {
            Source::Builtin(_) => self.title.clone(),
            Source::Extension(entry) => {
                format!("{}, from {}", self.title, entry.extension_name).into()
            }
        }
    }

    /// The settings section this entry opens (host or contributed).
    pub fn settings_section(&self) -> Option<SettingsSection> {
        match (&self.source, self.placement) {
            (Source::Builtin(Builtin::Settings(section)), _) => Some(*section),
            (Source::Extension(_), UiPlacement::SettingsSection) => {
                Some(SettingsSection::Contributed(self.key))
            }
            _ => None,
        }
    }

    /// The sidebar navigation row this entry is (host or contributed).
    pub fn sidebar_item(&self) -> Option<SidebarNavItem> {
        match (&self.source, self.placement) {
            (Source::Builtin(Builtin::Sidebar(item)), _) => Some(*item),
            (Source::Extension(_), UiPlacement::SidebarSection) => {
                Some(SidebarNavItem::Page(SidebarPage::Contributed(self.key)))
            }
            _ => None,
        }
    }
}

/// The host glyph for a declared icon name (`luna_proto::UI_ICONS`).
pub fn icon_for(name: Option<&str>) -> &'static str {
    match name {
        Some("list") => icons::LIST,
        Some("checklist") => icons::CHECKLIST,
        Some("notebook") => icons::NOTEBOOK,
        Some("info") => icons::INFO_CIRCLE,
        Some("star") => icons::STAR,
        Some("bolt") => icons::BOLT_FAST,
        Some("clipboard") => icons::CLIPBOARD_LIST,
        _ => icons::WIDGET,
    }
}

/// Whether a saved sidebar page names a contribution (`contribution:<id>`).
pub fn saved_contribution_id(saved: &str) -> Option<&str> {
    saved
        .strip_prefix(SAVED_CONTRIBUTION_PREFIX)
        .filter(|id| !id.is_empty())
}

/// The host's entries and the last snapshot of extension entries.
#[derive(Debug, Clone)]
pub struct ContributionRegistry {
    builtins: Vec<Contribution>,
    extensions: Vec<Contribution>,
    /// The applied snapshot's revision; `None` before the first one.
    revision: Option<String>,
    safe_mode: bool,
}

impl Default for ContributionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ContributionRegistry {
    /// The host's own entries, in their shipped order.
    pub fn new() -> Self {
        use BuiltinCommand as C;
        use UiPlacement::{Command, SettingsSection as Settings, SidebarSection as Sidebar};
        let mut builtins = vec![
            Contribution::builtin(
                "command.new-task",
                Command,
                "New task",
                icons::PEN_NEW_SQUARE,
                Builtin::Command(C::NewTask),
            ),
            Contribution::builtin(
                "command.new-project",
                Command,
                "New project",
                icons::FOLDER,
                Builtin::Command(C::NewProject),
            ),
            Contribution::builtin(
                "command.open-board",
                Command,
                "Open Board",
                icons::CLIPBOARD_LIST,
                Builtin::Command(C::OpenBoard),
            ),
            Contribution::builtin(
                "command.open-settings",
                Command,
                "Open settings",
                icons::SETTINGS_MINIMALISTIC,
                Builtin::Command(C::OpenSettings),
            ),
            Contribution::builtin(
                "command.toggle-activity",
                Command,
                "Toggle activity view",
                icons::BELL,
                Builtin::Command(C::ToggleActivity),
            ),
            Contribution::builtin(
                "command.toggle-theme",
                Command,
                "Switch theme",
                icons::PAINT_BRUSH,
                Builtin::Command(C::ToggleTheme),
            ),
        ];
        // Automations has its own sidebar page; Appshots is desktop only.
        builtins.extend(
            SettingsSection::ALL
                .into_iter()
                .filter(|section| *section != SettingsSection::Automations)
                .filter(|section| {
                    *section != SettingsSection::Appshots || crate::appshots::is_desktop()
                })
                .map(|section| {
                    Contribution::builtin(
                        &format!("settings.{}", section.key()),
                        Settings,
                        section.label(),
                        section.icon(),
                        Builtin::Settings(section),
                    )
                }),
        );
        builtins.extend(SidebarNavItem::ALL.into_iter().map(|item| {
            Contribution::builtin(
                &format!("sidebar.{}", item.key()),
                Sidebar,
                item.label(),
                item.icon(),
                Builtin::Sidebar(item),
            )
        }));
        Self {
            builtins,
            extensions: Vec::new(),
            revision: None,
            safe_mode: false,
        }
    }

    /// Replace the extension entries with `snapshot`'s. `true` when what is
    /// placed (or safe mode) changed. An entry whose key collides with a
    /// host entry or an earlier one is left out.
    pub fn apply(&mut self, snapshot: &UiContributionsSnapshot) -> bool {
        if self.revision.as_deref() == Some(snapshot.revision.as_str()) {
            return false;
        }
        let mut extensions: Vec<Contribution> = Vec::new();
        for entry in &snapshot.contributions {
            let contribution = Contribution::extension(entry);
            let taken = self
                .builtins
                .iter()
                .chain(extensions.iter())
                .any(|other| other.key == contribution.key);
            if taken {
                tracing::warn!(id = %contribution.id, "contribution id collides; not placed");
                continue;
            }
            extensions.push(contribution);
        }
        let changed = extensions != self.extensions || self.safe_mode != snapshot.safe_mode;
        self.extensions = extensions;
        self.safe_mode = snapshot.safe_mode;
        self.revision = Some(snapshot.revision.clone());
        changed
    }

    /// The engine went away or another one attached: the entries stay on
    /// screen until the next engine's first snapshot replaces them, but are
    /// no longer current (nothing falls back on their absence meanwhile).
    pub fn expire(&mut self) {
        self.revision = None;
    }

    /// Whether a snapshot from the current engine has been applied.
    pub fn loaded(&self) -> bool {
        self.revision.is_some()
    }

    pub fn safe_mode(&self) -> bool {
        self.safe_mode
    }

    /// Every entry of `placement`: the host's first, then extensions'.
    pub fn placed(&self, placement: UiPlacement) -> impl Iterator<Item = &Contribution> + '_ {
        self.builtins
            .iter()
            .chain(self.extensions.iter())
            .filter(move |contribution| contribution.placement == placement)
    }

    pub fn get(&self, key: ContributionKey) -> Option<&Contribution> {
        self.builtins
            .iter()
            .chain(self.extensions.iter())
            .find(|contribution| contribution.key == key)
    }

    /// The extension entry `key` of `placement`, if one is placed now.
    pub fn extension(&self, key: ContributionKey, placement: UiPlacement) -> Option<&Contribution> {
        self.extensions
            .iter()
            .find(|contribution| contribution.key == key && contribution.placement == placement)
    }

    pub fn find_id(&self, id: &str) -> Option<&Contribution> {
        self.builtins
            .iter()
            .chain(self.extensions.iter())
            .find(|contribution| contribution.id.as_ref() == id)
    }
}
