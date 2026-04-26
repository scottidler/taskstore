// Phase 3 test contract from docs/design/2026-04-25-list-tolerant.md.
//
// Tests live in their own file (Rust 2018+ submodule pattern: declared in
// store.rs as `#[cfg(test)] mod list_tolerant_tests;`, body here) so the
// existing 1700-line store.rs is not pushed further over the size threshold.

use super::*;
use crate::CorruptionError;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use tempfile::TempDir;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct Note {
    id: String,
    title: String,
    status: String,
    updated_at: i64,
}

impl Record for Note {
    fn id(&self) -> &str {
        &self.id
    }

    fn updated_at(&self) -> i64 {
        self.updated_at
    }

    fn collection_name() -> &'static str {
        "notes"
    }

    fn indexed_fields(&self) -> HashMap<String, IndexValue> {
        let mut m = HashMap::new();
        m.insert("status".to_string(), IndexValue::String(self.status.clone()));
        m
    }
}

fn fresh_store() -> (TempDir, Store) {
    let temp = TempDir::new().expect("tempdir");
    let store = Store::open_at(temp.path().join(".taskstore")).expect("open");
    (temp, store)
}

fn write_jsonl(store_path: &std::path::Path, body: &str) {
    fs::create_dir_all(store_path).unwrap();
    fs::write(store_path.join("notes.jsonl"), body).unwrap();
}

#[test]
fn list_tolerant_100_lines_3_corrupted_one_each_variant() {
    let temp = TempDir::new().unwrap();
    let store_path = temp.path().join(".taskstore");

    // 100 lines: 1 syntax-error, 1 missing id, 1 EOF (truncated trailing object),
    // and 97 valid records.
    let mut body = String::new();
    for i in 0..100 {
        if i == 10 {
            body.push_str("{not valid json}\n");
        } else if i == 20 {
            body.push_str("{\"title\":\"NoId\",\"status\":\"x\",\"updated_at\":1}\n");
        } else if i == 30 {
            // EOF-style: opens a record but never closes it
            body.push_str("{\"id\":\"trunc\",\"title\":\"never closes\"\n");
        } else {
            body.push_str(&format!(
                "{{\"id\":\"n{i:03}\",\"title\":\"t{i}\",\"status\":\"active\",\"updated_at\":{}}}\n",
                1000 + i
            ));
        }
    }
    write_jsonl(&store_path, &body);

    let store = Store::open_at(&store_path).unwrap();
    let result = store.list_tolerant::<Note>(&[]).unwrap();

    assert_eq!(result.corruption.len(), 3, "three corrupt lines expected");
    let lines: Vec<u64> = result.corruption.iter().map(|c| c.line).collect();
    assert!(lines.contains(&11), "syntax-error line should be reported");
    assert!(lines.contains(&21), "missing-id line should be reported");
    assert!(lines.contains(&31), "EOF-style truncated line should be reported");

    let mut found_invalid = false;
    let mut found_missing = false;
    for entry in &result.corruption {
        match &entry.error {
            CorruptionError::InvalidJson { .. } => found_invalid = true,
            CorruptionError::MissingId => found_missing = true,
            _ => {}
        }
        assert_eq!(entry.file, store_path.join("notes.jsonl"));
        assert!(!entry.raw.is_empty() || matches!(entry.error, CorruptionError::Io { .. }));
    }
    assert!(found_invalid, "InvalidJson variant must be present");
    assert!(found_missing, "MissingId variant must be present");

    // 97 valid lines, all with the same status -> 97 records, no filter applied.
    assert_eq!(result.records.len(), 97);
}

#[test]
fn list_tolerant_type_mismatch_lww() {
    let temp = TempDir::new().unwrap();
    let store_path = temp.path().join(".taskstore");

    // Same id, line 1 valid as Note, line 2 valid JSON but missing required field
    // ("title" is non-optional). Line 2's updated_at is later -> wins LWW; typed
    // deserialization fails on it and surfaces as TypeMismatch with line == 2.
    let body = r#"{"id":"x","title":"good","status":"active","updated_at":1000}
{"id":"x","status":"active","updated_at":2000}
"#;
    write_jsonl(&store_path, body);

    let store = Store::open_at(&store_path).unwrap();
    let result = store.list_tolerant::<Note>(&[]).unwrap();

    assert_eq!(result.records.len(), 0, "id x should NOT be in records");
    assert_eq!(result.corruption.len(), 1);
    assert_eq!(result.corruption[0].line, 2);
    assert!(matches!(
        result.corruption[0].error,
        CorruptionError::TypeMismatch { .. }
    ));
}

#[test]
fn list_tolerant_filter_pushdown_parity() {
    let (_temp, mut store) = fresh_store();
    for i in 0..20 {
        let n = Note {
            id: format!("n{i}"),
            title: format!("t{i}"),
            status: if i % 2 == 0 { "active" } else { "inactive" }.to_string(),
            updated_at: 1000 + i,
        };
        store.create(n).unwrap();
    }

    let filter = Filter {
        field: "status".to_string(),
        op: FilterOp::Eq,
        value: IndexValue::String("active".to_string()),
    };

    let via_list: Vec<Note> = store.list(std::slice::from_ref(&filter)).unwrap();
    let via_tolerant = store.list_tolerant::<Note>(std::slice::from_ref(&filter)).unwrap();

    let mut a: Vec<String> = via_list.iter().map(|n| n.id.clone()).collect();
    let mut b: Vec<String> = via_tolerant.records.iter().map(|n| n.id.clone()).collect();
    a.sort();
    b.sort();
    assert_eq!(a, b, "list and list_tolerant must return the same set");
    assert_eq!(via_tolerant.corruption.len(), 0);
}

