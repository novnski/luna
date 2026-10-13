//! CORE-06 / E08: local extension packages and explicit grants, driven
//! through the engine's public RPC methods against real package bytes in
//! disposable directories, with independent readback of the files the engine
//! wrote (the installed tree, the index and each profile's grants file).
//!
//! The package hash is recomputed here from the published contract (SHA-256
//! of the exact `luna-plugin.json` bytes), never read back from the engine.
//! Set `LUNA_CORE06_EVIDENCE_DIR` to retain each test's readback as JSON.
#![cfg(unix)]

use std::fs::{self, File};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use luna_engine::{AgentAccountsConfig, EngineCore, EngineProfile, HarnessId, HarnessRegistry};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

const MANIFEST: &str = "luna-plugin.json";
const NATIVE_CODE: &str = "luna.capability/native-code";
const PROVIDER_REGISTER: &str = "luna.capability/provider.register";
const NETWORK: &str = "luna.capability/network";

struct Engine {
    core: EngineCore,
    client: luna_rpc::RpcClient,
}

impl Engine {
    fn open(root: &Path, org: &str, user: &str) -> Self {
        let accounts = AgentAccountsConfig {
            data_dir: root.join("synthetic-provider-state"),
            claude_config_dir: root.join("synthetic-claude"),
            claude_config_file: root.join("synthetic-claude.json"),
            codex_home: root.join("synthetic-codex"),
        };
        accounts
            .disable_claude_keychain_fallback()
            .expect("seal synthetic Claude home");
        let core =
            EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
                EngineProfile::development(root, org, user),
                Arc::new(HarnessRegistry::new()),
                HarnessId::Mock,
                None,
                accounts,
            )
            .expect("assemble profile");
        let client = luna_rpc::memory_client(core.rpc_service());
        Self { core, client }
    }

    async fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        self.client
            .call(method, params)
            .await
            .map_err(|error| error.to_string())
    }

    async fn install(&self, path: &Path) -> Result<Value, String> {
        self.call("InstallExtension", json!({"path": path})).await
    }

    async fn set_enabled(
        &self,
        id: &str,
        hash: &str,
        enabled: bool,
        grants: &[&str],
    ) -> Result<Value, String> {
        self.call(
            "SetExtensionEnabled",
            json!({"extensionId": id, "packageHash": hash, "enabled": enabled, "grants": grants}),
        )
        .await
    }

    async fn packages(&self) -> Vec<Value> {
        let snapshot = self
            .call("ListExtensions", json!({}))
            .await
            .expect("list extensions");
        assert_eq!(snapshot["grantsError"], Value::Null, "{snapshot}");
        snapshot["packages"].as_array().cloned().unwrap_or_default()
    }

    async fn profile_id(&self) -> String {
        self.call("EngineInfo", json!({}))
            .await
            .expect("engine info")["profileId"]
            .as_str()
            .expect("profile id")
            .to_owned()
    }

    async fn close(self) {
        drop(self.client);
        self.core.shutdown().await;
        drop(self.core);
    }
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn platform() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

fn data_manifest(id: &str, version: &str) -> Value {
    json!({
        "manifestVersion": 1,
        "id": id,
        "version": version,
        "displayName": "CORE-06 fixture",
        "publisher": "CORE-06 E08",
        "api": {"major": 1, "minMinor": 0},
        "runtime": {"kind": "data"},
    })
}

/// A native provider package whose worker, if anything ever ran it, would
/// write `marker`.
fn native_manifest(id: &str, version: &str) -> Value {
    let mut manifest = data_manifest(id, version);
    manifest["runtime"] = json!({
        "kind": "native",
        "protocol": {"major": 1, "minMinor": 0},
        "entries": {platform(): "bin/worker"},
    });
    manifest["contributions"] = json!([
        {"id": format!("{id}.provider"), "kind": "provider", "title": "Echo"},
    ]);
    manifest["capabilities"] = json!([
        {"id": PROVIDER_REGISTER, "reason": "Adds the Echo provider"},
        {"id": NETWORK, "required": false, "reason": "Optional model listing"},
    ]);
    manifest["settings"] = json!({"schemaVersion": 1, "schema": "assets/settings.schema.json"});
    manifest["data"] = json!({"schemaVersion": 1});
    manifest
}

