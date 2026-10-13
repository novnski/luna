//! Typed tool file badges as their own Files controls (ZI-04).
//!
//! A read/write/edit/patch/image call already names its file, and the chip
//! header shows that file as a badge. An openable badge is a separate labeled
//! button inside the header: one click, Enter or Space hands the exact typed
//! path to the transcript's existing workspace link route ([`render::LinkUi`],
//! the same route Markdown file links take). The shell resolves it against
//! the OWNING chat's folder and opens or focuses that chat's Files tab, so
//! side chats, subagent tabs and remote chats keep their own checkout and
//! host. The badge stops the click there: the card never expands, collapses
//! or moves the composer.
//!
//! The decision whether a path can open is the shell's own resolver
//! ([`crate::workspace_links::resolve_workspace_file_link`]) over the path
//! as written, wrapped as a canonical `luna-file:` target so file names are
//! never reinterpreted as `:line` suffixes or URLs. A path it refuses
//! (outside the chat's folder, `..`, Windows separators, no folder) leaves
//! the badge inert with the reason on its tooltip and accessible label; a
//! click there keeps today's card behavior. Deleted files, symlinks and host
//! refusals are the Files tab's own truthful states. Nothing here reads a
//! file, parses tool prose or grants a path.

use super::*;
use crate::AccessibleButton as _;
use crate::interaction::HoverFill as _;
use crate::markdown::render::{LinkAction, LinkActivation, LinkOutcome, LinkTarget};

/// Why an inert badge cannot open (tooltip and accessible label).
const NO_WORKSPACE: &str = "This chat has no workspace folder";
const OUTSIDE_WORKSPACE: &str = "Outside this chat's workspace";
const UNSUPPORTED_PATH: &str = "Not a workspace file path";
/// The owning surface declined the activation (the chat is no longer the
/// one shown, or its folder changed since the badge was drawn).
pub(super) const REFUSED: &str = "Couldn't open in Files from here";

/// The file a typed tool call names, if any. Only typed paths: arbitrary
/// output or prose never becomes a file target.
pub(super) fn tool_file_path(call: &ToolCall) -> Option<&str> {
    match call {
        ToolCall::ReadFile { path }
        | ToolCall::WriteFile { path, .. }
        | ToolCall::EditFile { path, .. }
        | ToolCall::ApplyPatch { path: Some(path) }
        | ToolCall::ViewImage { path } => Some(path.as_str()),
        _ => None,
    }
}

/// Where a typed path leads under one chat's workspace folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum BadgeTarget {
    /// `target` is the link the shell resolves; `relative` its checkout path.
    Open {
        target: String,
        relative: String,
    },
    Unavailable(&'static str),
}

/// Classify `path` against the owning chat's folder with the shell's own
/// resolver, so what the badge offers is exactly what the shell will open.
pub(super) fn badge_target(path: &str, root: Option<&str>) -> BadgeTarget {
    let Some(root) = root.filter(|root| !root.is_empty()) else {
        return BadgeTarget::Unavailable(NO_WORKSPACE);
    };
    if path.is_empty() || path.chars().any(char::is_control) {
        return BadgeTarget::Unavailable(UNSUPPORTED_PATH);
    }
    let target = crate::workspace_links::file_mention_target(path);
    match crate::workspace_links::resolve_workspace_file_link(&target, root) {
        Some(link) => BadgeTarget::Open {
            target,
            relative: link.path,
        },
        None => {
            let typed = std::path::Path::new(path);
            // Component-wise: `/work/app-other` is outside `/work/app`.
            if typed.has_root() && !typed.starts_with(root) {
                BadgeTarget::Unavailable(OUTSIDE_WORKSPACE)
            } else {
                BadgeTarget::Unavailable(UNSUPPORTED_PATH)
            }
        }
    }
}

