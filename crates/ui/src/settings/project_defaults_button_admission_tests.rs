//! Failure-first rendered Defaults admission cases. No providers or shells run.
//! Uses the validated single raw Window/App delivery boundary; no input callback
//! is invoked manually. The shared held-RPC helper is unchanged.
use super::*;
use crate::{
    engine_epoch_fixture::{self as fixture, HeldRpc},
    pickers::{CheckoutKind, DraftConfig, Pickers, ResolvedRunConfig},
};
use gpui::{AppContext, TestAppContext};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::runtime::Runtime;

const A: &str = "defaults-admission-a";
const B: &str = "defaults-admission-b";
const BUTTON: &str = "qa-defaults-new-worktree";

fn runtime(cx: &mut TestAppContext) -> Runtime {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    cx.update(|cx| gpui_tokio::init_from_handle(cx, runtime.handle().clone()));
    runtime
}
fn space(id: &str, device: &str) -> Space {
    serde_json::from_value(json!({
        "id":id,"deviceId":device,"path":format!("/fixture/{device}/{id}"),
        "name":id,"gitDetected":true,"createdAt":"2026-09-26T00:00:00Z",
        "executionDefaults":{"permissionMode":"auto","checkout":"local"}
    }))
    .unwrap()
}
fn scope(scope: Option<&ProjectRequestScope>) -> Value {
    scope.map(|scope|json!({"project":scope.project_id,"owner":scope.project_device_id,"generation":scope.engine_generation,"engineDevice":scope.engine_device_id})).unwrap_or(Value::Null)
}
fn saves(rpc: &HeldRpc) -> Vec<Value> {
    rpc.trace()
        .into_iter()
        .filter(|event| {
            event["phase"] == "request"
                && event["method"] == methods::MUTATE
                && event["params"]["op"] == "setProjectExecutionDefaults"
        })
        .collect()
}
struct Scene {
    state: Entity<AppState>,
    page: Entity<ProjectsPage>,
    pickers: Entity<Pickers>,
    a: Arc<HeldRpc>,
}
impl Scene {
    fn new(cx: &mut TestAppContext, runtime: &Runtime) -> Self {
        let a = HeldRpc::new("Defaults-A", "device-a");
        a.reply(methods::GET_PROJECT_COMMANDS, Ok(json!([])), false);
        a.reply(methods::MUTATE, Ok(json!({"ok":true})), true);
        let state = cx.new(|_| AppState::new());
        fixture::attach(cx, &state, fixture::handle(runtime, a.clone()));
        state.update(cx, |state, cx| {
            state.selected_space = Some(A.into());
            state.selected_device = Some("device-a".into());
            state.apply_spaces(vec![space(A, "device-a"), space(B, "device-a")]);
            cx.notify();
        });
        fixture::drain(cx, runtime);
        let page = cx.new(|cx| ProjectsPage::new(state.clone(), cx));
        let pickers = cx.new(|cx| Pickers::new(state.clone(), cx));
        let scene = Self {
            state,
            page,
            pickers,
            a,
        };
        scene.seed_draft(cx);
        scene
    }
    fn seed_draft(&self, cx: &mut TestAppContext) {
        let draft = DraftConfig {
            harness: Some(HarnessId::Codex),
            model: Some("deliberate-current-draft".into()),
            permission_mode: Some(PermissionMode::Auto),
            checkout: CheckoutKind::Local,
            branch: Some("qa-draft-branch".into()),
            ..Default::default()
        };
        let value = json!({"draft":draft,"resolved":ResolvedRunConfig::default()});
        self.pickers.update(cx, |pickers, cx| {
            pickers.restore_draft_execution(&value, cx)
        });
    }
    fn view<C: AppContext>(&self, cx: &C) -> Value {
        let state=self.state.read_with(cx,|state,_|json!({"generation":state.engine_generation(),"engineDevice":state.engine().map(|e|e.engine_info().device_id.clone()),"composerProject":state.selected_space,"spaces":state.spaces}));
        let page=self.page.read_with(cx,|page,cx|json!({"selectedProject":page.selected_project_id,"liveScope":scope(page.current_request_scope(cx).as_ref()),"cachedScope":scope(page.request_scope.as_ref()),"busy":page.busy,"saved":page.saved,"error":page.error}));
        let draft = self
            .pickers
            .read_with(cx, |pickers, cx| pickers.persisted_draft_execution(cx));
        json!({"state":state,"page":page,"draft":draft})
    }
    fn defaults<C: AppContext>(&self, cx: &C, id: &str) -> ProjectExecutionDefaults {
        self.state.read_with(cx, |state, _| {
            state.space_row(id).unwrap().execution_defaults.clone()
        })
    }
}
// Agreed with Commands: only this page-specific root and two raw mouse events
// are repeated. Scene/service/pumping/attachment helpers are not duplicated.
struct ButtonSurface {
    page: Entity<ProjectsPage>,
    renders: Arc<AtomicUsize>,
}
impl Render for ButtonSurface {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.renders.fetch_add(1, Ordering::SeqCst);
        div().size_full().child(self.page.clone())
    }
}
fn paint(
    cx: &mut TestAppContext,
    runtime: &Runtime,
    scene: &Scene,
) -> (
    gpui::WindowHandle<ButtonSurface>,
    gpui::Point<gpui::Pixels>,
    gpui::Bounds<gpui::Pixels>,
    Arc<AtomicUsize>,
) {
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::default());
    });
    let renders = Arc::new(AtomicUsize::new(0));
    let page = scene.page.clone();
    let count = renders.clone();
    let window = cx.open_window(gpui::size(px(1120.0), px(900.0)), move |_, _| {
        ButtonSurface {
            page,
            renders: count,
        }
    });
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    visual.update(|window, app| window.draw(app).clear());
    // Settle the real child Commands-card read before taking the input frame.
    fixture::pump_until(cx, runtime, |_| {
        scene.a.settled(methods::GET_PROJECT_COMMANDS) > 0
    });
    fixture::drain(cx, runtime);
    visual.update(|window, app| window.draw(app).clear());
    let bounds = visual
        .debug_bounds(BUTTON)
        .expect("actual New worktree choice must be painted");
    (window, bounds.center(), bounds, renders)
}
fn mouse(window: &mut Window, app: &mut gpui::App, point: gpui::Point<gpui::Pixels>) {
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
fn settle(cx: &mut TestAppContext, runtime: &Runtime, scene: &Scene, b: Option<&Arc<HeldRpc>>) {
    scene.a.release(methods::MUTATE);
    if let Some(b) = b {
        b.release(methods::MUTATE);
    }
    fixture::pump_until(cx, runtime, |cx| {
        scene.a.settled(methods::MUTATE) == scene.a.calls(methods::MUTATE)
            && b.is_none_or(|b| b.settled(methods::MUTATE) == b.calls(methods::MUTATE))
            && !scene.page.read_with(cx, |page, _| page.busy)
    });
    fixture::drain(cx, runtime);
}
#[derive(Clone, Copy)]
enum Replacement {
    Project,
    SameDevice,
    DifferentDevice,
}
fn stale(cx: &mut TestAppContext, kind: Replacement, name: &str) {
    let runtime = runtime(cx);
    let _entered = runtime.enter();
    let scene = Scene::new(cx, &runtime);
    let (window, point, bounds, renders) = paint(cx, &runtime, &scene);
    let painted = scene.view(cx);
    let painted_render = renders.load(Ordering::SeqCst);
    let device = if matches!(kind, Replacement::DifferentDevice) {
        "device-b"
    } else {
        "device-a"
    };
    let b = HeldRpc::new("Defaults-B", device);
    b.reply(methods::GET_PROJECT_COMMANDS, Ok(json!([])), false);
    b.reply(methods::MUTATE, Ok(json!({"ok":true})), true);
    let replacement = fixture::handle(&runtime, b.clone());
    let target = if matches!(kind, Replacement::Project) {
        B
    } else {
        A
    };
    let expected = space(target, device).execution_defaults;
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    let retained = visual.debug_bounds(BUTTON).is_some();
    let delivery=visual.update(|window,app| {
        match kind {
            Replacement::Project=>scene.page.update(app,|page,cx|page.action(Action::Project(B.into()),cx)),
            Replacement::SameDevice|Replacement::DifferentDevice=>scene.state.update(app,|state,cx| {
                state.attach_engine_for_epoch_fixture(replacement,cx);
                state.selected_device=Some(device.into());
                state.selected_space=Some(A.into());
                state.apply_spaces(vec![space(A,device),space(B,device)]);
                cx.notify();
            }),
        }
        let before=scene.view(app);let before_render=renders.load(Ordering::SeqCst);
        mouse(window,app,point);
        json!({"before":before,"after":scene.view(app),"renderBefore":before_render,"renderAfter":renders.load(Ordering::SeqCst),"bounds":format!("{bounds:?}"),"point":format!("{point:?}")})
    });
    let after_update_render = renders.load(Ordering::SeqCst);
    fixture::drain(cx, &runtime);
    // Device navigation may legitimately reset the prior draft. Hydrate the
    // deliberate current draft before the held save can acknowledge, then
    // verify the stale input/response cannot change that current draft.
    scene.seed_draft(cx);
    fixture::drain(cx, &runtime);
    let draft_before = scene.view(cx)["draft"].clone();
    settle(cx, &runtime, &scene, Some(&b));
    let after = scene.view(cx);
    let a_saves = saves(&scene.a);
    let b_saves = saves(&b);
    fixture::write_trace(
        name,
        &json!({"painted":painted,"paintedRender":painted_render,"oldBoundsRetained":retained,"delivery":delivery,"afterUpdateRender":after_update_render,"after":after,"draftBeforeAcknowledgement":draft_before,"savesA":a_saves,"savesB":b_saves,"rpcA":scene.a.trace(),"rpcB":b.trace()}),
    );
    assert!(
        retained,
        "old choice must still have painted bounds before delivery"
    );
    assert_eq!(
        delivery["renderBefore"], painted_render,
        "fixture repainted before stale input"
    );
    assert_eq!(
        delivery["renderAfter"], painted_render,
        "fixture repainted during stale input"
    );
    assert_ne!(
        painted["page"]["liveScope"], delivery["before"]["page"]["liveScope"],
        "project or engine scope must actually change"
    );
    assert_eq!(
        a_saves.len() + b_saves.len(),
        0,
        "old rendered Defaults choice sent a save after scope replacement"
    );
    assert_eq!(scene.defaults(cx, target), expected);
    assert_eq!(after["draft"], draft_before);
}
#[gpui::test]
fn defaults_button_admission_project_replacement(cx: &mut TestAppContext) {
    stale(
        cx,
        Replacement::Project,
        "defaults-button-project-replacement",
    );
}
#[gpui::test]
fn defaults_button_admission_same_device_generation(cx: &mut TestAppContext) {
    stale(
        cx,
        Replacement::SameDevice,
        "defaults-button-same-device-generation",
    );
}
#[gpui::test]
fn defaults_button_admission_different_device(cx: &mut TestAppContext) {
    stale(
        cx,
        Replacement::DifferentDevice,
        "defaults-button-different-device",
    );
}

#[gpui::test]
fn defaults_button_admission_fresh_choice_once(cx: &mut TestAppContext) {
    let runtime = runtime(cx);
    let _entered = runtime.enter();
    let scene = Scene::new(cx, &runtime);
    scene
        .page
        .update(cx, |page, cx| page.action(Action::Project(B.into()), cx));
    fixture::drain(cx, &runtime);
    let expected = scene.defaults(cx, B);
    let (window, point, bounds, renders) = paint(cx, &runtime, &scene);
    let before = scene.view(cx);
    let painted_render = renders.load(Ordering::SeqCst);
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    let delivery=visual.update(|window,app| {mouse(window,app,point);json!({"afterInput":scene.view(app),"renderAtInput":renders.load(Ordering::SeqCst),"bounds":format!("{bounds:?}"),"point":format!("{point:?}")})});
    fixture::pump_until(cx, &runtime, |_| scene.a.calls(methods::MUTATE) == 1);
    settle(cx, &runtime, &scene, None);
    let requests = saves(&scene.a);
    let after = scene.view(cx);
    fixture::write_trace(
        "defaults-button-fresh-choice",
        &json!({"before":before,"after":after,"paintedRender":painted_render,"delivery":delivery,"requests":requests,"rpc":scene.a.trace()}),
    );
    assert_eq!(delivery["renderAtInput"], painted_render);
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["params"]["spaceId"], B);
    assert_eq!(requests[0]["params"]["targetDeviceId"], "device-a");
    assert_eq!(
        requests[0]["params"]["expectedDefaults"],
        serde_json::to_value(&expected).unwrap()
    );
    let mut selected = expected;
    selected.checkout = Some(ProjectCheckout::NewWorktree);
    assert_eq!(scene.defaults(cx, B), selected);
    assert_eq!(after["draft"], before["draft"]);
}

