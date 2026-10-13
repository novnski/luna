//! CORE-05 E03/E07 client parity against one real engine.
//!
//! The app's command boundary (the Rust SDK `Client::execute` the desktop
//! task client sends through, with the app's actor), the real `luna`
//! executable and the independent TypeScript SDK drive the same allowed and
//! denied rename. Each receipt is compared with the engine's durable command
//! event, read by a raw RPC client that bypasses both the harness and the SDK.
//!
//! Failures this pins, written before the SDK:
//! - a CLI or SDK client is allowed what another is refused (receipts differ);
//! - a target is inferred: a task outside the named project is changed;
//! - a lost reply is resent blindly or under a new id (a second send or event);
//! - JSON that is not a receipt decodes as one;
//! - a watch skips an event, or its engine stream outlives disposal;
//! - an engine speaking another API version is driven anyway.
//!
//! The harness only injects faults around the engine: it never produces a
//! receipt, and every command it forwards is executed by the engine.

use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use luna_engine::{
    AgentAccountsConfig, Auth, AuthConfig, EngineCore, EngineProfile, HarnessId, HarnessRegistry,
};
use luna_proto::{CommandReasonCode, CommandReceiptStatus, TaskCommandSnapshot};
use luna_rpc::{RpcClient, RpcError, RpcReply, RpcService, methods};
use luna_sdk::{Actor, Client, Fencing, TaskCommand, TaskCommandUpdate, TaskIntent};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Notify;

const PROJECT: &str = "05050505-0505-4505-8505-050505050505";
const OTHER_PROJECT: &str = "06060606-0606-4606-8606-060606060606";
const TASK: &str = "07070707-0707-4707-8707-070707070707";

#[derive(Default)]
struct Faults {
    /// Execute, then lose the reply (once per command id).
    lose_reply: Mutex<HashSet<String>>,
    /// Hide these commands' receipts from every watch stream.
    hide_receipt: Mutex<HashSet<String>>,
    /// Answer these receipt reads with JSON that is not a receipt.
    corrupt_receipt: Mutex<HashSet<String>>,
    /// Report this API version from DescribeApi.
    api_version: Mutex<Option<u16>>,
    /// Every `(method, commandId)` the engine was asked.
    calls: Mutex<Vec<(String, Option<String>)>>,
}

#[derive(Default)]
struct Watches {
    live: AtomicUsize,
    released: Notify,
}

impl Watches {
    fn live(&self) -> usize {
        self.live.load(Ordering::SeqCst)
    }

    /// Wait until every engine watch stream has been dropped.
    async fn drained(&self, within: Duration) {
        tokio::time::timeout(within, async {
            while self.live() > 0 {
                self.released.notified().await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "{} engine watch stream(s) outlived their client",
                self.live()
            )
        });
    }
}

/// Held by one engine `WatchTaskCommands` stream; dropped with it.
struct WatchLease(Arc<Watches>);

impl Drop for WatchLease {
    fn drop(&mut self) {
        self.0.live.fetch_sub(1, Ordering::SeqCst);
        self.0.released.notify_one();
    }
}

struct Harness {
    engine: Arc<dyn RpcService>,
    faults: Arc<Faults>,
    watches: Arc<Watches>,
}

#[async_trait::async_trait]
impl RpcService for Harness {
    async fn handle(&self, method: &str, params: Value) -> Result<RpcReply, RpcError> {
        let command_id = params
            .get("commandId")
            .and_then(Value::as_str)
            .map(str::to_owned);
        self.faults
            .calls
            .lock()
            .unwrap()
            .push((method.to_owned(), command_id.clone()));
        if method == methods::GET_TASK_COMMAND_RECEIPT
            && command_id
                .as_ref()
                .is_some_and(|id| self.faults.corrupt_receipt.lock().unwrap().contains(id))
        {
            // Well-formed JSON, but not a receipt: required fields are missing.
            return Ok(RpcReply::Value(
                json!({ "commandId": command_id, "status": "applied" }),
            ));
        }
        let reply = self.engine.handle(method, params).await?;
        if method == methods::EXECUTE_TASK_COMMAND
            && command_id
                .as_ref()
                .is_some_and(|id| self.faults.lose_reply.lock().unwrap().remove(id))
        {
            return Err(RpcError::Transport("reply lost by the E07 harness".into()));
        }
        match reply {
            RpcReply::Value(mut value) if method == methods::DESCRIBE_API => {
                if let Some(version) = *self.faults.api_version.lock().unwrap() {
                    value["apiVersion"] = version.into();
                }
                Ok(RpcReply::Value(value))
            }
            RpcReply::Stream(stream) if method == methods::WATCH_TASK_COMMANDS => {
                self.watches.live.fetch_add(1, Ordering::SeqCst);
                let lease = WatchLease(self.watches.clone());
                let faults = self.faults.clone();
                Ok(RpcReply::Stream(
                    stream
                        .filter(move |item| {
                            let _lease = &lease;
                            let hidden = item
                                .pointer("/receipt/commandId")
                                .and_then(Value::as_str)
                                .is_some_and(|id| faults.hide_receipt.lock().unwrap().contains(id));
                            futures::future::ready(!hidden)
                        })
                        .boxed(),
                ))
            }
            reply => Ok(reply),
        }
    }
}

