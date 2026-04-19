//! Example 01: Basic CRUD Operations
//!
//! This example demonstrates the fundamental create, read, update, and delete
//! operations with TaskStore.
//!
//! Run with: cargo run --example 01_basic_crud

use eyre::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use taskstore::{IndexValue, Record, Store, now_ms};

/// A simple note record for demonstration
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Note {
    id: String,
    title: String,
    content: String,
    created_at: i64,
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
        // No indexed fields for this simple example
        HashMap::new()
    }
}

fn main() -> Result<()> {
    // Create a temporary directory for this example
    let temp_dir = tempfile::tempdir()?;
    let store_path = temp_dir.path().to_path_buf();

    println!("TaskStore Basic CRUD Example");
    println!("============================\n");
    println!("Store path: {}\n", store_path.display());

    // Open (or create) the store
    let mut store = Store::open(&store_path)?;
    println!("Store opened successfully.\n");

    // CREATE: Add a new note
    println!("1. CREATE - Adding a new note...");
    let note = Note {
        id: "note-001".to_string(),
        title: "My First Note".to_string(),
        content: "This is the content of my first note.".to_string(),
        created_at: now_ms(),
        updated_at: now_ms(),
    };
    let id = store.create(note)?;
    println!("   Created note with ID: {}\n", id);

    // READ: Retrieve the note
    println!("2. READ - Retrieving the note...");
    let retrieved: Option<Note> = store.get("note-001")?;
    match &retrieved {
        Some(note) => {
            println!("   Found note:");
            println!("   - ID: {}", note.id);
            println!("   - Title: {}", note.title);
            println!("   - Content: {}", note.content);
        }
        None => println!("   Note not found!"),
    }
    println!();

    // UPDATE: Modify the note
    println!("3. UPDATE - Modifying the note...");
    if let Some(mut note) = retrieved {
        note.title = "My Updated Note".to_string();
        note.content = "This content has been updated.".to_string();
        note.updated_at = now_ms();
        store.update(note)?;
        println!("   Note updated successfully.\n");

        // Verify the update
        let updated: Option<Note> = store.get("note-001")?;
        if let Some(note) = updated {
            println!("   Verified update:");
            println!("   - New title: {}", note.title);
            println!("   - New content: {}", note.content);
        }
    }
    println!();

    // LIST: Show all notes
    println!("4. LIST - Showing all notes...");
    let all_notes: Vec<Note> = store.list(&[])?;
    println!("   Total notes: {}", all_notes.len());
    for note in &all_notes {
        println!("   - {} : {}", note.id, note.title);
    }
    println!();

    // DELETE: Remove the note
    println!("5. DELETE - Removing the note...");
    store.delete::<Note>("note-001")?;
    println!("   Note deleted.\n");

    // Verify deletion
    let deleted: Option<Note> = store.get("note-001")?;
    println!("   Verification: Note exists = {}\n", deleted.is_some());

    println!("Example complete!");
    Ok(())
}