#[gpui::test]
fn defaults_button_admission_live_busy_preserves_first_save(cx: &mut TestAppContext) {
    let runtime = runtime(cx);
    let _entered = runtime.enter();
    let scene = Scene::new(cx, &runtime);
    let expected = scene.defaults(cx, A);
    let (window, point, bounds, renders) = paint(cx, &runtime, &scene);
    let painted_render = renders.load(Ordering::SeqCst);
    let draft = scene.view(cx)["draft"].clone();
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    let delivery=visual.update(|window,app| {
        scene.page.update(app,|page,cx|page.action(Action::Permission(Some(PermissionMode::Full)),cx));
        let before=scene.view(app);mouse(window,app,point);
        json!({"before":before,"afterInput":scene.view(app),"renderAtInput":renders.load(Ordering::SeqCst),"bounds":format!("{bounds:?}"),"point":format!("{point:?}")})
    });
    fixture::pump_until(cx, &runtime, |_| scene.a.calls(methods::MUTATE) > 0);
    settle(cx, &runtime, &scene, None);
    let requests = saves(&scene.a);
    let after = scene.view(cx);
    fixture::write_trace(
        "defaults-button-live-busy",
        &json!({"delivery":delivery,"paintedRender":painted_render,"after":after,"requests":requests,"rpc":scene.a.trace()}),
    );
    assert_eq!(delivery["renderAtInput"], painted_render);
    assert_eq!(delivery["before"]["page"]["busy"], true);
    assert_eq!(
        requests.len(),
        1,
        "busy choice must not replace the first save"
    );
    assert_eq!(
        requests[0]["params"]["expectedDefaults"],
        serde_json::to_value(&expected).unwrap()
    );
    assert_eq!(requests[0]["params"]["defaults"]["permissionMode"], "full");
    assert_eq!(requests[0]["params"]["defaults"]["checkout"], "local");
    assert_eq!(after["draft"], draft);
}

