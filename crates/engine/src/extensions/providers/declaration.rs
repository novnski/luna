//! `luna-provider.json`, version 1: the static, reviewed facts of a package's
//! provider contributions. It is a listed (hash-pinned) package file and is
//! read without running any code. The worker's runtime `describe` reports
//! availability and its executable; it can never widen these facts.
//!
//! CORE-11 adds provider profile 2.0, the hosted harness profile: a
//! `hosted` section declares the adapter's own capabilities, its job
//! operations and its permission facts (`ProviderPermissionCapabilities`,
//! taken verbatim), and legacy Ask may be declared for chats that already
//! use it. Profile 1.0 declarations are read exactly as before.

use std::collections::{BTreeMap, BTreeSet};

use luna_proto::{
    ApprovalActionKind, PermissionMode, ProviderForkCapability, ProviderPermissionCapabilities,
    ReasoningLevel, RuntimeDelivery, SteeringMode,
};
use serde::{Deserialize, Serialize};

pub(super) const DECLARATION_FILE: &str = "luna-provider.json";
pub(super) const MAX_DECLARATION_BYTES: u64 = 64 * 1024;
const DECLARATION_VERSION: u32 = 1;
/// Provider profile this host implements.
pub(super) const PROFILE_MAJOR: u32 = 1;
pub(super) const PROFILE_MINOR: u32 = 0;
/// Provider profile 2.0, the hosted harness profile (CORE-11).
pub(super) const HOSTED_PROFILE_MAJOR: u32 = 2;

const MAX_PROVIDERS: usize = 16;
const MAX_NAME_CHARS: usize = 64;
const MAX_TEXT_CHARS: usize = 512;
const MAX_LIST: usize = 32;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DeclarationFile {
    declaration_version: u32,
    providers: Vec<RawProvider>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RawProvider {
    id: String,
    display_name: String,
    profile: ProfileVersion,
    #[serde(default)]
    optional: Vec<String>,
    /// Mode (`full`, `auto`) → how the provider maps it natively.
    permission_modes: BTreeMap<String, String>,
    #[serde(default)]
    automatic_reviewer: bool,
    #[serde(default)]
    reasoning_levels: Vec<ReasoningLevel>,
    #[serde(default)]
    unsupported: Vec<RawOmission>,
    #[serde(default)]
    limitations: Vec<String>,
    /// Provider profile 2.0 only.
    #[serde(default)]
    hosted: Option<RawHosted>,
}

/// The `hosted` section of a provider profile 2.0 declaration.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RawHosted {
    /// Absent: no mid-turn input.
    #[serde(default)]
    steering_mode: Option<SteeringMode>,
    #[serde(default)]
    deterministic_turn_end: bool,
    #[serde(default)]
    safe_recovery: bool,
    #[serde(default = "no_fork")]
    fork: ProviderForkCapability,
    #[serde(default)]
    native_child_stop: bool,
    #[serde(default = "no_delivery")]
    runtime_delivery: RuntimeDelivery,
    /// Job operations beyond `models` and `run`.
    #[serde(default)]
    operations: Vec<String>,
    /// The resolver's facts, verbatim.
    permissions: ProviderPermissionCapabilities,
}

fn no_fork() -> ProviderForkCapability {
    ProviderForkCapability::None
}

fn no_delivery() -> RuntimeDelivery {
    RuntimeDelivery::Unsupported
}

/// The optional job operations of provider profile 2.0 (`models` and `run`
/// are always served).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum HostedOperation {
    Title,
    Commands,
    Extensions,
    Mcp,
    Fork,
    History,
    Rewind,
    Compact,
}

impl HostedOperation {
    const ALL: [Self; 8] = [
        Self::Title,
        Self::Commands,
        Self::Extensions,
        Self::Mcp,
        Self::Fork,
        Self::History,
        Self::Rewind,
        Self::Compact,
    ];

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Title => "title",
            Self::Commands => "commands",
            Self::Extensions => "extensions",
            Self::Mcp => "mcp",
            Self::Fork => "fork",
            Self::History => "history",
            Self::Rewind => "rewind",
            Self::Compact => "compact",
        }
    }

    fn parse(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|operation| operation.name() == name)
    }

    fn missing_reason(self) -> &'static str {
        match self {
            Self::Title => "the provider has no restricted title generation",
            Self::Commands => "the provider lists no slash commands",
            Self::Extensions => "the provider reports no native extension inventory",
            Self::Mcp => "the provider reports no MCP status",
            Self::Fork => "the provider has no native fork",
            Self::History => "the provider has no native history readback",
            Self::Rewind => "the provider has no native rewind",
            Self::Compact => "the provider has no native compaction",
        }
    }
}

