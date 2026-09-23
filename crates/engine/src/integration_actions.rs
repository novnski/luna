//! Owner-managed integration actions. No production owner driver is registered.
//!
//! A source is re-read for every help or action request. The source supplies
//! identity and help, while an explicitly registered owner driver must provide
//! a revisioned, idempotent mutation and an authoritative readback. This module
//! never writes provider configuration or starts authentication itself.

use std::path::PathBuf;
use std::sync::Arc;

use zeron_proto::{
    HarnessId, IntegrationActionAvailability, IntegrationHelpRoute, IntegrationInventoryItem,
    IntegrationInventoryView, IntegrationOwner,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum IntegrationOperation {
    Enable,
    Disable,
    Authenticate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActionRequest {
    pub identity: String,
    pub operation: IntegrationOperation,
    pub expected_owner_revision: u64,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OwnerSnapshot {
    pub item: IntegrationInventoryItem,
    /// None means this source has no authoritative action revision.
    pub owner_revision: Option<u64>,
    pub enabled: Option<bool>,
    pub authenticated: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OwnerReceipt {
    pub identity: String,
    pub owner: IntegrationOwner,
    pub operation: IntegrationOperation,
    pub expected_owner_revision: u64,
    pub resulting_owner_revision: u64,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VerifiedAction {
    pub receipt: OwnerReceipt,
    pub observed: OwnerSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ActionError {
    InvalidIdentity,
    InvalidIdempotencyKey,
    NotFound,
    SourceUnavailable,
    Unsupported {
        help: IntegrationHelpRoute,
    },
    RevisionConflict,
    IdempotencyConflict,
    DuplicateOwnerDriver,
    DriverFailure,
    /// The action may have occurred; the caller must inspect owner state.
    Indeterminate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DriverError {
    RevisionConflict,
    IdempotencyConflict,
    /// A failure known to have happened before mutation.
    PreMutationFailure,
    /// The owner may have committed, but its response was lost.
    Indeterminate,
}

pub(crate) trait IntegrationSource: Send + Sync {
    fn snapshot(&self, identity: &str) -> Result<Option<OwnerSnapshot>, ()>;
}

/// An implementation must atomically bind each key to its complete intent,
/// revision check, mutation and durable receipt. `apply_if_revision` must not
/// retry an uncertain side effect internally.
pub(crate) trait IntegrationOwnerDriver: IntegrationSource {
    fn owner(&self) -> IntegrationOwner;
    fn receipt(&self, idempotency_key: &str) -> Result<Option<OwnerReceipt>, ()>;
    fn apply_if_revision(&self, request: &ActionRequest) -> Result<OwnerReceipt, DriverError>;
}

pub(crate) struct IntegrationActionService {
    source: Arc<dyn IntegrationSource>,
    drivers: Vec<Arc<dyn IntegrationOwnerDriver>>,
}

fn advertises(item: &IntegrationInventoryItem, operation: IntegrationOperation) -> bool {
    let availability = match operation {
        IntegrationOperation::Enable => item.actions.enable,
        IntegrationOperation::Disable => item.actions.disable,
        IntegrationOperation::Authenticate => item.actions.authenticate,
    };
    availability == IntegrationActionAvailability::Supported
}

impl IntegrationActionService {
    /// Production uses this constructor. It can return typed help, but cannot
    /// enable, disable or authenticate anything without a verified owner driver.
    pub(crate) fn read_only(source: Arc<dyn IntegrationSource>) -> Self {
        Self {
            source,
            drivers: Vec::new(),
        }
    }

    /// Driver registration is explicit; the production engine does not call it.
    pub(crate) fn with_driver(
        mut self,
        driver: Arc<dyn IntegrationOwnerDriver>,
    ) -> Result<Self, ActionError> {
        if driver.owner() == IntegrationOwner::Unknown
            || self
                .drivers
                .iter()
                .any(|existing| existing.owner() == driver.owner())
        {
            return Err(ActionError::DuplicateOwnerDriver);
        }
        self.drivers.push(driver);
        Ok(self)
    }

    pub(crate) fn help(&self, identity: &str) -> Result<IntegrationHelpRoute, ActionError> {
        Ok(self.resolve(identity)?.item.help_route)
    }

    pub(crate) fn act(&self, request: &ActionRequest) -> Result<VerifiedAction, ActionError> {
        if request.idempotency_key.is_empty()
            || request.idempotency_key.len() > 128
            || !request
                .idempotency_key
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        {
            return Err(ActionError::InvalidIdempotencyKey);
        }

        let catalog_item = self.resolve(&request.identity)?;
        let help = catalog_item.item.help_route.clone();
        let owner = catalog_item.item.owner;
        if owner == IntegrationOwner::Unknown || !advertises(&catalog_item.item, request.operation)
        {
            return Err(ActionError::Unsupported { help });
        }
        let Some(driver) = self.drivers.iter().find(|driver| driver.owner() == owner) else {
            return Err(ActionError::Unsupported { help });
        };
        let before = driver
            .snapshot(&request.identity)
            .map_err(|_| ActionError::SourceUnavailable)?
            .ok_or_else(|| ActionError::Unsupported { help: help.clone() })?;
        if before.item.identity.as_deref() != Some(request.identity.as_str())
            || before.item.owner != owner
        {
            return Err(ActionError::Unsupported { help });
        }
        if !advertises(&before.item, request.operation) || before.owner_revision.is_none() {
            return Err(ActionError::Unsupported { help });
        }

        // The owner journal is checked before revision comparison, so a
        // duplicate request can return its prior receipt after a restart.
        if let Some(receipt) = driver
            .receipt(&request.idempotency_key)
            .map_err(|_| ActionError::Indeterminate)?
        {
            self.check_intent(request, owner, &receipt)?;
            return self.verify_readback(driver.as_ref(), request, receipt);
        }
        if before.owner_revision != Some(request.expected_owner_revision) {
            return Err(ActionError::RevisionConflict);
        }

        let receipt = match driver.apply_if_revision(request) {
            Ok(receipt) => receipt,
            Err(DriverError::RevisionConflict) => return Err(ActionError::RevisionConflict),
            Err(DriverError::IdempotencyConflict) => return Err(ActionError::IdempotencyConflict),
            Err(DriverError::PreMutationFailure) => return Err(ActionError::DriverFailure),
            Err(DriverError::Indeterminate) => driver
                .receipt(&request.idempotency_key)
                .ok()
                .flatten()
                .ok_or(ActionError::Indeterminate)?,
        };
        self.check_intent(request, owner, &receipt)
            .map_err(|_| ActionError::Indeterminate)?;
        self.verify_readback(driver.as_ref(), request, receipt)
    }

    fn resolve(&self, identity: &str) -> Result<OwnerSnapshot, ActionError> {
        if identity.len() != 72
            || !identity.starts_with("intg_v1_")
            || !identity[8..].bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(ActionError::InvalidIdentity);
        }
        let snapshot = self
            .source
            .snapshot(identity)
            .map_err(|_| ActionError::SourceUnavailable)?
            .ok_or(ActionError::NotFound)?;
        if snapshot.item.identity.as_deref() != Some(identity) {
            return Err(ActionError::NotFound);
        }
        Ok(snapshot)
    }

    fn check_intent(
        &self,
        request: &ActionRequest,
        owner: IntegrationOwner,
        receipt: &OwnerReceipt,
    ) -> Result<(), ActionError> {
        if receipt.identity != request.identity
            || receipt.owner != owner
            || receipt.operation != request.operation
            || receipt.expected_owner_revision != request.expected_owner_revision
            || receipt.idempotency_key != request.idempotency_key
        {
            return Err(ActionError::IdempotencyConflict);
        }
        if receipt.resulting_owner_revision < receipt.expected_owner_revision {
            return Err(ActionError::Indeterminate);
        }
        Ok(())
    }

    fn verify_readback(
        &self,
        driver: &dyn IntegrationOwnerDriver,
        request: &ActionRequest,
        receipt: OwnerReceipt,
    ) -> Result<VerifiedAction, ActionError> {
        let observed = driver
            .snapshot(&request.identity)
            .ok()
            .flatten()
            .ok_or(ActionError::Indeterminate)?;
        let effect_matches = match request.operation {
            IntegrationOperation::Enable => observed.enabled == Some(true),
            IntegrationOperation::Disable => observed.enabled == Some(false),
            IntegrationOperation::Authenticate => observed.authenticated == Some(true),
        };
        if observed.item.identity.as_deref() != Some(request.identity.as_str())
            || observed.item.owner != receipt.owner
            || observed.owner_revision != Some(receipt.resulting_owner_revision)
            || !effect_matches
        {
            return Err(ActionError::Indeterminate);
        }
        Ok(VerifiedAction { receipt, observed })
    }
}

/// Explicit paths only; no HOME, CODEX_HOME or credential discovery. The
/// existing Codex inventory provides help routes and unsupported action facts.
pub(crate) struct CodexReadOnlySource {
    pub codex_home: PathBuf,
    pub project_root: Option<PathBuf>,
    pub machine_id: String,
}

impl IntegrationSource for CodexReadOnlySource {
    fn snapshot(&self, identity: &str) -> Result<Option<OwnerSnapshot>, ()> {
        let inventory = crate::integration_settings::codex_inventory(
            &self.codex_home,
            self.project_root.as_deref(),
            &self.machine_id,
        );
        Ok(inventory
            .items
            .into_iter()
            .find(|item| item.identity.as_deref() == Some(identity))
            .map(|item| OwnerSnapshot {
                item,
                owner_revision: None,
                enabled: None,
                authenticated: None,
            }))
    }
}

const MAX_ATTACHMENT_READBACK_AGE_MS: i64 = 30_000;
const MAX_ATTACHMENT_MEMBERS: usize = 512;
const MAX_ATTACHMENT_BINDING_BYTES: usize = 256;

/// Injected authoritative CURRENT task-to-provider-session binding. The
/// integration observations do not establish this binding themselves.
#[derive(Clone)]
#[allow(dead_code)] // Fake-only contract until a selected-session owner exists.
struct CurrentSessionBinding {
    task_id: String,
    provider: HarnessId,
    machine_id: String,
    provider_session_id: String,
    generation: u64,
    started_at_ms: i64,
}

/// Complete item-identity membership from a verified source owner for exactly
/// one selected session. Raw Claude/Codex name or cwd observations cannot
/// construct this evidence; no production owner supplies it today.
#[derive(Clone)]
#[allow(dead_code)] // Fake-only contract; never constructed from provider homes.
struct OwnerMembershipReadback {
    task_id: String,
    provider: HarnessId,
    machine_id: String,
    provider_session_id: String,
    generation: u64,
    owner: IntegrationOwner,
    checked_at_ms: i64,
    complete: bool,
    item_identities: Vec<String>,
}

fn valid_attachment_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_ATTACHMENT_BINDING_BYTES
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn valid_attachment_identity(identity: &str) -> bool {
    identity.len() == 72
        && identity.starts_with("intg_v1_")
        && identity[8..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// A pure fail-closed gate. `Some(false)` requires the same complete owner
/// readback as `Some(true)`; absence from a partial list is always unknown.
/// This does not mutate inventory, availability, attachment, or action facts.
#[allow(dead_code)] // No production selected-session/owner readback is wired.
fn effective_attachment_from_owner(
    inventory: &IntegrationInventoryView,
    identity: &str,
    current: &CurrentSessionBinding,
    owner_readback: Option<&OwnerMembershipReadback>,
    now_ms: i64,
) -> Option<bool> {
    if !valid_attachment_identity(identity)
        || inventory.provider != current.provider
        || inventory.machine_id != current.machine_id
        || !valid_attachment_component(&current.task_id)
        || !valid_attachment_component(&current.machine_id)
        || !valid_attachment_component(&current.provider_session_id)
        || current.generation == 0
        || current.started_at_ms <= 0
    {
        return None;
    }
    let mut matches = inventory
        .items
        .iter()
        .filter(|item| item.identity.as_deref() == Some(identity));
    let item = matches.next()?;
    if matches.next().is_some() || !item.configured || item.owner == IntegrationOwner::Unknown {
        return None;
    }

    let readback = owner_readback?;
    if !readback.complete
        || readback.owner != item.owner
        || readback.provider != current.provider
        || readback.machine_id != current.machine_id
        || readback.task_id != current.task_id
        || readback.provider_session_id != current.provider_session_id
        || readback.generation != current.generation
        || readback.checked_at_ms < current.started_at_ms
        || !now_ms
            .checked_sub(readback.checked_at_ms)
            .is_some_and(|age| (0..=MAX_ATTACHMENT_READBACK_AGE_MS).contains(&age))
        || readback.item_identities.len() > MAX_ATTACHMENT_MEMBERS
    {
        return None;
    }

    let mut members = std::collections::HashSet::new();
    for member in &readback.item_identities {
        if !valid_attachment_identity(member) || !members.insert(member.as_str()) {
            return None;
        }
        if inventory.items.iter().any(|item| {
            item.identity.as_deref() == Some(member.as_str()) && item.owner != readback.owner
        }) {
            return None;
        }
    }
    let attached = members.contains(identity);
    // Current configuration may be disabled after this session loaded the
    // integration. Only a conflicting selected-session observation can veto
    // the complete, binding-matched owner readback.
    if item.attached.is_some_and(|known| known != attached) {
        return None;
    }
    Some(attached)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::path::Path;
    use std::sync::Mutex;
    use zeron_proto::{IntegrationActionFacts, IntegrationKind, IntegrationSkillScope};

    const ID: &str = "intg_v1_0000000000000000000000000000000000000000000000000000000000000001";
    const OTHER_ID: &str =
        "intg_v1_0000000000000000000000000000000000000000000000000000000000000002";

    fn attachment_inventory() -> IntegrationInventoryView {
        let item = IntegrationInventoryItem {
            identity: Some(ID.into()),
            name: "duplicate-name".into(),
            kind: IntegrationKind::Skill,
            source: "synthetic global".into(),
            scope: "global".into(),
            configured: true,
            available: None,
            attached: None,
            reason: None,
            owner: IntegrationOwner::ProviderNative,
            actions: IntegrationActionFacts {
                enable: IntegrationActionAvailability::Unavailable,
                disable: IntegrationActionAvailability::Unavailable,
                authenticate: IntegrationActionAvailability::Unavailable,
                reason: Some("No action driver".into()),
            },
            help_route: IntegrationHelpRoute::SkillSource {
                scope: IntegrationSkillScope::Global,
            },
        };
        let mut same_name_other_source = item.clone();
        same_name_other_source.identity = Some(OTHER_ID.into());
        same_name_other_source.source = "synthetic project".into();
        same_name_other_source.scope = "project".into();
        IntegrationInventoryView {
            provider: HarnessId::Codex,
            machine_id: "synthetic-machine".into(),
            items: vec![item, same_name_other_source],
            unavailable_reason: None,
            help: "Synthetic help".into(),
        }
    }

    fn current_session() -> CurrentSessionBinding {
        CurrentSessionBinding {
            task_id: "synthetic-task".into(),
            provider: HarnessId::Codex,
            machine_id: "synthetic-machine".into(),
            provider_session_id: "synthetic-session".into(),
            generation: 7,
            started_at_ms: 1_000,
        }
    }

    fn owner_membership(identities: &[&str]) -> OwnerMembershipReadback {
        OwnerMembershipReadback {
            task_id: "synthetic-task".into(),
            provider: HarnessId::Codex,
            machine_id: "synthetic-machine".into(),
            provider_session_id: "synthetic-session".into(),
            generation: 7,
            owner: IntegrationOwner::ProviderNative,
            checked_at_ms: 1_010,
            complete: true,
            item_identities: identities.iter().map(|id| (*id).into()).collect(),
        }
    }

    #[test]
    fn complete_fresh_owner_membership_decides_exact_identity_only() {
        let inventory = attachment_inventory();
        let original = inventory.clone();
        let current = current_session();
        let attached = owner_membership(&[ID]);
        assert_eq!(
            effective_attachment_from_owner(&inventory, ID, &current, Some(&attached), 1_020),
            Some(true)
        );
        let other_same_name = owner_membership(&[OTHER_ID]);
        assert_eq!(
            effective_attachment_from_owner(
                &inventory,
                ID,
                &current,
                Some(&other_same_name),
                1_020
            ),
            Some(false),
            "same-name items from separate sources cannot match by name"
        );
        assert_eq!(
            effective_attachment_from_owner(
                &inventory,
                ID,
                &current,
                Some(&owner_membership(&[])),
                1_020
            ),
            Some(false)
        );
        assert_eq!(inventory, original, "the pure gate cannot mutate facts");
        assert!(inventory.items.iter().all(|item| {
            item.available.is_none()
                && item.attached.is_none()
                && item.actions.enable == IntegrationActionAvailability::Unavailable
                && item.actions.authenticate == IntegrationActionAvailability::Unavailable
        }));
    }

    #[test]
    fn raw_discovery_names_and_partial_or_stale_receipts_are_unknown() {
        let inventory = attachment_inventory();
        let current = current_session();
        let claude_status_and_names = ("connected", "duplicate-name");
        let codex_cwd_and_name = ("/synthetic-project", "duplicate-name");
        assert_eq!(claude_status_and_names.1, inventory.items[0].name);
        assert_eq!(codex_cwd_and_name.1, inventory.items[1].name);
        assert_eq!(
            effective_attachment_from_owner(&inventory, ID, &current, None, 1_020),
            None,
            "observed status, name, or cwd cannot stand in for owner readback"
        );

        let mut partial = owner_membership(&[ID]);
        partial.complete = false;
        assert_eq!(
            effective_attachment_from_owner(&inventory, ID, &current, Some(&partial), 1_020),
            None
        );
        let fresh = owner_membership(&[ID]);
        assert_eq!(
            effective_attachment_from_owner(&inventory, ID, &current, Some(&fresh), 31_011),
            None,
            "readback older than 30 seconds is stale"
        );
        assert_eq!(
            effective_attachment_from_owner(&inventory, ID, &current, Some(&fresh), 1_009),
            None,
            "a future-dated owner readback is not fresh"
        );
        let mut pre_session = fresh;
        pre_session.checked_at_ms = 999;
        assert_eq!(
            effective_attachment_from_owner(&inventory, ID, &current, Some(&pre_session), 1_020),
            None
        );
    }

    #[test]
    fn wrong_owner_or_selected_session_binding_is_unknown() {
        let inventory = attachment_inventory();
        let current = current_session();
        let base = owner_membership(&[ID]);
        let mut mismatches = Vec::new();
        let mut wrong = base.clone();
        wrong.owner = IntegrationOwner::Gateway;
        mismatches.push(wrong);
        let mut wrong = base.clone();
        wrong.provider = HarnessId::ClaudeCode;
        mismatches.push(wrong);
        let mut wrong = base.clone();
        wrong.machine_id = "other-machine".into();
        mismatches.push(wrong);
        let mut wrong = base.clone();
        wrong.task_id = "other-task".into();
        mismatches.push(wrong);
        let mut wrong = base.clone();
        wrong.provider_session_id = "other-session".into();
        mismatches.push(wrong);
        let mut wrong = base;
        wrong.generation = 8;
        mismatches.push(wrong);
        for mismatched in mismatches {
            assert_eq!(
                effective_attachment_from_owner(&inventory, ID, &current, Some(&mismatched), 1_020),
                None
            );
        }

        let mut wrong_current = current.clone();
        wrong_current.provider = HarnessId::ClaudeCode;
        assert_eq!(
            effective_attachment_from_owner(
                &inventory,
                ID,
                &wrong_current,
                Some(&owner_membership(&[ID])),
                1_020
            ),
            None
        );
        wrong_current = current.clone();
        wrong_current.machine_id = "other-machine".into();
        assert_eq!(
            effective_attachment_from_owner(
                &inventory,
                ID,
                &wrong_current,
                Some(&owner_membership(&[ID])),
                1_020
            ),
            None
        );
        wrong_current = current;
        wrong_current.provider_session_id.clear();
        assert_eq!(
            effective_attachment_from_owner(
                &inventory,
                ID,
                &wrong_current,
                Some(&owner_membership(&[ID])),
                1_020
            ),
            None
        );
    }

    #[test]
    fn duplicate_or_invalid_membership_and_conflicting_inventory_are_unknown() {
        let inventory = attachment_inventory();
        let current = current_session();
        for members in [vec![ID, ID], vec![ID, "duplicate-name"]] {
            let readback = owner_membership(&members);
            assert_eq!(
                effective_attachment_from_owner(&inventory, ID, &current, Some(&readback), 1_020),
                None
            );
        }
        let uppercase = format!("intg_v1_{}", "A".repeat(64));
        let noncanonical = owner_membership(&[&uppercase]);
        assert_eq!(
            effective_attachment_from_owner(&inventory, ID, &current, Some(&noncanonical), 1_020),
            None
        );
        let mut oversized = owner_membership(&[]);
        oversized.item_identities = (0..=MAX_ATTACHMENT_MEMBERS)
            .map(|index| format!("intg_v1_{index:064x}"))
            .collect();
        assert_eq!(
            effective_attachment_from_owner(&inventory, ID, &current, Some(&oversized), 1_020),
            None
        );
        let valid = owner_membership(&[ID]);
        let mut duplicate_inventory = inventory.clone();
        duplicate_inventory
            .items
            .push(duplicate_inventory.items[0].clone());
        assert_eq!(
            effective_attachment_from_owner(
                &duplicate_inventory,
                ID,
                &current,
                Some(&valid),
                1_020
            ),
            None
        );
        let mut unknown_owner = inventory.clone();
        unknown_owner.items[0].owner = IntegrationOwner::Unknown;
        assert_eq!(
            effective_attachment_from_owner(&unknown_owner, ID, &current, Some(&valid), 1_020),
            None
        );
        let mut disabled = inventory.clone();
        disabled.items[0].available = Some(false);
        assert_eq!(
            effective_attachment_from_owner(&disabled, ID, &current, Some(&valid), 1_020),
            Some(true),
            "disabled configuration cannot erase an already attached session"
        );
        let mut stale_disabled = owner_membership(&[]);
        stale_disabled.checked_at_ms = current.started_at_ms - 1;
        assert_eq!(
            effective_attachment_from_owner(&disabled, ID, &current, Some(&stale_disabled), 1_020),
            None,
            "stale absence cannot claim the disabled server detached"
        );
        let mut conflicting_attachment = inventory.clone();
        conflicting_attachment.items[0].attached = Some(true);
        assert_eq!(
            effective_attachment_from_owner(
                &conflicting_attachment,
                ID,
                &current,
                Some(&owner_membership(&[])),
                1_020
            ),
            None
        );
        let mut cross_owner = inventory;
        cross_owner.items[1].owner = IntegrationOwner::Gateway;
        assert_eq!(
            effective_attachment_from_owner(
                &cross_owner,
                ID,
                &current,
                Some(&owner_membership(&[ID, OTHER_ID])),
                1_020
            ),
            None
        );
        assert_eq!(
            effective_attachment_from_owner(
                &cross_owner,
                "duplicate-name",
                &current,
                Some(&valid),
                1_020
            ),
            None
        );
    }

    #[derive(Clone, serde::Serialize, serde::Deserialize)]
    struct FakeState {
        revision: u64,
        enabled: bool,
        authenticated: bool,
        receipts: BTreeMap<String, OwnerReceipt>,
    }

    impl Default for FakeState {
        fn default() -> Self {
            Self {
                revision: 0,
                enabled: false,
                authenticated: false,
                receipts: BTreeMap::new(),
            }
        }
    }

    #[derive(Default)]
    struct Faults {
        before_mutation: bool,
        indeterminate_without_receipt: bool,
        lose_response_after_commit: bool,
        change_before_apply: bool,
        mismatch_readback: bool,
    }

    struct FakeOwner {
        path: PathBuf,
        state: Mutex<FakeState>,
        faults: Mutex<Faults>,
    }

    impl FakeOwner {
        fn open(root: &Path) -> Self {
            std::fs::create_dir_all(root).unwrap();
            let path = root.join("synthetic-owner.json");
            let state = std::fs::read(&path)
                .ok()
                .map(|bytes| serde_json::from_slice(&bytes).unwrap())
                .unwrap_or_default();
            Self {
                path,
                state: Mutex::new(state),
                faults: Mutex::new(Faults::default()),
            }
        }

        fn persist(&self, state: &FakeState) {
            let parent = self.path.parent().unwrap();
            let mut file = tempfile::NamedTempFile::new_in(parent).unwrap();
            serde_json::to_writer(file.as_file_mut(), state).unwrap();
            file.as_file_mut().sync_all().unwrap();
            file.persist(&self.path).unwrap();
        }

        fn item(owner: IntegrationOwner) -> IntegrationInventoryItem {
            IntegrationInventoryItem {
                identity: Some(ID.into()),
                name: "synthetic skill".into(),
                kind: IntegrationKind::Skill,
                source: "synthetic owner".into(),
                scope: "global".into(),
                configured: true,
                available: None,
                attached: None,
                reason: None,
                owner,
                actions: IntegrationActionFacts {
                    enable: IntegrationActionAvailability::Supported,
                    disable: IntegrationActionAvailability::Supported,
                    authenticate: IntegrationActionAvailability::Supported,
                    reason: None,
                },
                help_route: IntegrationHelpRoute::SkillSource {
                    scope: IntegrationSkillScope::Global,
                },
            }
        }
    }

    impl IntegrationSource for FakeOwner {
        fn snapshot(&self, identity: &str) -> Result<Option<OwnerSnapshot>, ()> {
            if identity != ID {
                return Ok(None);
            }
            let mismatch = self.faults.lock().unwrap().mismatch_readback;
            let state = self.state.lock().unwrap();
            Ok(Some(OwnerSnapshot {
                item: Self::item(IntegrationOwner::ProviderNative),
                owner_revision: Some(state.revision),
                enabled: Some(if mismatch {
                    !state.enabled
                } else {
                    state.enabled
                }),
                authenticated: Some(state.authenticated),
            }))
        }
    }

    impl IntegrationOwnerDriver for FakeOwner {
        fn owner(&self) -> IntegrationOwner {
            IntegrationOwner::ProviderNative
        }

        fn receipt(&self, key: &str) -> Result<Option<OwnerReceipt>, ()> {
            Ok(self.state.lock().unwrap().receipts.get(key).cloned())
        }

        fn apply_if_revision(&self, request: &ActionRequest) -> Result<OwnerReceipt, DriverError> {
            if request.identity != ID {
                return Err(DriverError::PreMutationFailure);
            }
            let mut faults = self.faults.lock().unwrap();
            if faults.before_mutation {
                faults.before_mutation = false;
                return Err(DriverError::PreMutationFailure);
            }
            if faults.indeterminate_without_receipt {
                faults.indeterminate_without_receipt = false;
                return Err(DriverError::Indeterminate);
            }
            let mut state = self.state.lock().unwrap();
            if faults.change_before_apply {
                faults.change_before_apply = false;
                state.revision += 1;
                self.persist(&state);
            }
            if let Some(receipt) = state.receipts.get(&request.idempotency_key) {
                return if receipt.identity == request.identity
                    && receipt.operation == request.operation
                    && receipt.expected_owner_revision == request.expected_owner_revision
                {
                    Ok(receipt.clone())
                } else {
                    Err(DriverError::IdempotencyConflict)
                };
            }
            if state.revision != request.expected_owner_revision {
                return Err(DriverError::RevisionConflict);
            }
            match request.operation {
                IntegrationOperation::Enable => state.enabled = true,
                IntegrationOperation::Disable => state.enabled = false,
                IntegrationOperation::Authenticate => state.authenticated = true,
            }
            state.revision += 1;
            let receipt = OwnerReceipt {
                identity: request.identity.clone(),
                owner: IntegrationOwner::ProviderNative,
                operation: request.operation,
                expected_owner_revision: request.expected_owner_revision,
                resulting_owner_revision: state.revision,
                idempotency_key: request.idempotency_key.clone(),
            };
            state
                .receipts
                .insert(request.idempotency_key.clone(), receipt.clone());
            self.persist(&state);
            if faults.lose_response_after_commit {
                faults.lose_response_after_commit = false;
                Err(DriverError::Indeterminate)
            } else {
                Ok(receipt)
            }
        }
    }

    fn service(owner: Arc<FakeOwner>) -> IntegrationActionService {
        IntegrationActionService::read_only(owner.clone())
            .with_driver(owner)
            .unwrap()
    }

    fn request(operation: IntegrationOperation, revision: u64, key: &str) -> ActionRequest {
        ActionRequest {
            identity: ID.into(),
            operation,
            expected_owner_revision: revision,
            idempotency_key: key.into(),
        }
    }

    #[test]
    fn read_only_source_returns_typed_help_and_never_offers_an_action() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("synthetic-codex");
        std::fs::create_dir_all(home.join("skills/example")).unwrap();
        std::fs::write(home.join("skills/example/SKILL.md"), "# Synthetic").unwrap();
        let inventory = crate::integration_settings::codex_inventory(&home, None, "fake-machine");
        let id = inventory.items[0].identity.as_deref().unwrap();
        let source = CodexReadOnlySource {
            codex_home: home,
            project_root: None,
            machine_id: "fake-machine".into(),
        };
        let service = IntegrationActionService::read_only(Arc::new(source));
        assert_eq!(
            service.help(id),
            Ok(IntegrationHelpRoute::SkillSource {
                scope: IntegrationSkillScope::Global
            })
        );
        let mut request = request(IntegrationOperation::Enable, 0, "readonly");
        request.identity = id.into();
        assert!(matches!(
            service.act(&request),
            Err(ActionError::Unsupported { .. })
        ));
    }

    #[test]
    fn duplicate_key_survives_restart_and_changed_intent_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let owner = Arc::new(FakeOwner::open(root.path()));
        let original = request(IntegrationOperation::Enable, 0, "same-key");
        let first = service(owner).act(&original).unwrap();
        assert_eq!(first.receipt.resulting_owner_revision, 1);
        assert_eq!(first.observed.item.attached, None);

        let restarted = Arc::new(FakeOwner::open(root.path()));
        let service = service(restarted.clone());
        assert_eq!(service.act(&original).unwrap().receipt, first.receipt);
        assert_eq!(restarted.state.lock().unwrap().revision, 1);
        assert_eq!(
            service.act(&request(IntegrationOperation::Disable, 0, "same-key")),
            Err(ActionError::IdempotencyConflict)
        );
    }

    #[test]
    fn stale_and_concurrent_owner_revisions_cannot_apply() {
        let root = tempfile::tempdir().unwrap();
        let owner = Arc::new(FakeOwner::open(root.path()));
        let service = service(owner.clone());
        service
            .act(&request(IntegrationOperation::Enable, 0, "first"))
            .unwrap();
        assert_eq!(
            service.act(&request(IntegrationOperation::Disable, 0, "stale")),
            Err(ActionError::RevisionConflict)
        );
        owner.faults.lock().unwrap().change_before_apply = true;
        assert_eq!(
            service.act(&request(IntegrationOperation::Disable, 1, "raced")),
            Err(ActionError::RevisionConflict)
        );
        assert!(owner.state.lock().unwrap().enabled);
    }

    #[test]
    fn failures_and_mismatched_readback_never_report_success() {
        let root = tempfile::tempdir().unwrap();
        let owner = Arc::new(FakeOwner::open(root.path()));
        let service = service(owner.clone());
        owner.faults.lock().unwrap().before_mutation = true;
        let request = request(IntegrationOperation::Enable, 0, "failure");
        assert_eq!(service.act(&request), Err(ActionError::DriverFailure));
        assert_eq!(owner.state.lock().unwrap().revision, 0);

        owner.faults.lock().unwrap().mismatch_readback = true;
        assert_eq!(service.act(&request), Err(ActionError::Indeterminate));
        assert_eq!(owner.state.lock().unwrap().revision, 1);
    }

    #[test]
    fn lost_response_after_commit_resolves_from_owner_receipt_without_replay() {
        let root = tempfile::tempdir().unwrap();
        let owner = Arc::new(FakeOwner::open(root.path()));
        owner.faults.lock().unwrap().lose_response_after_commit = true;
        let request = request(IntegrationOperation::Authenticate, 0, "lost-response");
        let result = service(owner.clone()).act(&request).unwrap();
        assert_eq!(result.observed.authenticated, Some(true));
        assert_eq!(result.observed.item.attached, None);
        assert_eq!(owner.state.lock().unwrap().revision, 1);
        assert_eq!(owner.state.lock().unwrap().receipts.len(), 1);
    }

    #[test]
    fn uncertain_response_without_owner_receipt_is_never_replayed() {
        let root = tempfile::tempdir().unwrap();
        let owner = Arc::new(FakeOwner::open(root.path()));
        owner.faults.lock().unwrap().indeterminate_without_receipt = true;
        let request = request(IntegrationOperation::Enable, 0, "uncertain");
        assert_eq!(
            service(owner.clone()).act(&request),
            Err(ActionError::Indeterminate)
        );
        let state = owner.state.lock().unwrap();
        assert_eq!(state.revision, 0);
        assert!(state.receipts.is_empty());
    }

    #[test]
    fn names_paths_and_unknown_owners_are_not_action_targets() {
        let root = tempfile::tempdir().unwrap();
        let owner = Arc::new(FakeOwner::open(root.path()));
        let service = service(owner.clone());
        for invalid in [
            "synthetic skill",
            "/synthetic/skills/example",
            "intg_v1_short",
        ] {
            let mut request = request(IntegrationOperation::Enable, 0, "invalid");
            request.identity = invalid.into();
            assert_eq!(service.act(&request), Err(ActionError::InvalidIdentity));
        }
        let unknown = UnknownOwnerSource(owner);
        let service = IntegrationActionService::read_only(Arc::new(unknown))
            .with_driver(Arc::new(FakeOwner::open(root.path())))
            .unwrap();
        assert!(matches!(
            service.act(&request(IntegrationOperation::Enable, 0, "unknown")),
            Err(ActionError::Unsupported { .. })
        ));
        let owner = Arc::new(FakeOwner::open(root.path()));
        let service =
            IntegrationActionService::read_only(Arc::new(UnadvertisedSource(owner.clone())))
                .with_driver(owner)
                .unwrap();
        assert!(matches!(
            service.act(&request(IntegrationOperation::Enable, 0, "unadvertised")),
            Err(ActionError::Unsupported { .. })
        ));
    }

    struct UnknownOwnerSource(Arc<FakeOwner>);

    impl IntegrationSource for UnknownOwnerSource {
        fn snapshot(&self, identity: &str) -> Result<Option<OwnerSnapshot>, ()> {
            let mut snapshot = self.0.snapshot(identity)?;
            if let Some(snapshot) = snapshot.as_mut() {
                snapshot.item.owner = IntegrationOwner::Unknown;
            }
            Ok(snapshot)
        }
    }

    struct UnadvertisedSource(Arc<FakeOwner>);

    impl IntegrationSource for UnadvertisedSource {
        fn snapshot(&self, identity: &str) -> Result<Option<OwnerSnapshot>, ()> {
            let mut snapshot = self.0.snapshot(identity)?;
            if let Some(snapshot) = snapshot.as_mut() {
                snapshot.item.actions = IntegrationActionFacts::default();
            }
            Ok(snapshot)
        }
    }
}
