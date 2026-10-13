//! Canonical permission resolution. UI labels and legacy sandbox fields never
//! reach adapters directly: the engine intersects the request with provider,
//! parent, machine, and environment ceilings immediately before every run.

use std::path::Path;

use luna_proto::{
    ApprovalActionKind, ApprovalReviewer, HarnessId, PermissionCeilingSource,
    PermissionDegradationReason, PermissionEscalation, PermissionMode, PermissionNetworkPolicy,
    PermissionPolicy, PermissionScope, ProviderPermissionCapabilities, RunRequest, SandboxLevel,
};

pub fn provider_capabilities(harness: HarnessId) -> ProviderPermissionCapabilities {
    use ApprovalActionKind::{Command, ExternalPath, FileChange, Network, Tool};
    use PermissionMode::{Ask, Auto, Full};

    // CORE-08: a registered extension provider's reviewed declaration.
    if let Some(declared) = crate::extensions::providers::declared_permissions(harness) {
        return declared;
    }

    match (harness).kind() {
        luna_proto::BuiltinHarnessId::Codex => ProviderPermissionCapabilities {
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
                "Auto never asks: a request Codex's automatic review hands back to a person is declined and its reason shown; Full access runs it."
                    .into(),
            ],
        },
        luna_proto::BuiltinHarnessId::ClaudeCode => ProviderPermissionCapabilities {
            supported_modes: vec![Ask, Auto, Full],
            supports_workspace_sandbox: true,
            supports_automatic_reviewer: true,
            supports_live_mode_change: false,
            supports_task_allowance: false,
            approval_kinds: vec![Command, FileChange, Network, ExternalPath, Tool],
            network_control: "Claude permission mode and restricted workspace rules".into(),
            known_limitations: vec![
                "Auto never asks: a call Claude's auto review escalates instead of deciding is denied with Claude's reason; Full access runs it."
                    .into(),
                "Ask and Auto run Claude Code restricted: user, project and local settings files are not loaded, so their custom commands, output styles and hooks are unavailable there (the command list shows only what loads). Full loads them."
                    .into(),
                "Ask and Auto grant the built-in tools of Claude Code 2.1.286 by name, including Bash and WebFetch; Workflow and CronCreate stay unavailable, and tools added by newer CLI versions need an updated grant."
                    .into(),
                "Auto requires Claude Code 2.1.218 or later plus an eligible account, provider, and model (Fable, Opus 4.6+, or Sonnet 4.6+)."
                    .into(),
            ],
        },
        luna_proto::BuiltinHarnessId::Opencode => ProviderPermissionCapabilities {
            supported_modes: vec![Ask, Full],
            // Rule-based workspace confinement (external paths ask in Ask),
            // like Claude's restricted workspace: LUNA writes the session's
            // OpenCode 2 rules and reads them back before the first prompt,
            // and a run whose rules didn't apply never starts.
            supports_workspace_sandbox: true,
            supports_automatic_reviewer: false,
            supports_live_mode_change: false,
            // Like Claude, negotiated per approval: a card offers "Allow for
            // session" when the request carries OpenCode's own save patterns,
            // kept as a LUNA-owned session rule. LUNA never sends OpenCode's
            // project-wide `always`.
            supports_task_allowance: false,
            approval_kinds: vec![Command, FileChange, Network, ExternalPath, Tool],
            network_control:
                "OpenCode session permission rules (web fetch and search ask in Ask), read back before each run"
                    .into(),
            known_limitations: vec![
                "OpenCode has no automatic reviewer, so Approve for me is unavailable.".into(),
                "OpenCode has no OS sandbox: Ask confines tools with OpenCode's own permission rules, and an approved shell command runs unconfined."
                    .into(),
                "Allow for session is a LUNA-owned rule on the chat's OpenCode session, never an OpenCode saved rule: it lasts until LUNA restarts or the chat's permission mode changes, and covers the session that asked (a sub-agent's grant stays with that sub-agent)."
                    .into(),
                "Native sub-agents are capped at the chat's mode when OpenCode creates them; their own agent restrictions and your configured OpenCode denies stay in force."
                    .into(),
            ],
        },
        luna_proto::BuiltinHarnessId::Mock => ProviderPermissionCapabilities {
            supported_modes: vec![Ask, Auto, Full],
            supports_workspace_sandbox: true,
            supports_automatic_reviewer: true,
            supports_live_mode_change: false,
            supports_task_allowance: true,
            approval_kinds: vec![Command, FileChange, Network, ExternalPath, Tool],
            network_control: "test fixture".into(),
            known_limitations: vec!["Test harness only.".into()],
        },
        _ => ProviderPermissionCapabilities {
            supported_modes: vec![],
            supports_workspace_sandbox: false,
            supports_automatic_reviewer: false,
            supports_live_mode_change: false,
            supports_task_allowance: false,
            approval_kinds: vec![],
            network_control: "provider removed".into(),
            known_limitations: vec!["This provider is no longer supported by LUNA.".into()],
        },
    }
}

