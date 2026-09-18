//! Canonical permission resolution. UI labels and legacy sandbox fields never
//! reach adapters directly: the engine intersects the request with provider,
//! parent, machine, and environment ceilings immediately before every run.

use std::path::Path;

use zeron_proto::{
    ApprovalActionKind, ApprovalReviewer, HarnessId, PermissionCeilingSource,
    PermissionDegradationReason, PermissionEscalation, PermissionMode, PermissionNetworkPolicy,
    PermissionPolicy, PermissionScope, ProviderPermissionCapabilities, RunRequest, SandboxLevel,
};

pub fn provider_capabilities(harness: HarnessId) -> ProviderPermissionCapabilities {
    use ApprovalActionKind::{Command, ExternalPath, FileChange, Network, Tool};
    use PermissionMode::{Ask, Auto, Full};

    match harness {
        HarnessId::Codex => ProviderPermissionCapabilities {
            supported_modes: vec![Ask, Auto, Full],
            supports_workspace_sandbox: true,
            supports_automatic_reviewer: true,
            supports_live_mode_change: false,
            supports_task_allowance: true,
            approval_kinds: vec![Command, FileChange, Network, ExternalPath, Tool],
            network_control: "provider sandbox policy".into(),
            known_limitations: vec![
                "Mode changes apply at the next safe turn boundary by reconstructing the app-server session."
                    .into(),
                "Automatic review can be unavailable when account or organization policy disables it; provider startup then fails closed."
                    .into(),
            ],
        },
        HarnessId::ClaudeCode => ProviderPermissionCapabilities {
            supported_modes: vec![Ask, Auto, Full],
            supports_workspace_sandbox: true,
            supports_automatic_reviewer: true,
            supports_live_mode_change: false,
            supports_task_allowance: false,
            approval_kinds: vec![Command, FileChange, Network, ExternalPath, Tool],
            network_control: "Claude permission mode and restricted workspace rules".into(),
            known_limitations: vec![
                "Escalations returned by native auto review still require the user.".into(),
                "Auto requires Claude Code 2.1.218 or later plus an eligible account, provider, and model (Fable, Opus 4.6+, or Sonnet 4.6+)."
                    .into(),
            ],
        },
        HarnessId::Opencode => ProviderPermissionCapabilities {
            supported_modes: vec![Ask, Full],
            supports_workspace_sandbox: true,
            supports_automatic_reviewer: false,
            supports_live_mode_change: false,
            // Runtime-gated: verified 1.x is process scoped, while 2.x
            // persists `always` project-wide and must not advertise it.
            supports_task_allowance: false,
            approval_kinds: vec![Command, FileChange, Network, ExternalPath, Tool],
            network_control: "session-scoped OpenCode permission rules".into(),
            known_limitations: vec![
                "OpenCode --auto blanket-approves requests that are not denied; it is not an automatic reviewer, so Approve for me is unavailable."
                    .into(),
                "Allow for task is exposed only on verified 1.x process-scoped permission replies; 2.x disables it."
                    .into(),
            ],
        },
        HarnessId::Cursor => ProviderPermissionCapabilities {
            supported_modes: vec![],
            supports_workspace_sandbox: false,
            supports_automatic_reviewer: false,
            supports_live_mode_change: false,
            supports_task_allowance: false,
            approval_kinds: vec![],
            network_control: "unverified".into(),
            known_limitations: vec![
                "Cursor 3.6+ documents Auto-review, but the pinned @cursor/sdk 1.0.28 integration exposes no run-mode or sandbox contract; LUNA cannot select it truthfully."
                    .into(),
            ],
        },
        HarnessId::Devin
        | HarnessId::Grok
        | HarnessId::Hermes
        | HarnessId::Pi
        | HarnessId::Antigravity => ProviderPermissionCapabilities {
            supported_modes: vec![],
            supports_workspace_sandbox: false,
            supports_automatic_reviewer: false,
            supports_live_mode_change: false,
            supports_task_allowance: false,
            approval_kinds: vec![],
            network_control: "concrete ACP agent contract unverified".into(),
            known_limitations: vec![
                "ACP permission requests are optional user decisions, not proof that a concrete agent enforces a sandbox or native automatic reviewer."
                    .into(),
            ],
        },
        HarnessId::Mock => ProviderPermissionCapabilities {
            supported_modes: vec![Ask, Auto, Full],
            supports_workspace_sandbox: true,
            supports_automatic_reviewer: true,
            supports_live_mode_change: false,
            supports_task_allowance: true,
            approval_kinds: vec![Command, FileChange, Network, ExternalPath, Tool],
            network_control: "test fixture".into(),
            known_limitations: vec!["Test harness only.".into()],
        },
    }
}

