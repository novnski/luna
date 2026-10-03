//! Client side: request/stream multiplexing over string frames + the WebSocket dialer.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use futures::{SinkExt, StreamExt};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message as WsMessage;

use crate::{ClientFrame, RpcError, ServerFrame};

/// Bound memory while allowing short scheduling pauses. A view that stops
/// consuming for a full second is disconnected so it cannot indefinitely block
/// unrelated replies on the multiplexed transport. Watchers resubscribe from
/// their authoritative snapshot when their receiver closes.
const STREAM_QUEUE_CAP: usize = 256;
const STREAM_SEND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

struct StreamWatch(tokio::task::JoinHandle<()>);

impl Drop for StreamWatch {
    fn drop(&mut self) {
        self.0.abort();
    }
}

enum Pending {
    Call(oneshot::Sender<Result<serde_json::Value, RpcError>>),
    Stream {
        items: mpsc::Sender<serde_json::Value>,
        _watch: StreamWatch,
    },
    CheckedStream {
        items: mpsc::Sender<serde_json::Value>,
        ready: Option<oneshot::Sender<Result<(), RpcError>>>,
        _watch: StreamWatch,
    },
}

struct Shared {
    pending: Mutex<HashMap<u64, Pending>>,
    closed: AtomicBool,
    // RPC futures are also polled by GPUI's executor, which has no current
    // Tokio reactor. Background cleanup belongs to the connection's runtime.
    runtime: tokio::runtime::Handle,
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<u64, Pending>> {
        self.pending.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn insert(&self, id: u64, entry: Pending) -> Result<(), RpcError> {
        let mut pending = self.lock();
        if self.closed.load(Ordering::Relaxed) {
            return Err(RpcError::Closed);
        }
        pending.insert(id, entry);
        Ok(())
    }

    fn close(&self) {
        let drained = {
            let mut pending = self.lock();
            // Serialize EOF with registration, including a still-open outbound half.
            self.closed.store(true, Ordering::Relaxed);
            std::mem::take(&mut *pending)
        };
        // Dropping senders closes calls, readiness waits and stream receivers.
        // Dropping monitors also releases their sender clones.
        drop(drained);
    }
}

/// Cancellation of a local wait must not retain its reply slot. Do not cancel
/// the server mutation: it may already have crossed a durable commit boundary.
struct PendingCall<'a> {
    id: u64,
    shared: &'a Shared,
}

impl Drop for PendingCall<'_> {
    fn drop(&mut self) {
        self.shared.lock().remove(&self.id);
    }
}

fn cancel_stream(shared: &Shared, out: &mpsc::Sender<String>, id: u64) {
    let Ok(frame) = serde_json::to_string(&ClientFrame {
        id,
        method: None,
        params: serde_json::Value::Null,
        cancel: true,
    }) else {
        return;
    };
    match out.try_send(frame) {
        Ok(()) | Err(mpsc::error::TrySendError::Closed(_)) => {}
        Err(mpsc::error::TrySendError::Full(frame)) => {
            let out = out.clone();
            shared.runtime.spawn(async move {
                let _ = out.send(frame).await;
            });
        }
    }
}

/// A multiplexing RPC client over any string-frame duplex ([`crate::memory_client`] or
/// [`connect_ws`]). Cheap to clone-by-Arc internally; use one per connection.
pub struct RpcClient {
    out: mpsc::Sender<String>,
    shared: Arc<Shared>,
    next_id: AtomicU64,
    reader: tokio::task::JoinHandle<()>,
}

/// Checked stream receiver whose drop immediately cancels the server task.
pub struct RpcSubscription {
    id: u64,
    items: mpsc::Receiver<serde_json::Value>,
    out: mpsc::Sender<String>,
    shared: Arc<Shared>,
}

impl RpcSubscription {
    pub async fn recv(&mut self) -> Option<serde_json::Value> {
        self.items.recv().await
    }
}

impl Drop for RpcSubscription {
    fn drop(&mut self) {
        if self.shared.lock().remove(&self.id).is_none() {
            return;
        }
        cancel_stream(&self.shared, &self.out, self.id);
    }
}

