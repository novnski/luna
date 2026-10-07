//! Provider-aware admission for staged images and typed composer selections.
//!
//! One decision point for every route that admits a user turn (fresh
//! dispatch, durable Run/Steer commands, explicit in-turn steering and queue
//! steering). Codex keeps its native validator; Claude Code validates its own
//! inline-image and native-skill contract; OpenCode validates its native
//! `files[]` contract (PNG/JPEG/GIF/WebP up to 20 MiB, decodable, on this
//! host, and the model's image capability when its catalog was read) and
//! typed skills (like Claude: skills only, no plugin/app mentions).
//! Rejections are `RejectedBeforeAcceptance`, so the caller keeps the exact
//! draft envelope.
use crate::EngineError;
use luna_proto::{ComposerInput, HarnessId};

/// Validate one turn's images and typed selections for `harness` before any
/// provider work. `model` is the turn's `provider/model` selection, used by
/// providers whose models differ in image support. Providers without a
/// typed-input contract still reject selections; their image handling is
/// unchanged.
pub(crate) fn admit(
    harness: HarnessId,
    model: Option<&str>,
    attachments: &[String],
    input_items: &[ComposerInput],
) -> Result<(), EngineError> {
    match harness {
        HarnessId::Codex => luna_harness::validate_composer_inputs(attachments, input_items)?,
        HarnessId::ClaudeCode => {
            luna_harness::claude::validate_inputs(attachments, input_items)?;
            // CC-12: one Claude skill invocation per line, no plugin/app.
            luna_harness::claude::extensions::typed_skill(input_items)?;
        }
        HarnessId::Opencode => {
            luna_harness::opencode::validate_inputs(model, attachments, input_items)?;
        }
        _ if !input_items.is_empty() => {
            return Err(EngineError::Other(
                "Native plugin, app and skill selections need Codex (Claude Code and OpenCode run skills only). Keep the draft and choose a supported provider or remove the selections.".into(),
            ));
        }
        _ => {}
    }
    Ok(())
}

/// Whether an in-turn steer (explicit or queued) may carry staged images and
/// typed selections to this provider. OpenCode delivers a steer's images as
/// native `files[]` at its boundary, like any other route.
pub(crate) fn steers_native_inputs(harness: HarnessId) -> bool {
    matches!(
        harness,
        HarnessId::Codex | HarnessId::ClaudeCode | HarnessId::Opencode
    )
}
