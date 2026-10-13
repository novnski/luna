//! AW-09 rendered Diagnostics states over held memory RPC and the real
//! AppState engine-attachment path. No engine, provider or file walk runs:
//! the owners' replies are scripted, and what is checked is what the page
//! renders and which methods it sends.
use super::*;
use crate::engine_epoch_fixture::{self as fixture, HeldRpc};
use gpui::{AppContext, TestAppContext, px};
use luna_proto::{StorageExclusion, StorageExclusionReason};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::runtime::Runtime;

/// The six owners the page reads, and nothing else.
const READS: [&str; 6] = [
    methods::GET_ENGINE_DIAGNOSTICS,
    methods::LIST_HARNESSES,
    methods::GET_PROVIDER_UPDATES,
    methods::LIST_EXTENSION_INSTANCES,
    methods::GET_DELETION_CLEANUP_SUMMARY,
    methods::GET_STORAGE_USAGE,
];

fn runtime(cx: &mut TestAppContext) -> Runtime {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    cx.update(|cx| gpui_tokio::init_from_handle(cx, runtime.handle().clone()));
    runtime
}

struct Surface(Entity<DiagnosticsPage>);
impl Render for Surface {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.0.clone())
    }
}

fn engine_reply() -> Value {
    serde_json::to_value(EngineDiagnostics {
        engine_version: "0.0.0-fixture".into(),
        pid: 4242,
        started_at_ms: 1,
        uptime_ms: 65_000,
        os: "macos".into(),
        arch: "aarch64".into(),
        resources: EngineResources {
            resident_bytes: Some(64 << 20),
            cpu_user_ms: Some(1_500),
            cpu_system_ms: Some(500),
            threads: Some(12),
        },
        read_at_ms: 66_000,
    })
    .unwrap()
}

fn harnesses_reply() -> Value {
    json!([{
        "id":"codex","name":"Codex","supportsSteering":false,"steeringMode":"turn-boundary",
        "reasoningLevels":[],"installed":true,"enabled":true
    }])
}

fn updates_reply() -> Value {
    json!([{
        "harness":"codex","installedVersion":"0.50.0","latestVersion":"0.51.0",
        "updateAvailable":true,"updateCommand":null,"detail":null
    }])
}

fn category(
    id: StorageCategoryId,
    scope: StorageScope,
    state: StorageCategoryState,
    bytes: u64,
    unreadable: u64,
) -> StorageCategory {
    StorageCategory {
        id,
        scope,
        location: String::new(),
        state,
        bytes,
        files: u64::from(bytes > 0),
        directories: 1,
        unreadable,
        first_error: (unreadable > 0).then(|| "permission denied".to_owned()),
        items: Vec::new(),
    }
}

fn storage_reply(categories: Vec<StorageCategory>, complete: bool) -> Value {
    serde_json::to_value(StorageUsage {
        root: "~/fixture-data".into(),
        profile_location: "profiles/local".into(),
        total_bytes: categories.iter().map(|category| category.bytes).sum(),
        categories,
        excluded: vec![StorageExclusion {
            reason: StorageExclusionReason::OtherProfile,
            count: 1,
        }],
        entries: 1_500_000,
        complete,
        stop: (!complete).then_some(StorageScanStop::EntryLimit),
        started_at_ms: 1,
        elapsed_ms: 15_000,
    })
    .unwrap()
}

/// A scan stopped by its budget, with an unreadable folder and a missing one.
fn partial_storage() -> Value {
    storage_reply(
        vec![
            category(
                StorageCategoryId::Logs,
                StorageScope::Device,
                StorageCategoryState::Partial,
                4096,
                1,
            ),
            category(
                StorageCategoryId::Worktrees,
                StorageScope::Shared,
                StorageCategoryState::Partial,
                1 << 30,
                0,
            ),
            category(
                StorageCategoryId::TurnSnapshots,
                StorageScope::Device,
                StorageCategoryState::Missing,
                0,
                0,
            ),
        ],
        false,
    )
}

fn complete_storage(id: StorageCategoryId) -> Value {
    storage_reply(
        vec![category(
            id,
            StorageScope::Device,
            StorageCategoryState::Complete,
            300,
            0,
        )],
        true,
    )
}

