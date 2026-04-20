// Reader connection pool.
//
// A fixed-size pool of independent `rusqlite::Connection` instances. Each
// connection is `Send + !Sync`, so at most one task may hold a given
// connection at a time; the pool enforces this with a tokio semaphore.
//
// Reads run inside `tokio::task::spawn_blocking`: a reader task checks out a
// connection (async wait on semaphore), blocks the spawn_blocking thread for
// the SQL execution, then returns the connection when the guard drops. Total
// blocking-pool usage is bounded by the pool size.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use tokio::sync::Semaphore;
use tracing::debug;

use crate::{Error, Result};

pub(crate) struct ReaderPool {
    connections: Arc<Mutex<Vec<Connection>>>,
    semaphore: Arc<Semaphore>,
}

impl ReaderPool {
    /// Open `size` independent connections to `db_path` and apply the shared
    /// pragmas (WAL, busy_timeout, foreign_keys) to each one.
    pub(crate) fn open(db_path: PathBuf, size: usize) -> Result<Self> {
        assert!(size > 0, "reader pool size must be > 0");

        let mut connections = Vec::with_capacity(size);
        for _ in 0..size {
            let conn = Connection::open(&db_path)?;
            taskstore::apply_pragmas(&conn, &db_path).map_err(Error::from)?;
            connections.push(conn);
        }
        debug!(?db_path, size, "reader pool: opened connections");

        Ok(Self {
            connections: Arc::new(Mutex::new(connections)),
            semaphore: Arc::new(Semaphore::new(size)),
        })
    }

    /// Acquire a connection and run the closure inside `spawn_blocking`.
    /// The closure returns `Result<T, eyre::Report>`; this method maps it to
    /// the crate-local `Error` type.
    pub(crate) async fn run<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&Connection) -> eyre::Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let permit = self
            .semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| Error::StoreClosed)?;

        let connections = self.connections.clone();
        let handle = tokio::task::spawn_blocking(move || {
            let conn = {
                let mut guard = connections.lock().expect("reader pool mutex poisoned");
                guard.pop().expect("reader pool empty despite acquired permit")
            };

            let outcome = f(&conn);

            // Defensive rollback: if the closure started a transaction and
            // bailed, ensure we return a clean connection to the pool.
            if let Err(rollback_err) = conn.execute_batch("ROLLBACK") {
                // `no transaction` is the expected case when the closure did
                // not open one. Anything else is worth logging.
                let msg = rollback_err.to_string();
                if !msg.contains("no transaction is active") {
                    debug!(error = %msg, "reader rollback returned non-trivial status");
                }
            }

            connections.lock().expect("reader pool mutex poisoned").push(conn);

            drop(permit);

            outcome
        });

        handle
            .await
            .map_err(|e| Error::Other(format!("reader task panicked: {e}")))?
            .map_err(Error::from)
    }
}
