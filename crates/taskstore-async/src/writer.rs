// Writer thread dispatch.
//
// Owns a dedicated OS thread (NOT a tokio blocking-pool slot) holding the sync
// `taskstore::Store`. Receives closures over `&mut Store` via tokio mpsc and
// replies via oneshot. Each closure runs to completion before the thread reads
// the next command, which is exactly the single-writer serialization SQLite
// requires.

use std::thread;

use tokio::sync::{mpsc, oneshot};
use tracing::{debug, warn};

use crate::{Error, Result};

/// Boxed write closure. The writer thread applies it to its owned `&mut Store`.
pub(crate) type WriteTask = Box<dyn FnOnce(&mut taskstore::Store) + Send + 'static>;

pub(crate) struct WriterHandle {
    sender: Option<mpsc::Sender<WriteTask>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl WriterHandle {
    /// Spawn the writer thread, handing ownership of the sync `Store` to it.
    /// `queue_capacity` sets the mpsc bound used for backpressure.
    pub(crate) fn spawn(mut store: taskstore::Store, queue_capacity: usize) -> Self {
        let (sender, mut receiver) = mpsc::channel::<WriteTask>(queue_capacity);
        let thread = thread::Builder::new()
            .name("taskstore-async-writer".to_string())
            .spawn(move || {
                debug!("writer thread: started");
                while let Some(task) = receiver.blocking_recv() {
                    task(&mut store);
                }
                debug!("writer thread: channel closed, exiting");
            })
            .expect("failed to spawn taskstore-async-writer thread");

        Self {
            sender: Some(sender),
            thread: Some(thread),
        }
    }

    /// Run the given closure on the writer thread and await its result.
    ///
    /// Returns `Error::StoreClosed` if the writer has shut down (either
    /// explicitly via drop or because a previous command panicked and the
    /// thread exited).
    #[tracing::instrument(level = "trace", skip_all)]
    pub(crate) async fn dispatch<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&mut taskstore::Store) -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let (reply_tx, reply_rx) = oneshot::channel::<Result<T>>();

        let task: WriteTask = Box::new(move |store| {
            let outcome = f(store);
            // Ignore send errors: if the receiver is gone, the caller's future
            // was dropped before it could await the result. Nothing to do.
            let _ = reply_tx.send(outcome);
        });

        let sender = self.sender.as_ref().ok_or(Error::StoreClosed)?;
        sender.send(task).await.map_err(|_| Error::StoreClosed)?;

        reply_rx.await.map_err(|_| Error::StoreClosed)?
    }

    /// Async-safe shutdown: drop the sender to signal the writer thread, then
    /// move the `JoinHandle::join` call to a blocking task so it does not
    /// block the tokio reactor.
    ///
    /// Prefer this over relying on `Drop` when the store is being torn down
    /// from async code. The Drop impl is still correct but joins synchronously
    /// on whichever thread is dropping, which blocks a tokio worker if that
    /// happens on the runtime.
    pub(crate) async fn close(mut self) -> Result<()> {
        drop(self.sender.take());
        if let Some(thread) = self.thread.take() {
            tokio::task::spawn_blocking(move || {
                if let Err(panic) = thread.join() {
                    warn!(?panic, "writer thread panicked during shutdown");
                }
            })
            .await
            .map_err(|e| Error::Other(format!("writer shutdown task panicked: {e}")))?;
        }
        Ok(())
    }
}

impl Drop for WriterHandle {
    fn drop(&mut self) {
        // Drop the sender first so the writer thread sees the channel close
        // and exits after draining any in-flight commands.
        drop(self.sender.take());
        if let Some(thread) = self.thread.take()
            && let Err(panic) = thread.join()
        {
            warn!(?panic, "writer thread panicked during shutdown");
        }
    }
}
