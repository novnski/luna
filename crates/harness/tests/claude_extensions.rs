//! CC-12 E2E: ClaudeHarness against `tests/fixtures/fake-claude-extensions.sh`
//! (2.1.286 frame shapes; see the script header). Covers hook observation,
//! effective settings scope/precedence, typed skill dispatch and reload.
//! Failure cases: `$B/evidence/cc12-extensions/failure-cases.md`.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use futures::StreamExt;
use tokio::sync::{mpsc, oneshot};

use luna_harness::{CancellationToken, ClaudeHarness, Harness, HarnessError, RunControls};
use luna_proto::{
    AgentEvent, ComposerInput, DoneStatus, NativeCommandKind, NativeExtensionsView,
    NativeSettingsScope, PermissionMode, RunRequest, ToolActivityKind, ToolCall,
};
use serde_json::{Value, json};

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn fake() -> PathBuf {
    let path = fixtures().join("fake-claude-extensions.sh");
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
    path
}

struct World {
    _root: tempfile::TempDir,
    config: PathBuf,
    managed: PathBuf,
    project: PathBuf,
}

impl World {
    fn harness(&self) -> ClaudeHarness {
        ClaudeHarness::new()
            .with_executable(fake())
            .with_config_dir(&self.config)
            .with_managed_settings_dir(&self.managed)
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.config.join("fake-log.txt")).unwrap_or_default()
    }

    fn inits(&self) -> usize {
        self.log().lines().filter(|line| *line == "INIT").count()
    }

    fn write(&self, relative: &str, text: &str) {
        let path = if let Some(rest) = relative.strip_prefix("config/") {
            self.config.join(rest)
        } else if let Some(rest) = relative.strip_prefix("managed/") {
            self.managed.join(rest)
        } else {
            self.project.join(relative)
        };
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// Copy the real 2.1.286 initialize answers for the probe layout.
    fn canned(&self) {
        for (from, to) in [
            ("init-2.1.286-all-sources.json", "fake-initialize.json"),
            (
                "init-2.1.286-restricted-ask.json",
                "fake-initialize-restricted.json",
            ),
        ] {
            std::fs::copy(fixtures().join("claude").join(from), self.config.join(to)).unwrap();
        }
    }
}

fn hook(marker: &str) -> Value {
    json!({"hooks": [{"type": "command", "command": format!("echo {marker}")}]})
}

/// The exact layout the isolated 2.1.286 probe used
/// (`probes/probe-init-extensions.py`), so the canned answers are truthful.
fn world() -> World {
    let root = tempfile::tempdir().unwrap();
    let world = World {
        config: root.path().join("config"),
        managed: root.path().join("managed"),
        project: root.path().join("project"),
        _root: root,
    };
    std::fs::create_dir_all(&world.managed).unwrap();
    let md = |description: &str| format!("---\ndescription: {description}\n---\nnever run\n");
    let skill = |name: &str, description: &str| {
        format!("---\nname: {name}\ndescription: {description}\n---\nnever run\n")
    };
    world.write(
        "config/settings.json",
        &json!({"outputStyle": "user-style", "hooks": {
            "SessionStart": [hook("cc12-user-session-start")],
            "UserPromptSubmit": [hook("cc12-user-prompt")]}})
        .to_string(),
    );
    world.write(
        "config/commands/cc12-user-only.md",
        &md("cc12 user-only command"),
    );
    world.write("config/commands/cc12-collide.md", &md("cc12 USER collide"));
    world.write(
        "config/skills/cc12-user-skill/SKILL.md",
        &skill("cc12-user-skill", "cc12 user skill"),
    );
    world.write(
        "config/skills/cc12-skill-collide/SKILL.md",
        &skill("cc12-skill-collide", "cc12 USER skill collide"),
    );
    world.write(
        "config/output-styles/user-style.md",
        "---\nname: user-style\ndescription: cc12 user style\n---\nBe terse.\n",
    );
    world.write(
        ".claude/settings.json",
        &json!({"outputStyle": "Explanatory", "hooks": {
            "SessionStart": [hook("cc12-project-session-start")]}})
        .to_string(),
    );
    world.write(
        ".claude/commands/cc12-project-only.md",
        "---\ndescription: cc12 project-only command\nargument-hint: <thing>\n---\nnever run $ARGUMENTS\n",
    );
    world.write(
        ".claude/commands/cc12-collide.md",
        &md("cc12 PROJECT collide"),
    );
    world.write(
        ".claude/commands/ns/cc12-nested.md",
        &md("cc12 namespaced command"),
    );
    world.write(
        ".claude/skills/cc12-project-skill/SKILL.md",
        &skill("cc12-project-skill", "cc12 project skill"),
    );
    world.write(
        ".claude/skills/cc12-skill-collide/SKILL.md",
        &skill("cc12-skill-collide", "cc12 PROJECT skill collide"),
    );
    world.write(
        ".claude/output-styles/project-style.md",
        "---\nname: project-style\ndescription: cc12 project style\n---\nBe precise.\n",
    );
    world.write(
        ".claude/settings.local.json",
        &json!({"outputStyle": "project-style"}).to_string(),
    );
    world.write(".claude/commands/cc12-dual.md", &md("cc12 dual COMMAND"));
    world.write(
        ".claude/skills/cc12-dual/SKILL.md",
        &skill("cc12-dual", "cc12 dual SKILL"),
    );
    world
}

