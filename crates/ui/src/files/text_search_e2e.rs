//! ZI-05 Files text search driven through a real engine over the RPC
//! transport: only the current query, generation and host may publish, host
//! failures never read as "no matches", and a selected match lands on the
//! exact bytes of the real editor (or a truthful fallback when the file
//! changed or vanished).

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use gpui::{AppContext as _, TestAppContext, WindowHandle};
use luna_rpc::{RpcError, RpcReply, RpcService, methods};
use serde_json::{Value, json};

use super::super::text_search::{FileSearchMode, TextNavigationTarget, TextSearchPhase};
use super::*;

const CHAT: &str = "chat-text-search";
const SPACE: &str = "space-text-search";
const DEVICE: &str = "local";

/// The real engine, with `SearchWorkspaceText` answers optionally held per
/// query after the scan ran, so a reply can arrive after a newer one.
struct HeldAnswers {
    engine: Arc<dyn RpcService>,
    holds: Mutex<HashMap<String, Arc<tokio::sync::Semaphore>>>,
    requests: Mutex<Vec<Value>>,
}

impl HeldAnswers {
    fn hold(&self, query: &str) -> Arc<tokio::sync::Semaphore> {
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        self.holds
            .lock()
            .unwrap()
            .insert(query.to_string(), gate.clone());
        gate
    }
}

#[async_trait::async_trait]
impl RpcService for HeldAnswers {
    async fn handle(&self, method: &str, params: Value) -> Result<RpcReply, RpcError> {
        if method != methods::SEARCH_WORKSPACE_TEXT {
            return self.engine.handle(method, params).await;
        }
        self.requests.lock().unwrap().push(params.clone());
        // One-shot: only the first request for a held query waits.
        let gate = params["query"]
            .as_str()
            .and_then(|query| self.holds.lock().unwrap().remove(query));
        let reply = self.engine.handle(method, params).await;
        if let Some(gate) = gate {
            let _ = gate.acquire().await;
        }
        reply
    }
}

/// A host whose LUNA predates the operation answers every call this way.
struct OlderHost;

#[async_trait::async_trait]
impl RpcService for OlderHost {
    async fn handle(&self, method: &str, _params: Value) -> Result<RpcReply, RpcError> {
        Err(RpcError::UnknownMethod(method.into()))
    }
}

/// Fields drop in order: the engine before its data directory, and both
/// before the runtime its tasks run on.
struct Fixture {
    service: Arc<HeldAnswers>,
    core: luna_engine::EngineCore,
    _temp: tempfile::TempDir,
    root: PathBuf,
    runtime: tokio::runtime::Runtime,
}

