//! Example 07: Status Workflow
//!
//! This example demonstrates how to model status-based workflows
//! with validation, transitions, and querying by state.
//!
//! Run with: cargo run --example 07_status_workflow

use eyre::{Result, eyre};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use taskstore::{Filter, FilterOp, IndexValue, Record, Store, now_ms};

// ============================================================================
// Issue Tracker with Status Workflow
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum IssueStatus {
    Open,
    InProgress,
    InReview,
    Resolved,
    Closed,
    Wontfix,
}

impl IssueStatus {
    fn as_str(&self) -> &'static str {
        match self {
            IssueStatus::Open => "open",
            IssueStatus::InProgress => "in_progress",
            IssueStatus::InReview => "in_review",
            IssueStatus::Resolved => "resolved",
            IssueStatus::Closed => "closed",
            IssueStatus::Wontfix => "wontfix",
        }
    }

    /// Valid transitions from this status
    fn valid_transitions(&self) -> Vec<IssueStatus> {
        match self {
            IssueStatus::Open => vec![IssueStatus::InProgress, IssueStatus::Wontfix],
            IssueStatus::InProgress => vec![
                IssueStatus::InReview,
                IssueStatus::Open, // Back to backlog
            ],
            IssueStatus::InReview => vec![
                IssueStatus::InProgress, // Needs more work
                IssueStatus::Resolved,
            ],
            IssueStatus::Resolved => vec![
                IssueStatus::Closed,
                IssueStatus::Open, // Reopened
            ],
            IssueStatus::Closed => vec![
                IssueStatus::Open, // Reopened
            ],
            IssueStatus::Wontfix => vec![
                IssueStatus::Open, // Reconsidered
            ],
        }
    }

    fn can_transition_to(&self, target: IssueStatus) -> bool {
        self.valid_transitions().contains(&target)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Priority {
    Low,
    Medium,
    High,
    Critical,
}

impl Priority {
    fn as_str(&self) -> &'static str {
        match self {
            Priority::Low => "low",
            Priority::Medium => "medium",
            Priority::High => "high",
            Priority::Critical => "critical",
        }
    }

    fn to_int(self) -> i64 {
        match self {
            Priority::Low => 1,
            Priority::Medium => 2,
            Priority::High => 3,
            Priority::Critical => 4,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Issue {
    id: String,
    title: String,
    description: String,
    status: IssueStatus,
    priority: Priority,
    assignee: Option<String>,
    created_at: i64,
    updated_at: i64,
}

impl Record for Issue {
    fn id(&self) -> &str {
        &self.id
    }
    fn updated_at(&self) -> i64 {
        self.updated_at
    }
    fn collection_name() -> &'static str {
        "issues"
    }

    fn indexed_fields(&self) -> HashMap<String, IndexValue> {
        let mut fields = HashMap::new();
        fields.insert(
            "status".to_string(),
            IndexValue::String(self.status.as_str().to_string()),
        );
        fields.insert(
            "priority".to_string(),
            IndexValue::String(self.priority.as_str().to_string()),
        );
        fields.insert("priority_int".to_string(), IndexValue::Int(self.priority.to_int()));
        if let Some(assignee) = &self.assignee {
            fields.insert("assignee".to_string(), IndexValue::String(assignee.clone()));
        }
        fields
    }
}

impl Issue {
    /// Transition to a new status with validation
    fn transition(&mut self, new_status: IssueStatus) -> Result<()> {
        if !self.status.can_transition_to(new_status) {
            return Err(eyre!(
                "Invalid transition: {:?} -> {:?}. Valid targets: {:?}",
                self.status,
                new_status,
                self.status.valid_transitions()
            ));
        }
        self.status = new_status;
        self.updated_at = now_ms();
        Ok(())
    }
}

// ============================================================================
// Workflow Operations
// ============================================================================

fn start_work(store: &mut Store, issue_id: &str, assignee: &str) -> Result<()> {
    let mut issue: Issue = store
        .get(issue_id)?
        .ok_or_else(|| eyre!("Issue not found: {}", issue_id))?;

    issue.transition(IssueStatus::InProgress)?;
    issue.assignee = Some(assignee.to_string());
    store.update(issue)?;
    Ok(())
}

fn submit_for_review(store: &mut Store, issue_id: &str) -> Result<()> {
    let mut issue: Issue = store
        .get(issue_id)?
        .ok_or_else(|| eyre!("Issue not found: {}", issue_id))?;

    issue.transition(IssueStatus::InReview)?;
    store.update(issue)?;
    Ok(())
}

fn approve_review(store: &mut Store, issue_id: &str) -> Result<()> {
    let mut issue: Issue = store
        .get(issue_id)?
        .ok_or_else(|| eyre!("Issue not found: {}", issue_id))?;

    issue.transition(IssueStatus::Resolved)?;
    store.update(issue)?;
    Ok(())
}

fn close_issue(store: &mut Store, issue_id: &str) -> Result<()> {
    let mut issue: Issue = store
        .get(issue_id)?
        .ok_or_else(|| eyre!("Issue not found: {}", issue_id))?;

    issue.transition(IssueStatus::Closed)?;
    store.update(issue)?;
    Ok(())
}

fn main() -> Result<()> {
    let temp_dir = tempfile::tempdir()?;
    let store_path = temp_dir.path().to_path_buf();

    println!("TaskStore Status Workflow Example");
    println!("==================================\n");

    let mut store = Store::open(&store_path)?;

    // Create some issues
    println!("1. Creating issues...");
    let issues = vec![
        Issue {
            id: "ISS-001".to_string(),
            title: "Fix login bug".to_string(),
            description: "Users can't log in on mobile".to_string(),
            status: IssueStatus::Open,
            priority: Priority::Critical,
            assignee: None,
            created_at: now_ms(),
            updated_at: now_ms(),
        },
        Issue {
            id: "ISS-002".to_string(),
            title: "Add dark mode".to_string(),
            description: "Implement dark mode theme".to_string(),
            status: IssueStatus::Open,
            priority: Priority::Medium,
            assignee: None,
            created_at: now_ms(),
            updated_at: now_ms(),
        },
        Issue {
            id: "ISS-003".to_string(),
            title: "Update documentation".to_string(),
            description: "Docs are out of date".to_string(),
            status: IssueStatus::Open,
            priority: Priority::Low,
            assignee: None,
            created_at: now_ms(),
            updated_at: now_ms(),
        },
    ];

    for issue in issues {
        store.create(issue.clone())?;
        println!("   Created: {} - {} ({:?})", issue.id, issue.title, issue.priority);
    }
    println!();

    // Workflow: Work on the critical issue
    println!("2. Workflow: Processing ISS-001 (critical bug)...");

    println!("   Starting work (Alice assigned)...");
    start_work(&mut store, "ISS-001", "alice")?;

    let issue: Issue = store.get("ISS-001")?.unwrap();
    println!("   Status: {:?}, Assignee: {:?}", issue.status, issue.assignee);

    println!("   Submitting for review...");
    submit_for_review(&mut store, "ISS-001")?;

    let issue: Issue = store.get("ISS-001")?.unwrap();
    println!("   Status: {:?}", issue.status);

    println!("   Approving review...");
    approve_review(&mut store, "ISS-001")?;

    let issue: Issue = store.get("ISS-001")?.unwrap();
    println!("   Status: {:?}", issue.status);

    println!("   Closing issue...");
    close_issue(&mut store, "ISS-001")?;

    let issue: Issue = store.get("ISS-001")?.unwrap();
    println!("   Status: {:?}", issue.status);
    println!();

    // Try invalid transition
    println!("3. Testing invalid transition...");
    let mut issue2: Issue = store.get("ISS-002")?.unwrap();
    match issue2.transition(IssueStatus::Closed) {
        Ok(_) => println!("   Transition succeeded (unexpected!)"),
        Err(e) => println!("   Transition failed (expected): {}", e),
    }
    println!();

    // Query by status
    println!("4. Query issues by status...");

    let open_issues: Vec<Issue> = store.list(&[Filter {
        field: "status".to_string(),
        op: FilterOp::Eq,
        value: IndexValue::String("open".to_string()),
    }])?;
    println!("   Open issues: {}", open_issues.len());
    for issue in &open_issues {
        println!("   - {} : {}", issue.id, issue.title);
    }

    let closed_issues: Vec<Issue> = store.list(&[Filter {
        field: "status".to_string(),
        op: FilterOp::Eq,
        value: IndexValue::String("closed".to_string()),
    }])?;
    println!("   Closed issues: {}", closed_issues.len());
    for issue in &closed_issues {
        println!("   - {} : {}", issue.id, issue.title);
    }
    println!();

    // Query by priority
    println!("5. Query high-priority issues...");
    let urgent: Vec<Issue> = store.list(&[Filter {
        field: "priority_int".to_string(),
        op: FilterOp::Gte,
        value: IndexValue::Int(3), // High or Critical
    }])?;
    println!("   High/Critical priority issues:");
    for issue in &urgent {
        println!("   - {} : {} ({:?})", issue.id, issue.title, issue.priority);
    }
    println!();

    println!("Example complete!");
    println!("\nWorkflow states:");
    println!("  Open -> InProgress -> InReview -> Resolved -> Closed");
    println!("         \\-> Wontfix");

    Ok(())
}
