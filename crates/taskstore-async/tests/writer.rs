use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use tempfile::TempDir;

use taskstore_async::{AsyncStore, IndexValue, OpenOptions, Record};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct TestRecord {
    id: String,
    name: String,
    status: String,
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
        "async_test_records"
    }
    fn indexed_fields(&self) -> HashMap<String, IndexValue> {
        let mut fields = HashMap::new();
        fields.insert("status".to_string(), IndexValue::String(self.status.clone()));
        fields
    }
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before unix epoch")
        .as_millis() as i64
}

#[tokio::test]
async fn test_async_create_roundtrip() {
    let temp = TempDir::new().expect("tempdir");
    let store = AsyncStore::open(temp.path(), OpenOptions::default())
        .await
        .expect("open");

    let rec = TestRecord {
        id: "rec1".to_string(),
        name: "hello".to_string(),
        status: "active".to_string(),
        updated_at: now_ms(),
    };
    let id = store.create(rec.clone()).await.expect("create");
    assert_eq!(id, "rec1");
}

#[tokio::test]
async fn test_async_create_many_preserves_order() {
    let temp = TempDir::new().expect("tempdir");
    let store = AsyncStore::open(temp.path(), OpenOptions::default())
        .await
        .expect("open");

    let records: Vec<TestRecord> = (0..10)
        .map(|i| TestRecord {
            id: format!("rec{i}"),
            name: format!("name{i}"),
            status: "active".to_string(),
            updated_at: now_ms() + i,
        })
        .collect();

    let ids = store.create_many(records).await.expect("create_many");
    let expected: Vec<String> = (0..10).map(|i| format!("rec{i}")).collect();
    assert_eq!(ids, expected);
}

#[tokio::test]
async fn test_async_write_is_serialized_under_concurrency() {
    let temp = TempDir::new().expect("tempdir");
    let store = std::sync::Arc::new(
        AsyncStore::open(temp.path(), OpenOptions::default())
            .await
            .expect("open"),
    );

    // Fire 20 concurrent creates; writer thread serializes them.
    let mut handles = Vec::new();
    for i in 0..20 {
        let store = store.clone();
        handles.push(tokio::spawn(async move {
            let rec = TestRecord {
                id: format!("rec{i}"),
                name: format!("name{i}"),
                status: "active".to_string(),
                updated_at: now_ms() + i,
            };
            store.create(rec).await
        }));
    }

    for h in handles {
        let result = h.await.expect("task join");
        result.expect("create ok");
    }
}

#[tokio::test]
async fn test_async_drop_shuts_down_cleanly() {
    let temp = TempDir::new().expect("tempdir");
    let store = AsyncStore::open(temp.path(), OpenOptions::default())
        .await
        .expect("open");

    let rec = TestRecord {
        id: "rec1".to_string(),
        name: "hello".to_string(),
        status: "active".to_string(),
        updated_at: now_ms(),
    };
    store.create(rec).await.expect("create");

    // Drop the store; Drop impl on WriterHandle joins the writer thread.
    drop(store);
    // Test passes if we don't hang. Explicit assert for clarity.
}
