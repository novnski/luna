//! CORE-05 delivery: the TypeScript SDK cannot drift from the Rust SDK.
//! Its types and embedded manifest are exactly what the Rust types that
//! validate every reply generate, and its package carries this SDK's version.
//! Regenerate with the `generate` example after an intended change.

use std::path::Path;

#[test]
fn committed_sdk_bindings_match_the_rust_types() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut stale = Vec::new();
    for (path, generated) in luna_sdk::schema::files() {
        let committed = std::fs::read_to_string(root.join(path)).unwrap_or_default();
        if committed != generated {
            stale.push(path);
        }
    }
    assert!(
        stale.is_empty(),
        "stale generated SDK files {stale:?}; run `scripts/cargo.sh run --locked -p luna-sdk --features schema --example generate`"
    );
    let package: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("sdk/typescript/package.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        package["version"],
        luna_sdk::SDK_VERSION,
        "sdk/typescript/package.json and luna-sdk versions differ"
    );
}
