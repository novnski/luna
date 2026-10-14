//! CORE-12 UI contributions: an enabled package's `command`,
//! `settingsSection`, `sidebarSection` and `panel` contributions, read from
//! its hash-pinned `luna-ui.json` without running any code, validated, and
//! published to the native shell as data.
//!
//! The registry has no state of its own: every read derives it from the
//! installed index, this profile's grants file and safe mode, so a restart,
//! an update, a rollback or a removal can never leave a stale entry behind.
//! A change of what is enabled bumps a watch counter, and
//! `WatchUiContributions` re-reads and publishes a new snapshot when its
//! revision changed.
//!
//! Views and actions name the exact package hash the caller rendered from:
//! another (or no) package providing the id is `stale` and nothing runs. A
//! `task` contribution names its exact task, which must be local with a
//! checkout (the CORE-07 target rule). A worker view or action starts one
//! supervised instance through `StartExtensionInstance` (so safe mode,
//! quarantine, re-verification and the bounds all apply), calls one
//! required method with a deadline, and stops it. Worker output is
//! validated against the same bounds as a static view before the shell
//! sees it. Native workers are user-level code without an OS sandbox
//! (CORE-07); the entries carry their trust class.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use futures::StreamExt as _;
use futures::stream::BoxStream;
use luna_proto::contract::ContributionId;
use luna_proto::{
    CallExtensionInstanceRequest, ExtensionCallStatus, ExtensionRuntimeKind,
    GetUiContributionViewRequest, InvokeUiActionRequest, StartExtensionInstanceRequest,
    UI_ACTION_TIMEOUT_MS, UI_DECLARATION_FILE, UI_DECLARATION_VERSION, UI_ICONS, UI_MAX_ACTIONS,
    UI_MAX_CONTRIBUTIONS, UI_MAX_DECLARATION_BYTES, UI_SETTINGS_SECTIONS, UI_VIEW_TIMEOUT_MS,
    UiActionEntry, UiActionKind, UiActionOutcome, UiActionRun, UiContext,
    UiContributionDeclaration, UiContributionEntry, UiContributionRefusal, UiContributionsSnapshot,
    UiDeclaration, UiHostAction, UiOutcomeStatus, UiPlacement, UiView, UiViewKind, UiViewOutcome,
    UiViewSource, UiWorkerActionResult, check_ui_id, check_ui_message, check_ui_title,
};
use serde_json::{Value, json};
use tokio::sync::watch;

use super::manifest::{ValidatedManifest, sha256_hex};
use super::{ExtensionManager, store};

/// Order of an entry that declares none, and the largest one accepted.
const DEFAULT_ORDER: i32 = 500;
const MAX_ORDER: i32 = 1000;

/// Change notifications for `WatchUiContributions`, owned by the manager.
pub(super) struct UiHost {
    changes: watch::Sender<u64>,
}

impl Default for UiHost {
    fn default() -> Self {
        Self {
            changes: watch::channel(0).0,
        }
    }
}

/// One validated contribution of the enabled package that provides it.
struct Provided {
    entry: UiContributionEntry,
    declaration: UiContributionDeclaration,
}

/// Why a view or action did not run: its outcome status and the reason.
type Refusal = (UiOutcomeStatus, String);

impl ExtensionManager {
    /// Something that decides which UI contributions exist may have
    /// changed (enablement, a version switch, a removal, safe mode). Cheap;
    /// watchers re-read and publish only a changed revision.
    pub(super) fn ui_changed(&self) {
        self.0
            .ui
            .changes
            .send_modify(|generation| *generation = generation.wrapping_add(1));
    }

