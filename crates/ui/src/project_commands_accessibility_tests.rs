//! Rendered, held-RPC Commands card acceptance. No shell or provider runs.
use super::*;
use crate::engine_epoch_fixture::{self as fixture, HeldRpc};
use gpui::{AppContext, Focusable, TestAppContext};
use serde_json::json;
use std::sync::Arc;
use tokio::runtime::Runtime;

fn action() -> ProjectAction {
    ProjectAction {
        id: "build".into(),
        name: "Build".into(),
        command: "printf fixture-only".into(),
        setup_on_worktree_create: false,
        wait_for_setup: true,
        preview_url: None,
    }
}

fn runtime(cx: &mut TestAppContext) -> Runtime {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    cx.update(|cx| gpui_tokio::init_from_handle(cx, runtime.handle().clone()));
    runtime
}

struct CardSurface(Entity<ProjectCommandSettings>);
impl Render for CardSurface {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.0.clone())
    }
}

fn scene(
    cx: &mut TestAppContext,
    runtime: &Runtime,
) -> (
    Entity<ProjectCommandSettings>,
    Arc<HeldRpc>,
    gpui::WindowHandle<CardSurface>,
) {
    let rpc = HeldRpc::new("command-ax", "device-a");
    rpc.reply(methods::GET_PROJECT_COMMANDS, Ok(json!([action()])), false);
    rpc.reply(methods::SAVE_PROJECT_COMMANDS, Ok(json!([action()])), true);
    let state = cx.new(|_| AppState::new());
    fixture::attach(cx, &state, fixture::handle(runtime, rpc.clone()));
    let card = cx.new(|cx| ProjectCommandSettings::new(state, "fixture-project".into(), cx));
    fixture::pump_until(cx, runtime, |cx| !card.read_with(cx, |card, _| card.busy));
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::default());
    });
    let rendered = card.clone();
    let window = cx.open_window(gpui::size(px(960.0), px(900.0)), move |_, _| {
        CardSurface(rendered)
    });
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    visual.update(|window, app| window.draw(app).clear());
    (card, rpc, window)
}

fn click(window: &mut Window, app: &mut gpui::App, point: gpui::Point<gpui::Pixels>) {
    window.dispatch_event(
        gpui::PlatformInput::MouseDown(gpui::MouseDownEvent {
            position: point,
            button: gpui::MouseButton::Left,
            modifiers: Default::default(),
            click_count: 1,
            first_mouse: false,
        }),
        app,
    );
    window.dispatch_event(
        gpui::PlatformInput::MouseUp(gpui::MouseUpEvent {
            position: point,
            button: gpui::MouseButton::Left,
            modifiers: Default::default(),
            click_count: 1,
        }),
        app,
    );
}

fn key(window: &mut Window, app: &mut gpui::App, name: &str) {
    let keystroke = gpui::Keystroke::parse(name).unwrap();
    window.dispatch_event(
        gpui::PlatformInput::KeyDown(gpui::KeyDownEvent {
            keystroke: keystroke.clone(),
            is_held: false,
            prefer_character_input: false,
        }),
        app,
    );
    window.dispatch_event(
        gpui::PlatformInput::KeyUp(gpui::KeyUpEvent { keystroke }),
        app,
    );
}

