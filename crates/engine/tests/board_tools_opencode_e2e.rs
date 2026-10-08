//! A4 board tools E2E on OpenCode 2: a real engine, the OpenCode adapter and
//! its private per-run `luna` MCP endpoint (attached through the inline
//! `OPENCODE_CONFIG_CONTENT` layer, verified by `/api/mcp` readback before
//! any `luna_<tool>` is allowed, instructions in the session's `luna` entry),
//! with the deterministic fake server
//! (`harness/tests/fixtures/fake-opencode-board.py`) as the MCP client: it
//! announces each call on the bus (`session.tool.input.started` /
//! `session.tool.called`) and then calls the endpoint with
//! `_meta["ai.opencode/sessionID"]`, as OpenCode 2.0.22 does. Same scenario
//! and failure cases as `board_tools_e2e.rs`; evidence in
//! `target/qa/board/tools/opencode.json`.

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

const OPENCODE: Provider = Provider {
    name: "opencode",
    harness: HarnessId::Opencode,
    delivery: RuntimeDelivery::OpenCodeMcpTools,
    plan_prefix: ".luna-board-plan-",
    results_prefix: ".luna-board-results-",
    turn_prefix: ".luna-board-turn-",
    plan_prompt: "scenario:board-plan:",
    instructions_key: "instructions",
    compaction,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn opencode_board_tools_follow_role_authority_and_context_through_luna_mcp() {
    let harness =
        Arc::new(OpencodeHarness::new().with_executable(fixture_path("fake-opencode-board.py")));
    let fixture = boot(OPENCODE, harness);
    board_tools_scenario(&fixture).await;
    fixture.core.shutdown().await;
}
