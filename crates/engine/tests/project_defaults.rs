//! First-written integration acceptance: real RPC -> registry -> restart.
use luna_engine::{EngineCore, HarnessRegistry};
use luna_proto::{HarnessId, Space};
use luna_rpc::methods;
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};

fn assemble(path: &Path) -> EngineCore {
    std::fs::create_dir_all(path).unwrap();
    std::fs::write(path.join("device-id"), "defaults-owner").unwrap();
    EngineCore::assemble(
        path,
        Arc::new(HarnessRegistry::new()),
        HarnessId::Mock,
        None,
    )
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn project_defaults_rpc_persistence_clearing_and_existing_task_preservation() {
    let dir = tempfile::tempdir().unwrap();
    let core = assemble(dir.path());
    let client = luna_rpc::memory_client(core.rpc_service());
    core.workspace
        .create_space("a", "defaults-owner", "/project-a", None, true)
        .unwrap();
    core.workspace
        .create_space("b", "defaults-owner", "/project-b", None, true)
        .unwrap();
    core.workspace
        .create_space("foreign", "another-device", "/foreign", None, true)
        .unwrap();
    core.workspace
        .create_chat("existing", Some("a"), None, None, None)
        .unwrap();
    let existing = core.workspace.chat("existing").unwrap().unwrap();
    let defaults = json!({"provider":{"harness":"codex","model":"gpt-6-luna"},"permissionMode":"full","checkout":"newWorktree"});
    client
        .call(
            methods::MUTATE,
            json!({"op":"setProjectExecutionDefaults","spaceId":"a","defaults":defaults,"expectedDefaults":{}}),
        )
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(core.workspace.space("a").unwrap().unwrap()).unwrap()["executionDefaults"],
        defaults
    );
    assert_eq!(core.workspace.chat("existing").unwrap().unwrap(), existing);
    for (space, invalid) in [
        ("missing", defaults.clone()),
        ("foreign", defaults.clone()),
        ("a", json!({"provider":{"harness":"codex","model":" "}})),
        (
            "a",
            json!({"provider":{"harness":"codex","model":"x".repeat(513)}}),
        ),
        ("a", json!({"provider":{"harness":"mock","model":null}})),
        ("a", json!({"provider":{"harness":"missing"}})),
        ("a", json!({"checkout":"delete"})),
        ("a", json!({"unexpected":true})),
    ] {
        assert!(
            client
                .call(
                    methods::MUTATE,
                    json!({"op":"setProjectExecutionDefaults","spaceId":space,"defaults":invalid,"expectedDefaults":defaults})
                )
                .await
                .is_err(),
            "accepted invalid defaults for {space}"
        );
        assert_eq!(
            serde_json::to_value(core.workspace.space("a").unwrap().unwrap()).unwrap()["executionDefaults"],
            defaults
        );
    }
    assert!(client.call(methods::MUTATE, json!({"op":"setProjectExecutionDefaults","spaceId":"a","defaults":{},"expectedDefaults":{}})).await.is_err(), "stale settings write must fail");
    core.workspace.rename_space("a", Some("Renamed A")).unwrap();
    core.workspace.flush();
    drop(client);
    core.shutdown().await;
    drop(core);
    let core = assemble(dir.path());
    let a = core.workspace.space("a").unwrap().unwrap();
    assert_eq!(a.name.as_deref(), Some("Renamed A"));
    assert_eq!(
        serde_json::to_value(&a).unwrap()["executionDefaults"],
        defaults
    );
    assert_eq!(core.workspace.chat("existing").unwrap().unwrap(), existing);
    let client = luna_rpc::memory_client(core.rpc_service());
    client
        .call(
            methods::MUTATE,
            json!({"op":"setProjectExecutionDefaults","spaceId":"a","defaults":{},"expectedDefaults":defaults}),
        )
        .await
        .unwrap();
    core.workspace.flush();
    drop(client);
    core.shutdown().await;
    drop(core);
    let core = assemble(dir.path());
    assert_eq!(
        core.workspace
            .space("a")
            .unwrap()
            .unwrap()
            .execution_defaults,
        Default::default()
    );
    assert_eq!(core.workspace.chat("existing").unwrap().unwrap(), existing);
    if let Ok(path) = std::env::var("LUNA_PROJECT_DEFAULTS_EVIDENCE") {
        std::fs::write(path, serde_json::to_vec_pretty(&json!({"rpcValidation":"passed","restart":"passed","inheritClearing":"passed","existingTaskPreserved":"passed","spaces":core.workspace.read_spaces().unwrap()})).unwrap()).unwrap();
    }
    core.shutdown().await;
}

#[test]
fn project_defaults_old_space_json_inherits() {
    let old: Space = serde_json::from_value(
        json!({"id":"old","deviceId":"owner","path":"/old","createdAt":"2026-09-26T00:00:00Z"}),
    )
    .unwrap();
    assert_eq!(old.execution_defaults, Default::default());
    let mut registry = luna_doc::RegistryDoc::new("owner");
    registry.upsert_space(&old).unwrap();
    let bytes = registry.to_bytes().unwrap();
    let mut persisted: Value = serde_json::from_slice(&bytes).unwrap();
    // Remove the field from the actual registry upsert payload to reproduce
    // snapshots produced before project defaults existed.
    for batch in persisted["pending"].as_array_mut().unwrap() {
        for op in batch["ops"].as_array_mut().unwrap() {
            op["set"]
                .as_object_mut()
                .unwrap()
                .remove("executionDefaults");
        }
    }
    let migrated =
        luna_doc::RegistryDoc::from_bytes(&serde_json::to_vec(&persisted).unwrap(), "owner")
            .unwrap();
    assert_eq!(
        migrated.space("old").unwrap().unwrap().execution_defaults,
        Default::default()
    );
    let mut value = serde_json::to_value(old).unwrap();
    value["executionDefaults"] = Value::Null;
    // Legacy missing data is the migration path; null is rejected rather than
    // silently weakening a corrupt explicit setting.
    assert!(serde_json::from_value::<Space>(value).is_err());
}
