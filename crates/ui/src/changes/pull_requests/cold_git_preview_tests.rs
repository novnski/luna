//! Separate failure-first follow-up on runtime 81105db5. The original 22 PR
//! cases/helper stay frozen; this exercises the cold Git read owned by Changes.
use super::*;
use crate::engine_epoch_fixture::{self as rpc, HeldRpc};
use gpui::{AppContext as _, TestAppContext};
use serde_json::{Value, json};

const DEVICE: &str = "cold-git-device";
const CHAT: &str = "cold-git-chat";
const CWD: &str = "/synthetic/cold-git-checkout";

fn preview(engine: &str) -> GitActionPreview {
    GitActionPreview {
        checkout_id: "cold-checkout-identity".into(),
        cwd: CWD.into(),
        branch: format!("feature/{engine}"),
        head: if engine == "A" { "a" } else { "b" }.repeat(40),
        remote: Some("origin".into()),
        files: vec![],
        token: format!("git-token-{engine}"),
    }
}
fn snapshot(pane: &Entity<Changes>, cx: &mut TestAppContext) -> Value {
    pane.read_with(cx, |pane, cx| {
        json!({
            "engineGeneration":pane.state.read(cx).engine_generation(),
            "gitBusy":pane.git_busy,"gitPreview":pane.git_preview,
            "gitNotice":pane.git_notice,"gitTarget":pane.git_target,
            "currentGitKey":pane.git_context(cx).map(|(_,_,key)|key),
            "gitTaskRetained":pane.git_task.is_some(),"gitOpen":pane.git_open,
        })
    })
}
fn run_case(cx: &mut TestAppContext, name: &str, error: bool, manual_refresh: bool) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let _entered = runtime.enter();
    cx.update(|cx| {
        gpui_base::init(cx);
        gpui_tokio::init_from_handle(cx, runtime.handle().clone());
        cx.set_global(Theme::dark());
    });
    let a = HeldRpc::new("cold-A", DEVICE);
    let b = HeldRpc::new("cold-B", DEVICE);
    let method = methods::GET_GIT_ACTION_PREVIEW;
    a.reply(
        method,
        if error {
            Err("Held cold A preview error".into())
        } else {
            Ok(serde_json::to_value(preview("A")).unwrap())
        },
        true,
    );
    b.reply(
        method,
        Ok(serde_json::to_value(preview("B")).unwrap()),
        false,
    );
    b.reply(
        methods::GET_GIT_PR_STATUS,
        Ok(json!({"state":"noMatch"})),
        false,
    );
    b.reply(
        methods::LIST_GIT_CHECKPOINTS,
        Ok(json!({
            "checkoutId":"cold-checkout-identity","cwd":CWD,"checkpoints":[],
            "restoreAvailable":false,"restoreUnavailableReason":null
        })),
        false,
    );
    let state = cx.new(|_| {
        let mut state = AppState::new();
        state.chats = vec![
            serde_json::from_value(json!({
                "id":CHAT,"deviceId":DEVICE,"title":"Cold preview","archived":false,
                "cwd":CWD,"branch":"feature/A","checkoutId":"cold-checkout-identity",
                "createdAt":"2026-09-26T00:00:00Z"
            }))
            .unwrap(),
        ];
        state.selected_chat = Some(CHAT.into());
        state.local_device_id = Some(DEVICE.into());
        state.chats_synced = true;
        state
    });
    rpc::attach(cx, &state, rpc::handle(&runtime, a.clone()));
    let pane = cx.new(|cx| Changes::new(state.clone(), cx));
    let before_settled = a.settled(method);
    pane.update(cx, |pane, cx| {
        pane.scope = DiffScope::WorkingTree;
        pane.git_open = true;
        pane.load_git(cx);
    });
    rpc::pump_until(cx, &runtime, |_| a.calls(method) == 1);
    let before_replacement = snapshot(&pane, cx);
    assert!(
        before_replacement["gitPreview"].is_null(),
        "Fixture must start cold"
    );
    assert_eq!(before_replacement["gitBusy"], true);
    // The real observer runs, but there is no render frame or changed chat/cwd.
    rpc::attach(cx, &state, rpc::handle(&runtime, b.clone()));
    rpc::drain(cx, &runtime);
    if manual_refresh {
        pane.update(cx, |pane, cx| pane.load_git(cx));
        rpc::pump_until(cx, &runtime, |cx| {
            pane.read_with(cx, |pane, _| {
                !pane.git_busy
                    && pane
                        .git_preview
                        .as_ref()
                        .is_some_and(|p| p.token == "git-token-B")
            })
        });
        rpc::drain(cx, &runtime);
    }
    let before_a_settlement = snapshot(&pane, cx);
    a.release(method);
    // A read may already have been canceled by retirement/manual reload.
    rpc::pump_until(cx, &runtime, |_| a.settled(method) > before_settled);
    rpc::drain(cx, &runtime);
    let after = snapshot(&pane, cx);
    let trace_a = a.trace();
    let replies = trace_a
        .iter()
        .filter(|e| e["method"] == method && e["phase"] == "reply")
        .count();
    let canceled = trace_a
        .iter()
        .filter(|e| e["method"] == method && e["phase"] == "canceled")
        .count();
    rpc::write_trace(
        name,
        &json!({
            "case":name,"runtimeSource":"81105db5e78297a5d9343c731de997fef3a1636b",
            "aError":error,"manualRefresh":manual_refresh,
            "beforeReplacement":before_replacement,"beforeASettlement":before_a_settlement,
            "afterASettlement":after,"aPreviewCalls":a.calls(method),"bPreviewCalls":b.calls(method),
            "aReplies":replies,"aCanceled":canceled,"a":trace_a,"b":b.trace(),"hostedWrites":0,
        }),
    );
    assert_eq!(a.calls(method), 1, "Retired A preview must not be replayed");
    assert_eq!(replies + canceled, 1, "Admitted A read must settle once");
    for service in [&a, &b] {
        for write in [
            methods::EXECUTE_GIT_ACTION,
            methods::CREATE_GITHUB_DRAFT_PR,
            methods::RESTORE_GIT_CHECKPOINT,
        ] {
            assert_eq!(
                service.calls(write),
                0,
                "Cold preview recovery must be read-only"
            );
        }
    }
    assert_eq!(
        after["gitTarget"], after["currentGitKey"],
        "Same-key replacement is the required interleaving"
    );
    assert_eq!(
        after["gitBusy"], false,
        "Retired cold A read left Git loading owned by the old engine"
    );
    if !after["gitPreview"].is_null() {
        assert_eq!(
            after["gitPreview"]["token"], "git-token-B",
            "An available preview must come from B"
        );
    }
    if manual_refresh {
        assert_eq!(b.calls(method), 1);
        assert_eq!(
            after, before_a_settlement,
            "Late A must not overwrite the explicit B recovery"
        );
    }
}

#[gpui::test]
fn pr_ui_cold_git_preview_success_after_engine_replacement(cx: &mut TestAppContext) {
    run_case(
        cx,
        "pr_ui_cold_git_preview_success_after_engine_replacement",
        false,
        false,
    );
}
#[gpui::test]
fn pr_ui_cold_git_preview_error_after_engine_replacement(cx: &mut TestAppContext) {
    run_case(
        cx,
        "pr_ui_cold_git_preview_error_after_engine_replacement",
        true,
        false,
    );
}
#[gpui::test]
fn pr_ui_cold_git_preview_manual_b_refresh_control(cx: &mut TestAppContext) {
    run_case(
        cx,
        "pr_ui_cold_git_preview_manual_b_refresh_control",
        false,
        true,
    );
}
