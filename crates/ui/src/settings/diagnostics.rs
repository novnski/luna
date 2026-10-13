//! Settings → Diagnostics (AW-09). Read-only: each section reads one owner
//! that already exists, and the page sends no command.
//!
//! - *This app and engine*: the app version (`luna-update`), the attached
//!   engine's identity (`EngineInfo`), the viewport connection and edge
//!   posture (`AppState`), and the engine process (`GetEngineDiagnostics`).
//! - *Providers*: `ListHarnesses` and the cached `GetProviderUpdates` check;
//!   updates stay on Settings → Providers.
//! - *Extensions*: the CORE-07 `ListExtensionInstances` readback; management
//!   stays on Settings → Extensions.
//! - *Problems*: failures those owners already report, in one list.
//! - *Storage*: `GetStorageUsage`, a bounded read-only walk on the engine.
//!   There is no cleanup control: removing data is a separate, reviewed
//!   action.
//!
//! Every section loads on its own, so one failing owner never hides the
//! others. A refresh drops every reply still in flight, and replies are
//! fenced by the engine generation they were requested for.

use gpui::{AnyElement, App, Context, Entity, SharedString, Task, Window, div, prelude::*};
use luna_engine::registry::{HarnessDescriptor, descriptor_enabled};
use luna_proto::{
    ConnectivityState, EngineDiagnostics, EngineResources, ExtensionInstanceState,
    ExtensionInstancesSnapshot, HarnessId, ProviderUpdateStatus, StorageCategory,
    StorageCategoryId, StorageCategoryState, StorageExclusionReason, StorageScanStop, StorageScope,
    StorageUsage, WorkspaceScope,
};
use luna_rpc::methods;
use serde::de::DeserializeOwned;

use super::widgets;
use crate::button::ControlSize;
use crate::icons;
use crate::popover::{self, Loadable};
use crate::state::{AppState, ConnectionStatus, EngineHandle, EngineMode};
use crate::theme::Theme;
use crate::typography::ui_rems;

/// Running extension workers listed by name; the rest are counted.
const MAX_INSTANCE_ROWS: usize = 8;

/// `GetDeletionCleanupSummary` (the sidebar notice's source).
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct CleanupSummary {
    #[serde(default)]
    pending: u64,
    #[serde(default)]
    failed: u64,
    #[serde(default)]
    first_failure: Option<String>,
}

pub struct DiagnosticsPage {
    state: Entity<AppState>,
    scroll: widgets::PageScroll,
    /// The engine generation every section below was requested for.
    generation: u64,
    engine: Loadable<EngineDiagnostics>,
    harnesses: Loadable<Vec<HarnessDescriptor>>,
    updates: Loadable<Vec<ProviderUpdateStatus>>,
    extensions: Loadable<ExtensionInstancesSnapshot>,
    cleanup: Loadable<CleanupSummary>,
    storage: Loadable<StorageUsage>,
    /// The loads in flight; dropping one cancels it.
    tasks: Vec<Task<()>>,
    _engine_watch: gpui::Subscription,
}

