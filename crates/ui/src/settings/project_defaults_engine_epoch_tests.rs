//! Failure-first integration reproduction on unfenced runtime source 3e62cf03.
//! Exercises retained ProjectsPage callbacks over actual memory RPC and the
//! actual AppState engine-attachment path supplied by the shared fixture.
use super::*;
use crate::engine_epoch_fixture::{HeldRpc, attach, drain, handle, pump_until, write_trace};
use crate::pickers::{CheckoutKind, DraftConfig, Pickers, ResolvedRunConfig};
use gpui::{AppContext, TestAppContext};
use serde_json::{Value, json};

#[derive(Clone, Copy)]
enum Completion {
    SaveSuccess,
    SaveError,
    ModelsSuccess,
    ModelsError,
}

fn project(device: &str, model: &str) -> Space {
    serde_json::from_value(json!({
        "id":"epoch-shared-project", "deviceId":device, "path":"/epoch-project",
        "name":"Retained project", "gitDetected":true, "createdAt":"2026-09-26T00:00:00Z",
        "executionDefaults":{
            "provider":{"harness":"codex","model":model},
            "permissionMode":"auto", "checkout":"local"
        }
    }))
    .unwrap()
}

fn snapshot(
    cx: &mut TestAppContext,
    state: &Entity<AppState>,
    page: &Entity<ProjectsPage>,
    pickers: &Entity<Pickers>,
) -> Value {
    let state_value = state.read_with(cx, |state, _| {
        json!({
            "engineGeneration":state.engine_generation(),
            "engineDevice":state.engine().map(|engine|engine.engine_info().device_id.clone()),
            "localDevice":state.local_device_id,
            "selectedDevice":state.selected_device,
            "selectedProject":state.selected_space,
            "selectedChat":state.selected_chat,
            "spaces":state.spaces,
        })
    });
    let page_value=page.read_with(cx, |page,_| {
        let catalog=match &page.models {
            popover::Loadable::Idle=>json!({"state":"idle"}),
            popover::Loadable::Loading=>json!({"state":"loading"}),
            popover::Loadable::Ready(models)=>json!({"state":"ready","models":models}),
            popover::Loadable::Error(error)=>json!({"state":"error","error":error}),
        };
        json!({"project":page.selected_project_id,"busy":page.busy,"saved":page.saved,"error":page.error,"catalog":catalog})
    });
    let draft = pickers.read_with(cx, |pickers, cx| pickers.persisted_draft_execution(cx));
    json!({"state":state_value,"page":page_value,"draft":draft})
}

