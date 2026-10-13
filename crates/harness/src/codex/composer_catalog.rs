//! Installed-use catalog; no marketplace suggestions, thread resume or credential mutation.
use crate::{HarnessError, jsonrpc::RpcClient};
use luna_proto::{
    ComposerCatalog, ComposerCatalogEntry, ComposerCatalogKind as Kind, ComposerCatalogSlice,
    ComposerCatalogStatus as Status,
};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

type CatalogKey = (String, String, String);
type ActiveCatalogClient = (String, RpcClient);

#[derive(Clone, Default)]
pub(crate) struct CatalogConnections(Arc<Mutex<HashMap<CatalogKey, ActiveCatalogClient>>>);

pub(crate) struct CatalogLease {
    connections: CatalogConnections,
    key: (String, String, String),
    generation: String,
}
impl Drop for CatalogLease {
    fn drop(&mut self) {
        if let Ok(mut connections) = self.connections.0.lock()
            && connections
                .get(&self.key)
                .is_some_and(|(generation, _)| generation == &self.generation)
        {
            connections.remove(&self.key);
        }
    }
}
impl CatalogConnections {
    pub(crate) fn register(
        &self,
        home: &Path,
        cwd: &str,
        thread: &str,
        client: RpcClient,
    ) -> CatalogLease {
        let key = (
            home.to_string_lossy().into_owned(),
            cwd.to_owned(),
            thread.to_owned(),
        );
        let generation = uuid::Uuid::new_v4().to_string();
        if let Ok(mut connections) = self.0.lock() {
            connections.insert(key.clone(), (generation.clone(), client));
        }
        CatalogLease {
            connections: self.clone(),
            key,
            generation,
        }
    }
    pub(crate) fn get(&self, home: &Path, cwd: &Path, thread: &str) -> Option<RpcClient> {
        self.0
            .lock()
            .ok()?
            .get(&(
                home.to_string_lossy().into_owned(),
                cwd.to_string_lossy().into_owned(),
                thread.to_owned(),
            ))
            .map(|(_, client)| client.clone())
    }
}

fn unavailable(kind: Kind, detail: &str) -> ComposerCatalogSlice {
    ComposerCatalogSlice {
        kind,
        status: Status::Unavailable,
        disabled_count: 0,
        detail: Some(detail.into()),
    }
}
fn finish(
    kind: Kind,
    entries: &[ComposerCatalogEntry],
    disabled_count: usize,
) -> ComposerCatalogSlice {
    ComposerCatalogSlice {
        kind,
        status: if entries.iter().any(|row| row.kind == kind) {
            Status::Ready
        } else if disabled_count > 0 {
            Status::PolicyDisabled
        } else {
            Status::Empty
        },
        disabled_count,
        detail: None,
    }
}
fn text(value: &Value, key: &str, limit: usize) -> Option<String> {
    value
        .get(key)?
        .as_str()
        .filter(|value| {
            !value.is_empty() && value.len() <= limit && !value.chars().any(char::is_control)
        })
        .map(str::to_owned)
}
fn array<'a>(value: &'a Value, key: &str, max: usize) -> Result<&'a Vec<Value>, HarnessError> {
    value
        .get(key)
        .and_then(Value::as_array)
        .filter(|rows| rows.len() <= max)
        .ok_or_else(|| HarnessError::Protocol(format!("composer catalog has invalid {key} array")))
}
fn missing_method(error: &HarnessError) -> bool {
    let message = error.to_string();
    message.contains("code -32601")
}
async fn installed_or_legacy(
    client: &RpcClient,
    method: &str,
    params: Value,
    legacy: &str,
    legacy_params: Value,
) -> Result<(Value, bool), HarnessError> {
    match client.request(method, params).await {
        Ok(value) => Ok((value, false)),
        Err(error) if missing_method(&error) => client
            .request(legacy, legacy_params)
            .await
            .map(|value| (value, true)),
        Err(error) => Err(error),
    }
}

