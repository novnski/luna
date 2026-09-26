//! Failure-first GPUI/RPC integration. No command, terminal or provider is spawned.
//! The real retained Commands child and real TerminalPanel Output listener run.
use super::*;
use crate::{
    engine_epoch_fixture::{self as fixture, HeldRpc},
    terminal::panel::TerminalPanel,
};
use gpui::{AppContext, TestAppContext};
use luna_proto::{Chat, ProjectCommandStatus};
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use tokio::runtime::Runtime;

const CHAT: &str = "epoch-shared-chat";
const PROJECT: &str = "epoch-project";

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
fn status(label: &str) -> Value {
    serde_json::to_value(ProjectCommandsSnapshot {
        project_id: PROJECT.into(),
        cwd: format!("/{label}/checkout"),
        actions: vec![action()],
        runs: vec![],
    })
    .unwrap()
}
fn run(label: &str, device: &str) -> Value {
    serde_json::to_value(ProjectCommandRun {
        id: format!("{label}-run"),
        project_id: PROJECT.into(),
        device_id: device.into(),
        chat_id: CHAT.into(),
        cwd: format!("/{label}/checkout"),
        checkout_generation: format!("{label}-checkout"),
        action: action(),
        action_digest: format!("{label}-action"),
        setup: false,
        attempt: 1,
        status: ProjectCommandStatus::Running,
        terminal_id: Some(format!("{label}-terminal")),
        previous_terminal_ids: vec![],
        process_group_id: None,
        exit_code: None,
        detail: None,
        updated_at_ms: 1,
    })
    .unwrap()
}
fn runtime(cx: &mut TestAppContext) -> Runtime {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    cx.update(|cx| gpui_tokio::init_from_handle(cx, runtime.handle().clone()));
    runtime
}
struct Scene {
    state: Entity<AppState>,
    parent: Entity<TerminalPanel>,
    outputs: Arc<Mutex<Vec<Value>>>,
    _subscription: Subscription,
}
impl Scene {
    fn new(cx: &mut TestAppContext, runtime: &Runtime, rpc: Arc<HeldRpc>, device: &str) -> Self {
        let chat: Chat=serde_json::from_value(json!({"id":CHAT,"deviceId":device,"spaceId":PROJECT,"cwd":"/fixture/checkout","createdAt":"2026-09-26T00:00:00Z","archived":false})).unwrap();
        let state = cx.new(|_| {
            let mut state = AppState::new();
            state.chats = vec![chat];
            state.selected_chat = Some(CHAT.into());
            state.chats_synced = true;
            state
        });
        fixture::attach(cx, &state, fixture::handle(runtime, rpc));
        let parent = cx.new(|cx| TerminalPanel::new(state.clone(), cx));
        let commands = parent.read_with(cx, |panel, _| panel.command_panel_for_epoch_fixture());
        let outputs = Arc::new(Mutex::new(Vec::new()));
        let captured = outputs.clone();
        let subscription = cx.update(|cx| {
            cx.subscribe(&commands, move |_, event: &ProjectCommandEvent, _| {
                let ProjectCommandEvent::Output {
                    chat_id,
                    terminal_id,
                    ..
                } = event;
                captured
                    .lock()
                    .unwrap()
                    .push(json!({"chatId":chat_id,"terminalId":terminal_id}));
            })
        });
        cx.run_until_parked();
        Self {
            state,
            parent,
            outputs,
            _subscription: subscription,
        }
    }
    fn commands<C: gpui::AppContext>(&self, cx: &C) -> Entity<ProjectCommandPanel> {
        self.parent
            .read_with(cx, |parent, _| parent.command_panel_for_epoch_fixture())
    }
    fn open(&self, cx: &mut TestAppContext) {
        self.commands(cx).update(cx, |view, cx| {
            view.expanded = true;
            view.load(cx);
        });
    }
    fn execute(&self, cx: &mut TestAppContext) {
        self.commands(cx).update(cx, |view, cx| {
            view.execute(
                methods::RUN_PROJECT_COMMAND,
                "build".into(),
                String::new(),
                cx,
            )
        });
    }
    fn replace(&self, cx: &mut TestAppContext, runtime: &Runtime, rpc: Arc<HeldRpc>, device: &str) {
        // Keep the same visible task ID, with B's controlled owner projection.
        // EngineHandle replacement itself uses the production attach boundary.
        self.state
            .update(cx, |state, _| state.chats[0].device_id = device.into());
        fixture::attach(cx, &self.state, fixture::handle(runtime, rpc));
    }
    fn view<C: gpui::AppContext>(&self, cx: &C) -> Value {
        let scope=self.state.read_with(cx,|state,_|json!({"generation":state.engine_generation(),"device":state.engine().map(|e|e.engine_info().device_id.clone()),"chat":state.selected_chat}));
        let commands = self.commands(cx);
        commands.read_with(cx,|view,_|json!({"scope":scope,"cachedScope":{"generation":view.scope.engine_generation,"device":view.scope.engine_device,"chat":view.scope.chat},"entity":format!("{:?}",commands.entity_id()),"busy":view.busy,"snapshot":view.snapshot,"error":view.error,"outputs":self.outputs.lock().unwrap().clone()}))
    }
}