fn worker_script(marker: &Path) -> Vec<u8> {
    format!("#!/bin/sh\necho executed > '{}'\n", marker.display()).into_bytes()
}

/// The exact manifest bytes for `files`, listing each with its size and hash.
fn manifest_bytes(mut manifest: Value, files: &[(&str, &[u8])]) -> Vec<u8> {
    if manifest.get("files").is_none() {
        manifest["files"] = json!(
            files
                .iter()
                .map(|(path, bytes)| json!({"path": path, "size": bytes.len(), "sha256": sha256(bytes)}))
                .collect::<Vec<_>>()
        );
    }
    serde_json::to_vec_pretty(&manifest).unwrap()
}

/// Write a package directory; returns its package hash.
fn write_package(dir: &Path, manifest: Value, files: &[(&str, &[u8])]) -> String {
    for (path, bytes) in files {
        let target = dir.join(path);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, bytes).unwrap();
    }
    let bytes = manifest_bytes(manifest, files);
    fs::write(dir.join(MANIFEST), &bytes).unwrap();
    sha256(&bytes)
}

struct RawEntry<'a> {
    name: &'a str,
    kind: tar::EntryType,
    data: &'a [u8],
    link: Option<&'a str>,
}

fn raw<'a>(
    name: &'a str,
    kind: tar::EntryType,
    data: &'a [u8],
    link: Option<&'a str>,
) -> RawEntry<'a> {
    RawEntry {
        name,
        kind,
        data,
        link,
    }
}

/// A tar written header by header, so entry names, kinds and link targets are
/// exactly as given (the `tar` builder itself refuses `..` and absolute names).
fn raw_tar(out: &Path, entries: &[RawEntry<'_>]) {
    let mut builder = tar::Builder::new(File::create(out).unwrap());
    for entry in entries {
        let mut header = tar::Header::new_gnu();
        header.as_old_mut().name[..entry.name.len()].copy_from_slice(entry.name.as_bytes());
        header.set_entry_type(entry.kind);
        header.set_mode(0o644);
        header.set_size(entry.data.len() as u64);
        if let Some(link) = entry.link {
            header.set_link_name(link).unwrap();
        }
        header.set_cksum();
        builder.append(&header, entry.data).unwrap();
    }
    builder.into_inner().unwrap().sync_all().unwrap();
}

fn files_named(root: &Path, name: &str) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let meta = fs::symlink_metadata(&path).unwrap();
            if meta.is_dir() {
                pending.push(path.clone());
            }
            if path.file_name().and_then(|n| n.to_str()) == Some(name) {
                found.push(path);
            }
        }
    }
    found
}

fn entries(dir: &Path) -> Vec<String> {
    match fs::read_dir(dir) {
        Ok(read) => read
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => panic!("{}: {error}", dir.display()),
    }
}

fn evidence(name: &str, value: &Value) {
    if let Ok(dir) = std::env::var("LUNA_CORE06_EVIDENCE_DIR") {
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            Path::new(&dir).join(format!("{name}.json")),
            serde_json::to_vec_pretty(value).unwrap(),
        )
        .unwrap();
    }
}

