//! Native picker/chip interaction. Durable typed envelopes are owned by native-inputs.
//!
//! Provenance contract (joint review R1):
//! - A native identity exists only as an owner-bound anchored byte span in the
//!   editor. Raw link text never grants one: paste, programmatic `set_text`,
//!   prompt history and result insertion leave links as ordinary text.
//! - Edits shift anchors before/after them and remove any anchor they touch.
//!   Undo/Redo snapshots carry the anchors, so only a real Undo restores one.
//! - The owner is the engine generation plus the actual draft ID (canvas UUID
//!   or task ID). A new owner starts with no anchors and no undo history.
//! - Trusted durable restores call `Composer::restore_native_input_envelope`
//!   or `Composer::hydrate_native_input_envelope`; profile/engine resets call
//!   `Composer::reset_native_picker_owner`.
use super::*;
use luna_proto::{
    ComposerCatalog, ComposerCatalogEntry, ComposerCatalogKind as Kind,
    ComposerCatalogStatus as Status, ComposerInput as NativeInput,
};

// Nested so the fixture can read picker state without widening its fields.
#[cfg(feature = "appshots-fixture")]
#[path = "integrations_e2e.rs"]
mod e2e;
#[cfg(feature = "appshots-fixture")]
pub use e2e::run_integrations_picker_e2e;

/// Owner of editor anchors and undo history. `draft_id` is the real canvas
/// UUID or task draft ID, never the empty canvas map key.
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

/// Bounded per-owner anchors kept only for in-memory navigation back to a
/// byte-identical draft. Reset with the picker on every engine/profile change.
const PARKED_NATIVE_EDITORS: usize = 32;

/// Exact anchors an editor held for one owner when navigation replaced its
/// text. They are reattached only to the same owner and identical bytes.
#[derive(Debug, Clone)]
pub(super) struct ParkedNativeEditor {
    owner: NativeEditorOwner,
    content: String,
    spans: Vec<NativeLink>,
}

/// Accessibility and activation state of one native popup row. Rendering and
/// fixture readback share this so assistive technology sees what users see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct NativeRowSemantics {
    pub label: String,
    pub description: String,
    pub enabled: bool,
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
    /// Keyboard focus target for Retry (Tab from the composer, Enter/Space).
    retry_focus: Option<FocusHandle>,
    parked: Vec<ParkedNativeEditor>,
    #[cfg(feature = "appshots-fixture")]
    fixture_catalog: bool,
    send_task: Option<Task<()>>,
    send_verified: bool,
}

impl ComposerInput {
    /// Bind anchors and undo history to one draft owner. A different owner
    /// starts with no anchors and no undo/redo: nothing selected for another
    /// draft, canvas or engine generation can be restored into this one.
    pub(super) fn bind_native_owner(&mut self, owner: NativeEditorOwner) -> bool {
        match &self.native_owner {
            Some(current) if *current == owner => return false,
            None => {
                // First binding of this editor. Anchors only exist under an
                // owner, so earlier history is plain text of this same
                // document: adopt it rather than dropping the first Undo.
                for snapshot in self.undo_stack.iter_mut().chain(&mut self.redo_stack) {
                    snapshot.native_owner = Some(owner.clone());
                }
                self.native_owner = Some(owner);
                self.native_spans.clear();
                return true;
            }
            Some(_) => {}
        }
        self.native_owner = Some(owner);
        self.clear_native_provenance();
        true
    }

    /// Drop every anchor and the undo/redo snapshots that could restore one.
    fn clear_native_provenance(&mut self) {
        self.native_spans.clear();
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.last_edit = None;
        self.refresh_projection();
        self.needs_measure = true;
    }

    /// Typed identities in document order, one per identity. Only an anchor
    /// whose bytes still hold its own canonical link counts.
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

    /// Called with the raw range about to be replaced by `inserted` bytes,
    /// BEFORE the content changes. Anchors wholly before the edit stay, anchors
    /// wholly after it shift, and any anchor the edit touches is removed.
    /// Inserted text never creates an anchor, even when it is a valid link.
    pub(super) fn apply_native_edit(&mut self, range: &Range<usize>, inserted: usize) {
        let removed = range.end - range.start;
        self.native_spans.retain_mut(|span| {
            if span.range.end <= range.start {
                return true;
            }
            if span.range.start >= range.end {
                span.range =
                    (span.range.start - removed + inserted)..(span.range.end - removed + inserted);
                return true;
            }
            false
        });
    }