fn delayed_status(cx: &mut TestAppContext, error: bool) {
    let runtime = runtime(cx);
    let _entered = runtime.enter();
    let device_b = if error { "device-b" } else { "same-device" };
    let a = HeldRpc::new("A", "same-device");
    let b = HeldRpc::new("B", device_b);
    a.reply(
        methods::PROJECT_COMMAND_STATUS,
        if error {
            Err("A status failure".into())
        } else {
            Ok(status("A"))
        },
        true,
    );
    b.reply(methods::PROJECT_COMMAND_STATUS, Ok(status("B")), true);
    let scene = Scene::new(cx, &runtime, a.clone(), "same-device");
    scene.open(cx);
    fixture::pump_until(cx, &runtime, |_| {
        a.calls(methods::PROJECT_COMMAND_STATUS) == 1
    });
    let before = scene.view(cx);
    scene.replace(cx, &runtime, b.clone(), device_b);
    fixture::pump_until(cx, &runtime, |_| b.calls(methods::WATCH_SESSIONS) > 0);
    a.release(methods::PROJECT_COMMAND_STATUS);
    fixture::pump_until(cx, &runtime, |cx| {
        let view = scene.view(cx);
        a.settled(methods::PROJECT_COMMAND_STATUS) > 0
            && (view["snapshot"].is_object()
                || view["error"].is_string()
                || b.calls(methods::PROJECT_COMMAND_STATUS) > 0)
    });
    let after = scene.view(cx);
    let new_calls = b.calls(methods::PROJECT_COMMAND_STATUS);
    fixture::write_trace(
        if error {
            "commands-delayed-status-error"
        } else {
            "commands-delayed-status-success"
        },
        &json!({"before":before,"after":after,"newStatusCalls":new_calls,"oldRpc":a.trace(),"newRpc":b.trace()}),
    );
    b.release(methods::PROJECT_COMMAND_STATUS);
    fixture::drain(cx, &runtime);
    assert_ne!(before["scope"]["generation"], after["scope"]["generation"]);
    assert_eq!(after["scope"]["chat"], CHAT);
    assert_ne!(
        after["snapshot"]["cwd"], "/A/checkout",
        "old-engine status reached the replacement panel"
    );
    assert_ne!(
        after["error"], "A status failure",
        "old-engine error reached the replacement panel"
    );
    assert!(
        new_calls > 0,
        "replacement engine must be loadable without waiting for the old engine"
    );
}
#[gpui::test]
fn commands_engine_epoch_delayed_status_success(cx: &mut TestAppContext) {
    delayed_status(cx, false);
}
#[gpui::test]
fn commands_engine_epoch_delayed_status_error(cx: &mut TestAppContext) {
    delayed_status(cx, true);
}

