//! Conversation Find (IC-5): the display text a transcript row paints, keyed
//! by the renderer's own element discriminators, and the paint-only
//! highlight pass over those elements.
//!
//! Failure contract and evidence plan:
//! `docs/pending-tasks/interactive-chat-surfaces/ic-5/README.md`.
//!
//! The projection mirrors [`super::render::render_block`] exactly as the
//! transcript configures it: no task toggles (a task marker paints as its
//! `[ ]` text), no inline image media (an image paints as its alt text) and
//! Mermaid fences shown as diagrams. Each painted text element becomes one
//! segment, so a match can span inline runs (`**bold**`, a link label) but
//! never two blocks, and its highlight lands on the element showing it.
//! Link destinations, image sources and fence info strings never enter a
//! run's text, so they are not searchable.

use std::cell::Cell;
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;

use gpui::{
    AnyElement, BorderStyle, Bounds, Hsla, Pixels, SharedString, StyledText, Window, canvas, div,
    prelude::*, px, quad,
};

use super::link_presentation::OffsetMap;
use super::parser::{Block, InlineRun};
use super::render::{
    file_line_ix, list_child_ix, plain_file_reference_lines, quote_child_ix, range_rects,
    table_cell_ix,
};
use crate::theme::Theme;

/// How a painted element lays out its text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ElementKind {
    /// One wrapped text element (prose, heading, list item, quote child,
    /// table cell, user bubble, error detail).
    Text,
    /// A fenced code block: one painted element per `\n`-separated line.
    Code,
}

/// The renderer's discriminator for one painted element of a row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ElementKey {
    pub kind: ElementKind,
    pub ix: usize,
}

impl ElementKey {
    pub const fn text(ix: usize) -> Self {
        Self {
            kind: ElementKind::Text,
            ix,
        }
    }

    pub const fn code(ix: usize) -> Self {
        Self {
            kind: ElementKind::Code,
            ix,
        }
    }
}

/// The complete display text of one painted element.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextSegment {
    pub element: ElementKey,
    pub text: String,
}

/// How the owning surface renders the blocks being projected.
#[derive(Clone, Copy, Debug, Default)]
pub struct ProjectOptions<'a> {
    /// The root the renderer resolves bare workspace-file lines against.
    pub workspace_root: Option<&'a str>,
    /// Mermaid fences paint as diagrams (their source is not visible text).
    pub diagrams: bool,
}

/// Append a segment unless its text is blank (nothing a query can match).
pub fn push_segment(out: &mut Vec<TextSegment>, element: ElementKey, text: String) {
    if !text.trim().is_empty() {
        out.push(TextSegment { element, text });
    }
}

/// The flattened text a run list paints: styling and link destinations live
/// in `style`, so a phrase across `**bold**` or a link label stays whole.
fn run_text(runs: &[InlineRun]) -> String {
    runs.iter().map(|run| run.text.as_str()).collect()
}

/// Project one block (top-level or nested) whose element discriminator is
/// `ix`, in the renderer's paint order.
pub fn project_block(
    block: &Block,
    ix: usize,
    opts: ProjectOptions<'_>,
    out: &mut Vec<TextSegment>,
) {
    match block {
        Block::Paragraph { runs } | Block::Heading { runs, .. } => {
            project_runs(runs, ix, opts, out)
        }
        Block::CodeBlock { language, code } => {
            let diagram = opts.diagrams
                && language
                    .as_deref()
                    .is_some_and(|language| language.eq_ignore_ascii_case("mermaid"));
            if !diagram {
                push_segment(out, ElementKey::code(ix), code.clone());
            }
        }
        Block::BlockQuote { children } => {
            for (child_ix, child) in children.iter().enumerate() {
                project_block(child, quote_child_ix(ix, child_ix), opts, out);
            }
        }
        Block::List { items, .. } => {
            for (item_ix, item) in items.iter().enumerate() {
                for (child_ix, child) in item.iter().enumerate() {
                    project_block(child, list_child_ix(ix, item_ix, child_ix), opts, out);
                }
            }
        }
        Block::Table { header, rows, .. } => {
            // The renderer drops an empty header row before numbering rows.
            let all = std::iter::once(header.as_slice())
                .filter(|header| !header.is_empty())
                .chain(rows.iter().map(Vec::as_slice));
            for (row_ix, row) in all.enumerate() {
                for (column, cell) in row.iter().enumerate() {
                    push_segment(
                        out,
                        ElementKey::text(table_cell_ix(ix, row_ix, column)),
                        run_text(cell),
                    );
                }
            }
        }
        Block::Rule => {}
    }
}

fn project_runs(
    runs: &[InlineRun],
    ix: usize,
    opts: ProjectOptions<'_>,
    out: &mut Vec<TextSegment>,
) {
    // A hard-break list of workspace files paints one element per line.
    if let Some(lines) = opts
        .workspace_root
        .and_then(|root| plain_file_reference_lines(runs, root))
    {
        for (line_ix, line) in lines.into_iter().enumerate() {
            push_segment(out, ElementKey::text(file_line_ix(ix, line_ix)), line);
        }
        return;
    }
    push_segment(out, ElementKey::text(ix), run_text(runs));
}