/// Claude's classifier-backed Auto mode is model-gated by the provider. The
/// curated catalog uses concrete ids; the bare `opus`/`sonnet` aliases resolve
/// to current eligible models in the installed CLI. Unknown/default models are
/// not assumed eligible.
pub fn claude_auto_model_supported(model: Option<&str>) -> bool {
    let Some(model) = model.map(str::trim).filter(|model| !model.is_empty()) else {
        return false;
    };
    let model = model.to_ascii_lowercase();
    if model.contains("fable") || matches!(model.as_str(), "opus" | "sonnet") {
        return true;
    }
    ["opus", "sonnet"].into_iter().any(|family| {
        let Some((_, suffix)) = model.split_once(&format!("{family}-")) else {
            return false;
        };
        let mut version = suffix.split(|ch: char| !ch.is_ascii_digit());
        let major = version.next().and_then(|part| part.parse::<u32>().ok());
        let minor = version.next().and_then(|part| part.parse::<u32>().ok());
        matches!((major, minor), (Some(major), _) if major > 4)
            || matches!((major, minor), (Some(4), Some(minor)) if minor >= 6)
    })
}

/// Provider capability at the selected model boundary. Provider-level health
/// can describe conditional Auto support, while an actual run/picker must not
/// advertise Auto for a model the native reviewer excludes.
pub fn provider_capabilities_for(
    harness: HarnessId,
    model: Option<&str>,
) -> ProviderPermissionCapabilities {
    let mut capabilities = provider_capabilities(harness);
    if harness == HarnessId::ClaudeCode && !claude_auto_model_supported(model) {
        capabilities
            .supported_modes
            .retain(|mode| *mode != PermissionMode::Auto);
        capabilities.supports_automatic_reviewer = false;
    }
    capabilities
}

pub fn machine_name() -> String {
    gethostname::gethostname()
        .to_string_lossy()
        .trim()
        .to_string()
        .pipe(|name| {
            if name.is_empty() {
                "this machine".into()
            } else {
                name
            }
        })
}

trait Pipe: Sized {
    fn pipe<T>(self, f: impl FnOnce(Self) -> T) -> T {
        f(self)
    }
}
impl<T> Pipe for T {}

fn mode_from_env(name: &str) -> Option<PermissionMode> {
    match std::env::var(name)
        .ok()?
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "ask" => Some(PermissionMode::Ask),
        "auto" => Some(PermissionMode::Auto),
        "full" => Some(PermissionMode::Full),
        _ => None,
    }
}

fn mapping(harness: HarnessId, mode: PermissionMode) -> String {
    match (harness, mode) {
        (HarnessId::Codex, PermissionMode::Ask) => {
            "workspace-write + on-request + reviewer user".into()
        }
        (HarnessId::Codex, PermissionMode::Auto) => {
            "workspace-write + on-request + reviewer auto_review".into()
        }
        (HarnessId::Codex, PermissionMode::Full) => "danger-full-access + never".into(),
        (HarnessId::ClaudeCode, PermissionMode::Ask) => {
            "acceptEdits + restricted workspace + interactive escalation".into()
        }
        (HarnessId::ClaudeCode, PermissionMode::Auto) => {
            "auto + restricted workspace + user escalation".into()
        }
        (HarnessId::ClaudeCode, PermissionMode::Full) => {
            "bypassPermissions + dangerously-skip-permissions".into()
        }
        (HarnessId::Opencode, PermissionMode::Ask) => {
            "session rules: workspace read/edit allow; sensitive actions ask".into()
        }
        (HarnessId::Opencode, PermissionMode::Full) => {
            "session rule: allow all; stray requests allow once".into()
        }
        (HarnessId::Mock, mode) => format!("mock {mode:?}"),
        _ => "unavailable: provider enforcement is unverified".into(),
    }
}

