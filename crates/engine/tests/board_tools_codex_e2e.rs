//! A4 board tools E2E on Codex: a real engine, the Codex adapter and its
//! private per-process `luna` MCP endpoint (attached to every app-server
//! spawn, fresh and resumed), with the deterministic fake app-server
//! (`harness/tests/fixtures/fake-codex-board.py`) as the MCP client sending
//! Codex's exact thread/turn/call `_meta`. Same scenario and failure cases as
//! `board_tools_e2e.rs`; evidence in `target/qa/board/tools/codex.json`.

#![cfg(unix)]

mod board_tools_support;

use std::sync::Arc;

use board_tools_support::{Provider, board_tools_scenario, boot, fixture_path};
use luna_harness::CodexHarness;
use luna_proto::{HarnessId, RuntimeDelivery};
use serde_json::{Value, json};

/// A mid-turn provider compaction: Codex reports it as a contextCompaction
/// item of the active turn.
fn compaction() -> Value {
    let item = json!({"id": "board-compaction", "type": "contextCompaction"});
    json!({"frames": [
        {"method": "item/started", "params": {"threadId": "$THREAD", "turnId": "$TURN", "item": item}},
        {"method": "item/completed", "params": {"threadId": "$THREAD", "turnId": "$TURN", "item": item}},
    ]})
}

const CODEX: Provider = Provider {
    name: "codex",
    harness: HarnessId::Codex,
    delivery: RuntimeDelivery::CodexMcpTools,
    plan_prefix: ".luna-board-plan-",
    results_prefix: ".luna-board-results-",
    turn_prefix: ".luna-board-turn-",
    plan_prompt: "scenario:board-plan:",
    instructions_key: "instructions",
    compaction,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn codex_board_tools_follow_role_authority_and_context_through_luna_mcp() {
    let harness =
        Arc::new(CodexHarness::new().with_executable(fixture_path("fake-codex-board.py")));
    let fixture = boot(CODEX, harness);
    board_tools_scenario(&fixture).await;
    fixture.core.shutdown().await;
}
