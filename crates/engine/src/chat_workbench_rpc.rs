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
            Some((parent, state)) => {
                Ok(tab.side_parent_id.as_deref() == Some(parent.as_str()) && state != "deleted")
            }
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
            return Err(RpcError::Failed(
                "Main chat is unavailable or is a side chat".into(),
            ));
        }
        Ok(())
    }

    pub(super) fn read_chat_workbench(
        &self,
        main: &str,
    ) -> Result<ChatWorkbenchSnapshot, RpcError> {
        self.validate_workbench_main(main)?;
        let service = self.chat_workbenches_service()?;
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

    pub(super) fn mutate_chat_workbench(
        &self,
        request: MutateChatWorkbenchRequest,
    ) -> Result<luna_proto::ChatWorkbenchMutationResult, RpcError> {
        if self.engine_info.profile_id.as_deref() != Some(request.profile_id.as_str()) {
            return Err(RpcError::Failed(
                "Workbench profile changed; reload before changing this layout".into(),
            ));
        }
        let current = self.read_chat_workbench(&request.main_chat_id)?;
        let target = match &request.command {
            ChatWorkbenchCommand::OpenTab { tab, .. } => Some(tab),
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
            && !self.workbench_tab_exists(tab)?
        {
            return Err(RpcError::Failed(
                "Conversation is unavailable; its tab cannot be opened".into(),
            ));
        }
        self.chat_workbenches_service()?
            .mutate(&request)
            .map_err(|error| RpcError::Failed(error.to_string()))
    }
}
