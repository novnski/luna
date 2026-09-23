//! CodexHarness integration tests against the fake app server in
//! `tests/fixtures/fake-codex.sh` (no real `codex` binary involved).

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use tokio::sync::{mpsc, oneshot};

use zeron_harness::codex::native::CodexNativeClient;
use zeron_harness::{
    CancellationToken, CodexHarness, Harness, HarnessError, ProviderForkProgress, RunControls,
    SteerAcceptance, SteerMessage,
};
use zeron_proto::{
    AgentEvent, ContextOperationStatus, DoneStatus, HarnessId, ProviderForkBoundary,
    ProviderForkRequest, ReasoningLevel, RunRequest, SandboxLevel, TodoItem, ToolCall,
    UserInputAnswer, UserInputQuestion,
};

fn fixture_path() -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("fake-codex.sh");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
    }
    path
}

fn harness() -> CodexHarness {
    CodexHarness::new().with_executable(fixture_path())
}

fn native_client() -> CodexNativeClient {
    CodexNativeClient::new(std::env::temp_dir().join("luna-codex-native-fixture"))
        .with_executable(fixture_path())
}

fn request(prompt: &str) -> RunRequest {
    RunRequest {
        prompt: prompt.into(),
        harness: None,
        provider_account_id: None,
        model: Some("gpt-5.6-sol".into()),
        reasoning: Some(ReasoningLevel::Ultra),
        model_options: serde_json::Map::new(),
        cwd: String::new(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: true,
        permission_mode: zeron_proto::PermissionMode::Ask,
        parent_permission_ceiling: None,
        environment_permission_ceiling: None,
        permission_policy: None,
        attachments: Vec::new(),
        worktree: None,
        resume: None,
    }
}

/// Controls whose `request_input` answers every question with `answer_label`.
fn controls(
    answer_label: &'static str,
) -> (RunControls, mpsc::Sender<SteerMessage>, CancellationToken) {
    let (steer_tx, steer_rx) = mpsc::channel(8);
    let token = CancellationToken::new();
    let controls = RunControls {
        request_input: Box::new(move |questions| {
            let (tx, rx) = oneshot::channel();
            let answers: Vec<UserInputAnswer> = questions
                .iter()
                .map(|q| UserInputAnswer {
                    question_id: q.id.clone(),
                    labels: vec![answer_label.into()],
                })
                .collect();
            let _ = tx.send(answers);
            rx
        }),
        steering: steer_rx,
        interrupt: token.clone(),
        invoke_luna_tool: None,
    };
    (controls, steer_tx, token)
}

async fn run_to_end(
    harness: &CodexHarness,
    req: RunRequest,
    controls: RunControls,
) -> Vec<AgentEvent> {
    let stream = harness.run(req, controls).await.expect("run starts");
    tokio::time::timeout(
        Duration::from_secs(10),
        stream.map(|r| r.expect("stream event")).collect::<Vec<_>>(),
    )
    .await
    .expect("run finished in time")
}

fn synthetic_runtime_contract() -> zeron_proto::LunaAgentRuntimeContract {
    use zeron_proto::{
        LunaAgentRuntimeContract, RuntimeContext, RuntimeDelivery, RuntimeInstructionSection,
        RuntimeInstructionSource, RuntimeToolDefinition,
    };
    LunaAgentRuntimeContract {
        version: 1,
        content_hash: "synthetic-contract-hash".into(),
        context: RuntimeContext {
            app_version: "test".into(),
            protocol_version: 1,
            thread_id: "thread-a".into(),
            parent_thread_id: None,
            source_thread_id: None,
            project_id: None,
            project_name: None,
            environment_id: "environment-a".into(),
            machine_id: "machine-a".into(),
            machine_name: "machine-a".into(),
            workspace_root: "/synthetic".into(),
            worktree_id: None,
            binding_generation: Some(1),
            provider_profile_id: Some("synthetic-profile".into()),
            branch: None,
            harness: HarnessId::Codex,
            model: None,
            reasoning: None,
            execution_route: "local".into(),
            granted_scopes: Vec::new(),
        },
        environment: std::collections::BTreeMap::from([
            ("LUNA_THREAD_ID".into(), "thread-a".into()),
            ("LUNA_ENVIRONMENT_ID".into(), "environment-a".into()),
            ("LUNA_MACHINE_ID".into(), "machine-a".into()),
        ]),
        local_endpoints: Vec::new(),
        instructions: vec![RuntimeInstructionSection {
            name: "LUNA core".into(),
            source: RuntimeInstructionSource::LunaCore,
            provenance: "synthetic fixture".into(),
            text: "Run inside LUNA.".into(),
        }],
        tools: vec![RuntimeToolDefinition {
            name: "luna_status".into(),
            version: 1,
            description: "Read this task's status.".into(),
            input_schema: serde_json::json!({"type":"object","properties":{},"additionalProperties":false}),
            output_schema: serde_json::json!({"type":"object"}),
            required_scopes: Vec::new(),
            deadline_ms: 2_000,
            idempotency: "read-only".into(),
            presentation: "status".into(),
            activity_classification: "status".into(),
        }],
        omitted_tools: Vec::new(),
        delivery: RuntimeDelivery::CodexDynamicTools,
    }
}

#[tokio::test]
async fn synthetic_luna_dynamic_tool_is_scoped_to_new_thread_and_exact_turn() {
    let home = tempfile::tempdir().unwrap();
    let mut req = request("scenario:runtime-contract");
    req.model_options.insert(
        zeron_harness::codex::LUNA_CODEX_HOME_OPTION.into(),
        home.path().to_str().unwrap().into(),
    );
    req.model_options.insert(
        zeron_proto::LUNA_RUNTIME_CONTRACT_OPTION.into(),
        serde_json::to_value(synthetic_runtime_contract()).unwrap(),
    );
    let (mut run_controls, _steer, _token) = controls("Yes");
    let invocations = Arc::new(Mutex::new(Vec::new()));
    let observed = Arc::clone(&invocations);
    run_controls.invoke_luna_tool = Some(Arc::new(move |invocation| {
        let observed = Arc::clone(&observed);
        Box::pin(async move {
            if invocation.name != "luna_status" || invocation.arguments != serde_json::json!({}) {
                return Err(HarnessError::Protocol("unexpected tool request".into()));
            }
            observed.lock().unwrap().push((
                invocation.context.provider_thread_id().to_owned(),
                invocation.context.provider_turn_id().to_owned(),
                invocation.context.provider_call_id().to_owned(),
            ));
            Ok(serde_json::json!({"threadId":"thread-a"}))
        })
    }));
    let events = run_to_end(&harness(), req, run_controls).await;
    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::Done {
                status: DoneStatus::Completed,
                ..
            }
        )),
        "{events:?}"
    );
    assert!(
        events.iter().any(|event| matches!(event,
            AgentEvent::ToolCall { id, call: ToolCall::Mcp { server, tool, .. } }
            if id == "item-runtime" && server == "luna" && tool == "luna_status"
        )),
        "{events:?}"
    );
    assert!(
        events.iter().any(|event| matches!(event,
            AgentEvent::ToolResult { id, is_error: false, .. } if id == "item-runtime"
        )),
        "{events:?}"
    );
    assert_eq!(
        *invocations.lock().unwrap(),
        vec![("th-1".into(), "t-runtime".into(), "call-runtime".into())]
    );

    let mut resume = request("scenario:runtime-contract-resume");
    resume.resume = Some("synthetic-resume".into());
    resume.model_options.insert(
        zeron_harness::codex::LUNA_CODEX_HOME_OPTION.into(),
        home.path().to_str().unwrap().into(),
    );
    resume.model_options.insert(
        zeron_proto::LUNA_RUNTIME_CONTRACT_OPTION.into(),
        serde_json::to_value(synthetic_runtime_contract()).unwrap(),
    );
    let (mut resume_controls, _steer, _token) = controls("Yes");
    resume_controls.invoke_luna_tool =
        Some(Arc::new(|_| Box::pin(async { Ok(serde_json::json!({})) })));
    let events = run_to_end(&harness(), resume, resume_controls).await;
    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::Done {
                status: DoneStatus::Completed,
                ..
            }
        )),
        "{events:?}"
    );
}

#[tokio::test]
async fn cancelled_luna_turn_rejects_late_tool_request_before_executor() {
    let home = tempfile::tempdir().unwrap();
    let mut req = request("scenario:runtime-contract-cancelled");
    req.model_options.insert(
        zeron_harness::codex::LUNA_CODEX_HOME_OPTION.into(),
        home.path().to_str().unwrap().into(),
    );
    req.model_options.insert(
        zeron_proto::LUNA_RUNTIME_CONTRACT_OPTION.into(),
        serde_json::to_value(synthetic_runtime_contract()).unwrap(),
    );
    let (mut run_controls, _steer, token) = controls("Yes");
    let calls = Arc::new(Mutex::new(0));
    let observed = Arc::clone(&calls);
    run_controls.invoke_luna_tool = Some(Arc::new(move |_| {
        let observed = Arc::clone(&observed);
        Box::pin(async move {
            *observed.lock().unwrap() += 1;
            Ok(serde_json::json!({}))
        })
    }));
    let mut stream = harness().run(req, run_controls).await.expect("run starts");
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match stream.next().await {
                Some(Ok(AgentEvent::ProviderCheckpoint { checkpoint_id }))
                    if checkpoint_id == "t-runtime-cancelled" =>
                {
                    break;
                }
                Some(Ok(_)) => {}
                other => panic!("missing synthetic turn checkpoint: {other:?}"),
            }
        }
    })
    .await
    .expect("checkpoint arrived");
    token.cancel();
    let events = tokio::time::timeout(
        Duration::from_secs(10),
        stream
            .map(|result| result.expect("stream event"))
            .collect::<Vec<_>>(),
    )
    .await
    .expect("cancelled run finished");
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::Done {
            status: DoneStatus::Interrupted,
            ..
        }
    )));
    assert_eq!(*calls.lock().unwrap(), 0);
}

#[tokio::test]
async fn declared_luna_tools_without_engine_handler_fail_before_provider_start() {
    let home = tempfile::tempdir().unwrap();
    let mut req = request("synthetic-no-handler");
    req.model_options.insert(
        zeron_harness::codex::LUNA_CODEX_HOME_OPTION.into(),
        home.path().to_str().unwrap().into(),
    );
    req.model_options.insert(
        zeron_proto::LUNA_RUNTIME_CONTRACT_OPTION.into(),
        serde_json::to_value(synthetic_runtime_contract()).unwrap(),
    );
    let (controls, _steer, _token) = controls("Yes");
    let result = harness().run(req, controls).await;
    assert!(
        matches!(result, Err(HarnessError::Protocol(message)) if message.contains("no matching engine executor"))
    );
}

