//! Read-only, execution-host scoped native composer discovery.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ComposerCatalogKind {
    Plugin,
    App,
    Skill,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ComposerCatalogStatus {
    Ready,
    Empty,
    PolicyDisabled,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComposerCatalogEntry {
    pub kind: ComposerCatalogKind,
    pub id: String,
    pub name: String,
    /// Exact native Mention URI or absolute host SKILL.md path. Never a label-derived ID.
    pub path: String,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComposerCatalogSlice {
    pub kind: ComposerCatalogKind,
    pub status: ComposerCatalogStatus,
    pub disabled_count: usize,
    #[serde(default)]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComposerCatalog {
    /// Opaque host/account/home/cwd/thread/executable scope. Contains no credentials or host paths.
    pub scope: String,
    pub fetched_at_ms: i64,
    pub ttl_ms: u64,
    pub stale: bool,
    pub entries: Vec<ComposerCatalogEntry>,
    pub slices: Vec<ComposerCatalogSlice>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComposerCatalogRequest {
    #[serde(default)]
    pub chat_id: Option<String>,
    #[serde(default)]
    pub space_id: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub provider_account_id: Option<String>,
    #[serde(default)]
    pub refresh: bool,
}
