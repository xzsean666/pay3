use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pay3::{
    chain::{
        ChainBlock, ChainError, ChainHeaderReader, TransferLog, TransferLogCapacityLimits,
        TransferLogCapacityReport, TransferLogRange, TransferLogSource,
    },
    db::repositories::PaymentWindowCandidate,
    domain::{
        BlockHash, ChainBlockRef, EvmAddress, OrderStatus, PaymentChainStatus, PaymentMatchStatus,
        RawAmount, TxHash,
    },
    services::{
        payment_windows::{PaymentWindowLookup, PaymentWindowLookupError},
        payments::{PaymentMatcher, PaymentMatchingConfig},
    },
    transfer_log_store::StreamId,
};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

type LookupCall = (u64, EvmAddress, Vec<EvmAddress>);

#[derive(Clone, Debug)]
struct FakeTransferLogSource {
    logs: Arc<Vec<TransferLog>>,
    calls: Arc<Mutex<Vec<TransferLogRange>>>,
}

impl FakeTransferLogSource {
    fn new(logs: Vec<TransferLog>) -> Self {
        Self {
            logs: Arc::new(logs),
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn calls(&self) -> Vec<TransferLogRange> {
        self.calls
            .lock()
            .expect("source calls lock poisoned")
            .clone()
    }
}

#[async_trait]
impl TransferLogSource for FakeTransferLogSource {
    async fn transfer_logs(&self, range: TransferLogRange) -> Result<Vec<TransferLog>, ChainError> {
        self.calls
            .lock()
            .expect("source calls lock poisoned")
            .push(range);
        Ok(self
            .logs
            .iter()
            .filter(|log| log.block.number >= range.from_block && log.block.number <= range.to_block)
            .cloned()
            .collect())
    }

    async fn capacity_probe(
        &self,
        range: TransferLogRange,
        limits: TransferLogCapacityLimits,
    ) -> Result<TransferLogCapacityReport, ChainError> {
        Ok(TransferLogCapacityReport {
            range,
            log_count: 0,
            max_logs_in_single_block: 0,
            limits,
        })
    }
}

#[derive(Clone, Debug, Default)]
struct FakeWindowLookup {
    candidates: Arc<Mutex<Vec<PaymentWindowCandidate>>>,
    calls: Arc<Mutex<Vec<LookupCall>>>,
}

impl FakeWindowLookup {
    fn with_candidates(candidates: Vec<PaymentWindowCandidate>) -> Self {
        Self {
            candidates: Arc::new(Mutex::new(candidates)),
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn calls(&self) -> Vec<LookupCall> {
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
    ) -> Result<Vec<PaymentWindowCandidate>, PaymentWindowLookupError> {
        self.calls
            .lock()
            .expect("lookup calls lock poisoned")
            .push((chain_id, token_address, to_addresses.to_vec()));

        Ok(self
            .candidates
            .lock()
            .expect("candidates lock poisoned")
            .iter()
            .filter(|candidate| to_addresses.contains(&candidate.receive_address))
            .cloned()
            .collect())
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
async fn payment_matching_batches_unique_to_addresses() {
    let stream = stream();
    let first = transfer_log(stream, 10, 1, address(10), 10);
    let second_same_to = transfer_log(stream, 11, 2, address(10), 11);
    let third = transfer_log(stream, 12, 3, address(20), 12);
    let source = FakeTransferLogSource::new(vec![first.clone(), second_same_to.clone(), third.clone()]);
    let lookup = FakeWindowLookup::with_candidates(vec![
        candidate(1, stream, address(10), 1, 20),
        candidate(2, stream, address(20), 1, 20),
    ]);

    let matched = matcher(source.clone(), lookup.clone(), 20, 10)
        .match_range(10, 12)
        .await
        .unwrap();

    assert_eq!(
        source.calls(),
        vec![TransferLogRange::new(
            stream.chain_id,
            stream.token_address,
            10,
            12
        )]
    );
    assert_eq!(
        lookup.calls(),
        vec![(stream.chain_id, stream.token_address, vec![address(10), address(20)])],
        "lookup input must contain only unique addresses across logs in the range"
    );
    assert_eq!(matched.len(), 3);
}

#[tokio::test]
async fn payment_matching_classifies_on_time_late_and_outside_window_by_block_timestamp() {
    let stream = stream();
    let logs = vec![
        transfer_log(stream, 10, 1, address(10), 10),
        transfer_log(stream, 11, 2, address(20), 16),
        transfer_log(stream, 12, 3, address(30), 40),
    ];
    let lookup = FakeWindowLookup::with_candidates(vec![
        candidate(1, stream, address(10), 10, 15),
        candidate(2, stream, address(20), 10, 15),
        candidate(3, stream, address(30), 20, 30),
    ]);

    let matched = matcher(FakeTransferLogSource::new(logs), lookup, 20, 10)
        .match_range(10, 12)
        .await
        .unwrap();

    let statuses = matched
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
        matched[1].block_time,
        now() + Duration::seconds(16)
    );
}

#[tokio::test]
async fn payment_matching_marks_observed_until_required_confirmations_are_reached() {
    let stream = stream();
    let logs = vec![transfer_log(stream, 10, 1, address(10), 10)];
    let lookup = FakeWindowLookup::with_candidates(vec![candidate(1, stream, address(10), 1, 20)]);

    let observed = matcher(
        FakeTransferLogSource::new(logs.clone()),
        lookup.clone(),
        11,
        3,
    )
    .match_range(10, 10)
    .await
    .unwrap();
    assert_eq!(observed[0].confirmations, 2);
    assert_eq!(
        observed[0].chain_status,
        PaymentChainStatus::Observed
    );

    let confirmed = matcher(FakeTransferLogSource::new(logs), lookup, 11, 2)
        .match_range(10, 10)
        .await
        .unwrap();
    assert_eq!(
        confirmed[0].chain_status,
        PaymentChainStatus::Confirmed
    );
}

#[tokio::test]
async fn payment_matching_does_not_generate_payments_without_candidates() {
    let stream = stream();
    let matched = matcher(
        FakeTransferLogSource::new(vec![transfer_log(stream, 10, 1, address(10), 10)]),
        FakeWindowLookup::default(),
        20,
        1,
    )
    .match_range(10, 10)
    .await
    .unwrap();

    assert!(matched.is_empty());
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

    let matched = matcher(
        FakeTransferLogSource::new(vec![transfer_log(stream, 10, 1, address(10), 10)]),
        FakeWindowLookup::with_candidates(vec![bad_chain, bad_token]),
        20,
        1,
    )
    .match_range(10, 10)
    .await
    .unwrap();

    assert!(matched.is_empty());
}

#[tokio::test]
async fn payment_matching_ignores_ambiguous_candidates_for_the_same_log() {
    let stream = stream();
    let matched = matcher(
        FakeTransferLogSource::new(vec![transfer_log(stream, 10, 1, address(10), 10)]),
        FakeWindowLookup::with_candidates(vec![
            candidate(1, stream, address(10), 1, 20),
            candidate(2, stream, address(10), 1, 20),
        ]),
        20,
        1,
    )
    .match_range(10, 10)
    .await
    .unwrap();

    assert!(matched.is_empty());
}

fn matcher(
    source: FakeTransferLogSource,
    lookup: FakeWindowLookup,
    head_number: u64,
    min_confirmations: u64,
) -> PaymentMatcher<FakeTransferLogSource, FakeWindowLookup, FakeHeadReader> {
    PaymentMatcher::new(
        source,
        lookup,
        FakeHeadReader {
            head: ChainBlockRef::new(head_number, block_hash(250)),
        },
        PaymentMatchingConfig {
            stream: stream(),
            min_confirmations,
            page_limit: 100,
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

fn transfer_log(
    stream: StreamId,
    block_number: u64,
    log_index: u64,
    to_address: EvmAddress,
    block_second: i64,
) -> TransferLog {
    TransferLog {
        chain_id: stream.chain_id,
        token_address: stream.token_address,
        block: ChainBlock::new(
            block_number,
            block_hash(block_number as u8),
            block_hash(block_number.saturating_sub(1) as u8),
            now() + Duration::seconds(block_second),
        ),
        tx_hash: tx_hash(log_index as u8),
        log_index,
        from_address: address(200),
        to_address,
        amount_raw: RawAmount::from(100),
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