#[derive(Clone)]
enum BadgeAction {
    Open {
        label: SharedString,
        open: Rc<dyn Fn(&mut Window, &mut gpui::App)>,
    },
    Unavailable(&'static str),
}

/// One rendered badge's identity, action and last refusal.
#[derive(Clone)]
pub(super) struct ToolFileBadge {
    /// Element id, unique in the transcript (`{row}#f{ix}`).
    key: SharedString,
    /// The tool part, for the test-build bounds lookup.
    part_id: SharedString,
    action: BadgeAction,
    refusal: Option<SharedString>,
}

impl ToolFileBadge {
    pub(super) fn refusal(&self) -> Option<SharedString> {
        self.refusal.clone()
    }
}

impl Transcript {
    /// The badge for `tool`, when it names a file and this view has the
    /// workspace link route (a view without one, such as the Board
    /// authoring discussion, keeps the plain badge).
    pub(super) fn tool_file_badge(
        &self,
        row_id: &SharedString,
        ix: usize,
        tool: &ToolItem,
        cx: &Context<Self>,
    ) -> Option<ToolFileBadge> {
        let path = tool_file_path(&tool.call)?;
        let links = self.link_ui()?;
        // The owning chat: the pane's or subagent tab's own chat, else this
        // view's selected chat. Never the shell's current selection.
        let owner = links.source_session.clone()?;
        let root = self
            .state
            .read(cx)
            .chats
            .iter()
            .find(|chat| chat.id == owner)
            .and_then(|chat| chat.cwd.clone());
        let key: SharedString = format!("{row_id}#f{ix}").into();
        let action = match badge_target(path, root.as_deref()) {
            BadgeTarget::Open { target, relative } => {
                let activation = LinkActivation {
                    target: LinkTarget {
                        label: relative.clone(),
                        original: target,
                        // Never a web destination, whatever the path spells.
                        navigation: Err("workspace file"),
                    },
                    action: LinkAction::Primary,
                    source_session: Some(owner),
                };
                let view = cx.weak_entity();
                let refusal_key = key.clone();
                BadgeAction::Open {
                    label: format!("Open {relative} in Files").into(),
                    open: Rc::new(move |window: &mut Window, cx: &mut gpui::App| {
                        let outcome = (links.handler)(&activation, window, cx);
                        let refused = outcome == LinkOutcome::Rejected;
                        view.update(cx, |this, cx| {
                            this.file_badge_refusal =
                                refused.then(|| (refusal_key.clone(), REFUSED.into()));
                            cx.notify();
                        })
                        .ok();
                    }),
                }
            }
            BadgeTarget::Unavailable(reason) => BadgeAction::Unavailable(reason),
        };
        let refusal = self
            .file_badge_refusal
            .as_ref()
            .filter(|(refused, _)| *refused == key)
            .map(|(_, message)| message.clone());
        Some(ToolFileBadge {
            key,
            part_id: tool.part_id.clone().into(),
            action,
            refusal,
        })
    }
}

/// The header's file badge: icon well and file name. With `badge`, it is
/// the open control (or an inert, explained label); without, the plain
/// badge this view has always drawn.
pub(super) fn render_file_badge(
    path: &str,
    badge: Option<&ToolFileBadge>,
    hover_text: bool,
    theme: &Theme,
) -> AnyElement {
    let name = file_badge_name(path).to_owned();
    let chip = |bg: Option<Hsla>| {
        div()
            .min_w_0()
            .h(px(22.0))
            .flex()
            .items_center()
            .overflow_hidden()
            .gap(px(6.0))
            .rounded(px(crate::theme::radius::SM))
            .when_some(bg, |chip, bg| chip.bg(bg))
            .pl(px(1.0))
            .pr(px(6.0))
            .text_color(theme.text.opacity(0.85))
            .child(
                div()
                    .size(px(20.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(crate::theme::radius::XS))
                    .bg(crate::file_icons::well_bg(theme))
                    .child(
                        crate::file_icons::icon(
                            crate::file_icons::FileIconIdentity::file(path),
                            theme.appearance,
                        )
                        .size(px(14.0)),
                    ),
            )
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .child(SharedString::from(name.clone())),
            )
    };
    let element = match badge {
        None => {
            let chip = chip(Some(theme.ink(0.06)));
            if hover_text {
                chip.id("tool-file-badge")
                    .group_hover("tool-header", |style| style.text_color(theme.text))
                    .into_any_element()
            } else {
                chip.into_any_element()
            }
        }
        Some(badge) => {
            let part_id = badge.part_id.clone();
            let chip = chip(None)
                .id(badge.key.clone())
                .debug_selector(move || format!("tool-file-badge-{part_id}"))
                .when(hover_text, |chip| {
                    chip.group_hover("tool-header", |style| style.text_color(theme.text))
                });
            match &badge.action {
                BadgeAction::Open { label, open } => {
                    let open = open.clone();
                    chip.accessible_button(label.clone(), theme.focus_ring())
                        .cursor_pointer()
                        .hover_fill(
                            format!("tl-{}", badge.key),
                            theme.ink(0.06),
                            theme.ink(0.11),
                        )
                        .tooltip(crate::popover::text_tooltip(path.to_owned()))
                        .tooltip_show_delay(crate::popover::TOOLTIP_DELAY)
                        // The badge's own activation: the card under it must
                        // neither expand nor collapse (GPUI fires the
                        // innermost click first; stopping here ends it).
                        .on_click(move |_, window, cx| {
                            cx.stop_propagation();
                            (open)(window, cx);
                        })
                        .into_any_element()
                }
                BadgeAction::Unavailable(reason) => chip
                    .bg(theme.ink(0.06))
                    .role(gpui::Role::Label)
                    .aria_label(format!("{name}: {reason}"))
                    .tooltip(crate::popover::text_tooltip(format!("{reason} · {path}")))
                    .tooltip_show_delay(crate::popover::TOOLTIP_DELAY)
                    .into_any_element(),
            }
        }
    };
    crate::frost::frosted(5.0, 16.0, element).into_any_element()
}

#[cfg(test)]
mod tests {
    //! The badge's own contract (ZI-04 F1, F3, F5): real painted badges in a
    //! main transcript and a side-chat pane transcript, activated by pointer
    //! and by Enter/Space. Each activation reaches the view's route exactly
    //! once, names the owning chat, never a web destination, and resolves
    //! (with the shell's resolver) to that badge's own typed path; the card
    //! never expands or collapses. Refused paths stay inert. The route here
    //! only records what the badge hands it: opening, tab reuse, unsaved
    //! buffers and host routing are the shell's, owned by
    //! `shell::tool_file_open_e2e`.
    use super::*;
    use gpui::{AppContext as _, Bounds, TestAppContext, VisualTestContext};
    use luna_doc::TranscriptFrame;

