# Design Document: Introduce `taskstore-async` sibling crate

**Author:** Scott A. Idler
**Date:** 2026-04-20
**Status:** Implemented
**Review Passes Completed:** 4/5 (Draft, Correctness, Clarity, Edge Cases) + Architect round 1 (findings incorporated)

## Summary

Add async-native persistence without forcing tokio on sync consumers. A new `taskstore-async` sibling crate wraps sync `taskstore` with a writer-thread + reader-connection-pool shell, exposing `async fn` throughout. Sync `taskstore` gets WAL mode at `Store::open`, a mandatory `busy_timeout` on every connection, a persistent per-collection index schema table that makes `Store::sync()` self-contained, a transactional sync that no longer exposes intermediate empty state, and a minor breaking release. The CLI binary and `taskstore-merge` git driver stay sync-native. `loopr-v5` becomes the first async consumer and its wrapper shrinks to a ~80-line typed-ID adapter because all concurrency plumbing now lives in `taskstore-async`.

## Problem Statement

### Background

`scottidler/taskstore` is a two-crate Cargo workspace (after the `traits-crate-extraction` restructure):

```
crates/
├── taskstore-traits/   pure types (Record, IndexValue, Filter, FilterOp), serde-only
└── taskstore/          Store, JSONL, SQLite, CLI bin, taskstore-merge git driver
```

The sync `Store` API serves three of its own consumers today:

1. **`taskstore` CLI binary** (`src/main.rs`, 254 lines) - one-shot batch ops (`sync`, `list`, `get`, `sql`, `install-hooks`).
2. **`taskstore-merge`** (`src/bin/taskstore-merge.rs`, 402 lines) - invoked synchronously by git as a merge driver during `git merge` / `git rebase`. Latency-sensitive subprocess.
3. **External library consumers** - anything importing `taskstore::Store`.

The primary external consumer driving change is **`loopr-v5`**, a multi-stage tokio-native daemon (IPC server, TUI client, AutoResearch variant sweeps). Its Stage 5 store wrapper currently owns a substantial concurrency adapter: `Arc<Mutex<taskstore::Store>>` with `tokio::task::spawn_blocking` on every call, plus `LockPoisoned` / `JoinError` variants in its typed error taxonomy. Under load (many concurrent IPC clients) that pattern risks blocking-pool exhaustion: if N clients park on the mutex inside `spawn_blocking`, they all consume tokio blocking-pool slots waiting for the lock, which can deadlock unrelated `spawn_blocking` users (tracing-appender, subprocess I/O).

### Problem

Three requirements pull in opposite directions:

1. `loopr-v5` wants an `async fn` surface and internal concurrency that doesn't leak blocking-pool slots.
2. `taskstore`'s own CLI + git merge driver want to stay sync. A git merge driver pulling up a tokio runtime for each invocation adds ~10-50ms of startup overhead to an operation that's ~10-50ms of real work, and drags 300+ transitive crates into a subprocess git shells out to.
3. SQLite is synchronous at the kernel (`rusqlite::Connection` is `Send + !Sync`). Any "async API" is structurally a thread-offload shell over sync operations. There is no async SQL underneath to expose.

Making `taskstore` async-only would tax consumers 1 and 2 (and any future sync consumer) to serve consumer 3. Making `loopr-v5` own the shell keeps that tax local but duplicates a concurrency pattern every future async consumer would reinvent.

### Goals

- `loopr-v5` can depend on a single crate that exposes `async fn` throughout and owns all concurrency internally.
- Sync `taskstore` keeps its current shape; CLI and merge driver remain free of tokio.
- Concurrent read throughput via SQLite WAL mode + a bounded reader connection pool.
- Zero blocking-pool exhaustion under contention: writers use a dedicated OS thread (not the blocking pool); readers bound blocking-pool usage by the reader pool size.
- Zero dual-maintenance: `taskstore-async` delegates to sync `taskstore` logic, does not re-implement CRUD.
- `taskstore-traits` stays untouched (pure types, no tokio).

### Non-Goals

- Deprecating or rewriting sync `taskstore`.
- Publishing any crate to crates.io (git deps only, same as today).
- Async CLI or async merge driver; both stay sync.
- Connection-pool autoscaling, reconnection, or failover; fixed pool size from `OpenOptions`.
- Changing the `Record` trait or filter semantics.
- Streaming / paginated `list` API. The existing `list<T>` materializes `Vec<T>` up front; that stays. A streaming variant is future work.

### Prerequisites: Inherited Sync Defects

Review (Architect round 1) surfaced three pre-existing defects in sync `taskstore` that are currently tolerable because sync consumers run `Store::sync()` + `Store::rebuild_indexes::<T>()` in the same process and never observe intermediate state. Under WAL + a long-running async daemon + git-hook-driven sync from a separate process, they become visible and dangerous. All three are hard prerequisites for the async crate; the design cannot ship on top of them unfixed.

**Defect 1: `Store::sync()` is not transactional.**

