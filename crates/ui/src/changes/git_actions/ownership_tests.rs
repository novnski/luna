//! Additional same-context write ownership cases, authored before the coherent
//! read/write ownership correction. Frozen lifetime cases/helper are unchanged.
use super::*;
use crate::engine_epoch_fixture::{self as rpc, HeldRpc};
use gpui::{AppContext as _, TestAppContext};
use serde_json::{Value, json};

const CWD: &str = "/synthetic/git-ownership";
const DRAFT: &str = "Original commit draft";
const EDITED: &str = "Newer authored draft";
#[derive(Clone, Copy, Debug)]
enum Write {
    Commit,
    Restore,
}
impl Write {
    fn method(self) -> &'static str {
        match self {
            Self::Commit => methods::EXECUTE_GIT_ACTION,
            Self::Restore => methods::RESTORE_GIT_CHECKPOINT,
        }
    }
}
#[derive(Clone, Copy, Debug)]
enum During {
    Refresh,
    CloseReopen,
    Edit,
}
fn preview(after: bool) -> GitActionPreview {
    GitActionPreview {
        checkout_id: "owned-checkout".into(),
        cwd: CWD.into(),
        branch: "feature/owned".into(),
        head: if after {
            "b".repeat(40)
        } else {
            "a".repeat(40)
        },
        remote: Some("origin".into()),
        files: vec![luna_proto::GitFileState {
            path: "note.rs".into(),
            staged: true,
            unstaged: false,
        }],
        token: if after { "after" } else { "before" }.into(),
    }
}
fn snapshot(pane: &Entity<Changes>, cx: &mut TestAppContext) -> Value {
    pane.read_with(cx,|pane,cx|json!({"busy":pane.git_busy,"preview":pane.git_preview,"draft":pane.git_message.read(cx).text(),"notice":pane.git_notice,"restoreReceipt":pane.checkpoint_receipt,"restoreNotice":pane.checkpoint_notice,"open":pane.git_open}))
}
fn run(cx: &mut TestAppContext, name: &str, write: Write, during: During) {
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
    let service = HeldRpc::new("ownership-A", "device");
    service.reply(
        methods::GET_GIT_ACTION_PREVIEW,
        Ok(serde_json::to_value(preview(false)).unwrap()),
        false,
    );
    service.reply(
        methods::GET_GIT_PR_STATUS,
        Ok(json!({"state":"noMatch"})),
        false,
    );
    service.reply(methods::LIST_GIT_CHECKPOINTS,Ok(json!({"checkoutId":"owned-checkout","cwd":CWD,"checkpoints":[],"restoreAvailable":true,"restoreUnavailableReason":null})),false);
    service.reply(methods::PREVIEW_GIT_CHECKPOINT_RESTORE,Ok(json!({"checkoutId":"owned-checkout","cwd":CWD,"turnId":"turn","point":"before","paths":[],"token":"restore-before"})),false);
    let state=cx.new(|_|{let mut state=AppState::new();state.chats=vec![serde_json::from_value(json!({"id":"chat","deviceId":"device","title":"Ownership","archived":false,"cwd":CWD,"branch":"feature/owned","checkoutId":"owned-checkout","createdAt":"2026-09-26T00:00:00Z"})).unwrap()];state.selected_chat=Some("chat".into());state.local_device_id=Some("device".into());state.chats_synced=true;state});
    rpc::attach(cx, &state, rpc::handle(&runtime, service.clone()));
    let pane = cx.new(|cx| Changes::new(state, cx));
    pane.update(cx, |pane, cx| {
        pane.scope = DiffScope::WorkingTree;
        pane.git_open = true;
        pane.load_git(cx);
    });
    rpc::pump_until(cx, &runtime, |cx| {
        pane.read_with(cx, |p, _| p.git_preview.is_some() && !p.git_busy)
    });
    rpc::drain(cx, &runtime);
    pane.update(cx, |pane, cx| {
        pane.git_message
            .update(cx, |input, cx| input.set_text(DRAFT, cx))
    });
    if matches!(write, Write::Restore) {
        pane.update(cx, |pane, cx| {
            pane.preview_checkpoint_restore("turn".into(), cx)
        });
        rpc::pump_until(cx, &runtime, |cx| {
            pane.read_with(cx, |p, _| p.checkpoint_restore.is_some())
        });
        rpc::drain(cx, &runtime);
    }
    let result = match write {
        Write::Commit => serde_json::to_value(GitActionReceipt {
            commit_sha: Some("b".repeat(40)),
            committed: true,
            pushed: false,
            push_uncertain: false,
            error: None,
            preview: preview(true),
        })
        .unwrap(),
        Write::Restore => {
            json!({"restored":true,"recoveryRef":"refs/luna/recovery/owned","paths":[]})
        }
    };
    let method = write.method();
    service.reply(method, Ok(result), true);
    let settled_before = service.settled(method);
    pane.update(cx, |pane, cx| match write {
        Write::Commit => pane.git_action(GitActionKind::Commit, vec!["note.rs".into()], cx),
        Write::Restore => pane.restore_checkpoint(cx),
    });
    rpc::pump_until(cx, &runtime, |_| service.calls(method) == 1);
    let reads_before = service.calls(methods::GET_GIT_ACTION_PREVIEW);
    pane.update(cx, |pane, cx| match during {
        During::Refresh => pane.load_git(cx),
        During::CloseReopen => {
            pane.git_open = false;
            cx.notify();
            pane.git_open = true;
            pane.load_git(cx);
        }
        During::Edit => pane
            .git_message
            .update(cx, |input, cx| input.set_text(EDITED, cx)),
    });
    rpc::drain(cx, &runtime);
    let held = snapshot(&pane, cx);
    let reads_held = service.calls(methods::GET_GIT_ACTION_PREVIEW);
    service.reply(
        methods::GET_GIT_ACTION_PREVIEW,
        Ok(serde_json::to_value(preview(true)).unwrap()),
        false,
    );
    service.release(method);
    rpc::pump_until(cx, &runtime, |_| service.settled(method) > settled_before);
    rpc::drain(cx, &runtime);
    let after = snapshot(&pane, cx);
    let trace = service.trace();
    let replies = trace
        .iter()
        .filter(|e| e["method"] == method && e["phase"] == "reply")
        .count();
    let canceled = trace
        .iter()
        .filter(|e| e["method"] == method && e["phase"] == "canceled")
        .count();
    rpc::write_trace(
        name,
        &json!({"case":name,"write":format!("{write:?}"),"during":format!("{during:?}"),"held":held,"after":after,"previewReadsBefore":reads_before,"previewReadsWhileHeld":reads_held,"writeCalls":service.calls(method),"writeReplies":replies,"writeCanceled":canceled,"trace":trace,"hostedWrites":0}),
    );
    assert_eq!(service.calls(method), 1);
    assert_eq!(replies, 1);
    assert_eq!(
        canceled, 0,
        "Accepted writes retain their operation lifetime"
    );
    assert_eq!(
        held["busy"], true,
        "Refresh/close must not clear accepted-write busy ownership"
    );
    assert_eq!(
        reads_held, reads_before,
        "Refresh waits for the write's authoritative completion"
    );
    assert_eq!(after["busy"], false);
    assert_eq!(after["preview"]["token"], "after");
    match write {
        Write::Commit => assert_eq!(
            after["draft"],
            if matches!(during, During::Edit) {
                EDITED
            } else {
                ""
            }
        ),
        Write::Restore => assert!(
            after["restoreReceipt"]
                .as_str()
                .unwrap()
                .contains("refs/luna/recovery/owned")
        ),
    }
}
macro_rules! ownership_case {
    ($name:ident,$write:ident,$during:ident) => {
        #[gpui::test]
        fn $name(cx: &mut TestAppContext) {
            run(cx, stringify!($name), Write::$write, During::$during);
        }
    };
}
ownership_case!(git_ui_ownership_commit_refresh, Commit, Refresh);
ownership_case!(git_ui_ownership_commit_close_reopen, Commit, CloseReopen);
ownership_case!(git_ui_ownership_restore_refresh, Restore, Refresh);
ownership_case!(git_ui_ownership_restore_close_reopen, Restore, CloseReopen);
ownership_case!(git_ui_ownership_commit_preserves_newer_draft, Commit, Edit);