impl DiagnosticsPage {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        // A replaced or re-attached engine is read again at once; replies
        // still due from its predecessor are dropped by the generation fence.
        let engine_watch = cx.observe(&state, |page: &mut Self, state, cx| {
            if state.read(cx).engine_generation() != page.generation {
                page.refresh(cx);
            }
        });
        let mut page = Self {
            _engine_watch: engine_watch,
            state,
            scroll: widgets::PageScroll::default(),
            generation: 0,
            engine: Loadable::Idle,
            harnesses: Loadable::Idle,
            updates: Loadable::Idle,
            extensions: Loadable::Idle,
            cleanup: Loadable::Idle,
            storage: Loadable::Idle,
            tasks: Vec::new(),
        };
        page.refresh(cx);
        page
    }

    /// Read every section again from its owner. A section that already shows
    /// this engine's value keeps it on screen until the new one lands.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let (generation, engine) = {
            let state = self.state.read(cx);
            (state.engine_generation(), state.engine().cloned())
        };
        let same_engine = generation == self.generation;
        self.generation = generation;
        self.tasks.clear();
        let Some(engine) = engine else {
            fn offline<T>() -> Loadable<T> {
                Loadable::Error("Engine not connected".into())
            }
            self.engine = offline();
            self.harnesses = offline();
            self.updates = offline();
            self.extensions = offline();
            self.cleanup = offline();
            self.storage = offline();
            cx.notify();
            return;
        };
        begin(&mut self.engine, same_engine);
        begin(&mut self.harnesses, same_engine);
        begin(&mut self.updates, same_engine);
        begin(&mut self.extensions, same_engine);
        begin(&mut self.cleanup, same_engine);
        begin(&mut self.storage, same_engine);
        let none = serde_json::json!({});
        self.load(
            &engine,
            methods::GET_ENGINE_DIAGNOSTICS,
            none.clone(),
            |page, value| page.engine = value,
            cx,
        );
        self.load(
            &engine,
            methods::LIST_HARNESSES,
            none.clone(),
            |page, value| page.harnesses = value,
            cx,
        );
        // The engine's cached check; Settings → Providers owns forced checks.
        self.load(
            &engine,
            methods::GET_PROVIDER_UPDATES,
            serde_json::json!({ "force": false }),
            |page, value| page.updates = value,
            cx,
        );
        self.load(
            &engine,
            methods::LIST_EXTENSION_INSTANCES,
            none.clone(),
            |page, value| page.extensions = value,
            cx,
        );
        self.load(
            &engine,
            methods::GET_DELETION_CLEANUP_SUMMARY,
            none.clone(),
            |page, value| page.cleanup = value,
            cx,
        );
        self.load(
            &engine,
            methods::GET_STORAGE_USAGE,
            none,
            |page, value| page.storage = value,
            cx,
        );
        cx.notify();
    }

    fn load<T: DeserializeOwned + 'static>(
        &mut self,
        engine: &EngineHandle,
        method: &'static str,
        params: serde_json::Value,
        apply: impl FnOnce(&mut Self, Loadable<T>) + 'static,
        cx: &mut Context<Self>,
    ) {
        let generation = self.generation;
        let engine = engine.clone();
        self.tasks.push(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(method, params)
                .await
                .map_err(|error| error.to_string())
                .and_then(|value| {
                    serde_json::from_value::<T>(value).map_err(|error| error.to_string())
                });
            this.update(cx, |page, cx| {
                // A reply for an engine this page no longer shows.
                if page.generation != generation {
                    return;
                }
                apply(
                    page,
                    match result {
                        Ok(value) => Loadable::Ready(value),
                        Err(error) => Loadable::Error(error),
                    },
                );
                cx.notify();
            })
            .ok();
        }));
    }

    fn on_scroll_hovered(&mut self, hovered: &bool, _: &mut Window, cx: &mut Context<Self>) {
        if self.scroll.set_list_hovered(*hovered) {
            cx.notify();
        }
    }
}

/// Show loading unless the slot already holds this engine's value.
fn begin<T>(slot: &mut Loadable<T>, same_engine: bool) {
    if !(same_engine && slot.ready().is_some()) {
        *slot = Loadable::Loading;
    }
}

impl popover::ScrollRailHost for DiagnosticsPage {
    fn rail_bar(&mut self) -> &mut popover::MenuScrollbarState {
        self.scroll.rail_bar()
    }
    fn rail_scroll(&self) -> Option<gpui::ScrollHandle> {
        self.scroll.rail_scroll()
    }
}

// ---------------------------------------------------------------------------
// What the attached state says (read once per render)
// ---------------------------------------------------------------------------

struct AppFacts {
    app_version: &'static str,
    placement: String,
    profile: String,
    connection: String,
    connection_failure: Option<String>,
    sync: Option<String>,
    sync_failure: Option<String>,
}