/// Providers whose adapter enforces an engine-resolved read-only sandbox
/// (Codex natively; Claude Code by launching without its edit, write and
/// command tools). Review-constrained and legacy read-only threads refuse
/// every other provider instead of silently running with write access.
pub fn provider_preserves_read_only(harness: HarnessId) -> bool {
    matches!(
        (harness).kind(),
        luna_proto::BuiltinHarnessId::Codex
            | luna_proto::BuiltinHarnessId::ClaudeCode
            | luna_proto::BuiltinHarnessId::Mock
    )
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

/// Product Full access supersedes the native providers' legacy sandbox field.
/// Preserve explicit legacy restrictions for other modes and test harnesses.
pub(crate) fn effective_sandbox(
    harness: HarnessId,
    sandbox: SandboxLevel,
    mode: PermissionMode,
) -> SandboxLevel {
    if matches!(
        (harness).kind(),
        luna_proto::BuiltinHarnessId::Codex | luna_proto::BuiltinHarnessId::ClaudeCode
    ) && mode == PermissionMode::Full
    {
        SandboxLevel::DangerFullAccess
    } else {
        sandbox
    }
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
    if let Some(declared) = crate::extensions::providers::declared_mapping(harness, mode) {
        return declared;
    }
    match ((harness).kind(), mode) {
        (luna_proto::BuiltinHarnessId::Codex, PermissionMode::Ask) => {
            "workspace-write + on-request + reviewer user".into()
        }
        (luna_proto::BuiltinHarnessId::Codex, PermissionMode::Auto) => {
            "workspace-write + on-request + reviewer auto_review".into()
        }
        (luna_proto::BuiltinHarnessId::Codex, PermissionMode::Full) => "danger-full-access + never".into(),
        (luna_proto::BuiltinHarnessId::ClaudeCode, PermissionMode::Ask) => {
            "acceptEdits + restricted workspace + named command/web tools + interactive escalation"
                .into()
        }
        (luna_proto::BuiltinHarnessId::ClaudeCode, PermissionMode::Auto) => {
            "auto + restricted workspace + named command/web tools + escalations denied".into()
        }
        (luna_proto::BuiltinHarnessId::ClaudeCode, PermissionMode::Full) => {
            "bypassPermissions + dangerously-skip-permissions".into()
        }
        (luna_proto::BuiltinHarnessId::Opencode, PermissionMode::Ask) => {
            "session rules: reads allow; shell, edits, web and external paths ask; configured denies stay"
                .into()
        }
        (luna_proto::BuiltinHarnessId::Opencode, PermissionMode::Full) => {
            "session rules: allow all except configured denies; stray requests allow once".into()
        }
        (luna_proto::BuiltinHarnessId::Mock, mode) => format!("mock {mode:?}"),
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
        // Auto never asks: what the native reviewer escalates is declined
        // (docs/pending-tasks/permission-modes).
        PermissionMode::Auto => (
            PermissionScope::Workspace,
            ApprovalReviewer::Automatic,
            PermissionEscalation::Deny,
        ),
        PermissionMode::Full => (
            PermissionScope::Full,
            ApprovalReviewer::None,
            PermissionEscalation::None,
        ),
    };
    let network_policy = match ((harness).kind(), effective) {
        (_, PermissionMode::Full) => PermissionNetworkPolicy::Unrestricted,
        // Codex workspace-write deliberately enables network access, matching
        // the adapter's thread config and turn policy. The workspace boundary
        // applies to filesystem writes, not a network deny policy.
        (luna_proto::BuiltinHarnessId::Codex, _) => PermissionNetworkPolicy::Unrestricted,
        (luna_proto::BuiltinHarnessId::ClaudeCode, _) => PermissionNetworkPolicy::Restricted,
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
    let mut request = preview_request_for(harness, model, requested);
    resolve_run_policy(harness, &mut request)
}

/// A preview request can pass through the same inherited-access admission as
/// dispatch before its policy is resolved, without starting a provider.
pub(crate) fn preview_request_for(
    harness: HarnessId,
    model: Option<&str>,
    requested: PermissionMode,
) -> RunRequest {
    RunRequest {
        review: None,
        goal: None,
        input_items: Vec::new(),
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(mode: PermissionMode) -> RunRequest {
        RunRequest {
            review: None,
            goal: None,
            input_items: Vec::new(),
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
        assert_eq!(policy.network_policy, PermissionNetworkPolicy::Unrestricted);
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
        assert_eq!(policy.network_policy, PermissionNetworkPolicy::Unrestricted);
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
