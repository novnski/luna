//! Provider-aware admission for staged images and typed composer selections.
//!
//! One decision point for every route that admits a user turn (fresh
//! dispatch, durable Run/Steer commands, explicit in-turn steering and queue
//! steering). Codex keeps its native validator; Claude Code validates its own
//! inline-image and native-skill contract. Rejections are
//! `RejectedBeforeAcceptance`, so the caller keeps the exact draft envelope.
use crate::EngineError;
use luna_proto::{ComposerInput, HarnessId};

/// Validate one turn's images and typed selections for `harness` before any
/// provider work. Providers without a typed-input contract still reject
/// selections; their image handling is unchanged.
pub(crate) fn admit(
    harness: HarnessId,
    attachments: &[String],
    input_items: &[ComposerInput],
) -> Result<(), EngineError> {
    match harness {
        HarnessId::Codex => luna_harness::validate_composer_inputs(attachments, input_items)?,
        HarnessId::ClaudeCode => luna_harness::claude::validate_inputs(attachments, input_items)?,
        _ if !input_items.is_empty() => {
            return Err(EngineError::Other(
                "Native plugin, app and skill selections need Codex (Claude Code runs skills only). Keep the draft and choose a supported provider or remove the selections.".into(),
            ));
        }
        _ => {}
    }
    Ok(())
}

/// Whether an in-turn steer (explicit or queued) may carry staged images and
/// typed selections to this provider.
pub(crate) fn steers_native_inputs(harness: HarnessId) -> bool {
    matches!(harness, HarnessId::Codex | HarnessId::ClaudeCode)
}