struct Fixture {
    root: TempDir,
    _core: EngineCore,
    machine: String,
    faults: Arc<Faults>,
    watches: Arc<Watches>,
    port: u16,
    server: tokio::task::JoinHandle<()>,
    /// Raw RPC straight to the engine: bypasses the harness and the SDK.
    oracle: RpcClient,
}

fn synthetic_accounts(root: &Path) -> AgentAccountsConfig {
    AgentAccountsConfig {
        data_dir: root.join("provider-state"),
        claude_config_dir: root.join("synthetic-claude"),
        claude_config_file: root.join("synthetic-claude.json"),
        codex_home: root.join("synthetic-codex"),
    }
}

async fn fixture() -> Fixture {
    let root = TempDir::new().unwrap();
    let accounts = synthetic_accounts(root.path());
    accounts.disable_claude_keychain_fallback().unwrap();
    let core = EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        EngineProfile::development(&root.path().join("data"), "core05-org", "core05-user"),
        Arc::new(HarnessRegistry::new()),
        HarnessId::Mock,
        None,
        accounts,
    )
    .unwrap();
    let mut auth = AuthConfig::new("http://127.0.0.1:1", root.path().join("data"));
    auth.use_in_memory_credentials_for_tests();
    core.set_auth(Auth::new(auth));
    let machine = core.device_id.clone();
    let engine: Arc<dyn RpcService> = core.rpc_service();
    let oracle = luna_rpc::memory_client(engine.clone());
    for (space, name) in [(PROJECT, "project"), (OTHER_PROJECT, "other-project")] {
        let path = root.path().join(name);
        std::fs::create_dir_all(&path).unwrap();
        oracle
            .call(
                methods::MUTATE,
                json!({ "op": "createSpace", "spaceId": space, "deviceId": machine, "path": path }),
            )
            .await
            .unwrap();
    }
    oracle
        .call(
            methods::MUTATE,
            json!({
                "op": "createChat", "chatId": TASK, "spaceId": PROJECT,
                "config": {
                    "harness": "mock", "model": null, "reasoning": null,
                    "sandbox": "workspace-write"
                }
            }),
        )
        .await
        .unwrap();
    let faults = Arc::new(Faults::default());
    let watches = Arc::new(Watches::default());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(luna_rpc::serve_ws_listener(
        listener,
        Arc::new(Harness {
            engine,
            faults: faults.clone(),
            watches: watches.clone(),
        }),
    ));
    Fixture {
        root,
        _core: core,
        machine,
        faults,
        watches,
        port,
        server,
        oracle,
    }
}

impl Fixture {
    /// A Rust SDK client through the harness, as any IPC client connects.
    async fn sdk(&self) -> Client<RpcClient> {
        Client::new(
            luna_rpc::connect_ws(&format!("ws://127.0.0.1:{}", self.port))
                .await
                .unwrap(),
        )
    }

    /// The app's command identity: the device principal and client.
    fn app(&self) -> Actor {
        Actor::new(self.machine.clone(), self.machine.clone())
    }

    async fn events(&self) -> Vec<Value> {
        let read = self
            .oracle
            .call(
                methods::GET_TASK_COMMAND_SNAPSHOT,
                json!({ "taskId": TASK, "afterTaskSequence": 0, "limit": 100 }),
            )
            .await
            .unwrap();
        read["events"].as_array().cloned().unwrap_or_default()
    }

    /// The single durable event receipt of a command.
    async fn durable(&self, command_id: &str) -> Value {
        let events = self.events().await;
        let matching: Vec<&Value> = events
            .iter()
            .filter(|event| event["commandId"] == command_id)
            .collect();
        assert_eq!(
            matching.len(),
            1,
            "command {command_id} has {} durable events",
            matching.len()
        );
        matching[0]["receipt"].clone()
    }