`crates/taskstore/src/store.rs:616-685` (`pub fn sync`) executes `DELETE FROM record_indexes` and `DELETE FROM records` at the top, then a per-row `INSERT OR REPLACE` loop over every record in every JSONL file, then updates `sync_metadata`. No `begin_transaction()` / `commit()` wraps it. Under the default DELETE-journal, SQLite's file lock happens to serialize this against readers in the same process. Under WAL, readers and writers proceed concurrently by design, so any reader that queries during a `sync()` observes an empty or partial database and returns wrong answers (zero results, stale subsets, etc.). This is not a theoretical race; it is guaranteed under the target deployment (daemon reading while git-hook CLI syncs).

**Defect 2: CLI `taskstore sync` erases indexes and cannot rebuild them.**

`Store::sync()` deletes `record_indexes` and deliberately does not repopulate it (the comment at `store.rs:665-666` makes this explicit: *"We don't restore indexes during sync since we don't know which fields were indexed. Call `rebuild_indexes<T>()` after sync."*). The CLI's `Commands::Sync` handler at `main.rs:80` just calls `store.sync()?` and exits; it cannot call `rebuild_indexes::<T>()` because the CLI has no generic `T`. And `Store::install_git_hooks` at `store.rs:760-764` installs `taskstore sync` as pre-commit, post-merge, post-rebase, pre-push, and post-checkout hooks. Every common git operation against a repo that uses `.taskstore/` therefore wipes `record_indexes` in the shared DB file, stamps `sync_metadata` with a fresh `file_mtime`, and leaves the store in a state where `is_stale()` reports clean but all filtered `list()` queries return zero rows. Any async daemon attached to that same DB continues to run, blissfully unaware that its query cache has been silently nuked by a `git commit`.

**Defect 3: No `PRAGMA busy_timeout` is ever set.**

`grep PRAGMA crates/taskstore/src/store.rs` returns zero hits. SQLite's default `busy_timeout` is 0, meaning any lock contention fails immediately with `SQLITE_BUSY`. With a single sync consumer this is rarely hit. With an async daemon holding a writer thread + a reader pool + a git-hook subprocess racing to write, `SQLITE_BUSY` will surface as transient user-facing errors.

Section "Implementation Plan" below promotes these fixes to Phase 1a-1c, ahead of any async-crate scaffolding.

## Proposed Solution

### Overview

Add a third crate to the workspace:

```
crates/
├── taskstore-traits/   unchanged
├── taskstore/          sync, Prerequisites 1-3 fixed, WAL enabled, query module extracted
└── taskstore-async/    NEW, async-native (initial release)
```

Sync `taskstore` changes fall into two buckets:

1. **Prerequisites** (mandatory, per §Prerequisites above): wrap `Store::sync` in a SQLite transaction; introduce a persistent `record_index_fields` schema table so `sync` rebuilds `record_indexes` without needing a generic `T`; set `PRAGMA busy_timeout` on every connection at open.
2. **Async-enabling additions**: enable `PRAGMA journal_mode = WAL` at `Store::open` with verification; extract a small read-query module so both the sync `Store` and the async reader pool share the same SQL code paths.

No external API break beyond the WAL + transactional-sync + busy_timeout behavior changes. `rebuild_indexes::<T>()` stays on the public API for schema-evolution scenarios (see §Architecture below) but is no longer required after `sync()` for correctness.

`taskstore-async` depends on `taskstore`, `taskstore-traits`, and tokio. It exposes `AsyncStore` whose public surface mirrors the sync `Store` API, all methods `async`. Internally, `AsyncStore` owns:

- **One writer thread** (a dedicated `std::thread`, not a tokio task) holding one `rusqlite::Connection` for writes. Receives commands via `tokio::sync::mpsc`, replies via `tokio::sync::oneshot`.
- **One reader pool** of N independent `rusqlite::Connection`s, acquired via `tokio::sync::Semaphore` and used inside `tokio::task::spawn_blocking`.

Loopr-v5 imports `taskstore_async::AsyncStore`. Its Stage 5 wrapper becomes a thin typed-ID + typed-error adapter with no `Arc`, no `Mutex`, no `spawn_blocking`.

### Architecture

```
                    ┌───────────────────────────────────────────────────┐
                    │                  AsyncStore                       │
                    │                                                   │
  async fn create ──┼──► WriteCmd ──► mpsc ─────►  ┌─────────────────┐  │
  async fn update   │                              │  Writer Thread   │  │
  async fn delete   │                              │  (std::thread)   │  │
  async fn sync     │                              │  owns 1 Conn (W) │  │
  async fn rebuild  │                              │  owns Store      │  │
                    │  oneshot::Reply ◄────────────┤                  │  │
                    │                              └─────────────────┘  │
                    │                                                   │
  async fn get   ───┼──► acquire ──► Semaphore ──► spawn_blocking ─────►│
  async fn list  ───┼─── reader      (N slots)     closure holds one    │
  async fn is_stale │    connection                reader Connection    │
                    │                              runs sync SQL        │
                    │  ◄─────────────────────────── release on drop     │
                    │                                                   │
                    │    [Reader pool: N independent Connections,       │
                    │     each !Sync, isolated per spawn_blocking]      │
                    └───────────────────────────────────────────────────┘
                                           │
                                           ▼
                            ┌─────────────────────────────┐
                            │   .taskstore/ (on disk)     │
                            │   ├── taskstore.db (WAL)    │
                            │   ├── taskstore.db-wal      │
                            │   ├── taskstore.db-shm      │
                            │   └── *.jsonl               │
                            └─────────────────────────────┘
```