/// JSON-built so later additive `RunRequest` fields default.
fn request(prompt: &str, cwd: &Path, mode: PermissionMode) -> RunRequest {
    let mut request: RunRequest = serde_json::from_value(json!({
        "prompt": prompt, "cwd": cwd.to_string_lossy(), "sandbox": "workspace-write",
        "model": null, "reasoning": null, "resume": null,
    }))
    .unwrap();
    request.permission_policy = Some(luna_proto::PermissionPolicy {
        requested_mode: mode,
        effective_mode: mode,
        ..Default::default()
    });
    request
}

fn controls() -> RunControls {
    let (_steer_tx, steer_rx) = mpsc::channel(1);
    RunControls {
        request_input: Box::new(|_| oneshot::channel().1),
        steering: steer_rx,
        interrupt: CancellationToken::new(),
        invoke_luna_tool: None,
    }
}

async fn run_all(harness: &ClaudeHarness, request: RunRequest) -> Vec<AgentEvent> {
    let stream = harness.run(request, controls()).await.expect("run starts");
    tokio::time::timeout(
        Duration::from_secs(15),
        stream
            .map(|event| event.expect("event"))
            .collect::<Vec<_>>(),
    )
    .await
    .expect("run finished")
}

fn position(events: &[AgentEvent], predicate: impl Fn(&AgentEvent) -> bool) -> usize {
    events
        .iter()
        .position(predicate)
        .unwrap_or_else(|| panic!("event missing: {events:#?}"))
}

