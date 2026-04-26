//! Example 11: Corruption-aware audit (`list_tolerant`)
//!
//! Demonstrates `Store::list_tolerant<T>` over a deliberately corrupted JSONL.
//! `list<T>` would silently drop the malformed lines (and `sync()` warn-logs
//! and discards them); `list_tolerant<T>` surfaces them as `CorruptionEntry`s
//! with file/line/raw/error attribution.
//!
//! Run with: cargo run --example 11_corruption_audit

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use taskstore::{CorruptionError, IndexValue, Record, Result, Store};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Note {
    id: String,
    title: String,
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
        HashMap::new()
    }
}

fn main() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store_path = temp.path().join(".taskstore");
    fs::create_dir_all(&store_path)?;

    // Write a JSONL file with three good lines, one syntactically invalid line,
    // and one valid JSON line that lacks an `id` field.
    let jsonl = r#"{"id":"n1","title":"First","updated_at":1000}
{malformed json
{"id":"n2","title":"Second","updated_at":2000}
{"title":"No id","updated_at":3000}
{"id":"n3","title":"Third","updated_at":4000}
"#;
    fs::write(store_path.join("notes.jsonl"), jsonl)?;

    let store = Store::open_at(&store_path)?;

    println!("TaskStore list_tolerant Example");
    println!("================================\n");

    let result = store.list_tolerant::<Note>(&[])?;

    println!("Live records: {}", result.records.len());
    for note in &result.records {
        println!("  - {}: {}", note.id, note.title);
    }
    println!();

    println!("Corruption entries: {}", result.corruption.len());
    for entry in &result.corruption {
        let kind = match &entry.error {
            CorruptionError::InvalidJson { category, .. } => format!("InvalidJson({category:?})"),
            CorruptionError::MissingId => "MissingId".to_string(),
            CorruptionError::TypeMismatch { .. } => "TypeMismatch".to_string(),
            CorruptionError::Io { kind } => format!("Io({kind:?})"),
        };
        println!(
            "  - {} line {} [{}]: {}",
            entry.file.display(),
            entry.line,
            kind,
            entry.raw.lines().next().unwrap_or("")
        );
    }
    println!();

    println!("Tip: pair `list_tolerant` with `list` for hot-path reads.");
    println!("`list_tolerant` re-parses JSONL on every call - reserve for audit / sweep paths.");

    Ok(())
}
