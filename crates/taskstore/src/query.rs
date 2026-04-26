// Read-query helpers that operate on a `&rusqlite::Connection` with no `&mut self`
// and no generic record type. Both the sync `Store` and the async reader pool use
// these so the SQL lives in one place.

use crate::error::{Error, Result};
use rusqlite::{Connection, OptionalExtension};
use std::fs;
use std::path::Path;
use taskstore_traits::{Filter, FilterOp, IndexValue};

fn filter_op_to_sql(op: FilterOp) -> &'static str {
    match op {
        FilterOp::Eq => "=",
        FilterOp::Ne => "!=",
        FilterOp::Gt => ">",
        FilterOp::Lt => "<",
        FilterOp::Gte => ">=",
        FilterOp::Lte => "<=",
        FilterOp::Contains => "LIKE",
    }
}

/// Fetch the raw `data_json` for a record by collection + id, or `None` if absent.
pub fn get_data_json(conn: &Connection, collection: &str, id: &str) -> Result<Option<String>> {
    let mut stmt = conn.prepare("SELECT data_json FROM records WHERE collection = ?1 AND id = ?2")?;
    let result = stmt
        .query_row(rusqlite::params![collection, id], |row| row.get::<_, String>(0))
        .optional()?;
    Ok(result)
}

/// Fetch all matching `data_json` strings for a collection, optionally filtered
/// by indexed fields. Ordering matches the sync `Store::list` contract:
/// `ORDER BY r.updated_at DESC`.
///
/// Caller is responsible for validating `filter.field` names (both `Store::list`
/// and the async layer do this via `crate::store::Store::validate_field_name`
/// before invoking).
pub fn list_data_jsons(conn: &Connection, collection: &str, filters: &[Filter]) -> Result<Vec<String>> {
    if filters.is_empty() {
        let mut stmt = conn.prepare("SELECT data_json FROM records WHERE collection = ?1 ORDER BY updated_at DESC")?;
        let rows = stmt.query_map([collection], |row| row.get::<_, String>(0))?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }
        return Ok(results);
    }

    let mut query = String::from(
        "SELECT DISTINCT r.data_json
         FROM records r
         WHERE r.collection = ?1",
    );

    for (i, filter) in filters.iter().enumerate() {
        crate::store::Store::validate_field_name(&filter.field)?;

        let join_alias = format!("idx{i}");
        query.push_str(&format!(
            " AND EXISTS (
                SELECT 1 FROM record_indexes {alias}
                WHERE {alias}.collection = r.collection
                  AND {alias}.id = r.id
                  AND {alias}.field_name = ?{field_param}",
            alias = join_alias,
            field_param = i + 2
        ));

        match &filter.value {
            IndexValue::String(_) => {
                query.push_str(&format!(
                    " AND {}.field_value_str {} ?{}",
                    join_alias,
                    filter_op_to_sql(filter.op),
                    i + 2 + filters.len()
                ));
            }
            IndexValue::Int(_) => {
                query.push_str(&format!(
                    " AND {}.field_value_int {} ?{}",
                    join_alias,
                    filter_op_to_sql(filter.op),
                    i + 2 + filters.len()
                ));
            }
            IndexValue::Bool(_) => {
                query.push_str(&format!(
                    " AND {}.field_value_bool {} ?{}",
                    join_alias,
                    filter_op_to_sql(filter.op),
                    i + 2 + filters.len()
                ));
            }
        }

        query.push(')');
    }

    query.push_str(" ORDER BY r.updated_at DESC");

    let mut stmt = conn.prepare(&query)?;

    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    params.push(Box::new(collection.to_string()));
    for filter in filters {
        params.push(Box::new(filter.field.clone()));
    }
    for filter in filters {
        match &filter.value {
            IndexValue::String(s) => params.push(Box::new(s.clone())),
            IndexValue::Int(i) => params.push(Box::new(*i)),
            IndexValue::Bool(b) => params.push(Box::new(*b as i64)),
        }
    }
    let params_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();

    let rows = stmt.query_map(params_refs.as_slice(), |row| row.get::<_, String>(0))?;
    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

/// Returns `true` if any JSONL file under `base_path` has been modified since the
/// last recorded sync, or if there are JSONL files that have never been synced.
pub fn is_stale(conn: &Connection, base_path: &Path) -> Result<bool> {
    for entry in fs::read_dir(base_path).map_err(|e| Error::Other(format!("Failed to read store directory: {e}")))? {
        let entry = entry?;
        let path = entry.path();

        if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }

        let Some(collection) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };

        let metadata = fs::metadata(&path)?;
        let file_mtime = metadata
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        let stored_mtime: Option<i64> = conn
            .query_row(
                "SELECT file_mtime FROM sync_metadata WHERE collection = ?1",
                [collection],
                |row| row.get(0),
            )
            .optional()?;

        match stored_mtime {
            None => return Ok(true),
            Some(mtime) if file_mtime > mtime => return Ok(true),
            _ => continue,
        }
    }

    Ok(false)
}
