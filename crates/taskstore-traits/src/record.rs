// Generic record trait for any storable type

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Core trait that any storable record must implement
pub trait Record: Serialize + for<'de> Deserialize<'de> + Clone + Send + Sync + 'static {
    /// Unique identifier for this record
    fn id(&self) -> &str;

    /// Timestamp when this record was last updated (milliseconds since epoch)
    fn updated_at(&self) -> i64;

    /// Collection name for this record type (e.g., "plans", "specs")
    /// Determines the JSONL filename: {collection}.jsonl
    fn collection_name() -> &'static str
    where
        Self: Sized;

    /// Fields to index for filtering
    /// Return empty HashMap if no fields should be indexed
    fn indexed_fields(&self) -> HashMap<String, IndexValue> {
        HashMap::new()
    }
}

/// Value types that can be indexed for filtering
#[derive(Debug, Clone, PartialEq)]
pub enum IndexValue {
    String(String),
    Int(i64),
    Bool(bool),
}

impl std::fmt::Display for IndexValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IndexValue::String(s) => write!(f, "{}", s),
            IndexValue::Int(i) => write!(f, "{}", i),
            IndexValue::Bool(b) => write!(f, "{}", b),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct TestRecord {
        id: String,
        name: String,
        updated_at: i64,
    }

    impl Record for TestRecord {
        fn id(&self) -> &str {
            &self.id
        }

        fn updated_at(&self) -> i64 {
            self.updated_at
        }

        fn collection_name() -> &'static str {
            "test"
        }
    }

    #[test]
    fn test_record_trait_implementation() {
        let record = TestRecord {
            id: "test-1".to_string(),
            name: "Test".to_string(),
            updated_at: 1000,
        };

        assert_eq!(record.id(), "test-1");
        assert_eq!(record.updated_at(), 1000);
        assert_eq!(TestRecord::collection_name(), "test");
        assert!(record.indexed_fields().is_empty());
    }

    #[test]
    fn test_index_value_display() {
        assert_eq!(IndexValue::String("test".to_string()).to_string(), "test");
        assert_eq!(IndexValue::Int(42).to_string(), "42");
        assert_eq!(IndexValue::Bool(true).to_string(), "true");
    }
}