impl AppFacts {
    fn read(state: &Entity<AppState>, cx: &App) -> Self {
        let state = state.read(cx);
        let engine = state.engine();
        let placement = match engine.map(EngineHandle::mode) {
            Some(EngineMode::InProcess) => "Runs inside this app".to_owned(),
            Some(EngineMode::Remote { url }) => format!("Separate engine at {}", endpoint(&url)),
            None => "No engine attached".to_owned(),
        };
        let profile = match engine.map(EngineHandle::engine_info) {
            Some(info) => format!(
                "{} profile · device {}",
                scope_label(info.workspace_scope),
                short_id(&info.device_id)
            ),
            None => "Unknown".to_owned(),
        };
        let (connection, connection_failure) = match &state.connection {
            ConnectionStatus::Ready => ("Connected".to_owned(), None),
            ConnectionStatus::Connecting => ("Connecting…".to_owned(), None),
            ConnectionStatus::Failed(error) => (format!("Failed: {error}"), Some(error.clone())),
        };
        let connectivity = &state.connectivity;
        let sync = match connectivity.state {
            ConnectivityState::Disabled => None,
            ConnectivityState::Connected => Some("Connected".to_owned()),
            ConnectivityState::Offline => Some("Offline: no network path".to_owned()),
            ConnectivityState::Reconnecting => Some("Reconnecting".to_owned()),
        };
        let sync_failure = matches!(
            connectivity.state,
            ConnectivityState::Offline | ConnectivityState::Reconnecting
        )
        .then(|| connectivity.last_failure.clone())
        .flatten();
        Self {
            app_version: luna_update::current_version(),
            placement,
            profile,
            connection,
            connection_failure,
            sync,
            sync_failure,
        }
    }
}

/// `host:port` of an engine URL, never its path, query or credentials.
fn endpoint(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(parsed) => match (parsed.host_str(), parsed.port()) {
            (Some(host), Some(port)) => format!("{host}:{port}"),
            (Some(host), None) => host.to_owned(),
            _ => parsed.scheme().to_owned(),
        },
        Err(_) => "an unknown address".to_owned(),
    }
}

fn short_id(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

fn scope_label(scope: WorkspaceScope) -> &'static str {
    match scope {
        WorkspaceScope::Local => "Local",
        WorkspaceScope::Synced => "Synced",
        WorkspaceScope::Development => "Development",
    }
}

// ---------------------------------------------------------------------------
// Formatting (pure)
// ---------------------------------------------------------------------------

/// Byte count in the largest whole unit (`512 B`, `3.4 MB`).
fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// `2h 5m`, `4m 12s`, `9s`.
fn format_duration(ms: u64) -> String {
    let total = ms / 1000;
    let (days, hours, minutes, seconds) = (
        total / 86_400,
        (total / 3600) % 24,
        (total / 60) % 60,
        total % 60,
    );
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else if minutes > 0 {
        format!("{minutes}m {seconds}s")
    } else {
        format!("{seconds}s")
    }
}

fn resources_text(resources: &EngineResources) -> String {
    let mut parts = Vec::new();
    if let Some(bytes) = resources.resident_bytes {
        parts.push(format!("{} resident", format_bytes(bytes)));
    }
    if let (Some(user), Some(system)) = (resources.cpu_user_ms, resources.cpu_system_ms) {
        parts.push(format!(
            "{} CPU",
            format_duration(user.saturating_add(system))
        ));
    }
    if let Some(threads) = resources.threads {
        parts.push(format!("{threads} threads"));
    }
    if parts.is_empty() {
        "Not available on this platform".to_owned()
    } else {
        parts.join(" · ")
    }
}

/// The stable key (for element ids) and the label of a storage category.
fn category_text(id: StorageCategoryId) -> (&'static str, &'static str) {
    use StorageCategoryId::*;
    match id {
        WorkspaceDatabase => ("workspaceDatabase", "Workspace database"),
        RunJournals => ("runJournals", "Run journals"),
        Attachments => ("attachments", "Attachments"),
        TerminalHistory => ("terminalHistory", "Terminal history"),
        ExtensionData => ("extensionData", "Extension data"),
        ProfileState => ("profileState", "Other profile state"),
        TurnSnapshots => ("turnSnapshots", "Turn snapshots"),
        ModelCache => ("modelCache", "Model catalogs"),
        UsageCache => ("usageCache", "Usage scans"),
        ExtensionPackages => ("extensionPackages", "Extension packages"),
        Logs => ("logs", "Logs"),
        Repositories => ("repositories", "Cloned and created repositories"),
        UpdateDownloads => ("updateDownloads", "Update downloads"),
        LegacyAttachments => ("legacyAttachments", "Older attachments"),
        BackupStaging => ("backupStaging", "Backup and restore staging"),
        DeviceSettings => ("deviceSettings", "Settings, accounts and identity"),
        Unrecognized => ("unrecognized", "Unrecognized items"),
        Backups => ("backups", "Backup archives"),
        Worktrees => ("worktrees", "Managed worktrees"),
        Unknown => ("unknown", "Other"),
    }
}

