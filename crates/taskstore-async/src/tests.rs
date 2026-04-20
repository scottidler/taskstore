use super::*;

#[test]
fn test_open_options_default() {
    let opts = OpenOptions::default();
    assert_eq!(opts.read_connections, 4);
    assert_eq!(opts.writer_queue_capacity, 128);
}

#[test]
fn test_error_store_closed_display() {
    let err = Error::StoreClosed;
    assert_eq!(err.to_string(), "store task channel closed (store shut down)");
}

#[test]
fn test_error_wal_unsupported_display() {
    let err = Error::WalUnsupported {
        path: std::path::PathBuf::from("/mnt/nfs/store.db"),
    };
    assert!(err.to_string().contains("WAL mode could not be enabled"));
    assert!(err.to_string().contains("/mnt/nfs/store.db"));
}

#[test]
fn test_error_other_display() {
    let err = Error::Other("custom reason".to_string());
    assert_eq!(err.to_string(), "custom reason");
}

#[test]
fn test_error_from_eyre_report() {
    let report = eyre::eyre!("underlying failure").wrap_err("context");
    let err: Error = report.into();
    match err {
        Error::Other(msg) => {
            assert!(msg.contains("context"));
            assert!(msg.contains("underlying failure"));
        }
        other => panic!("expected Error::Other, got {other:?}"),
    }
}