**Why WAL is load-bearing, not a nice-to-have:**

In SQLite's default DELETE-journal mode, the database file is locked exclusively during any write, and readers cannot proceed while a write holds the lock (and vice versa). A reader pool without WAL serializes at the file-lock level, defeating the entire point of the pool. WAL mode lets multiple readers run concurrently with a single active writer; it is the SQLite feature that makes the architecture above work. `Store::open` must set `PRAGMA journal_mode = WAL` on every connection it hands out, and must verify the pragma took (SQLite silently downgrades if the filesystem doesn't support it, e.g. some network mounts).

**Why a dedicated writer thread, not `spawn_blocking`:**

`spawn_blocking` runs on tokio's shared blocking pool (default 512 threads). If `loopr-v5` has 50 concurrent IPC clients each calling `create`, and each `spawn_blocking` parks on a shared lock, you can consume 50 pool slots for one write at a time. A dedicated OS thread owned by `AsyncStore` is outside the pool entirely: it serializes writes correctly (SQLite requires it) without ever touching blocking-pool slots.

**Why `spawn_blocking` is fine for readers:**

Reader connections are independent (each is its own `rusqlite::Connection`). There's no shared lock to park on. The bound on blocking-pool usage is the reader pool size (default 4-8, far below tokio's 512-slot default). Any 9th concurrent reader waits on an async semaphore, not a blocking-pool thread.

**Why WAL on its own is not enough (transactional `sync` + busy_timeout):**

WAL mode enables concurrent readers + one writer. It does NOT make `Store::sync()` atomic. The current `sync()` deletes both `records` and `record_indexes` then inserts row-by-row with no transaction wrapping it (see §Prerequisites, Defect 1). Under WAL, a reader running during a sync sees the empty mid-sync state and returns wrong answers. Fix: wrap the entire `sync()` body in `BEGIN IMMEDIATE` ... `COMMIT`. Readers under WAL continue to see the pre-sync snapshot until the commit completes. `BEGIN IMMEDIATE` (not `BEGIN DEFERRED`) acquires the write lock up front so the sync doesn't start reading and then fail to upgrade later.

Similarly, WAL does not prevent inter-process write contention. An async daemon's writer thread racing a git-hook `taskstore sync` subprocess can hit `SQLITE_BUSY` on the WAL file's shared-memory lock. Fix: set `PRAGMA busy_timeout = 5000` on every connection (writer and readers). This gives contended locks up to five seconds to clear before erroring, which swallows the common case (one operation finishing while the other waits) without papering over true deadlocks.

**Why a persistent index-schema table is the right fix for the index-rebuild gap:**

Three shapes were considered (captured in §Alternatives Considered for full record):

- (a) Persist per-collection indexed-field definitions in a new schema table so `sync()` can rebuild `record_indexes` without a generic `T`.
- (b) Have `is_stale()` detect an empty `record_indexes` table and trigger an in-process rebuild.
- (c) Replace the git-hook-invoked `taskstore sync` with a daemon-aware signaling mechanism so the daemon itself (which has `T`) runs the sync.

Shape (a) is chosen because it fixes the root cause: `sync()` genuinely has enough information to rebuild indexes, it just currently doesn't record the field-type metadata. Shape (b) papers over the failure mode (indexes would still be wiped and rebuilt on every open, which is wasteful), and shape (c) is operationally fragile (what if hooks are installed manually, or the daemon is down when a commit lands?). Shape (a) also makes the CLI's `taskstore sync` correct on its own, independent of who invokes it.

The new schema table:

```sql
CREATE TABLE IF NOT EXISTS record_index_fields (
    collection TEXT NOT NULL,
    field_name TEXT NOT NULL,
    field_type TEXT NOT NULL,  -- 'string' | 'int' | 'bool'
    PRIMARY KEY (collection, field_name)
);
```

On every `Store::create` / `create_many`, the existing `update_indexes_tx` call also upserts `(collection, field_name, field_type)` rows derived from `record.indexed_fields()`. The upsert is idempotent and cheap. `Store::sync()` then rebuilds `record_indexes` by: reading each `records.data_json`, looking up the collection's registered fields from `record_index_fields`, extracting each field from the JSON using the recorded type, and inserting into `record_indexes`. No `T` required.

