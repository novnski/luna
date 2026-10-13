//! ZI-04 end to end through the production Shell: a typed tool badge in a
//! real transcript row is clicked, the shell's existing workspace link route
//! resolves it under the OWNING chat, and the Files tab reads it from the
//! owning host through real engines.
//!
//! Two real `EngineCore`s stand behind one relay that dispatches on the
//! address the UI sets (`targetDeviceId`), as the engine's device routing
//! does. The local host also holds a decoy chat with the remote chat's id and
//! different bytes at the same relative path, and the main and side checkouts
//! share a relative path with different bytes, so a read under the wrong
//! chat or host shows up as a wrong content hash. Expected hashes are SHA-256
//! of the bytes this file wrote, computed here.
//!
//! Owners: the badge's own gesture contract (one activation per click or
//! key, the card never toggles, refused paths inert) is
//! `transcript::file_badges::tests`; this file owns resolution, tab reuse,
//! unsaved buffers, drafts, host routing and truthful failures.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui::{AppContext as _, TestAppContext, VisualTestContext};
use luna_doc::{MessagePart, MessageRole, MessageStatus, SessionMessageEntry, TranscriptFrame};
use luna_rpc::{RpcError, RpcReply, RpcService};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use super::*;

const LOCAL: &str = "local";
const REMOTE_DEVICE: &str = "zi04-remote-mac";
const MAIN: &str = "zi04-main";
const SIDE: &str = "zi04-side";
const REMOTE: &str = "zi04-remote";
const DRAFT: &str = "keep this queued draft";

/// Methods that would send, queue or write: none may follow a badge.
const FORBIDDEN: &[&str] = &[
    methods::QUEUE_COMMAND,
    methods::QUEUE_MESSAGE,
    methods::SEND_SIDE_CHAT,
    methods::SEND_QUEUED_MESSAGE_NOW,
    methods::STEER_QUEUED_MESSAGE_NOW,
    methods::EXECUTE_TASK_COMMAND,
    methods::BEGIN_DRAFT_SUBMISSION,
    methods::WRITE_WORKSPACE_FILE,
];

#[derive(Debug, Clone)]
struct Call {
    host: &'static str,
    method: String,
    params: Value,
    reply: Result<Option<Value>, String>,
}

/// Device routing between the two real hosts: a request addressed to the
/// remote device goes there (without the address, as the relay delivers
/// it); everything else is the local host's.
struct Relay {
    local: Arc<dyn RpcService>,
    remote: Arc<dyn RpcService>,
    calls: Mutex<Vec<Call>>,
}

#[async_trait::async_trait]
impl RpcService for Relay {
    async fn handle(&self, method: &str, mut params: Value) -> Result<RpcReply, RpcError> {
        let remote = params.get("targetDeviceId").and_then(Value::as_str) == Some(REMOTE_DEVICE);
        if remote && let Some(object) = params.as_object_mut() {
            object.remove("targetDeviceId");
        }
        let (host, service) = if remote {
            ("remote", &self.remote)
        } else {
            ("local", &self.local)
        };
        let reply = service.handle(method, params.clone()).await;
        let logged = match &reply {
            Ok(RpcReply::Value(value)) => Ok(Some(value.clone())),
            Ok(RpcReply::Stream(_)) => Ok(None),
            Err(error) => Err(error.to_string()),
        };
        self.calls.lock().unwrap().push(Call {
            host,
            method: method.to_owned(),
            params,
            reply: logged,
        });
        reply
    }
}

/// Fields drop in order: engines, then their directories, then the runtime
/// their tasks run on.
struct Hosts {
    relay: Arc<Relay>,
    _local: luna_engine::EngineCore,
    _remote: luna_engine::EngineCore,
    main_root: PathBuf,
    side_root: PathBuf,
    remote_root: PathBuf,
    ui_data: PathBuf,
    _temp: tempfile::TempDir,
    runtime: tokio::runtime::Runtime,
}

