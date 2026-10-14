//! History over a real engine and a real repository with no commits yet
//! (issue #9): the pane settles on its empty state instead of re-reading the
//! page on every frame (a permanent "Loading history…"), and the first commit
//! appears once the checkout moves, even while a page read is on its way.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use gpui::{AppContext as _, TestAppContext, WindowHandle};
use luna_rpc::{RpcError, RpcReply, RpcService, methods};
use serde_json::{Value, json};
use tokio::sync::watch;

use super::*;

const CHAT: &str = "chat-unborn-history";
const SPACE: &str = "space-unborn-history";
const DEVICE: &str = "local";

/// The real engine, counting the History pages the pane asks for.
struct CountedEngine {
    engine: Arc<dyn RpcService>,
    history_reads: Mutex<usize>,
    /// Pages the engine has finished reading from the repository.
    history_answers: Mutex<usize>,
    /// While true, a read page waits here on its way back to the pane.
    hold_pages: watch::Sender<bool>,
}

#[async_trait::async_trait]
impl RpcService for CountedEngine {
    async fn handle(&self, method: &str, params: Value) -> Result<RpcReply, RpcError> {
        if method != methods::LIST_GIT_HISTORY {
            return self.engine.handle(method, params).await;
        }
        *self.history_reads.lock().unwrap() += 1;
        let reply = self.engine.handle(method, params).await;
        *self.history_answers.lock().unwrap() += 1;
        let mut held = self.hold_pages.subscribe();
        let _ = held.wait_for(|held| !held).await;
        reply
    }
}

struct Fixture {
    runtime: tokio::runtime::Runtime,
    temp: tempfile::TempDir,
    root: PathBuf,
    core: luna_engine::EngineCore,
    service: Arc<CountedEngine>,
    window: WindowHandle<Changes>,
}

impl Fixture {
    /// A task in a freshly initialized repository, with its History open.
    fn open(cx: &mut TestAppContext, hold_pages: bool) -> Self {
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
                history_answers: Mutex::new(0),
                hold_pages: watch::Sender::new(hold_pages),
            });
            (temp, root, core, service)
        };

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
        Self {
            runtime,
            temp,
            root,
            core,
            service,
            window,
        }
    }

    fn reads(&self) -> usize {
        *self.service.history_reads.lock().unwrap()
    }

    fn answers(&self) -> usize {
        *self.service.history_answers.lock().unwrap()
    }

    /// Drive both executors and paint frames until `done` holds. Painting
    /// matters: every frame renders the pane, which asks History to load.
    fn pump(
        &self,
        cx: &mut TestAppContext,
        what: &str,
        mut done: impl FnMut(&Self, &Changes, &App) -> bool,
    ) {
        for _ in 0..1500 {
            self.frame(cx);
            if self
                .window
                .read_with(cx, |pane, cx| done(self, pane, cx))
                .unwrap()
            {
                return;
            }
        }
        panic!("History fixture never reached: {what}");
    }

    fn frame(&self, cx: &mut TestAppContext) {
        cx.run_until_parked();
        self.runtime
            .block_on(async { tokio::time::sleep(Duration::from_millis(2)).await });
        cx.update_window(self.window.into(), |_, window, cx| {
            window.refresh();
            window.draw(cx).clear();
        })
        .unwrap();
    }

    /// Frames under app-state churn: a streaming turn notifies constantly.
    fn churn(&self, cx: &mut TestAppContext, frames: usize) {
        for _ in 0..frames {
            self.window
                .update(cx, |pane, _, cx| pane.state.update(cx, |_, cx| cx.notify()))
                .unwrap();
            self.frame(cx);
        }
    }

    fn shutdown(self) {
        let Self {
            runtime,
            temp,
            core,
            service,
            ..
        } = self;
        service.hold_pages.send_replace(false);
        runtime.block_on(core.shutdown());
        let _entered = runtime.enter();
        drop(service);
        drop(core);
        drop(temp);
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

fn commit_count(pane: &Changes, cx: &App) -> Option<usize> {
    pane.history.as_ref()?.read(cx).commit_count()
}

#[gpui::test]
fn unborn_history_settles_then_shows_the_first_commit(cx: &mut TestAppContext) {
    let fixture = Fixture::open(cx, false);
    fixture.pump(cx, "the empty first page", |_, pane, cx| {
        commit_count(pane, cx) == Some(0)
    });
    // Frames and app-state churn must not re-ask for a page the engine
    // already answered with no commits. The checkout's first revision may
    // arrive meanwhile and re-read it once: the page was requested before
    // the checkout had one.
    fixture.churn(cx, 20);
    assert!(fixture.reads() <= 2, "History re-read an empty page");

    fixture.pump(cx, "the checkout's diff", |_, pane, cx| {
        pane.resolved(cx).is_some()
    });
    fixture.churn(cx, 20);
    let settled = fixture.reads();
    fixture.churn(cx, 20);
    assert_eq!(
        fixture.reads(),
        settled,
        "History re-read an empty page of an unchanged checkout"
    );

    // The issue's recovery step: the first commit lands from the terminal.
    git(
        &fixture.root,
        &["commit", "-q", "--allow-empty", "-m", "QA baseline"],
    );
    fixture.pump(cx, "the first commit", |_, pane, cx| {
        commit_count(pane, cx) == Some(1)
    });
    fixture.shutdown();
}

#[gpui::test]
fn a_commit_while_the_first_page_is_on_its_way_still_appears(cx: &mut TestAppContext) {
    let fixture = Fixture::open(cx, true);
    // The engine has read the empty repository; its answer is still on the
    // way when the first commit lands and the checkout's diff reports it.
    fixture.pump(cx, "the first page read", |fixture, _, _| {
        fixture.answers() == 1
    });
    git(
        &fixture.root,
        &["commit", "-q", "--allow-empty", "-m", "QA baseline"],
    );
    fixture.pump(cx, "the checkout's diff", |_, pane, cx| {
        pane.resolved(cx).is_some()
    });
    fixture.service.hold_pages.send_replace(false);
    fixture.pump(cx, "the first commit", |_, pane, cx| {
        commit_count(pane, cx) == Some(1)
    });
    fixture.shutdown();
}