Schema evolution (e.g., adding a newly-indexed field to an existing type) is the one case where `rebuild_indexes::<T>()` is still needed: new fields only get registered in `record_index_fields` when records carrying them are written, so pre-existing records won't retroactively gain the new index until their next `update` or a full `rebuild_indexes::<T>()` pass. `rebuild_indexes::<T>()` remains on the public API for this case and is documented as "call after schema evolution, not after sync." It is removed from the "must-call-after-sync" contract.

### Data Model

Two schema changes in the existing SQLite DB, both additive:

1. New `record_index_fields` table (see §Architecture above) capturing `(collection, field_name, field_type)` tuples. Populated on every write that carries indexed fields; read by `sync()` to drive index rebuild without a generic `T`.
2. No change to `records`, `record_indexes`, or `sync_metadata` schemas.

On-disk file layout changes only by the WAL auxiliary files (`taskstore.db-wal`, `taskstore.db-shm`), both already in the store's generated `.gitignore` (see `crates/taskstore/src/store.rs` `create_gitignore`).

JSONL files on disk are unaffected. The Bead Store contract (JSONL as source of truth, SQLite as rebuildable cache) is preserved: an operator can still delete the SQLite DB and everything rebuilds from JSONL on the next `Store::open`, with `record_index_fields` repopulated as writes flow through.

### API Design

**New crate `taskstore-async`:**

```rust
use std::path::{Path, PathBuf};
use taskstore_traits::{Filter, IndexValue, Record};

pub struct AsyncStore { /* writer thread handle + reader pool + sender */ }

pub struct OpenOptions {
    pub read_connections: usize,      // default: 4
    pub writer_queue_capacity: usize, // default: 128
}

impl Default for OpenOptions { /* sets defaults above */ }

impl AsyncStore {
    pub async fn open<P: AsRef<Path>>(path: P, opts: OpenOptions) -> Result<Self, Error>;

    pub fn base_path(&self) -> &Path;

    // Writes (routed to writer thread)
    pub async fn create<T: Record>(&self, record: T) -> Result<String, Error>;
    pub async fn create_many<T: Record>(&self, records: Vec<T>) -> Result<Vec<String>, Error>;
    pub async fn update<T: Record>(&self, record: T) -> Result<(), Error>;
    pub async fn delete<T: Record>(&self, id: &str) -> Result<(), Error>;
    pub async fn delete_by_index<T: Record>(&self, field: &str, value: IndexValue) -> Result<usize, Error>;
    pub async fn sync(&self) -> Result<(), Error>;
    pub async fn rebuild_indexes<T: Record>(&self) -> Result<usize, Error>;
    pub async fn install_git_hooks(&self) -> Result<(), Error>;

    // Reads (routed to reader pool)
    pub async fn get<T: Record>(&self, id: &str) -> Result<Option<T>, Error>;
    pub async fn list<T: Record>(&self, filters: &[Filter]) -> Result<Vec<T>, Error>;
    pub async fn is_stale(&self) -> Result<bool, Error>;

    // Explicit graceful shutdown (optional; Drop also works)
    pub async fn close(self) -> Result<(), Error>;
}

impl Drop for AsyncStore {
    fn drop(&mut self) { /* close mpsc sender; join writer thread */ }
}
```

Method signatures intentionally mirror sync `Store`: same names, same arguments, same return shapes, just `async fn` and `&self` everywhere (no `&mut self`, because internal state lives behind the writer thread).

**Typed errors** (library rule: `thiserror` for libs):

```rust
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("store I/O failure: {0}")]
    Io(#[from] std::io::Error),

    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("store task channel closed (store shut down)")]
    StoreClosed,

    #[error("WAL mode could not be enabled on {path} (filesystem may not support it)")]
    WalUnsupported { path: PathBuf },

    #[error("{0}")]
    Other(String),
}
```

The `Other(String)` variant captures `eyre::Report` from sync `taskstore` by formatting it. Once sync `taskstore` grows typed errors (separate work, not this doc), the `Other` variant can narrow.

**Sync `taskstore` changes** (scoped to this release; see §Implementation Plan for ordering):

Prerequisite fixes (mandatory per §Prerequisites):

- `Store::open` sets `PRAGMA busy_timeout = 5000` on every connection it creates. This includes the writer connection owned by the Store and any reader connection the async crate opens against the same DB.
- `Store::sync` is wrapped in `BEGIN IMMEDIATE` ... `COMMIT`. Any error inside the transaction rolls back; readers under WAL continue to see the pre-sync snapshot until commit.
- New `record_index_fields (collection, field_name, field_type)` schema table created in `Store::create_schema`. `Store::create` / `create_many` upsert into it (via `update_indexes_tx`) on every write carrying indexed fields. `Store::sync` reads it to rebuild `record_indexes` without a generic `T`.
- `rebuild_indexes::<T>()` stays on the public API, semantically narrowed to schema-evolution scenarios. Its doc comment explicitly notes it is no longer required after `sync()` for correctness.

Async-enabling additions:

