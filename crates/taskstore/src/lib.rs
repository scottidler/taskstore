// TaskStore - Generic persistent state management with SQLite+JSONL+Git

pub mod filter;
pub mod jsonl;
pub mod record;
pub mod store;

// Re-export main types for convenience
pub use filter::{Filter, FilterOp};
pub use record::{IndexValue, Record};
pub use store::{Store, now_ms};

// Re-export rusqlite for CLI use
pub use rusqlite;
