// JSONL file operations

use eyre::{Context, Result};
use fs2::FileExt;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use tracing::{info, warn};

/// Append a record to a JSONL file
pub fn append_jsonl<T: Serialize>(path: &Path, record: &T) -> Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .context("Failed to open JSONL file for appending")?;

    // Acquire exclusive lock before writing
    file.lock_exclusive().context("Failed to acquire file lock")?;

    let json = serde_json::to_string(record)?;
    writeln!(file, "{}", json)?;
    file.sync_all()?; // Ensure data is flushed to disk

    // Lock is automatically released when file is dropped
    Ok(())
}

/// Read all records from a JSONL file, returning latest version per ID
///
/// This assumes records have an "id" field and "updated_at" field.
/// For records with duplicate IDs, the one with the highest updated_at wins.
pub fn read_jsonl_latest(path: &Path) -> Result<HashMap<String, Value>> {
    if !path.exists() {
        // File doesn't exist yet, return empty map
        return Ok(HashMap::new());
    }

    let file = File::open(path).context("Failed to open JSONL file")?;

    // Acquire shared lock to allow concurrent reads while blocking writes
    file.lock_shared().context("Failed to acquire shared file lock")?;

    let reader = BufReader::new(file);
    let mut records: HashMap<String, Value> = HashMap::new();

    for (line_num, line) in reader.lines().enumerate() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                warn!(
                    file = ?path,
                    line = line_num + 1,
                    error = ?e,
                    "Failed to read line, skipping"
                );
                continue;
            }
        };

        if line.trim().is_empty() {
            continue;
        }

        let record: Value = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                warn!(
                    file = ?path,
                    line = line_num + 1,
                    error = ?e,
                    "Failed to parse JSON, skipping"
                );
                continue;
            }
        };

        // Extract id and updated_at from JSON
        let id = match record.get("id").and_then(|v| v.as_str()) {
            Some(id_str) => id_str.to_string(),
            None => {
                warn!(
                    file = ?path,
                    line = line_num + 1,
                    "Record missing 'id' field, skipping"
                );
                continue;
            }
        };

        let updated_at = record.get("updated_at").and_then(|v| v.as_i64()).unwrap_or(0);

        // Keep the record with the latest updated_at
        if let Some(existing) = records.get(&id) {
            let existing_updated_at = existing.get("updated_at").and_then(|v| v.as_i64()).unwrap_or(0);
            if updated_at > existing_updated_at {
                records.insert(id, record);
            }
        } else {
            records.insert(id, record);
        }
    }

    info!(
        file = ?path,
        count = records.len(),
        "Loaded latest records from JSONL"
    );

    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_append_jsonl() {
        let temp = TempDir::new().unwrap();
        let jsonl_path = temp.path().join("test.jsonl");

        let record = json!({
            "id": "test-1",
            "name": "Test",
            "updated_at": 1000
        });

        append_jsonl(&jsonl_path, &record).unwrap();

        let content = fs::read_to_string(&jsonl_path).unwrap();
        assert!(content.contains("\"id\":\"test-1\""));
        assert!(content.contains("\"name\":\"Test\""));
    }

    #[test]
    fn test_read_jsonl_latest() {
        let temp = TempDir::new().unwrap();
        let jsonl_path = temp.path().join("test.jsonl");

        // Write multiple versions of same record
        let record1 = json!({
            "id": "test-1",
            "name": "Version 1",
            "updated_at": 1000
        });

        let record2 = json!({
            "id": "test-1",
            "name": "Version 2",
            "updated_at": 2000
        });

        append_jsonl(&jsonl_path, &record1).unwrap();
        append_jsonl(&jsonl_path, &record2).unwrap();

        // Read should return latest version
        let records = read_jsonl_latest(&jsonl_path).unwrap();
        assert_eq!(records.len(), 1);

        let latest = records.get("test-1").unwrap();
        assert_eq!(latest.get("name").and_then(|v| v.as_str()), Some("Version 2"));
        assert_eq!(latest.get("updated_at").and_then(|v| v.as_i64()), Some(2000));
    }

    #[test]
    fn test_read_jsonl_nonexistent_file() {
        let temp = TempDir::new().unwrap();
        let jsonl_path = temp.path().join("nonexistent.jsonl");

        let records = read_jsonl_latest(&jsonl_path).unwrap();
        assert_eq!(records.len(), 0);
    }

    #[test]
    fn test_read_jsonl_malformed_line() {
        let temp = TempDir::new().unwrap();
        let jsonl_path = temp.path().join("test.jsonl");

        // Write valid record, then malformed, then another valid
        fs::write(
            &jsonl_path,
            r#"{"id":"test-1","name":"Valid","updated_at":1000}
{malformed json}
{"id":"test-2","name":"Also Valid","updated_at":1000}
"#,
        )
        .unwrap();

        let records = read_jsonl_latest(&jsonl_path).unwrap();
        // Should skip malformed line and load the two valid records
        assert_eq!(records.len(), 2);
        assert!(records.contains_key("test-1"));
        assert!(records.contains_key("test-2"));
    }
}
