// TaskStore - Generic persistent state management with SQLite+JSONL+Git

pub mod corruption;
pub mod error;
pub mod jsonl;
pub mod query;
pub mod store;

pub use corruption::list_tolerant_at;
pub use error::{Error, Result};

// Re-export trait modules so `use taskstore::record::Record` and
// `use taskstore::filter::Filter` keep resolving after the workspace split.
pub use taskstore_traits::{filter, record};

// Re-export flat types for `use taskstore::Record` / `use taskstore::Filter`.
pub use taskstore_traits::{Filter, FilterOp, IndexValue, Record};

// Re-export corruption-aware types for `use taskstore::ListResult`, etc.
pub use taskstore_traits::{Category, CorruptionEntry, CorruptionError, ListResult, match_filter};

pub use store::{Store, apply_pragmas, now_ms};

// Re-export rusqlite for CLI use
pub use rusqlite;
