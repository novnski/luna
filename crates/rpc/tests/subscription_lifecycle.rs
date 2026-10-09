//! Transport acceptance cases, written before the lifecycle fix:
//! - Dropping a quiet view releases its server stream without another event.
//! - A stalled view cannot indefinitely block other requests on the socket.
//! - Completed streams still close and preserve ordered data.
//! - Requests after inbound EOF fail even if the outbound half stays open.
//! - Disconnecting the client releases every live server stream.
//! - Canceling the server connection releases child request tasks as well.
//! - GPUI callers can subscribe without a current Tokio runtime; the client
//!   must use its owning runtime for background cleanup.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use luna_rpc::{RpcClient, RpcError, RpcReply, RpcService};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::{Notify, mpsc};

struct Lease(Arc<Notify>);

impl Drop for Lease {
    fn drop(&mut self) {
        self.0.notify_one();
    }
}

struct Service {
    released: Arc<Notify>,
}

#[async_trait]
impl RpcService for Service {
    async fn handle(&self, method: &str, params: Value) -> Result<RpcReply, RpcError> {
        match method {
            "Echo" => Ok(RpcReply::Value(params)),
            "Watch" | "Flood" => {
                let flood = method == "Flood";
                let lease = Lease(self.released.clone());
                Ok(RpcReply::Stream(
                    futures::stream::unfold((lease, 0), move |(lease, n)| async move {
                        if !flood && n > 0 {
                            futures::future::pending::<()>().await;
                        }
                        Some((json!(n), (lease, n + 1)))
                    })
                    .boxed(),
                ))
            }
            "Count" => Ok(RpcReply::Stream(
                futures::stream::iter((0..100).map(|n| json!(n))).boxed(),
            )),
            _ => Err(RpcError::UnknownMethod(method.into())),
        }
    }
}

struct Socket {
    listener: tokio::task::JoinHandle<()>,
}

impl Drop for Socket {
    fn drop(&mut self) {
        self.listener.abort();
    }
}

async fn connect() -> (Socket, RpcClient, Arc<Notify>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let released = Arc::new(Notify::new());
    let service = Arc::new(Service {
        released: released.clone(),
    });
    let listener = tokio::spawn(luna_rpc::serve_ws_listener(listener, service));
    let client = luna_rpc::connect_ws(&format!("ws://{address}"))
        .await
        .unwrap();
    (Socket { listener }, client, released)
}

#[tokio::test]
async fn closing_a_quiet_view_releases_its_server_lease() {
    let (_socket, client, released) = connect().await;
    let mut stream = client.subscribe("Watch", Value::Null).await.unwrap();
    assert_eq!(stream.recv().await, Some(json!(0)));
    drop(stream);
    tokio::time::timeout(Duration::from_secs(2), released.notified())
        .await
        .expect("idle server subscription survived its view");
    assert_eq!(client.call("Echo", json!("alive")).await.unwrap(), "alive");
}

#[tokio::test]
async fn stalled_view_does_not_wedge_other_socket_requests() {
    let (_socket, client, released) = connect().await;
    let mut stream = client.subscribe("Flood", Value::Null).await.unwrap();
    assert_eq!(stream.recv().await, Some(json!(0)));
    // The unconsumed stream fills while the independent Echo response is queued.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let reply = tokio::time::timeout(Duration::from_secs(3), client.call("Echo", json!(42)))
        .await
        .expect("one stalled stream blocked every RPC reply")
        .unwrap();
    assert_eq!(reply, json!(42));
    tokio::time::timeout(Duration::from_secs(2), released.notified())
        .await
        .expect("stalled stream was not canceled at the server");
    let mut previous = 0;
    while let Some(item) = stream.recv().await {
        previous += 1;
        assert_eq!(item, json!(previous));
    }
    assert!(previous <= 256, "stream memory must stay bounded");
}

#[tokio::test]
async fn finite_stream_preserves_order_and_closes() {
    let (_socket, client, _) = connect().await;
    let mut stream = client.subscribe("Count", Value::Null).await.unwrap();
    for n in 0..100 {
        assert_eq!(stream.recv().await, Some(json!(n)));
    }
    assert_eq!(stream.recv().await, None);
}

#[tokio::test]
async fn dropping_client_releases_a_live_watch() {
    let (_socket, client, released) = connect().await;
    let mut stream = client.subscribe("Watch", Value::Null).await.unwrap();
    assert_eq!(stream.recv().await, Some(json!(0)));
    drop(client);
    tokio::time::timeout(Duration::from_secs(2), released.notified())
        .await
        .expect("disconnected client retained a server watcher");
    assert_eq!(stream.recv().await, None);
}

#[tokio::test]
async fn inbound_eof_rejects_new_requests_on_a_half_open_transport() {
    let (out, mut requests) = mpsc::channel(8);
    let (responses, inbound) = mpsc::channel(8);
    let client = Arc::new(RpcClient::new(out, inbound));
    let pending_client = client.clone();
    let pending = tokio::spawn(async move { pending_client.call("Echo", Value::Null).await });
    requests.recv().await.unwrap();
    drop(responses);
    assert!(matches!(pending.await.unwrap(), Err(RpcError::Closed)));
    let result = tokio::time::timeout(Duration::from_secs(1), client.call("Echo", Value::Null))
        .await
        .expect("new request was registered after the reader exited");
    assert!(matches!(result, Err(RpcError::Closed)));
}

#[tokio::test]
async fn canceling_server_connection_releases_child_stream_tasks() {
    let (requests, inbound) = mpsc::channel(8);
    let (out, mut responses) = mpsc::channel(8);
    let released = Arc::new(Notify::new());
    let service = Arc::new(Service {
        released: released.clone(),
    });
    let server = tokio::spawn(luna_rpc::serve_connection(service, out, inbound));
    requests
        .send(json!({"id": 1, "method": "Watch", "params": null}).to_string())
        .await
        .unwrap();
    responses.recv().await.unwrap();
    server.abort();
    let _ = server.await;
    tokio::time::timeout(Duration::from_secs(2), released.notified())
        .await
        .expect("aborted connection left a child stream task running");
}

#[test]
fn subscriptions_from_a_foreign_executor_use_the_clients_runtime() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let (_socket, client, released) = runtime.block_on(connect());
    assert!(tokio::runtime::Handle::try_current().is_err());
    // Each drop must release its own server lease: `Notify` holds a single
    // permit, so one wait after both drops could not tell them apart.
    let lease_released = || {
        runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(2), released.notified())
                .await
                .expect("dropped subscription kept its server lease");
        })
    };
    futures::executor::block_on(async {
        let mut plain = client.subscribe("Watch", Value::Null).await.unwrap();
        assert_eq!(plain.recv().await, Some(json!(0)));
        drop(plain);
    });
    lease_released();
    futures::executor::block_on(async {
        let mut checked = client
            .subscribe_checked("Watch", Value::Null)
            .await
            .unwrap();
        assert_eq!(checked.recv().await, Some(json!(0)));
        drop(checked);
        assert_eq!(client.call("Echo", json!("alive")).await.unwrap(), "alive");
    });
    lease_released();
}
