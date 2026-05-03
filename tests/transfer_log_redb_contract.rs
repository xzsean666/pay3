use pay3::{
    domain::{BlockHash, EvmAddress, RawAmount, TxHash},
    transfer_log_store::{
        LogPageToken, LogSourceKind, ScanTargetMode, StoredBlockHeader, StoredTransferLog,
        StreamId, TransferLogCursor, TransferLogStreamConfig,
    },
};
use tempfile::NamedTempFile;
use time::{OffsetDateTime, macros::datetime};

mod transfer_log_store {
    pub use pay3::transfer_log_store::*;
}

#[path = "../src/transfer_log_store/redb_store.rs"]
mod redb_store;

use redb_store::{RangeManifestDto, RedbTransferLogBatch, RedbTransferLogStore};

#[test]
fn transfer_log_redb_persists_config_cursor_and_header_across_reopen() {
    let file = NamedTempFile::new().unwrap();
    let stream = stream();
    let config = config(stream);
    let now = now();
    let cursor = TransferLogCursor::initial(&config, 7, now);
    let header = header(stream, 42);

    {
        let store = RedbTransferLogStore::open(file.path()).unwrap();
        store.save_stream_config(&config).unwrap();
        store.save_cursor(&cursor).unwrap();
        store.save_block_header(&header).unwrap();
    }

    let store = RedbTransferLogStore::open(file.path()).unwrap();
    assert_eq!(store.load_stream_config(stream).unwrap(), Some(config));
    assert_eq!(store.load_cursor(stream).unwrap(), Some(cursor));
    assert_eq!(store.load_block_header(stream, 42).unwrap(), Some(header));
}

#[test]
fn transfer_log_redb_logs_page_uses_numeric_order_and_exclusive_token() {
    let file = NamedTempFile::new().unwrap();
    let store = RedbTransferLogStore::open(file.path()).unwrap();
    let stream = stream();

    for log in [
        log(stream, 10, 0),
        log(stream, 2, 1),
        log(stream, 2, 0),
        log(stream, 1, 9),
    ] {
        store.upsert_transfer_log(&log).unwrap();
    }

    let first = store.logs_page(stream, None, 2).unwrap();
    assert_eq!(
        first
            .logs
            .iter()
            .map(StoredTransferLog::page_token)
            .collect::<Vec<_>>(),
        vec![LogPageToken::new(1, 9), LogPageToken::new(2, 0)]
    );
    assert_eq!(first.next_token, Some(LogPageToken::new(2, 0)));
    assert_eq!(first.complete_to_block, Some(1));

    let second = store.logs_page(stream, first.next_token, 2).unwrap();
    assert_eq!(
        second
            .logs
            .iter()
            .map(StoredTransferLog::page_token)
            .collect::<Vec<_>>(),
        vec![LogPageToken::new(2, 1), LogPageToken::new(10, 0)]
    );
    assert_eq!(second.next_token, Some(LogPageToken::new(10, 0)));
    assert_eq!(second.complete_to_block, Some(10));
}

#[test]
fn transfer_log_redb_batch_writes_logs_headers_range_and_cursor_atomically() {
    let file = NamedTempFile::new().unwrap();
    let store = RedbTransferLogStore::open(file.path()).unwrap();
    let stream = stream();
    let config = config(stream);
    let mut cursor = TransferLogCursor::initial(&config, 1, now());
    cursor.next_block = 13;
    cursor.last_completed_block = Some(12);

    store
        .write_batch(RedbTransferLogBatch {
            stream,
            headers: vec![header(stream, 11), header(stream, 12)],
            logs: vec![log(stream, 11, 0), log(stream, 12, 0)],
            range_manifest: Some(RangeManifestDto {
                stream,
                from_block: 11,
                to_block: 12,
                block_count: 2,
                log_count: 2,
                writer_epoch: cursor.writer_epoch,
                completed_at: now(),
            }),
            cursor: cursor.clone(),
        })
        .unwrap();

    assert_eq!(store.load_cursor(stream).unwrap(), Some(cursor));
    assert!(store.load_block_header(stream, 11).unwrap().is_some());
    assert_eq!(store.logs_page(stream, None, 10).unwrap().logs.len(), 2);
    assert_eq!(store.range_manifests(stream, 10).unwrap().len(), 1);
}