/// The description line of one storage row.
fn category_description(category: &StorageCategory) -> String {
    let mut parts = Vec::new();
    if !category.location.is_empty() {
        parts.push(category.location.clone());
    }
    if !category.items.is_empty() {
        parts.push(category.items.join(", "));
    }
    parts.push(match category.files {
        1 => "1 file".to_owned(),
        files => format!("{files} files"),
    });
    match category.scope {
        StorageScope::Profile => parts.push("this profile".to_owned()),
        StorageScope::Shared => parts.push("shared with other LUNA data folders".to_owned()),
        StorageScope::Device | StorageScope::Unknown => {}
    }
    if category.state == StorageCategoryState::NotScanned {
        parts.push("not measured: the walk stopped first".to_owned());
    }
    parts.join(" · ")
}

fn stop_text(stop: Option<StorageScanStop>) -> &'static str {
    match stop {
        Some(StorageScanStop::Deadline) => "its time budget",
        Some(StorageScanStop::EntryLimit) => "its item budget",
        Some(StorageScanStop::Unknown) | None => "a budget",
    }
}

/// The runtime's own spelling of an instance state (`running`, `crashed`).
fn instance_state_label(state: ExtensionInstanceState) -> String {
    serde_json::to_value(state)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_owned())
}

/// Every failure the owners report, as `(source, message)`.
fn problems(
    facts: &AppFacts,
    cleanup: &Loadable<CleanupSummary>,
    extensions: &Loadable<ExtensionInstancesSnapshot>,
    storage: &Loadable<StorageUsage>,
) -> Vec<(&'static str, String)> {
    let mut found = Vec::new();
    if let Some(error) = &facts.connection_failure {
        found.push(("Engine connection", error.clone()));
    }
    if let Some(error) = &facts.sync_failure {
        found.push(("Sync", error.clone()));
    }
    match cleanup {
        Loadable::Ready(summary) if summary.failed > 0 => found.push((
            "Deletion cleanup",
            format!(
                "{} failed, {} pending. {}",
                summary.failed,
                summary.pending,
                summary
                    .first_failure
                    .as_deref()
                    .unwrap_or("Review the local cleanup status.")
            ),
        )),
        Loadable::Error(error) => {
            found.push(("Deletion cleanup", format!("Status unavailable: {error}")))
        }
        _ => {}
    }
    if let Loadable::Ready(snapshot) = extensions {
        if let Some(error) = &snapshot.state_error {
            found.push(("Extensions", error.clone()));
        }
        for quarantined in &snapshot.quarantined {
            found.push((
                "Extensions",
                format!(
                    "{} {} is quarantined: {}",
                    quarantined.extension_id.as_str(),
                    quarantined.version,
                    quarantined.reason
                ),
            ));
        }
        for instance in snapshot.instances.iter().filter(|instance| {
            matches!(
                instance.state,
                ExtensionInstanceState::Crashed | ExtensionInstanceState::Failed
            )
        }) {
            let detail = instance
                .last_exit
                .as_ref()
                .and_then(|exit| exit.detail.clone())
                .unwrap_or_else(|| "no detail recorded".to_owned());
            found.push((
                "Extensions",
                format!(
                    "{} {} {}: {detail}",
                    instance.extension_id.as_str(),
                    instance.version,
                    instance_state_label(instance.state)
                ),
            ));
        }
    }
    if let Loadable::Ready(usage) = storage {
        for category in usage
            .categories
            .iter()
            .filter(|category| category.unreadable > 0)
        {
            found.push((
                "Storage",
                format!(
                    "{}: {} item{} couldn't be read ({})",
                    category_text(category.id).1,
                    category.unreadable,
                    if category.unreadable == 1 { "" } else { "s" },
                    category.first_error.as_deref().unwrap_or("unknown error")
                ),
            ));
        }
    }
    found
}