- `Store::open` sets `PRAGMA journal_mode = WAL` on its connection and verifies the pragma took. Returns `Err` if the filesystem doesn't support WAL (e.g., some network mounts silently downgrade).
- New internal module `taskstore::query` exposes read-query helpers as free functions taking `&rusqlite::Connection` (e.g., `query::get_record(conn, collection, id)`, `query::list_records(conn, collection, filters)`, `query::is_stale(conn, base_path)`). The existing `Store::get` / `Store::list` / `Store::is_stale` methods delegate to these helpers. Pure internal refactor; external API is unchanged.

Versioning: bump from `0.3.x` to the next breaking release. The schema change (new table) and WAL enablement are the breaking bits; transactional `sync` and `busy_timeout` are behavior-safe but included in the same bump.

### Implementation Plan

Each phase ends with `otto ci` green on the workspace. Phases 1a-1d all land in sync `taskstore` before any async scaffolding starts; they are the prerequisite hardening of the sync core.

#### Phase 1a: Pragmas (WAL + busy_timeout)
**Model:** sonnet

- In `Store::open`: after `Connection::open`, set `PRAGMA busy_timeout = 5000` unconditionally.
- Set `PRAGMA journal_mode = WAL` and read back the returned mode via `query_row`. If the returned mode is not `"wal"`, return an error (`eyre` context: "WAL mode could not be enabled on {path}; filesystem may not support it").
- Add a small helper `fn apply_pragmas(conn: &Connection, path: &Path) -> Result<()>` so reader pools in the async crate can reuse the exact same setup.
- Existing tests should continue to pass. Add one test: open a Store, confirm `SELECT * FROM pragma_journal_mode()` returns `"wal"`.

#### Phase 1b: Persistent index-field schema table
**Model:** opus

- Extend `Store::create_schema` with the `record_index_fields` table above.
- In `Store::update_indexes_tx` (or a new peer helper `register_index_fields_tx`): when iterating over `record.indexed_fields()`, upsert `(collection, field_name, field_type)` rows via `INSERT OR IGNORE`. Inferring `field_type` from `IndexValue::String`/`Int`/`Bool` is a two-line match.
- Do not change any existing public signatures.
- Tests: after `Store::create` with indexed fields, assert `record_index_fields` contains the expected rows. After re-inserting the same record, assert no duplicate rows.

#### Phase 1c: Transactional sync that rebuilds indexes without `T`
**Model:** opus

- Rewrite `Store::sync` body to begin with `let tx = self.db.transaction_with_behavior(TransactionBehavior::Immediate)?;`, perform all deletes/inserts via `tx`, and `tx.commit()?` at the end.
- Replace the existing per-row records insert loop with: after inserting records into the `records` table via `tx`, run a follow-up pass that reads `record_index_fields` for each collection, extracts the corresponding JSON fields from each `data_json` using the recorded `field_type`, and inserts into `record_indexes` via the same `tx`.
- Remove the comment at `store.rs:665-666` and the "call `rebuild_indexes<T>()` after sync" requirement from the CLI's `Commands::Sync` handler documentation.
- Update `Store::rebuild_indexes::<T>` doc comment: "Call after schema evolution (e.g., adding a newly-indexed field to an existing type). No longer required after `sync()` for correctness."
- Tests: sync with a concurrent reader in a second thread (use `Arc<Barrier>`) and assert the reader never observes an empty `records` table. Sync after a simulated git-hook scenario (write records, drop indexes manually, sync) and assert all indexed fields resolve on subsequent `list` with filters.

#### Phase 1d: Read-query module extraction
**Model:** sonnet

- New module `crates/taskstore/src/query.rs` (module-entry style per `rust.md` 2018+ convention; not `query/mod.rs`).
- Extract `Store::get`, `Store::list`, `Store::is_stale` query bodies into free functions taking `&rusqlite::Connection`. Existing `Store` methods become thin delegates.
- Pure internal refactor. Existing tests continue to pass.

#### Phase 2: Scaffold `taskstore-async` crate
**Model:** sonnet

- `crates/taskstore-async/Cargo.toml`: declare deps (`taskstore`, `taskstore-traits`, `tokio` with `rt`+`sync`+`macros`, `rusqlite` with `bundled`, `thiserror`, `tracing`).
- `crates/taskstore-async/src/lib.rs`: module declarations + re-exports of `taskstore_traits::{Record, Filter, IndexValue, FilterOp}` for convenience.
- Update workspace `Cargo.toml` members.
- `Error` enum with `thiserror` variants above.
- `OpenOptions` with `Default` impl.
- Empty `AsyncStore` stub that compiles.

#### Phase 3: Writer thread + command dispatch
**Model:** opus

- Define `WriteCmd` enum mirroring each mutating method (`Create`, `CreateMany`, `Update`, `Delete`, `DeleteByIndex`, `Sync`, `RebuildIndexes`, `InstallGitHooks`), each carrying its args plus a `tokio::sync::oneshot::Sender<Result<_, Error>>`.
- Spawn dedicated OS thread in `AsyncStore::open`. Thread owns a sync `taskstore::Store`, receives `WriteCmd` from `mpsc::Receiver`, dispatches to the corresponding sync method, sends result back via oneshot.
- Thread exits cleanly when `Receiver::recv` returns `None` (last `Sender` dropped).
- `AsyncStore::close(self)`: drops the sender, `await`s a shutdown signal from the thread, joins. `Drop` impl joins synchronously.

