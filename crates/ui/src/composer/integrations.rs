//! Native picker/chip interaction. Durable typed envelopes are owned by native-inputs.
use super::*;
use luna_proto::{
    ComposerCatalog, ComposerCatalogEntry, ComposerCatalogKind as Kind,
    ComposerCatalogStatus as Status, ComposerInput as NativeInput,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct NativeEditorOwner {
    pub engine_generation: u64,
    pub draft_id: String,
}

#[derive(Debug, Clone)]
pub(super) struct NativeLink {
    pub range: Range<usize>,
    pub item: NativeInput,
}
pub(super) fn identity(item: &NativeInput) -> (&str, &str) {
    match item {
        NativeInput::Mention { name, path } | NativeInput::Skill { name, path } => (name, path),
    }
}
pub(super) fn entry_input(entry: &ComposerCatalogEntry) -> NativeInput {
    match entry.kind {
        Kind::Skill => NativeInput::Skill {
            name: entry.name.clone(),
            path: entry.path.clone(),
        },
        _ => NativeInput::Mention {
            name: entry.name.clone(),
            path: entry.path.clone(),
        },
    }
}
pub(super) fn native_link(item: &NativeInput) -> String {
    let (name, path) = identity(item);
    let target = match item {
        NativeInput::Mention { .. } => {
            let (scheme, id) = path.split_once("://").unwrap_or(("", ""));
            format!("{scheme}://{}", percent_encode_path(id))
        }
        NativeInput::Skill { .. } => format!("skill://{}", percent_encode_path(path)),
    };
    format!("[@{}]({target})", escape_mention_label(name))
}
fn host_path_is_absolute(path: &str) -> bool {
    let bytes = path.as_bytes();
    path.starts_with('/')
        || path.starts_with("\\\\")
        || (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'/' | b'\\'))
}

/// Parse readable chip projections only. This NEVER grants native selection;
/// reconcile below requires an independently selected/restored typed envelope.
pub(super) fn native_links(raw: &str) -> Vec<NativeLink> {
    let mut links = Vec::new();
    let mut search = 0;
    while let Some(start) = raw[search..].find('[').map(|offset| search + offset) {
        let Some(label_end) = label_close(raw, start + 1) else {
            search = start + 1;
            continue;
        };
        let target_start = label_end + 2;
        let Some(end) = raw[target_start..]
            .find(')')
            .map(|offset| target_start + offset + 1)
        else {
            break;
        };
        search = end;
        let target = &raw[target_start..end - 1];
        let Some((scheme, encoded)) = target.split_once("://") else {
            continue;
        };
        if !matches!(scheme, "plugin" | "app" | "skill") {
            continue;
        }
        let Some(value) = percent_decode_path(encoded).filter(|value| {
            !value.is_empty() && value.len() <= 4096 && !value.chars().any(char::is_control)
        }) else {
            continue;
        };
        if percent_encode_path(&value) != encoded
            || (scheme == "skill" && !host_path_is_absolute(&value))
        {
            continue;
        }
        let Some(label) = raw[start + 1..label_end].strip_prefix('@') else {
            continue;
        };
        let mut name = String::new();
        let mut chars = label.chars();
        while let Some(ch) = chars.next() {
            name.push(if ch == '\\' {
                chars.next().unwrap_or(ch)
            } else {
                ch
            });
        }
        if name.is_empty() || name.len() > 256 || escape_mention_label(&name) != label {
            continue;
        }
        let item = if scheme == "skill" {
            NativeInput::Skill { name, path: value }
        } else {
            NativeInput::Mention {
                name,
                path: format!("{scheme}://{value}"),
            }
        };
        links.push(NativeLink {
            range: start..end,
            item,
        });
    }
    links
}

#[derive(Default)]
pub(super) struct IntegrationPicker {
    key: Option<String>,
    catalog: Option<ComposerCatalog>,
    task: Option<Task<()>>,
    generation: u64,
    force_refresh: bool,
    pub loading: bool,
    pub error: Option<SharedString>,
    retry_focus: Option<FocusHandle>,
    #[cfg(feature = "appshots-fixture")]
    fixture_catalog: bool,
    send_task: Option<Task<()>>,
    send_verified: bool,
}

