// Generic store implementation using JSONL + SQLite

use crate::jsonl;
use eyre::{Context, Result, eyre};
use fs2::FileExt;
use rusqlite::Connection;
use std::fs;
use std::path::{Path, PathBuf};
use taskstore_traits::{Filter, FilterOp, IndexValue, Record};
use tracing::{debug, info, warn};

const CURRENT_VERSION: u32 = 1;
const BUSY_TIMEOUT_MS: i64 = 5000;

/// Apply pragmas required by taskstore on a freshly-opened connection.
///
/// Sets `busy_timeout` for inter-process contention tolerance, enables WAL mode
/// for concurrent-reader/single-writer semantics, and turns on foreign-key
/// enforcement. Verifies WAL actually took (SQLite silently downgrades on
/// filesystems that do not support it, e.g. some network mounts).
pub fn apply_pragmas(conn: &Connection, db_path: &Path) -> Result<()> {
    conn.pragma_update(None, "busy_timeout", BUSY_TIMEOUT_MS)?;

    let mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(eyre!(
            "WAL mode could not be enabled on {} (got {:?}); filesystem may not support it",
            db_path.display(),
            mode
        ));
    }

    conn.pragma_update(None, "foreign_keys", true)?;

    Ok(())
}

/// Generic persistent store with SQLite cache and JSONL source of truth
pub struct Store {
    base_path: PathBuf,
    db: Connection,
}

impl Store {
    /// Open or create a store at `$CWD/.taskstore/`.
    ///
    /// Convenience default for CLI / script callers who run from a directory
    /// where `.taskstore/` should live. Library consumers (including
    /// `taskstore-async`) should prefer [`Store::open_at`] so the base
    /// directory is explicit and not dependent on process CWD.
    pub fn open() -> Result<Self> {
        let cwd = std::env::current_dir().context("Failed to read current working directory")?;
        Self::open_at(cwd.join(".taskstore"))
    }

    /// Open or create a store at the exact path given.
    ///
    /// The path is used as-is; no `.taskstore` is appended, no repo-root
    /// discovery happens. Parent directories are created as needed.
    pub fn open_at<P: AsRef<Path>>(base_path: P) -> Result<Self> {
        let base_path = base_path.as_ref().to_path_buf();

        // Create directory if it doesn't exist
        fs::create_dir_all(&base_path).context("Failed to create store directory")?;

        // Open SQLite database
        let db_path = base_path.join("taskstore.db");
        let db = Connection::open(&db_path).context("Failed to open SQLite database")?;

        apply_pragmas(&db, &db_path).context("Failed to apply SQLite pragmas")?;

        let mut store = Self {
            base_path: base_path.clone(),
            db,
        };

        // Initialize schema
        store.create_schema()?;

        // Write .gitignore
        store.create_gitignore()?;

        // Write/check version
        store.write_version()?;

        // Sync if stale
        if store.is_stale()? {
            info!("Database is stale, syncing from JSONL files");
            store.sync()?;
        }

        Ok(store)
    }

    /// Get the base path of this store
    pub fn base_path(&self) -> &Path {
        &self.base_path
    }

    /// Get a reference to the SQLite database connection
    pub fn db(&self) -> &Connection {
        &self.db
    }

    /// Create database schema
    fn create_schema(&self) -> Result<()> {
        debug!("Creating database schema");

        self.db.execute_batch(
            r#"
            -- Generic records table
            CREATE TABLE IF NOT EXISTS records (
                collection TEXT NOT NULL,
                id TEXT NOT NULL,
                data_json TEXT NOT NULL,
                updated_at INTEGER NOT NULL,
                PRIMARY KEY (collection, id)
            );

            CREATE INDEX IF NOT EXISTS idx_records_collection ON records(collection);
            CREATE INDEX IF NOT EXISTS idx_records_updated_at ON records(collection, updated_at);

            -- Generic indexes table (for filtering on indexed fields)
            CREATE TABLE IF NOT EXISTS record_indexes (
                collection TEXT NOT NULL,
                id TEXT NOT NULL,
                field_name TEXT NOT NULL,
                field_value_str TEXT,
                field_value_int INTEGER,
                field_value_bool INTEGER,
                PRIMARY KEY (collection, id, field_name),
                FOREIGN KEY (collection, id) REFERENCES records(collection, id) ON DELETE CASCADE
            );

            CREATE INDEX IF NOT EXISTS idx_record_indexes_field_str ON record_indexes(collection, field_name, field_value_str);
            CREATE INDEX IF NOT EXISTS idx_record_indexes_field_int ON record_indexes(collection, field_name, field_value_int);
            CREATE INDEX IF NOT EXISTS idx_record_indexes_field_bool ON record_indexes(collection, field_name, field_value_bool);

            -- Per-collection index field schema. Populated on every write that carries
            -- indexed fields; read by sync() to rebuild record_indexes without a generic T.
            CREATE TABLE IF NOT EXISTS record_index_fields (
                collection TEXT NOT NULL,
                field_name TEXT NOT NULL,
                field_type TEXT NOT NULL CHECK (field_type IN ('string', 'int', 'bool')),
                PRIMARY KEY (collection, field_name)
            );

            -- Sync metadata for staleness detection
            CREATE TABLE IF NOT EXISTS sync_metadata (
                collection TEXT PRIMARY KEY,
                last_sync_time INTEGER NOT NULL,
                file_mtime INTEGER NOT NULL
            );
            "#,
        )?;

        Ok(())
    }

    /// Create .gitignore file
    fn create_gitignore(&self) -> Result<()> {
        let gitignore_path = self.base_path.join(".gitignore");
        if !gitignore_path.exists() {
            fs::write(
                gitignore_path,
                "taskstore.db\ntaskstore.db-shm\ntaskstore.db-wal\ntaskstore.log\n",
            )?;
        }
        Ok(())
    }

    /// Write version file
    fn write_version(&self) -> Result<()> {
        let version_path = self.base_path.join(".version");
        if !version_path.exists() {
            fs::write(version_path, CURRENT_VERSION.to_string())?;
        }
        Ok(())
    }

    /// Check if database needs syncing from JSONL
    ///
    /// Returns true if any JSONL file has been modified since the last sync,
    /// or if there are JSONL files that have never been synced.
    pub fn is_stale(&self) -> Result<bool> {
        crate::query::is_stale(&self.db, &self.base_path)
    }

