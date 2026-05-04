use std::path::Path;

use pay3::transfer_log_store::redb_store::RedbTransferLogStore as RuntimeRedbTransferLogStore;
use pay3::{
    chain::{ChainBlock, FakeErc20ChainClient, TransferLog},
    domain::{BlockHash, ChainBlockRef, EvmAddress, RawAmount, TxHash},
    transfer_log_store::{
        LogPageToken, LogSourceKind, PollOutcome, RedbTransferLogIngestor, ScanTargetMode,
        StoredBlockHeader, StoredTransferLog, StreamId, TransferLogCursor, TransferLogIngestor,
        TransferLogReader, TransferLogStoreError, TransferLogStreamConfig,
    },
};
use tempfile::NamedTempFile;
use time::{OffsetDateTime, macros::datetime};

mod transfer_log_store {
    pub use pay3::transfer_log_store::*;
}

#[path = "../src/transfer_log_store/redb_store.rs"]
#[allow(dead_code)]
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

#[test]
fn transfer_log_redb_prune_before_block_removes_old_data_and_preserves_cursor() {
    let file = NamedTempFile::new().unwrap();
    let store = RedbTransferLogStore::open(file.path()).unwrap();
    let stream = stream();
    let config = config(stream);

    let mut old_cursor = TransferLogCursor::initial(&config, 1, now());
    old_cursor.next_block = 4;
    old_cursor.last_completed_block = Some(3);
    old_cursor.last_completed_hash = Some(hash(3));

    store
        .write_batch(RedbTransferLogBatch {
            stream,
            headers: vec![header(stream, 2), header(stream, 3)],
            logs: vec![log(stream, 2, 0)],
            range_manifest: Some(RangeManifestDto {
                stream,
                from_block: 2,
                to_block: 3,
                block_count: 2,
                log_count: 1,
                writer_epoch: old_cursor.writer_epoch,
                completed_at: now(),
            }),
            cursor: old_cursor,
        })
        .unwrap();

    let mut new_cursor = TransferLogCursor::initial(&config, 1, now());
    new_cursor.next_block = 6;
    new_cursor.last_completed_block = Some(5);
    new_cursor.last_completed_hash = Some(hash(5));

    store
        .write_batch(RedbTransferLogBatch {
            stream,
            headers: vec![header(stream, 4), header(stream, 5)],
            logs: vec![log(stream, 4, 0)],
            range_manifest: Some(RangeManifestDto {
                stream,
                from_block: 4,
                to_block: 5,
                block_count: 2,
                log_count: 1,
                writer_epoch: new_cursor.writer_epoch,
                completed_at: now(),
            }),
            cursor: new_cursor.clone(),
        })
        .unwrap();

    let cursor_before = store.load_cursor(stream).unwrap().unwrap();
    store.prune_before_block(stream, 4).unwrap();

    assert_eq!(store.load_cursor(stream).unwrap(), Some(cursor_before));
    assert!(store.load_block_header(stream, 2).unwrap().is_none());
    assert!(store.load_block_header(stream, 3).unwrap().is_none());
    assert!(store.load_block_header(stream, 4).unwrap().is_some());
    assert!(store.load_block_header(stream, 5).unwrap().is_some());
    assert_eq!(
        store.logs_in_range(stream, 4, 5, 10).unwrap(),
        vec![log(stream, 4, 0)]
    );
    assert_eq!(
        store.range_manifests(stream, 10).unwrap(),
        vec![RangeManifestDto {
            stream,
            from_block: 4,
            to_block: 5,
            block_count: 2,
            log_count: 1,
            writer_epoch: new_cursor.writer_epoch,
            completed_at: now(),
        }]
    );
}

#[tokio::test]
async fn redb_ingestor_polls_persists_and_reopens_reader_state() {
    let file = NamedTempFile::new().unwrap();
    let stream = stream();
    let chain = runtime_chain(10).push_transfer_log(chain_log(stream, 9, 0, 0x42));
    let ingestor = runtime_ingestor(chain.clone(), file.path());

    ingestor.ensure_stream(config(stream)).await.unwrap();
    let outcome = ingestor.poll_once(stream).await.unwrap();

    assert!(matches!(
        outcome,
        PollOutcome::Advanced {
            from_block: 9,
            to_block: 10,
            log_count: 1,
            ..
        }
    ));
    drop(ingestor);

    let reopened = runtime_ingestor(chain, file.path());
    let cursor = reopened.cursor(stream).await.unwrap();
    assert_eq!(cursor.next_block, 11);
    assert_eq!(cursor.last_completed_block, Some(10));
    assert!(
        reopened.block_header(stream, 10).await.unwrap().is_some(),
        "empty log blocks must still persist headers"
    );
    assert_eq!(
        reopened.logs_page(stream, None, 10).await.unwrap().logs,
        vec![stored_from_chain_log(chain_log(stream, 9, 0, 0x42))]
    );
}

