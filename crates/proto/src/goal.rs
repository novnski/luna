//! Provider-owned goals. Only an explicit goal control or native goal tool
//! creates/changes a goal; turn completion never implies goal completion.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GoalStatus {
    Active,
    Paused,
    Blocked,
    UsageLimited,
    BudgetLimited,
    Complete,
}

impl GoalStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Active => "Active",
            Self::Paused => "Paused",
            Self::Blocked => "Blocked",
            Self::UsageLimited => "Usage limited",
            Self::BudgetLimited => "Budget limited",
            Self::Complete => "Complete",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadGoal {
    pub thread_id: String,
    pub objective: String,
    pub status: GoalStatus,
    pub token_budget: Option<i64>,
    pub tokens_used: i64,
    pub time_used_seconds: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Absence on Set preserves the budget. Unlimited explicitly sends null.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "camelCase")]
pub enum GoalBudget {
    Unlimited,
    Limited { tokens: i64 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "camelCase")]
pub enum GoalOperation {
    Get,
    Set {
        #[serde(default)]
        objective: Option<String>,
        #[serde(default)]
        status: Option<GoalStatus>,
        #[serde(default, rename = "tokenBudget")]
        token_budget: Option<GoalBudget>,
    },
    Clear,
}

impl GoalOperation {
    pub fn validate(&self) -> Result<(), &'static str> {
        if let Self::Set {
            objective,
            status,
            token_budget,
        } = self
        {
            if objective
                .as_ref()
                .is_some_and(|text| text.trim().is_empty())
            {
                return Err("A goal objective cannot be empty.");
            }
            if objective
                .as_ref()
                .is_some_and(|text| text.chars().count() > 4_000)
            {
                return Err("A goal objective must be at most 4000 characters.");
            }
            if matches!(token_budget, Some(GoalBudget::Limited { tokens }) if *tokens <= 0) {
                return Err("A goal token budget must be positive.");
            }
            if matches!(
                status,
                Some(GoalStatus::UsageLimited | GoalStatus::BudgetLimited)
            ) {
                return Err("Only Codex can set usage or budget limited status.");
            }
            if objective.is_none() && status.is_none() && token_budget.is_none() {
                return Err("Choose a goal change before saving.");
            }
        }
        Ok(())
    }
}

/// Cached native observation, scoped to the accepted provider binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalReadback {
    pub thread_id: String,
    pub binding_generation: u32,
    pub goal: Option<ThreadGoal>,
    pub checked_at_ms: i64,
    /// A live subscription can observe Codex's autonomous continuation.
    /// False means an active goal waits for the next user message to resume.
    pub continuation_ready: bool,
}

impl GoalReadback {
    /// A command reply must not overwrite a newer native notification that
    /// reached the durable document before receipt finalization.
    pub fn supersedes(&self, current: Option<&Self>) -> bool {
        let Some(current) = current else {
            return true;
        };
        if current.thread_id != self.thread_id
            || current.binding_generation != self.binding_generation
        {
            return self.binding_generation >= current.binding_generation;
        }
        if current.checked_at_ms > self.checked_at_ms {
            return false;
        }
        if let (Some(old), Some(new)) = (&current.goal, &self.goal)
            && old.created_at == new.created_at
            && (old.updated_at > new.updated_at || old.tokens_used > new.tokens_used)
        {
            return false;
        }
        true
    }
}
