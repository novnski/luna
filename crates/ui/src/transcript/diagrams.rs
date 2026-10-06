//! Chat-only ownership for the native Files Mermaid adapter. No network reads.
use super::*;
use crate::image_media::{MediaImage, release_media};
use crate::markdown::mermaid;
use std::collections::VecDeque;

const MAX_DIAGRAMS: usize = 32;
const MAX_MEDIA_BYTES: usize = 64 * 1024 * 1024;
const MAX_HEIGHT: f32 = 480.0;
// Only the mutable top-level block is reparsed to inspect fence closure. Very
// large containers keep source until they stabilize or the message completes.
const MAX_LIVE_BLOCK_BYTES: usize = 128 * 1024;

#[derive(Default)]
pub(super) struct DiagramCache {
    style: u32,
    epoch: u64,
    allowed: HashSet<Arc<str>>,
    limit_reached: bool,
    eligible: HashMap<SharedString, HashSet<Arc<str>>>,
    ready: HashMap<Arc<str>, Result<MediaImage, String>>,
    queue: VecDeque<Arc<str>>,
    active: Option<Arc<str>>,
    task: Option<Task<()>>,
    source_visible: HashSet<SharedString>,
    target: (f32, f32),
    dpi: f32,
}

impl DiagramCache {
    pub(super) fn clear(&mut self, cx: &mut gpui::App) {
        self.epoch = self.epoch.wrapping_add(1);
        self.task = None;
        self.active = None;
        self.queue.clear();
        self.allowed.clear();
        self.limit_reached = false;
        self.eligible.clear();
        self.source_visible.clear();
        release_media(self.ready.drain().filter_map(|(_, result)| result.ok()), cx);
    }
}

fn sources(block: &Block, out: &mut Vec<Arc<str>>) -> bool {
    match block {
        Block::CodeBlock { language, code }
            if language
                .as_deref()
                .is_some_and(|l| l.eq_ignore_ascii_case("mermaid")) =>
        {
            // Oversized sources remain ordinary copyable code with a diagnostic;
            // never retain an additional oversized copy in the media cache.
            if code.len() <= mermaid::MAX_SOURCE_BYTES {
                out.push(Arc::from(code.as_str()));
            }
            true
        }
        Block::BlockQuote { children } => children
            .iter()
            .fold(false, |found, b| sources(b, out) || found),
        Block::List { items, .. } => items
            .iter()
            .flatten()
            .fold(false, |found, b| sources(b, out) || found),
        _ => false,
    }
}

