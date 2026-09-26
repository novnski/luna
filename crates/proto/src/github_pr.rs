//! Explicit same-repository GitHub draft creation and read-only details.
use crate::{ChangeRequestSummary, GitActionPreview};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitPrDraftPreview {
    pub checkout: GitActionPreview,
    /// One reviewed intent; retries must retain this UUID and immutable payload.
    pub operation_id: String,
    /// Normalized host/owner/repository; never a credential-bearing remote URL.
    pub repository: String,
    pub upstream: String,
    pub base: String,
    pub head: String,
    pub title: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitPrCreateRequest {
    pub preview: GitPrDraftPreview,
    #[serde(default)]
    pub reconcile_only: bool,
    pub title: String,
    pub body: String,
    pub base: String,
    pub head: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GitPrCreateOutcome {
    Created,
    Existing,
    Uncertain,
    Rejected,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitPrCreateReceipt {
    pub outcome: GitPrCreateOutcome,
    pub details: Option<GitPrDetails>,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitPrDetails {
    pub summary: ChangeRequestSummary,
    pub is_draft: bool,
    pub body: String,
    pub checks: Vec<GitPrCheck>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitPrCheck {
    pub name: String,
    pub status: String,
    pub url: Option<String>,
}