impl RpcClient {
    /// Wrap an existing duplex: `out` carries client frames, `inbound` server frames.
    pub fn new(out: mpsc::Sender<String>, mut inbound: mpsc::Receiver<String>) -> Self {
        let shared = Arc::new(Shared {
            pending: Mutex::new(HashMap::new()),
            closed: AtomicBool::new(false),
            runtime: tokio::runtime::Handle::current(),
        });
        let reader_shared = shared.clone();
        let reader_out = out.clone();
        let reader = tokio::spawn(async move {
            while let Some(payload) = inbound.recv().await {
                for line in payload.lines() {
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }
                    let frame: ServerFrame = match serde_json::from_str(line) {
                        Ok(frame) => frame,
                        Err(err) => {
                            tracing::warn!(error = %err, "rpc: dropping malformed server frame");
                            continue;
                        }
                    };
                    route_frame(&reader_shared, &reader_out, frame).await;
                }
            }
            reader_shared.close();
        });
        Self {
            out,
            shared,
            next_id: AtomicU64::new(1),
            reader,
        }
    }

    /// Unary request.
    pub async fn call(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, RpcError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.shared.insert(id, Pending::Call(tx))?;
        let _pending = PendingCall {
            id,
            shared: &self.shared,
        };
        self.send(ClientFrame {
            id,
            method: Some(method.into()),
            params,
            cancel: false,
        })
        .await
        .inspect_err(|_| {
            self.shared.lock().remove(&id);
        })?;
        rx.await.map_err(|_| RpcError::Closed)?
    }

    /// Typed unary request.
    pub async fn call_as<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<T, RpcError> {
        let value = self.call(method, params).await?;
        serde_json::from_value(value).map_err(|e| RpcError::BadParams(e.to_string()))
    }

    /// Streaming request: items arrive on the receiver; it closes when the server sends
    /// `{done}` or `{err}`, the connection drops, or its queue stays full for a
    /// second. Dropping the receiver cancels even an idle server-side stream.
    pub async fn subscribe(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<mpsc::Receiver<serde_json::Value>, RpcError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel(STREAM_QUEUE_CAP);
        let watch = self.watch_receiver(id, tx.clone());
        self.shared.insert(
            id,
            Pending::Stream {
                items: tx,
                _watch: watch,
            },
        )?;
        self.send(ClientFrame {
            id,
            method: Some(method.into()),
            params,
            cancel: false,
        })
        .await
        .inspect_err(|_| {
            self.shared.lock().remove(&id);
        })?;
        Ok(rx)
    }

    /// Streaming request with a server acknowledgement before returning.
    ///
    /// Use this for optional/versioned stream methods: an older server's
    /// `unknown method` is returned distinctly instead of looking like an empty stream.
    pub async fn subscribe_checked(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<RpcSubscription, RpcError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (items_tx, items_rx) = mpsc::channel(STREAM_QUEUE_CAP);
        let (ready_tx, ready_rx) = oneshot::channel();
        let watch = self.watch_receiver(id, items_tx.clone());
        self.shared.insert(
            id,
            Pending::CheckedStream {
                items: items_tx,
                ready: Some(ready_tx),
                _watch: watch,
            },
        )?;
        self.send(ClientFrame {
            id,
            method: Some(method.into()),
            params,
            cancel: false,
        })
        .await
        .inspect_err(|_| {
            self.shared.lock().remove(&id);
        })?;
        let subscription = RpcSubscription {
            id,
            items: items_rx,
            out: self.out.clone(),
            shared: self.shared.clone(),
        };
        ready_rx.await.map_err(|_| RpcError::Closed)??;
        Ok(subscription)
    }

    fn watch_receiver(&self, id: u64, items: mpsc::Sender<serde_json::Value>) -> StreamWatch {
        let shared = Arc::downgrade(&self.shared);
        let out = self.out.clone();
        StreamWatch(self.shared.runtime.spawn(async move {
            items.closed().await;
            if let Some(shared) = shared.upgrade()
                && shared.lock().remove(&id).is_some()
            {
                cancel_stream(&shared, &out, id);
            }
        }))
    }

    async fn send(&self, frame: ClientFrame) -> Result<(), RpcError> {
        let json = serde_json::to_string(&frame)
            .map_err(|e| RpcError::Transport(format!("serialize frame: {e}")))?;
        self.out.send(json).await.map_err(|_| RpcError::Closed)
    }
}

impl Drop for RpcClient {
    fn drop(&mut self) {
        self.shared.close();
        self.reader.abort();
    }
}

