# Design Document: `list_tolerant<T>` - Corruption-Aware Bulk Read

**Author:** Scott Idler
**Date:** 2026-04-25
**Status:** Implemented
**Replaces:** earlier 2026-04-25 draft written against taskstore v0.2.2 (pre-workspace-split). That draft is superseded by this document; do not implement against it.

## Summary

Three-phase change to taskstore against the v0.5.x workspace surface:

1. **Phase 1 - Error unification.** Move the typed `Error` enum from `taskstore-async` into `taskstore` itself (as `taskstore::Error`, defined in `crates/taskstore/src/error.rs`). Migrate every public sync API from `eyre::Result<T>` to `Result<T, taskstore::Error>`. Drop the `eyre` dependency from both `taskstore` and `taskstore-async`. `taskstore-async::Error` becomes a re-export of `taskstore::Error`.
2. **Phase 2 - Corruption types and tolerant JSONL reader.** Add `ListResult<T>`, `CorruptionEntry`, `CorruptionError`, `Category`, and `match_filter` to `taskstore-traits` (lean, I/O-free). Refactor `taskstore::jsonl::read_jsonl_latest` so its tolerance is shared between today's silent-skip caller (`sync()`) and a new diagnostic-returning variant. Add `taskstore::list_tolerant_at<T>` as a free function that does the work.
3. **Phase 3 - Public surface on both stores.** `Store::list_tolerant<T>` (sync wrapper) and `AsyncStore::list_tolerant<T>` (async wrapper that dispatches via `tokio::task::spawn_blocking` directly, bypassing both the writer thread and the reader pool). Re-export the corruption types from `taskstore-async`.

The driving consumer is loopr's daemon-boot sweep, which wants to inspect every collection on startup and surface corruption rather than silently self-heal. The same surface serves any audit/doctor/repair tool that needs line-level visibility.

This is a workspace-wide breaking change: every public method on `Store` changes return type. One coordinated bump rather than two.

## Problem Statement

### Background: v0.5.x architecture

```
taskstore-traits  v0.1.0  - lean, I/O-free.   serde dep only.
                            Owns Record, Filter, FilterOp, IndexValue.
taskstore         v0.5.x  - sync core.        eyre::Result throughout (today).
                            Owns Store, jsonl.rs, query.rs, apply_pragmas.
taskstore-async   v0.2.x  - async wrapper.    thiserror Error enum (today).
                            AsyncStore wraps Store with a writer thread + reader pool.
                            Has impl From<eyre::Report> for Error to bridge.
```

Read paths today:

- `Store::list<T>(&[Filter])` -> `query::list_data_jsons(&Connection, ...)` -> SQLite `record_indexes` join -> `Vec<String>` (JSON) -> typed deserialize.
- `AsyncStore::list<T>(&[Filter])` -> reader pool -> same `query::list_data_jsons` -> typed deserialize on the async side.
- JSONL is the source of truth. SQLite is a derived cache populated by `Store::sync()` calling `jsonl::read_jsonl_latest`. `is_stale()` triggers re-sync when a JSONL mtime advances.

`read_jsonl_latest` is **already tolerant** of malformed lines (`jsonl.rs:50-104`). It iterates lines, calls `serde_json::from_str`, and on failure does `warn!(...) ; continue`. There is even a passing test for the behavior (`test_read_jsonl_malformed_line`). That tolerance is correct for boot resilience: a single bad line should not prevent the daemon from starting. But the diagnostic is thrown on the floor. By the time `list<T>` returns, the caller sees `Ok(records_present_in_sqlite)` with no signal that anything was missing.

### Problem

A caller has no way to ask "did any records fail to load?" The information exists at the JSONL parsing layer but is dropped before reaching the public API. Specifically:

- A line that fails `serde_json::from_str` is logged and skipped.
- A line with valid JSON but no `id` field is logged and skipped.
- A line whose underlying `read_line` returns `Err` (rare; partial reads, NUL bytes, FS hiccups) is logged and skipped.
- A line that parses as `serde_json::Value` but does not deserialize to the typed `T` is invisible at the JSONL layer (typed deserialization happens later, against SQLite, where it propagates an error for the whole call - the corruption story for typed deserialization is split across two layers).

A second problem, surfaced by the consumer ask: today `taskstore` returns `eyre::Result` and `taskstore-async` returns its own typed `Error`. Consumers (loopr) want a single typed error across the workspace so they can match on variants instead of formatting opaque `eyre::Report` strings. The right time to unify is when introducing the new corruption-aware surface, since the new method's signature should land on the final error type the first time.

### Goals

- One typed `taskstore::Error` enum used by both crates. `eyre` removed from the workspace.
- One new method on `Store` and one on `AsyncStore`: `list_tolerant<T: Record>(&[Filter]) -> Result<ListResult<T>, Error>`.
- Reads JSONL directly. SQLite is bypassed for this method.
- Returns line-level corruption signal: number of malformed lines and where they live.
- No change to existing `list<T>` *behavior* (return type changes type-only, semantics identical).
- No change to JSONL on-disk format.
- No automatic recovery, quarantine, or repair.
- Internal refactor of the JSONL reader so the existing tolerant behavior is reused, not duplicated.

### Non-Goals

- **Per-collection variants** (`list_plans_tolerant` etc.). The existing API is generic over `T: Record`; one method covers all collections.
- **Streaming iterator.** Last-write-wins-per-id requires buffering the whole file; a streaming iterator that gives that up has different semantics from `list`. Not worth the surface area.
- **Audit-only helper.** `list_tolerant::<T>(&[])` with no filter is the audit. One method covers both jobs.
- **Change to `get(id)`.** Single-record reads keep their existing error behavior (now typed).
- **Change to `updated_at` defaulting.** Existing reader silently defaults missing/non-int `updated_at` to 0; preserved. Not classified as corruption.
- **Auto-recovery, quarantine moves, JSONL format changes.** Out of scope.
- **Performance parity with `list<T>`.** `list_tolerant` re-parses JSONL on every call. Documented as a sweep/audit path, not a hot read.
- **Splitting `Error` into a base sync error and an async-extension enum.** The unified enum carries variants both layers may produce; sync code never produces `StoreClosed`. Documented; not enforced at the type level.

