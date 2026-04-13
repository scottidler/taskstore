# Design Document: `create_many` Batch Record Creation

**Author:** Scott Idler
**Date:** 2026-04-13
**Status:** Implemented
**Review Passes Completed:** 5/5

## Summary

Add a `create_many<T: Record>(&mut self, records: Vec<T>) -> Result<Vec<String>>` method to `Store` that writes a batch of same-type records with minimal overhead. JSONL gets a single `write_all` call (one lock, one fsync), and SQLite wraps all inserts + index updates in one transaction. Validation is all-or-nothing: if any record fails, nothing is written. This eliminates the per-record lock-acquire/release overhead that loopr's `persist_hierarchy` currently pays when creating Plan + Spec + Phase + Work records in a loop.

## Problem Statement

### Background

TaskStore uses a dual-storage architecture: JSONL files are the source of truth (append-only, git-friendly), and SQLite is a derived read cache. Every write goes through `Store::create`, which:

1. Opens the JSONL file, acquires an exclusive `fs2` file lock, writes one JSON line, calls `sync_all`, releases the lock (`store.rs:404-424`)
2. Opens a SQLite transaction, does `INSERT OR REPLACE` into `records`, deletes + reinserts indexes, commits (`store.rs:188-215`)

This is correct for single-record writes. But loopr's decomposer creates hierarchies of 10-30+ records at once (1 Plan + N Specs + N Phases + N Works). Today, `persist_hierarchy` (`loopr/src/daemon/handlers/doc.rs:358-408`) calls `store.create()` in a loop - each call independently acquires the file lock, serializes, writes one line, syncs, releases, then opens+commits a SQLite transaction.

### Problem

The per-record overhead compounds:
- **N file lock round-trips** instead of 1
- **N `sync_all` fsync calls** instead of 1
- **N SQLite transactions** instead of 1
- **No batch atomicity** - if the process crashes after record 5 of 20, JSONL has a partial batch with no way to identify or roll back the incomplete set

### Goals

- Single JSONL write for the entire batch (one lock acquisition, one `write_all`, one `sync_all`)
- Single SQLite transaction for the entire batch (one `BEGIN`/`COMMIT`)
- All-or-nothing validation: if any record fails validation, nothing is written
- Best-effort JSONL atomicity: a single `write_all` call minimizes the crash window, but POSIX does not guarantee a single write is atomic for regular files (see Atomicity Note below)
- Homogeneous batches only (`Vec<T>` where all records share one `collection_name()`)
- Return the list of created IDs in insertion order

### Non-Goals

- **Heterogeneous batch writes** - loopr's `persist_hierarchy` creates 4 different types (Plan, Spec, Phase, Work). The caller will issue one `create_many` call per type, not one call for all types. This is intentional - mixing types in a single call would require trait objects or an enum wrapper, adding complexity for marginal benefit. Four `create_many` calls (4 locks, 4 transactions) is a huge improvement over 20+ individual `create` calls.
- **Cross-type atomicity** - if Specs succeed but Phases fail, the caller (loopr) handles the partial state. True cross-type atomicity would require a fundamentally different JSONL layout (one file instead of per-collection files).
- **JSONL compaction** - append-only JSONL files grow unboundedly; that is a separate concern.
- **Concurrent writer support** - `Store` takes `&mut self`, so only one writer exists at compile time. The file lock is defense-in-depth for multi-process scenarios.

## Proposed Solution

### Overview

Add `create_many` to `Store` and a new `append_jsonl_many` helper. The method validates all records upfront (fail-fast before any I/O), serializes the entire batch into a single newline-delimited buffer, writes it with one `write_all` + `sync_all`, then inserts all records into SQLite in a single transaction.

### API Design

```rust
/// Create multiple records of the same type in a single atomic batch.
///
/// All records are validated upfront. If any record fails validation,
/// no records are written. JSONL receives a single `write_all` call
/// for the entire batch. SQLite wraps all inserts in one transaction.
///
/// Returns the IDs of all created records, in insertion order.
/// An empty input vec returns an empty vec (no-op).
pub fn create_many<T: Record>(&mut self, records: Vec<T>) -> Result<Vec<String>> {
    // ...
}
```

