// JSONL file operations
//
// Two read entrypoints share one streaming parser:
// - `read_jsonl_latest`: today's silent-skip behavior (used by `sync()`).
//   No memory regression: the parser stream-reduces into the dedup HashMap,
//   never materializing a `Vec` of all parsed lines.
// - `read_jsonl_latest_with_corruption`: same iteration, but Err arms become
//   `CorruptionEntry` instead of being warn-logged. Used by `list_tolerant`.

use crate::corruption::truncate_for_raw;
use crate::error::{Error, Result, category_from_serde_json};
use fs2::FileExt;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use taskstore_traits::{CorruptionEntry, CorruptionError};
use tracing::{info, warn};

/// Append a record to a JSONL file
pub fn append_jsonl<T: Serialize>(path: &Path, record: &T) -> Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| Error::Other(format!("Failed to open JSONL file for appending: {e}")))?;

    // Acquire exclusive lock before writing
    file.lock_exclusive()
        .map_err(|e| Error::Other(format!("Failed to acquire file lock: {e}")))?;

    let json = serde_json::to_string(record)?;
    writeln!(file, "{}", json)?;
    file.sync_all()?; // Ensure data is flushed to disk

    // Lock is automatically released when file is dropped
    Ok(())
}

/// One line from a JSONL file as seen by the streaming parser.
pub(crate) struct ParsedLine {
    pub(crate) line_no: u64,
    pub(crate) raw: String,
    pub(crate) outcome: std::result::Result<(String, Value), CorruptionError>,
}

/// Streaming parser. Opens `path` under a shared `fs2` lock, then calls
/// `for_each(parsed)` once per non-empty line. The callback receives an owned
/// `ParsedLine` and decides what to keep; the value drops when the callback
/// returns. Both wrappers stream-reduce inside the callback so peak memory is
/// proportional to unique records, not total lines.
fn for_each_jsonl_line<F>(path: &Path, mut for_each: F) -> Result<()>
where
    F: FnMut(ParsedLine),
{
    if !path.exists() {
        return Ok(());
    }

    let file = File::open(path).map_err(|e| Error::Other(format!("Failed to open JSONL file: {e}")))?;
    file.lock_shared()
        .map_err(|e| Error::Other(format!("Failed to acquire shared file lock: {e}")))?;

    let reader = BufReader::new(file);

    for (line_idx, line_result) in reader.lines().enumerate() {
        let line_no = (line_idx as u64) + 1;
        let line = match line_result {
            Ok(l) => l,
            Err(e) => {
                for_each(ParsedLine {
                    line_no,
                    raw: String::new(),
                    outcome: Err(CorruptionError::Io { kind: e.kind() }),
                });
                continue;
            }
        };

        if line.trim().is_empty() {
            continue;
        }

        let raw = truncate_for_raw(&line);

        match serde_json::from_str::<Value>(&line) {
            Ok(value) => match value.get("id").and_then(|v| v.as_str()) {
                Some(id) => {
                    let id = id.to_string();
                    for_each(ParsedLine {
                        line_no,
                        raw,
                        outcome: Ok((id, value)),
                    });
                }
                None => {
                    for_each(ParsedLine {
                        line_no,
                        raw,
                        outcome: Err(CorruptionError::MissingId),
                    });
                }
            },
            Err(e) => {
                for_each(ParsedLine {
                    line_no,
                    raw,
                    outcome: Err(CorruptionError::InvalidJson {
                        msg: e.to_string(),
                        category: category_from_serde_json(e.classify()),
                    }),
                });
            }
        }
    }

    Ok(())
}

/// Read all records from a JSONL file, returning latest version per ID.
///
/// This assumes records have an "id" field and "updated_at" field.
/// For records with duplicate IDs, the one with the highest updated_at wins.
///
/// Tolerates malformed lines: warn-logs each one and continues. Callers that
/// want diagnostic visibility into the skipped lines should use
/// [`read_jsonl_latest_with_corruption`] instead.
pub fn read_jsonl_latest(path: &Path) -> Result<HashMap<String, Value>> {
    let path_for_log = path.to_path_buf();
    let mut records: HashMap<String, Value> = HashMap::new();

    for_each_jsonl_line(path, |parsed| match parsed.outcome {
        Ok((id, value)) => {
            let updated_at = value.get("updated_at").and_then(|v| v.as_i64()).unwrap_or(0);
            if let Some(existing) = records.get(&id) {
                let existing_updated_at = existing.get("updated_at").and_then(|v| v.as_i64()).unwrap_or(0);
                if updated_at > existing_updated_at {
                    records.insert(id, value);
                }
            } else {
                records.insert(id, value);
            }
        }
        Err(err) => {
            warn!(
                file = ?path_for_log,
                line = parsed.line_no,
                error = ?err,
                "Failed to load JSONL line, skipping"
            );
        }
    })?;

    info!(
        file = ?path,
        count = records.len(),
        "Loaded latest records from JSONL"
    );

    Ok(records)
}