fn retained_defaults_case(
    cx: &mut TestAppContext,
    name: &str,
    different_device: bool,
    completion: Completion,
) {
    // The shared pump drives real memory RPC on the GPUI test thread; a
    // worker-thread wake violates GPUI's deterministic scheduler contract.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let _entered = runtime.enter();
    cx.update(|cx| gpui_tokio::init_from_handle(cx, runtime.handle().clone()));
    let device_a = "epoch-device-a";
    let device_b = if different_device {
        "epoch-device-b"
    } else {
        device_a
    };
    let rpc_a = HeldRpc::new("defaults-A", device_a);
    let rpc_b = HeldRpc::new("defaults-B", device_b);
    let method = match completion {
        Completion::SaveSuccess | Completion::SaveError => methods::MUTATE,
        Completion::ModelsSuccess | Completion::ModelsError => methods::LIST_MODELS,
    };
    let reply = match completion {
        Completion::SaveSuccess => Ok(json!({"ok":true})),
        Completion::SaveError => Err("late engine A save failure".into()),
        Completion::ModelsSuccess => Ok(
            json!([{"id":"late-engine-A-model","label":"Late A model","reasoningLevels":[],"options":[]}]),
        ),
        Completion::ModelsError => Err("late engine A model failure".into()),
    };
    rpc_a.reply(method, reply, true);
    rpc_b.reply(methods::MUTATE, Ok(json!({"ok":true})), false);
    rpc_b.reply(methods::LIST_MODELS,Ok(json!([{"id":"current-engine-B-model","label":"Current B model","reasoningLevels":[],"options":[]}])),false);
    let state = cx.new(|_| AppState::new());
    attach(cx, &state, handle(&runtime, rpc_a.clone()));
    state.update(cx, |state, cx| {
        state.selected_device = Some(device_a.into());
        state.selected_space = Some("epoch-shared-project".into());
        state.apply_spaces(vec![project(device_a, "initial-engine-A-project-model")]);
        cx.notify();
    });
    drain(cx, &runtime);
    let page = cx.new(|cx| ProjectsPage::new(state.clone(), cx));
    let pickers = cx.new(|cx| Pickers::new(state.clone(), cx));
    let old_generation = state.read_with(cx, |state, _| state.engine_generation());
    page.update(cx, |page, cx| match completion {
        Completion::SaveSuccess | Completion::SaveError => {
            let space = page.selected(cx).unwrap();
            let mut defaults = space.execution_defaults.clone();
            defaults.provider.as_mut().unwrap().model = Some("late-engine-A-save".into());
            page.save(space, defaults, cx);
        }
        Completion::ModelsSuccess | Completion::ModelsError => {
            page.load_models(HarnessId::Codex, cx)
        }
    });
    pump_until(cx, &runtime, |_| rpc_a.calls(method) == 1);
    assert_eq!(
        rpc_a.settled(method),
        0,
        "the old A request must remain held before replacement"
    );

    // Use the real attachment boundary, then supply B's controlled workspace
    // projection. Same project/provider intentionally avoids incidental reset.
    attach(cx, &state, handle(&runtime, rpc_b.clone()));
    let current_b_project = project(device_b, "current-engine-B-project-model");
    state.update(cx, |state, cx| {
        state.selected_device = Some(device_b.into());
        state.selected_space = Some("epoch-shared-project".into());
        state.selected_chat = None;
        state.apply_spaces(vec![current_b_project.clone()]);
        cx.notify();
    });
    drain(cx, &runtime);
    let new_generation = state.read_with(cx, |state, _| state.engine_generation());
    assert!(
        new_generation > old_generation,
        "actual attach must advance engine generation"
    );
    assert_eq!(
        state.read_with(cx, |state, _| state
            .engine()
            .unwrap()
            .engine_info()
            .device_id
            .clone()),
        device_b
    );
    let explicit_b_draft = DraftConfig {
        harness: Some(HarnessId::Codex),
        model: Some("deliberate-engine-B-draft".into()),
        permission_mode: Some(PermissionMode::Full),
        checkout: CheckoutKind::Local,
        branch: Some("engine-b-draft-branch".into()),
        ..Default::default()
    };
    let persisted = json!({"draft":explicit_b_draft,"resolved":ResolvedRunConfig::default()});
    pickers.update(cx, |pickers, cx| {
        pickers.restore_draft_execution(&persisted, cx)
    });
    drain(cx, &runtime);
    let before = snapshot(cx, &state, &page, &pickers);
    rpc_a.release(method);
    // Settlement is observed at the RPC boundary, including cancellation once
    // a future correction retires the old call. No direct callback imitation.
    pump_until(cx, &runtime, |cx| {
        rpc_a.settled(method) == 1
            && page.read_with(cx, |page, _| match completion {
                Completion::SaveSuccess | Completion::SaveError => !page.busy,
                Completion::ModelsSuccess | Completion::ModelsError => {
                    !matches!(page.models, popover::Loadable::Loading)
                }
            })
    });
    drain(cx, &runtime);
    let after = snapshot(cx, &state, &page, &pickers);
    write_trace(
        name,
        &json!({
            "baselineRuntime":"3e62cf03", "differentDevice":different_device,
            "method":method, "oldGeneration":old_generation,"newGeneration":new_generation,
            "before":before,"after":after,"engineA":rpc_a.trace(),"engineB":rpc_b.trace(),
        }),
    );
    assert_eq!(
        after, before,
        "late engine A completion changed current engine B state"
    );
    assert_eq!(
        after["page"]["busy"], false,
        "engine B must not remain busy for retired A work"
    );
    assert_ne!(
        after["page"]["catalog"]["state"], "loading",
        "engine B must not retain A's loading state"
    );
    assert_eq!(
        pickers.read_with(cx, |pickers, _| pickers.draft().clone()),
        explicit_b_draft
    );
    assert_eq!(
        state.read_with(cx, |state, _| state
            .space_row("epoch-shared-project")
            .unwrap()
            .clone()),
        current_b_project
    );
}

#[gpui::test]
fn engine_epoch_defaults_save_success_same_device(cx: &mut TestAppContext) {
    retained_defaults_case(
        cx,
        "defaults-save-success-same-device",
        false,
        Completion::SaveSuccess,
    );
}
#[gpui::test]
fn engine_epoch_defaults_save_error_same_device(cx: &mut TestAppContext) {
    retained_defaults_case(
        cx,
        "defaults-save-error-same-device",
        false,
        Completion::SaveError,
    );
}
#[gpui::test]
fn engine_epoch_defaults_models_success_same_device(cx: &mut TestAppContext) {
    retained_defaults_case(
        cx,
        "defaults-models-success-same-device",
        false,
        Completion::ModelsSuccess,
    );
}
#[gpui::test]
fn engine_epoch_defaults_models_error_same_device(cx: &mut TestAppContext) {
    retained_defaults_case(
        cx,
        "defaults-models-error-same-device",
        false,
        Completion::ModelsError,
    );
}
#[gpui::test]
fn engine_epoch_defaults_save_success_different_device(cx: &mut TestAppContext) {
    retained_defaults_case(
        cx,
        "defaults-save-success-different-device",
        true,
        Completion::SaveSuccess,
    );
}
#[gpui::test]
fn engine_epoch_defaults_save_error_different_device(cx: &mut TestAppContext) {
    retained_defaults_case(
        cx,
        "defaults-save-error-different-device",
        true,
        Completion::SaveError,
    );
}
#[gpui::test]
fn engine_epoch_defaults_models_success_different_device(cx: &mut TestAppContext) {
    retained_defaults_case(
        cx,
        "defaults-models-success-different-device",
        true,
        Completion::ModelsSuccess,
    );
}
#[gpui::test]
fn engine_epoch_defaults_models_error_different_device(cx: &mut TestAppContext) {
    retained_defaults_case(
        cx,
        "defaults-models-error-different-device",
        true,
        Completion::ModelsError,
    );
}