    /// `ListUiContributions`. Blocking file reads; never starts anything.
    pub(crate) fn ui_snapshot(&self) -> UiContributionsSnapshot {
        let (provided, refused, safe_mode) = self.ui_read();
        let contributions: Vec<UiContributionEntry> = provided
            .into_iter()
            .map(|provided| provided.entry)
            .collect();
        let revision = serde_json::to_vec(&json!({
            "safeMode": safe_mode,
            "contributions": contributions,
            "refused": refused,
        }))
        .map(|bytes| sha256_hex(&bytes))
        .unwrap_or_default();
        UiContributionsSnapshot {
            revision,
            safe_mode,
            contributions,
            refused,
        }
    }

    /// `WatchUiContributions`: the current snapshot, then a new one each
    /// time its revision changes. Holds no strong reference to the manager,
    /// so the stream ends once the engine is gone.
    pub(crate) fn ui_changes(&self) -> BoxStream<'static, UiContributionsSnapshot> {
        let weak = Arc::downgrade(&self.0);
        let changes = self.0.ui.changes.subscribe();
        futures::stream::unfold(
            (weak, changes, None::<String>),
            |(weak, mut changes, last)| async move {
                loop {
                    if last.is_some() && changes.changed().await.is_err() {
                        return None;
                    }
                    let manager = ExtensionManager(weak.upgrade()?);
                    let snapshot = tokio::task::spawn_blocking(move || manager.ui_snapshot())
                        .await
                        .ok()?;
                    if last.as_deref() == Some(snapshot.revision.as_str()) {
                        continue;
                    }
                    let revision = snapshot.revision.clone();
                    return Some((snapshot, (weak, changes, Some(revision))));
                }
            },
        )
        .boxed()
    }

    /// `GetUiContributionView`: the static view, or the worker's, validated.
    pub(crate) async fn ui_view(
        &self,
        request: GetUiContributionViewRequest,
    ) -> Result<UiViewOutcome, String> {
        self.available()?;
        let mut outcome = UiViewOutcome {
            contribution_id: request.contribution_id.clone(),
            package_hash: request.package_hash.clone(),
            task_id: request.task_id.clone(),
            status: UiOutcomeStatus::Ok,
            view: None,
            message: None,
        };
        let resolved = {
            let manager = self.clone();
            let request = request.clone();
            super::blocking(move || {
                Ok(manager.ui_resolve(
                    &request.contribution_id,
                    &request.package_hash,
                    request.task_id.as_deref(),
                ))
            })
            .await?
        };
        let provided = match resolved {
            Ok(provided) => provided,
            Err((status, message)) => {
                outcome.status = status;
                outcome.message = Some(message);
                return Ok(outcome);
            }
        };
        let result = match &provided.declaration.view {
            None => Err((
                UiOutcomeStatus::Refused,
                format!("{} has no view", request.contribution_id.as_str()),
            )),
            Some(UiViewSource::Static(view)) => Ok(view.clone()),
            Some(UiViewSource::Worker(call)) => {
                let params = json!({
                    "contributionId": request.contribution_id,
                    "taskId": request.task_id,
                });
                match self
                    .ui_worker_call(
                        &provided,
                        &call.method,
                        params,
                        request.task_id.as_deref(),
                        UI_VIEW_TIMEOUT_MS,
                        None,
                    )
                    .await
                {
                    Ok(value) => worker_view(&provided.declaration, value)
                        .map_err(|reason| (UiOutcomeStatus::Failed, reason)),
                    Err(reason) => Err((UiOutcomeStatus::Failed, reason)),
                }
            }
        };
        match result {
            Ok(view) => outcome.view = Some(view),
            Err((status, message)) => {
                outcome.status = status;
                outcome.message = Some(message);
            }
        }
        Ok(outcome)
    }

    /// `InvokeUiAction`: a host action is returned for the shell to perform;
    /// a worker action runs once (never replayed) and its result is
    /// validated before it is returned.
    pub(crate) async fn ui_action(
        &self,
        request: InvokeUiActionRequest,
    ) -> Result<UiActionOutcome, String> {
        self.available()?;
        let mut outcome = UiActionOutcome {
            contribution_id: request.contribution_id.clone(),
            package_hash: request.package_hash.clone(),
            action_id: request.action_id.clone(),
            task_id: request.task_id.clone(),
            status: UiOutcomeStatus::Ok,
            host: None,
            view: None,
            message: None,
        };
        let resolved = {
            let manager = self.clone();
            let request = request.clone();
            super::blocking(move || {
                Ok(manager.ui_resolve(
                    &request.contribution_id,
                    &request.package_hash,
                    request.task_id.as_deref(),
                ))
            })
            .await?
        };
        let provided = match resolved {
            Ok(provided) => provided,
            Err((status, message)) => {
                outcome.status = status;
                outcome.message = Some(message);
                return Ok(outcome);
            }
        };
        let Some(action) = provided
            .declaration
            .actions
            .iter()
            .find(|action| action.id == request.action_id)
        else {
            outcome.status = UiOutcomeStatus::Refused;
            outcome.message = Some(format!(
                "{} has no action `{}`",
                request.contribution_id.as_str(),
                request.action_id
            ));
            return Ok(outcome);
        };
        match &action.run {
            UiActionRun::Host(host) => outcome.host = Some(host.clone()),
            UiActionRun::Worker(call) => {
                let params = json!({
                    "contributionId": request.contribution_id,
                    "actionId": request.action_id,
                    "taskId": request.task_id,
                    "requestId": request.request_id,
                });
                let result = self
                    .ui_worker_call(
                        &provided,
                        &call.method,
                        params,
                        request.task_id.as_deref(),
                        UI_ACTION_TIMEOUT_MS,
                        request.request_id.clone(),
                    )
                    .await
                    .and_then(|value| worker_action_result(&provided.declaration, value));
                match result {
                    Ok(result) => {
                        outcome.message = result.message;
                        outcome.view = result.view;
                    }
                    Err(reason) => {
                        outcome.status = UiOutcomeStatus::Failed;
                        outcome.message = Some(reason);
                    }
                }
            }
        }
        Ok(outcome)
    }

    /// The contribution `id` as `package_hash` provides it now, with its
    /// task checked. Blocking.
    fn ui_resolve(
        &self,
        id: &ContributionId,
        package_hash: &str,
        task_id: Option<&str>,
    ) -> Result<Provided, Refusal> {
        let (provided, refused, safe_mode) = self.ui_read();
        if safe_mode {
            return Err((
                UiOutcomeStatus::Refused,
                "safe mode is on, so extension views and actions stay off; turn it off in Settings → Extensions".into(),
            ));
        }
        let Some(provided) = provided
            .into_iter()
            .find(|provided| provided.entry.id == *id)
        else {
            let reason = refused
                .iter()
                .find(|refusal| refusal.contribution_id == id.as_str())
                .map(|refusal| format!("{} is not shown: {}", id.as_str(), refusal.reason))
                .unwrap_or_else(|| {
                    format!(
                        "no enabled extension provides {} any more; refresh",
                        id.as_str()
                    )
                });
            return Err((UiOutcomeStatus::Stale, reason));
        };
        if provided.entry.package_hash != package_hash {
            return Err((
                UiOutcomeStatus::Stale,
                format!(
                    "{} is now provided by {} {}; refresh and try again",
                    id.as_str(),
                    provided.entry.extension_name,
                    provided.entry.version
                ),
            ));
        }
        match (provided.entry.context, task_id) {
            (UiContext::Task, None) => Err((
                UiOutcomeStatus::Refused,
                format!("{} acts on one task; name it", id.as_str()),
            )),
            (UiContext::None, Some(_)) => Err((
                UiOutcomeStatus::Refused,
                format!("{} does not act on a task; leave the task out", id.as_str()),
            )),
            (UiContext::Task, Some(task)) => match self.0.runtime.bind_task(task) {
                Ok(_) => Ok(provided),
                Err(reason) => Err((UiOutcomeStatus::Refused, reason)),
            },
            (UiContext::None, None) => Ok(provided),
        }
    }

    /// Start one supervised instance for exactly this package (and task),
    /// call `method` once with a deadline, and stop the instance.
    async fn ui_worker_call(
        &self,
        provided: &Provided,
        method: &str,
        params: Value,
        task_id: Option<&str>,
        timeout_ms: u64,
        request_id: Option<String>,
    ) -> Result<Value, String> {
        let label = provided.entry.extension_name.clone();
        let start: StartExtensionInstanceRequest = serde_json::from_value(json!({
            "extensionId": provided.entry.extension_id,
            "packageHash": provided.entry.package_hash,
            "taskId": task_id,
            "require": [method],
        }))
        .map_err(|error| format!("the {method} start could not be formed: {error}"))?;
        let instance = self
            .start_instance(start)
            .await
            .map_err(|error| format!("{label} could not start its worker: {error}"))?;
        let call: Result<CallExtensionInstanceRequest, String> = serde_json::from_value(json!({
            "instanceId": instance.instance_id,
            "method": method,
            "params": params,
            "timeoutMs": timeout_ms,
            "requestId": request_id,
        }))
        .map_err(|error| format!("the {method} call could not be formed: {error}"));
        let outcome = match call {
            Ok(call) => self.call_instance(call).await,
            Err(error) => Err(error),
        };
        if let Err(error) = self.stop_instance(&instance.instance_id).await {
            tracing::debug!(instance = %instance.instance_id, %error, "UI worker instance already gone");
        }
        let outcome = outcome.map_err(|error| format!("{label} did not take {method}: {error}"))?;
        match outcome.status {
            ExtensionCallStatus::Ok => Ok(outcome.result.unwrap_or(Value::Null)),
            ExtensionCallStatus::Failed => {
                let (code, message) = outcome
                    .error
                    .map(|error| (error.code, error.message))
                    .unwrap_or_else(|| ("failed".into(), String::new()));
                Err(format!("{label} refused {method} ({code}): {message}"))
            }
            ExtensionCallStatus::DeadlineExceeded => Err(format!(
                "{label} did not answer {method} within {timeout_ms} ms"
            )),
            ExtensionCallStatus::Busy => Err(format!("{label} was busy and did not take {method}")),
            // `workerExited`, and any later status: the outcome is unknown
            // and nothing is sent again.
            _ => Err(format!(
                "{label}'s worker exited during {method}; it is not sent again"
            )),
        }
    }

    /// Every UI contribution of this profile's enabled packages, refused
    /// ones with their reason, and whether safe mode is on (when it is,
    /// every declared contribution is refused). Blocking.
    fn ui_read(&self) -> (Vec<Provided>, Vec<UiContributionRefusal>, bool) {
        let mut provided = Vec::new();
        let mut refused = Vec::new();
        if self.0.unavailable.is_some() {
            return (provided, refused, false);
        }
        // An unreadable runtime state refuses every start; views follow it.
        let safe_mode = !matches!(self.0.runtime.safe_mode(), Ok(None));
        let (index, file) = match (self.0.packages.read_index(), self.0.grants.read()) {
            (Ok(index), Ok(file)) => (index, file),
            (Err(error), _) | (_, Err(error)) => {
                tracing::warn!(%error, "extension UI contributions hidden: enablement is unreadable");
                return (provided, refused, safe_mode);
            }
        };
        for (extension_id, grant) in &file.extensions {
            let Some(record) = index
                .iter()
                .find(|record| record.package_hash == grant.package_hash)
            else {
                continue;
            };
            let validated = match self.0.packages.read_manifest(&record.package_hash) {
                Ok(validated) => validated,
                Err(error) => {
                    tracing::warn!(extension = %extension_id, %error, "enabled package unreadable");
                    continue;
                }
            };
            let declared: Vec<(String, UiPlacement)> = validated
                .manifest
                .contributions
                .iter()
                .filter_map(|contribution| {
                    UiPlacement::from_manifest_kind(&contribution.kind)
                        .map(|placement| (contribution.id.as_str().to_owned(), placement))
                })
                .collect();
            if declared.is_empty() {
                continue;
            }
            let refuse = |refused: &mut Vec<UiContributionRefusal>, id: &str, reason: String| {
                refused.push(UiContributionRefusal {
                    contribution_id: id.to_owned(),
                    extension_id: record.id.clone(),
                    version: record.version.clone(),
                    package_hash: record.package_hash.clone(),
                    reason,
                });
            };
            if safe_mode {
                for (id, _) in &declared {
                    refuse(
                        &mut refused,
                        id,
                        "safe mode is on; extension views stay hidden until it is turned off"
                            .into(),
                    );
                }
                continue;
            }
            let runtime = validated.manifest.runtime.kind;
            match self.read_ui_declaration(&validated, &declared) {
                Err(reason) => {
                    for (id, _) in &declared {
                        refuse(&mut refused, id, reason.clone());
                    }
                }
                Ok(declarations) => {
                    for (id, declaration) in declarations {
                        match declaration {
                            Ok(declaration) => provided.push(Provided {
                                entry: UiContributionEntry {
                                    id: declaration.id.clone(),
                                    placement: declaration.placement,
                                    title: declaration.title.trim().to_owned(),
                                    icon: declaration.icon.clone(),
                                    order: declaration.order.unwrap_or(DEFAULT_ORDER),
                                    context: declaration.context,
                                    extension_id: record.id.clone(),
                                    extension_name: record.display_name.clone(),
                                    version: record.version.clone(),
                                    package_hash: record.package_hash.clone(),
                                    trust: runtime.trust(),
                                    view: declaration.view.as_ref().map(|view| match view {
                                        UiViewSource::Static(_) => UiViewKind::Static,
                                        UiViewSource::Worker(_) => UiViewKind::Worker,
                                    }),
                                    actions: declaration
                                        .actions
                                        .iter()
                                        .map(|action| UiActionEntry {
                                            id: action.id.clone(),
                                            title: action.title.trim().to_owned(),
                                            runs: match action.run {
                                                UiActionRun::Host(_) => UiActionKind::Host,
                                                UiActionRun::Worker(_) => UiActionKind::Worker,
                                            },
                                        })
                                        .collect(),
                                },
                                declaration,
                            }),
                            Err(reason) => refuse(&mut refused, &id, reason),
                        }
                    }
                }
            }
        }
        provided.sort_by(|a, b| {
            (a.entry.placement, a.entry.order, a.entry.id.as_str()).cmp(&(
                b.entry.placement,
                b.entry.order,
                b.entry.id.as_str(),
            ))
        });
        refused.sort_by(|a, b| a.contribution_id.cmp(&b.contribution_id));
        (provided, refused, safe_mode)
    }

    /// The package's `luna-ui.json`: listed, read from the installed tree
    /// and checked against the manifest's pin before it is parsed. `Err`
    /// refuses every UI contribution of the package; a per-contribution
    /// `Err` refuses only that one.
    #[allow(clippy::type_complexity)]
    fn read_ui_declaration(
        &self,
        validated: &ValidatedManifest,
        declared: &[(String, UiPlacement)],
    ) -> Result<Vec<(String, Result<UiContributionDeclaration, String>)>, String> {
        let pinned = validated
            .manifest
            .files
            .iter()
            .find(|file| file.path == UI_DECLARATION_FILE)
            .ok_or_else(|| format!("the package lists no {UI_DECLARATION_FILE}"))?;
        let path = self
            .0
            .packages
            .package_dir(&validated.package_hash)
            .join(UI_DECLARATION_FILE);
        let bytes = store::read_regular_file(&path, UI_MAX_DECLARATION_BYTES)
            .map_err(|error| format!("{UI_DECLARATION_FILE} could not be read: {error}"))?;
        if sha256_hex(&bytes) != pinned.sha256 {
            return Err(format!(
                "{UI_DECLARATION_FILE} changed after installation; reinstall the package"
            ));
        }
        let declaration: UiDeclaration = serde_json::from_slice(&bytes)
            .map_err(|error| format!("{UI_DECLARATION_FILE} is invalid: {error}"))?;
        if declaration.declaration_version != UI_DECLARATION_VERSION {
            return Err(format!(
                "{UI_DECLARATION_FILE} is version {}; this LUNA reads version {UI_DECLARATION_VERSION}",
                declaration.declaration_version
            ));
        }
        if declaration.contributions.len() > UI_MAX_CONTRIBUTIONS {
            return Err(format!(
                "{UI_DECLARATION_FILE} declares {} contributions; at most {UI_MAX_CONTRIBUTIONS} are placed",
                declaration.contributions.len()
            ));
        }
        let mut by_id: BTreeMap<String, UiContributionDeclaration> = BTreeMap::new();
        for contribution in declaration.contributions {
            let id = contribution.id.as_str().to_owned();
            let Some((_, kind)) = declared.iter().find(|(declared, _)| *declared == id) else {
                return Err(format!(
                    "{UI_DECLARATION_FILE} declares {id}, which the manifest does not list as a UI contribution"
                ));
            };
            if *kind != contribution.placement {
                return Err(format!(
                    "{UI_DECLARATION_FILE} places {id} as {}, but the manifest lists it as {}",
                    contribution.placement.manifest_kind(),
                    kind.manifest_kind()
                ));
            }
            if by_id.insert(id.clone(), contribution).is_some() {
                return Err(format!("{UI_DECLARATION_FILE} declares {id} twice"));
            }
        }
        let views: HashSet<&str> = declared
            .iter()
            .filter(|(_, placement)| placement.has_view())
            .map(|(id, _)| id.as_str())
            .collect();
        let runtime = validated.manifest.runtime.kind;
        Ok(declared
            .iter()
            .map(|(id, _)| {
                let result = match by_id.get(id) {
                    None => Err(format!("it has no declaration in {UI_DECLARATION_FILE}")),
                    Some(contribution) => check_declaration(contribution, runtime, &views)
                        .map(|()| contribution.clone()),
                };
                (id.clone(), result)
            })
            .collect())
    }
}

