//! Live probe (OC-14): discovery through the Claude-path hooks against the
//! REAL OpenCode 2 server, in a throwaway project with one project command
//! and one project skill. Run it under an isolated HOME/XDG (the evidence
//! runner `run-probe.sh`).
//!
//!     cargo run -p luna-harness --example opencode_discovery_probe -- <model>
//!
//! Steps: `commands_in` for the project (commands AND skills, skills tagged
//! `Skill`); `native_extensions` for the project; `run_title` (one tool-less
//! generation in a throwaway session); a turn whose model adds a second
//! project command file, which must show in the next run's
//! `AvailableCommands` and the next `commands_in` without a restart (and in
//! the same run when OpenCode announces it before the turn ends).

use futures::StreamExt;
use luna_harness::{CancellationToken, CommandScope, Harness, OpencodeHarness, RunControls};
use luna_proto::{AgentEvent, PermissionMode, RunRequest, SandboxLevel, SlashCommand};
use tokio::sync::mpsc;

fn controls() -> (RunControls, mpsc::Sender<luna_harness::SteerMessage>) {
    let (steer_tx, steering) = mpsc::channel(8);
    (
        RunControls {
            request_input: Box::new(|_| {
                let (tx, rx) = tokio::sync::oneshot::channel();
                let _ = tx.send(Vec::new());
                rx
            }),
            steering,
            interrupt: CancellationToken::new(),
            invoke_luna_tool: None,
        },
        steer_tx,
    )
}

