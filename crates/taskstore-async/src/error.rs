use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("store I/O failure: {0}")]
    Io(#[from] std::io::Error),

    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("store task channel closed (store shut down)")]
    StoreClosed,

    #[error("WAL mode could not be enabled on {path} (filesystem may not support it)")]
    WalUnsupported { path: PathBuf },

    #[error("{0}")]
    Other(String),
}

impl From<eyre::Report> for Error {
    fn from(err: eyre::Report) -> Self {
        Error::Other(format!("{err:#}"))
    }
}
