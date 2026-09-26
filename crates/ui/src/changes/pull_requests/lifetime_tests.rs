//! Failure-first GPUI -> memory RPC integration of the actual retained Changes
//! entity. No render is driven: hidden panes must enforce the same provenance.
use super::*;
use crate::engine_epoch_fixture::{self as rpc, HeldRpc};
use gpui::{AppContext as _, TestAppContext};
use luna_proto::{ChangeRequestState, ChangeRequestSummary, GitPrCheck};
use serde_json::{Value, json};
use tokio::runtime::Runtime;

const DEVICE: &str = "pr-ui-device";
const CHAT: &str = "pr-ui-chat";
const CWD: &str = "/synthetic/pr-ui-checkout";
const OP_A: &str = "11111111-1111-4111-8111-111111111111";
const OP_B: &str = "22222222-2222-4222-8222-222222222222";
const TITLE: &str = "User reviewed title";
const BODY: &str = "User authored description\n\nKeep this draft.";

#[derive(Clone, Copy, Debug)]
enum Operation {
    Preview,
    Details,
    Create,
}
impl Operation {
    fn method(self) -> &'static str {
        match self {
            Self::Preview => methods::GET_GITHUB_PR_PREVIEW,
            Self::Details => methods::GET_GITHUB_PR_DETAILS,
            Self::Create => methods::CREATE_GITHUB_DRAFT_PR,
        }
    }
}
#[derive(Clone, Copy, Debug)]
enum Transition {
    Engine,
    ReplacementGap,
    Selection,
    Cwd,
    Close,
    Branch,
    Remote,
    Unchanged,
}
struct Fixture {
    state: Entity<AppState>,
    pane: Entity<Changes>,
    a: Arc<HeldRpc>,
    b: Arc<HeldRpc>,
}

