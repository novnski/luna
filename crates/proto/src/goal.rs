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
    /// Claude `/goal` only: its checker (a small model run after each turn)
    /// and the latest reason it gave. Codex goals have none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evaluation: Option<GoalEvaluation>,
}

/// Claude's goal checker state, as Claude reports it. The objective is a
/// completion condition; Claude has no token budget and no pause.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalEvaluation {
    /// Turns the checker has evaluated since the goal was set.
    #[serde(default)]
    pub checks: u32,
    /// The checker's most recent "not yet met" reason (bounded).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_reason: Option<String>,
}

/// A provider-native scheduled prompt observed in a Claude session
/// (`CronCreate`, or the dynamic `/loop` wakeup from `ScheduleWakeup`).
/// LUNA never schedules or fires these: Claude owns them, and they run only
/// while that session's Claude process is open.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeSchedule {
    /// Claude's own job id (`CronCreate`), or `wakeup` for the dynamic loop.
    pub id: String,
    pub kind: NativeScheduleKind,
    /// Claude's human-readable schedule ("every 5 minutes", "in 10m").
    pub schedule: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cron: Option<String>,
    /// The prompt Claude enqueues when it fires (bounded).
    #[serde(default)]
    pub prompt: String,
    #[serde(default)]
    pub recurring: bool,
    /// Claude persisted it to `.claude/scheduled_tasks.json`; otherwise it is
    /// session-only and dies with the Claude process.
    #[serde(default)]
    pub durable: bool,
    pub created_at_ms: i64,
    /// A wakeup's exact next fire time (Claude's `scheduledFor`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_fire_at_ms: Option<i64>,
    /// Claude expires recurring jobs 7 days after creation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_ms: Option<i64>,
    pub state: NativeScheduleState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NativeScheduleKind {
    Cron,
    Wakeup,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NativeScheduleState {
    /// Claude reported it scheduled and has not reported it gone.
    Scheduled,
    /// `CronDelete` or `ScheduleWakeup {stop:true}` cancelled it.
    Cancelled,
    /// Claude's own `CronList` no longer lists it (fired one-shot, expired).
    Ended,
}

impl NativeSchedule {
    /// Claude's per-session maximum (`CronCreate` refuses more than 50).
    pub const LIMIT: usize = 50;

    pub fn is_live(&self, now_ms: i64) -> bool {
        self.state == NativeScheduleState::Scheduled
            && self.expires_at_ms.is_none_or(|at| at > now_ms)
            && self
                .next_fire_at_ms
                .is_none_or(|at| self.recurring || at > now_ms)
    }
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

/// "Send as goal": Codex's own start sequence (`thread/goal/clear`, then
/// `thread/goal/set {objective, status: active}`) on a LOADED runtime, which
/// starts the goal turn itself. The objective is also the user's transcript
/// message; Codex history carries it only as hidden goal context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalStartRequest {
    pub objective: String,
    /// `None` starts the goal without a token budget.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_budget: Option<GoalBudget>,
}

impl GoalStartRequest {
    pub fn validate(&self) -> Result<(), &'static str> {
        GoalOperation::Set {
            objective: Some(self.objective.clone()),
            status: Some(GoalStatus::Active),
            token_budget: self.token_budget.clone(),
        }
        .validate()
    }
}

/// Durable transcript presentation for goals. Codex history has no user item
/// for a goal start and no item for the model's `update_goal`, so LUNA keeps
/// the marks beside its own transcript entries (bounded, newest last).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalMarks {
    /// User entries that were sent as goals ("Sent as goal").
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sent: Vec<String>,
    /// Assistant entries whose provider turn achieved a goal.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub achieved: Vec<GoalAchievedMark>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalAchievedMark {
    pub entry_id: String,
    /// Codex's own accounting (`timeUsedSeconds`) at completion.
    pub time_used_seconds: i64,
}

impl GoalMarks {
    pub const LIMIT: usize = 64;

    pub fn is_empty(&self) -> bool {
        self.sent.is_empty() && self.achieved.is_empty()
    }

    pub fn is_sent(&self, entry_id: &str) -> bool {
        self.sent.iter().any(|id| id == entry_id)
    }

    pub fn achieved(&self, entry_id: &str) -> Option<&GoalAchievedMark> {
        self.achieved.iter().find(|mark| mark.entry_id == entry_id)
    }

    pub fn mark_sent(&mut self, entry_id: &str) {
        if !self.is_sent(entry_id) {
            self.sent.push(entry_id.to_owned());
            let excess = self.sent.len().saturating_sub(Self::LIMIT);
            self.sent.drain(..excess);
        }
    }

    pub fn mark_achieved(&mut self, entry_id: &str, time_used_seconds: i64) {
        self.achieved.retain(|mark| mark.entry_id != entry_id);
        self.achieved.push(GoalAchievedMark {
            entry_id: entry_id.to_owned(),
            time_used_seconds,
        });
        let excess = self.achieved.len().saturating_sub(Self::LIMIT);
        self.achieved.drain(..excess);
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