## Architecture: Where Things Live

```
taskstore-traits/src/
    corruption.rs   NEW   ListResult<T>, CorruptionEntry, CorruptionError, Category
    filter.rs       MOD   add pub fn match_filter(...)
    lib.rs          MOD   re-export new types

taskstore/src/
    error.rs        NEW   pub enum Error, pub type Result<T>, From impls,
                          impl From<serde_json::error::Category> for Category
    corruption.rs   NEW   pub fn list_tolerant_at<T>(base_path, &[Filter]) -> Result<ListResult<T>>
    jsonl.rs        MOD   add read_jsonl_latest_with_corruption; refactor read_jsonl_latest
                          to share the parse loop. Convert eyre::Result -> Result<_, Error>.
    store.rs        MOD   convert all signatures eyre -> Error.
                          Add Store::list_tolerant<T>.
    query.rs        MOD   convert all signatures eyre -> Error.
    main.rs         MOD   convert eyre -> Error.
    bin/*.rs        MOD   convert eyre -> Error.
    examples/*.rs   MOD   convert all 10 examples eyre -> Error.
    Cargo.toml      MOD   drop eyre, add thiserror.

taskstore-async/src/
    error.rs        DEL   contents move to taskstore::error.
    lib.rs          MOD   pub use taskstore::Error;  (re-export)
    store.rs        MOD   add AsyncStore::list_tolerant<T>.
    reader.rs       MOD   closure bound: eyre::Result -> Result<_, Error>.
    writer.rs       MOD   same.
    Cargo.toml      MOD   drop eyre.

  All async-crate signatures keep their existing public surface; the `Result<T>`
  alias points to the re-exported Error.
```

### Why these placements

- **Corruption types in `taskstore-traits`.** Both `taskstore` and `taskstore-async` need to name them. Putting them in `traits` lets each crate import directly without one re-exporting from the other. The types are pure data (no I/O, no serde_json dep), which fits the crate's "lean, I/O-free" charter. The `From<serde_json::error::Category>` impl lives in `taskstore::error.rs`, where `serde_json` is already a dep, keeping `traits` clean.
- **`Error` in `taskstore`, not `taskstore-traits`.** `Error` carries `Sqlite(rusqlite::Error)`, `Serde(serde_json::Error)`, `Io(io::Error)`. Putting it in `traits` would force `rusqlite` and `serde_json` into the lean crate. `taskstore` is the lowest layer that already depends on those.
- **`match_filter` in `taskstore-traits/src/filter.rs`.** Pure function over `&HashMap<String, IndexValue>` and `&Filter`. No I/O. Sits next to `Filter`/`FilterOp` so the in-Rust mirror is co-located with the SQL representation it parallels.
- **`list_tolerant_at` as a free function in `taskstore/src/corruption.rs`.** This is the load-bearing primitive. It takes `(base_path: &Path, filters: &[Filter]) -> Result<ListResult<T>, Error>`. Both `Store::list_tolerant` (sync wrapper, one line) and `AsyncStore::list_tolerant` (async wrapper via `spawn_blocking`) call it. This mirrors the established pattern of `query::list_data_jsons` (one free function, two thin wrappers).
- **AsyncStore dispatches via `spawn_blocking` directly, not via reader pool or writer thread.** No SQLite connection is needed; consuming a reader-pool slot would block legitimate SQLite reads for nothing. The writer thread's queue is the wrong shape (would serialize JSONL reads behind writes). `read_jsonl_latest` already takes a shared `fs2` lock, so concurrent JSONL reads across spawn_blocking tasks are safe.

## Proposed Solution - Phase 1: Error Unification

### Type definition

`crates/taskstore/src/error.rs`:

```rust
use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("store I/O failure: {0}")]
    Io(#[from] std::io::Error),

    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    /// Produced only by AsyncStore when the writer-thread channel is closed.
    /// Sync code never produces this variant.
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
```

`StoreClosed` and `WalUnsupported` are carried in the unified enum even though sync `Store` cannot produce `StoreClosed`. This is a small architectural smell traded for one type, one Result alias, one mental model. If a future sync caller pattern-matches on `StoreClosed` and gets unreachable code, they can match exhaustively without a new error type. Splitting into a base+extension would be a separate refactor.

### Migration pattern for `.context(...)` sites

Today (eyre):

```rust
let file = File::open(path).context("Failed to open JSONL file")?;
```

After Phase 1 (typed):

```rust
let file = File::open(path)
    .map_err(|e| Error::Other(format!("Failed to open JSONL file {}: {e}", path.display())))?;
```

Or, where the context truly is just "I/O happened on this file," let the `From<io::Error>` pass through:

```rust
let file = File::open(path)?;   // io::Error auto-converts to Error::Io
```

Migration rule: preserve information, not phrasing. If today's `.context("...")` adds operational context the user reads when an error surfaces, capture it as `Error::Other(format!(...))`. If today's `.context("...")` is just restating "this was an I/O call," let the From impl through.

`eyre!("...")` sites become `Error::Other("...".to_string())`. `bail!("...")` sites become `return Err(Error::Other("...".to_string()))`.

### Phase 1 file scope

- `crates/taskstore/Cargo.toml`: remove `eyre = "0.6.12"`; add `thiserror = "2"`.
- `crates/taskstore/src/lib.rs`: declare `pub mod error;`; re-export `pub use error::{Error, Result};`.
- `crates/taskstore/src/error.rs`: new file with the enum above.
- `crates/taskstore/src/store.rs`: ~1700 lines. Replace `use eyre::{Context, Result, eyre}` with `use crate::error::{Error, Result}`. Walk every `.context(...)`, `eyre!(...)`, `bail!(...)`, `wrap_err(...)` site. ~37 sites across the crate.
- `crates/taskstore/src/jsonl.rs`: same conversion.
- `crates/taskstore/src/query.rs`: same conversion.
- `crates/taskstore/src/main.rs`, `bin/taskstore-merge.rs`: top-level uses eyre's anyhow-equivalent for return types; convert.
- `crates/taskstore/examples/01-10*.rs`: 10 examples; convert each.
- `crates/taskstore-async/Cargo.toml`: drop `eyre = "0.6"`.
- `crates/taskstore-async/src/error.rs`: delete.
- `crates/taskstore-async/src/lib.rs`: replace `mod error;` and `pub use error::Error;` with `pub use taskstore::{Error, Result};`. The crate-local `pub type Result<T>` alias stays; it now points to the re-exported Error.
- `crates/taskstore-async/src/reader.rs`: closure bound `FnOnce(&Connection) -> eyre::Result<T>` becomes `FnOnce(&Connection) -> Result<T>` where `Result` is the re-export. Drop the `From<eyre::Report> for Error` ergonomics.
- `crates/taskstore-async/src/writer.rs`: same.
- `crates/taskstore-async/tests/reader.rs`, `writer.rs`, `src/tests.rs`: any error-construction or matching updates.