// ---------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------

fn info_row(
    theme: &Theme,
    title: impl Into<SharedString>,
    value: impl Into<SharedString>,
) -> gpui::Div {
    widgets::card_row(theme, false).child(widgets::row_text(theme, title, Some(value.into())))
}

/// A row whose only content is a status strip, inside its section.
fn strip_row(theme: &Theme, strip: gpui::Div) -> gpui::Div {
    widgets::card_row(theme, false).child(strip.mt(ui_rems(0.0)).w_full())
}

fn loading_row(
    theme: &Theme,
    selector: &'static str,
    rows: usize,
    cx: &mut Context<DiagnosticsPage>,
) -> gpui::Div {
    let skeleton = popover::skeleton_rows(selector, theme, rows, cx.entity_id(), cx);
    widgets::card_row(theme, false).child(
        div()
            .w_full()
            .debug_selector(move || selector.into())
            .child(skeleton),
    )
}

fn size_text(theme: &Theme, text: String) -> gpui::Div {
    widgets::row_controls().child(
        div()
            .text_size(ui_rems(13.0))
            .text_color(theme.text)
            .child(SharedString::from(text)),
    )
}

impl DiagnosticsPage {
    fn engine_card(&self, theme: &Theme, facts: &AppFacts, cx: &mut Context<Self>) -> gpui::Div {
        let version = match self
            .engine
            .ready()
            .map(|engine| engine.engine_version.as_str())
        {
            Some(engine) if engine != facts.app_version => {
                format!("LUNA {} · engine {engine}", facts.app_version)
            }
            _ => format!("LUNA {}", facts.app_version),
        };
        let mut card = widgets::section_card(theme)
            .child(widgets::section_header(
                theme,
                "This app and engine",
                Some("Versions, the engine process and its connection.".into()),
                Vec::new(),
            ))
            .child(info_row(theme, "Version", version));
        card = match &self.engine {
            Loadable::Idle | Loadable::Loading => {
                card.child(loading_row(theme, "diag-engine-loading", 2, cx))
            }
            Loadable::Error(error) => card.child(
                strip_row(
                    theme,
                    widgets::error_strip(
                        theme,
                        SharedString::from(format!("Couldn't read the engine process: {error}")),
                    ),
                )
                .debug_selector(|| "diag-engine-error".into()),
            ),
            Loadable::Ready(engine) => card
                .child(
                    info_row(
                        theme,
                        "Engine process",
                        format!(
                            "{} · pid {} · up {} · {} {}",
                            facts.placement,
                            engine.pid,
                            format_duration(engine.uptime_ms),
                            engine.os,
                            engine.arch
                        ),
                    )
                    .debug_selector(|| "diag-engine-ready".into()),
                )
                .child(info_row(
                    theme,
                    "Resources",
                    resources_text(&engine.resources),
                )),
        };
        card.child(info_row(theme, "Profile", facts.profile.clone()))
            .child(info_row(theme, "Connection", facts.connection.clone()))
            .when_some(facts.sync.clone(), |card, sync| {
                card.child(info_row(theme, "Sync", sync))
            })
    }