#[tokio::test]
async fn ordinary_threads_are_native_visible_and_strict_resume_never_rebinds() {
    let (run_controls, _steer, _token) = controls("Yes");
    let mut req = request("scenario:native-title");
    req.model_options.insert(
        zeron_harness::codex::LUNA_NATIVE_THREAD_NAME_OPTION.into(),
        "[LUNA] Native fixture".into(),
    );
    let events = run_to_end(&harness(), req, run_controls).await;
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::Done {
            status: DoneStatus::Completed,
            ..
        }
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ProviderTaskChanged { change } if change == "thread/name/updated"
    )));

    let (run_controls, _steer, _token) = controls("Yes");
    let mut req = request("strict resume must fail");
    req.resume = Some("resume-fail".into());
    req.model_options.insert(
        zeron_harness::codex::LUNA_STRICT_RESUME_OPTION.into(),
        true.into(),
    );
    let events = run_to_end(&harness(), req, run_controls).await;
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::SessionStarted { .. }))
    );
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::Done {
            status: DoneStatus::Errored,
            ..
        }
    )));
}

#[tokio::test]
async fn native_metadata_client_uses_supported_methods_only() {
    let client = native_client();
    let observed = client
        .observe_thread("qa-import")
        .await
        .unwrap()
        .expect("fixture thread exists");
    assert_eq!(observed.id, "qa-import");
    assert_eq!(observed.name.as_deref(), Some("[LUNA QA] Imported fixture"));
    assert_eq!(observed.archived, Some(false));
    assert_eq!(client.observe_thread("missing-thread").await.unwrap(), None);
    client
        .set_name("qa-import", "[LUNA] Renamed fixture")
        .await
        .unwrap();
    client.set_archived("qa-import", true).await.unwrap();
    client.set_archived("qa-import", false).await.unwrap();
}

#[tokio::test]
async fn native_fork_reports_acknowledged_id_before_mismatch_or_timeout_and_not_on_preflight() {
    let request = |source_session_id: &str| ProviderForkRequest {
        source_session_id: source_session_id.into(),
        boundary: ProviderForkBoundary::Tip,
        cwd: "/tmp".into(),
        model: Some("gpt-5.6-sol".into()),
        provider_home: None,
        force_file_auth: true,
    };

    let home = tempfile::tempdir().unwrap();
    let client = CodexNativeClient::new(home.path()).with_executable(fixture_path());
    let progress = Arc::new(Mutex::new(Vec::new()));
    let observed = progress.clone();
    let report = move |event| {
        observed.lock().unwrap().push(event);
        Ok(())
    };
    let error = client
        .fork_thread_staged(request("fork-source-mismatch"), &report)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("history diverged"));
    assert_eq!(
        progress.lock().unwrap().last(),
        Some(&ProviderForkProgress::Acknowledged {
            provider_session_id: "forked-fork-source-mismatch".into()
        })
    );

    let progress = Arc::new(Mutex::new(Vec::new()));
    let observed = progress.clone();
    let report = move |event| {
        observed.lock().unwrap().push(event);
        Ok(())
    };
    let timed_out = tokio::time::timeout(
        Duration::from_millis(100),
        client.fork_thread_staged(request("fork-source-timeout"), &report),
    )
    .await;
    assert!(timed_out.is_err());
    assert_eq!(
        progress.lock().unwrap().last(),
        Some(&ProviderForkProgress::Acknowledged {
            provider_session_id: "forked-fork-source-timeout".into()
        })
    );

    let progress = Arc::new(Mutex::new(Vec::new()));
    let observed = progress.clone();
    let report = move |event| {
        observed.lock().unwrap().push(event);
        Ok(())
    };
    let error = client
        .fork_thread_staged(request("fork-source-preflight-fail"), &report)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("preflight failure"));
    assert!(progress.lock().unwrap().is_empty());
    assert_eq!(
        std::fs::read_to_string(home.path().join("fork-mutations"))
            .unwrap()
            .lines()
            .count(),
        2
    );
}

#[tokio::test]
async fn native_compaction_resumes_bound_thread_before_start_and_waits_for_matching_completion() {
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let config = "[features.context_management]\nexperimental_mode = true\n";
    std::fs::write(home.path().join("config.toml"), config).unwrap();
    let base = CodexNativeClient::new(home.path()).with_executable(fixture_path());
    let client = base.for_execution_home(home.path(), true);
    tokio::time::timeout(
        Duration::from_secs(5),
        client.compact_thread(
            "compact-ok",
            cwd.path().to_str().unwrap(),
            Some("gpt-5.6-sol"),
        ),
    )
    .await
    .expect("fixture compaction completed in time")
    .expect("fixture compaction succeeded");
    assert_eq!(
        std::fs::read_to_string(home.path().join("compact-trace")).unwrap(),
        "resume\ncompact\nmatching-completion\n"
    );
    assert!(home.path().join("compact-called").is_file());
    assert!(
        home.path().join("compact-matching-completion").is_file(),
        "the request ACK and another thread's completion must not finish compaction"
    );
    assert_eq!(
        std::fs::read_to_string(home.path().join("compact-config-observed")).unwrap(),
        config
    );
    assert_eq!(
        std::fs::read_to_string(home.path().join("compact-cwd"))
            .unwrap()
            .trim(),
        std::fs::canonicalize(cwd.path()).unwrap().to_str().unwrap()
    );
    let resume = std::fs::read_to_string(home.path().join("compact-resume-request")).unwrap();
    assert!(resume.contains(&format!("\"cwd\":\"{}\"", cwd.path().display())));
    assert!(resume.contains("\"model\":\"gpt-5.6-sol\""));
    let argv = std::fs::read_to_string(home.path().join("compact-argv")).unwrap();
    assert!(argv.contains("cli_auth_credentials_store=\"file\""));
}

#[tokio::test]
async fn native_compaction_load_owner_and_binding_failures_never_start_or_replay_compaction() {
    for (thread_id, expected) in [
        ("compact-load-fail", "could not be loaded"),
        ("compact-owner-active", "active in another owner"),
        ("compact-wrong-binding", "instead of bound thread"),
    ] {
        let home = tempfile::tempdir().unwrap();
        let client = CodexNativeClient::new(home.path()).with_executable(fixture_path());
        let error = tokio::time::timeout(
            Duration::from_secs(5),
            client.compact_thread(thread_id, "", None),
        )
        .await
        .expect("fixture failure returned in time")
        .expect_err("unsafe compact must fail before dispatch");
        assert!(error.to_string().contains(expected), "{thread_id}: {error}");
        assert!(
            !home.path().join("compact-called").exists(),
            "{thread_id} must not dispatch or replay compact"
        );
        let trace = std::fs::read_to_string(home.path().join("compact-trace")).unwrap();
        assert_eq!(trace.lines().count(), 1, "{thread_id}: {trace}");
    }
}

#[tokio::test]
async fn ordinary_start_and_resume_inherit_the_selected_codex_home_config() {
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let config = "[features.context_management]\nexperimental_mode = true\n";
    std::fs::write(home.path().join("config.toml"), config).unwrap();

    for (prompt, resume, prefix, force_file_auth) in [
        (
            "scenario:context-config-start",
            None,
            "ordinary-start",
            false,
        ),
        (
            "scenario:context-config-resume",
            Some("context-resume"),
            "ordinary-resume",
            true,
        ),
    ] {
        let (run_controls, _steer, _token) = controls("Yes");
        let mut req = request(prompt);
        req.cwd = cwd.path().to_string_lossy().into_owned();
        req.resume = resume.map(str::to_owned);
        req.model_options.insert(
            zeron_harness::codex::LUNA_CODEX_HOME_OPTION.into(),
            home.path().to_string_lossy().into_owned().into(),
        );
        if force_file_auth {
            req.model_options.insert(
                zeron_harness::codex::LUNA_CODEX_FILE_AUTH_OPTION.into(),
                true.into(),
            );
        }
        let events = run_to_end(&harness(), req, run_controls).await;
        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::Done {
                status: DoneStatus::Completed,
                ..
            }
        )));
        assert_eq!(
            std::fs::read_to_string(home.path().join(format!("{prefix}-config-observed"))).unwrap(),
            config
        );
        assert_eq!(
            std::fs::read_to_string(home.path().join(format!("{prefix}-cwd")))
                .unwrap()
                .trim(),
            std::fs::canonicalize(cwd.path()).unwrap().to_str().unwrap()
        );
        let request =
            std::fs::read_to_string(home.path().join(format!("{prefix}-request"))).unwrap();
        assert!(request.contains("\"model\":\"gpt-5.6-sol\""));
        assert!(request.contains(&format!("\"cwd\":\"{}\"", cwd.path().display())));
        let argv = std::fs::read_to_string(home.path().join(format!("{prefix}-argv"))).unwrap();
        assert_eq!(
            argv.contains("cli_auth_credentials_store=\"file\""),
            force_file_auth,
            "{prefix}: {argv}"
        );
    }
}

#[tokio::test]
async fn automatic_context_compaction_is_correlated_and_never_completes_the_turn() {
    let (run_controls, _steer, _token) = controls("Yes");
    let events = run_to_end(
        &harness(),
        request("scenario:auto-context-complete"),
        run_controls,
    )
    .await;
    let lifecycle = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ContextCompaction {
                operation_id,
                status,
                thread_id,
                turn_id,
            } => Some((operation_id, status, thread_id, turn_id)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(lifecycle.len(), 2, "{events:?}");
    assert_eq!(lifecycle[0].1, &ContextOperationStatus::Running);
    assert_eq!(lifecycle[1].1, &ContextOperationStatus::Completed);
    assert_eq!(lifecycle[0].0, lifecycle[1].0);
    assert_eq!(lifecycle[0].2.as_str(), "th-1");
    assert_eq!(lifecycle[0].3.as_str(), "t-auto-context");
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, AgentEvent::Done { .. }))
            .count(),
        1,
        "context compaction must not complete the enclosing turn"
    );
    assert!(events.contains(&AgentEvent::ContextUsage {
        tokens: Some(0),
        window: Some(100_000),
    }));
}

