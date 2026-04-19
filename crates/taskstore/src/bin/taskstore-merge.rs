// Git merge driver for JSONL files
//
// Usage: taskstore-merge %O %A %B
// Where: %O = ancestor file, %A = ours, %B = theirs
//
// Exit codes:
//   0 = merge successful
//   1 = conflict (manual resolution required)
//   2 = error

use eyre::{Context, Result};
use serde_json::Value;
use std::collections::HashMap;
use std::env;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process;

fn main() {
    if let Err(e) = run() {
        eprintln!("Error: {:#}", e);
        process::exit(2);
    }
}

fn run() -> Result<()> {
    let args: Vec<String> = env::args().collect();

    if args.len() != 4 {
        eprintln!("Usage: taskstore-merge <ancestor> <ours> <theirs>");
        eprintln!("Example: taskstore-merge plans.jsonl.base plans.jsonl plans.jsonl.theirs");
        return Ok(());
    }

    let ancestor_path = &args[1];
    let ours_path = &args[2];
    let theirs_path = &args[3];

    let result = merge_jsonl_files(ancestor_path, ours_path, theirs_path)?;

    // Write merged result to ours file (this is what git expects)
    fs::write(ours_path, result.content)?;

    if result.has_conflicts {
        eprintln!("Merge completed with conflicts - manual resolution required");
        process::exit(1);
    }

    Ok(())
}

struct MergeResult {
    content: String,
    has_conflicts: bool,
}

/// Merge three JSONL files using three-way merge logic
fn merge_jsonl_files(ancestor_path: &str, ours_path: &str, theirs_path: &str) -> Result<MergeResult> {
    // Parse all three files
    let ancestor_records = parse_jsonl(ancestor_path)?;
    let ours_records = parse_jsonl(ours_path)?;
    let theirs_records = parse_jsonl(theirs_path)?;

    // Build maps of latest record per ID
    let ancestor_map = build_latest_map(ancestor_records);
    let ours_map = build_latest_map(ours_records);
    let theirs_map = build_latest_map(theirs_records);

    // Perform three-way merge
    let mut merged = HashMap::new();
    let mut conflicts = Vec::new();

    // Collect all unique IDs
    let mut all_ids: Vec<String> = ours_map
        .keys()
        .chain(theirs_map.keys())
        .map(|k| k.to_string())
        .collect();
    all_ids.sort();
    all_ids.dedup();

    for id in all_ids {
        let ancestor = ancestor_map.get(&id);
        let ours = ours_map.get(&id);
        let theirs = theirs_map.get(&id);

        match (ancestor, ours, theirs) {
            (None, Some(o), None) => {
                // Added in ours only
                merged.insert(id, o.clone());
            }
            (None, None, Some(t)) => {
                // Added in theirs only
                merged.insert(id, t.clone());
            }
            (Some(_), Some(_o), None) => {
                // Deleted in theirs, keep deletion
                // (don't add to merged)
            }
            (Some(_), None, Some(_t)) => {
                // Deleted in ours, keep deletion
                // (don't add to merged)
            }
            (None, Some(o), Some(t)) => {
                // Added in both (concurrent add)
                if records_equal(o, t) {
                    merged.insert(id.clone(), o.clone());
                } else {
                    // Different versions added, use timestamp resolution
                    let ours_timestamp = get_updated_at(o);
                    let theirs_timestamp = get_updated_at(t);

                    if ours_timestamp > theirs_timestamp {
                        merged.insert(id.clone(), o.clone());
                    } else if theirs_timestamp > ours_timestamp {
                        merged.insert(id.clone(), t.clone());
                    } else {
                        // Same timestamp, conflict
                        conflicts.push((id.clone(), o.clone(), t.clone()));
                    }
                }
            }
            (Some(_), Some(o), Some(t)) => {
                // Modified in both (or one), need to merge
                if records_equal(o, t) {
                    // Both made same change
                    merged.insert(id.clone(), o.clone());
                } else {
                    // Different changes, pick based on timestamp
                    let ours_timestamp = get_updated_at(o);
                    let theirs_timestamp = get_updated_at(t);

                    if ours_timestamp > theirs_timestamp {
                        merged.insert(id.clone(), o.clone());
                    } else if theirs_timestamp > ours_timestamp {
                        merged.insert(id.clone(), t.clone());
                    } else {
                        // Same timestamp, conflict
                        conflicts.push((id.clone(), o.clone(), t.clone()));
                    }
                }
            }
            _ => {
                // Other cases: (None, None, None) and (Some(_), None, None)
                // These shouldn't happen as we're iterating over keys from ours/theirs
                // but we need to handle them for exhaustiveness
            }
        }
    }

    // Build output
    let mut output = String::new();
    let has_conflicts = !conflicts.is_empty();

    // Write merged records (sorted by ID for determinism)
    let mut ids: Vec<_> = merged.keys().collect();
    ids.sort();

    for id in ids {
        let record = &merged[id];
        output.push_str(&serde_json::to_string(record)?);
        output.push('\n');
    }

    // Write conflicts
    for (id, ours, theirs) in conflicts {
        output.push_str(&format!("<<<<<<< OURS ({})\n", id));
        output.push_str(&serde_json::to_string(&ours)?);
        output.push_str("\n=======\n");
        output.push_str(&serde_json::to_string(&theirs)?);
        output.push_str("\n>>>>>>> THEIRS\n");
    }

    Ok(MergeResult {
        content: output,
        has_conflicts,
    })
}

