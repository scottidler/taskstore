# TaskStore

**Generic persistent state management with SQLite+JSONL+Git integration**

TaskStore is a Rust library and CLI for managing persistent state with a unique architecture that combines:
- **SQLite** for fast queries
- **JSONL** (JSON Lines) as the git-friendly source of truth
- **Git merge driver** for conflict-free collaboration

## Features

- **Dual storage**: SQLite for speed, JSONL for git compatibility
- **Three-way merge**: Custom git merge driver for JSONL files with timestamp-based conflict resolution
- **Automatic syncing**: Git hooks automatically rebuild SQLite from JSONL
- **Schema versioning**: Automatic migrations with version tracking
- **Append-only semantics**: JSONL files track full history, latest version wins
- **Type-safe API**: Generic Record trait for any Rust type

## Workspace structure

This repository is a three-crate Cargo workspace:

| Crate | Purpose | Deps |
|-------|---------|------|
| `taskstore-traits` | `Record` trait, `IndexValue`, `Filter`, `FilterOp` | `serde` only |
| `taskstore` | Sync storage engine, CLI, `taskstore-merge` git driver, re-exports traits | `rusqlite`, `fs2`, `chrono`, etc. |
| `taskstore-async` | Async-native wrapper: dedicated writer thread + reader connection pool over WAL-mode SQLite | `taskstore`, `tokio`, `thiserror` |

Sync consumers (CLIs, scripts, the git merge driver) depend on `taskstore`. Async consumers (tokio-native daemons, IPC servers) depend on `taskstore-async`, which owns the concurrency shell so the calling code never needs `Arc<Mutex<Store>>` or `spawn_blocking` glue of its own.

### Depending on the lean trait surface only

If your crate only needs `Record` and filter types (no `Store`, no I/O), depend on `taskstore-traits` directly:

```toml
[dependencies]
taskstore-traits = { git = "ssh://git@github.com/scottidler/taskstore", branch = "main" }
```

Existing consumers of `taskstore` keep working unchanged - all types are re-exported from the full crate:

```rust
use taskstore::{Record, IndexValue, Filter, FilterOp};  // still works
```

### Depending on both crates (critical: use the same source pointer)

If your workspace depends on both `taskstore-traits` and `taskstore`, declare both in your root `[workspace.dependencies]` and inherit with `workspace = true`. This structurally prevents split-brain errors where Cargo instantiates `taskstore-traits` twice from different commits, causing opaque type-mismatch errors (`expected Record, found Record`).

```toml
# Your root Cargo.toml
[workspace.dependencies]
taskstore-traits = { git = "ssh://git@github.com/scottidler/taskstore", branch = "main" }
taskstore        = { git = "ssh://git@github.com/scottidler/taskstore", branch = "main" }

# Lean crate (domain logic only)
[dependencies]
taskstore-traits = { workspace = true }

# Full crate (needs Store)
[dependencies]
taskstore = { workspace = true }
```

Both deps must share the same source pointer (`branch`, `rev`, or same-commit tags). The recommended default is `branch = "main"` for both; use `rev = "<sha>"` for reproducible builds.

## Installation

### Build from source

```bash
cargo build --release
sudo cp target/release/taskstore /usr/local/bin/
sudo cp target/release/taskstore-merge /usr/local/bin/
```

### Install git integration

```bash
taskstore install-hooks
```

This installs:
- Git merge driver for JSONL files
- Pre-commit, post-merge, post-rebase, pre-push, post-checkout hooks
- `.gitattributes` configuration for `*.jsonl` files

## Usage

### Implementing the Record Trait

Define your own types that implement the `Record` trait:

```rust
use taskstore::{Record, IndexValue};
use serde::{Serialize, Deserialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Plan {
    id: String,
    title: String,
    status: String,
    created_at: i64,
    updated_at: i64,
}

impl Record for Plan {
    fn id(&self) -> &str {
        &self.id
    }

    fn updated_at(&self) -> i64 {
        self.updated_at
    }

    fn collection_name() -> &'static str {
        "plans"  // Stored in plans.jsonl
    }

    fn indexed_fields(&self) -> HashMap<String, IndexValue> {
        let mut fields = HashMap::new();
        fields.insert("status".to_string(), IndexValue::String(self.status.clone()));
        fields
    }
}
```