impl ComposerInput {
    pub(super) fn bind_native_owner(&mut self, owner: NativeEditorOwner) -> bool {
        if self.native_owner.as_ref() == Some(&owner) {
            return false;
        }
        self.native_owner = Some(owner);
        self.native_spans.clear();
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.last_edit = None;
        self.refresh_projection();
        self.needs_measure = true;
        true
    }

    pub(super) fn native_input_items(&self) -> Vec<NativeInput> {
        let mut items = Vec::new();
        for span in &self.native_spans {
            if self.content.get(span.range.clone()) == Some(native_link(&span.item).as_str())
                && !items.contains(&span.item)
            {
                items.push(span.item.clone());
            }
        }
        items
    }

    /// Edits can move an existing anchor, never grant authority to inserted text.
    pub(super) fn apply_native_edit(&mut self, range: &Range<usize>, inserted: usize) {
        let removed = range.end - range.start;
        self.native_spans.retain_mut(|span| {
            if span.range.end <= range.start {
                return true;
            }
            if span.range.start >= range.end {
                // An insertion inside a span invalidates it; either edge stays outside.
                if range.is_empty() && span.range.start < range.start {
                    return false;
                }
                span.range =
                    (span.range.start - removed + inserted)..(span.range.end - removed + inserted);
                return true;
            }
            false
        });
    }

    fn set_text_with_native_inputs(
        &mut self,
        text: String,
        items: Vec<NativeInput>,
        cx: &mut Context<Self>,
    ) {
        // `set_text` clears history and spans. Hydrate only this trusted envelope.
        self.set_text(text, cx);
        let links = native_links(&self.content);
        let mut seen = Vec::new();
        for item in items {
            if seen.contains(&item) {
                continue;
            }
            seen.push(item.clone());
            if let Some(link) = links.iter().find(|link| link.item == item) {
                self.native_spans.push(link.clone());
            } else {
                if !self.content.is_empty() && !self.content.ends_with(char::is_whitespace) {
                    self.content.push(' ');
                }
                let start = self.content.len();
                self.content.push_str(&native_link(&item));
                self.native_spans.push(NativeLink {
                    range: start..self.content.len(),
                    item,
                });
            }
        }
        self.native_spans.sort_by_key(|span| span.range.start);
        self.refresh_projection();
        let end = self.content.len();
        self.selected_range = end..end;
        self.needs_measure = true;
        cx.notify();
    }

    pub(super) fn replace_native_mention(
        &mut self,
        range: Range<usize>,
        item: &NativeInput,
        cx: &mut Context<Self>,
    ) {
        if self.read_only {
            return;
        }
        self.invalidate_mention_tooltip();
        let link = native_link(item);
        let separator = self.content[range.end..]
            .chars()
            .next()
            .filter(|ch| ch.is_whitespace() && *ch != '\n' && *ch != '\r');
        let inserted = if separator.is_some() {
            link
        } else {
            format!("{link} ")
        };
        self.record_edit(&range, &inserted);
        self.apply_native_edit(&range, inserted.len());
        self.content.replace_range(range.clone(), &inserted);
        self.native_spans.push(NativeLink {
            range: range.start..range.start + native_link(item).len(),
            item: item.clone(),
        });
        self.native_spans.sort_by_key(|span| span.range.start);
        self.refresh_projection();
        let cursor = range.start + inserted.len() + separator.map(char::len_utf8).unwrap_or(0);
        self.selected_range = cursor..cursor;
        self.selection_reversed = false;
        self.follow_cursor = true;
        self.reset_blink();
        self.needs_measure = true;
        cx.emit(ComposerInputEvent::Edited);
        cx.notify();
    }
}

