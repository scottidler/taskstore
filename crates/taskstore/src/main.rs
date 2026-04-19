use clap::{Parser, Subcommand};
use eyre::Result;
use rusqlite::params;
use std::path::PathBuf;
use taskstore::{Store, rusqlite};

#[derive(Parser)]
#[command(name = "taskstore")]
#[command(about = "TaskStore CLI - Generic persistent state management with SQLite+JSONL+Git")]
#[command(version = env!("GIT_DESCRIBE"))]
struct Cli {
    /// Path to the store directory (default: current directory)
    #[arg(short, long, default_value = ".")]
    store_path: PathBuf,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Sync SQLite database from JSONL files
    Sync,

    /// Install git hooks for automatic syncing
    InstallHooks,

    /// List all collections in the store
    Collections,

    /// List records in a collection
    List {
        /// Collection name (e.g., "loops", "loop_executions")
        collection: String,

        /// Filter by field=value (can be repeated)
        #[arg(short, long)]
        filter: Vec<String>,

        /// Limit number of results
        #[arg(short, long)]
        limit: Option<usize>,
    },

    /// Get a specific record by ID
    Get {
        /// Collection name
        collection: String,

        /// Record ID
        id: String,
    },

    /// Show indexes for a collection
    Indexes {
        /// Collection name
        collection: String,
    },

    /// Run raw SQL query (read-only)
    Sql {
        /// SQL query to execute
        query: String,
    },
}

fn main() -> Result<()> {
    // Setup tracing
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();

    // Open store
    let store = Store::open(&cli.store_path)?;

    match cli.command {
        Commands::Sync => {
            let mut store = store;
            println!("Syncing database from JSONL files...");
            store.sync()?;
            println!("Sync complete");
        }
        Commands::InstallHooks => {
            println!("Installing git hooks...");
            store.install_git_hooks()?;
            println!("Git hooks installed successfully");
        }
        Commands::Collections => {
            println!("Collections in store:");
            let db = store.db();
            let mut stmt = db.prepare(
                "SELECT DISTINCT collection, COUNT(*) as count FROM records GROUP BY collection ORDER BY collection",
            )?;
            let rows = stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))?;
            for row in rows {
                let (collection, count) = row?;
                println!("  {} ({} records)", collection, count);
            }
        }
        Commands::List {
            collection,
            filter,
            limit,
        } => {
            let db = store.db();
            let limit_clause = limit.map(|l| format!(" LIMIT {}", l)).unwrap_or_default();

            if filter.is_empty() {
                // No filters - list all
                let mut stmt = db.prepare(&format!(
                    "SELECT id, data_json FROM records WHERE collection = ?1 ORDER BY updated_at DESC{}",
                    limit_clause
                ))?;
                let rows = stmt.query_map(params![&collection], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?;

                for row in rows {
                    let (id, json) = row?;
                    // Pretty print JSON
                    let value: serde_json::Value = serde_json::from_str(&json)?;
                    println!("--- {} ---", id);
                    println!("{}", serde_json::to_string_pretty(&value)?);
                    println!();
                }
            } else {
                // With filters - join record_indexes
                let mut conditions = vec!["r.collection = ?1".to_string()];
                let mut bind_values: Vec<String> = vec![collection.clone()];

                for (i, f) in filter.iter().enumerate() {
                    let parts: Vec<&str> = f.splitn(2, '=').collect();
                    if parts.len() != 2 {
                        eprintln!("Invalid filter format: {} (expected field=value)", f);
                        continue;
                    }
                    let field = parts[0];
                    let value = parts[1];
                    let alias = format!("idx{}", i);

                    conditions.push(format!(
                        "EXISTS (SELECT 1 FROM record_indexes {} WHERE {}.collection = r.collection AND {}.id = r.id AND {}.field_name = ?{} AND {}.field_value_str = ?{})",
                        alias, alias, alias, alias, bind_values.len() + 1, alias, bind_values.len() + 2
                    ));
                    bind_values.push(field.to_string());
                    bind_values.push(value.to_string());
                }

                let query = format!(
                    "SELECT r.id, r.data_json FROM records r WHERE {} ORDER BY r.updated_at DESC{}",
                    conditions.join(" AND "),
                    limit_clause
                );

                let mut stmt = db.prepare(&query)?;
                let bind_refs: Vec<&dyn rusqlite::ToSql> =
                    bind_values.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
                let rows = stmt.query_map(bind_refs.as_slice(), |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?;

                for row in rows {
                    let (id, json) = row?;
                    let value: serde_json::Value = serde_json::from_str(&json)?;
                    println!("--- {} ---", id);
                    println!("{}", serde_json::to_string_pretty(&value)?);
                    println!();
                }
            }
        }
        Commands::Get { collection, id } => {
            let db = store.db();
            let mut stmt = db.prepare("SELECT data_json FROM records WHERE collection = ?1 AND id = ?2")?;
            let result: Option<String> = stmt.query_row(params![&collection, &id], |row| row.get(0)).ok();

            match result {
                Some(json) => {
                    let value: serde_json::Value = serde_json::from_str(&json)?;
                    println!("{}", serde_json::to_string_pretty(&value)?);
                }
                None => {
                    eprintln!("Record not found: {}:{}", collection, id);
                    std::process::exit(1);
                }
            }
        }
        Commands::Indexes { collection } => {
            let db = store.db();
            let mut stmt = db.prepare(
                "SELECT id, field_name, field_value_str, field_value_int, field_value_bool
                 FROM record_indexes WHERE collection = ?1 ORDER BY id, field_name",
            )?;
            let rows = stmt.query_map(params![&collection], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                ))
            })?;

            println!("Indexes for collection '{}':", collection);
            let mut current_id = String::new();
            for row in rows {
                let (id, field, str_val, int_val, bool_val) = row?;
                if id != current_id {
                    println!("\n  {}:", id);
                    current_id = id;
                }
                let value = str_val
                    .map(|s| format!("\"{}\"", s))
                    .or(int_val.map(|i| i.to_string()))
                    .or(bool_val.map(|b| (b != 0).to_string()))
                    .unwrap_or_else(|| "null".to_string());
                println!("    {} = {}", field, value);
            }
            println!();
        }
        Commands::Sql { query } => {
            let db = store.db();
            let mut stmt = db.prepare(&query)?;
            let column_count = stmt.column_count();
            let column_names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();

            // Print header
            println!("{}", column_names.join("\t"));
            println!("{}", "-".repeat(column_names.len() * 20));

            let rows = stmt.query_map([], |row| {
                let mut values = Vec::new();
                for i in 0..column_count {
                    let val: rusqlite::types::Value = row.get(i)?;
                    let s = match val {
                        rusqlite::types::Value::Null => "NULL".to_string(),
                        rusqlite::types::Value::Integer(i) => i.to_string(),
                        rusqlite::types::Value::Real(f) => f.to_string(),
                        rusqlite::types::Value::Text(s) => s,
                        rusqlite::types::Value::Blob(b) => format!("<blob {} bytes>", b.len()),
                    };
                    values.push(s);
                }
                Ok(values)
            })?;

            for row in rows {
                let values = row?;
                println!("{}", values.join("\t"));
            }
        }
    }

    Ok(())
}