#[tokio::test]
async fn stale_turn_and_duplicate_full_compaction_lifecycles_are_ignored() {
    let (run_controls, _steer, _token) = controls("Yes");
    let events = run_to_end(
        &harness(),
        request("scenario:auto-context-stale-duplicate"),
        run_controls,
    )
    .await;
    let lifecycle = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ContextCompaction {
                operation_id,
                status,
                turn_id,
                ..
            } => Some((operation_id, status, turn_id)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(lifecycle.len(), 2, "{events:?}");
    assert_eq!(lifecycle[0].1, &ContextOperationStatus::Running);
    assert_eq!(lifecycle[1].1, &ContextOperationStatus::Completed);
    assert_eq!(lifecycle[0].0, lifecycle[1].0);
    assert_eq!(lifecycle[0].2.as_str(), "t-current-b");
    assert!(
        lifecycle
            .iter()
            .all(|(_, _, turn)| turn.as_str() != "t-stale-a"),
        "a completed turn must not reopen a compaction slot"
    );
}

#[tokio::test]
async fn canonical_terminal_turn_resolves_pending_compaction_before_done() {
    for (scenario, expected_done) in [
        ("scenario:auto-context-terminal-failed", DoneStatus::Errored),
        (
            "scenario:auto-context-terminal-success-missing",
            DoneStatus::Completed,
        ),
    ] {
        let (run_controls, steer, token) = controls("Yes");
        let harness = harness();
        let mut stream = harness
            .run(request(scenario), run_controls)
            .await
            .expect("run starts");
        let events = tokio::time::timeout(Duration::from_secs(5), async {
            let mut events = Vec::new();
            while let Some(event) = stream.next().await {
                let event = event.expect("stream event");
                let done = matches!(event, AgentEvent::Done { .. });
                events.push(event);
                if done {
                    break;
                }
            }
            events
        })
        .await
        .expect("terminal event arrived");
        let lifecycle = events
            .iter()
            .filter_map(|event| match event {
                AgentEvent::ContextCompaction { status, .. } => Some(*status),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            lifecycle,
            vec![
                ContextOperationStatus::Running,
                ContextOperationStatus::Unknown
            ],
            "{scenario}: {events:?}"
        );
        assert!(matches!(
            events.last(),
            Some(AgentEvent::Done { status, .. }) if *status == expected_done
        ));
        assert!(
            tokio::time::timeout(Duration::from_millis(100), stream.next())
                .await
                .is_err(),
            "{scenario}: the provider process and steering mailbox must still be open after Done"
        );
        drop(steer);
        token.cancel();
    }
}

#[tokio::test]
async fn interrupted_or_disconnected_automatic_compaction_becomes_unknown_once() {
    for (scenario, expected_done) in [
        ("scenario:auto-context-interrupted", DoneStatus::Interrupted),
        ("scenario:auto-context-disconnect", DoneStatus::Errored),
    ] {
        let (run_controls, _steer, _token) = controls("Yes");
        let events = run_to_end(&harness(), request(scenario), run_controls).await;
        let statuses = events
            .iter()
            .filter_map(|event| match event {
                AgentEvent::ContextCompaction { status, .. } => Some(*status),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            statuses,
            vec![
                ContextOperationStatus::Running,
                ContextOperationStatus::Unknown
            ],
            "{scenario}: {events:?}"
        );
        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::Done { status, .. } if *status == expected_done
        )));
    }
}

#[tokio::test]
#[ignore = "creates one clearly marked native Codex QA thread and one short GPT-5.6 Luna turn"]
async fn live_l06_native_visibility_rename_archive_and_reopen() {
    let home = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")))
        .expect("CODEX_HOME or HOME");
    let client = CodexNativeClient::new(home);
    let name = format!(
        "[LUNA QA L06] native reconciliation {}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    );
    let (run_controls, steer, _token) = controls("No");
    drop(steer);
    let mut req = request("Reply exactly L06-QA.");
    req.model = Some("gpt-5.6-luna".into());
    req.reasoning = Some(ReasoningLevel::Low);
    req.cwd = std::env::temp_dir().to_string_lossy().into_owned();
    req.sandbox = SandboxLevel::ReadOnly;
    req.model_options.insert(
        zeron_harness::codex::LUNA_NATIVE_THREAD_NAME_OPTION.into(),
        name.clone().into(),
    );
    let stream = CodexHarness::new()
        .run(req, run_controls)
        .await
        .expect("start live QA turn");
    let events = tokio::time::timeout(
        Duration::from_secs(120),
        stream
            .map(|event| event.expect("live QA event"))
            .collect::<Vec<_>>(),
    )
    .await
    .expect("live QA turn finished in time");
    let thread_id = events
        .iter()
        .find_map(|event| match event {
            AgentEvent::SessionStarted { session_id, .. } => Some(session_id.clone()),
            _ => None,
        })
        .expect("live QA thread id");
    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::Done {
                status: DoneStatus::Completed,
                ..
            }
        )),
        "{events:?}"
    );
    let created = client
        .observe_thread(&thread_id)
        .await
        .expect("read live QA thread")
        .expect("live QA thread remains visible");
    assert!(!created.ephemeral);
    assert_eq!(created.name.as_deref(), Some(name.as_str()));

    let renamed = format!("{name} renamed");
    client
        .set_name(&created.id, &renamed)
        .await
        .expect("rename QA thread");
    let readback = client
        .observe_thread(&created.id)
        .await
        .expect("read renamed QA thread")
        .expect("renamed QA thread remains visible");
    assert_eq!(readback.name.as_deref(), Some(renamed.as_str()));

    client
        .set_archived(&created.id, true)
        .await
        .expect("archive QA thread");
    assert_eq!(
        client
            .observe_thread(&created.id)
            .await
            .expect("read archived QA thread")
            .expect("archived QA thread remains readable")
            .archived,
        Some(true)
    );
    client
        .set_archived(&created.id, false)
        .await
        .expect("reopen QA thread");
    let reopened = client
        .observe_thread(&created.id)
        .await
        .expect("read reopened QA thread")
        .expect("reopened QA thread remains visible");
    assert_eq!(reopened.archived, Some(false));
    println!(
        "L06_NATIVE_QA thread_id={} name={} cwd={} cli_version={}",
        reopened.id,
        reopened.name.unwrap_or_default(),
        reopened.cwd,
        reopened.cli_version.unwrap_or_default()
    );
}

#[tokio::test]
async fn reasoning_preserves_summary_parts_and_item_boundaries_per_thread() {
    let (controls, _steer, _token) = controls("Yes");
    let events = run_to_end(&harness(), request("scenario:reasoning"), controls).await;
    let mut parent = String::new();
    let mut child = String::new();
    for event in &events {
        match event {
            AgentEvent::ReasoningDelta { text } => parent.push_str(text),
            AgentEvent::Subagent { event, .. } => {
                if let AgentEvent::ReasoningDelta { text } = event.as_ref() {
                    child.push_str(text);
                }
            }
            _ => {}
        }
    }
    assert_eq!(
        parent,
        "**Implementing file badges**\n\n**Preparing fixture screenshots**\n\nChecking the final result."
    );
    assert_eq!(child, "**Checking layout**\n\nInspecting the output panel.");
}

#[tokio::test]
async fn happy_path_maps_deltas_items_usage_and_done() {
    let (controls, _steer, _token) = controls("Yes");
    let mut req = request("scenario:happy");
    req.cwd = "/tmp".into();
    req.model_options.insert(
        "serviceTier".into(),
        serde_json::Value::String("fast".into()),
    );
    let events = run_to_end(&harness(), req, controls).await;

    // SessionStarted from thread/start's thread id.
    let starts: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::SessionStarted {
                harness,
                model,
                cwd,
                session_id,
                ..
            } => Some((harness, model, cwd, session_id)),
            _ => None,
        })
        .collect();
    assert_eq!(starts.len(), 1, "{events:?}");
    let (h, model, cwd, session_id) = starts[0];
    assert_eq!(*h, HarnessId::Codex);
    assert_eq!(model, "gpt-5.6-sol");
    assert_eq!(cwd, "/tmp");
    assert_eq!(session_id, "th-1");

    // Deltas — both wire spellings accepted.
    assert!(events.contains(&AgentEvent::TextDelta {
        text: "Hello".into()
    }));
    assert!(events.contains(&AgentEvent::ReasoningDelta {
        text: "thinking hard".into()
    }));
    assert!(events.contains(&AgentEvent::ReasoningDelta {
        text: "summary".into()
    }));

    // commandExecution: ToolCall at started only, exit code 1 => error result.
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AgentEvent::ToolCall { id, .. } if id == "c1"))
            .count(),
        1
    );
    assert!(events.contains(&AgentEvent::ToolCall {
        id: "c1".into(),
        call: ToolCall::Exec {
            command: "ls -la".into()
        },
    }));
    assert!(events.contains(&AgentEvent::ToolResult {
        id: "c1".into(),
        is_error: true,
        output: None,
        diff: None,
    }));

    // fileChange (single add): WriteFile, refreshed at completion.
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(
                e,
                AgentEvent::ToolCall {
                    id,
                    call: ToolCall::WriteFile { path, content: None }
                } if id == "f1" && path == "/tmp/new.rs"
            ))
            .count(),
        2,
        "started + completion-refresh: {events:?}"
    );
    assert!(events.contains(&AgentEvent::ToolResult {
        id: "f1".into(),
        is_error: false,
        output: None,
        diff: None,
    }));

    // mcpToolCall with failed status.
    assert!(events.contains(&AgentEvent::ToolCall {
        id: "mcp1".into(),
        call: ToolCall::Mcp {
            server: "linear".into(),
            tool: "search".into(),
            input: Some(serde_json::json!({"q": "bug"})),
        },
    }));
    assert!(events.contains(&AgentEvent::ToolResult {
        id: "mcp1".into(),
        is_error: true,
        output: None,
        diff: None,
    }));

    // webSearch lifecycle.
    assert!(events.contains(&AgentEvent::ToolCall {
        id: "w1".into(),
        call: ToolCall::WebSearch {
            query: "rust".into()
        },
    }));
    assert!(events.contains(&AgentEvent::ToolResult {
        id: "w1".into(),
        is_error: false,
        output: None,
        diff: None,
    }));

    // Completion-only todoList still opens and closes the lifecycle.
    assert!(events.contains(&AgentEvent::ToolCall {
        id: "td1".into(),
        call: ToolCall::Todo {
            items: vec![
                TodoItem {
                    text: "a".into(),
                    done: true
                },
                TodoItem {
                    text: "b".into(),
                    done: false
                },
            ]
        },
    }));
    assert!(events.contains(&AgentEvent::ToolResult {
        id: "td1".into(),
        is_error: false,
        output: None,
        diff: None,
    }));

    // Streamed agentMessage must not re-emit its completed text…
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::TextDelta { text } if text == "Hello world")),
        "streamed message text re-emitted: {events:?}"
    );
    // …but a never-streamed one falls back to the completed text.
    assert!(events.contains(&AgentEvent::TextDelta {
        text: "unstreamed tail".into()
    }));
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AgentEvent::AssistantMessageCompleted { .. }))
            .count(),
        2
    );

    // Usage rides just before the terminal Done.
    let usage_pos = events
        .iter()
        .position(|e| {
            matches!(
                e,
                AgentEvent::Usage {
                    input_tokens: 42,
                    output_tokens: 7
                }
            )
        })
        .expect("usage emitted");
    let done_pos = events
        .iter()
        .position(|e| matches!(e, AgentEvent::Done { .. }))
        .expect("done emitted");
    assert!(usage_pos < done_pos);
    assert_eq!(
        events.last(),
        Some(&AgentEvent::Done {
            status: DoneStatus::Completed,
            result: None,
            error: None,
            session_id: Some("th-1".into()),
        })
    );
}