fn request(model: &str, cwd: &str, prompt: String) -> RunRequest {
    RunRequest {
        review: None,
        goal: None,
        input_items: Vec::new(),
        prompt,
        harness: None,
        provider_account_id: None,
        model: Some(model.into()),
        reasoning: None,
        model_options: serde_json::Map::new(),
        cwd: cwd.into(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: true,
        permission_mode: PermissionMode::Ask,
        parent_permission_ceiling: None,
        environment_permission_ceiling: None,
        permission_policy: None,
        attachments: Vec::new(),
        resume: None,
        worktree: None,
    }
}

fn show(label: &str, commands: &[SlashCommand]) {
    let rows = commands
        .iter()
        .map(|command| format!("{}:{:?}", command.name, command.kind))
        .collect::<Vec<_>>()
        .join(", ");
    eprintln!("{label} [{rows}]");
}

#[tokio::main]
async fn main() {
    let model = std::env::args().nth(1).expect("usage: <provider/model>");
    let project = tempfile::tempdir().expect("throwaway project");
    let root = project.path();
    std::fs::create_dir_all(root.join(".opencode/command")).unwrap();
    std::fs::create_dir_all(root.join(".opencode/skill/probe-skill")).unwrap();
    std::fs::write(
        root.join(".opencode/command/probe-hello.md"),
        "---\ndescription: Probe project command\n---\nReply with exactly: HELLO-PROBE\n",
    )
    .unwrap();
    std::fs::write(
        root.join(".opencode/skill/probe-skill/SKILL.md"),
        "---\nname: probe-skill\ndescription: Probe project skill\n---\nWhen used, reply SKILL-PROBE.\n",
    )
    .unwrap();
    let cwd = root.to_str().expect("UTF-8 path").to_owned();
    let harness = OpencodeHarness::new();
    let scope = CommandScope {
        cwd: Some(root.to_path_buf()),
        ..CommandScope::default()
    };
    let mut failures = Vec::new();

    // 1. commands_in: project command + project skill, skills tagged.
    let first = harness.commands_in(&scope).await.expect("commands_in");
    show("COMMANDS_IN", &first);
    let has = |commands: &[SlashCommand], name: &str, skill: bool| {
        commands
            .iter()
            .any(|command| command.name == name && command.kind.is_command() != skill)
    };
    if !has(&first, "probe-hello", false) || !has(&first, "probe-skill", true) {
        failures.push("commands_in lacks the project command or skill".to_owned());
    }

    // 2. native_extensions for the same launch location.
    match harness
        .native_extensions(Some(root), PermissionMode::Ask, false)
        .await
    {
        Ok(view) => {
            eprintln!(
                "EXTENSIONS verified={} error={:?} policy={:?}",
                view.native_verified, view.native_error, view.launch_policy
            );
            for source in &view.sources {
                eprintln!(
                    "  SOURCE {:?} {} present={} loaded={}",
                    source.scope, source.display_path, source.present, source.loaded
                );
            }
            for command in &view.commands {
                eprintln!(
                    "  ENTRY {:?} {} scope={:?}",
                    command.kind, command.name, command.scope
                );
            }
            if !view.native_verified {
                failures.push("native_extensions not verified".into());
            }
        }
        Err(error) => failures.push(format!("native_extensions: {error}")),
    }

    // 3. run_title: one tool-less generation in a throwaway session.
    let title_prompt = format!(
        "{}\n\nSession request (JSON string):\n{}",
        luna_harness::TITLE_INSTRUCTIONS,
        serde_json::to_string("Please add retry with exponential backoff to the upload client")
            .unwrap()
    );
    let title_dir = tempfile::tempdir().unwrap();
    let (title_controls, _title_steer) = controls();
    match harness
        .run_title(
            request(&model, title_dir.path().to_str().unwrap(), title_prompt),
            title_controls,
        )
        .await
    {
        Ok(stream) => {
            let events = stream.collect::<Vec<_>>().await;
            let mut text = String::new();
            let mut done = None;
            for event in events {
                match event {
                    Ok(AgentEvent::TextDelta { text: delta }) => text.push_str(&delta),
                    Ok(AgentEvent::Done { status, .. }) => done = Some(status),
                    Ok(AgentEvent::ToolCall { .. }) => failures.push("title used a tool".into()),
                    Ok(other) => eprintln!("TITLE EV {other:?}"),
                    Err(error) => failures.push(format!("title stream: {error}")),
                }
            }
            eprintln!("TITLE {:?} done={done:?}", text.trim());
            if text.trim().is_empty() || done != Some(luna_proto::DoneStatus::Completed) {
                failures.push("title empty or not completed".into());
            }
        }
        Err(error) => failures.push(format!("run_title: {error}")),
    }

    // 4. A run that adds a project command: refreshed without a restart.
    let (run_controls, _steer) = controls();
    let mut stream = harness
        .run(
            request(
                &model,
                &cwd,
                "Create the file .opencode/command/probe-added.md in the current directory \
                 with exactly this content (three lines):\n---\ndescription: Added by the \
                 probe\n---\nThen wait for nothing else and reply with only: CREATED"
                    .into(),
            ),
            run_controls,
        )
        .await
        .expect("run starts");
    let mut announced = Vec::new();
    while let Some(event) = tokio::time::timeout(std::time::Duration::from_secs(240), stream.next())
        .await
        .ok()
        .flatten()
    {
        match event {
            Ok(AgentEvent::AvailableCommands { commands }) => {
                show("AVAILABLE", &commands);
                announced.push(commands);
            }
            Ok(AgentEvent::ToolCall { call, .. }) => eprintln!("TOOL {call:?}"),
            Ok(AgentEvent::Done { status, error, .. }) => {
                eprintln!("DONE {status:?} {error:?}");
                // Give OpenCode's watcher a moment to announce the change.
            }
            Ok(_) => {}
            Err(error) => eprintln!("ERR {error}"),
        }
    }
    let refreshed_in_run = announced
        .iter()
        .skip(1)
        .any(|commands| has(commands, "probe-added", false));
    eprintln!(
        "REFRESHED_IN_RUN {refreshed_in_run} (file exists: {})",
        root.join(".opencode/command/probe-added.md").is_file()
    );
    // The next run reads its location's live catalog at start; a change
    // made after the previous run's last frame retires the cache there.
    let (next_controls, _next_steer) = controls();
    let mut next = harness
        .run(
            request(&model, &cwd, "Reply with only: OK".into()),
            next_controls,
        )
        .await
        .expect("second run starts");
    while let Some(event) = tokio::time::timeout(std::time::Duration::from_secs(240), next.next())
        .await
        .ok()
        .flatten()
    {
        match event {
            Ok(AgentEvent::AvailableCommands { commands }) => show("AVAILABLE_NEXT", &commands),
            Ok(AgentEvent::Done { status, .. }) => eprintln!("DONE_NEXT {status:?}"),
            _ => {}
        }
    }
    let after = harness.commands_in(&scope).await.expect("commands_in");
    show("COMMANDS_IN_AFTER", &after);
    if !has(&after, "probe-added", false) {
        failures.push("commands_in did not pick up the added command".into());
    }

    if failures.is_empty() {
        eprintln!("=== PASS");
    } else {
        for failure in &failures {
            eprintln!("FAIL {failure}");
        }
        std::process::exit(1);
    }
}
