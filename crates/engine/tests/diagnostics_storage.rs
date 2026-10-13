//! AW-09: read-only storage accounting and engine diagnostics, through the
//! engine's own RPC service over a real disposable local profile. The sizes
//! are checked against an independent walk written here, the planted trees
//! are snapshotted before and after to prove the scan wrote nothing, and the
//! two methods are checked to stay owner-only. No provider, network, Keychain
//! or normal LUNA profile is used.
//!
//! Set `LUNA_AW09_EVIDENCE_DIR` to keep the replies and the independent walk.
#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use luna_engine::{
    AgentAccountsConfig, Auth, AuthConfig, EngineCore, EngineProfile, HarnessRegistry,
};
use luna_proto::{
    EngineDiagnostics, HarnessId, StorageCategory, StorageCategoryId, StorageCategoryState,
    StorageExclusionReason, StorageScope, StorageUsage,
};
use luna_rpc::remote_auth::{CreatePairing, IssuedPairing, RedeemPairing, RemoteScope};
use luna_rpc::{memory_client, methods};
use serde_json::{Value, json};

/// The engine resolves its worktree root from the environment when it is
/// assembled; every test in this binary shares this one disposable root.
fn worktrees_root() -> PathBuf {
    static ROOT: OnceLock<(tempfile::TempDir, PathBuf)> = OnceLock::new();
    let (_, path) = ROOT.get_or_init(|| {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().canonicalize().unwrap();
        // Set once, before either test assembles an engine.
        unsafe { std::env::set_var("LUNA_WORKTREES_DIR", &path) };
        (root, path)
    });
    path.clone()
}

fn accounts(root: &Path) -> AgentAccountsConfig {
    AgentAccountsConfig {
        data_dir: root.join("synthetic-provider-state"),
        claude_config_dir: root.join("synthetic-claude"),
        claude_config_file: root.join("synthetic-claude.json"),
        codex_home: root.join("synthetic-codex"),
    }
}

fn engine(root: &Path) -> EngineCore {
    worktrees_root();
    EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        EngineProfile::local(&root.join("data")).unwrap(),
        Arc::new(HarnessRegistry::new()),
        HarnessId::Mock,
        None,
        accounts(root),
    )
    .unwrap()
}

fn plant(path: &Path, bytes: usize) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, vec![b'x'; bytes]).unwrap();
}

/// An independent walk with the documented rules: apparent sizes of files
/// and links, links never followed, a folder that cannot be listed counted
/// once as unreadable.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Walk {
    bytes: u64,
    files: u64,
    directories: u64,
    unreadable: u64,
}

impl Walk {
    fn of(paths: &[PathBuf]) -> Self {
        let mut walk = Self::default();
        for path in paths {
            walk.add(path);
        }
        walk
    }

    fn add(&mut self, path: &Path) {
        let Ok(metadata) = fs::symlink_metadata(path) else {
            return;
        };
        if !metadata.is_dir() {
            self.files += 1;
            self.bytes += metadata.len();
            return;
        }
        self.directories += 1;
        match fs::read_dir(path) {
            Ok(entries) => {
                for entry in entries {
                    self.add(&entry.unwrap().path());
                }
            }
            Err(_) => self.unreadable += 1,
        }
    }

    fn json(&self) -> Value {
        json!({"bytes":self.bytes,"files":self.files,"directories":self.directories,"unreadable":self.unreadable})
    }
}

fn measured(category: &StorageCategory) -> Walk {
    Walk {
        bytes: category.bytes,
        files: category.files,
        directories: category.directories,
        unreadable: category.unreadable,
    }
}

/// Every entry under `paths` with its type, size, mtime and mode, without
/// following links or listing unreadable folders. Equal before and after
/// means nothing was created, removed, written or re-permissioned.
fn snapshot(paths: &[PathBuf]) -> BTreeMap<PathBuf, (bool, u64, i128, u32)> {
    fn visit(path: &Path, into: &mut BTreeMap<PathBuf, (bool, u64, i128, u32)>) {
        let Ok(metadata) = fs::symlink_metadata(path) else {
            return;
        };
        let mtime = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|since| since.as_nanos() as i128)
            .unwrap_or(-1);
        into.insert(
            path.to_path_buf(),
            (
                metadata.is_dir(),
                metadata.len(),
                mtime,
                metadata.permissions().mode(),
            ),
        );
        if metadata.is_dir()
            && let Ok(entries) = fs::read_dir(path)
        {
            for entry in entries {
                visit(&entry.unwrap().path(), into);
            }
        }
    }
    let mut map = BTreeMap::new();
    for path in paths {
        visit(path, &mut map);
    }
    map
}