impl Composer {
    fn native_catalog_request(
        &self,
        refresh: bool,
        cx: &App,
    ) -> Option<(String, serde_json::Value)> {
        let config = self.pickers.read(cx).resolved(cx);
        if config.harness != Some(HarnessId::Codex) {
            return None;
        }
        let state = self.state.read(cx);
        let mut params = serde_json::Map::new();
        if let Some(chat) = state.selected_chat_row() {
            params.insert("chatId".into(), chat.id.clone().into());
            params.insert("targetDeviceId".into(), chat.device_id.clone().into());
        } else if let Some(space) = state.selected_space_row() {
            params.insert("spaceId".into(), space.id.clone().into());
            params.insert("targetDeviceId".into(), space.device_id.clone().into());
            if let crate::pickers::CheckoutPlan::ReuseWorktree { path, .. } =
                self.pickers.read(cx).checkout_plan()
            {
                params.insert("path".into(), path.into());
            }
        } else {
            return None;
        }
        params.insert(
            "providerAccountId".into(),
            serde_json::to_value(config.provider_account_id).ok()?,
        );
        let key = serde_json::Value::Object(params.clone()).to_string();
        params.insert("refresh".into(), refresh.into());
        Some((key, serde_json::Value::Object(params)))
    }

    fn native_editor_owner(&self, cx: &App) -> NativeEditorOwner {
        NativeEditorOwner {
            engine_generation: self.state.read(cx).engine_generation(),
            draft_id: self.draft_id_for(&self.current_key),
        }
    }

    pub(super) fn ensure_native_editor_owner(&mut self, cx: &mut Context<Self>) {
        let owner = self.native_editor_owner(cx);
        if self
            .input
            .update(cx, |input, _| input.bind_native_owner(owner))
        {
            self.input_items.remove(&self.current_key);
        }
    }

    /// Trusted draft/queue/recovery hydration; never called for a plain text edit.
    pub(crate) fn restore_native_input_envelope(
        &mut self,
        text: String,
        items: Vec<NativeInput>,
        cx: &mut Context<Self>,
    ) {
        self.ensure_native_editor_owner(cx);
        self.suppress_next_draft_edit = true;
        self.input.update(cx, |input, cx| {
            input.set_text_with_native_inputs(text, items, cx)
        });
        self.reconcile_native_selections(cx);
    }

    pub(crate) fn hydrate_native_input_envelope(&mut self, cx: &mut Context<Self>) {
        let text = self.input.read(cx).text().to_owned();
        let items = self
            .input_items
            .get(&self.current_key)
            .cloned()
            .unwrap_or_default();
        self.restore_native_input_envelope(text, items, cx);
    }

    pub(crate) fn reset_native_picker_owner(&mut self, cx: &mut Context<Self>) {
        let generation = self.integrations.generation.wrapping_add(1);
        self.integrations = IntegrationPicker {
            generation,
            ..Default::default()
        };
        self.ensure_native_editor_owner(cx);
    }

    pub(super) fn reconcile_native_selections(&mut self, cx: &mut Context<Self>) {
        self.ensure_native_editor_owner(cx);
        let selected = self.input.read(cx).native_input_items();
        if selected.is_empty() {
            self.input_items.remove(&self.current_key);
        } else {
            self.input_items.insert(self.current_key.clone(), selected);
        }
    }