    async fn revision(&self) -> u64 {
        let read = self
            .oracle
            .call(
                methods::GET_TASK_COMMAND_SNAPSHOT,
                json!({ "taskId": TASK }),
            )
            .await
            .unwrap();
        read["snapshot"]["taskRevision"].as_u64().unwrap()
    }

    async fn title(&self) -> String {
        let mut chats = self
            .oracle
            .subscribe_checked(methods::WATCH_CHATS, json!({}))
            .await
            .unwrap();
        let chats = chats.recv().await.unwrap();
        chats
            .as_array()
            .unwrap()
            .iter()
            .find(|chat| chat["id"] == TASK)
            .unwrap()["title"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    }

    fn calls(&self, method: &str, command_id: &str) -> usize {
        self.faults
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(called, id)| called == method && id.as_deref() == Some(command_id))
            .count()
    }

    async fn cli(&self, args: &[&str]) -> (i32, Value) {
        let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_luna"))
            .args(args)
            .env("LUNA_DATA_DIR", self.root.path().join("child-data"))
            .env("CODEX_HOME", self.root.path().join("synthetic-codex"))
            .env(
                "CLAUDE_CONFIG_DIR",
                self.root.path().join("synthetic-claude"),
            )
            .env("LUNA_WORKOS_CLIENT_ID", "")
            .env("LUNA_EDGE_URL", "http://127.0.0.1:1")
            .output()
            .await
            .unwrap();
        let body = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "CLI JSON parse failed ({error}), stdout={}, stderr={}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        });
        (output.status.code().unwrap(), body)
    }

    /// `luna task --json rename` for the fixture task, plus `extra`.
    async fn cli_rename(&self, extra: &[&str]) -> (i32, Value) {
        let port = self.port.to_string();
        let mut args: Vec<&str> = vec![
            "task",
            "--json",
            "--port",
            &port,
            "rename",
            "--machine",
            &self.machine,
            "--environment",
            &self.machine,
            "--project",
            PROJECT,
            "--task",
            TASK,
        ];
        args.extend_from_slice(extra);
        self.cli(&args).await
    }
}

fn json<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap()
}

fn rename(actor: &Actor, command_id: &str, title: &str) -> TaskCommand<TaskIntent> {
    TaskCommand::new(
        actor.clone(),
        TASK,
        TaskIntent::Rename {
            title: title.into(),
        },
    )
    .with_command_id(command_id)
}