### Implementation Details

#### Validation phase (fail-fast, nothing written yet)

All validation, serialization, and indexed field extraction happens before any I/O. This ensures the "all-or-nothing" guarantee is real - if anything fails, no bytes have been written to JSONL or SQLite.

```rust
if records.is_empty() {
    return Ok(vec![]);
}

let collection = T::collection_name();
Self::validate_collection_name(collection)?;

// Pre-validate IDs, pre-serialize JSON, pre-extract and validate indexed fields
let mut ids = Vec::with_capacity(records.len());
let mut seen = std::collections::HashSet::with_capacity(records.len());
let mut prepared: Vec<(String, String, HashMap<String, IndexValue>, i64)> =
    Vec::with_capacity(records.len());

for record in &records {
    let id = record.id().to_string();
    Self::validate_id(&id)?;
    if !seen.insert(id.clone()) {
        return Err(eyre!("Duplicate ID in batch: {}", id));
    }

    // Pre-serialize (catches serde failures before any I/O)
    let data_json = serde_json::to_string(record)
        .context("Failed to serialize record")?;

    // Pre-extract and validate indexed fields
    let fields = record.indexed_fields();
    for field_name in fields.keys() {
        Self::validate_field_name(field_name)?;
    }

    let updated_at = record.updated_at();
    ids.push(id.clone());
    prepared.push((id, data_json, fields, updated_at));
}
```

After this loop, `prepared` holds everything needed for both JSONL and SQLite writes. No serialization or validation happens during the write phases.

#### JSONL write phase (single syscall)

Build the buffer from pre-serialized JSON strings, then write with a single `write_all`. No serialization happens here - it was all done in the validation phase.

```rust
fn append_jsonl_batch(&self, collection: &str, json_lines: &[&str]) -> Result<()> {
    let jsonl_path = self.base_path.join(format!("{}.jsonl", collection));

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&jsonl_path)
        .context("Failed to open JSONL file for appending")?;

    file.lock_exclusive().context("Failed to acquire file lock")?;

    // Build buffer from already-serialized JSON strings (pre-allocate to avoid reallocations)
    let total_len: usize = json_lines.iter().map(|s| s.len() + 1).sum();
    let mut buf = String::with_capacity(total_len);
    for json in json_lines {
        buf.push_str(json);
        buf.push('\n');
    }

    use std::io::Write;
    file.write_all(buf.as_bytes())?;
    file.sync_all()?;

    Ok(())
}
```

**Atomicity note:** POSIX does not guarantee that a single `write()` syscall is atomic for regular files beyond `PIPE_BUF` (typically 4096 bytes). For typical batch sizes (10-30 records, ~200-500 bytes each = 2-15KB), the kernel will almost certainly complete the write in one shot. For very large batches, the kernel may split the write across multiple pages.

The crash-during-write failure mode: if the process dies mid-`write_all`, JSONL could contain K complete JSON lines + 1 partial (truncated) line from the batch. The K complete records are valid and will be picked up by JSONL replay. The partial line is skipped with a warning (`read_jsonl_latest`, `jsonl.rs:68-79`). The remaining N-K-1 records are lost.

This is strictly better than the status quo (N individual `create` calls), where a crash between call 5 and call 6 leaves 5 records and loses 15 - the same partial-batch outcome but with a much wider crash window (N separate file opens, locks, writes, syncs vs 1). The file lock ensures no other writer interleaves during the batch write.

#### SQLite write phase (single transaction)

Uses the same pre-serialized JSON and pre-validated indexed fields from the validation phase. No serialization or field validation happens here - only SQLite I/O.

```rust
let tx = self.db.transaction()?;

for (id, data_json, fields, updated_at) in &prepared {
    tx.execute(
        "INSERT OR REPLACE INTO records (collection, id, data_json, updated_at)
         VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![collection, id, data_json, updated_at],
    )?;

    // update_indexes_tx only does DELETE + INSERT - no validation needed
    // (field names were already validated in the preparation phase)
    Self::update_indexes_tx(&tx, collection, id, fields)?;
}

tx.commit()?;
```