    fn parked_native_state(&self) -> Option<ParkedNativeEditor> {
        let owner = self.native_owner.clone()?;
        (!self.native_spans.is_empty()).then(|| ParkedNativeEditor {
            owner,
            content: self.content.clone(),
            spans: self.native_spans.clone(),
        })
    }

    /// Reattach parked anchors only for the bound owner and identical bytes.
    fn unpark_native_state(&mut self, parked: ParkedNativeEditor) -> bool {
        if self.native_owner.as_ref() != Some(&parked.owner)
            || self.content != parked.content
            || parked.spans.iter().any(|span| {
                self.content.get(span.range.clone()) != Some(native_link(&span.item).as_str())
            })
        {
            return false;
        }
        self.native_spans = parked.spans;
        self.refresh_projection();
        self.needs_measure = true;
        true
    }

    /// Programmatic replacement that only appends (for example an explicit
    /// result handoff). Anchors in the unchanged prefix keep their identity;
    /// the appended text stays ordinary text.
    pub(super) fn set_text_keeping_native_prefix(
        &mut self,
        text: impl Into<String>,
        cx: &mut Context<Self>,
    ) {
        let prefix = self.content.clone();
        let spans = std::mem::take(&mut self.native_spans);
        self.set_text(text, cx);
        if self.content.starts_with(&prefix) && !spans.is_empty() {
            self.native_spans = spans;
            self.refresh_projection();
        }
    }

    /// Trusted envelope hydration: anchor each typed item to its first
    /// canonical link in `text`, appending a visible link for an item whose
    /// link is missing, so no identity is ever hidden. Clears undo/redo like
    /// any document replacement. Returns whether the text changed (and an
    /// `Edited` event was emitted).
    fn set_text_with_native_inputs(
        &mut self,
        mut text: String,
        items: Vec<NativeInput>,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.single_line {
            text = text.replace(['\r', '\n'], " ");
        }
        let links = native_links(&text);
        let mut spans: Vec<NativeLink> = Vec::new();
        for item in items {
            if spans.iter().any(|span| span.item == item) {
                continue;
            }
            if let Some(link) = links.iter().find(|link| link.item == item) {
                spans.push(link.clone());
            } else {
                if !text.is_empty() && !text.ends_with(char::is_whitespace) {
                    text.push(' ');
                }
                let start = text.len();
                text.push_str(&native_link(&item));
                spans.push(NativeLink {
                    range: start..text.len(),
                    item,
                });
            }
        }
        let changed = self.content != text;
        if changed {
            self.set_text(text, cx);
        } else {
            self.invalidate_mention_tooltip();
            self.marked_range = None;
            let end = self.content.len();
            self.selected_range = end..end;
            self.selection_reversed = false;
            self.follow_cursor = true;
        }
        self.clear_native_provenance();
        spans.sort_by_key(|span| span.range.start);
        self.native_spans = spans;
        self.refresh_projection();
        cx.notify();
        changed
    }

