//! Minimal LOCAL setup snapshot shared by the new-session hint and Settings.
//! It normalizes existing harness/account/catalog/workspace evidence only; it
//! does not install, authenticate, change permissions, or become the eventual
//! full remote/MCP/OS diagnostics service.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use zeron_proto::{AgentAccountsSnapshot, AgentAuthenticationState, HarnessId, Model};

use crate::agent_accounts::AgentAccounts;
use crate::registry::{HarnessDescriptor, HarnessRegistry, descriptor_enabled};

const CONTRACT_VERSION: u32 = 1;
const PROBE_TIMEOUT: Duration = Duration::from_secs(12);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SetupOverall {
    Ready,
    NeedsAction,
    Degraded,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EvidenceFreshness {
    Fresh,
    Stale,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EvidenceState {
    Confirmed,
    Missing,
    Unsupported,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProviderReadinessState {
    NotInstalled,
    Installed,
    Configured,
    Authenticated,
    Available,
    Unsupported,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SetupCapabilityState {
    Supported,
    Unsupported,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SetupCapabilityKind {
    ModelSelection,
    Reasoning,
    Steering,
    Resume,
    Fork,
    ApprovalModes,
    NativeSessionIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupCapability {
    pub kind: SetupCapabilityKind,
    pub state: SetupCapabilityState,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SetupRemediationKind {
    Refresh,
    OpenAgents,
    OpenAccounts,
    SelectProject,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupRemediation {
    pub kind: SetupRemediationKind,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderReadiness {
    pub harness: HarnessId,
    pub name: String,
    pub state: ProviderReadinessState,
    pub freshness: EvidenceFreshness,
    pub evidence_checked_at_ms: i64,
    pub installed: EvidenceState,
    pub configured: EvidenceState,
    pub authenticated: EvidenceState,
    pub enabled: bool,
    pub detail: String,
    pub model_count: usize,
    pub capabilities: Vec<SetupCapability>,
    pub remediation: Option<SetupRemediation>,
}

impl ProviderReadiness {
    pub fn is_fresh_available(&self) -> bool {
        self.state == ProviderReadinessState::Available
            && self.freshness == EvidenceFreshness::Fresh
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkspaceReadinessState {
    Available,
    NeedsAction,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkspaceCheckoutKind {
    CurrentCheckout,
    LinkedWorktree,
    Directory,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceReadiness {
    pub canonical_path: Option<String>,
    pub state: WorkspaceReadinessState,
    pub checkout_kind: WorkspaceCheckoutKind,
    pub detail: String,
    pub remediation: Option<SetupRemediation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupHealthSnapshot {
    pub contract_version: u32,
    pub environment_id: String,
    pub checked_at_ms: i64,
    pub overall: SetupOverall,
    pub workspace: WorkspaceReadiness,
    pub providers: Vec<ProviderReadiness>,
}

#[derive(Clone, Default)]
pub struct SetupHealth {
    last_available: Arc<Mutex<HashMap<HarnessId, ProviderReadiness>>>,
}

impl SetupHealth {
    fn cache(&self) -> MutexGuard<'_, HashMap<HarnessId, ProviderReadiness>> {
        self.last_available
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    pub async fn snapshot(
        &self,
        environment_id: &str,
        workspace_path: Option<&str>,
        registry: &Arc<HarnessRegistry>,
        agent_accounts: &AgentAccounts,
        native_session_harnesses: &HashSet<HarnessId>,
    ) -> SetupHealthSnapshot {
        let workspace = inspect_workspace(workspace_path);
        let accounts = agent_accounts.list(false).await.ok();
        let mut providers = Vec::new();
        for descriptor in registry
            .descriptors()
            .into_iter()
            .filter(|descriptor| descriptor.id != HarnessId::Mock)
        {
            providers.push(
                self.inspect_provider(
                    descriptor,
                    accounts.as_ref(),
                    registry,
                    native_session_harnesses,
                )
                .await,
            );
        }

        let fresh = providers.iter().any(ProviderReadiness::is_fresh_available);
        let stale = providers.iter().any(|provider| {
            provider.state == ProviderReadinessState::Available
                && provider.freshness == EvidenceFreshness::Stale
        });
        let unknown = providers.iter().any(|provider| {
            provider.state == ProviderReadinessState::Unknown
                || provider.freshness == EvidenceFreshness::Stale
        });
        let overall = match (workspace.state, fresh, stale, unknown) {
            (WorkspaceReadinessState::Available, true, _, _) => SetupOverall::Ready,
            (WorkspaceReadinessState::Available, false, true, _) => SetupOverall::Degraded,
            (_, false, _, true) => SetupOverall::Unavailable,
            _ => SetupOverall::NeedsAction,
        };
        SetupHealthSnapshot {
            contract_version: CONTRACT_VERSION,
            environment_id: environment_id.into(),
            checked_at_ms: chrono::Utc::now().timestamp_millis(),
            overall,
            workspace,
            providers,
        }
    }

    async fn inspect_provider(
        &self,
        descriptor: HarnessDescriptor,
        accounts: Option<&AgentAccountsSnapshot>,
        registry: &Arc<HarnessRegistry>,
        native_session_harnesses: &HashSet<HarnessId>,
    ) -> ProviderReadiness {
        let now = chrono::Utc::now().timestamp_millis();
        let native_session_identity_observed = native_session_harnesses.contains(&descriptor.id);
        if !descriptor.installed {
            return unprobed(
                descriptor,
                ProviderReadinessState::NotInstalled,
                EvidenceState::Missing,
                "CLI not found on this machine.",
                SetupRemediationKind::OpenAgents,
                now,
                native_session_identity_observed,
            );
        }
        if !descriptor_enabled(&descriptor) {
            return unprobed(
                descriptor,
                ProviderReadinessState::Installed,
                EvidenceState::Confirmed,
                "Installed, but disabled for this machine.",
                SetupRemediationKind::OpenAgents,
                now,
                native_session_identity_observed,
            );
        }

        let models = match registry.resolve(descriptor.id) {
            Ok(harness) => tokio::time::timeout(PROBE_TIMEOUT, harness.models()).await,
            Err(_) => {
                return self.stale_or_unknown(
                    &descriptor,
                    "Provider resolution failed.",
                    now,
                    native_session_identity_observed,
                );
            }
        };
        let models = match models {
            Ok(Ok(models)) => models,
            Ok(Err(_)) => {
                return self.stale_or_unknown(
                    &descriptor,
                    "Catalog probe failed.",
                    now,
                    native_session_identity_observed,
                );
            }
            Err(_) => {
                return self.stale_or_unknown(
                    &descriptor,
                    "Catalog probe timed out.",
                    now,
                    native_session_identity_observed,
                );
            }
        };
        let configured = if models.is_empty() {
            EvidenceState::Missing
        } else {
            EvidenceState::Confirmed
        };
        let authenticated = authentication(descriptor.id, accounts);
        let state = classify_provider(descriptor.id, configured, authenticated);
        let mut provider = ProviderReadiness {
            harness: descriptor.id,
            name: descriptor.name.clone(),
            state,
            freshness: EvidenceFreshness::Fresh,
            evidence_checked_at_ms: now,
            installed: EvidenceState::Confirmed,
            configured,
            authenticated,
            enabled: true,
            detail: provider_detail(state, authenticated, models.len()),
            model_count: models.len(),
            capabilities: capabilities(
                &descriptor,
                Some(&models),
                native_session_identity_observed,
            ),
            remediation: remediation(state, authenticated),
        };
        if provider.is_fresh_available() {
            self.cache().insert(provider.harness, provider.clone());
        } else if state == ProviderReadinessState::Authenticated && models.is_empty() {
            provider.detail = "Authenticated, but the model catalog is empty.".into();
        }
        provider
    }

    fn stale_or_unknown(
        &self,
        descriptor: &HarnessDescriptor,
        failure: &str,
        now: i64,
        native_session_identity_observed: bool,
    ) -> ProviderReadiness {
        if let Some(mut previous) = self.cache().get(&descriptor.id).cloned() {
            previous.freshness = EvidenceFreshness::Stale;
            previous.detail = format!("Last known available; refresh failed: {failure}");
            previous.remediation = Some(remediation_value(SetupRemediationKind::Refresh));
            if native_session_identity_observed
                && let Some(capability) = previous.capabilities.iter_mut().find(|capability| {
                    capability.kind == SetupCapabilityKind::NativeSessionIdentity
                })
            {
                capability.state = SetupCapabilityState::Supported;
                capability.detail = "Observed from a LUNA provider session.".into();
            }
            return previous;
        }
        unprobed(
            descriptor.clone(),
            ProviderReadinessState::Unknown,
            EvidenceState::Confirmed,
            failure,
            SetupRemediationKind::Refresh,
            now,
            native_session_identity_observed,
        )
    }
}

fn known_account_provider(harness: HarnessId) -> bool {
    matches!(
        harness,
        HarnessId::ClaudeCode | HarnessId::Codex | HarnessId::Cursor
    )
}

fn authentication(harness: HarnessId, accounts: Option<&AgentAccountsSnapshot>) -> EvidenceState {
    if !known_account_provider(harness) {
        return EvidenceState::Unsupported;
    }
    let Some(accounts) = accounts else {
        return EvidenceState::Unknown;
    };
    if accounts.accounts.iter().any(|account| {
        account.harness == harness
            && account.active
            && account.authentication == AgentAuthenticationState::Authenticated
    }) {
        EvidenceState::Confirmed
    } else if accounts.accounts.iter().any(|account| {
        account.harness == harness
            && account.active
            && account.authentication == AgentAuthenticationState::Expired
    }) {
        EvidenceState::Missing
    } else if accounts
        .accounts
        .iter()
        .any(|account| account.harness == harness && account.active)
    {
        EvidenceState::Unknown
    } else {
        EvidenceState::Missing
    }
}

fn classify_provider(
    _harness: HarnessId,
    configured: EvidenceState,
    authenticated: EvidenceState,
) -> ProviderReadinessState {
    if configured != EvidenceState::Confirmed {
        return if authenticated == EvidenceState::Confirmed {
            ProviderReadinessState::Authenticated
        } else {
            ProviderReadinessState::Installed
        };
    }
    match authenticated {
        EvidenceState::Confirmed => ProviderReadinessState::Available,
        EvidenceState::Missing | EvidenceState::Unsupported => ProviderReadinessState::Configured,
        EvidenceState::Unknown => ProviderReadinessState::Unknown,
    }
}

fn provider_detail(
    state: ProviderReadinessState,
    authentication: EvidenceState,
    models: usize,
) -> String {
    match state {
        ProviderReadinessState::Available => format!("Authenticated with {models} models."),
        ProviderReadinessState::Configured if authentication == EvidenceState::Unsupported => {
            format!("Catalog available ({models} models); execution readiness is unverified.")
        }
        ProviderReadinessState::Configured => "Configured; sign-in is still required.".into(),
        ProviderReadinessState::Authenticated => "Authenticated; no models are available.".into(),
        ProviderReadinessState::Installed => "Installed, but not ready to run.".into(),
        ProviderReadinessState::NotInstalled => "CLI not found on this machine.".into(),
        ProviderReadinessState::Unsupported => "Unsupported by this build.".into(),
        ProviderReadinessState::Unknown => "Readiness could not be verified.".into(),
    }
}

fn remediation(
    state: ProviderReadinessState,
    authentication: EvidenceState,
) -> Option<SetupRemediation> {
    match state {
        ProviderReadinessState::Available => None,
        ProviderReadinessState::Configured if authentication == EvidenceState::Missing => {
            Some(remediation_value(SetupRemediationKind::OpenAccounts))
        }
        ProviderReadinessState::Configured => {
            Some(remediation_value(SetupRemediationKind::Refresh))
        }
        ProviderReadinessState::Installed | ProviderReadinessState::NotInstalled => {
            Some(remediation_value(SetupRemediationKind::OpenAgents))
        }
        ProviderReadinessState::Authenticated
        | ProviderReadinessState::Unsupported
        | ProviderReadinessState::Unknown => Some(remediation_value(SetupRemediationKind::Refresh)),
    }
}

fn remediation_value(kind: SetupRemediationKind) -> SetupRemediation {
    let label = match kind {
        SetupRemediationKind::Refresh => "Check again",
        SetupRemediationKind::OpenAgents => "Manage agents",
        SetupRemediationKind::OpenAccounts => "Open accounts",
        SetupRemediationKind::SelectProject => "Choose project",
    };
    SetupRemediation {
        kind,
        label: label.into(),
    }
}

fn unprobed(
    descriptor: HarnessDescriptor,
    state: ProviderReadinessState,
    installed: EvidenceState,
    detail: &str,
    remediation: SetupRemediationKind,
    now: i64,
    native_session_identity_observed: bool,
) -> ProviderReadiness {
    ProviderReadiness {
        harness: descriptor.id,
        name: descriptor.name.clone(),
        state,
        freshness: EvidenceFreshness::Fresh,
        evidence_checked_at_ms: now,
        installed,
        configured: EvidenceState::Unknown,
        authenticated: EvidenceState::Unknown,
        enabled: descriptor_enabled(&descriptor),
        detail: detail.into(),
        model_count: 0,
        capabilities: capabilities(&descriptor, None, native_session_identity_observed),
        remediation: Some(remediation_value(remediation)),
    }
}

fn capability(
    kind: SetupCapabilityKind,
    state: SetupCapabilityState,
    detail: &str,
) -> SetupCapability {
    SetupCapability {
        kind,
        state,
        detail: detail.into(),
    }
}

fn capabilities(
    descriptor: &HarnessDescriptor,
    models: Option<&[Model]>,
    native_session_identity_observed: bool,
) -> Vec<SetupCapability> {
    let catalog = match models {
        Some(models) if !models.is_empty() => SetupCapabilityState::Supported,
        Some(_) => SetupCapabilityState::Unsupported,
        None => SetupCapabilityState::Unknown,
    };
    vec![
        capability(
            SetupCapabilityKind::ModelSelection,
            catalog,
            "Derived from the current model catalog probe.",
        ),
        capability(
            SetupCapabilityKind::Reasoning,
            if descriptor.reasoning_levels.is_empty() {
                SetupCapabilityState::Unsupported
            } else {
                SetupCapabilityState::Supported
            },
            "Declared by the provider descriptor.",
        ),
        capability(
            SetupCapabilityKind::Steering,
            if descriptor.supports_steering {
                SetupCapabilityState::Supported
            } else {
                SetupCapabilityState::Unsupported
            },
            "Declared by the provider descriptor.",
        ),
        capability(
            SetupCapabilityKind::Resume,
            SetupCapabilityState::Unknown,
            "No explicit declaration is exposed in L03.",
        ),
        capability(
            SetupCapabilityKind::Fork,
            SetupCapabilityState::Unknown,
            "Provider-native fork is unverified in L03.",
        ),
        capability(
            SetupCapabilityKind::ApprovalModes,
            if crate::permissions::provider_capabilities(descriptor.id)
                .supported_modes
                .is_empty()
            {
                SetupCapabilityState::Unsupported
            } else {
                SetupCapabilityState::Supported
            },
            &permission_capability_detail(descriptor.id),
        ),
        capability(
            SetupCapabilityKind::NativeSessionIdentity,
            if native_session_identity_observed {
                SetupCapabilityState::Supported
            } else {
                SetupCapabilityState::Unknown
            },
            if native_session_identity_observed {
                "Observed from a LUNA provider session."
            } else {
                "Observed only after a real session starts."
            },
        ),
    ]
}

fn permission_capability_detail(harness: HarnessId) -> String {
    let capabilities = crate::permissions::provider_capabilities(harness);
    if capabilities.supported_modes.is_empty() {
        return capabilities
            .known_limitations
            .first()
            .cloned()
            .unwrap_or_else(|| "No verified permission enforcement contract.".into());
    }
    let modes = [
        zeron_proto::PermissionMode::Auto,
        zeron_proto::PermissionMode::Full,
    ]
    .into_iter()
    .filter(|mode| capabilities.supports(*mode))
    .map(|mode| match mode {
        zeron_proto::PermissionMode::Auto => "Auto",
        zeron_proto::PermissionMode::Full => "Full",
        zeron_proto::PermissionMode::Ask => unreachable!(),
    })
    .collect::<Vec<_>>()
    .join(", ");
    if modes.is_empty() {
        "Verified selectable permission modes: none. Legacy Ask remains internal only.".into()
    } else {
        format!(
            "Verified selectable provider permission modes: {modes}. Legacy Ask remains internal only."
        )
    }
}

fn workspace_problem(
    state: WorkspaceReadinessState,
    path: Option<String>,
    detail: &str,
) -> WorkspaceReadiness {
    WorkspaceReadiness {
        canonical_path: path,
        state,
        checkout_kind: WorkspaceCheckoutKind::Unknown,
        detail: detail.into(),
        remediation: Some(remediation_value(SetupRemediationKind::SelectProject)),
    }
}

fn inspect_workspace(path: Option<&str>) -> WorkspaceReadiness {
    let Some(path) = path.filter(|path| !path.trim().is_empty()) else {
        return workspace_problem(
            WorkspaceReadinessState::NeedsAction,
            None,
            "Choose an existing local project in the composer.",
        );
    };
    let Ok(canonical) = std::fs::canonicalize(Path::new(path)) else {
        return workspace_problem(
            WorkspaceReadinessState::NeedsAction,
            None,
            "The selected project path is unavailable on this machine.",
        );
    };
    let canonical_path = Some(canonical.to_string_lossy().into_owned());
    let Ok(metadata) = std::fs::metadata(&canonical) else {
        return workspace_problem(
            WorkspaceReadinessState::Unknown,
            canonical_path,
            "The selected path could not be inspected.",
        );
    };
    if !metadata.is_dir() {
        return workspace_problem(
            WorkspaceReadinessState::NeedsAction,
            canonical_path,
            "The selected project path is not a directory.",
        );
    }
    let checkout_kind = if crate::workspace_host::linked_worktree_root(&canonical).is_some() {
        WorkspaceCheckoutKind::LinkedWorktree
    } else if canonical.join(".git").exists() {
        WorkspaceCheckoutKind::CurrentCheckout
    } else {
        WorkspaceCheckoutKind::Directory
    };
    let detail = match checkout_kind {
        WorkspaceCheckoutKind::CurrentCheckout => {
            "Existing checkout selected; new tasks stay here by default."
        }
        WorkspaceCheckoutKind::LinkedWorktree => {
            "Existing linked worktree selected explicitly; its path is reused."
        }
        WorkspaceCheckoutKind::Directory => "Existing local project directory selected.",
        WorkspaceCheckoutKind::Unknown => unreachable!(),
    };
    WorkspaceReadiness {
        canonical_path,
        state: WorkspaceReadinessState::Available,
        checkout_kind,
        detail: detail.into(),
        remediation: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(authentication: AgentAuthenticationState) -> zeron_proto::AgentAccount {
        zeron_proto::AgentAccount {
            id: "fixture-account".into(),
            harness: HarnessId::Codex,
            email: Some("fixture@example.com".into()),
            plan_label: None,
            active: true,
            authentication,
            usage_windows: Vec::new(),
            display_name: None,
            organization: None,
            auth_kind: Some(zeron_proto::AgentAuthKind::Oauth),
            switchable: false,
            saved_at: None,
        }
    }

    #[test]
    fn provider_authentication_state_stays_distinct_from_legacy_warnings() {
        let mut snapshot = AgentAccountsSnapshot {
            accounts: vec![account(AgentAuthenticationState::Authenticated)],
            warnings: vec![zeron_proto::AgentAccountWarning {
                harness: HarnessId::Codex,
                message: "legacy copies disabled".into(),
            }],
        };
        assert_eq!(
            authentication(HarnessId::Codex, Some(&snapshot)),
            EvidenceState::Confirmed
        );
        snapshot.accounts[0].authentication = AgentAuthenticationState::Expired;
        assert_eq!(
            authentication(HarnessId::Codex, Some(&snapshot)),
            EvidenceState::Missing
        );
        snapshot.accounts[0].authentication = AgentAuthenticationState::Unavailable;
        assert_eq!(
            authentication(HarnessId::Codex, Some(&snapshot)),
            EvidenceState::Unknown
        );
    }
    use zeron_proto::{ReasoningLevel, SteeringMode};

    #[test]
    fn provider_progress_requires_more_than_an_installed_cli() {
        let cases = [
            (
                EvidenceState::Missing,
                EvidenceState::Missing,
                ProviderReadinessState::Installed,
            ),
            (
                EvidenceState::Confirmed,
                EvidenceState::Missing,
                ProviderReadinessState::Configured,
            ),
            (
                EvidenceState::Missing,
                EvidenceState::Confirmed,
                ProviderReadinessState::Authenticated,
            ),
            (
                EvidenceState::Confirmed,
                EvidenceState::Confirmed,
                ProviderReadinessState::Available,
            ),
            (
                EvidenceState::Confirmed,
                EvidenceState::Unknown,
                ProviderReadinessState::Unknown,
            ),
        ];
        for (configured, authenticated, expected) in cases {
            assert_eq!(
                classify_provider(HarnessId::Codex, configured, authenticated),
                expected
            );
        }
        assert_eq!(
            classify_provider(
                HarnessId::Opencode,
                EvidenceState::Confirmed,
                EvidenceState::Unsupported,
            ),
            ProviderReadinessState::Configured
        );
        assert_eq!(
            classify_provider(
                HarnessId::Opencode,
                EvidenceState::Confirmed,
                EvidenceState::Unknown,
            ),
            ProviderReadinessState::Unknown
        );
    }

    #[test]
    fn approval_modes_report_only_verified_provider_contracts() {
        assert_eq!(
            permission_capability_detail(HarnessId::Codex),
            "Verified selectable provider permission modes: Auto, Full. Legacy Ask remains internal only."
        );
        assert_eq!(
            permission_capability_detail(HarnessId::Opencode),
            "Verified selectable provider permission modes: Full. Legacy Ask remains internal only."
        );
        assert!(
            permission_capability_detail(HarnessId::Cursor).contains("pinned @cursor/sdk 1.0.28")
        );
        assert!(
            permission_capability_detail(HarnessId::Grok)
                .contains("not proof that a concrete agent enforces")
        );
    }

    #[test]
    fn workspace_paths_round_trip_as_current_checkout_or_explicit_worktree() {
        let checkout = tempfile::tempdir().unwrap();
        std::fs::create_dir(checkout.path().join(".git")).unwrap();
        let current = inspect_workspace(checkout.path().to_str());
        assert_eq!(
            current.checkout_kind,
            WorkspaceCheckoutKind::CurrentCheckout
        );
        assert_eq!(
            current.canonical_path.as_deref(),
            std::fs::canonicalize(checkout.path()).unwrap().to_str()
        );

        let worktree = tempfile::tempdir().unwrap();
        std::fs::write(
            worktree.path().join(".git"),
            "gitdir: /tmp/repo/.git/worktrees/x\n",
        )
        .unwrap();
        assert_eq!(
            inspect_workspace(worktree.path().to_str()).checkout_kind,
            WorkspaceCheckoutKind::LinkedWorktree
        );
    }

    #[test]
    fn missing_workspace_is_actionable() {
        let workspace = inspect_workspace(None);
        assert_eq!(workspace.state, WorkspaceReadinessState::NeedsAction);
        assert!(workspace.canonical_path.is_none());
        assert_eq!(
            workspace.remediation.unwrap().kind,
            SetupRemediationKind::SelectProject
        );
    }

    #[test]
    fn failed_refresh_keeps_last_available_evidence_stale_not_green() {
        let health = SetupHealth::default();
        let descriptor = HarnessDescriptor {
            id: HarnessId::Codex,
            name: "Codex".into(),
            supports_steering: true,
            steering_mode: SteeringMode::StepBoundary,
            reasoning_levels: vec![ReasoningLevel::High],
            installed: true,
            enabled: Some(true),
        };
        let cached = ProviderReadiness {
            harness: HarnessId::Codex,
            name: "Codex".into(),
            state: ProviderReadinessState::Available,
            freshness: EvidenceFreshness::Fresh,
            evidence_checked_at_ms: 10,
            installed: EvidenceState::Confirmed,
            configured: EvidenceState::Confirmed,
            authenticated: EvidenceState::Confirmed,
            enabled: true,
            detail: "ready".into(),
            model_count: 1,
            capabilities: Vec::new(),
            remediation: None,
        };
        health.cache().insert(HarnessId::Codex, cached);
        let stale = health.stale_or_unknown(&descriptor, "probe failed", 20, false);
        assert_eq!(stale.freshness, EvidenceFreshness::Stale);
        assert_eq!(stale.evidence_checked_at_ms, 10);
        assert!(!stale.is_fresh_available());
    }
}
