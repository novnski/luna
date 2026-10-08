//! A4 board tools E2E on Claude Code: a real engine, the Claude adapter and
//! its shared per-process `luna` MCP endpoint, with the deterministic fake
//! CLI (`harness/tests/fixtures/fake-claude-luna.py`) as the MCP client, as
//! the real 2.1.286 CLI is. The same scenario runs on Codex
//! (`board_tools_codex_e2e.rs`) and OpenCode (`board_tools_opencode_e2e.rs`)
//! from `board_tools_support/`.
//!
//! Failure cases, written before the tools:
//!
//! 1. Board tools are advertised without a verified delivery path, or missing
//!    on a provider: not listed on the initial launch, or lost on a resumed
//!    run (new process, new endpoint and token).
//! 2. A role sees tools or modes it may not use: reviewer-only
//!    `review_submit` or authoring-only `board_draft_checkpoint` listed for an
//!    ordinary chat; a side chat's `create_side_chat` offers `withContext`;
//!    a side chat's destination offers its own side chat.
//! 3. A hidden tool or mode still works when called anyway (the provider
//!    fence or the engine must refuse it regardless of the schema).
//! 4. Authority comes from model input: a model-supplied caller, origin,
//!    parent, destination or chat id acts as another chat instead of being
//!    rejected; a side chat creates a nested side chat, a context fork, or a
//!    child of itself; the side caller changes parent or becomes a main chat.
//! 5. Cross-project access: reading, listing, creating or editing another
//!    project's tickets succeeds (or the foreign ticket changes).
//! 6. Saving creates a chat or starts work; a retried create with the same
//!    request key (also after resume) creates a second ticket or returns a
//!    different receipt; the same key with a different payload is accepted;
//!    two chats reusing a key collide.
//! 7. A stale edit overwrites a newer field instead of returning
//!    `staleRevision` with the current ticket.
//! 8. A tool claims an operation occurred when its slice is not available
//!    (must answer `notImplemented` and change nothing), or a lifecycle call
//!    is attributed to the wrong actor; Done or acceptance is settable by an
//!    agent.
//! 9. The canonical board/role context is missing or wrong: absent from the
//!    initial launch, a resumed run, a side chat's first turn or after
//!    compaction; inferred from titles instead of lineage; the scopes
//!    `board.read`/`board.write` are not granted.
//! 10. Board tools create chats they do not report (orphans), or a started
//!     worker is not a parent-locked side chat of the requesting main chat.
//! 11. A tool result is unbounded or not marked as untrusted content.
//! 12. Approval cards appear for routine board operations the user asked for.
//!
//! Evidence: `target/qa/board/tools/claude.json` (+ `manifest.json`).

#![cfg(unix)]

mod board_tools_support;

use std::sync::Arc;

use board_tools_support::{Provider, board_tools_scenario, boot, fixture_path};
use luna_harness::ClaudeHarness;
use luna_proto::{HarnessId, RuntimeDelivery};
use serde_json::{Value, json};

fn compaction() -> Value {
    json!({"frame": {"type": "system", "subtype": "compact_boundary",
        "uuid": "board-compact-boundary", "compact_metadata": {
            "trigger": "auto", "pre_tokens": 100000, "post_tokens": 12000, "duration_ms": 1000}}})
}

const CLAUDE: Provider = Provider {
    name: "claude",
    harness: HarnessId::ClaudeCode,
    delivery: RuntimeDelivery::ClaudeMcpTools,
    plan_prefix: ".luna-claude-plan-",
    results_prefix: ".luna-claude-results-",
    turn_prefix: ".luna-claude-turn-",
    plan_prompt: "scenario:luna-plan:",
    instructions_key: "systemPrompt",
    compaction,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn claude_board_tools_follow_role_authority_and_context_through_luna_mcp() {
    let harness =
        Arc::new(ClaudeHarness::new().with_executable(fixture_path("fake-claude-luna.py")));
    let fixture = boot(CLAUDE, harness);
    board_tools_scenario(&fixture).await;
    fixture.core.shutdown().await;
}
