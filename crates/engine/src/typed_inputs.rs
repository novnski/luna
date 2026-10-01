//! Provider-aware admission of typed composer selections (CC-12).
//!
//! Codex receives plugin/app/skill items natively. Claude receives exactly one
//! skill or custom command, dispatched as its native slash invocation by the
//! adapter (which also checks the launch's own effective catalog). Every other
//! provider, and any Claude plugin/app mention, is rejected before acceptance
//! so the draft and its selections stay recoverable.
use luna_proto::{ComposerInput, HarnessId};

use crate::EngineError;

pub(crate) fn admit(
    harness: HarnessId,
    attachments: &[String],
    items: &[ComposerInput],
) -> Result<(), EngineError> {
    match harness {
        HarnessId::Codex => Ok(luna_harness::validate_composer_inputs(attachments, items)?),
        HarnessId::ClaudeCode => {
            luna_harness::claude::extensions::typed_skill(items)?;
            Ok(())
        }
        _ if items.is_empty() => Ok(()),
        _ => Err(EngineError::Other(
            "Native plugin, app and skill selections require Codex or Claude. Keep the draft and choose a supporting provider or remove the selections.".into(),
        )),
    }
}