### Phase 1 acceptance

- `cargo build -p taskstore` succeeds.
- `cargo build -p taskstore-async` succeeds.
- `cargo test -p taskstore` and `-p taskstore-async`: existing tests pass unchanged in semantics; only error type assertions (if any) update.
- `cargo run --example 01_basic_crud` etc.: all 10 examples run.
- `cargo deny` / `cargo tree | grep eyre` returns empty across the workspace.

No new feature is added in Phase 1. It is a pure type-flip refactor. Behavior is identical.

## Proposed Solution - Phase 2: Corruption Types and Tolerant JSONL Reader

### Public types

`crates/taskstore-traits/src/corruption.rs`:

```rust
use std::io::ErrorKind;
use std::path::PathBuf;

/// Result of a corruption-aware bulk read.
///
/// `records` is the live record set after last-write-wins-per-id and tombstone
/// filtering. `corruption` lists every line that could not be turned into a
/// usable record, with file/line attribution.
///
/// Marked `#[non_exhaustive]` so future fields (e.g., an `omitted_count: u64`
/// for a capped-corruption mode - see Open Questions Q1) can be added without
/// a breaking change. Construct via `ListResult::new` rather than struct
/// literal; cross-crate callers (including `taskstore::list_tolerant_at`)
/// cannot use literal-construction on a `#[non_exhaustive]` type.
#[derive(Debug)]
#[non_exhaustive]
pub struct ListResult<T> {
    pub records: Vec<T>,
    pub corruption: Vec<CorruptionEntry>,
}

impl<T> ListResult<T> {
    pub fn new(records: Vec<T>, corruption: Vec<CorruptionEntry>) -> Self {
        Self { records, corruption }
    }
}

#[derive(Debug, Clone)]
pub struct CorruptionEntry {
    pub file: PathBuf,
    /// 1-indexed line number within the JSONL file.
    pub line: u64,
    /// Original line bytes (`String::from_utf8_lossy` for non-UTF-8),
    /// truncated at a UTF-8 char boundary at or below 4096 bytes (using
    /// `str::floor_char_boundary`, stable since Rust 1.80) with the literal
    /// "...[truncated]" appended when the original exceeded that size.
    /// Naive byte slicing (`&s[..4096]`) panics on multi-byte boundaries
    /// and is explicitly forbidden in the truncation helper.
    /// For TypeMismatch entries, this is the parsed `serde_json::Value`
    /// re-serialized; the original line text is no longer in scope at
    /// type-deserialization time.
    pub raw: String,
    pub error: CorruptionError,
}

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

    /// `BufReader::read_line` returned Err for this line (partial read,
    /// NUL byte, transient FS error, etc.).
    Io { kind: ErrorKind },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Syntax,
    Eof,
    Data,
    Io,
}
```

`Category` has no `From<serde_json::error::Category>` impl in `taskstore-traits` (no serde_json dep there). Rust's orphan rule forbids placing the `impl From<serde_json::error::Category> for Category` in `taskstore` either - both the trait (`From` parameterised over a foreign type) and the type (`Category`, defined in `taskstore-traits`) are foreign to `taskstore`. The conversion is therefore a free function in `taskstore::error`:

```rust
pub(crate) fn category_from_serde_json(c: serde_json::error::Category) -> Category {
    use serde_json::error::Category as S;
    match c {
        S::Io => Category::Io,
        S::Syntax => Category::Syntax,
        S::Data => Category::Data,
        S::Eof => Category::Eof,
    }
}
```

`jsonl::read_jsonl_latest_with_corruption` calls this when constructing `CorruptionError::InvalidJson`. If a future caller outside `taskstore` needs the conversion, the function can be made `pub`; today it is `pub(crate)`.

### `match_filter` helper

`crates/taskstore-traits/src/filter.rs` adds:

```rust
use std::collections::HashMap;
use crate::record::IndexValue;

/// In-Rust mirror of the SQL produced by `list_data_jsons`. Returns true iff
/// the record's indexed fields satisfy `f`.
///
/// `Contains` is ASCII-case-insensitive (mirrors SQLite `LIKE`'s default).
/// Non-ASCII case-folding is intentionally not handled - SQLite's `LIKE` does
/// not handle it either, and parity is more valuable than completeness.
pub fn match_filter(fields: &HashMap<String, IndexValue>, f: &Filter) -> bool {
    let Some(actual) = fields.get(&f.field) else { return false; };
    // Type guard: the SQL path keys off field_value_str/_int/_bool columns; a
    // type mismatch yields no matches there. Mirror that.
    match (actual, &f.value) {
        (IndexValue::String(a), IndexValue::String(b)) => match f.op {
            FilterOp::Eq => a == b,
            FilterOp::Ne => a != b,
            FilterOp::Gt => a > b,
            FilterOp::Lt => a < b,
            FilterOp::Gte => a >= b,
            FilterOp::Lte => a <= b,
            FilterOp::Contains => a.to_ascii_lowercase().contains(&b.to_ascii_lowercase()),
        },
        (IndexValue::Int(a), IndexValue::Int(b))   => match f.op { /* numeric ops; Contains -> false */ },
        (IndexValue::Bool(a), IndexValue::Bool(b)) => match f.op { /* Eq/Ne only; others -> false */ },
        _ => false,
    }
}
```

Test contract pins SQL-vs-Rust parity for every `FilterOp` variant on every `IndexValue` shape, including ASCII-case-insensitivity for `Contains`.

### Tolerant JSONL reader

`crates/taskstore/src/jsonl.rs` extracts a private helper:

```rust
struct ParsedLine {
    line_no: u64,
    raw: String,                              // original bytes, lossy UTF-8 (truncated)
    outcome: Result<(String, Value), CorruptionError>,  // (id, value) or per-line error
}