**Note:** `update_indexes_tx` still calls `validate_field_name` internally (it's part of the existing code at `store.rs:465`). Since we already validated all field names in the preparation phase, this is redundant but harmless - it adds no failure modes that weren't already caught.

#### Complete method (authoritative - the sections above are explanatory breakdowns)

```rust
pub fn create_many<T: Record>(&mut self, records: Vec<T>) -> Result<Vec<String>> {
    if records.is_empty() {
        return Ok(vec![]);
    }

    let collection = T::collection_name();
    debug!("create_many: collection={} count={}", collection, records.len());
    if records.len() > 1000 {
        warn!("create_many: large batch of {} records for {}", records.len(), collection);
    }
    Self::validate_collection_name(collection)?;

    // === Validation + preparation phase (no I/O) ===
    let mut ids = Vec::with_capacity(records.len());
    let mut seen = std::collections::HashSet::with_capacity(records.len());
    let mut prepared: Vec<(String, String, HashMap<String, IndexValue>, i64)> =
        Vec::with_capacity(records.len());

    for record in &records {
        let id = record.id().to_string();
        Self::validate_id(&id)?;
        if !seen.insert(id.clone()) {
            return Err(eyre!("Duplicate ID in batch: {}", id));
        }

        let data_json = serde_json::to_string(record)
            .context("Failed to serialize record")?;

        let fields = record.indexed_fields();
        for field_name in fields.keys() {
            Self::validate_field_name(field_name)?;
        }

        ids.push(id.clone());
        prepared.push((id, data_json, fields, record.updated_at()));
    }

    // === JSONL write phase (single write_all + sync_all) ===
    let json_lines: Vec<&str> = prepared.iter().map(|(_, json, _, _)| json.as_str()).collect();
    self.append_jsonl_batch(collection, &json_lines)?;

    // === SQLite write phase (single transaction) ===
    let tx = self.db.transaction()?;
    for (id, data_json, fields, updated_at) in &prepared {
        tx.execute(
            "INSERT OR REPLACE INTO records (collection, id, data_json, updated_at)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![collection, id, data_json, updated_at],
        )?;
        Self::update_indexes_tx(&tx, collection, id, fields)?;
    }
    tx.commit()?;

    info!("create_many: committed {} records to {}", ids.len(), collection);
    Ok(ids)
}
```

**Single serialization:** Each record is serialized exactly once during the preparation phase. The resulting JSON string is reused for both the JSONL buffer and the SQLite `data_json` column. This is strictly better than the existing `create` method, which serializes twice (once in `append_jsonl_generic`, once for SQLite).

#### INSERT OR REPLACE semantics

`create_many` preserves the same `INSERT OR REPLACE` semantics as `create`. If a record with the same (collection, id) already exists, it is silently overwritten. This matches the existing behavior and supports the update-as-create pattern (`update` delegates to `create` at `store.rs:242-244`).

### Consumer Usage (loopr)

After `create_many` is available, loopr's `persist_hierarchy` would change from:

```rust
// Before: N individual creates, each with its own lock + fsync + transaction
for r in hierarchy.specs {
    store.lock()?.create(r.clone())?;
    // ...
}
```

To:

```rust
// After: one create_many per type (4 calls total instead of N)
let spec_ids = store.lock()?.create_many(hierarchy.specs.clone())?;
for (r, _id) in hierarchy.specs.iter().zip(&spec_ids) {
    stores.write_specs()?.insert(r.id.clone(), r.clone());
    // ... markdown writes, event broadcasts
}
```

The in-memory map inserts, markdown writes, and event broadcasts remain per-record in the caller - `create_many` only handles the store-level persistence (JSONL + SQLite).

### Data Model

No schema changes. `create_many` writes to the same `records` and `record_indexes` tables using the same column layout. The only difference is the number of rows inserted per transaction.

### Implementation Plan

#### Phase 1: Add `append_jsonl_batch` helper
**Model:** sonnet
- Add `append_jsonl_batch` to `Store` as a private method (same module as `append_jsonl_generic`)
- Takes `&[&str]` (pre-serialized JSON lines), not generic `Record` types
- Follows the same pattern: open file, lock_exclusive, write, sync_all, drop
- Builds a multi-line buffer from the pre-serialized strings and calls `write_all` once

#### Phase 2: Add `create_many` method
**Model:** sonnet
- Add `create_many<T: Record>` to `Store`'s public API
- Validation, JSONL write, SQLite transaction as described above
- Place it after `create` in the CRUD section (`store.rs:~215`)

#### Phase 3: Tests
**Model:** sonnet
- `test_create_many_basic` - create 3 records, verify all retrievable via `get` and `list`
- `test_create_many_empty` - empty vec returns empty vec, no side effects
- `test_create_many_single` - single-element vec behaves identically to `create`
- `test_create_many_duplicate_ids` - returns error, nothing written
- `test_create_many_invalid_id` - one bad ID fails the whole batch
- `test_create_many_indexes` - verify indexed fields work for batch-created records
- `test_create_many_overwrites` - verify INSERT OR REPLACE works for existing IDs
- `test_create_many_jsonl_atomicity` - verify all records appear in JSONL file after batch write
- `test_create_many_validation_before_write` - verify that a batch with one invalid indexed field name fails without writing anything to JSONL

#### Phase 4: Re-export and version bump
**Model:** sonnet
- Ensure `create_many` is accessible via `taskstore::Store`
- Bump patch version in Cargo.toml

#### Phase 5: Refactor `create` to delegate to `create_many`
**Model:** sonnet
- Rewrite `create` as: `self.create_many(vec![record]).map(|ids| ids.into_iter().next().unwrap())`
- This fixes the legacy torn-write bug in `create` (serialization + field validation after JSONL write)
- Guarantees `create` and `create_many` can never diverge in behavior
- Update `create`'s tests to verify the pre-validation guarantee (e.g., invalid field name does not write to JSONL)

## Alternatives Considered

### Alternative 1: Heterogeneous batch via trait object
- **Description:** `create_many_dyn(&mut self, records: Vec<Box<dyn Record>>)` accepting mixed types
- **Pros:** Single call for loopr's entire hierarchy (Plan + Specs + Phases + Works)
- **Cons:** Requires `dyn Record` which conflicts with `collection_name()` being `where Self: Sized`. Would need a separate `DynRecord` trait or enum wrapper. Adds complexity for marginal improvement (4 calls vs 1).
- **Why not chosen:** The Rust conventions for this project prefer generics over `dyn` trait objects. Four type-safe `create_many` calls is cleaner.

### Alternative 2: Heterogeneous batch via enum wrapper
- **Description:** Define `AnyRecord` enum with variants for each known type, dispatch internally
- **Pros:** Type-safe, no trait objects
- **Cons:** TaskStore is a generic library - it should not know about loopr's domain types. The enum would couple the library to its consumer.
- **Why not chosen:** Violates separation of concerns.

### Alternative 3: Transaction wrapper (begin/commit around individual creates)
- **Description:** Add `begin_batch`/`commit_batch` methods that let the caller wrap multiple `create` calls
- **Pros:** Maximum flexibility, works for heterogeneous batches
- **Cons:** JSONL writes still happen per-record (no buffer batching). The file lock would need to be held across calls, requiring a different API shape (borrow checker implications). More error-prone for callers.
- **Why not chosen:** Doesn't solve the JSONL atomicity problem, and the API is harder to use correctly.

### Alternative 4: Do nothing - let the caller loop
- **Description:** Keep the status quo; loopr calls `create` in a loop
- **Pros:** No changes to taskstore
- **Cons:** Per-record overhead remains. No batch atomicity for JSONL. The performance cost scales with hierarchy size.
- **Why not chosen:** The overhead is measurable and the atomicity gap is a real correctness concern.

## Technical Considerations

### Dependencies

No new dependencies. Uses existing `fs2`, `serde_json`, `rusqlite`, `eyre`.

### Performance

- **JSONL:** 1 file open + 1 lock + 1 write + 1 fsync instead of N of each. For N=20, this eliminates ~19 lock round-trips and ~19 fsync calls. fsync is the most expensive operation (~1-10ms each on SSD).
- **SQLite:** 1 transaction instead of N. Each SQLite transaction involves a journal write + fsync, so batching eliminates ~(N-1) journal round-trips.
- **Memory:** The buffer holds the entire batch serialized as a string. For 20 records at ~300 bytes each, that is ~6KB - negligible.

### Security

No new attack surface. Validation is the same as `create` (collection name, record ID). The batch does not introduce any new input paths.

### Testing Strategy

Unit tests in `store.rs` using `tempfile::TempDir`, same pattern as existing tests. Covers: happy path, empty input, single-element, duplicate IDs, invalid IDs, index verification, JSONL file verification, overwrite behavior.

### Rollout Plan

1. Implement in taskstore, run `otto ci`
2. Bump taskstore version
3. Update loopr's `Cargo.toml` to use the new version
4. Update `persist_hierarchy` in loopr to call `create_many` per type instead of looping `create`

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Partial JSONL write on crash (torn write) | Low | Low | JSONL replay skips the one malformed line. K complete records survive, 1 partial is skipped, N-K-1 are lost. This is the same class of failure as individual `create` but with a much narrower crash window. |
| Large batch exceeds reasonable memory | Very Low | Low | Batches are bounded by hierarchy size (~30 records max in practice). No artificial limit needed. |
| SQLite transaction fails after JSONL write succeeds | Low | Low | Same as current `create` behavior - JSONL has the data, SQLite rebuilds from JSONL on next sync. |
| Duplicate ID within batch causes silent overwrite | Medium | Medium | Explicit duplicate check in validation phase returns an error before any writes. |
| Invalid indexed field name fails during SQLite phase (after JSONL write) | None | None | **Fixed in this design.** Indexed field names are pre-validated during the preparation phase, before any I/O. This is strictly better than `create`, which validates field names inside `update_indexes_tx` after the JSONL write. The redundant validation in `update_indexes_tx` remains as defense-in-depth but cannot trigger a new failure mode. |

## Open Questions

- [x] Should `create_many` preserve `INSERT OR REPLACE` semantics? **Yes** - matches existing `create` behavior.
- [x] Should we check for duplicate IDs within the batch? **Yes** - fail the whole batch if duplicates found.
- [ ] Should we add an `update_many` alias (like `update` delegates to `create`)? Probably yes for API symmetry, but can be a follow-up.
- [ ] Should we add a `delete_many` for batch tombstone writes? Same pattern (buffer + single write), natural companion. Follow-up.

## Architect Review Notes

### Round 1 Findings (all resolved)
- **Pre-serialize + pre-validate before I/O:** All serialization, indexed field extraction, and field name validation now happen in a preparation phase before any JSONL or SQLite writes. True fail-fast guarantee.
- **Single serialization:** Eliminated double serialization by reusing pre-serialized JSON for both JSONL and SQLite.
- **Buffer pre-allocation:** `String::with_capacity` sized to exact total length avoids heap reallocations.
- **Batch size warning:** `warn!` log for batches over 1000 records provides operational visibility without arbitrary hard limits.

### Round 2 Findings (all resolved)
- **Refactor `create` to delegate to `create_many`:** Added as Phase 5. Fixes the same torn-write bug in the existing `create` method and prevents future behavioral divergence between the two methods.
- **Buffer allocation optimization:** Applied `with_capacity` pre-calculation in `append_jsonl_batch`.

### Status: Approved

## References

- TaskStore `create` method: `store.rs:188-215`
- JSONL append helper: `store.rs:404-424`
- Loopr `persist_hierarchy`: `loopr/src/daemon/handlers/doc.rs:358-408`
- Loopr TaskStore rules: `loopr/.claude/rules/taskstore.md`
- Existing design doc: `docs/design/2026-01-15-implementation-readiness-fixes.md`
