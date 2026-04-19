//! Example 03: Custom Record Types
//!
//! This example shows how to define various custom record types with
//! different field types, enums, and optional fields.
//!
//! Run with: cargo run --example 03_custom_records

use eyre::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use taskstore::{IndexValue, Record, Store, now_ms};

// ============================================================================
// Example 1: Record with enum status
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum ProjectStatus {
    Planning,
    Active,
    OnHold,
    Complete,
    Cancelled,
}

impl ProjectStatus {
    fn as_str(&self) -> &'static str {
        match self {
            ProjectStatus::Planning => "planning",
            ProjectStatus::Active => "active",
            ProjectStatus::OnHold => "on_hold",
            ProjectStatus::Complete => "complete",
            ProjectStatus::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Project {
    id: String,
    name: String,
    status: ProjectStatus,
    budget: i64,
    created_at: i64,
    updated_at: i64,
}

impl Record for Project {
    fn id(&self) -> &str {
        &self.id
    }

    fn updated_at(&self) -> i64 {
        self.updated_at
    }

    fn collection_name() -> &'static str {
        "projects"
    }

    fn indexed_fields(&self) -> HashMap<String, IndexValue> {
        let mut fields = HashMap::new();
        fields.insert(
            "status".to_string(),
            IndexValue::String(self.status.as_str().to_string()),
        );
        fields.insert("budget".to_string(), IndexValue::Int(self.budget));
        fields
    }
}

// ============================================================================
// Example 2: Record with optional fields
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Employee {
    id: String,
    name: String,
    email: String,
    department: Option<String>,
    manager_id: Option<String>,
    active: bool,
    created_at: i64,
    updated_at: i64,
}

impl Record for Employee {
    fn id(&self) -> &str {
        &self.id
    }

    fn updated_at(&self) -> i64 {
        self.updated_at
    }

    fn collection_name() -> &'static str {
        "employees"
    }

    fn indexed_fields(&self) -> HashMap<String, IndexValue> {
        let mut fields = HashMap::new();
        fields.insert("active".to_string(), IndexValue::Bool(self.active));
        if let Some(dept) = &self.department {
            fields.insert("department".to_string(), IndexValue::String(dept.clone()));
        }
        fields
    }
}

// ============================================================================
// Example 3: Record with nested data (stored as JSON)
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Address {
    street: String,
    city: String,
    country: String,
    postal_code: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Customer {
    id: String,
    name: String,
    email: String,
    address: Address,  // Nested struct
    tags: Vec<String>, // Array field
    order_count: i64,
    created_at: i64,
    updated_at: i64,
}

impl Record for Customer {
    fn id(&self) -> &str {
        &self.id
    }

    fn updated_at(&self) -> i64 {
        self.updated_at
    }

    fn collection_name() -> &'static str {
        "customers"
    }

    fn indexed_fields(&self) -> HashMap<String, IndexValue> {
        let mut fields = HashMap::new();
        // Index the city from nested address
        fields.insert("city".to_string(), IndexValue::String(self.address.city.clone()));
        fields.insert("country".to_string(), IndexValue::String(self.address.country.clone()));
        fields.insert("order_count".to_string(), IndexValue::Int(self.order_count));
        fields
    }
}

