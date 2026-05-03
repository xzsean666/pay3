use std::cmp::Ordering;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::OffsetDateTime;

use crate::domain::{BlockHash, EvmAddress, RawAmount, TxHash};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct StreamId {
    pub chain_id: u64,
    pub token_address: EvmAddress,
}

impl StreamId {
    pub const fn new(chain_id: u64, token_address: EvmAddress) -> Self {
        Self {
            chain_id,
            token_address,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferLogStreamConfig {
    pub chain_id: u64,
    pub token_address: EvmAddress,
    pub start_block: u64,
    pub poll_interval_ms: u64,
    pub batch_size_blocks: u64,
    pub max_batch_size_blocks: u64,
    pub max_logs_per_page: usize,
    pub max_unique_to_addresses_per_batch: usize,
    pub max_db_fallback_addresses: usize,
    pub capacity_probe_blocks: u64,
    pub reorg_lookback_blocks: u64,
    pub target_mode: ScanTargetMode,
    pub rpc_max_retries: u32,
    pub log_source: LogSourceKind,
}

impl TransferLogStreamConfig {
    pub const fn stream_id(&self) -> StreamId {
        StreamId::new(self.chain_id, self.token_address)
    }

    pub fn identity_conflict(
        &self,
        existing: &Self,
    ) -> Option<TransferLogStreamConfigIdentityConflict> {
        if self.stream_id() == existing.stream_id() && self.start_block != existing.start_block {
            Some(TransferLogStreamConfigIdentityConflict {
                stream: self.stream_id(),
                existing_start_block: existing.start_block,
                requested_start_block: self.start_block,
            })
        } else {
            None
        }
    }

    pub fn validate_page_limits(&self) -> Result<(), TransferLogTypeError> {
        ensure_nonzero_log_limit("max_logs_per_page", self.max_logs_per_page)?;
        ensure_nonzero_log_limit(
            "max_unique_to_addresses_per_batch",
            self.max_unique_to_addresses_per_batch,
        )?;
        ensure_nonzero_log_limit("max_db_fallback_addresses", self.max_db_fallback_addresses)?;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferLogStreamConfigIdentityConflict {
    pub stream: StreamId,
    pub existing_start_block: u64,
    pub requested_start_block: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanTargetMode {
    SafeTag,
    FinalizedTag,
    LatestMinusConfirmations(u64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogSourceKind {
    RpcRange,
    Indexer,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferLogCursor {
    pub stream: StreamId,
    pub start_block: u64,
    pub next_block: u64,
    pub last_completed_block: Option<u64>,
    pub last_completed_hash: Option<BlockHash>,
    pub target_mode: ScanTargetMode,
    pub reorg_epoch: u64,
    pub last_reorg_from: Option<u64>,
    pub last_reorg_at: Option<OffsetDateTime>,
    pub writer_epoch: u64,
    pub updated_at: OffsetDateTime,
}

impl TransferLogCursor {
    pub fn initial(
        config: &TransferLogStreamConfig,
        writer_epoch: u64,
        now: OffsetDateTime,
    ) -> Self {
        Self {
            stream: config.stream_id(),
            start_block: config.start_block,
            next_block: config.start_block,
            last_completed_block: None,
            last_completed_hash: None,
            target_mode: config.target_mode,
            reorg_epoch: 0,
            last_reorg_from: None,
            last_reorg_at: None,
            writer_epoch,
            updated_at: now,
        }
    }

    pub fn record_rewind(&mut self, rewind_to_block: u64, writer_epoch: u64, now: OffsetDateTime) {
        self.reorg_epoch = self.reorg_epoch.saturating_add(1);
        self.last_reorg_from = Some(rewind_to_block);
        self.last_reorg_at = Some(now);
        self.next_block = rewind_to_block;
        self.last_completed_block = rewind_to_block.checked_sub(1);
        self.last_completed_hash = None;
        self.writer_epoch = writer_epoch;
        self.updated_at = now;
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredTransferLog {
    pub chain_id: u64,
    pub token_address: EvmAddress,
    pub block_number: u64,
    pub block_hash: BlockHash,
    pub block_timestamp: OffsetDateTime,
    pub tx_hash: TxHash,
    pub tx_index: Option<u64>,
    pub log_index: u64,
    pub from_address: EvmAddress,
    pub to_address: EvmAddress,
    pub amount_raw: RawAmount,
    pub removed: bool,
    pub observed_at: OffsetDateTime,
}

impl StoredTransferLog {
    pub const fn stream_id(&self) -> StreamId {
        StreamId::new(self.chain_id, self.token_address)
    }

    pub const fn page_token(&self) -> LogPageToken {
        LogPageToken::new(self.block_number, self.log_index)
    }

    pub fn cmp_position(&self, other: &Self) -> Ordering {
        self.page_token().cmp(&other.page_token())
    }

    pub fn is_after_token(&self, token: LogPageToken) -> bool {
        self.page_token() > token
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredBlockHeader {
    pub chain_id: u64,
    pub token_address: EvmAddress,
    pub block_number: u64,
    pub block_hash: BlockHash,
    pub parent_hash: BlockHash,
    pub timestamp: OffsetDateTime,
    pub scanned_at: OffsetDateTime,
}

impl StoredBlockHeader {
    pub const fn stream_id(&self) -> StreamId {
        StreamId::new(self.chain_id, self.token_address)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct LogPageToken {
    pub block_number: u64,
    pub log_index: u64,
}

impl LogPageToken {
    pub const fn new(block_number: u64, log_index: u64) -> Self {
        Self {
            block_number,
            log_index,
        }
    }

    pub const fn from_log(log: &StoredTransferLog) -> Self {
        log.page_token()
    }

    pub fn includes_log_exclusively(self, log: &StoredTransferLog) -> bool {
        log.is_after_token(self)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogsPage {
    pub stream: StreamId,
    pub logs: Vec<StoredTransferLog>,
    pub next_token: Option<LogPageToken>,
    pub complete_to_block: Option<u64>,
}

impl LogsPage {
    pub fn new(
        stream: StreamId,
        logs: Vec<StoredTransferLog>,
        next_token: Option<LogPageToken>,
        complete_to_block: Option<u64>,
    ) -> Self {
        Self {
            stream,
            logs,
            next_token,
            complete_to_block,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RewindReason {
    Reorg,
    DeepReorg,
    Manual,
    Reset,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamState {
    pub stream: StreamId,
    pub config: TransferLogStreamConfig,
    pub cursor: TransferLogCursor,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum TransferLogTypeError {
    #[error("{field} must be nonzero")]
    ZeroLimit { field: &'static str },
}

pub fn validate_logs_in_range_limit(max_logs: usize) -> Result<(), TransferLogTypeError> {
    ensure_nonzero_log_limit("max_logs", max_logs)
}

pub fn validate_logs_page_limit(limit: usize) -> Result<(), TransferLogTypeError> {
    ensure_nonzero_log_limit("limit", limit)
}

fn ensure_nonzero_log_limit(field: &'static str, limit: usize) -> Result<(), TransferLogTypeError> {
    if limit == 0 {
        Err(TransferLogTypeError::ZeroLimit { field })
    } else {
        Ok(())
    }
}