#[tokio::test]
async fn redb_ingestor_rewinds_persisted_state_on_reorg_and_rescans() {
    let file = NamedTempFile::new().unwrap();
    let stream = stream();
    let chain = runtime_chain(9).push_transfer_log(chain_log(stream, 9, 0, 0x42));
    let ingestor = runtime_ingestor(chain.clone(), file.path());
    ingestor.ensure_stream(config(stream)).await.unwrap();
    ingestor.poll_once(stream).await.unwrap();
    drop(ingestor);

    let new_block = chain_block(9, 0xaa);
    let new_log = TransferLog {
        block: new_block,
        ..chain_log(stream, 9, 0, 0x99)
    };
    chain
        .replace_block_for_reorg(new_block)
        .push_transfer_log(new_log.clone());

    let reorg_ingestor = runtime_ingestor(chain.clone(), file.path());
    let rewound = reorg_ingestor.poll_once(stream).await.unwrap();
    assert!(matches!(
        rewound,
        PollOutcome::Rewound {
            from_block: 9,
            cursor: TransferLogCursor {
                reorg_epoch: 1,
                next_block: 9,
                ..
            },
            ..
        }
    ));
    drop(reorg_ingestor);

    let ingestor = runtime_ingestor(chain, file.path());
    ingestor.poll_once(stream).await.unwrap();
    let logs = ingestor.logs_in_range(stream, 9, 9, 10).await.unwrap();
    assert_eq!(logs, vec![stored_from_chain_log(new_log)]);
    assert_eq!(ingestor.cursor(stream).await.unwrap().reorg_epoch, 1);
}

#[tokio::test]
async fn redb_ingestor_capacity_gate_fails_single_hot_block_without_advancing() {
    let file = NamedTempFile::new().unwrap();
    let stream = stream();
    let mut stream_config = config(stream);
    stream_config.batch_size_blocks = 1;
    stream_config.max_logs_per_page = 1;
    let chain = runtime_chain(9)
        .push_transfer_log(chain_log(stream, 9, 0, 0x42))
        .push_transfer_log(chain_log(stream, 9, 1, 0x43));
    let ingestor = runtime_ingestor(chain, file.path());

    ingestor.ensure_stream(stream_config).await.unwrap();
    let error = ingestor.poll_once(stream).await.unwrap_err();

    assert!(matches!(error, TransferLogStoreError::NotReady { .. }));
    assert_eq!(ingestor.cursor(stream).await.unwrap().next_block, 9);
    assert!(ingestor.block_header(stream, 9).await.unwrap().is_none());
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
        target_mode: ScanTargetMode::SafeTag,
        rpc_max_retries: 3,
        log_source: LogSourceKind::RpcRange,
    }
}

fn runtime_ingestor(
    chain: FakeErc20ChainClient,
    path: &Path,
) -> RedbTransferLogIngestor<FakeErc20ChainClient> {
    RedbTransferLogIngestor::with_clock(
        chain,
        RuntimeRedbTransferLogStore::open(path).unwrap(),
        now,
    )
}

fn runtime_chain(safe_head: u64) -> FakeErc20ChainClient {
    let chain = FakeErc20ChainClient::new(1);
    for number in 9..=safe_head {
        chain.insert_block(chain_block(number, number));
    }
    chain.set_safe_head(ChainBlockRef::new(safe_head, hash(safe_head)))
}

fn chain_log(stream: StreamId, block_number: u64, log_index: u64, to_byte: u8) -> TransferLog {
    TransferLog {
        chain_id: stream.chain_id,
        token_address: stream.token_address,
        block: chain_block(block_number, block_number),
        tx_hash: tx(block_number, log_index),
        log_index,
        from_address: address(0x01),
        to_address: address(to_byte),
        amount_raw: RawAmount::from(block_number + log_index),
    }
}

fn chain_block(number: u64, hash_byte: u64) -> ChainBlock {
    ChainBlock::new(
        number,
        hash(hash_byte),
        hash(hash_byte.saturating_sub(1)),
        now() + time::Duration::seconds(number as i64),
    )
}

fn stored_from_chain_log(log: TransferLog) -> StoredTransferLog {
    StoredTransferLog {
        chain_id: log.chain_id,
        token_address: log.token_address,
        block_number: log.block.number,
        block_hash: log.block.hash,
        block_timestamp: log.block.timestamp,
        tx_hash: log.tx_hash,
        tx_index: None,
        log_index: log.log_index,
        from_address: log.from_address,
        to_address: log.to_address,
        amount_raw: log.amount_raw,
        removed: false,
        observed_at: now(),
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
