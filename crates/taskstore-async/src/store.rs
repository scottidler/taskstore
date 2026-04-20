// Public `AsyncStore` type.
//
// Owns one writer thread (dedicated OS thread holding the sync Store) and
// eventually a reader connection pool (Phase 4). Public surface mirrors the
// sync API; every method is `async fn` and takes `&self` because internal
// state lives behind the writer thread and the reader pool.

use std::path::{Path, PathBuf};

use taskstore_traits::{IndexValue, Record};

use crate::writer::WriterHandle;
use crate::{Error, OpenOptions, Result};

pub struct AsyncStore {
    base_path: PathBuf,
    writer: WriterHandle,
}

impl AsyncStore {
    /// Open or create an async store at the given path.
    ///
    /// Bootstraps by opening a sync `taskstore::Store` on a dedicated thread
    /// (to keep the tokio reactor clean), then hands ownership to the writer
    /// thread.
    pub async fn open<P: AsRef<Path>>(path: P, opts: OpenOptions) -> Result<Self> {
        let path = path.as_ref().to_path_buf();

        let store = tokio::task::spawn_blocking({
            let path = path.clone();
            move || taskstore::Store::open(&path)
        })
        .await
        .map_err(|e| Error::Other(format!("bootstrap task panicked: {e}")))??;

        let base_path = store.base_path().to_path_buf();
        let writer = WriterHandle::spawn(store, opts.writer_queue_capacity);

        Ok(Self { base_path, writer })
    }

    /// Path to the `.taskstore/` directory this store manages.
    pub fn base_path(&self) -> &Path {
        &self.base_path
    }

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
}
