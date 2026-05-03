pub mod rpc;

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::OffsetDateTime;

use crate::{
    domain::{BlockHash, ChainBlockRef, EvmAddress, RawAmount, TxHash},
    services::orders::{OrderChainHeadReader, OrderServiceError},
};

pub use rpc::{
    ERC20_TRANSFER_TOPIC, HttpJsonRpcProvider, JsonRpcProvider, RpcProviderChainStatus,
    RpcProviderManager, RpcProviderReadiness, RpcRangeSource, SharedJsonRpcProvider,
};

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ChainError {
    #[error("chain RPC unavailable: {message}")]
    RpcUnavailable { message: String },

    #[error("malformed chain RPC response: {message}")]
    MalformedRpcResponse { message: String },

    #[error(
        "RPC provider hash mismatch for {context}: {left_provider} returned {left_hash}, {right_provider} returned {right_hash}"
    )]
    ProviderHashMismatch {
        context: String,
        left_provider: String,
        left_hash: BlockHash,
        right_provider: String,
        right_hash: BlockHash,
    },

    #[error("invalid block range {from_block}..={to_block}")]
    InvalidBlockRange { from_block: u64, to_block: u64 },

    #[error("block not found: {number}")]
    BlockNotFound { number: u64 },

    #[error("transaction not found: {tx_hash}")]
    TransactionNotFound { tx_hash: TxHash },

    #[error("configured chain id {expected} does not match provider chain id {actual}")]
    ChainIdMismatch { expected: u64, actual: u64 },

    #[error("transfer log source capacity exceeded: {log_count} logs, max {max_logs}")]
    CapacityExceeded { log_count: usize, max_logs: usize },
}

impl ChainError {
    pub fn rpc_unavailable(message: impl Into<String>) -> Self {
        Self::RpcUnavailable {
            message: message.into(),
        }
    }

