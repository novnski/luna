//! Device-local composer draft records shared by the desktop and engine.

use serde::{Deserialize, Serialize};

pub const COMPOSER_DRAFT_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DraftBlobState {
    Saved,
    WaitingForUpload,
    NeedsReattachment,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftBlob {
    pub attachment_id: String,
    pub name: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub sha256: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
    pub state: DraftBlobState,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftAppshot {
    pub id: String,
    pub screenshot_attachment_id: String,
    pub app_name: String,
    #[serde(default)]
    pub bundle_identifier: Option<String>,
    #[serde(default)]
    pub window_title: Option<String>,
    pub accessibility_format_version: u32,
    pub accessibility_content: String,
    pub accessibility_truncated: bool,
    #[serde(default)]
    pub screenshot_dimensions: Option<(u32, u32)>,
    pub captured_at_ms: i64,
}

/// Flexible context payload. Review comments are currently the only distinct
/// composer context object; quoted/terminal references already live in text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct DraftContext {
    #[serde(default)]
    pub review_comments: Vec<serde_json::Value>,
    #[serde(default)]
    pub quoted: Option<serde_json::Value>,
    #[serde(default)]
    pub terminal: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComposerDraft {
    pub version: u32,
    pub draft_id: String,
    #[serde(default)]
    pub task_id: Option<String>,
    pub revision: u64,
    pub text: String,
    #[serde(default)]
    pub attachments: Vec<DraftBlob>,
    #[serde(default)]
    pub appshots: Vec<DraftAppshot>,
    #[serde(default)]
    pub context: DraftContext,
    /// Exact existing picker state, kept out of the identity key so changing
    /// accounts/models never orphans the user's content.
    #[serde(default)]
    pub execution: serde_json::Value,
    pub updated_at_ms: i64,
}

impl ComposerDraft {
    pub fn saved_blob_paths(&self) -> impl Iterator<Item = &str> {
        self.attachments.iter().filter_map(|blob| {
            (blob.state == DraftBlobState::Saved)
                .then_some(blob.path.as_deref())
                .flatten()
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DraftSubmissionState {
    Pending,
    Recoverable,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftSubmission {
    pub command_id: String,
    pub state: DraftSubmissionState,
    pub snapshot: ComposerDraft,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub queue_id: Option<String>,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ComposerDraftRead {
    #[serde(default)]
    pub draft: Option<ComposerDraft>,
    #[serde(default)]
    pub submissions: Vec<DraftSubmission>,
}
