//! The core's own Tokio runtime and its cancellation primitive (IOS-00).
//!
//! One small multi-thread runtime per process, created on first use and never
//! dropped: platform threads never block on it, and dropping a runtime from
//! async context panics. Every exported `async` method spawns its work here
//! and awaits it, so the futures UniFFI hands to Swift are executor-agnostic.
//!
//! Swift's UniFFI bindings never drop a Rust future when the awaiting Swift
//! task is cancelled; they wait for it to finish. Cancellation therefore
//! crosses the boundary explicitly, as a [`Cancellation`] the platform
//! cancels (the app bridges `Task` cancellation to it). The awaited task also
//! aborts when its future is dropped, so a Rust caller that stops waiting
//! never leaves work running behind it.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};
use std::task::{Context, Poll};

use tokio::runtime::Runtime;
use tokio::sync::watch;

use crate::session::SessionError;

pub(crate) fn runtime() -> &'static Runtime {
    static RUNTIME: OnceLock<Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("luna-core")
            .enable_all()
            .build()
            .expect("LUNA core runtime")
    })
}

/// One operation's cancellation, created and cancelled by the platform.
/// Cancelling is idempotent and may happen before the operation starts.
#[derive(uniffi::Object)]
pub struct Cancellation {
    cancelled: watch::Sender<bool>,
}

#[uniffi::export]
impl Cancellation {
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            cancelled: watch::Sender::new(false),
        })
    }

    pub fn cancel(&self) {
        self.cancelled.send_replace(true);
    }

    pub fn is_cancelled(&self) -> bool {
        *self.cancelled.borrow()
    }
}

impl Cancellation {
    /// Resolves once cancelled (at once when already cancelled).
    pub(crate) async fn cancelled(&self) {
        let mut cancelled = self.cancelled.subscribe();
        // The sender lives in `self`, so this only returns once it is true.
        let _ = cancelled.wait_for(|cancelled| *cancelled).await;
    }
}

/// Run `future` on the core runtime until it finishes or `cancel` fires.
/// A cancelled operation's future is dropped on the runtime, which releases
/// what it holds (a pending reply slot, an engine stream) before this
/// returns [`SessionError::Cancelled`].
pub(crate) async fn run<T, F>(
    cancel: Option<Arc<Cancellation>>,
    future: F,
) -> Result<T, SessionError>
where
    T: Send + 'static,
    F: Future<Output = Result<T, SessionError>> + Send + 'static,
{
    Spawned(runtime().spawn(async move {
        match cancel {
            Some(cancel) => tokio::select! {
                biased;
                _ = cancel.cancelled() => Err(SessionError::Cancelled),
                result = future => result,
            },
            None => future.await,
        }
    }))
    .await
}

/// A spawned operation that is aborted when its awaiting future is dropped.
struct Spawned<T>(tokio::task::JoinHandle<Result<T, SessionError>>);

impl<T> Future for Spawned<T> {
    type Output = Result<T, SessionError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match Pin::new(&mut self.0).poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Ok(result)) => Poll::Ready(result),
            Poll::Ready(Err(error)) if error.is_cancelled() => {
                Poll::Ready(Err(SessionError::Cancelled))
            }
            Poll::Ready(Err(error)) => Poll::Ready(Err(SessionError::Engine {
                detail: format!("core task failed: {error}"),
            })),
        }
    }
}

impl<T> Drop for Spawned<T> {
    fn drop(&mut self) {
        // A no-op once the task has finished.
        self.0.abort();
    }
}
