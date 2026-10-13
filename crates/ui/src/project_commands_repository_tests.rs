//! AW-11 rendered acceptance at the project-commands owner. Repository
//! commands show their provenance and this device's trust, have no Run
//! control until their exact content is accepted, and Accept sends the digest
//! of the content that was painted; it never runs anything. Held RPC only:
//! the engine's own trust, precedence and refusal decisions are proven over
//! real checkouts in `crates/engine/tests/project_config.rs`.
use super::*;
use luna_proto::{
    PROJECT_CONFIG_FILE, ProjectCommandTrust, ProjectConfigState, ProjectRepositoryCommand,
    ProjectRepositoryConfig,
};

fn repository_command(
    id: &str,
    name: &str,
    trust: ProjectCommandTrust,
    overridden: bool,
) -> ProjectRepositoryCommand {
    ProjectRepositoryCommand {
        action: ProjectAction {
            id: id.into(),
            name: name.into(),
            command: format!("printf fixture-{id}\nprintf second-line"),
            setup_on_worktree_create: false,
            wait_for_setup: true,
            preview_url: None,
        },
        digest: format!("{id}-digest"),
        trust,
        overridden,
    }
}
fn layer(commands: Vec<ProjectRepositoryCommand>) -> Value {
    serde_json::to_value(ProjectRepositoryConfig {
        checkout: "/fixture/checkout".into(),
        file: PROJECT_CONFIG_FILE.into(),
        state: ProjectConfigState::Loaded,
        detail: None,
        commands,
    })
    .unwrap()
}
/// Repaint, then record which selectors have painted bounds.
fn painted(visual: &mut gpui::VisualTestContext, selectors: &[&'static str]) -> Value {
    visual.update(|window, app| window.draw(app).clear());
    let mut map = serde_json::Map::new();
    for selector in selectors {
        map.insert(
            (*selector).into(),
            json!(visual.debug_bounds(selector).is_some()),
        );
    }
    Value::Object(map)
}
fn trust_requests(rpc: &HeldRpc) -> Vec<Value> {
    rpc.trace()
        .into_iter()
        .filter(|event| {
            event["phase"] == "request" && event["method"] == methods::SET_PROJECT_COMMAND_TRUST
        })
        .map(|event| event["params"].clone())
        .collect()
}

#[gpui::test]
fn commands_panel_runs_repository_commands_only_after_exact_acceptance(cx: &mut TestAppContext) {
    let runtime = runtime(cx);
    let _entered = runtime.enter();
    let rpc = HeldRpc::new("repository-trust", "device-a");
    let unaccepted = repository_command("lint", "Lint", ProjectCommandTrust::Unaccepted, false);
    let mut accepted = unaccepted.clone();
    accepted.trust = ProjectCommandTrust::Accepted;
    let mut before = status("A");
    before["repository"] = layer(vec![unaccepted]);
    let mut after = status("A");
    after["actions"] = json!([action(), accepted.action]);
    after["repository"] = layer(vec![accepted.clone()]);
    rpc.reply(methods::PROJECT_COMMAND_STATUS, Ok(before.clone()), false);
    rpc.reply(
        methods::SET_PROJECT_COMMAND_TRUST,
        Ok(layer(vec![accepted])),
        false,
    );
    let scene = Scene::new(cx, &runtime, rpc.clone(), "device-a");
    scene.open(cx);
    fixture::pump_until(cx, &runtime, |cx| {
        let view = scene.view(cx);
        view["snapshot"] == before && view["busy"] == false
    });
    let (window, review, _, _) = button_window(cx, &scene, "qa-repo-command-review-lint");
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    let selectors = [
        "qa-command-run-build",
        "qa-command-run-lint",
        "qa-command-repository-lint",
        "qa-repo-command-review-lint",
        "qa-repo-command-content-lint",
        "qa-repo-command-accept-lint",
    ];
    let discovered = painted(&mut visual, &selectors);
    visual.update(|window, app| mouse_click_rendered(window, app, review));
    let reviewing = painted(&mut visual, &selectors);
    let accept = visual
        .debug_bounds("qa-repo-command-accept-lint")
        .map(|bounds| bounds.center());
    // The engine answers the follow-up status read with the accepted layer.
    rpc.reply(methods::PROJECT_COMMAND_STATUS, Ok(after.clone()), false);
    if let Some(accept) = accept {
        visual.update(|window, app| mouse_click_rendered(window, app, accept));
    }
    fixture::pump_until(cx, &runtime, |cx| {
        let view = scene.view(cx);
        view["snapshot"] == after && view["busy"] == false
    });
    let accepted = painted(&mut visual, &selectors);
    let requests = trust_requests(&rpc);
    fixture::write_trace(
        "commands-repository-trust",
        &json!({
            "source": file!(),
            "discovered": discovered,
            "reviewing": reviewing,
            "accepted": accepted,
            "trustRequests": requests,
            "runRequests": rpc.calls(methods::RUN_PROJECT_COMMAND),
            "rpc": rpc.trace(),
        }),
    );
    assert_eq!(
        discovered["qa-command-run-build"], true,
        "this device's command keeps Run"
    );
    assert_eq!(
        discovered["qa-command-run-lint"], false,
        "an unaccepted repository command has no Run"
    );
    assert_eq!(
        discovered["qa-repo-command-accept-lint"], false,
        "Accept is offered only once the exact content is shown"
    );
    assert_eq!(reviewing["qa-repo-command-content-lint"], true);
    assert_eq!(reviewing["qa-repo-command-accept-lint"], true);
    assert_eq!(reviewing["qa-command-run-lint"], false);
    assert_eq!(
        requests,
        vec![
            json!({"projectId":PROJECT,"chatId":CHAT,"actionId":"lint","digest":"lint-digest","trusted":true})
        ],
        "one acceptance, pinned to the painted content's digest"
    );
    assert_eq!(
        rpc.calls(methods::RUN_PROJECT_COMMAND),
        0,
        "accepting never runs the command"
    );
    assert_eq!(accepted["qa-command-run-lint"], true);
    assert_eq!(
        accepted["qa-command-repository-lint"], true,
        "a runnable repository command keeps its provenance"
    );
    assert_eq!(accepted["qa-repo-command-review-lint"], false);
}

struct RepositoryCardSurface(Entity<ProjectCommandSettings>);
impl Render for RepositoryCardSurface {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.0.clone())
    }
}