#[tokio::test]
async fn steering_uses_turn_steer_with_expected_turn_id() {
    let (controls, steer, _token) = controls("Yes");
    steer
        .send(SteerMessage {
            prompt: "redirect please".into(),
            message_id: None,
            delivery: zeron_harness::SteerDelivery::AllowFollowUpTurn,
            acceptance: None,
        })
        .await
        .expect("steer queued");
    let events = run_to_end(&harness(), request("scenario:steer"), controls).await;

    let steered = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::Steered {
                assistant_message_id,
                next_assistant_message_id,
            } => Some((
                assistant_message_id.clone(),
                next_assistant_message_id.clone(),
            )),
            _ => None,
        })
        .expect("Steered emitted: {events:?}");
    assert!(steered.0.is_some() && steered.1.is_some());
    assert_ne!(steered.0, steered.1);

    // The fake only emits this delta after verifying expectedTurnId + text.
    assert!(events.contains(&AgentEvent::TextDelta {
        text: "steered".into()
    }));
    assert_eq!(
        events.last(),
        Some(&AgentEvent::Done {
            status: DoneStatus::Completed,
            result: None,
            error: None,
            session_id: Some("th-1".into()),
        })
    );
}

#[tokio::test]
async fn explicit_steer_ack_does_not_claim_incorporation_from_a_text_echo() {
    let (controls, steer, _token) = controls("Yes");
    let (accepted_tx, accepted_rx) = oneshot::channel();
    steer
        .send(SteerMessage {
            prompt: "redirect please".into(),
            message_id: Some("queued-steer".into()),
            delivery: zeron_harness::SteerDelivery::RequireActiveTurn,
            acceptance: Some(accepted_tx),
        })
        .await
        .expect("steer queued");
    let events = run_to_end(&harness(), request("scenario:steer"), controls).await;

    assert_eq!(
        accepted_rx.await.expect("acceptance response"),
        SteerAcceptance::Accepted
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::Steered { .. })),
        "an identity-free user echo cannot settle an explicit steer: {events:?}"
    );
    assert!(events.contains(&AgentEvent::TextDelta {
        text: "steered".into()
    }));
}

#[tokio::test]
async fn explicit_steers_are_single_flight_across_stale_and_duplicate_echoes() {
    let (controls, steer, _token) = controls("Yes");
    let (first_tx, first_rx) = oneshot::channel();
    let (second_tx, second_rx) = oneshot::channel();
    for (message_id, acceptance) in [("queued-steer-a", first_tx), ("queued-steer-b", second_tx)] {
        steer
            .send(SteerMessage {
                prompt: "same prompt".into(),
                message_id: Some(message_id.into()),
                delivery: zeron_harness::SteerDelivery::RequireActiveTurn,
                acceptance: Some(acceptance),
            })
            .await
            .expect("steer queued");
    }
    drop(steer);
    let events = run_to_end(
        &harness(),
        request("scenario:steer-correlation-ambiguous"),
        controls,
    )
    .await;

    assert_eq!(
        first_rx.await.expect("first acceptance"),
        SteerAcceptance::Accepted
    );
    assert!(matches!(
        second_rx.await.expect("second acceptance"),
        SteerAcceptance::Rejected(_)
    ));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::Steered { .. })),
        "stale/duplicate identity-free echoes must not prove incorporation: {events:?}"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, AgentEvent::Done { .. }))
            .count(),
        1,
        "single-flight rejection must not create a fallback turn: {events:?}"
    );
}

#[tokio::test]
async fn explicit_steer_rejection_never_falls_back_to_turn_start() {
    let (controls, steer, _token) = controls("Yes");
    let (accepted_tx, accepted_rx) = oneshot::channel();
    steer
        .send(SteerMessage {
            prompt: "redirect please".into(),
            message_id: Some("queued-steer".into()),
            delivery: zeron_harness::SteerDelivery::RequireActiveTurn,
            acceptance: Some(accepted_tx),
        })
        .await
        .expect("steer queued");
    drop(steer);
    let events = run_to_end(&harness(), request("scenario:steer-race"), controls).await;

    assert!(matches!(
        accepted_rx.await.expect("acceptance response"),
        SteerAcceptance::Rejected(_)
    ));
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, AgentEvent::Done { .. }))
            .count(),
        1,
        "explicit steering must not manufacture a second provider turn: {events:?}"
    );
    assert!(!events.contains(&AgentEvent::TextDelta {
        text: "fallback".into()
    }));
}

#[tokio::test]
async fn explicit_steer_lost_response_is_unknown_and_never_replayed() {
    let (controls, steer, _token) = controls("Yes");
    let (accepted_tx, accepted_rx) = oneshot::channel();
    steer
        .send(SteerMessage {
            prompt: "redirect please".into(),
            message_id: Some("queued-steer".into()),
            delivery: zeron_harness::SteerDelivery::RequireActiveTurn,
            acceptance: Some(accepted_tx),
        })
        .await
        .expect("steer queued");
    drop(steer);
    let events = run_to_end(
        &harness(),
        request("scenario:steer-lost-response"),
        controls,
    )
    .await;

    assert!(matches!(
        accepted_rx.await.expect("acceptance response"),
        SteerAcceptance::Unknown(_)
    ));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::Steered { .. }))
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, AgentEvent::Done { .. }))
            .count(),
        1,
        "a lost response must not manufacture a fallback turn: {events:?}"
    );
}

#[tokio::test]
async fn explicit_steer_ack_without_provider_echo_is_not_incorporated() {
    let (controls, steer, _token) = controls("Yes");
    let (accepted_tx, accepted_rx) = oneshot::channel();
    steer
        .send(SteerMessage {
            prompt: "redirect please".into(),
            message_id: Some("queued-steer".into()),
            delivery: zeron_harness::SteerDelivery::RequireActiveTurn,
            acceptance: Some(accepted_tx),
        })
        .await
        .expect("steer queued");
    drop(steer);
    let events = run_to_end(
        &harness(),
        request("scenario:steer-accepted-only"),
        controls,
    )
    .await;

    assert_eq!(
        accepted_rx.await.expect("acceptance response"),
        SteerAcceptance::Accepted
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::Steered { .. })),
        "an RPC ACK alone must not synthesize incorporation: {events:?}"
    );
}

#[tokio::test]
async fn rejected_steer_falls_back_to_a_follow_up_turn() {
    let (controls, steer, _token) = controls("Yes");
    steer
        .send(SteerMessage {
            prompt: "redirect please".into(),
            message_id: None,
            delivery: zeron_harness::SteerDelivery::AllowFollowUpTurn,
            acceptance: None,
        })
        .await
        .expect("steer queued");
    let events = run_to_end(&harness(), request("scenario:steer-race"), controls).await;

    // Two turns: the raced one completes, then the fallback carries the steer.
    let dones: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Done { status, .. } => Some(*status),
            _ => None,
        })
        .collect();
    assert_eq!(
        dones,
        vec![DoneStatus::Completed, DoneStatus::Completed],
        "{events:?}"
    );
    let steered_pos = events
        .iter()
        .position(|e| matches!(e, AgentEvent::Steered { .. }))
        .expect("Steered emitted on fallback");
    let first_done_pos = events
        .iter()
        .position(|e| matches!(e, AgentEvent::Done { .. }))
        .expect("first done");
    assert!(
        first_done_pos < steered_pos,
        "fallback turn starts after the raced turn ends: {events:?}"
    );
    // Only emitted by the fake when the fallback turn/start carried the text.
    assert!(events.contains(&AgentEvent::TextDelta {
        text: "fallback".into()
    }));
}

#[tokio::test]
async fn approvals_round_trip_as_input_requests() {
    // Approvals must reach the ENGINE's input bridge (`request_input`) — and
    // the harness must NOT emit its own `InputRequested`/`InputResolved`
    // twins: the bridge owns that lifecycle (it mints the request id the
    // resolver is parked under; a harness-emitted copy folded an unanswerable
    // duplicate chip into the doc).
    let asked: Arc<Mutex<Vec<UserInputQuestion>>> = Arc::new(Mutex::new(Vec::new()));
    let (steer_tx, steer_rx) = mpsc::channel(8);
    let _steer = steer_tx;
    let token = CancellationToken::new();
    let seen = asked.clone();
    let controls = RunControls {
        request_input: Box::new(move |questions| {
            seen.lock().unwrap().extend(questions.iter().cloned());
            let (tx, rx) = oneshot::channel();
            let answers: Vec<UserInputAnswer> = questions
                .iter()
                .map(|q| UserInputAnswer {
                    question_id: q.id.clone(),
                    labels: vec!["Allow once".into()],
                })
                .collect();
            let _ = tx.send(answers);
            rx
        }),
        steering: steer_rx,
        interrupt: token.clone(),
        invoke_luna_tool: None,
    };
    let mut req = request("scenario:approve");
    req.auto_approve = false;
    let events = run_to_end(&harness(), req, controls).await;

    let asked = asked.lock().unwrap();
    assert_eq!(asked.len(), 2, "{events:?}");
    assert_eq!(asked[0].header, "Approve command");
    assert!(asked[0].question.contains("rm -rf /tmp/x"));
    assert_eq!(
        asked[0].options,
        vec!["Deny", "Allow once", "Allow for task", "Cancel"]
    );
    assert_eq!(
        asked[0].approval.as_ref().unwrap().provider,
        HarnessId::Codex
    );
    assert_eq!(asked[1].header, "Approve file change");
    assert!(asked[1].question.contains("/tmp/a.rs"));
    assert!(
        !events.iter().any(|e| matches!(
            e,
            AgentEvent::InputRequested { .. } | AgentEvent::InputResolved { .. }
        )),
        "harness must not emit input lifecycle events itself: {events:?}"
    );

    // The fake only completes the turn after seeing BOTH accept decisions.
    assert_eq!(
        events.last(),
        Some(&AgentEvent::Done {
            status: DoneStatus::Completed,
            result: None,
            error: None,
            session_id: Some("th-1".into()),
        })
    );
}

#[tokio::test]
async fn approval_no_answer_becomes_decline() {
    let (controls, _steer, _token) = controls("Deny");
    let mut req = request("scenario:decline");
    req.auto_approve = false;
    let events = run_to_end(&harness(), req, controls).await;

    // The fake only completes the turn after seeing the decline decision.
    assert!(
        matches!(
            events.last(),
            Some(AgentEvent::Done {
                status: DoneStatus::Completed,
                ..
            })
        ),
        "{events:?}"
    );
}

#[tokio::test]
async fn computer_use_approval_and_provider_evidence_stay_on_exact_turn() {
    let (controls, _steer, _token) = controls("Allow once");
    let home = tempfile::tempdir().unwrap();
    let events = run_to_end(
        &harness(),
        computer_use_request("scenario:computer-use:allow", home.path()),
        controls,
    )
    .await;
    let calls: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolCall {
                call:
                    ToolCall::ComputerUse {
                        status,
                        app,
                        screenshot_observed,
                        ..
                    },
                ..
            } => Some((*status, app.as_deref(), *screenshot_observed)),
            _ => None,
        })
        .collect();
    assert_eq!(
        calls,
        vec![
            (
                zeron_proto::ComputerUseStatus::Running,
                Some("Finder"),
                false
            ),
            (
                zeron_proto::ComputerUseStatus::ProviderCompleted,
                Some("Finder"),
                true
            ),
        ],
        "{events:?}"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, AgentEvent::ToolResult { .. }))
            .count(),
        1
    );
    assert!(!format!("{events:?}").contains("private pixels"));
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Done {
            status: DoneStatus::Completed,
            ..
        })
    ));
}