/// A validated provider profile 2.0 section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HostedDeclaration {
    pub steering_mode: Option<SteeringMode>,
    pub deterministic_turn_end: bool,
    pub safe_recovery: bool,
    pub fork: ProviderForkCapability,
    pub native_child_stop: bool,
    pub runtime_delivery: RuntimeDelivery,
    pub operations: BTreeSet<HostedOperation>,
    pub permissions: ProviderPermissionCapabilities,
}

impl HostedDeclaration {
    pub fn offers(&self, operation: HostedOperation) -> bool {
        self.operations.contains(&operation)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RawOmission {
    capability: String,
    reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ProfileVersion {
    pub major: u32,
    pub minor: u32,
}

/// Optional parts of provider profile 1.0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum OptionalProfile {
    /// `resume` in `run.start`, echoed by the run's `session` event.
    Resume,
    /// `inputRequest` events and `provider.run.answer` with answers.
    Questions,
    /// `approvalRequest` events and `provider.run.answer` with a decision.
    Approvals,
    /// Staged image paths in `run.start.attachments`.
    Attachments,
}

impl OptionalProfile {
    const ALL: [Self; 4] = [
        Self::Resume,
        Self::Questions,
        Self::Approvals,
        Self::Attachments,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::Resume => "resume",
            Self::Questions => "questions",
            Self::Approvals => "approvals",
            Self::Attachments => "attachments",
        }
    }

    fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|profile| profile.name() == name)
    }

    fn missing_reason(self) -> &'static str {
        match self {
            Self::Resume => {
                "the provider cannot continue a saved conversation; a chat with a stored provider session is refused before the worker starts"
            }
            Self::Questions => {
                "the provider cannot ask questions during a run; such an event fails the run"
            }
            Self::Approvals => {
                "the provider sends no approval requests to LUNA; such an event fails the run"
            }
            Self::Attachments => {
                "the provider takes text prompts only; a message with attachments is refused before the worker starts"
            }
        }
    }
}

/// Capabilities that provider profile 1.0 does not carry at all.
const NOT_IN_PROFILE: &[(&str, &str)] = &[
    (
        "steer",
        "provider profile 1.0 has no mid-turn input; messages sent during a turn wait for its end",
    ),
    ("fork", "provider profile 1.0 has no provider-native fork"),
    (
        "rewind",
        "provider profile 1.0 has no provider-native rewind",
    ),
    (
        "compaction",
        "provider profile 1.0 has no provider-native compaction",
    ),
    (
        "childControl",
        "provider profile 1.0 has no native child sessions to stop",
    ),
    ("nativeGoals", "provider profile 1.0 has no native goals"),
    (
        "titles",
        "provider profile 1.0 has no restricted title generation",
    ),
    (
        "accounts",
        "provider profile 1.0 has no provider-owned accounts; the CLI uses its own sign-in",
    ),
    (
        "lunaTools",
        "provider profile 1.0 does not deliver LUNA's runtime tools to the provider",
    ),
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Omission {
    pub capability: String,
    pub reason: String,
}

/// One validated provider declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ProviderDeclaration {
    pub display_name: String,
    pub profile: ProfileVersion,
    pub optional: BTreeSet<OptionalProfile>,
    /// Usable modes → the provider's own description of how it maps them.
    pub modes: BTreeMap<PermissionMode, String>,
    pub automatic_reviewer: bool,
    pub reasoning_levels: Vec<ReasoningLevel>,
    /// What is not offered, with why: the provider's own list, then what this
    /// host dropped or profile 1.0 lacks.
    pub omitted: Vec<Omission>,
    pub limitations: Vec<String>,
    /// Provider profile 2.0's hosted section.
    pub hosted: Option<HostedDeclaration>,
}