fn trace_rendered_controls(
    visual: &mut gpui::VisualTestContext,
    rpc: &HeldRpc,
    name: &str,
    case: &str,
    selectors: &[&'static str],
) {
    let focused =
        visual.update(|window, app| window.focused(app).map(|handle| format!("{handle:?}")));
    let mut rendered = serde_json::Map::new();
    for selector in selectors {
        let bounds = visual.debug_bounds(*selector);
        rendered.insert(
            (*selector).into(),
            json!({"present":bounds.is_some(),"bounds":bounds.map(|bounds| format!("{bounds:?}"))}),
        );
    }
    fixture::write_trace(
        name,
        &json!({
            "source":{"file":file!(),"case":case},
            "renderedSelectors":rendered,
            "focusedControl":focused,
            "rpcCount":{"get":rpc.calls(methods::GET_PROJECT_COMMANDS),"save":rpc.calls(methods::SAVE_PROJECT_COMMANDS)},
            "rpc":rpc.trace().into_iter().take(12).collect::<Vec<_>>(),
        }),
    );
}

fn focus_from_preview(
    visual: &mut gpui::VisualTestContext,
    preview: &gpui::FocusHandle,
    steps: usize,
    selector: &'static str,
    rpc: &HeldRpc,
    trace_name: &str,
) -> gpui::FocusHandle {
    let bounds = visual.debug_bounds(selector);
    let focused = visual.update(|window, app| {
        window.focus(preview, app);
        for _ in 0..steps {
            window.focus_next(app);
        }
        window.focused(app)
    });
    fixture::write_trace(
        trace_name,
        &json!({
            "source":file!(),
            "targetSelector":selector,
            "targetBounds":bounds.map(|bounds| format!("{bounds:?}")),
            "startingHandle":format!("{preview:?}"),
            "tabSteps":steps,
            "focusedHandle":focused.as_ref().map(|handle| format!("{handle:?}")),
            "rpcCount":{"get":rpc.calls(methods::GET_PROJECT_COMMANDS),"save":rpc.calls(methods::SAVE_PROJECT_COMMANDS)},
        }),
    );
    focused.expect("target control must receive keyboard focus after tab navigation")
}

#[gpui::test]
fn command_card_rendered_edit_toggles_and_cancel(cx: &mut TestAppContext) {
    let runtime = runtime(cx);
    let _entered = runtime.enter();
    let (card, rpc, window) = scene(cx, &runtime);
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    trace_rendered_controls(
        &mut visual,
        &rpc,
        "project-command-ax-rendered-cancel-closed",
        "command_card_rendered_edit_toggles_and_cancel",
        &["qa-command-edit-build", "qa-command-remove-build"],
    );
    let edit = visual
        .debug_bounds("qa-command-edit-build")
        .expect("visible Edit must have a rendered button target");
    assert!(visual.debug_bounds("qa-command-remove-build").is_some());
    visual.update(|window, app| click(window, app, edit.center()));
    assert_eq!(
        card.read_with(cx, |card, _| card.editing.clone()),
        Some("build".into())
    );
    visual.update(|window, app| window.draw(app).clear());

    trace_rendered_controls(
        &mut visual,
        &rpc,
        "project-command-ax-rendered-cancel-editor",
        "command_card_rendered_edit_toggles_and_cancel",
        &[
            "qa-command-setup",
            "qa-command-wait",
            "qa-command-save",
            "qa-command-cancel",
        ],
    );

    let setup = visual
        .debug_bounds("qa-command-setup")
        .expect("setup choice must be painted");
    let wait = visual
        .debug_bounds("qa-command-wait")
        .expect("wait choice must be painted");
    assert!(visual.debug_bounds("qa-command-save").is_some());
    assert!(visual.debug_bounds("qa-command-cancel").is_some());
    visual.update(|window, app| click(window, app, setup.center()));
    assert!(card.read_with(cx, |card, _| card.setup));
    visual.update(|window, app| window.draw(app).clear());
    let preview_focus = card.read_with(cx, |card, cx| card.preview.read(cx).focus_handle(cx));
    let _setup_focus = focus_from_preview(
        &mut visual,
        &preview_focus,
        1,
        "qa-command-setup",
        &rpc,
        "project-command-ax-rendered-setup-focused",
    );
    visual.update(|window, app| key(window, app, "space"));
    assert!(!card.read_with(cx, |card, _| card.setup));
    visual.update(|window, app| click(window, app, wait.center()));
    assert!(!card.read_with(cx, |card, _| card.wait));
    visual.update(|window, app| window.draw(app).clear());
    let _wait_focus = focus_from_preview(
        &mut visual,
        &preview_focus,
        2,
        "qa-command-wait",
        &rpc,
        "project-command-ax-rendered-wait-focused",
    );
    visual.update(|window, app| key(window, app, "enter"));
    assert!(card.read_with(cx, |card, _| card.wait));

    // From Preview URL, the editor's tab stops are Setup, Wait, Save, Cancel.
    // Space on Cancel must leave persisted actions intact and issue no save.
    visual.update(|window, app| window.draw(app).clear());
    let _cancel_focus = focus_from_preview(
        &mut visual,
        &preview_focus,
        4,
        "qa-command-cancel",
        &rpc,
        "project-command-ax-rendered-cancel-focused",
    );
    visual.update(|window, app| key(window, app, "space"));
    let after = card.read_with(cx, |card, _| {
        json!({"editing":card.editing,"actions":card.actions,"setup":card.setup,"wait":card.wait})
    });
    fixture::write_trace(
        "project-command-ax-rendered-cancel",
        &json!({"after":after,"rpc":rpc.trace(),"path":"rendered Edit → setup/wait mouse and keyboard → Cancel keyboard"}),
    );
    assert!(card.read_with(cx, |card, _| card.editing.is_none()));
    assert_eq!(rpc.calls(methods::SAVE_PROJECT_COMMANDS), 0);
    assert_eq!(card.read_with(cx, |card, _| card.actions.len()), 1);
}

#[gpui::test]
fn command_card_keyboard_remove_sends_one_synthetic_save(cx: &mut TestAppContext) {
    let runtime = runtime(cx);
    let _entered = runtime.enter();
    let (_card, rpc, window) = scene(cx, &runtime);
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    trace_rendered_controls(
        &mut visual,
        &rpc,
        "project-command-ax-rendered-remove-closed",
        "command_card_keyboard_remove_sends_one_synthetic_save",
        &["qa-command-edit-build", "qa-command-remove-build"],
    );
    assert!(visual.debug_bounds("qa-command-remove-build").is_some());
    // The closed card's tab order is Add, Edit, Remove. Record the focused
    // handle and rendered target before invoking the destructive fixture RPC.
    let focused = visual.update(|window, app| {
        window.focus_next(app);
        window.focus_next(app);
        window.focus_next(app);
        window.focused(app)
    });
    fixture::write_trace(
        "project-command-ax-rendered-remove-focused",
        &json!({
            "source":file!(),
            "targetSelector":"qa-command-remove-build",
            "targetBounds":visual.debug_bounds("qa-command-remove-build").map(|bounds| format!("{bounds:?}")),
            "focusedHandle":focused.as_ref().map(|handle| format!("{handle:?}")),
            "tabSteps":3,
            "rpcCount":{"get":rpc.calls(methods::GET_PROJECT_COMMANDS),"save":rpc.calls(methods::SAVE_PROJECT_COMMANDS)},
        }),
    );
    assert!(focused.is_some());
    visual.update(|window, app| key(window, app, "enter"));
    fixture::pump_until(cx, &runtime, |_| {
        rpc.calls(methods::SAVE_PROJECT_COMMANDS) == 1
    });
    let requests = rpc.trace();
    fixture::write_trace(
        "project-command-ax-rendered-remove",
        &json!({"rpc":requests.clone(),"path":"rendered tab order → Remove Enter"}),
    );
    rpc.release(methods::SAVE_PROJECT_COMMANDS);
    assert_eq!(rpc.calls(methods::SAVE_PROJECT_COMMANDS), 1);
    assert_eq!(
        requests
            .iter()
            .filter(|entry| entry["phase"] == "request"
                && entry["method"] == methods::SAVE_PROJECT_COMMANDS)
            .count(),
        1,
    );
}

#[gpui::test]
fn command_card_keyboard_save_is_single_flight(cx: &mut TestAppContext) {
    let runtime = runtime(cx);
    let _entered = runtime.enter();
    let (card, rpc, window) = scene(cx, &runtime);
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    trace_rendered_controls(
        &mut visual,
        &rpc,
        "project-command-ax-rendered-save-closed",
        "command_card_keyboard_save_is_single_flight",
        &["qa-command-edit-build", "qa-command-remove-build"],
    );
    let edit = visual
        .debug_bounds("qa-command-edit-build")
        .expect("Edit must be painted before opening the editor");
    visual.update(|window, app| click(window, app, edit.center()));
    visual.update(|window, app| window.draw(app).clear());
    trace_rendered_controls(
        &mut visual,
        &rpc,
        "project-command-ax-rendered-save-editor",
        "command_card_keyboard_save_is_single_flight",
        &[
            "qa-command-setup",
            "qa-command-wait",
            "qa-command-save",
            "qa-command-cancel",
        ],
    );
    let setup = visual
        .debug_bounds("qa-command-setup")
        .expect("setup choice must be painted");
    assert!(visual.debug_bounds("qa-command-save").is_some());
    visual.update(|window, app| click(window, app, setup.center()));
    visual.update(|window, app| window.draw(app).clear());
    let preview_focus = card.read_with(cx, |card, cx| card.preview.read(cx).focus_handle(cx));
    let _save_focus = focus_from_preview(
        &mut visual,
        &preview_focus,
        3,
        "qa-command-save",
        &rpc,
        "project-command-ax-rendered-save-focused",
    );
    // From Preview URL, Tab passes Setup and Wait before Save. A second Enter
    // before the held save reply must not issue another old-frame request.
    visual.update(|window, app| {
        key(window, app, "enter");
        key(window, app, "enter");
    });
    fixture::pump_until(cx, &runtime, |_| {
        rpc.calls(methods::SAVE_PROJECT_COMMANDS) >= 1
    });
    let requests = rpc.trace();
    fixture::write_trace(
        "project-command-ax-rendered-save",
        &json!({"rpc":requests.clone(),"busy":card.read_with(cx, |card, _| card.busy),"path":"rendered Setup → Save Enter twice while reply held"}),
    );
    rpc.release(methods::SAVE_PROJECT_COMMANDS);
    assert_eq!(rpc.calls(methods::SAVE_PROJECT_COMMANDS), 1);
}
