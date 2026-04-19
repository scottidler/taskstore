# Design Document: Extract `taskstore-traits` into its own crate

**Author:** Scott A. Idler
**Date:** 2026-04-19
**Status:** In Review
**Review Passes Completed:** 5/5 + Architect rounds 1 & 2 (findings incorporated)

## Summary

Convert `scottidler/taskstore` from a single-crate repo into a two-member Cargo workspace. Extract the `Record` trait, `IndexValue` enum, `Filter` struct, and `FilterOp` enum into a new lean crate (`taskstore-traits`) whose only dependency is `serde`. The existing `taskstore` crate continues to own `Store`, JSONL persistence, the CLI, and all heavy deps (`rusqlite`, `fs2`, `chrono`, `tracing-subscriber`), and re-exports the trait types so current consumers keep compiling unchanged.

## Problem Statement

### Background

`taskstore` is currently a single crate shipping four logically distinct concerns out of one `src/` tree:

1. **Trait surface** — `Record`, `IndexValue`, `Filter`, `FilterOp` (`src/record.rs`, `src/filter.rs`, ~140 lines total, serde-only).
2. **Storage engine** — `Store` plus JSONL append/replay and SQLite indexing (`src/store.rs`, `src/jsonl.rs`, ~1,500 lines, heavy deps).
3. **CLI** — `src/main.rs` plus `src/bin/taskstore-merge.rs`.
4. **Examples** — ten example programs in `examples/`.

Every downstream consumer pays the full transitive cost — `rusqlite` (bundled SQLite source), `fs2`, `chrono`, `tracing-subscriber`, `clap`, `dirs`, `env_logger`, `colored`, `uuid` — even if they only import `Record` to pattern-match on their own domain types. For a workspace like `scottidler/loopr-v5` that has a strict "pure symbol layer" discipline (no I/O, no filesystem, no SQL in its `domain` crate), this is an architectural leak: source-level purity is undermined by transitive-dep pollution.

### Problem

The lean trait surface — which is genuinely independent of all storage-engine deps — is physically coupled to the heavy engine in a single crate. There is no way for downstream consumers to depend on the trait without dragging the engine along.

### Goals

- Downstream consumers can depend on only the trait surface with a trivial dep tree (`serde` + its transitives, nothing else).
- Existing consumers of `taskstore` (loopr-v4 and older) continue to compile unchanged after the restructure.
- File moves preserve git history (`git mv`).
- The two crates version independently so trait changes and engine changes don't force each other's bumps.
- Zero semantic or behavioral changes — this is pure physical reorganization.

### Non-Goals

- Renaming `taskstore` or `Record`.
- Changing the `Record` trait's shape, method signatures, or defaults.
- Restructuring how `Store::open` works.
- Adding new features to the merge driver, CLI, or storage engine.
- Publishing either crate to crates.io (git deps only, as today).
- Migrating consumers — that's downstream work tracked separately.

## Proposed Solution

### Overview

Introduce a Cargo workspace at the repo root with two members under `crates/`. The trait-surface files move into `taskstore-traits`; everything else moves into `taskstore` and re-exports the trait types. The workspace manifest lives at the repo root with no `[package]` block.

**What downstream sees:**