fn delayed_execute(cx: &mut TestAppContext, device_b: &str) {
    let runtime = runtime(cx);
    let _entered = runtime.enter();
    let a = HeldRpc::new("A", "device-a");
    let b = HeldRpc::new("B", device_b);
    a.reply(methods::PROJECT_COMMAND_STATUS, Ok(status("A")), false);
    a.reply(methods::RUN_PROJECT_COMMAND, Ok(run("A", "device-a")), true);
    b.reply(methods::PROJECT_COMMAND_STATUS, Ok(status("B")), true);
    b.reply(methods::LIST_TERMINALS, Ok(json!([])), false);
    let scene = Scene::new(cx, &runtime, a.clone(), "device-a");
    scene.open(cx);
    fixture::pump_until(cx, &runtime, |cx| {
        scene.view(cx)["snapshot"]["cwd"] == "/A/checkout"
    });
    scene.execute(cx);
    fixture::pump_until(cx, &runtime, |_| a.calls(methods::RUN_PROJECT_COMMAND) == 1);
    let before = scene.view(cx);
    scene.replace(cx, &runtime, b.clone(), device_b);
    fixture::pump_until(cx, &runtime, |_| b.calls(methods::WATCH_SESSIONS) > 0);
    a.release(methods::RUN_PROJECT_COMMAND);
    // Baseline's old completion starts B's load after emitting Output. A fixed
    // observer can instead start B's load on replacement and cancel A's waiter.
    fixture::pump_until(cx, &runtime, |_| {
        a.settled(methods::RUN_PROJECT_COMMAND) > 0 && b.calls(methods::PROJECT_COMMAND_STATUS) > 0
    });
    b.release(methods::PROJECT_COMMAND_STATUS);
    fixture::pump_until(cx, &runtime, |cx| {
        b.settled(methods::PROJECT_COMMAND_STATUS) > 0 && scene.view(cx)["busy"] == false
    });
    let after = scene.view(cx);
    let terminal_lookups = b.calls(methods::LIST_TERMINALS);
    fixture::write_trace(
        if device_b == "device-a" {
            "commands-delayed-execute-same-device"
        } else {
            "commands-delayed-execute-different-device"
        },
        &json!({"before":before,"after":after,"newTerminalLookups":terminal_lookups,"oldRpc":a.trace(),"newRpc":b.trace()}),
    );
    assert!(
        scene.outputs.lock().unwrap().is_empty(),
        "old-engine execution emitted Output on the replacement connection"
    );
    assert_eq!(
        terminal_lookups, 0,
        "actual parent listener looked up old-engine output through B"
    );
    assert_eq!(after["snapshot"]["cwd"], "/B/checkout");
}

#[gpui::test]
fn commands_engine_epoch_execute_same_device_generation(cx: &mut TestAppContext) {
    delayed_execute(cx, "device-a");
}

#[gpui::test]
fn commands_engine_epoch_execute_different_device(cx: &mut TestAppContext) {
    delayed_execute(cx, "device-b");
}

#[gpui::test]
fn commands_engine_epoch_stuck_old_request_cannot_keep_new_engine_busy(cx: &mut TestAppContext) {
    let runtime = runtime(cx);
    let _entered = runtime.enter();
    let a = HeldRpc::new("A", "same-device");
    let b = HeldRpc::new("B", "same-device");
    a.reply(methods::PROJECT_COMMAND_STATUS, Ok(status("A")), true);
    b.reply(methods::PROJECT_COMMAND_STATUS, Ok(status("B")), false);
    let scene = Scene::new(cx, &runtime, a.clone(), "same-device");
    scene.open(cx);
    fixture::pump_until(cx, &runtime, |_| {
        a.calls(methods::PROJECT_COMMAND_STATUS) == 1
    });
    let before = scene.view(cx);
    scene.replace(cx, &runtime, b.clone(), "same-device");
    fixture::pump_until(cx, &runtime, |_| b.calls(methods::WATCH_SESSIONS) > 0);
    // Exercise one actual production poll tick while A remains unreleased.
    cx.executor()
        .advance_clock(std::time::Duration::from_secs(1));
    for _ in 0..100 {
        cx.run_until_parked();
        runtime.block_on(tokio::task::yield_now());
        if b.settled(methods::PROJECT_COMMAND_STATUS) > 0 && scene.view(cx)["busy"] == false {
            break;
        }
    }
    let after = scene.view(cx);
    let new_calls = b.calls(methods::PROJECT_COMMAND_STATUS);
    fixture::write_trace(
        "commands-stuck-old-request",
        &json!({"before":before,"after":after,"newStatusCalls":new_calls,"oldRpcBeforeRelease":a.trace(),"newRpc":b.trace()}),
    );
    a.release(methods::PROJECT_COMMAND_STATUS);
    fixture::pump_until(cx, &runtime, |_| {
        a.settled(methods::PROJECT_COMMAND_STATUS) > 0
    });
    assert!(
        new_calls > 0,
        "A's held request prevented B's production poll from loading"
    );
    assert_eq!(after["busy"], false, "old engine retained busy state on B");
    assert_eq!(after["snapshot"]["cwd"], "/B/checkout");
}

