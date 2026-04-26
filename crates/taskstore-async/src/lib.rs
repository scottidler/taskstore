// taskstore-async - async-native wrapper over the sync taskstore crate.
//
// Wraps a blocking `taskstore::Store` with a dedicated writer thread and a
// bounded reader connection pool. Exposes an `async fn` surface whose method
// signatures mirror the sync API so porting is mechanical.

#![deny(clippy::unwrap_used)]
#![deny(unused_variables)]

mod options;
mod reader;
mod store;
mod writer;

#[cfg(test)]
mod tests;

pub use options::OpenOptions;
pub use store::AsyncStore;

// Re-export the unified error type from taskstore. `taskstore_async::Error`
// and `taskstore_async::Result` resolve to the same types as their sync
// counterparts, so consumers see one Error across both surfaces.
pub use taskstore::{Error, Result};

// Convenience re-exports so consumers can `use taskstore_async::{Record, Filter}`
// without pulling taskstore-traits directly.
pub use taskstore_traits::{Filter, FilterOp, IndexValue, Record};