#[tokio::test]
async fn computer_use_denial_has_no_computer_action() {
    let (controls, _steer, _token) = controls("Deny");
    let home = tempfile::tempdir().unwrap();
    let events = run_to_end(
        &harness(),
        computer_use_request("scenario:computer-use:deny", home.path()),
        controls,
    )
    .await;
    assert!(
        !events.iter().any(|event| matches!(
            event,
            AgentEvent::ToolCall {
                call: ToolCall::ComputerUse { .. },
                ..
            }
        )),
        "{events:?}"
    );
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Done {
            status: DoneStatus::Completed,
            ..
        })
    ));
}

#[tokio::test]
async fn computer_use_without_luna_task_binding_remains_unavailable() {
    let (controls, _steer, _token) = controls("Allow once");
    let home = tempfile::tempdir().unwrap();
    let events = run_to_end(
        &harness(),
        isolated_computer_request("scenario:computer-use:allow", home.path()),
        controls,
    )
    .await;
    assert!(
        !events.iter().any(|event| matches!(
            event,
            AgentEvent::ToolCall {
                call: ToolCall::ComputerUse { .. },
                ..
            }
        )),
        "{events:?}"
    );
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Done {
            status: DoneStatus::Errored,
            ..
        })
    ));
}

#[tokio::test]
async fn provider_permission_event_uses_exact_turn_and_one_request_grant() {
    let home = tempfile::tempdir().unwrap();
    for (scenario, choice) in [
        ("scenario:computer-use:permission-allow", "Allow once"),
        ("scenario:computer-use:permission-deny", "Deny"),
    ] {
        let (controls, _steer, _token) = controls(choice);
        let events = run_to_end(
            &harness(),
            computer_use_request(scenario, home.path()),
            controls,
        )
        .await;
        assert!(
            matches!(
                events.last(),
                Some(AgentEvent::Done {
                    status: DoneStatus::Completed,
                    ..
                })
            ),
            "{scenario}: {events:?}"
        );
    }
}

fn isolated_computer_request(prompt: &str, home: &std::path::Path) -> RunRequest {
    let mut request = request(prompt);
    request.model_options.insert(
        zeron_harness::codex::LUNA_CODEX_HOME_OPTION.into(),
        home.to_string_lossy().to_string().into(),
    );
    request
}

fn computer_use_request(prompt: &str, home: &std::path::Path) -> RunRequest {
    let mut request = isolated_computer_request(prompt, home);
    let mut contract = synthetic_runtime_contract();
    contract.tools.clear();
    request.model_options.insert(
        zeron_proto::LUNA_RUNTIME_CONTRACT_OPTION.into(),
        serde_json::to_value(contract).unwrap(),
    );
    request
}

type ParkedApproval = (
    Vec<UserInputQuestion>,
    oneshot::Sender<Vec<UserInputAnswer>>,
);

fn parked_computer_controls() -> (
    RunControls,
    mpsc::UnboundedReceiver<ParkedApproval>,
    mpsc::Sender<SteerMessage>,
) {
    let (approval_tx, approval_rx) = mpsc::unbounded_channel();
    let (steer_tx, steer_rx) = mpsc::channel(8);
    let controls = RunControls {
        request_input: Box::new(move |questions| {
            let (tx, rx) = oneshot::channel();
            let _ = approval_tx.send((questions, tx));
            rx
        }),
        steering: steer_rx,
        interrupt: CancellationToken::new(),
        invoke_luna_tool: None,
    };
    (controls, approval_rx, steer_tx)
}

async fn computer_use_answer_after_barrier(scenario: &str, barrier: &str) -> Vec<AgentEvent> {
    let home = tempfile::tempdir().unwrap();
    let (controls, mut approvals, _steer) = parked_computer_controls();
    let mut stream = harness()
        .run(computer_use_request(scenario, home.path()), controls)
        .await
        .unwrap();
    let (questions, response) = tokio::time::timeout(Duration::from_secs(5), approvals.recv())
        .await
        .expect("provider approval requested")
        .expect("approval channel open");
    assert_eq!(questions.len(), 1);
    assert_eq!(
        questions[0].approval.as_ref().unwrap().turn_id.as_deref(),
        Some("t2")
    );
    let mut events = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(5), stream.next())
            .await
            .expect("barrier event arrives")
            .expect("stream stays open through barrier")
            .expect("event succeeds");
        let reached = matches!(&event, AgentEvent::TextDelta { text } if text == barrier);
        events.push(event);
        if reached {
            break;
        }
    }
    // The provider's old-turn notifications have already passed through the
    // adapter before this answer; a fast callback cannot mask the race.
    let _ = response.send(vec![UserInputAnswer {
        question_id: questions[0].id.clone(),
        labels: vec!["Allow once".into()],
    }]);
    let tail = tokio::time::timeout(
        Duration::from_secs(10),
        stream
            .map(|event| event.expect("event succeeds"))
            .collect::<Vec<_>>(),
    )
    .await
    .expect("synthetic stream settles");
    events.extend(tail);
    events
}

#[tokio::test]
async fn stale_t1_lifecycle_cannot_cancel_t2_approval_or_computer_call() {
    for scenario in ["start", "completed", "failed", "aborted"] {
        let prompt = format!("scenario:computer-use:late-t1-{scenario}");
        let events = computer_use_answer_after_barrier(&prompt, "stale-delivered").await;
        let statuses: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                AgentEvent::ToolCall {
                    call:
                        ToolCall::ComputerUse {
                            status, turn_id, ..
                        },
                    ..
                } if turn_id == "t2" => Some(*status),
                _ => None,
            })
            .collect();
        assert_eq!(
            statuses,
            vec![
                zeron_proto::ComputerUseStatus::Running,
                zeron_proto::ComputerUseStatus::ProviderCompleted
            ],
            "{scenario}: {events:?}"
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event,
                    AgentEvent::Done {
                        status: DoneStatus::Completed,
                        ..
                    }
                ))
                .count(),
            2,
            "{scenario}: {events:?}"
        );
        assert!(
            !events.iter().any(|event| matches!(
                event,
                AgentEvent::Done {
                    status: DoneStatus::Errored | DoneStatus::Interrupted,
                    ..
                }
            )),
            "{scenario}: {events:?}"
        );
    }
}

#[tokio::test]
async fn next_turn_response_owns_approval_before_started_notification() {
    let home = tempfile::tempdir().unwrap();
    let (controls, steer, _token) = controls("Allow once");
    let mut stream = harness()
        .run(
            computer_use_request("scenario:computer-use:response-before-start", home.path()),
            controls,
        )
        .await
        .unwrap();
    let mut events = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(5), stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let first_done = matches!(
            &event,
            AgentEvent::Done {
                status: DoneStatus::Completed,
                ..
            }
        );
        events.push(event);
        if first_done {
            break;
        }
    }
    steer
        .send(SteerMessage {
            prompt: "follow-up t2".into(),
            message_id: None,
            delivery: zeron_harness::SteerDelivery::AllowFollowUpTurn,
            acceptance: None,
        })
        .await
        .unwrap();
    events.extend(
        tokio::time::timeout(
            Duration::from_secs(10),
            stream.map(|event| event.unwrap()).collect::<Vec<_>>(),
        )
        .await
        .unwrap(),
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                event,
                AgentEvent::Done {
                    status: DoneStatus::Completed,
                    ..
                }
            ))
            .count(),
        2,
        "{events:?}"
    );
    assert!(
        !events.iter().any(|event| matches!(
            event,
            AgentEvent::Done {
                status: DoneStatus::Errored,
                ..
            }
        )),
        "{events:?}"
    );
}

#[tokio::test]
async fn ended_t2_withholds_a_delayed_allow_once_answer() {
    let events =
        computer_use_answer_after_barrier("scenario:computer-use:late-answer", "turn-ended").await;
    assert!(
        events.contains(&AgentEvent::TextDelta {
            text: "late-approval-withheld".into()
        }),
        "{events:?}"
    );
    assert!(
        !events.iter().any(|event| matches!(
            event,
            AgentEvent::ToolCall {
                call: ToolCall::ComputerUse { .. },
                ..
            }
        )),
        "{events:?}"
    );
}

#[tokio::test]
async fn computer_use_disconnect_reports_unknown_without_replay() {
    let (controls, _steer, _token) = controls("Deny");
    let home = tempfile::tempdir().unwrap();
    let events = run_to_end(
        &harness(),
        computer_use_request("scenario:computer-use:disconnect", home.path()),
        controls,
    )
    .await;
    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::ToolCall {
                call: ToolCall::ComputerUse {
                    status: zeron_proto::ComputerUseStatus::Unknown,
                    ..
                },
                ..
            }
        )),
        "{events:?}"
    );
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Done {
            status: DoneStatus::Errored,
            ..
        })
    ));
}

#[tokio::test]
async fn computer_use_interrupt_reports_unknown_without_provider_success() {
    let (controls, _steer, token) = controls("Deny");
    let home = tempfile::tempdir().unwrap();
    let mut stream = harness()
        .run(
            computer_use_request("scenario:computer-use:cancel", home.path()),
            controls,
        )
        .await
        .unwrap();
    let mut events = Vec::new();
    while let Some(event) = tokio::time::timeout(Duration::from_secs(10), stream.next())
        .await
        .unwrap()
    {
        let event = event.unwrap();
        let started = matches!(
            &event,
            AgentEvent::ToolCall {
                call: ToolCall::ComputerUse {
                    status: zeron_proto::ComputerUseStatus::Running,
                    ..
                },
                ..
            }
        );
        events.push(event);
        if started {
            token.cancel();
        }
    }
    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::ToolCall {
                call: ToolCall::ComputerUse {
                    status: zeron_proto::ComputerUseStatus::Unknown,
                    ..
                },
                ..
            }
        )),
        "{events:?}"
    );
    assert!(
        !events.iter().any(|event| matches!(
            event,
            AgentEvent::ToolCall {
                call: ToolCall::ComputerUse {
                    status: zeron_proto::ComputerUseStatus::ProviderCompleted,
                    ..
                },
                ..
            }
        )),
        "a completion after interrupt must not claim provider completion: {events:?}"
    );
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Done {
            status: DoneStatus::Interrupted,
            ..
        })
    ));
}

#[tokio::test]
async fn interrupt_sends_turn_interrupt_and_maps_aborted() {
    let (controls, _steer, token) = controls("Yes");
    let mut stream = harness()
        .run(request("scenario:interrupt"), controls)
        .await
        .expect("run starts");

    let events = tokio::time::timeout(Duration::from_secs(10), async move {
        let mut events = Vec::new();
        while let Some(ev) = stream.next().await {
            let ev = ev.expect("stream event");
            if matches!(&ev, AgentEvent::TextDelta { text } if text == "working") {
                token.cancel(); // interrupt mid-turn
            }
            events.push(ev);
        }
        events
    })
    .await
    .expect("interrupt completed in time");

    assert_eq!(
        events.last(),
        Some(&AgentEvent::Done {
            status: DoneStatus::Interrupted,
            result: None,
            error: None,
            session_id: Some("th-1".into()),
        })
    );
}