pub(crate) async fn read(
    client: &RpcClient,
    cwd: &Path,
    thread: Option<&str>,
) -> Result<ComposerCatalog, HarnessError> {
    let cwd = cwd
        .to_str()
        .ok_or_else(|| HarnessError::Protocol("catalog cwd is not UTF-8".into()))?;
    let before = client
        .request("account/read", json!({"refreshToken": false}))
        .await?;
    let mut catalog = ComposerCatalog::default();
    let policy_before = thread.and_then(|thread| client.composer_policy(thread));
    // Only the loaded client's thread/start/resume and settings notifications
    // prove thread plugin policy. thread/read has no disabledPluginIds field.
    let disabled_plugins = thread
        .map(|thread| client.composer_policy(thread))
        .unwrap_or_else(|| Some(Vec::new()));
    if let Some(disabled_plugins) = disabled_plugins {
        match installed_or_legacy(
            client,
            "plugin/installed",
            json!({"cwds": [cwd]}),
            "plugin/list",
            json!({"cwds": [cwd]}),
        )
        .await
        .and_then(|(value, _)| plugins(&value, &disabled_plugins, &mut catalog.entries))
        {
            Ok(disabled) => catalog
                .slices
                .push(finish(Kind::Plugin, &catalog.entries, disabled)),
            Err(_) => catalog.slices.push(unavailable(
                Kind::Plugin,
                "Installed plugins could not be read",
            )),
        }
    } else {
        catalog.slices.push(unavailable(
            Kind::Plugin,
            "Thread plugin policy is unavailable",
        ));
    }
    let app_result = installed_or_legacy(
        client,
        "app/installed",
        json!({"threadId": thread, "forceRefresh": thread.is_none()}),
        "app/list",
        json!({"threadId": thread}),
    )
    .await;
    match app_result {
        Ok((value, legacy)) => {
            match apps(client, &value, legacy, thread, &mut catalog.entries).await {
                Ok(disabled) => {
                    let mut slice = finish(Kind::App, &catalog.entries, disabled);
                    if legacy {
                        slice.detail = Some("Compatibility catalog: enabled accessible apps; native callable state unavailable".into());
                    }
                    catalog.slices.push(slice);
                }
                Err(_) => catalog
                    .slices
                    .push(unavailable(Kind::App, "Installed apps could not be read")),
            }
        }
        Err(_) => catalog.slices.push(unavailable(
            Kind::App,
            if thread.is_some() {
                "Apps require this thread's connected native runtime"
            } else {
                "Installed apps could not be read"
            },
        )),
    }
    match client
        .request("skills/list", json!({"cwds": [cwd]}))
        .await
        .and_then(|value| skills(&value, cwd, &mut catalog.entries))
    {
        Ok(disabled) => catalog
            .slices
            .push(finish(Kind::Skill, &catalog.entries, disabled)),
        Err(_) => catalog
            .slices
            .push(unavailable(Kind::Skill, "Native skills could not be read")),
    }
    let after = client
        .request("account/read", json!({"refreshToken": false}))
        .await?;
    if before.get("account") != after.get("account") {
        return Err(HarnessError::Protocol(
            "Account changed during composer discovery".into(),
        ));
    }
    if thread.and_then(|thread| client.composer_policy(thread)) != policy_before {
        return Err(HarnessError::Protocol(
            "Thread policy changed during discovery; retry".into(),
        ));
    }
    // Never expose partially parsed rows from a failed/policy-unknown slice.
    catalog.entries.retain(|row| {
        catalog
            .slices
            .iter()
            .any(|slice| slice.kind == row.kind && slice.status == Status::Ready)
    });
    let mut seen = std::collections::HashSet::new();
    catalog
        .entries
        .retain(|row| seen.insert((row.kind, row.path.clone())));
    catalog.entries.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then(a.path.cmp(&b.path))
    });
    Ok(catalog)
}

