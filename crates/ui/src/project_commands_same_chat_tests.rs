//! Separate same-chat project/cwd scope cases. Parent a679a411 is unchanged.
use super::*;
use luna_proto::Space;

#[derive(Clone, Copy, Debug)]
enum Transition {
    ProjectMove,
    Materialization,
}
struct ContextData {
    _root: tempfile::TempDir,
    project_root: String,
    nested_root: String,
    old_cwd: String,
    new_cwd: String,
    transition: Transition,
}
impl ContextData {
    fn new(transition: Transition) -> Self {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().canonicalize().unwrap();
        let repo = base.join("repo");
        let nested = repo.join("sub");
        let worktree = base.join("worktrees/materialized");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir_all(&worktree).unwrap();
        let old = if matches!(transition, Transition::ProjectMove) {
            nested.clone()
        } else {
            repo.clone()
        };
        let new = if matches!(transition, Transition::ProjectMove) {
            nested.clone()
        } else {
            worktree
        };
        Self {
            _root: root,
            project_root: repo.to_string_lossy().into(),
            nested_root: nested.to_string_lossy().into(),
            old_cwd: old.to_string_lossy().into(),
            new_cwd: new.to_string_lossy().into(),
            transition,
        }
    }
    fn project(&self, current: bool) -> &'static str {
        if current && matches!(self.transition, Transition::ProjectMove) {
            "root-project"
        } else {
            PROJECT
        }
    }
    fn snapshot(&self, current: bool) -> Value {
        let mut value = status("context");
        value["projectId"] = self.project(current).into();
        value["cwd"] = if current {
            self.new_cwd.clone()
        } else {
            self.old_cwd.clone()
        }
        .into();
        let destination = current && matches!(self.transition, Transition::ProjectMove);
        value["actions"][0]["name"] = if destination {
            "Destination Build"
        } else {
            "Source Build"
        }
        .into();
        value["actions"][0]["command"] = if destination {
            "printf destination-command"
        } else {
            "printf source-command"
        }
        .into();
        value
    }
    fn install(&self, scene: &Scene, cx: &mut TestAppContext) {
        let old_root = if matches!(self.transition, Transition::ProjectMove) {
            &self.nested_root
        } else {
            &self.project_root
        };
        let make_space = |id: &str, path: &str| -> Space {
            serde_json::from_value(json!({"id":id,"deviceId":"device-a","path":path,"gitDetected":true,"createdAt":"2026-09-26T00:00:00Z"})).unwrap()
        };
        let mut spaces = vec![make_space(PROJECT, old_root)];
        if matches!(self.transition, Transition::ProjectMove) {
            spaces.push(make_space("root-project", &self.project_root));
        }
        scene.state.update(cx, |state, cx| {
            state.spaces = spaces;
            state.selected_space = Some(PROJECT.into());
            let chat = &mut state.chats[0];
            chat.space_id = Some(PROJECT.into());
            chat.cwd = Some(self.old_cwd.clone());
            chat.checkout_id = Some("original-checkout".into());
            cx.notify();
        });
    }
    fn apply<C: gpui::AppContext>(&self, scene: &Scene, cx: &mut C) {
        scene.state.update(cx, |state, cx| {
            state.selected_space = Some(self.project(true).into());
            let chat = &mut state.chats[0];
            chat.space_id = Some(self.project(true).into());
            chat.cwd = Some(self.new_cwd.clone());
            if matches!(self.transition, Transition::Materialization) {
                chat.checkout_id = Some("materialized-checkout".into());
            }
            cx.notify();
        });
    }
    fn live<C: gpui::AppContext>(&self, scene: &Scene, cx: &C) -> Value {
        scene.state.read_with(cx,|state,_|json!({"projectId":state.chats[0].space_id,"cwd":state.chats[0].cwd,"checkoutId":state.chats[0].checkout_id,"taskId":state.chats[0].id,"deviceId":state.chats[0].device_id,"engineGeneration":state.engine_generation(),"engineDeviceId":state.engine().map(|engine|engine.engine_info().device_id.clone())}))
    }
}
fn assert_context_transition(before: &Value, after: &Value, transition: Transition) {
    for field in ["engineGeneration", "engineDeviceId", "taskId", "deviceId"] {
        assert_eq!(
            before[field], after[field],
            "live {field} must stay stable in this same-chat case"
        );
    }
    match transition {
        Transition::ProjectMove => {
            assert_ne!(before["projectId"], after["projectId"]);
            assert_eq!(before["cwd"], after["cwd"]);
            assert_eq!(before["checkoutId"], after["checkoutId"]);
        }
        Transition::Materialization => {
            assert_eq!(before["projectId"], after["projectId"]);
            assert_ne!(before["cwd"], after["cwd"]);
            assert_ne!(before["checkoutId"], after["checkoutId"]);
        }
    }
}
fn stale_context_run(cx: &mut TestAppContext, transition: Transition) {
    let runtime = runtime(cx);
    let _entered = runtime.enter();
    let data = ContextData::new(transition);
    let rpc = HeldRpc::new("same-engine", "device-a");
    rpc.reply(
        methods::PROJECT_COMMAND_STATUS,
        Ok(data.snapshot(false)),
        false,
    );
    rpc.reply(
        methods::RUN_PROJECT_COMMAND,
        Ok(run("current-context", "device-a")),
        true,
    );
    let scene = Scene::new(cx, &runtime, rpc.clone(), "device-a");
    data.install(&scene, cx);
    scene.open(cx);
    fixture::pump_until(cx, &runtime, |cx| {
        scene.view(cx)["snapshot"] == data.snapshot(false)
    });
    let (window, point, renders, bounds) = button_window(cx, &scene, "qa-command-run-build");
    let painted = scene.view(cx);
    let before_task = data.live(&scene, cx);
    let painted_render = renders.load(Ordering::SeqCst);
    rpc.reply(
        methods::PROJECT_COMMAND_STATUS,
        Ok(data.snapshot(true)),
        true,
    );
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    let bounds_present = visual.debug_bounds("qa-command-run-build").is_some();
    let delivery=visual.update(|window,app| {
        data.apply(&scene,app);
        let live=data.live(&scene,app);let before=scene.view(app);let before_render=renders.load(Ordering::SeqCst);
        mouse_click_rendered(window,app,point);
        json!({"liveTask":live,"before":before,"after":scene.view(app),"beforeRender":before_render,"afterRender":renders.load(Ordering::SeqCst),"paintedBounds":format!("{bounds:?}"),"mousePoint":format!("{point:?}")})
    });
    fixture::drain(cx, &runtime);
    let dispatched = rpc.calls(methods::RUN_PROJECT_COMMAND);
    fixture::write_trace(
        &format!("commands-same-chat-stale-run-{transition:?}"),
        &json!({"painted":painted,"beforeTask":before_task,"delivery":delivery,"currentBackendSnapshot":data.snapshot(true),"runRequests":dispatched,"rpc":rpc.trace()}),
    );
    rpc.release(methods::RUN_PROJECT_COMMAND);
    rpc.release(methods::PROJECT_COMMAND_STATUS);
    assert!(bounds_present);
    assert_eq!(delivery["beforeRender"], painted_render);
    assert_eq!(delivery["afterRender"], painted_render);
    assert_context_transition(&before_task, &delivery["liveTask"], transition);
    assert_eq!(
        dispatched, 0,
        "old project/checkout action was carried into the task's new authoritative context"
    );
}
fn stale_context_status(cx: &mut TestAppContext, transition: Transition) {
    let runtime = runtime(cx);
    let _entered = runtime.enter();
    let data = ContextData::new(transition);
    let rpc = HeldRpc::new("same-engine", "device-a");
    rpc.reply(
        methods::PROJECT_COMMAND_STATUS,
        Ok(data.snapshot(false)),
        true,
    );
    let scene = Scene::new(cx, &runtime, rpc.clone(), "device-a");
    data.install(&scene, cx);
    scene.open(cx);
    fixture::pump_until(cx, &runtime, |_| {
        rpc.calls(methods::PROJECT_COMMAND_STATUS) == 1
    });
    let before = scene.view(cx);
    let before_task = data.live(&scene, cx);
    data.apply(&scene, cx);
    // Release the captured old reply before replacing the route, without
    // pumping either executor; fresh requests then see only the current data.
    rpc.release(methods::PROJECT_COMMAND_STATUS);
    rpc.reply(
        methods::PROJECT_COMMAND_STATUS,
        Ok(data.snapshot(true)),
        false,
    );
    fixture::pump_until(cx, &runtime, |cx| {
        rpc.settled(methods::PROJECT_COMMAND_STATUS) > 0 && scene.view(cx)["busy"] == false
    });
    let after = scene.view(cx);
    let after_task = data.live(&scene, cx);
    fixture::write_trace(
        &format!("commands-same-chat-stale-status-{transition:?}"),
        &json!({"before":before,"after":after,"beforeTask":before_task,"afterTask":after_task,"currentBackendSnapshot":data.snapshot(true),"rpc":rpc.trace()}),
    );
    assert_context_transition(&before_task, &after_task, transition);
    assert_eq!(
        after["snapshot"],
        data.snapshot(true),
        "old status must not replace current project/checkout data"
    );
}
#[gpui::test]
fn commands_same_chat_stale_run_project_move(cx: &mut TestAppContext) {
    stale_context_run(cx, Transition::ProjectMove);
}
#[gpui::test]
fn commands_same_chat_stale_status_project_move(cx: &mut TestAppContext) {
    stale_context_status(cx, Transition::ProjectMove);
}
#[gpui::test]
fn commands_same_chat_stale_run_materialization(cx: &mut TestAppContext) {
    stale_context_run(cx, Transition::Materialization);
}
#[gpui::test]
fn commands_same_chat_stale_status_materialization(cx: &mut TestAppContext) {
    stale_context_status(cx, Transition::Materialization);
}

