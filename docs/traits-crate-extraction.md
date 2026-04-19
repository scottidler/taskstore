# Requirements: Extract `taskstore-traits` into its own crate

**Status:** Requirements doc (not a design doc). Authored as a cross-repo ask from `scottidler/loopr-v5`. Not yet implemented in this repo. Not committed by the loopr-v5 session.

**Date authored:** 2026-04-19

**Author:** Scott A. Idler (via loopr-v5 architectural review sessions)

## Motivation

`scottidler/loopr-v5` is a 13-crate Cargo workspace that orchestrates AI agents across target git repositories. The `domain` crate in that workspace is architecturally positioned as the **pure symbol layer** — it holds record types (`Plan`, `Spec`, `Phase`, `Work`, `Bundle`, `Tick`) and their FSM transition tables, with no I/O, no filesystem, no database, no network.

`domain` needs the `Record` trait so its records can be persisted by the downstream `store` crate (which wraps `taskstore::Store`). Currently `domain` depends on `scottidler/taskstore` (full crate, v0.2.3) to get the trait.

**The problem:** the full `taskstore` crate bundles the `Record` trait together with `Store`, which in turn depends on:

| Transitive dep | Purpose in `taskstore` |
|---|---|
| `rusqlite` (bundled) | SQLite cache of JSONL state |
| `fs2` | Cross-process file locking around the JSONL files |
| `tracing-subscriber` | Logging configuration helpers |
| `chrono` | Timestamp handling |

Every workspace member in loopr-v5 that depends on `domain` transitively compiles all of the above, even if they only want to pattern-match on `Plan::status`. That makes the "pure symbol layer" claim aspirational at the source level (no `use rusqlite`) but not enforced at the transitive-dep level.

The Architect review (round 3) flagged this as a false claim in the vision doc. The fix is to extract the lean trait surface into its own crate.

## Proposed structure

Turn `scottidler/taskstore` from a single crate into a Cargo workspace with two member crates:

```
taskstore/                     (repo root; workspace manifest)
├── Cargo.toml                 (workspace root, members = ["crates/*"])
├── crates/
│   ├── taskstore-traits/      NEW crate
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs         re-exports Record, IndexValue, Filter, FilterOp
│   │       ├── record.rs      (moved from current src/record.rs)
│   │       └── filter.rs      (moved from current src/filter.rs)
│   │
│   └── taskstore/             existing crate, slimmed
│       ├── Cargo.toml         depends on taskstore-traits
│       └── src/
│           ├── lib.rs         re-exports Store + re-exports traits
│           ├── store.rs       (unchanged content)
│           ├── jsonl.rs       (unchanged content)
│           ├── main.rs        (CLI; unchanged content)
│           └── bin/
│               └── taskstore-merge.rs
```

## `taskstore-traits` Cargo.toml requirements

```toml
[package]
name = "taskstore-traits"
version = "0.1.0"         # independently versioned; starts at 0.1
edition = "2024"
authors = ["Your Name <your.email@example.com>"]
license = "MIT"
description = "Record trait and filter types for taskstore; lean, I/O-free"

[dependencies]
serde = { version = "1", features = ["derive"] }
# Nothing else. No rusqlite, no fs2, no chrono, no tracing.
```

**Hard requirement:** the `taskstore-traits` crate must compile with ONLY `serde` + `std`. Anyone depending on it should get a trivial dep tree. Verify with `cargo tree -p taskstore-traits`.

## `taskstore` Cargo.toml requirements

`taskstore` (the slim wrapper) depends on `taskstore-traits` as a path dep within the workspace, plus keeps all of its current deps (rusqlite, fs2, etc.):

```toml
[package]
name = "taskstore"
version = "0.3.0"         # bump for the restructure
edition = "2024"
# ... existing fields ...

[dependencies]
taskstore-traits = { path = "../taskstore-traits" }
# All existing taskstore deps stay: rusqlite, fs2, tracing, tracing-subscriber,
# chrono, clap, colored, dirs, env_logger, eyre, log, serde, serde_json,
# serde_yaml, uuid
```

## Public API requirements

### `taskstore-traits` exports

- `Record` trait (moved verbatim from `src/record.rs`)
- `IndexValue` enum (moved verbatim)
- `Filter` struct and `FilterOp` enum (moved from `src/filter.rs`)