/// pulldown-cmark's fenced Start/End range includes the closing delimiter,
/// while Text ranges end before it. On implicit EOF/container closure there
/// are no delimiter bytes after the final Text event. Use that distinction
/// instead of guessing from diagram syntax or mending an unfinished fence.
fn closed_sources(source: &str) -> HashSet<Arc<str>> {
    use pulldown_cmark::{CodeBlockKind, Event, Parser, Tag, TagEnd};
    let mut current: Option<(u8, usize, usize, String)> = None;
    let mut closed = HashSet::new();
    for (event, range) in Parser::new(source).into_offset_iter() {
        match event {
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(info)))
                if info
                    .split_whitespace()
                    .next()
                    .is_some_and(|l| l.eq_ignore_ascii_case("mermaid")) =>
            {
                let fence = source.as_bytes()[range.start];
                let count = source.as_bytes()[range.start..]
                    .iter()
                    .take_while(|&&byte| byte == fence)
                    .count();
                let body_start = source[range.clone()]
                    .find('\n')
                    .map_or(range.end, |offset| range.start + offset + 1);
                current = Some((fence, count, body_start, String::new()));
            }
            Event::Text(text) => {
                if let Some((_, _, end, code)) = &mut current {
                    *end = (*end).max(range.end);
                    code.push_str(&text);
                }
            }
            Event::End(TagEnd::CodeBlock) => {
                if let Some((fence, count, end, mut code)) = current.take() {
                    let tail = source.get(end..range.end).unwrap_or_default().trim_end();
                    if tail.bytes().rev().take_while(|&byte| byte == fence).count() >= count {
                        if code.ends_with('\n') {
                            code.pop();
                        }
                        if code.len() <= mermaid::MAX_SOURCE_BYTES {
                            closed.insert(Arc::from(code));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    closed
}

impl Transcript {
    pub(super) fn sync_diagrams(&mut self, rows: &[Row], cx: &mut Context<Self>) {
        let mut allowed = HashSet::new();
        let mut limit_reached = false;
        let mut eligible = HashMap::new();
        let mut active_ids = HashSet::new();
        for row in rows {
            let (tree, block_ix, live) = match &row.kind {
                RowKind::Markdown { tree, block_ix } => (tree, *block_ix, false),
                RowKind::LiveMarkdown { tree, block_ix } => (tree, *block_ix, true),
                _ => continue,
            };
            let Some(top) = tree.blocks.get(block_ix) else {
                continue;
            };
            let mut candidates = Vec::new();
            if !sources(&top.block, &mut candidates) {
                continue;
            }
            let closed = if live {
                let parser = self.live_parsers.get(part_prefix(&row.id));
                if parser.is_some_and(|p| block_ix < p.stable_prefix_blocks()) {
                    candidates.iter().cloned().collect()
                } else {
                    parser
                        .and_then(|p| p.source().get(top.range.clone()))
                        .filter(|source| source.len() <= MAX_LIVE_BLOCK_BYTES)
                        .map(closed_sources)
                        .unwrap_or_default()
                }
            } else {
                candidates.iter().cloned().collect()
            };
            let mut row_sources = HashSet::new();
            for code in candidates {
                if !closed.contains(&code) {
                    continue;
                }
                if allowed.contains(&code) || allowed.len() < MAX_DIAGRAMS {
                    allowed.insert(code.clone());
                    row_sources.insert(allowed.get(&code).unwrap().clone());
                } else {
                    limit_reached = true;
                }
            }
            eligible.insert(row.id.clone(), row_sources);
            for ix in render::code_block_indices(&top.block, block_ix) {
                active_ids.insert(SharedString::from(format!("{}-mermaid-{ix}", row.id)));
            }
        }
        self.diagrams
            .source_visible
            .retain(|id| active_ids.contains(id));
        let removed: Vec<_> = self
            .diagrams
            .ready
            .keys()
            .filter(|source| !allowed.contains(*source))
            .cloned()
            .collect();
        release_media(
            removed
                .into_iter()
                .filter_map(|source| self.diagrams.ready.remove(&source).and_then(Result::ok)),
            cx,
        );
        self.diagrams
            .queue
            .retain(|source| allowed.contains(source));
        if self.diagrams.allowed != allowed || self.diagrams.eligible != eligible {
            self.list.remeasure();
        }
        self.diagrams.allowed = allowed;
        self.diagrams.limit_reached = limit_reached;
        self.diagrams.eligible = eligible;
    }

    pub(super) fn refresh_diagram_style(&mut self, window: &Window, cx: &mut Context<Self>) {
        let style = crate::theme::style_generation();
        if self.diagrams.style != style {
            self.diagrams.epoch = self.diagrams.epoch.wrapping_add(1);
            self.diagrams.task = None;
            self.diagrams.active = None;
            self.diagrams.queue.clear();
            release_media(self.diagrams.ready.drain().filter_map(|(_, r)| r.ok()), cx);
            self.diagrams.style = style;
            self.list.remeasure();
        }
        let panel = f32::from(self.list.viewport_bounds().size.width);
        let width = if panel > 0.0 {
            panel
        } else {
            f32::from(window.viewport_size().width)
        };
        let target = ((width.min(self.content_width) - 48.0).max(1.0), MAX_HEIGHT);
        let dpi = window.scale_factor();
        if self.diagrams.target != target || self.diagrams.dpi != dpi {
            self.diagrams.target = target;
            self.diagrams.dpi = dpi;
            let mut retired = Vec::new();
            let mut used: usize = self
                .diagrams
                .ready
                .values()
                .filter_map(|r| r.as_ref().ok())
                .map(|m| m.bytes)
                .sum();
            for result in self.diagrams.ready.values_mut() {
                let Ok(media) = result else {
                    continue;
                };
                let next = media.preview_for_view(target, dpi);
                let next_used = used.saturating_sub(media.bytes).saturating_add(next.bytes);
                if next_used > MAX_MEDIA_BYTES {
                    used = used.saturating_sub(media.bytes);
                    if let Ok(old) =
                        std::mem::replace(result, Err("Chat diagram memory limit reached".into()))
                    {
                        retired.push(old);
                    }
                } else if !Arc::ptr_eq(&media.image, &next.image) {
                    used = next_used;
                    retired.push(std::mem::replace(media, next));
                }
            }
            release_media(retired, cx);
        }
    }

    fn start_diagram_job(&mut self, cx: &mut Context<Self>) {
        if self.diagrams.task.is_some() {
            return;
        }
        let Some(code) = self.diagrams.queue.pop_front() else {
            return;
        };
        let epoch = self.diagrams.epoch;
        let style = self.diagrams.style;
        let palette = mermaid::Palette::from_theme(Theme::of(cx));
        self.diagrams.active = Some(code.clone());
        self.diagrams.task = Some(cx.spawn(async move |this, cx| {
            let source = code.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    let svg = mermaid::render(&source, &palette)?;
                    crate::image_media::decode_image("image/svg+xml", svg.into_bytes())
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                if view.diagrams.epoch != epoch || view.diagrams.style != style {
                    return;
                }
                view.diagrams.task = None;
                view.diagrams.active = None;
                if view.diagrams.allowed.contains(&code) {
                    let result = result.and_then(|media| {
                        let media = media.preview_for_view(view.diagrams.target, view.diagrams.dpi);
                        let used: usize = view
                            .diagrams
                            .ready
                            .values()
                            .filter_map(|r| r.as_ref().ok())
                            .map(|m| m.bytes)
                            .sum();
                        if used.saturating_add(media.bytes) > MAX_MEDIA_BYTES {
                            Err("Chat diagram memory limit reached".into())
                        } else {
                            Ok(media)
                        }
                    });
                    view.diagrams.ready.insert(code, result);
                    view.list.remeasure();
                    if view.pinned {
                        view.wake_spring();
                    }
                    if view.own_turn.is_some() {
                        view.own_turn_kick = true;
                    }
                    cx.notify();
                }
                view.start_diagram_job(cx);
            });
        }));
    }

    pub(super) fn diagram_ui_for(
        &mut self,
        row_id: &SharedString,
        cx: &mut Context<Self>,
    ) -> Option<render::MediaUi> {
        let eligible = self.diagrams.eligible.get(row_id)?.clone();
        // Each source starts once, including failures. A single task drains only
        // sources requested by laid-out rows, never an entire offscreen history.
        for code in &eligible {
            if !self.diagrams.ready.contains_key(code)
                && self.diagrams.active.as_ref() != Some(code)
                && !self.diagrams.queue.contains(code)
            {
                self.diagrams.queue.push_back(code.clone());
            }
        }
        self.start_diagram_job(cx);
        let ready = self.diagrams.ready.clone();
        let allowed = self.diagrams.allowed.clone();
        let limit_reached = self.diagrams.limit_reached;
        let shown = self.diagrams.source_visible.clone();
        let owner = cx.weak_entity();
        let target = self.diagrams.target;
        Some(render::MediaUi {
            image: None,
            diagram: Some(Rc::new(move |code, id, theme| {
                let state = eligible.contains(code).then(|| ready.get(code)).flatten();
                let diagnostic: Option<SharedString> = if code.len() > mermaid::MAX_SOURCE_BYTES {
                    Some("Diagram exceeds preview complexity limit".into())
                } else if !eligible.contains(code) && !allowed.contains(code) {
                    limit_reached.then(|| "Chat diagram preview limit reached (32 sources)".into())
                } else {
                    state
                        .and_then(|result| result.as_ref().err())
                        .map(|error| error.clone().into())
                };
                let body = match state {
                    Some(Ok(media)) => {
                        let scale = (target.0 / media.width)
                            .min(MAX_HEIGHT / media.height)
                            .min(1.0);
                        div()
                            .w_full()
                            .child(
                                div()
                                    .w_full()
                                    .max_w(px(media.width * scale))
                                    .mx_auto()
                                    .aspect_ratio(media.width / media.height)
                                    .child(
                                        img(media.image.clone())
                                            .size_full()
                                            .object_fit(ObjectFit::Contain),
                                    ),
                            )
                            .into_any_element()
                    }
                    _ => div()
                        .p(px(12.0))
                        .text_color(theme.content_muted)
                        .child("Diagram preview unavailable; source shown")
                        .into_any_element(),
                };
                let shown_source = shown.contains(&id) || !matches!(state, Some(Ok(_)));
                let weak = owner.clone();
                render::DiagramUi {
                    body,
                    show_source: shown_source,
                    can_toggle: matches!(state, Some(Ok(_))),
                    diagnostic,
                    toggle_source: Rc::new(move |_, cx| {
                        let _ = weak.update(cx, |view, cx| {
                            if !view.diagrams.source_visible.remove(&id) {
                                view.diagrams.source_visible.insert(id.clone());
                            }
                            view.list.remeasure();
                            if view.pinned {
                                view.wake_spring();
                            }
                            if view.own_turn.is_some() {
                                view.own_turn_kick = true;
                            }
                            cx.notify();
                        });
                    }),
                }
            })),
        })
    }
}

#[cfg(feature = "appshots-fixture")]
impl Transcript {
    /// Native fixture evidence of the production cache, not provider proof.
    pub fn fixture_mermaid_readback(&self) -> serde_json::Value {
        serde_json::json!({
            "chatId": self.chat_id,
            "rows": self.rows.len(),
            "allowed": self.diagrams.allowed.len(),
            "rendered": self.diagrams.ready.values().filter(|r| r.is_ok()).count(),
            "errors": self.diagrams.ready.values().filter_map(|r| r.as_ref().err()).collect::<Vec<_>>(),
            "pending": self.diagrams.task.is_some() || !self.diagrams.queue.is_empty(),
            "limitReached": self.diagrams.limit_reached,
            "bytes": self.diagrams.ready.values().filter_map(|r| r.as_ref().ok()).map(|m| m.bytes).sum::<usize>(),
            "style": self.diagrams.style,
            "epoch": self.diagrams.epoch,
            "sourceVisible": self.diagrams.source_visible,
            "target": self.diagrams.target,
        })
    }
}