    const MAIN: &str = "badge-main";
    const SIDE: &str = "badge-side";
    const MAIN_ROOT: &str = "/work/luna main";
    const SIDE_ROOT: &str = "/work/luna side";

    /// The classifier at its boundary: what opens is the shell resolver's
    /// answer for the path exactly as typed, under the owning folder.
    #[cfg(unix)]
    #[test]
    fn typed_paths_open_only_as_written_inside_the_owning_folder() {
        let cases: &[(&str, Option<&str>, Result<&str, &str>)] = &[
            (
                "/work/luna main/src/naïve café.rs",
                Some(MAIN_ROOT),
                Ok("src/naïve café.rs"),
            ),
            ("src/lib.rs", Some(MAIN_ROOT), Ok("src/lib.rs")),
            // `#` and spaces are part of the name, never a fragment.
            (
                "/work/luna main/docs/R&D #L4.md",
                Some(MAIN_ROOT),
                Ok("docs/R&D #L4.md"),
            ),
            // A colon suffix is not read as a line: refusing beats opening
            // `src/lib.rs`, which is not the file the tool named.
            (
                "/work/luna main/src/lib.rs:12",
                Some(MAIN_ROOT),
                Err(UNSUPPORTED_PATH),
            ),
            (
                "/work/luna mainframe/src/lib.rs",
                Some(MAIN_ROOT),
                Err(OUTSIDE_WORKSPACE),
            ),
            ("/etc/hosts", Some(MAIN_ROOT), Err(OUTSIDE_WORKSPACE)),
            (
                "/work/luna main/../secret.rs",
                Some(MAIN_ROOT),
                Err(UNSUPPORTED_PATH),
            ),
            ("../secret.rs", Some(MAIN_ROOT), Err(UNSUPPORTED_PATH)),
            (
                "C:\\repo\\src\\main.rs",
                Some(MAIN_ROOT),
                Err(UNSUPPORTED_PATH),
            ),
            (
                "https://example.com/src/lib.rs",
                Some(MAIN_ROOT),
                Err(UNSUPPORTED_PATH),
            ),
            (
                "/work/luna main/src/",
                Some(MAIN_ROOT),
                Err(UNSUPPORTED_PATH),
            ),
            ("/work/luna main", Some(MAIN_ROOT), Err(UNSUPPORTED_PATH)),
            ("", Some(MAIN_ROOT), Err(UNSUPPORTED_PATH)),
            (
                "/work/luna main/src/a\nb.rs",
                Some(MAIN_ROOT),
                Err(UNSUPPORTED_PATH),
            ),
            ("/work/luna main/src/lib.rs", None, Err(NO_WORKSPACE)),
            ("/work/luna main/src/lib.rs", Some(""), Err(NO_WORKSPACE)),
        ];
        for (path, root, expected) in cases {
            let actual: Result<String, &str> = match badge_target(path, *root) {
                BadgeTarget::Open { relative, .. } => Ok(relative),
                BadgeTarget::Unavailable(reason) => Err(reason),
            };
            assert_eq!(
                actual.as_deref().map_err(|reason| *reason),
                *expected,
                "{path:?} under {root:?}"
            );
        }
    }