fn result_for<'a>(events: &'a [AgentEvent], id: &str) -> Vec<(&'a bool, &'a str)> {
    events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolResult {
                id: result_id,
                is_error,
                output,
                ..
            } if result_id == id => Some((is_error, output.as_deref().unwrap_or_default())),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn hook_lifecycle_becomes_root_hook_chips_with_truthful_outcomes() {
    let world = world();
    let events = run_all(
        &world.harness(),
        request("scenario:hooks", &world.project, PermissionMode::Full),
    )
    .await;
    // C2: normal launches opt into every hook event.
    let argv = world.log();
    assert!(argv.contains("[--include-hook-events]"), "{argv}");

    // C3: the SessionStart chip lands after the run boundary, not before it.
    let started = position(&events, |e| matches!(e, AgentEvent::SessionStarted { .. }));
    let session_hook = "hook:280a2fc8-3647-4632-8142-c1d9c11b9dee";
    let chip = position(&events, |e| {
        matches!(e, AgentEvent::ToolCall { id, call: ToolCall::Unknown { name, .. } }
            if id == session_hook && name == "Hook: SessionStart:startup")
    });
    assert!(started < chip, "{events:#?}");
    assert_eq!(
        result_for(&events, session_hook),
        vec![(&false, "Hook completed\n\ncc12-user-session-start\n")]
    );
    assert_eq!(
        result_for(&events, "hook:hk-prompt"),
        vec![(&false, "Hook completed")]
    );

    // C4/C5: cumulative progress forwarded once per new suffix; exit 2 blocks.
    let activity: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolActivity {
                id,
                kind: ToolActivityKind::Stdout,
                text,
            } if id == "hook:hk-guard" => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(activity, vec!["checking command\n", "policy loaded\n"]);
    let guard = result_for(&events, "hook:hk-guard");
    assert_eq!(guard.len(), 1);
    assert!(*guard[0].0);
    assert!(
        guard[0]
            .1
            .starts_with("Hook blocked the tool call: rm -rf is blocked by project policy\n\n"),
        "{guard:?}"
    );
    let guard_call = position(&events, |e| {
        matches!(e, AgentEvent::ToolCall { id, call } if id == "hook:hk-guard"
            && call.native_hook_name() == Some("PreToolUse:Bash"))
    });
    assert!(
        guard_call
            < position(
                &events,
                |e| matches!(e, AgentEvent::ToolResult { id, .. } if id == "hook:hk-guard")
            )
    );

    // C6/C9: JSON decision, response without a start still gets a chip.
    position(
        &events,
        |e| matches!(e, AgentEvent::ToolCall { id, .. } if id == "hook:hk-lint"),
    );
    let lint = result_for(&events, "hook:hk-lint");
    assert!(
        lint[0]
            .1
            .starts_with("Hook sent feedback to Claude: lint failed: 2 errors"),
        "{lint:?}"
    );
    assert!(*lint[0].0);

    // C8, C7 + C10 (duplicate terminal frame ignored).
    assert!(
        result_for(&events, "hook:hk-notify")[0]
            .1
            .starts_with("Hook cancelled (timed out or interrupted)")
    );
    let stop = result_for(&events, "hook:hk-stop");
    assert_eq!(stop.len(), 1, "{stop:?}");
    assert!(
        stop[0]
            .1
            .starts_with("Hook failed (exit 1); Claude continued\n\nnotify-send: no display")
    );

    // C11: SessionEnd after the final result is not surfaced.
    assert!(
        !events.iter().any(|e| matches!(e,
            AgentEvent::ToolCall { id, .. } | AgentEvent::ToolResult { id, .. } if id == "hook:hk-end")),
        "{events:#?}"
    );
    // D4: commands_changed is a typed replacement list.
    assert!(events.iter().any(|e| matches!(e, AgentEvent::AvailableCommands { commands }
        if commands.iter().any(|c| c.name == "deploy-notes" && c.input_hint.as_deref() == Some("<version>")))));
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Done {
            status: DoneStatus::Completed,
            ..
        })
    ));
    // C14: the chip reads as a Hook, not a generic Tool.
    let call = ToolCall::Unknown {
        name: "Hook: PreToolUse:Bash".into(),
        input: None,
    };
    assert_eq!(
        luna_proto::view::tool_chip_content(&call),
        ("Hook", "PreToolUse · Bash".to_owned())
    );
}

#[tokio::test]
async fn hooks_after_a_root_result_wait_for_the_next_root_activity() {
    let world = world();
    let events = run_all(
        &world.harness(),
        request("scenario:idle-hooks", &world.project, PermissionMode::Full),
    )
    .await;
    let dones: Vec<usize> = events
        .iter()
        .enumerate()
        .filter(|(_, e)| matches!(e, AgentEvent::Done { .. }))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(dones.len(), 2, "{events:#?}");
    let held = position(
        &events,
        |e| matches!(e, AgentEvent::ToolCall { id, .. } if id == "hook:hk-bg"),
    );
    let woke = position(
        &events,
        |e| matches!(e, AgentEvent::TextDelta { text } if text == "woke"),
    );
    assert!(
        dones[0] < held && held < woke && woke < dones[1],
        "{events:#?}"
    );
    assert!(
        !events.iter().any(|e| matches!(e,
            AgentEvent::ToolCall { id, .. } | AgentEvent::ToolResult { id, .. } if id == "hook:hk-idle-notify")),
        "idle Notification hooks are not transcript chips: {events:#?}"
    );
}

#[tokio::test]
async fn hook_seen_before_a_preinit_failure_still_precedes_done() {
    let world = world();
    let events = run_all(
        &world.harness(),
        request(
            "scenario:preinit-result",
            &world.project,
            PermissionMode::Full,
        ),
    )
    .await;
    let chip = position(
        &events,
        |e| matches!(e, AgentEvent::ToolCall { id, .. } if id == "hook:hk-early"),
    );
    let done = position(&events, |e| matches!(e, AgentEvent::Done { .. }));
    assert!(chip < done, "{events:#?}");
    assert_eq!(
        result_for(&events, "hook:hk-early"),
        vec![(&false, "Hook completed\n\nwarming cache\n")]
    );
}

