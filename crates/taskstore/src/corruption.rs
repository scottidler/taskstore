// Corruption-aware bulk-read implementation.
//
// Public types live in `taskstore-traits`; this module owns the load-bearing
// `list_tolerant_at` free function that both `Store::list_tolerant` and
// `AsyncStore::list_tolerant` consume, plus the `truncate_for_raw` helper used
// by the streaming JSONL reader.

use crate::error::Result;
use crate::jsonl::read_jsonl_latest_with_corruption;
use std::path::Path;
use taskstore_traits::{CorruptionEntry, CorruptionError, Filter, ListResult, Record, match_filter};

/// Maximum byte length of `CorruptionEntry::raw` before truncation.
pub(crate) const RAW_TRUNCATE_BYTES: usize = 4096;

/// Suffix appended when truncation occurs.
pub(crate) const RAW_TRUNCATE_MARKER: &str = "...[truncated]";

/// Truncate `s` at a UTF-8 char boundary at or below `RAW_TRUNCATE_BYTES`,
/// appending `RAW_TRUNCATE_MARKER` if truncation occurred.
///
/// Uses `str::floor_char_boundary` (stable since Rust 1.80) so a multi-byte
/// scalar value spanning the cutoff is rounded down to its start. Naive
/// byte slicing (`&s[..4096]`) would panic on such inputs and is forbidden.
pub(crate) fn truncate_for_raw(s: &str) -> String {
    if s.len() <= RAW_TRUNCATE_BYTES {
        return s.to_string();
    }
    let cut = s.floor_char_boundary(RAW_TRUNCATE_BYTES);
    format!("{}{}", &s[..cut], RAW_TRUNCATE_MARKER)
}

/// Corruption-aware bulk read against a JSONL collection at `base_path`.
///
/// Reads `<base_path>/<T::collection_name()>.jsonl` directly (bypasses SQLite),
/// applies tombstone filtering and `match_filter` in Rust, and returns every
/// line that could not be turned into a usable record.
///
/// Re-parses JSONL on every call; not a hot read. Used by both
/// `Store::list_tolerant` (called directly) and `AsyncStore::list_tolerant`
/// (dispatched via `tokio::task::spawn_blocking`).
pub fn list_tolerant_at<T: Record>(base_path: &Path, filters: &[Filter]) -> Result<ListResult<T>> {
    let path = base_path.join(format!("{}.jsonl", T::collection_name()));
    let (map, mut corruption) = read_jsonl_latest_with_corruption(&path)?;

    let mut records = Vec::new();
    for (_id, (line_no, value)) in map {
        // Tombstone filter: matches sync()'s behavior. A {"deleted": true, ...}
        // row that parses cleanly is dropped from records and is NOT corruption.
        if value.get("deleted").and_then(|v| v.as_bool()) == Some(true) {
            continue;
        }

        match serde_json::from_value::<T>(value.clone()) {
            Ok(record) => {
                if filters.iter().all(|f| match_filter(&record.indexed_fields(), f)) {
                    records.push(record);
                }
            }
            Err(e) => {
                corruption.push(CorruptionEntry {
                    file: path.clone(),
                    line: line_no,
                    raw: truncate_for_raw(&value.to_string()),
                    error: CorruptionError::TypeMismatch { msg: e.to_string() },
                });
            }
        }
    }

    Ok(ListResult::new(records, corruption))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_for_raw_short_unchanged() {
        let s = "short string";
        assert_eq!(truncate_for_raw(s), s);
    }

    #[test]
    fn test_truncate_for_raw_at_boundary_unchanged() {
        let s = "a".repeat(RAW_TRUNCATE_BYTES);
        assert_eq!(truncate_for_raw(&s), s);
    }

    #[test]
    fn test_truncate_for_raw_long_appends_marker() {
        let s = "a".repeat(RAW_TRUNCATE_BYTES + 100);
        let out = truncate_for_raw(&s);
        assert!(out.ends_with(RAW_TRUNCATE_MARKER));
        // body is RAW_TRUNCATE_BYTES "a"s + the marker
        assert_eq!(out.len(), RAW_TRUNCATE_BYTES + RAW_TRUNCATE_MARKER.len());
    }

    #[test]
    fn test_truncate_for_raw_multibyte_no_panic() {
        // Build a string where the byte at index 4095 would land in the middle
        // of a 4-byte scalar (an emoji is 4 bytes in UTF-8). 4092 ASCII chars
        // followed by an emoji means the emoji starts at byte 4092 and spans
        // 4092..4096. floor_char_boundary(4096) returns 4096 (boundary).
        // To force a mid-scalar cutoff we need bytes that put a scalar's
        // start before 4096 but its end past 4096. 4094 ASCII + emoji starts
        // at 4094, spans 4094..4098 - so 4096 falls mid-scalar.
        let mut s = String::new();
        for _ in 0..4094 {
            s.push('a');
        }
        s.push('🚀'); // 4 bytes
        s.push_str(&"b".repeat(100));

        // Sanity: original length is well over the truncate threshold.
        assert!(s.len() > RAW_TRUNCATE_BYTES);

        // Must not panic. Result must be valid UTF-8 (it is by String type)
        // and end with the truncation marker.
        let out = truncate_for_raw(&s);
        assert!(out.ends_with(RAW_TRUNCATE_MARKER));
        // The scalar was rounded down: cut should be at byte 4094 (the last
        // valid char boundary at or below 4096).
        let body_len = out.len() - RAW_TRUNCATE_MARKER.len();
        assert!(body_len <= RAW_TRUNCATE_BYTES);
    }
}