/// Resolve and stamp a run request. `sandbox` and `auto_approve` are derived
/// outputs only; ambiguous legacy inputs cannot grant Full.
pub fn resolve_run_policy(harness: HarnessId, request: &mut RunRequest) -> PermissionPolicy {
    let capabilities = provider_capabilities_for(harness, request.model.as_deref());
    let requested = request.permission_mode;
    let mut effective = requested;
    let mut ceiling_source = None;
    let mut reason = None;

    let mut clamp = |ceiling: Option<PermissionMode>, source, degradation| {
        if let Some(ceiling) = ceiling
            && effective > ceiling
        {
            effective = ceiling;
            ceiling_source = Some(source);
            reason = Some(degradation);
        }
    };
    clamp(
        request.parent_permission_ceiling,
        PermissionCeilingSource::ParentThread,
        PermissionDegradationReason::ParentCeiling,
    );
    clamp(
        mode_from_env("LUNA_PERMISSION_CEILING"),
        PermissionCeilingSource::Machine,
        PermissionDegradationReason::MachineCeiling,
    );
    clamp(
        request.environment_permission_ceiling,
        PermissionCeilingSource::Environment,
        PermissionDegradationReason::EnvironmentCeiling,
    );

    if !capabilities.supports(effective) {
        effective = [
            PermissionMode::Full,
            PermissionMode::Auto,
            PermissionMode::Ask,
        ]
        .into_iter()
        .find(|candidate| *candidate <= effective && capabilities.supports(*candidate))
        .unwrap_or(PermissionMode::Ask);
        ceiling_source = Some(PermissionCeilingSource::Provider);
        reason = Some(PermissionDegradationReason::ProviderUnsupported);
    }

    let root = std::fs::canonicalize(Path::new(&request.cwd))
        .unwrap_or_else(|_| request.cwd.clone().into())
        .to_string_lossy()
        .into_owned();
    let (scope, reviewer, escalation) = match effective {
        PermissionMode::Ask => (
            PermissionScope::Workspace,
            ApprovalReviewer::User,
            PermissionEscalation::Ask,
        ),
        PermissionMode::Auto => (
            PermissionScope::Workspace,
            ApprovalReviewer::Automatic,
            PermissionEscalation::Ask,
        ),
        PermissionMode::Full => (
            PermissionScope::Full,
            ApprovalReviewer::None,
            PermissionEscalation::None,
        ),
    };
    let network_policy = match (harness, effective) {
        (_, PermissionMode::Full) => PermissionNetworkPolicy::Unrestricted,
        (HarnessId::Codex | HarnessId::ClaudeCode, _) => PermissionNetworkPolicy::Restricted,
        _ => PermissionNetworkPolicy::ProviderDefault,
    };
    let policy = PermissionPolicy {
        requested_mode: requested,
        effective_mode: effective,
        scope,
        reviewer,
        escalation,
        network_policy,
        workspace_roots: vec![root],
        machine_name: machine_name(),
        provider_mapping: mapping(harness, effective),
        ceiling_source,
        degradation_reason: reason,
    };
    request.sandbox = match effective {
        PermissionMode::Full => SandboxLevel::DangerFullAccess,
        PermissionMode::Ask | PermissionMode::Auto => SandboxLevel::WorkspaceWrite,
    };
    // This legacy bit means only "Full stray requests may be accepted". Auto
    // is a native reviewer, never blanket approval.
    request.auto_approve = effective == PermissionMode::Full;
    request.permission_policy = Some(policy.clone());
    policy
}

/// Auto/Full are the only selectable product modes. If either resolves all the
/// way down to internal Ask, running it would put an Ask policy behind an Auto
/// or Full label. Legacy callers that explicitly carry Ask remain safe.
pub fn hidden_ask_fallback(policy: &PermissionPolicy) -> bool {
    policy.requested_mode != PermissionMode::Ask && policy.effective_mode == PermissionMode::Ask
}

pub fn provider_can_run(harness: HarnessId) -> bool {
    !provider_capabilities(harness).supported_modes.is_empty()
}

/// UI/diagnostic preview using the same resolver. Dispatch resolves again and
/// remains authoritative, so a remote/environment ceiling can only make the
/// final result safer.
pub fn resolve_preview(harness: HarnessId, requested: PermissionMode) -> PermissionPolicy {
    resolve_preview_for(harness, None, requested)
}

