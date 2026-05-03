pub mod chain {
    pub use pay3::chain::*;
}

pub mod db {
    pub use pay3::db::*;
}

pub mod domain {
    pub use pay3::domain::*;
}

pub mod transfer_log_store {
    pub use pay3::transfer_log_store::*;
}

#[allow(dead_code)]
#[path = "../src/services/payment_windows.rs"]
mod payment_windows;

pub mod services {
    pub mod payment_windows {
        pub use crate::payment_windows::*;
    }
}

#[allow(dead_code)]
#[path = "../src/services/payments.rs"]
mod payments;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pay3::{
    chain::{ChainBlock, ChainError, ChainHeaderReader},
    db::repositories::PaymentWindowCandidate,
    domain::{
        BlockHash, ChainBlockRef, EvmAddress, OrderStatus, PaymentChainStatus, PaymentMatchStatus,
        RawAmount, TxHash,
    },
    transfer_log_store::{
        LogPageToken, LogsPage, ScanTargetMode, StoredBlockHeader, StoredTransferLog, StreamId,
        TransferLogCursor, TransferLogReader, TransferLogStoreError,
    },
};
use payment_windows::PaymentWindowLookup;
use payments::{PaymentMatcher, PaymentMatchingConfig, PaymentRejectionReason, RejectedPaymentLog};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

#[derive(Clone, Debug)]
struct FakeLogReader {
    stream: StreamId,
    logs: Arc<Vec<StoredTransferLog>>,
    complete_to_block: Option<u64>,
    reorg_epoch: u64,
    calls: Arc<Mutex<Vec<(Option<LogPageToken>, usize)>>>,
}