    fn providers_card(&self, theme: &Theme, cx: &mut Context<Self>) -> gpui::Div {
        let card = widgets::section_card(theme).child(widgets::section_header(
            theme,
            "Providers",
            Some(
                "CLI availability and versions on this device. Manage them in Settings → Providers."
                    .into(),
            ),
            Vec::new(),
        ));
        let list = match &self.harnesses {
            Loadable::Idle | Loadable::Loading => {
                return card.child(loading_row(theme, "diag-providers-loading", 2, cx));
            }
            Loadable::Error(error) => {
                return card.child(
                    strip_row(
                        theme,
                        widgets::error_strip(
                            theme,
                            SharedString::from(format!("Couldn't list providers: {error}")),
                        ),
                    )
                    .debug_selector(|| "diag-providers-error".into()),
                );
            }
            Loadable::Ready(list) => list,
        };
        let updates = self.updates.ready();
        let rows: Vec<AnyElement> = list
            .iter()
            .filter(|descriptor| descriptor.id != HarnessId::Mock)
            .map(|descriptor| {
                let status = updates.and_then(|statuses| {
                    statuses
                        .iter()
                        .find(|status| status.harness == descriptor.id)
                });
                let mut parts = vec![
                    if descriptor.installed {
                        "Installed"
                    } else {
                        "Not installed"
                    }
                    .to_owned(),
                    if descriptor_enabled(descriptor) {
                        "enabled"
                    } else {
                        "off"
                    }
                    .to_owned(),
                ];
                if let Some(status) = status {
                    if let Some(version) = &status.installed_version {
                        parts.push(format!("v{version}"));
                    }
                    if status.update_available
                        && let Some(latest) = &status.latest_version
                    {
                        parts.push(format!("v{latest} available"));
                    }
                    if let Some(detail) = &status.detail {
                        parts.push(detail.clone());
                    }
                }
                info_row(theme, descriptor.name.clone(), parts.join(" · ")).into_any_element()
            })
            .collect();
        let card = card.child(
            div()
                .flex()
                .flex_col()
                .debug_selector(|| "diag-providers-ready".into())
                .children(rows),
        );
        match &self.updates {
            Loadable::Error(error) => card.child(
                strip_row(
                    theme,
                    widgets::warning_strip(
                        theme,
                        SharedString::from(format!("Version check unavailable: {error}")),
                    ),
                )
                .debug_selector(|| "diag-provider-versions-error".into()),
            ),
            Loadable::Idle | Loadable::Loading => {
                card.child(info_row(theme, "Versions", "Checking installed versions…"))
            }
            Loadable::Ready(_) => card,
        }
    }

    fn extensions_card(&self, theme: &Theme, cx: &mut Context<Self>) -> gpui::Div {
        let card = widgets::section_card(theme).child(widgets::section_header(
            theme,
            "Extensions",
            Some(
                "Extension workers and their resource use. Manage them in Settings → Extensions."
                    .into(),
            ),
            Vec::new(),
        ));
        let snapshot = match &self.extensions {
            Loadable::Idle | Loadable::Loading => {
                return card.child(loading_row(theme, "diag-extensions-loading", 1, cx));
            }
            // Unavailable is an ordinary answer (a remote engine, a profile
            // whose extension store failed to open), not a page failure.
            Loadable::Error(error) => {
                return card.child(
                    info_row(theme, "Unavailable", error.clone())
                        .debug_selector(|| "diag-extensions-unavailable".into()),
                );
            }
            Loadable::Ready(snapshot) => snapshot,
        };
        let live = |instance: &&luna_proto::ExtensionInstanceView| {
            matches!(
                instance.state,
                ExtensionInstanceState::Starting
                    | ExtensionInstanceState::Running
                    | ExtensionInstanceState::Draining
            )
        };
        let running = snapshot.instances.iter().filter(live).count();
        let resident: u64 = snapshot
            .instances
            .iter()
            .filter(live)
            .filter_map(|instance| instance.usage.resident_bytes)
            .sum();
        let mut summary = vec![format!(
            "{running} running · {} recorded · {} quarantined",
            snapshot.instances.len(),
            snapshot.quarantined.len()
        )];
        if resident > 0 {
            summary.push(format!("{} resident", format_bytes(resident)));
        }
        if snapshot.safe_mode {
            summary.push("safe mode is on".to_owned());
        }
        let rows: Vec<AnyElement> = snapshot
            .instances
            .iter()
            .filter(live)
            .take(MAX_INSTANCE_ROWS)
            .map(|instance| {
                let mut parts = vec![instance_state_label(instance.state)];
                if let Some(pid) = instance.pid {
                    parts.push(format!("pid {pid}"));
                }
                if let Some(bytes) = instance.usage.resident_bytes {
                    parts.push(format!("{} resident", format_bytes(bytes)));
                }
                parts.push(format!(
                    "{} restarts · {} faults",
                    instance.usage.restarts, instance.usage.faults
                ));
                info_row(
                    theme,
                    format!("{} {}", instance.extension_id.as_str(), instance.version),
                    parts.join(" · "),
                )
                .into_any_element()
            })
            .collect();
        card.child(
            div()
                .flex()
                .flex_col()
                .debug_selector(|| "diag-extensions-ready".into())
                .child(info_row(theme, "Workers", summary.join(" · ")))
                .children(rows),
        )
    }

