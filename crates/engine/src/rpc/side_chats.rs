use super::*;

pub(super) fn handles(method: &str) -> bool {
    matches!(
        method,
        methods::CREATE_SIDE_CHAT
            | methods::LIST_SIDE_CHATS
            | methods::READ_SIDE_CHAT
            | methods::SEND_SIDE_CHAT
            | methods::UNLOAD_SIDE_CHAT
            | methods::STOP_SIDE_CHAT
            | methods::RENEW_SIDE_CHAT
            | methods::ANSWER_SIDE_CHAT
    )
}

impl EngineRpc {
    pub(super) async fn handle_side_chat(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<RpcReply, RpcError> {
        self.handle_side_chat_with_fence(method, params, None).await
    }

    pub(super) async fn handle_side_chat_with_fence(
        &self,
        method: &str,
        params: serde_json::Value,
        fence: Option<&crate::cross_task_tools::ApprovedSendFence>,
    ) -> Result<RpcReply, RpcError> {
        if let Some(fence) = fence {
            self.sessions
                .with_live_approved_send(fence, || Ok(()))
                .map_err(|error| RpcError::Failed(error.to_string()))?;
        }
        // Side chats are local to the owning profile. Never ignore an explicit
        // remote target and accidentally create a local provider session.
        if params
            .get("targetDeviceId")
            .and_then(|value| value.as_str())
            .is_some_and(|id| id != self.engine_info.device_id)
        {
            return Err(RpcError::Failed(
                "Open the parent on its owning device to use side chats".into(),
            ));
        }
        let service = self
            .side_chats
            .as_ref()
            .ok_or_else(|| RpcError::Failed("Side chat service unavailable".into()))?;
        let fail = |error: anyhow::Error| RpcError::Failed(error.to_string());
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Params {
            #[serde(default)]
            id: String,
            #[serde(default)]
            parent_id: String,
            #[serde(default)]
            request_id: String,
            #[serde(default)]
            origin_message_id: Option<String>,
            #[serde(default)]
            anchor_text: Option<String>,
            #[serde(default)]
            force_snapshot: bool,
            #[serde(default)]
            lease: Option<String>,
            #[serde(default)]
            before: Option<i64>,
            #[serde(default)]
            message_id: String,
            #[serde(default)]
            text: String,
            #[serde(default)]
            answers: Vec<luna_proto::UserInputAnswer>,
            #[serde(default)]
            decision: Option<luna_proto::ApprovalDecision>,
        }
        let p: Params = parse_params(params)?;
        match method {
            methods::CREATE_SIDE_CHAT => {
                let id = uuid::Uuid::parse_str(&p.request_id)
                    .map_err(|error| RpcError::Failed(error.to_string()))?
                    .to_string();
                if service.contains(&id) {
                    return RpcReply::value(
                        &service
                            .prepare(
                                &p.parent_id,
                                &p.request_id,
                                false,
                                p.origin_message_id.as_deref(),
                                p.anchor_text.as_deref(),
                                p.force_snapshot,
                            )
                            .await
                            .map_err(fail)?,
                    );
                }
                if self.task_identity.view(&id).is_some() {
                    return Err(RpcError::Failed(
                        "Creation request ID belongs to an existing or deleted task".into(),
                    ));
                }
                let chat = self
                    .workspace
                    .chat(&p.parent_id)
                    .map_err(|error| RpcError::Failed(error.to_string()))?
                    .ok_or_else(|| RpcError::Failed("Parent conversation unavailable".into()))?;
                let config = chat
                    .config
                    .as_ref()
                    .ok_or_else(|| RpcError::Failed("Choose the parent's provider first".into()))?;
                let native = !p.force_snapshot
                    && capability_for(config.harness) != ProviderForkCapability::None;
                let readiness = if native {
                    Some(
                        self.ensure_history_operation_ready(&p.parent_id)
                            .map_err(|error| RpcError::Failed(error.to_string()))?,
                    )
                } else {
                    None
                };
                let created = service
                    .prepare(
                        &p.parent_id,
                        &p.request_id,
                        native,
                        p.origin_message_id.as_deref(),
                        p.anchor_text.as_deref(),
                        p.force_snapshot,
                    )
                    .await
                    .map_err(fail)?;
                let Some((source, binding)) = readiness else {
                    let command: TaskCommandParams = parse_params(serde_json::json!({
                        "commandId": format!("side-chat-create:{}", p.request_id), "taskId":id,
                        "intent": { "kind":"createChat", "sideParentId":p.parent_id,
                            "deviceId":chat.device_id, "spaceId":chat.space_id, "cwd":chat.cwd,
                            "branch":chat.branch, "config":created.meta.config,
                            "lineage":{"sourceTaskId":p.parent_id,"lifecycleOwnerTaskId":id}
                        }
                    }))?;
                    // A portable fork creates a new task; fence both the source
                    // context and the new task's command admission.
                    let mut creation_fence = fence.cloned();
                    if let Some(source_fence) = fence {
                        self.sessions
                            .with_live_approved_send(source_fence, || Ok(()))
                            .map_err(|error| RpcError::Failed(error.to_string()))?;
                    }
                    if let Some(fence) = &mut creation_fence {
                        fence.request.task_id = id.clone();
                        fence.target_snapshot = self.current_task_command_snapshot(&id)?;
                    }
                    return match self
                        .execute_task_command_with_send_fence(
                            command,
                            false,
                            creation_fence.as_ref(),
                        )
                        .await
                    {
                        Ok(receipt)
                            if receipt.status == luna_proto::CommandReceiptStatus::Applied =>
                        {
                            RpcReply::value(&service.activate(&id).await.map_err(fail)?)
                        }
                        result => {
                            let detail = format!(
                                "Side chat creation did not complete: {result:?}. It will not be replayed."
                            );
                            service.creation_failed(&id, &detail);
                            Err(RpcError::Failed(detail))
                        }
                    };
                };
                let snapshot = self.current_task_command_snapshot(&p.parent_id)?;
                let (boundary, message_text) = if let Some(origin) = p.origin_message_id.as_deref()
                {
                    let entry = self
                        .doc_host
                        .open(&p.parent_id)
                        .map_err(|error| RpcError::Failed(error.to_string()))?
                        .doc()
                        .read_entries()
                        .map_err(|error| RpcError::Failed(error.to_string()))?
                        .into_iter()
                        .find(|entry| entry.id == origin)
                        .ok_or_else(|| RpcError::Failed("Origin message is unavailable".into()))?;
                    let text = entry
                        .parts
                        .iter()
                        .filter_map(|part| match part {
                            MessagePart::Text { text, .. } => Some(text.trim()),
                            _ => None,
                        })
                        .filter(|text| !text.is_empty())
                        .collect::<Vec<_>>()
                        .join("\n");
                    (
                        if entry.role == MessageRole::User {
                            "beforeUserMessage"
                        } else {
                            "afterCompletedTurn"
                        },
                        Some(text),
                    )
                } else {
                    ("tip", None)
                };
                let command: TaskCommandParams = parse_params(serde_json::json!({
                    "commandId": format!("side-chat-create:{}", p.request_id),
                    "taskId": p.parent_id,
                    "expectedTaskRevision": snapshot.task_revision,
                    "expectedBindingGeneration": snapshot.binding_generation,
                    "intent": {
                        "kind":"forkTask", "sideChat":true, "newTaskId":id,
                        "boundary":boundary, "messageId":p.origin_message_id, "title":created.meta.title,
                        "sourceLink":format!("luna://open/chat/{}?workspace=local", p.parent_id),
                        "review": {
                            "taskRevision":snapshot.task_revision,
                            "bindingGeneration":snapshot.binding_generation,
                            "providerSessionId":binding.provider_task_id,
                            "providerAccountId":source.config.as_ref().and_then(|config| config.provider_account_id.clone()),
                            "cwd":source.cwd.unwrap_or_default(),
                            "messageId":p.origin_message_id, "messageText":message_text
                        }
                    }
                }))?;
                let result = self
                    .execute_task_command_with_send_fence(command, false, fence)
                    .await;
                match result {
                    Ok(receipt) if receipt.status == luna_proto::CommandReceiptStatus::Applied => {
                        RpcReply::value(&service.activate(&id).await.map_err(fail)?)
                    }
                    result => {
                        let detail = match result {
                            Ok(receipt)
                                if receipt.status == luna_proto::CommandReceiptStatus::Pending =>
                            {
                                format!(
                                    "Native fork did not complete ({:?}): {}. Creation will not be replayed; receipt {} preserves the outcome.",
                                    receipt.status,
                                    receipt.reason.as_ref().map_or(
                                        "Inspect the saved receipt for the provider result",
                                        |reason| reason.message.as_str()
                                    ),
                                    receipt.command_id
                                )
                            }
                            Ok(receipt) => format!(
                                "Native fork could not be created ({:?}): {}. Receipt {} records the refusal.",
                                receipt.status,
                                receipt.reason.as_ref().map_or(
                                    "The request did not pass history validation",
                                    |reason| reason.message.as_str()
                                ),
                                receipt.command_id,
                            ),
                            Err(error) => format!(
                                "Native fork failed: {error}. Creation will not be replayed."
                            ),
                        };
                        service.creation_failed(&id, &detail);
                        Err(RpcError::Failed(detail))
                    }
                }
            }
            methods::LIST_SIDE_CHATS => {
                RpcReply::value(&service.list(&p.parent_id, p.before).map_err(fail)?)
            }
            methods::READ_SIDE_CHAT => RpcReply::value(
                &service
                    .read(&p.id, p.lease.as_deref(), p.before)
                    .await
                    .map_err(fail)?,
            ),
            methods::UNLOAD_SIDE_CHAT => RpcReply::value(
                &service
                    .unload(&p.id, p.lease.as_deref())
                    .await
                    .map_err(fail)?,
            ),
            methods::RENEW_SIDE_CHAT => RpcReply::value(
                &service
                    .renew(&p.id, p.lease.as_deref().unwrap_or_default())
                    .await
                    .map_err(fail)?,
            ),
            methods::STOP_SIDE_CHAT => {
                service
                    .stop(&p.id, p.lease.as_deref().unwrap_or_default())
                    .await
                    .map_err(fail)?;
                RpcReply::value(&serde_json::json!({"stopped":true}))
            }
            methods::ANSWER_SIDE_CHAT => {
                service
                    .answer(
                        &p.id,
                        p.lease.as_deref().unwrap_or_default(),
                        &p.request_id,
                        p.answers,
                        p.decision,
                    )
                    .await
                    .map_err(fail)?;
                RpcReply::value(&serde_json::json!({"resolved":true}))
            }
            methods::SEND_SIDE_CHAT => {
                service
                    .send(
                        &p.id,
                        p.lease.as_deref().unwrap_or_default(),
                        &p.message_id,
                        &p.text,
                    )
                    .await
                    .map_err(fail)?;
                RpcReply::value(&serde_json::json!({"accepted":true}))
            }
            _ => unreachable!(),
        }
    }
}
