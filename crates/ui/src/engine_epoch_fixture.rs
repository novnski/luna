//! Held-RPC integration support. Compiled only for GPUI tests, never in the app.
use crate::state::{AppState, EngineHandle};
use async_trait::async_trait;
use gpui::{Entity, TestAppContext};
use luna_proto::{EngineInfo, WorkspaceScope};
use luna_rpc::{RpcError, RpcReply, RpcService, memory_client};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{runtime::Runtime, sync::Notify};

#[derive(Clone)]
struct Reply {
    value: Result<Value, String>,
    release: Option<Arc<Notify>>,
}
pub(crate) struct HeldRpc {
    label: String,
    device_id: String,
    routes: Mutex<HashMap<String, Reply>>,
    events: Arc<Mutex<Vec<Value>>>,
}
struct RequestEnd {
    label: String,
    method: String,
    events: Arc<Mutex<Vec<Value>>>,
    replied: bool,
}
impl Drop for RequestEnd {
    fn drop(&mut self) {
        if !self.replied {
            self.events
                .lock()
                .unwrap()
                .push(json!({"phase":"canceled","engine":self.label,"method":self.method}));
        }
    }
}
impl HeldRpc {
    pub(crate) fn new(label: &str, device_id: &str) -> Arc<Self> {
        Arc::new(Self {
            label: label.into(),
            device_id: device_id.into(),
            routes: Mutex::new(HashMap::new()),
            events: Arc::new(Mutex::new(Vec::new())),
        })
    }
    pub(crate) fn reply(&self, method: &str, result: Result<Value, String>, held: bool) {
        self.routes.lock().unwrap().insert(
            method.into(),
            Reply {
                value: result,
                release: held.then(|| Arc::new(Notify::new())),
            },
        );
    }
    pub(crate) fn release(&self, method: &str) {
        if let Some(release) = self
            .routes
            .lock()
            .unwrap()
            .get(method)
            .and_then(|route| route.release.clone())
        {
            release.notify_one();
        }
    }
    pub(crate) fn calls(&self, method: &str) -> usize {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|event| event["phase"] == "request" && event["method"] == method)
            .count()
    }
    pub(crate) fn settled(&self, method: &str) -> usize {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|event| {
                (event["phase"] == "reply" || event["phase"] == "canceled")
                    && event["method"] == method
            })
            .count()
    }
    pub(crate) fn trace(&self) -> Vec<Value> {
        self.events.lock().unwrap().clone()
    }
}
#[async_trait]
impl RpcService for HeldRpc {
    async fn handle(&self, method: &str, params: Value) -> Result<RpcReply, RpcError> {
        self.events.lock().unwrap().push(json!({"phase":"request","engine":self.label,"device":self.device_id,"method":method,"params":params}));
        let mut end = RequestEnd {
            label: self.label.clone(),
            method: method.into(),
            events: self.events.clone(),
            replied: false,
        };
        let route = self.routes.lock().unwrap().get(method).cloned();
        let Some(route) = route else {
            return Err(RpcError::UnknownMethod(method.into()));
        };
        if let Some(release) = route.release {
            release.notified().await;
        }
        self.events.lock().unwrap().push(json!({"phase":"reply","engine":self.label,"device":self.device_id,"method":method,"ok":route.value.is_ok()}));
        end.replied = true;
        route.value.map(RpcReply::Value).map_err(RpcError::Failed)
    }
}
pub(crate) fn handle(runtime: &Runtime, rpc: Arc<HeldRpc>) -> EngineHandle {
    let _entered = runtime.enter();
    let info = EngineInfo {
        device_id: rpc.device_id.clone(),
        workspace_scope: WorkspaceScope::Local,
        capabilities: vec![],
    };
    let url = format!("memory://epoch-fixture/{}", rpc.label);
    EngineHandle::from_authenticated_remote(memory_client(rpc), info, url)
}
pub(crate) fn attach(cx: &mut TestAppContext, state: &Entity<AppState>, handle: EngineHandle) {
    state.update(cx, |state, cx| {
        state.attach_engine_for_epoch_fixture(handle, cx)
    });
}
pub(crate) fn pump_until(
    cx: &mut TestAppContext,
    runtime: &Runtime,
    mut done: impl FnMut(&mut TestAppContext) -> bool,
) {
    for _ in 0..250 {
        cx.run_until_parked();
        runtime.block_on(tokio::task::yield_now());
        if done(cx) {
            return;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(
        done(cx),
        "engine epoch integration did not reach its requested boundary"
    );
}
pub(crate) fn drain(cx: &mut TestAppContext, runtime: &Runtime) {
    // Do not advance virtual time: the next periodic poll must not hide an old
    // reply. Tests explicitly advance the clock when exercising the poller.
    for _ in 0..20 {
        cx.run_until_parked();
        runtime.block_on(tokio::task::yield_now());
    }
}
pub(crate) fn write_trace(name: &str, value: &Value) {
    let root = std::env::var_os("LUNA_EPOCH_QA_ARTIFACTS")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("luna-engine-epoch-qa"));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join(format!("{name}.json"));
    std::fs::write(&path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
    eprintln!("epoch trace: {}", path.display());
}
