//! Goal controls on Claude Code tasks (CC-14): read Claude's own goal record
//! and run Claude's own `/goal clear`. Pause, resume, budget and in-place
//! edits do not exist in Claude and are rejected with an explanation.

use luna_proto::{CommandReasonCode, CommandReceipt, GoalOperation, HarnessId};

use super::{EngineRpc, NativeMetadataCommandGuard};
use crate::claude_ongoing::{ClaudeGoalControl, claude_binding, goal_operation};
use crate::workspace_host::TaskProjectionError;

impl EngineRpc {
    pub(super) async fn apply_claude_goal_operation(
        &self,
        command_id: &str,
        task_id: &str,
        cwd: &str,
        operation: GoalOperation,
    ) -> Result<Option<CommandReceipt>, TaskProjectionError> {
        let harness = self
            .registry
            .resolve(HarnessId::ClaudeCode)
            .map_err(|error| TaskProjectionError::Mutation(error.into()))?;
        let binding = claude_binding(
            self.task_identity.view(task_id),
            task_id,
            &self.engine_info.device_id,
            None,
        );
        let mutating = matches!(operation, GoalOperation::Clear);
        let mut cancellation_guard = mutating.then(|| NativeMetadataCommandGuard {
            doc_host: &self.doc_host,
            command_id,
            armed: true,
        });
        let outcome =
            goal_operation(&self.sessions, harness, binding, task_id, cwd, &operation).await;
        if let Some(guard) = &mut cancellation_guard {
            guard.armed = false;
        }
        let readback = match outcome {
            Ok(ClaudeGoalControl::Readback(readback)) => readback,
            Ok(ClaudeGoalControl::Unsupported(message)) => {
                return self.reject_context_command(
                    command_id,
                    CommandReasonCode::UnsupportedOperation,
                    &message,
                    "Use Clear, or send a new goal to replace this one.",
                );
            }
            Ok(ClaudeGoalControl::Busy(message)) => {
                return self.reject_context_command(
                    command_id,
                    CommandReasonCode::UnsupportedOperation,
                    &message,
                    "Wait until Claude is idle, then try again.",
                );
            }
            // A rejected start proves nothing ran; anything else after the
            // clear was dispatched is delivery-unknown and never replayed.
            Err(error) => {
                return Err(
                    if !mutating
                        || matches!(
                            &error,
                            crate::EngineError::Harness(
                                luna_harness::HarnessError::RejectedBeforeAcceptance(_)
                                    | luna_harness::HarnessError::NotInstalled(_)
                            )
                        )
                    {
                        TaskProjectionError::Mutation(error)
                    } else {
                        TaskProjectionError::ProviderUnknown(error)
                    },
                );
            }
        };
        let receipt = self
            .doc_host
            .complete_goal_operation(task_id, command_id, &readback)
            .map_err(|error| {
                if mutating {
                    TaskProjectionError::ProviderUnknown(error)
                } else {
                    TaskProjectionError::Commit(error)
                }
            })?;
        Ok(Some(receipt))
    }
}
