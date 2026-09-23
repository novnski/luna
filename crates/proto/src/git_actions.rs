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

/// One-shot PR lookup for the exact checkout/branch/remote shown by Git actions.
/// NoMatch is only returned after a successful provider query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum GitPrStatus {
    Linked {
        #[serde(rename = "changeRequest")]
        change_request: crate::ChangeRequestSummary,
    },
    NoMatch,
    UnsupportedHost,
    Unavailable {
        reason: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitPrStatusRequest {
    pub cwd: String,
    pub checkout_id: String,
    pub expected_token: String,
    pub branch: String,
    pub remote: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pr_status_wire_keeps_no_match_distinct_from_unsupported_and_unavailable() {
        for (status, state) in [
            (GitPrStatus::NoMatch, "noMatch"),
            (GitPrStatus::UnsupportedHost, "unsupportedHost"),
            (
                GitPrStatus::Unavailable {
                    reason: "offline".into(),
                },
                "unavailable",
            ),
        ] {
            let wire = serde_json::to_value(&status).unwrap();
            assert_eq!(wire["state"], state);
            assert_eq!(serde_json::from_value::<GitPrStatus>(wire).unwrap(), status);
        }
        let linked = GitPrStatus::Linked {
            change_request: crate::ChangeRequestSummary {
                provider: "github".into(),
                number: 12,
                title: "Test".into(),
                url: "https://github.invalid/alice/repo/pull/12".into(),
                state: crate::ChangeRequestState::Open,
                base_ref: "main".into(),
                head_ref: "feature".into(),
            },
        };
        let wire = serde_json::to_value(&linked).unwrap();
        assert_eq!(wire["state"], "linked");
        assert_eq!(wire["changeRequest"]["number"], 12);
        assert_eq!(serde_json::from_value::<GitPrStatus>(wire).unwrap(), linked);
    }
}