impl ProviderDeclaration {
    pub fn supports(&self, profile: OptionalProfile) -> bool {
        self.optional.contains(&profile)
    }

    pub fn supports_mode(&self, mode: PermissionMode) -> bool {
        self.modes.contains_key(&mode)
    }

    /// Whether `provider.run.answer` must be advertised by the worker.
    pub fn needs_answer_operation(&self) -> bool {
        self.supports(OptionalProfile::Questions) || self.supports(OptionalProfile::Approvals)
    }

    /// The resolver's view: only declared, runnable modes; everything that is
    /// omitted is stated as a known limitation.
    pub fn permissions(&self) -> ProviderPermissionCapabilities {
        use ApprovalActionKind::{Command, ExternalPath, FileChange, Network, Other, Tool};
        let trust = format!(
            "{} runs as a native extension worker with your account's access; LUNA does not sandbox it.",
            self.display_name
        );
        if let Some(hosted) = &self.hosted {
            // Profile 2.0 declares the adapter's own facts; only the trust
            // statement is added.
            let mut permissions = hosted.permissions.clone();
            permissions.known_limitations.insert(0, trust);
            return permissions;
        }
        let mut known_limitations = vec![trust];
        for (mode, mapping) in &self.modes {
            let label = match mode {
                PermissionMode::Full => "Full access",
                PermissionMode::Auto => "Approve for me",
                PermissionMode::Ask => "Ask",
            };
            known_limitations.push(format!("{label}: {mapping}"));
        }
        known_limitations.extend(self.limitations.iter().cloned());
        known_limitations.extend(
            self.omitted.iter().map(|omission| {
                format!("Not offered: {} ({})", omission.capability, omission.reason)
            }),
        );
        ProviderPermissionCapabilities {
            supported_modes: self.modes.keys().copied().collect(),
            supports_workspace_sandbox: false,
            supports_automatic_reviewer: self.supports_mode(PermissionMode::Auto),
            supports_live_mode_change: false,
            supports_task_allowance: false,
            approval_kinds: if self.supports(OptionalProfile::Approvals) {
                vec![Command, FileChange, Network, ExternalPath, Tool, Other]
            } else {
                Vec::new()
            },
            network_control: "provider-owned; declared by the extension and not enforced by LUNA"
                .into(),
            known_limitations,
        }
    }
}

fn bounded(label: &str, value: &str, max: usize) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > max || value.chars().any(char::is_control) {
        return Err(format!("{label} must be 1 to {max} printable characters"));
    }
    Ok(value.to_owned())
}

/// Validate the declaration file against the manifest's provider
/// contributions. The outer error refuses the whole file; each contribution
/// then has its own result.
pub(super) fn parse(
    bytes: &[u8],
    contributions: &[String],
) -> Result<BTreeMap<String, Result<ProviderDeclaration, String>>, String> {
    let file: DeclarationFile = serde_json::from_slice(bytes)
        .map_err(|error| format!("{DECLARATION_FILE} is invalid: {error}"))?;
    if file.declaration_version != DECLARATION_VERSION {
        return Err(format!(
            "{DECLARATION_FILE} version {} is not supported; this LUNA reads version {DECLARATION_VERSION}",
            file.declaration_version
        ));
    }
    if file.providers.len() > MAX_PROVIDERS {
        return Err(format!(
            "{DECLARATION_FILE} declares more than {MAX_PROVIDERS} providers"
        ));
    }
    let mut seen = BTreeSet::new();
    for provider in &file.providers {
        if !contributions.contains(&provider.id) {
            return Err(format!(
                "{DECLARATION_FILE} declares {}, which the manifest does not contribute as a provider",
                provider.id
            ));
        }
        if !seen.insert(provider.id.clone()) {
            return Err(format!(
                "{DECLARATION_FILE} declares {} more than once",
                provider.id
            ));
        }
    }
    let mut results = BTreeMap::new();
    for id in contributions {
        let result = match file.providers.iter().find(|provider| provider.id == *id) {
            Some(raw) => validate(raw),
            None => Err(format!("{DECLARATION_FILE} has no declaration for {id}")),
        };
        results.insert(id.clone(), result);
    }
    Ok(results)
}

