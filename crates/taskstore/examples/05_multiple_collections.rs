//! Example 05: Multiple Collections
//!
//! This example demonstrates working with multiple record types
//! in the same store, each in their own collection (JSONL file).
//!
//! Run with: cargo run --example 05_multiple_collections

use eyre::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use taskstore::{Filter, FilterOp, IndexValue, Record, Store, now_ms};

// ============================================================================
// Collection 1: Users
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
struct User {
    id: String,
    username: String,
    email: String,
    role: String,
    active: bool,
    created_at: i64,
    updated_at: i64,
}

impl Record for User {
    fn id(&self) -> &str {
        &self.id
    }

    fn updated_at(&self) -> i64 {
        self.updated_at
    }

    fn collection_name() -> &'static str {
        "users"
    }

    fn indexed_fields(&self) -> HashMap<String, IndexValue> {
        let mut fields = HashMap::new();
        fields.insert("role".to_string(), IndexValue::String(self.role.clone()));
        fields.insert("active".to_string(), IndexValue::Bool(self.active));
        fields
    }
}

// ============================================================================
// Collection 2: Posts
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Post {
    id: String,
    author_id: String, // Foreign key to User
    title: String,
    content: String,
    published: bool,
    view_count: i64,
    created_at: i64,
    updated_at: i64,
}

impl Record for Post {
    fn id(&self) -> &str {
        &self.id
    }

    fn updated_at(&self) -> i64 {
        self.updated_at
    }

    fn collection_name() -> &'static str {
        "posts"
    }

    fn indexed_fields(&self) -> HashMap<String, IndexValue> {
        let mut fields = HashMap::new();
        fields.insert("author_id".to_string(), IndexValue::String(self.author_id.clone()));
        fields.insert("published".to_string(), IndexValue::Bool(self.published));
        fields.insert("view_count".to_string(), IndexValue::Int(self.view_count));
        fields
    }
}

// ============================================================================
// Collection 3: Comments
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Comment {
    id: String,
    post_id: String,   // Foreign key to Post
    author_id: String, // Foreign key to User
    content: String,
    created_at: i64,
    updated_at: i64,
}

impl Record for Comment {
    fn id(&self) -> &str {
        &self.id
    }

    fn updated_at(&self) -> i64 {
        self.updated_at
    }

    fn collection_name() -> &'static str {
        "comments"
    }

    fn indexed_fields(&self) -> HashMap<String, IndexValue> {
        let mut fields = HashMap::new();
        fields.insert("post_id".to_string(), IndexValue::String(self.post_id.clone()));
        fields.insert("author_id".to_string(), IndexValue::String(self.author_id.clone()));
        fields
    }
}