    pub fn malformed_rpc_response(message: impl Into<String>) -> Self {
        Self::MalformedRpcResponse {
            message: message.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainBlock {
    pub number: u64,
    pub hash: BlockHash,
    pub parent_hash: BlockHash,
    pub timestamp: OffsetDateTime,
}

impl ChainBlock {
    pub const fn new(
        number: u64,
        hash: BlockHash,
        parent_hash: BlockHash,
        timestamp: OffsetDateTime,
    ) -> Self {
        Self {
            number,
            hash,
            parent_hash,
            timestamp,
        }
    }

    pub const fn block_ref(self) -> ChainBlockRef {
        ChainBlockRef::new(self.number, self.hash)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferLog {
    pub chain_id: u64,
    pub token_address: EvmAddress,
    pub block: ChainBlock,
    pub tx_hash: TxHash,
    pub log_index: u64,
    pub from_address: EvmAddress,
    pub to_address: EvmAddress,
    pub amount_raw: RawAmount,
}

impl TransferLog {
    pub fn position(&self) -> TransferLogPosition {
        TransferLogPosition {
            block_number: self.block.number,
            log_index: self.log_index,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TransferLogPosition {
    pub block_number: u64,
    pub log_index: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransactionStatus {
    Success,
    Reverted,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TxReceipt {
    pub tx_hash: TxHash,
    pub block: ChainBlockRef,
    pub status: TransactionStatus,
    pub gas_used: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferLogRange {
    pub chain_id: u64,
    pub token_address: EvmAddress,
    pub from_block: u64,
    pub to_block: u64,
}

impl TransferLogRange {
    pub const fn new(
        chain_id: u64,
        token_address: EvmAddress,
        from_block: u64,
        to_block: u64,
    ) -> Self {
        Self {
            chain_id,
            token_address,
            from_block,
            to_block,
        }
    }

    pub fn validate(self) -> Result<(), ChainError> {
        if self.from_block > self.to_block {
            return Err(ChainError::InvalidBlockRange {
                from_block: self.from_block,
                to_block: self.to_block,
            });
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferLogCapacityLimits {
    pub max_logs: usize,
    pub max_logs_per_block: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferLogCapacityReport {
    pub range: TransferLogRange,
    pub log_count: usize,
    pub max_logs_in_single_block: usize,
    pub limits: TransferLogCapacityLimits,
}

impl TransferLogCapacityReport {
    pub fn is_within_limits(&self) -> bool {
        self.log_count <= self.limits.max_logs
            && self.max_logs_in_single_block <= self.limits.max_logs_per_block
    }

    pub fn ensure_within_limits(&self) -> Result<(), ChainError> {
        if self.is_within_limits() {
            Ok(())
        } else {
            Err(ChainError::CapacityExceeded {
                log_count: self.log_count,
                max_logs: self.limits.max_logs,
            })
        }
    }
}

#[async_trait]
pub trait ChainHeaderReader: Send + Sync {
    async fn latest_head(&self) -> Result<ChainBlockRef, ChainError>;
    async fn safe_head(&self) -> Result<ChainBlockRef, ChainError>;
    async fn finalized_head(&self) -> Result<ChainBlockRef, ChainError>;
    async fn block_by_number(&self, number: u64) -> Result<ChainBlock, ChainError>;
}

#[async_trait]
impl<T> OrderChainHeadReader for T
where
    T: ChainHeaderReader,
{
    async fn current_head(&self) -> Result<ChainBlockRef, OrderServiceError> {
        self.latest_head()
            .await
            .map_err(|error| OrderServiceError::chain_head_unavailable(error.to_string()))
    }
}

#[async_trait]
pub trait TransferLogSource: Send + Sync {
    async fn transfer_logs(&self, range: TransferLogRange) -> Result<Vec<TransferLog>, ChainError>;

    async fn capacity_probe(
        &self,
        range: TransferLogRange,
        limits: TransferLogCapacityLimits,
    ) -> Result<TransferLogCapacityReport, ChainError>;
}

#[async_trait]
pub trait Erc20ChainClient: ChainHeaderReader + TransferLogSource {
    async fn token_balance(
        &self,
        token: EvmAddress,
        owner: EvmAddress,
    ) -> Result<RawAmount, ChainError>;

    async fn transaction_receipt(&self, tx: TxHash) -> Result<Option<TxReceipt>, ChainError>;

    async fn broadcast_signed_tx(&self, signed_tx: Vec<u8>) -> Result<TxHash, ChainError>;
}

#[derive(Clone, Debug)]
pub struct FakeErc20ChainClient {
    state: Arc<Mutex<FakeChainState>>,
}

impl FakeErc20ChainClient {
    pub fn new(chain_id: u64) -> Self {
        Self {
            state: Arc::new(Mutex::new(FakeChainState {
                chain_id,
                ..FakeChainState::default()
            })),
        }
    }

    pub fn insert_block(&self, block: ChainBlock) -> Self {
        let mut state = self.state.lock().expect("fake chain mutex poisoned");
        state.head = Some(match state.head {
            Some(existing) if existing.number > block.number => existing,
            _ => block.block_ref(),
        });
        state.blocks.insert(block.number, block);
        drop(state);
        self.clone()
    }

    pub fn set_latest_head(&self, head: ChainBlockRef) -> Self {
        self.state.lock().expect("fake chain mutex poisoned").head = Some(head);
        self.clone()
    }

    pub fn set_safe_head(&self, head: ChainBlockRef) -> Self {
        self.state
            .lock()
            .expect("fake chain mutex poisoned")
            .safe_head = Some(head);
        self.clone()
    }

    pub fn set_finalized_head(&self, head: ChainBlockRef) -> Self {
        self.state
            .lock()
            .expect("fake chain mutex poisoned")
            .finalized_head = Some(head);
        self.clone()
    }

    pub fn push_transfer_log(&self, log: TransferLog) -> Self {
        self.state
            .lock()
            .expect("fake chain mutex poisoned")
            .logs
            .push(log);
        self.clone()
    }

    pub fn set_balance(&self, token: EvmAddress, owner: EvmAddress, balance: RawAmount) -> Self {
        self.state
            .lock()
            .expect("fake chain mutex poisoned")
            .balances
            .insert((token, owner), balance);
        self.clone()
    }

    pub fn set_receipt(&self, receipt: TxReceipt) -> Self {
        self.state
            .lock()
            .expect("fake chain mutex poisoned")
            .receipts
            .insert(receipt.tx_hash, receipt);
        self.clone()
    }

    pub fn fail_next(&self, message: impl Into<String>) -> Self {
        self.state
            .lock()
            .expect("fake chain mutex poisoned")
            .next_error = Some(message.into());
        self.clone()
    }

    pub fn replace_block_for_reorg(&self, block: ChainBlock) -> Self {
        let mut state = self.state.lock().expect("fake chain mutex poisoned");
        state.blocks.insert(block.number, block);
        state
            .logs
            .retain(|log| log.block.number != block.number || log.block.hash == block.hash);
        state.head = Some(match state.head {
            Some(existing) if existing.number > block.number => existing,
            _ => block.block_ref(),
        });
        drop(state);
        self.clone()
    }

    pub fn calls(&self) -> Vec<FakeChainCall> {
        self.state
            .lock()
            .expect("fake chain mutex poisoned")
            .calls
            .clone()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FakeChainCall {
    LatestHead,
    SafeHead,
    FinalizedHead,
    BlockByNumber(u64),
    TransferLogs(TransferLogRange),
    CapacityProbe(TransferLogRange),
    TokenBalance {
        token: EvmAddress,
        owner: EvmAddress,
    },
    TransactionReceipt(TxHash),
    BroadcastSignedTx(usize),
}

#[derive(Clone, Debug, Default)]
struct FakeChainState {
    chain_id: u64,
    blocks: BTreeMap<u64, ChainBlock>,
    logs: Vec<TransferLog>,
    head: Option<ChainBlockRef>,
    safe_head: Option<ChainBlockRef>,
    finalized_head: Option<ChainBlockRef>,
    balances: BTreeMap<(EvmAddress, EvmAddress), RawAmount>,
    receipts: BTreeMap<TxHash, TxReceipt>,
    next_error: Option<String>,
    broadcast_counter: u64,
    calls: Vec<FakeChainCall>,
}

impl FakeChainState {
    fn take_error(&mut self) -> Result<(), ChainError> {
        if let Some(message) = self.next_error.take() {
            Err(ChainError::rpc_unavailable(message))
        } else {
            Ok(())
        }
    }
}

#[async_trait]
impl ChainHeaderReader for FakeErc20ChainClient {
    async fn latest_head(&self) -> Result<ChainBlockRef, ChainError> {
        let mut state = self.state.lock().expect("fake chain mutex poisoned");
        state.calls.push(FakeChainCall::LatestHead);
        state.take_error()?;
        state
            .head
            .ok_or(ChainError::BlockNotFound { number: u64::MAX })
    }

    async fn safe_head(&self) -> Result<ChainBlockRef, ChainError> {
        let mut state = self.state.lock().expect("fake chain mutex poisoned");
        state.calls.push(FakeChainCall::SafeHead);
        state.take_error()?;
        state
            .safe_head
            .or(state.head)
            .ok_or(ChainError::BlockNotFound { number: u64::MAX })
    }

    async fn finalized_head(&self) -> Result<ChainBlockRef, ChainError> {
        let mut state = self.state.lock().expect("fake chain mutex poisoned");
        state.calls.push(FakeChainCall::FinalizedHead);
        state.take_error()?;
        state
            .finalized_head
            .or(state.safe_head)
            .or(state.head)
            .ok_or(ChainError::BlockNotFound { number: u64::MAX })
    }

    async fn block_by_number(&self, number: u64) -> Result<ChainBlock, ChainError> {
        let mut state = self.state.lock().expect("fake chain mutex poisoned");
        state.calls.push(FakeChainCall::BlockByNumber(number));
        state.take_error()?;
        state
            .blocks
            .get(&number)
            .copied()
            .ok_or(ChainError::BlockNotFound { number })
    }
}

#[async_trait]
impl TransferLogSource for FakeErc20ChainClient {
    async fn transfer_logs(&self, range: TransferLogRange) -> Result<Vec<TransferLog>, ChainError> {
        range.validate()?;
        let mut state = self.state.lock().expect("fake chain mutex poisoned");
        state.calls.push(FakeChainCall::TransferLogs(range));
        state.take_error()?;
        if range.chain_id != state.chain_id {
            return Err(ChainError::ChainIdMismatch {
                expected: range.chain_id,
                actual: state.chain_id,
            });
        }

        let canonical_blocks = state
            .blocks
            .iter()
            .filter(|(number, _)| **number >= range.from_block && **number <= range.to_block)
            .map(|(number, block)| (*number, block.hash))
            .collect::<BTreeMap<_, _>>();
        let mut logs = state
            .logs
            .iter()
            .filter(|log| {
                log.chain_id == range.chain_id
                    && log.token_address == range.token_address
                    && log.block.number >= range.from_block
                    && log.block.number <= range.to_block
                    && canonical_blocks.get(&log.block.number) == Some(&log.block.hash)
            })
            .cloned()
            .collect::<Vec<_>>();
        logs.sort_by_key(TransferLog::position);
        Ok(logs)
    }

    async fn capacity_probe(
        &self,
        range: TransferLogRange,
        limits: TransferLogCapacityLimits,
    ) -> Result<TransferLogCapacityReport, ChainError> {
        {
            let mut state = self.state.lock().expect("fake chain mutex poisoned");
            state.calls.push(FakeChainCall::CapacityProbe(range));
        }

        let logs = self.transfer_logs(range).await?;
        let mut block_counts = BTreeMap::<u64, usize>::new();
        for log in &logs {
            *block_counts.entry(log.block.number).or_default() += 1;
        }

        Ok(TransferLogCapacityReport {
            range,
            log_count: logs.len(),
            max_logs_in_single_block: block_counts.values().copied().max().unwrap_or_default(),
            limits,
        })
    }
}

#[async_trait]
impl Erc20ChainClient for FakeErc20ChainClient {
    async fn token_balance(
        &self,
        token: EvmAddress,
        owner: EvmAddress,
    ) -> Result<RawAmount, ChainError> {
        let mut state = self.state.lock().expect("fake chain mutex poisoned");
        state
            .calls
            .push(FakeChainCall::TokenBalance { token, owner });
        state.take_error()?;
        Ok(*state
            .balances
            .get(&(token, owner))
            .unwrap_or(&RawAmount::ZERO))
    }

    async fn transaction_receipt(&self, tx: TxHash) -> Result<Option<TxReceipt>, ChainError> {
        let mut state = self.state.lock().expect("fake chain mutex poisoned");
        state.calls.push(FakeChainCall::TransactionReceipt(tx));
        state.take_error()?;
        Ok(state.receipts.get(&tx).cloned())
    }

    async fn broadcast_signed_tx(&self, signed_tx: Vec<u8>) -> Result<TxHash, ChainError> {
        let mut state = self.state.lock().expect("fake chain mutex poisoned");
        state
            .calls
            .push(FakeChainCall::BroadcastSignedTx(signed_tx.len()));
        state.take_error()?;
        state.broadcast_counter += 1;

        let mut bytes = [0u8; 32];
        bytes[..8].copy_from_slice(&state.broadcast_counter.to_be_bytes());
        Ok(TxHash::from_bytes(bytes))
    }
}

pub fn transfer_log_addresses(logs: &[TransferLog]) -> BTreeSet<EvmAddress> {
    logs.iter().map(|log| log.to_address).collect()
}

#[cfg(test)]
mod tests {
    use time::macros::datetime;

    use super::*;
    use crate::services::orders::OrderChainHeadReader;

    #[tokio::test]
    async fn fake_transfer_logs_are_range_filtered_token_filtered_and_sorted() {
        let token = address(0x11);
        let other_token = address(0x22);
        let client = fake_client();
        client
            .push_transfer_log(log(3, token, 1, 30, 30))
            .push_transfer_log(log(2, token, 1, 20, 20))
            .push_transfer_log(log(2, other_token, 1, 21, 21))
            .push_transfer_log(log(1, token, 1, 10, 10));

        let logs = client
            .transfer_logs(TransferLogRange::new(1, token, 2, 3))
            .await
            .unwrap();

        assert_eq!(
            logs.iter().map(TransferLog::position).collect::<Vec<_>>(),
            vec![
                TransferLogPosition {
                    block_number: 2,
                    log_index: 20
                },
                TransferLogPosition {
                    block_number: 3,
                    log_index: 30
                }
            ]
        );
        assert!(logs.iter().all(|log| log.token_address == token));
    }

    #[tokio::test]
    async fn invalid_range_and_chain_id_mismatch_are_reported() {
        let client = fake_client();

        assert!(matches!(
            client
                .transfer_logs(TransferLogRange::new(1, address(0x11), 3, 2))
                .await,
            Err(ChainError::InvalidBlockRange {
                from_block: 3,
                to_block: 2
            })
        ));
        assert!(matches!(
            client
                .transfer_logs(TransferLogRange::new(999, address(0x11), 1, 2))
                .await,
            Err(ChainError::ChainIdMismatch {
                expected: 999,
                actual: 1
            })
        ));
    }

    #[tokio::test]
    async fn fake_client_can_inject_rpc_failure() {
        let client = fake_client().fail_next("timeout");

        assert!(matches!(
            client.latest_head().await,
            Err(ChainError::RpcUnavailable { .. })
        ));
        assert!(client.latest_head().await.is_ok());
    }

    #[tokio::test]
    async fn reorg_replaces_canonical_block_and_hides_old_logs() {
        let token = address(0x11);
        let client = fake_client();
        let old_log = log(2, token, 1, 0, 10);
        client.push_transfer_log(old_log.clone());

        assert_eq!(
            client
                .transfer_logs(TransferLogRange::new(1, token, 2, 2))
                .await
                .unwrap(),
            vec![old_log]
        );

        let new_block = block(2, 0xaa);
        let new_log = TransferLog {
            block: new_block,
            ..log(2, token, 1, 0, 11)
        };
        client
            .replace_block_for_reorg(new_block)
            .push_transfer_log(new_log.clone());

        assert_eq!(
            client
                .transfer_logs(TransferLogRange::new(1, token, 2, 2))
                .await
                .unwrap(),
            vec![new_log]
        );
    }

    #[tokio::test]
    async fn capacity_probe_reports_limits_without_advancing_state() {
        let token = address(0x11);
        let client = fake_client();
        client
            .push_transfer_log(log(2, token, 1, 0, 10))
            .push_transfer_log(log(2, token, 1, 1, 11))
            .push_transfer_log(log(3, token, 1, 0, 12));

        let report = client
            .capacity_probe(
                TransferLogRange::new(1, token, 1, 3),
                TransferLogCapacityLimits {
                    max_logs: 10,
                    max_logs_per_block: 1,
                },
            )
            .await
            .unwrap();

        assert_eq!(report.log_count, 3);
        assert_eq!(report.max_logs_in_single_block, 2);
        assert!(!report.is_within_limits());
        assert!(matches!(
            report.ensure_within_limits(),
            Err(ChainError::CapacityExceeded {
                log_count: 3,
                max_logs: 10
            })
        ));
    }

    #[tokio::test]
    async fn fake_client_supports_balances_receipts_and_broadcasts() {
        let client = fake_client();
        let token = address(0x11);
        let owner = address(0x77);
        let receipt = TxReceipt {
            tx_hash: tx_hash(44),
            block: ChainBlockRef::new(2, block_hash(2)),
            status: TransactionStatus::Success,
            gas_used: Some(21_000),
        };
        client
            .set_balance(token, owner, RawAmount::from(123))
            .set_receipt(receipt.clone());

        assert_eq!(
            client.token_balance(token, owner).await.unwrap(),
            RawAmount::from(123)
        );
        assert_eq!(
            client.transaction_receipt(tx_hash(44)).await.unwrap(),
            Some(receipt)
        );
        assert_eq!(
            client.broadcast_signed_tx(vec![0xde, 0xad]).await.unwrap(),
            tx_hash_from_counter(1)
        );
    }

    #[tokio::test]
    async fn chain_header_reader_can_drive_order_service_head_reader() {
        let client = fake_client();
        let head = OrderChainHeadReader::current_head(&client).await.unwrap();

        assert_eq!(head, ChainBlockRef::new(3, block_hash(3)));
    }

    #[test]
    fn transfer_log_addresses_collects_unique_to_addresses() {
        let token = address(0x11);
        let logs = [
            log(1, token, 1, 0, 10),
            log(1, token, 1, 1, 10),
            log(1, token, 1, 2, 11),
        ];

        assert_eq!(
            transfer_log_addresses(&logs),
            BTreeSet::from([address(10), address(11)])
        );
    }

    fn fake_client() -> FakeErc20ChainClient {
        let client = FakeErc20ChainClient::new(1);
        client
            .insert_block(block(1, 1))
            .insert_block(block(2, 2))
            .insert_block(block(3, 3))
            .set_safe_head(ChainBlockRef::new(2, block_hash(2)))
            .set_finalized_head(ChainBlockRef::new(1, block_hash(1)))
    }

    fn log(
        block_number: u64,
        token_address: EvmAddress,
        chain_id: u64,
        log_index: u64,
        to_byte: u8,
    ) -> TransferLog {
        TransferLog {
            chain_id,
            token_address,
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
            datetime!(2026-05-03 00:00:00 UTC) + time::Duration::seconds(number as i64),
        )
    }

    fn address(byte: u8) -> EvmAddress {
        EvmAddress::from_bytes([byte; 20])
    }

    fn tx_hash(value: u64) -> TxHash {
        let mut bytes = [0u8; 32];
        bytes[24..].copy_from_slice(&value.to_be_bytes());
        TxHash::from_bytes(bytes)
    }

    fn tx_hash_from_counter(value: u64) -> TxHash {
        let mut bytes = [0u8; 32];
        bytes[..8].copy_from_slice(&value.to_be_bytes());
        TxHash::from_bytes(bytes)
    }

    fn block_hash(byte: u8) -> BlockHash {
        BlockHash::from_bytes([byte; 32])
    }
}
