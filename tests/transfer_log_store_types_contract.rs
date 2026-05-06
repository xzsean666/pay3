mod domain {
    pub use pay3::domain::*;
}

#[path = "../src/transfer_log_store/types.rs"]
#[allow(dead_code)]
mod types;

use time::macros::datetime;

use domain::{BlockHash, EvmAddress, RawAmount, TxHash};
use types::{
    LogPageToken, LogSourceKind, ScanTargetMode, StoredTransferLog, TransferLogCursor,
    TransferLogStreamConfig, TransferLogTypeError, validate_logs_in_range_limit,
    validate_logs_page_limit,
};

fn address(byte: u8) -> EvmAddress {
    EvmAddress::from_bytes([byte; 20])
}

fn block_hash(byte: u8) -> BlockHash {
    BlockHash::from_bytes([byte; 32])
}

fn tx_hash(byte: u8) -> TxHash {
    TxHash::from_bytes([byte; 32])
}

fn config(start_block: u64) -> TransferLogStreamConfig {
    TransferLogStreamConfig {
        chain_id: 1,
        token_address: address(0x11),
        start_block,
        poll_interval_ms: 1_000,
        batch_size_blocks: 100,
        max_batch_size_blocks: 1_000,
        max_logs_per_page: 500,
        max_unique_to_addresses_per_batch: 500,
        max_db_fallback_addresses: 500,
        capacity_probe_blocks: 100,
        reorg_lookback_blocks: 64,
        target_mode: ScanTargetMode::SafeTag,
        rpc_max_retries: 3,
        log_source: LogSourceKind::RpcRange,
    }
}

fn stored_log(block_number: u64, log_index: u64) -> StoredTransferLog {
    StoredTransferLog {
        chain_id: 1,
        token_address: address(0x11),
        block_number,
        block_hash: block_hash(block_number as u8),
        block_timestamp: datetime!(2026-05-03 10:00 UTC),
        tx_hash: tx_hash(log_index as u8),
        tx_index: Some(0),
        log_index,
        from_address: address(0x22),
        to_address: address(0x33),
        amount_raw: RawAmount::from(42),
        removed: false,
        observed_at: datetime!(2026-05-03 10:01 UTC),
    }
}

#[test]
fn page_tokens_order_and_serialize_as_exclusive_log_positions() {
    let earlier = LogPageToken::new(10, 7);
    let same_block_later = LogPageToken::new(10, 8);
    let next_block = LogPageToken::new(11, 0);

    assert!(earlier < same_block_later);
    assert!(same_block_later < next_block);

    let json = serde_json::to_string(&same_block_later).unwrap();
    assert_eq!(json, r#"{"block_number":10,"log_index":8}"#);
    let decoded: LogPageToken = serde_json::from_str(&json).unwrap();
    assert_eq!(decoded, same_block_later);

    assert!(!earlier.includes_log_exclusively(&stored_log(10, 7)));
    assert!(earlier.includes_log_exclusively(&stored_log(10, 8)));
    assert!(earlier.includes_log_exclusively(&stored_log(11, 0)));
}

#[test]
fn stored_logs_have_position_ordering_helpers() {
    let mut logs = [stored_log(12, 0), stored_log(10, 8), stored_log(10, 7)];
    logs.sort_by(|left, right| left.cmp_position(right));

    assert_eq!(
        logs.iter()
            .map(StoredTransferLog::page_token)
            .collect::<Vec<_>>(),
        vec![
            LogPageToken::new(10, 7),
            LogPageToken::new(10, 8),
            LogPageToken::new(12, 0),
        ]
    );
}

#[test]
fn cursor_tracks_reorg_and_writer_epoch_fields() {
    let now = datetime!(2026-05-03 10:00 UTC);
    let mut cursor = TransferLogCursor::initial(&config(100), 7, now);

    assert_eq!(cursor.reorg_epoch, 0);
    assert_eq!(cursor.last_reorg_from, None);
    assert_eq!(cursor.last_reorg_at, None);
    assert_eq!(cursor.writer_epoch, 7);

    let reorg_at = datetime!(2026-05-03 11:00 UTC);
    cursor.record_rewind(120, 8, reorg_at);

    assert_eq!(cursor.reorg_epoch, 1);
    assert_eq!(cursor.last_reorg_from, Some(120));
    assert_eq!(cursor.last_reorg_at, Some(reorg_at));
    assert_eq!(cursor.writer_epoch, 8);
    assert_eq!(cursor.next_block, 120);
    assert_eq!(cursor.last_completed_block, Some(119));
}

#[test]
fn stream_config_reports_identity_conflict_inputs() {
    let existing = config(100);
    let mut requested = config(101);
    let conflict = requested.identity_conflict(&existing).unwrap();

    assert_eq!(conflict.stream, existing.stream_id());
    assert_eq!(conflict.existing_start_block, 100);
    assert_eq!(conflict.requested_start_block, 101);

    requested.start_block = existing.start_block;
    assert_eq!(requested.identity_conflict(&existing), None);

    requested.token_address = address(0x12);
    requested.start_block = 101;
    assert_eq!(requested.identity_conflict(&existing), None);
}

#[test]
fn log_read_limits_must_be_nonzero() {
    assert_eq!(
        validate_logs_in_range_limit(0),
        Err(TransferLogTypeError::ZeroLimit { field: "max_logs" })
    );
    assert_eq!(
        validate_logs_page_limit(0),
        Err(TransferLogTypeError::ZeroLimit { field: "limit" })
    );
    assert!(validate_logs_in_range_limit(1).is_ok());
    assert!(validate_logs_page_limit(1).is_ok());
}
