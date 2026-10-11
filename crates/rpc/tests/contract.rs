//! CORE-01 published contract (C4, C10): the committed `contract/v1/` files
//! are exactly what the serving code generates, and every catalogued
//! operation has one well-formed, unique namespaced id and wire spelling.
//! Regenerate with the `contract` example after an intended change.

use std::collections::HashSet;
use std::path::Path;

use luna_proto::contract::{NamespacedId, ProviderRef};
use luna_rpc::operations::OPERATIONS;

#[test]
fn committed_contract_matches_the_generated_contract() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("contract/v1");
    let mut stale = Vec::new();
    for (path, generated) in luna_rpc::contract::files() {
        let committed = std::fs::read_to_string(root.join(path)).unwrap_or_default();
        if committed != generated {
            stale.push(path);
        }
    }
    assert!(
        stale.is_empty(),
        "stale contract files {stale:?}; run `scripts/cargo.sh run --locked -p luna-rpc --features schema --example contract`"
    );
}

#[test]
fn every_operation_has_one_unique_well_formed_identity() {
    let mut ids = HashSet::new();
    let mut methods = HashSet::new();
    for op in OPERATIONS {
        let id =
            NamespacedId::parse(op.id).unwrap_or_else(|error| panic!("{}: {error}", op.method));
        assert_eq!(id.namespace(), "luna.core", "{}", op.method);
        assert!(op.version >= 1, "{}", op.method);
        assert!(ids.insert(op.id), "duplicate operation id {}", op.id);
        assert!(
            methods.insert(op.method),
            "duplicate wire method {}",
            op.method
        );
    }
    let declared: HashSet<_> = luna_rpc::methods::ALL.iter().copied().collect();
    assert_eq!(methods, declared, "catalog rows and methods::ALL differ");
}

/// C8: the Rust id parsers accept exactly what the shared, hand-written table
/// says; `scripts/qa-contract-reader.py` checks the same table against the
/// published JSON Schemas, so the two implementations agree.
#[test]
fn id_parsers_agree_with_the_shared_table() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/contract-ids.json");
    let rows: Vec<serde_json::Value> =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    for row in rows {
        let value = row["value"].as_str().unwrap();
        let namespaced = NamespacedId::parse(value).is_ok();
        assert_eq!(
            namespaced, row["accepts"]["namespacedId"],
            "NamespacedId {value:?}"
        );
        let provider = ProviderRef::parse(value);
        assert_eq!(
            provider.is_ok(),
            row["accepts"]["providerRef"],
            "ProviderRef {value:?}"
        );
        if let (Ok(provider), Some(canonical)) = (provider, row["canonical"].as_str()) {
            assert_eq!(provider.as_str(), value, "kept byte-for-byte");
            assert_eq!(provider.canonical().as_str(), canonical, "{value:?}");
            let wire = serde_json::to_string(&provider).unwrap();
            assert_eq!(
                wire,
                serde_json::to_string(value).unwrap(),
                "lossless round trip"
            );
            let back: ProviderRef = serde_json::from_str(&wire).unwrap();
            assert_eq!(back, provider);
        }
    }
}
