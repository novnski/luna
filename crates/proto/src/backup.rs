//! Redacted local backup control-plane values. Raw recovery material is input
//! only and is never returned in a status, receipt, preview or error.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackupAvailability {
    Unavailable,
    Unconfigured,
    Ready,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupSummary {
    pub archive_path: String,
    pub archive_sha256: String,
    pub created_at_ms: i64,
    pub total_bytes: u64,
    pub entry_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupStatus {
    pub availability: BackupAvailability,
    pub reason: Option<String>,
    pub source_root: Option<String>,
    pub destination_dir: Option<String>,
    pub automatic_enabled: bool,
    pub key_configured: bool,
    pub last_success: Option<BackupSummary>,
    pub retention_candidates: usize,
    pub staged_restore: Option<BackupStageStatus>,
    pub restore_activation_available: bool,
    pub restore_reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupConfigureRequest {
    pub source_root: String,
    pub destination_dir: String,
    pub automatic_enabled: bool,
}

/// Deliberately has no Debug or Serialize implementation: a caller may send
/// owner-held material or an absolute owner-held file path to a local engine,
/// but neither is sent back on the wire. Hex ingress is synthetic-only.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupKeyConfigureRequest {
    #[serde(default)]
    pub recovery_key_hex: Option<String>,
    #[serde(default)]
    pub recovery_key_file_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupArchiveRequest {
    pub archive_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupPreview {
    pub archive_path: String,
    pub source_root: String,
    pub created_at_ms: i64,
    pub total_bytes: u64,
    pub entry_count: usize,
    pub database_count: usize,
    pub document_count: i64,
    pub task_event_count: i64,
    pub attachment_count: usize,
    pub excluded_categories: Vec<String>,
    pub conflicts: Vec<String>,
    pub unsupported: Vec<String>,
    pub restore_activation_available: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupStageStatus {
    pub archive_path: String,
    pub staging_path: String,
    pub verified_at_ms: i64,
    pub database_count: usize,
    pub attachment_count: usize,
    pub restore_activation_available: bool,
    pub reason: String,
}