fn validate(raw: &RawProvider) -> Result<ProviderDeclaration, String> {
    let display_name = bounded("displayName", &raw.display_name, MAX_NAME_CHARS)?;
    if raw.profile.major != PROFILE_MAJOR && raw.profile.major != HOSTED_PROFILE_MAJOR {
        return Err(format!(
            "declares provider profile {}.{}; this LUNA implements provider profiles {PROFILE_MAJOR}.{PROFILE_MINOR} and {HOSTED_PROFILE_MAJOR}.0",
            raw.profile.major, raw.profile.minor
        ));
    }
    if raw.profile.major == PROFILE_MAJOR && raw.hosted.is_some() {
        return Err("declares a hosted section, which belongs to provider profile 2.0".into());
    }
    if raw.unsupported.len() > MAX_LIST || raw.limitations.len() > MAX_LIST {
        return Err(format!(
            "unsupported and limitations list at most {MAX_LIST} entries each"
        ));
    }
    let mut omitted = Vec::new();
    for entry in &raw.unsupported {
        omitted.push(Omission {
            capability: bounded("unsupported.capability", &entry.capability, MAX_NAME_CHARS)?,
            reason: bounded("unsupported.reason", &entry.reason, MAX_TEXT_CHARS)?,
        });
    }
    let limitations = raw
        .limitations
        .iter()
        .map(|limitation| bounded("limitations", limitation, MAX_TEXT_CHARS))
        .collect::<Result<Vec<_>, _>>()?;
    if raw.profile.major == HOSTED_PROFILE_MAJOR {
        return validate_hosted(raw, display_name, omitted, limitations);
    }
    let note = |omitted: &mut Vec<Omission>, capability: &str, reason: String| {
        if !omitted.iter().any(|entry| entry.capability == capability) {
            omitted.push(Omission {
                capability: capability.to_owned(),
                reason,
            });
        }
    };

    let mut optional = BTreeSet::new();
    for name in &raw.optional {
        match OptionalProfile::parse(name) {
            Some(profile) => {
                optional.insert(profile);
            }
            None => note(
                &mut omitted,
                &bounded("optional", name, MAX_NAME_CHARS)?,
                "an optional profile this LUNA does not know; it is not used".into(),
            ),
        }
    }

    let mut modes = BTreeMap::new();
    for (mode, mapping) in &raw.permission_modes {
        let mapping = bounded("permissionModes", mapping, MAX_TEXT_CHARS)?;
        match mode.as_str() {
            "full" => {
                modes.insert(PermissionMode::Full, mapping);
            }
            "auto" if raw.automatic_reviewer && optional.contains(&OptionalProfile::Approvals) => {
                modes.insert(PermissionMode::Auto, mapping);
            }
            "auto" => note(
                &mut omitted,
                "auto",
                "Approve for me needs the provider's own automatic reviewer and the approvals profile; LUNA never turns Auto into a human question".into(),
            ),
            "ask" => note(
                &mut omitted,
                "ask",
                "legacy Ask is not offered for extension providers".into(),
            ),
            other => note(
                &mut omitted,
                &bounded("permissionModes", other, MAX_NAME_CHARS)?,
                "a permission mode this LUNA does not know".into(),
            ),
        }
    }
    if modes.is_empty() {
        return Err(
            "declares no permission mode LUNA can run: Full access, or Approve for me with an automatic reviewer"
                .into(),
        );
    }

    let mut reasoning_levels = Vec::new();
    for level in &raw.reasoning_levels {
        if !reasoning_levels.contains(level) {
            reasoning_levels.push(*level);
        }
    }

    for profile in OptionalProfile::ALL {
        if !optional.contains(&profile) {
            note(
                &mut omitted,
                profile.name(),
                profile.missing_reason().into(),
            );
        }
    }
    for (capability, reason) in NOT_IN_PROFILE {
        note(&mut omitted, capability, (*reason).into());
    }

    Ok(ProviderDeclaration {
        display_name,
        profile: raw.profile,
        optional,
        modes,
        automatic_reviewer: raw.automatic_reviewer,
        reasoning_levels,
        omitted,
        limitations,
        hosted: None,
    })
}