#[gpui::test]
fn commands_same_chat_fresh_context_controls(cx: &mut TestAppContext) {
    let runtime = runtime(cx);
    let _entered = runtime.enter();
    let mut results = Vec::new();
    for transition in [Transition::ProjectMove, Transition::Materialization] {
        let data = ContextData::new(transition);
        let rpc = HeldRpc::new("same-engine", "device-a");
        rpc.reply(
            methods::PROJECT_COMMAND_STATUS,
            Ok(data.snapshot(true)),
            false,
        );
        rpc.reply(
            methods::RUN_PROJECT_COMMAND,
            Ok(run("current-context", "device-a")),
            true,
        );
        let scene = Scene::new(cx, &runtime, rpc.clone(), "device-a");
        data.install(&scene, cx);
        data.apply(&scene, cx);
        scene.open(cx);
        fixture::pump_until(cx, &runtime, |cx| {
            scene.view(cx)["snapshot"] == data.snapshot(true)
        });
        let (window, point, _, bounds) = button_window(cx, &scene, "qa-command-run-build");
        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        visual.update(|window, app| mouse_click_rendered(window, app, point));
        fixture::pump_until(cx, &runtime, |_| {
            rpc.calls(methods::RUN_PROJECT_COMMAND) == 1
        });
        results.push(json!({"transition":format!("{transition:?}"),"liveTask":data.live(&scene,cx),"view":scene.view(cx),"bounds":format!("{bounds:?}"),"rpc":rpc.trace()}));
        rpc.release(methods::RUN_PROJECT_COMMAND);
        assert_eq!(rpc.calls(methods::RUN_PROJECT_COMMAND), 1);
    }
    fixture::write_trace("commands-same-chat-fresh-context-controls", &json!(results));
}
#[gpui::test]
fn commands_same_chat_unrelated_metadata_control(cx: &mut TestAppContext) {
    let runtime = runtime(cx);
    let _entered = runtime.enter();
    let data = ContextData::new(Transition::ProjectMove);
    let rpc = HeldRpc::new("same-engine", "device-a");
    rpc.reply(
        methods::PROJECT_COMMAND_STATUS,
        Ok(data.snapshot(false)),
        true,
    );
    rpc.reply(
        methods::RUN_PROJECT_COMMAND,
        Ok(run("same-context", "device-a")),
        true,
    );
    let scene = Scene::new(cx, &runtime, rpc.clone(), "device-a");
    data.install(&scene, cx);
    scene.open(cx);
    fixture::pump_until(cx, &runtime, |_| {
        rpc.calls(methods::PROJECT_COMMAND_STATUS) == 1
    });
    let before = data.live(&scene, cx);
    scene.state.update(cx, |state, cx| {
        state.chats[0].title = Some("Renamed only".into());
        state.spaces[0].name = Some("Project label only".into());
        cx.notify();
    });
    rpc.release(methods::PROJECT_COMMAND_STATUS);
    fixture::pump_until(cx, &runtime, |cx| {
        scene.view(cx)["snapshot"] == data.snapshot(false) && scene.view(cx)["busy"] == false
    });
    let (window, point, _, _) = button_window(cx, &scene, "qa-command-run-build");
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    visual.update(|window, app| mouse_click_rendered(window, app, point));
    fixture::pump_until(cx, &runtime, |_| {
        rpc.calls(methods::RUN_PROJECT_COMMAND) == 1
    });
    fixture::write_trace(
        "commands-same-chat-unrelated-metadata-control",
        &json!({"beforeTask":before,"afterTask":data.live(&scene,cx),"view":scene.view(cx),"rpc":rpc.trace()}),
    );
    rpc.release(methods::RUN_PROJECT_COMMAND);
    assert_eq!(before, data.live(&scene, cx));
    assert_eq!(rpc.calls(methods::RUN_PROJECT_COMMAND), 1);
}