fn main() -> Result<()> {
    let temp_dir = tempfile::tempdir()?;
    let store_path = temp_dir.path().to_path_buf();

    println!("TaskStore Custom Records Example");
    println!("=================================\n");

    let mut store = Store::open(&store_path)?;

    // Create projects with enum status
    println!("1. Creating projects with enum status...");
    let projects = vec![
        Project {
            id: "proj-001".to_string(),
            name: "Website Redesign".to_string(),
            status: ProjectStatus::Active,
            budget: 50000,
            created_at: now_ms(),
            updated_at: now_ms(),
        },
        Project {
            id: "proj-002".to_string(),
            name: "Mobile App".to_string(),
            status: ProjectStatus::Planning,
            budget: 100000,
            created_at: now_ms(),
            updated_at: now_ms(),
        },
    ];
    for proj in &projects {
        store.create(proj.clone())?;
        println!("   Created: {} - {} ({:?})", proj.id, proj.name, proj.status);
    }
    println!();

    // Create employees with optional fields
    println!("2. Creating employees with optional fields...");
    let employees = vec![
        Employee {
            id: "emp-001".to_string(),
            name: "Alice Johnson".to_string(),
            email: "alice@example.com".to_string(),
            department: Some("Engineering".to_string()),
            manager_id: None, // No manager (top-level)
            active: true,
            created_at: now_ms(),
            updated_at: now_ms(),
        },
        Employee {
            id: "emp-002".to_string(),
            name: "Bob Smith".to_string(),
            email: "bob@example.com".to_string(),
            department: Some("Engineering".to_string()),
            manager_id: Some("emp-001".to_string()), // Reports to Alice
            active: true,
            created_at: now_ms(),
            updated_at: now_ms(),
        },
        Employee {
            id: "emp-003".to_string(),
            name: "Carol Davis".to_string(),
            email: "carol@example.com".to_string(),
            department: None, // No department assigned yet
            manager_id: None,
            active: false, // Inactive employee
            created_at: now_ms(),
            updated_at: now_ms(),
        },
    ];
    for emp in &employees {
        store.create(emp.clone())?;
        println!(
            "   Created: {} - {} (dept={:?}, active={})",
            emp.id, emp.name, emp.department, emp.active
        );
    }
    println!();

    // Create customers with nested data
    println!("3. Creating customers with nested address...");
    let customers = vec![
        Customer {
            id: "cust-001".to_string(),
            name: "Acme Corp".to_string(),
            email: "contact@acme.com".to_string(),
            address: Address {
                street: "123 Main St".to_string(),
                city: "New York".to_string(),
                country: "USA".to_string(),
                postal_code: "10001".to_string(),
            },
            tags: vec!["enterprise".to_string(), "priority".to_string()],
            order_count: 42,
            created_at: now_ms(),
            updated_at: now_ms(),
        },
        Customer {
            id: "cust-002".to_string(),
            name: "Global Inc".to_string(),
            email: "info@global.com".to_string(),
            address: Address {
                street: "456 High St".to_string(),
                city: "London".to_string(),
                country: "UK".to_string(),
                postal_code: "EC1A 1BB".to_string(),
            },
            tags: vec!["international".to_string()],
            order_count: 15,
            created_at: now_ms(),
            updated_at: now_ms(),
        },
    ];
    for cust in &customers {
        store.create(cust.clone())?;
        println!(
            "   Created: {} - {} ({}, {})",
            cust.id, cust.name, cust.address.city, cust.address.country
        );
    }
    println!();

    // Query by enum status
    println!("4. Query projects by status = 'active':");
    let active_projects: Vec<Project> = store.list(&[taskstore::Filter {
        field: "status".to_string(),
        op: taskstore::FilterOp::Eq,
        value: IndexValue::String("active".to_string()),
    }])?;
    for proj in &active_projects {
        println!("   - {} : {}", proj.id, proj.name);
    }
    println!();

    // Query by optional field
    println!("5. Query employees by department = 'Engineering':");
    let engineers: Vec<Employee> = store.list(&[taskstore::Filter {
        field: "department".to_string(),
        op: taskstore::FilterOp::Eq,
        value: IndexValue::String("Engineering".to_string()),
    }])?;
    for emp in &engineers {
        println!("   - {} : {}", emp.id, emp.name);
    }
    println!();

    // Query by nested field
    println!("6. Query customers by city = 'New York':");
    let ny_customers: Vec<Customer> = store.list(&[taskstore::Filter {
        field: "city".to_string(),
        op: taskstore::FilterOp::Eq,
        value: IndexValue::String("New York".to_string()),
    }])?;
    for cust in &ny_customers {
        println!("   - {} : {} (tags: {:?})", cust.id, cust.name, cust.tags);
    }
    println!();

    println!("Example complete!");
    Ok(())
}
