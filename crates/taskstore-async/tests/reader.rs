use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tempfile::TempDir;

use taskstore_async::{AsyncStore, Filter, FilterOp, IndexValue, OpenOptions, Record};

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
        "reader_test_records"
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

async fn seeded_store(records: usize) -> (TempDir, AsyncStore) {
    let temp = TempDir::new().expect("tempdir");
    let store = AsyncStore::open_at(temp.path().join(".taskstore"), OpenOptions::default())
        .await
        .expect("open");
    let recs: Vec<TestRecord> = (0..records)
        .map(|i| TestRecord {
            id: format!("rec{i:04}"),
            name: format!("name{i}"),
            status: if i % 2 == 0 { "active" } else { "inactive" }.to_string(),
            updated_at: now_ms() + i as i64,
        })
        .collect();
    store.create_many(recs).await.expect("create_many");
    (temp, store)
}

#[tokio::test]
async fn test_async_get() {
    let (_temp, store) = seeded_store(5).await;
    let got: Option<TestRecord> = store.get("rec0002").await.expect("get");
    let rec = got.expect("record present");
    assert_eq!(rec.id, "rec0002");
    assert_eq!(rec.status, "active");
}

#[tokio::test]
async fn test_async_get_missing_returns_none() {
    let (_temp, store) = seeded_store(5).await;
    let got: Option<TestRecord> = store.get("nonexistent").await.expect("get");
    assert!(got.is_none());
}

#[tokio::test]
async fn test_async_list_unfiltered() {
    let (_temp, store) = seeded_store(10).await;
    let results: Vec<TestRecord> = store.list(&[]).await.expect("list");
    assert_eq!(results.len(), 10);
}

#[tokio::test]
async fn test_async_list_filtered() {
    let (_temp, store) = seeded_store(10).await;
    let filters = vec![Filter {
        field: "status".to_string(),
        op: FilterOp::Eq,
        value: IndexValue::String("active".to_string()),
    }];
    let results: Vec<TestRecord> = store.list(&filters).await.expect("list");
    assert_eq!(results.len(), 5);
    for r in results {
        assert_eq!(r.status, "active");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_async_concurrent_reads() {
    let (_temp, store) = seeded_store(100).await;
    let store = Arc::new(store);

    // 16 concurrent unfiltered list calls - must all see the full corpus.
    let mut handles = Vec::new();
    for _ in 0..16 {
        let store = store.clone();
        handles.push(tokio::spawn(async move {
            let r: Vec<TestRecord> = store.list(&[]).await.expect("list");
            assert_eq!(r.len(), 100);
        }));
    }
    for h in handles {
        h.await.expect("join");
    }
}

#[tokio::test]
async fn test_async_is_stale_reports_uncached_jsonl() {
    // `create_many` appends to JSONL and inserts into SQLite but does not
    // advance `sync_metadata`, which is the signal `is_stale` keys off.
    // This mirrors the sync Store contract: stale = True after writes until
    // the next `sync()` call stamps sync_metadata. This test locks in the
    // current behavior so a regression shows up here first.
    let (_temp, store) = seeded_store(3).await;
    let stale_before = store.is_stale().await.expect("is_stale");
    assert!(
        stale_before,
        "is_stale must report true when JSONL mtime > sync_metadata"
    );

    store.sync().await.expect("sync");
    let stale_after = store.is_stale().await.expect("is_stale");
    assert!(!stale_after, "after an explicit sync, is_stale must return false");
}