/// Every rule a contribution must meet before the shell places it.
fn check_declaration(
    contribution: &UiContributionDeclaration,
    runtime: ExtensionRuntimeKind,
    views: &HashSet<&str>,
) -> Result<(), String> {
    check_ui_title("the title", &contribution.title)?;
    if let Some(icon) = &contribution.icon
        && !UI_ICONS.contains(&icon.as_str())
    {
        return Err(format!(
            "the icon `{icon}` is not one of {}",
            UI_ICONS.join(", ")
        ));
    }
    if let Some(order) = contribution.order
        && !(0..=MAX_ORDER).contains(&order)
    {
        return Err(format!("the order {order} is outside 0 to {MAX_ORDER}"));
    }
    match contribution.placement {
        UiPlacement::Command => {
            if contribution.view.is_some() {
                return Err("a command has no view; it runs its one action".into());
            }
            if contribution.actions.len() != 1 {
                return Err(format!(
                    "a command runs exactly one action; it declares {}",
                    contribution.actions.len()
                ));
            }
        }
        placement => {
            if contribution.view.is_none() {
                return Err(format!(
                    "a {} shows a view; declare a static or worker view",
                    placement.manifest_kind()
                ));
            }
            if contribution.actions.len() > UI_MAX_ACTIONS {
                return Err(format!(
                    "it declares {} actions; at most {UI_MAX_ACTIONS} are placed",
                    contribution.actions.len()
                ));
            }
        }
    }
    if contribution.placement == UiPlacement::Panel && contribution.context != UiContext::Task {
        return Err("a panel is shown beside one task; declare context `task`".into());
    }
    let code_runs = |what: &str| -> Result<(), String> {
        if runtime == ExtensionRuntimeKind::Data {
            Err(format!(
                "{what} needs a worker, and a data package runs no code; use a host action or a static view"
            ))
        } else {
            Ok(())
        }
    };
    let mut ids = HashSet::new();
    for action in &contribution.actions {
        check_ui_id("an action id", &action.id)?;
        if !ids.insert(action.id.as_str()) {
            return Err(format!("the action id `{}` is repeated", action.id));
        }
        check_ui_title(
            &format!("the title of action `{}`", action.id),
            &action.title,
        )?;
        match &action.run {
            UiActionRun::Worker(call) => {
                code_runs(&format!("action `{}`", action.id))?;
                check_worker_method(&call.method)?;
            }
            UiActionRun::Host(UiHostAction::NewTask) => {}
            UiActionRun::Host(UiHostAction::OpenSettings(open)) => {
                if !UI_SETTINGS_SECTIONS.contains(&open.section.as_str()) {
                    return Err(format!(
                        "action `{}` opens the unknown settings section `{}`",
                        action.id, open.section
                    ));
                }
            }
            UiActionRun::Host(UiHostAction::OpenContribution(open)) => {
                if !views.contains(open.contribution_id.as_str()) {
                    return Err(format!(
                        "action `{}` opens {}, which is not a view of this package",
                        action.id,
                        open.contribution_id.as_str()
                    ));
                }
            }
        }
    }
    let action_ids: Vec<&str> = contribution
        .actions
        .iter()
        .map(|action| action.id.as_str())
        .collect();
    match &contribution.view {
        None => {}
        Some(UiViewSource::Static(view)) => view
            .validate(&action_ids)
            .map_err(|reason| format!("its view is invalid: {reason}"))?,
        Some(UiViewSource::Worker(call)) => {
            code_runs("a worker view")?;
            check_worker_method(&call.method)?;
        }
    }
    Ok(())
}