#[tokio::test]
async fn title_runs_do_not_request_hook_events() {
    let world = world();
    let stream = world
        .harness()
        .run_title(
            request("title please", &world.project, PermissionMode::Ask),
            controls(),
        )
        .await
        .unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(10), stream.collect::<Vec<_>>()).await;
    let log = world.log();
    assert!(log.contains("[--setting-sources] []"), "{log}");
    assert!(!log.contains("--include-hook-events"), "{log}");
}

fn row<'a>(
    view: &'a NativeExtensionsView,
    name: &str,
    scope: NativeSettingsScope,
    kind: NativeCommandKind,
) -> &'a luna_proto::NativeCommandView {
    view.commands
        .iter()
        .find(|row| row.name == name && row.scope == scope && row.kind == kind)
        .unwrap_or_else(|| panic!("{name} {scope:?} {kind:?} missing: {:#?}", view.commands))
}

#[tokio::test]
async fn effective_view_follows_launch_policy_scope_and_precedence() {
    let world = world();
    world.canned();
    let harness = world.harness();
    let full = harness
        .native_extensions(Some(&world.project), PermissionMode::Full, false)
        .await
        .unwrap();
    std::fs::write(
        world._root.path().join("view-full.json"),
        serde_json::to_vec_pretty(&full).unwrap(),
    )
    .unwrap();
    assert!(full.native_verified, "{full:#?}");
    assert!(full.launch_policy.starts_with("All setting sources load"));
    for scope in [
        NativeSettingsScope::Local,
        NativeSettingsScope::Project,
        NativeSettingsScope::User,
    ] {
        let source = full.sources.iter().find(|s| s.scope == scope).unwrap();
        assert!(source.loaded && source.present, "{source:?}");
    }
    let managed = full
        .sources
        .iter()
        .find(|s| s.scope == NativeSettingsScope::Managed)
        .unwrap();
    assert!(!managed.present && !managed.loaded);
    // A4: local > project > user; Claude's own answer agrees.
    assert_eq!(full.output_style.configured_effective, "project-style");
    assert_eq!(
        full.output_style.configured_scope,
        Some(NativeSettingsScope::Local)
    );
    assert_eq!(full.output_style.native.as_deref(), Some("project-style"));
    let project_style = full
        .output_style
        .configured
        .iter()
        .find(|v| v.scope == NativeSettingsScope::Project)
        .unwrap();
    assert!(!project_style.applied);
    assert_eq!(project_style.reason.as_deref(), Some("overridden by local"));
    // A5: custom style files load with their scope.
    let available: Vec<_> = full
        .output_style
        .available
        .iter()
        .map(|s| (s.name.as_str(), s.scope))
        .collect();
    assert!(available.contains(&("user-style", NativeSettingsScope::User)));
    assert!(available.contains(&("project-style", NativeSettingsScope::Project)));
    // A6: user command beats project command; Claude lists the user one.
    let user = row(
        &full,
        "cc12-collide",
        NativeSettingsScope::User,
        NativeCommandKind::Command,
    );
    assert!(user.configured_effective);
    assert_eq!(user.native, Some(true));
    let project = row(
        &full,
        "cc12-collide",
        NativeSettingsScope::Project,
        NativeCommandKind::Command,
    );
    assert!(!project.configured_effective);
    assert_eq!(project.shadowed_by.as_deref(), Some("User command"));
    assert_eq!(project.native, Some(false));
    // A7: user skill beats project skill.
    assert!(
        row(
            &full,
            "cc12-skill-collide",
            NativeSettingsScope::User,
            NativeCommandKind::Skill
        )
        .configured_effective
    );
    assert!(
        !row(
            &full,
            "cc12-skill-collide",
            NativeSettingsScope::Project,
            NativeCommandKind::Skill
        )
        .configured_effective
    );
    // A8: same-scope skill beats command.
    let dual_skill = row(
        &full,
        "cc12-dual",
        NativeSettingsScope::Project,
        NativeCommandKind::Skill,
    );
    assert!(dual_skill.configured_effective && dual_skill.native == Some(true));
    let dual_command = row(
        &full,
        "cc12-dual",
        NativeSettingsScope::Project,
        NativeCommandKind::Command,
    );
    assert_eq!(dual_command.shadowed_by.as_deref(), Some("Project skill"));
    // A9: namespaced name; argument hints survive.
    let nested = row(
        &full,
        "ns:cc12-nested",
        NativeSettingsScope::Project,
        NativeCommandKind::Command,
    );
    assert!(nested.configured_effective && nested.native == Some(true));
    assert_eq!(
        row(
            &full,
            "cc12-project-only",
            NativeSettingsScope::Project,
            NativeCommandKind::Command
        )
        .argument_hint
        .as_deref(),
        Some("<thing>")
    );
    // A10/A14: hooks merge across scopes; display never carries the command line.
    let hooks: Vec<_> = full
        .hooks
        .iter()
        .map(|h| (h.event.as_str(), h.scope, h.effective, h.handler.as_str()))
        .collect();
    assert!(hooks.contains(&(
        "SessionStart",
        NativeSettingsScope::User,
        true,
        "command · echo"
    )));
    assert!(hooks.contains(&(
        "UserPromptSubmit",
        NativeSettingsScope::User,
        true,
        "command · echo"
    )));
    assert!(hooks.contains(&(
        "SessionStart",
        NativeSettingsScope::Project,
        true,
        "command · echo"
    )));
    assert!(
        !serde_json::to_string(&full)
            .unwrap()
            .contains("cc12-user-session-start")
    );
    assert!(
        full.sources
            .iter()
            .all(|s| !s.display_path.starts_with('/') || s.scope == NativeSettingsScope::Managed)
    );

    // A1/A2: Ask launches with --restricted: nothing user/project/local loads.
    let ask = harness
        .native_extensions(Some(&world.project), PermissionMode::Ask, false)
        .await
        .unwrap();
    std::fs::write(
        world._root.path().join("view-ask.json"),
        serde_json::to_vec_pretty(&ask).unwrap(),
    )
    .unwrap();
    assert!(
        ask.launch_policy.starts_with("Restricted mode"),
        "{}",
        ask.launch_policy
    );
    for scope in [
        NativeSettingsScope::Local,
        NativeSettingsScope::Project,
        NativeSettingsScope::User,
    ] {
        let source = ask.sources.iter().find(|s| s.scope == scope).unwrap();
        assert!(!source.loaded);
        assert_eq!(source.reason.as_deref(), Some("ignored: restricted mode"));
    }
    assert_eq!(ask.output_style.configured_effective, "default");
    assert_eq!(ask.output_style.native.as_deref(), Some("default"));
    assert!(ask.output_style.configured.iter().all(|v| !v.applied));
    assert!(
        ask.output_style
            .available
            .iter()
            .all(|s| s.scope == NativeSettingsScope::BuiltIn)
    );
    assert!(
        ask.commands
            .iter()
            .all(|c| !c.configured_effective && c.native == Some(false))
    );
    assert!(
        ask.hooks
            .iter()
            .all(|h| !h.effective && h.reason.as_deref() == Some("ignored: restricted mode"))
    );
    // Copy both views into the evidence dir when asked.
    if let Some(dir) = std::env::var_os("CC12_EVIDENCE_DIR") {
        let dir = PathBuf::from(dir);
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(
            dir.join("view-full.json"),
            serde_json::to_vec_pretty(&full).unwrap(),
        )
        .unwrap();
        std::fs::write(
            dir.join("view-ask.json"),
            serde_json::to_vec_pretty(&ask).unwrap(),
        )
        .unwrap();
    }
}