fn evidence(name: &str, value: &Value) {
    if let Ok(dir) = std::env::var("LUNA_AW09_EVIDENCE_DIR") {
        let dir = PathBuf::from(dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(name), serde_json::to_vec_pretty(value).unwrap()).unwrap();
    }
}

fn category(usage: &StorageUsage, id: StorageCategoryId) -> &StorageCategory {
    usage
        .category(id)
        .unwrap_or_else(|| panic!("{id:?} is always reported"))
}

/// S1–S9: every planted part of the profile is measured exactly as an
/// independent walk measures it; an unreadable folder and a link out of the
/// data folder are partial and contained instead of fatal or followed;
/// missing folders are reported missing and stay missing; other profiles are
/// counted, never measured; the marked backup sibling and the shared
/// worktree root are their own categories; and the walk writes nothing.
#[tokio::test(flavor = "multi_thread")]
async fn storage_usage_matches_an_independent_walk_and_never_writes() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let core = engine(&root);
    let client = memory_client(core.rpc_service());
    let data = root.join("data");
    let profile = data.join("profiles/local");
    let worktrees = worktrees_root();

    // This data folder's device-level parts. (Turn snapshots stay absent:
    // assembly sweeps every other run's store in the background, so a
    // planted store could vanish mid-test; it is checked as missing.)
    plant(&data.join("logs/luna-headed.log"), 4096);
    plant(&data.join("logs/luna-headed.log.old"), 1500);
    let sealed = data.join("logs/sealed");
    plant(&sealed.join("private.log"), 777);
    plant(&data.join("model-cache/catalog.json"), 300);
    plant(&data.join("usage-scan-v2.json"), 2048);
    plant(&data.join("usage-snapshots-v1.json"), 512);
    plant(&data.join(".luna-l17-snapshot-abc123/docs.sqlite3"), 800);
    plant(&data.join("adapters/antigravity-acp/1.1.1/bin"), 5000);
    // A link out of the data folder must never be followed.
    let outside = root.join("outside");
    plant(&outside.join("large.bin"), 100_000);
    std::os::unix::fs::symlink(&outside, data.join("logs/escape")).unwrap();
    // Other profiles opened from the same data folder.
    plant(&data.join("profiles/other-local/docs.sqlite3"), 9_999);
    plant(
        &data.join("orgs/fixture-org/fixture-user/docs.sqlite3"),
        8_888,
    );
    // This profile.
    plant(&profile.join("uploads/attachment.bin"), 1234);
    plant(&profile.join("journals/fixture-chat.jsonl"), 321);
    // The marked managed backup sibling, and the shared worktree root.
    let backups = root.join("data.backups");
    plant(
        &backups.join(".luna-l17-isolated-backups"),
        "LUNA private offline backup destination\n".len(),
    );
    plant(&backups.join("luna-1-fixture.tar.gz"), 4000);
    plant(&worktrees.join("task-a/README.md"), 64);
    plant(&worktrees.join("task-a/src/lib.rs"), 128);
    fs::set_permissions(&sealed, fs::Permissions::from_mode(0o000)).unwrap();
    // As root the mode does not stop a listing; the walks then agree on that.
    let sealed_unreadable = fs::read_dir(&sealed).is_err();

    let planted = [
        data.join("logs"),
        data.join("model-cache"),
        data.join("usage-scan-v2.json"),
        data.join("usage-snapshots-v1.json"),
        data.join(".luna-l17-snapshot-abc123"),
        data.join("adapters"),
        data.join("profiles/other-local"),
        data.join("orgs"),
        profile.join("uploads"),
        profile.join("journals"),
        backups.clone(),
        worktrees.clone(),
        outside.clone(),
    ];
    let absent = [
        data.join("turn-snapshots"),
        data.join("repos"),
        data.join("updates"),
        data.join("uploads"),
    ];
    let before = snapshot(&planted);
    assert!(absent.iter().all(|path| !path.exists()));

    let reply = client
        .call(methods::GET_STORAGE_USAGE, json!({}))
        .await
        .expect("storage usage reads a profile with unreadable and missing parts");
    let usage: StorageUsage = serde_json::from_value(reply.clone()).unwrap();

    let expected: Vec<(StorageCategoryId, Vec<PathBuf>)> = vec![
        (StorageCategoryId::Logs, vec![data.join("logs")]),
        (
            StorageCategoryId::ModelCache,
            vec![data.join("model-cache")],
        ),
        (
            StorageCategoryId::UsageCache,
            vec![
                data.join("usage-scan-v2.json"),
                data.join("usage-snapshots-v1.json"),
            ],
        ),
        (
            StorageCategoryId::BackupStaging,
            vec![data.join(".luna-l17-snapshot-abc123")],
        ),
        (
            StorageCategoryId::Attachments,
            vec![profile.join("uploads")],
        ),
        (
            StorageCategoryId::RunJournals,
            vec![profile.join("journals")],
        ),
        (StorageCategoryId::Backups, vec![backups.clone()]),
        (StorageCategoryId::Worktrees, vec![worktrees.clone()]),
    ];
    let mut walks = serde_json::Map::new();
    for (id, paths) in &expected {
        let walk = Walk::of(paths);
        walks.insert(format!("{id:?}"), walk.json());
        assert_eq!(measured(category(&usage, *id)), walk, "{id:?}");
    }
    // Unrecognized: exactly the top-level names it lists, measured as such.
    let unrecognized = category(&usage, StorageCategoryId::Unrecognized);
    assert!(
        unrecognized.items.iter().any(|item| item == "adapters"),
        "{:?}",
        unrecognized.items
    );
    for owned in ["profiles", "orgs", "logs", "worktrees", "data.backups"] {
        assert!(
            !unrecognized
                .items
                .iter()
                .any(|item| item == owned || item.starts_with(&format!("{owned}/"))),
            "{owned} must not be unrecognized: {:?}",
            unrecognized.items
        );
    }
    let unrecognized_walk = Walk::of(
        &unrecognized
            .items
            .iter()
            .map(|item| data.join(item))
            .collect::<Vec<_>>(),
    );
    walks.insert("Unrecognized".into(), unrecognized_walk.json());
    assert_eq!(measured(unrecognized), unrecognized_walk);
    evidence("storage-usage.json", &reply);
    evidence("independent-walk.json", &Value::Object(walks));

    // S4: the sealed folder is one unreadable entry, reported without a path.
    let logs = category(&usage, StorageCategoryId::Logs);
    if sealed_unreadable {
        assert_eq!(logs.unreadable, 1);
        assert_eq!(logs.state, StorageCategoryState::Partial);
        let error = logs.first_error.as_deref().expect("a read failure kind");
        assert!(
            !error.contains("sealed") && !error.contains(&*root.to_string_lossy()),
            "{error}"
        );
    } else {
        eprintln!("SKIPPED unreadable: running as a user that can list mode-000 folders");
    }
    // S2: the link counts as itself; the 100 KB target is never reached.
    assert!(logs.bytes < 100_000, "{}", logs.bytes);

    // S3: missing parts are reported, not fatal, and not created.
    for id in [
        StorageCategoryId::TurnSnapshots,
        StorageCategoryId::Repositories,
        StorageCategoryId::UpdateDownloads,
        StorageCategoryId::LegacyAttachments,
    ] {
        let missing = category(&usage, id);
        assert_eq!(missing.state, StorageCategoryState::Missing, "{id:?}");
        assert_eq!((missing.bytes, missing.files), (0, 0), "{id:?}");
    }
    for id in [
        StorageCategoryId::ModelCache,
        StorageCategoryId::UsageCache,
        StorageCategoryId::BackupStaging,
        StorageCategoryId::Attachments,
        StorageCategoryId::RunJournals,
        StorageCategoryId::Backups,
        StorageCategoryId::Worktrees,
        StorageCategoryId::WorkspaceDatabase,
    ] {
        assert_eq!(
            category(&usage, id).state,
            StorageCategoryState::Complete,
            "{id:?}"
        );
    }
    assert!(category(&usage, StorageCategoryId::WorkspaceDatabase).bytes > 0);

    // S6: two other profiles are counted and never walked.
    assert_eq!(usage.excluded.len(), 1);
    assert_eq!(
        usage.excluded[0].reason,
        StorageExclusionReason::OtherProfile
    );
    assert_eq!(usage.excluded[0].count, 2);

    // Scope and location: this profile, this data folder, and outside it.
    assert_eq!(usage.profile_location, "profiles/local");
    assert_eq!(
        category(&usage, StorageCategoryId::RunJournals).scope,
        StorageScope::Profile
    );
    assert_eq!(
        category(&usage, StorageCategoryId::RunJournals).location,
        "profiles/local/journals"
    );
    assert_eq!(
        category(&usage, StorageCategoryId::Logs).scope,
        StorageScope::Device
    );
    assert_eq!(
        category(&usage, StorageCategoryId::Worktrees).scope,
        StorageScope::Shared
    );
    assert!(usage.complete && usage.stop.is_none());
    assert_eq!(
        usage.total_bytes,
        usage
            .categories
            .iter()
            .map(|category| category.bytes)
            .sum::<u64>()
    );

    // S1: a second walk changes nothing either; nothing was created.
    client
        .call(methods::GET_STORAGE_USAGE, json!({}))
        .await
        .unwrap();
    assert_eq!(snapshot(&planted), before, "the walk must not write");
    assert!(
        absent.iter().all(|path| !path.exists()),
        "the walk must not create missing folders"
    );

    fs::set_permissions(&sealed, fs::Permissions::from_mode(0o755)).unwrap();
    core.shutdown().await;
}

