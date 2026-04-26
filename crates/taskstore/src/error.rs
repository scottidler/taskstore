// Unified error type for the taskstore workspace.
//
// Re-exported from `taskstore-async` so consumers see one `Error` type across
// both surfaces. `StoreClosed` is produced only by `AsyncStore`; sync code
// never constructs it.

use std::path::PathBuf;
use taskstore_traits::Category;

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

/// Convert `serde_json::error::Category` to taskstore's `Category` mirror.
///
/// This is a free function rather than `impl From<...> for Category` because
/// `Category` lives in `taskstore-traits` (which has no `serde_json` dep, by
/// design) and Rust's orphan rule forbids the impl in `taskstore`: both the
/// trait (`From<foreign_type>`) and the type (`Category`) are foreign here.
pub(crate) fn category_from_serde_json(c: serde_json::error::Category) -> Category {
    use serde_json::error::Category as S;
    match c {
        S::Io => Category::Io,
        S::Syntax => Category::Syntax,
        S::Data => Category::Data,
        S::Eof => Category::Eof,
    }
}