fn check_worker_method(method: &str) -> Result<(), String> {
    check_ui_id("a worker method", method)?;
    if method.starts_with("luna.") || method.starts_with("provider.") {
        return Err(format!(
            "the worker method `{method}` uses a reserved prefix (`luna.`, `provider.`)"
        ));
    }
    Ok(())
}

fn action_ids(declaration: &UiContributionDeclaration) -> Vec<&str> {
    declaration
        .actions
        .iter()
        .map(|action| action.id.as_str())
        .collect()
}

/// A worker's view, held to the same bounds as a static one.
fn worker_view(declaration: &UiContributionDeclaration, value: Value) -> Result<UiView, String> {
    let view: UiView = serde_json::from_value(value)
        .map_err(|error| format!("the worker returned an invalid view: {error}"))?;
    view.validate(&action_ids(declaration))
        .map_err(|reason| format!("the worker's view was not shown: {reason}"))?;
    Ok(view)
}

/// A worker action's result: an optional bounded message and view.
fn worker_action_result(
    declaration: &UiContributionDeclaration,
    value: Value,
) -> Result<UiWorkerActionResult, String> {
    let result: UiWorkerActionResult = if value.is_null() {
        UiWorkerActionResult::default()
    } else {
        serde_json::from_value(value)
            .map_err(|error| format!("the worker returned an invalid result: {error}"))?
    };
    if let Some(message) = &result.message {
        check_ui_message(message)
            .map_err(|reason| format!("the worker's message was not shown: {reason}"))?;
    }
    if let Some(view) = &result.view {
        if !declaration.placement.has_view() {
            return Err("the worker returned a view for a command, which has none".into());
        }
        view.validate(&action_ids(declaration))
            .map_err(|reason| format!("the worker's view was not shown: {reason}"))?;
    }
    Ok(result)
}
