//! Example 06: Record Relationships
//!
//! This example demonstrates how to model relationships between records:
//! - One-to-many (parent-child)
//! - Many-to-many (via join records)
//! - Self-referential (tree structures)
//!
//! Run with: cargo run --example 06_relationships

use eyre::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use taskstore::{Filter, FilterOp, IndexValue, Record, Store, now_ms};

// ============================================================================
// One-to-Many: Team has many Members
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Team {
    id: String,
    name: String,
    department: String,
    created_at: i64,
    updated_at: i64,
}

impl Record for Team {
    fn id(&self) -> &str {
        &self.id
    }
    fn updated_at(&self) -> i64 {
        self.updated_at
    }
    fn collection_name() -> &'static str {
        "teams"
    }

    fn indexed_fields(&self) -> HashMap<String, IndexValue> {
        let mut fields = HashMap::new();
        fields.insert("department".to_string(), IndexValue::String(self.department.clone()));
        fields
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Member {
    id: String,
    name: String,
    team_id: String, // Foreign key to Team
    role: String,
    created_at: i64,
    updated_at: i64,
}

impl Record for Member {
    fn id(&self) -> &str {
        &self.id
    }
    fn updated_at(&self) -> i64 {
        self.updated_at
    }
    fn collection_name() -> &'static str {
        "members"
    }

    fn indexed_fields(&self) -> HashMap<String, IndexValue> {
        let mut fields = HashMap::new();
        fields.insert("team_id".to_string(), IndexValue::String(self.team_id.clone()));
        fields.insert("role".to_string(), IndexValue::String(self.role.clone()));
        fields
    }
}

// ============================================================================
// Many-to-Many: Articles and Tags (via ArticleTag join)
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Article {
    id: String,
    title: String,
    content: String,
    created_at: i64,
    updated_at: i64,
}

impl Record for Article {
    fn id(&self) -> &str {
        &self.id
    }
    fn updated_at(&self) -> i64 {
        self.updated_at
    }
    fn collection_name() -> &'static str {
        "articles"
    }

    fn indexed_fields(&self) -> HashMap<String, IndexValue> {
        HashMap::new()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Tag {
    id: String,
    name: String,
    created_at: i64,
    updated_at: i64,
}

impl Record for Tag {
    fn id(&self) -> &str {
        &self.id
    }
    fn updated_at(&self) -> i64 {
        self.updated_at
    }
    fn collection_name() -> &'static str {
        "tags"
    }

    fn indexed_fields(&self) -> HashMap<String, IndexValue> {
        let mut fields = HashMap::new();
        fields.insert("name".to_string(), IndexValue::String(self.name.clone()));
        fields
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ArticleTag {
    id: String,
    article_id: String,
    tag_id: String,
    created_at: i64,
    updated_at: i64,
}

impl Record for ArticleTag {
    fn id(&self) -> &str {
        &self.id
    }
    fn updated_at(&self) -> i64 {
        self.updated_at
    }
    fn collection_name() -> &'static str {
        "article_tags"
    }

    fn indexed_fields(&self) -> HashMap<String, IndexValue> {
        let mut fields = HashMap::new();
        fields.insert("article_id".to_string(), IndexValue::String(self.article_id.clone()));
        fields.insert("tag_id".to_string(), IndexValue::String(self.tag_id.clone()));
        fields
    }
}

// ============================================================================
// Self-Referential: Categories with parent_id (tree structure)
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Category {
    id: String,
    name: String,
    parent_id: Option<String>, // None = root category
    depth: i64,
    created_at: i64,
    updated_at: i64,
}

impl Record for Category {
    fn id(&self) -> &str {
        &self.id
    }
    fn updated_at(&self) -> i64 {
        self.updated_at
    }
    fn collection_name() -> &'static str {
        "categories"
    }

    fn indexed_fields(&self) -> HashMap<String, IndexValue> {
        let mut fields = HashMap::new();
        if let Some(parent) = &self.parent_id {
            fields.insert("parent_id".to_string(), IndexValue::String(parent.clone()));
        }
        fields.insert("depth".to_string(), IndexValue::Int(self.depth));
        fields
    }
}

