// Generic store implementation using JSONL + SQLite

use crate::filter::{Filter, FilterOp};
use crate::jsonl;
use crate::record::{IndexValue, Record};
use eyre::{Context, Result, eyre};
use fs2::FileExt;
use rusqlite::Connection;
use rusqlite::OptionalExtension;
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

const CURRENT_VERSION: u32 = 1;

/// Generic persistent store with SQLite cache and JSONL source of truth
pub struct Store {
    base_path: PathBuf,
    db: Connection,
}

impl Store {
    /// Open or create a store at the given path
    ///
    /// The store will be created in a `.taskstore` subdirectory of the given path.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let base_path = path.as_ref().join(".taskstore");

        // Create directory if it doesn't exist
        fs::create_dir_all(&base_path).context("Failed to create store directory")?;

        // Open SQLite database
        let db_path = base_path.join("taskstore.db");
        let db = Connection::open(&db_path).context("Failed to open SQLite database")?;

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
        // Check each JSONL file
        for entry in fs::read_dir(&self.base_path)? {
            let entry = entry?;
            let path = entry.path();

            if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                continue;
            }

            let collection = match path.file_stem().and_then(|s| s.to_str()) {
                Some(c) => c,
                None => continue,
            };

            // Get file modification time
            let metadata = fs::metadata(&path)?;
            let file_mtime = metadata
                .modified()?
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);

            // Check if we have sync metadata for this collection
            let stored_mtime: Option<i64> = self
                .db
                .query_row(
                    "SELECT file_mtime FROM sync_metadata WHERE collection = ?1",
                    [collection],
                    |row| row.get(0),
                )
                .optional()?;

            match stored_mtime {
                None => return Ok(true),                              // Never synced
                Some(mtime) if file_mtime > mtime => return Ok(true), // File modified
                _ => continue,
            }
        }

        Ok(false)
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

        let mut stmt = self
            .db
            .prepare("SELECT data_json FROM records WHERE collection = ?1 AND id = ?2")?;

        let result = stmt
            .query_row(rusqlite::params![collection, id], |row| {
                let json: String = row.get(0)?;
                Ok(json)
            })
            .optional()?;

        match result {
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

        // If no filters, return all records
        if filters.is_empty() {
            let mut stmt = self
                .db
                .prepare("SELECT data_json FROM records WHERE collection = ?1 ORDER BY updated_at DESC")?;

            let rows = stmt.query_map([collection], |row| row.get::<_, String>(0))?;

            let mut results = Vec::new();
            for row_result in rows {
                let data_json = row_result?;
                let record: T = serde_json::from_str(&data_json).context("Failed to deserialize record")?;
                results.push(record);
            }
            return Ok(results);
        }

        // With filters: query the record_indexes table
        let mut query = String::from(
            "SELECT DISTINCT r.data_json
             FROM records r
             WHERE r.collection = ?1",
        );

        for (i, filter) in filters.iter().enumerate() {
            Self::validate_field_name(&filter.field)?;

            let join_alias = format!("idx{}", i);
            query.push_str(&format!(
                " AND EXISTS (
                    SELECT 1 FROM record_indexes {}
                    WHERE {}.collection = r.collection
                      AND {}.id = r.id
                      AND {}.field_name = ?{}",
                join_alias,
                join_alias,
                join_alias,
                join_alias,
                i + 2
            ));

            // Add value comparison based on type
            match &filter.value {
                IndexValue::String(_) => {
                    query.push_str(&format!(
                        " AND {}.field_value_str {} ?{}",
                        join_alias,
                        filter.op.to_sql(),
                        i + 2 + filters.len()
                    ));
                }
                IndexValue::Int(_) => {
                    query.push_str(&format!(
                        " AND {}.field_value_int {} ?{}",
                        join_alias,
                        filter.op.to_sql(),
                        i + 2 + filters.len()
                    ));
                }
                IndexValue::Bool(_) => {
                    query.push_str(&format!(
                        " AND {}.field_value_bool {} ?{}",
                        join_alias,
                        filter.op.to_sql(),
                        i + 2 + filters.len()
                    ));
                }
            }

            query.push(')');
        }

        query.push_str(" ORDER BY r.updated_at DESC");

        let mut stmt = self.db.prepare(&query)?;

        // Bind parameters: collection, then field names, then values
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        params.push(Box::new(collection.to_string()));

        // Field names
        for filter in filters {
            params.push(Box::new(filter.field.clone()));
        }

        // Values
        for filter in filters {
            match &filter.value {
                IndexValue::String(s) => params.push(Box::new(s.clone())),
                IndexValue::Int(i) => params.push(Box::new(*i)),
                IndexValue::Bool(b) => params.push(Box::new(*b as i64)),
            }
        }

        let params_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();

        let rows = stmt.query_map(params_refs.as_slice(), |row| row.get::<_, String>(0))?;

        let mut results = Vec::new();
        for row_result in rows {
            let data_json = row_result?;
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
            Self::validate_field_name(field_name)?;

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

    fn validate_field_name(name: &str) -> Result<()> {
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

    /// Sync SQLite database from JSONL files
    ///
    /// After sync, call `rebuild_indexes::<T>()` for each record type to restore indexes.
    pub fn sync(&mut self) -> Result<()> {
        info!("Syncing database from JSONL files");

        // Clear all tables
        self.db.execute("DELETE FROM record_indexes", [])?;
        self.db.execute("DELETE FROM records", [])?;

        // Read all JSONL files
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

            // Get file modification time for staleness tracking
            let file_mtime = fs::metadata(&path)?
                .modified()?
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);

            // Read records from JSONL
            let records = jsonl::read_jsonl_latest(&path)?;

            // Insert into SQLite
            for (id, record) in records {
                // Skip tombstones
                if record.get("deleted").and_then(|v| v.as_bool()).unwrap_or(false) {
                    continue;
                }

                let data_json = serde_json::to_string(&record)?;
                let updated_at = record.get("updated_at").and_then(|v| v.as_i64()).unwrap_or(0);

                self.db.execute(
                    "INSERT OR REPLACE INTO records (collection, id, data_json, updated_at)
                     VALUES (?1, ?2, ?3, ?4)",
                    rusqlite::params![collection, &id, data_json, updated_at],
                )?;

                // Note: We don't restore indexes during sync since we don't know
                // which fields were indexed. Call rebuild_indexes<T>() after sync.
            }

            // Record sync metadata for this collection
            self.db.execute(
                "INSERT OR REPLACE INTO sync_metadata (collection, last_sync_time, file_mtime)
                 VALUES (?1, ?2, ?3)",
                rusqlite::params![collection, now_ms(), file_mtime],
            )?;
        }

        // Clean up orphaned sync metadata (for deleted JSONL files)
        self.db.execute(
            "DELETE FROM sync_metadata WHERE collection NOT IN (SELECT DISTINCT collection FROM records)",
            [],
        )?;

        info!("Sync complete");
        Ok(())
    }

    /// Rebuild indexes for a specific record type after sync
    ///
    /// Call this for each record type after `sync()` completes. The method:
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

        let _store = Store::open(temp.path()).unwrap();
        let store_path = temp.path().join(".taskstore");
        assert!(store_path.exists());
        assert!(store_path.join("taskstore.db").exists());
        assert!(store_path.join(".gitignore").exists());
        assert!(store_path.join(".version").exists());
    }

    #[test]
    fn test_generic_create() {
        let temp = TempDir::new().unwrap();
        let mut store = Store::open(temp.path()).unwrap();

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
        let store = Store::open(temp.path()).unwrap();

        let result: Option<TestRecord> = store.get("nonexistent").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_generic_update() {
        let temp = TempDir::new().unwrap();
        let mut store = Store::open(temp.path()).unwrap();

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
        let mut store = Store::open(temp.path()).unwrap();

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
        let mut store = Store::open(temp.path()).unwrap();

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
        let mut store = Store::open(temp.path()).unwrap();

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
            op: crate::filter::FilterOp::Eq,
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
        let mut store = Store::open(temp.path()).unwrap();

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
        let mut store = Store::open(temp.path()).unwrap();

        let ids = store.create_many::<TestRecord>(vec![]).unwrap();
        assert!(ids.is_empty());

        // No JSONL file created
        let jsonl_path = temp.path().join(".taskstore/test_records.jsonl");
        assert!(!jsonl_path.exists());
    }

    #[test]
    fn test_create_many_single() {
        let temp = TempDir::new().unwrap();
        let mut store = Store::open(temp.path()).unwrap();

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
        let mut store = Store::open(temp.path()).unwrap();

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
        let mut store = Store::open(temp.path()).unwrap();

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
        let mut store = Store::open(temp.path()).unwrap();

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
                op: crate::filter::FilterOp::Eq,
                value: IndexValue::String("active".to_string()),
            }])
            .unwrap();
        assert_eq!(active.len(), 2);

        // Filter by active = false
        let inactive: Vec<TestRecord> = store
            .list(&[Filter {
                field: "active".to_string(),
                op: crate::filter::FilterOp::Eq,
                value: IndexValue::Bool(false),
            }])
            .unwrap();
        assert_eq!(inactive.len(), 1);
        assert_eq!(inactive[0].id, "rec2");
    }

    #[test]
    fn test_create_many_overwrites() {
        let temp = TempDir::new().unwrap();
        let mut store = Store::open(temp.path()).unwrap();

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
    }

    #[test]
    fn test_create_many_jsonl_batch_write() {
        let temp = TempDir::new().unwrap();
        let mut store = Store::open(temp.path()).unwrap();

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
        let mut store = Store::open(temp.path()).unwrap();

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
        let mut store = Store::open(temp.path()).unwrap();

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
        let store2 = Store::open(temp.path()).unwrap();

        // All records should be recoverable from JSONL
        for i in 1..=3 {
            let retrieved: Option<TestRecord> = store2.get(&format!("rec{}", i)).unwrap();
            assert!(retrieved.is_some(), "rec{} should be recoverable from JSONL", i);
            assert_eq!(retrieved.unwrap().name, format!("Record {}", i));
        }
    }
}