fn plugins(
    value: &Value,
    disabled_ids: &[Value],
    entries: &mut Vec<ComposerCatalogEntry>,
) -> Result<usize, HarnessError> {
    if value
        .get("marketplaceLoadErrors")
        .and_then(Value::as_array)
        .is_some_and(|errors| !errors.is_empty())
    {
        return Err(HarnessError::Protocol(
            "plugin marketplace load failed".into(),
        ));
    }
    let mut disabled = 0;
    let mut total = 0;
    for marketplace in array(value, "marketplaces", 64)? {
        for plugin in array(marketplace, "plugins", 1024)? {
            total += 1;
            if total > 2048 {
                return Err(HarnessError::Protocol("plugin catalog too large".into()));
            }
            if plugin.get("installed").and_then(Value::as_bool) != Some(true) {
                continue;
            }
            let id = text(plugin, "id", 512)
                .ok_or_else(|| HarnessError::Protocol("plugin lacks identity".into()))?;
            if plugin.get("enabled").and_then(Value::as_bool) != Some(true)
                || plugin.get("availability").and_then(Value::as_str) == Some("DISABLED_BY_ADMIN")
                || disabled_ids
                    .iter()
                    .any(|disabled| disabled.as_str() == Some(&id))
            {
                disabled += 1;
                continue;
            }
            let interface = &plugin["interface"];
            let name = text(interface, "displayName", 256)
                .or_else(|| text(plugin, "name", 256))
                .unwrap_or_else(|| id.clone());
            entries.push(ComposerCatalogEntry {
                reviewed_revision: None,
                kind: Kind::Plugin,
                path: format!("plugin://{id}"),
                id,
                name,
                description: text(interface, "shortDescription", 1024),
            });
        }
    }
    Ok(disabled)
}
async fn apps(
    client: &RpcClient,
    value: &Value,
    legacy: bool,
    thread: Option<&str>,
    entries: &mut Vec<ComposerCatalogEntry>,
) -> Result<usize, HarnessError> {
    // An unconsumed legacy cursor is a partial catalog, not an authoritative empty list.
    if legacy
        && value
            .get("nextCursor")
            .is_some_and(|cursor| !cursor.is_null())
    {
        return Err(HarnessError::Protocol(
            "legacy app catalog is incomplete".into(),
        ));
    }
    let mut eligible = Vec::new();
    let mut disabled = 0;
    for app in array(value, "apps", 1024)? {
        let enabled = app
            .get(if legacy { "isEnabled" } else { "enabled" })
            .and_then(Value::as_bool)
            == Some(true);
        let callable = app
            .get(if legacy { "isAccessible" } else { "callable" })
            .and_then(Value::as_bool)
            == Some(true);
        if !enabled || !callable {
            disabled += 1;
            continue;
        }
        let id = text(app, "id", 512)
            .ok_or_else(|| HarnessError::Protocol("app lacks identity".into()))?;
        eligible.push(ComposerCatalogEntry {
            reviewed_revision: None,
            kind: Kind::App,
            path: format!("app://{id}"),
            name: text(app, if legacy { "name" } else { "runtimeName" }, 256)
                .unwrap_or_else(|| id.clone()),
            id,
            description: text(app, "description", 1024),
        });
    }
    if !legacy {
        for batch in eligible.chunks_mut(100) {
            let ids: Vec<_> = batch.iter().map(|entry| &entry.id).collect();
            if let Ok(metadata) = client
                .request(
                    "app/read",
                    json!({"appIds": ids, "threadId": thread, "includeTools": false}),
                )
                .await
                && let Ok(rows) = array(&metadata, "apps", 100)
            {
                for entry in batch {
                    if let Some(meta) = rows
                        .iter()
                        .find(|meta| meta.get("id").and_then(Value::as_str) == Some(&entry.id))
                    {
                        if let Some(name) = text(meta, "name", 256) {
                            entry.name = name;
                        }
                        entry.description = text(meta, "description", 1024);
                    }
                }
            }
        }
    }
    entries.extend(eligible);
    Ok(disabled)
}
fn skills(
    value: &Value,
    cwd: &str,
    entries: &mut Vec<ComposerCatalogEntry>,
) -> Result<usize, HarnessError> {
    let group = array(value, "data", 64)?
        .iter()
        .find(|group| group.get("cwd").and_then(Value::as_str) == Some(cwd))
        .ok_or_else(|| HarnessError::Protocol("skills omit requested cwd".into()))?;
    if group
        .get("errors")
        .and_then(Value::as_array)
        .is_some_and(|errors| !errors.is_empty())
    {
        return Err(HarnessError::Protocol("skill discovery failed".into()));
    }
    let mut disabled = 0;
    for skill in array(group, "skills", 1024)? {
        if skill.get("enabled").and_then(Value::as_bool) != Some(true) {
            disabled += 1;
            continue;
        }
        let name = text(skill, "name", 256)
            .ok_or_else(|| HarnessError::Protocol("skill lacks name".into()))?;
        let path = text(skill, "path", 4096)
            .filter(|path| Path::new(path).is_absolute())
            .ok_or_else(|| HarnessError::Protocol("skill lacks host path".into()))?;
        entries.push(ComposerCatalogEntry {
            reviewed_revision: None,
            kind: Kind::Skill,
            id: path.clone(),
            path,
            name,
            description: text(skill, "description", 1024),
        });
    }
    Ok(disabled)
}

/// Validate selections immediately before native dispatch under this loaded
/// runtime's account/workspace/thread policy. Pasted text never supplies items.
pub(crate) async fn validate_inputs(
    client: &RpcClient,
    cwd: &Path,
    thread: &str,
    items: &[luna_proto::ComposerInput],
) -> Result<(), HarnessError> {
    if items.is_empty() {
        return Ok(());
    }
    let catalog = tokio::time::timeout(
        std::time::Duration::from_secs(8),
        read(client, cwd, Some(thread)),
    )
    .await
    .map_err(|_| {
        HarnessError::RejectedBeforeAcceptance(
            "Selected integrations could not be verified; your input remains available".into(),
        )
    })?
    .map_err(|_| {
        HarnessError::RejectedBeforeAcceptance(
            "Selected integrations could not be verified under this account".into(),
        )
    })?;
    for item in items {
        let (kind, path) = match item {
            luna_proto::ComposerInput::Mention { path, .. } => (
                if path.starts_with("plugin://") {
                    Kind::Plugin
                } else {
                    Kind::App
                },
                path,
            ),
            luna_proto::ComposerInput::Skill { path, .. } => (Kind::Skill, path),
        };
        if !catalog
            .entries
            .iter()
            .any(|entry| entry.kind == kind && &entry.path == path)
        {
            return Err(HarnessError::RejectedBeforeAcceptance("A selected integration is unavailable in this account or thread; remove it or restore access before sending".into()));
        }
    }
    Ok(())
}