impl Fixture {
    fn new(cx: &mut TestAppContext, files: &[(&str, String)]) -> Self {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
        });
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        // Engine assembly spawns onto this runtime; the guard must end
        // before the runtime moves into the fixture.
        let (temp, root, core, service) = {
            let _entered = runtime.enter();
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().canonicalize().unwrap().join("plain folder ü");
            for (path, contents) in files {
                let path = root.join(path);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, contents).unwrap();
            }
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
                .create_space(SPACE, DEVICE, &root.to_string_lossy(), None, false)
                .unwrap();
            core.workspace
                .create_chat(CHAT, Some(SPACE), None, None, None)
                .unwrap();
            let service = Arc::new(HeldAnswers {
                engine: core.rpc_service(),
                holds: Mutex::new(HashMap::new()),
                requests: Mutex::new(Vec::new()),
            });
            (temp, root, core, service)
        };
        Self {
            service,
            core,
            _temp: temp,
            root,
            runtime,
        }
    }

    fn window(
        &self,
        cx: &mut TestAppContext,
        service: Arc<dyn RpcService>,
    ) -> WindowHandle<FilesSurface> {
        self.surface(cx, service, None)
    }

    /// A Files browser, or the owner's editor tab for `editor_path`.
    fn surface(
        &self,
        cx: &mut TestAppContext,
        service: Arc<dyn RpcService>,
        editor_path: Option<&str>,
    ) -> WindowHandle<FilesSurface> {
        let _entered = self.runtime.enter();
        let client = luna_rpc::memory_client(service);
        let cwd = self.root.to_string_lossy().to_string();
        let editor_path = editor_path.map(str::to_string);
        cx.add_window(move |_, cx| {
            let state = cx.new(move |_| {
                let mut state = crate::state::AppState::new();
                state.set_test_engine(crate::state::EngineHandle::from_test_client(client));
                state.local_device_id = Some(DEVICE.into());
                state.chats = vec![
                    serde_json::from_value(json!({
                        "id": CHAT, "deviceId": DEVICE, "spaceId": SPACE, "cwd": cwd,
                        "title": "Text search", "archived": false,
                        "createdAt": "2026-10-13T00:00:00Z",
                    }))
                    .unwrap(),
                ];
                state
            });
            match editor_path {
                Some(path) => FilesSurface::new_editor(
                    state,
                    CHAT.into(),
                    path,
                    false,
                    1000,
                    13.0,
                    false,
                    false,
                    cx,
                ),
                None => FilesSurface::new(state, CHAT.into(), false, 1000, 13.0, false, false, cx),
            }
        })
    }

    fn drain(&self, cx: &mut TestAppContext) {
        for _ in 0..30 {
            cx.run_until_parked();
            self.runtime.block_on(Self::tick());
        }
    }

    /// One short real-time wait driven by the runtime. The timer must be
    /// created inside the runtime, so the sleep is built within the future.
    async fn tick() {
        tokio::time::sleep(Duration::from_millis(2)).await;
    }

    fn sent(&self) -> usize {
        self.service.requests.lock().unwrap().len()
    }

    /// Drive both executors (and paint) until `done` holds.
    fn pump(
        &self,
        cx: &mut TestAppContext,
        window: WindowHandle<FilesSurface>,
        what: &str,
        mut done: impl FnMut(&FilesSurface, &App) -> bool,
    ) {
        for _ in 0..1500 {
            cx.run_until_parked();
            self.runtime.block_on(Self::tick());
            cx.update_window(window.into(), |_, window, cx| {
                window.refresh();
                window.draw(cx).clear();
            })
            .unwrap();
            if window
                .read_with(cx, |surface, cx| done(surface, cx))
                .unwrap()
            {
                return;
            }
        }
        panic!("text search fixture never reached: {what}");
    }

    fn search(&self, cx: &mut TestAppContext, window: WindowHandle<FilesSurface>, query: &str) {
        window
            .update(cx, |surface, _, cx| {
                surface.set_search_mode(FileSearchMode::Text, cx);
                surface
                    .search
                    .update(cx, |input, cx| input.set_text(query, cx));
            })
            .unwrap();
        // Past the as-you-type debounce.
        cx.executor().advance_clock(Duration::from_millis(400));
    }

    fn settled(&self, cx: &mut TestAppContext, window: WindowHandle<FilesSurface>, query: &str) {
        let query = query.to_string();
        self.pump(cx, window, "a published answer", move |surface, _| {
            let state = &surface.search_state.text;
            state.query == query
                && !matches!(
                    state.phase,
                    TextSearchPhase::Searching | TextSearchPhase::Idle
                )
        });
    }

    fn shutdown(self) {
        let Self {
            service,
            core,
            _temp,
            runtime,
            ..
        } = self;
        runtime.block_on(core.shutdown());
        let _entered = runtime.enter();
        drop(service);
        drop(core);
        drop(_temp);
    }
}

fn status(window: WindowHandle<FilesSurface>, cx: &mut TestAppContext) -> String {
    window
        .read_with(cx, |surface, _| surface.text_search_status().to_string())
        .unwrap()
}

/// Byte range of the `occurrence`th `needle` on 1-based `line` of `text`,
/// computed from the fixture alone.
fn expected_range(
    text: &str,
    line: usize,
    needle: &str,
    occurrence: usize,
) -> std::ops::Range<usize> {
    let line_start: usize = text
        .split('\n')
        .take(line - 1)
        .map(|line| line.len() + 1)
        .sum();
    let line_text = text.split('\n').nth(line - 1).unwrap();
    let start = line_text
        .match_indices(needle)
        .nth(occurrence)
        .map(|(start, _)| start)
        .unwrap();
    line_start + start..line_start + start + needle.len()
}

fn select_row(window: WindowHandle<FilesSurface>, cx: &mut TestAppContext, row: usize) {
    window
        .update(cx, |surface, _, cx| {
            surface.search_state.text.active = row;
            surface.activate_text_search_result(cx);
        })
        .unwrap();
}

fn editor_selection(
    window: WindowHandle<FilesSurface>,
    cx: &mut TestAppContext,
    path: &str,
) -> Option<(
    std::ops::Range<usize>,
    String,
    Option<std::ops::Range<usize>>,
)> {
    window
        .read_with(cx, |surface, cx| {
            let editor = surface
                .preview
                .documents
                .get(path)?
                .editor
                .as_ref()?
                .read(cx);
            Some((
                editor.selected_range(),
                editor.value().to_string(),
                editor.visible_row_range(),
            ))
        })
        .unwrap()
}

fn long_file() -> String {
    let mut text = String::new();
    for line in 1..=600 {
        if line == 400 {
            text.push_str("    let deep = \"needle\";\n");
        } else {
            text.push_str(&format!("// filler {line}\n"));
        }
    }
    text
}