fn write(root: &Path, path: &str, bytes: &str) {
    let path = root.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

fn sha256(bytes: &str) -> String {
    Sha256::digest(bytes.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn engine(data: &Path, device: &str) -> luna_engine::EngineCore {
    std::fs::create_dir_all(data).unwrap();
    std::fs::write(data.join("device-id"), device).unwrap();
    luna_engine::EngineCore::assemble(
        data,
        Arc::new(luna_engine::HarnessRegistry::new()),
        luna_proto::HarnessId::Mock,
        None,
    )
    .unwrap()
}

impl Hosts {
    fn new() -> Self {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let temp = tempfile::tempdir().unwrap();
        // Canonical roots: the host compares canonical paths.
        let base = temp.path().canonicalize().unwrap();
        let main_root = base.join("main checkout");
        let side_root = base.join("side checkout");
        let remote_root = base.join("remote checkout");
        let decoy_root = base.join("decoy checkout");
        let outside = base.join("outside");
        write(&main_root, "src/naïve café.rs", "fn café() {} // main\n");
        write(&main_root, "notes/plan.md", "# Plan\n");
        write(&main_root, "src/a.rs", "pub fn a() {}\n");
        write(&main_root, "src/b.rs", "pub fn b() {}\n");
        write(&main_root, "src/same.rs", "main bytes\n");
        write(&main_root, "src/gone.rs", "removed after the tool ran\n");
        write(&side_root, "src/same.rs", "side bytes\n");
        write(&remote_root, "src/same.rs", "remote bytes\n");
        write(&decoy_root, "src/same.rs", "decoy bytes\n");
        write(&outside, "secret.txt", "OUTSIDE-SECRET\n");
        std::os::unix::fs::symlink(outside.join("secret.txt"), main_root.join("src/escape.rs"))
            .unwrap();
        std::os::unix::fs::symlink(&outside, main_root.join("linked")).unwrap();
        let (local, remote) = {
            let _entered = runtime.enter();
            let local = engine(&base.join("engine-local"), LOCAL);
            for (space, root, chat) in [
                ("zi04-space-main", &main_root, MAIN),
                ("zi04-space-side", &side_root, SIDE),
                // Same chat id as the remote chat, different bytes: what a
                // read sent to the wrong host would return.
                ("zi04-space-decoy", &decoy_root, REMOTE),
            ] {
                local
                    .workspace
                    .create_space(space, LOCAL, &root.to_string_lossy(), None, false)
                    .unwrap();
                local
                    .workspace
                    .create_chat(chat, Some(space), None, None, None)
                    .unwrap();
            }
            let remote = engine(&base.join("engine-remote"), REMOTE_DEVICE);
            remote
                .workspace
                .create_space(
                    "zi04-space-remote",
                    REMOTE_DEVICE,
                    &remote_root.to_string_lossy(),
                    None,
                    false,
                )
                .unwrap();
            remote
                .workspace
                .create_chat(REMOTE, Some("zi04-space-remote"), None, None, None)
                .unwrap();
            (local, remote)
        };
        let relay = Arc::new(Relay {
            local: local.rpc_service(),
            remote: remote.rpc_service(),
            calls: Mutex::new(Vec::new()),
        });
        Self {
            relay,
            _local: local,
            _remote: remote,
            main_root,
            side_root,
            remote_root,
            ui_data: base.join("ui"),
            _temp: temp,
            runtime,
        }
    }

    fn calls(&self) -> Vec<Call> {
        self.relay.calls.lock().unwrap().clone()
    }

    /// Every read of `path` for `chat`: (host, reply).
    fn reads(&self, chat: &str, path: &str) -> Vec<(&'static str, Result<Option<Value>, String>)> {
        self.calls()
            .into_iter()
            .filter(|call| {
                call.method == methods::READ_WORKSPACE_FILE
                    && call.params["chatId"] == chat
                    && call.params["path"] == path
            })
            .map(|call| (call.host, call.reply))
            .collect()
    }

    fn rows(&self) -> Vec<Value> {
        [
            (MAIN, LOCAL, "zi04-space-main", &self.main_root),
            (SIDE, LOCAL, "zi04-space-side", &self.side_root),
            (
                REMOTE,
                REMOTE_DEVICE,
                "zi04-space-remote",
                &self.remote_root,
            ),
        ]
        .into_iter()
        .map(|(id, device, space, cwd)| {
            json!({
                "id": id, "deviceId": device, "spaceId": space,
                "cwd": cwd.to_string_lossy(), "title": id, "archived": false,
                "createdAt": "2026-10-13T00:00:00Z",
            })
        })
        .collect()
    }
}

/// The production main transcript beside the shell's active file tab, so
/// badges and the editor are really painted and clicked.
struct Host {
    shell: Entity<Shell>,
    _observe: Subscription,
}

impl Render for Host {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let shell = self.shell.read(cx);
        let transcript = shell.transcript.clone();
        let file = match shell.resolved_right_active(cx) {
            RightSurface::File(id) => shell.file_surfaces.get(&id).cloned(),
            _ => None,
        };
        div()
            .size_full()
            .flex()
            .flex_row()
            .child(div().w(px(760.0)).h_full().relative().child(transcript))
            .children(file.map(|file| div().flex_1().min_w_0().h_full().child(file)))
    }
}

fn tool(id: &str, call: luna_proto::ToolCall) -> MessagePart {
    MessagePart::Tool {
        id: id.into(),
        call,
        is_error: false,
        resolved: true,
        output: None,
        diff: None,
        output_ref: None,
        output_bytes: None,
        diff_ref: None,
        diff_stats: None,
        subagent_ref: None,
        subagent_status: None,
        subagent_tail: None,
        subagent_activity: None,
    }
}

fn read(path: &Path) -> luna_proto::ToolCall {
    luna_proto::ToolCall::ReadFile {
        path: path.to_string_lossy().into_owned(),
    }
}

/// A live turn: its trailing tool group is open, every card closed.
fn turn(parts: Vec<MessagePart>) -> SessionMessageEntry {
    SessionMessageEntry {
        id: "zi04-turn".into(),
        role: MessageRole::Assistant,
        parts,
        created_at: 1_790_000_000_000,
        device_id: LOCAL.into(),
        status: Some(MessageStatus::Streaming),
        continuation_of: None,
    }
}

fn shell_window<'a>(
    cx: &'a mut TestAppContext,
    hosts: &Hosts,
    selected: &str,
    entries: Vec<SessionMessageEntry>,
) -> (Entity<Shell>, &'a mut VisualTestContext) {
    let ui_data = hosts.ui_data.clone();
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::default());
        crate::app_menus::init(cx);
        settings::init(settings::UiSettings::default(), ui_data.clone(), cx);
        gpui_tokio::init_from_handle(cx, hosts.runtime.handle().clone());
        crate::motion::set_reduced_motion(cx, true);
    });
    let service: Arc<dyn RpcService> = hosts.relay.clone();
    let client = {
        let _entered = hosts.runtime.enter();
        luna_rpc::memory_client(service)
    };
    let rows = hosts.rows();
    let selected = selected.to_owned();
    let (host, cx) = cx.add_window_view(move |_, cx| {
        let shell = cx.new(|cx| {
            let state = cx.new(|_| {
                let mut state = AppState::new();
                state.set_test_engine(crate::state::EngineHandle::from_test_client(client));
                state.local_device_id = Some(LOCAL.into());
                state.chats = rows
                    .into_iter()
                    .map(|row| serde_json::from_value(row).unwrap())
                    .collect();
                state.selected_chat = Some(selected.clone());
                state
            });
            let mut shell = Shell::new(
                state,
                EngineBootConfig {
                    data_dir: ui_data,
                    ipc_port: 0,
                    edge_url: "http://127.0.0.1:1".into(),
                    edge_token: None,
                    org_id: None,
                    workos_client_id: None,
                    default_harness: luna_proto::HarnessId::Mock,
                },
                cx,
            );
            shell.active_chat = selected;
            shell
        });
        let observe = cx.observe(&shell, |_, _, cx| cx.notify());
        Host {
            shell,
            _observe: observe,
        }
    });
    let shell = host.read_with(cx, |host, _| host.shell.clone());
    let state = shell.read_with(cx, |shell, _| shell.state.clone());
    state.update(cx, |state, cx| {
        state
            .apply_transcript_frame(TranscriptFrame::reset(&entries))
            .expect("frame applies");
        cx.notify();
    });
    (shell, cx)
}

