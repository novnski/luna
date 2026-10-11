//! CORE-01 published contract: the operation catalog and the JSON Schemas of
//! the minimal public types, generated from the code that serves them. The
//! `contract` example writes these files under `contract/v1/`; the `contract`
//! test fails when the committed copies drift.

use serde::Serialize;

use crate::operations::{CATALOG_VERSION, OPERATIONS, OperationSpec};

/// The `operations.json` document.
#[derive(Debug, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OperationCatalog {
    pub catalog_version: u16,
    pub operations: &'static [OperationSpec],
}

fn pretty<T: Serialize>(value: &T) -> String {
    let mut text = serde_json::to_string_pretty(value).expect("contract value serializes");
    text.push('\n');
    text
}

fn schema<T: schemars::JsonSchema>() -> String {
    pretty(&schemars::schema_for!(T))
}

/// Every published file, as `(path under contract/v1, contents)`.
pub fn files() -> Vec<(&'static str, String)> {
    use luna_proto::contract::{
        ContentTypeId, ContributionId, ExtensionId, NamespacedId, ProviderRef,
    };
    vec![
        (
            "operations.json",
            pretty(&OperationCatalog {
                catalog_version: CATALOG_VERSION,
                operations: OPERATIONS,
            }),
        ),
        (
            "schemas/operation-catalog.json",
            schema::<OperationCatalog>(),
        ),
        ("schemas/client-frame.json", schema::<crate::ClientFrame>()),
        ("schemas/server-frame.json", schema::<crate::ServerFrame>()),
        (
            "schemas/task-command-envelope.json",
            schema::<luna_proto::TaskCommandEnvelope<luna_proto::TaskCommandIntentEnvelope>>(),
        ),
        (
            "schemas/command-receipt.json",
            schema::<luna_proto::CommandReceipt>(),
        ),
        (
            "schemas/task-command-read.json",
            schema::<luna_proto::TaskCommandRead>(),
        ),
        ("schemas/namespaced-id.json", schema::<NamespacedId>()),
        ("schemas/provider-ref.json", schema::<ProviderRef>()),
        ("schemas/extension-id.json", schema::<ExtensionId>()),
        ("schemas/contribution-id.json", schema::<ContributionId>()),
        ("schemas/content-type-id.json", schema::<ContentTypeId>()),
    ]
}