// ============================================================================
// Helper functions for relationship queries
// ============================================================================

fn get_team_members(store: &Store, team_id: &str) -> Result<Vec<Member>> {
    store.list(&[Filter {
        field: "team_id".to_string(),
        op: FilterOp::Eq,
        value: IndexValue::String(team_id.to_string()),
    }])
}

fn get_article_tags(store: &Store, article_id: &str) -> Result<Vec<String>> {
    let joins: Vec<ArticleTag> = store.list(&[Filter {
        field: "article_id".to_string(),
        op: FilterOp::Eq,
        value: IndexValue::String(article_id.to_string()),
    }])?;

    Ok(joins.into_iter().map(|j| j.tag_id).collect())
}

fn get_child_categories(store: &Store, parent_id: &str) -> Result<Vec<Category>> {
    store.list(&[Filter {
        field: "parent_id".to_string(),
        op: FilterOp::Eq,
        value: IndexValue::String(parent_id.to_string()),
    }])
}

fn main() -> Result<()> {
    let temp_dir = tempfile::tempdir()?;
    let store_path = temp_dir.path().to_path_buf();

    println!("TaskStore Relationships Example");
    println!("================================\n");

    let mut store = Store::open(&store_path)?;

    // ========================================================================
    // One-to-Many: Teams and Members
    // ========================================================================
    println!("1. One-to-Many: Teams and Members");
    println!("----------------------------------");

    // Create teams
    let engineering = Team {
        id: "team-eng".to_string(),
        name: "Engineering".to_string(),
        department: "Product".to_string(),
        created_at: now_ms(),
        updated_at: now_ms(),
    };
    let marketing = Team {
        id: "team-mkt".to_string(),
        name: "Marketing".to_string(),
        department: "Growth".to_string(),
        created_at: now_ms(),
        updated_at: now_ms(),
    };
    store.create(engineering)?;
    store.create(marketing)?;

    // Create members (belong to teams)
    let members = vec![
        Member {
            id: "mem-001".to_string(),
            name: "Alice".to_string(),
            team_id: "team-eng".to_string(),
            role: "lead".to_string(),
            created_at: now_ms(),
            updated_at: now_ms(),
        },
        Member {
            id: "mem-002".to_string(),
            name: "Bob".to_string(),
            team_id: "team-eng".to_string(),
            role: "developer".to_string(),
            created_at: now_ms(),
            updated_at: now_ms(),
        },
        Member {
            id: "mem-003".to_string(),
            name: "Carol".to_string(),
            team_id: "team-mkt".to_string(),
            role: "manager".to_string(),
            created_at: now_ms(),
            updated_at: now_ms(),
        },
    ];
    for m in members {
        store.create(m)?;
    }

    // Query: Get members of Engineering team
    println!("   Engineering team members:");
    let eng_members = get_team_members(&store, "team-eng")?;
    for m in &eng_members {
        println!("   - {} ({})", m.name, m.role);
    }
    println!();

    // ========================================================================
    // Many-to-Many: Articles and Tags
    // ========================================================================
    println!("2. Many-to-Many: Articles and Tags");
    println!("-----------------------------------");

    // Create tags
    let tags = vec![
        Tag {
            id: "tag-rust".to_string(),
            name: "rust".to_string(),
            created_at: now_ms(),
            updated_at: now_ms(),
        },
        Tag {
            id: "tag-database".to_string(),
            name: "database".to_string(),
            created_at: now_ms(),
            updated_at: now_ms(),
        },
        Tag {
            id: "tag-tutorial".to_string(),
            name: "tutorial".to_string(),
            created_at: now_ms(),
            updated_at: now_ms(),
        },
    ];
    for t in tags {
        store.create(t)?;
    }

    // Create articles
    let articles = vec![
        Article {
            id: "art-001".to_string(),
            title: "Getting Started with Rust".to_string(),
            content: "...".to_string(),
            created_at: now_ms(),
            updated_at: now_ms(),
        },
        Article {
            id: "art-002".to_string(),
            title: "Building a Database".to_string(),
            content: "...".to_string(),
            created_at: now_ms(),
            updated_at: now_ms(),
        },
    ];
    for a in articles {
        store.create(a)?;
    }

    // Create article-tag relationships
    let article_tags = vec![
        ArticleTag {
            id: "at-001".to_string(),
            article_id: "art-001".to_string(),
            tag_id: "tag-rust".to_string(),
            created_at: now_ms(),
            updated_at: now_ms(),
        },
        ArticleTag {
            id: "at-002".to_string(),
            article_id: "art-001".to_string(),
            tag_id: "tag-tutorial".to_string(),
            created_at: now_ms(),
            updated_at: now_ms(),
        },
        ArticleTag {
            id: "at-003".to_string(),
            article_id: "art-002".to_string(),
            tag_id: "tag-rust".to_string(),
            created_at: now_ms(),
            updated_at: now_ms(),
        },
        ArticleTag {
            id: "at-004".to_string(),
            article_id: "art-002".to_string(),
            tag_id: "tag-database".to_string(),
            created_at: now_ms(),
            updated_at: now_ms(),
        },
    ];
    for at in article_tags {
        store.create(at)?;
    }

    // Query: Get tags for article art-001
    println!("   Tags for 'Getting Started with Rust':");
    let art1_tags = get_article_tags(&store, "art-001")?;
    for tag_id in &art1_tags {
        if let Some(tag) = store.get::<Tag>(tag_id)? {
            println!("   - {}", tag.name);
        }
    }
    println!();

    // ========================================================================
    // Self-Referential: Category Tree
    // ========================================================================
    println!("3. Self-Referential: Category Tree");
    println!("-----------------------------------");

    // Create category tree
    let categories = vec![
        Category {
            id: "cat-electronics".to_string(),
            name: "Electronics".to_string(),
            parent_id: None, // Root
            depth: 0,
            created_at: now_ms(),
            updated_at: now_ms(),
        },
        Category {
            id: "cat-computers".to_string(),
            name: "Computers".to_string(),
            parent_id: Some("cat-electronics".to_string()),
            depth: 1,
            created_at: now_ms(),
            updated_at: now_ms(),
        },
        Category {
            id: "cat-laptops".to_string(),
            name: "Laptops".to_string(),
            parent_id: Some("cat-computers".to_string()),
            depth: 2,
            created_at: now_ms(),
            updated_at: now_ms(),
        },
        Category {
            id: "cat-phones".to_string(),
            name: "Phones".to_string(),
            parent_id: Some("cat-electronics".to_string()),
            depth: 1,
            created_at: now_ms(),
            updated_at: now_ms(),
        },
    ];
    for c in categories {
        store.create(c)?;
    }

    // Print tree structure
    fn print_tree(store: &Store, parent_id: Option<&str>, indent: usize) -> Result<()> {
        let categories: Vec<Category> = if let Some(pid) = parent_id {
            get_child_categories(store, pid)?
        } else {
            // Get root categories (depth = 0)
            store.list(&[Filter {
                field: "depth".to_string(),
                op: FilterOp::Eq,
                value: IndexValue::Int(0),
            }])?
        };

        for cat in categories {
            println!("   {}{}", "  ".repeat(indent), cat.name);
            print_tree(store, Some(&cat.id), indent + 1)?;
        }
        Ok(())
    }

    println!("   Category tree:");
    print_tree(&store, None, 0)?;
    println!();

    // Query: Get all depth-1 categories
    println!("   Direct children of root:");
    let depth1: Vec<Category> = store.list(&[Filter {
        field: "depth".to_string(),
        op: FilterOp::Eq,
        value: IndexValue::Int(1),
    }])?;
    for cat in &depth1 {
        println!("   - {}", cat.name);
    }
    println!();

    println!("Example complete!");
    Ok(())
}