/// Streaming helper. Opens `path` under a shared `fs2` lock, then calls
/// `for_each(parsed)` once per non-empty line. The callback receives an
/// owned `ParsedLine` and decides what to keep; the value is dropped when
/// the callback returns. **The helper never materializes a `Vec<ParsedLine>`** -
/// both wrappers stream-reduce inside the callback so peak memory is
/// proportional to unique records, not total lines.
fn for_each_jsonl_line<F>(path: &Path, for_each: F) -> Result<()>
where
    F: FnMut(ParsedLine);
```

Both wrappers consume the streaming helper. Their public signatures are unchanged from today's `read_jsonl_latest` plus one new entry point:

```rust
/// Today's behavior. sync() and any other caller that does not care about
/// corruption keeps using this. Public signature unchanged. Implementation
/// is now: call `for_each_jsonl_line`, warn-log on `outcome.is_err()` in
/// the callback, otherwise stream-reduce into HashMap<String, Value> with
/// LWW-by-updated_at. Obsolete Values are overwritten in place; the
/// `ParsedLine` falls out of scope each iteration.
pub fn read_jsonl_latest(path: &Path) -> Result<HashMap<String, Value>>;

/// New entrypoint. Same streaming reduction, but the callback also pushes
/// CorruptionEntry into the corruption Vec on the Err arm and carries
/// `line_no` alongside the LWW-winning Value. Memory grows only with K
/// (corrupt lines), not with total writes. Each map value carries the
/// line number of the LWW-winning line so downstream typed deserialization
/// can attribute a TypeMismatch back to a specific line.
pub fn read_jsonl_latest_with_corruption(
    path: &Path,
) -> Result<(HashMap<String, (u64, Value)>, Vec<CorruptionEntry>)>;
```

**Architect-flagged failure mode (avoided by the closure contract):** an earlier draft of this design had `for_each_jsonl_line` return `Vec<ParsedLine>`. That would have introduced an O(total-lines-including-superseded-versions) memory regression in `sync()`'s hot path - dramatically worse than today's O(unique-records) profile. The streaming closure shape is non-negotiable for that reason.

The line-number-bearing return type is unique to the with-corruption variant. `read_jsonl_latest`'s existing signature is preserved; `sync()` does not learn about line numbers and pays no extra memory cost. The 8 bytes-per-record overhead is paid only by `list_tolerant` callers.

The existing `test_read_jsonl_malformed_line` continues to pass against `read_jsonl_latest` unchanged.

### `list_tolerant_at` free function

`crates/taskstore/src/corruption.rs`:

```rust
use std::path::Path;
use taskstore_traits::{CorruptionEntry, CorruptionError, Filter, ListResult, Record, match_filter};

use crate::error::Result;
use crate::jsonl::read_jsonl_latest_with_corruption;

