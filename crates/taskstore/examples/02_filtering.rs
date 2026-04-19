//! Example 02: Filtering and Querying
//!
//! This example demonstrates how to use filters to query records
//! based on indexed fields.
//!
//! Run with: cargo run --example 02_filtering

use eyre::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use taskstore::{Filter, FilterOp, IndexValue, Record, Store, now_ms};

/// A task with multiple indexed fields for filtering
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Task {
    id: String,
    title: String,
    status: String, // "pending", "in_progress", "complete"
    priority: i64,  // 1-10
    assigned: bool,
    created_at: i64,
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
        fields.insert("assigned".to_string(), IndexValue::Bool(self.assigned));
        fields
    }
}

fn main() -> Result<()> {
    let temp_dir = tempfile::tempdir()?;
    let store_path = temp_dir.path().to_path_buf();

    println!("TaskStore Filtering Example");
    println!("===========================\n");

    let mut store = Store::open(&store_path)?;

    // Create sample tasks
    println!("Creating sample tasks...\n");
    let tasks = vec![
        Task {
            id: "task-001".to_string(),
            title: "Write documentation".to_string(),
            status: "pending".to_string(),
            priority: 5,
            assigned: true,
            created_at: now_ms(),
            updated_at: now_ms(),
        },
        Task {
            id: "task-002".to_string(),
            title: "Fix critical bug".to_string(),
            status: "in_progress".to_string(),
            priority: 10,
            assigned: true,
            created_at: now_ms(),
            updated_at: now_ms(),
        },
        Task {
            id: "task-003".to_string(),
            title: "Code review".to_string(),
            status: "pending".to_string(),
            priority: 7,
            assigned: false,
            created_at: now_ms(),
            updated_at: now_ms(),
        },
        Task {
            id: "task-004".to_string(),
            title: "Update tests".to_string(),
            status: "complete".to_string(),
            priority: 3,
            assigned: true,
            created_at: now_ms(),
            updated_at: now_ms(),
        },
        Task {
            id: "task-005".to_string(),
            title: "Deploy to staging".to_string(),
            status: "pending".to_string(),
            priority: 8,
            assigned: false,
            created_at: now_ms(),
            updated_at: now_ms(),
        },
    ];

    for task in &tasks {
        store.create(task.clone())?;
        println!(
            "  Created: {} - {} (status={}, priority={}, assigned={})",
            task.id, task.title, task.status, task.priority, task.assigned
        );
    }
    println!();

    // Filter 1: By status (string equality)
    println!("1. Filter by status = 'pending':");
    let pending: Vec<Task> = store.list(&[Filter {
        field: "status".to_string(),
        op: FilterOp::Eq,
        value: IndexValue::String("pending".to_string()),
    }])?;
    for task in &pending {
        println!("   - {} : {}", task.id, task.title);
    }
    println!("   Found: {} tasks\n", pending.len());

    // Filter 2: By priority (integer comparison)
    println!("2. Filter by priority >= 7:");
    let high_priority: Vec<Task> = store.list(&[Filter {
        field: "priority".to_string(),
        op: FilterOp::Gte,
        value: IndexValue::Int(7),
    }])?;
    for task in &high_priority {
        println!("   - {} : {} (priority={})", task.id, task.title, task.priority);
    }
    println!("   Found: {} tasks\n", high_priority.len());

    // Filter 3: By boolean (assigned)
    println!("3. Filter by assigned = false (unassigned tasks):");
    let unassigned: Vec<Task> = store.list(&[Filter {
        field: "assigned".to_string(),
        op: FilterOp::Eq,
        value: IndexValue::Bool(false),
    }])?;
    for task in &unassigned {
        println!("   - {} : {}", task.id, task.title);
    }
    println!("   Found: {} tasks\n", unassigned.len());

    // Filter 4: Multiple filters (AND logic)
    println!("4. Filter by status = 'pending' AND priority > 5:");
    let urgent_pending: Vec<Task> = store.list(&[
        Filter {
            field: "status".to_string(),
            op: FilterOp::Eq,
            value: IndexValue::String("pending".to_string()),
        },
        Filter {
            field: "priority".to_string(),
            op: FilterOp::Gt,
            value: IndexValue::Int(5),
        },
    ])?;
    for task in &urgent_pending {
        println!("   - {} : {} (priority={})", task.id, task.title, task.priority);
    }
    println!("   Found: {} tasks\n", urgent_pending.len());

    // Filter 5: Not equal
    println!("5. Filter by status != 'complete' (active tasks):");
    let active: Vec<Task> = store.list(&[Filter {
        field: "status".to_string(),
        op: FilterOp::Ne,
        value: IndexValue::String("complete".to_string()),
    }])?;
    for task in &active {
        println!("   - {} : {} (status={})", task.id, task.title, task.status);
    }
    println!("   Found: {} tasks\n", active.len());

    println!("Example complete!");
    Ok(())
}
