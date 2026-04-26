// Corruption-aware bulk-read types.
//
// Lives in taskstore-traits so both `taskstore` and `taskstore-async` can name
// these types directly. The crate has no I/O dependency, so `Category` is a
// plain enum here; the `From<serde_json::error::Category>` conversion is a
// free function in the consumer crate (`taskstore::error::category_from_serde_json`)
// to keep this crate I/O-free.

use std::io::ErrorKind;
use std::path::PathBuf;

/// Result of a corruption-aware bulk read.
///
/// `records` is the live record set after last-write-wins-per-id and tombstone
/// filtering. `corruption` lists every line that could not be turned into a
/// usable record, with file/line attribution.
///
/// Marked `#[non_exhaustive]` so future fields (e.g. an `omitted_count: u64`
/// for a capped-corruption mode) can be added without a breaking change.
/// Cross-crate construction must go through [`ListResult::new`]; struct-literal
/// construction is forbidden by the attribute outside the defining crate.
#[derive(Debug)]
#[non_exhaustive]
pub struct ListResult<T> {
    pub records: Vec<T>,
    pub corruption: Vec<CorruptionEntry>,
}

impl<T> ListResult<T> {
    /// Construct a `ListResult` from records and corruption entries.
    pub fn new(records: Vec<T>, corruption: Vec<CorruptionEntry>) -> Self {
        Self { records, corruption }
    }
}

/// One corrupt line surfaced by `list_tolerant<T>`.
#[derive(Debug, Clone)]
pub struct CorruptionEntry {
    pub file: PathBuf,
    /// 1-indexed line number within the JSONL file.
    pub line: u64,
    /// Original line bytes (`String::from_utf8_lossy` for non-UTF-8),
    /// truncated at a UTF-8 char boundary at or below 4096 bytes (using
    /// `str::floor_char_boundary`) with the literal "...[truncated]"
    /// appended when the original exceeded that size. Naive byte slicing
    /// would panic on multi-byte boundaries and is forbidden in the
    /// truncation helper.
    ///
    /// For `TypeMismatch` entries, this is the parsed `serde_json::Value`
    /// re-serialized; the original line text is no longer in scope at
    /// type-deserialization time.
    pub raw: String,
    pub error: CorruptionError,
}

/// What went wrong with one line.
#[derive(Debug, Clone)]
pub enum CorruptionError {
    /// `serde_json::from_str` failed on this line.
    /// `category` is taskstore's mirror of `serde_json::error::Category`,
    /// owned to avoid a serde_json semver coupling on the public surface.
    InvalidJson { msg: String, category: Category },

    /// Line is valid JSON but lacks an `id` field.
    MissingId,

    /// Line is valid JSON, has an `id`, but does not deserialize to `T`.
    TypeMismatch { msg: String },

    /// `BufReader::lines()` yielded `Err` for this line (rare; partial reads,
    /// NUL bytes, transient FS errors).
    Io { kind: ErrorKind },
}

/// Taskstore-owned mirror of `serde_json::error::Category`.
///
/// Conversion from `serde_json::error::Category` is implemented as a free
/// function in `taskstore::error::category_from_serde_json` (not a `From` impl)
/// because Rust's orphan rule forbids `impl From<foreign> for Category`
/// outside `taskstore-traits`, and adding `serde_json` as a dependency to this
/// crate would violate its lean, I/O-free charter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Syntax,
    Eof,
    Data,
    Io,
}
