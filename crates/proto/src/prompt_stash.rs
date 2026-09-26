//! Device-local prompts saved independently of the active composer draft.

use serde::{Deserialize, Serialize};

use crate::{DraftAppshot, DraftBlob, DraftBlobState, DraftContext};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptStashEntry {
    pub id: String,
    /// User-visible first-line preview. The full prompt remains in `text`.
    pub label: String,
    pub text: String,
    #[serde(default)]
    pub attachments: Vec<DraftBlob>,
    #[serde(default)]
    pub appshots: Vec<DraftAppshot>,
    #[serde(default)]
    pub context: DraftContext,
    pub created_at_ms: i64,
}

impl PromptStashEntry {
    pub fn saved_blob_paths(&self) -> impl Iterator<Item = &str> {
        self.attachments.iter().filter_map(|blob| {
            (blob.state == DraftBlobState::Saved)
                .then_some(blob.path.as_deref())
                .flatten()
        })
    }
}
