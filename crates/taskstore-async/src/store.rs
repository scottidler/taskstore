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
    /// Open or create an async store at the given path.
    ///
    /// Bootstraps by opening a sync `taskstore::Store` on a dedicated thread
    /// (to keep the tokio reactor clean). Hands ownership of that Store to the
    /// writer thread, then opens `opts.read_connections` additional independent
    /// connections for the reader pool.
    pub async fn open<P: AsRef<Path>>(path: P, opts: OpenOptions) -> Result<Self> {
        let path = path.as_ref().to_path_buf();

        let store = tokio::task::spawn_blocking({
            let path = path.clone();
            move || taskstore::Store::open(&path)
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

    // -- Writes (routed to writer thread) ----------------------------------

    /// Create a record. Returns the persisted id.
    pub async fn create<T: Record>(&self, record: T) -> Result<String> {
        self.writer.dispatch(move |store| store.create(record)).await
    }

    /// Atomically create a batch of records of the same type.
    pub async fn create_many<T: Record>(&self, records: Vec<T>) -> Result<Vec<String>> {
        self.writer.dispatch(move |store| store.create_many(records)).await
    }

    /// Update a record (alias for create under the current semantics).
    pub async fn update<T: Record>(&self, record: T) -> Result<()> {
        self.writer.dispatch(move |store| store.update(record)).await
    }

    /// Delete a record by id.
    pub async fn delete<T: Record>(&self, id: &str) -> Result<()> {
        let id = id.to_string();
        self.writer.dispatch(move |store| store.delete::<T>(&id)).await
    }

    /// Delete all records whose indexed field matches `value`.
    pub async fn delete_by_index<T: Record>(&self, field: &str, value: IndexValue) -> Result<usize> {
        let field = field.to_string();
        self.writer
            .dispatch(move |store| store.delete_by_index::<T>(&field, value))
            .await
    }

    /// Rebuild indexes for a specific record type (schema-evolution path;
    /// not required after `sync()` for correctness).
    pub async fn rebuild_indexes<T: Record>(&self) -> Result<usize> {
        self.writer.dispatch(move |store| store.rebuild_indexes::<T>()).await
    }

    /// Sync the SQLite cache from JSONL.
    pub async fn sync(&self) -> Result<()> {
        self.writer.dispatch(move |store| store.sync()).await
    }

    /// Install git hooks that keep the cache in sync with the JSONL files
    /// when git history moves.
    pub async fn install_git_hooks(&self) -> Result<()> {
        self.writer.dispatch(move |store| store.install_git_hooks()).await
    }

    // -- Reads (routed to reader pool) -------------------------------------

    /// Fetch a single record by id, or `None` if absent.
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

    /// Returns true if any JSONL file has been modified since the last sync,
    /// or if there are JSONL files that have never been synced.
    pub async fn is_stale(&self) -> Result<bool> {
        let base_path = self.base_path.clone();
        self.readers
            .run(move |conn| taskstore::query::is_stale(conn, &base_path))
            .await
    }
}
