//! Engine lifecycle through its real RPC bootstrap and catalog discovery path.
//! Failure modes: a retired engine probes provider accounts after shutdown,
//! or a dropped engine remains retained by its periodic refresh task.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use async_trait::async_trait;
use futures::stream::BoxStream;
use luna_engine::{EngineCore, HarnessRegistry};
use luna_harness::{Harness, HarnessError, RunControls};
use luna_proto::{AgentEvent, HarnessId, Model, ReasoningLevel, RunRequest, SteeringMode};

struct Catalog(Arc<AtomicUsize>);

#[async_trait]
impl Harness for Catalog {
    fn id(&self) -> HarnessId {
        HarnessId::Opencode
    }
    fn display_name(&self) -> &str {
        "Synthetic catalog"
    }
    fn supports_steering(&self) -> bool {
        false
    }
    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::TurnBoundary
    }
    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        &[]
    }
    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(vec![])
    }
    async fn run(
        &self,
        _: RunRequest,
        _: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        unreachable!("catalog discovery must not run a prompt")
    }
}

#[tokio::test]
async fn retired_engines_stop_background_catalog_discovery() {
    // Keep the runtime alive beyond the production bootstrap delay. Tokio test
    // teardown would otherwise hide the leaked provider probe.
    for explicit_shutdown in [true, false] {
        let root = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let registry = Arc::new(HarnessRegistry::new());
        registry.register(Arc::new(Catalog(calls.clone())));
        let weak_registry = Arc::downgrade(&registry);
        let core = EngineCore::assemble(root.path(), registry, HarnessId::Opencode, None).unwrap();
        let rpc = core.rpc_service();
        if explicit_shutdown {
            assert!(core.shutdown().await);
        }
        drop(rpc);
        drop(core);
        tokio::time::sleep(Duration::from_millis(2300)).await;
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "retired engine must not contact a provider"
        );
        if explicit_shutdown {
            assert!(
                weak_registry.upgrade().is_none(),
                "catalog worker must release the retired registry"
            );
        }
    }
}