#[tokio::test]
async fn managed_policy_and_disable_all_hooks_bound_hook_effect() {
    let world = world();
    world.write(
        "managed/managed-settings.json",
        &json!({"outputStyle": "Concise", "allowManagedHooksOnly": true, "hooks": {
            "PreToolUse": [{"matcher": "Bash", "hooks": [
                {"type": "command", "command": "\"/opt/org/bin/guard.sh\" --token=SECRET"}]}]}})
        .to_string(),
    );
    let view = world
        .harness()
        .native_extensions(Some(&world.project), PermissionMode::Full, false)
        .await
        .unwrap();
    let managed = view
        .hooks
        .iter()
        .find(|h| h.scope == NativeSettingsScope::Managed)
        .unwrap();
    assert!(managed.effective);
    assert_eq!(managed.matcher.as_deref(), Some("Bash"));
    assert_eq!(managed.handler, "command · guard.sh");
    assert!(!serde_json::to_string(&view).unwrap().contains("SECRET"));
    assert!(
        view.hooks
            .iter()
            .filter(|h| h.scope != NativeSettingsScope::Managed)
            .all(|h| !h.effective
                && h.reason.as_deref() == Some("managed policy allows managed hooks only"))
    );
    // Managed outranks every other scope for scalar settings.
    assert_eq!(view.output_style.configured_effective, "Concise");
    assert_eq!(
        view.output_style.configured_scope,
        Some(NativeSettingsScope::Managed)
    );
    // The fake CLI does not read managed files: the mismatch is stated, and
    // Claude's own answer stays authoritative.
    assert_eq!(view.output_style.native.as_deref(), Some("default"));
    assert!(
        view.limitations
            .iter()
            .any(|l| l.contains("Claude's answer is authoritative"))
    );

    // A11: disableAllHooks in local settings switches off user/project hooks;
    // a managed hook (no managed-only policy) still runs.
    world.write(
        "managed/managed-settings.json",
        &json!({"hooks": {"Stop": [{"hooks": [{"type": "http", "url": "https://user:pw@hooks.example.org/stop?x=1"}]}]}})
            .to_string(),
    );
    world.write(
        ".claude/settings.local.json",
        &json!({"disableAllHooks": true}).to_string(),
    );
    let view = world
        .harness()
        .native_extensions(Some(&world.project), PermissionMode::Full, true)
        .await
        .unwrap();
    let managed = view
        .hooks
        .iter()
        .find(|h| h.scope == NativeSettingsScope::Managed)
        .unwrap();
    assert!(managed.effective);
    assert_eq!(managed.handler, "http · hooks.example.org");
    assert!(
        view.hooks
            .iter()
            .filter(|h| h.scope != NativeSettingsScope::Managed)
            .all(|h| !h.effective
                && h.reason.as_deref() == Some("disabled by disableAllHooks in local settings"))
    );
    // A13: malformed JSON is reported, not silently empty.
    world.write(".claude/settings.json", "{ not json");
    let view = world
        .harness()
        .native_extensions(Some(&world.project), PermissionMode::Full, true)
        .await
        .unwrap();
    let project = view
        .sources
        .iter()
        .find(|s| s.scope == NativeSettingsScope::Project)
        .unwrap();
    assert!(
        project
            .reason
            .as_deref()
            .unwrap_or_default()
            .starts_with("unreadable: invalid JSON")
    );
}