/// One highlighted match in an element's own text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FindRange {
    pub range: Range<usize>,
    pub active: bool,
}

/// One row's Find highlights: ranges per painted element, in that element's
/// own text (code: block-relative, split per painted line at paint time).
pub struct FindPaint {
    elements: HashMap<ElementKey, Vec<FindRange>>,
    wash: Hsla,
    active_wash: Hsla,
    active_border: Hsla,
    /// Window-space bounds of the active match's first painted line, written
    /// during paint for the owner's post-layout reveal correction.
    active_rect: Rc<Cell<Option<Bounds<Pixels>>>>,
}

impl FindPaint {
    pub fn new(theme: &Theme, active_rect: Rc<Cell<Option<Bounds<Pixels>>>>) -> Self {
        Self {
            elements: HashMap::new(),
            wash: theme.warning.opacity(0.28),
            active_wash: theme.warning.opacity(0.58),
            active_border: theme.warning,
            active_rect,
        }
    }

    pub fn push(&mut self, element: ElementKey, range: Range<usize>, active: bool) {
        self.elements
            .entry(element)
            .or_default()
            .push(FindRange { range, active });
    }

    pub fn ranges(&self, element: ElementKey) -> &[FindRange] {
        self.elements.get(&element).map_or(&[], Vec::as_slice)
    }

    /// The parts of code block `ix`'s ranges painted on one line, where
    /// `line` is that line's span in block offsets; shifted into the line.
    pub fn line_ranges(&self, ix: usize, line: Range<usize>) -> Vec<FindRange> {
        self.ranges(ElementKey::code(ix))
            .iter()
            .filter_map(|found| {
                let start = found.range.start.max(line.start);
                let end = found.range.end.min(line.end);
                (start < end).then(|| FindRange {
                    range: start - line.start..end - line.start,
                    active: found.active,
                })
            })
            .collect()
    }

    /// Paint `ranges` under the glyphs of `layout`; call from a canvas that
    /// paints before the text.
    pub(crate) fn paint(
        &self,
        window: &mut Window,
        layout: &gpui::TextLayout,
        ranges: &[FindRange],
    ) {
        self.paint_mapped(window, layout, ranges, None);
    }

    /// [`Self::paint`] for an element showing a width-shortened presentation:
    /// `offsets` maps the original offsets the ranges use to displayed ones.
    pub(super) fn paint_mapped(
        &self,
        window: &mut Window,
        layout: &gpui::TextLayout,
        ranges: &[FindRange],
        offsets: Option<&OffsetMap>,
    ) {
        for found in ranges {
            let range = offsets.map_or_else(
                || found.range.clone(),
                |map| map.displayed_range(found.range.clone()),
            );
            let rects = range_rects(layout, &range, 0.0, 0.0);
            if found.active
                && let Some(first) = rects.first()
            {
                self.active_rect.set(Some(*first));
            }
            for rect in rects {
                window.paint_quad(quad(
                    rect,
                    px(3.0),
                    if found.active {
                        self.active_wash
                    } else {
                        self.wash
                    },
                    px(if found.active { 1.0 } else { 0.0 }),
                    if found.active {
                        self.active_border
                    } else {
                        gpui::transparent_black()
                    },
                    BorderStyle::default(),
                ));
            }
        }
    }
}