#[gpui::test]
fn commands_engine_epoch_same_engine_control_remains_usable(cx: &mut TestAppContext) {
    let runtime = runtime(cx);
    let _entered = runtime.enter();
    let a = HeldRpc::new("A", "device-a");
    a.reply(methods::PROJECT_COMMAND_STATUS, Ok(status("A")), false);
    a.reply(methods::RUN_PROJECT_COMMAND, Ok(run("A", "device-a")), true);
    a.reply(methods::LIST_TERMINALS, Ok(json!([])), false);
    let scene = Scene::new(cx, &runtime, a.clone(), "device-a");
    scene.open(cx);
    fixture::pump_until(cx, &runtime, |cx| {
        scene.view(cx)["snapshot"]["cwd"] == "/A/checkout"
    });
    let before = scene.view(cx);
    scene.execute(cx);
    fixture::pump_until(cx, &runtime, |_| a.calls(methods::RUN_PROJECT_COMMAND) == 1);
    a.release(methods::RUN_PROJECT_COMMAND);
    fixture::pump_until(cx, &runtime, |cx| {
        a.calls(methods::LIST_TERMINALS) > 0 && scene.view(cx)["busy"] == false
    });
    let after = scene.view(cx);
    let emitted = scene.outputs.lock().unwrap().clone();
    fixture::write_trace(
        "commands-same-engine-control",
        &json!({"before":before,"after":after,"rpc":a.trace()}),
    );
    assert_eq!(
        emitted,
        vec![json!({"chatId":CHAT,"terminalId":"A-terminal"})]
    );
    assert_eq!(before["scope"], after["scope"]);
    assert_eq!(after["snapshot"]["cwd"], "/A/checkout");
    let calls = a.calls(methods::PROJECT_COMMAND_STATUS);
    let parent_weak = scene.parent.downgrade();
    let commands_weak = scene.commands(cx).downgrade();
    // GPUI releases dropped entity values during an App update. Dropping the
    // outer Scene outside that boundary leaves its queued parent value still
    // owning the child until the next update, so advancing time first would
    // test an entity which had not actually been disposed yet.
    cx.update(|_| drop(scene));
    cx.run_until_parked();
    let parent_released = parent_weak.upgrade().is_none();
    let commands_released = commands_weak.upgrade().is_none();
    cx.executor()
        .advance_clock(std::time::Duration::from_secs(2));
    fixture::drain(cx, &runtime);
    fixture::write_trace(
        "commands-same-engine-control-drop",
        &json!({
            "parentReleased": parent_released,
            "commandsReleased": commands_released,
            "statusCallsBeforeDrop": calls,
            "statusCallsAfterDrop": a.calls(methods::PROJECT_COMMAND_STATUS),
            "rpc": a.trace(),
        }),
    );
    assert!(
        parent_released,
        "parent must be disposed before checking the child poller"
    );
    assert!(
        commands_released,
        "Commands child must be disposed before advancing its timer"
    );
    assert_eq!(
        a.calls(methods::PROJECT_COMMAND_STATUS),
        calls,
        "dropped entity kept polling"
    );
}

