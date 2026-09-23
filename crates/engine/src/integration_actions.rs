//! Owner-managed integration actions. No production owner driver is registered.
//!
//! A source is re-read for every help or action request. The source supplies
//! identity and help, while an explicitly registered owner driver must provide
//! a revisioned, idempotent mutation and an authoritative readback. This module
//! never writes provider configuration or starts authentication itself.

use std::path::PathBuf;
use std::sync::Arc;

use zeron_proto::{
    IntegrationActionAvailability, IntegrationHelpRoute, IntegrationInventoryItem, IntegrationOwner,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::path::Path;
    use std::sync::Mutex;
    use zeron_proto::{IntegrationActionFacts, IntegrationKind, IntegrationSkillScope};

    const ID: &str = "intg_v1_0000000000000000000000000000000000000000000000000000000000000001";

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
