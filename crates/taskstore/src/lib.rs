// TaskStore - Generic persistent state management with SQLite+JSONL+Git

pub mod jsonl;
pub mod store;

// Re-export trait modules so `use taskstore::record::Record` and
// `use taskstore::filter::Filter` keep resolving after the workspace split.
pub use taskstore_traits::{filter, record};

// Re-export flat types for `use taskstore::Record` / `use taskstore::Filter`.
pub use taskstore_traits::{Filter, FilterOp, IndexValue, Record};

pub use store::{Store, now_ms};

// Re-export rusqlite for CLI use
pub use rusqlite;
