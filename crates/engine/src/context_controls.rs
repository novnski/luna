//! Provider capability policy for harness-owned context controls.

use luna_proto::{CompactMechanism, ContextControlCapabilities, HarnessId};

pub fn capabilities_for(harness: HarnessId) -> ContextControlCapabilities {
    let (context_usage, compact, unavailable, note) = match harness {
        HarnessId::Codex => (
            true,
            Some(CompactMechanism::NativeOperation),
            None,
            "Applies to the next turn on the same Codex thread; the provider retains its context unless it reports otherwise.",
        ),
        HarnessId::Opencode => (
            true,
            Some(CompactMechanism::ProviderCommand),
            None,
            "Applies to the next turn on the same OpenCode session; OpenCode owns any resulting compaction.",
        ),
        HarnessId::Mock => (
            true,
            Some(CompactMechanism::NativeOperation),
            None,
            "Applies to the next synthetic turn without changing the current turn.",
        ),
        HarnessId::ClaudeCode => (
            true,
            None,
            Some("LUNA has no verified out-of-turn Claude compact operation."),
            "Applies to the next resumed Claude turn; Claude does not expose a context-reset readback here.",
        ),
        _ => (
            false,
            None,
            Some("This harness does not expose a verified native compact operation."),
            "Applies to the next turn; this harness does not report whether the model change resets context.",
        ),
    };
    ContextControlCapabilities {
        context_usage,
        compact,
        compact_unavailable_reason: unavailable.map(str::to_string),
        clear_context: true,
        model_change: true,
        provider_switch: true,
        model_change_note: note.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_verified_compactors_are_enabled() {
        assert_eq!(
            capabilities_for(HarnessId::Codex).compact,
            Some(CompactMechanism::NativeOperation)
        );
        assert_eq!(
            capabilities_for(HarnessId::Opencode).compact,
            Some(CompactMechanism::ProviderCommand)
        );
        assert!(capabilities_for(HarnessId::ClaudeCode).compact.is_none());
        assert!(
            capabilities_for(HarnessId::ClaudeCode)
                .compact_unavailable_reason
                .is_some()
        );
    }
}
