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
use std::sync::{Arc, Mutex};
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
    fn commands(&self, cx: &TestAppContext) -> Entity<ProjectCommandPanel> {
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
    fn view(&self, cx: &TestAppContext) -> Value {
        let scope=self.state.read_with(cx,|state,_|json!({"generation":state.engine_generation(),"device":state.engine().map(|e|e.engine_info().device_id.clone()),"chat":state.selected_chat}));
        let commands = self.commands(cx);
        commands.read_with(cx,|view,_|json!({"scope":scope,"entity":format!("{:?}",commands.entity_id()),"busy":view.busy,"snapshot":view.snapshot,"error":view.error,"outputs":self.outputs.lock().unwrap().clone()}))
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
