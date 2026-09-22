use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitFileState {
    pub path: String,
    pub staged: bool,
    pub unstaged: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitActionPreview {
    pub checkout_id: String,
    pub cwd: String,
    pub branch: String,
    pub head: String,
    pub remote: Option<String>,
    pub files: Vec<GitFileState>,
    /// Opaque freshness token covering checkout, index, worktree and push target.
    pub token: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GitActionKind {
    Stage,
    Unstage,
    Commit,
    Push,
    CommitAndPush,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitActionRequest {
    pub cwd: String,
    pub checkout_id: String,
    pub expected_token: String,
    pub kind: GitActionKind,
    #[serde(default)]
    pub paths: Vec<String>,
    pub message: Option<String>,
    pub remote: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitActionReceipt {
    pub commit_sha: Option<String>,
    pub committed: bool,
    pub pushed: bool,
    /// The push process ended without a trustworthy exit status; remote state
    /// must be checked before another push attempt.
    #[serde(default)]
    pub push_uncertain: bool,
    pub error: Option<String>,
    pub preview: GitActionPreview,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitMessageRequest {
    pub cwd: String,
    pub checkout_id: String,
    pub expected_token: String,
    pub paths: Vec<String>,
    pub harness: Option<crate::HarnessId>,
}
