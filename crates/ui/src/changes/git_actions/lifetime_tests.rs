//! Prepared before adjacent Git UI runtime corrections. Uses real retained
//! Changes handlers with memory RPC only: no Git, provider, or hosted writes.
use super::*;
use crate::engine_epoch_fixture::{self as rpc, HeldRpc};
use gpui::{AppContext as _, TestAppContext};
use serde_json::{Value, json};
use tokio::runtime::Runtime;

const DEVICE: &str = "git-write-ui-device";
const CHAT: &str = "git-write-ui-chat";
const CWD_A: &str = "/synthetic/git-write-A";
const CWD_B: &str = "/synthetic/git-write-B";
const DRAFT: &str = "User commit-message draft";
const EDITED: &str = "A newer user edit";
const READS: [&str; 3] = [
    methods::GET_GIT_ACTION_PREVIEW,
    methods::GET_GIT_PR_STATUS,
    methods::LIST_GIT_CHECKPOINTS,
];
const WRITES: [&str; 3] = [
    methods::EXECUTE_GIT_ACTION,
    methods::RESTORE_GIT_CHECKPOINT,
    methods::CREATE_GITHUB_DRAFT_PR,
];

#[derive(Clone, Copy, Debug)]
enum Operation {
    Commit,
    Restore,
    Message,
}
impl Operation {
    fn method(self) -> &'static str {
        match self {
            Self::Commit => methods::EXECUTE_GIT_ACTION,
            Self::Restore => methods::RESTORE_GIT_CHECKPOINT,
            Self::Message => methods::GENERATE_GIT_COMMIT_MESSAGE,
        }
    }
    fn is_write(self) -> bool {
        !matches!(self, Self::Message)
    }
}
#[derive(Clone, Copy, Debug)]
enum Transition {
    Engine,
    Checkout,
    Current,
    UserEdit,
}
struct Fixture {
    state: Entity<AppState>,
    pane: Entity<Changes>,
    a: Arc<HeldRpc>,
    b: Arc<HeldRpc>,
}
fn preview(label: &str, cwd: &str) -> GitActionPreview {
    GitActionPreview {
        checkout_id: if cwd == CWD_A {
            "checkout-A"
        } else {
            "checkout-B"
        }
        .into(),
        cwd: cwd.into(),
        branch: format!("feature/{label}"),
        head: "a".repeat(40),
        remote: Some("origin".into()),
        token: format!("git-{label}"),
        files: vec![luna_proto::GitFileState {
            path: "note.rs".into(),
            staged: true,
            unstaged: false,
        }],
    }
}
fn routes(service: &HeldRpc, label: &str, cwd: &str) {
    service.reply(
        methods::GET_GIT_ACTION_PREVIEW,
        Ok(serde_json::to_value(preview(label, cwd)).unwrap()),
        false,
    );
    service.reply(
        methods::GET_GIT_PR_STATUS,
        Ok(json!({"state":"noMatch"})),
        false,
    );
    service.reply(methods::LIST_GIT_CHECKPOINTS,Ok(json!({"checkoutId":"checkout-A","cwd":cwd,"checkpoints":[],"restoreAvailable":true,"restoreUnavailableReason":null})),false);
    service.reply(methods::PREVIEW_GIT_CHECKPOINT_RESTORE,Ok(json!({"checkoutId":"checkout-A","cwd":cwd,"turnId":"turn-A","point":"before","paths":["note.rs"],"token":"restore-A"})),false);
}
fn response(operation: Operation) -> Value {
    match operation {
        Operation::Commit => serde_json::to_value(GitActionReceipt {
            commit_sha: Some("a".repeat(40)),
            committed: true,
            pushed: false,
            push_uncertain: false,
            error: None,
            preview: preview("A-after-commit", CWD_A),
        })
        .unwrap(),
        Operation::Restore => {
            json!({"restored":true,"recoveryRef":"refs/luna/recovery/A","paths":["note.rs"]})
        }
        Operation::Message => json!({"message":"Suggested by A"}),
    }
}
fn fixture(cx: &mut TestAppContext, runtime: &Runtime) -> Fixture {
    cx.update(|cx| {
        gpui_base::init(cx);
        gpui_tokio::init_from_handle(cx, runtime.handle().clone());
        cx.set_global(Theme::dark());
    });
    let a = HeldRpc::new("git-A", DEVICE);
    let b = HeldRpc::new("git-B", DEVICE);
    routes(&a, "A", CWD_A);
    routes(&b, "B", CWD_A);
    let state=cx.new(|_| {
        let mut state=AppState::new();
        state.chats=vec![serde_json::from_value(json!({"id":CHAT,"deviceId":DEVICE,"title":"Git UI lifetime","archived":false,"cwd":CWD_A,"branch":"feature/A","checkoutId":"checkout-A","createdAt":"2026-09-26T00:00:00Z"})).unwrap()];
        state.selected_chat=Some(CHAT.into());state.local_device_id=Some(DEVICE.into());state.chats_synced=true;state
    });
    rpc::attach(cx, &state, rpc::handle(runtime, a.clone()));
    let pane = cx.new(|cx| Changes::new(state.clone(), cx));
    pane.update(cx, |pane, cx| {
        pane.scope = DiffScope::WorkingTree;
        pane.git_open = true;
        pane.load_git(cx);
    });
    rpc::pump_until(cx, runtime, |cx| {
        pane.read_with(cx, |pane, _| !pane.git_busy && pane.git_preview.is_some())
    });
    rpc::drain(cx, runtime);
    pane.update(cx, |pane, cx| {
        pane.git_message
            .update(cx, |input, cx| input.set_text(DRAFT, cx))
    });
    Fixture { state, pane, a, b }
}
fn snapshot(f: &Fixture, cx: &mut TestAppContext) -> Value {
    f.pane.read_with(cx,|pane,cx| {
        let mut selected:Vec<_>=pane.git_selected.iter().cloned().collect();selected.sort();
        json!({"engineGeneration":pane.state.read(cx).engine_generation(),
            "currentContext":pane.git_context(cx),"gitTarget":pane.git_target,"gitBusy":pane.git_busy,
            "gitPreview":pane.git_preview,"gitNotice":pane.git_notice,"gitSelected":selected,
            "commitDraft":pane.git_message.read(cx).text(),"prBadge":pane.git_pr_status,
            "checkpointPreview":pane.checkpoint_restore,"checkpointReceipt":pane.checkpoint_receipt,
            "checkpointNotice":pane.checkpoint_notice})
    })
}
fn read_counts(service: &HeldRpc) -> Value {
    let mut counts = serde_json::Map::new();
    for method in READS {
        counts.insert(method.into(), json!(service.calls(method)));
    }
    Value::Object(counts)
}
fn run_case(
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
    if matches!(operation, Operation::Restore) {
        f.pane.update(cx, |pane, cx| {
            pane.preview_checkpoint_restore("turn-A".into(), cx)
        });
        rpc::pump_until(cx, &runtime, |cx| {
            f.pane
                .read_with(cx, |pane, _| pane.checkpoint_restore.is_some())
        });
        rpc::drain(cx, &runtime);
    }
    let method = operation.method();
    f.a.reply(
        method,
        if fail {
            Err("Delayed Git UI error from A".into())
        } else {
            Ok(response(operation))
        },
        true,
    );
    let before_calls = f.a.calls(method);
    let before_settled = f.a.settled(method);
    let trace_start = f.a.trace().len();
    f.pane.update(cx, |pane, cx| match operation {
        Operation::Commit => pane.git_action(GitActionKind::Commit, vec!["note.rs".into()], cx),
        Operation::Restore => pane.restore_checkpoint(cx),
        Operation::Message => pane.generate_git_message(cx),
    });
    rpc::pump_until(cx, &runtime, |_| f.a.calls(method) == before_calls + 1);
    match change {
        Transition::Engine => rpc::attach(cx, &f.state, rpc::handle(&runtime, f.b.clone())),
        Transition::Checkout => {
            // Current-engine follow-up reads must model the new checkout,
            // without altering the already-held original A mutation response.
            routes(&f.a, "B", CWD_B);
            f.state.update(cx, |state, cx| {
                let chat = state.chats.iter_mut().find(|c| c.id == CHAT).unwrap();
                chat.cwd = Some(CWD_B.into());
                chat.checkout_id = Some("checkout-B".into());
                chat.branch = Some("feature/B".into());
                cx.notify();
            });
        }
        Transition::UserEdit => f.pane.update(cx, |pane, cx| {
            pane.git_message
                .update(cx, |input, cx| input.set_text(EDITED, cx))
        }),
        Transition::Current => {}
    }
    rpc::drain(cx, &runtime);
    let before = snapshot(&f, cx);
    let reads_a_before = read_counts(&f.a);
    let reads_b_before = read_counts(&f.b);
    f.a.release(method);
    rpc::pump_until(cx, &runtime, |_| f.a.settled(method) > before_settled);
    rpc::drain(cx, &runtime);
    let after = snapshot(&f, cx);
    let trace_a = f.a.trace();
    let admitted: Vec<_> = trace_a
        .iter()
        .skip(trace_start)
        .filter(|event| event["method"] == method)
        .collect();
    let replied = admitted.iter().filter(|e| e["phase"] == "reply").count();
    let canceled = admitted.iter().filter(|e| e["phase"] == "canceled").count();
    let reads_a_after = read_counts(&f.a);
    let reads_b_after = read_counts(&f.b);
    rpc::write_trace(
        name,
        &json!({"case":name,"preparedAgainst":"9089e97d3b42129ac078928702ff04fdf571064b",
        "operation":format!("{operation:?}"),"transition":format!("{change:?}"),"errorReply":fail,
        "beforeRelease":before,"afterRelease":after,"stateMatchesBefore":before==after,
        "aReplies":replied,"aCanceled":canceled,"readsABefore":reads_a_before,"readsAAfter":reads_a_after,
        "readsBBefore":reads_b_before,"readsBAfter":reads_b_after,"a":trace_a,"b":f.b.trace(),"hostedWrites":0}),
    );
    assert_eq!(
        f.a.calls(method),
        before_calls + 1,
        "No replay of the admitted A request"
    );
    assert_eq!(
        f.a.settled(method) - before_settled,
        1,
        "Admitted request must settle exactly once"
    );
    if operation.is_write() {
        assert_eq!(
            canceled, 0,
            "Accepted Git/restore write must not be canceled on UI retirement"
        );
        assert_eq!(replied, 1, "Accepted write must complete once");
    } else {
        assert_eq!(replied + canceled, 1, "A read may reply or cancel once");
    }
    for write in WRITES {
        assert_eq!(f.b.calls(write), 0, "No write may be replayed through B");
    }
    match change {
        Transition::Engine | Transition::Checkout => {
            assert_eq!(
                after, before,
                "A completion mutated the replacement Git/restore/draft view"
            );
            assert_eq!(
                reads_a_after, reads_a_before,
                "Retired completion started current-checkout follow-up reads on A"
            );
            assert_eq!(
                reads_b_after, reads_b_before,
                "Retired completion started follow-up reads through B"
            );
        }
        Transition::Current => match operation {
            Operation::Commit => {
                assert_eq!(after["gitBusy"], false);
                assert_eq!(after["commitDraft"], "");
                assert_eq!(after["gitPreview"]["token"], "git-A-after-commit");
            }
            Operation::Restore => assert!(
                after["checkpointReceipt"]
                    .as_str()
                    .unwrap()
                    .contains("refs/luna/recovery/A")
            ),
            Operation::Message => {
                assert_eq!(after["gitBusy"], false);
                assert_eq!(after["commitDraft"], "Suggested by A");
            }
        },
        Transition::UserEdit => {
            assert_eq!(after["gitBusy"], false);
            assert_eq!(after["commitDraft"], EDITED);
        }
    }
}
macro_rules! case {
    ($name:ident,$op:ident,$fail:expr,$change:ident) => {
        #[gpui::test]
        fn $name(cx: &mut TestAppContext) {
            run_case(
                cx,
                stringify!($name),
                Operation::$op,
                $fail,
                Transition::$change,
            );
        }
    };
}
case!(
    git_ui_lifetime_commit_success_after_engine,
    Commit,
    false,
    Engine
);
case!(
    git_ui_lifetime_commit_error_after_engine,
    Commit,
    true,
    Engine
);
case!(
    git_ui_lifetime_commit_success_after_checkout,
    Commit,
    false,
    Checkout
);
case!(
    git_ui_lifetime_restore_success_after_engine,
    Restore,
    false,
    Engine
);
case!(
    git_ui_lifetime_restore_error_after_engine,
    Restore,
    true,
    Engine
);
case!(
    git_ui_lifetime_restore_success_after_checkout,
    Restore,
    false,
    Checkout
);
case!(
    git_ui_lifetime_message_success_after_engine,
    Message,
    false,
    Engine
);
case!(
    git_ui_lifetime_message_error_after_engine,
    Message,
    true,
    Engine
);
case!(
    git_ui_lifetime_message_success_after_checkout,
    Message,
    false,
    Checkout
);
case!(
    git_ui_lifetime_commit_current_context_control,
    Commit,
    false,
    Current
);
case!(
    git_ui_lifetime_restore_current_context_control,
    Restore,
    false,
    Current
);
case!(
    git_ui_lifetime_message_current_context_control,
    Message,
    false,
    Current
);
case!(
    git_ui_lifetime_message_newer_edit_control,
    Message,
    false,
    UserEdit
);