fn scene(
    cx: &mut TestAppContext,
    runtime: &Runtime,
    rpc: &Arc<HeldRpc>,
) -> (
    Entity<AppState>,
    Entity<DiagnosticsPage>,
    gpui::VisualTestContext,
) {
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::default());
    });
    let state = cx.new(|_| AppState::new());
    fixture::attach(cx, &state, fixture::handle(runtime, rpc.clone()));
    fixture::drain(cx, runtime);
    let page = cx.new(|cx| DiagnosticsPage::new(state.clone(), cx));
    let rendered = page.clone();
    let window = cx.open_window(gpui::size(px(960.0), px(2600.0)), move |_, _| {
        Surface(rendered)
    });
    let visual = gpui::VisualTestContext::from_window(window.into(), cx);
    (state, page, visual)
}

/// Draw, then report which `selectors` were painted.
fn rendered(
    visual: &mut gpui::VisualTestContext,
    selectors: &[&'static str],
) -> serde_json::Map<String, Value> {
    visual.update(|window, app| window.draw(app).clear());
    selectors
        .iter()
        .copied()
        .map(|selector| {
            (
                selector.to_owned(),
                Value::Bool(visual.debug_bounds(selector).is_some()),
            )
        })
        .collect()
}

fn assert_painted(
    painted: &serde_json::Map<String, Value>,
    present: &[&str],
    absent: &[&str],
    stage: &str,
) {
    for selector in present {
        assert_eq!(painted[*selector], true, "{stage}: {selector} must render");
    }
    for selector in absent {
        assert_eq!(
            painted[*selector], false,
            "{stage}: {selector} must not render"
        );
    }
}

/// S11: every request is a catalogued query or watch (the page and the
/// AppState attachment never send a command), and each owner was read once.
fn assert_read_only(rpc: &HeldRpc) {
    for event in rpc.trace() {
        if event["phase"] != "request" {
            continue;
        }
        let method = event["method"].as_str().unwrap();
        let kind = luna_rpc::operations::spec(method).map(|spec| spec.kind);
        assert_ne!(
            kind,
            Some(luna_rpc::operations::OperationKind::Command),
            "{method} is a command; Diagnostics must only read"
        );
    }
    for method in READS {
        assert_eq!(rpc.calls(method), 1, "{method} is read once per refresh");
    }
}

const SELECTORS: [&str; 18] = [
    "diag-engine-loading",
    "diag-engine-ready",
    "diag-engine-error",
    "diag-providers-loading",
    "diag-providers-ready",
    "diag-providers-error",
    "diag-provider-versions-error",
    "diag-extensions-loading",
    "diag-extensions-ready",
    "diag-extensions-unavailable",
    "diag-storage-loading",
    "diag-storage-ready",
    "diag-storage-error",
    "diag-storage-partial",
    "diag-storage-row-logs",
    "diag-storage-unreadable-logs",
    "diag-storage-row-turnSnapshots",
    "diag-cleanup-note",
];

/// U2 then partial states: with every owner held, each section shows its own
/// loading state; once the quick owners answer they render while storage is
/// still measuring; a budget-stopped scan with an unreadable folder renders
/// as partial (lower bounds, the unreadable count, missing folders listed
/// apart), and the unavailable extension runtime is a row, not a failure.
#[gpui::test]
fn diagnostics_sections_load_independently_and_render_partial_storage(cx: &mut TestAppContext) {
    let runtime = runtime(cx);
    let _entered = runtime.enter();
    let rpc = HeldRpc::new("diagnostics-a", "device-a");
    rpc.reply(methods::GET_ENGINE_DIAGNOSTICS, Ok(engine_reply()), true);
    rpc.reply(methods::LIST_HARNESSES, Ok(harnesses_reply()), true);
    rpc.reply(methods::GET_PROVIDER_UPDATES, Ok(updates_reply()), true);
    rpc.reply(
        methods::LIST_EXTENSION_INSTANCES,
        Err("Extensions are unavailable on this engine".into()),
        true,
    );
    rpc.reply(
        methods::GET_DELETION_CLEANUP_SUMMARY,
        Ok(json!({"pending":0,"failed":0,"firstFailure":null})),
        true,
    );
    rpc.reply(methods::GET_STORAGE_USAGE, Ok(partial_storage()), true);
    let (_state, _page, mut visual) = scene(cx, &runtime, &rpc);
    fixture::pump_until(cx, &runtime, |_| {
        READS.iter().all(|method| rpc.calls(method) == 1)
    });

    let loading = rendered(&mut visual, &SELECTORS);
    assert_painted(
        &loading,
        &[
            "diag-engine-loading",
            "diag-providers-loading",
            "diag-extensions-loading",
            "diag-storage-loading",
        ],
        &[
            "diag-engine-ready",
            "diag-storage-ready",
            "diag-cleanup-note",
        ],
        "all held",
    );

    let quick = [
        methods::GET_ENGINE_DIAGNOSTICS,
        methods::LIST_HARNESSES,
        methods::GET_PROVIDER_UPDATES,
        methods::LIST_EXTENSION_INSTANCES,
        methods::GET_DELETION_CLEANUP_SUMMARY,
    ];
    for method in quick {
        rpc.release(method);
    }
    fixture::pump_until(cx, &runtime, |_| {
        quick.iter().all(|method| rpc.settled(method) == 1)
    });
    fixture::drain(cx, &runtime);
    let measuring = rendered(&mut visual, &SELECTORS);
    assert_painted(
        &measuring,
        &[
            "diag-engine-ready",
            "diag-providers-ready",
            "diag-extensions-unavailable",
            "diag-storage-loading",
        ],
        &[
            "diag-engine-loading",
            "diag-providers-loading",
            "diag-provider-versions-error",
            "diag-extensions-loading",
            "diag-storage-ready",
        ],
        "storage still measuring",
    );

    rpc.release(methods::GET_STORAGE_USAGE);
    fixture::pump_until(cx, &runtime, |_| {
        rpc.settled(methods::GET_STORAGE_USAGE) == 1
    });
    fixture::drain(cx, &runtime);
    let partial = rendered(&mut visual, &SELECTORS);
    assert_painted(
        &partial,
        &[
            "diag-storage-ready",
            "diag-storage-partial",
            "diag-storage-row-logs",
            "diag-storage-unreadable-logs",
            "diag-cleanup-note",
        ],
        &["diag-storage-loading", "diag-storage-row-turnSnapshots"],
        "partial storage",
    );
    assert_read_only(&rpc);
    fixture::write_trace(
        "aw09-diagnostics-loading-partial",
        &json!({
            "source": file!(),
            "loading": loading, "measuring": measuring, "partial": partial,
            "rpc": rpc.trace(),
        }),
    );
}

/// U1: failing owners show their own error inside their own section while
/// every other section still renders its data, and an owner's reported
/// failure (deletion cleanup) is listed under Problems.
#[gpui::test]
fn diagnostics_sections_fail_independently(cx: &mut TestAppContext) {
    let runtime = runtime(cx);
    let _entered = runtime.enter();
    let rpc = HeldRpc::new("diagnostics-errors", "device-a");
    rpc.reply(
        methods::GET_ENGINE_DIAGNOSTICS,
        Err("engine diagnostics refused".into()),
        false,
    );
    rpc.reply(methods::LIST_HARNESSES, Ok(harnesses_reply()), false);
    rpc.reply(
        methods::GET_PROVIDER_UPDATES,
        Err("version check timed out".into()),
        false,
    );
    rpc.reply(
        methods::LIST_EXTENSION_INSTANCES,
        Ok(json!({"instances":[],"quarantined":[]})),
        false,
    );
    rpc.reply(
        methods::GET_DELETION_CLEANUP_SUMMARY,
        Ok(json!({"pending":2,"failed":1,"firstFailure":"worktree is still in use"})),
        false,
    );
    rpc.reply(
        methods::GET_STORAGE_USAGE,
        Err("storage scan stopped: task panicked".into()),
        false,
    );
    let (_state, _page, mut visual) = scene(cx, &runtime, &rpc);
    fixture::pump_until(cx, &runtime, |_| {
        READS.iter().all(|method| rpc.settled(method) == 1)
    });
    fixture::drain(cx, &runtime);
    let mut selectors = SELECTORS.to_vec();
    selectors.extend(["diag-problems", "diag-problems-none"]);
    let painted = rendered(&mut visual, &selectors);
    assert_painted(
        &painted,
        &[
            "diag-engine-error",
            "diag-providers-ready",
            "diag-provider-versions-error",
            "diag-extensions-ready",
            "diag-storage-error",
            "diag-problems",
        ],
        &[
            "diag-engine-ready",
            "diag-engine-loading",
            "diag-storage-ready",
            "diag-storage-loading",
            "diag-problems-none",
            "diag-cleanup-note",
        ],
        "independent failures",
    );
    assert_read_only(&rpc);
    fixture::write_trace(
        "aw09-diagnostics-independent-failures",
        &json!({"source": file!(), "painted": painted, "rpc": rpc.trace()}),
    );
}

/// Every owner but storage answers at once.
fn reply_quick_owners(rpc: &HeldRpc) {
    rpc.reply(methods::GET_ENGINE_DIAGNOSTICS, Ok(engine_reply()), false);
    rpc.reply(methods::LIST_HARNESSES, Ok(harnesses_reply()), false);
    rpc.reply(methods::GET_PROVIDER_UPDATES, Ok(updates_reply()), false);
    rpc.reply(
        methods::LIST_EXTENSION_INSTANCES,
        Ok(json!({"instances":[],"quarantined":[]})),
        false,
    );
    rpc.reply(
        methods::GET_DELETION_CLEANUP_SUMMARY,
        Ok(json!({"pending":0,"failed":0})),
        false,
    );
}

/// U3: when the engine is replaced, the page reads the new engine on its
/// own, and a storage walk still running on the previous engine never lands.
#[gpui::test]
fn late_storage_reply_from_previous_engine_is_dropped(cx: &mut TestAppContext) {
    let runtime = runtime(cx);
    let _entered = runtime.enter();
    let rpc_a = HeldRpc::new("diagnostics-old", "device-a");
    reply_quick_owners(&rpc_a);
    rpc_a.reply(
        methods::GET_STORAGE_USAGE,
        Ok(complete_storage(StorageCategoryId::ModelCache)),
        true,
    );
    let (state, _page, mut visual) = scene(cx, &runtime, &rpc_a);
    fixture::pump_until(cx, &runtime, |_| {
        rpc_a.calls(methods::GET_STORAGE_USAGE) == 1
    });

    let rpc_b = HeldRpc::new("diagnostics-new", "device-b");
    reply_quick_owners(&rpc_b);
    rpc_b.reply(
        methods::GET_STORAGE_USAGE,
        Ok(complete_storage(StorageCategoryId::Logs)),
        false,
    );
    // The real attachment boundary; the page re-reads the new engine itself.
    fixture::attach(cx, &state, fixture::handle(&runtime, rpc_b.clone()));
    fixture::pump_until(cx, &runtime, |_| {
        rpc_b.settled(methods::GET_STORAGE_USAGE) == 1
    });
    rpc_a.release(methods::GET_STORAGE_USAGE);
    fixture::pump_until(cx, &runtime, |_| {
        rpc_a.settled(methods::GET_STORAGE_USAGE) == 1
    });
    fixture::drain(cx, &runtime);
    let selectors = [
        "diag-storage-ready",
        "diag-storage-row-logs",
        "diag-storage-row-modelCache",
    ];
    let painted = rendered(&mut visual, &selectors);
    assert_painted(
        &painted,
        &["diag-storage-ready", "diag-storage-row-logs"],
        &["diag-storage-row-modelCache"],
        "after replacement",
    );
    assert_read_only(&rpc_b);
    fixture::write_trace(
        "aw09-diagnostics-engine-replacement",
        &json!({
            "source": file!(), "painted": painted,
            "oldEngine": rpc_a.trace(), "newEngine": rpc_b.trace(),
        }),
    );
}