### CRUD Operations

```rust
use taskstore::{Store, now_ms};

let mut store = Store::open(".")?;

// Create
let plan = Plan {
    id: "plan-001".to_string(),
    title: "My Plan".to_string(),
    status: "active".to_string(),
    created_at: now_ms(),
    updated_at: now_ms(),
};
store.create(plan)?;

// Get
let plan: Option<Plan> = store.get("plan-001")?;

// Update
let mut plan = plan.unwrap();
plan.status = "complete".to_string();
plan.updated_at = now_ms();
store.update(plan)?;

// Delete
store.delete::<Plan>("plan-001")?;

// List all
let plans: Vec<Plan> = store.list(&[])?;
```

### Filtering

```rust
use taskstore::{Filter, FilterOp, IndexValue};

// List with filters
let active_plans: Vec<Plan> = store.list(&[
    Filter {
        field: "status".to_string(),
        op: FilterOp::Eq,
        value: IndexValue::String("active".to_string()),
    }
])?;

// Multiple filters (AND logic)
let filtered: Vec<Plan> = store.list(&[
    Filter {
        field: "status".to_string(),
        op: FilterOp::Eq,
        value: IndexValue::String("active".to_string()),
    },
    Filter {
        field: "priority".to_string(),
        op: FilterOp::Gt,
        value: IndexValue::Int(5),
    },
])?;
```

### CLI Commands

```bash
# Initialize/sync database
taskstore sync

# Install git hooks
taskstore install-hooks
```

## Architecture

### Storage Pattern: Bead Store

TaskStore uses a "Bead Store" pattern where:

1. **JSONL is the source of truth**
   - Append-only log of all changes
   - Git-friendly plain text format
   - Each record has `id`, `updated_at` fields
   - Multiple versions of same record can exist

2. **SQLite is a derived cache**
   - Built from JSONL files
   - Keeps only latest version per ID
   - Optimized for fast queries with indexes
   - Can be regenerated at any time

3. **Write-through pattern**
   - All writes go to JSONL first
   - Then written to SQLite
   - If SQLite fails, JSONL still has the data

### Collection-Based Storage

Records are stored in `{collection}.jsonl` files based on `collection_name()`:

```
.taskstore/
├── plans.jsonl         # Plan records
├── tasks.jsonl         # Task records
├── users.jsonl         # User records
└── taskstore.db        # SQLite cache
```

### Generic Schema

```sql
-- All records stored here
CREATE TABLE records (
    collection TEXT NOT NULL,
    id TEXT NOT NULL,
    data_json TEXT NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (collection, id)
);

-- Indexed fields for filtering
CREATE TABLE record_indexes (
    collection TEXT NOT NULL,
    id TEXT NOT NULL,
    field_name TEXT NOT NULL,
    field_value_str TEXT,
    field_value_int INTEGER,
    field_value_bool INTEGER,
    PRIMARY KEY (collection, id, field_name)
);
```

### Git Integration

#### Custom Merge Driver

The `taskstore-merge` binary implements a three-way merge algorithm:

1. Parse ancestor, ours, and theirs JSONL files
2. Build maps of latest record per ID
3. For each ID, determine merge outcome:
   - **Added in one branch**: Use that version
   - **Deleted in one branch**: Keep deletion
   - **Modified in both**: Use newest `updated_at` timestamp
   - **Same timestamp**: Create conflict marker

Exit codes:
- `0` - Merge successful
- `1` - Conflicts require manual resolution
- `2` - Error occurred

#### Git Hooks

All hooks run `taskstore sync` to keep SQLite in sync with JSONL:

- **pre-commit**: Sync before committing
- **post-merge**: Sync after pulling/merging
- **post-rebase**: Sync after rebasing
- **pre-push**: Sync before pushing
- **post-checkout**: Sync after branch changes

Hooks are idempotent and won't break existing hooks.

### Filtering System

The `Record` trait allows types to define indexed fields:

