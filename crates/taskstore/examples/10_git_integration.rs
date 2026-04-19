//! Example 10: Git Integration
//!
//! This example demonstrates TaskStore's git integration features:
//! - Installing git hooks
//! - Merge driver configuration
//! - JSONL file format for git
//!
//! Run with: cargo run --example 10_git_integration
//!
//! Note: This example works best when run in an actual git repository.

use eyre::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::process::Command;
use taskstore::{IndexValue, Record, Store, now_ms};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Config {
    id: String,
    key: String,
    value: String,
    updated_at: i64,
}

impl Record for Config {
    fn id(&self) -> &str {
        &self.id
    }
    fn updated_at(&self) -> i64 {
        self.updated_at
    }
    fn collection_name() -> &'static str {
        "config"
    }

    fn indexed_fields(&self) -> HashMap<String, IndexValue> {
        let mut fields = HashMap::new();
        fields.insert("key".to_string(), IndexValue::String(self.key.clone()));
        fields
    }
}

fn main() -> Result<()> {
    println!("TaskStore Git Integration Example");
    println!("==================================\n");

    // Create a temporary directory and init git repo
    let temp_dir = tempfile::tempdir()?;
    let repo_path = temp_dir.path();

    println!("1. Setting up git repository...");
    Command::new("git").args(["init"]).current_dir(repo_path).output()?;
    println!("   Initialized git repo at: {}\n", repo_path.display());

    // Create taskstore in the repo (Store::open auto-adds .taskstore subdir)
    let mut store = Store::open(repo_path)?;
    let store_path = repo_path.join(".taskstore");

    // Create some config records
    println!("2. Creating configuration records...");
    let configs = vec![
        Config {
            id: "cfg-001".to_string(),
            key: "app.name".to_string(),
            value: "MyApp".to_string(),
            updated_at: now_ms(),
        },
        Config {
            id: "cfg-002".to_string(),
            key: "app.version".to_string(),
            value: "1.0.0".to_string(),
            updated_at: now_ms(),
        },
        Config {
            id: "cfg-003".to_string(),
            key: "app.debug".to_string(),
            value: "false".to_string(),
            updated_at: now_ms(),
        },
    ];

    for cfg in configs {
        store.create(cfg.clone())?;
        println!("   {} = {}", cfg.key, cfg.value);
    }
    println!();

    // Install git hooks
    println!("3. Installing git hooks...");
    match store.install_git_hooks() {
        Ok(_) => {
            println!("   Git hooks installed successfully.");

            // Show installed hooks
            let hooks_dir = repo_path.join(".git/hooks");
            println!("\n   Installed hooks:");
            for hook in &["pre-commit", "post-merge", "post-rebase", "pre-push", "post-checkout"] {
                let hook_path = hooks_dir.join(hook);
                if hook_path.exists() {
                    println!("   - {}", hook);
                }
            }
        }
        Err(e) => {
            println!("   Failed to install hooks: {}", e);
        }
    }
    println!();

    // Show .gitattributes
    println!("4. Checking .gitattributes...");
    let gitattributes_path = repo_path.join(".gitattributes");
    if gitattributes_path.exists() {
        let content = std::fs::read_to_string(&gitattributes_path)?;
        println!("   Contents:");
        for line in content.lines() {
            println!("   {}", line);
        }
    } else {
        println!("   .gitattributes not found");
    }
    println!();

    // Show git config for merge driver
    println!("5. Checking git merge driver config...");
    let output = Command::new("git")
        .args(["config", "--local", "--get-regexp", "merge.taskstore"])
        .current_dir(repo_path)
        .output()?;

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        println!("   Merge driver configuration:");
        for line in stdout.lines() {
            println!("   {}", line);
        }
    } else {
        println!("   Merge driver not configured");
    }
    println!();

    // Show JSONL file format
    println!("6. JSONL file format (git-friendly):");
    let jsonl_path = store_path.join("config.jsonl");
    if jsonl_path.exists() {
        let content = std::fs::read_to_string(&jsonl_path)?;
        println!("   File: config.jsonl");
        println!("   ---------------------");
        for line in content.lines() {
            // Pretty print JSON for readability
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(line) {
                println!("   {}", serde_json::to_string(&value)?);
            }
        }
    }
    println!();

    // Show .gitignore
    println!("7. Checking .taskstore/.gitignore...");
    let gitignore_path = store_path.join(".gitignore");
    if gitignore_path.exists() {
        let content = std::fs::read_to_string(&gitignore_path)?;
        println!("   Contents (files excluded from git):");
        for line in content.lines() {
            println!("   - {}", line);
        }
    }
    println!();

    // Simulate what would be committed
    println!("8. Files that would be committed:");
    let output = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(repo_path)
        .output()?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        println!("   {}", line);
    }
    println!();

    // Demonstrate the merge scenario
    println!("9. Git workflow with TaskStore:");
    println!("   --------------------------------");
    println!("   a) Developer A creates records (appends to JSONL)");
    println!("   b) Developer B creates different records (appends to JSONL)");
    println!("   c) Git merge occurs:");
    println!("      - taskstore-merge driver activates");
    println!("      - Three-way merge by record ID");
    println!("      - Conflicts resolved by updated_at timestamp");
    println!("   d) post-merge hook runs 'taskstore sync'");
    println!("   e) SQLite rebuilt from merged JSONL");
    println!();

    println!("Example complete!");
    println!("\nKey files:");
    println!("  .taskstore/*.jsonl     - Committed to git (source of truth)");
    println!("  .taskstore/taskstore.db - Ignored (derived cache)");
    println!("  .gitattributes         - Configures merge driver");
    println!("  .git/hooks/*           - Auto-sync hooks");

    Ok(())
}
