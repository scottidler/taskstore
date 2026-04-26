// Query filtering for generic records

use crate::record::IndexValue;
use std::collections::HashMap;

/// Filter for querying records
#[derive(Debug, Clone)]
pub struct Filter {
    /// Field name to filter on
    pub field: String,
    /// Comparison operator
    pub op: FilterOp,
    /// Value to compare against
    pub value: IndexValue,
}

/// Comparison operators for filtering
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterOp {
    Eq,       // ==
    Ne,       // !=
    Gt,       // >
    Lt,       // <
    Gte,      // >=
    Lte,      // <=
    Contains, // LIKE %value%
}

impl std::fmt::Display for FilterOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FilterOp::Eq => write!(f, "="),
            FilterOp::Ne => write!(f, "!="),
            FilterOp::Gt => write!(f, ">"),
            FilterOp::Lt => write!(f, "<"),
            FilterOp::Gte => write!(f, ">="),
            FilterOp::Lte => write!(f, "<="),
            FilterOp::Contains => write!(f, "LIKE"),
        }
    }
}

/// In-Rust mirror of the SQL produced by `taskstore::query::list_data_jsons`.
/// Returns `true` iff the record's indexed `fields` satisfy `f`.
///
/// Parity rules (must match the SQL path):
///
/// - **Field absent.** Returns `false`. Mirrors SQL `EXISTS` semantics: a row
///   without an entry in `record_indexes` for this `field_name` joins to no row.
/// - **Type mismatch** (filter value is `Int`, indexed value is `String`, etc).
///   Returns `false`. Mirrors the SQL path's per-column typing: filter values
///   route to one of `field_value_str`/`field_value_int`/`field_value_bool`
///   columns; the others are NULL and `NULL <op> X` evaluates to NULL/false.
/// - **`Contains`** is ASCII-case-insensitive substring match (lowercase both
///   sides, then `str::contains`). Mirrors SQLite `LIKE`'s default ASCII-case
///   behavior. Non-ASCII case-folding is intentionally not handled - SQLite
///   `LIKE` does not handle it either, and parity is more valuable than
///   completeness.
/// - **`Contains` on Int/Bool**. Returns `false`. SQLite `LIKE` on a non-string
///   column produces no matches in the existing schema.
pub fn match_filter(fields: &HashMap<String, IndexValue>, f: &Filter) -> bool {
    let Some(actual) = fields.get(&f.field) else {
        return false;
    };
    match (actual, &f.value) {
        (IndexValue::String(a), IndexValue::String(b)) => match f.op {
            FilterOp::Eq => a == b,
            FilterOp::Ne => a != b,
            FilterOp::Gt => a > b,
            FilterOp::Lt => a < b,
            FilterOp::Gte => a >= b,
            FilterOp::Lte => a <= b,
            FilterOp::Contains => a.to_ascii_lowercase().contains(&b.to_ascii_lowercase()),
        },
        (IndexValue::Int(a), IndexValue::Int(b)) => match f.op {
            FilterOp::Eq => a == b,
            FilterOp::Ne => a != b,
            FilterOp::Gt => a > b,
            FilterOp::Lt => a < b,
            FilterOp::Gte => a >= b,
            FilterOp::Lte => a <= b,
            FilterOp::Contains => false,
        },
        (IndexValue::Bool(a), IndexValue::Bool(b)) => match f.op {
            FilterOp::Eq => a == b,
            FilterOp::Ne => a != b,
            FilterOp::Gt | FilterOp::Lt | FilterOp::Gte | FilterOp::Lte | FilterOp::Contains => false,
        },
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields_str(field: &str, val: &str) -> HashMap<String, IndexValue> {
        let mut m = HashMap::new();
        m.insert(field.to_string(), IndexValue::String(val.to_string()));
        m
    }

    fn fields_int(field: &str, val: i64) -> HashMap<String, IndexValue> {
        let mut m = HashMap::new();
        m.insert(field.to_string(), IndexValue::Int(val));
        m
    }

    fn fields_bool(field: &str, val: bool) -> HashMap<String, IndexValue> {
        let mut m = HashMap::new();
        m.insert(field.to_string(), IndexValue::Bool(val));
        m
    }

    fn filter_str(field: &str, op: FilterOp, val: &str) -> Filter {
        Filter {
            field: field.to_string(),
            op,
            value: IndexValue::String(val.to_string()),
        }
    }

    fn filter_int(field: &str, op: FilterOp, val: i64) -> Filter {
        Filter {
            field: field.to_string(),
            op,
            value: IndexValue::Int(val),
        }
    }

    fn filter_bool(field: &str, op: FilterOp, val: bool) -> Filter {
        Filter {
            field: field.to_string(),
            op,
            value: IndexValue::Bool(val),
        }
    }

    #[test]
    fn test_filter_creation() {
        let filter = Filter {
            field: "status".to_string(),
            op: FilterOp::Eq,
            value: IndexValue::String("active".to_string()),
        };

        assert_eq!(filter.field, "status");
        assert_eq!(filter.op, FilterOp::Eq);
    }

    #[test]
    fn test_filter_op_display() {
        assert_eq!(FilterOp::Eq.to_string(), "=");
        assert_eq!(FilterOp::Ne.to_string(), "!=");
    }

    #[test]
    fn test_match_filter_string_eq_ne() {
        let f = fields_str("status", "active");
        assert!(match_filter(&f, &filter_str("status", FilterOp::Eq, "active")));
        assert!(!match_filter(&f, &filter_str("status", FilterOp::Eq, "inactive")));
        assert!(match_filter(&f, &filter_str("status", FilterOp::Ne, "inactive")));
        assert!(!match_filter(&f, &filter_str("status", FilterOp::Ne, "active")));
    }

    #[test]
    fn test_match_filter_string_lt_gt_gte_lte() {
        let f = fields_str("status", "b");
        assert!(match_filter(&f, &filter_str("status", FilterOp::Gt, "a")));
        assert!(!match_filter(&f, &filter_str("status", FilterOp::Gt, "b")));
        assert!(match_filter(&f, &filter_str("status", FilterOp::Lt, "c")));
        assert!(!match_filter(&f, &filter_str("status", FilterOp::Lt, "b")));
        assert!(match_filter(&f, &filter_str("status", FilterOp::Gte, "b")));
        assert!(match_filter(&f, &filter_str("status", FilterOp::Lte, "b")));
    }

    #[test]
    fn test_match_filter_int_ops() {
        let f = fields_int("count", 10);
        assert!(match_filter(&f, &filter_int("count", FilterOp::Eq, 10)));
        assert!(!match_filter(&f, &filter_int("count", FilterOp::Eq, 11)));
        assert!(match_filter(&f, &filter_int("count", FilterOp::Gt, 5)));
        assert!(match_filter(&f, &filter_int("count", FilterOp::Lt, 11)));
        assert!(match_filter(&f, &filter_int("count", FilterOp::Gte, 10)));
        assert!(match_filter(&f, &filter_int("count", FilterOp::Lte, 10)));
        assert!(!match_filter(&f, &filter_int("count", FilterOp::Contains, 10))); // Contains on int -> false
    }

    #[test]
    fn test_match_filter_bool_eq_ne() {
        let f = fields_bool("active", true);
        assert!(match_filter(&f, &filter_bool("active", FilterOp::Eq, true)));
        assert!(!match_filter(&f, &filter_bool("active", FilterOp::Eq, false)));
        assert!(match_filter(&f, &filter_bool("active", FilterOp::Ne, false)));
        // ordering ops on bool -> false (matches SQL parity)
        assert!(!match_filter(&f, &filter_bool("active", FilterOp::Gt, false)));
        assert!(!match_filter(&f, &filter_bool("active", FilterOp::Contains, true)));
    }

    #[test]
    fn test_match_filter_contains_ascii_case_insensitive() {
        let f = fields_str("status", "ACTIVE-task");
        // both ways: filter upper, indexed mixed; filter lower, indexed mixed.
        assert!(match_filter(&f, &filter_str("status", FilterOp::Contains, "active")));
        assert!(match_filter(&f, &filter_str("status", FilterOp::Contains, "ACTIVE")));
        assert!(match_filter(&f, &filter_str("status", FilterOp::Contains, "TASK")));
        assert!(!match_filter(&f, &filter_str("status", FilterOp::Contains, "absent")));
    }

    #[test]
    fn test_match_filter_field_absent_returns_false() {
        let f = fields_str("status", "active");
        // Filter references a different field -> no match (mirrors SQL EXISTS).
        assert!(!match_filter(&f, &filter_str("priority", FilterOp::Eq, "active")));
    }

    #[test]
    fn test_match_filter_type_mismatch_returns_false() {
        let f = fields_str("count", "10"); // indexed as String
        // Filter wants Int -> mismatch, returns false (mirrors SQL per-column typing).
        assert!(!match_filter(&f, &filter_int("count", FilterOp::Eq, 10)));

        let f = fields_int("count", 10); // indexed as Int
        // Filter wants String -> mismatch, returns false.
        assert!(!match_filter(&f, &filter_str("count", FilterOp::Eq, "10")));
    }
}