    pub(super) fn update_native_rows(&mut self) {
        let query = self
            .mention
            .token
            .as_ref()
            .map(|token| token.query.to_lowercase())
            .unwrap_or_default();
        self.mention.native_results = self
            .integrations
            .catalog
            .as_ref()
            .map(|catalog| {
                catalog
                    .entries
                    .iter()
                    .filter(|entry| {
                        query.is_empty()
                            || entry.name.to_lowercase().contains(&query)
                            || entry.id.to_lowercase().contains(&query)
                    })
                    .take(40)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        let count = self.mention.results.len() + self.mention.native_results.len();
        if self.mention.active.is_none_or(|active| active >= count) {
            self.mention.active = (count > 0).then_some(0);
        }
    }

    pub(super) fn ensure_native_catalog(&mut self, fresh_open: bool, cx: &mut Context<Self>) {
        let Some((key, mut params)) = self.native_catalog_request(false, cx) else {
            self.integrations.generation = self.integrations.generation.wrapping_add(1);
            self.integrations.task = None;
            self.integrations.key = None;
            self.integrations.catalog = None;
            self.integrations.error = None;
            self.integrations.loading = false;
            self.mention.native_results.clear();
            return;
        };
        let changed = self.integrations.key.as_ref() != Some(&key);
        if changed {
            self.integrations.generation = self.integrations.generation.wrapping_add(1);
            self.integrations.task = None;
            self.integrations.catalog = None;
            self.integrations.key = Some(key.clone());
            self.integrations.loading = false;
            self.integrations.error = None;
        }
        let expired = self.integrations.catalog.as_ref().is_some_and(|catalog| {
            chrono::Utc::now()
                .timestamp_millis()
                .saturating_sub(catalog.fetched_at_ms)
                >= catalog.ttl_ms as i64
        });
        if !changed
            && (self.integrations.loading
                || (!fresh_open && !expired && self.integrations.catalog.is_some()))
        {
            self.update_native_rows();
            return;
        }
        if self.mention.token.is_none()
            && self
                .input_items
                .get(&self.current_key)
                .is_none_or(Vec::is_empty)
        {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.integrations.loading = true;
        if std::mem::take(&mut self.integrations.force_refresh) {
            params["refresh"] = true.into();
        }
        let generation = self.integrations.generation;
        let owner = self.native_editor_owner(cx);
        self.integrations.task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(methods::LIST_COMPOSER_CATALOG, params)
                .await;
            this.update(cx, |composer, cx| {
                if composer.native_editor_owner(cx) != owner || composer.integrations.generation != generation
                    || composer.integrations.key.as_ref() != Some(&key)
                    || composer
                        .native_catalog_request(false, cx)
                        .as_ref()
                        .map(|(key, _)| key)
                        != Some(&key)
                {
                    return;
                }
                composer.integrations.loading = false;
                match result.and_then(|value| {
                    serde_json::from_value::<ComposerCatalog>(value)
                        .map_err(|_| RpcError::Failed("Invalid catalog response".into()))
                }) {
                    Ok(catalog) => {
                        composer.integrations.catalog = Some(catalog);
                        composer.integrations.error = None;
                    }
                    Err(_) => {
                        if let Some(catalog) = composer.integrations.catalog.as_mut() {
                            catalog.stale = true;
                        }
                        composer.integrations.error = Some(
                            "Native integrations unavailable on this host. Retry discovery.".into(),
                        );
                    }
                }
                if let Some(items) = composer.input_items.get(&composer.current_key)
                    && !items.is_empty()
                    && composer.integrations.catalog.as_ref().is_none_or(|catalog| catalog.stale || items.iter().any(|item| {
                        let (_, path) = identity(item);
                        !catalog.entries.iter().any(|entry| entry.path == path)
                    })) {
                    composer.failure = Some("Selected integration availability changed. Your selections and draft are preserved; verify access before sending.".into());
                    composer.failure_key = Some(composer.current_key.clone());
                } else if composer.failure.as_deref() == Some("Selected integration availability changed. Your selections and draft are preserved; verify access before sending.")
                    && composer.failure_key.as_deref() == Some(composer.current_key.as_str()) {
                    composer.failure = None;
                    composer.failure_key = None;
                }
                composer.update_native_rows();
                composer.sync_mention_controls(cx);
                cx.notify();
            })
            .ok();
        }));
        self.update_native_rows();
    }

    pub(super) fn retry_native_catalog(&mut self, cx: &mut Context<Self>) {
        self.integrations.generation = self.integrations.generation.wrapping_add(1);
        self.integrations.task = None;
        self.integrations.loading = false;
        self.integrations.force_refresh = true;
        self.ensure_native_catalog(true, cx);
        cx.notify();
    }

    pub(super) fn accept_native_mention(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(entry) = self.mention.native_results.get(index).cloned() else {
            return;
        };
        if self.integrations.loading
            || self.integrations.catalog.as_ref().is_none_or(|catalog| {
                catalog.stale
                    || !catalog
                        .slices
                        .iter()
                        .any(|slice| slice.kind == entry.kind && slice.status == Status::Ready)
            })
        {
            return;
        }
        let Some(token) = self.mention.token.clone() else {
            return;
        };
        let item = entry_input(&entry);
        self.ensure_native_editor_owner(cx);
        self.input.update(cx, |input, cx| {
            input.replace_native_mention(token.range, &item, cx)
        });
        self.reconcile_native_selections(cx);
        self.reset_mention(None, cx);
        self.mark_draft_dirty(cx);
        cx.notify();
    }

