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

/// LUN-13/LUN-12: actual upload -> durable user command -> Claude's private MCP
/// -> ticket -> separate Git worktree -> provider image bytes. Foreign uploads,
/// prose-only paths and stale image edits must fail at this same boundary.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ticket_fix_images_and_delete_cross_real_claude_mcp() {
    use base64::Engine as _;
    use board_tools_support::{Evidence, reply};
    use std::time::Duration;
    // Re-exec only this scenario with a private worktree root. Environment
    // mutation in a parallel Rust test would affect unrelated engine fixtures.
    if std::env::var_os("LUNA_BOARD_IMAGE_FIXTURE_CHILD").is_none() {
        let worktrees = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "ticket_fix_images_and_delete_cross_real_claude_mcp",
                "--nocapture",
            ])
            .env("LUNA_BOARD_IMAGE_FIXTURE_CHILD", "1")
            .env("LUNA_WORKTREES_DIR", worktrees.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "isolated image scenario failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let harness =
        Arc::new(ClaudeHarness::new().with_executable(fixture_path("fake-claude-luna.py")));
    let f = boot(CLAUDE, harness);
    for args in [
        vec!["init", "-b", "main"],
        vec![
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.test",
            "commit",
            "--allow-empty",
            "-m",
            "Initial",
        ],
    ] {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(&f.project_a)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=";
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(png)
        .unwrap();
    let mut paths = Vec::new();
    for id in ["user-image", "foreign-image"] {
        f.rpc_ok("UploadChunk", json!({"uploadId":id,"seq":0,"data":png}))
            .await;
        paths.push(
            f.rpc_ok(
                "UploadCommit",
                json!({"uploadId":id,"fileName":"screenshot.png"}),
            )
            .await["path"]
                .as_str()
                .unwrap()
                .to_owned(),
        );
    }
    let workspace_image = f.project_a.join("arbitrary.png");
    std::fs::write(&workspace_image, &bytes).unwrap();
    f.run_plan_inputs(
        &f.other,
        "foreign-upload",
        &[json!({"tool":"luna_status","arguments":{}})],
        Some((vec![paths[1].clone()], String::new())),
    )
    .await;
    let plan = vec![
        json!({"tool":"board_create","arguments":{"requestId":"image-ticket","title":"Screenshot regression","attachments":[paths[0]]}}),
        json!({"tool":"board_create","arguments":{"requestId":"foreign-image-ticket","title":"Forbidden foreign image","attachments":[paths[1]]}}),
        json!({"tool":"board_create","arguments":{"requestId":"arbitrary-image-ticket","title":"Forbidden arbitrary path","attachments":[workspace_image]}}),
        json!({"tool":"board_update","arguments":{"requestId":"bad-image-update","workItemId":"$ref:0.workItemId","expectedRevision":1,"changes":{"attachments":[paths[1]]}}}),
        json!({"tool":"board_update","arguments":{"requestId":"clear-image","workItemId":"$ref:0.workItemId","expectedRevision":1,"changes":{"attachments":[]}}}),
        json!({"tool":"board_update","arguments":{"requestId":"restore-image","workItemId":"$ref:0.workItemId","expectedRevision":2,"changes":{"attachments":[paths[0]]}}}),
        json!({"tool":"board_update","arguments":{"requestId":"stale-image","workItemId":"$ref:0.workItemId","expectedRevision":1,"changes":{"attachments":[]}}}),
        json!({"tool":"board_create","arguments":{"requestId":"delete-candidate","title":"Disposable delete proof"}}),
        json!({"tool":"board_control","arguments":{"requestId":"delete-candidate-action","workItemId":"$ref:7.workItemId","expectedRevision":1,"action":"delete"}}),
        json!({"tool":"board_control","arguments":{"requestId":"delete-candidate-action","workItemId":"$ref:7.workItemId","expectedRevision":1,"action":"delete"}}),
        json!({"tool":"board_start","arguments":{"requestId":"launch-image","workItemId":"$ref:0.workItemId","destination":"newMain","execution":{"checkout":{"kind":"newWorktree","branch":"luna/image-fixture"}}}}),
        json!({"tool":"board_create","arguments":{"requestId":"side-image-ticket","title":"Side screenshot regression","attachments":[paths[0]]}}),
        json!({"tool":"board_start","arguments":{"requestId":"launch-side-image","workItemId":"$ref:11.workItemId","destination":"sideChat","execution":{"checkout":{"kind":"newWorktree","branch":"luna/image-side-fixture"}}}}),
    ];
    let results = f.run_plan_inputs(&f.main, "ticket-images", &plan, Some((vec![paths[0].clone()], format!("Only the first image is attached. This other path is merely mentioned in text: {}", paths[1])))).await;
    let artifact_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/qa/board/tools/image-flow");
    std::fs::create_dir_all(&artifact_dir).unwrap();
    std::fs::write(
        artifact_dir.join("private-mcp-results.json"),
        serde_json::to_vec_pretty(&results).unwrap(),
    )
    .unwrap();
    std::fs::write(artifact_dir.join("source.png"), &bytes).unwrap();
    let mut evidence = Evidence::new("claude-ticket-images");
    evidence.artifact("private-mcp-results", &results);
    let image_path = std::fs::canonicalize(&paths[0]).unwrap();
    let created = reply(&results, 0).ok().unwrap().clone();
    assert_eq!(created["item"]["attachments"], json!([image_path]));
    for index in [1, 2, 3] {
        reply(&results, index)
            .board_error(&["invalidInput"])
            .unwrap();
    }
    assert_eq!(reply(&results, 4).ok().unwrap()["revision"], 2);
    assert_eq!(
        reply(&results, 5).ok().unwrap()["item"]["attachments"],
        json!([image_path])
    );
    reply(&results, 6).board_error(&["staleRevision"]).unwrap();
    assert_eq!(reply(&results, 8).ok().unwrap()["item"]["deleted"], true);
    assert_eq!(
        reply(&results, 8).ok().unwrap(),
        reply(&results, 9).ok().unwrap()
    );
    reply(&results, 10).ok().unwrap();
    let id = created["workItemId"].as_str().unwrap();
    let worker = f
        .working_chat(id, Duration::from_secs(20))
        .await
        .expect("working chat created");
    let cwd = f
        .core
        .workspace
        .chat(&worker)
        .unwrap()
        .unwrap()
        .cwd
        .unwrap();
    assert_ne!(std::path::Path::new(&cwd), f.project_a);
    let capture = std::path::Path::new(&cwd).join(format!(".luna-claude-turn-{worker}.input.json"));
    let received: Value = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Ok(text) = std::fs::read_to_string(&capture) {
                break serde_json::from_str(&text).unwrap();
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    let images: Vec<_> = received
        .as_array()
        .unwrap()
        .iter()
        .filter(|block| block["type"] == "image")
        .collect();
    std::fs::write(
        artifact_dir.join("worker-input.json"),
        serde_json::to_vec_pretty(&received).unwrap(),
    )
    .unwrap();
    assert_eq!(images.len(), 1, "{received}");
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(images[0]["source"]["data"].as_str().unwrap())
            .unwrap(),
        bytes
    );
    f.wait_idle(&worker).await;
    let detail = f.rpc_ok("BoardRead", json!({"workItemId":id})).await;
    std::fs::write(
        artifact_dir.join("ticket-detail.json"),
        serde_json::to_vec_pretty(&detail).unwrap(),
    )
    .unwrap();
    let rejected = f.rpc_call("BoardDelete", json!({"requestId":"delete-ready","workItemId":id,"expectedRevision":detail["view"]["item"]["revision"]})).await;
    assert!(
        rejected.is_err(),
        "Ready tickets cannot be deleted while admitted"
    );
    evidence.artifact("worktree-provider-input", &received);
    evidence.artifact("ticket-detail", &detail);
    evidence.check(
        "uploaded bytes arrive in the separate working checkout",
        "one exact PNG",
        Ok(json!({"sha256":board_tools_support::sha256(&bytes),"cwd":cwd,"workingChat":worker})),
    );
    reply(&results, 12).ok().unwrap();
    let side_id = reply(&results, 11).ok().unwrap()["workItemId"]
        .as_str()
        .unwrap()
        .to_owned();
    let side_worker = f
        .working_chat(&side_id, Duration::from_secs(20))
        .await
        .unwrap();
    let side = f.core.workspace.chat(&side_worker).unwrap().unwrap();
    let side_cwd = side.cwd.unwrap();
    assert_ne!(std::path::Path::new(&side_cwd), f.project_a);
    let side_capture =
        std::path::Path::new(&side_cwd).join(format!(".luna-claude-turn-{side_worker}.input.json"));
    let side_input: Value = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Ok(text) = std::fs::read_to_string(&side_capture) {
                break serde_json::from_str(&text).unwrap();
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    let side_image = side_input
        .as_array()
        .unwrap()
        .iter()
        .find(|block| block["type"] == "image")
        .expect("side worker received image");
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(side_image["source"]["data"].as_str().unwrap())
            .unwrap(),
        bytes
    );
    std::fs::write(
        artifact_dir.join("side-worker-input.json"),
        serde_json::to_vec_pretty(&side_input).unwrap(),
    )
    .unwrap();
    evidence.check("side worker preserves its chosen worktree and attachment", "exact PNG in separate side checkout", Ok(json!({"cwd":side_cwd,"workingChat":side_worker,"sha256":board_tools_support::sha256(&bytes)})));
    f.wait_idle(&side_worker).await;
    evidence.write();
    f.core.shutdown().await;
}