const LIB: &str = "fn größe() {}\nlet naïve = \"Needle\"; // 🦀 needle needs\n";

/// Regressions caught: an older keystroke's answer replacing the newer one;
/// UTF-8 byte offsets applied as character offsets (selection shifted by the
/// multibyte text before the match); a match opened without scrolling its
/// row into view; another file's match navigating the wrong tab.
#[gpui::test]
fn late_answers_never_publish_and_matches_select_exact_editor_bytes(cx: &mut TestAppContext) {
    let long = long_file();
    let fixture = Fixture::new(
        cx,
        &[
            ("docs/guide notes.md", "a needle in markdown\n".into()),
            ("src/lib.rs", LIB.into()),
            ("src/long.rs", long.clone()),
        ],
    );
    let window = fixture.window(cx, fixture.service.clone());

    let early = fixture.service.hold("need");
    fixture.search(cx, window, "need");
    fixture.pump(cx, window, "the first keystroke's request", |_, _| {
        fixture.sent() == 1
    });
    fixture.search(cx, window, "needle");
    fixture.settled(cx, window, "needle");
    let published = status(window, cx);
    assert_eq!(published, "4 matches in 3 files");
    // The earlier keystroke's answer ("need" also matches "needs") arrives
    // late; it must not replace the newer answer.
    early.add_permits(1);
    fixture.drain(cx);
    assert_eq!(status(window, cx), published);
    let requests = fixture.service.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 2, "{requests:?}");
    assert_eq!(requests[0]["searchSession"], requests[1]["searchSession"]);
    assert!(requests[0]["generation"].as_u64() < requests[1]["generation"].as_u64());
    assert_eq!(requests[1]["query"], "needle");

    // Rows: docs file, its match; lib.rs, "Needle", "needle"; long.rs, its match.
    select_row(window, cx, 4);
    fixture.pump(cx, window, "lib.rs selection", |surface, _| {
        surface.preview.text_navigation.is_none()
            && surface.preview.active.as_deref() == Some("src/lib.rs")
    });
    let (selected, text, _) = editor_selection(window, cx, "src/lib.rs").unwrap();
    assert_eq!(selected, expected_range(LIB, 2, "needle", 0));
    assert_eq!(&text[selected], "needle");
    window
        .read_with(cx, |surface, _| {
            assert!(surface.preview.text_notice.is_none())
        })
        .unwrap();

    // From an editor tab, another file's match opens that file's own tab.
    let opened = Arc::new(Mutex::new(Vec::new()));
    let entity = window.root(cx).unwrap();
    let _events = cx.update(|cx| {
        let opened = opened.clone();
        cx.subscribe(&entity, move |_, event: &FilesEvent, _| {
            if let FilesEvent::OpenFileAt { path, target } = event {
                opened.lock().unwrap().push((path.clone(), target.clone()));
            }
        })
    });
    select_row(window, cx, 6);
    cx.run_until_parked();
    let (path, target): (String, TextNavigationTarget) = opened.lock().unwrap().pop().unwrap();
    assert_eq!((path.as_str(), target.line), ("src/long.rs", 400));

    // What the shell does for that event: the owner's editor tab navigates.
    let tab = fixture.surface(cx, fixture.service.clone(), Some("src/long.rs"));
    tab.update(cx, |surface, _, cx| {
        surface.navigate_to_text_match(target, cx)
    })
    .unwrap();
    fixture.pump(cx, tab, "long.rs selection revealed", |surface, cx| {
        surface.preview.text_navigation.is_none()
            && surface
                .preview
                .documents
                .get("src/long.rs")
                .and_then(|document| document.editor.as_ref())
                .and_then(|editor| editor.read(cx).visible_row_range())
                .is_some_and(|rows| rows.contains(&399))
    });
    let (selected, text, visible) = editor_selection(tab, cx, "src/long.rs").unwrap();
    assert_eq!(selected, expected_range(&long, 400, "needle", 0));
    assert_eq!(&text[selected], "needle");
    let visible = visible.expect("laid-out editor");
    assert!(visible.contains(&399), "row 400 not revealed: {visible:?}");
    fixture.shutdown();
}