#### Phase 4: Reader pool
**Model:** opus

- `ReaderPool` struct holding `Arc<Semaphore>` + `Arc<Mutex<VecDeque<Connection>>>` (or equivalent - a bounded `mpsc` of Connections is a slick alternative, consider during implementation).
- `pool.acquire() -> ReaderGuard` is an async operation that waits on the semaphore and pops a connection. `ReaderGuard::Drop` returns the connection to the pool.
- Each reader `Connection` opens the same SQLite file and runs the shared `taskstore::apply_pragmas(&conn, &base_path)` helper introduced in Phase 1a, which sets `PRAGMA journal_mode = WAL` (idempotent), `PRAGMA busy_timeout = 5000`, and `PRAGMA foreign_keys = ON`.
- `AsyncStore::get` / `list` / `is_stale` acquire a guard, then `spawn_blocking(move || query::...(&conn, ...))`. Guard drops at end of closure; connection returns to pool.

#### Phase 5: Public `AsyncStore` API
**Model:** sonnet

- Wire each `async fn` method to the writer thread (via `WriteCmd` + oneshot) or the reader pool (via `acquire` + `spawn_blocking`).
- `base_path` is a simple accessor to a `PathBuf` captured at open time.
- `AsyncStore::open` bootstraps via one sync `Store::open` call (creates schema, gitignore, version file, does initial stale sync), hands that Store to the writer thread, then opens N additional raw Connections for the reader pool.

#### Phase 6: Tests
**Model:** sonnet

- Unit tests: `OpenOptions` defaults; error variants' `Display`; writer-thread shutdown on sender drop; reader pool acquire/release behavior.
- Integration tests (in `crates/taskstore-async/tests/`): open/create/get/list/close round-trip; concurrent `list` correctness (e.g., 16 concurrent reads against 1000-record corpus); write ordering (100 sequential creates produce the same order in `list`); no-panic-on-drop-while-pending (drop `AsyncStore` while a write future is outstanding).
- A smoke test using `#[tokio::test]`.

#### Phase 7: Docs + version bumps
**Model:** sonnet

- Update repo `README.md` to describe the three crates.
- `bump -M` on workspace (next breaking release) covers both `taskstore` and `taskstore-async` initial ship.
- Tag as flat `v*` on workspace root (per the `single-tag-scheme` memory rule). A single workspace tag covers both crates; consumers pin per-crate versions via `Cargo.toml`.
- Update `loopr-v5`'s design doc (separate work tracked in that repo) to depend on `taskstore-async`.

## Alternatives Considered

### Alternative 1: Async-only `taskstore` (drop sync entirely)

- **Description:** Rewrite `taskstore::Store` itself to be async-native. Sync consumers wrap with `Runtime::new()?.block_on(...)`.
- **Pros:** One crate, one API, one version. No dual surface. Matches modern idiom (sqlx, tokio-postgres).
- **Cons:** Taxes the CLI binary and `taskstore-merge` git driver with tokio. Merge driver overhead becomes significant: tokio runtime startup is ~10-50ms for an operation that's itself ~10-50ms, effectively doubling latency; drags 300+ transitive crates into a subprocess git shells out to; wrong mental model for one-shot batch tools.
- **Why not chosen:** The merge driver is a today-consumer, not a hypothetical. Forcing async on it is architecturally wrong. Any future sync consumer would pay the same tax.

### Alternative 2: Feature flag (`taskstore = { features = ["async"] }`)

- **Description:** One crate, one manifest, opt-in async surface behind `#[cfg(feature = "async")]`. tokio becomes an optional dep.
- **Pros:** Single crate to version and release.
- **Cons:** Doesn't match the workspace's existing "separate crate as scope signal" discipline (set by `taskstore-traits` extraction: we already chose separate crates over feature flags once). Feature-flag archaeology during debugging. Harder to audit which consumers pull tokio transitively.
- **Why not chosen:** Consistency with the prior restructure. Separate crate gives a clearer dep graph. The extra `Cargo.toml` is cheap.

### Alternative 3: Loopr owns the concurrency shell

- **Description:** `loopr-v5`'s store wrapper keeps `Arc<Mutex<taskstore::Store>>` + `spawn_blocking`, adds a writer thread internally if needed. `taskstore` stays sync-only, no new crate.
- **Pros:** Zero change to `taskstore`.
- **Cons:** Duplicated pattern if any future async consumer appears. Loopr's design doc keeps a substantial concurrency section that's really about "how to async-wrap a sync SQLite store" (not a loopr concern). `LockPoisoned` / `JoinError` pollute loopr's typed errors. Blocking-pool exhaustion risk stays in loopr's failure surface.
- **Why not chosen:** The concurrency model is a property of "how to serve async SQLite," which is taskstore's domain, not loopr's. Factoring it once here beats re-deriving it per consumer.