pub fn list_tolerant_at<T: Record>(base_path: &Path, filters: &[Filter]) -> Result<ListResult<T>> {
    let path = base_path.join(format!("{}.jsonl", T::collection_name()));
    let (map, mut corruption) = read_jsonl_latest_with_corruption(&path)?;

    let mut records = Vec::new();
    for (id, (line_no, value)) in map {
        // Tombstone filter: matches sync()'s behavior. A {"deleted": true, ...}
        // row that parses cleanly is dropped from records and is NOT corruption.
        if value.get("deleted").and_then(|v| v.as_bool()) == Some(true) {
            continue;
        }

        match serde_json::from_value::<T>(value.clone()) {
            Ok(record) => {
                if filters.iter().all(|f| match_filter(&record.indexed_fields(), f)) {
                    records.push(record);
                }
            }
            Err(e) => {
                corruption.push(CorruptionEntry {
                    file: path.clone(),
                    line: line_no,
                    raw: truncate_for_raw(&value.to_string()),
                    error: CorruptionError::TypeMismatch { msg: e.to_string() },
                });
            }
        }
    }

    Ok(ListResult::new(records, corruption))
}
```

`truncate_for_raw` truncates at a UTF-8 char boundary at or below 4096 bytes and appends `"...[truncated]"` if the input exceeded that size. Implementation:

```rust
fn truncate_for_raw(s: &str) -> String {
    if s.len() <= 4096 {
        return s.to_string();
    }
    let cut = s.floor_char_boundary(4096);
    format!("{}...[truncated]", &s[..cut])
}
```

Used by both the JSONL helper (for original line text) and the type-mismatch path (for the re-serialized Value). Naive byte slicing (`&s[..4096]`) panics when the 4096th byte falls inside a multi-byte UTF-8 scalar value (e.g., a line padded with emoji or Cyrillic) and is explicitly forbidden.

### Phase 2 acceptance

- `cargo build` succeeds across the workspace.
- New unit tests pass:
  - `taskstore_traits::filter::tests::match_filter_*` covers SQL-vs-Rust parity for every `FilterOp` variant.
  - `taskstore::jsonl::tests::test_read_jsonl_latest_with_corruption_*` covers each `CorruptionError` variant being produced for the right input.
- No public surface change at the `Store` or `AsyncStore` layer yet (Phase 3 adds the methods).

## Proposed Solution - Phase 3: Public Surface on Both Stores

### Sync wrapper

`crates/taskstore/src/store.rs` adds:

```rust
impl Store {
    /// Corruption-aware bulk read. Reads JSONL directly (bypasses SQLite),
    /// applies tombstone filtering and `match_filter` in Rust, and returns
    /// every line that could not be turned into a usable record.
    ///
    /// Re-parses JSONL on every call. Not a hot read - prefer `list<T>` for
    /// frequent queries and reserve `list_tolerant<T>` for audit/sweep paths.
    /// Does not trigger sync() and does not write to SQLite.
    pub fn list_tolerant<T: Record>(&self, filters: &[Filter]) -> Result<ListResult<T>> {
        crate::corruption::list_tolerant_at(&self.base_path, filters)
    }
}
```

### Async wrapper

`crates/taskstore-async/src/store.rs` adds:

```rust
impl AsyncStore {
    /// Async corruption-aware bulk read. Dispatches the JSONL parse to
    /// `tokio::task::spawn_blocking` directly, bypassing both the writer
    /// thread (no need to serialize behind writes) and the reader pool
    /// (no SQLite connection needed). The underlying file lock in
    /// `read_jsonl_latest_with_corruption` is the same shared `fs2` lock
    /// the existing `read_jsonl_latest` uses.
    #[tracing::instrument(level = "debug", skip_all, fields(
        collection = T::collection_name(),
        filter_count = filters.len(),
    ))]
    pub async fn list_tolerant<T: Record>(&self, filters: &[Filter]) -> Result<ListResult<T>> {
        let base_path = self.base_path.clone();
        let filters = filters.to_vec();
        tokio::task::spawn_blocking(move || {
            taskstore::list_tolerant_at::<T>(&base_path, &filters)
        })
        .await
        .map_err(|e| Error::Other(format!("list_tolerant task panicked: {e}")))?
    }
}
```

`crates/taskstore-async/src/lib.rs` re-exports the corruption types:

```rust
pub use taskstore_traits::{Category, CorruptionEntry, CorruptionError, ListResult};
```

### Behavior specifications

| Input | Output |
|-------|--------|
| Well-formed JSONL, mix of records and tombstones | `Ok(ListResult { records: live_records_after_lww_and_tombstone, corruption: [] })` |
| Line fails `serde_json::from_str` | `CorruptionEntry { error: InvalidJson { msg, category }, .. }`. Skipped from `records`. |
| Line is valid JSON but lacks `id` | `CorruptionEntry { error: MissingId, .. }`. Skipped from `records`. |
| Line is valid JSON, has `id`, fails `T` deserialize | `CorruptionEntry { error: TypeMismatch { msg }, .. }`. Skipped from `records`. |
| `BufReader::lines()` yields `Err` for one line | `CorruptionEntry { error: Io { kind }, raw: String::new(), line: <count of lines yielded so far + 1>, .. }`. The `lines()` iterator yields whole lines or errors; there is no partial-line bytes to put in `raw`. |
| `{"deleted": true, ...}` well-formed | Filtered from `records`. Not corruption. |
| Empty JSONL file | `Ok(ListResult { records: [], corruption: [] })` |
| Missing JSONL (collection never written) | Same as empty. |
| Unreadable JSONL (perms, FS error on file open) | `Err(Error::Io(...))`. No `ListResult`. File-open failures are not per-line; they propagate as the outer error. |
| `updated_at` missing or non-int on otherwise valid line | Existing default-to-0 behavior preserved. Not corruption. |
| Line bytes are not valid UTF-8 | Treated as `InvalidJson` (parse fails on invalid UTF-8). `raw` populated via `String::from_utf8_lossy`. |
| Tombstone line that fails JSON parse | `InvalidJson` corruption. The id it was meant to tombstone is unknown, so the prior live record (if any) stays in `records`. **This is identical to today's `sync()` behavior** - a corrupt tombstone never made it into SQLite either, so today's `list<T>` already returns the un-tombstoned record with no diagnostic. The improvement here is *visibility*: `list_tolerant` exposes the failed tombstone as a `CorruptionEntry`, where today the same scenario silently leaves the record alive. Auditors who see a corrupt tombstone for an id they expected to be deleted should remediate (rewrite the tombstone, run compaction); they should not assume the record is intentionally alive. |
| Filter references field absent from `T::indexed_fields()` | `match_filter` returns false. Mirrors SQL `EXISTS` semantics. |
| Filter `value` type does not match indexed field's `IndexValue` variant | `match_filter` returns false. Mirrors SQL path's per-column typing. |
| Filter op is `Contains` | ASCII-case-insensitive substring match (lowercase both sides, then `str::contains`). Identical filter returns identical record set via `list<T>` (SQLite `LIKE`) and `list_tolerant<T>` (`match_filter`). |
| Two valid lines for id X: line 10 valid as `T`, line 20 valid JSON with later `updated_at` but invalid as `T` | LWW selects line 20's `Value`. Typed deserialize fails. id X surfaces as `TypeMismatch` corruption (with `line == 20`); the older valid record from line 10 is **not** in `records`. Matches `sync()`'s existing behavior. |

### Counting contract

- `corruption.len() == K` where K is the number of malformed lines (or LWW-winning typed-mismatch ids).
- `records.len()` is **not** asserted as N - K. LWW-per-id and tombstone filtering still apply on top.
- `CorruptionEntry`s are not deduplicated. Two malformed lines that happen to share content each produce their own entry.

### `list<T>` behavior is unchanged in semantics

After Phase 1 the return type is `Result<Vec<T>, Error>` instead of `eyre::Result<Vec<T>>`, but the records returned are identical to today: SQLite-backed, no corruption signal. The new signal lives only in `list_tolerant`.

## Testing Strategy

Tests are added per phase. Each phase's PR is green before the next merges.

### Phase 1 tests

- All existing tests pass with mechanical `eyre::Result` -> `Result<_, Error>` updates.
- Spot-check: a removed eyre `.context("Failed to open X")` produces an `Error::Other("Failed to open X: <io detail>")` or an `Error::Io(_)` (depending on chosen migration); test that the surfaced error string contains the expected operational context.

### Phase 2 tests

In `taskstore-traits/src/filter/tests.rs` (extracted to a `tests` submodule per the rules):

- `match_filter_string_eq_ne_lt_gt_gte_lte`: equality and ordering on `IndexValue::String`.
- `match_filter_int_*`, `match_filter_bool_eq_ne`: same on `Int`/`Bool`.
- `match_filter_contains_ascii_case_insensitive`: `Contains "ACTIVE"` matches indexed value `"active"` and vice versa.
- `match_filter_field_absent_returns_false`: SQL parity for missing field.
- `match_filter_type_mismatch_returns_false`: SQL parity for `Int` filter on `String`-indexed field.

In `taskstore/src/jsonl/tests.rs`:

- `test_read_jsonl_latest_with_corruption_*`: one test per `CorruptionError` variant (`InvalidJson` with each `Category`, `MissingId`, `Io`).
- `test_read_jsonl_latest_with_corruption_lww`: line 10 valid + line 20 valid same id later updated_at => map carries `(20, line20_value)`, no corruption.
- `test_read_jsonl_latest_unchanged_silent_skip`: existing `test_read_jsonl_malformed_line` re-runs against the refactored implementation and still returns 2 records, no Err, no panic.

### Phase 3 tests

In `taskstore/src/store/tests.rs` (or a new `store_list_tolerant.rs` module):

- `list_tolerant_100_lines_3_corrupted_one_each_variant`: 100 lines, one Syntax-error, one EOF-truncation (last line cut off), one MissingId. `corruption.len() == 3`; each entry carries `file`, `line`, `raw`, `error`.
- `list_tolerant_type_mismatch_lww`: line 10 valid as `T`, line 20 same id, later `updated_at`, valid JSON, invalid for `T`. `corruption[].line == 20` and `corruption[].error` is `TypeMismatch`. `records` does not contain id X.
- `list_tolerant_filter_pushdown_parity`: write 100 records spanning two `status` values; identical `Filter::eq("status", "active")` returns the same set via `Store::list<T>` (post-sync, SQLite) and `Store::list_tolerant<T>` (`match_filter`).
- `list_tolerant_contains_case_parity`: write records whose indexed `status` differs only in case; `Filter::contains("status", "ACTIVE")` returns the same set via both methods.
- `list_tolerant_unreadable_jsonl_returns_err`: chmod 000 (or simulate via temp dir); `list_tolerant` returns `Err(Error::Io(_))`, no `ListResult`.
- `list_tolerant_empty_returns_empty`: empty file -> `Ok(ListResult { records: [], corruption: [] })`.
- `list_tolerant_missing_returns_empty`: nonexistent JSONL -> same.
- `list_tolerant_tombstones_filtered_not_counted`: 100 lines including 5 well-formed tombstones; tombstones absent from `records`, absent from `corruption`.
- `list_tolerant_corrupt_tombstone_leaves_target_alive`: write a valid record then a tombstone-shaped line that fails to parse; `corruption` contains 1 entry, `records` still contains the original record.
- `list_tolerant_raw_truncation_marker`: write a >4 KB malformed line; `corruption[0].raw.ends_with("...[truncated]")` and `raw.len() <= 4096 + "...[truncated]".len()`.
- `list_tolerant_raw_truncation_multibyte_no_panic`: write a malformed line whose 4096th byte falls inside a multi-byte UTF-8 scalar (e.g. emoji-padded line so byte 4096 lands mid-codepoint); `truncate_for_raw` must not panic and must return a valid UTF-8 String. This is the regression test for the architect-flagged truncation pitfall.
- `list_tolerant_after_sync_list_returns_silent_skip`: same corrupted JSONL; run `sync()`, then `list<T>` returns `Ok(records_present)` (existing silent-skip preserved). Corruption signal is unique to `list_tolerant`.

In `taskstore-async/tests/`:

- `test_async_list_tolerant_basic`: same scenario as `list_tolerant_100_lines_3_corrupted_one_each_variant`, exercised under `#[tokio::test]`. Verify the spawn_blocking dispatch returns the same shape and content as the sync path.
- `test_async_list_tolerant_concurrent_with_writes`: a write task on `AsyncStore` runs concurrent with `list_tolerant`; both complete (shared file lock allows concurrent reads while writes block; verify the writer is not blocked by `list_tolerant` either - the dispatch goes through spawn_blocking, not the writer thread).