/// Let the UI, both engines and the paint settle.
fn settle(cx: &mut VisualTestContext, hosts: &Hosts) {
    for _ in 0..40 {
        cx.run_until_parked();
        hosts
            .runtime
            .block_on(async { tokio::time::sleep(Duration::from_millis(2)).await });
        cx.update(|window, cx| window.draw(cx).clear());
    }
}

fn click_badge(cx: &mut VisualTestContext, hosts: &Hosts, part: &str) {
    let selector: &'static str = Box::leak(format!("tool-file-badge-{part}").into_boxed_str());
    let at = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("badge {part} was not painted"))
        .center();
    cx.simulate_click(at, gpui::Modifiers::default());
    settle(cx, hosts);
}

type FileTab = (u64, String, String, String);

/// The file tabs as (id, panel, owner, path), and the active right surface.
fn file_tabs(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> (Vec<FileTab>, RightSurface) {
    shell.read_with(cx, |shell, cx| {
        let mut tabs: Vec<_> = shell
            .file_surface_owners
            .iter()
            .map(|(id, (panel, owner))| {
                (
                    *id,
                    panel.clone(),
                    owner.clone(),
                    shell.file_surface_paths[id].clone(),
                )
            })
            .collect();
        tabs.sort();
        (tabs, shell.resolved_right_active(cx))
    })
}

fn tab_for(shell: &Entity<Shell>, cx: &mut VisualTestContext, owner: &str, path: &str) -> u64 {
    file_tabs(shell, cx)
        .0
        .into_iter()
        .find(|(_, _, tab_owner, tab_path)| tab_owner == owner && tab_path == path)
        .map(|(id, ..)| id)
        .unwrap_or_else(|| panic!("no {owner} tab for {path}"))
}

#[track_caller]
fn assert_read(hosts: &Hosts, chat: &str, path: &str, host: &str, bytes: &str) {
    let reads = hosts.reads(chat, path);
    assert!(!reads.is_empty(), "{chat} never read {path}");
    for (from, reply) in reads {
        assert_eq!(from, host, "{chat}:{path} was read from the wrong host");
        let reply = reply.unwrap_or_else(|error| panic!("{chat}:{path} failed: {error}"));
        let reply = reply.expect("a unary file read");
        assert_eq!(
            reply["contentHash"].as_str(),
            Some(sha256(bytes).as_str()),
            "{chat}:{path} is not the owning checkout's file"
        );
    }
}

fn assert_nothing_sent(hosts: &Hosts) {
    let sent: Vec<_> = hosts
        .calls()
        .into_iter()
        .filter(|call| FORBIDDEN.contains(&call.method.as_str()))
        .map(|call| call.method)
        .collect();
    assert!(sent.is_empty(), "a file badge sent or wrote: {sent:?}");
}

fn draft(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> String {
    shell.read_with(cx, |shell, cx| {
        shell.composer.read(cx).input.read(cx).text().to_owned()
    })
}

#[gpui::test]
fn badges_open_the_owning_chats_exact_file_once_and_keep_drafts_and_buffers(
    cx: &mut TestAppContext,
) {
    let _selection = crate::markdown::selection::test_state_lock();
    let hosts = Hosts::new();
    let root = hosts.main_root.clone();
    let (shell, cx) = shell_window(
        cx,
        &hosts,
        MAIN,
        vec![turn(vec![
            tool("m-read", read(&root.join("src/naïve café.rs"))),
            tool(
                "m-write",
                luna_proto::ToolCall::WriteFile {
                    path: root.join("notes/plan.md").to_string_lossy().into_owned(),
                    content: None,
                },
            ),
            // Codex reports one multi-file change as `f3` and `f3#1`.
            tool(
                "f3",
                luna_proto::ToolCall::EditFile {
                    path: root.join("src/a.rs").to_string_lossy().into_owned(),
                    old_string: None,
                    new_string: None,
                },
            ),
            tool(
                "f3#1",
                luna_proto::ToolCall::ApplyPatch {
                    path: Some(root.join("src/b.rs").to_string_lossy().into_owned()),
                },
            ),
            tool("m-gone", read(&root.join("src/gone.rs"))),
            tool("m-escape", read(&root.join("src/escape.rs"))),
            tool("m-linked", read(&root.join("linked/secret.txt"))),
            tool("m-outside", read(Path::new("/etc/hosts"))),
        ])],
    );
    settle(cx, &hosts);
    let input = shell.read_with(cx, |shell, cx| shell.composer.read(cx).input.clone());
    input.update(cx, |input, cx| input.set_text(DRAFT, cx));
    settle(cx, &hosts);
    assert_eq!(
        draft(&shell, cx),
        DRAFT,
        "the fixture's draft did not settle"
    );

    // Read, write and each file of the multi-file change: the main chat's
    // own tab for that exact path, read from this checkout.
    for (part, path, bytes) in [
        ("m-read", "src/naïve café.rs", "fn café() {} // main\n"),
        ("m-write", "notes/plan.md", "# Plan\n"),
        ("f3", "src/a.rs", "pub fn a() {}\n"),
        ("f3#1", "src/b.rs", "pub fn b() {}\n"),
    ] {
        click_badge(cx, &hosts, part);
        let id = tab_for(&shell, cx, MAIN, path);
        let (tabs, active) = file_tabs(&shell, cx);
        assert_eq!(
            active,
            RightSurface::File(id),
            "{part} did not show its tab"
        );
        assert!(
            tabs.iter()
                .any(|(tab, panel, ..)| *tab == id && panel == MAIN),
            "{part} opened outside the main chat's pane"
        );
        assert_read(&hosts, MAIN, path, "local", bytes);
    }
    assert_eq!(file_tabs(&shell, cx).0.len(), 4);

    // An unsaved edit in the read file's tab survives activating its badge
    // again: the same tab comes forward, nothing is duplicated or reloaded
    // over the buffer.
    click_badge(cx, &hosts, "m-read");
    let read_tab = tab_for(&shell, cx, MAIN, "src/naïve café.rs");
    let editor = shell.read_with(cx, |shell, _| shell.file_surfaces[&read_tab].clone());
    cx.update(|window, cx| editor.update(cx, |editor, cx| editor.focus_editor(window, cx)));
    settle(cx, &hosts);
    cx.simulate_input("unsaved");
    settle(cx, &hosts);
    assert!(
        editor.read_with(cx, |editor, _| editor.has_unsaved_changes()),
        "typing did not reach the opened editor"
    );
    click_badge(cx, &hosts, "f3");
    click_badge(cx, &hosts, "m-read");
    let (tabs_after, active) = file_tabs(&shell, cx);
    assert_eq!(tabs_after.len(), 4, "a repeated badge duplicated a tab");
    assert_eq!(active, RightSurface::File(read_tab));
    assert!(
        editor.read_with(cx, |editor, _| editor.has_unsaved_changes()),
        "the unsaved buffer was replaced"
    );

    // Deleted after the tool ran: the tab opens and the host says so; no
    // other source is consulted.
    std::fs::remove_file(root.join("src/gone.rs")).unwrap();
    click_badge(cx, &hosts, "m-gone");
    tab_for(&shell, cx, MAIN, "src/gone.rs");
    let gone = hosts.reads(MAIN, "src/gone.rs");
    assert!(!gone.is_empty());
    for (from, reply) in gone {
        assert_eq!(from, "local");
        let error = reply.expect_err("a deleted file read as present");
        assert!(error.contains("file not found"), "{error}");
    }

    // Symlinks out of the checkout are the host's to refuse: the link itself
    // is a read-only entry without text, a linked folder is not traversed,
    // and the outside bytes never reach the UI.
    click_badge(cx, &hosts, "m-escape");
    let escape = hosts.reads(MAIN, "src/escape.rs");
    assert!(!escape.is_empty());
    for (_, reply) in escape {
        let reply = reply.unwrap().unwrap();
        assert_eq!(reply["readOnlyReason"], "symlink");
        assert!(reply["text"].is_null());
    }
    click_badge(cx, &hosts, "m-linked");
    let linked = hosts.reads(MAIN, "linked/secret.txt");
    assert!(!linked.is_empty());
    for (_, reply) in linked {
        let error = reply.expect_err("a symlinked folder was traversed");
        assert!(error.contains("symlink"), "{error}");
    }
    assert!(
        !hosts
            .calls()
            .iter()
            .any(|call| format!("{:?}", call.reply).contains("OUTSIDE-SECRET")),
        "bytes outside the checkout reached the UI"
    );

    // Outside the chat's folder: inert, no tab, no read.
    let before = file_tabs(&shell, cx).0.len();
    click_badge(cx, &hosts, "m-outside");
    assert_eq!(file_tabs(&shell, cx).0.len(), before);
    assert!(
        !hosts
            .calls()
            .iter()
            .any(|call| call.method == methods::READ_WORKSPACE_FILE
                && call.params["path"]
                    .as_str()
                    .is_some_and(|path| path.contains("hosts"))),
        "an outside path was read"
    );

    assert_eq!(draft(&shell, cx), DRAFT, "the composer draft changed");
    assert_nothing_sent(&hosts);
}

#[gpui::test]
fn a_side_chat_badge_opens_under_the_side_chats_own_checkout(cx: &mut TestAppContext) {
    let _selection = crate::markdown::selection::test_state_lock();
    let hosts = Hosts::new();
    let (shell, cx) = shell_window(cx, &hosts, MAIN, Vec::new());
    settle(cx, &hosts);
    // The side chat's real workbench pane hands the shell the activation its
    // badge builds (the badge side is `transcript::file_badges::tests`).
    let typed = hosts.side_root.join("src/same.rs");
    let outcome = cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            let pane = shell.workbench_view(
                &luna_proto::ChatWorkbenchTab {
                    chat_id: SIDE.into(),
                    side_parent_id: Some(MAIN.into()),
                },
                cx,
            );
            let links = pane
                .read(cx)
                .transcript()
                .read(cx)
                .link_ui()
                .expect("a pane transcript has the workspace route");
            assert_eq!(links.source_session.as_deref(), Some(SIDE));
            let activation = crate::markdown::render::LinkActivation {
                target: crate::markdown::render::LinkTarget {
                    label: "src/same.rs".into(),
                    original: crate::workspace_links::file_mention_target(&typed.to_string_lossy()),
                    navigation: Err("workspace file"),
                },
                action: crate::markdown::render::LinkAction::Primary,
                source_session: links.source_session,
            };
            shell.activate_session_link(&activation, window, cx)
        })
    });
    assert_eq!(outcome, crate::markdown::render::LinkOutcome::Internal);
    settle(cx, &hosts);
    let id = tab_for(&shell, cx, SIDE, "src/same.rs");
    let (tabs, active) = file_tabs(&shell, cx);
    assert_eq!(active, RightSurface::File(id));
    assert_eq!(
        tabs.iter()
            .find(|(tab, ..)| *tab == id)
            .map(|(_, panel, ..)| panel.as_str()),
        Some(MAIN),
        "the side chat's tab belongs in its Main's pane"
    );
    // The side checkout's bytes, never the main checkout's same-named file.
    assert_read(&hosts, SIDE, "src/same.rs", "local", "side bytes\n");
    assert!(hosts.reads(MAIN, "src/same.rs").is_empty());
    assert_nothing_sent(&hosts);
}

#[gpui::test]
fn a_remote_chat_badge_reads_the_owning_hosts_bytes(cx: &mut TestAppContext) {
    let _selection = crate::markdown::selection::test_state_lock();
    let hosts = Hosts::new();
    let typed = hosts.remote_root.join("src/same.rs");
    let (shell, cx) = shell_window(
        cx,
        &hosts,
        REMOTE,
        vec![turn(vec![tool("r-read", read(&typed))])],
    );
    settle(cx, &hosts);
    click_badge(cx, &hosts, "r-read");
    let id = tab_for(&shell, cx, REMOTE, "src/same.rs");
    assert_eq!(file_tabs(&shell, cx).1, RightSurface::File(id));
    // Addressed to the owning device; the local decoy with the same chat id
    // and path is never asked.
    assert_read(&hosts, REMOTE, "src/same.rs", "remote", "remote bytes\n");
    click_badge(cx, &hosts, "r-read");
    assert_eq!(
        file_tabs(&shell, cx).0.len(),
        1,
        "a repeated badge duplicated a tab"
    );
    assert_nothing_sent(&hosts);
}