    pub(super) fn native_catalog_notice(&self) -> Option<SharedString> {
        if let Some(error) = &self.integrations.error {
            return Some(error.clone());
        }
        if self.integrations.loading {
            return Some("Loading native integrations…".into());
        }
        let catalog = self.integrations.catalog.as_ref()?;
        if catalog.stale {
            return Some(
                "Saved integrations shown; availability could not be verified. Retry discovery."
                    .into(),
            );
        }
        let unavailable: Vec<_> = catalog
            .slices
            .iter()
            .filter(|slice| slice.status == Status::Unavailable)
            .map(|slice| kind_label(slice.kind))
            .collect();
        if !unavailable.is_empty() {
            return Some(
                format!("Unavailable: {}. Retry discovery.", unavailable.join(", ")).into(),
            );
        }
        if let Some(detail) = catalog
            .slices
            .iter()
            .find_map(|slice| slice.detail.as_ref())
        {
            return Some(detail.clone().into());
        }
        if catalog.entries.is_empty() {
            return Some(
                if catalog
                    .slices
                    .iter()
                    .any(|slice| slice.status == Status::PolicyDisabled)
                {
                    "Installed integrations are disabled by provider policy"
                } else {
                    "No installed native integrations"
                }
                .into(),
            );
        }
        None
    }

    /// Fresh host eligibility check before saving a send/queue mutation. Exact
    /// loaded-runtime dispatch validation is repeated in the native adapter.
    pub(super) fn defer_native_send(
        &mut self,
        text: &str,
        behavior: Option<luna_proto::FollowUpBehavior>,
        cx: &mut Context<Self>,
    ) -> bool {
        if std::mem::take(&mut self.integrations.send_verified) {
            return false;
        }
        let items = self
            .input_items
            .get(&self.current_key)
            .cloned()
            .unwrap_or_default();
        if items.is_empty() {
            return false;
        }
        if self.integrations.send_task.is_some() {
            return true;
        }
        let Some((scope, params)) = self.native_catalog_request(true, cx) else {
            self.failure = Some("Selected integrations require their Codex execution host. Your draft is preserved.".into());
            self.failure_key = Some(self.current_key.clone());
            cx.notify();
            return true;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return false;
        };
        let key = self.current_key.clone();
        let owner = self.native_editor_owner(cx);
        let generation = self.integrations.generation;
        let submitted_text = text.to_owned();
        let raw = self.input.read(cx).text().to_owned();
        self.integrations.send_task = Some(cx.spawn(async move |this, cx| {
            let result = engine.client().call(methods::LIST_COMPOSER_CATALOG, params).await;
            this.update(cx, |composer, cx| {
                if composer.native_editor_owner(cx) != owner || composer.integrations.generation != generation { return; }
                composer.integrations.send_task = None;
                if composer.current_key != key || composer.input.read(cx).text() != raw
                    || composer.input_items.get(&key) != Some(&items)
                    || composer.native_catalog_request(true, cx).as_ref().map(|(key, _)| key) != Some(&scope) { return; }
                let catalog = result.ok().and_then(|value| serde_json::from_value::<ComposerCatalog>(value).ok());
                let valid = catalog.as_ref().is_some_and(|catalog| !catalog.stale && items.iter().all(|item| {
                    let (_, path) = identity(item);
                    catalog.entries.iter().any(|entry| entry.path == path)
                }));
                if valid {
                    composer.integrations.catalog = catalog;
                    composer.integrations.key = Some(scope);
                    composer.integrations.send_verified = true;
                    composer.send(submitted_text, behavior, cx);
                } else {
                    composer.failure = Some("A selected integration is unavailable in this account or thread. Remove it or retry after restoring access; your draft is preserved.".into());
                    composer.failure_key = Some(key);
                    cx.notify();
                }
            }).ok();
        }));
        cx.notify();
        true
    }
}
pub(super) fn kind_label(kind: Kind) -> &'static str {
    match kind {
        Kind::Plugin => "Plugins",
        Kind::App => "Apps",
        Kind::Skill => "Skills",
    }
}