// Outbound-admission reproduction: actual painted callbacks and mouse dispatch,
// not a direct invocation of execute() with a synthetic origin predicate.
struct ButtonSurface {
    commands: Entity<ProjectCommandPanel>,
    renders: Arc<AtomicUsize>,
}
impl Render for ButtonSurface {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.renders.fetch_add(1, Ordering::SeqCst);
        div().size_full().child(self.commands.clone())
    }
}
fn button_window(
    cx: &mut TestAppContext,
    scene: &Scene,
    selector: &'static str,
) -> (
    gpui::WindowHandle<ButtonSurface>,
    gpui::Point<gpui::Pixels>,
    Arc<AtomicUsize>,
    gpui::Bounds<gpui::Pixels>,
) {
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::default());
    });
    let commands = scene.commands(cx);
    let renders = Arc::new(AtomicUsize::new(0));
    let view_renders = renders.clone();
    let window = cx.open_window(gpui::size(px(680.0), px(420.0)), move |_, _| {
        ButtonSurface {
            commands,
            renders: view_renders,
        }
    });
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    // A typed WindowHandle update leases ButtonSurface. Drawing while that
    // lease is held would recursively update the same root. Use the raw
    // Window/App boundary, just like real window rendering and event dispatch.
    visual.update(|window, cx| window.draw(cx).clear());
    let bounds = visual
        .debug_bounds(selector)
        .expect("the actual enabled command button was painted");
    (window, bounds.center(), renders, bounds)
}
fn mouse_click_rendered(window: &mut Window, cx: &mut gpui::App, point: gpui::Point<gpui::Pixels>) {
    window.dispatch_event(
        gpui::PlatformInput::MouseDown(gpui::MouseDownEvent {
            position: point,
            button: gpui::MouseButton::Left,
            modifiers: Default::default(),
            click_count: 1,
            first_mouse: false,
        }),
        cx,
    );
    window.dispatch_event(
        gpui::PlatformInput::MouseUp(gpui::MouseUpEvent {
            position: point,
            button: gpui::MouseButton::Left,
            modifiers: Default::default(),
            click_count: 1,
        }),
        cx,
    );
}
fn stale_button_admission(cx: &mut TestAppContext, method: &'static str) {
    let runtime = runtime(cx);
    let _entered = runtime.enter();
    let a = HeldRpc::new("A", "same-device");
    let b = HeldRpc::new("B", "same-device");
    let mut snapshot = status("A");
    let selector = if method == methods::RUN_PROJECT_COMMAND {
        "qa-command-run-build"
    } else {
        let mut receipt = run("A", "same-device");
        if method == methods::RETRY_PROJECT_COMMAND {
            receipt["status"] = json!("failed");
        }
        snapshot["runs"] = json!([receipt]);
        "qa-command-control-A-run"
    };
    a.reply(methods::PROJECT_COMMAND_STATUS, Ok(snapshot), false);
    b.reply(methods::PROJECT_COMMAND_STATUS, Ok(status("B")), true);
    b.reply(method, Ok(run("B", "same-device")), true);
    let scene = Scene::new(cx, &runtime, a.clone(), "same-device");
    scene.open(cx);
    fixture::pump_until(cx, &runtime, |cx| {
        scene.view(cx)["snapshot"]["cwd"] == "/A/checkout"
    });
    let (window, point, renders, painted_bounds) = button_window(cx, &scene, selector);
    let painted_render = renders.load(Ordering::SeqCst);
    let painted = scene.view(cx);
    let replacement = fixture::handle(&runtime, b.clone());
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    let old_bounds_retained = visual.debug_bounds(selector).is_some();
    // Test-support eagerly paints dirty windows when an outer App update
    // finishes. Keep the actual connection transition and actual mouse input
    // within one raw Window/App update, measuring before that test-only paint.
    let (before_click, before_input_render, after_input_render, delivery) =
        visual.update(|window, app| {
            scene.state.update(app, |state, cx| {
                state.chats[0].device_id = "same-device".into();
                state.attach_engine_for_epoch_fixture(replacement, cx);
            });
            let before_click = scene.view(app);
            let before_input_render = renders.load(Ordering::SeqCst);
            mouse_click_rendered(window, app, point);
            let after_input_render = renders.load(Ordering::SeqCst);
            let delivery = json!({
                "paintedBounds":format!("{painted_bounds:?}"),"inputPoint":format!("{point:?}"),
                "liveBefore":before_click,"liveAfter":scene.view(app),
                "renderBefore":before_input_render,"renderAfter":after_input_render,
            });
            (
                before_click,
                before_input_render,
                after_input_render,
                delivery,
            )
        });
    let after_update_render = renders.load(Ordering::SeqCst);
    fixture::drain(cx, &runtime);
    let dispatched = b.calls(method);
    fixture::write_trace(
        &format!("commands-button-stale-{method}"),
        &json!({
            "painted":painted,"beforeClick":before_click,"afterClick":scene.view(cx),"delivery":delivery,
            "oldBoundsRetained":old_bounds_retained,"dispatchedToB":dispatched,"oldRpc":a.trace(),"newRpc":b.trace(),
            "paintedRender":painted_render,"beforeInputRender":before_input_render,"afterInputRender":after_input_render,"afterUpdateRender":after_update_render,
            "mousePath":"Window::dispatch_event(MouseDown/MouseUp)","buttonCenter":format!("{point:?}"),
        }),
    );
    b.release(method);
    b.release(methods::PROJECT_COMMAND_STATUS);
    assert!(
        old_bounds_retained,
        "fixture must deliver input to the retained painted frame"
    );
    assert_eq!(
        painted_render, before_input_render,
        "fixture unexpectedly redrew before stale input"
    );
    assert_eq!(
        painted_render, after_input_render,
        "mouse dispatch unexpectedly repainted the callback frame"
    );
    assert_ne!(
        painted["scope"]["generation"],
        before_click["scope"]["generation"]
    );
    assert_eq!(
        dispatched, 0,
        "an old rendered {method} callback was rebound to engine B"
    );
}
#[gpui::test]
fn commands_button_admission_stale_run(cx: &mut TestAppContext) {
    stale_button_admission(cx, methods::RUN_PROJECT_COMMAND);
}
#[gpui::test]
fn commands_button_admission_stale_retry(cx: &mut TestAppContext) {
    stale_button_admission(cx, methods::RETRY_PROJECT_COMMAND);
}
#[gpui::test]
fn commands_button_admission_stale_stop(cx: &mut TestAppContext) {
    stale_button_admission(cx, methods::STOP_PROJECT_COMMAND);
}

