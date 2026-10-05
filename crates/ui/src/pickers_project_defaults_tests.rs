//! Preimplementation GPUI integration cases for project boundaries and catalogs.
use super::*;
use gpui::AppContext;
use luna_proto::{ProjectCheckout, ProjectExecutionDefaults, ProjectProviderDefault};

fn project(id: &str, harness: HarnessId, model: &str, checkout: ProjectCheckout) -> Space {
    serde_json::from_value(serde_json::json!({
        "id":id,"deviceId":"owner","path":format!("/{id}"),"gitDetected":true,
        "createdAt":"2026-09-26T00:00:00Z","executionDefaults":{
            "provider":{"harness":harness,"model":model},"permissionMode":"full","checkout":checkout
        }
    }))
    .unwrap()
}

#[gpui::test]
fn project_defaults_switch_restart_clear_and_preserve_draft_choices(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let mut global = ComposerDefaults::default();
    global.remember_model(HarnessId::Codex, "global-model".into(), "Global".into());
    global
        .permission_by_harness
        .insert(HarnessId::Codex, PermissionMode::Auto);
    global.save(dir.path()).unwrap();
    let a = project(
        "a",
        HarnessId::Codex,
        "project-a",
        ProjectCheckout::NewWorktree,
    );
    let b = project(
        "b",
        HarnessId::ClaudeCode,
        "project-b",
        ProjectCheckout::Local,
    );
    let spaces = vec![a.clone(), b];
    let state = cx.new(|_| {
        let mut s = AppState::new();
        s.data_dir = Some(dir.path().into());
        s.local_device_id = Some("owner".into());
        s.selected_device = Some("owner".into());
        s.selected_space = Some("a".into());
        s.spaces = spaces.clone();
        s.provider_defaults.providers.insert(
            HarnessId::Codex,
            luna_proto::ProviderExecutionDefault {
                model: Some("global-model".into()),
                ..Default::default()
            },
        );
        s
    });
    let pickers = cx.new(|cx| Pickers::new(state.clone(), cx));
    for (id, harness, model, checkout) in [
        (
            "a",
            HarnessId::Codex,
            "project-a",
            CheckoutKind::NewWorktree,
        ),
        ("b", HarnessId::ClaudeCode, "project-b", CheckoutKind::Local),
        (
            "a",
            HarnessId::Codex,
            "project-a",
            CheckoutKind::NewWorktree,
        ),
    ] {
        state.update(cx, |state, cx| {
            state.selected_space = Some(id.into());
            cx.notify();
        });
        cx.run_until_parked();
        pickers.read_with(cx, |p, cx| {
            let resolved = p.resolved(cx);
            assert_eq!(resolved.harness, Some(harness));
            assert_eq!(resolved.model.as_deref(), Some(model));
            assert_eq!(resolved.permission_mode, PermissionMode::Full);
            assert_eq!(p.draft().checkout, checkout);
        });
    }
    pickers.update(cx, |p, cx| {
        p.pick_harness(HarnessId::ClaudeCode, cx);
        p.config.model = Some("deliberate-draft".into());
        p.config.permission_mode = Some(PermissionMode::Full);
        p.pick_checkout(CheckoutKind::Local, cx);
    });
    state.update(cx, |s, cx| {
        s.spaces[0].name = Some("renamed".into());
        cx.notify();
    });
    cx.run_until_parked();
    pickers.read_with(cx, |p, cx| {
        assert_eq!(p.resolved(cx).model.as_deref(), Some("deliberate-draft"));
        assert_eq!(p.draft().checkout, CheckoutKind::Local);
    });
    assert_eq!(
        ComposerDefaults::load(dir.path()),
        global,
        "project overrides must not overwrite global fallback"
    );
    // Reconstruct the picker, as on app restart, from the same persisted global
    // and registry projection. Engine test separately verifies disk replay.
    let restarted = cx.new(|cx| Pickers::new(state.clone(), cx));
    restarted.read_with(cx, |p, cx| {
        assert_eq!(p.resolved(cx).model.as_deref(), Some("project-a"))
    });
    state.update(cx, |s, cx| {
        s.spaces[0].execution_defaults = ProjectExecutionDefaults::default();
        cx.notify();
    });
    cx.run_until_parked();
    let inherited = cx.new(|cx| Pickers::new(state.clone(), cx));
    inherited.read_with(cx, |p, cx| {
        assert_eq!(p.resolved(cx).model.as_deref(), Some("global-model"));
        assert_eq!(p.resolved(cx).permission_mode, PermissionMode::Auto);
        assert_eq!(p.draft().checkout, CheckoutKind::Local);
    });
    // Selecting an existing task uses its config even if project defaults differ.
    let chat: luna_proto::Chat=serde_json::from_value(serde_json::json!({
        "id":"existing","deviceId":"owner","archived":false,"createdAt":"2026-09-26T00:00:00Z",
        "spaceId":"a","config":{"harness":"opencode","model":"existing-model","modelOptions":{},"sandbox":"workspace-write","permissionMode":"full"}
    })).unwrap();
    state.update(cx, |s, cx| {
        s.chats = vec![chat.clone()];
        s.selected_chat = Some("existing".into());
        cx.notify();
    });
    cx.run_until_parked();
    inherited.read_with(cx, |p, cx| {
        assert_eq!(p.resolved(cx).harness, Some(HarnessId::Opencode));
        assert_eq!(p.resolved(cx).model.as_deref(), Some("existing-model"));
    });
    assert_eq!(state.read_with(cx, |s, _| s.chats[0].clone()), chat);
}

#[gpui::test]
fn project_defaults_unavailable_choice_is_retained_and_blocked(cx: &mut gpui::TestAppContext) {
    let state = cx.new(|_| {
        let mut s = AppState::new();
        s.spaces = vec![project(
            "a",
            HarnessId::Codex,
            "missing-model",
            ProjectCheckout::Local,
        )];
        s.selected_space = Some("a".into());
        s
    });
    let pickers = cx.new(|cx| Pickers::new(state, cx));
    pickers.update(cx, |p, cx| {
        p.harnesses = Loadable::Ready(vec![]);
        assert!(
            p.permission_run_block_reason(cx)
                .unwrap()
                .contains("provider")
        );
        // A catalog for a different model must not silently replace the requested id.
        p.models.insert(HarnessId::Codex, Loadable::Ready(vec![]));
        assert_eq!(p.resolved(cx).model.as_deref(), Some("missing-model"));
        p.config.harness = Some(HarnessId::Codex);
        assert!(p.permission_run_block_reason(cx).unwrap().contains("model"));
        p.config.model = Some("chosen-model".into());
        assert_eq!(p.resolved(cx).model.as_deref(), Some("chosen-model"));
        // Full is already the project's selected mode; no second confirmation.
        assert_eq!(p.permission_run_block_reason(cx), None);
        p.pick_permission(PermissionMode::Full, cx);
        assert_eq!(p.permission_run_block_reason(cx), None);
    });
}

#[test]
fn project_defaults_provider_only_does_not_import_remembered_model() {
    let mut global = ComposerDefaults::default();
    global.remember_model(HarnessId::Codex, "last-used".into(), "Last used".into());
    let project = ProjectExecutionDefaults {
        provider: Some(ProjectProviderDefault {
            harness: HarnessId::Codex,
            model: None,
        }),
        ..Default::default()
    };
    let resolved = crate::settings::project_defaults::resolve_new_task_defaults(&project, &global);
    assert_eq!(resolved.harness, Some(HarnessId::Codex));
    assert_eq!(resolved.model, None);
}
