//! Explicit, app-owned visible-text context transfers. Native identifiers,
//! hidden reasoning, tool payloads and permission grants are not included.
use crate::{ChatConfig, HarnessId};
use serde::{Deserialize, Serialize};

pub const CONTEXT_TRANSFER_VERSION: u32 = 1;
pub const MAX_CONTEXT_TRANSFER_BYTES: usize = 96 * 1024;
pub const MAX_CONTEXT_TRANSFER_ITEMS: usize = 2048;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ContextTransferKind {
    ProviderHandoff,
    PortableFork,
    ForkMerge,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextTransferNewTarget {
    pub config: ChatConfig,
    #[serde(default)]
    pub title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextTransferRequest {
    pub kind: ContextTransferKind,
    pub source_task_id: String,
    pub target_task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_config: Option<ChatConfig>,
    /// Portable-fork only. The target UUID is reserved by the client; preview
    /// creates nothing and apply creates an independent task atomically with
    /// its context in the source's project/checkout. Cancellation leaves no row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_target: Option<ContextTransferNewTarget>,
    /// Empty selects the newest eligible visible items within the budget.
    #[serde(default)]
    pub message_ids: Vec<String>,
    /// Preview only; applying a transfer never submits this as a prompt.
    #[serde(default)]
    pub prompt_text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextTransferBasis {
    pub task_id: String,
    pub title: String,
    pub provider: HarnessId,
    pub task_revision: u64,
    pub binding_id: Option<String>,
    pub binding_generation: Option<u32>,
    /// Changes at binding replacement, compaction or rewind.
    pub context_key: String,
    pub history_hash: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ContextCapacitySource {
    ObservedContext,
    ConservativeUnknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextTransferBudget {
    pub capacity_source: ContextCapacitySource,
    pub capacity_tokens: Option<u64>,
    pub used_tokens: Option<u64>,
    pub reserved_tokens: u64,
    pub prompt_tokens: u64,
    pub max_bytes: u64,
    /// One estimated token per UTF-8 byte: an explicit conservative allowance,
    /// not a provider tokenizer or native capacity guarantee.
    pub estimation: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextTransferItem {
    pub message_id: String,
    pub role: String,
    pub text: String,
    pub content_hash: String,
    pub original_bytes: u64,
    pub start_byte: u64,
    pub end_byte: u64,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextTransferOmission {
    pub message_id: String,
    pub role: String,
    pub content_hash: String,
    pub original_bytes: u64,
    pub start_byte: u64,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextTransferPreview {
    pub schema_version: u32,
    pub preview_id: String,
    pub payload_hash: String,
    pub request: ContextTransferRequest,
    pub source: ContextTransferBasis,
    pub target: ContextTransferBasis,
    pub included: Vec<ContextTransferItem>,
    pub omitted: Vec<ContextTransferOmission>,
    pub already_delivered: Vec<String>,
    pub delta: bool,
    pub budget: ContextTransferBudget,
    pub text: String,
    pub estimated_tokens: u64,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ContextTransferStatus {
    Planned,
    Delivered,
    Unknown,
    Failed,
    Consumed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextTransferReceipt {
    pub id: String,
    pub kind: ContextTransferKind,
    pub source: ContextTransferBasis,
    pub target: ContextTransferBasis,
    pub status: ContextTransferStatus,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub included_message_ids: Vec<String>,
    pub omitted_message_ids: Vec<String>,
    pub bytes: u64,
    pub estimated_tokens: u64,
    pub budget: ContextTransferBudget,
    pub correlation_id: Option<String>,
    pub reason: Option<String>,
}
