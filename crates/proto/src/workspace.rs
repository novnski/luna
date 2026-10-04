//! Workspace lifecycle types shared by the engine and its clients.

use serde::{Deserialize, Serialize};

/// Protocol features are advertised explicitly because personal/integration
/// builds may share a semver with upstream while exposing a different RPC and
/// document surface.
pub mod capabilities {
    /// Refuses an older Luna or unrelated listener on LUNA's local IPC port.
    pub const LUNA_OWNER_IPC_V1: &str = "luna-owner-ipc-v1";
    pub const MESSAGE_QUEUE_V1: &str = "message-queue-v1";
    pub const MESSAGE_QUEUE_ACTIONS_V1: &str = "message-queue-actions-v1";
    pub const MESSAGE_QUEUE_ATTACHMENTS_V1: &str = "message-queue-attachments-v1";
    pub const MESSAGE_QUEUE_CLEAN_ATTACHMENT_TEXT_V1: &str =
        "message-queue-clean-attachment-text-v1";
    pub const MESSAGE_QUEUE_EDIT_LEASE_V1: &str = "message-queue-edit-lease-v1";
    pub const TASK_IDENTITY_V1: &str = "task-identity-v1";
    pub const SESSION_HISTORY_SEARCH_V1: &str = "session-history-search-v1";
    pub const REMOTE_AUTH_V1: &str = "remote-auth-v1";
    /// Fork threads accept ordinary task commands and per-chat RPCs, and
    /// publish project membership together with source provenance.
    pub const SIDE_CHAT_TASK_COMMANDS_V1: &str = "side-chat-task-commands-v1";

    /// Persistent fork threads have no separate lease, expiry or End lifecycle.
    pub const SIDE_CHAT_PERSISTENCE_V1: &str = "side-chat-persistence-v1";

    /// Profile-local tab/split layouts and user-created blank child tabs.
    pub const CHAT_WORKBENCH_V1: &str = "chat-workbench-v1";

    pub const CURRENT: &[&str] = &[
        LUNA_OWNER_IPC_V1,
        MESSAGE_QUEUE_V1,
        MESSAGE_QUEUE_ACTIONS_V1,
        MESSAGE_QUEUE_ATTACHMENTS_V1,
        MESSAGE_QUEUE_CLEAN_ATTACHMENT_TEXT_V1,
        MESSAGE_QUEUE_EDIT_LEASE_V1,
        TASK_IDENTITY_V1,
        SESSION_HISTORY_SEARCH_V1,
        SIDE_CHAT_TASK_COMMANDS_V1,
        SIDE_CHAT_PERSISTENCE_V1,
        CHAT_WORKBENCH_V1,
    ];

    pub fn current() -> Vec<String> {
        CURRENT.iter().map(|value| (*value).to_string()).collect()
    }
}

/// The fixed data boundary selected when an engine runtime is assembled.
///
/// Authentication can change while a runtime is alive, but its workspace scope
/// cannot. Switching scopes requires assembling a new runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkspaceScope {
    Local,
    Synced,
    Development,
}

/// Stable information about the engine runtime reached by a client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineInfo {
    pub device_id: String,
    pub workspace_scope: WorkspaceScope,
    /// Stable opaque identity for the resolved profile, not just this device.
    /// Absent on older engines and before an account/local profile is resolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
    /// Supported protocol/document features. Missing on older engines.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
}

impl EngineInfo {
    pub fn supports(&self, capability: &str) -> bool {
        self.capabilities.iter().any(|value| value == capability)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_scope_uses_wire_safe_names() {
        for (scope, encoded) in [
            (WorkspaceScope::Local, "\"local\""),
            (WorkspaceScope::Synced, "\"synced\""),
            (WorkspaceScope::Development, "\"development\""),
        ] {
            assert_eq!(serde_json::to_string(&scope).unwrap(), encoded);
            assert_eq!(
                serde_json::from_str::<WorkspaceScope>(encoded).unwrap(),
                scope
            );
        }
    }

    #[test]
    fn engine_info_uses_camel_case_fields() {
        let info = EngineInfo {
            device_id: "device-1".into(),
            workspace_scope: WorkspaceScope::Local,
            profile_id: None,
            capabilities: capabilities::current(),
        };
        assert_eq!(
            serde_json::to_value(&info).unwrap(),
            serde_json::json!({
                "deviceId": "device-1",
                "workspaceScope": "local",
                "capabilities": [
                    "luna-owner-ipc-v1",
                    "message-queue-v1",
                    "message-queue-actions-v1",
                    "message-queue-attachments-v1",
                    "message-queue-clean-attachment-text-v1",
                    "message-queue-edit-lease-v1",
                    "task-identity-v1",
                    "session-history-search-v1",
                    "side-chat-task-commands-v1"
                ],
            })
        );
    }

    #[test]
    fn old_engine_info_defaults_to_no_capabilities() {
        let info: EngineInfo = serde_json::from_value(serde_json::json!({
            "deviceId": "old",
            "workspaceScope": "synced"
        }))
        .unwrap();
        assert!(info.capabilities.is_empty());
        assert!(!info.supports(capabilities::MESSAGE_QUEUE_V1));
    }
}