fn main() -> Result<()> {
    let temp_dir = tempfile::tempdir()?;
    let store_path = temp_dir.path().to_path_buf();

    println!("TaskStore Multiple Collections Example");
    println!("======================================\n");

    let mut store = Store::open(&store_path)?;

    // Create users
    println!("1. Creating users...");
    let users = vec![
        User {
            id: "user-001".to_string(),
            username: "alice".to_string(),
            email: "alice@example.com".to_string(),
            role: "admin".to_string(),
            active: true,
            created_at: now_ms(),
            updated_at: now_ms(),
        },
        User {
            id: "user-002".to_string(),
            username: "bob".to_string(),
            email: "bob@example.com".to_string(),
            role: "author".to_string(),
            active: true,
            created_at: now_ms(),
            updated_at: now_ms(),
        },
        User {
            id: "user-003".to_string(),
            username: "charlie".to_string(),
            email: "charlie@example.com".to_string(),
            role: "reader".to_string(),
            active: false,
            created_at: now_ms(),
            updated_at: now_ms(),
        },
    ];
    for user in &users {
        store.create(user.clone())?;
        println!("   Created user: {} ({})", user.username, user.role);
    }
    println!();

    // Create posts
    println!("2. Creating posts...");
    let posts = vec![
        Post {
            id: "post-001".to_string(),
            author_id: "user-001".to_string(),
            title: "Welcome to TaskStore".to_string(),
            content: "This is the first post...".to_string(),
            published: true,
            view_count: 150,
            created_at: now_ms(),
            updated_at: now_ms(),
        },
        Post {
            id: "post-002".to_string(),
            author_id: "user-002".to_string(),
            title: "Advanced Filtering".to_string(),
            content: "Learn about filtering...".to_string(),
            published: true,
            view_count: 75,
            created_at: now_ms(),
            updated_at: now_ms(),
        },
        Post {
            id: "post-003".to_string(),
            author_id: "user-001".to_string(),
            title: "Draft Post".to_string(),
            content: "Work in progress...".to_string(),
            published: false,
            view_count: 0,
            created_at: now_ms(),
            updated_at: now_ms(),
        },
    ];
    for post in &posts {
        store.create(post.clone())?;
        println!("   Created post: {} (by {})", post.title, post.author_id);
    }
    println!();

    // Create comments
    println!("3. Creating comments...");
    let comments = vec![
        Comment {
            id: "comment-001".to_string(),
            post_id: "post-001".to_string(),
            author_id: "user-002".to_string(),
            content: "Great introduction!".to_string(),
            created_at: now_ms(),
            updated_at: now_ms(),
        },
        Comment {
            id: "comment-002".to_string(),
            post_id: "post-001".to_string(),
            author_id: "user-003".to_string(),
            content: "Very helpful, thanks!".to_string(),
            created_at: now_ms(),
            updated_at: now_ms(),
        },
        Comment {
            id: "comment-003".to_string(),
            post_id: "post-002".to_string(),
            author_id: "user-001".to_string(),
            content: "Nice article Bob!".to_string(),
            created_at: now_ms(),
            updated_at: now_ms(),
        },
    ];
    for comment in &comments {
        store.create(comment.clone())?;
        println!("   Created comment on {} by {}", comment.post_id, comment.author_id);
    }
    println!();

    // Show collection counts
    println!("4. Collection counts:");
    let all_users: Vec<User> = store.list(&[])?;
    let all_posts: Vec<Post> = store.list(&[])?;
    let all_comments: Vec<Comment> = store.list(&[])?;
    println!("   Users: {}", all_users.len());
    println!("   Posts: {}", all_posts.len());
    println!("   Comments: {}", all_comments.len());
    println!();

    // Query across collections
    println!("5. Cross-collection queries:");

    // Get all posts by Alice
    println!("\n   Posts by user-001 (Alice):");
    let alice_posts: Vec<Post> = store.list(&[Filter {
        field: "author_id".to_string(),
        op: FilterOp::Eq,
        value: IndexValue::String("user-001".to_string()),
    }])?;
    for post in &alice_posts {
        println!("   - {} (views: {})", post.title, post.view_count);
    }

    // Get all comments on post-001
    println!("\n   Comments on post-001:");
    let post_comments: Vec<Comment> = store.list(&[Filter {
        field: "post_id".to_string(),
        op: FilterOp::Eq,
        value: IndexValue::String("post-001".to_string()),
    }])?;
    for comment in &post_comments {
        println!("   - {} says: \"{}\"", comment.author_id, comment.content);
    }

    // Get active admins
    println!("\n   Active admin users:");
    let admins: Vec<User> = store.list(&[
        Filter {
            field: "role".to_string(),
            op: FilterOp::Eq,
            value: IndexValue::String("admin".to_string()),
        },
        Filter {
            field: "active".to_string(),
            op: FilterOp::Eq,
            value: IndexValue::Bool(true),
        },
    ])?;
    for user in &admins {
        println!("   - {} ({})", user.username, user.email);
    }
    println!();

    // Show JSONL files created
    println!("6. JSONL files created:");
    for entry in std::fs::read_dir(&store_path)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "jsonl") {
            let metadata = std::fs::metadata(&path)?;
            println!(
                "   - {} ({} bytes)",
                path.file_name().unwrap().to_string_lossy(),
                metadata.len()
            );
        }
    }
    println!();

    println!("Example complete!");
    Ok(())
}