#[gpui::test]
fn commands_button_admission_live_busy_rejects_old_enabled_callback(cx: &mut TestAppContext) {
    let runtime = runtime(cx);
    let _entered = runtime.enter();
    let a = HeldRpc::new("A", "device-a");
    a.reply(methods::PROJECT_COMMAND_STATUS, Ok(status("A")), false);
    a.reply(methods::RUN_PROJECT_COMMAND, Ok(run("A", "device-a")), true);
    let scene = Scene::new(cx, &runtime, a.clone(), "device-a");
    scene.open(cx);
    fixture::pump_until(cx, &runtime, |cx| {
        scene.view(cx)["snapshot"]["cwd"] == "/A/checkout"
    });
    let (window, point, renders, painted_bounds) =
        button_window(cx, &scene, "qa-command-run-build");
    let painted_render = renders.load(Ordering::SeqCst);
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    let (before_click, input_render, delivery) = visual.update(|window, app| {
        scene.commands(app).update(app, |view, cx| {
            view.execute(
                methods::RUN_PROJECT_COMMAND,
                "already-starting".into(),
                String::new(),
                cx,
            )
        });
        let before_click = scene.view(app);
        mouse_click_rendered(window, app, point);
        let input_render = renders.load(Ordering::SeqCst);
        let delivery = json!({"paintedBounds":format!("{painted_bounds:?}"),"inputPoint":format!("{point:?}"),"liveBefore":before_click,"liveAfter":scene.view(app),"renderAtInput":input_render});
        (before_click, input_render, delivery)
    });
    let after_update_render = renders.load(Ordering::SeqCst);
    fixture::drain(cx, &runtime);
    let trace = a.trace();
    let build_requests = trace
        .iter()
        .filter(|entry| {
            entry["phase"] == "request"
                && entry["method"] == methods::RUN_PROJECT_COMMAND
                && entry["params"]["actionId"] == "build"
        })
        .count();
    fixture::write_trace(
        "commands-button-live-busy",
        &json!({"beforeClick":before_click,"afterClick":scene.view(cx),"buildRequests":build_requests,"delivery":delivery,"rpc":trace,"paintedRender":painted_render,"inputRender":input_render,"afterUpdateRender":after_update_render,"mousePath":"Window::dispatch_event(MouseDown/MouseUp)"}),
    );
    a.release(methods::RUN_PROJECT_COMMAND);
    assert_eq!(
        painted_render, input_render,
        "busy input must use the enabled painted frame"
    );
    assert_eq!(
        before_click["busy"], true,
        "busy must come from the actual first execution path"
    );
    assert_eq!(
        build_requests, 0,
        "old enabled button dispatched a second action while live busy"
    );
}