### Alternative 4: `async-trait` + `dyn AsyncStore`

- **Description:** Expose `AsyncStore` as a trait with `async-trait` macro; swap implementations for testing.
- **Pros:** Mockable at trait level.
- **Cons:** `async-trait` adds `Box<Pin<dyn Future>>` allocation per call; `dyn` trait objects violate project rust conventions ("Use generics for DI, never `dyn`"). Testing already works with real `AsyncStore` + `tempfile::TempDir`.
- **Why not chosen:** Violates convention, adds overhead, no win for testability.

## Technical Considerations

### Dependencies

New to `taskstore-async`:

- `tokio` (features: `rt`, `sync`, `macros`, `time`). No `rt-multi-thread` requirement - caller chooses runtime flavor.
- `rusqlite` (feature: `bundled`) - for raw Connections in the reader pool.
- `thiserror` - library-rule compliance.
- `tracing` - function-level instrumentation per rust rules.
- `taskstore`, `taskstore-traits` - path deps within the workspace.

New to sync `taskstore`: none. WAL is already supported by `rusqlite` (bundled SQLite has it compiled in). The query-module extraction uses only existing deps.

### Performance

- **Read concurrency:** N reader connections × WAL mode = up to N concurrent reads. Default N=4; tunable via `OpenOptions`. Writes never block readers under WAL.
- **Write throughput:** Serialized by the writer thread (SQLite requires it). Throughput ceiling is single-writer SQLite throughput, which is on the order of 10-50k simple inserts/sec on local SSD. For the target workload (loopr's IPC ops measured in milliseconds, fewer than 100 writes/sec), this is far above the operating point.
- **Latency:** Writer-thread round trip adds ~µs of channel overhead. Reader `spawn_blocking` adds ~µs of task scheduling. Both vanish below SQLite's own operation latency.
- **Tokio blocking-pool usage:** Bounded by reader pool size N. With default N=4 and tokio's default 512-slot pool, no risk of exhaustion.
- **`list` materialization cost:** The inherited `Store::list` contract materializes the entire result set into `Vec<T>` before returning, which means a large unfiltered `list` on a reader holds that reader connection for the duration of SQL execution plus JSON deserialization of every row. With pool size N and K concurrent large lists, up to K readers are parked until JSON deserialization completes; a (K+1)-th caller queues on the semaphore. Operationally acceptable for the target workload (collections on the order of 1k records); flagged for future work if unbounded collections appear. Mitigations available without API change: enforce a default limit on unfiltered `list` at the async layer, or add a streaming `list_stream` method. Both are deferred (see Non-Goals).
- **Transactional `sync` duration:** `BEGIN IMMEDIATE` ... `COMMIT` holds the write lock for the whole sync. Concurrent reads under WAL proceed unblocked (they see the pre-sync snapshot). Concurrent writes block. For stores with large JSONL corpora this is a real latency floor on the commit: measure during Phase 1c testing; if it exceeds ~1s for realistic corpora, consider chunked sync (multiple smaller transactions) as follow-up work.

### Security

- No new attack surface: WAL is a storage format, not a network protocol.
- Crash semantics improve under WAL (WAL checkpoints provide cleaner recovery than DELETE journal).
- Same input validation (`Store::validate_collection_name`, `validate_id`, `validate_field_name`) applies unchanged.

### Testing Strategy

- **Unit:** `OpenOptions::default`; `Error` Display formatting; writer-thread shutdown path; reader-pool acquire/release.
- **Integration (`tests/`):** smoke round-trip; concurrent read correctness (16-way `list` against a seeded corpus); write ordering under burst; drop-while-pending safety.
- **Property-style (optional):** random interleaving of reads + writes, assert final state matches sync `Store` reference.
- **Fakes not needed:** the only external dep is the filesystem; use `tempfile::TempDir`.
- Integration tests run under `#[tokio::test]`; the `taskstore` crate's existing test suite continues to cover sync Store behavior.

### Rollout Plan

1. Ship Phases 1a-1d (sync `taskstore` hardening) on a feature branch; verify existing `taskstore` tests pass, new tests for transactional sync and concurrent read correctness pass, and no on-disk regressions on an existing `.taskstore/` corpus.
2. Ship Phases 2-6 (the async crate) on a follow-up branch.
3. Bump workspace version (single flat `v*` tag on main, annotated, per memory rule).
4. Verify loopr-v5 Stage 5 migration against the new crate in a loopr branch.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| WAL not supported on some filesystem (e.g., network mount) | Low | Medium | `Store::open` verifies `PRAGMA journal_mode` return value and errors loudly with `Error::WalUnsupported` rather than silently falling back to DELETE journal. |
| Writer thread panics mid-dispatch, leaving outstanding oneshots unresolved | Low | High | Panic in writer thread closes the mpsc Receiver. All pending oneshot `Receiver`s see a closed channel and return `Error::StoreClosed`. Writer thread uses `std::panic::catch_unwind` around each command; a panicked command returns `Error::Other("store panicked: {msg}")` to the caller, then the thread exits. Subsequent calls return `StoreClosed`. |
| Drop order issue: tokio runtime shut down before `AsyncStore` dropped | Low | Medium | `Drop` impl joins the writer thread synchronously (doesn't await anything). Reader pool cleanup just drops Connections (rusqlite handles cleanup). No async work required at drop. |
| `spawn_blocking` reader holds a pool slot long enough to matter | Very Low | Low | Default reader pool N=4; tokio blocking pool default 512 slots. Ceiling is 4 slots at any instant, 0.8% of the pool. |
| Bootstrap `Store::open` does schema creation; readers open after and race against an in-progress `sync()` | Low | Low | `AsyncStore::open` awaits sync `Store::open` completion (including initial `sync()`) before the reader pool accepts any requests. Reader pool is not exposed until `open` returns. |
| Git merge driver indirectly reads SQLite DB while loopr daemon has it open in WAL | Very Low | Low | Merge driver operates on JSONL files, not the SQLite DB (see `crates/taskstore/src/bin/taskstore-merge.rs`). WAL's multi-reader semantics would permit it anyway. |
| User expects sync `Store` and async `AsyncStore` to interop in-process on the same `.taskstore/` | Medium | Medium | Document explicitly: pick one per process. SQLite WAL is process-safe, but two Rust-level owners of the same DB file creates confusing write-ordering semantics. First user to hit this: flag in release notes. |
| `eyre::Report` -> `Error::Other(String)` loses structured error info | Medium | Low | Acceptable for initial release. Follow-up work to introduce typed errors in sync `taskstore` will let `taskstore-async::Error` narrow `Other` into specific variants. |
| Inter-process write contention: daemon writer thread and git-hook `taskstore sync` subprocess race for the write lock | Medium | Medium | Mandatory `PRAGMA busy_timeout = 5000` on every connection (Phase 1a) swallows the common case where one operation finishes while the other waits. True deadlocks surface as `SQLITE_BUSY` after the timeout; the writer thread converts these to `Error::Other` with context, and the git-hook subprocess exits non-zero, which the hook's trailing `|| true` currently masks. Follow-up: propose removing `|| true` from the generated hook so sync failures surface to the operator. |
| Git-hook `taskstore sync` silently wipes `record_indexes`, `is_stale()` reports clean | Was critical under default sync behavior | Eliminated | Phase 1b (persistent `record_index_fields` table) + Phase 1c (rewritten `sync` that repopulates indexes from that table) close this gap. The CLI's `taskstore sync` is now correct on its own; no daemon involvement needed. |
| Large `Vec<T>` returned from `list` holds reader connection during JSON deserialization | Medium | Low | Documented in §Performance; operationally acceptable for expected corpus sizes. Deferred mitigations: default limit on async `list`, or streaming variant. |

## Open Questions

- [ ] Reader pool implementation: hand-rolled `Semaphore + VecDeque<Connection>`, or `deadpool-sqlite`? Hand-rolled is ~50 lines and zero new transitive deps; deadpool brings a proven pool but adds a dep. Decide during Phase 4 based on whether we want connection health-checking / recycle semantics.
- [ ] Should `AsyncStore::open` accept a sync `taskstore::Store` for handoff (testing injection point), or always construct internally? Probably internal-only for v0.1; revisit if test needs arise.
- [ ] Include per-request `tracing::span` on `AsyncStore` methods via `#[tracing::instrument]`, or only internal spans on the writer dispatch loop? Favor `instrument` on public methods with `skip_all` and explicit `fields(collection=...)` per rust rules.
- [ ] `busy_timeout = 5000ms` chosen as a conservative default. Validate during Phase 1c testing; may need tuning if realistic sync durations exceed this under heavy daemon + git-hook contention.
- [ ] Should the generated git hooks drop the trailing `|| true` so that `taskstore sync` failures surface to the operator? Current behavior masks all sync errors. Out of scope for the async crate, but worth raising as a separate hardening ticket.
- [ ] Chunked sync: if Phase 1c testing shows `sync()` holding the write lock for more than ~1s on realistic corpora, consider splitting into per-collection transactions. Defer until measured.

## References

- `docs/design/2026-04-19-traits-crate-extraction.md` - prior two-crate split, sets the "separate crate as scope signal" precedent.
- `docs/storage-architecture.md` - Bead Store pattern, SQLite-as-rebuildable-cache invariant, JSONL-as-source-of-truth.
- `crates/taskstore/src/store.rs` - current sync `Store` implementation; the baseline `taskstore-async` wraps.
- `crates/taskstore/src/bin/taskstore-merge.rs` - sync-native consumer #1 (git merge driver), a motivating constraint.
- [SQLite WAL documentation](https://www.sqlite.org/wal.html) - semantics and constraints of WAL mode.
- Loopr-v5 Stage 5 store wrapper thread (user-supplied context): the driving async consumer.