#[test]
fn transfer_log_redb_rewind_deletes_block_and_after_and_records_reorg_cursor() {
    let file = NamedTempFile::new().unwrap();
    let store = RedbTransferLogStore::open(file.path()).unwrap();
    let stream = stream();
    let config = config(stream);
    let mut cursor = TransferLogCursor::initial(&config, 3, now());
    cursor.next_block = 14;
    cursor.last_completed_block = Some(13);
    cursor.last_completed_hash = Some(hash(13));

    store
        .write_batch(RedbTransferLogBatch {
            stream,
            headers: vec![header(stream, 9), header(stream, 10), header(stream, 13)],
            logs: vec![log(stream, 9, 0), log(stream, 10, 0), log(stream, 13, 0)],
            range_manifest: Some(RangeManifestDto {
                stream,
                from_block: 9,
                to_block: 13,
                block_count: 5,
                log_count: 3,
                writer_epoch: cursor.writer_epoch,
                completed_at: now(),
            }),
            cursor,
        })
        .unwrap();

    let rewound = store
        .rewind_delete_from_block(stream, 10, datetime!(2026-01-01 00:00 UTC))
        .unwrap()
        .unwrap();

    assert_eq!(rewound.next_block, 10);
    assert_eq!(rewound.last_completed_block, Some(9));
    assert_eq!(rewound.last_completed_hash, None);
    assert_eq!(rewound.reorg_epoch, 1);
    assert_eq!(rewound.last_reorg_from, Some(10));
    assert!(store.load_block_header(stream, 9).unwrap().is_some());
    assert!(store.load_block_header(stream, 10).unwrap().is_none());
    assert_eq!(
        store.logs_page(stream, None, 10).unwrap().logs,
        vec![log(stream, 9, 0)]
    );
    assert!(store.range_manifests(stream, 10).unwrap().is_empty());
}

fn stream() -> StreamId {
    StreamId::new(1, address(0x11))
}

fn config(stream: StreamId) -> TransferLogStreamConfig {
    TransferLogStreamConfig {
        chain_id: stream.chain_id,
        token_address: stream.token_address,
        start_block: 9,
        poll_interval_ms: 1_000,
        batch_size_blocks: 10,
        max_batch_size_blocks: 50,
        max_logs_per_page: 100,
        max_unique_to_addresses_per_batch: 100,
        max_db_fallback_addresses: 100,
        capacity_probe_blocks: 10,
        reorg_lookback_blocks: 12,
        target_mode: ScanTargetMode::LatestMinusConfirmations(6),
        rpc_max_retries: 3,
        log_source: LogSourceKind::RpcRange,
    }
}

fn header(stream: StreamId, block: u64) -> StoredBlockHeader {
    StoredBlockHeader {
        chain_id: stream.chain_id,
        token_address: stream.token_address,
        block_number: block,
        block_hash: hash(block),
        parent_hash: hash(block.saturating_sub(1)),
        timestamp: now(),
        scanned_at: now(),
    }
}

fn log(stream: StreamId, block: u64, index: u64) -> StoredTransferLog {
    StoredTransferLog {
        chain_id: stream.chain_id,
        token_address: stream.token_address,
        block_number: block,
        block_hash: hash(block),
        block_timestamp: now(),
        tx_hash: tx(block, index),
        tx_index: Some(index),
        log_index: index,
        from_address: address(0x01),
        to_address: address(0x02),
        amount_raw: RawAmount::from(block + index),
        removed: false,
        observed_at: now(),
    }
}

fn now() -> OffsetDateTime {
    datetime!(2025-01-01 00:00 UTC)
}

fn address(byte: u8) -> EvmAddress {
    EvmAddress::from_bytes([byte; 20])
}

fn hash(byte: u64) -> BlockHash {
    BlockHash::from_bytes([byte as u8; 32])
}

fn tx(block: u64, index: u64) -> TxHash {
    let mut bytes = [0; 32];
    bytes[..8].copy_from_slice(&block.to_be_bytes());
    bytes[8..16].copy_from_slice(&index.to_be_bytes());
    TxHash::from_bytes(bytes)
}