fn git_preview() -> GitActionPreview {
    GitActionPreview {
        checkout_id: "checkout-identity".into(),
        cwd: CWD.into(),
        branch: "feature/pr".into(),
        head: "1".repeat(40),
        remote: Some("origin".into()),
        files: vec![],
        token: "git-token-a".into(),
    }
}
fn preview(engine: &str) -> GitPrDraftPreview {
    GitPrDraftPreview {
        checkout: git_preview(),
        operation_id: if engine == "A" { OP_A } else { OP_B }.into(),
        repository: "github.com/acme/luna".into(),
        upstream: "origin/feature/pr".into(),
        base: "main".into(),
        head: "feature/pr".into(),
        title: format!("Suggested by {engine}"),
    }
}
fn details(engine: &str) -> GitPrDetails {
    GitPrDetails {
        summary: ChangeRequestSummary {
            provider: "github".into(),
            number: 42,
            title: format!("PR owned by {engine}"),
            url: "https://github.com/acme/luna/pull/42".into(),
            state: ChangeRequestState::Open,
            base_ref: "main".into(),
            head_ref: "feature/pr".into(),
        },
        is_draft: true,
        body: format!("Description from {engine}"),
        checks: vec![GitPrCheck {
            name: format!("Check from {engine}"),
            status: "SUCCESS".into(),
            url: None,
        }],
    }
}
fn response(operation: Operation, engine: &str) -> Value {
    match operation {
        Operation::Preview => serde_json::to_value(preview(engine)).unwrap(),
        Operation::Details => serde_json::to_value(details(engine)).unwrap(),
        Operation::Create => serde_json::to_value(GitPrCreateReceipt {
            outcome: GitPrCreateOutcome::Created,
            details: Some(details(engine)),
            message: format!("Created by {engine}"),
        })
        .unwrap(),
    }
}
fn seed(state: &mut AppState) {
    let chat = |id: &str| {
        serde_json::from_value(json!({
        "id":id,"deviceId":DEVICE,"title":id,"archived":false,"cwd":CWD,
        "branch":"feature/pr","checkoutId":"checkout-identity","createdAt":"2026-09-26T00:00:00Z"
    })).unwrap()
    };
    state.chats = vec![chat(CHAT), chat("pr-ui-other-chat")];
    state.selected_chat = Some(CHAT.into());
    state.local_device_id = Some(DEVICE.into());
    state.chats_synced = true;
}
fn routes(service: &HeldRpc, engine: &str) {
    service.reply(
        methods::GET_GIT_ACTION_PREVIEW,
        Ok(serde_json::to_value(git_preview()).unwrap()),
        false,
    );
    service.reply(
        methods::GET_GIT_PR_STATUS,
        Ok(json!({"state":"noMatch"})),
        false,
    );
    service.reply(methods::LIST_GIT_CHECKPOINTS, Ok(json!({"checkoutId":"checkout-identity","cwd":CWD,"checkpoints":[],"restoreAvailable":false,"restoreUnavailableReason":null})), false);
    for operation in [Operation::Preview, Operation::Details, Operation::Create] {
        service.reply(operation.method(), Ok(response(operation, engine)), false);
    }
}
fn fixture(cx: &mut TestAppContext, runtime: &Runtime) -> Fixture {
    cx.update(|cx| {
        gpui_base::init(cx);
        gpui_tokio::init_from_handle(cx, runtime.handle().clone());
        cx.set_global(Theme::dark());
    });
    let a = HeldRpc::new("A", DEVICE);
    let b = HeldRpc::new("B", DEVICE);
    routes(&a, "A");
    routes(&b, "B");
    let state = cx.new(|_| {
        let mut state = AppState::new();
        seed(&mut state);
        state
    });
    rpc::attach(cx, &state, rpc::handle(runtime, a.clone()));
    let pane = cx.new(|cx| Changes::new(state.clone(), cx));
    pane.update(cx, |pane, cx| {
        pane.scope = DiffScope::WorkingTree;
        pane.git_open = true;
        pane.load_git(cx);
    });
    rpc::pump_until(cx, runtime, |cx| {
        pane.read_with(cx, |pane, _| pane.git_preview.is_some() && !pane.git_busy)
    });
    rpc::drain(cx, runtime);
    Fixture { state, pane, a, b }
}
fn edit(f: &Fixture, cx: &mut TestAppContext) {
    f.pane.update(cx, |pane, cx| {
        pane.pr_panel
            .title
            .update(cx, |input, cx| input.set_text(TITLE, cx));
        pane.pr_panel
            .body
            .update(cx, |input, cx| input.set_text(BODY, cx));
    });
}
fn prime_preview(f: &Fixture, cx: &mut TestAppContext, runtime: &Runtime) {
    f.pane.update(cx, |pane, cx| pane.load_pr_panel(None, cx));
    rpc::pump_until(cx, runtime, |cx| {
        f.pane.read_with(cx, |pane, _| {
            pane.pr_panel.preview.is_some() && !pane.pr_panel.busy
        })
    });
    edit(f, cx);
}
fn snapshot(f: &Fixture, cx: &mut TestAppContext) -> Value {
    f.pane.read_with(cx, |pane, cx| {
        json!({
            "engineGeneration":pane.state.read(cx).engine_generation(),
            "selectedChat":pane.state.read(cx).selected_chat,
            "open":pane.pr_panel.open,"busy":pane.pr_panel.busy,
            "requestGeneration":pane.pr_panel.generation,
            "preview":pane.pr_panel.preview,"details":pane.pr_panel.details,
            "notice":pane.pr_panel.notice,"reconcileOnly":pane.pr_panel.reconcile_only,
            "badge":pane.git_pr_status,
            "title":pane.pr_panel.title.read(cx).text(),"body":pane.pr_panel.body.read(cx).text(),
            "base":pane.pr_panel.base.read(cx).text(),"head":pane.pr_panel.head.read(cx).text(),
        })
    })
}
fn transition(f: &Fixture, cx: &mut TestAppContext, runtime: &Runtime, transition: Transition) {
    match transition {
        Transition::Engine => rpc::attach(cx, &f.state, rpc::handle(runtime, f.b.clone())),
        Transition::ReplacementGap => {
            f.state
                .update(cx, |state, cx| state.prepare_runtime_replacement(cx));
            rpc::drain(cx, runtime); // State observation runs; Changes never renders.
            rpc::attach(cx, &f.state, rpc::handle(runtime, f.b.clone()));
            f.state.update(cx, |state, cx| {
                seed(state);
                cx.notify();
            });
        }
        Transition::Selection => f.state.update(cx, |state, cx| {
            state.selected_chat = Some("pr-ui-other-chat".into());
            cx.notify();
        }),
        Transition::Cwd => f.state.update(cx, |state, cx| {
            state.chats.iter_mut().find(|c| c.id == CHAT).unwrap().cwd =
                Some("/synthetic/other-checkout".into());
            cx.notify();
        }),
        Transition::Close => f.pane.update(cx, |pane, cx| pane.close_pr_panel(cx)),
        Transition::Branch | Transition::Remote | Transition::Unchanged => {
            let mut preview = git_preview();
            match transition {
                Transition::Branch => {
                    preview.branch = "feature/new-branch".into();
                    preview.token = "branch-token-b".into();
                }
                Transition::Remote => {
                    preview.remote = Some("different-remote".into());
                    preview.token = "remote-token-b".into();
                }
                _ => {}
            }
            let expected = preview.token.clone();
            f.a.reply(
                methods::GET_GIT_ACTION_PREVIEW,
                Ok(serde_json::to_value(preview).unwrap()),
                false,
            );
            f.pane.update(cx, |pane, cx| pane.load_git(cx));
            rpc::pump_until(cx, runtime, |cx| {
                f.pane.read_with(cx, |pane, _| {
                    !pane.git_busy
                        && pane
                            .git_preview
                            .as_ref()
                            .is_some_and(|p| p.token == expected)
                })
            });
        }
    }
    rpc::drain(cx, runtime);
}
fn late_case(
    cx: &mut TestAppContext,
    name: &str,
    operation: Operation,
    fail: bool,
    change: Transition,
) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let _entered = runtime.enter();
    let f = fixture(cx, &runtime);
    if matches!(operation, Operation::Create) {
        prime_preview(&f, cx, &runtime);
    } else {
        edit(&f, cx);
    }
    let method = operation.method();
    f.a.reply(
        method,
        if fail {
            Err("Delayed failure from A".into())
        } else {
            Ok(response(operation, "A"))
        },
        true,
    );
    let before_calls = f.a.calls(method);
    // Capture before admission/transition: a close can already settle the held
    // request by cancellation while transition() drains the real executors.
    let before_settled = f.a.settled(method);
    let before_trace = f.a.trace();
    let before_replies = before_trace
        .iter()
        .filter(|event| event["method"] == method && event["phase"] == "reply")
        .count();
    let before_canceled = before_trace
        .iter()
        .filter(|event| event["method"] == method && event["phase"] == "canceled")
        .count();
    f.pane.update(cx, |pane, cx| match operation {
        Operation::Preview => pane.load_pr_panel(None, cx),
        Operation::Details => pane.load_pr_panel(Some(42), cx),
        Operation::Create => pane.submit_draft_pr(cx),
    });
    rpc::pump_until(cx, &runtime, |_| f.a.calls(method) == before_calls + 1);
    transition(&f, cx, &runtime, change);
    let before = snapshot(&f, cx);
    // settled counts both reply and cancellation. If transition already
    // canceled a read, this barrier must classify it instead of timing out.
    f.a.release(method);
    rpc::pump_until(cx, &runtime, |_| f.a.settled(method) > before_settled);
    rpc::drain(cx, &runtime);
    let after = snapshot(&f, cx);
    let trace_a = f.a.trace();
    let canceled = trace_a
        .iter()
        .filter(|event| event["method"] == method && event["phase"] == "canceled")
        .count()
        - before_canceled;
    let replies = trace_a
        .iter()
        .filter(|event| event["method"] == method && event["phase"] == "reply")
        .count()
        - before_replies;
    let settled = f.a.settled(method) - before_settled;
    let read_cancellation_allowed =
        !matches!(operation, Operation::Create) && !matches!(change, Transition::Unchanged);
    rpc::write_trace(
        name,
        &json!({"case":name,"operation":format!("{operation:?}"),"transition":format!("{change:?}"),"failureReply":fail,
        "beforeRelease":before,"afterRelease":after,"stateMatchesBefore":before==after,
        "aCalls":f.a.calls(method),"aSettledBefore":before_settled,"aSettled":f.a.settled(method),
        "admittedRequestSettlements":settled,"admittedRequestReplies":replies,"admittedRequestCanceled":canceled,
        "readCancellationAllowed":read_cancellation_allowed,
        "bCreates":f.b.calls(methods::CREATE_GITHUB_DRAFT_PR),"a":trace_a,"b":f.b.trace(),"hostedWrites":0}),
    );
    assert_eq!(
        f.a.calls(method),
        before_calls + 1,
        "A must not be replayed"
    );
    assert_eq!(
        settled, 1,
        "The admitted A request must settle exactly once"
    );
    if matches!(operation, Operation::Create) {
        // Retiring a view must not cancel the accepted write's backend future:
        // that would kill gh and leave its journal outcome uncertain.
        assert_eq!(canceled, 0, "An accepted A CREATE must not be canceled");
        assert_eq!(
            replies, 1,
            "An accepted A CREATE must complete exactly once"
        );
    } else if read_cancellation_allowed {
        assert_eq!(
            replies + canceled,
            1,
            "A retired read may reply or be canceled once"
        );
    } else {
        assert_eq!(
            canceled, 0,
            "An unchanged Git refresh must not cancel its valid read"
        );
        assert_eq!(replies, 1);
    }
    assert_eq!(
        f.b.calls(methods::CREATE_GITHUB_DRAFT_PR),
        0,
        "No create may be replayed through B"
    );
    if matches!(change, Transition::Unchanged) {
        assert!(
            !after["busy"].as_bool().unwrap(),
            "An unchanged refresh must permit the original completion"
        );
        assert!(after["notice"].is_null());
        assert_eq!(after["preview"]["operationId"], OP_A);
    } else {
        assert_eq!(
            after, before,
            "Late A completion changed the replacement/closed PR context"
        );
    }
}
macro_rules! late_test {
    ($name:ident,$op:ident,$fail:expr,$change:ident) => {
        #[gpui::test]
        fn $name(cx: &mut TestAppContext) {
            late_case(
                cx,
                stringify!($name),
                Operation::$op,
                $fail,
                Transition::$change,
            );
        }
    };
}
late_test!(
    pr_ui_lifetime_preview_success_after_engine,
    Preview,
    false,
    Engine
);
late_test!(
    pr_ui_lifetime_preview_error_after_engine,
    Preview,
    true,
    Engine
);
late_test!(
    pr_ui_lifetime_details_checks_success_after_engine,
    Details,
    false,
    Engine
);
late_test!(
    pr_ui_lifetime_details_checks_error_after_engine,
    Details,
    true,
    Engine
);
late_test!(
    pr_ui_lifetime_create_success_after_engine,
    Create,
    false,
    Engine
);
late_test!(
    pr_ui_lifetime_create_error_after_engine,
    Create,
    true,
    Engine
);
late_test!(
    pr_ui_lifetime_preview_after_hidden_replacement_gap,
    Preview,
    false,
    ReplacementGap
);
late_test!(
    pr_ui_lifetime_details_after_hidden_replacement_gap,
    Details,
    false,
    ReplacementGap
);
late_test!(
    pr_ui_lifetime_create_after_hidden_replacement_gap,
    Create,
    false,
    ReplacementGap
);
late_test!(
    pr_ui_lifetime_preview_after_branch_change,
    Preview,
    false,
    Branch
);
late_test!(
    pr_ui_lifetime_details_after_remote_change,
    Details,
    false,
    Remote
);
late_test!(
    pr_ui_lifetime_create_after_branch_change,
    Create,
    false,
    Branch
);
late_test!(
    pr_ui_lifetime_create_after_remote_change,
    Create,
    false,
    Remote
);
late_test!(
    pr_ui_lifetime_preview_after_selection_change,
    Preview,
    false,
    Selection
);
late_test!(
    pr_ui_lifetime_create_after_selection_change,
    Create,
    false,
    Selection
);
late_test!(pr_ui_lifetime_preview_after_cwd_change, Preview, false, Cwd);
late_test!(pr_ui_lifetime_create_after_cwd_change, Create, false, Cwd);
late_test!(pr_ui_lifetime_preview_after_close, Preview, false, Close);
late_test!(pr_ui_lifetime_create_after_close, Create, false, Close);
late_test!(
    pr_ui_lifetime_pending_preview_survives_unchanged_git_refresh,
    Preview,
    false,
    Unchanged
);

