pub mod redb_store;
pub mod types;

use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use thiserror::Error;
use time::OffsetDateTime;

pub use types::*;

use crate::{
    chain::{ChainBlock, ChainError, ChainHeaderReader, TransferLogRange, TransferLogSource},
    domain::BlockHash,
};

#[derive(Debug, Error)]
pub enum TransferLogStoreError {
    #[error("stream not found: {stream:?}")]
    StreamNotFound { stream: StreamId },

    #[error(
        "stream config conflict for {stream:?}: existing start block {existing_start_block}, requested {requested_start_block}"
    )]
    StreamConfigConflict {
        stream: StreamId,
        existing_start_block: u64,
        requested_start_block: u64,
    },

    #[error("invalid limit {field}: must be greater than zero")]
    InvalidLimit { field: &'static str },

    #[error("log store is not ready: {reason}")]
    NotReady { reason: String },

    #[error("deep reorg detected for {stream:?} from block {from_block}")]
    DeepReorgDetected { stream: StreamId, from_block: u64 },

    #[error(transparent)]
    Chain(#[from] ChainError),

    #[error(transparent)]
    Type(#[from] TransferLogTypeError),
}

impl TransferLogStoreError {
    fn invalid_limit(field: &'static str) -> Self {
        Self::InvalidLimit { field }
    }
}

pub type TransferLogStoreResult<T> = Result<T, TransferLogStoreError>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PollOutcome {
    Idle {
        cursor: TransferLogCursor,
    },
    Advanced {
        stream: StreamId,
        from_block: u64,
        to_block: u64,
        log_count: usize,
        cursor: TransferLogCursor,
    },
    Rewound {
        stream: StreamId,
        from_block: u64,
        cursor: TransferLogCursor,
    },
}

#[async_trait]
pub trait TransferLogIngestor: Send + Sync {
    async fn ensure_stream(
        &self,
        config: TransferLogStreamConfig,
    ) -> TransferLogStoreResult<StreamState>;

    async fn poll_once(&self, stream: StreamId) -> TransferLogStoreResult<PollOutcome>;

    async fn rewind_to(
        &self,
        stream: StreamId,
        block: u64,
        reason: RewindReason,
    ) -> TransferLogStoreResult<()>;
}

#[async_trait]
pub trait TransferLogReader: Send + Sync {
    async fn cursor(&self, stream: StreamId) -> TransferLogStoreResult<TransferLogCursor>;

    async fn block_header(
        &self,
        stream: StreamId,
        block: u64,
    ) -> TransferLogStoreResult<Option<StoredBlockHeader>>;

    async fn logs_in_range(
        &self,
        stream: StreamId,
        from: u64,
        to: u64,
        max_logs: usize,
    ) -> TransferLogStoreResult<Vec<StoredTransferLog>>;

    async fn logs_page(
        &self,
        stream: StreamId,
        after: Option<LogPageToken>,
        limit: usize,
    ) -> TransferLogStoreResult<LogsPage>;
}

#[derive(Clone)]
pub struct InMemoryTransferLogStore<S> {
    source: S,
    state: Arc<Mutex<StoreState>>,
    now: Arc<dyn Fn() -> OffsetDateTime + Send + Sync>,
}

impl<S> InMemoryTransferLogStore<S> {
    pub fn new(source: S) -> Self {
        Self::with_clock(source, OffsetDateTime::now_utc)
    }

    pub fn with_clock(source: S, now: impl Fn() -> OffsetDateTime + Send + Sync + 'static) -> Self {
        Self {
            source,
            state: Arc::new(Mutex::new(StoreState::default())),
            now: Arc::new(now),
        }
    }

    fn now(&self) -> OffsetDateTime {
        (self.now)()
    }
}

#[derive(Clone, Debug, Default)]
struct StoreState {
    streams: HashMap<StreamId, StreamData>,
}

#[derive(Clone, Debug)]
struct StreamData {
    config: TransferLogStreamConfig,
    state: StreamState,
    cursor: TransferLogCursor,
    headers: BTreeMap<u64, StoredBlockHeader>,
    logs: BTreeMap<LogPageToken, StoredTransferLog>,
    range_manifests: Vec<RangeManifest>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RangeManifest {
    from_block: u64,
    to_block: u64,
    log_count: usize,
    writer_epoch: u64,
    completed_at: OffsetDateTime,
}

#[async_trait]
impl<S> TransferLogIngestor for InMemoryTransferLogStore<S>
where
    S: ChainHeaderReader + TransferLogSource + Send + Sync,
{
    async fn ensure_stream(
        &self,
        config: TransferLogStreamConfig,
    ) -> TransferLogStoreResult<StreamState> {
        config.validate_page_limits()?;
        let stream = config.stream_id();
        let now = self.now();
        let mut state = self
            .state
            .lock()
            .expect("transfer log store mutex poisoned");

        if let Some(existing) = state.streams.get_mut(&stream) {
            if let Some(conflict) = config.identity_conflict(&existing.config) {
                return Err(TransferLogStoreError::StreamConfigConflict {
                    stream: conflict.stream,
                    existing_start_block: conflict.existing_start_block,
                    requested_start_block: conflict.requested_start_block,
                });
            }

            existing.config = config;
            existing.state.updated_at = now;
            return Ok(existing.state.clone());
        }

        let cursor = TransferLogCursor::initial(&config, 1, now);
        let stream_state = StreamState {
            stream,
            config: config.clone(),
            cursor: cursor.clone(),
            created_at: now,
            updated_at: now,
        };
        state.streams.insert(
            stream,
            StreamData {
                config,
                state: stream_state.clone(),
                cursor,
                headers: BTreeMap::new(),
                logs: BTreeMap::new(),
                range_manifests: Vec::new(),
            },
        );

        Ok(stream_state)
    }

    async fn poll_once(&self, stream: StreamId) -> TransferLogStoreResult<PollOutcome> {
        let (config, cursor) = {
            let state = self
                .state
                .lock()
                .expect("transfer log store mutex poisoned");
            let data = state
                .streams
                .get(&stream)
                .ok_or(TransferLogStoreError::StreamNotFound { stream })?;
            (data.config.clone(), data.cursor.clone())
        };

        if let Some(outcome) = self.detect_reorg(stream, &config, &cursor).await? {
            return Ok(outcome);
        }

        let target = resolve_target(&self.source, &config).await?;
        if target < cursor.next_block {
            return Ok(PollOutcome::Idle { cursor });
        }

        let from_block = cursor.next_block;
        let to_block = from_block
            .saturating_add(config.batch_size_blocks.saturating_sub(1))
            .min(target);
        let mut headers = Vec::new();
        for number in from_block..=to_block {
            headers.push(self.source.block_by_number(number).await?);
        }

        let logs = self
            .source
            .transfer_logs(TransferLogRange::new(
                config.chain_id,
                config.token_address,
                from_block,
                to_block,
            ))
            .await?;

        let now = self.now();
        let mut state = self
            .state
            .lock()
            .expect("transfer log store mutex poisoned");
        let data = state
            .streams
            .get_mut(&stream)
            .ok_or(TransferLogStoreError::StreamNotFound { stream })?;
        let headers_by_number = headers
            .iter()
            .map(|header| (header.number, *header))
            .collect::<BTreeMap<_, _>>();

        for header in headers {
            data.headers.insert(
                header.number,
                stored_header(data.config.chain_id, data.config.token_address, header, now),
            );
        }

        let mut stored_log_count = 0usize;
        for log in logs {
            let Some(header) = headers_by_number.get(&log.block.number) else {
                continue;
            };
            if header.hash != log.block.hash {
                continue;
            }

            let stored = stored_log(log, header.timestamp, now);
            data.logs.insert(stored.page_token(), stored);
            stored_log_count += 1;
        }

        let completed_hash = headers_by_number
            .get(&to_block)
            .map(|header| header.hash)
            .unwrap_or(BlockHash::ZERO);
        data.cursor.next_block = to_block.saturating_add(1);
        data.cursor.last_completed_block = Some(to_block);
        data.cursor.last_completed_hash = Some(completed_hash);
        data.cursor.updated_at = now;
        data.range_manifests.push(RangeManifest {
            from_block,
            to_block,
            log_count: stored_log_count,
            writer_epoch: data.cursor.writer_epoch,
            completed_at: now,
        });
        data.state.cursor = data.cursor.clone();
        data.state.updated_at = now;

        Ok(PollOutcome::Advanced {
            stream,
            from_block,
            to_block,
            log_count: stored_log_count,
            cursor: data.cursor.clone(),
        })
    }

    async fn rewind_to(
        &self,
        stream: StreamId,
        block: u64,
        _reason: RewindReason,
    ) -> TransferLogStoreResult<()> {
        let now = self.now();
        let mut state = self
            .state
            .lock()
            .expect("transfer log store mutex poisoned");
        let data = state
            .streams
            .get_mut(&stream)
            .ok_or(TransferLogStoreError::StreamNotFound { stream })?;

        let rewind_to = block.max(data.config.start_block);
        apply_rewind(data, rewind_to, now);
        Ok(())
    }
}

impl<S> InMemoryTransferLogStore<S>
where
    S: ChainHeaderReader + TransferLogSource + Send + Sync,
{
    async fn detect_reorg(
        &self,
        stream: StreamId,
        config: &TransferLogStreamConfig,
        cursor: &TransferLogCursor,
    ) -> TransferLogStoreResult<Option<PollOutcome>> {
        let Some(last_completed_block) = cursor.last_completed_block else {
            return Ok(None);
        };
        let Some(last_completed_hash) = cursor.last_completed_hash else {
            return Ok(None);
        };

        let current = self.source.block_by_number(last_completed_block).await?;
        if current.hash == last_completed_hash {
            return Ok(None);
        }

        let reorg_floor = last_completed_block
            .saturating_sub(config.reorg_lookback_blocks.saturating_sub(1))
            .max(config.start_block);
        let mut first_diverged = last_completed_block;
        for number in reorg_floor..=last_completed_block {
            let current = self.source.block_by_number(number).await?;
            let stored_hash = {
                let state = self
                    .state
                    .lock()
                    .expect("transfer log store mutex poisoned");
                state
                    .streams
                    .get(&stream)
                    .and_then(|stream| stream.headers.get(&number))
                    .map(|header| header.block_hash)
            };
            if stored_hash != Some(current.hash) {
                first_diverged = number;
                break;
            }
        }

        let now = self.now();
        let mut state = self
            .state
            .lock()
            .expect("transfer log store mutex poisoned");
        let data = state
            .streams
            .get_mut(&stream)
            .ok_or(TransferLogStoreError::StreamNotFound { stream })?;
        apply_rewind(data, first_diverged, now);
        Ok(Some(PollOutcome::Rewound {
            stream,
            from_block: first_diverged,
            cursor: data.cursor.clone(),
        }))
    }
}

#[async_trait]
impl<S> TransferLogReader for InMemoryTransferLogStore<S>
where
    S: Send + Sync,
{
    async fn cursor(&self, stream: StreamId) -> TransferLogStoreResult<TransferLogCursor> {
        let state = self
            .state
            .lock()
            .expect("transfer log store mutex poisoned");
        Ok(state
            .streams
            .get(&stream)
            .ok_or(TransferLogStoreError::StreamNotFound { stream })?
            .cursor
            .clone())
    }

    async fn block_header(
        &self,
        stream: StreamId,
        block: u64,
    ) -> TransferLogStoreResult<Option<StoredBlockHeader>> {
        let state = self
            .state
            .lock()
            .expect("transfer log store mutex poisoned");
        Ok(state
            .streams
            .get(&stream)
            .ok_or(TransferLogStoreError::StreamNotFound { stream })?
            .headers
            .get(&block)
            .cloned())
    }

    async fn logs_in_range(
        &self,
        stream: StreamId,
        from: u64,
        to: u64,
        max_logs: usize,
    ) -> TransferLogStoreResult<Vec<StoredTransferLog>> {
        if max_logs == 0 {
            return Err(TransferLogStoreError::invalid_limit("max_logs"));
        }

        let state = self
            .state
            .lock()
            .expect("transfer log store mutex poisoned");
        let data = state
            .streams
            .get(&stream)
            .ok_or(TransferLogStoreError::StreamNotFound { stream })?;
        Ok(data
            .logs
            .values()
            .filter(|log| log.block_number >= from && log.block_number <= to)
            .take(max_logs)
            .cloned()
            .collect())
    }

    async fn logs_page(
        &self,
        stream: StreamId,
        after: Option<LogPageToken>,
        limit: usize,
    ) -> TransferLogStoreResult<LogsPage> {
        if limit == 0 {
            return Err(TransferLogStoreError::invalid_limit("limit"));
        }

        let state = self
            .state
            .lock()
            .expect("transfer log store mutex poisoned");
        let data = state
            .streams
            .get(&stream)
            .ok_or(TransferLogStoreError::StreamNotFound { stream })?;
        let mut logs = Vec::new();
        for (token, log) in &data.logs {
            if after.is_some_and(|after| *token <= after) {
                continue;
            }
            if logs.len() == limit {
                break;
            }
            logs.push(log.clone());
        }

        let next_token = logs.last().map(StoredTransferLog::page_token);
        let has_more =
            next_token.is_some_and(|token| data.logs.keys().any(|candidate| *candidate > token));
        let complete_to_block = complete_to_block(&logs, data, has_more);

        Ok(LogsPage {
            stream,
            logs,
            next_token,
            complete_to_block,
        })
    }
}

async fn resolve_target<S>(
    source: &S,
    config: &TransferLogStreamConfig,
) -> TransferLogStoreResult<u64>
where
    S: ChainHeaderReader + Send + Sync,
{
    let head = match config.target_mode {
        ScanTargetMode::SafeTag => source.safe_head().await?,
        ScanTargetMode::FinalizedTag => source.finalized_head().await?,
        ScanTargetMode::LatestMinusConfirmations(confirmations) => {
            let latest = source.latest_head().await?;
            if latest.number < confirmations {
                return Ok(0);
            }
            return Ok(latest.number - confirmations);
        }
    };
    Ok(head.number)
}

fn stored_header(
    chain_id: u64,
    token_address: crate::domain::EvmAddress,
    block: ChainBlock,
    scanned_at: OffsetDateTime,
) -> StoredBlockHeader {
    StoredBlockHeader {
        chain_id,
        token_address,
        block_number: block.number,
        block_hash: block.hash,
        parent_hash: block.parent_hash,
        timestamp: block.timestamp,
        scanned_at,
    }
}

fn stored_log(
    log: crate::chain::TransferLog,
    block_timestamp: OffsetDateTime,
    observed_at: OffsetDateTime,
) -> StoredTransferLog {
    StoredTransferLog {
        chain_id: log.chain_id,
        token_address: log.token_address,
        block_number: log.block.number,
        block_hash: log.block.hash,
        block_timestamp,
        tx_hash: log.tx_hash,
        tx_index: None,
        log_index: log.log_index,
        from_address: log.from_address,
        to_address: log.to_address,
        amount_raw: log.amount_raw,
        removed: false,
        observed_at,
    }
}

fn apply_rewind(data: &mut StreamData, from_block: u64, now: OffsetDateTime) {
    data.headers.retain(|block, _| *block < from_block);
    data.logs.retain(|_, log| log.block_number < from_block);
    data.range_manifests
        .retain(|range| range.to_block < from_block);
    data.cursor
        .record_rewind(from_block, data.cursor.writer_epoch, now);
    data.state.cursor = data.cursor.clone();
    data.state.updated_at = now;
}

fn complete_to_block(logs: &[StoredTransferLog], data: &StreamData, has_more: bool) -> Option<u64> {
    let last = logs.last()?;
    if !has_more {
        return Some(last.block_number);
    }

    let last_token = last.page_token();
    let next = data
        .logs
        .range((
            std::ops::Bound::Excluded(last_token),
            std::ops::Bound::Unbounded,
        ))
        .next();
    match next {
        Some((_, next_log)) if next_log.block_number == last.block_number => {
            last.block_number.checked_sub(1)
        }
        _ => Some(last.block_number),
    }
}

#[cfg(test)]
mod tests {
    use time::macros::datetime;

    use super::*;
    use crate::{
        chain::{FakeErc20ChainClient, TransferLog},
        domain::{EvmAddress, RawAmount, TxHash},
    };

    #[tokio::test]
    async fn ensure_stream_initializes_cursor_at_start_block() {
        let store = store(chain());
        let state = store.ensure_stream(config(2, 10)).await.unwrap();

        assert_eq!(state.stream, stream());
        assert_eq!(state.cursor.start_block, 2);
        assert_eq!(state.cursor.next_block, 2);
        assert_eq!(state.cursor.last_completed_block, None);
    }

    #[tokio::test]
    async fn poll_once_scans_from_cursor_and_stores_empty_block_headers() {
        let chain = chain().push_transfer_log(log(2, 0, 0x20));
        let store = store(chain);
        store.ensure_stream(config(2, 2)).await.unwrap();

        let outcome = store.poll_once(stream()).await.unwrap();
        let PollOutcome::Advanced {
            from_block,
            to_block,
            log_count,
            cursor,
            ..
        } = outcome
        else {
            panic!("expected advanced poll outcome");
        };

        assert_eq!(from_block, 2);
        assert_eq!(to_block, 3);
        assert_eq!(log_count, 1);
        assert_eq!(cursor.next_block, 4);
        assert_eq!(cursor.last_completed_block, Some(3));
        assert!(store.block_header(stream(), 2).await.unwrap().is_some());
        assert!(
            store.block_header(stream(), 3).await.unwrap().is_some(),
            "empty log blocks must still store headers"
        );
    }

    #[tokio::test]
    async fn logs_page_uses_exclusive_token_and_complete_block_floor() {
        let chain = chain()
            .push_transfer_log(log(2, 0, 0x20))
            .push_transfer_log(log(2, 1, 0x21))
            .push_transfer_log(log(3, 0, 0x22));
        let store = store(chain);
        store.ensure_stream(config(2, 2)).await.unwrap();
        store.poll_once(stream()).await.unwrap();

        let first = store.logs_page(stream(), None, 1).await.unwrap();
        assert_eq!(first.logs.len(), 1);
        assert_eq!(first.next_token, Some(LogPageToken::new(2, 0)));
        assert_eq!(
            first.complete_to_block,
            Some(1),
            "limit cut within block 2, so block 2 is not complete"
        );

        let second = store
            .logs_page(stream(), first.next_token, 1)
            .await
            .unwrap();
        assert_eq!(second.logs[0].page_token(), LogPageToken::new(2, 1));
        assert_eq!(second.complete_to_block, Some(2));
    }

    #[tokio::test]
    async fn logs_in_range_requires_nonzero_limit() {
        let store = store(chain());
        store.ensure_stream(config(2, 2)).await.unwrap();

        assert!(matches!(
            store.logs_in_range(stream(), 2, 3, 0).await,
            Err(TransferLogStoreError::InvalidLimit { field: "max_logs" })
        ));
    }

    #[tokio::test]
    async fn start_block_conflict_is_reported() {
        let store = store(chain());
        store.ensure_stream(config(2, 2)).await.unwrap();

        assert!(matches!(
            store.ensure_stream(config(3, 2)).await,
            Err(TransferLogStoreError::StreamConfigConflict {
                existing_start_block: 2,
                requested_start_block: 3,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn reorg_detection_rewinds_and_next_poll_rescans_new_branch() {
        let chain = chain().push_transfer_log(log(2, 0, 0x20));
        let store = store(chain.clone());
        store.ensure_stream(config(2, 1)).await.unwrap();
        store.poll_once(stream()).await.unwrap();

        let new_block = block(2, 0xaa);
        let new_log = TransferLog {
            block: new_block,
            ..log(2, 0, 0x99)
        };
        chain
            .replace_block_for_reorg(new_block)
            .push_transfer_log(new_log);

        let rewound = store.poll_once(stream()).await.unwrap();
        let PollOutcome::Rewound {
            from_block, cursor, ..
        } = rewound
        else {
            panic!("expected reorg rewind");
        };
        assert_eq!(from_block, 2);
        assert_eq!(cursor.reorg_epoch, 1);
        assert_eq!(cursor.next_block, 2);

        store.poll_once(stream()).await.unwrap();
        let logs = store.logs_in_range(stream(), 2, 2, 10).await.unwrap();
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].to_address, address(0x99));
        assert_eq!(logs[0].block_hash, block_hash(0xaa));
    }

    fn store(chain: FakeErc20ChainClient) -> InMemoryTransferLogStore<FakeErc20ChainClient> {
        InMemoryTransferLogStore::with_clock(chain, || datetime!(2026-05-03 12:00 UTC))
    }

    fn chain() -> FakeErc20ChainClient {
        let chain = FakeErc20ChainClient::new(1);
        chain
            .insert_block(block(1, 1))
            .insert_block(block(2, 2))
            .insert_block(block(3, 3))
            .set_safe_head(crate::domain::ChainBlockRef::new(3, block_hash(3)))
    }

    fn config(start_block: u64, batch_size_blocks: u64) -> TransferLogStreamConfig {
        TransferLogStreamConfig {
            chain_id: 1,
            token_address: token(),
            start_block,
            poll_interval_ms: 1_000,
            batch_size_blocks,
            max_batch_size_blocks: 100,
            max_logs_per_page: 100,
            max_unique_to_addresses_per_batch: 100,
            max_db_fallback_addresses: 100,
            capacity_probe_blocks: 10,
            reorg_lookback_blocks: 8,
            target_mode: ScanTargetMode::SafeTag,
            rpc_max_retries: 3,
            log_source: LogSourceKind::RpcRange,
        }
    }

    fn stream() -> StreamId {
        StreamId::new(1, token())
    }

    fn log(block_number: u64, log_index: u64, to_byte: u8) -> TransferLog {
        TransferLog {
            chain_id: 1,
            token_address: token(),
            block: block(block_number, block_number as u8),
            tx_hash: tx_hash(block_number * 100 + log_index),
            log_index,
            from_address: address(0xf0),
            to_address: address(to_byte),
            amount_raw: RawAmount::from(1000 + log_index),
        }
    }

    fn block(number: u64, hash_byte: u8) -> ChainBlock {
        ChainBlock::new(
            number,
            block_hash(hash_byte),
            block_hash(hash_byte.saturating_sub(1)),
            datetime!(2026-05-03 00:00 UTC) + time::Duration::seconds(number as i64),
        )
    }

    fn token() -> EvmAddress {
        address(0x11)
    }

    fn address(byte: u8) -> EvmAddress {
        EvmAddress::from_bytes([byte; 20])
    }

    fn tx_hash(value: u64) -> TxHash {
        let mut bytes = [0u8; 32];
        bytes[24..].copy_from_slice(&value.to_be_bytes());
        TxHash::from_bytes(bytes)
    }

    fn block_hash(byte: u8) -> BlockHash {
        BlockHash::from_bytes([byte; 32])
    }
}