- **Existing consumer who does `use taskstore::Record`** — no change required. Their `Cargo.toml` still points at `taskstore`, the re-export preserves the import path, and `cargo build` continues to succeed. Transitive deps unchanged.
- **New consumer who wants lean deps (e.g., loopr-v5's `domain`)** — adds `taskstore-traits` to their workspace dependencies alongside or instead of `taskstore`, and imports `Record` from `taskstore_traits` directly. `cargo tree` for that crate now shows only `serde`.
- **Consumer who pins `taskstore` by git tag** — the last flat tag is `v0.2.3`; going forward the tag is `taskstore-v0.3.0`. Flat-tag pins resolve to the pre-split release (which is fine; it still works). Upgraders retarget to `taskstore-v0.3.0`.

### Architecture

```
taskstore/                        (workspace root)
├── Cargo.toml                    [workspace] only, members = ["crates/*"]
├── Cargo.lock                    (shared)
├── .otto.yml                     (updated for workspace flags)
├── README.md, LICENSE, docs/
└── crates/
    ├── taskstore-traits/         NEW
    │   ├── Cargo.toml            serde-only
    │   └── src/
    │       ├── lib.rs            re-exports from record, filter
    │       ├── record.rs         moved verbatim from src/record.rs
    │       └── filter.rs         moved from src/filter.rs, `to_sql` removed
    └── taskstore/                EXISTING, slimmed
        ├── Cargo.toml            depends on taskstore-traits (path)
        ├── build.rs              moved from repo root
        ├── taskstore.yml         moved from repo root
        ├── examples/             moved from repo root
        └── src/
            ├── lib.rs            re-exports traits + Store
            ├── store.rs          owns SQL mapping (to_sql moved here)
            ├── jsonl.rs          unchanged
            ├── main.rs           unchanged
            └── bin/taskstore-merge.rs   unchanged
```

Dependency edge: `taskstore` → `taskstore-traits` (path dep inside the workspace). No reverse edge. `taskstore-traits` depends on `serde` only.

### Data Model

No data model changes. All trait definitions, enum variants, and struct fields move verbatim. The one surgical change:

**`FilterOp::to_sql` moves out of `taskstore-traits` and into `taskstore::store` as a private helper.** It is currently `pub(crate)` with `#[allow(dead_code)]`, used only by three call sites in `store.rs` (lines 391/399/407). It has no place in a SQL-agnostic traits crate. Only the `test_filter_op_to_sql` unit test moves with the helper; the other two tests in `filter.rs` (`test_filter_creation`, `test_filter_op_display`) stay in the traits crate.

### API Design

**`taskstore-traits` public API:**

```rust
// crates/taskstore-traits/src/lib.rs
pub mod filter;
pub mod record;

pub use filter::{Filter, FilterOp};
pub use record::{IndexValue, Record};
```

**`taskstore` public API (unchanged from today, every current import path preserved):**

```rust
// crates/taskstore/src/lib.rs
pub mod jsonl;
pub mod store;

// Re-export BOTH the flat types AND the modules. This matters: today's
// consumers can write either `use taskstore::Record` or
// `use taskstore::record::Record`, and both must keep working.
pub use taskstore_traits::{filter, record};
pub use taskstore_traits::{Filter, FilterOp, IndexValue, Record};

pub use store::{Store, now_ms};

// Kept for consumers who pass rusqlite types through taskstore.
pub use rusqlite;
```

The `pub use taskstore_traits::{filter, record};` line is the one the module-level consumers depend on. Without it, `use taskstore::record::Record` — valid today — would silently stop resolving after the restructure.

**Internal import rewrites in `taskstore`:**

| Old (single crate) | New (workspace) |
|---|---|
| `use crate::record::{IndexValue, Record};` | `use taskstore_traits::{IndexValue, Record};` |
| `use crate::filter::{Filter, FilterOp};` | `use taskstore_traits::{Filter, FilterOp};` |
| `crate::filter::FilterOp::Eq` (six test-site refs in `store.rs`) | `taskstore_traits::FilterOp::Eq` |

The `use crate::record::IndexValue;` line at the top of `filter.rs` is unchanged — `record` and `filter` are siblings in the new `taskstore-traits` crate, so the intra-crate path still resolves.

### Cargo manifests

**Root `Cargo.toml`:**

```toml
[workspace]
resolver = "3"
members = ["crates/*"]

[workspace.package]
edition = "2024"
authors = ["Scott A. Idler <scott.a.idler@gmail.com>"]
license = "MIT"
```

**`crates/taskstore-traits/Cargo.toml`:**

```toml
[package]
name = "taskstore-traits"
version = "0.1.0"
edition.workspace = true
authors.workspace = true
license.workspace = true
description = "Record trait and filter types for taskstore; lean, I/O-free"

[dependencies]
serde = { version = "1", features = ["derive"] }
```

**`crates/taskstore/Cargo.toml`:**

```toml
[package]
name = "taskstore"
version = "0.3.0"
edition.workspace = true
authors.workspace = true
license.workspace = true
description = "A CLI application generated by rust-scaffold"
build = "build.rs"

[lib]
name = "taskstore"
path = "src/lib.rs"

[[bin]]
name = "taskstore"
path = "src/main.rs"

[dependencies]
taskstore-traits = { path = "../taskstore-traits", version = "0.1" }

chrono = "0.4"
clap = { version = "4.5.54", features = ["derive"] }
colored = "3.0.0"
dirs = "6.0.0"
env_logger = "0.11.8"
eyre = "0.6.12"
fs2 = "0.4"
log = "0.4.29"
rusqlite = { version = "0.38.0", features = ["bundled"] }
serde = { version = "1.0.228", features = ["derive"] }
serde_json = "1.0.149"
serde_yaml = "0.9.34"
tracing = "0.1.44"
tracing-subscriber = "0.3.22"
uuid = { version = "1.19.0", features = ["v7"] }

[dev-dependencies]
tempfile = "3.24.0"
```

### Implementation Plan

#### Phase 1: Create workspace skeleton
**Model:** sonnet
- Create `crates/taskstore-traits/src/` and `crates/taskstore/src/bin/` directory tree.
- Write the new root `Cargo.toml` with `[workspace]` + `members = ["crates/*"]`.
- Write `crates/taskstore-traits/Cargo.toml` with serde-only deps.
- Write `crates/taskstore/Cargo.toml` copied from the existing root `Cargo.toml`, with path-dep on `taskstore-traits` added.
- Confirm `cargo metadata` sees both members before moving any source.

#### Phase 2: Move source files with git history preserved
**Model:** sonnet
- `git mv src/record.rs crates/taskstore-traits/src/record.rs`
- `git mv src/filter.rs crates/taskstore-traits/src/filter.rs`
- Write `crates/taskstore-traits/src/lib.rs` (the re-exports above).
- `git mv src/store.rs crates/taskstore/src/store.rs`
- `git mv src/jsonl.rs crates/taskstore/src/jsonl.rs`
- `git mv src/lib.rs crates/taskstore/src/lib.rs`
- `git mv src/main.rs crates/taskstore/src/main.rs`
- `git mv src/bin/taskstore-merge.rs crates/taskstore/src/bin/taskstore-merge.rs`
- `git mv build.rs crates/taskstore/build.rs`
- `git mv taskstore.yml crates/taskstore/taskstore.yml`
- `git mv examples crates/taskstore/examples`
- `git rm` the old root `Cargo.toml`'s `[package]` block by overwriting with the workspace-only root manifest.

#### Phase 3: Rewrite imports and relocate `to_sql`
**Model:** sonnet

Import rewrites (in `crates/taskstore/`):
- `lib.rs`: replace `pub mod filter; pub mod record;` with **both** the module re-export and the flat re-export:
  ```rust
  pub use taskstore_traits::{filter, record};
  pub use taskstore_traits::{Filter, FilterOp, IndexValue, Record};
  ```
  The module re-export preserves `use taskstore::record::Record` / `use taskstore::filter::Filter` paths; the flat re-export preserves `use taskstore::Record` / `use taskstore::Filter`. Both forms compile today; both must compile after. Keep `pub mod store; pub mod jsonl; pub use store::{Store, now_ms}; pub use rusqlite;` as-is.
- `store.rs`: replace `use crate::record::{IndexValue, Record};` with `use taskstore_traits::{IndexValue, Record};`. Same pattern for the `Filter, FilterOp` import.
- `store.rs`: rewrite the six test-site `crate::filter::FilterOp::Eq` references (pre-move lines 1105, 1365, 1375, 1423, 1432, 1442) to `taskstore_traits::FilterOp::Eq`.

`FilterOp::to_sql` relocation:
- Move the impl out of `crates/taskstore-traits/src/filter.rs` into `crates/taskstore/src/store.rs` as a private free function: `fn filter_op_to_sql(op: FilterOp) -> &'static str`.
- Update the three call sites (previously `filter.op.to_sql()`) to `filter_op_to_sql(filter.op)`.
- Move **only** the `test_filter_op_to_sql` unit test from `filter.rs` into `store.rs`'s `#[cfg(test)] mod tests` block. The other two tests in `filter.rs` — `test_filter_creation` and `test_filter_op_display` — stay in `taskstore-traits` because they exercise trait-crate types only.
- Drop the `#[allow(dead_code)]` attribute (no longer needed; the function has real callers in `store.rs`).
- Leave `use crate::record::IndexValue;` at the top of `filter.rs` intact — the intra-crate path still works.

`README.md` update (lives in Commit B alongside the compile-green changes):
- Add a new "Workspace structure" or "Usage with workspaces" subsection near the top of the "Usage" area. Content:
  - State that `taskstore` is now a two-crate workspace: `taskstore-traits` (lean, serde-only) and `taskstore` (full engine, re-exports traits).
  - Recommend the `[workspace.dependencies]` pattern for dual-crate consumers — show the toml snippet from the pinning policy section.
  - Document the pinning requirement: both deps must share a source pointer (branch, rev, or same-commit tags). Summarize the split-brain failure mode in one sentence so readers know why.
  - Recommended default: `branch = "main"` on both.
- Existing "Installation" and "Usage" snippets stay the same; the flat-crate import `use taskstore::{Record, IndexValue};` still works and the README should not rewrite it.

Build-script edits (separate edits, easy to forget):
- **Rerun paths** in `crates/taskstore/build.rs`:
  - `cargo:rerun-if-changed=.git/HEAD` → `cargo:rerun-if-changed=../../.git/HEAD`
  - `cargo:rerun-if-changed=.git/refs/` → `cargo:rerun-if-changed=../../.git/refs/`
  - Build-script rerun paths are relative to the crate's manifest directory; `.git/` now lives two levels up at the workspace root.
- **Tag filter + prefix strip** in `crates/taskstore/build.rs`:
  - `git describe --tags --always` → `git describe --tags --match 'taskstore-v*' --always`.
  - Two tag series now coexist (`taskstore-v*` and `taskstore-traits-v*`). Without the filter, `git describe` picks whichever prefixed tag is topologically nearest, which can produce CLI `--version` output like `taskstore-traits-v0.1.0-3-gabc123` on the `taskstore` binary. The `--match` flag scopes the search to the binary's own tag series.
  - After the describe call succeeds, strip the `taskstore-v` prefix so `--version` reads `0.3.0` rather than `taskstore-v0.3.0`. Use a `match` (not a chained `.strip_prefix().map().unwrap_or(git_describe)` — the chain fails NLL borrow analysis because `unwrap_or` eagerly moves `git_describe` while `strip_prefix` is still borrowing it):
    ```rust
    let git_describe = match git_describe.strip_prefix("taskstore-v") {
        Some(rest) => rest.to_string(),
        None => git_describe,
    };
    ```
    Reasoning: today's `--version` shows `0.2.3` (the tag minus the leading `v`); keeping that shape avoids a behavior shift visible to CLI users. The `None` arm is defensive — if `git describe` falls back to a bare sha, the original string flows through unchanged.

#### Phase 4: Verify workspace builds green
**Model:** sonnet
- `cargo check --workspace --all-targets` green.
- `cargo clippy --workspace --all-targets -- -D warnings` green.
- `cargo fmt --all --check` green.
- `cargo test --workspace --all-features` green (all existing tests pass).
- `cargo tree -p taskstore-traits` shows only `serde` and its transitives; no `rusqlite`, `fs2`, `chrono`, `tracing-subscriber`, `clap`, `dirs`, `env_logger`, `colored`, `uuid`.
- `otto examples` runs all ten example programs green (they build and run against the new workspace layout).
- **Backward-compat smoke test for consumers:** write a throwaway file under `/tmp/` with both import styles and compile it against the local workspace as a path dep:
  ```rust
  use taskstore::{Record, IndexValue, Filter, FilterOp};         // flat
  use taskstore::record::{Record as R2, IndexValue as I2};      // module path
  use taskstore::filter::{Filter as F2, FilterOp as O2};        // module path
  fn _assert() { let _: Option<&dyn Fn()> = None; }
  ```
  If this compiles, every current consumer's import survives the restructure. If not, `lib.rs` is missing one of the re-exports.
- Grep `~/repos/scottidler/loopr*` (the consumer repos we know about) for `taskstore::record::` and `taskstore::filter::` to confirm the module-path form is actually in use somewhere — sanity check that the extra re-export isn't dead weight.

#### Phase 5: Update CI configs, commit, tag, push
**Model:** sonnet

`.otto.yml` edits:
- `envs.VERSION` (line 5): `$(git describe --tags --always --dirty 2>/dev/null || echo 'dev')` → `$(git describe --tags --match 'taskstore-v*' --always --dirty 2>/dev/null | sed 's/^taskstore-v//' || echo 'dev')`. Same motivation as the `build.rs` tag-filter and prefix-strip: without `--match`, the env var would pick up `taskstore-traits-v*` when it's topologically nearest; without the `sed`, `$VERSION` would include the crate-name prefix and diverge from `--version` output.
- `check` task: `cargo check --all-targets --all-features` → `cargo check --workspace --all-targets --all-features`. Same for `cargo clippy` (add `--workspace`).
- `test` task: `cargo test --all-features` → `cargo test --workspace --all-features`.
- `examples` task: iterate `crates/taskstore/examples/*.rs` and run `cargo run -p taskstore --example "$name"` (package flag required in a workspace).
- `cov` task: add `--workspace` to `cargo llvm-cov`.
- `build` task: add `--workspace` to `cargo build --release`.
- `install` task: `cargo install --path .` → `cargo install --path crates/taskstore`.

`.github/workflows/ci.yml` edits:
- `cargo clippy -- -D warnings` → `cargo clippy --workspace --all-targets -- -D warnings`.
- `cargo test --verbose` → `cargo test --workspace --verbose`.
- `cargo build --release --verbose` → `cargo build --workspace --release --verbose`. Binary artifact paths at `target/release/taskstore` and `target/release/taskstore-merge` remain correct (workspace target dir is still at repo root).

`.github/workflows/binary-release.yml` edits:
- Trigger `tags: ['v*']` → `tags: ['taskstore-v*']`. Rationale: binary release should fire on `taskstore-v0.3.0` but not on `taskstore-traits-v0.1.0` (library, no binaries to publish). The narrower pattern also skips the existing `v0.2.x` flat tags from being re-triggered accidentally if they're ever re-pushed.
- `cargo build --release --target ${{ matrix.target }}` → `cargo build --workspace --release --target ${{ matrix.target }}`. Not strictly required (virtual workspaces build all members by default), but matches the explicit `--workspace` style in `ci.yml` so future readers don't wonder whether the omission is deliberate.
- **Artifact filename prefix strip** — the current packaging uses `${{ github.ref_name }}`, which under the new tag scheme becomes `taskstore-v0.3.0`. The downloaded archive would then be `taskstore-taskstore-v0.3.0-linux-amd64.tar.gz` (double prefix). Fix: add a step early in the job that computes a clean tag variable, and reference it in every archive/checksum name:
  ```yaml
  - name: Strip crate prefix from tag for filename
    run: echo "CLEAN_TAG=${GITHUB_REF_NAME#taskstore-}" >> $GITHUB_ENV
  ```
  Then change `taskstore-${{ github.ref_name }}-${{ matrix.suffix }}.tar.gz` to `taskstore-${{ env.CLEAN_TAG }}-${{ matrix.suffix }}.tar.gz` (and the corresponding `.sha256` line). Output for `taskstore-v0.3.0` is `taskstore-v0.3.0-linux-amd64.tar.gz`, matching the pre-split naming shape (`taskstore-v0.2.3-linux-amd64.tar.gz`).
- Artifact source paths (`target/${{ matrix.target }}/release/taskstore{,-merge}`) stay unchanged.

Final verification: `otto ci` green end-to-end.

Commit strategy (two commits so `git log --follow` cleanly traces renames):
1. **Commit A — physical move:** skeleton files (three new `Cargo.toml` manifests and `crates/taskstore-traits/src/lib.rs`) staged together with the `git mv` renames of every source file. No content edits on the moved files. This commit will not compile on its own; that's acceptable because the next commit restores green.
2. **Commit B — compile green:** import rewrites in `store.rs`, `to_sql` relocation, test relocation (only `test_filter_op_to_sql`), `build.rs` path + tag-match fixes, `.otto.yml` workspace-flag updates, both GitHub Actions workflow updates, and the `README.md` "Workspace structure" subsection (content spec in Phase 3).

Version bumps land in a third commit via `bump` (see tag scheme below).

Tag scheme:
- Two independently-versioned crates means a flat `v0.3.0` is ambiguous. Use `<crate>-v<version>` annotated tags: `taskstore-traits-v0.1.0`, `taskstore-v0.3.0`.
- The flat `v0.2.0`..`v0.2.3` series is historical and stays in place. No new flat `v*` tags after this restructure.
- `bump` likely does not know about crate-prefixed tags. Fallback: edit each crate's `Cargo.toml` version manually, commit, then tag with `git tag -a taskstore-traits-v0.1.0 -m "Initial release of taskstore-traits"` and `git tag -a taskstore-v0.3.0 -m "Workspace restructure; extract taskstore-traits"`.
- Push everything in one shot: `git push --follow-tags origin main`.

## Alternatives Considered

### Alternative 1: Feature-flag the heavy deps inside the existing single crate

- **Description:** Keep `taskstore` as one crate. Gate `Store`, `rusqlite`, `fs2`, etc. behind an opt-in feature like `engine` (default-on), and expose a `traits-only` build that disables it.
- **Pros:** No workspace restructure. No new git tags. Downstream can pick via `default-features = false`.
- **Cons:** Cargo feature gates apply at compile time but the crate metadata still lists all deps as `optional = true`. Downstream who read `Cargo.lock` or `cargo tree` still see the engine deps as available; enforcement is weaker than a physical split. Also: gating `pub use rusqlite;` and all the internal uses of `rusqlite`/`fs2` across `store.rs` via `#[cfg(feature = "engine")]` adds ~30 cfg sites and makes the crate harder to reason about. Finally, the `domain` crate in loopr-v5 would still pay the compile-time graph analysis cost for the optional deps during resolution.
- **Why not chosen:** Doesn't achieve the enforcement the downstream asked for. The requirements doc explicitly wants the physical split so that `cargo tree -p taskstore-traits` is provably lean.

### Alternative 2: Move only `Record` + `IndexValue`, keep `Filter`/`FilterOp` in `taskstore`

- **Description:** The strictest reading of "pure symbol layer" is that `domain` needs only `Record` for persistence. `Filter` is a query construct used by `Store::query`, arguably an engine concern.
- **Pros:** `taskstore-traits` becomes even leaner. Fewer types to move.
- **Cons:** Downstream consumers who want to build queries against their own trait impls — or pattern-match on `FilterOp` enum variants in their own code — now need `taskstore` even when they don't need `Store`. The requirements doc explicitly names `Filter` and `FilterOp` as part of the trait surface. `Filter { field, op, value }` is a data structure, not an engine — it has no behavior that touches I/O. (Stripping `to_sql` out makes that true; see Phase 3.)
- **Why not chosen:** Hair-splitting. Moving the query data structures with the trait they serve is the cleaner boundary and is what was requested.

### Alternative 3: Independent repos (`taskstore-traits` as its own repo)

- **Description:** Create `scottidler/taskstore-traits` as a separate GitHub repo. `taskstore` depends on it via git.
- **Pros:** Complete physical separation. The lean crate's history has no storage-engine noise.
- **Cons:** Two repos to tag and ship. Changes to the trait must be made in repo A, pushed, tagged; then repo B's `Cargo.toml` updated to the new tag and pushed. No atomic commits. Losing git history via copy is worse than `git mv` within a workspace.
- **Why not chosen:** The trait and engine evolve together often enough that one-repo/two-crate is the right granularity. The requirements doc specifies a single-repo workspace.

## Technical Considerations

### Dependencies

- `taskstore-traits` — `serde` with `derive` feature. Nothing else.
- `taskstore` — all current deps plus path-dep on `taskstore-traits`. No new deps introduced.

### Performance

N/A. No runtime behavior changes. Workspace compile time for `taskstore` is marginally faster because `taskstore-traits` becomes a separate compilation unit that can be reused across consumers, but the effect is small.

### Security

N/A. No security surface touched.

### Testing Strategy

- All existing unit tests in `record.rs` and `filter.rs` move with the files; they continue to run under `cargo test -p taskstore-traits`.
- All existing tests in `store.rs`, `jsonl.rs`, integration tests, and examples run under `cargo test -p taskstore`.
- New verification step: `cargo tree -p taskstore-traits` asserted to show only `serde`.
- `otto examples` runs all 10 examples end-to-end to confirm the engine crate still works externally.

### Rollout Plan

- Three commits on `main` (Commit A: physical move; Commit B: compile-green; Commit C: version bumps), pushed together with tags via `git push --follow-tags origin main`. Existing single-crate consumers (those that depend only on `taskstore`) need no coordination — their `use taskstore::Record` and `use taskstore::record::Record` imports continue to compile via re-export. New dual-crate consumers (those depending on both `taskstore` and `taskstore-traits`) must follow the pinning policy below.
- Tag both crates with annotated tags on Commit C: `taskstore-traits-v0.1.0` and `taskstore-v0.3.0`. Use tag prefixes because a single `v0.3.0` tag is ambiguous in a workspace.
- Downstream consumers opt in on their own schedule. Loopr-v5 will add `taskstore-traits` to its workspace deps and have `domain` depend on the lean crate; loopr-v4 and older can stay on `taskstore` without any change.

### Downstream pinning policy (critical)

A consumer that depends on **both** crates (`taskstore-traits` in one member, `taskstore` in another) must pin them to the **same git source pointer** — same branch name, same `rev` sha, or same-commit tags. Mixing source pointers corrupts the build in a subtle way worth understanding before anyone gets bitten:

**The failure mode:** Because we publish via git (not crates.io), Cargo identifies a crate source by `(repo_url, git_rev)`. If a consumer writes:

```toml
taskstore-traits = { git = "ssh://git@github.com/scottidler/taskstore", tag = "taskstore-traits-v0.1.0" }  # commit X
taskstore        = { git = "ssh://git@github.com/scottidler/taskstore", tag = "taskstore-v0.3.0" }        # commit Y, X ≠ Y
```

Cargo fetches the repo at two separate commits. The directly-depended `taskstore-traits` comes from commit X. But `taskstore`'s internal manifest says `taskstore-traits = { path = "../taskstore-traits" }`, which resolves against the tree at commit Y — producing a **second, distinct** `taskstore-traits` crate. Rust's type identity is tied to the crate instance, so `taskstore_traits::Record` from commit X and `taskstore_traits::Record` from commit Y are incompatible types even if the source is byte-identical. Downstream code gets errors like `expected trait taskstore_traits::Record, found trait taskstore_traits::Record` — the kind of error that wastes hours before the duplicate-crate cause clicks.

**Supported consumer patterns:**

| Pattern | Pin style | Safe? | Notes |
|---|---|---|---|
| Track `main` | `{ git = "…", branch = "main" }` on both | Yes | Both resolve to the branch tip at `cargo update` time. This is what the requirements doc from loopr-v5 specifies. |
| Pin to a specific commit | `{ git = "…", rev = "<sha>" }` on both, same sha | Yes | Strongest reproducibility. Use when you want deterministic builds. |
| Pin one crate by tag, the other must share the same commit | `{ git = "…", tag = "taskstore-v0.3.0" }` on both | Yes | Works only because `taskstore-traits-v0.1.0` and `taskstore-v0.3.0` happen to point at the same commit (they are both created on Commit C and pushed together). Fragile the moment one crate releases without the other. |
| Different tags for each crate | tag-per-crate that point at different commits | **No** | Split-brain. Fails with opaque type-mismatch errors. |

**Recommended default:** consumers use `branch = "main"` for both deps (matches the loopr-v5 plan). Consumers who need reproducibility pin both to `rev = "<sha>"`.

**Best practice — declare via `[workspace.dependencies]`:** downstream workspace consumers should declare both git deps centrally in their root `Cargo.toml` and inherit in member crates with `workspace = true`. This is load-bearing: it moves split-brain prevention from "read the policy and comply" to "structurally enforced by the manifest shape." A single declaration per crate means Cargo sees one source pointer per crate, and there is no way for a member to accidentally pin differently.

```toml
# Consumer's root Cargo.toml
[workspace.dependencies]
taskstore-traits = { git = "ssh://git@github.com/scottidler/taskstore", branch = "main" }
taskstore        = { git = "ssh://git@github.com/scottidler/taskstore", branch = "main" }

# Consumer's domain/Cargo.toml (lean side)
[dependencies]
taskstore-traits = { workspace = true }

# Consumer's store/Cargo.toml (full side)
[dependencies]
taskstore = { workspace = true }
```

Any member overriding the workspace entry (dropping `workspace = true` and writing its own `{ git = …, tag = … }` block) re-opens the split-brain door. The policy check is "does `grep -r taskstore-traits Cargo.toml` show a single declaration?" If yes, safe. If two or more, verify they share a source pointer.

**Alternative we rejected:** publishing to crates.io would let Cargo dedupe by `(name, version)` across any source. Out of scope per the Non-Goals; revisit if a consumer actually hits a workflow that can't use branch or rev pinning.

This policy needs to land in `README.md` as part of the restructure so downstream readers don't have to archaeology the design doc to learn it. See Phase 3 for the execution step.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| Downstream pinned to `branch = "main"` rebuilds mid-restructure | Medium | Low | Single `git push --follow-tags` means the three commits land together on remote. Downstream rebuilds pick up a consistent state. Re-exports preserve `use taskstore::Record`. |
| `FilterOp::to_sql` removal breaks an unknown downstream caller | Low | Medium | `to_sql` is `pub(crate)` today — by Cargo's visibility rules it cannot be called from outside the crate. Safe to move without any consumer-facing API concern. |
| `git mv` loses history if combined with content edits in the same commit | Low | Medium | Commit A is pure `git mv` + new-file adds. Imports and SQL relocation land in Commit B. `git log --follow` then traces through Commit A cleanly. |
| `build.rs` rerun paths become stale after the move and trigger unnecessary rebuilds (or fail to rebuild on HEAD changes) | High | Low | Phase 3 updates `cargo:rerun-if-changed` paths from `.git/HEAD` and `.git/refs/` to `../../.git/HEAD` and `../../.git/refs/`. Verify by touching `.git/HEAD` and confirming a rebuild. |
| Independent tag scheme breaks downstream consumers that pin `tag = "v0.2.x"` | Low | Medium | The existing flat tags `v0.2.0`..`v0.2.3` stay. No new flat tags after the split; from here on use `taskstore-v0.3.0` and `taskstore-traits-v0.1.0`. Consumers still pinning to flat tags continue resolving the last flat release. |
| `bump` tool doesn't handle workspace crates gracefully | Medium | Low | Fall back to manual `cargo set-version` or direct `Cargo.toml` edit + `git tag -a` if `bump` misbehaves. Either way the commits and tags land correctly. |
| `otto examples` breaks because examples moved | High | Low | Update `.otto.yml` in Commit B. The `-p taskstore` flag and the new `crates/taskstore/examples/*.rs` glob together make the task work in the workspace layout. |
| Consumer of `taskstore::rusqlite` re-export silently breaks | Low | Medium | Keep the `pub use rusqlite;` line in `crates/taskstore/src/lib.rs`. |
| `binary-release.yml` trigger `tags: ['v*']` misses the new `taskstore-v*` tags; no release artifacts published | High | High | Phase 5 retargets the trigger to `taskstore-v*`. Verify by pushing a pre-release tag like `taskstore-v0.3.0-rc1` and watching the workflow fire. |
| `build.rs` `git describe` picks the wrong tag series and corrupts `taskstore --version` | High | Low | Phase 5 adds `--match 'taskstore-v*'` to the `git describe` call. Worst case: `--version` prints a non-matching format; CLI still functions. |
| GitHub Actions `ci.yml` steps run without `--workspace` and quietly skip the new crate | Medium | Medium | Phase 5 adds `--workspace` to clippy, test, and build steps. |
| Consumer who wrote `use taskstore::record::Record` (module path, not flat) breaks silently because `pub mod record;` is no longer in `taskstore::lib.rs` | Medium | High | `lib.rs` re-exports both the modules (`pub use taskstore_traits::{filter, record};`) and the flat types. Both forms resolve. Verify with `cargo check --workspace` plus a grep of downstream repos before declaring done. |
| CLI `--version` output changes shape (today `0.2.3`, naive post-split `taskstore-v0.3.0`) and confuses users or scripts that parse it | Medium | Low | `build.rs` strips the `taskstore-v` prefix via `match` (not chained `unwrap_or`, which fails NLL). Falls back to the raw string if the prefix doesn't match (e.g., dev builds without a reachable tag). |
| **Downstream duplicate-crate split-brain** — consumer depends on both crates at different git commits; Cargo instantiates `taskstore-traits` twice; type identity diverges; opaque `expected Record, found Record` errors | Medium | **Critical** | New "Downstream pinning policy" section spells out the constraint: pin both deps to the same source pointer (branch, rev, or same-commit tags). `README.md` update carries the policy to consumers. Also verify first consumer (loopr-v5) uses `branch = "main"` uniformly before they push; mis-pinning fails loud at build time. |
| `binary-release.yml` artifact filename double-prefixed (`taskstore-taskstore-v0.3.0-...`) under new tag scheme | High | Low | Phase 5 adds `CLEAN_TAG=${GITHUB_REF_NAME#taskstore-}` step and references `${{ env.CLEAN_TAG }}` in archive + checksum names. |
| `.otto.yml` `VERSION` env var picks wrong tag series during local builds | Medium | Low | Phase 5 adds `--match 'taskstore-v*'` and the same `sed 's/^taskstore-v//'` prefix strip as `build.rs`. |

## Decisions made in this doc

These are the non-obvious judgment calls resolved during design; recorded here so reviewers can object specifically:

- **Downstream pinning policy:** consumers that depend on both `taskstore-traits` and `taskstore` must pin them to the **same git source pointer** (same `branch`, same `rev`, or same-commit tags). Best-practice pattern is declaring both deps centrally in the consumer's root `[workspace.dependencies]` and inheriting via `workspace = true` in member crates — this structurally prevents the split-brain failure mode rather than relying on convention. The default recommended source pointer is `branch = "main"` for both. See the "Downstream pinning policy" section for the failure mode this prevents.
- **Module re-exports in `taskstore::lib.rs`:** both flat (`pub use taskstore_traits::{Filter, FilterOp, IndexValue, Record};`) and module-level (`pub use taskstore_traits::{filter, record};`). Consumers who wrote `taskstore::record::Record` today must keep compiling; the module re-export is not optional.
- **Tag scheme:** `<crate>-v<version>` annotated tags on `main` (`taskstore-traits-v0.1.0`, `taskstore-v0.3.0`). The flat `v0.2.x` series ends at `v0.2.3` and is not extended.
- **`--version` string shape:** strip the `taskstore-v` prefix in `build.rs` so CLI output stays as `0.3.0`, matching today's `0.2.3`. Without the strip, users would suddenly see `taskstore-v0.3.0` — a user-visible behavior shift for no gain.
- **`build.rs` home:** stays in `crates/taskstore/` (only `taskstore::main` uses `env!("GIT_DESCRIBE")` via `clap`'s `version` attribute; `taskstore-traits` has no binary).
- **Examples home:** all ten move with `taskstore`. They exercise `Store`, not the trait alone. No traits-only examples until a consumer asks.
- **`taskstore-traits` initial version:** `0.1.0`. The traits have an independent stability story; tying them to `taskstore`'s `0.2.x` would falsely imply lockstep.
- **`taskstore` version bump:** `0.3.0`. Workspace layout change is a material enough event to bump the minor, even though the public API is unchanged (backward-compat re-exports).
- **`pub use rusqlite;` re-export:** kept. Removing is a breaking change; there's no cost to keeping it since `taskstore` depends on `rusqlite` anyway.
- **`FilterOp::to_sql` placement:** moves into `store.rs` as a private free function. SQL knowledge has no business in a SQL-agnostic traits crate.

## Open Questions

- [ ] Does `bump` actually work cleanly when invoked inside a workspace member's crate directory? Needs an empirical check during Phase 5. If not, fall back to `cargo set-version` or direct edit + manual `git tag -a`.
- [ ] Confirm with the first cross-consumer (loopr-v5) that their workspace-deps block pins both crates to the same source pointer before they build against the new tags. Cheap to verify with a grep of their `Cargo.toml` files for `taskstore` and `taskstore-traits` declarations; catches mis-pinning before it manifests as a type-identity error.

## References

- [Requirements doc from loopr-v5](../traits-crate-extraction.md)
- [Cargo Workspaces documentation](https://doc.rust-lang.org/cargo/reference/workspaces.html)
- Existing design docs: [`taskstore-design.md`](../taskstore-design.md), [`storage-architecture.md`](../storage-architecture.md), [`implementation-guide.md`](../implementation-guide.md)