/// Provider profile 2.0. The adapter in the worker settles its own
/// approvals (Full answers stray asks once; legacy Ask shows its cards);
/// the bridge refuses any approval card in Full or Auto.
fn validate_hosted(
    raw: &RawProvider,
    display_name: String,
    mut omitted: Vec<Omission>,
    limitations: Vec<String>,
) -> Result<ProviderDeclaration, String> {
    let Some(hosted) = &raw.hosted else {
        return Err(format!(
            "declares provider profile {}.{} without a hosted section; provider profile 2.0 is the hosted harness profile",
            raw.profile.major, raw.profile.minor
        ));
    };
    if !raw.optional.is_empty() {
        return Err(
            "lists optional profiles, which belong to provider profile 1.0; profile 2.0 lists hosted.operations"
                .into(),
        );
    }
    let note = |omitted: &mut Vec<Omission>, capability: &str, reason: String| {
        if !omitted.iter().any(|entry| entry.capability == capability) {
            omitted.push(Omission {
                capability: capability.to_owned(),
                reason,
            });
        }
    };
    let mut modes = BTreeMap::new();
    for (mode, mapping) in &raw.permission_modes {
        let mapping = bounded("permissionModes", mapping, MAX_TEXT_CHARS)?;
        match mode.as_str() {
            "full" => {
                modes.insert(PermissionMode::Full, mapping);
            }
            // Legacy Ask, for chats that already use it; never a user mode
            // (docs/pending-tasks/permission-modes).
            "ask" => {
                modes.insert(PermissionMode::Ask, mapping);
            }
            "auto" if hosted.permissions.supports_automatic_reviewer => {
                modes.insert(PermissionMode::Auto, mapping);
            }
            "auto" => note(
                &mut omitted,
                "auto",
                "Approve for me needs the provider's own automatic reviewer; LUNA never turns Auto into a human question".into(),
            ),
            other => note(
                &mut omitted,
                &bounded("permissionModes", other, MAX_NAME_CHARS)?,
                "a permission mode this LUNA does not know".into(),
            ),
        }
    }
    if !modes.contains_key(&PermissionMode::Full) && !modes.contains_key(&PermissionMode::Auto) {
        return Err(
            "declares no permission mode LUNA can run: Full access, or Approve for me with an automatic reviewer"
                .into(),
        );
    }
    let declared: BTreeSet<PermissionMode> = modes.keys().copied().collect();
    let advertised: BTreeSet<PermissionMode> =
        hosted.permissions.supported_modes.iter().copied().collect();
    if declared != advertised {
        return Err(
            "hosted.permissions.supportedModes must list exactly the declared permission modes"
                .into(),
        );
    }
    let mut operations = BTreeSet::new();
    for name in &hosted.operations {
        match HostedOperation::parse(name) {
            Some(operation) => {
                operations.insert(operation);
            }
            None => note(
                &mut omitted,
                &bounded("hosted.operations", name, MAX_NAME_CHARS)?,
                "an operation this LUNA does not know; it is not used".into(),
            ),
        }
    }
    if hosted.fork != ProviderForkCapability::None && !operations.contains(&HostedOperation::Fork) {
        return Err("declares a fork capability without the fork operation".into());
    }
    for operation in HostedOperation::ALL {
        if !operations.contains(&operation) {
            note(
                &mut omitted,
                operation.name(),
                operation.missing_reason().into(),
            );
        }
    }
    if hosted.steering_mode.is_none() {
        note(
            &mut omitted,
            "steer",
            "the provider takes no input during a turn".into(),
        );
    }
    let mut reasoning_levels = Vec::new();
    for level in &raw.reasoning_levels {
        if !reasoning_levels.contains(level) {
            reasoning_levels.push(*level);
        }
    }
    Ok(ProviderDeclaration {
        display_name,
        profile: raw.profile,
        optional: BTreeSet::new(),
        modes,
        automatic_reviewer: hosted.permissions.supports_automatic_reviewer,
        reasoning_levels,
        omitted,
        limitations,
        hosted: Some(HostedDeclaration {
            steering_mode: hosted.steering_mode,
            deterministic_turn_end: hosted.deterministic_turn_end,
            safe_recovery: hosted.safe_recovery,
            fork: hosted.fork,
            native_child_stop: hosted.native_child_stop,
            runtime_delivery: hosted.runtime_delivery,
            operations,
            permissions: hosted.permissions.clone(),
        }),
    })
}