    // ========================================================================
    // Generic CRUD API
    // ========================================================================

    /// Create a new record
    pub fn create<T: Record>(&mut self, record: T) -> Result<String> {
        self.create_many(vec![record]).map(|ids| {
            ids.into_iter()
                .next()
                .expect("create_many returned empty vec for single record")
        })
    }

    /// Get a record by ID
    pub fn get<T: Record>(&self, id: &str) -> Result<Option<T>> {
        let collection = T::collection_name();
        match crate::query::get_data_json(&self.db, collection, id)? {
            Some(json) => {
                let record: T = serde_json::from_str(&json).context("Failed to deserialize record from database")?;
                Ok(Some(record))
            }
            None => Ok(None),
        }
    }

    /// Create multiple records of the same type in a single atomic batch.
    ///
    /// All records are validated upfront. If any record fails validation,
    /// no records are written. JSONL receives a single `write_all` call
    /// for the entire batch. SQLite wraps all inserts in one transaction.
    ///
    /// Returns the IDs of all created records, in insertion order.
    /// An empty input vec returns an empty vec (no-op).
    ///
    /// **Duplicate ID behavior:** Unlike calling `create` in a loop (where each call
    /// silently overwrites via `INSERT OR REPLACE`), `create_many` rejects duplicate IDs
    /// within the same batch and returns an error before any I/O. Callers migrating a
    /// loop of `create` calls to a single `create_many` must deduplicate the input first.
    pub fn create_many<T: Record>(&mut self, records: Vec<T>) -> Result<Vec<String>> {
        if records.is_empty() {
            return Ok(vec![]);
        }

        let collection = T::collection_name();
        debug!("create_many: collection={} count={}", collection, records.len());
        if records.len() > 1000 {
            warn!(
                "create_many: large batch of {} records for {}",
                records.len(),
                collection
            );
        }
        Self::validate_collection_name(collection)?;

        // === Validation + preparation phase (no I/O) ===
        let mut ids = Vec::with_capacity(records.len());
        let mut seen = std::collections::HashSet::with_capacity(records.len());
        let mut prepared: Vec<(String, String, std::collections::HashMap<String, IndexValue>, i64)> =
            Vec::with_capacity(records.len());

        for record in &records {
            let id = record.id().to_string();
            Self::validate_id(&id)?;
            if !seen.insert(id.clone()) {
                return Err(eyre!("Duplicate ID in batch: {}", id));
            }

            let data_json = serde_json::to_string(record).context("Failed to serialize record")?;

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

    /// Update a record (same as create for now)
    pub fn update<T: Record>(&mut self, record: T) -> Result<()> {
        self.create(record)?;
        Ok(())
    }

    /// Delete a record
    pub fn delete<T: Record>(&mut self, id: &str) -> Result<()> {
        let collection = T::collection_name();

        // 1. Append tombstone to JSONL
        let tombstone = serde_json::json!({
            "id": id,
            "deleted": true,
            "updated_at": crate::now_ms(),
        });
        self.append_jsonl_raw(collection, &tombstone)?;

        // 2. Delete from SQLite
        self.db.execute(
            "DELETE FROM records WHERE collection = ?1 AND id = ?2",
            rusqlite::params![collection, id],
        )?;

        Ok(())
    }

    /// Delete all records matching an indexed field value.
    /// Returns the number of records deleted.
    pub fn delete_by_index<T: Record>(&mut self, field: &str, value: IndexValue) -> Result<usize> {
        // First list the matching records
        let filters = vec![Filter {
            field: field.to_string(),
            op: FilterOp::Eq,
            value,
        }];
        let records: Vec<T> = self.list(&filters)?;

        // Delete each one
        let count = records.len();
        for record in records {
            self.delete::<T>(record.id())?;
        }

        Ok(count)
    }

    /// List records with optional filtering
    pub fn list<T: Record>(&self, filters: &[Filter]) -> Result<Vec<T>> {
        let collection = T::collection_name();
        let rows = crate::query::list_data_jsons(&self.db, collection, filters)?;
        let mut results = Vec::with_capacity(rows.len());
        for data_json in rows {
            let record: T = serde_json::from_str(&data_json).context("Failed to deserialize record")?;
            results.push(record);
        }
        Ok(results)
    }

    // ========================================================================
    // Helper methods
    // ========================================================================

    fn append_jsonl_raw(&self, collection: &str, value: &serde_json::Value) -> Result<()> {
        let jsonl_path = self.base_path.join(format!("{}.jsonl", collection));

        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&jsonl_path)
            .context("Failed to open JSONL file for appending")?;

        // Acquire exclusive lock before writing
        file.lock_exclusive().context("Failed to acquire file lock")?;

        let json = serde_json::to_string(value)?;

        use std::io::Write;
        writeln!(file, "{}", json)?;
        file.sync_all()?;

        // Lock is automatically released when file is dropped
        Ok(())
    }

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

    fn update_indexes_tx(
        tx: &rusqlite::Transaction,
        collection: &str,
        id: &str,
        fields: &std::collections::HashMap<String, IndexValue>,
    ) -> Result<()> {
        debug!(collection, id, field_count = fields.len(), "update_indexes_tx: called");

        // Delete old indexes
        tx.execute(
            "DELETE FROM record_indexes WHERE collection = ?1 AND id = ?2",
            rusqlite::params![collection, id],
        )?;

        // Insert new indexes
        for (field_name, value) in fields {
            debug!(collection, id, field_name, ?value, "update_indexes_tx: inserting index");
            // Field names were already validated in create_many's preparation phase before
            // any I/O. This call is defense-in-depth for direct callers of update_indexes_tx
            // and cannot introduce a new failure mode when reached via create_many.
            Self::validate_field_name(field_name)?;

            let field_type = match value {
                IndexValue::String(_) => "string",
                IndexValue::Int(_) => "int",
                IndexValue::Bool(_) => "bool",
            };
            tx.execute(
                "INSERT OR IGNORE INTO record_index_fields (collection, field_name, field_type)
                 VALUES (?1, ?2, ?3)",
                rusqlite::params![collection, field_name, field_type],
            )?;

            match value {
                IndexValue::String(s) => {
                    tx.execute(
                        "INSERT INTO record_indexes (collection, id, field_name, field_value_str, field_value_int, field_value_bool)
                         VALUES (?1, ?2, ?3, ?4, NULL, NULL)",
                        rusqlite::params![collection, id, field_name, s],
                    )?;
                }
                IndexValue::Int(i) => {
                    tx.execute(
                        "INSERT INTO record_indexes (collection, id, field_name, field_value_str, field_value_int, field_value_bool)
                         VALUES (?1, ?2, ?3, NULL, ?4, NULL)",
                        rusqlite::params![collection, id, field_name, i],
                    )?;
                }
                IndexValue::Bool(b) => {
                    tx.execute(
                        "INSERT INTO record_indexes (collection, id, field_name, field_value_str, field_value_int, field_value_bool)
                         VALUES (?1, ?2, ?3, NULL, NULL, ?4)",
                        rusqlite::params![collection, id, field_name, *b as i64],
                    )?;
                }
            }
        }

        Ok(())
    }

    fn validate_collection_name(name: &str) -> Result<()> {
        if name.is_empty() {
            return Err(eyre!("Collection name cannot be empty"));
        }
        if name.len() > 64 {
            return Err(eyre!("Collection name too long: {} (max 64 chars)", name));
        }
        if !name.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-') {
            return Err(eyre!(
                "Invalid collection name: {} (must be alphanumeric with _/-)",
                name
            ));
        }
        Ok(())
    }