#[gpui::test]
fn commands_button_admission_fresh_same_engine_control(cx: &mut TestAppContext) {
    let runtime = runtime(cx);
    let _entered = runtime.enter();
    let a = HeldRpc::new("A", "device-a");
    a.reply(methods::PROJECT_COMMAND_STATUS, Ok(status("A")), false);
    a.reply(methods::RUN_PROJECT_COMMAND, Ok(run("A", "device-a")), true);
    let scene = Scene::new(cx, &runtime, a.clone(), "device-a");
    scene.open(cx);
    fixture::pump_until(cx, &runtime, |cx| {
        scene.view(cx)["snapshot"]["cwd"] == "/A/checkout"
    });
    let (window, point, renders, painted_bounds) =
        button_window(cx, &scene, "qa-command-run-build");
    let painted_render = renders.load(Ordering::SeqCst);
    let before_click = scene.view(cx);
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    let (input_render, delivery) = visual.update(|window, app| {
        mouse_click_rendered(window, app, point);
        let input_render = renders.load(Ordering::SeqCst);
        (input_render, json!({"paintedBounds":format!("{painted_bounds:?}"),"inputPoint":format!("{point:?}"),"liveAfter":scene.view(app),"renderAtInput":input_render}))
    });
    let after_update_render = renders.load(Ordering::SeqCst);
    fixture::pump_until(cx, &runtime, |_| a.calls(methods::RUN_PROJECT_COMMAND) == 1);
    fixture::write_trace(
        "commands-button-fresh-control",
        &json!({"beforeClick":before_click,"afterClick":scene.view(cx),"rpc":a.trace(),"delivery":delivery,"paintedRender":painted_render,"inputRender":input_render,"afterUpdateRender":after_update_render,"mousePath":"Window::dispatch_event(MouseDown/MouseUp)"}),
    );
    a.release(methods::RUN_PROJECT_COMMAND);
    assert_eq!(before_click["busy"], false);
    assert_eq!(
        a.calls(methods::RUN_PROJECT_COMMAND),
        1,
        "fresh eligible rendered button must dispatch exactly once"
    );
}

#[path = "project_commands_same_chat_tests.rs"]
mod same_chat_context_tests;