    /// The view's link route, recording what the badge hands it.
    #[derive(Default)]
    struct Route {
        opened: RefCell<Vec<LinkActivation>>,
        refuse: Cell<bool>,
    }

    fn route_for(source: Option<&str>, route: &Rc<Route>) -> render::LinkUi {
        let route = route.clone();
        render::LinkUi {
            source_session: source.map(str::to_owned),
            handler: Rc::new(
                move |activation: &LinkActivation, _: &mut Window, _: &mut gpui::App| {
                    route.opened.borrow_mut().push(activation.clone());
                    if route.refuse.get() {
                        LinkOutcome::Rejected
                    } else {
                        LinkOutcome::Internal
                    }
                },
            ),
        }
    }

    struct Panes {
        main: Entity<Transcript>,
        side: Entity<Transcript>,
    }

    impl Render for Panes {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .flex()
                .flex_col()
                .child(div().flex_1().min_h_0().relative().child(self.main.clone()))
                .child(div().flex_1().min_h_0().relative().child(self.side.clone()))
        }
    }

    fn tool(id: &str, call: ToolCall) -> MessagePart {
        MessagePart::Tool {
            id: id.into(),
            call,
            is_error: false,
            resolved: true,
            output: None,
            diff: None,
            output_ref: None,
            output_bytes: None,
            diff_ref: None,
            diff_stats: None,
            subagent_ref: None,
            subagent_status: None,
            subagent_tail: None,
            subagent_activity: None,
        }
    }

    fn read(path: &str) -> ToolCall {
        ToolCall::ReadFile { path: path.into() }
    }

    /// A live turn: its trailing tool group is open, every card closed.
    fn turn(id: &str, parts: Vec<MessagePart>) -> SessionMessageEntry {
        SessionMessageEntry {
            id: id.into(),
            role: MessageRole::Assistant,
            parts,
            created_at: 1_790_000_000_000,
            device_id: "local".into(),
            status: Some(MessageStatus::Streaming),
            continuation_of: None,
        }
    }

    fn draw(cx: &mut VisualTestContext) {
        for _ in 0..3 {
            cx.run_until_parked();
            cx.update(|window, cx| window.draw(cx).clear());
        }
    }