#[tokio::test]
async fn app_cli_and_rust_sdk_share_receipts_refusals_recovery_and_watch_cleanup() {
    let f = fixture().await;
    let port = f.port.to_string();
    let mut sdk = f.sdk().await;
    let app = f.app();

    // Discovery: this engine serves every SDK operation at the SDK's version.
    let api = sdk
        .negotiate()
        .await
        .unwrap()
        .cloned()
        .expect("API description");
    assert_eq!(api.api_version, luna_sdk::API_VERSION);
    for (_, method) in luna_sdk::ops::OPERATIONS {
        let spec = luna_rpc::operations::spec(method).unwrap();
        assert_eq!(
            api.operation(spec.id).map(|served| served.version),
            Some(spec.version),
            "{method}"
        );
    }
    let (code, body) = f.cli(&["inspect", "--json", "--port", &port, "api"]).await;
    assert_eq!(code, 0, "{body}");
    assert_eq!(body["data"]["engine"]["apiVersion"], luna_sdk::API_VERSION);
    assert!(
        body["data"]["sdkOperations"]
            .as_array()
            .unwrap()
            .iter()
            .all(|op| op["served"] == true),
        "{body}"
    );

    // Allowed: the same rename through the app's boundary and the CLI.
    let start = f.revision().await;
    let sdk_allowed = sdk
        .execute(&rename(&app, "e07-sdk-allowed", "E07 parity"))
        .await
        .unwrap();
    assert_eq!(sdk_allowed.status, CommandReceiptStatus::Applied);
    assert_eq!(json(&sdk_allowed), f.durable("e07-sdk-allowed").await);
    assert_eq!(f.title().await, "E07 parity");
    // The CLI trims a title as the app's rename dialog does.
    let (code, body) = f
        .cli_rename(&[
            "--title",
            "  E07 parity  ",
            "--command-id",
            "e07-cli-allowed",
        ])
        .await;
    assert_eq!(code, 0, "{body}");
    let cli_allowed = &body["data"]["receipt"];
    assert_eq!(cli_allowed, &f.durable("e07-cli-allowed").await);
    assert_eq!(cli_allowed["status"], "applied");
    assert_eq!(cli_allowed["intent"], "rename");
    assert_eq!(
        cli_allowed["payloadHash"],
        sdk_allowed.payload_hash.as_str()
    );
    assert_eq!(cli_allowed["actorId"], f.machine.as_str());
    assert_eq!(cli_allowed["clientId"], "luna-cli");
    assert_eq!(f.revision().await, start + 2);

    // Denied: a stale revision fence is refused for both, as a durable receipt.
    let current = sdk
        .task_command_snapshot(TASK, None, None)
        .await
        .unwrap()
        .snapshot;
    let stale = TaskCommandSnapshot {
        task_revision: 0,
        ..current
    };
    let sdk_denied = sdk
        .execute(&rename(&app, "e07-sdk-denied", "Stale").with_fencing(Fencing::Reviewed(stale)))
        .await
        .unwrap();
    assert_eq!(sdk_denied.status, CommandReceiptStatus::Conflict);
    assert_eq!(
        sdk_denied.reason.as_ref().map(|reason| reason.code),
        Some(CommandReasonCode::StaleTaskRevision)
    );
    assert_eq!(json(&sdk_denied), f.durable("e07-sdk-denied").await);
    let (code, body) = f
        .cli_rename(&[
            "--title",
            "Stale",
            "--command-id",
            "e07-cli-denied",
            "--expected-revision",
            "0",
        ])
        .await;
    assert_eq!(code, 10, "{body}");
    assert_eq!(body["error"]["kind"], "commandRefused");
    let cli_denied = &body["data"]["receipt"];
    assert_eq!(cli_denied, &f.durable("e07-cli-denied").await);
    assert_eq!(cli_denied["status"], "conflict");
    assert_eq!(cli_denied["reason"]["code"], "staleTaskRevision");
    assert_eq!(cli_denied["payloadHash"], sdk_denied.payload_hash.as_str());
    assert_eq!(f.revision().await, start + 2, "a refusal changed the task");
    assert_eq!(f.title().await, "E07 parity");

    // The target is exact: the task outside the named project is refused
    // before any command is sent.
    let events = f.events().await.len();
    let (code, body) = f
        .cli(&[
            "task",
            "--json",
            "--port",
            &port,
            "rename",
            "--machine",
            &f.machine,
            "--environment",
            &f.machine,
            "--project",
            OTHER_PROJECT,
            "--task",
            TASK,
            "--title",
            "Elsewhere",
        ])
        .await;
    assert_eq!(code, 5, "{body}");
    assert_eq!(body["error"]["kind"], "notFound");
    assert_eq!(
        f.events().await.len(),
        events,
        "a refused target sent a command"
    );

    // Unknown outcome: the engine executes, the reply is lost; each client
    // reads the receipt back by the same id instead of sending again.
    for (client, command_id) in [("sdk", "e07-sdk-lost"), ("cli", "e07-cli-lost")] {
        f.faults
            .lose_reply
            .lock()
            .unwrap()
            .insert(command_id.into());
        let before = f.revision().await;
        let receipt = if client == "sdk" {
            json(
                &sdk.execute(&rename(&app, command_id, "E07 recovered"))
                    .await
                    .unwrap(),
            )
        } else {
            let (code, body) = f
                .cli_rename(&["--title", "E07 recovered", "--command-id", command_id])
                .await;
            assert_eq!(code, 0, "{body}");
            body["data"]["receipt"].clone()
        };
        assert_eq!(receipt, f.durable(command_id).await, "{client}");
        assert_eq!(receipt["status"], "applied", "{client}");
        assert_eq!(f.revision().await, before + 1, "{client}");
        assert_eq!(
            f.calls(methods::EXECUTE_TASK_COMMAND, command_id),
            1,
            "{client} resent a command whose receipt it could read"
        );
        assert!(
            f.calls(methods::GET_TASK_COMMAND_RECEIPT, command_id) >= 1,
            "{client} never read the lost command back"
        );
    }

    // A reply that is not a receipt is never decoded as one.
    f.faults
        .corrupt_receipt
        .lock()
        .unwrap()
        .insert("e07-corrupt".into());
    let error = sdk
        .task_command_receipt("e07-corrupt", None)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), "invalidResponse", "{error}");
    let (code, body) = f
        .cli(&[
            "inspect",
            "--json",
            "--port",
            &port,
            "receipt",
            "--command-id",
            "e07-corrupt",
        ])
        .await;
    assert_eq!(code, 7, "{body}");
    assert_eq!(body["error"]["kind"], "malformedResponse");

    // Watch: a receipt hidden from the stream is repaired from the durable
    // page, in order, before the receipt that revealed the gap.
    let before = sdk
        .task_command_snapshot(TASK, None, None)
        .await
        .unwrap()
        .snapshot
        .task_sequence;
    let mut watch = sdk
        .watch_task_commands(TASK, Some(before), None)
        .await
        .unwrap();
    match watch.next().await {
        Some(Ok(TaskCommandUpdate::Snapshot { read })) => assert!(read.events.is_empty()),
        other => panic!("watch did not start with its snapshot: {other:?}"),
    }
    assert_eq!(f.watches.live(), 1);
    f.faults
        .hide_receipt
        .lock()
        .unwrap()
        .insert("e07-sdk-gap".into());
    let gap = sdk
        .execute(&rename(&app, "e07-sdk-gap", "E07 gap"))
        .await
        .unwrap();
    let after = sdk
        .execute(&rename(&app, "e07-sdk-after-gap", "E07 after gap"))
        .await
        .unwrap();
    let mut delivered = Vec::new();
    while delivered.len() < 2 {
        match tokio::time::timeout(Duration::from_secs(10), watch.next())
            .await
            .expect("watch stalled")
        {
            Some(Ok(TaskCommandUpdate::Event { receipt })) => delivered.push(receipt),
            Some(Ok(TaskCommandUpdate::Progress { receipt })) => {
                assert_eq!(receipt.status, CommandReceiptStatus::Pending, "{receipt:?}");
            }
            other => panic!("unexpected watch update {other:?}"),
        }
    }
    assert_eq!(delivered, vec![gap, after.clone()]);
    assert_eq!(json(&delivered[0]), f.durable("e07-sdk-gap").await);
    assert_eq!(json(&delivered[1]), f.durable("e07-sdk-after-gap").await);
    assert_eq!(watch.cursor(), Some(after.snapshot.task_sequence));
    // Disposal alone, with the client still connected, ends the engine stream.
    drop(watch);
    f.watches.drained(Duration::from_secs(5)).await;
    assert_eq!(sdk.engine_info().await.unwrap().device_id, f.machine);

    // The CLI watch replays the durable history and releases its stream.
    let (code, body) = f
        .cli(&[
            "inspect",
            "--json",
            "--port",
            &port,
            "watch-commands",
            "--machine",
            &f.machine,
            "--environment",
            &f.machine,
            "--project",
            PROJECT,
            "--task",
            TASK,
            "--after-task-sequence",
            "0",
            "--count",
            "1",
        ])
        .await;
    assert_eq!(code, 0, "{body}");
    let update = &body["data"]["updates"][0];
    assert_eq!(update["kind"], "snapshot", "{body}");
    assert_eq!(update["read"]["events"], Value::Array(f.events().await));
    f.watches.drained(Duration::from_secs(5)).await;

    // Another API version: both clients refuse before sending anything.
    *f.faults.api_version.lock().unwrap() = Some(luna_sdk::API_VERSION + 1);
    let events = f.events().await.len();
    let (code, body) = f.cli_rename(&["--title", "Newer API"]).await;
    assert_eq!(code, 12, "{body}");
    assert_eq!(body["error"]["kind"], "incompatible");
    let mut newer = f.sdk().await;
    assert_eq!(newer.negotiate().await.unwrap_err().kind(), "incompatible");
    assert_eq!(f.events().await.len(), events);

    f.server.abort();
}

