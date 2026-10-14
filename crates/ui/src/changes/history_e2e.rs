//! History over a real engine and a real repository with no commits yet
//! (issue #9): the pane settles on its empty state instead of re-reading the
//! page on every frame (a permanent "Loading history…"), and the first commit
//! appears once the checkout moves.

use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

use gpui::{AppContext as _, TestAppContext, WindowHandle};
use luna_rpc::{RpcError, RpcReply, RpcService, methods};
use serde_json::{Value, json};

use super::*;

const CHAT: &str = "chat-unborn-history";
const SPACE: &str = "space-unborn-history";
const DEVICE: &str = "local";

/// The real engine, counting the History pages the pane asks for.
struct CountedEngine {
    engine: Arc<dyn RpcService>,
    history_reads: Mutex<usize>,
}

#[async_trait::async_trait]
impl RpcService for CountedEngine {
    async fn handle(&self, method: &str, params: Value) -> Result<RpcReply, RpcError> {
        if method == methods::LIST_GIT_HISTORY {
            *self.history_reads.lock().unwrap() += 1;
        }
        self.engine.handle(method, params).await
    }
}

fn git(root: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@test")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@test")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Drive both executors and paint frames until `done` holds. Painting
/// matters: every frame renders the pane, which asks History to load.
fn pump(
    cx: &mut TestAppContext,
    runtime: &tokio::runtime::Runtime,
    window: WindowHandle<Changes>,
    what: &str,
    mut done: impl FnMut(&Changes, &App) -> bool,
) {
    for _ in 0..1500 {
        frame(cx, runtime, window);
        if window.read_with(cx, |pane, cx| done(pane, cx)).unwrap() {
            return;
        }
    }
    panic!("History fixture never reached: {what}");
}

fn frame(
    cx: &mut TestAppContext,
    runtime: &tokio::runtime::Runtime,
    window: WindowHandle<Changes>,
) {
    cx.run_until_parked();
    runtime.block_on(async { tokio::time::sleep(Duration::from_millis(2)).await });
    cx.update_window(window.into(), |_, window, cx| {
        window.refresh();
        window.draw(cx).clear();
    })
    .unwrap();
}

fn commit_count(pane: &Changes, cx: &App) -> Option<usize> {
    pane.history.as_ref()?.read(cx).commit_count()
}

#[gpui::test]
fn unborn_history_settles_then_shows_the_first_commit(cx: &mut TestAppContext) {
    cx.update(|cx| {
        crate::history::init(
            Default::default(),
            Default::default(),
            Default::default(),
            Default::default(),
            cx,
        );
        gpui_base::init(cx);
        cx.set_global(Theme::dark());
    });
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    // Engine assembly spawns onto this runtime.
    let (temp, root, core, service) = {
        let _entered = runtime.enter();
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap().join("habit tracker");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("index.html"), "<h1>Habits</h1>\n").unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        let data = temp.path().join("engine");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(data.join("device-id"), DEVICE).unwrap();
        let core = luna_engine::EngineCore::assemble(
            &data,
            Arc::new(luna_engine::HarnessRegistry::new()),
            luna_proto::HarnessId::Mock,
            None,
        )
        .unwrap();
        core.workspace
            .create_space(SPACE, DEVICE, &root.to_string_lossy(), None, true)
            .unwrap();
        core.workspace
            .create_chat(CHAT, Some(SPACE), None, None, None)
            .unwrap();
        let service = Arc::new(CountedEngine {
            engine: core.rpc_service(),
            history_reads: Mutex::new(0),
        });
        (temp, root, core, service)
    };
    let reads = || *service.history_reads.lock().unwrap();

    let window = {
        let _entered = runtime.enter();
        let client = luna_rpc::memory_client(service.clone());
        let cwd = root.to_string_lossy().to_string();
        cx.add_window(move |_, cx| {
            let state = cx.new(move |_| {
                let mut state = AppState::new();
                state.set_test_engine(EngineHandle::from_test_client(client));
                state.local_device_id = Some(DEVICE.into());
                state.chats = vec![
                    serde_json::from_value(json!({
                        "id": CHAT, "deviceId": DEVICE, "spaceId": SPACE, "cwd": cwd,
                        "title": "Habit tracker", "archived": false,
                        "createdAt": "2026-10-14T09:40:00Z",
                    }))
                    .unwrap(),
                ];
                state.selected_chat = Some(CHAT.into());
                state
            });
            Changes::for_history(state, cx)
        })
    };
    // The shell's hook for a freshly mounted surface tab.
    window
        .update(cx, |pane, _, cx| pane.ensure_content(cx))
        .unwrap();

    pump(cx, &runtime, window, "the empty first page", |pane, cx| {
        commit_count(pane, cx) == Some(0)
    });
    // Frames and app-state churn (a streaming turn notifies constantly) must
    // not re-ask for a page the engine already answered with no commits.
    for _ in 0..20 {
        window
            .update(cx, |pane, _, cx| pane.state.update(cx, |_, cx| cx.notify()))
            .unwrap();
        frame(cx, &runtime, window);
    }
    assert_eq!(reads(), 1, "History re-read an empty page");

    pump(cx, &runtime, window, "the checkout's diff", |pane, cx| {
        pane.resolved(cx).is_some()
    });
    assert_eq!(reads(), 1, "the first diff revision is not a checkout move");

    // The issue's recovery step: the first commit lands from the terminal.
    git(
        &root,
        &["commit", "-q", "--allow-empty", "-m", "QA baseline"],
    );
    pump(cx, &runtime, window, "the first commit", |pane, cx| {
        commit_count(pane, cx) == Some(1)
    });

    runtime.block_on(core.shutdown());
    let _entered = runtime.enter();
    drop(service);
    drop(core);
    drop(temp);
}
