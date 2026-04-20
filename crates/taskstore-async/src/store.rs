// Public `AsyncStore` type. Phases 3-5 populate this.

use std::path::{Path, PathBuf};

use crate::{OpenOptions, Result};

/// Async-native wrapper over `taskstore::Store`.
///
/// Owns one writer thread (dedicated OS thread holding the sync `Store`) and a
/// bounded reader connection pool. All writes route through the writer thread;
/// reads fan out across the pool.
pub struct AsyncStore {
    base_path: PathBuf,
}

impl AsyncStore {
    /// Open or create an async store at the given path.
    ///
    /// Phase 2 stub: bootstraps via sync `taskstore::Store::open` only. Writer
    /// thread and reader pool wiring land in Phases 3-5.
    pub async fn open<P: AsRef<Path>>(path: P, _opts: OpenOptions) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let bootstrap = tokio::task::spawn_blocking({
            let path = path.clone();
            move || taskstore::Store::open(&path)
        })
        .await
        .map_err(|e| crate::Error::Other(format!("bootstrap task panicked: {e}")))??;

        let base_path = bootstrap.base_path().to_path_buf();
        drop(bootstrap);

        Ok(Self { base_path })
    }

    /// Path to the `.taskstore/` directory this store manages.
    pub fn base_path(&self) -> &Path {
        &self.base_path
    }
}