    fn problems_card(&self, theme: &Theme, facts: &AppFacts) -> gpui::Div {
        let found = problems(facts, &self.cleanup, &self.extensions, &self.storage);
        let pending = [
            self.cleanup.is_loading(),
            self.extensions.is_loading(),
            self.storage.is_loading(),
        ]
        .into_iter()
        .filter(|loading| *loading)
        .count();
        let description = match (found.len(), pending) {
            (0, 0) => "Failures LUNA's services report.".to_owned(),
            (_, 0) => format!("{} reported.", found.len()),
            (_, pending) => format!("Still reading {pending} of LUNA's services…"),
        };
        let card = widgets::section_card(theme).child(widgets::section_header(
            theme,
            "Problems",
            Some(description.into()),
            Vec::new(),
        ));
        if found.is_empty() {
            return card.child(
                info_row(
                    theme,
                    "No problems reported",
                    "Nothing needs attention right now.",
                )
                .debug_selector(|| "diag-problems-none".into()),
            );
        }
        card.child(
            div()
                .flex()
                .flex_col()
                .debug_selector(|| "diag-problems".into())
                .children(
                    found
                        .into_iter()
                        .map(|(source, message)| info_row(theme, source, message)),
                ),
        )
    }

    fn storage_card(&self, theme: &Theme, cx: &mut Context<Self>) -> gpui::Div {
        let usage = match &self.storage {
            Loadable::Idle | Loadable::Loading => {
                return widgets::section_card(theme)
                    .child(widgets::section_header(
                        theme,
                        "Storage",
                        Some("Measuring what LUNA keeps on disk…".into()),
                        Vec::new(),
                    ))
                    .child(loading_row(theme, "diag-storage-loading", 3, cx));
            }
            Loadable::Error(error) => {
                return widgets::section_card(theme)
                    .child(widgets::section_header(theme, "Storage", None, Vec::new()))
                    .child(
                        strip_row(
                            theme,
                            widgets::error_strip(
                                theme,
                                SharedString::from(format!("Couldn't measure storage: {error}")),
                            ),
                        )
                        .debug_selector(|| "diag-storage-error".into()),
                    );
            }
            Loadable::Ready(usage) => usage,
        };
        let total = if usage.complete {
            format_bytes(usage.total_bytes)
        } else {
            format!("At least {}", format_bytes(usage.total_bytes))
        };
        let badge: Vec<AnyElement> = if usage.complete {
            Vec::new()
        } else {
            vec![widgets::badge(theme, "Partial").into_any_element()]
        };
        let mut card = widgets::section_card(theme).child(widgets::section_header(
            theme,
            "Storage",
            Some(
                format!(
                    "{total} in {} · {} items measured in {}",
                    usage.root,
                    usage.entries,
                    format_duration(usage.elapsed_ms)
                )
                .into(),
            ),
            badge,
        ));
        if !usage.complete {
            card = card.child(
                strip_row(
                    theme,
                    widgets::warning_strip(
                        theme,
                        SharedString::from(format!(
                            "The walk stopped at {}; these sizes are lower bounds.",
                            stop_text(usage.stop)
                        )),
                    ),
                )
                .debug_selector(|| "diag-storage-partial".into()),
            );
        }
        let mut missing = Vec::new();
        let mut rows: Vec<AnyElement> = Vec::new();
        for category in &usage.categories {
            let (key, label) = category_text(category.id);
            match category.state {
                StorageCategoryState::Missing => {
                    missing.push(label);
                    continue;
                }
                StorageCategoryState::Unknown => continue,
                _ => {}
            }
            let size = match category.state {
                StorageCategoryState::NotScanned => "—".to_owned(),
                StorageCategoryState::Partial if category.unreadable == 0 => {
                    format!("≥ {}", format_bytes(category.bytes))
                }
                _ => format_bytes(category.bytes),
            };
            let text = widgets::row_text(theme, label, None)
                .child(widgets::row_description(
                    theme,
                    category_description(category),
                ))
                .when(category.unreadable > 0, |text| {
                    text.child(
                        widgets::row_description(
                            theme,
                            format!(
                                "{} item{} couldn't be read ({}); the size leaves {} out.",
                                category.unreadable,
                                if category.unreadable == 1 { "" } else { "s" },
                                category.first_error.as_deref().unwrap_or("unknown error"),
                                if category.unreadable == 1 {
                                    "it"
                                } else {
                                    "them"
                                },
                            ),
                        )
                        .debug_selector(move || format!("diag-storage-unreadable-{key}")),
                    )
                });
            rows.push(
                widgets::card_row(theme, false)
                    .debug_selector(move || format!("diag-storage-row-{key}"))
                    .child(text)
                    .child(size_text(theme, size))
                    .into_any_element(),
            );
        }
        card = card.child(
            div()
                .flex()
                .flex_col()
                .debug_selector(|| "diag-storage-ready".into())
                .children(rows),
        );
        if !missing.is_empty() {
            card = card.child(info_row(theme, "Not created yet", missing.join(", ")));
        }
        for exclusion in &usage.excluded {
            if exclusion.reason == StorageExclusionReason::OtherProfile {
                card = card.child(info_row(
                    theme,
                    "Other profiles",
                    format!(
                        "{} folder{} of other LUNA profiles in this data folder, not measured.",
                        exclusion.count,
                        if exclusion.count == 1 { "" } else { "s" }
                    ),
                ));
            }
        }
        // No cleanup control by design (AW-09): removing data needs its own
        // reviewed preview, ownership and active-use checks.
        card.child(
            info_row(
                theme,
                "Cleanup",
                "Not available here. Removing LUNA data is a separate, reviewed action; this page only reads.",
            )
            .debug_selector(|| "diag-cleanup-note".into()),
        )
    }
}

