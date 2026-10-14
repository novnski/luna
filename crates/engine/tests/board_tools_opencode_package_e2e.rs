//! A4 board tools on OpenCode 2 through its first-party provider package
//! (CORE-11 / E11): the retained OpenCode board scenario of
//! `board_tools_opencode_e2e.rs`, unchanged, with the compiled adapter that
//! `boot` registers replaced by `luna.provider/opencode`. The adapter, its
//! private per-run `luna` MCP endpoint and its `opencode serve` now live in
//! the turn's package worker; each tool call crosses the worker as the
//! adapter-matched context and comes back as the engine executor's result.
//! Same plans, failure cases and deterministic server
//! (`harness/tests/fixtures/fake-opencode-board.py`); evidence in
//! `target/qa/board/tools/opencode-package.json`.

#![cfg(unix)]

mod board_tools_support;

use std::sync::Arc;

use board_tools_support::{Provider, board_tools_scenario, boot, fixture_path};
use luna_harness::OpencodeHarness;
use luna_proto::{HarnessId, RuntimeDelivery};
use serde_json::{Value, json};

/// A mid-turn automatic compaction on the session bus.
fn compaction() -> Value {
    json!({"frames": [
        {"type": "session.compaction.started", "data": {"sessionID": "$SESSION", "reason": "auto"}},
        {"type": "session.compaction.ended", "data": {"sessionID": "$SESSION", "reason": "auto"}},
    ]})
}

const OPENCODE_PACKAGE: Provider = Provider {
    name: "opencode-package",
    harness: HarnessId::Opencode,
    delivery: RuntimeDelivery::OpenCodeMcpTools,
    plan_prefix: ".luna-board-plan-",
    results_prefix: ".luna-board-results-",
    turn_prefix: ".luna-board-turn-",
    plan_prompt: "scenario:board-plan:",
    instructions_key: "instructions",
    compaction,
};

/// Regressions caught: LUNA tools stop reaching OpenCode once its adapter
/// runs in the package worker (no attachment, a lost or forged call
/// context, a result that never returns), or the board scenario changes
/// behaviour on the package path.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn opencode_package_board_tools_follow_role_authority_and_context_through_luna_mcp() {
    let fake = fixture_path("fake-opencode-board.py");
    // SAFETY: set before this single-test binary starts the engine or any
    // thread that reads the environment. The worker inherits both.
    unsafe {
        std::env::set_var(
            "LUNA_OPENCODE_WORKER",
            env!("CARGO_BIN_EXE_luna-opencode-worker"),
        );
        std::env::set_var("OPENCODE_EXECUTABLE", &fake);
    }
    let harness = Arc::new(OpencodeHarness::new().with_executable(fake));
    let fixture = boot(OPENCODE_PACKAGE, harness);
    let providers = fixture.rpc_ok("ListExtensionProviders", json!({})).await;
    assert_eq!(
        providers["firstParty"][0]["replacesBuiltin"], true,
        "the compiled adapter still serves OpenCode: {providers}"
    );
    board_tools_scenario(&fixture).await;
    fixture.core.shutdown().await;
}