/// Plain inherited-style text with Find highlights under it (a row whose
/// text is otherwise a bare string child, such as an error chip's detail).
pub fn highlighted_text(
    text: SharedString,
    paint: Rc<FindPaint>,
    element: ElementKey,
) -> AnyElement {
    let styled = StyledText::new(text);
    let layout = styled.layout().clone();
    let underlay = canvas(
        |_, _, _| (),
        move |_, _, window, _| paint.paint(window, &layout, paint.ranges(element)),
    )
    .absolute()
    .size_full();
    div()
        .relative()
        .child(underlay)
        .child(styled)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    //! The projection's single owner: it must name exactly the elements the
    //! real renderer paints, with exactly their painted text. A renderer
    //! change to a discriminator, flattening or split path (task markers,
    //! tables without headers, workspace-file lines, code lines) fails here
    //! before Find highlights can land on the wrong element.
    use super::super::parser::{BlockTree, parse_full};
    use super::super::render::{self, RenderOptions};
    use super::*;
    use gpui::{Context, Render, TestAppContext, size};

    const ROW: &str = "find-probe";
    const ROOT: &str = "/workspace";
    const SOURCE: &str = concat!(
        "# Heading with **bold** text\n\n",
        "A paragraph with *emphasis*, `code` and a [label](https://hidden.example/x).\n\n",
        "- first item\n- [ ] open task\n  - nested item\n\n",
        "> quoted paragraph\n>\n> - quoted list item\n\n",
        "| Head A | Head B |\n| --- | --- |\n| cell one | cell two |\n\n",
        "```rust\nfn main() {\n    let x = 1;\n}\n```\n\n",
        "src/main.rs\\\nCargo.toml\n\n",
        "---\n\n",
        "![alt text](image.png) after image\n\n",
        "```mermaid\nflowchart LR\n  A --> B\n```",
    );

    struct Probe {
        tree: BlockTree,
        find: Rc<FindPaint>,
    }

    impl Render for Probe {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let theme = Theme::of(cx).clone();
            let mut opts = RenderOptions::settled(ROW.into());
            opts.workspace_root = Some(ROOT.into());
            opts.find = Some(self.find.clone());
            div()
                .size_full()
                .flex()
                .flex_col()
                .child(render::selection_frame_reset())
                .children(self.tree.blocks.iter().enumerate().map(|(ix, top)| {
                    render::render_block(&top.block, ix, ix, &opts, &theme, window, None)
                }))
        }
    }

    #[gpui::test]
    fn projection_names_each_painted_element_with_its_painted_text(cx: &mut TestAppContext) {
        let _selection = super::super::selection::test_state_lock();
        cx.update(|cx| cx.set_global(Theme::dark()));
        let tree = parse_full(SOURCE);
        // Mermaid source shows as code when the surface has no diagram
        // media (this probe); a diagram surface hides it from Find.
        let project = |diagrams| {
            let mut segments = Vec::new();
            for (ix, top) in tree.blocks.iter().enumerate() {
                let opts = ProjectOptions {
                    workspace_root: Some(ROOT),
                    diagrams,
                };
                project_block(&top.block, ix, opts, &mut segments);
            }
            segments
        };
        let segments = project(false);
        let texts: Vec<&str> = segments
            .iter()
            .map(|segment| segment.text.as_str())
            .collect();
        assert_eq!(
            texts,
            [
                "Heading with bold text",
                "A paragraph with emphasis, code and a label.",
                "first item",
                "[ ] open task",
                "nested item",
                "quoted paragraph",
                "quoted list item",
                "Head A",
                "Head B",
                "cell one",
                "cell two",
                "fn main() {\n    let x = 1;\n}",
                "src/main.rs",
                "Cargo.toml",
                "alt text after image",
                "flowchart LR\n  A --> B",
            ],
            "display text: no markers, destinations or sources; one segment per block"
        );
        let diagram_texts: Vec<String> = project(true).into_iter().map(|s| s.text).collect();
        assert!(!diagram_texts.iter().any(|text| text.contains("flowchart")));
        assert_eq!(diagram_texts.len(), segments.len() - 1);

        let paragraph = &segments[1];
        let label = paragraph.text.find("label").unwrap();
        let active_rect = Rc::new(Cell::new(None));
        let mut find = FindPaint::new(&Theme::dark(), active_rect.clone());
        find.push(paragraph.element, label..label + "label".len(), true);
        let (_, cx) = cx.add_window_view(|_, _| Probe {
            tree: tree.clone(),
            find: Rc::new(find),
        });
        cx.simulate_resize(size(px(900.0), px(1400.0)));
        cx.update(|window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });

        for segment in &segments {
            let painted = match segment.element.kind {
                ElementKind::Text => {
                    render::selection_test_snapshot(&format!("{ROW}:{}", segment.element.ix))
                        .0
                        .to_string()
                }
                ElementKind::Code => (0..segment.text.split('\n').count())
                    .map(|line| {
                        render::selection_test_snapshot(&format!(
                            "{ROW}-code{}-line{line}",
                            segment.element.ix
                        ))
                        .0
                        .to_string()
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            };
            assert_eq!(painted, segment.text, "element {:?}", segment.element);
        }

        // The active highlight paints on the element that shows the label.
        let rect = active_rect.get().expect("active match painted");
        let bounds = render::selection_test_bounds(&format!("{ROW}:{}", paragraph.element.ix));
        assert!(
            bounds.contains(&rect.origin) && rect.size.width > px(0.0),
            "highlight {rect:?} outside its element {bounds:?}"
        );

        // A match spanning code lines paints its part on each line it
        // crosses, in that line's own offsets.
        let code = &segments[11];
        let mut lines = FindPaint::new(&Theme::dark(), Rc::default());
        let start = code.text.find("{\n").unwrap();
        lines.push(code.element, start..start + "{\n    let".len(), false);
        let first = 0..code.text.find('\n').unwrap();
        let second = first.end + 1..first.end + 1 + "    let x = 1;".len();
        let ix = code.element.ix;
        assert_eq!(
            lines.line_ranges(ix, first.clone()),
            [FindRange {
                range: start..first.end,
                active: false
            }]
        );
        assert_eq!(
            lines.line_ranges(ix, second),
            [FindRange {
                range: 0.."    let".len(),
                active: false
            }]
        );
    }
}