    /// Picker acceptance: the only non-restore path that creates an anchor.
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
        let link_len = link.len();
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
            range: range.start..range.start + link_len,
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
        } else {
            let space = state.selected_space_row()?;
            params.insert("spaceId".into(), space.id.clone().into());
            params.insert("targetDeviceId".into(), space.device_id.clone().into());
            if let crate::pickers::CheckoutPlan::ReuseWorktree { path, .. } =
                self.pickers.read(cx).checkout_plan()
            {
                params.insert("path".into(), path.into());
            }
        }
        params.insert(
            "providerAccountId".into(),
            serde_json::to_value(config.provider_account_id).ok()?,
        );
        let key = serde_json::Value::Object(params.clone()).to_string();
        params.insert("refresh".into(), refresh.into());
        Some((key, serde_json::Value::Object(params)))
    }

    /// The actual draft owner: engine generation plus the real canvas UUID
    /// (never the shared empty canvas key) or the task's draft ID.
    fn native_editor_owner(&self, cx: &App) -> NativeEditorOwner {
        NativeEditorOwner {
            engine_generation: self.state.read(cx).engine_generation(),
            draft_id: self.draft_id_for(&self.current_key),
        }
    }

    /// Remember the editor's exact anchors under its current owner before
    /// navigation replaces the text.
    pub(super) fn park_native_editor(&mut self, cx: &App) {
        let Some(parked) = self.input.read(cx).parked_native_state() else {
            return;
        };
        let parked_list = &mut self.integrations.parked;
        parked_list.retain(|entry| entry.owner != parked.owner);
        parked_list.push(parked);
        if parked_list.len() > PARKED_NATIVE_EDITORS {
            parked_list.remove(0);
        }
    }

    fn store_native_selections(&mut self, selected: Vec<NativeInput>) {
        if selected.is_empty() {
            self.input_items.remove(&self.current_key);
        } else {
            self.input_items.insert(self.current_key.clone(), selected);
        }
    }

    /// Bind the editor to the current owner. On an owner change the typed map
    /// entry for the current key is NOT trusted (the empty canvas key is shared
    /// by every canvas UUID): it is rebuilt only from exact parked anchors for
    /// this owner, otherwise cleared until a trusted restore hydrates it.
    pub(super) fn ensure_native_editor_owner(&mut self, cx: &mut Context<Self>) {
        let owner = self.native_editor_owner(cx);
        if self.input.read(cx).native_owner.as_ref() == Some(&owner) {
            return;
        }
        self.park_native_editor(cx);
        let parked = self
            .integrations
            .parked
            .iter()
            .position(|entry| entry.owner == owner)
            .map(|index| self.integrations.parked.remove(index));
        self.input.update(cx, |input, _| {
            input.bind_native_owner(owner);
            if let Some(parked) = parked {
                input.unpark_native_state(parked);
            }
        });
        let selected = self.input.read(cx).native_input_items();
        self.store_native_selections(selected);
    }

    /// Navigation just replaced the editor text with the destination draft.
    /// Bind its owner; exact parked anchors return when the bytes and typed
    /// envelope still match, otherwise the destination's typed envelope
    /// (`input_items[current_key]`, written only by anchored reconcile or
    /// native-inputs' trusted restores, and cleared on every engine reset)
    /// is hydrated. Raw link text alone never grants an identity.
    pub(super) fn restore_native_after_navigation(&mut self, cx: &mut Context<Self>) {
        let items = self
            .input_items
            .get(&self.current_key)
            .cloned()
            .unwrap_or_default();
        self.ensure_native_editor_owner(cx);
        if self.input.read(cx).native_input_items() != items {
            let text = self.input.read(cx).text().to_owned();
            self.restore_native_input_envelope(text, items, cx);
        }
    }

    /// Trusted durable restore (draft load, queue edit, failed-send/rejection
    /// recovery, stash). Call only after `current_key`/`canvas_draft_id` name
    /// the draft that owns this envelope. Replaces the editor text, anchors
    /// each item to its canonical link (appending a visible link if missing),
    /// clears undo/redo and writes `input_items[current_key]`. Never call it
    /// for user edits, paste, prompt history or result insertion.
    pub(crate) fn restore_native_input_envelope(
        &mut self,
        text: String,
        items: Vec<NativeInput>,
        cx: &mut Context<Self>,
    ) {
        self.ensure_native_editor_owner(cx);
        let changed = self.input.update(cx, |input, cx| {
            input.set_text_with_native_inputs(text, items, cx)
        });
        if changed {
            // The restore is not a user edit; its `Edited` must not dirty the draft.
            self.suppress_next_draft_edit = true;
        }
        self.reconcile_native_selections(cx);
    }

    /// Trusted restore from state already assembled for the current key: the
    /// editor text as set (for example by `set_input_text_without_draft_edit`)
    /// plus `input_items[current_key]`. Same semantics as
    /// `restore_native_input_envelope`; emits no second `Edited` when every
    /// item's link is already present.
    pub(crate) fn hydrate_native_input_envelope(&mut self, cx: &mut Context<Self>) {
        let items = self
            .input_items
            .get(&self.current_key)
            .cloned()
            .unwrap_or_default();
        let text = self.input.read(cx).text().to_owned();
        self.restore_native_input_envelope(text, items, cx);
    }

    /// Engine/profile boundary: drop catalog rows, pending catalog/send tasks,
    /// send verification, parked anchors, editor anchors and undo/redo, then
    /// bind the editor to the current owner. Callers keep clearing their own
    /// durable maps (`input_items`) as before.
    pub(crate) fn reset_native_picker_owner(&mut self, cx: &mut Context<Self>) {
        let generation = self.integrations.generation.wrapping_add(1);
        self.integrations = IntegrationPicker {
            generation,
            ..Default::default()
        };
        self.mention.native_results.clear();
        let owner = self.native_editor_owner(cx);
        self.input.update(cx, |input, _| {
            input.native_owner = Some(owner);
            input.clear_native_provenance();
        });
        self.input_items.remove(&self.current_key);
    }

    /// Mirror the editor's anchored identities into the typed map. This is the
    /// only way live edits change `input_items`; raw link text is never parsed.
    pub(super) fn reconcile_native_selections(&mut self, cx: &mut Context<Self>) {
        self.ensure_native_editor_owner(cx);
        let selected = self.input.read(cx).native_input_items();
        self.store_native_selections(selected);
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
        // Auto-select the first row only. Retry is reached explicitly (arrows,
        // Tab, pointer or AT) so Enter never retries when no row exists.
        let rows = self.mention.results.len() + self.mention.native_results.len();
        if self
            .mention
            .active
            .is_none_or(|active| active >= self.mention_item_count())
        {
            self.mention.active = (rows > 0).then_some(0);
        }
    }

    /// Retry is shown (and keyboard-reachable) whenever the popup reports a
    /// native catalog status: loading, failure, stale, unavailable or empty.
    pub(super) fn native_retry_visible(&self) -> bool {
        self.native_catalog_notice().is_some()
    }

    /// Keyboard order matches visual order: file rows, native rows, Retry.
    pub(super) fn mention_item_count(&self) -> usize {
        self.mention.results.len()
            + self.mention.native_results.len()
            + usize::from(self.native_retry_visible())
    }

    pub(super) fn native_retry_index(&self) -> Option<usize> {
        self.native_retry_visible()
            .then(|| self.mention.results.len() + self.mention.native_results.len())
    }

    pub(super) fn retry_focus_handle(&mut self, cx: &mut Context<Self>) -> FocusHandle {
        self.integrations
            .retry_focus
            .get_or_insert_with(|| cx.focus_handle())
            .clone()
    }

    pub(super) fn ensure_native_catalog(&mut self, fresh_open: bool, cx: &mut Context<Self>) {
        #[cfg(feature = "appshots-fixture")]
        if self.integrations.fixture_catalog {
            self.update_native_rows();
            return;
        }
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
        let engine_generation = self.state.read(cx).engine_generation();
        self.integrations.task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(methods::LIST_COMPOSER_CATALOG, params)
                .await;
            this.update(cx, |composer, cx| {
                // Rows belong to the execution scope, not to one draft. A retry,
                // scope change or engine/profile reset supersedes this response;
                // draft-owned state below is recomputed for the current owner.
                if composer.integrations.generation != generation
                    || composer.state.read(cx).engine_generation() != engine_generation
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
                composer.reconcile_native_selections(cx);
                if let Some(items) = composer.input_items.get(&composer.current_key)
                    && !items.is_empty()
                    && composer
                        .integrations
                        .catalog
                        .as_ref()
                        .is_none_or(|catalog| {
                            catalog.stale
                                || items.iter().any(|item| {
                                    let (_, path) = identity(item);
                                    !catalog.entries.iter().any(|entry| entry.path == path)
                                })
                        })
                {
                    composer.failure = Some(AVAILABILITY_CHANGED.into());
                    composer.failure_key = Some(composer.current_key.clone());
                } else if composer.failure.as_deref() == Some(AVAILABILITY_CHANGED)
                    && composer.failure_key.as_deref() == Some(composer.current_key.as_str())
                {
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
        self.sync_mention_controls(cx);
        cx.notify();
    }

    /// Stale, loading or not-ready rows are visible but cannot be selected.
    pub(super) fn native_entry_enabled(&self, entry: &ComposerCatalogEntry) -> bool {
        !self.integrations.loading
            && self.integrations.catalog.as_ref().is_some_and(|catalog| {
                !catalog.stale
                    && catalog
                        .slices
                        .iter()
                        .any(|slice| slice.kind == entry.kind && slice.status == Status::Ready)
            })
    }

    /// Role-independent accessible name/description for native row `index`.
    /// Duplicate names stay distinguishable by kind, and by exact ID when the
    /// kind also matches; the description always carries the exact identity.
    pub(super) fn native_row_semantics(&self, index: usize) -> Option<NativeRowSemantics> {
        let entry = self.mention.native_results.get(index)?;
        let kind = kind_singular(entry.kind);
        let duplicate = self
            .mention
            .native_results
            .iter()
            .enumerate()
            .any(|(other, row)| other != index && row.kind == entry.kind && row.name == entry.name);
        let label = if duplicate {
            format!("{kind} {} ({})", entry.name, entry.id)
        } else {
            format!("{kind} {}", entry.name)
        };
        let enabled = self.native_entry_enabled(entry);
        let mut description = entry.path.clone();
        if let Some(detail) = entry.description.as_deref().filter(|text| !text.is_empty()) {
            description.push_str(". ");
            description.push_str(detail);
        }
        if !enabled {
            description.push_str(". Unavailable: ");
            description.push_str(if self.integrations.loading {
                "discovery in progress"
            } else if self.integrations.catalog.as_ref().is_some_and(|c| c.stale) {
                "availability could not be verified; retry discovery"
            } else {
                "this integration type is unavailable on the execution host"
            });
        }
        Some(NativeRowSemantics {
            label,
            description,
            enabled,
        })
    }

    pub(super) fn accept_native_mention(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(entry) = self.mention.native_results.get(index).cloned() else {
            return;
        };
        if !self.native_entry_enabled(&entry) {
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
        self.reconcile_native_selections(cx);
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
        let submitted_text = text.to_owned();
        let raw = self.input.read(cx).text().to_owned();
        // Dropped (and so cancelled) by `reset_native_picker_owner`.
        self.integrations.send_task = Some(cx.spawn(async move |this, cx| {
            let result = engine.client().call(methods::LIST_COMPOSER_CATALOG, params).await;
            this.update(cx, |composer, cx| {
                // Only one check is ever in flight; release it before any fence
                // so a stale result can never block the next send.
                composer.integrations.send_task = None;
                if composer.native_editor_owner(cx) != owner
                    || composer.current_key != key
                    || composer.input.read(cx).text() != raw
                    || composer.input_items.get(&key) != Some(&items)
                    || composer.native_catalog_request(true, cx).as_ref().map(|(key, _)| key) != Some(&scope)
                {
                    cx.notify();
                    return;
                }
                let catalog = result.ok().and_then(|value| serde_json::from_value::<ComposerCatalog>(value).ok());
                let valid = catalog.as_ref().is_some_and(|catalog| !catalog.stale && items.iter().all(|item| {
                    let (_, path) = identity(item);
                    catalog.entries.iter().any(|entry| entry.path == path)
                }));
                if valid {
                    composer.integrations.generation = composer.integrations.generation.wrapping_add(1);
                    composer.integrations.task = None;
                    composer.integrations.loading = false;
                    composer.integrations.error = None;
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

const AVAILABILITY_CHANGED: &str = "Selected integration availability changed. Your selections and draft are preserved; verify access before sending.";

pub(super) fn kind_label(kind: Kind) -> &'static str {
    match kind {
        Kind::Plugin => "Plugins",
        Kind::App => "Apps",
        Kind::Skill => "Skills",
    }
}

pub(super) fn kind_singular(kind: Kind) -> &'static str {
    match kind {
        Kind::Plugin => "Plugin",
        Kind::App => "App",
        Kind::Skill => "Skill",
    }
}
