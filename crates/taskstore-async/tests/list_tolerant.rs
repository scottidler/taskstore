// Async surface tests for AsyncStore::list_tolerant<T>.
//
// Verifies the spawn_blocking dispatch returns the same shape and content as
// the sync path, and that concurrent writes are not blocked by an in-flight
// list_tolerant read (the dispatch goes through spawn_blocking, not the writer
// thread or reader pool).

use std::collections::HashMap;
use std::fs;

use serde::{Deserialize, Serialize};
use tempfile::TempDir;

use taskstore_async::{AsyncStore, CorruptionError, IndexValue, OpenOptions, Record};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct Note {
    id: String,
    title: String,
    status: String,
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
        let mut m = HashMap::new();
        m.insert("status".to_string(), IndexValue::String(self.status.clone()));
        m
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
async fn test_async_list_tolerant_basic() {
    let temp = TempDir::new().unwrap();
    let store_path = temp.path().join(".taskstore");
    fs::create_dir_all(&store_path).unwrap();

    // 100 lines: one syntactically invalid, one missing id, one EOF-truncated,
    // 97 valid. Mirrors the sync test.
    let mut body = String::new();
    for i in 0..100 {
        if i == 10 {
            body.push_str("{not valid json}\n");
        } else if i == 20 {
            body.push_str("{\"title\":\"NoId\",\"status\":\"a\",\"updated_at\":1}\n");
        } else if i == 30 {
            body.push_str("{\"id\":\"trunc\",\"title\":\"never closes\"\n");
        } else {
            body.push_str(&format!(
                "{{\"id\":\"n{i:03}\",\"title\":\"t{i}\",\"status\":\"a\",\"updated_at\":{}}}\n",
                1000 + i
            ));
        }
    }
    fs::write(store_path.join("notes.jsonl"), &body).unwrap();

    let store = AsyncStore::open_at(&store_path, OpenOptions::default())
        .await
        .expect("open");

    let result = store.list_tolerant::<Note>(&[]).await.expect("list_tolerant");

    assert_eq!(result.corruption.len(), 3);
    let lines: Vec<u64> = result.corruption.iter().map(|c| c.line).collect();
    assert!(lines.contains(&11));
    assert!(lines.contains(&21));
    assert!(lines.contains(&31));

    let mut found_invalid = false;
    let mut found_missing = false;
    for entry in &result.corruption {
        match &entry.error {
            CorruptionError::InvalidJson { .. } => found_invalid = true,
            CorruptionError::MissingId => found_missing = true,
            _ => {}
        }
    }
    assert!(found_invalid);
    assert!(found_missing);
    assert_eq!(result.records.len(), 97);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_async_list_tolerant_concurrent_with_writes() {
    // Verify list_tolerant does not serialize behind the writer queue: a write
    // task and a list_tolerant call dispatched together should both make
    // progress (write completes; list_tolerant returns whatever's on disk at
    // the moment its file lock is held).
    let temp = TempDir::new().unwrap();
    let store_path = temp.path().join(".taskstore");
    fs::create_dir_all(&store_path).unwrap();

    // Seed with one valid record so list_tolerant has something to return.
    let body = r#"{"id":"seed","title":"initial","status":"a","updated_at":1000}
"#;
    fs::write(store_path.join("notes.jsonl"), body).unwrap();

    let store = AsyncStore::open_at(&store_path, OpenOptions::default())
        .await
        .expect("open");
    // Sync once so SQLite is populated for any list calls.
    store.sync().await.expect("sync");

    // Kick off a write and a list_tolerant concurrently. Both must complete.
    let store_for_write = &store;
    let write_handle = async {
        let n = Note {
            id: "concurrent".to_string(),
            title: "added during read".to_string(),
            status: "a".to_string(),
            updated_at: now_ms(),
        };
        store_for_write.create(n).await.expect("write completes")
    };
    let read_handle = store.list_tolerant::<Note>(&[]);

    let (_write_id, read_result) = tokio::join!(write_handle, read_handle);

    let result = read_result.expect("list_tolerant completes");
    // Either the read landed before the write (1 record) or after (2 records),
    // but never errors and never deadlocks.
    assert!(
        result.records.len() == 1 || result.records.len() == 2,
        "got {} records: read should see seed alone or seed+concurrent",
        result.records.len()
    );
    assert_eq!(result.corruption.len(), 0);
}
