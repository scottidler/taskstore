// Public `AsyncStore` type.
//
// Owns one writer thread (dedicated OS thread holding the sync Store) and a
// bounded reader connection pool. Public surface mirrors the sync API; every
// method is `async fn` and takes `&self` because internal state lives behind
// the writer thread and the reader pool.

use std::path::{Path, PathBuf};

use taskstore_traits::{Filter, IndexValue, Record};

use crate::reader::ReaderPool;
use crate::writer::WriterHandle;
use crate::{Error, OpenOptions, Result};

pub struct AsyncStore {
    base_path: PathBuf,
    writer: WriterHandle,
    readers: ReaderPool,
}

impl AsyncStore {
    /// Open or create an async store at `$CWD/.taskstore/`.
    ///
    /// Convenience wrapper over [`AsyncStore::open_at`] for callers that want
    /// the legacy location. Daemons and library consumers that need an
    /// explicit path should use [`AsyncStore::open_at`] directly.
    #[tracing::instrument(level = "info", skip_all, fields(
        read_connections = opts.read_connections,
        writer_queue_capacity = opts.writer_queue_capacity,
    ))]
    pub async fn open(opts: OpenOptions) -> Result<Self> {
        let cwd = std::env::current_dir()
            .map_err(|e| Error::Other(format!("Failed to read current working directory: {e}")))?;
        Self::open_at(cwd.join(".taskstore"), opts).await
    }

    /// Open or create an async store at the exact path given.
    ///
    /// The path is used as-is; no `.taskstore` append, no auto-discovery.
    /// Parent directories are created as needed. Bootstraps by opening a sync
    /// `taskstore::Store` on a dedicated thread (to keep the tokio reactor
    /// clean), hands ownership of that Store to the writer thread, then opens
    /// `opts.read_connections` additional independent connections for the
    /// reader pool.
    #[tracing::instrument(level = "info", skip_all, fields(
        read_connections = opts.read_connections,
        writer_queue_capacity = opts.writer_queue_capacity,
    ))]
    pub async fn open_at<P: AsRef<Path>>(base_path: P, opts: OpenOptions) -> Result<Self> {
        let base_path = base_path.as_ref().to_path_buf();

        let store = tokio::task::spawn_blocking({
            let base_path = base_path.clone();
            move || taskstore::Store::open_at(&base_path)
        })
        .await
        .map_err(|e| Error::Other(format!("bootstrap task panicked: {e}")))??;

        let base_path = store.base_path().to_path_buf();
        let db_path = base_path.join("taskstore.db");

        // Bootstrap hands its Connection to the writer thread; open independent
        // reader Connections separately on the same DB file.
        let read_conns = opts.read_connections;
        let readers = tokio::task::spawn_blocking({
            let db_path = db_path.clone();
            move || ReaderPool::open(db_path, read_conns)
        })
        .await
        .map_err(|e| Error::Other(format!("reader pool bootstrap task panicked: {e}")))??;

        let writer = WriterHandle::spawn(store, opts.writer_queue_capacity);

        Ok(Self {
            base_path,
            writer,
            readers,
        })
    }

    /// Path to the `.taskstore/` directory this store manages.
    pub fn base_path(&self) -> &Path {
        &self.base_path
    }

    /// Explicit graceful shutdown: drains outstanding writes and joins the
    /// writer thread on a blocking task so the tokio reactor is not blocked.
    ///
    /// `Drop` is still correct if `close` is not called - the destructor
    /// drops the sender and synchronously joins the thread - but calling
    /// `close` is strictly better when tearing down from async context
    /// because it moves the join off the reactor thread.
    #[tracing::instrument(level = "info", skip_all)]
    pub async fn close(self) -> Result<()> {
        let Self {
            base_path: _,
            writer,
            readers,
        } = self;
        // Reader connections close on drop; nothing async required.
        drop(readers);
        writer.close().await
    }

    // -- Writes (routed to writer thread) ----------------------------------

    /// Create a record. Returns the persisted id.
    #[tracing::instrument(level = "debug", skip_all, fields(collection = T::collection_name()))]
    pub async fn create<T: Record>(&self, record: T) -> Result<String> {
        self.writer.dispatch(move |store| store.create(record)).await
    }

    /// Atomically create a batch of records of the same type.
    #[tracing::instrument(level = "debug", skip_all, fields(collection = T::collection_name(), count = records.len()))]
    pub async fn create_many<T: Record>(&self, records: Vec<T>) -> Result<Vec<String>> {
        self.writer.dispatch(move |store| store.create_many(records)).await
    }

    /// Update a record (alias for create under the current semantics).
    #[tracing::instrument(level = "debug", skip_all, fields(collection = T::collection_name()))]
    pub async fn update<T: Record>(&self, record: T) -> Result<()> {
        self.writer.dispatch(move |store| store.update(record)).await
    }

    /// Delete a record by id.
    #[tracing::instrument(level = "debug", skip_all, fields(collection = T::collection_name(), id = %id))]
    pub async fn delete<T: Record>(&self, id: &str) -> Result<()> {
        let id = id.to_string();
        self.writer.dispatch(move |store| store.delete::<T>(&id)).await
    }

    /// Delete all records whose indexed field matches `value`.
    #[tracing::instrument(level = "debug", skip_all, fields(collection = T::collection_name(), field = %field))]
    pub async fn delete_by_index<T: Record>(&self, field: &str, value: IndexValue) -> Result<usize> {
        let field = field.to_string();
        self.writer
            .dispatch(move |store| store.delete_by_index::<T>(&field, value))
            .await
    }

    /// Rebuild indexes for a specific record type (schema-evolution path;
    /// not required after `sync()` for correctness).
    #[tracing::instrument(level = "debug", skip_all, fields(collection = T::collection_name()))]
    pub async fn rebuild_indexes<T: Record>(&self) -> Result<usize> {
        self.writer.dispatch(move |store| store.rebuild_indexes::<T>()).await
    }

    /// Sync the SQLite cache from JSONL.
    #[tracing::instrument(level = "info", skip_all)]
    pub async fn sync(&self) -> Result<()> {
        self.writer.dispatch(move |store| store.sync()).await
    }

    /// Install git hooks that keep the cache in sync with the JSONL files
    /// when git history moves.
    #[tracing::instrument(level = "info", skip_all)]
    pub async fn install_git_hooks(&self) -> Result<()> {
        self.writer.dispatch(move |store| store.install_git_hooks()).await
    }

    // -- Reads (routed to reader pool) -------------------------------------

    /// Fetch a single record by id, or `None` if absent.
    #[tracing::instrument(level = "debug", skip_all, fields(collection = T::collection_name(), id = %id))]
    pub async fn get<T: Record>(&self, id: &str) -> Result<Option<T>> {
        let collection = T::collection_name();
        let id = id.to_string();
        let raw = self
            .readers
            .run(move |conn| taskstore::query::get_data_json(conn, collection, &id))
            .await?;
        match raw {
            Some(json) => {
                let record: T = serde_json::from_str(&json)?;
                Ok(Some(record))
            }
            None => Ok(None),
        }
    }

    /// List records with optional filtering. Materializes the full result set.
    #[tracing::instrument(level = "debug", skip_all, fields(collection = T::collection_name(), filter_count = filters.len()))]
    pub async fn list<T: Record>(&self, filters: &[Filter]) -> Result<Vec<T>> {
        let collection = T::collection_name();
        let filters = filters.to_vec();
        let raws = self
            .readers
            .run(move |conn| taskstore::query::list_data_jsons(conn, collection, &filters))
            .await?;
        let mut records = Vec::with_capacity(raws.len());
        for json in raws {
            let record: T = serde_json::from_str(&json)?;
            records.push(record);
        }
        Ok(records)
    }

    /// Corruption-aware bulk read.
    ///
    /// Dispatches the JSONL parse to `tokio::task::spawn_blocking` directly,
    /// bypassing both the writer thread (no need to serialize behind writes)
    /// and the reader pool (no SQLite connection needed). The shared `fs2`
    /// lock taken inside `read_jsonl_latest_with_corruption` is the same one
    /// today's `sync()` uses, so concurrent JSONL reads across tasks are safe.
    #[tracing::instrument(level = "debug", skip_all, fields(
        collection = T::collection_name(),
        filter_count = filters.len(),
    ))]
    pub async fn list_tolerant<T: Record>(&self, filters: &[Filter]) -> Result<taskstore_traits::ListResult<T>> {
        let base_path = self.base_path.clone();
        let filters = filters.to_vec();
        tokio::task::spawn_blocking(move || taskstore::list_tolerant_at::<T>(&base_path, &filters))
            .await
            .map_err(|e| Error::Other(format!("list_tolerant task panicked: {e}")))?
    }

    /// Returns true if any JSONL file has been modified since the last sync,
    /// or if there are JSONL files that have never been synced.
    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn is_stale(&self) -> Result<bool> {
        let base_path = self.base_path.clone();
        self.readers
            .run(move |conn| taskstore::query::is_stale(conn, &base_path))
            .await
    }
}