/// Regressions caught: a changed file still claiming the searched position,
/// a vanished file showing a stale precise match, and a capped or
/// unreachable/older host rendered as a complete "no matches" answer.
#[gpui::test]
fn changed_vanished_capped_and_unavailable_answers_stay_truthful(cx: &mut TestAppContext) {
    let crowded = (0..25)
        .map(|line| format!("needle {line}\n"))
        .collect::<String>();
    let fixture = Fixture::new(
        cx,
        &[
            ("docs/guide notes.md", "a needle in markdown\n".into()),
            ("src/crowded.txt", crowded),
            ("src/lib.rs", LIB.into()),
        ],
    );
    let window = fixture.window(cx, fixture.service.clone());

    fixture.search(cx, window, "zz-absent-literal");
    fixture.settled(cx, window, "zz-absent-literal");
    assert_eq!(status(window, cx), "No matches in 3 files");

    fixture.search(cx, window, "needle");
    fixture.settled(cx, window, "needle");
    // 1 + 20 (capped) + 2.
    let capped = status(window, cx);
    assert!(
        capped.starts_with("23 matches in 3 files so far") && capped.contains("more matches"),
        "{capped}"
    );

    // Rows: docs (0), match (1); crowded (2), 20 matches (3..=22);
    // lib.rs (23), "Needle" (24), "needle" (25).
    std::fs::write(
        fixture.root.join("src/lib.rs"),
        format!("// inserted above\n{LIB}"),
    )
    .unwrap();
    select_row(window, cx, 25);
    fixture.pump(cx, window, "moved match", |surface, _| {
        surface.preview.text_navigation.is_none()
            && surface.preview.active.as_deref() == Some("src/lib.rs")
    });
    let (selected, text, _) = editor_selection(window, cx, "src/lib.rs").unwrap();
    assert!(text[selected.clone()].eq_ignore_ascii_case("needle"));
    assert_eq!(
        text[..selected.start].matches('\n').count() + 1,
        3,
        "nearest occurrence is on the shifted line"
    );
    let notice = window
        .read_with(cx, |surface, _| surface.preview.text_notice.clone())
        .unwrap();
    assert!(
        notice
            .as_ref()
            .is_some_and(|(path, notice)| path == "src/lib.rs" && notice.contains("line 3")),
        "{notice:?}"
    );

    // The docs match's file disappears before it is opened.
    let window = fixture.window(cx, fixture.service.clone());
    fixture.search(cx, window, "needle");
    fixture.settled(cx, window, "needle");
    std::fs::remove_file(fixture.root.join("docs/guide notes.md")).unwrap();
    select_row(window, cx, 1);
    fixture.pump(cx, window, "vanished match", |surface, _| {
        surface.preview.text_navigation.is_none()
            && surface.preview.active.as_deref() == Some("docs/guide notes.md")
    });
    window
        .read_with(cx, |surface, _| {
            let document = &surface.preview.documents["docs/guide notes.md"];
            assert!(matches!(document.phase, DocumentPhase::Error(_)));
            assert!(document.editor.is_none());
            assert!(
                surface
                    .preview
                    .text_notice
                    .as_ref()
                    .is_some_and(|(_, notice)| notice.contains("could not be opened"))
            );
        })
        .unwrap();

    // The chat moves to a device this engine cannot reach while an answer
    // from the old host is still in flight: the old answer never publishes
    // and nothing is searched locally instead.
    let window = fixture.window(cx, fixture.service.clone());
    let late = fixture.service.hold("needle");
    let before = fixture.sent();
    fixture.search(cx, window, "needle");
    fixture.pump(cx, window, "held request", |_, _| {
        fixture.sent() == before + 1
    });
    window
        .update(cx, |surface, _, cx| {
            surface.state.update(cx, |state, cx| {
                state.chats[0].device_id = "remote-device".into();
                cx.notify();
            });
        })
        .unwrap();
    cx.executor().advance_clock(Duration::from_millis(400));
    fixture.pump(cx, window, "unreachable host", |surface, _| {
        matches!(
            surface.search_state.text.phase,
            TextSearchPhase::Unreachable(_)
        )
    });
    late.add_permits(1);
    fixture.drain(cx);
    let unreachable = status(window, cx);
    assert!(unreachable.contains("unreachable"), "{unreachable}");
    assert!(!unreachable.contains("No matches"), "{unreachable}");
    let last = fixture
        .service
        .requests
        .lock()
        .unwrap()
        .last()
        .cloned()
        .unwrap();
    assert_eq!(last["targetDeviceId"], "remote-device");

    // A host whose LUNA predates the operation.
    let older = fixture.window(cx, Arc::new(OlderHost));
    fixture.search(cx, older, "needle");
    fixture.settled(cx, older, "needle");
    let unsupported = status(older, cx);
    assert!(
        matches!(
            older
                .read_with(cx, |surface, _| surface.search_state.text.phase.clone())
                .unwrap(),
            TextSearchPhase::Unsupported
        ) && !unsupported.contains("No matches"),
        "{unsupported}"
    );
    fixture.shutdown();
}