#[tokio::test]
async fn typed_skill_dispatches_native_invocation_after_images() {
    let world = world();
    world.canned();
    let png = world._root.path().join("shot.png");
    std::fs::write(
        &png,
        [
            0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0x0d, b'I', b'H', b'D', b'R',
        ],
    )
    .unwrap();
    let mut req = request("summarize the parser", &world.project, PermissionMode::Full);
    req.attachments = vec![png.to_string_lossy().into_owned()];
    // A run-only settings layer (fast mode) must merge with the hook opt-out.
    req.model_options.insert("fastMode".into(), json!("on"));
    req.input_items = vec![ComposerInput::Skill {
        name: "cc12-project-only".into(),
        path: world
            .project
            .join(".claude/commands/cc12-project-only.md")
            .to_string_lossy()
            .into_owned(),
    }];
    let events = run_all(&world.harness(), req).await;
    assert!(
        matches!(
            events.last(),
            Some(AgentEvent::Done {
                status: DoneStatus::Completed,
                ..
            })
        ),
        "{events:#?}"
    );
    let log = world.log();
    // B7: the launch's own catalog was read first, without a bypass flag.
    let init_at = log.find("INIT").expect("readback ran");
    let readback_argv = log[..init_at]
        .lines()
        .rev()
        .find(|l| l.starts_with("ARGV:"))
        .unwrap();
    assert!(!readback_argv.contains("dangerously"), "{readback_argv}");
    assert!(
        readback_argv.contains(r#"[--settings] [{"disableAllHooks":true,"fastMode":true}]"#),
        "{readback_argv}"
    );
    assert_eq!(readback_argv.matches("[--settings]").count(), 1, "{readback_argv}");
    assert!(
        readback_argv.contains("[--permission-mode] [default]"),
        "{readback_argv}"
    );
    // B2/B3: images first, then ONE text block starting with the command.
    let user_line = log
        .lines()
        .filter_map(|l| l.strip_prefix("STDIN: "))
        .find(|l| l.contains("\"type\":\"user\""))
        .expect("user message written");
    let frame: Value = serde_json::from_str(user_line).unwrap();
    let content = frame["message"]["content"].as_array().unwrap();
    assert_eq!(content.len(), 2, "{content:?}");
    assert_eq!(content[0]["type"], "image");
    assert_eq!(content[1]["type"], "text");
    let text = content[1]["text"].as_str().unwrap();
    assert!(
        text.starts_with("/cc12-project-only summarize the parser"),
        "{text}"
    );
    // B9: an existing identical prefix is not doubled.
    assert_eq!(
        luna_harness::claude::extensions::skill_invocation_text(
            "cc12-project-only",
            "/cc12-project-only  x"
        ),
        "/cc12-project-only  x"
    );
}

#[tokio::test]
async fn typed_skill_outside_the_effective_catalog_is_rejected_before_any_user_message() {
    let world = world();
    world.canned();
    let mut req = request("summarize", &world.project, PermissionMode::Ask);
    req.input_items = vec![ComposerInput::Skill {
        name: "cc12-project-only".into(),
        path: world
            .project
            .join(".claude/commands/cc12-project-only.md")
            .to_string_lossy()
            .into_owned(),
    }];
    let error = match world.harness().run(req, controls()).await {
        Err(error) => error,
        Ok(_) => panic!("restricted launch must reject the project command"),
    };
    let HarnessError::RejectedBeforeAcceptance(message) = error else {
        panic!("{error:?}");
    };
    assert!(
        message.contains("/cc12-project-only") && message.contains("Restricted mode"),
        "{message}"
    );
    let log = world.log();
    assert!(log.contains("INIT"));
    assert!(!log.contains("\"type\":\"user\""), "{log}");

    // B4/B5/B6: provider-incompatible selections never spawn a turn.
    for items in [
        vec![ComposerInput::Mention {
            name: "Linear".into(),
            path: "app://linear".into(),
        }],
        vec![
            ComposerInput::Skill {
                name: "a".into(),
                path: "/x/a.md".into(),
            },
            ComposerInput::Skill {
                name: "b".into(),
                path: "/x/b.md".into(),
            },
        ],
        vec![ComposerInput::Skill {
            name: "bad name".into(),
            path: "/x/a.md".into(),
        }],
    ] {
        let mut req = request("x", &world.project, PermissionMode::Full);
        req.input_items = items;
        assert!(matches!(
            world.harness().run(req, controls()).await,
            Err(HarnessError::RejectedBeforeAcceptance(_))
        ));
    }
}

#[tokio::test]
async fn extension_view_reloads_when_settings_files_change() {
    let root = tempfile::tempdir().unwrap();
    let world = World {
        config: root.path().join("config"),
        managed: root.path().join("managed"),
        project: root.path().join("project"),
        _root: root,
    };
    std::fs::create_dir_all(&world.project).unwrap();
    world.write(
        "config/commands/first.md",
        "---\ndescription: first\n---\nx\n",
    );
    let harness = world.harness();
    // D5: the verified view is reused until a relevant file changes.
    let before = harness
        .native_extensions(Some(&world.project), PermissionMode::Full, false)
        .await
        .unwrap();
    assert!(before.native_verified);
    let again = harness
        .native_extensions(Some(&world.project), PermissionMode::Full, false)
        .await
        .unwrap();
    assert_eq!(before, again);
    assert_eq!(world.inits(), 1, "unchanged files reuse the verified view");
    world.write(
        ".claude/settings.json",
        &json!({"outputStyle": "Learning"}).to_string(),
    );
    let after = harness
        .native_extensions(Some(&world.project), PermissionMode::Full, false)
        .await
        .unwrap();
    assert_ne!(before.fingerprint, after.fingerprint);
    assert_eq!(after.output_style.configured_effective, "Learning");
    assert_eq!(world.inits(), 2);
    // A new command file is a change too (D2), and Claude's answer follows it.
    world.write(
        "config/commands/fresh.md",
        "---\ndescription: fresh\n---\nx\n",
    );
    let fresh = harness
        .native_extensions(Some(&world.project), PermissionMode::Full, false)
        .await
        .unwrap();
    assert_eq!(world.inits(), 3);
    assert!(
        fresh
            .commands
            .iter()
            .any(|c| c.name == "fresh" && c.native == Some(true))
    );
    // An explicit refresh always re-reads.
    harness
        .native_extensions(Some(&world.project), PermissionMode::Full, true)
        .await
        .unwrap();
    assert_eq!(world.inits(), 4);
    // The readback never runs the user's hooks.
    let log = world.log();
    assert!(
        log.contains("[--settings] [{\"disableAllHooks\":true}]"),
        "{log}"
    );
}