/// P1/P2/S10: the engine reports its own process and samples its resources;
/// a paired remote session holding every read scope is refused both methods,
/// and the owner's own call never reaches another device.
#[tokio::test(flavor = "multi_thread")]
async fn engine_diagnostics_report_this_process_and_stay_local() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let core = engine(&root);
    let mut auth = AuthConfig::new("http://127.0.0.1:1", root.join("data"));
    auth.use_in_memory_credentials_for_tests();
    core.set_auth(Auth::new(auth));
    let client = memory_client(core.rpc_service());

    let reply = client
        .call(methods::GET_ENGINE_DIAGNOSTICS, json!({}))
        .await
        .unwrap();
    evidence("engine-diagnostics.json", &reply);
    let diagnostics: EngineDiagnostics = serde_json::from_value(reply).unwrap();
    assert_eq!(diagnostics.pid, std::process::id());
    assert_eq!(diagnostics.engine_version, env!("CARGO_PKG_VERSION"));
    assert_eq!(diagnostics.os, std::env::consts::OS);
    assert!(diagnostics.started_at_ms <= diagnostics.read_at_ms);
    assert!(diagnostics.resources.cpu_user_ms.is_some());
    if cfg!(any(target_os = "macos", target_os = "linux")) {
        assert!(
            diagnostics
                .resources
                .resident_bytes
                .is_some_and(|bytes| bytes > 0)
        );
        assert!(
            diagnostics
                .resources
                .threads
                .is_some_and(|threads| threads > 0)
        );
    }

    let pairing: IssuedPairing = serde_json::from_value(
        client
            .call(
                methods::CREATE_REMOTE_PAIRING,
                serde_json::to_value(CreatePairing {
                    label: "AW-09 read-only viewer".into(),
                    scopes: vec![
                        RemoteScope::EnvironmentRead,
                        RemoteScope::TasksRead,
                        RemoteScope::FilesRead,
                    ],
                    pairing_ttl_secs: 300,
                    session_ttl_secs: 300,
                })
                .unwrap(),
            )
            .await
            .unwrap(),
    )
    .unwrap();
    let remote = memory_client(core.remote_rpc_service().unwrap());
    let _session = remote
        .redeem_remote_pairing(RedeemPairing {
            target: pairing.target.clone(),
            credential: pairing.credential.clone(),
            client_label: "AW-09 read-only viewer".into(),
        })
        .await
        .unwrap();
    let mut refusals = serde_json::Map::new();
    for method in [methods::GET_ENGINE_DIAGNOSTICS, methods::GET_STORAGE_USAGE] {
        let remote_error = remote
            .call(method, json!({}))
            .await
            .unwrap_err()
            .to_string();
        assert!(
            remote_error.starts_with("remote authorization denied:"),
            "{method} must stay owner-only: {remote_error}"
        );
        let foreign = client
            .call(method, json!({"targetDeviceId":"another-device"}))
            .await
            .unwrap_err()
            .to_string();
        assert!(
            foreign.contains("device that runs the engine"),
            "{method} must refuse another device: {foreign}"
        );
        refusals.insert(
            method.into(),
            json!({"remoteSession": remote_error, "foreignTarget": foreign}),
        );
    }
    evidence("remote-refusals.json", &Value::Object(refusals));

    core.shutdown().await;
}
