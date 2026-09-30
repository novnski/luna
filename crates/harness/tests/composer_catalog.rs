//! Subprocess contract E2E; the peer is synthetic, not real-provider proof.
#![cfg(unix)]
use luna_harness::codex::native::CodexNativeClient;
use luna_proto::{ComposerCatalogKind, ComposerCatalogStatus};

#[tokio::test]
async fn installed_catalog_preserves_exact_identities_and_filters_policy() {
    let cwd = tempfile::tempdir().unwrap();
    let client = CodexNativeClient::new(cwd.path()).with_executable(
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/fake-composer-catalog.py"),
    );
    let catalog = client
        .read_composer_catalog(cwd.path(), None)
        .await
        .unwrap();
    assert_eq!(catalog.entries.len(), 3);
    assert!(catalog.entries.iter().any(
        |row| row.kind == ComposerCatalogKind::Plugin && row.path == "plugin://opaque@catalog"
    ));
    assert!(
        catalog
            .entries
            .iter()
            .any(|row| row.kind == ComposerCatalogKind::App && row.path == "app://app_exact")
    );
    assert!(
        catalog
            .entries
            .iter()
            .any(|row| row.kind == ComposerCatalogKind::Skill
                && row.path.ends_with("/Skill (one)/SKILL.md"))
    );
    assert!(
        catalog
            .slices
            .iter()
            .all(|slice| slice.status == ComposerCatalogStatus::Ready)
    );
    let thread_catalog = client
        .read_composer_catalog(cwd.path(), Some("unloaded"))
        .await
        .unwrap();
    assert!(
        !thread_catalog
            .entries
            .iter()
            .any(|row| row.path == "plugin://opaque@catalog")
    );
    assert_eq!(
        thread_catalog
            .slices
            .iter()
            .find(|slice| slice.kind == ComposerCatalogKind::Plugin)
            .unwrap()
            .status,
        ComposerCatalogStatus::Unavailable
    );
    assert_eq!(
        thread_catalog
            .slices
            .iter()
            .find(|slice| slice.kind == ComposerCatalogKind::App)
            .unwrap()
            .status,
        ComposerCatalogStatus::Unavailable
    );
    if let Some(directory) = std::env::var_os("LUNA_PICKER_EVIDENCE_DIR") {
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(std::path::PathBuf::from(directory).join("adapter-catalog.json"),
            serde_json::to_vec_pretty(&serde_json::json!({"evidenceKind": "synthetic-app-server-contract", "catalog": catalog, "unloadedThread": thread_catalog})).unwrap()).unwrap();
    }
}