/// Every malformed, unsafe or incompatible package is refused before
/// anything is installed: nothing escapes the staging tree, no index or
/// package directory appears, staging is cleaned up and outside files keep
/// their bytes. Regressions caught: a `..`/absolute archive entry or a link
/// writes outside the store, a symlink smuggles an outside file in under a
/// matching hash, a tampered or unlisted file is accepted, an install hook
/// field is silently ignored, a local package claims LUNA's namespace, or an
/// incompatible API, platform or missing dependency installs anyway.
#[tokio::test]
async fn malformed_and_incompatible_packages_are_refused_without_installing() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let sources = tmp.path().join("sources");
    fs::create_dir_all(&sources).unwrap();
    let outside = tmp.path().join("outside-secret.txt");
    fs::write(&outside, b"outside bytes\n").unwrap();
    let outside_hash = sha256(b"outside bytes\n");

    let notes: &[u8] = b"release notes\n";
    let listed = manifest_bytes(
        data_manifest("core06.fixture/archive", "1.0.0"),
        &[("notes.txt", notes)],
    );
    let file = tar::EntryType::Regular;
    let mut cases: Vec<(&str, PathBuf, &str)> = Vec::new();

    // Five `..` from `extensions/staging/<id>/tree` reach the temp root.
    let traversal = sources.join("traversal.tar");
    raw_tar(
        &traversal,
        &[
            raw(MANIFEST, file, &listed, None),
            raw("notes.txt", file, notes, None),
            raw("../../../../../escaped.txt", file, b"escaped\n", None),
        ],
    );
    cases.push(("archive traversal", traversal, "escapes the package root"));

    let absolute = sources.join("absolute.tar");
    let absolute_name = format!("{}/escaped.txt", tmp.path().display());
    raw_tar(
        &absolute,
        &[
            raw(MANIFEST, file, &listed, None),
            raw(&absolute_name, file, b"escaped\n", None),
        ],
    );
    cases.push((
        "archive absolute path",
        absolute,
        "escapes the package root",
    ));

    let symlink_tar = sources.join("symlink.tar");
    let outside_str = outside.to_str().unwrap();
    raw_tar(
        &symlink_tar,
        &[
            raw(MANIFEST, file, &listed, None),
            raw("notes.txt", tar::EntryType::Symlink, b"", Some(outside_str)),
        ],
    );
    cases.push(("archive symlink", symlink_tar, "is a link"));

    let hardlink_tar = sources.join("hardlink.tar");
    raw_tar(
        &hardlink_tar,
        &[
            raw(MANIFEST, file, &listed, None),
            raw("notes.txt", tar::EntryType::Link, b"", Some(outside_str)),
        ],
    );
    cases.push(("archive hard link", hardlink_tar, "is a link"));

    // The manifest even pins the outside file's real hash: a link is refused
    // as a link, not as a mismatch.
    let outside_bytes: &[u8] = b"outside bytes\n";
    let symlink_dir = sources.join("symlink-dir");
    fs::create_dir_all(&symlink_dir).unwrap();
    fs::write(
        symlink_dir.join(MANIFEST),
        manifest_bytes(
            data_manifest("core06.fixture/symlink", "1.0.0"),
            &[("notes.txt", outside_bytes)],
        ),
    )
    .unwrap();
    std::os::unix::fs::symlink(&outside, symlink_dir.join("notes.txt")).unwrap();
    cases.push(("directory symlink", symlink_dir, "is a symbolic link"));

    let tampered = sources.join("tampered");
    write_package(
        &tampered,
        data_manifest("core06.fixture/tampered", "1.0.0"),
        &[("notes.txt", notes)],
    );
    fs::write(tampered.join("notes.txt"), b"RELEASE NOTES\n").unwrap();
    cases.push((
        "hash mismatch",
        tampered,
        "does not match its manifest hash",
    ));

    let unlisted = sources.join("unlisted");
    write_package(
        &unlisted,
        data_manifest("core06.fixture/unlisted", "1.0.0"),
        &[("notes.txt", notes)],
    );
    fs::write(unlisted.join("extra.sh"), b"#!/bin/sh\n").unwrap();
    cases.push(("unlisted file", unlisted, "extra.sh is not listed"));

    let hook = sources.join("install-hook");
    let mut hook_manifest = data_manifest("core06.fixture/hook", "1.0.0");
    hook_manifest["scripts"] = json!({"postinstall": "notes.txt"});
    write_package(&hook, hook_manifest, &[("notes.txt", notes)]);
    cases.push(("install hook field", hook, "unknown field `scripts`"));

    let reserved = sources.join("reserved");
    write_package(
        &reserved,
        data_manifest("luna.provider/codex", "1.0.0"),
        &[("notes.txt", notes)],
    );
    cases.push(("reserved namespace", reserved, "reserved for LUNA"));

    let future_api = sources.join("future-api");
    let mut future_manifest = data_manifest("core06.fixture/future", "1.0.0");
    future_manifest["api"] = json!({"major": 2, "minMinor": 0});
    write_package(&future_api, future_manifest, &[("notes.txt", notes)]);
    cases.push(("incompatible API", future_api, "requires extension API 2.0"));

    let marker = tmp.path().join("worker-ran");
    let worker = worker_script(&marker);
    let schema: &[u8] = br#"{"type":"object"}"#;
    let other_platform = sources.join("other-platform");
    let mut other_manifest = native_manifest("core06.fixture/other", "1.0.0");
    other_manifest["runtime"]["entries"] = json!({"plan9-mips": "bin/worker"});
    write_package(
        &other_platform,
        other_manifest,
        &[
            ("bin/worker", worker.as_slice()),
            ("assets/settings.schema.json", schema),
        ],
    );
    cases.push((
        "no entry point for this platform",
        other_platform,
        "no native entry point for this device",
    ));

    let missing_dependency = sources.join("missing-dependency");
    let mut dependent = data_manifest("core06.fixture/dependent", "1.0.0");
    dependent["dependencies"] = json!([{"id": "core06.fixture/shared", "minVersion": "2.0.0"}]);
    write_package(&missing_dependency, dependent, &[("notes.txt", notes)]);
    cases.push((
        "missing dependency",
        missing_dependency,
        "requires core06.fixture/shared >= 2.0.0, which is not installed",
    ));

    let engine = Engine::open(&data, "org-a", "user-a");
    let mut refusals = Vec::new();
    for (label, path, expected) in &cases {
        let error = engine
            .install(path)
            .await
            .expect_err(&format!("{label} must be refused"));
        assert!(
            error.contains(expected),
            "{label}: expected {expected:?}, got {error:?}"
        );
        refusals.push(json!({"case": label, "error": error}));
    }
    assert!(engine.packages().await.is_empty());
    engine.close().await;

    let store = data.join("extensions");
    assert!(
        files_named(tmp.path(), "escaped.txt").is_empty(),
        "an archive entry escaped the staging tree"
    );
    assert_eq!(entries(&store.join("staging")), Vec::<String>::new());
    assert_eq!(entries(&store.join("packages")), Vec::<String>::new());
    assert!(!store.join("installed.json").exists());
    assert_eq!(sha256(&fs::read(&outside).unwrap()), outside_hash);
    assert!(!marker.exists(), "a package worker ran during install");
    evidence("e08-refusals", &json!({"refusals": refusals}));
}

