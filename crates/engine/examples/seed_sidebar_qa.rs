//! Seed the existing production-shell fixture pattern into only Sidebar QA.
//! Run once, before its first app launch, after a separate Cargo/native grant.
//! No provider, Edge, IPC server, account home, or primary app is contacted.
use std::{path::PathBuf, sync::Arc};

use anyhow::{Result, ensure};
use luna_engine::{AgentAccountsConfig, EngineCore, EngineProfile, HarnessRegistry};
use luna_proto::HarnessId;
use serde_json::json;

#[tokio::main]
async fn main() -> Result<()> {
    let worktree = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let root = worktree.join("target/luna-qa-data/sidebar");
    ensure!(
        !root.join("profiles/local/docs.sqlite3").exists(),
        "Sidebar QA already has a profile. Preserve it; seed only before the first app launch."
    );
    // Do not race a running QA app, and never stop one to seed a fixture.
    let _port = std::net::TcpListener::bind("127.0.0.1:59787")?;
    std::fs::create_dir_all(&root)?;
    let synthetic = root.join("synthetic-agent-fixture");
    std::fs::create_dir_all(&synthetic)?;
    let core = EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        EngineProfile::local(&root)?,
        Arc::new(HarnessRegistry::new()),
        HarnessId::Mock,
        None,
        AgentAccountsConfig {
            data_dir: synthetic.join("provider-state"),
            claude_config_dir: synthetic.join("claude"),
            claude_config_file: synthetic.join("claude.json"),
            codex_home: synthetic.join("codex"),
        },
    )?;
    ensure!(
        core.workspace.read_chats()?.is_empty(),
        "Sidebar QA already contains work. Preserve it; this seed only runs on an empty QA profile."
    );
    let parent = "87878787-8787-4787-8787-878787878787";
    let child = format!("{parent}--sub--synthetic-available");
    core.workspace.create_chat(
        parent,
        None,
        Some(&core.device_id),
        Some(serde_json::from_value(json!({
            "harness":"mock", "model":"synthetic-fixture", "reasoning":null,
            "sandbox":"read-only", "permissionMode":"ask", "effectivePermissionMode":"ask"
        }))?),
        Some(synthetic.to_string_lossy().into()),
    )?;
    core.workspace
        .rename_chat(parent, "Agents layout fixture · synthetic")?;
    let entry = |id: &str, parts| -> Result<luna_doc::SessionMessageEntry> {
        Ok(serde_json::from_value(json!({
            "id":id, "role":"assistant", "parts":parts,
            "createdAt":chrono::Utc::now().timestamp_millis(),
            "deviceId":core.device_id, "status":"complete"
        }))?)
    };
    core.doc_host.open(parent)?.doc().push_message(&entry("synthetic-agents", json!([
        {"kind":"text","id":"fixture-note","text":"Synthetic layout and accessibility fixture. These are not real provider agents."},
        {"kind":"tool","id":"fixture-available","call":{"kind":"unknown","name":"Agent: Fixture with transcript","input":{"model":"synthetic"}},
         "resolved":true,"subagentRef":child,"subagentStatus":"done","subagentTail":"Synthetic result saved for UI QA"},
        {"kind":"tool","id":"fixture-missing","call":{"kind":"unknown","name":"Agent: Fixture without reference","input":null},
         "resolved":true,"subagentStatus":"unobserved","subagentTail":"Transcript reference intentionally absent"}
    ]))?)?;
    core.doc_host.open(&child)?.doc().push_message(&entry(
        "synthetic-child-result",
        json!([
            {"kind":"text","id":"fixture-result","text":"SYNTHETIC_AGENT_FIXTURE_RESULT"}
        ]),
    )?)?;
    ensure!(
        core.shutdown().await,
        "Fixture shutdown did not finish cleanly"
    );
    let output = worktree.join("target/qa/sidebar/agents-fixture-seed.json");
    std::fs::create_dir_all(output.parent().unwrap())?;
    std::fs::write(
        &output,
        serde_json::to_vec_pretty(&json!({
            "synthetic":true, "realProviderCalls":0, "root":root,
            "parentId":parent,"availableChildDocId":child,
            "expectedCards":[{"id":"fixture-available","role":"button","opensTranscript":true},
                {"id":"fixture-missing","role":"group","focusable":false,"reason":"Transcript unavailable"}]
        }))?,
    )?;
    println!("SIDEBAR_SYNTHETIC_FIXTURE_SEEDED {}", output.display());
    Ok(())
}