/// Map of `id -> (line_no, Value)`: the LWW-winning value for each id along
/// with the JSONL line number it came from. Used by
/// [`read_jsonl_latest_with_corruption`] so downstream typed deserialization
/// can attribute a `TypeMismatch` back to a specific line.
pub type LatestRecordsByLine = HashMap<String, (u64, Value)>;

/// Read all records plus diagnostics for malformed lines.
///
/// Same iteration and last-write-wins reduction as [`read_jsonl_latest`], but
/// the per-line errors are returned as `CorruptionEntry`s rather than
/// warn-logged and dropped. The returned map carries the line number of each
/// LWW-winning line so a downstream typed deserialization can attribute a
/// `TypeMismatch` back to a specific line.
pub fn read_jsonl_latest_with_corruption(path: &Path) -> Result<(LatestRecordsByLine, Vec<CorruptionEntry>)> {
    let mut records: LatestRecordsByLine = HashMap::new();
    let mut corruption: Vec<CorruptionEntry> = Vec::new();
    let path_for_entries = path.to_path_buf();

    for_each_jsonl_line(path, |parsed| match parsed.outcome {
        Ok((id, value)) => {
            let updated_at = value.get("updated_at").and_then(|v| v.as_i64()).unwrap_or(0);
            let line_no = parsed.line_no;
            if let Some((_, existing)) = records.get(&id) {
                let existing_updated_at = existing.get("updated_at").and_then(|v| v.as_i64()).unwrap_or(0);
                if updated_at > existing_updated_at {
                    records.insert(id, (line_no, value));
                }
            } else {
                records.insert(id, (line_no, value));
            }
        }
        Err(err) => {
            corruption.push(CorruptionEntry {
                file: path_for_entries.clone(),
                line: parsed.line_no,
                raw: parsed.raw,
                error: err,
            });
        }
    })?;

    Ok((records, corruption))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;
    use taskstore_traits::Category;
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
        // Existing test: silent-skip behavior preserved by the refactored helper.
        let temp = TempDir::new().unwrap();
        let jsonl_path = temp.path().join("test.jsonl");

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

    #[test]
    fn test_read_jsonl_latest_with_corruption_invalid_json() {
        let temp = TempDir::new().unwrap();
        let jsonl_path = temp.path().join("test.jsonl");

        fs::write(
            &jsonl_path,
            r#"{"id":"test-1","name":"Valid","updated_at":1000}
{malformed json}
{"id":"test-2","name":"Also Valid","updated_at":1000}
"#,
        )
        .unwrap();

        let (records, corruption) = read_jsonl_latest_with_corruption(&jsonl_path).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(corruption.len(), 1);
        assert_eq!(corruption[0].line, 2);
        assert!(matches!(
            corruption[0].error,
            CorruptionError::InvalidJson {
                category: Category::Syntax,
                ..
            }
        ));
        assert_eq!(corruption[0].file, jsonl_path);
        assert!(corruption[0].raw.contains("malformed"));
    }

    #[test]
    fn test_read_jsonl_latest_with_corruption_missing_id() {
        let temp = TempDir::new().unwrap();
        let jsonl_path = temp.path().join("test.jsonl");

        fs::write(
            &jsonl_path,
            r#"{"id":"test-1","name":"Valid","updated_at":1000}
{"name":"NoId","updated_at":1000}
"#,
        )
        .unwrap();

        let (records, corruption) = read_jsonl_latest_with_corruption(&jsonl_path).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(corruption.len(), 1);
        assert_eq!(corruption[0].line, 2);
        assert!(matches!(corruption[0].error, CorruptionError::MissingId));
    }

    #[test]
    fn test_read_jsonl_latest_with_corruption_lww_carries_line_no() {
        let temp = TempDir::new().unwrap();
        let jsonl_path = temp.path().join("test.jsonl");

        // Same id, line 1 older, line 2 newer (later updated_at wins).
        fs::write(
            &jsonl_path,
            r#"{"id":"test-1","name":"Old","updated_at":1000}
{"id":"test-1","name":"New","updated_at":2000}
"#,
        )
        .unwrap();

        let (records, corruption) = read_jsonl_latest_with_corruption(&jsonl_path).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(corruption.len(), 0);
        let (line_no, value) = records.get("test-1").unwrap();
        assert_eq!(*line_no, 2);
        assert_eq!(value.get("name").and_then(|v| v.as_str()), Some("New"));
    }

    #[test]
    fn test_read_jsonl_latest_with_corruption_empty_file() {
        let temp = TempDir::new().unwrap();
        let jsonl_path = temp.path().join("test.jsonl");
        fs::write(&jsonl_path, "").unwrap();

        let (records, corruption) = read_jsonl_latest_with_corruption(&jsonl_path).unwrap();
        assert_eq!(records.len(), 0);
        assert_eq!(corruption.len(), 0);
    }

    #[test]
    fn test_read_jsonl_latest_with_corruption_missing_file() {
        let temp = TempDir::new().unwrap();
        let jsonl_path = temp.path().join("nonexistent.jsonl");

        let (records, corruption) = read_jsonl_latest_with_corruption(&jsonl_path).unwrap();
        assert_eq!(records.len(), 0);
        assert_eq!(corruption.len(), 0);
    }
}
