//! Provider-aware delivery of a child chat's report into its parent's queue.
//!
//! A child report is model-authored text from a separate provider session.
//! For a Claude parent it travels as Claude's own host-injected cross-session
//! envelope. Claude then classifies it as `origin.kind = "peer"` and tells the
//! model it is not the user's approval. As unmarked text it would read as the
//! user speaking. Claude parks a peer message whose declared permission class
//! differs from the receiver's, and LUNA cannot answer that hold, so
//! continuation is skipped up front with a visible notice. Other providers
//! keep the existing report text unchanged.

use luna_harness::claude::peer::{delivers_without_hold, luna_child_report_envelope};
use luna_proto::{Chat, HarnessId, PermissionMode};

pub(crate) enum ParentContinuation {
    /// Queue this exact text as the parent's held follow-up.
    Queue(String),
    /// Do not continue the parent; show this notice instead.
    Skip(String),
}

pub(crate) fn mode_label(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::Ask => "Ask",
        PermissionMode::Auto => "Auto",
        PermissionMode::Full => "Full access",
    }
}

pub(crate) fn parent_continuation(parent: &Chat, child: &Chat, report: &str) -> ParentContinuation {
    let Some(parent_config) = parent.config.as_ref() else {
        return ParentContinuation::Queue(report.to_owned());
    };
    if parent_config.harness != HarnessId::ClaudeCode {
        return ParentContinuation::Queue(report.to_owned());
    }
    let parent_mode = parent_config.effective_permission_mode;
    let child_mode = child
        .config
        .as_ref()
        .map(|config| config.effective_permission_mode)
        .or_else(|| {
            child
                .child_thread
                .as_ref()
                .map(|thread| thread.permission_ceiling)
        })
        .unwrap_or(PermissionMode::Ask);
    if !delivers_without_hold(child_mode, parent_mode) {
        return ParentContinuation::Skip(format!(
            "Not continued automatically: this chat runs in {} and the child in {}. Claude holds a cross-session report whose sender permission class differs from its own, and LUNA has no approval surface for that hold. Review the child's result above and continue manually.",
            mode_label(parent_mode),
            mode_label(child_mode)
        ));
    }
    match luna_child_report_envelope(&child.id, child.title.as_deref(), child_mode, report) {
        Some(envelope) => ParentContinuation::Queue(envelope),
        None => ParentContinuation::Skip(
            "Not continued automatically: this child's id cannot be expressed as a Claude peer session, and LUNA does not send child output to Claude as your own words. Continue manually."
                .into(),
        ),
    }
}
