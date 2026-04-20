// taskstore-async - async-native wrapper over the sync taskstore crate.
//
// Wraps a blocking `taskstore::Store` with a dedicated writer thread and a
// bounded reader connection pool. Exposes an `async fn` surface whose method
// signatures mirror the sync API so porting is mechanical.

#![deny(clippy::unwrap_used)]
#![deny(unused_variables)]

mod error;
mod options;
mod reader;
mod store;
mod writer;

pub use error::Error;
pub use options::OpenOptions;
pub use store::AsyncStore;

// Convenience re-exports so consumers can `use taskstore_async::{Record, Filter}`
// without pulling taskstore-traits directly.
pub use taskstore_traits::{Filter, FilterOp, IndexValue, Record};

pub type Result<T> = std::result::Result<T, Error>;