```rust
fn indexed_fields(&self) -> HashMap<String, IndexValue> {
    let mut fields = HashMap::new();
    fields.insert("status".to_string(), IndexValue::String(self.status.clone()));
    fields.insert("priority".to_string(), IndexValue::Int(self.priority));
    fields.insert("archived".to_string(), IndexValue::Bool(self.archived));
    fields
}
```

Supported filter operators:
- `FilterOp::Eq` - Equal to
- `FilterOp::Ne` - Not equal to
- `FilterOp::Gt` - Greater than
- `FilterOp::Lt` - Less than
- `FilterOp::Gte` - Greater than or equal
- `FilterOp::Lte` - Less than or equal
- `FilterOp::Contains` - String contains (SQL LIKE)

### Sync Logic

When syncing:

1. Clear all SQLite tables
2. Read all JSONL files
3. For each record with same ID, keep latest `updated_at`
4. Insert latest versions into SQLite
5. Skip tombstone records (`{"deleted": true}`)

This ensures SQLite always reflects the current state from JSONL.

## Development

### Project Structure

```
taskstore/
├── src/
│   ├── lib.rs           # Library entry point
│   ├── record.rs        # Record trait and IndexValue
│   ├── filter.rs        # Filter and FilterOp
│   ├── store.rs         # Core Store implementation
│   ├── jsonl.rs         # JSONL file operations
│   ├── main.rs          # CLI application
│   └── bin/
│       └── taskstore-merge.rs  # Git merge driver
├── docs/
│   └── *.md             # Design documentation
├── Cargo.toml
└── build.rs             # Build script for git version
```

### Running Tests

```bash
cargo test
```

Test coverage includes:
- Record trait implementation
- Filter operations
- Store CRUD operations
- JSONL read/write
- Merge driver three-way merge scenarios

### Example Implementation

```rust
use taskstore::{Record, IndexValue, Store, now_ms};
use serde::{Serialize, Deserialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Task {
    id: String,
    title: String,
    status: String,
    priority: i64,
    updated_at: i64,
}

impl Record for Task {
    fn id(&self) -> &str {
        &self.id
    }

    fn updated_at(&self) -> i64 {
        self.updated_at
    }

    fn collection_name() -> &'static str {
        "tasks"
    }

    fn indexed_fields(&self) -> HashMap<String, IndexValue> {
        let mut fields = HashMap::new();
        fields.insert("status".to_string(), IndexValue::String(self.status.clone()));
        fields.insert("priority".to_string(), IndexValue::Int(self.priority));
        fields
    }
}

fn main() -> eyre::Result<()> {
    let mut store = Store::open(".")?;

    let task = Task {
        id: "task-001".to_string(),
        title: "Example Task".to_string(),
        status: "pending".to_string(),
        priority: 10,
        updated_at: now_ms(),
    };

    store.create(task)?;
    Ok(())
}
```

## Design Philosophy

1. **JSONL as truth**: All state lives in append-only JSONL files
2. **SQLite as cache**: Fast queries without corrupting the source
3. **Git-native**: Leverage git for version control and collaboration
4. **Automatic resolution**: Use timestamps to auto-resolve most conflicts
5. **Fail-safe**: If SQLite corrupts, regenerate from JSONL
6. **Generic**: No domain-specific types, works with any Record implementation

## Use Cases

- **Concurrent processes**: Multiple processes working on different records
- **Distributed teams**: Git-based collaboration with automatic conflict resolution
- **Audit trails**: JSONL files preserve full history of changes
- **Reproducibility**: Regenerate exact state from JSONL files
- **Offline work**: Work offline, sync when reconnected
- **Type flexibility**: Define your own domain types

## Performance

- **Reads**: Fast queries via SQLite indexes on indexed fields
- **Writes**: Append to JSONL (O(1)), then SQLite
- **Sync**: Full rebuild from JSONL (typically <100ms for 1000s of records)
- **Merge**: Three-way merge is O(n) where n = unique IDs

## Limitations

- JSONL files grow unbounded (no compaction yet)
- Full sync on every merge (no incremental updates)
- Timestamp-based conflict resolution (assumes synchronized clocks)
- Indexed fields defined at compile time (can't add dynamically)
- No built-in data validation beyond Rust types

## License

[License TBD]

## Contributing

[Contributing guidelines TBD]