/// Read the driver's next JSON event line.
async fn next_event(
    lines: &mut tokio::io::Lines<tokio::io::BufReader<tokio::process::ChildStdout>>,
) -> Value {
    loop {
        let line = tokio::time::timeout(Duration::from_secs(60), lines.next_line())
            .await
            .expect("TypeScript driver stalled")
            .unwrap()
            .expect("TypeScript driver exited early");
        if let Ok(event) = serde_json::from_str::<Value>(&line) {
            return event;
        }
    }
}

#[tokio::test]
#[ignore = "needs Node.js 23.6+ to run the TypeScript SDK; run with --ignored (LUNA_NODE overrides `node`)"]
async fn typescript_sdk_matches_the_rust_clients_against_one_engine() {
    let f = fixture().await;
    let sdk = f.sdk().await;
    let app = f.app();
    // Rust references for the same allowed and denied rename.
    let reference = sdk
        .execute(&rename(&app, "e07-ref-allowed", "E07 parity"))
        .await
        .unwrap();
    let current = sdk
        .task_command_snapshot(TASK, None, None)
        .await
        .unwrap()
        .snapshot;
    let reference_denied = sdk
        .execute(
            &rename(&app, "e07-ref-denied", "Stale").with_fencing(Fencing::Reviewed(
                TaskCommandSnapshot {
                    task_revision: 0,
                    ..current
                },
            )),
        )
        .await
        .unwrap();
    f.faults
        .lose_reply
        .lock()
        .unwrap()
        .insert("e07-ts-lost".into());
    f.faults
        .corrupt_receipt
        .lock()
        .unwrap()
        .insert("e07-ts-corrupt".into());
    f.faults
        .hide_receipt
        .lock()
        .unwrap()
        .insert("e07-ts-gap".into());

    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../sdk/typescript/test/parity.ts");
    let input = json!({
        "port": f.port, "taskId": TASK, "title": "E07 parity", "staleTitle": "Stale",
        "lostCommandId": "e07-ts-lost", "corruptCommandId": "e07-ts-corrupt",
        "gapCommandId": "e07-ts-gap",
    });
    let mut child =
        tokio::process::Command::new(std::env::var("LUNA_NODE").unwrap_or_else(|_| "node".into()))
            .arg(script)
            .arg(input.to_string())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("node starts");
    let mut lines = tokio::io::BufReader::new(child.stdout.take().unwrap()).lines();
    let mut stdin = child.stdin.take().unwrap();
    let disposed = next_event(&mut lines).await;
    assert_eq!(disposed["event"], "disposed", "{disposed}");
    // The TypeScript watch is disposed but its connection is still open.
    f.watches.drained(Duration::from_secs(5)).await;
    stdin.write_all(b"continue\n").await.unwrap();
    let result = next_event(&mut lines).await;
    assert_eq!(result["event"], "result", "{result}");
    assert!(child.wait().await.unwrap().success());

    assert_eq!(result["api"]["apiVersion"], luna_sdk::API_VERSION);
    let allowed = &result["allowed"];
    assert_eq!(allowed, &f.durable("e07-ts-allowed").await);
    assert_eq!(allowed["status"], "applied");
    assert_eq!(allowed["payloadHash"], reference.payload_hash.as_str());
    assert_eq!(allowed["actorId"], f.machine.as_str());
    assert_eq!(allowed["clientId"], "luna-sdk-ts");
    let denied = &result["denied"];
    assert_eq!(denied, &f.durable("e07-ts-denied").await);
    assert_eq!(denied["status"], "conflict");
    assert_eq!(denied["reason"]["code"], "staleTaskRevision");
    assert_eq!(
        denied["payloadHash"],
        reference_denied.payload_hash.as_str()
    );
    let recovered = &result["recovered"];
    assert_eq!(recovered, &f.durable("e07-ts-lost").await);
    assert_eq!(recovered["status"], "applied");
    assert_eq!(f.calls(methods::EXECUTE_TASK_COMMAND, "e07-ts-lost"), 1);
    assert!(f.calls(methods::GET_TASK_COMMAND_RECEIPT, "e07-ts-lost") >= 1);
    assert_eq!(result["invalid"]["kind"], "invalidResponse", "{result}");

    let updates = result["watch"]["updates"].as_array().unwrap();
    assert_eq!(updates[0]["kind"], "snapshot", "{result}");
    assert_eq!(updates[0]["read"]["events"], json!([]));
    let events: Vec<&Value> = updates.iter().filter(|u| u["kind"] == "event").collect();
    assert_eq!(events.len(), 2, "{result}");
    for (event, command_id) in events.iter().zip(["e07-ts-gap", "e07-ts-after-gap"]) {
        assert_eq!(
            event["receipt"],
            f.durable(command_id).await,
            "{command_id}"
        );
    }
    assert!(
        updates
            .iter()
            .filter(|u| u["kind"] == "progress")
            .all(|u| u["receipt"]["status"] == "pending"),
        "{result}"
    );
    assert_eq!(
        result["watch"]["cursor"],
        f.durable("e07-ts-after-gap").await["snapshot"]["taskSequence"]
    );
    assert_eq!(result["afterDispose"], f.machine.as_str());
    f.watches.drained(Duration::from_secs(5)).await;
    f.server.abort();
}