impl Render for DiagnosticsPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let facts = AppFacts::read(&self.state, cx);
        let refresh = crate::button::icon_button(
            "diagnostics-refresh",
            icons::REFRESH,
            ControlSize::Md,
            true,
            &theme,
        )
        .aria_label("Refresh diagnostics")
        .tooltip(popover::text_tooltip("Refresh diagnostics"))
        .on_click(cx.listener(|page, _, _, cx| page.refresh(cx)));
        let engine_card = self.engine_card(&theme, &facts, cx);
        let providers_card = self.providers_card(&theme, cx);
        let extensions_card = self.extensions_card(&theme, cx);
        let problems_card = self.problems_card(&theme, &facts);
        let storage_card = self.storage_card(&theme, cx);
        let scroll_bar = popover::rail(self, "diagnostics-page-scrollbar", &theme, cx);
        div()
            .id("diagnostics-page-host")
            .relative()
            .size_full()
            .on_hover(cx.listener(Self::on_scroll_hovered))
            .child(
                div()
                    .id("diagnostics-page")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll.scroll)
                    .child(
                        widgets::page_column()
                            .child(widgets::page_title_bar(
                                &theme,
                                "Diagnostics",
                                None,
                                vec![refresh.into_any_element()],
                            ))
                            .child(widgets::page_subtitle(
                                &theme,
                                "Read-only status from LUNA's own services. Nothing here changes, stops or deletes anything.",
                            ))
                            .child(engine_card)
                            .child(providers_card)
                            .child(extensions_card)
                            .child(problems_card)
                            .child(storage_card),
                    ),
            )
            .children(scroll_bar)
    }
}

#[cfg(test)]
#[path = "diagnostics_tests.rs"]
mod tests;
