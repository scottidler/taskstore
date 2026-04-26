// Unified error type for the taskstore workspace.
//
// Re-exported from `taskstore-async` so consumers see one `Error` type across
// both surfaces. `StoreClosed` is produced only by `AsyncStore`; sync code
// never constructs it.

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("store I/O failure: {0}")]
    Io(#[from] std::io::Error),

    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    /// Produced only by `AsyncStore` when the writer-thread channel is closed.
    /// Sync `Store` code never constructs this variant.
    #[error("store task channel closed (store shut down)")]
    StoreClosed,

    #[error("WAL mode could not be enabled on {path} (filesystem may not support it)")]
    WalUnsupported { path: PathBuf },

    /// Catch-all for context messages that don't fit a typed variant.
    /// Used when wrapping a lower-level error with operational context;
    /// prefer typed variants for new error sites.
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;