#[tokio::test]
async fn unresponsive_child_is_reaped_with_interrupted_done() {
    let harness = CodexHarness::new()
        .with_executable(fixture_path())
        .with_graces(Duration::from_millis(100), Duration::from_millis(500));
    let (controls, _steer, token) = controls("Yes");
    let mut stream = harness
        .run(request("scenario:wedge"), controls)
        .await
        .expect("run starts");

    let events = tokio::time::timeout(Duration::from_secs(10), async move {
        let mut events = Vec::new();
        while let Some(ev) = stream.next().await {
            let ev = ev.expect("stream event");
            if matches!(&ev, AgentEvent::TextDelta { text } if text == "working") {
                token.cancel();
            }
            events.push(ev);
        }
        events
    })
    .await
    .expect("escalation completed in time");

    assert_eq!(
        events.last(),
        Some(&AgentEvent::Done {
            status: DoneStatus::Interrupted,
            result: None,
            error: None,
            session_id: Some("th-1".into()),
        })
    );
}

#[tokio::test]
async fn turn_failed_maps_to_errored_done() {
    let (controls, _steer, _token) = controls("Yes");
    let events = run_to_end(&harness(), request("scenario:fail"), controls).await;
    assert_eq!(
        events.last(),
        Some(&AgentEvent::Done {
            status: DoneStatus::Errored,
            result: None,
            error: Some("boom".into()),
            session_id: Some("th-1".into()),
        })
    );
}

#[tokio::test]
async fn resume_falls_back_to_fresh_thread() {
    let (controls, _steer, _token) = controls("Yes");
    let mut req = request("scenario:resumed");
    req.resume = Some("resume-fail".into());
    let events = run_to_end(&harness(), req, controls).await;

    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::SessionStarted { session_id, .. } if session_id == "th-fresh"
        )),
        "fresh thread expected: {events:?}"
    );
    assert_eq!(
        events.last(),
        Some(&AgentEvent::Done {
            status: DoneStatus::Completed,
            result: None,
            error: None,
            session_id: Some("th-fresh".into()),
        })
    );
}

#[tokio::test]
async fn resume_reuses_the_existing_thread() {
    let (controls, _steer, _token) = controls("Yes");
    let mut req = request("scenario:resumed");
    req.resume = Some("resume-ok".into());
    let events = run_to_end(&harness(), req, controls).await;

    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::SessionStarted { session_id, .. } if session_id == "th-resumed"
        )),
        "resumed thread expected: {events:?}"
    );
}

#[tokio::test]
async fn missing_binary_is_not_installed() {
    let harness = CodexHarness::new().with_executable("/nonexistent/codex-nowhere");
    let (controls, _steer, _token) = controls("Yes");
    let err = harness
        .run(request("scenario:happy"), controls)
        .await
        .err()
        .expect("spawn fails");
    assert!(matches!(err, HarnessError::NotInstalled(_)), "{err:?}");
}

#[tokio::test]
async fn models_discovers_visible_catalog_with_pagination() {
    let models = harness().models().await.expect("models");
    assert_eq!(models.len(), 3);
    assert_eq!(models[0].id, "gpt-6-astra");
    assert_eq!(models[1].id, "gpt-5.6-terra");
    assert_eq!(models[2].id, "gpt-5.6-sol");
    assert!(models[0].reasoning_levels.contains(&ReasoningLevel::Ultra));
    assert!(models.iter().any(|m| m.id == "gpt-5.6-sol"));
    let tier = models[0]
        .options
        .iter()
        .find(|option| option.id == "serviceTier")
        .expect("Astra service tier");
    assert_eq!(tier.choices[0].id, "default");
    assert_eq!(tier.choices[1].id, "fast");
    assert_eq!(tier.choices.len(), 2, "priority and fast dedupe");

    // A failed probe stays useful and includes the new model in the fallback.
    let fallback = CodexHarness::new()
        .with_executable("/bin/false")
        .models()
        .await
        .expect("fallback models");
    assert_eq!(fallback.len(), 9);
    assert_eq!(fallback[0].id, "gpt-6-astra");

    let missing = CodexHarness::new().with_executable("/nonexistent/codex-nowhere");
    // models() requires a resolvable binary… but with_executable trusts the
    // caller's path, so only the default resolution can report NotInstalled —
    // exercise the harness identity surface instead.
    assert_eq!(missing.id(), HarnessId::Codex);
    // "Codex" — comet composer/defaults.ts HARNESS_LABEL (and the registry's
    // lazy descriptor must stay in lockstep).
    assert_eq!(missing.display_name(), "Codex");
    assert_eq!(missing.reasoning_levels().len(), 7);
}

#[tokio::test]
async fn resumed_parent_recovers_v1_and_v2_child_owners_without_replaying_chips() {
    for mode in ["v1", "v2"] {
        let mut req = request("scenario:resumed-child");
        req.resume = Some(format!("resume-with-child-{mode}"));
        let (controls, _steer, _token) = controls("Yes");
        let events = run_to_end(&harness(), req, controls).await;
        assert!(
            !events.iter().any(
                |e| matches!(e, AgentEvent::ToolCall { call, .. } if call.is_subagent_spawn())
            )
        );
        assert!(events.iter().any(|e| matches!(e,
            AgentEvent::Subagent { parent_tool_use_id, event }
            if parent_tool_use_id == "spawn-alpha" && matches!(event.as_ref(), AgentEvent::TextDelta { text } if text == "resumed alpha")
        )), "{mode}: {events:?}");
    }
}

#[tokio::test]
async fn v2_lifecycle_reuses_chips_and_reopens_the_same_child_for_followup() {
    let (controls, _steer, _token) = controls("Yes");
    let events = run_to_end(&harness(), request("scenario:v2-lifecycle"), controls).await;
    let spawns: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolCall { id, call } if call.is_subagent_spawn() => Some(id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(spawns, ["spawn-alpha", "spawn-beta"]);
    let mut alpha_text = String::new();
    let mut alpha_users = Vec::new();
    let mut alpha_done = Vec::new();
    for e in &events {
        if let AgentEvent::Subagent {
            parent_tool_use_id,
            event,
        } = e
        {
            assert!(spawns.contains(&parent_tool_use_id.as_str()));
            if parent_tool_use_id == "spawn-alpha" {
                match event.as_ref() {
                    AgentEvent::TextDelta { text } => alpha_text.push_str(text),
                    AgentEvent::UserMessage { text } => alpha_users.push(text.as_str()),
                    AgentEvent::Done { status, error, .. } => {
                        alpha_done.push((*status, error.as_deref()))
                    }
                    _ => {}
                }
            }
        }
    }
    assert_eq!(alpha_text, "first alpha\n\nsecond alpha\n\n");
    assert_eq!(alpha_users, ["First assignment"]);
    assert_eq!(
        alpha_done,
        [
            (DoneStatus::Completed, None),
            (DoneStatus::Errored, Some("followup failed"))
        ]
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AgentEvent::Done { .. }))
            .count(),
        1
    );
}

#[tokio::test]
async fn v1_spawns_bind_children_and_controls_do_not_create_agents() {
    let (controls, _steer, _token) = controls("Yes");
    let events = run_to_end(&harness(), request("scenario:v1-subagents"), controls).await;
    let spawns: std::collections::HashSet<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolCall { id, call } if call.is_subagent_spawn() => Some(id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        spawns,
        std::collections::HashSet::from(["spawn-alpha", "spawn-beta"])
    );
    for e in &events {
        if let AgentEvent::Subagent {
            parent_tool_use_id, ..
        } = e
        {
            assert!(spawns.contains(parent_tool_use_id.as_str()), "{e:?}");
        }
    }
    for (owner, text) in [
        ("spawn-alpha", "alpha answer"),
        ("spawn-beta", "beta answer"),
    ] {
        assert!(events.iter().any(|e| matches!(e,
            AgentEvent::Subagent { parent_tool_use_id, event }
            if parent_tool_use_id == owner && matches!(event.as_ref(), AgentEvent::TextDelta { text: t } if t == text)
        )));
    }
    assert!(events.iter().any(|e| matches!(e,
        AgentEvent::Subagent { parent_tool_use_id, event }
        if parent_tool_use_id == "spawn-beta" && matches!(event.as_ref(), AgentEvent::ToolCall { id, .. } if id == "beta-tool")
    )));
    assert!(events.iter().any(|e| matches!(e,
        AgentEvent::Subagent { parent_tool_use_id, event }
        if parent_tool_use_id == "spawn-beta" && matches!(event.as_ref(), AgentEvent::UserMessage { text } if text == "Also check gamma")
    )));
    assert_eq!(events.iter().filter(|e| matches!(e, AgentEvent::Subagent { event, .. } if matches!(event.as_ref(), AgentEvent::Done { .. }))).count(), 2);
    let parent: String = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::TextDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(parent, "parent answer");
}

#[tokio::test]
async fn child_identity_survives_early_output_and_later_activity_ids() {
    let (controls, _steer, _token) = controls("Yes");
    let events = run_to_end(&harness(), request("scenario:child-identity"), controls).await;
    let spawn = events
        .iter()
        .position(|e| {
            matches!(e,
                AgentEvent::ToolCall { id, .. } if id == "spawn-alpha"
            )
        })
        .unwrap();
    let early = events.iter().position(|e| matches!(e,
        AgentEvent::Subagent { parent_tool_use_id, event }
        if parent_tool_use_id == "spawn-alpha"
            && matches!(event.as_ref(), AgentEvent::TextDelta { text } if text == "early alpha")
    )).unwrap();
    assert!(
        spawn < early,
        "the chip must exist before buffered traffic binds"
    );
    let mut alpha = String::new();
    let mut beta = String::new();
    for e in &events {
        if let AgentEvent::Subagent {
            parent_tool_use_id,
            event,
        } = e
        {
            assert!(matches!(
                parent_tool_use_id.as_str(),
                "spawn-alpha" | "spawn-beta"
            ));
            if let AgentEvent::TextDelta { text } = event.as_ref() {
                if parent_tool_use_id == "spawn-alpha" {
                    alpha.push_str(text);
                } else {
                    beta.push_str(text);
                }
            }
        }
    }
    assert_eq!(alpha, "early alphalater alpha");
    assert_eq!(beta, "beta outputbeta continues");
    let parent: String = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::TextDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(parent, "parent output");
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AgentEvent::Done { .. }))
            .count(),
        1
    );
}