async fn route_frame(shared: &Arc<Shared>, out: &mpsc::Sender<String>, frame: ServerFrame) {
    let id = frame.id;
    if let Some(err) = frame.err {
        let error = wire_error(err);
        match shared.lock().remove(&id) {
            Some(Pending::Call(tx)) => {
                let _ = tx.send(Err(error));
            }
            Some(Pending::CheckedStream {
                ready: Some(ready), ..
            }) => {
                let _ = ready.send(Err(error));
            }
            Some(Pending::Stream { .. })
            | Some(Pending::CheckedStream { ready: None, .. })
            | None => {
                // Stream errored: the sender drop closes the receiver.
                tracing::debug!(id, %error, "rpc: stream ended with error");
            }
        }
        return;
    }
    if let Some(value) = frame.ok {
        let mut pending = shared.lock();
        if matches!(pending.get(&id), Some(Pending::Call(_))) {
            if let Some(Pending::Call(tx)) = pending.remove(&id) {
                let _ = tx.send(Ok(value));
            }
        } else if let Some(Pending::CheckedStream { ready, .. }) = pending.get_mut(&id)
            && let Some(ready) = ready.take()
        {
            let _ = ready.send(Ok(()));
        }
        return;
    }
    if let Some(item) = frame.item {
        let ready = match shared.lock().get_mut(&id) {
            Some(Pending::CheckedStream { ready, .. }) => ready.take(),
            _ => None,
        };
        if let Some(ready) = ready {
            let _ = ready.send(Ok(()));
        }
        // Never hold the map lock while allowing a briefly busy consumer to drain.
        let tx = match shared.lock().get(&id) {
            Some(Pending::Stream { items, .. }) => Some(items.clone()),
            Some(Pending::CheckedStream { items, .. }) => Some(items.clone()),
            _ => None,
        };
        let dead = match tx {
            Some(tx) => match tokio::time::timeout(STREAM_SEND_TIMEOUT, tx.send(item)).await {
                Ok(result) => result.is_err(),
                Err(_) => {
                    tracing::warn!(
                        id,
                        "rpc: canceling stalled stream; receiver must resubscribe"
                    );
                    true
                }
            },
            None => false,
        };
        if dead {
            // Receiver was dropped — cancel server-side and forget the stream.
            shared.lock().remove(&id);
            cancel_stream(shared, out, id);
        }
        return;
    }
    if frame.done
        && let Some(Pending::CheckedStream {
            ready: Some(ready), ..
        }) = shared.lock().remove(&id)
    {
        let _ = ready.send(Ok(()));
    }
}

fn wire_error(error: String) -> RpcError {
    if let Some(method) = error.strip_prefix("unknown method: ") {
        RpcError::UnknownMethod(method.to_owned())
    } else if let Some(message) = error.strip_prefix("bad params: ") {
        RpcError::BadParams(message.to_owned())
    } else if let Some(message) = error.strip_prefix("transport: ") {
        RpcError::Transport(message.to_owned())
    } else if error == "connection closed" {
        RpcError::Closed
    } else {
        RpcError::Failed(error)
    }
}

/// How long a dial may take before we give up.
///
/// This is localhost: a real engine answers in milliseconds. Without a bound,
/// *any* other process holding the port accepts the TCP connection and then
/// never completes the WebSocket handshake, and the caller waits forever — a
/// stranger on port 27654 would hang the app at boot rather than degrade it.
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Dial a WebSocket RPC server (`ws://127.0.0.1:{ipc_port}`).
pub async fn connect_ws(url: &str) -> Result<RpcClient, RpcError> {
    let (ws, _) = tokio::time::timeout(CONNECT_TIMEOUT, tokio_tungstenite::connect_async(url))
        .await
        .map_err(|_| RpcError::Transport(format!("timed out dialing {url}")))?
        .map_err(|e| RpcError::Transport(e.to_string()))?;
    let (mut sink, mut stream) = ws.split();
    let (out_tx, mut out_rx) = mpsc::channel::<String>(256);
    let (in_tx, in_rx) = mpsc::channel::<String>(256);
    tokio::spawn(async move {
        loop {
            tokio::select! {
                frame = out_rx.recv() => match frame {
                    Some(text) => {
                        if sink.send(WsMessage::Text(text)).await.is_err() {
                            break;
                        }
                    }
                    None => {
                        let _ = sink.send(WsMessage::Close(None)).await;
                        break;
                    }
                },
                message = stream.next() => match message {
                    Some(Ok(WsMessage::Text(text))) => {
                        if in_tx.send(text).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(WsMessage::Close(_))) | Some(Err(_)) | None => break,
                    Some(Ok(_)) => {}
                },
            }
        }
    });
    Ok(RpcClient::new(out_tx, in_rx))
}
