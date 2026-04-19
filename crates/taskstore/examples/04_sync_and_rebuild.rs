//! Example 04: Sync and Rebuild Operations
//!
//! This example demonstrates how TaskStore handles sync operations:
//! - Rebuilding SQLite from JSONL files
//! - Rebuilding indexes after sync
//! - Staleness detection
//!
//! Run with: cargo run --example 04_sync_and_rebuild

use eyre::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use taskstore::{Filter, FilterOp, IndexValue, Record, Store};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Document {
    id: String,
    title: String,
    category: String,
    version: i64,
    updated_at: i64,
}

impl Record for Document {
    fn id(&self) -> &str {
        &self.id
    }

    fn updated_at(&self) -> i64 {
        self.updated_at
    }

    fn collection_name() -> &'static str {
        "documents"
    }

    fn indexed_fields(&self) -> HashMap<String, IndexValue> {
        let mut fields = HashMap::new();
        fields.insert("category".to_string(), IndexValue::String(self.category.clone()));
        fields.insert("version".to_string(), IndexValue::Int(self.version));
        fields
    }
}

fn main() -> Result<()> {
    let temp_dir = tempfile::tempdir()?;
    // Store::open auto-adds .taskstore subdir, so we need the actual path for file access
    let store_path = temp_dir.path().join(".taskstore");

    println!("TaskStore Sync and Rebuild Example");
    println!("===================================\n");

    // Phase 1: Create initial data
    println!("Phase 1: Creating initial documents...");
    {
        let mut store = Store::open(temp_dir.path())?;

        let docs = vec![
            Document {
                id: "doc-001".to_string(),
                title: "Getting Started".to_string(),
                category: "tutorial".to_string(),
                version: 1,
                updated_at: 1000,
            },
            Document {
                id: "doc-002".to_string(),
                title: "API Reference".to_string(),
                category: "reference".to_string(),
                version: 2,
                updated_at: 2000,
            },
        ];

        for doc in docs {
            store.create(doc.clone())?;
            println!("   Created: {} - {}", doc.id, doc.title);
        }

        // Verify filtering works
        let tutorials: Vec<Document> = store.list(&[Filter {
            field: "category".to_string(),
            op: FilterOp::Eq,
            value: IndexValue::String("tutorial".to_string()),
        }])?;
        println!("   Tutorials found: {}", tutorials.len());
    }
    println!();

    // Phase 2: Simulate external modification (like git merge)
    println!("Phase 2: Simulating external JSONL modification...");
    {
        // Directly append to the JSONL file (simulating git merge bringing in changes)
        let jsonl_path = store_path.join("documents.jsonl");
        let mut file = fs::OpenOptions::new().append(true).open(&jsonl_path)?;

        // Add a new document via direct JSONL append
        let new_doc = serde_json::json!({
            "id": "doc-003",
            "title": "Best Practices",
            "category": "guide",
            "version": 1,
            "updated_at": 3000
        });
        writeln!(file, "{}", serde_json::to_string(&new_doc)?)?;
        file.sync_all()?;

        // Update an existing document with newer timestamp
        let updated_doc = serde_json::json!({
            "id": "doc-001",
            "title": "Getting Started (Updated)",
            "category": "tutorial",
            "version": 2,
            "updated_at": 4000
        });
        writeln!(file, "{}", serde_json::to_string(&updated_doc)?)?;
        file.sync_all()?;

        println!("   Added doc-003 directly to JSONL");
        println!("   Updated doc-001 directly in JSONL");
    }
    println!();

    // Phase 3: Reopen store - should detect staleness and sync
    println!("Phase 3: Reopening store (auto-detects staleness)...");
    {
        // Give filesystem time to update mtime
        std::thread::sleep(std::time::Duration::from_millis(100));

        let mut store = Store::open(temp_dir.path())?;

        // Check if store detected the changes
        let all_docs: Vec<Document> = store.list(&[])?;
        println!("   Total documents after sync: {}", all_docs.len());

        for doc in &all_docs {
            println!("   - {} : {} (v{})", doc.id, doc.title, doc.version);
        }

        // Note: After sync, indexes need to be rebuilt!
        println!("\n   Rebuilding indexes for Document type...");
        let indexed_count = store.rebuild_indexes::<Document>()?;
        println!("   Rebuilt indexes for {} documents", indexed_count);
    }
    println!();

    // Phase 4: Verify filtering works after rebuild
    println!("Phase 4: Verifying filters work after index rebuild...");
    {
        let store = Store::open(temp_dir.path())?;

        let tutorials: Vec<Document> = store.list(&[Filter {
            field: "category".to_string(),
            op: FilterOp::Eq,
            value: IndexValue::String("tutorial".to_string()),
        }])?;
        println!("   Tutorials: {} found", tutorials.len());
        for doc in &tutorials {
            println!("   - {} : {} (v{})", doc.id, doc.title, doc.version);
        }

        let guides: Vec<Document> = store.list(&[Filter {
            field: "category".to_string(),
            op: FilterOp::Eq,
            value: IndexValue::String("guide".to_string()),
        }])?;
        println!("   Guides: {} found", guides.len());
        for doc in &guides {
            println!("   - {} : {}", doc.id, doc.title);
        }
    }
    println!();

    // Phase 5: Manual sync
    println!("Phase 5: Demonstrating manual sync...");
    {
        let mut store = Store::open(temp_dir.path())?;

        println!("   Calling store.sync() explicitly...");
        store.sync()?;
        println!("   Sync complete.");

        println!("   Rebuilding indexes...");
        let count = store.rebuild_indexes::<Document>()?;
        println!("   Rebuilt indexes for {} documents", count);

        let all: Vec<Document> = store.list(&[])?;
        println!("   Total documents: {}", all.len());
    }
    println!();

    println!("Example complete!");
    println!("\nKey takeaways:");
    println!("  1. Store::open() auto-detects stale state and syncs");
    println!("  2. After sync, call rebuild_indexes::<T>() for each type");
    println!("  3. JSONL is source of truth - external changes are imported");
    println!("  4. Multiple versions of same ID in JSONL: latest wins");

    Ok(())
}