#[test]
fn list_tolerant_contains_case_parity() {
    let (_temp, mut store) = fresh_store();
    // Mixed-case statuses; Contains should match either case.
    for (i, status) in ["ACTIVE-task", "active-todo", "inactive", "Active-extra"]
        .iter()
        .enumerate()
    {
        let n = Note {
            id: format!("n{i}"),
            title: format!("t{i}"),
            status: status.to_string(),
            updated_at: 1000 + i as i64,
        };
        store.create(n).unwrap();
    }

    let filter = Filter {
        field: "status".to_string(),
        op: FilterOp::Contains,
        value: IndexValue::String("ACTIVE".to_string()),
    };

    let via_list: Vec<Note> = store.list(std::slice::from_ref(&filter)).unwrap();
    let via_tolerant = store.list_tolerant::<Note>(std::slice::from_ref(&filter)).unwrap();

    let mut a: Vec<String> = via_list.iter().map(|n| n.id.clone()).collect();
    let mut b: Vec<String> = via_tolerant.records.iter().map(|n| n.id.clone()).collect();
    a.sort();
    b.sort();
    assert_eq!(a, b, "Contains case-insensitive parity between list and list_tolerant");
}

#[test]
fn list_tolerant_empty_returns_empty() {
    let temp = TempDir::new().unwrap();
    let store_path = temp.path().join(".taskstore");
    write_jsonl(&store_path, ""); // empty file

    let store = Store::open_at(&store_path).unwrap();
    let result = store.list_tolerant::<Note>(&[]).unwrap();

    assert!(result.records.is_empty());
    assert!(result.corruption.is_empty());
}

#[test]
fn list_tolerant_missing_returns_empty() {
    let (_temp, store) = fresh_store();
    // No JSONL written; collection has never been touched.
    let result = store.list_tolerant::<Note>(&[]).unwrap();
    assert!(result.records.is_empty());
    assert!(result.corruption.is_empty());
}

#[test]
fn list_tolerant_tombstones_filtered_not_counted() {
    let temp = TempDir::new().unwrap();
    let store_path = temp.path().join(".taskstore");

    // 10 records, 5 of which have tombstone lines appended after them.
    let mut body = String::new();
    for i in 0..10 {
        body.push_str(&format!(
            "{{\"id\":\"n{i}\",\"title\":\"t{i}\",\"status\":\"a\",\"updated_at\":1000}}\n"
        ));
    }
    for i in 0..5 {
        body.push_str(&format!("{{\"id\":\"n{i}\",\"deleted\":true,\"updated_at\":2000}}\n"));
    }
    write_jsonl(&store_path, &body);

    let store = Store::open_at(&store_path).unwrap();
    let result = store.list_tolerant::<Note>(&[]).unwrap();

    assert_eq!(result.records.len(), 5, "5 non-tombstoned records remain");
    assert_eq!(result.corruption.len(), 0, "valid tombstones are NOT corruption");
}

#[test]
fn list_tolerant_corrupt_tombstone_leaves_target_alive() {
    let temp = TempDir::new().unwrap();
    let store_path = temp.path().join(".taskstore");

    // Valid record, then a malformed line that was meant to be a tombstone
    // but failed to parse. Per design: target stays alive; corruption surfaces.
    let body = r#"{"id":"victim","title":"alive","status":"a","updated_at":1000}
{"id":"victim","deleted":true,"updated_at":truncated_garbage
"#;
    write_jsonl(&store_path, body);

    let store = Store::open_at(&store_path).unwrap();
    let result = store.list_tolerant::<Note>(&[]).unwrap();

    assert_eq!(result.records.len(), 1, "victim record stays alive");
    assert_eq!(result.records[0].id, "victim");
    assert_eq!(result.corruption.len(), 1, "corrupt tombstone line surfaces");
    assert!(matches!(
        result.corruption[0].error,
        CorruptionError::InvalidJson { .. }
    ));
}

#[test]
fn list_tolerant_after_sync_list_returns_silent_skip() {
    let temp = TempDir::new().unwrap();
    let store_path = temp.path().join(".taskstore");

    // Two valid records, one malformed line in between.
    let body = r#"{"id":"a","title":"first","status":"x","updated_at":1000}
{not valid json}
{"id":"b","title":"second","status":"x","updated_at":1000}
"#;
    write_jsonl(&store_path, body);

    let mut store = Store::open_at(&store_path).unwrap();
    store.sync().expect("sync should succeed despite malformed line");

    // list<T> after sync: returns the two valid records, no error, no signal.
    let via_list: Vec<Note> = store.list(&[]).unwrap();
    assert_eq!(
        via_list.len(),
        2,
        "sync silently skips malformed line; list returns the valid pair"
    );

    // list_tolerant: same valid pair PLUS the corruption signal.
    let via_tolerant = store.list_tolerant::<Note>(&[]).unwrap();
    assert_eq!(via_tolerant.records.len(), 2);
    assert_eq!(via_tolerant.corruption.len(), 1);
    assert_eq!(via_tolerant.corruption[0].line, 2);
}
