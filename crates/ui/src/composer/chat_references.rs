//! `#` completion is local navigation/context. It never creates a provider
//! tool grant: the durable text is an exact, workspace-scoped conversation URL.
use super::chat_reference_search::{
    ChatReferenceGroup, ChatReferenceIndex, ChatReferenceKind, ChatReferenceMatch,
    ChatReferenceRecord, DEFAULT_LIMIT_PER_GROUP,
};
use super::*;
use gpui::AnyElement;
use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicU64, Ordering},
};

#[cfg(feature = "appshots-fixture")]
#[path = "chat_references_e2e.rs"]
mod e2e;
#[cfg(feature = "appshots-fixture")]
pub use e2e::run_chat_references_e2e;

pub(crate) struct ChatReferenceCache {
    engine: u64,
    version: u64,
    records: Vec<ChatReferenceRecord>,
    index: OnceLock<Arc<ChatReferenceIndex>>,
    builds: AtomicU64,
}

impl AppState {
    pub(crate) fn invalidate_chat_references(&mut self) {
        self.chat_reference_version = self.chat_reference_version.wrapping_add(1);
        self.chat_reference_cache = None;
    }

    fn reference_catalog(&mut self) -> Arc<ChatReferenceCache> {
        if let Some(cache) = &self.chat_reference_cache
            && cache.engine == self.engine_generation()
            && cache.version == self.chat_reference_version
        {
            return cache.clone();
        }
        let projects: HashMap<&str, &luna_proto::Space> = self
            .spaces
            .iter()
            .map(|space| (space.id.as_str(), space))
            .collect();
        // Side chats may be spaceless rows. Resolve their source project once
        // per catalog snapshot with memoized, cycle-safe lineage walking.
        // Known projects are seeded first; every unresolved edge is visited
        // at most once, including deeply nested side chats.
        let mut parents: HashMap<&str, &str> = self
            .chats
            .iter()
            .filter_map(|chat| {
                chat.child_thread
                    .as_ref()
                    .map(|child| (chat.id.as_str(), child.parent_task_id.as_str()))
            })
            .collect();
        parents.extend(
            self.fork_source_rows
                .iter()
                .map(|(id, parent)| (id.as_str(), parent.as_str())),
        );
        parents.extend(
            self.side_chat_rows
                .iter()
                .map(|(id, row)| (id.as_str(), row.parent_id.as_str())),
        );
        let mut resolved: HashMap<&str, Option<&str>> = self
            .chats
            .iter()
            .filter_map(|chat| {
                chat.space_id
                    .as_deref()
                    .map(|space| (chat.id.as_str(), Some(space)))
            })
            .collect();
        for chat in &self.chats {
            let mut path = Vec::new();
            let mut seen = std::collections::HashSet::new();
            let mut cursor = chat.id.as_str();
            let project = loop {
                if let Some(project) = resolved.get(cursor) {
                    break *project;
                }
                if !seen.insert(cursor) {
                    break None;
                }
                path.push(cursor);
                match parents.get(cursor) {
                    Some(parent) => cursor = parent,
                    None => break None,
                }
            };
            for id in path {
                resolved.insert(id, project);
            }
        }
        let records = self
            .chats
            .iter()
            .map(|chat| {
                let kind = if self.side_chat_rows.contains_key(&chat.id)
                    || self.fork_source_rows.contains_key(&chat.id)
                {
                    ChatReferenceKind::SideChat
                } else if chat.child_thread.is_some() {
                    ChatReferenceKind::NewThread
                } else {
                    ChatReferenceKind::Thread
                };
                let project_id = resolved
                    .get(chat.id.as_str())
                    .copied()
                    .flatten()
                    .map(str::to_owned);
                ChatReferenceRecord {
                    id: chat.id.clone(),
                    title: chat.title.clone().unwrap_or_else(|| "Untitled chat".into()),
                    project_label: project_id.as_deref().and_then(|id| projects.get(id)).map(
                        |space| {
                            space.name.clone().unwrap_or_else(|| {
                                std::path::Path::new(&space.path)
                                    .file_name()
                                    .map(|s| s.to_string_lossy().into_owned())
                                    .unwrap_or_else(|| space.path.clone())
                            })
                        },
                    ),
                    project_id,
                    harness: chat.config.as_ref().map(|config| config.harness),
                    kind,
                    parent_id: (kind == ChatReferenceKind::NewThread)
                        .then(|| {
                            parents
                                .get(chat.id.as_str())
                                .map(|parent| (*parent).to_owned())
                        })
                        .flatten(),
                    source_id: (kind == ChatReferenceKind::SideChat)
                        .then(|| {
                            parents
                                .get(chat.id.as_str())
                                .map(|parent| (*parent).to_owned())
                        })
                        .flatten(),
                    archived: chat.archived,
                    updated_at_ms: chat
                        .last_message_at
                        .unwrap_or(chat.created_at)
                        .timestamp_millis(),
                }
            })
            .collect();
        let cache = Arc::new(ChatReferenceCache {
            engine: self.engine_generation(),
            version: self.chat_reference_version,
            records,
            index: OnceLock::new(),
            builds: AtomicU64::new(0),
        });
        self.chat_reference_cache = Some(cache.clone());
        cache
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Scope {
    engine: u64,
    version: u64,
    chat: Option<String>,
    project: Option<String>,
}

pub(super) struct ChatReferenceState {
    pub token: Option<MentionToken>,
    pub items: Vec<ChatReferenceMatch>,
    pub active: Option<usize>,
    pub loading: bool,
    scope: Option<Scope>,
    dismissed: Option<(Range<usize>, String)>,
    generation: Arc<AtomicU64>,
    task: Option<Task<()>>,
    pub(super) scroll: gpui::ScrollHandle,
}
impl ChatReferenceState {
    /// What the popup shows, for its exit copy (audit CP-11): no task, no
    /// scope, no dismissal memory.
    pub(super) fn exit_copy(&self) -> Self {
        Self {
            token: self.token.clone(),
            items: self.items.clone(),
            active: self.active,
            loading: self.loading,
            scroll: self.scroll.clone(),
            ..Self::default()
        }
    }
}

impl Default for ChatReferenceState {
    fn default() -> Self {
        Self {
            token: None,
            items: Vec::new(),
            active: None,
            loading: false,
            scope: None,
            dismissed: None,
            generation: Arc::new(AtomicU64::new(0)),
            task: None,
            scroll: gpui::ScrollHandle::new(),
        }
    }
}

pub(super) fn chat_token(text: &str, cursor: usize) -> Option<MentionToken> {
    if cursor > text.len() || !text.is_char_boundary(cursor) {
        return None;
    }
    let prefix = &text[..cursor];
    let line_start = prefix.rfind('\n').map_or(0, |at| at + 1);
    // A completion is not a Markdown heading, fenced/inline code or URL fragment.
    if prefix[..line_start]
        .lines()
        .filter(|line| line.trim_start().starts_with("```"))
        .count()
        % 2
        != 0
    {
        return None;
    }
    let at = prefix[line_start..].rfind('#')? + line_start;
    if at > 0
        && !text[..at]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_whitespace() || matches!(c, '(' | '{'))
    {
        return None;
    }
    if text[line_start..at].chars().filter(|c| *c == '`').count() % 2 != 0 {
        return None;
    }
    let query = &text[at + 1..cursor];
    if query.len() > 192
        || query.starts_with(char::is_whitespace)
        || query
            .chars()
            .any(|c| matches!(c, '#' | '@' | '[' | ']' | '(' | ')' | '`' | '\r' | '\n'))
    {
        return None;
    }
    // Mid-word cursor edits replace the rest of that word, not following prose.
    let end = text[cursor..]
        .char_indices()
        .find_map(|(offset, c)| {
            (c.is_whitespace() || matches!(c, ',' | '.' | '!' | '?' | ';' | ')' | ']'))
                .then_some(cursor + offset)
        })
        .unwrap_or(text.len());
    Some(MentionToken {
        range: at..end,
        query: query.into(),
    })
}

pub(super) fn chat_links(raw: &str) -> Vec<FileMentionLink> {
    if !raw.contains("luna://open/chat/") {
        return Vec::new();
    }
    let mut links = Vec::new();
    let mut search = 0;
    while let Some(start) = raw[search..].find("[#").map(|offset| search + offset) {
        let Some(label_end) = label_close(raw, start + 1) else {
            search = start + 2;
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
        let Ok(link) = crate::links::parse_luna_conversation_link(target) else {
            continue;
        };
        if link.chat_id.is_empty()
            || link.chat_id.len() > 256
            || link.chat_id.chars().any(char::is_control)
            || link.workspace.is_empty()
            || link.workspace.len() > 128
            || link.workspace.chars().any(char::is_control)
            || crate::links::luna_conversation_link(&link.chat_id, &link.workspace) != target
        {
            continue;
        }
        let label = &raw[start + 2..label_end];
        let mut title = String::new();
        let mut chars = label.chars();
        while let Some(ch) = chars.next() {
            title.push(if ch == '\\' {
                chars.next().unwrap_or(ch)
            } else {
                ch
            });
        }
        if title.is_empty()
            || title.chars().any(char::is_control)
            || escape_mention_label(&title) != label
        {
            continue;
        }
        links.push(FileMentionLink {
            range: start..end,
            basename: title,
            path: target.into(),
            is_dir: false,
        });
    }
    links
}

impl ComposerInput {
    fn replace_chat_reference(
        &mut self,
        range: Range<usize>,
        title: &str,
        target: &str,
        cx: &mut Context<Self>,
    ) {
        if self.read_only || range.end > self.content.len() {
            return;
        }
        let link = format!("[#{}]({target})", escape_mention_label(title));
        let separator = self.content[range.end..]
            .chars()
            .next()
            .filter(|c| c.is_whitespace() && !matches!(c, '\r' | '\n'));
        let inserted = if separator.is_some() {
            link
        } else {
            format!("{link} ")
        };
        self.invalidate_mention_tooltip();
        self.record_edit(&range, &inserted);
        self.apply_native_edit(&range, inserted.len());
        self.content.replace_range(range.clone(), &inserted);
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
    fn reference_scope(&self, cx: &App) -> Scope {
        let state = self.state.read(cx);
        Scope {
            engine: state.engine_generation(),
            version: state.chat_reference_version,
            chat: self.binding.chat_id(state).map(str::to_owned),
            project: self.binding.space_id(state),
        }
    }
    pub(super) fn reset_chat_references(
        &mut self,
        dismissed: Option<(Range<usize>, String)>,
        cx: &mut Context<Self>,
    ) {
        self.chat_reference
            .generation
            .fetch_add(1, Ordering::Relaxed);
        self.chat_reference = ChatReferenceState {
            dismissed,
            ..Default::default()
        };
        self.sync_mention_controls(cx);
    }
    pub(super) fn refresh_chat_reference_source(&mut self, cx: &mut Context<Self>) {
        if self.chat_reference.token.is_none() {
            return;
        }
        if self.chat_reference.scope.as_ref() != Some(&self.reference_scope(cx)) {
            let (text, cursor) = {
                let input = self.input.read(cx);
                (input.text().to_owned(), input.cursor_offset())
            };
            self.update_chat_references(&text, cursor, cx);
        }
    }
    pub(super) fn update_chat_references(
        &mut self,
        text: &str,
        cursor: usize,
        cx: &mut Context<Self>,
    ) {
        let token = chat_token(text, cursor);
        let scope = self.reference_scope(cx);
        if token.as_ref().is_some_and(|token| {
            self.chat_reference
                .dismissed
                .as_ref()
                .is_some_and(|(range, value)| {
                    token.range == *range && text.get(range.clone()) == Some(value.as_str())
                })
        }) {
            return;
        }
        if token == self.chat_reference.token && self.chat_reference.scope.as_ref() == Some(&scope)
        {
            return;
        }
        if token.is_none() {
            self.reset_chat_references(None, cx);
            return;
        }
        let request = self
            .chat_reference
            .generation
            .fetch_add(1, Ordering::Relaxed)
            + 1;
        self.chat_reference.task = None;
        if self.chat_reference.scope.as_ref() != Some(&scope) || self.chat_reference.token.is_none()
        {
            self.chat_reference.items.clear();
            self.chat_reference.active = None;
            crate::popover::reset_menu_scroll(&self.chat_reference.scroll, &mut self.popup_bar);
        }
        self.chat_reference.token = token.clone();
        self.chat_reference.scope = Some(scope.clone());
        self.chat_reference.dismissed = None;
        self.chat_reference.loading = true;
        let token = token.expect("checked");
        let generation = self.chat_reference.generation.clone();
        let cache = self.state.update(cx, |state, _| state.reference_catalog());
        self.chat_reference.task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(45))
                .await;
            let scope_for_search = scope.clone();
            let query = token.query.clone();
            let results = cx
                .background_executor()
                .spawn(async move {
                    if generation.load(Ordering::Relaxed) != request {
                        return None;
                    }
                    let index = cache.index.get_or_init(|| {
                        cache.builds.fetch_add(1, Ordering::Relaxed);
                        Arc::new(ChatReferenceIndex::new(cache.records.clone()))
                    });
                    let cancelled = || generation.load(Ordering::Relaxed) != request;
                    let result = index.search(
                        &query,
                        scope_for_search.chat.as_deref(),
                        scope_for_search.project.as_deref(),
                        DEFAULT_LIMIT_PER_GROUP,
                        Some(&cancelled),
                    );
                    (!cancelled()).then_some(result)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.chat_reference.generation.load(Ordering::Relaxed) != request
                    || this.reference_scope(cx) != scope
                    || this.chat_reference.token.as_ref() != Some(&token)
                {
                    return;
                }
                let Some(results) = results else {
                    return;
                };
                this.chat_reference.items = results
                    .related
                    .into_iter()
                    .chain(results.project)
                    .chain(results.other)
                    .collect();
                this.chat_reference.active = (!this.chat_reference.items.is_empty()).then_some(0);
                this.chat_reference.loading = false;
                this.sync_mention_controls(cx);
                cx.notify();
            });
        }));
        self.sync_mention_controls(cx);
        cx.notify();
    }
    pub(super) fn move_chat_reference(&mut self, delta: isize, cx: &mut Context<Self>) {
        self.chat_reference.active = crate::popover::menu_step(
            self.chat_reference.active,
            self.chat_reference.items.len(),
            delta,
        );
        if let Some(active) = self.chat_reference.active {
            let mut prior = None;
            let mut headers = 0;
            for item in self.chat_reference.items.iter().take(active + 1) {
                if prior != Some(item.group) {
                    headers += 1;
                    prior = Some(item.group);
                }
            }
            self.chat_reference.scroll.scroll_to_item(active + headers);
        }
        self.sync_mention_controls(cx);
        cx.notify();
    }
    pub(super) fn dismiss_chat_references(&mut self, cx: &mut Context<Self>) {
        let dismissed = self.chat_reference.token.as_ref().and_then(|token| {
            self.input
                .read(cx)
                .text()
                .get(token.range.clone())
                .map(|text| (token.range.clone(), text.to_owned()))
        });
        self.reset_chat_references(dismissed, cx);
        cx.notify();
    }
    pub(super) fn accept_chat_reference(&mut self, cx: &mut Context<Self>) {
        if self.chat_reference.loading
            || self.chat_reference.scope.as_ref() != Some(&self.reference_scope(cx))
        {
            return;
        }
        let Some(token) = self.chat_reference.token.clone() else {
            return;
        };
        let input = self.input.read(cx);
        if chat_token(input.text(), input.cursor_offset()).as_ref() != Some(&token) {
            return;
        }
        let Some(item) = self
            .chat_reference
            .active
            .and_then(|active| self.chat_reference.items.get(active))
        else {
            return;
        };
        let state = self.state.read(cx);
        let Some(chat) = state.chats.iter().find(|chat| chat.id == item.record.id) else {
            return;
        };
        let Some(workspace) = crate::links::workspace_locator(
            state.workspace_scope,
            state.auth.as_ref(),
            state.local_device_id.as_deref(),
        ) else {
            return;
        };
        let title = luna_proto::view::single_line(chat.title.as_deref().unwrap_or("Untitled chat"));
        let target = crate::links::luna_conversation_link(&chat.id, &workspace);
        self.input.update(cx, |input, cx| {
            input.replace_chat_reference(token.range, &title, &target, cx)
        });
        self.reset_chat_references(None, cx);
        cx.notify();
    }
    /// The `#` popup in the composer's completion family: the file/slash
    /// popups' card, rows and spacing (`popover::menu_row`, `MENU_GAP`,
    /// `menu_heading`), one compact row per chat — provider mark, title,
    /// restrained project metadata — never a two-line or full-bleed band.
    pub(super) fn render_chat_reference_popup(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.chat_reference.token.is_some() {
            self.completion_exits.chat_reference = None;
            let popup = self.render_chat_reference_popup_body(theme, None, cx);
            self.completion_exits.chat_reference_last = Some(self.chat_reference.exit_copy());
            return popup;
        }
        if let Some(last) = self.completion_exits.chat_reference_last.take()
            && self.completion_slot_free()
        {
            self.completion_exits.chat_reference = Some((last, std::time::Instant::now()));
            self.schedule_completion_exit_reap(cx);
        }
        let since = self.completion_exits.chat_reference.as_ref()?.1;
        if !completion_exit_live(since) || !self.completion_slot_free() {
            self.completion_exits.chat_reference = None;
            return None;
        }
        let held = self.completion_exits.chat_reference.as_ref()?.0.exit_copy();
        let live = std::mem::replace(&mut self.chat_reference, held);
        let popup = self.render_chat_reference_popup_body(theme, Some(since), cx);
        self.chat_reference = live;
        popup
    }

    fn render_chat_reference_popup_body(
        &mut self,
        theme: &Theme,
        closing: Option<std::time::Instant>,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        self.chat_reference.token.as_ref()?;
        let theme = theme.for_popup();
        let mut card = crate::popover::stack_popup_card(&theme)
            .w_full()
            .max_h(crate::typography::ui_rems(320.0))
            .overflow_hidden()
            // Completion choices belong to the input: keep it focused until
            // mouse-up can accept a choice.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| window.focus(&this.input.focus_handle(cx), cx)),
            )
            .when(closing.is_none(), |card| {
                card.on_mouse_down_out(
                    cx.listener(|this, _, _, cx| this.dismiss_chat_references(cx)),
                )
            });
        if self.chat_reference.items.is_empty() {
            card = card.child(if self.chat_reference.loading {
                crate::popover::skeleton_rows(
                    "chat-reference-loading",
                    &theme,
                    3,
                    cx.entity_id(),
                    cx,
                )
            } else {
                div()
                    .px(crate::typography::ui_rems(12.0))
                    .py(crate::typography::ui_rems(10.0))
                    .text_size(crate::typography::ui_rems(12.0))
                    .text_color(theme.text_muted)
                    .child("No matching chats")
                    .into_any_element()
            });
        } else {
            let count = self.chat_reference.items.len();
            // One list child per heading and per row: `move_chat_reference`
            // scrolls by (row + headings so far).
            let mut rows: Vec<AnyElement> = Vec::with_capacity(count + 3);
            let mut previous = None;
            for (index, item) in self.chat_reference.items.iter().enumerate() {
                if previous != Some(item.group) {
                    let heading = match item.group {
                        ChatReferenceGroup::Related => "Related chats",
                        ChatReferenceGroup::Project => "This project",
                        ChatReferenceGroup::Other => "Other projects",
                    };
                    rows.push(
                        div()
                            .id(("chat-reference-heading", index))
                            .role(gpui::Role::Heading)
                            .aria_label(heading)
                            .flex()
                            .flex_col()
                            .when(previous.is_some(), |group| {
                                group.child(crate::popover::menu_separator())
                            })
                            .child(crate::popover::menu_heading(&theme, heading))
                            .into_any_element(),
                    );
                    previous = Some(item.group);
                }
                rows.push(self.chat_reference_row(index, count, item, &theme, cx));
            }
            card = card.child(
                div()
                    .id("chat-reference-scroll-wrap")
                    .relative()
                    .on_hover(cx.listener(Self::on_popup_list_hover))
                    .child(
                        div()
                            .id("chat-reference-list")
                            .role(gpui::Role::ListBox)
                            .aria_label("Chat references")
                            .max_h(crate::typography::ui_rems(312.0))
                            .flex()
                            .flex_col()
                            .gap(px(crate::popover::MENU_GAP))
                            .overflow_y_scroll()
                            .track_scroll(&self.chat_reference.scroll)
                            .children(rows),
                    )
                    .children(crate::popover::rail(
                        self,
                        "chat-reference-scrollbar",
                        &theme,
                        cx,
                    )),
            );
        }
        Some(crate::popover::full_width_menu_above(
            "chat-reference-popup",
            card.into_any_element(),
            closing,
        ))
    }

    /// One compact single-line option, shaped like a file-mention row: the
    /// provider's brand mark, the chat title in the menu's own type, its
    /// conversation type (Thread / New thread / Side chat) as a quiet badge
    /// that never truncates, then the project (and archived state) as
    /// trailing metadata — the first thing to give way. The type leads the
    /// accessible description; the provider name stays there too.
    fn chat_reference_row(
        &self,
        index: usize,
        count: usize,
        item: &ChatReferenceMatch,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selected = self.chat_reference.active == Some(index);
        let record = &item.record;
        let provider = record
            .harness
            .map(crate::shell::child_chats::provider_label)
            .unwrap_or("Chat");
        let project = record.project_label.as_deref().unwrap_or("Unassigned");
        let kind = record.kind.label();
        let detail = format!(
            "{kind}: {} {project} · {provider}{}",
            record.kind.description(),
            if record.archived { " · Archived" } else { "" }
        );
        let metadata = if record.archived {
            format!("{project} · Archived")
        } else {
            project.to_owned()
        };
        let (mark, tint) = record
            .harness
            .map(crate::pickers::harness_brand_icon)
            .unwrap_or((crate::icons::CHAT_ROUND_LINE, None));
        crate::popover::menu_row_cursor(theme, selected)
            .id(("chat-reference-option", index))
            // The pointer and the arrow keys share one cursor (audit CP-13).
            .on_mouse_move(cx.listener(move |this, _: &gpui::MouseMoveEvent, _, cx| {
                if this.chat_reference.active != Some(index) {
                    this.chat_reference.active = Some(index);
                    cx.notify();
                }
            }))
            .role(gpui::Role::ListBoxOption)
            .aria_label(format!("#{}", record.title))
            .aria_description(detail)
            .aria_selected(selected)
            .aria_position_in_set(index + 1)
            .aria_size_of_set(count)
            .when(selected, |row| row.aria_active_descendant())
            .min_w_0()
            .gap(crate::typography::ui_rems(8.0))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.chat_reference.active = Some(index);
                this.accept_chat_reference(cx);
            }))
            .child(
                crate::icons::icon(mark)
                    .size(crate::typography::ui_rems(14.0))
                    .flex_none()
                    .text_color(tint.unwrap_or(theme.text_muted)),
            )
            .child(
                // The title leads; a long one truncates before it can crowd
                // out the type badge or the project entirely.
                div()
                    .min_w_0()
                    .max_w(gpui::relative(0.66))
                    .truncate()
                    .text_color(theme.text)
                    .child(record.title.clone()),
            )
            .child(conversation_type_badge(theme, kind))
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .truncate()
                    .text_size(crate::typography::ui_rems(12.0))
                    .text_color(theme.text_muted)
                    .child(metadata),
            )
            .into_any_element()
    }
}

/// A conversation-type badge (Thread / New thread / Side chat): the tab
/// strip's Main badge geometry in the chip tiles' neutral wash, so it reads
/// as a quiet label rather than a selection, provider or project cue. Never
/// shrinks: the title and project truncate around it.
fn conversation_type_badge(theme: &Theme, label: &'static str) -> gpui::Div {
    div()
        .flex_none()
        .h(crate::typography::ui_rems(16.0))
        .px(crate::typography::ui_rems(5.0))
        .flex()
        .items_center()
        .rounded(px(4.0))
        .bg(crate::theme::ink(0.06))
        .text_size(crate::typography::ui_rems(11.0))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(theme.text_muted)
        .child(label)
}