Round-trip tests use `tempfile::TempDir` and the public `Store::open_at` / `AsyncStore::open_at` constructors. Corrupt-line tests write JSONL bytes directly with `std::fs::write` to bypass the writer's validation.

## Implementation Plan

### Phase 1: Error unification
**Model:** sonnet

1. Add `crates/taskstore/src/error.rs` with the enum and `Result` alias above. Add `impl From<serde_json::error::Category> for Category` (forward-declared; the `Category` type lives in `taskstore-traits` after Phase 2 - this impl moves to `taskstore-traits`'s consumer crate at that point. For Phase 1 in isolation, omit the `Category` impl entirely; it is added in Phase 2 alongside the type it converts to.).
2. `pub mod error;` in `lib.rs`; `pub use error::{Error, Result};`.
3. Replace `eyre = "0.6.12"` with `thiserror = "2"` in `crates/taskstore/Cargo.toml`.
4. Convert `store.rs`, `jsonl.rs`, `query.rs`: each `use eyre::...` becomes `use crate::error::{Error, Result}`. Walk every `.context(...)`, `eyre!(...)`, `bail!(...)`, `wrap_err(...)` site; convert per the migration pattern above.
5. Convert `main.rs`, `bin/taskstore-merge.rs`, `examples/01-10*.rs`.
6. `crates/taskstore-async/Cargo.toml`: drop `eyre`.
7. `crates/taskstore-async/src/error.rs`: delete.
8. `crates/taskstore-async/src/lib.rs`: replace `mod error;` and `pub use error::Error;` with `pub use taskstore::{Error, Result};`. Crate-local `Result<T>` alias becomes `pub type Result<T> = taskstore::Result<T>;` (or remove the alias and import directly).
9. `crates/taskstore-async/src/reader.rs`, `writer.rs`: closure bound and any internal `eyre::Report` references convert.
10. Update tests where they reference types removed (e.g., `From<eyre::Report>`).
11. `cargo build && cargo test` passes across the workspace; `cargo tree | grep eyre` returns nothing.

### Phase 2: Corruption types and tolerant JSONL reader
**Model:** sonnet

1. Add `crates/taskstore-traits/src/corruption.rs` with `ListResult<T>`, `CorruptionEntry`, `CorruptionError`, `Category`. Add `pub fn match_filter(...)` to `crates/taskstore-traits/src/filter.rs`.
2. Re-export from `crates/taskstore-traits/src/lib.rs`.
3. Add `taskstore-traits = { path = "../taskstore-traits" }` is already present; no Cargo.toml change.
4. Add `pub(crate) fn category_from_serde_json` in `crates/taskstore/src/error.rs`. (Not a `From` impl - orphan rule forbids it; see the type-definition section.)
5. Refactor `crates/taskstore/src/jsonl.rs`: extract the parse loop into a private streaming helper `for_each_jsonl_line<F: FnMut(ParsedLine)>`. `read_jsonl_latest` becomes a thin wrapper whose callback warn-logs and discards the Err arm and stream-reduces into `HashMap<String, Value>`. Add `read_jsonl_latest_with_corruption`, whose callback stream-reduces into `HashMap<String, (u64, Value)>` and pushes Err entries into a `Vec<CorruptionEntry>`. **Neither variant ever materializes a `Vec<ParsedLine>`** - that shape would regress `sync()`'s memory profile and is forbidden.
6. Add `crates/taskstore/src/corruption.rs` with `list_tolerant_at<T>` and `truncate_for_raw`.
7. Re-export `taskstore_traits::{Category, CorruptionEntry, CorruptionError, ListResult, match_filter}` from `crates/taskstore/src/lib.rs`.
8. Add Phase 2 tests (filter parity, JSONL helper variants).
9. `cargo build && cargo test` passes.

### Phase 3: Public methods and async surface
**Model:** sonnet

1. Add `Store::list_tolerant<T>` in `crates/taskstore/src/store.rs`.
2. Add `AsyncStore::list_tolerant<T>` in `crates/taskstore-async/src/store.rs` with `tracing::instrument`. Re-export corruption types from `crates/taskstore-async/src/lib.rs`.
3. Add Phase 3 tests (sync and async).
4. Add an example: `crates/taskstore/examples/11_corruption_audit.rs` showing `list_tolerant::<T>(&[])` over a deliberately-corrupted store.
5. `cargo build && cargo test` passes; example runs.

## Alternatives Considered

### A1: Mutate `list<T>` to return corruption alongside records

Change `list<T>` to return `(Vec<T>, Vec<CorruptionEntry>)` or a wrapping type.

- **Pros:** One method instead of two.
- **Cons:** Breaking change to a hot-path API. Forces every caller to handle a corruption vec they may not care about. Forces `list` to read JSONL directly, losing the SQLite index path.
- **Why not chosen:** The signal is needed by a small number of audit callers; the hot path should not pay for it.

### A2: `last_sync_corruption()` accessor on `Store`

Have `sync()` remember what it skipped; expose `Store::last_sync_corruption() -> &[CorruptionEntry]`.

- **Pros:** Auditor reads SQLite (fast); corruption signal decoupled from any single read.
- **Cons:** Stateful. `sync()` only runs when `is_stale()` returns true, so the accessor reflects "what was skipped at last sync," not "what is corrupt right now." A caller that wants ground truth would have to force a sync first.
- **Why not chosen:** Adds complexity for a use case JSONL-direct read covers more directly. Could still be added later as `list_tolerant_cached` if profiling shows the JSONL re-parse hurts.

### A3: Per-collection methods (`list_plans_tolerant` etc.)

- **Cons:** TaskStore's API is generic over `T: Record`; per-collection variants would invert the API shape.

### A4: Streaming iterator

`iter_records_tolerant() -> impl Iterator<Item = (u64, Result<T, CorruptionError>)>`.

- **Cons:** LWW-per-id requires seeing every line before yielding any record - a streaming iterator either gives that up (different semantics from `list`) or buffers internally (not really streaming).

### A5: Defer error unification, keep eyre on sync

Land `list_tolerant` on `eyre::Result<ListResult<T>>` in sync, async-Error in async. Refactor errors later.

- **Pros:** Smaller PR.
- **Cons:** The new method debuts on the wrong error type for the consumer (Loopr) and gets retrofitted later. Two breaking changes for downstream instead of one.
- **Why not chosen:** Error type and `list_tolerant` ship in the same release; one breaking change.

### A6: Split error into base sync + async-extension

`taskstore::Error` (no `StoreClosed`), `taskstore_async::AsyncError` wrapping it with the channel-closed variant.

- **Pros:** Sync code is statically prevented from constructing `StoreClosed`. Domain isolation: each crate's error type carries only the failure modes that crate produces.
- **Cons:** Every consumer that touches both `Store` and `AsyncStore` (Loopr, the eventual driver of this work) ends up with **two** error types in flight. They either define their own consumer-side wrapping enum (`enum LooprError { Sync(taskstore::Error), Async(taskstore_async::AsyncError) }` plus matching `From` impls) or duplicate match arms wherever they handle errors from both surfaces. Both options push complexity outward, off taskstore and onto every consumer. The "domain isolation" win is internal aesthetic; the cost is paid by everyone downstream. Additionally, a `From<taskstore::Error> for taskstore_async::AsyncError` shim is needed at every async->sync boundary inside `taskstore-async` itself - the writer thread, the reader pool, every method on `AsyncStore` that delegates to the sync `Store`.
- **Why not chosen:** One enum optimizes for the consumer surface. `StoreClosed`'s presence in sync code is documented and not exposed via any sync return path that would surprise a sync-only caller (sync APIs that internally never produce it cannot be coerced into producing it). If a future audit shows the unified enum causes real consumer confusion, the split can be done as a follow-up; this design defers A6 on consumer-ergonomics grounds rather than foreclosing it.

## Technical Considerations

### Dependencies

- `taskstore` adds `thiserror = "2"`, drops `eyre`.
- `taskstore-async` drops `eyre`.
- `taskstore-traits` adds nothing (no I/O, no error type).

### Performance

`list_tolerant` re-parses the full JSONL file on every call. Cost scales linearly with file size. This is intentional: callers opt into the cost when they want detection. The hot path (`list<T>`) is untouched and continues to use the SQLite index.

Memory: `read_jsonl_latest_with_corruption` materializes a `HashMap<String, (u64, Value)>` of every live record plus a `Vec<CorruptionEntry>` (typically empty). Peak memory is proportional to collection size. The 8 bytes-per-record overhead for line numbers is paid only by `list_tolerant` callers; `sync()` continues to use the line-number-free `read_jsonl_latest`.

Per-record allocator pressure during filtering: `match_filter` is called once per surviving record; each call invokes `Record::indexed_fields()` which allocates an owned `HashMap`. The SQL path avoids this allocation by pushing the comparison into SQLite. For an audit sweep this is the cost of "JSONL-direct, no SQL pushdown" - acceptable. If profiling shows it's the dominant cost, a future `Record::match_field(&self, field, value, op) -> bool` default method can avoid the HashMap; that change is additive and out of scope here.

### Security

`raw` returns the bytes of a corrupt line, truncated to 4 KB. If a JSONL collection ever contains user-supplied content, callers logging `CorruptionEntry` should treat `raw` with the same care as any other user-content field. Documented on the type.

### Concurrency

`read_jsonl_latest_with_corruption` takes a shared `fs2` lock identical to the existing `read_jsonl_latest`. Concurrent writers (exclusive lock) block `list_tolerant` and vice versa. This matches the existing read discipline; no new contention shape.

`AsyncStore::list_tolerant` dispatches via `tokio::task::spawn_blocking` and does **not** consume a reader-pool slot or queue behind the writer thread. SQLite reads on the same `AsyncStore` continue at full concurrency; writes continue at full throughput.

### Rollout Plan

Single coordinated breaking-change release: `taskstore 0.6.0` and `taskstore-async 0.3.0`. Phase 1 in its own PR, Phase 2 in its own PR, Phase 3 in its own PR. All three merged before the version bump tag. Loopr pins to the new versions in one go.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| `serde_json::error::Category` semver coupling if re-exported directly | Medium | Low | taskstore-owned `Category` mirror with `From<serde_json::error::Category>` |
| Two parallel JSONL parsers drift over time | Low | Medium | Share a single iteration loop; strict and tolerant variants are thin wrappers |
| Caller misuses `list_tolerant` as a hot path | Low | Medium | Document on the method that this re-parses JSONL on every call; provide `list<T>` as the hot-path alternative |
| `raw` field exposes sensitive content to logs | Low | Low | Document on the type; truncate to 4 KB; let callers redact |
| Phase 1 missing a `.context(...)` site loses operational message | Medium | Low | Migration is mechanical; CI grep for `eyre`/`.context`/`.wrap_err` after Phase 1 must return nothing in the workspace |
| `Error::Other(String)` becomes the lazy default for new error sites | Medium | Low | New error sites should add typed variants when the failure shape is meaningful; `Other(String)` is for context messages, not for skipping the type design |
| Async `list_tolerant` panics in `spawn_blocking` produce confusing errors | Low | Low | Surface as `Error::Other("list_tolerant task panicked: {e}")`; consistent with existing `AsyncStore::open_at` panic mapping |
| Auditor misinterprets a corrupt tombstone as live-by-design | Low | Medium | Document explicitly on the tombstone behavior row that a `CorruptionError::InvalidJson` whose contents were *meant* to be a tombstone leaves the target record in `records`; auditors should remediate (rewrite the tombstone), not assume the record is intentionally alive |
| `list_tolerant` parse holds shared `fs2` lock long enough to delay writes on the same collection | Medium | Low | Same lock shape as today's `read_jsonl_latest` (called by `sync()`); `list_tolerant` adds a new caller of an existing pattern, not a new pattern. Documented as audit/sweep, not hot read. Future mitigation if needed: chunked parse with periodic lock release |

## Open Questions

### Q1: Should the corruption Vec be capped?

The current design returns every `CorruptionEntry` produced during the parse - the Vec is unbounded. Architect review surfaced the failure mode: a long-running daemon that, over time, has written a large number of malformed lines to one collection produces a `list_tolerant` call that loads every one of them into memory in a single audit pass.

Two options:

- **(a) Keep unbounded.** Match the framing: `list_tolerant` is an audit/sweep tool, not a hot path; if you have millions of corrupt lines, the bigger problem is the JSONL itself. Document the memory shape on the method.
- **(b) Cap at N entries with `omitted_count: u64` field on `ListResult`.** Add a module-level `LIST_TOLERANT_MAX_CORRUPTION_ENTRIES: usize = 10_000`; once reached, stop pushing entries and increment the counter. Caller can re-run focused audits if needed.

Recommendation: ship v0.6.0 with **(a)**, revisit if the unbounded shape causes a real incident. **(b)** is additive and can be introduced later by adding a field to `ListResult` (backwards-compatible if `ListResult` is annotated `#[non_exhaustive]` from day one - which this design specifies in the type definition section as a low-cost safeguard against this exact future change).

### Decisions already made (recorded for reference)

- Error type: `taskstore::Error` (single enum, async-only `StoreClosed` variant documented).
- File: `crates/taskstore/src/error.rs`.
- Drop `eyre` from both `taskstore` and `taskstore-async`.
- Corruption types and `match_filter`: `taskstore-traits`.
- `From<serde_json::error::Category> for Category`: free function in `taskstore` (orphan rule forbids the impl).
- Phase order: error unification first (Phase 1), corruption types second (Phase 2), public methods third (Phase 3).
- Async dispatch: `tokio::task::spawn_blocking` directly, not reader pool, not writer thread.
- JSONL parse helper is streaming (closure-based), never returns a `Vec<ParsedLine>` - protects `sync()`'s memory profile.
- `truncate_for_raw` uses `floor_char_boundary`, not byte slicing - no UTF-8 panics.

## References

- `crates/taskstore/src/jsonl.rs:36` - existing `read_jsonl_latest` (the function being refactored).
- `crates/taskstore/src/jsonl.rs:181` - existing `test_read_jsonl_malformed_line` (current tolerant behavior; preserved).
- `crates/taskstore/src/store.rs:335` - existing `Store::list<T>` (the read path being supplemented, not modified in semantics).
- `crates/taskstore/src/query.rs:39` - existing `query::list_data_jsons` (the SQL filter compiler whose semantics `match_filter` mirrors).
- `crates/taskstore-async/src/error.rs` - existing `taskstore_async::Error` (moves to `taskstore::error` in Phase 1).
- `crates/taskstore-async/src/store.rs:184` - existing `AsyncStore::list<T>` (style reference for the async wrapper).
- `crates/taskstore-async/src/reader.rs:50` - existing `ReaderPool::run` (the closure-bound site that needs adjusting in Phase 1).
- `docs/design/2026-04-13-create-many.md` - prior additive method on `Store` (style reference for additive design).