    fn badge(cx: &mut VisualTestContext, part: &str) -> Bounds<gpui::Pixels> {
        let selector: &'static str = Box::leak(format!("tool-file-badge-{part}").into_boxed_str());
        cx.debug_bounds(selector)
            .unwrap_or_else(|| panic!("badge {part} was not painted"))
    }

    fn click(cx: &mut VisualTestContext, at: gpui::Point<gpui::Pixels>) {
        cx.simulate_click(at, gpui::Modifiers::default());
        draw(cx);
    }

    fn click_badge(cx: &mut VisualTestContext, part: &str) {
        let at = badge(cx, part).center();
        click(cx, at);
    }

    /// Enter or Space on the focused control: key down, then key up (GPUI
    /// synthesizes the keyboard click on the release).
    fn press(cx: &mut VisualTestContext, key: &str) {
        let keystroke = gpui::Keystroke::parse(key).unwrap();
        cx.update(|window, cx| {
            window.dispatch_event(
                gpui::PlatformInput::KeyDown(gpui::KeyDownEvent {
                    keystroke: keystroke.clone(),
                    is_held: false,
                    prefer_character_input: false,
                }),
                cx,
            );
            window.dispatch_event(
                gpui::PlatformInput::KeyUp(gpui::KeyUpEvent { keystroke }),
                cx,
            );
        });
        draw(cx);
    }

    #[track_caller]
    fn assert_opened(route: &Route, count: usize, owner: &str, root: &str, relative: &str) {
        let opened = route.opened.borrow();
        assert_eq!(
            opened.len(),
            count,
            "one activation per gesture ({relative})"
        );
        let activation = opened.last().unwrap();
        assert_eq!(activation.source_session.as_deref(), Some(owner));
        assert_eq!(activation.action, LinkAction::Primary);
        assert!(
            activation.target.navigation.is_err(),
            "a file badge is never a web destination"
        );
        assert_eq!(
            crate::workspace_links::resolve_workspace_file_link(&activation.target.original, root)
                .map(|link| link.path)
                .as_deref(),
            Some(relative),
        );
    }

