//! Pure payment matching service over ERC20 Transfer logs.

use std::collections::{BTreeMap, BTreeSet};

use async_trait::async_trait;
use thiserror::Error;
use uuid::Uuid;

use crate::{
    chain::{
        ChainError, ChainHeaderReader, TransferLog, TransferLogRange, TransferLogSource,
    },
    db::repositories::{MatchedPaymentInput, PaymentWindowCandidate},
    domain::{ChainBlockRef, EvmAddress, PaymentChainStatus, PaymentMatchStatus},
    services::payment_windows::{PaymentWindowLookup, PaymentWindowLookupError},
    transfer_log_store::{
        LogPageToken, StoredTransferLog, StreamId, TransferLogStoreError,
    },
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PaymentMatchingConfig {
    pub stream: StreamId,
    pub min_confirmations: u64,
    pub page_limit: usize,
    pub max_unique_to_addresses_per_batch: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaymentMatchPage {
    pub matched_payments: Vec<MatchedPaymentInput>,
    pub rejected: Vec<RejectedPaymentLog>,
    pub next_token: Option<LogPageToken>,
    pub complete_to_block: Option<u64>,
    pub kv_reorg_epoch: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredPaymentMatchInput {
    pub logs: Vec<StoredTransferLog>,
    pub candidates: Vec<PaymentWindowCandidate>,
    pub head: ChainBlockRef,
    pub next_token: Option<LogPageToken>,
    pub complete_to_block: Option<u64>,
    pub kv_reorg_epoch: u64,
}

impl PaymentMatchPage {
    pub fn recompute_order_ids(&self) -> Vec<Uuid> {
        self.matched_payments
            .iter()
            .map(|payment| payment.order_id)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RejectedPaymentLog {
    pub tx_hash: crate::domain::TxHash,
    pub log_index: u64,
    pub to_address: EvmAddress,
    pub reason: PaymentRejectionReason,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaymentRejectionReason {
    RemovedLog,
    AmbiguousCandidates,
}

#[derive(Debug, Error)]
pub enum PaymentMatchingError {
    #[error("invalid payment matching config: {field} must be greater than zero")]
    InvalidConfig { field: &'static str },

    #[error("unique to-address limit exceeded: {actual} addresses, max {max}")]
    UniqueAddressLimitExceeded { actual: usize, max: usize },

    #[error(transparent)]
    TransferLogStore(Box<TransferLogStoreError>),

    #[error(transparent)]
    Chain(Box<ChainError>),

    #[error(transparent)]
    PaymentWindowLookup(Box<PaymentWindowLookupError>),
}

impl From<TransferLogStoreError> for PaymentMatchingError {
    fn from(error: TransferLogStoreError) -> Self {
        Self::TransferLogStore(Box::new(error))
    }
}

impl From<ChainError> for PaymentMatchingError {
    fn from(error: ChainError) -> Self {
        Self::Chain(Box::new(error))
    }
}

impl From<PaymentWindowLookupError> for PaymentMatchingError {
    fn from(error: PaymentWindowLookupError) -> Self {
        Self::PaymentWindowLookup(Box::new(error))
    }
}

#[async_trait]
pub trait PaymentRangeMatcher: Send + Sync {
    fn config(&self) -> PaymentMatchingConfig;

    async fn match_range(
        &self,
        from_block: u64,
        to_block: u64,
    ) -> Result<Vec<MatchedPaymentInput>, PaymentMatchingError>;
}

pub struct PaymentMatcher<S, W, H> {
    log_source: S,
    window_lookup: W,
    head_reader: H,
    config: PaymentMatchingConfig,
}

impl<S, W, H> PaymentMatcher<S, W, H> {
    pub const fn new(
        log_source: S,
        window_lookup: W,
        head_reader: H,
        config: PaymentMatchingConfig,
    ) -> Self {
        Self {
            log_source,
            window_lookup,
            head_reader,
            config,
        }
    }

    pub const fn config(&self) -> PaymentMatchingConfig {
        self.config
    }
}

#[async_trait]
impl<S, W, H> PaymentRangeMatcher for PaymentMatcher<S, W, H>
where
    S: TransferLogSource,
    W: PaymentWindowLookup,
    H: ChainHeaderReader,
{
    fn config(&self) -> PaymentMatchingConfig {
        self.config
    }

    async fn match_range(
        &self,
        from_block: u64,
        to_block: u64,
    ) -> Result<Vec<MatchedPaymentInput>, PaymentMatchingError> {
        self.match_range(from_block, to_block).await
    }
}

impl<S, W, H> PaymentMatcher<S, W, H>
where
    S: TransferLogSource,
    W: PaymentWindowLookup,
    H: ChainHeaderReader,
{
    pub async fn match_range(
        &self,
        from_block: u64,
        to_block: u64,
    ) -> Result<Vec<MatchedPaymentInput>, PaymentMatchingError> {
        self.validate_config()?;

        let range = TransferLogRange::new(
            self.config.stream.chain_id,
            self.config.stream.token_address,
            from_block,
            to_block,
        );
        let logs = self.log_source.transfer_logs(range).await?;
        let head = self.head_reader.latest_head().await?;

        let lookup_addresses = unique_to_addresses_from_transfer_logs(&logs);
        if lookup_addresses.len() > self.config.max_unique_to_addresses_per_batch {
            return Err(PaymentMatchingError::UniqueAddressLimitExceeded {
                actual: lookup_addresses.len(),
                max: self.config.max_unique_to_addresses_per_batch,
            });
        }

        let candidates = if lookup_addresses.is_empty() {
            Vec::new()
        } else {
            self.window_lookup
                .lookup_batch(
                    self.config.stream.chain_id,
                    self.config.stream.token_address,
                    &lookup_addresses,
                )
                .await?
        };

        Ok(match_transfer_logs(
            self.config.stream,
            self.config.min_confirmations,
            logs,
            candidates,
            head,
        ))
    }

    fn validate_config(&self) -> Result<(), PaymentMatchingError> {
        if self.config.max_unique_to_addresses_per_batch == 0 {
            return Err(PaymentMatchingError::InvalidConfig {
                field: "max_unique_to_addresses_per_batch",
            });
        }
        Ok(())
    }
}

pub fn match_stored_transfer_logs(
    stream: StreamId,
    min_confirmations: u64,
    input: StoredPaymentMatchInput,
) -> PaymentMatchPage {
    let candidates_by_address = group_candidates_by_address(input.candidates);

    let mut matched_payments = Vec::new();
    let mut rejected = Vec::new();

    for log in input.logs {
        if log.removed {
            rejected.push(rejected_log(&log, PaymentRejectionReason::RemovedLog));
            continue;
        }

        let Some(candidates) = candidates_by_address.get(&log.to_address) else {
            continue;
        };
        let matching_candidates = candidates_for_log(&log, candidates, stream);
        if matching_candidates.is_empty() {
            continue;
        }

        let eligible_candidates = matching_candidates
            .iter()
            .copied()
            .filter(|candidate| candidate_is_within_monitor_window(&log, candidate))
            .collect::<Vec<_>>();

        let selected = match eligible_candidates.len() {
            1 => Some(eligible_candidates[0]),
            count if count > 1 => {
                rejected.push(rejected_log(
                    &log,
                    PaymentRejectionReason::AmbiguousCandidates,
                ));
                None
            }
            _ if matching_candidates.len() == 1 => Some(matching_candidates[0]),
            _ => {
                rejected.push(rejected_log(
                    &log,
                    PaymentRejectionReason::AmbiguousCandidates,
                ));
                None
            }
        };

        if let Some(candidate) = selected {
            matched_payments.push(matched_payment_input(
                &log,
                candidate,
                input.head,
                min_confirmations,
            ));
        }
    }

    PaymentMatchPage {
        matched_payments,
        rejected,
        next_token: input.next_token,
        complete_to_block: input.complete_to_block,
        kv_reorg_epoch: input.kv_reorg_epoch,
    }
}

fn group_candidates_by_address(
    candidates: Vec<PaymentWindowCandidate>,
) -> BTreeMap<EvmAddress, Vec<PaymentWindowCandidate>> {
    let mut by_address: BTreeMap<EvmAddress, Vec<PaymentWindowCandidate>> = BTreeMap::new();
    for candidate in candidates {
        by_address
            .entry(candidate.receive_address)
            .or_default()
            .push(candidate);
    }
    by_address
}

fn candidates_for_log<'a>(
    log: &StoredTransferLog,
    candidates: &'a [PaymentWindowCandidate],
    stream: StreamId,
) -> Vec<&'a PaymentWindowCandidate> {
    candidates
        .iter()
        .filter(|candidate| {
            candidate.chain_id == stream.chain_id
                && candidate.token_address == stream.token_address
                && candidate.receive_address == log.to_address
                && log.chain_id == stream.chain_id
                && log.token_address == stream.token_address
        })
        .collect()
}

fn candidate_is_within_monitor_window(
    log: &StoredTransferLog,
    candidate: &PaymentWindowCandidate,
) -> bool {
    log.block_number >= candidate.window_from_block.number
        && log.block_timestamp <= candidate.monitor_until
}

fn match_status(log: &StoredTransferLog, candidate: &PaymentWindowCandidate) -> PaymentMatchStatus {
    if log.block_number >= candidate.window_from_block.number
        && log.block_timestamp <= candidate.expires_at
    {
        PaymentMatchStatus::OnTime
    } else if candidate_is_within_monitor_window(log, candidate) {
        PaymentMatchStatus::Late
    } else {
        PaymentMatchStatus::OutsideWindow
    }
}

fn chain_status(
    log_block: ChainBlockRef,
    head: ChainBlockRef,
    min_confirmations: u64,
) -> (PaymentChainStatus, u64) {
    let confirmations = log_block.confirmations_against(head).unwrap_or(0);
    let status = if min_confirmations == 0 || confirmations >= min_confirmations {
        PaymentChainStatus::Confirmed
    } else {
        PaymentChainStatus::Observed
    };
    (status, confirmations)
}

fn matched_payment_input(
    log: &StoredTransferLog,
    candidate: &PaymentWindowCandidate,
    head: ChainBlockRef,
    min_confirmations: u64,
) -> MatchedPaymentInput {
    let log_block = ChainBlockRef::new(log.block_number, log.block_hash);
    let (chain_status, confirmations) = chain_status(log_block, head, min_confirmations);

    MatchedPaymentInput {
        id: Uuid::new_v4(),
        order_id: candidate.order_id,
        child_account_id: candidate.child_account_id,
        chain_id: log.chain_id,
        token_address: log.token_address,
        tx_hash: log.tx_hash,
        log_index: log.log_index,
        from_address: log.from_address,
        to_address: log.to_address,
        amount_raw: log.amount_raw,
        block_number: log.block_number,
        block_hash: log.block_hash,
        block_time: log.block_timestamp,
        confirmations,
        match_status: match_status(log, candidate),
        chain_status,
    }
}

fn rejected_log(log: &StoredTransferLog, reason: PaymentRejectionReason) -> RejectedPaymentLog {
    RejectedPaymentLog {
        tx_hash: log.tx_hash,
        log_index: log.log_index,
        to_address: log.to_address,
        reason,
    }
}

pub fn match_transfer_logs(
    stream: StreamId,
    min_confirmations: u64,
    logs: Vec<TransferLog>,
    candidates: Vec<PaymentWindowCandidate>,
    head: ChainBlockRef,
) -> Vec<MatchedPaymentInput> {
    let candidates_by_address = group_candidates_by_address(candidates);
    let mut matched_payments = Vec::new();

    for log in logs {
        let Some(address_candidates) = candidates_by_address.get(&log.to_address) else {
            continue;
        };
        let matching_candidates = candidates_for_transfer_log(&log, address_candidates, stream);
        if matching_candidates.is_empty() {
            continue;
        }

        let eligible_candidates = matching_candidates
            .iter()
            .copied()
            .filter(|candidate| candidate_is_within_monitor_window_transfer(&log, candidate))
            .collect::<Vec<_>>();

        let selected = match eligible_candidates.len() {
            1 => Some(eligible_candidates[0]),
            count if count > 1 => None,
            _ if matching_candidates.len() == 1 => Some(matching_candidates[0]),
            _ => None,
        };

        if let Some(candidate) = selected {
            matched_payments.push(matched_payment_input_from_transfer_log(
                &log,
                candidate,
                head,
                min_confirmations,
            ));
        }
    }

    matched_payments
}

fn unique_to_addresses_from_transfer_logs(logs: &[TransferLog]) -> Vec<EvmAddress> {
    logs.iter()
        .map(|log| log.to_address)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn candidates_for_transfer_log<'a>(
    log: &TransferLog,
    candidates: &'a [PaymentWindowCandidate],
    stream: StreamId,
) -> Vec<&'a PaymentWindowCandidate> {
    candidates
        .iter()
        .filter(|candidate| {
            candidate.chain_id == stream.chain_id
                && candidate.token_address == stream.token_address
                && candidate.receive_address == log.to_address
                && log.chain_id == stream.chain_id
                && log.token_address == stream.token_address
        })
        .collect()
}

fn candidate_is_within_monitor_window_transfer(
    log: &TransferLog,
    candidate: &PaymentWindowCandidate,
) -> bool {
    log.block.number >= candidate.window_from_block.number
        && log.block.timestamp <= candidate.monitor_until
}

fn match_status_transfer(log: &TransferLog, candidate: &PaymentWindowCandidate) -> PaymentMatchStatus {
    if log.block.number >= candidate.window_from_block.number
        && log.block.timestamp <= candidate.expires_at
    {
        PaymentMatchStatus::OnTime
    } else if candidate_is_within_monitor_window_transfer(log, candidate) {
        PaymentMatchStatus::Late
    } else {
        PaymentMatchStatus::OutsideWindow
    }
}

fn matched_payment_input_from_transfer_log(
    log: &TransferLog,
    candidate: &PaymentWindowCandidate,
    head: ChainBlockRef,
    min_confirmations: u64,
) -> MatchedPaymentInput {
    let log_block = ChainBlockRef::new(log.block.number, log.block.hash);
    let (chain_status, confirmations) = chain_status(log_block, head, min_confirmations);

    MatchedPaymentInput {
        id: Uuid::new_v4(),
        order_id: candidate.order_id,
        child_account_id: candidate.child_account_id,
        chain_id: log.chain_id,
        token_address: log.token_address,
        tx_hash: log.tx_hash,
        log_index: log.log_index,
        from_address: log.from_address,
        to_address: log.to_address,
        amount_raw: log.amount_raw,
        block_number: log.block.number,
        block_hash: log.block.hash,
        block_time: log.block.timestamp,
        confirmations,
        match_status: match_status_transfer(log, candidate),
        chain_status,
    }
}