/// Parse a JSONL file into a vector of JSON values
fn parse_jsonl(path: &str) -> Result<Vec<Value>> {
    let path_obj = Path::new(path);
    if !path_obj.exists() {
        return Ok(Vec::new());
    }

    let file = File::open(path).context("Failed to open JSONL file")?;
    let reader = BufReader::new(file);
    let mut records = Vec::new();

    for line in reader.lines() {
        let line = line.context("Failed to read line")?;
        if line.trim().is_empty() {
            continue;
        }

        let value: Value = serde_json::from_str(&line).context("Failed to parse JSON line")?;
        records.push(value);
    }

    Ok(records)
}

/// Build a map of ID -> latest record (by updated_at)
fn build_latest_map(records: Vec<Value>) -> HashMap<String, Value> {
    let mut map = HashMap::new();

    for record in records {
        if let Some(id) = record.get("id").and_then(|v| v.as_str()) {
            let id = id.to_string();
            let timestamp = get_updated_at(&record);

            if let Some(existing) = map.get(&id) {
                let existing_timestamp = get_updated_at(existing);
                if timestamp > existing_timestamp {
                    map.insert(id, record);
                }
            } else {
                map.insert(id, record);
            }
        }
    }

    map
}

/// Get updated_at timestamp from a record (or created_at as fallback)
fn get_updated_at(record: &Value) -> i64 {
    record
        .get("updated_at")
        .and_then(|v| v.as_i64())
        .or_else(|| record.get("created_at").and_then(|v| v.as_i64()))
        .unwrap_or(0)
}

/// Check if two records are semantically equal (ignoring formatting)
fn records_equal(a: &Value, b: &Value) -> bool {
    a == b
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_merge_no_conflict() {
        let temp = TempDir::new().unwrap();

        // Ancestor
        let ancestor = temp.path().join("ancestor.jsonl");
        fs::write(
            &ancestor,
            r#"{"id":"1","title":"Original","updated_at":1000}
"#,
        )
        .unwrap();

        // Ours (updated title)
        let ours = temp.path().join("ours.jsonl");
        fs::write(
            &ours,
            r#"{"id":"1","title":"Updated by us","updated_at":2000}
"#,
        )
        .unwrap();

        // Theirs (no change)
        let theirs = temp.path().join("theirs.jsonl");
        fs::write(
            &theirs,
            r#"{"id":"1","title":"Original","updated_at":1000}
"#,
        )
        .unwrap();

        let result = merge_jsonl_files(
            ancestor.to_str().unwrap(),
            ours.to_str().unwrap(),
            theirs.to_str().unwrap(),
        )
        .unwrap();

        assert!(!result.has_conflicts);
        assert!(result.content.contains("Updated by us"));
    }

    #[test]
    fn test_merge_both_modified_newer_wins() {
        let temp = TempDir::new().unwrap();

        let ancestor = temp.path().join("ancestor.jsonl");
        fs::write(
            &ancestor,
            r#"{"id":"1","title":"Original","updated_at":1000}
"#,
        )
        .unwrap();

        let ours = temp.path().join("ours.jsonl");
        fs::write(
            &ours,
            r#"{"id":"1","title":"Updated by us","updated_at":2000}
"#,
        )
        .unwrap();

        let theirs = temp.path().join("theirs.jsonl");
        fs::write(
            &theirs,
            r#"{"id":"1","title":"Updated by them","updated_at":3000}
"#,
        )
        .unwrap();

        let result = merge_jsonl_files(
            ancestor.to_str().unwrap(),
            ours.to_str().unwrap(),
            theirs.to_str().unwrap(),
        )
        .unwrap();

        assert!(!result.has_conflicts);
        assert!(result.content.contains("Updated by them")); // Theirs wins (newer)
    }

    #[test]
    fn test_merge_same_timestamp_conflict() {
        let temp = TempDir::new().unwrap();

        let ancestor = temp.path().join("ancestor.jsonl");
        fs::write(
            &ancestor,
            r#"{"id":"1","title":"Original","updated_at":1000}
"#,
        )
        .unwrap();

        let ours = temp.path().join("ours.jsonl");
        fs::write(
            &ours,
            r#"{"id":"1","title":"Updated by us","updated_at":2000}
"#,
        )
        .unwrap();

        let theirs = temp.path().join("theirs.jsonl");
        fs::write(
            &theirs,
            r#"{"id":"1","title":"Updated by them","updated_at":2000}
"#,
        )
        .unwrap();

        let result = merge_jsonl_files(
            ancestor.to_str().unwrap(),
            ours.to_str().unwrap(),
            theirs.to_str().unwrap(),
        )
        .unwrap();

        assert!(result.has_conflicts);
        assert!(result.content.contains("<<<<<<< OURS"));
        assert!(result.content.contains(">>>>>>> THEIRS"));
    }

    #[test]
    fn test_merge_added_in_both() {
        let temp = TempDir::new().unwrap();

        let ancestor = temp.path().join("ancestor.jsonl");
        fs::write(&ancestor, "").unwrap();

        let ours = temp.path().join("ours.jsonl");
        fs::write(
            &ours,
            r#"{"id":"1","title":"Added by us","updated_at":1000}
"#,
        )
        .unwrap();

        let theirs = temp.path().join("theirs.jsonl");
        fs::write(
            &theirs,
            r#"{"id":"1","title":"Added by them","updated_at":2000}
"#,
        )
        .unwrap();

        let result = merge_jsonl_files(
            ancestor.to_str().unwrap(),
            ours.to_str().unwrap(),
            theirs.to_str().unwrap(),
        )
        .unwrap();

        assert!(!result.has_conflicts);
        assert!(result.content.contains("Added by them")); // Newer wins
    }
}