#[gpui::test]
fn settings_card_shows_repository_trust_and_pins_the_displayed_digest(cx: &mut TestAppContext) {
    let runtime = runtime(cx);
    let _entered = runtime.enter();
    let rpc = HeldRpc::new("repository-card", "device-a");
    // This device's `test` overrides the repository's `test`.
    let mut own_test = action();
    own_test.id = "test".into();
    own_test.name = "My test".into();
    let commands = vec![
        repository_command("lint", "Lint", ProjectCommandTrust::Unaccepted, false),
        repository_command("deploy", "Deploy", ProjectCommandTrust::Changed, false),
        repository_command("fmt", "Format", ProjectCommandTrust::Accepted, false),
        repository_command("test", "Test", ProjectCommandTrust::Unaccepted, true),
    ];
    let mut after = commands.clone();
    after[0].trust = ProjectCommandTrust::Accepted;
    rpc.reply(
        methods::GET_PROJECT_COMMANDS,
        Ok(json!([action(), own_test])),
        false,
    );
    rpc.reply(
        methods::GET_PROJECT_CONFIGURATION,
        Ok(layer(commands)),
        false,
    );
    rpc.reply(methods::SET_PROJECT_COMMAND_TRUST, Ok(layer(after)), false);
    let state = cx.new(|_| AppState::new());
    fixture::attach(cx, &state, fixture::handle(&runtime, rpc.clone()));
    let card = cx.new(|cx| ProjectCommandSettings::new(state, PROJECT.into(), cx));
    fixture::pump_until(cx, &runtime, |cx| {
        card.read_with(cx, |card, _| !card.busy && card.repository.is_some())
    });
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::default());
    });
    let rendered = card.clone();
    let window = cx.open_window(gpui::size(px(960.0), px(1400.0)), move |_, _| {
        RepositoryCardSurface(rendered)
    });
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    let selectors = [
        "qa-repo-command-state-lint-unaccepted",
        "qa-repo-command-state-lint-accepted",
        "qa-repo-command-state-deploy-changed",
        "qa-repo-command-state-fmt-accepted",
        "qa-repo-command-state-test-overridden",
        "qa-repo-command-content-lint",
        "qa-repo-command-content-deploy",
        "qa-repo-command-content-fmt",
        "qa-repo-command-accept-lint",
        "qa-repo-command-accept-deploy",
        "qa-repo-command-accept-fmt",
        "qa-repo-command-accept-test",
        "qa-repo-command-revoke-lint",
        "qa-repo-command-revoke-fmt",
        "qa-repo-command-revoke-test",
        "qa-repo-command-override-deploy",
        "qa-repo-command-override-test",
        "qa-command-overrides-test",
        "qa-command-overrides-build",
    ];
    let discovered = painted(&mut visual, &selectors);
    let accept = visual
        .debug_bounds("qa-repo-command-accept-lint")
        .map(|bounds| bounds.center());
    if let Some(accept) = accept {
        visual.update(|window, app| mouse_click_rendered(window, app, accept));
    }
    fixture::pump_until(cx, &runtime, |cx| {
        rpc.settled(methods::SET_PROJECT_COMMAND_TRUST) == 1
            && card.read_with(cx, |card, _| !card.trust_busy)
    });
    let accepted = painted(&mut visual, &selectors);
    // Override opens this device's draft prefilled with the repository
    // content; nothing is saved until the user saves it.
    let override_deploy = visual
        .debug_bounds("qa-repo-command-override-deploy")
        .map(|bounds| bounds.center());
    if let Some(point) = override_deploy {
        visual.update(|window, app| mouse_click_rendered(window, app, point));
    }
    let draft = card.read_with(cx, |card, cx| {
        json!({
            "editing": card.editing,
            "name": card.name.read(cx).text(),
            "command": card.command.read(cx).text(),
        })
    });
    let requests = trust_requests(&rpc);
    fixture::write_trace(
        "settings-repository-trust",
        &json!({
            "source": file!(),
            "discovered": discovered,
            "accepted": accepted,
            "overrideDraft": draft,
            "trustRequests": requests,
            "saveRequests": rpc.calls(methods::SAVE_PROJECT_COMMANDS),
            "rpc": rpc.trace(),
        }),
    );
    for (selector, expected) in [
        ("qa-repo-command-state-lint-unaccepted", true),
        ("qa-repo-command-state-deploy-changed", true),
        ("qa-repo-command-state-fmt-accepted", true),
        ("qa-repo-command-state-test-overridden", true),
        ("qa-repo-command-content-lint", true),
        ("qa-repo-command-content-deploy", true),
        ("qa-repo-command-content-fmt", false),
        ("qa-repo-command-accept-lint", true),
        ("qa-repo-command-accept-deploy", true),
        ("qa-repo-command-accept-fmt", false),
        ("qa-repo-command-revoke-fmt", true),
        ("qa-repo-command-accept-test", false),
        ("qa-repo-command-revoke-test", false),
        ("qa-repo-command-override-test", false),
        ("qa-command-overrides-test", true),
        ("qa-command-overrides-build", false),
    ] {
        assert_eq!(discovered[selector], expected, "discovered {selector}");
    }
    assert_eq!(
        requests,
        vec![json!({"projectId":PROJECT,"actionId":"lint","digest":"lint-digest","trusted":true})],
        "the project-folder view pins the digest it painted"
    );
    assert_eq!(accepted["qa-repo-command-state-lint-accepted"], true);
    assert_eq!(accepted["qa-repo-command-revoke-lint"], true);
    assert_eq!(accepted["qa-repo-command-accept-lint"], false);
    assert_eq!(draft["editing"], "deploy");
    assert_eq!(draft["name"], "Deploy");
    assert_eq!(
        draft["command"],
        "printf fixture-deploy\nprintf second-line"
    );
    assert_eq!(rpc.calls(methods::SAVE_PROJECT_COMMANDS), 0);
    assert_eq!(rpc.calls(methods::RUN_PROJECT_COMMAND), 0);
}