    #[gpui::test]
    fn badges_hand_their_own_file_to_the_owning_route_once_without_toggling_the_card(
        cx: &mut TestAppContext,
    ) {
        let _selection = crate::markdown::selection::test_state_lock();
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
            crate::settings::init(crate::settings::UiSettings::default(), dir.path(), cx);
            crate::motion::set_reduced_motion(cx, true);
        });
        let route = Rc::new(Route::default());
        let state = cx.new(|_| {
            let mut state = AppState::new();
            state.selected_chat = Some(MAIN.into());
            state.chats = [(MAIN, MAIN_ROOT), (SIDE, SIDE_ROOT)]
                .into_iter()
                .map(|(id, cwd)| {
                    serde_json::from_value(serde_json::json!({
                        "id": id, "deviceId": "local", "spaceId": "badge-space",
                        "cwd": cwd, "title": id, "archived": false,
                        "createdAt": "2026-10-13T00:00:00Z",
                    }))
                    .unwrap()
                })
                .collect();
            state
        });
        let (panes, cx) = cx.add_window_view({
            let state = state.clone();
            let route = route.clone();
            move |_, cx| {
                let main = cx.new(|cx| Transcript::new(state.clone(), cx));
                let side = cx.new(|cx| Transcript::for_pane_chat(state.clone(), SIDE.into(), cx));
                // As the shell wires them: the main view's route names the
                // selected chat; a workbench pane's names its own chat.
                main.update(cx, |view, _| {
                    view.set_workspace_link_handler(route_for(None, &route))
                });
                side.update(cx, |view, _| {
                    view.set_workspace_link_handler(route_for(Some(SIDE), &route))
                });
                Panes { main, side }
            }
        });
        state.update(cx, |state, cx| {
            // Codex reports one multi-file change as `f3` and `f3#1`.
            let main = turn(
                "m1",
                vec![
                    tool("m-read", read("/work/luna main/src/naïve café.rs")),
                    tool(
                        "m-write",
                        ToolCall::WriteFile {
                            path: "/work/luna main/notes/plan.md".into(),
                            content: None,
                        },
                    ),
                    tool(
                        "f3",
                        ToolCall::EditFile {
                            path: "/work/luna main/src/a.rs".into(),
                            old_string: None,
                            new_string: None,
                        },
                    ),
                    tool(
                        "f3#1",
                        ToolCall::ApplyPatch {
                            path: Some("/work/luna main/src/b.rs".into()),
                        },
                    ),
                    tool("m-outside", read("/etc/hosts")),
                ],
            );
            state
                .apply_transcript_frame(TranscriptFrame::reset(&[main]))
                .expect("frame applies");
            // The side chat names a main-checkout path: not its own file.
            let side = turn(
                "s1",
                vec![
                    tool("s-read", read("/work/luna side/src/naïve café.rs")),
                    tool("s-main", read("/work/luna main/src/a.rs")),
                ],
            );
            state.set_subagent_snapshot(SIDE.into(), vec![side]);
            cx.notify();
        });
        draw(cx);
        let (main, side) = panes.read_with(cx, |panes, _| (panes.main.clone(), panes.side.clone()));
        let untouched = |cx: &mut VisualTestContext, view: &Entity<Transcript>| {
            view.read_with(cx, |view, _| view.tool_details.is_empty())
        };
        assert!(untouched(cx, &main) && untouched(cx, &side));

        // Pointer, then Enter and Space on the badge the click focused.
        click_badge(cx, "m-read");
        assert_opened(&route, 1, MAIN, MAIN_ROOT, "src/naïve café.rs");
        press(cx, "enter");
        assert_opened(&route, 2, MAIN, MAIN_ROOT, "src/naïve café.rs");
        press(cx, "space");
        assert_opened(&route, 3, MAIN, MAIN_ROOT, "src/naïve café.rs");
        assert!(untouched(cx, &main), "badge activation toggled the card");

        // Write, and each file of the multi-file change, open their own path.
        for (part, relative, count) in [
            ("m-write", "notes/plan.md", 4),
            ("f3", "src/a.rs", 5),
            ("f3#1", "src/b.rs", 6),
        ] {
            click_badge(cx, part);
            assert_opened(&route, count, MAIN, MAIN_ROOT, relative);
        }
        assert!(untouched(cx, &main), "badge activation toggled the card");

        // A declined activation says so beside that badge until the next one.
        route.refuse.set(true);
        click_badge(cx, "m-write");
        assert_opened(&route, 7, MAIN, MAIN_ROOT, "notes/plan.md");
        assert_eq!(
            main.read_with(cx, |view, _| view.file_badge_refusal.clone()),
            Some((SharedString::from("m1#g0#f1"), SharedString::from(REFUSED)))
        );
        route.refuse.set(false);
        click_badge(cx, "m-write");
        assert_opened(&route, 8, MAIN, MAIN_ROOT, "notes/plan.md");
        assert_eq!(
            main.read_with(cx, |view, _| view.file_badge_refusal.clone()),
            None
        );

        // The side pane's badge names the side chat and its own folder; a
        // path under the main checkout is outside it and stays inert.
        click_badge(cx, "s-read");
        assert_opened(&route, 9, SIDE, SIDE_ROOT, "src/naïve café.rs");
        assert!(untouched(cx, &side), "badge activation toggled the card");
        click_badge(cx, "s-main");
        assert_eq!(route.opened.borrow().len(), 9, "an outside path opened");
        click_badge(cx, "m-outside");
        assert_eq!(route.opened.borrow().len(), 9, "an outside path opened");

        // Control: the card itself, beside the badge, still toggles.
        let read = badge(cx, "m-read");
        click(
            cx,
            gpui::point(read.right() + gpui::px(48.0), read.center().y),
        );
        assert_eq!(route.opened.borrow().len(), 9);
        assert_eq!(
            main.read_with(cx, |view, _| view
                .tool_details
                .get(&SharedString::from("m1#g0#d0"))
                .and_then(|fold| fold.open)),
            Some(true),
            "the card beside the badge no longer toggles"
        );
    }
}