#[tokio::test]
async fn child_thread_routing_tags_and_never_settles_parent() {
    let (controls, _steer, _token) = controls("Yes");
    let events = run_to_end(&harness(), request("scenario:subagent"), controls).await;

    // Exactly one Done — the child's turn/completed must NOT settle the
    // parent turn (the swallowed-catch-all bug class this table exists for).
    let dones: Vec<usize> = events
        .iter()
        .enumerate()
        .filter_map(|(i, e)| matches!(e, AgentEvent::Done { .. }).then_some(i))
        .collect();
    assert_eq!(dones.len(), 1, "one parent Done only: {events:?}");

    // Parent output that follows the child's turn/completed still streams.
    let late_parent = events
        .iter()
        .position(|e| matches!(e, AgentEvent::TextDelta { text } if text == "parent still going"))
        .expect("parent delta after child turn end");
    assert!(late_parent < dones[0]);

    // The spawn chip lives on the parent feed, named from the agent path,
    // and resolves when the activity completes.
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::ToolCall { id, call: ToolCall::Unknown { name, .. } }
            if id == "call_alpha" && name == "Agent: alpha"
    )));
    assert!(events.iter().any(
        |e| matches!(e, AgentEvent::ToolResult { id, is_error: false, .. } if id == "call_alpha")
    ));
    // The root's own subAgentActivity produces no chip.
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolCall { id, .. } if id == "call_root")),
        "root self-activity must not register or render: {events:?}"
    );

    // Child deltas and items arrive tagged with the spawn call id — never
    // bare (child threads stream deltas on this wire; live-verified 0.146.1).
    assert!(events.contains(&AgentEvent::Subagent {
        parent_tool_use_id: "call_alpha".into(),
        event: Box::new(AgentEvent::TextDelta {
            text: "child says hi".into()
        }),
    }));
    assert!(events.contains(&AgentEvent::Subagent {
        parent_tool_use_id: "call_alpha".into(),
        event: Box::new(AgentEvent::ToolCall {
            id: "cs1".into(),
            call: ToolCall::Exec {
                command: "echo hi".into()
            },
        }),
    }));
    assert!(events.contains(&AgentEvent::Subagent {
        parent_tool_use_id: "call_alpha".into(),
        event: Box::new(AgentEvent::ToolResult {
            id: "cs1".into(),
            is_error: false,
            output: None,
            diff: None,
        }),
    }));
    // The parent's steer (a userMessage item on the CHILD thread) arrives as
    // exactly one tagged UserMessage — completed only, never doubled by the
    // started lifecycle event, never leaked untagged.
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(
                e,
                AgentEvent::Subagent { parent_tool_use_id, event }
                    if parent_tool_use_id == "call_alpha"
                        && matches!(event.as_ref(), AgentEvent::UserMessage { text } if text == "also check the rebuild")
            ))
            .count(),
        1,
        "{events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::UserMessage { .. })),
        "steer leaked into the parent feed: {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolCall { id, .. } if id == "cs1")),
        "child tool call leaked into the parent feed: {events:?}"
    );

    // The child's turn/completed (and later thread/closed) become tagged
    // terminal events — real fan-outs never call close_agent, so the turn
    // end is what flips the chip off "running".
    assert!(
        events
            .iter()
            .filter(|e| matches!(
                e,
                AgentEvent::Subagent { parent_tool_use_id, event }
                    if parent_tool_use_id == "call_alpha"
                        && matches!(event.as_ref(), AgentEvent::Done { status: DoneStatus::Completed, .. })
            ))
            .count()
            >= 1,
        "{events:?}"
    );
    // The tagged terminal must arrive from turn/completed — BEFORE the
    // parent delta that follows it in the script (not only at thread/closed).
    let child_done = events
        .iter()
        .position(|e| {
            matches!(
                e,
                AgentEvent::Subagent { parent_tool_use_id, event }
                    if parent_tool_use_id == "call_alpha"
                        && matches!(event.as_ref(), AgentEvent::Done { .. })
            )
        })
        .expect("tagged done");
    let late_parent_delta = events
        .iter()
        .position(|e| matches!(e, AgentEvent::TextDelta { text } if text == "parent still going"))
        .expect("parent delta");
    assert!(child_done < late_parent_delta, "{events:?}");
}

/// Run once per multi-agent mode using a wrapper supplied via
/// CODEX_SUBAGENT_TEST_EXECUTABLE. The wrapper can set feature flags for its
/// process without changing the user's config. This deliberately consumes
/// model calls and is never part of the offline suite.
#[tokio::test]
#[ignore = "real Codex spawn, followup and resume; needs install, auth and network"]
async fn live_subagent_spawn_and_followup_keep_one_transcript() {
    let executable = std::env::var_os("CODEX_SUBAGENT_TEST_EXECUTABLE")
        .expect("set CODEX_SUBAGENT_TEST_EXECUTABLE to a wrapper selecting v1 or v2");
    let harness = CodexHarness::new().with_executable(executable);
    let cwd = tempfile::tempdir().unwrap();
    let mut req = request(
        "This is an integration smoke test. Spawn EXACTLY ONE subagent (name it alpha if naming is supported). Its entire task is: reply exactly child-first. It must not use any tools or spawn agents. Wait for it to finish, then reply exactly parent-first. Do not close the child; we will reuse it. Do not inspect or change any files.",
    );
    req.cwd = cwd.path().display().to_string();
    req.reasoning = Some(ReasoningLevel::Low);
    if let Ok(model) = std::env::var("CODEX_SUBAGENT_TEST_MODEL") {
        req.model = Some(model);
    }
    let (run_controls, mut steer, mut interrupt) = controls("Yes");
    let mut stream = harness
        .run(req.clone(), run_controls)
        .await
        .expect("run starts");
    let mut events = Vec::new();
    for turn in 0..3 {
        if turn == 1 {
            steer.send(SteerMessage {
                prompt: "Reuse the SAME existing subagent for one more task: reply exactly child-second. Use followup_task if available, otherwise send_input. Do not spawn a new agent. Wait for it to finish, then reply exactly parent-second. Do not inspect or change files.".into(),
                message_id: None,
                delivery: zeron_harness::SteerDelivery::AllowFollowUpTurn,
                acceptance: None,
            }).await.unwrap();
        }
        if turn == 2 {
            // End the first app-server process while idle, then resume the
            // parent in a fresh process and address its already-known child.
            interrupt.cancel();
            tokio::time::timeout(Duration::from_secs(10), async {
                while stream.next().await.is_some() {}
            })
            .await
            .expect("old app-server stops");
            req.resume = events.iter().find_map(|e| match e {
                AgentEvent::SessionStarted { session_id, .. } => Some(session_id.clone()),
                _ => None,
            });
            req.prompt = "The app-server has restarted. Reuse the SAME existing subagent again: reply exactly child-third. Use followup_task if available. Otherwise first use resume_agent with the existing child's id to reactivate it, then send_input. Do not spawn another agent. Wait for its actual child-third reply before replying exactly parent-third. If a tool fails, recover using the same child id; never claim the child finished without its reply. Do not inspect or change files.".into();
            let (run_controls, new_steer, new_interrupt) = controls("Yes");
            steer = new_steer;
            interrupt = new_interrupt;
            stream = harness
                .run(req.clone(), run_controls)
                .await
                .expect("parent resumes");
        }
        let result = tokio::time::timeout(Duration::from_secs(120), async {
            while let Some(event) = stream.next().await {
                let event = event.expect("stream event");
                let done = matches!(event, AgentEvent::Done { .. });
                let failed = matches!(
                    event,
                    AgentEvent::Done {
                        status: DoneStatus::Errored | DoneStatus::Interrupted,
                        ..
                    }
                );
                events.push(event);
                assert!(!failed, "parent failed: {:?}", events.last());
                if done {
                    return;
                }
            }
            panic!("stream ended before parent completion");
        })
        .await;
        if result.is_err() {
            interrupt.cancel();
        }
        result.expect("live turn finishes within 120 seconds");
    }
    drop(stream);
    if let Some(path) = std::env::var_os("CODEX_SUBAGENT_TEST_CAPTURE") {
        std::fs::write(path, serde_json::to_vec_pretty(&events).unwrap()).unwrap();
    }
    let spawns: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolCall { id, call } if call.is_subagent_spawn() => Some(id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(spawns.len(), 1, "one original spawn: {spawns:?}");
    if let Ok(mode) = std::env::var("CODEX_SUBAGENT_TEST_MODE") {
        let expected = match mode.as_str() {
            "v1" => "collabAgentToolCall",
            "v2" => "subAgentActivity",
            _ => panic!("unknown mode {mode}"),
        };
        assert!(
            events.iter().any(|e| matches!(e,
                AgentEvent::ToolCall { call: ToolCall::Unknown { input: Some(input), .. }, .. }
                if input.get("type").and_then(serde_json::Value::as_str) == Some(expected)
            )),
            "the model must actually use {mode}"
        );
    }
    let mut text = String::new();
    let mut terminals = 0;
    for event in &events {
        if let AgentEvent::Subagent {
            parent_tool_use_id,
            event,
        } = event
        {
            assert_eq!(parent_tool_use_id, spawns[0]);
            if let AgentEvent::TextDelta { text: delta } = event.as_ref() {
                text.push_str(delta);
            }
            if matches!(
                event.as_ref(),
                AgentEvent::Done {
                    status: DoneStatus::Completed,
                    ..
                }
            ) {
                terminals += 1;
            }
        }
    }
    for reply in ["child-first", "child-second", "child-third"] {
        assert_eq!(text.matches(reply).count(), 1, "child transcript: {text:?}");
    }
    assert_eq!(terminals, 3, "one child completion per assignment");
}

/// Live smoke against the REAL codex app-server (installed + authed):
/// one trivial turn, ending on turn/completed.
/// `cargo test -p zeron-harness --test codex -- --ignored`.
#[tokio::test]
#[ignore = "spawns the real codex app-server; needs install + auth + network"]
async fn live_real_app_server_single_turn() {
    let harness = CodexHarness::new();
    let mut req = request("Reply with exactly the word: pong");
    req.cwd = std::env::temp_dir().display().to_string();
    let (controls, _steer, _token) = controls("Yes");
    let mut stream = harness.run(req, controls).await.expect("run starts");
    // The session parks after the turn (steering mailbox open) — collect up
    // to the first Done, not stream end.
    let events = tokio::time::timeout(Duration::from_secs(120), async {
        let mut events = Vec::new();
        while let Some(ev) = stream.next().await {
            let ev = ev.expect("stream event");
            let done = matches!(ev, AgentEvent::Done { .. });
            events.push(ev);
            if done {
                break;
            }
        }
        events
    })
    .await
    .expect("live turn finished in time");
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::SessionStarted { .. })),
        "{events:?}"
    );
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Done {
            status: DoneStatus::Completed,
            ..
        })
    ));
}

// ---------------------------------------------------------------------------
// Slash-command discovery
// ---------------------------------------------------------------------------