    pub(crate) fn validate_field_name(name: &str) -> Result<()> {
        if name.is_empty() {
            return Err(eyre!("Field name cannot be empty"));
        }
        if name.len() > 64 {
            return Err(eyre!("Field name too long: {} (max 64 chars)", name));
        }
        if !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return Err(eyre!("Invalid field name: {} (must be alphanumeric with _)", name));
        }
        Ok(())
    }

    /// Validate record ID
    fn validate_id(id: &str) -> Result<()> {
        // Check not empty or whitespace-only
        if id.trim().is_empty() {
            return Err(eyre!("Record ID cannot be empty or whitespace-only"));
        }

        // Check reasonable length (prevent DoS via huge IDs)
        if id.len() > 256 {
            return Err(eyre!("Record ID too long: {} chars (max 256)", id.len()));
        }

        Ok(())
    }

    // ========================================================================
    // Sync operations
    // ========================================================================

    /// Sync SQLite database from JSONL files.
    ///
    /// Atomic: the whole operation runs inside a single `BEGIN IMMEDIATE`
    /// transaction, so concurrent readers under WAL mode never observe an
    /// empty or partially-populated database. On error, the entire sync
    /// rolls back.
    ///
    /// Rebuilds `record_indexes` from the persistent `record_index_fields`
    /// schema, so no separate `rebuild_indexes::<T>()` call is required after
    /// sync for correctness. `rebuild_indexes::<T>()` remains available for
    /// schema-evolution scenarios where a record type gained a new indexed
    /// field since prior records were written.
    pub fn sync(&mut self) -> Result<()> {
        info!("Syncing database from JSONL files");

        let tx = self
            .db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

        // Clear records and indexes; record_index_fields is preserved because it
        // is schema metadata accumulated over the store's lifetime, not per-row
        // state.
        tx.execute("DELETE FROM record_indexes", [])?;
        tx.execute("DELETE FROM records", [])?;

        // Read all JSONL files and insert records
        for entry in fs::read_dir(&self.base_path)? {
            let entry = entry?;
            let path = entry.path();

            if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                continue;
            }

            let collection = path
                .file_stem()
                .and_then(|s| s.to_str())
                .ok_or_else(|| eyre!("Invalid JSONL filename: {:?}", path))?;

            debug!("Syncing collection: {}", collection);

            let file_mtime = fs::metadata(&path)?
                .modified()?
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);

            let records = jsonl::read_jsonl_latest(&path)?;

            for (id, record) in records {
                if record.get("deleted").and_then(|v| v.as_bool()).unwrap_or(false) {
                    continue;
                }

                let data_json = serde_json::to_string(&record)?;
                let updated_at = record.get("updated_at").and_then(|v| v.as_i64()).unwrap_or(0);

                tx.execute(
                    "INSERT OR REPLACE INTO records (collection, id, data_json, updated_at)
                     VALUES (?1, ?2, ?3, ?4)",
                    rusqlite::params![collection, &id, data_json, updated_at],
                )?;
            }

            tx.execute(
                "INSERT OR REPLACE INTO sync_metadata (collection, last_sync_time, file_mtime)
                 VALUES (?1, ?2, ?3)",
                rusqlite::params![collection, now_ms(), file_mtime],
            )?;
        }

        tx.execute(
            "DELETE FROM sync_metadata WHERE collection NOT IN (SELECT DISTINCT collection FROM records)",
            [],
        )?;

        Self::rebuild_indexes_from_schema_tx(&tx)?;

        tx.commit()?;

        info!("Sync complete");
        Ok(())
    }

    /// Rebuild `record_indexes` from the stored data_json + `record_index_fields` schema.
    ///
    /// For each (collection, field_name, field_type) tuple in `record_index_fields`,
    /// walk every record in that collection and extract the field value from
    /// `data_json`, inserting the typed value into `record_indexes`. Runs entirely
    /// inside the caller's transaction.
    fn rebuild_indexes_from_schema_tx(tx: &rusqlite::Transaction) -> Result<()> {
        let field_specs: Vec<(String, String, String)> = {
            let mut stmt = tx.prepare("SELECT collection, field_name, field_type FROM record_index_fields")?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };

        for (collection, field_name, field_type) in field_specs {
            let records_for_coll: Vec<(String, String)> = {
                let mut stmt = tx.prepare("SELECT id, data_json FROM records WHERE collection = ?1")?;
                let rows = stmt.query_map([&collection], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            };

            for (id, data_json) in records_for_coll {
                let data: serde_json::Value =
                    serde_json::from_str(&data_json).context("Failed to parse data_json during index rebuild")?;
                let Some(field_value) = data.get(&field_name) else {
                    continue;
                };

                match field_type.as_str() {
                    "string" => {
                        if let Some(s) = field_value.as_str() {
                            tx.execute(
                                "INSERT OR REPLACE INTO record_indexes (collection, id, field_name, field_value_str, field_value_int, field_value_bool)
                                 VALUES (?1, ?2, ?3, ?4, NULL, NULL)",
                                rusqlite::params![&collection, &id, &field_name, s],
                            )?;
                        }
                    }
                    "int" => {
                        if let Some(i) = field_value.as_i64() {
                            tx.execute(
                                "INSERT OR REPLACE INTO record_indexes (collection, id, field_name, field_value_str, field_value_int, field_value_bool)
                                 VALUES (?1, ?2, ?3, NULL, ?4, NULL)",
                                rusqlite::params![&collection, &id, &field_name, i],
                            )?;
                        }
                    }
                    "bool" => {
                        if let Some(b) = field_value.as_bool() {
                            tx.execute(
                                "INSERT OR REPLACE INTO record_indexes (collection, id, field_name, field_value_str, field_value_int, field_value_bool)
                                 VALUES (?1, ?2, ?3, NULL, NULL, ?4)",
                                rusqlite::params![&collection, &id, &field_name, b as i64],
                            )?;
                        }
                    }
                    _ => {
                        warn!(
                            collection,
                            field_name, field_type, "unknown field_type in record_index_fields"
                        );
                    }
                }
            }
        }

        Ok(())
    }

    /// Rebuild indexes for a specific record type using its compile-time schema.
    ///
    /// Call this after schema evolution (for example, when a record type gains a
    /// newly-indexed field and pre-existing records should retroactively acquire
    /// that index). **No longer required after `sync()` for correctness**: `sync`
    /// now rebuilds `record_indexes` from the persistent `record_index_fields`
    /// schema. `rebuild_indexes::<T>` remains useful because it re-deserializes
    /// each record to `T` and consults `T::indexed_fields()`, which captures
    /// index-field definitions that may not yet be registered in
    /// `record_index_fields` (for example, newly-added fields that have not yet
    /// been written to any record).
    ///
    /// The method:
    /// - Reads all records from SQLite for the collection
    /// - Deserializes each to type T to extract `indexed_fields()`
    /// - Rebuilds the `record_indexes` table entries
    ///
    /// Returns the number of records successfully indexed.
    ///
    /// # Edge case handling
    /// If records in the collection don't deserialize to type T (e.g., wrong type
    /// passed), those records are skipped with a warning log. This prevents crashes
    /// while alerting to potential misconfiguration.
    pub fn rebuild_indexes<T: Record>(&mut self) -> Result<usize> {
        let collection = T::collection_name();

        // Get raw JSON from SQLite (bypass list<T> to handle deserialization errors)
        // Use a block to ensure stmt is dropped before we start a transaction
        let records_data: Vec<(String, String)> = {
            let mut stmt = self
                .db
                .prepare("SELECT id, data_json FROM records WHERE collection = ?1")?;

            let rows = stmt.query_map([collection], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;

            rows.filter_map(|r| r.ok()).collect()
        };

        let tx = self.db.transaction()?;
        let mut count = 0;

        for (id, data_json) in records_data {
            // Attempt deserialization - skip records that don't match type T
            let record: T = match serde_json::from_str(&data_json) {
                Ok(r) => r,
                Err(e) => {
                    warn!(
                        collection = collection,
                        id = &id,
                        error = ?e,
                        "Skipping record that doesn't match type"
                    );
                    continue;
                }
            };

            Self::update_indexes_tx(&tx, collection, &id, &record.indexed_fields())?;
            count += 1;
        }

        tx.commit()?;
        debug!(collection = collection, count = count, "Rebuilt indexes for collection");
        Ok(count)
    }

    // ========================================================================
    // Git Integration
    // ========================================================================

    /// Install git hooks for automatic sync
    pub fn install_git_hooks(&self) -> Result<()> {
        info!("Installing git hooks");

        // Find git directory
        let git_dir = self.find_git_dir()?;
        let hooks_dir = git_dir.join("hooks");

        // Create hooks directory if it doesn't exist
        fs::create_dir_all(&hooks_dir).context("Failed to create hooks directory")?;

        // Install all hooks
        self.install_hook(&hooks_dir, "pre-commit", "taskstore sync")?;
        self.install_hook(&hooks_dir, "post-merge", "taskstore sync")?;
        self.install_hook(&hooks_dir, "post-rebase", "taskstore sync")?;
        self.install_hook(&hooks_dir, "pre-push", "taskstore sync")?;
        self.install_hook(&hooks_dir, "post-checkout", "taskstore sync")?;

        // Install .gitattributes for merge driver
        self.install_gitattributes()?;

        info!("Git hooks installed successfully");
        Ok(())
    }

    fn find_git_dir(&self) -> Result<PathBuf> {
        let mut current = self.base_path.clone();

        // Walk up to find .git
        loop {
            let git_path = current.join(".git");
            if git_path.exists() {
                if git_path.is_dir() {
                    return Ok(git_path);
                } else {
                    // Worktree - read .git file
                    let content = fs::read_to_string(&git_path)?;
                    let gitdir = content
                        .strip_prefix("gitdir: ")
                        .ok_or_else(|| eyre!("Invalid .git file format"))?
                        .trim();
                    return Ok(PathBuf::from(gitdir));
                }
            }

            if !current.pop() {
                break;
            }
        }

        Err(eyre!("Not in a git repository"))
    }

    fn install_hook(&self, hooks_dir: &Path, hook_name: &str, command: &str) -> Result<()> {
        let hook_path = hooks_dir.join(hook_name);
        let hook_content = format!("#!/bin/sh\n# Auto-generated by taskstore\n{}\n", command);

        if hook_path.exists() {
            let existing = fs::read_to_string(&hook_path)?;
            if existing.contains(command) {
                debug!("Hook {} already contains command", hook_name);
                return Ok(());
            }
            // Append to existing hook
            fs::write(&hook_path, format!("{}\n{}", existing, command))?;
        } else {
            fs::write(&hook_path, hook_content)?;
        }

        // Make executable (Unix only)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&hook_path)?.permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&hook_path, perms)?;
        }

        Ok(())
    }

    fn install_gitattributes(&self) -> Result<()> {
        // Find repo root
        let mut repo_root = self.base_path.clone();
        while !repo_root.join(".git").exists() && repo_root.pop() {}

        let gitattributes_path = repo_root.join(".gitattributes");
        let merge_rule = ".taskstore/*.jsonl merge=taskstore-merge";

        if gitattributes_path.exists() {
            let existing = fs::read_to_string(&gitattributes_path)?;
            if existing.contains(merge_rule) {
                info!(".gitattributes already configured");
                return Ok(());
            }

            // Append rule
            let mut file = fs::OpenOptions::new().append(true).open(&gitattributes_path)?;
            use std::io::Write;
            writeln!(file, "\n{}", merge_rule)?;
        } else {
            // Create new
            fs::write(&gitattributes_path, format!("{}\n", merge_rule))?;
        }

        // Configure git merge driver
        self.configure_merge_driver()?;

        info!(".gitattributes configured");
        Ok(())
    }

    fn configure_merge_driver(&self) -> Result<()> {
        use std::process::Command;

        // Find repo root so git config --local targets the correct .git/config
        let mut repo_root = self.base_path.clone();
        while !repo_root.join(".git").exists() && repo_root.pop() {}

        let output = Command::new("git")
            .current_dir(&repo_root)
            .args([
                "config",
                "--local",
                "merge.taskstore-merge.name",
                "TaskStore JSONL merge driver",
            ])
            .output()?;

        if !output.status.success() {
            return Err(eyre!("Failed to configure merge driver name"));
        }

        let output = Command::new("git")
            .current_dir(&repo_root)
            .args([
                "config",
                "--local",
                "merge.taskstore-merge.driver",
                "taskstore-merge %O %A %B %P",
            ])
            .output()?;

        if !output.status.success() {
            return Err(eyre!("Failed to configure merge driver command"));
        }

        Ok(())
    }
}