#[gpui::test]
fn commands_retry_current_stop_survives_status_poll(cx: &mut TestAppContext) {
    let runtime = runtime(cx);
    let _entered = runtime.enter();
    let rpc = HeldRpc::new("retry-stop", "device-a");
    let mut attempt_one = run("A", "device-a");
    attempt_one["status"] = json!("canceled");
    attempt_one["terminalId"] = json!("A-terminal-1");
    let mut attempt_two = run("A", "device-a");
    attempt_two["attempt"] = json!(2);
    attempt_two["terminalId"] = json!("A-terminal-2");
    attempt_two["previousTerminalIds"] = json!(["A-terminal-1"]);
    let mut initial = status("A");
    initial["runs"] = json!([attempt_one]);
    let mut running = status("A");
    running["runs"] = json!([attempt_two.clone()]);
    rpc.reply(methods::PROJECT_COMMAND_STATUS, Ok(initial), false);
    rpc.reply(
        methods::RETRY_PROJECT_COMMAND,
        Ok(attempt_two.clone()),
        false,
    );
    let mut canceled = attempt_two.clone();
    canceled["status"] = json!("canceled");
    canceled["detail"] = json!("Stopped by you");
    rpc.reply(methods::STOP_PROJECT_COMMAND, Ok(canceled.clone()), true);

    let scene = Scene::new(cx, &runtime, rpc.clone(), "device-a");
    scene.open(cx);
    fixture::pump_until(cx, &runtime, |cx| {
        scene.view(cx)["snapshot"]["runs"][0]["attempt"] == 1 && scene.view(cx)["busy"] == false
    });
    let (window, retry_point, _, _) = button_window(cx, &scene, "qa-command-control-A-run");
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    // The next status result belongs to the retried attempt, not the old
    // snapshot. The actual Retry button must initiate exactly one write.
    rpc.reply(methods::PROJECT_COMMAND_STATUS, Ok(running.clone()), false);
    visual.update(|window, app| mouse_click_rendered(window, app, retry_point));
    fixture::pump_until(cx, &runtime, |cx| {
        let view = scene.view(cx);
        view["snapshot"]["runs"][0]["attempt"] == 2 && view["busy"] == false
    });
    let after_retry = scene.view(cx);
    let prior_status_calls = rpc.calls(methods::PROJECT_COMMAND_STATUS);
    // Match N3's visible Running/Stop row while a production one-second
    // status read is in flight. Its response remains held through the click.
    rpc.reply(methods::PROJECT_COMMAND_STATUS, Ok(running), true);
    cx.executor()
        .advance_clock(std::time::Duration::from_secs(1));
    fixture::pump_until(cx, &runtime, |cx| {
        rpc.calls(methods::PROJECT_COMMAND_STATUS) > prior_status_calls
            && scene.view(cx)["busy"] == true
    });
    visual.update(|window, app| window.draw(app).clear());
    let stop_bounds = visual
        .debug_bounds("qa-command-control-A-run")
        .expect("the Running attempt's Stop control remains painted");
    let stop_point = stop_bounds.center();
    let before_stop = scene.view(cx);
    visual.update(|window, app| mouse_click_rendered(window, app, stop_point));
    fixture::drain(cx, &runtime);
    let first_stop_calls = rpc.calls(methods::STOP_PROJECT_COMMAND);
    let mut after_stop = Value::Null;
    if first_stop_calls > 0 {
        // The same still-visible control must not send a duplicate write
        // while Stop's reply is pending.
        visual.update(|window, app| mouse_click_rendered(window, app, stop_point));
        fixture::drain(cx, &runtime);
        let mut canceled_snapshot = status("A");
        canceled_snapshot["runs"] = json!([canceled]);
        rpc.reply(
            methods::PROJECT_COMMAND_STATUS,
            Ok(canceled_snapshot),
            false,
        );
        rpc.release(methods::STOP_PROJECT_COMMAND);
        fixture::pump_until(cx, &runtime, |cx| {
            scene.view(cx)["snapshot"]["runs"][0]["status"] == "canceled"
                && scene.view(cx)["busy"] == false
        });
        after_stop = scene.view(cx);
    }
    let trace = json!({
        "afterRetry":after_retry,"beforeStop":before_stop,"afterStop":after_stop,
        "stopBounds":format!("{stop_bounds:?}"),"stopPoint":format!("{stop_point:?}"),
        "statusCallsBeforeHeldPoll":prior_status_calls,
        "statusCallsWithHeldPoll":rpc.calls(methods::PROJECT_COMMAND_STATUS),
        "firstStopCalls":first_stop_calls,"totalStopCalls":rpc.calls(methods::STOP_PROJECT_COMMAND),
        "retryCalls":rpc.calls(methods::RETRY_PROJECT_COMMAND),
        "runCalls":rpc.calls(methods::RUN_PROJECT_COMMAND),"rpc":rpc.trace(),
        "mousePath":"Window::dispatch_event(MouseDown/MouseUp)"
    });
    fixture::write_trace("commands-retry-stop-during-status-poll", &trace);
    assert_eq!(after_retry["snapshot"]["runs"][0]["id"], "A-run");
    assert_eq!(
        after_retry["snapshot"]["runs"][0]["terminalId"],
        "A-terminal-2"
    );
    assert_eq!(before_stop["snapshot"]["runs"][0]["status"], "running");
    assert_eq!(rpc.calls(methods::RETRY_PROJECT_COMMAND), 1);
    assert_eq!(rpc.calls(methods::RUN_PROJECT_COMMAND), 0);
    assert_eq!(
        first_stop_calls, 1,
        "visible Stop was lost during a status read"
    );
    assert_eq!(rpc.calls(methods::STOP_PROJECT_COMMAND), 1);
    assert_eq!(after_stop["snapshot"]["runs"][0]["status"], "canceled");
}