#[tokio::test]
async fn commands_come_from_skills_list() {
    let h = harness();
    let commands = h.commands().await.expect("discovery succeeds");
    assert_eq!(
        commands.len(),
        2,
        "same-name skills across cwd groups dedupe: {commands:?}"
    );
    assert_eq!(commands[0].name, "imagegen");
    assert_eq!(
        commands[0].description, "Generate or edit images",
        "interface.shortDescription wins over the model-facing paragraph"
    );
    assert_eq!(commands[1].name, "bare");
    assert_eq!(
        commands[1].description, "No interface block",
        "top-level description is the fallback"
    );
    assert_eq!(h.commands().await.expect("cache hit"), commands);
}

#[tokio::test]
async fn skill_observations_keep_duplicate_names_in_separate_cwd_groups() {
    let observed = harness()
        .discover_skill_observations()
        .await
        .expect("synthetic skills/list succeeds");
    assert!(!observed.omitted);
    let pairs = observed
        .entries
        .iter()
        .map(|entry| (entry.cwd.as_str(), entry.name.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(
        pairs,
        [("/w", "imagegen"), ("/w", "bare"), ("/x", "imagegen"),]
    );
}

#[tokio::test]
async fn native_skill_catalog_uses_isolated_home_and_exact_workspace() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("codex-home");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let client = CodexNativeClient::new(&home).with_executable(fixture_path());
    let entries = client.read_skill_catalog(&workspace).await.unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].name, "imagegen");
    assert!(entries[0].enabled);
    assert_eq!(
        entries[0].path,
        workspace
            .canonicalize()
            .unwrap()
            .join(".agents/skills/imagegen/SKILL.md")
    );
    assert!(!entries[1].enabled);

    std::fs::write(home.join("skill-catalog-fail"), "").unwrap();
    assert!(client.read_skill_catalog(&workspace).await.is_err());
}

#[tokio::test]
async fn native_mcp_config_keeps_commands_and_tokens_out_of_inventory() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("codex-home");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let client = CodexNativeClient::new(&home).with_executable(fixture_path());
    let entries = client.read_mcp_config(&workspace).await.unwrap();
    assert_eq!(entries.len(), 2);
    assert!(
        entries
            .iter()
            .find(|entry| entry.name == "test")
            .unwrap()
            .enabled
    );
    assert!(
        !entries
            .iter()
            .find(|entry| entry.name == "off")
            .unwrap()
            .enabled
    );
    let debug = format!("{entries:?}");
    assert!(!debug.contains("SECRET_COMMAND_SENTINEL"));
    assert!(!debug.contains("SECRET_TOKEN_SENTINEL"));

    std::fs::write(home.join("mcp-config-fail"), "").unwrap();
    assert!(client.read_mcp_config(&workspace).await.is_err());
}

#[tokio::test]
async fn native_mcp_config_uses_each_selected_project_cwd() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("codex-home");
    let project_a = root.path().join("project-a");
    let project_b = root.path().join("project-b");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&project_a).unwrap();
    std::fs::create_dir_all(&project_b).unwrap();
    std::fs::write(home.join("mcp-config-trace"), "").unwrap();
    std::fs::write(project_a.join(".luna-mcp-project-a"), "").unwrap();
    std::fs::write(project_b.join(".luna-mcp-project-b"), "").unwrap();
    let client = CodexNativeClient::new(&home).with_executable(fixture_path());
    for (project, expected) in [(&project_a, "project-a"), (&project_b, "project-b")] {
        let entries = client.read_mcp_config(project).await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, expected);
        assert_eq!(
            std::fs::read_to_string(home.join("mcp-config-last-cwd"))
                .unwrap()
                .trim(),
            project.canonicalize().unwrap().to_str().unwrap()
        );
        let request = std::fs::read_to_string(home.join("mcp-config-last-request")).unwrap();
        assert!(request.contains("\"method\":\"config/read\""));
        assert!(request.contains("\"includeLayers\":false"));
    }
}

#[test]
fn skill_observation_parser_ignores_unknown_and_malformed_payloads() {
    use zeron_harness::codex::observe_skill_discovery;

    let missing = observe_skill_discovery(&serde_json::json!({}));
    assert!(missing.omitted);
    assert!(missing.entries.is_empty());
    let malformed = observe_skill_discovery(&serde_json::json!({"data": {"cwd": "/bad"}}));
    assert!(malformed.omitted);
    assert!(malformed.entries.is_empty());

    let observed = observe_skill_discovery(&serde_json::json!({
        "data": [
            {"cwd": "/synthetic", "skills": [
                {"name": "good", "description": "SECRET_DESCRIPTION_SENTINEL",
                 "auth": {"token": "SECRET_TOKEN_SENTINEL"}},
                {"name": "bad/name"}, {"name": " leading"},
                {"name": 42}, {"other": "unknown"}
            ], "credentials": "SECRET_CREDENTIAL_SENTINEL"},
            {"cwd": "/bad\npath", "skills": [{"name": "skipped"}]},
            {"cwd": "/missing-skills"}
        ],
        "unknown": "SECRET_UNKNOWN_SENTINEL"
    }));
    assert!(observed.omitted);
    assert_eq!(observed.entries.len(), 1);
    assert_eq!(observed.entries[0].cwd, "/synthetic");
    assert_eq!(observed.entries[0].name, "good");
    let debug = format!("{observed:?}");
    for secret in [
        "SECRET_DESCRIPTION_SENTINEL",
        "SECRET_TOKEN_SENTINEL",
        "SECRET_CREDENTIAL_SENTINEL",
        "SECRET_UNKNOWN_SENTINEL",
    ] {
        assert!(!debug.contains(secret));
    }
}

#[test]
fn skill_observation_parser_bounds_groups_names_and_total_retention() {
    use zeron_harness::codex::observe_skill_discovery;

    let too_many_groups = (0..65)
        .map(|index| serde_json::json!({"cwd": format!("/root-{index}"), "skills": []}))
        .collect::<Vec<_>>();
    let observed = observe_skill_discovery(&serde_json::json!({"data": too_many_groups}));
    assert!(observed.omitted);
    assert!(observed.entries.is_empty());

    let too_many_skills = (0..129)
        .map(|index| serde_json::json!({"name": format!("skill{index}")}))
        .collect::<Vec<_>>();
    let observed = observe_skill_discovery(&serde_json::json!({
        "data": [{"cwd": "/synthetic", "skills": too_many_skills}]
    }));
    assert!(observed.omitted);
    assert!(observed.entries.is_empty());

    let observed = observe_skill_discovery(&serde_json::json!({
        "data": [
            {"cwd": "x".repeat(1_025), "skills": [{"name": "skipped"}]},
            {"cwd": "/synthetic", "skills": [
                {"name": "x".repeat(129)}, {"name": "kept"}
            ]}
        ]
    }));
    assert!(observed.omitted);
    assert_eq!(observed.entries.len(), 1);
    assert_eq!(observed.entries[0].name, "kept");

    let groups = (0..5)
        .map(|group| {
            serde_json::json!({
                "cwd": format!("/group-{group}"),
                "skills": (0..128)
                    .map(|skill| serde_json::json!({"name": format!("skill{skill}")}))
                    .collect::<Vec<_>>()
            })
        })
        .collect::<Vec<_>>();
    let observed = observe_skill_discovery(&serde_json::json!({"data": groups}));
    assert!(observed.omitted);
    assert_eq!(observed.entries.len(), 512);
}

/// Live smoke against the real CLI: `cargo test -p zeron-harness --test
/// codex -- --ignored live_commands`.
#[tokio::test]
#[ignore]
async fn live_commands_discovery() {
    let h = CodexHarness::new();
    let commands = h.commands().await.expect("live discovery");
    eprintln!("{} commands, first: {:?}", commands.len(), commands.first());
}

#[tokio::test]
async fn title_run_preserves_read_only_and_replaces_coding_instructions() {
    let (controls, _steer, token) = controls("Yes");
    let stream = harness()
        .run_title(request("scenario:title"), controls)
        .await
        .unwrap();
    let events = tokio::time::timeout(Duration::from_secs(10), async {
        let mut stream = stream;
        let mut events = Vec::new();
        while let Some(event) = stream.next().await {
            let event = event.unwrap();
            let done = matches!(event, AgentEvent::Done { .. });
            events.push(event);
            if done {
                break;
            }
        }
        events
    })
    .await
    .unwrap();
    token.cancel();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::TextDelta { text } if text == "Fix Login Flow")),
        "{events:?}"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::Done {
                status: DoneStatus::Completed,
                ..
            }
        )),
        "{events:?}"
    );
}

#[tokio::test]
async fn image_generation_fake_lifecycle_reaches_done_without_inline_payload() {
    for scenario in ["success", "failure", "missing-path"] {
        let (controls, _steer, _token) = controls("Yes");
        let events = run_to_end(
            &harness(),
            request(&format!("scenario:image-{scenario}")),
            controls,
        )
        .await;
        assert!(events.iter().any(|e| matches!(e, AgentEvent::Done { .. })));
        let results: Vec<_> = events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    AgentEvent::ToolCall { .. }
                        | AgentEvent::ToolResult { .. }
                        | AgentEvent::GeneratedImage { .. }
                        | AgentEvent::Error { .. }
                )
            })
            .collect();
        assert_eq!(results.len(), 4);
        assert!(matches!(results[0], AgentEvent::ToolCall { .. }));
        assert!(matches!(results[1], AgentEvent::ToolCall { .. }));
        assert!(
            matches!(results[2], AgentEvent::ToolResult { is_error, .. } if *is_error == (scenario != "success"))
        );
        assert_eq!(
            matches!(results[3], AgentEvent::GeneratedImage { .. }),
            scenario == "success"
        );
        assert!(
            !serde_json::to_string(&events)
                .unwrap()
                .contains("INLINE_IMAGE_SENTINEL")
        );
    }
}

/// Opt-in provider smoke; the engine integration test verifies the subsequent
/// import lands under profile uploads. See docs/generated-images-validation.md.
#[tokio::test]
#[ignore = "consumes image quota; requires real Codex auth and image generation access"]
async fn real_image_generation_smoke() {
    let (controls, _steer, token) = controls("Yes");
    let mut req = request(
        "Use image generation to create a small green goblin portrait. Generate an image, not text or code.",
    );
    req.model = None;
    req.reasoning = None;
    req.cwd = std::env::temp_dir().display().to_string();
    let mut stream = CodexHarness::new().run(req, controls).await.unwrap();
    let events = tokio::time::timeout(Duration::from_secs(300), async {
        let mut events = Vec::new();
        while let Some(event) = stream.next().await {
            let event = event.unwrap();
            let done = matches!(event, AgentEvent::Done { .. });
            events.push(event);
            if done {
                break;
            }
        }
        events
    })
    .await
    .expect("generation completes in five minutes");
    token.cancel();
    let path = events
        .iter()
        .find_map(|e| {
            if let AgentEvent::GeneratedImage { path, .. } = e {
                Some(path)
            } else {
                None
            }
        })
        .expect("Codex returns savedPath");
    assert!(std::path::Path::new(path).is_absolute());
    assert!(std::path::Path::new(path).is_file());
    assert!(serde_json::to_vec(&events).unwrap().len() < 64 * 1024);
}
