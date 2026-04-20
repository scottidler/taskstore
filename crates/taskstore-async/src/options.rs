/// Configuration for opening an `AsyncStore`.
#[derive(Debug, Clone)]
pub struct OpenOptions {
    /// Number of independent SQLite connections in the reader pool.
    /// Each connection is used inside `tokio::task::spawn_blocking` for one
    /// read operation at a time. Default: 4.
    pub read_connections: usize,

    /// Upper bound on in-flight write commands queued to the writer thread.
    /// Backpressure: further `async fn` write calls await until a slot opens.
    /// Default: 128.
    pub writer_queue_capacity: usize,
}

impl Default for OpenOptions {
    fn default() -> Self {
        Self {
            read_connections: 4,
            writer_queue_capacity: 128,
        }
    }
}
