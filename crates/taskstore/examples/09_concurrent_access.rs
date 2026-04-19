//! Example 09: Concurrent Access
//!
//! This example demonstrates TaskStore's file locking mechanism
//! that prevents concurrent write corruption.
//!
//! Run with: cargo run --example 09_concurrent_access

use eyre::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Barrier};
use std::thread;
use taskstore::{IndexValue, Record, Store, now_ms};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Counter {
    id: String,
    name: String,
    value: i64,
    updated_at: i64,
}

impl Record for Counter {
    fn id(&self) -> &str {
        &self.id
    }
    fn updated_at(&self) -> i64 {
        self.updated_at
    }
    fn collection_name() -> &'static str {
        "counters"
    }

    fn indexed_fields(&self) -> HashMap<String, IndexValue> {
        let mut fields = HashMap::new();
        fields.insert("value".to_string(), IndexValue::Int(self.value));
        fields
    }
}

fn main() -> Result<()> {
    let temp_dir = tempfile::tempdir()?;
    // Store::open auto-adds .taskstore subdir
    let base_path = temp_dir.path().to_path_buf();
    let store_path = temp_dir.path().join(".taskstore"); // For direct file access

    println!("TaskStore Concurrent Access Example");
    println!("====================================\n");

    // Create initial store and counter
    {
        let mut store = Store::open(&base_path)?;
        store.create(Counter {
            id: "main-counter".to_string(),
            name: "Main Counter".to_string(),
            value: 0,
            updated_at: now_ms(),
        })?;
        println!("Created initial counter with value = 0\n");
    }

    // Spawn multiple threads that each create records
    println!("1. Concurrent record creation (10 threads, 10 records each)...");

    let num_threads = 10;
    let records_per_thread = 10;
    let barrier = Arc::new(Barrier::new(num_threads));
    let base_path_arc = Arc::new(base_path.clone());

    let handles: Vec<_> = (0..num_threads)
        .map(|thread_id| {
            let barrier = Arc::clone(&barrier);
            let base_path = Arc::clone(&base_path_arc);

            thread::spawn(move || {
                // Wait for all threads to be ready
                barrier.wait();

                // Open store (each thread gets its own connection)
                let mut store = Store::open(base_path.as_ref()).unwrap();

                for i in 0..records_per_thread {
                    let counter = Counter {
                        id: format!("counter-{}-{}", thread_id, i),
                        name: format!("Thread {} Counter {}", thread_id, i),
                        value: (thread_id * 100 + i) as i64,
                        updated_at: now_ms(),
                    };
                    store.create(counter).unwrap();
                }

                thread_id
            })
        })
        .collect();

    // Wait for all threads
    for handle in handles {
        let thread_id = handle.join().unwrap();
        println!("   Thread {} completed", thread_id);
    }
    println!();

    // Verify all records were created
    println!("2. Verifying all records...");
    {
        let store = Store::open(&base_path)?;
        let all_counters: Vec<Counter> = store.list(&[])?;

        // Should have 1 initial + (10 threads * 10 records) = 101 records
        let expected = 1 + (num_threads * records_per_thread);
        println!("   Expected records: {}", expected);
        println!("   Actual records: {}", all_counters.len());

        if all_counters.len() == expected {
            println!("   All records created successfully.");
        } else {
            println!("   WARNING: Record count mismatch!");
        }
    }
    println!();

    // Verify JSONL file integrity
    println!("3. Verifying JSONL file integrity...");
    {
        let jsonl_path = store_path.join("counters.jsonl");
        let content = std::fs::read_to_string(&jsonl_path)?;
        let lines: Vec<&str> = content.lines().collect();

        println!("   JSONL lines: {}", lines.len());

        // Parse each line to verify it's valid JSON
        let mut valid_count = 0;
        let mut invalid_count = 0;
        for line in &lines {
            if serde_json::from_str::<serde_json::Value>(line).is_ok() {
                valid_count += 1;
            } else {
                invalid_count += 1;
                println!("   Invalid line: {}", line);
            }
        }

        println!("   Valid JSON lines: {}", valid_count);
        if invalid_count > 0 {
            println!("   Invalid JSON lines: {} (CORRUPTION DETECTED!)", invalid_count);
        } else {
            println!("   No corruption detected.");
        }
    }
    println!();

    // Demonstrate sequential updates (same record)
    println!("4. Sequential updates to same record...");
    {
        let base_path_clone = base_path.clone();
        let handles: Vec<_> = (0..5)
            .map(|i| {
                let path = base_path_clone.clone();
                thread::spawn(move || {
                    // Small delay to stagger threads
                    thread::sleep(std::time::Duration::from_millis(i * 10));

                    let mut store = Store::open(&path).unwrap();

                    // Read current value
                    let counter: Counter = store.get("main-counter").unwrap().unwrap();

                    // Update with incremented value
                    let updated = Counter {
                        id: counter.id,
                        name: counter.name,
                        value: counter.value + 1,
                        updated_at: now_ms(),
                    };
                    store.update(updated).unwrap();

                    i
                })
            })
            .collect();

        for handle in handles {
            let thread_id = handle.join().unwrap();
            println!("   Thread {} updated counter", thread_id);
        }

        // Check final value
        let store = Store::open(&base_path)?;
        let counter: Counter = store.get("main-counter")?.unwrap();
        println!("   Final counter value: {}", counter.value);
        println!("   (Note: Due to race conditions, may not be exactly 5)");
    }
    println!();

    println!("Example complete!");
    println!("\nKey points:");
    println!("  - File locking (fs2) prevents JSONL corruption during concurrent writes");
    println!("  - Each thread should open its own Store instance");
    println!("  - Read-modify-write cycles may still have race conditions");
    println!("  - For atomic increments, use transactions or application-level locking");

    Ok(())
}