// Helper function for timestamps
pub fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("System time before Unix epoch")
        .as_millis() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};
    use std::collections::HashMap;
    use tempfile::TempDir;

    // Test record type
    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
    struct TestRecord {
        id: String,
        name: String,
        status: String,
        count: i64,
        active: bool,
        updated_at: i64,
    }

    impl Record for TestRecord {
        fn id(&self) -> &str {
            &self.id
        }

        fn updated_at(&self) -> i64 {
            self.updated_at
        }

        fn collection_name() -> &'static str {
            "test_records"
        }

        fn indexed_fields(&self) -> HashMap<String, IndexValue> {
            let mut fields = HashMap::new();
            fields.insert("status".to_string(), IndexValue::String(self.status.clone()));
            fields.insert("count".to_string(), IndexValue::Int(self.count));
            fields.insert("active".to_string(), IndexValue::Bool(self.active));
            fields
        }
    }

    #[test]
    fn test_store_open_creates_directory() {
        let temp = TempDir::new().unwrap();

        let _store = Store::open_at(temp.path().join(".taskstore")).unwrap();
        let store_path = temp.path().join(".taskstore");
        assert!(store_path.exists());
        assert!(store_path.join("taskstore.db").exists());
        assert!(store_path.join(".gitignore").exists());
        assert!(store_path.join(".version").exists());
    }

    #[test]
    fn test_record_index_fields_populated_on_create() {
        let temp = TempDir::new().unwrap();
        let mut store = Store::open_at(temp.path().join(".taskstore")).unwrap();

        let record = TestRecord {
            id: "rec1".to_string(),
            name: "Test".to_string(),
            status: "active".to_string(),
            count: 1,
            active: true,
            updated_at: now_ms(),
        };
        store.create(record).unwrap();

        let mut rows: Vec<(String, String, String)> = store
            .db()
            .prepare("SELECT collection, field_name, field_type FROM record_index_fields ORDER BY field_name")
            .unwrap()
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        rows.sort();

        assert_eq!(
            rows,
            vec![
                ("test_records".to_string(), "active".to_string(), "bool".to_string()),
                ("test_records".to_string(), "count".to_string(), "int".to_string()),
                ("test_records".to_string(), "status".to_string(), "string".to_string()),
            ]
        );
    }

    #[test]
    fn test_record_index_fields_no_duplicates_on_reinsert() {
        let temp = TempDir::new().unwrap();
        let mut store = Store::open_at(temp.path().join(".taskstore")).unwrap();

        for i in 0..3 {
            let record = TestRecord {
                id: format!("rec{i}"),
                name: "Test".to_string(),
                status: "active".to_string(),
                count: i,
                active: true,
                updated_at: now_ms(),
            };
            store.create(record).unwrap();
        }

        let count: i64 = store
            .db()
            .query_row(
                "SELECT COUNT(*) FROM record_index_fields WHERE collection = 'test_records'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 3, "should have exactly 3 field registrations, not 9");
    }

    #[test]
    fn test_sync_rebuilds_indexes_without_generic_type() {
        let temp = TempDir::new().unwrap();
        let mut store = Store::open_at(temp.path().join(".taskstore")).unwrap();

        // Seed records so record_index_fields gets populated
        for i in 0..3 {
            let record = TestRecord {
                id: format!("rec{i}"),
                name: format!("Test {i}"),
                status: if i % 2 == 0 { "active" } else { "inactive" }.to_string(),
                count: i,
                active: i % 2 == 0,
                updated_at: now_ms(),
            };
            store.create(record).unwrap();
        }

        // Nuke record_indexes directly (simulating what the current git-hook
        // `taskstore sync` used to leave behind).
        store.db().execute("DELETE FROM record_indexes", []).unwrap();
        let indexes_before: i64 = store
            .db()
            .query_row("SELECT COUNT(*) FROM record_indexes", [], |row| row.get(0))
            .unwrap();
        assert_eq!(indexes_before, 0);

        // Force staleness so sync actually runs on the JSONL corpus
        store.db().execute("DELETE FROM sync_metadata", []).unwrap();

        store.sync().unwrap();

        let indexes_after: i64 = store
            .db()
            .query_row("SELECT COUNT(*) FROM record_indexes", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            indexes_after, 9,
            "3 records x 3 indexed fields (status, count, active) = 9 index rows"
        );

        // And filtered queries should work without a rebuild_indexes<T>() call
        let filters = vec![Filter {
            field: "status".to_string(),
            op: FilterOp::Eq,
            value: IndexValue::String("active".to_string()),
        }];
        let results: Vec<TestRecord> = store.list(&filters).unwrap();
        assert_eq!(results.len(), 2, "expected 2 active records, got {}", results.len());
    }

    #[test]
    fn test_sync_is_transactional() {
        use std::sync::{Arc, Barrier};
        use std::thread;

        let temp = TempDir::new().unwrap();
        let path = temp.path().join(".taskstore");
        {
            let mut store = Store::open_at(&path).unwrap();
            for i in 0..20 {
                let record = TestRecord {
                    id: format!("rec{i}"),
                    name: format!("Test {i}"),
                    status: "active".to_string(),
                    count: i,
                    active: true,
                    updated_at: now_ms(),
                };
                store.create(record).unwrap();
            }
        }

        let barrier = Arc::new(Barrier::new(2));

        let writer_path = path.clone();
        let writer_barrier = barrier.clone();
        let writer = thread::spawn(move || {
            let mut store = Store::open_at(&writer_path).unwrap();
            // Force staleness
            store.db().execute("DELETE FROM sync_metadata", []).unwrap();
            writer_barrier.wait();
            store.sync().unwrap();
        });

        let reader_path = path.clone();
        let reader_barrier = barrier.clone();
        let reader = thread::spawn(move || {
            let store = Store::open_at(&reader_path).unwrap();
            reader_barrier.wait();
            let mut observed_empty = false;
            for _ in 0..200 {
                let n: i64 = store
                    .db()
                    .query_row(
                        "SELECT COUNT(*) FROM records WHERE collection = 'test_records'",
                        [],
                        |row| row.get(0),
                    )
                    .unwrap();
                if n == 0 {
                    observed_empty = true;
                    break;
                }
                if n != 20 {
                    panic!("reader observed intermediate count {n}, must be 0 or 20");
                }
            }
            assert!(
                !observed_empty,
                "reader observed an empty records table during sync; sync is not transactional"
            );
        });

        writer.join().unwrap();
        reader.join().unwrap();
    }

    #[test]
    fn test_store_open_enables_wal_mode() {
        let temp = TempDir::new().unwrap();
        let store = Store::open_at(temp.path().join(".taskstore")).unwrap();

        let mode: String = store
            .db()
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode.to_lowercase(), "wal", "journal_mode should be wal, got {mode:?}");

        let timeout: i64 = store
            .db()
            .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
            .unwrap();
        assert_eq!(timeout, BUSY_TIMEOUT_MS);

        let fk: i64 = store
            .db()
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .unwrap();
        assert_eq!(fk, 1);
    }

    #[test]
    fn test_generic_create() {
        let temp = TempDir::new().unwrap();
        let mut store = Store::open_at(temp.path().join(".taskstore")).unwrap();

        let record = TestRecord {
            id: "rec1".to_string(),
            name: "Test Record 1".to_string(),
            status: "active".to_string(),
            count: 42,
            active: true,
            updated_at: now_ms(),
        };

        let id = store.create(record.clone()).unwrap();
        assert_eq!(id, "rec1");

        // Verify JSONL file was created
        let jsonl_path = temp.path().join(".taskstore/test_records.jsonl");
        assert!(jsonl_path.exists());

        // Verify record in SQLite
        let retrieved: Option<TestRecord> = store.get("rec1").unwrap();
        assert!(retrieved.is_some());
        let retrieved = retrieved.unwrap();
        assert_eq!(retrieved.name, "Test Record 1");
        assert_eq!(retrieved.status, "active");
        assert_eq!(retrieved.count, 42);
        assert!(retrieved.active);
    }

    #[test]
    fn test_generic_get_nonexistent() {
        let temp = TempDir::new().unwrap();
        let store = Store::open_at(temp.path().join(".taskstore")).unwrap();

        let result: Option<TestRecord> = store.get("nonexistent").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_generic_update() {
        let temp = TempDir::new().unwrap();
        let mut store = Store::open_at(temp.path().join(".taskstore")).unwrap();

        // Create initial record
        let mut record = TestRecord {
            id: "rec1".to_string(),
            name: "Original".to_string(),
            status: "draft".to_string(),
            count: 1,
            active: false,
            updated_at: 1000,
        };
        store.create(record.clone()).unwrap();

        // Update record
        record.name = "Updated".to_string();
        record.status = "active".to_string();
        record.count = 2;
        record.active = true;
        record.updated_at = 2000;
        store.update(record.clone()).unwrap();

        // Verify update
        let retrieved: Option<TestRecord> = store.get("rec1").unwrap();
        assert!(retrieved.is_some());
        let retrieved = retrieved.unwrap();
        assert_eq!(retrieved.name, "Updated");
        assert_eq!(retrieved.status, "active");
        assert_eq!(retrieved.count, 2);
        assert!(retrieved.active);
        assert_eq!(retrieved.updated_at, 2000);
    }

    #[test]
    fn test_generic_delete() {
        let temp = TempDir::new().unwrap();
        let mut store = Store::open_at(temp.path().join(".taskstore")).unwrap();

        // Create record
        let record = TestRecord {
            id: "rec1".to_string(),
            name: "To Delete".to_string(),
            status: "active".to_string(),
            count: 1,
            active: true,
            updated_at: now_ms(),
        };
        store.create(record).unwrap();

        // Delete record
        store.delete::<TestRecord>("rec1").unwrap();

        // Verify deleted from SQLite
        let retrieved: Option<TestRecord> = store.get("rec1").unwrap();
        assert!(retrieved.is_none());

        // Verify tombstone in JSONL
        let jsonl_path = temp.path().join(".taskstore/test_records.jsonl");
        let content = fs::read_to_string(jsonl_path).unwrap();
        assert!(content.contains("\"deleted\":true"));
    }

    #[test]
    fn test_generic_list_no_filters() {
        let temp = TempDir::new().unwrap();
        let mut store = Store::open_at(temp.path().join(".taskstore")).unwrap();

        // Create multiple records
        for i in 1..=3 {
            let record = TestRecord {
                id: format!("rec{}", i),
                name: format!("Record {}", i),
                status: "active".to_string(),
                count: i,
                active: true,
                updated_at: now_ms(),
            };
            store.create(record).unwrap();
        }

        // List all records
        let records: Vec<TestRecord> = store.list(&[]).unwrap();
        assert_eq!(records.len(), 3);
    }

    #[test]
    fn test_generic_list_with_filter() {
        let temp = TempDir::new().unwrap();
        let mut store = Store::open_at(temp.path().join(".taskstore")).unwrap();

        // Create records with different statuses
        let record1 = TestRecord {
            id: "rec1".to_string(),
            name: "Record 1".to_string(),
            status: "active".to_string(),
            count: 1,
            active: true,
            updated_at: now_ms(),
        };
        let record2 = TestRecord {
            id: "rec2".to_string(),
            name: "Record 2".to_string(),
            status: "draft".to_string(),
            count: 2,
            active: true,
            updated_at: now_ms(),
        };

        store.create(record1).unwrap();
        store.create(record2).unwrap();

        // Filter by status = "active"
        let filters = vec![Filter {
            field: "status".to_string(),
            op: taskstore_traits::FilterOp::Eq,
            value: IndexValue::String("active".to_string()),
        }];

        let records: Vec<TestRecord> = store.list(&filters).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].status, "active");
    }

    #[test]
    fn test_validation_collection_name() {
        // Valid
        assert!(Store::validate_collection_name("valid_name").is_ok());
        assert!(Store::validate_collection_name("valid-name").is_ok());

        // Invalid
        assert!(Store::validate_collection_name("invalid/name").is_err());
        assert!(Store::validate_collection_name("").is_err());
        assert!(Store::validate_collection_name(&"a".repeat(65)).is_err());
    }

    #[test]
    fn test_validation_field_name() {
        // Valid
        assert!(Store::validate_field_name("valid_field").is_ok());

        // Invalid
        assert!(Store::validate_field_name("invalid-field").is_err());
        assert!(Store::validate_field_name("").is_err());
        assert!(Store::validate_field_name(&"a".repeat(65)).is_err());
    }

    // ========================================================================
    // create_many tests
    // ========================================================================

    // Record type whose indexed_fields() returns a hyphenated (invalid) field name,
    // used to test that validation catches bad field names before any I/O.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct BadFieldRecord {
        id: String,
        updated_at: i64,
    }

    impl Record for BadFieldRecord {
        fn id(&self) -> &str {
            &self.id
        }
        fn updated_at(&self) -> i64 {
            self.updated_at
        }
        fn collection_name() -> &'static str {
            "bad_field_records"
        }
        fn indexed_fields(&self) -> HashMap<String, IndexValue> {
            let mut fields = HashMap::new();
            fields.insert("bad-field".to_string(), IndexValue::String("val".to_string()));
            fields
        }
    }

    #[test]
    fn test_create_many_basic() {
        let temp = TempDir::new().unwrap();
        let mut store = Store::open_at(temp.path().join(".taskstore")).unwrap();

        let records: Vec<TestRecord> = (1..=3)
            .map(|i| TestRecord {
                id: format!("rec{}", i),
                name: format!("Record {}", i),
                status: "active".to_string(),
                count: i,
                active: true,
                updated_at: 1000 * i,
            })
            .collect();

        let ids = store.create_many(records.clone()).unwrap();
        assert_eq!(ids, vec!["rec1", "rec2", "rec3"]);

        // All retrievable via get
        for record in &records {
            let retrieved: Option<TestRecord> = store.get(record.id()).unwrap();
            assert!(retrieved.is_some());
            assert_eq!(retrieved.unwrap().name, record.name);
        }

        // All visible via list
        let listed: Vec<TestRecord> = store.list(&[]).unwrap();
        assert_eq!(listed.len(), 3);
    }

    #[test]
    fn test_create_many_empty() {
        let temp = TempDir::new().unwrap();
        let mut store = Store::open_at(temp.path().join(".taskstore")).unwrap();

        let ids = store.create_many::<TestRecord>(vec![]).unwrap();
        assert!(ids.is_empty());

        // No JSONL file created
        let jsonl_path = temp.path().join(".taskstore/test_records.jsonl");
        assert!(!jsonl_path.exists());
    }

    #[test]
    fn test_create_many_single() {
        let temp = TempDir::new().unwrap();
        let mut store = Store::open_at(temp.path().join(".taskstore")).unwrap();

        let record = TestRecord {
            id: "rec1".to_string(),
            name: "Record 1".to_string(),
            status: "active".to_string(),
            count: 1,
            active: true,
            updated_at: 1000,
        };

        let ids = store.create_many(vec![record.clone()]).unwrap();
        assert_eq!(ids, vec!["rec1"]);

        let retrieved: Option<TestRecord> = store.get("rec1").unwrap();
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap(), record);
    }

    #[test]
    fn test_create_many_duplicate_ids() {
        let temp = TempDir::new().unwrap();
        let mut store = Store::open_at(temp.path().join(".taskstore")).unwrap();

        // Pre-populate to establish the JSONL file
        store
            .create(TestRecord {
                id: "existing".to_string(),
                name: "Existing".to_string(),
                status: "active".to_string(),
                count: 0,
                active: true,
                updated_at: 1000,
            })
            .unwrap();

        let jsonl_path = temp.path().join(".taskstore/test_records.jsonl");
        let size_before = fs::metadata(&jsonl_path).unwrap().len();

        // Batch with duplicate IDs should fail
        let result = store.create_many(vec![
            TestRecord {
                id: "dup".to_string(),
                name: "First".to_string(),
                status: "active".to_string(),
                count: 1,
                active: true,
                updated_at: 1001,
            },
            TestRecord {
                id: "dup".to_string(),
                name: "Second".to_string(),
                status: "active".to_string(),
                count: 2,
                active: true,
                updated_at: 1002,
            },
        ]);

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Duplicate ID"));

        // JSONL must be unchanged (nothing written)
        let size_after = fs::metadata(&jsonl_path).unwrap().len();
        assert_eq!(size_before, size_after);
    }

    #[test]
    fn test_create_many_invalid_id() {
        let temp = TempDir::new().unwrap();
        let mut store = Store::open_at(temp.path().join(".taskstore")).unwrap();

        // Pre-populate to establish the JSONL file
        store
            .create(TestRecord {
                id: "existing".to_string(),
                name: "Existing".to_string(),
                status: "active".to_string(),
                count: 0,
                active: true,
                updated_at: 1000,
            })
            .unwrap();

        let jsonl_path = temp.path().join(".taskstore/test_records.jsonl");
        let size_before = fs::metadata(&jsonl_path).unwrap().len();

        // Batch containing an invalid (whitespace-only) ID should fail
        let result = store.create_many(vec![
            TestRecord {
                id: "valid".to_string(),
                name: "Valid".to_string(),
                status: "active".to_string(),
                count: 1,
                active: true,
                updated_at: 1001,
            },
            TestRecord {
                id: "   ".to_string(),
                name: "Invalid".to_string(),
                status: "active".to_string(),
                count: 2,
                active: true,
                updated_at: 1002,
            },
        ]);

        assert!(result.is_err());

        // JSONL must be unchanged (nothing written)
        let size_after = fs::metadata(&jsonl_path).unwrap().len();
        assert_eq!(size_before, size_after);
    }

    #[test]
    fn test_create_many_indexes() {
        let temp = TempDir::new().unwrap();
        let mut store = Store::open_at(temp.path().join(".taskstore")).unwrap();

        store
            .create_many(vec![
                TestRecord {
                    id: "rec1".to_string(),
                    name: "Record 1".to_string(),
                    status: "active".to_string(),
                    count: 1,
                    active: true,
                    updated_at: 1000,
                },
                TestRecord {
                    id: "rec2".to_string(),
                    name: "Record 2".to_string(),
                    status: "draft".to_string(),
                    count: 2,
                    active: false,
                    updated_at: 1001,
                },
                TestRecord {
                    id: "rec3".to_string(),
                    name: "Record 3".to_string(),
                    status: "active".to_string(),
                    count: 3,
                    active: true,
                    updated_at: 1002,
                },
            ])
            .unwrap();

        // Filter by status = "active"
        let active: Vec<TestRecord> = store
            .list(&[Filter {
                field: "status".to_string(),
                op: taskstore_traits::FilterOp::Eq,
                value: IndexValue::String("active".to_string()),
            }])
            .unwrap();
        assert_eq!(active.len(), 2);

        // Filter by active = false
        let inactive: Vec<TestRecord> = store
            .list(&[Filter {
                field: "active".to_string(),
                op: taskstore_traits::FilterOp::Eq,
                value: IndexValue::Bool(false),
            }])
            .unwrap();
        assert_eq!(inactive.len(), 1);
        assert_eq!(inactive[0].id, "rec2");
    }

    #[test]
    fn test_create_many_overwrites() {
        let temp = TempDir::new().unwrap();
        let mut store = Store::open_at(temp.path().join(".taskstore")).unwrap();

        // Create an initial record via create
        store
            .create(TestRecord {
                id: "rec1".to_string(),
                name: "Original".to_string(),
                status: "draft".to_string(),
                count: 1,
                active: false,
                updated_at: 1000,
            })
            .unwrap();

        // Overwrite via create_many (INSERT OR REPLACE)
        store
            .create_many(vec![TestRecord {
                id: "rec1".to_string(),
                name: "Updated".to_string(),
                status: "active".to_string(),
                count: 99,
                active: true,
                updated_at: 2000,
            }])
            .unwrap();

        let retrieved: Option<TestRecord> = store.get("rec1").unwrap();
        let retrieved = retrieved.unwrap();
        assert_eq!(retrieved.name, "Updated");
        assert_eq!(retrieved.count, 99);
        assert!(retrieved.active);

        // Verify indexes were updated: old values ("draft", false) must be gone,
        // new values ("active", true) must be queryable.
        let by_draft: Vec<TestRecord> = store
            .list(&[Filter {
                field: "status".to_string(),
                op: taskstore_traits::FilterOp::Eq,
                value: IndexValue::String("draft".to_string()),
            }])
            .unwrap();
        assert!(by_draft.is_empty(), "stale 'draft' index entry should be gone");

        let by_active: Vec<TestRecord> = store
            .list(&[Filter {
                field: "status".to_string(),
                op: taskstore_traits::FilterOp::Eq,
                value: IndexValue::String("active".to_string()),
            }])
            .unwrap();
        assert_eq!(by_active.len(), 1);
        assert_eq!(by_active[0].id, "rec1");

        let by_inactive: Vec<TestRecord> = store
            .list(&[Filter {
                field: "active".to_string(),
                op: taskstore_traits::FilterOp::Eq,
                value: IndexValue::Bool(false),
            }])
            .unwrap();
        assert!(
            by_inactive.is_empty(),
            "stale 'active=false' index entry should be gone"
        );
    }

    #[test]
    fn test_create_many_jsonl_batch_write() {
        let temp = TempDir::new().unwrap();
        let mut store = Store::open_at(temp.path().join(".taskstore")).unwrap();

        let records: Vec<TestRecord> = (1..=3)
            .map(|i| TestRecord {
                id: format!("rec{}", i),
                name: format!("Record {}", i),
                status: "active".to_string(),
                count: i,
                active: true,
                updated_at: 1000 * i,
            })
            .collect();

        store.create_many(records).unwrap();

        // JSONL file must have exactly 3 lines, each valid JSON
        let jsonl_path = temp.path().join(".taskstore/test_records.jsonl");
        let content = fs::read_to_string(&jsonl_path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 3);
        for line in &lines {
            assert!(serde_json::from_str::<serde_json::Value>(line).is_ok());
        }
    }

    #[test]
    fn test_create_many_validation_before_write() {
        let temp = TempDir::new().unwrap();
        let mut store = Store::open_at(temp.path().join(".taskstore")).unwrap();

        // BadFieldRecord returns a hyphenated field name which fails validate_field_name
        let result = store.create_many(vec![BadFieldRecord {
            id: "rec1".to_string(),
            updated_at: 1000,
        }]);

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Invalid field name"));

        // JSONL must not exist - nothing was written
        let jsonl_path = temp.path().join(".taskstore/bad_field_records.jsonl");
        assert!(!jsonl_path.exists());
    }

    #[test]
    fn test_create_many_jsonl_heals_sqlite() {
        let temp = TempDir::new().unwrap();
        let mut store = Store::open_at(temp.path().join(".taskstore")).unwrap();

        let records: Vec<TestRecord> = (1..=3)
            .map(|i| TestRecord {
                id: format!("rec{}", i),
                name: format!("Record {}", i),
                status: "active".to_string(),
                count: i,
                active: true,
                updated_at: 1000 * i,
            })
            .collect();

        store.create_many(records).unwrap();
        drop(store);

        // Delete the database to simulate corruption/loss
        let db_path = temp.path().join(".taskstore/taskstore.db");
        fs::remove_file(&db_path).unwrap();

        // Re-open forces sync from JSONL
        let store2 = Store::open_at(temp.path().join(".taskstore")).unwrap();

        // All records should be recoverable from JSONL
        for i in 1..=3 {
            let retrieved: Option<TestRecord> = store2.get(&format!("rec{}", i)).unwrap();
            assert!(retrieved.is_some(), "rec{} should be recoverable from JSONL", i);
            assert_eq!(retrieved.unwrap().name, format!("Record {}", i));
        }
    }
}