/// Installing is inert and disabled; enabling binds this profile to the
/// exact hash and exact grant set; grants survive restart, never apply to
/// another profile (even when its file is copied there) and cannot revive a
/// package whose installed bytes changed. Regressions caught: install runs
/// the worker or enables it, the installed copy differs from the reviewed
/// bytes or stays writable, enable accepts a missing required grant, an
/// unrequested grant or a forged `confirmed` flag, grants are lost on
/// restart, leak into another profile, or a tampered package re-enables.
#[tokio::test]
async fn install_is_inert_and_grants_bind_exact_package_and_profile() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let source = tmp.path().join("echo.luna-plugin");
    let marker = tmp.path().join("worker-ran");
    let worker = worker_script(&marker);
    let schema: &[u8] = br#"{"type":"object","properties":{"greeting":{"type":"string"}}}"#;
    let files: [(&str, &[u8]); 2] = [
        ("bin/worker", worker.as_slice()),
        ("assets/settings.schema.json", schema),
    ];
    let id = "core06.fixture/echo";
    let hash = write_package(&source, native_manifest(id, "1.0.0"), &files);

    let a = Engine::open(&data, "org-a", "user-a");
    let installed = a.install(&source).await.expect("install");
    assert_eq!(installed["packageHash"], hash);
    assert_eq!(installed["state"], "disabled");
    assert_eq!(installed["trust"], "native");
    assert_eq!(installed["integrity"]["status"], "verified");
    let capabilities: Vec<(String, bool, bool)> = installed["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["id"].as_str().unwrap().to_owned(),
                c["required"].as_bool().unwrap(),
                c["granted"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        capabilities,
        vec![
            (NATIVE_CODE.to_owned(), true, false),
            (PROVIDER_REGISTER.to_owned(), true, false),
            (NETWORK.to_owned(), false, false),
        ]
    );
    assert_eq!(installed["settingsSchemaVersion"], 1);
    assert!(!marker.exists(), "install ran the package worker");

    // Independent readback of the committed tree: same bytes, read-only,
    // the worker executable, and nothing extra.
    let tree = data.join("extensions/packages").join(&hash);
    assert_eq!(
        fs::read(tree.join(MANIFEST)).unwrap(),
        fs::read(source.join(MANIFEST)).unwrap()
    );
    for (path, bytes) in &files {
        assert_eq!(fs::read(tree.join(path)).unwrap(), *bytes, "{path}");
        let mode = fs::metadata(tree.join(path)).unwrap().permissions().mode();
        assert_eq!(mode & 0o222, 0, "{path} is writable");
    }
    let worker_mode = fs::metadata(tree.join("bin/worker"))
        .unwrap()
        .permissions()
        .mode();
    assert_ne!(worker_mode & 0o111, 0, "the entry point is not executable");
    let index: Value =
        serde_json::from_slice(&fs::read(data.join("extensions/installed.json")).unwrap()).unwrap();
    assert_eq!(index["packages"].as_array().unwrap().len(), 1);
    assert_eq!(index["packages"][0]["packageHash"], hash);

    // Identical bytes again: the same package, not a second one.
    assert_eq!(
        a.install(&source).await.expect("reinstall")["packageHash"],
        hash
    );
    assert_eq!(a.packages().await.len(), 1);

    let unknown = "0".repeat(64);
    let refusals = [
        (
            a.set_enabled(id, &unknown, true, &[NATIVE_CODE, PROVIDER_REGISTER])
                .await,
            "not installed",
        ),
        (
            a.set_enabled(id, &hash, true, &[PROVIDER_REGISTER]).await,
            "luna.capability/native-code is required",
        ),
        (
            a.set_enabled(
                id,
                &hash,
                true,
                &[
                    NATIVE_CODE,
                    PROVIDER_REGISTER,
                    "luna.capability/workspace.write",
                ],
            )
            .await,
            "luna.capability/workspace.write was not requested",
        ),
        (
            a.call(
                "SetExtensionEnabled",
                json!({"extensionId": id, "packageHash": hash, "enabled": true, "confirmed": true}),
            )
            .await,
            "unknown field `confirmed`",
        ),
    ];
    for (result, expected) in &refusals {
        let error = result.as_ref().expect_err(expected);
        assert!(
            error.contains(expected),
            "expected {expected:?}, got {error:?}"
        );
    }
    assert_eq!(a.packages().await[0]["state"], "disabled");

    let enabled = a
        .set_enabled(id, &hash, true, &[PROVIDER_REGISTER, NATIVE_CODE])
        .await
        .expect("enable with exact grants");
    assert_eq!(enabled["state"], "enabled");
    let granted: Vec<&str> = enabled["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["granted"] == true)
        .map(|c| c["id"].as_str().unwrap())
        .collect();
    assert_eq!(granted, [NATIVE_CODE, PROVIDER_REGISTER]);
    assert!(!marker.exists(), "enable ran the package worker");

    let profile_a = a.profile_id().await;
    let grants_a = data.join("orgs/org-a/user-a/extension-grants.json");
    let file_a: Value = serde_json::from_slice(&fs::read(&grants_a).unwrap()).unwrap();
    assert_eq!(file_a["profileId"], profile_a);
    assert_eq!(file_a["extensions"][id]["packageHash"], hash);
    assert_eq!(
        file_a["extensions"][id]["grants"],
        json!([NATIVE_CODE, PROVIDER_REGISTER])
    );
    a.close().await;

    // Restart: the same exact grant.
    let a = Engine::open(&data, "org-a", "user-a");
    let after_restart = a.packages().await;
    assert_eq!(after_restart[0]["packageHash"], hash);
    assert_eq!(after_restart[0]["state"], "enabled");
    a.close().await;

    // Another profile on the same device sees the installed candidate, off.
    let b = Engine::open(&data, "org-b", "user-b");
    let other = b.packages().await;
    assert_eq!(other.len(), 1);
    assert_eq!(other[0]["packageHash"], hash);
    assert_eq!(other[0]["state"], "disabled");
    let profile_b = b.profile_id().await;
    assert_ne!(profile_a, profile_b);
    b.close().await;

    // A grants file copied into that profile (sync, restore, hand copy)
    // still grants nothing there.
    let grants_b = data.join("orgs/org-b/user-b/extension-grants.json");
    fs::copy(&grants_a, &grants_b).unwrap();
    let b = Engine::open(&data, "org-b", "user-b");
    assert_eq!(b.packages().await[0]["state"], "disabled");
    b.close().await;

    // Change the installed worker without changing its size.
    let a = Engine::open(&data, "org-a", "user-a");
    let installed_worker = tree.join("bin/worker");
    fs::set_permissions(&installed_worker, fs::Permissions::from_mode(0o755)).unwrap();
    let mut changed = worker.clone();
    changed[0] = b'X';
    fs::write(&installed_worker, &changed).unwrap();
    let inspected = a
        .call("InspectExtension", json!({"packageHash": hash}))
        .await
        .expect("inspect");
    assert_eq!(inspected["integrity"]["status"], "failed");
    let disabled = a
        .set_enabled(id, &hash, false, &[])
        .await
        .expect("disable a changed package");
    assert_eq!(disabled["state"], "disabled");
    let error = a
        .set_enabled(id, &hash, true, &[NATIVE_CODE, PROVIDER_REGISTER])
        .await
        .expect_err("a changed package cannot be enabled");
    assert!(error.contains("failed verification"), "{error}");
    a.close().await;

    assert!(!marker.exists(), "something ran the package worker");
    evidence(
        "e08-grants",
        &json!({
            "packageHash": hash,
            "profileA": profile_a,
            "profileB": profile_b,
            "grantsFileA": file_a,
            "afterRestart": after_restart,
            "otherProfile": other,
            "inspectAfterTamper": inspected,
        }),
    );
}

/// Dependencies resolve by version at install and by this profile's enabled
/// versions at enable; versions are immutable. Regressions caught: a package
/// installs or enables against a missing or out-of-range dependency, two
/// versions of one package are enabled together, a dependency is disabled
/// under an enabled dependent, or a version is silently replaced by
/// different bytes.
#[tokio::test]
async fn dependencies_must_be_installed_and_enabled_in_range() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let notes: &[u8] = b"notes\n";
    let package = |name: &str, manifest: Value| {
        let dir = tmp.path().join(name);
        let hash = write_package(&dir, manifest, &[("notes.txt", notes)]);
        (dir, hash)
    };
    let shared = "core06.fixture/shared";
    let dependent_id = "core06.fixture/dependent";
    let (shared_1, shared_1_hash) = package("shared-1", data_manifest(shared, "1.0.0"));
    let (shared_2, shared_2_hash) = package("shared-2", data_manifest(shared, "2.0.0"));
    let mut dependent_manifest = data_manifest(dependent_id, "1.0.0");
    dependent_manifest["dependencies"] =
        json!([{"id": shared, "minVersion": "2.0.0", "maxVersion": "3.0.0"}]);
    let (dependent, dependent_hash) = package("dependent", dependent_manifest);
    let mut changed_manifest = data_manifest(shared, "2.0.0");
    changed_manifest["description"] = json!("different bytes, same version");
    let (changed, _) = package("shared-2-changed", changed_manifest);

    let engine = Engine::open(&data, "org-a", "user-a");
    engine
        .install(&shared_1)
        .await
        .expect("install shared 1.0.0");
    let error = engine.install(&dependent).await.expect_err("out of range");
    assert!(error.contains("installed: 1.0.0"), "{error}");
    engine
        .install(&shared_2)
        .await
        .expect("install shared 2.0.0");
    engine.install(&dependent).await.expect("install dependent");
    let error = engine
        .install(&changed)
        .await
        .expect_err("immutable version");
    assert!(
        error.contains("already installed with different contents"),
        "{error}"
    );

    let error = engine
        .set_enabled(dependent_id, &dependent_hash, true, &[])
        .await
        .expect_err("dependency not enabled");
    assert!(error.contains("enable it first"), "{error}");
    engine
        .set_enabled(shared, &shared_1_hash, true, &[])
        .await
        .expect("enable shared 1.0.0");
    let error = engine
        .set_enabled(dependent_id, &dependent_hash, true, &[])
        .await
        .expect_err("enabled dependency out of range");
    assert!(error.contains("the enabled version is 1.0.0"), "{error}");
    let error = engine
        .set_enabled(shared, &shared_2_hash, true, &[])
        .await
        .expect_err("one enabled version per package");
    assert!(
        error.contains("disable it before enabling 2.0.0"),
        "{error}"
    );
    engine
        .set_enabled(shared, &shared_1_hash, false, &[])
        .await
        .expect("disable 1.0.0");
    engine
        .set_enabled(shared, &shared_2_hash, true, &[])
        .await
        .expect("enable 2.0.0");
    engine
        .set_enabled(dependent_id, &dependent_hash, true, &[])
        .await
        .expect("enable dependent");
    let error = engine
        .set_enabled(shared, &shared_2_hash, false, &[])
        .await
        .expect_err("dependency in use");
    assert!(error.contains("depends on"), "{error}");

    let states: Vec<(String, String, String)> = engine
        .packages()
        .await
        .iter()
        .map(|p| {
            (
                p["id"].as_str().unwrap().to_owned(),
                p["version"].as_str().unwrap().to_owned(),
                p["state"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        states,
        [
            (shared.to_owned(), "1.0.0".to_owned(), "disabled".to_owned()),
            (shared.to_owned(), "2.0.0".to_owned(), "enabled".to_owned()),
            (
                dependent_id.to_owned(),
                "1.0.0".to_owned(),
                "enabled".to_owned()
            ),
        ]
    );
    engine.close().await;
    evidence("e08-dependencies", &json!({"states": states}));
}

/// What a crash between staging and commit leaves never becomes an installed
/// or enabled package: on the next open the staged copy, the unlisted
/// package directory, an unrenamed index and an unrenamed grants file are all
/// removed, and a clean install of the same package is a disabled candidate.
/// The residue is the real package bytes at the store's documented paths,
/// with a forged grant for this very profile in the temp file.
#[tokio::test]
async fn crash_residue_between_stage_and_commit_never_installs_or_grants() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let source = tmp.path().join("residue.luna-plugin");
    let notes: &[u8] = b"notes\n";
    let id = "core06.fixture/residue";
    let hash = write_package(&source, data_manifest(id, "1.0.0"), &[("notes.txt", notes)]);
    let manifest = fs::read(source.join(MANIFEST)).unwrap();

    let engine = Engine::open(&data, "org-a", "user-a");
    let profile_id = engine.profile_id().await;
    engine.close().await;

    let store = data.join("extensions");
    let copy_package = |into: &Path| {
        fs::create_dir_all(into).unwrap();
        fs::write(into.join(MANIFEST), &manifest).unwrap();
        fs::write(into.join("notes.txt"), notes).unwrap();
    };
    copy_package(&store.join("staging/2f1c7e0e-crash/tree"));
    copy_package(&store.join("packages").join(&hash));
    let record = json!({
        "id": id, "version": "1.0.0", "packageHash": hash,
        "displayName": "CORE-06 fixture", "publisher": "CORE-06 E08", "runtime": "data",
        "origin": {"kind": "local", "source": source},
        "installedAtMs": 1, "sizeBytes": notes.len(), "fileCount": 1,
    });
    fs::write(
        store.join(".installed-crash.tmp"),
        serde_json::to_vec(&json!({"version": 1, "packages": [record]})).unwrap(),
    )
    .unwrap();
    let store_root = data.join("orgs/org-a/user-a");
    fs::write(
        store_root.join(".extension-grants-crash.tmp"),
        serde_json::to_vec(&json!({
            "version": 1, "profileId": profile_id, "revision": 1,
            "extensions": {id: {"packageHash": hash, "version": "1.0.0", "grants": [], "enabledAtMs": 1}},
        }))
        .unwrap(),
    )
    .unwrap();

    let engine = Engine::open(&data, "org-a", "user-a");
    assert!(
        engine.packages().await.is_empty(),
        "residue became a package"
    );
    assert_eq!(entries(&store.join("staging")), Vec::<String>::new());
    assert_eq!(entries(&store.join("packages")), Vec::<String>::new());
    assert!(!store.join(".installed-crash.tmp").exists());
    assert!(!store.join("installed.json").exists());
    assert!(!store_root.join(".extension-grants-crash.tmp").exists());
    assert!(!store_root.join("extension-grants.json").exists());

    let installed = engine.install(&source).await.expect("clean install");
    assert_eq!(installed["packageHash"], hash);
    assert_eq!(installed["state"], "disabled");
    assert_eq!(entries(&store.join("packages")), vec![hash.clone()]);
    engine.close().await;
    evidence(
        "e08-crash-residue",
        &json!({"packageHash": hash, "installed": installed}),
    );
}
