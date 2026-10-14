//! RM-04 test support: the provider boundary for the remote task-command
//! tests (`remote_task_commands.rs`, and the mobile outbox campaign in
//! `remote_access.rs`). A recording harness registered as the engine's
//! default provider: it records each test prompt's run (prompt and
//! checkout) and, when holding, keeps that turn running until the test
//! finishes that prompt or the engine interrupts it, so "a turn is running"
//! is a state the test controls rather than a race. It resumes the session
//! the engine names, so a task's later turns stay on its one binding.
//! Prompts without the [`PROMPT`] prefix (title runs) finish at once and are
//! not recorded.
#![allow(dead_code)]

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use luna_engine::HarnessRegistry;
use luna_harness::{Harness, HarnessError, RunControls};
use luna_proto::{
    AgentEvent, DoneStatus, HarnessId, Model, ReasoningLevel, RunRequest, SteeringMode,
};

/// Prompts the tests send start with this.
pub const PROMPT: &str = "rm04: ";
/// Finishing this releases every held turn (shutdown).
pub const ALL: &str = "*";

/// One provider run of a test prompt, as the provider saw it.
#[derive(Debug, Clone)]
pub struct Seen {
    pub prompt: String,
    pub cwd: String,
}

/// What a test reads and controls at the provider boundary.
#[derive(Clone)]
pub struct Provider {
    seen: Arc<Mutex<Vec<Seen>>>,
    finished: tokio::sync::watch::Sender<HashSet<String>>,
}

impl Provider {
    pub fn new() -> Self {
        Self {
            seen: Arc::new(Mutex::new(Vec::new())),
            finished: tokio::sync::watch::Sender::new(HashSet::new()),
        }
    }

    /// A registry whose only (and default, `mock`) provider is this one.
    /// With `hold`, each test prompt's turn runs until [`Provider::finish`].
    pub fn registry(&self, hold: bool) -> Arc<HarnessRegistry> {
        let registry = HarnessRegistry::new();
        registry.register(Arc::new(RecordingHarness {
            hold,
            seen: self.seen.clone(),
            finished: self.finished.clone(),
        }));
        Arc::new(registry)
    }

    /// End the held turn of `prompt`, now or whenever it starts.
    pub fn finish(&self, prompt: &str) {
        self.finished.send_modify(|set| {
            set.insert(prompt.to_owned());
        });
    }

    pub fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }

    pub fn runs_of(&self, prompt: &str) -> Vec<Seen> {
        self.seen()
            .into_iter()
            .filter(|seen| seen.prompt == prompt)
            .collect()
    }

    /// The first run of `prompt`, once the provider received it.
    pub async fn ran(&self, prompt: &str) -> Seen {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(seen) = self.runs_of(prompt).into_iter().next() {
                return seen;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the provider never received {prompt:?}; it saw {:?}",
                self.seen()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

struct RecordingHarness {
    hold: bool,
    seen: Arc<Mutex<Vec<Seen>>>,
    finished: tokio::sync::watch::Sender<HashSet<String>>,
}

#[async_trait]
impl Harness for RecordingHarness {
    fn id(&self) -> HarnessId {
        HarnessId::Mock
    }
    fn display_name(&self) -> &str {
        "RM-04 recording provider"
    }
    fn supports_steering(&self) -> bool {
        false
    }
    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::TurnBoundary
    }
    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        &[ReasoningLevel::Medium]
    }
    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        Ok(vec![])
    }
    async fn run(
        &self,
        request: RunRequest,
        controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        // A follow-up turn resumes the task's session, as a real provider's
        // does: the engine names it from the task's binding, and a new id on
        // a bound task is a silent rebind it refuses (the turn errors out).
        let session = request
            .resume
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let started = futures::stream::iter(vec![Ok(AgentEvent::SessionStarted {
            harness: HarnessId::Mock,
            model: "rm04-mock".into(),
            tools: vec![],
            cwd: request.cwd.clone(),
            session_id: session.clone(),
            assistant_message_id: uuid::Uuid::new_v4().to_string(),
        })]);
        let test_prompt = request.prompt.starts_with(PROMPT);
        if test_prompt {
            self.seen.lock().unwrap().push(Seen {
                prompt: request.prompt.clone(),
                cwd: request.cwd.clone(),
            });
        }
        let mut finished = (test_prompt && self.hold).then(|| self.finished.subscribe());
        let prompt = request.prompt;
        let interrupt = controls.interrupt.clone();
        let done = futures::stream::once(async move {
            let status = match finished.as_mut() {
                None => DoneStatus::Completed,
                Some(finished) => tokio::select! {
                    _ = finished.wait_for(|set| set.contains(&prompt) || set.contains(ALL)) => {
                        DoneStatus::Completed
                    }
                    _ = interrupt.cancelled() => DoneStatus::Interrupted,
                },
            };
            Ok(AgentEvent::Done {
                status,
                result: None,
                error: None,
                session_id: Some(session),
            })
        });
        Ok(started.chain(done).boxed())
    }
}