#[gpui::test]
fn defaults_button_admission_same_project_watch_keyboard(cx: &mut TestAppContext) {
    let runtime = runtime(cx);
    let _entered = runtime.enter();
    let scene = Scene::new(cx, &runtime);
    let (window, point, _, renders) = paint(cx, &runtime, &scene);
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    // Focus the real New worktree control through its actual mouse path. Its
    // successful save is setup; the measured keyboard action moves to the
    // adjacent Current checkout tab stop and must save exactly once.
    visual.update(|window, app| {
        mouse(window, app, point);
        assert!(
            window.focused(app).is_some(),
            "actual mouse target must obtain focus"
        );
    });
    fixture::pump_until(cx, &runtime, |_| scene.a.calls(methods::MUTATE) == 1);
    settle(cx, &runtime, &scene, None);
    scene.state.update(cx, |state, cx| {
        state
            .spaces
            .iter_mut()
            .find(|space| space.id == A)
            .unwrap()
            .name = Some("Renamed in the same scope".into());
        cx.notify();
    });
    fixture::drain(cx, &runtime);
    visual.update(|window, app| window.draw(app).clear());
    let expected = scene.defaults(cx, A);
    let before = scene.view(cx);
    let calls_before = scene.a.calls(methods::MUTATE);
    let delivery=visual.update(|window,app| {
        let previous=window.focused(app).expect("mouse-focused real control remains focused");
        window.focus_prev(app);
        let moved=!previous.is_focused(window);
        let render_before=renders.load(Ordering::SeqCst);
        window.dispatch_event(gpui::PlatformInput::KeyDown(gpui::KeyDownEvent {keystroke:gpui::Keystroke::parse("enter").unwrap(),is_held:false,prefer_character_input:false}),app);
        window.dispatch_event(gpui::PlatformInput::KeyUp(gpui::KeyUpEvent {keystroke:gpui::Keystroke::parse("enter").unwrap()}),app);
        json!({"focusMovedToPreviousTabStop":moved,"renderBeforeKey":render_before,"renderAfterKey":renders.load(Ordering::SeqCst),"afterInput":scene.view(app),"keyboardPath":"Window::focus_prev + dispatch_event(KeyDown/KeyUp)","dirtyKeyMayRedraw":true})
    });
    fixture::pump_until(cx, &runtime, |_| {
        scene.a.calls(methods::MUTATE) > calls_before
    });
    settle(cx, &runtime, &scene, None);
    let requests = saves(&scene.a);
    let after = scene.view(cx);
    fixture::write_trace(
        "defaults-button-watch-keyboard",
        &json!({"before":before,"after":after,"delivery":delivery,"callsBeforeKeyboard":calls_before,"requests":requests,"rpc":scene.a.trace()}),
    );
    assert_eq!(delivery["focusMovedToPreviousTabStop"], true);
    assert_eq!(
        scene.a.calls(methods::MUTATE),
        calls_before + 1,
        "one keyboard activation must save once"
    );
    assert_eq!(
        requests.last().unwrap()["params"]["expectedDefaults"],
        serde_json::to_value(expected).unwrap()
    );
    assert_eq!(
        requests.last().unwrap()["params"]["defaults"]["checkout"],
        "local"
    );
    assert_eq!(
        scene
            .state
            .read_with(cx, |state, _| state.space_row(A).unwrap().name.clone()),
        Some("Renamed in the same scope".into())
    );
    assert_eq!(after["draft"], before["draft"]);
}
