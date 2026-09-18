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
            ],
        },
        HarnessId::Opencode => ProviderPermissionCapabilities {
            supported_modes: vec![Ask, Full],
            supports_workspace_sandbox: true,
            supports_automatic_reviewer: false,
            supports_live_mode_change: false,
            supports_task_allowance: true,
            approval_kinds: vec![Command, FileChange, Network, ExternalPath, Tool],
            network_control: "session-scoped OpenCode permission rules".into(),
            known_limitations: vec![
                "Approve for me safely degrades to Ask because OpenCode exposes no automatic reviewer."
                    .into(),
                "Allow for task is session-scoped and is never persisted to project configuration."
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
                "The pinned Cursor SDK exposes no verified sandbox or approval contract; LUNA does not start it under a claimed permission mode."
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
                "ACP permission choices do not prove that this concrete agent enforces a workspace sandbox."
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
    let capabilities = provider_capabilities(harness);
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

pub fn provider_can_run(harness: HarnessId) -> bool {
    !provider_capabilities(harness).supported_modes.is_empty()
}

/// UI/diagnostic preview using the same resolver. Dispatch resolves again and
/// remains authoritative, so a remote/environment ceiling can only make the
/// final result safer.
pub fn resolve_preview(harness: HarnessId, requested: PermissionMode) -> PermissionPolicy {
    let mut request = RunRequest {
        prompt: String::new(),
        harness: Some(harness),
        model: None,
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
        assert_eq!(
            policy.degradation_reason,
            Some(PermissionDegradationReason::ProviderUnsupported)
        );
    }

    #[test]
    fn every_ceiling_only_reduces_privilege() {
        let mut request = request(PermissionMode::Full);
        request.parent_permission_ceiling = Some(PermissionMode::Auto);
        request.environment_permission_ceiling = Some(PermissionMode::Ask);
        let policy = resolve_run_policy(HarnessId::Codex, &mut request);
        assert_eq!(policy.effective_mode, PermissionMode::Ask);
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