impl FakeLogReader {
    fn new(stream: StreamId, logs: Vec<StoredTransferLog>) -> Self {
        Self {
            stream,
            logs: Arc::new(logs),
            complete_to_block: Some(99),
            reorg_epoch: 7,
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn calls(&self) -> Vec<(Option<LogPageToken>, usize)> {
        self.calls
            .lock()
            .expect("reader calls lock poisoned")
            .clone()
    }
}

#[async_trait]
impl TransferLogReader for FakeLogReader {
    async fn cursor(&self, _stream: StreamId) -> Result<TransferLogCursor, TransferLogStoreError> {
        Ok(TransferLogCursor {
            stream: self.stream,
            start_block: 1,
            next_block: 100,
            last_completed_block: self.complete_to_block,
            last_completed_hash: None,
            target_mode: ScanTargetMode::SafeTag,
            reorg_epoch: self.reorg_epoch,
            last_reorg_from: None,
            last_reorg_at: None,
            writer_epoch: 1,
            updated_at: now(),
        })
    }

    async fn block_header(
        &self,
        _stream: StreamId,
        _block: u64,
    ) -> Result<Option<StoredBlockHeader>, TransferLogStoreError> {
        Ok(None)
    }

    async fn logs_in_range(
        &self,
        _stream: StreamId,
        _from: u64,
        _to: u64,
        _max_logs: usize,
    ) -> Result<Vec<StoredTransferLog>, TransferLogStoreError> {
        Ok(Vec::new())
    }

    async fn logs_page(
        &self,
        stream: StreamId,
        after: Option<LogPageToken>,
        limit: usize,
    ) -> Result<LogsPage, TransferLogStoreError> {
        self.calls
            .lock()
            .expect("reader calls lock poisoned")
            .push((after, limit));

        let logs = self
            .logs
            .iter()
            .filter(|log| after.is_none_or(|token| token.includes_log_exclusively(log)))
            .take(limit)
            .cloned()
            .collect::<Vec<_>>();
        let next_token = logs.last().map(LogPageToken::from_log);

        Ok(LogsPage::new(
            stream,
            logs,
            next_token,
            self.complete_to_block,
        ))
    }
}

#[derive(Clone, Debug, Default)]
struct FakeWindowLookup {
    candidates: Arc<Mutex<Vec<PaymentWindowCandidate>>>,
    calls: Arc<Mutex<Vec<(u64, EvmAddress, Vec<EvmAddress>)>>>,
}

impl FakeWindowLookup {
    fn with_candidates(candidates: Vec<PaymentWindowCandidate>) -> Self {
        Self {
            candidates: Arc::new(Mutex::new(candidates)),
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn calls(&self) -> Vec<(u64, EvmAddress, Vec<EvmAddress>)> {
        self.calls
            .lock()
            .expect("lookup calls lock poisoned")
            .clone()
    }
}

#[async_trait]
impl PaymentWindowLookup for FakeWindowLookup {
    async fn lookup_batch(
        &self,
        chain_id: u64,
        token_address: EvmAddress,
        to_addresses: &[EvmAddress],
    ) -> Vec<PaymentWindowCandidate> {
        self.calls
            .lock()
            .expect("lookup calls lock poisoned")
            .push((chain_id, token_address, to_addresses.to_vec()));

        self.candidates
            .lock()
            .expect("candidates lock poisoned")
            .iter()
            .filter(|candidate| to_addresses.contains(&candidate.receive_address))
            .cloned()
            .collect()
    }
}

#[derive(Clone, Copy, Debug)]
struct FakeHeadReader {
    head: ChainBlockRef,
}

#[async_trait]
impl ChainHeaderReader for FakeHeadReader {
    async fn latest_head(&self) -> Result<ChainBlockRef, ChainError> {
        Ok(self.head)
    }

    async fn safe_head(&self) -> Result<ChainBlockRef, ChainError> {
        Ok(self.head)
    }

    async fn finalized_head(&self) -> Result<ChainBlockRef, ChainError> {
        Ok(self.head)
    }

    async fn block_by_number(&self, number: u64) -> Result<ChainBlock, ChainError> {
        Ok(ChainBlock::new(
            number,
            block_hash(number as u8),
            block_hash(number.saturating_sub(1) as u8),
            now(),
        ))
    }
}

#[tokio::test]
async fn payment_matching_passes_page_token_and_batches_unique_to_addresses() {
    let stream = stream();
    let first = stored_log(stream, 10, 1, address(10), 10);
    let second_same_to = stored_log(stream, 11, 2, address(10), 11);
    let third = stored_log(stream, 12, 3, address(20), 12);
    let after = Some(LogPageToken::new(9, 9));
    let reader = FakeLogReader::new(
        stream,
        vec![first.clone(), second_same_to.clone(), third.clone()],
    );
    let lookup = FakeWindowLookup::with_candidates(vec![
        candidate(1, stream, address(10), 1, 20),
        candidate(2, stream, address(20), 1, 20),
    ]);

    let page = matcher(reader.clone(), lookup.clone(), 3, 2, 10)
        .match_next_page(after)
        .await
        .unwrap();

    assert_eq!(reader.calls(), vec![(after, 2)]);
    assert_eq!(
        lookup.calls(),
        vec![(stream.chain_id, stream.token_address, vec![address(10)],)],
        "lookup input must contain only unique addresses in the current page"
    );
    assert_eq!(
        page.next_token,
        Some(LogPageToken::from_log(&second_same_to))
    );
    assert_eq!(page.complete_to_block, Some(99));
    assert_eq!(page.kv_reorg_epoch, 7);
    assert_eq!(page.matched_payments.len(), 2);
}

#[tokio::test]
async fn payment_matching_classifies_on_time_late_and_outside_window_by_block_timestamp() {
    let stream = stream();
    let logs = vec![
        stored_log(stream, 10, 1, address(10), 10),
        stored_log(stream, 11, 2, address(20), 16),
        stored_log(stream, 12, 3, address(30), 40),
    ];
    let lookup = FakeWindowLookup::with_candidates(vec![
        candidate(1, stream, address(10), 10, 15),
        candidate(2, stream, address(20), 10, 15),
        candidate(3, stream, address(30), 20, 30),
    ]);

    let page = matcher(FakeLogReader::new(stream, logs), lookup, 20, 10, 10)
        .match_next_page(None)
        .await
        .unwrap();

    let statuses = page
        .matched_payments
        .iter()
        .map(|payment| payment.match_status)
        .collect::<Vec<_>>();
    assert_eq!(
        statuses,
        vec![
            PaymentMatchStatus::OnTime,
            PaymentMatchStatus::Late,
            PaymentMatchStatus::OutsideWindow,
        ]
    );
    assert_eq!(
        page.matched_payments[1].block_time,
        now() + Duration::seconds(16)
    );
}

#[tokio::test]
async fn payment_matching_marks_observed_until_required_confirmations_are_reached() {
    let stream = stream();
    let logs = vec![stored_log(stream, 10, 1, address(10), 10)];
    let lookup = FakeWindowLookup::with_candidates(vec![candidate(1, stream, address(10), 1, 20)]);

    let observed = matcher(
        FakeLogReader::new(stream, logs.clone()),
        lookup.clone(),
        11,
        10,
        3,
    )
    .match_next_page(None)
    .await
    .unwrap();
    assert_eq!(observed.matched_payments[0].confirmations, 2);
    assert_eq!(
        observed.matched_payments[0].chain_status,
        PaymentChainStatus::Observed
    );

    let confirmed = matcher(FakeLogReader::new(stream, logs), lookup, 11, 10, 2)
        .match_next_page(None)
        .await
        .unwrap();
    assert_eq!(
        confirmed.matched_payments[0].chain_status,
        PaymentChainStatus::Confirmed
    );
}

#[tokio::test]
async fn payment_matching_does_not_generate_payments_without_candidates() {
    let stream = stream();
    let page = matcher(
        FakeLogReader::new(stream, vec![stored_log(stream, 10, 1, address(10), 10)]),
        FakeWindowLookup::default(),
        20,
        10,
        1,
    )
    .match_next_page(None)
    .await
    .unwrap();

    assert!(page.matched_payments.is_empty());
    assert!(page.rejected.is_empty());
}

#[tokio::test]
async fn payment_matching_filters_candidate_chain_and_token_mismatches() {
    let stream = stream();
    let bad_chain = PaymentWindowCandidate {
        chain_id: 999,
        ..candidate(1, stream, address(10), 1, 20)
    };
    let bad_token = PaymentWindowCandidate {
        token_address: address(99),
        ..candidate(2, stream, address(10), 1, 20)
    };

    let page = matcher(
        FakeLogReader::new(stream, vec![stored_log(stream, 10, 1, address(10), 10)]),
        FakeWindowLookup::with_candidates(vec![bad_chain, bad_token]),
        20,
        10,
        1,
    )
    .match_next_page(None)
    .await
    .unwrap();

    assert!(page.matched_payments.is_empty());
    assert!(page.rejected.is_empty());
}

#[tokio::test]
async fn payment_matching_rejects_ambiguous_candidates_for_the_same_log() {
    let stream = stream();
    let page = matcher(
        FakeLogReader::new(stream, vec![stored_log(stream, 10, 1, address(10), 10)]),
        FakeWindowLookup::with_candidates(vec![
            candidate(1, stream, address(10), 1, 20),
            candidate(2, stream, address(10), 1, 20),
        ]),
        20,
        10,
        1,
    )
    .match_next_page(None)
    .await
    .unwrap();

    assert!(page.matched_payments.is_empty());
    assert_eq!(
        page.rejected,
        vec![RejectedPaymentLog {
            tx_hash: tx_hash(1),
            log_index: 1,
            to_address: address(10),
            reason: PaymentRejectionReason::AmbiguousCandidates,
        }]
    );
}

fn matcher(
    reader: FakeLogReader,
    lookup: FakeWindowLookup,
    head_number: u64,
    page_limit: usize,
    min_confirmations: u64,
) -> PaymentMatcher<FakeLogReader, FakeWindowLookup, FakeHeadReader> {
    PaymentMatcher::new(
        reader,
        lookup,
        FakeHeadReader {
            head: ChainBlockRef::new(head_number, block_hash(250)),
        },
        PaymentMatchingConfig {
            stream: stream(),
            min_confirmations,
            page_limit,
            max_unique_to_addresses_per_batch: 10,
        },
    )
}

fn candidate(
    seed: u8,
    stream: StreamId,
    receive_address: EvmAddress,
    window_from_block: u64,
    expires_at_second: i64,
) -> PaymentWindowCandidate {
    PaymentWindowCandidate {
        order_id: Uuid::from_u128(u128::from(seed)),
        child_account_id: Uuid::from_u128(u128::from(seed) + 100),
        receive_address,
        chain_id: stream.chain_id,
        token_address: stream.token_address,
        expected_amount_raw: RawAmount::from(100),
        paid_amount_raw: RawAmount::ZERO,
        order_status: OrderStatus::Pending,
        window_from: now(),
        window_from_block: ChainBlockRef::new(window_from_block, block_hash(1)),
        expires_at: now() + Duration::seconds(expires_at_second),
        monitor_until: now() + Duration::seconds(expires_at_second + 10),
    }
}

fn stored_log(
    stream: StreamId,
    block_number: u64,
    log_index: u64,
    to_address: EvmAddress,
    block_second: i64,
) -> StoredTransferLog {
    StoredTransferLog {
        chain_id: stream.chain_id,
        token_address: stream.token_address,
        block_number,
        block_hash: block_hash(block_number as u8),
        block_timestamp: now() + Duration::seconds(block_second),
        tx_hash: tx_hash(log_index as u8),
        tx_index: Some(0),
        log_index,
        from_address: address(200),
        to_address,
        amount_raw: RawAmount::from(100),
        removed: false,
        observed_at: now() + Duration::hours(1),
    }
}

fn stream() -> StreamId {
    StreamId::new(1, address(9))
}

fn now() -> OffsetDateTime {
    OffsetDateTime::UNIX_EPOCH
}

const fn address(seed: u8) -> EvmAddress {
    EvmAddress::from_bytes([seed; 20])
}

const fn block_hash(seed: u8) -> BlockHash {
    BlockHash::from_bytes([seed; 32])
}

const fn tx_hash(seed: u8) -> TxHash {
    TxHash::from_bytes([seed; 32])
}