pub fn resolve_preview_for(
    harness: HarnessId,
    model: Option<&str>,
    requested: PermissionMode,
) -> PermissionPolicy {
    let mut request = RunRequest {
        prompt: String::new(),
        harness: Some(harness),
        provider_account_id: None,
        model: model.map(str::to_owned),
        reasoning: None,
        model_options: Default::default(),
        cwd: ".".into(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: false,
        permission_mode: requested,
        parent_permission_ceiling: None,
        environment_permission_ceiling: None,
        permission_policy: None,
        resume: None,
        attachments: Vec::new(),
        worktree: None,
    };
    resolve_run_policy(harness, &mut request)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(mode: PermissionMode) -> RunRequest {
        RunRequest {
            prompt: "safe test".into(),
            harness: Some(HarnessId::Codex),
            provider_account_id: None,
            model: None,
            reasoning: None,
            model_options: Default::default(),
            cwd: ".".into(),
            sandbox: SandboxLevel::DangerFullAccess,
            auto_approve: true,
            permission_mode: mode,
            parent_permission_ceiling: None,
            environment_permission_ceiling: None,
            permission_policy: None,
            resume: None,
            attachments: vec![],
            worktree: None,
        }
    }

    #[test]
    fn legacy_danger_fields_cannot_broaden_default_ask() {
        let mut request = request(PermissionMode::Ask);
        let policy = resolve_run_policy(HarnessId::Codex, &mut request);
        assert_eq!(policy.effective_mode, PermissionMode::Ask);
        assert_eq!(policy.network_policy, PermissionNetworkPolicy::Restricted);
        assert_eq!(request.sandbox, SandboxLevel::WorkspaceWrite);
        assert!(!request.auto_approve);
    }

    #[test]
    fn unsupported_auto_degrades_to_ask_never_full() {
        let mut request = request(PermissionMode::Auto);
        let policy = resolve_run_policy(HarnessId::Opencode, &mut request);
        assert_eq!(policy.requested_mode, PermissionMode::Auto);
        assert_eq!(policy.effective_mode, PermissionMode::Ask);
        assert!(hidden_ask_fallback(&policy));
        assert_eq!(
            policy.degradation_reason,
            Some(PermissionDegradationReason::ProviderUnsupported)
        );
    }

    #[test]
    fn claude_auto_is_model_gated_and_hidden_ask_is_never_selectable() {
        assert!(claude_auto_model_supported(Some("claude-fable-5-1")));
        assert!(claude_auto_model_supported(Some("claude-opus-4-7")));
        assert!(claude_auto_model_supported(Some("claude-sonnet-5")));
        assert!(!claude_auto_model_supported(Some("claude-haiku-4-5")));
        assert!(!claude_auto_model_supported(None));

        let eligible = resolve_preview_for(
            HarnessId::ClaudeCode,
            Some("claude-opus-4-7"),
            PermissionMode::Auto,
        );
        assert_eq!(eligible.effective_mode, PermissionMode::Auto);
        assert!(!hidden_ask_fallback(&eligible));

        let ineligible = resolve_preview_for(
            HarnessId::ClaudeCode,
            Some("claude-haiku-4-5"),
            PermissionMode::Auto,
        );
        assert_eq!(ineligible.effective_mode, PermissionMode::Ask);
        assert!(hidden_ask_fallback(&ineligible));
    }

    #[test]
    fn every_ceiling_only_reduces_privilege() {
        let mut request = request(PermissionMode::Full);
        request.parent_permission_ceiling = Some(PermissionMode::Auto);
        request.environment_permission_ceiling = Some(PermissionMode::Ask);
        let policy = resolve_run_policy(HarnessId::Codex, &mut request);
        assert_eq!(policy.effective_mode, PermissionMode::Ask);
        assert!(hidden_ask_fallback(&policy));
        assert_eq!(policy.network_policy, PermissionNetworkPolicy::Restricted);
        assert_ne!(request.sandbox, SandboxLevel::DangerFullAccess);
    }

    #[test]
    fn provider_matrix_only_advertises_verified_modes() {
        use PermissionMode::{Ask, Auto, Full};
        assert_eq!(
            provider_capabilities(HarnessId::Codex).supported_modes,
            vec![Ask, Auto, Full]
        );
        assert_eq!(
            provider_capabilities(HarnessId::ClaudeCode).supported_modes,
            vec![Ask, Auto, Full]
        );
        assert_eq!(
            provider_capabilities(HarnessId::Opencode).supported_modes,
            vec![Ask, Full]
        );
        assert!(
            !provider_capabilities(HarnessId::Opencode).supports_task_allowance,
            "task allowance is negotiated from the live OpenCode protocol"
        );
        assert!(
            provider_capabilities(HarnessId::Cursor)
                .supported_modes
                .is_empty()
        );
        assert!(
            provider_capabilities(HarnessId::Hermes)
                .supported_modes
                .is_empty()
        );
        assert_eq!(
            resolve_preview(HarnessId::Codex, Full).network_policy,
            PermissionNetworkPolicy::Unrestricted
        );
    }
}