No behavior change; purely a physical relocation.

### `taskstore` exports

`taskstore/src/lib.rs` continues to re-export everything it did before, but for the trait types it re-exports from `taskstore-traits`:

```rust
// Re-export traits so existing consumers of `taskstore` (like loopr-v4) keep
// compiling without changes:
pub use taskstore_traits::{Record, IndexValue, Filter, FilterOp};

// Existing store / JSONL / merge exports remain:
pub use store::{Store, now_ms};
pub use jsonl;
// ... etc.
```

**Critical:** every existing consumer that does `use taskstore::Record` must continue to work unchanged. The re-export preserves backward compatibility for loopr-v3/v4, scottidler/loopr (archived), and anything else that currently depends on `taskstore`.

## Workspace root Cargo.toml

```toml
[workspace]
resolver = "3"
members = ["crates/*"]

[workspace.package]
edition = "2024"
authors = ["Your Name <your.email@example.com>"]
license = "MIT"

# Optionally pin common deps here; left to author's preference.
```

Delete or empty the repo-root `Cargo.toml`'s `[package]` block; the crates move under `crates/`.

## Migration requirements

1. `git mv src/record.rs crates/taskstore-traits/src/record.rs`
2. `git mv src/filter.rs crates/taskstore-traits/src/filter.rs`
3. Move `src/{lib.rs,jsonl.rs,main.rs,store.rs,bin/}` to `crates/taskstore/src/` (git mv equivalent).
4. Update `crates/taskstore/src/lib.rs` to re-export from `taskstore-traits`.
5. Update `crates/taskstore/src/store.rs`'s internal imports: `use crate::record::Record;` → `use taskstore_traits::Record;`. Same for `filter`.
6. Write the two new `crates/*/Cargo.toml` files per the requirements above. Existing deps in `taskstore/Cargo.toml` move with the crate.
7. Write the workspace root `Cargo.toml`.
8. `cargo check --workspace` green.
9. `cargo test --workspace` green (existing tests still pass).
10. Verify `cargo tree -p taskstore-traits` shows only `serde` + its transitive deps (no rusqlite, no fs2).

## Version semantics

- `taskstore-traits` starts at `0.1.0`. The trait surface is stable from day one; breaking changes bump minor during 0.x.
- `taskstore` bumps to `0.3.0` for the restructure. Public API is unchanged (backward-compat re-exports), but the workspace layout is a material change worth a version.

## Backward compatibility

`scottidler/taskstore`'s current consumers (primarily `scottidler/loopr-v4` and older loopr branches) use `taskstore::Record` today. Because `taskstore/src/lib.rs` re-exports from `taskstore-traits`, those imports continue to work unchanged. No downstream code must change to continue functioning.

New consumers who care about lean transitive deps (loopr-v5's `domain`) depend on `taskstore-traits` directly.

## What this unblocks in loopr-v5

After this split lands and a new `taskstore` tag exists (or the workspace crates have independent tags):

1. In `scottidler/loopr-v5`, update workspace.dependencies to add `taskstore-traits` as a git dep alongside `taskstore`:

   ```toml
   # loopr-v5/Cargo.toml workspace.dependencies
   taskstore-traits = { git = "ssh://git@github.com/scottidler/taskstore", branch = "main" }
   taskstore        = { git = "ssh://git@github.com/scottidler/taskstore", branch = "main" }
   ```

2. `domain/Cargo.toml` depends on `taskstore-traits` only:

   ```toml
   [dependencies]
   derive = { path = "../derive" }
   taskstore-traits = { workspace = true }
   ```

3. `store/Cargo.toml` depends on full `taskstore`:

   ```toml
   [dependencies]
   derive = { path = "../derive" }
   taskstore = { workspace = true }
   ```

4. `docs/vision.md` in loopr-v5 drops the "Open Question" about transitive-dep pollution. The purity claim becomes transitive-dep-enforced, not just source-level.

## Out of scope for this extraction

- Renaming `taskstore` to anything else.
- Changing the `Record` trait's shape.
- Restructuring how `Store::open` works.
- Adding new features to the merge driver, CLI, or storage engine.

This is a **pure physical reorganization** of existing code. Zero semantic changes.
