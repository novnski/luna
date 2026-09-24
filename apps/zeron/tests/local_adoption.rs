//! Actual CLI boundary for an unconfigured local LUNA candidate.

use std::process::{Command, Output};

fn luna(root: &std::path::Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_luna"));
    command.args(args);
    command.env("LUNA_DATA_DIR", root);
    command.env("LUNA_IPC_PORT", "0");
    for name in [
        "LUNA_EDGE_URL",
        "LUNA_WORKOS_CLIENT_ID",
        "LUNA_EDGE_TOKEN",
        "LUNA_WORKOS_API_BASE",
    ] {
        command.env_remove(name);
    }
    // An inherited Zeron setup must not configure LUNA.
    command.env("ZERON_EDGE_URL", "https://edge.zeron.sh");
    command.env("ZERON_WORKOS_CLIENT_ID", "client_legacy");
    command.env("ZERON_EDGE_TOKEN", "synthetic-legacy-bearer");
    command
}

fn text(output: &Output) -> String {
    format!(
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn bare_candidate_is_local_and_ignores_zeron_service_variables() {
    let root = tempfile::tempdir().unwrap();
    let output = luna(root.path(), &["status"]).output().unwrap();
    assert!(output.status.success(), "{}", text(&output));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Mode:     local only"), "{stdout}");
    assert!(
        stdout.contains("Edge:     disabled (local only)"),
        "{stdout}"
    );
    assert!(!stdout.contains("edge.zeron.sh"), "{stdout}");
    assert!(!root.path().join("profiles").exists());
}

#[test]
fn explicit_loopback_development_config_is_available_without_a_network_probe() {
    let root = tempfile::tempdir().unwrap();
    let output = luna(root.path(), &["status"])
        .env("LUNA_EDGE_URL", "http://127.0.0.1:1")
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", text(&output));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Mode:     development"), "{stdout}");
    assert!(stdout.contains("http://127.0.0.1:1"), "{stdout}");
}

#[test]
fn partial_or_zeron_service_configuration_fails_closed() {
    let root = tempfile::tempdir().unwrap();
    let partial = luna(root.path(), &["status"])
        .env("LUNA_WORKOS_CLIENT_ID", "synthetic-client")
        .output()
        .unwrap();
    assert!(!partial.status.success(), "{}", text(&partial));
    assert!(text(&partial).contains("requires an explicit owned LUNA_EDGE_URL"));

    let foreign = luna(root.path(), &["status"])
        .env("LUNA_EDGE_URL", "https://edge.zeron.sh")
        .output()
        .unwrap();
    assert!(!foreign.status.success(), "{}", text(&foreign));
    assert!(text(&foreign).contains("cannot use Zeron's service endpoint"));
}

#[test]
fn local_candidate_refuses_login_and_unowned_updates() {
    let root = tempfile::tempdir().unwrap();
    let login = luna(root.path(), &["login"]).output().unwrap();
    assert!(!login.status.success(), "{}", text(&login));
    assert!(text(&login).contains("sync is unconfigured"));

    let update = luna(root.path(), &["update", "--check"]).output().unwrap();
    assert!(!update.status.success(), "{}", text(&update));
    assert!(text(&update).contains("updates are disabled"));
    assert!(!root.path().join("profiles").exists());
}

#[test]
fn actual_binary_previews_and_restores_only_its_synthetic_original_root() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("synthetic-luna");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("device-id"), "synthetic-device").unwrap();
    std::fs::write(root.join("ui-settings.json"), b"synthetic-before").unwrap();
    let store = zeron_sync::DocsStore::open(root.join("profiles/local")).unwrap();
    store
        .save_snapshot("synthetic-doc", b"synthetic LUNA data")
        .unwrap();
    drop(store);

    let backup = luna(&root, &["backup", "now"]).output().unwrap();
    assert!(backup.status.success(), "{}", text(&backup));
    let archive = String::from_utf8_lossy(&backup.stdout)
        .lines()
        .next()
        .unwrap()
        .to_string();
    let preview = luna(&root, &["backup", "preview", &archive])
        .output()
        .unwrap();
    assert!(preview.status.success(), "{}", text(&preview));
    let summary = String::from_utf8_lossy(&preview.stdout);
    assert!(summary.contains("entries "), "{summary}");
    assert!(summary.contains("required bytes "), "{summary}");
    assert!(summary.contains("unsupported 0"), "{summary}");

    let copied_root = fixture.path().join("copied-luna");
    std::fs::create_dir(&copied_root).unwrap();
    std::fs::write(copied_root.join("device-id"), "synthetic-device").unwrap();
    let copied = luna(&copied_root, &["backup", "preview", &archive])
        .output()
        .unwrap();
    assert!(copied.status.success(), "{}", text(&copied));
    assert!(text(&copied).contains("original data-root path"));
    let refused = luna(&copied_root, &["backup", "restore", &archive])
        .output()
        .unwrap();
    assert!(!refused.status.success(), "{}", text(&refused));

    std::fs::write(root.join("ui-settings.json"), b"synthetic-after").unwrap();
    let restored = luna(&root, &["backup", "restore", &archive])
        .output()
        .unwrap();
    assert!(restored.status.success(), "{}", text(&restored));
    assert_eq!(
        std::fs::read(root.join("ui-settings.json")).unwrap(),
        b"synthetic-before"
    );
    assert!(text(&restored).contains("previous files "));
}