#[gpui::test]
fn pr_ui_lifetime_loaded_draft_survives_unchanged_git_refresh(cx: &mut TestAppContext) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let _entered = runtime.enter();
    let f = fixture(cx, &runtime);
    prime_preview(&f, cx, &runtime);
    let before = snapshot(&f, cx);
    transition(&f, cx, &runtime, Transition::Unchanged);
    let after = snapshot(&f, cx);
    rpc::write_trace(
        "pr_ui_lifetime_loaded_draft_survives_unchanged_git_refresh",
        &json!({"before":before,"after":after,"a":f.a.trace(),"hostedWrites":0}),
    );
    assert_eq!(
        before, after,
        "Identical Git refresh must retain title/body and preview authority"
    );
}

#[gpui::test]
fn pr_ui_lifetime_cached_a_preview_cannot_dispatch_through_b(cx: &mut TestAppContext) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let _entered = runtime.enter();
    let f = fixture(cx, &runtime);
    prime_preview(&f, cx, &runtime);
    transition(&f, cx, &runtime, Transition::Engine);
    f.pane.update(cx, |pane, cx| pane.submit_draft_pr(cx));
    rpc::drain(cx, &runtime);
    rpc::write_trace(
        "pr_ui_lifetime_cached_a_preview_cannot_dispatch_through_b",
        &json!({"phase":"before_B_refresh","state":snapshot(&f,cx),"a":f.a.trace(),"b":f.b.trace(),"hostedWrites":0}),
    );
    assert_eq!(
        f.b.calls(methods::CREATE_GITHUB_DRAFT_PR),
        0,
        "Cached A preview must not authorize B dispatch"
    );
    assert_eq!(snapshot(&f, cx)["title"], TITLE);
    assert_eq!(snapshot(&f, cx)["body"], BODY);
    f.pane.update(cx, |pane, cx| pane.load_pr_panel(None, cx));
    rpc::pump_until(cx, &runtime, |cx| {
        f.pane.read_with(cx, |pane, _| {
            pane.pr_panel
                .preview
                .as_ref()
                .is_some_and(|p| p.operation_id == OP_B)
                && !pane.pr_panel.busy
        })
    });
    assert_eq!(
        snapshot(&f, cx)["title"],
        TITLE,
        "Refresh must preserve the user's authored title"
    );
    assert_eq!(
        snapshot(&f, cx)["body"],
        BODY,
        "Refresh must preserve the user's authored body"
    );
    f.pane.update(cx, |pane, cx| pane.submit_draft_pr(cx));
    rpc::pump_until(cx, &runtime, |_| {
        f.b.settled(methods::CREATE_GITHUB_DRAFT_PR) == 1
    });
    rpc::drain(cx, &runtime);
    let request =
        f.b.trace()
            .into_iter()
            .find(|event| {
                event["method"] == methods::CREATE_GITHUB_DRAFT_PR && event["phase"] == "request"
            })
            .unwrap();
    assert_eq!(request["params"]["preview"]["operationId"], OP_B);
    assert_eq!(request["params"]["title"], TITLE);
    assert_eq!(request["params"]["body"], BODY);
    assert_eq!(f.b.calls(methods::CREATE_GITHUB_DRAFT_PR), 1);
    rpc::write_trace(
        "pr_ui_lifetime_cached_a_preview_fresh_b_dispatch",
        &json!({"state":snapshot(&f,cx),"a":f.a.trace(),"b":f.b.trace(),"hostedWrites":0}),
    );
}
