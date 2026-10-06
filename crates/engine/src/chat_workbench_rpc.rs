//! Local desktop layout facade. Presentation commands never execute agents.
use super::*;
use luna_proto::{
    ChatWorkbenchCommand, ChatWorkbenchSnapshot, ChatWorkbenchTab, MutateChatWorkbenchRequest,
};

impl EngineRpc {
    pub(crate) fn with_chat_workbenches(
        mut self,
        service: crate::chat_workbench::ChatWorkbenches,
    ) -> Self {
        self.chat_workbenches = Some(service);
        self.engine_info
            .capabilities
            .push(luna_proto::capabilities::CHAT_WORKBENCH_V1.into());
        self
    }

    pub(super) fn chat_workbenches_service(
        &self,
    ) -> Result<&crate::chat_workbench::ChatWorkbenches, RpcError> {
        self.chat_workbenches.as_ref().ok_or_else(|| {
            RpcError::Failed("Chat layouts are available only on the local desktop engine".into())
        })
    }

    pub(super) fn chat_workbench_reader(&self) -> Result<ChatWorkbenchReader, RpcError> {
        Ok(ChatWorkbenchReader {
            workspace: self.workspace.clone(),
            side_chats: self.side_chats.clone(),
            service: self.chat_workbenches_service()?.clone(),
        })
    }

    pub(super) fn read_chat_workbench(
        &self,
        main: &str,
    ) -> Result<ChatWorkbenchSnapshot, RpcError> {
        self.chat_workbench_reader()?.read_chat_workbench(main)
    }

    pub(super) fn mutate_chat_workbench(
        &self,
        request: MutateChatWorkbenchRequest,
    ) -> Result<luna_proto::ChatWorkbenchMutationResult, RpcError> {
        self.mutate_chat_workbench_with(request, false)
    }

    /// Applies a destination-free command to the current layout; the request's
    /// `expected_revision` is ignored. See `ChatWorkbenches::mutate_latest`.
    pub(super) fn mutate_chat_workbench_latest(
        &self,
        request: MutateChatWorkbenchRequest,
    ) -> Result<luna_proto::ChatWorkbenchMutationResult, RpcError> {
        self.mutate_chat_workbench_with(request, true)
    }

    fn mutate_chat_workbench_with(
        &self,
        request: MutateChatWorkbenchRequest,
        latest: bool,
    ) -> Result<luna_proto::ChatWorkbenchMutationResult, RpcError> {
        if self.engine_info.profile_id.as_deref() != Some(request.profile_id.as_str()) {
            return Err(RpcError::Failed(
                "Workbench profile changed; reload before changing this layout".into(),
            ));
        }
        let reader = self.chat_workbench_reader()?;
        let current = reader.read_chat_workbench(&request.main_chat_id)?;
        let target = match &request.command {
            ChatWorkbenchCommand::OpenTab { tab, .. }
            | ChatWorkbenchCommand::OpenBackgroundTab { tab, .. } => Some(tab),
            ChatWorkbenchCommand::MoveTab { chat_id, .. }
            | ChatWorkbenchCommand::FocusTab { chat_id } => current
                .root
                .tabs()
                .into_iter()
                .find(|tab| &tab.chat_id == chat_id),
            ChatWorkbenchCommand::ReopenClosed { chat_id } => current
                .closed_tabs
                .iter()
                .find(|closed| chat_id.as_ref().is_none_or(|id| &closed.tab.chat_id == id))
                .map(|closed| &closed.tab),
            _ => None,
        };
        if let Some(tab) = target
            && !reader.workbench_tab_exists(tab)?
        {
            return Err(RpcError::Failed(
                "Conversation is unavailable; its tab cannot be opened".into(),
            ));
        }
        let service = self.chat_workbenches_service()?;
        if latest {
            service.mutate_latest(&request)
        } else {
            service.mutate(&request)
        }
        .map_err(|error| RpcError::Failed(error.to_string()))
    }
}

/// The read facade can live for a stream without retaining the entire RPC
/// service. Registry changes and layout changes share identical validation.
pub(super) struct ChatWorkbenchReader {
    workspace: WorkspaceHost,
    side_chats: Option<crate::side_chats::SideChats>,
    service: crate::chat_workbench::ChatWorkbenches,
}

impl ChatWorkbenchReader {
    fn workbench_tab_exists(&self, tab: &ChatWorkbenchTab) -> Result<bool, RpcError> {
        if !luna_proto::valid_workbench_id(&tab.chat_id)
            || tab
                .side_parent_id
                .as_ref()
                .is_some_and(|id| !luna_proto::valid_workbench_id(id))
        {
            return Err(RpcError::Failed(
                "Invalid chat identity in workbench".into(),
            ));
        }
        let side = self
            .side_chats
            .as_ref()
            .map(|service| service.layout_identity(&tab.chat_id))
            .transpose()
            .map_err(|error| RpcError::Failed(error.to_string()))?
            .flatten();
        match side {
            Some((parent, state)) => Ok(state != "deleted"
                && tab
                    .side_parent_id
                    .as_deref()
                    .is_none_or(|source| source == parent)
                && self
                    .workspace
                    .chat(&tab.chat_id)
                    .map_err(|error| RpcError::Failed(error.to_string()))?
                    .is_some()),
            None if tab.side_parent_id.is_some() => Ok(false),
            None => Ok(self
                .workspace
                .chat(&tab.chat_id)
                .map_err(|error| RpcError::Failed(error.to_string()))?
                .is_some()),
        }
    }

    fn validate_workbench_main(&self, main: &str) -> Result<(), RpcError> {
        if !self.workbench_tab_exists(&ChatWorkbenchTab::chat(main))? {
            return Err(RpcError::Failed("Main chat is unavailable".into()));
        }
        Ok(())
    }

    pub(super) fn read_chat_workbench(
        &self,
        main: &str,
    ) -> Result<ChatWorkbenchSnapshot, RpcError> {
        self.validate_workbench_main(main)?;
        let service = &self.service;
        let snapshot = service
            .get(main)
            .map_err(|error| RpcError::Failed(error.to_string()))?;
        let mut missing = HashSet::new();
        for tab in snapshot
            .root
            .tabs()
            .into_iter()
            .chain(snapshot.closed_tabs.iter().map(|closed| &closed.tab))
        {
            if !self.workbench_tab_exists(tab)? {
                missing.insert(tab.chat_id.clone());
            }
        }
        if missing.is_empty() {
            Ok(snapshot)
        } else {
            service
                .remove_missing(main, &missing)
                .map_err(|error| RpcError::Failed(error.to_string()))
        }
    }
}
