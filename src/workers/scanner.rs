//! Payment scanner worker tick over persisted KV transfer logs.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration as StdDuration;

use async_trait::async_trait;
use thiserror::Error;
use time::Duration;
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::{
    chain::{ChainError, ChainHeaderReader},
    db::repositories::{
        CommitScannedBatch, ConfirmObservedPaymentsBatch, PaymentConfirmationCandidate,
        PaymentRecord, PaymentRepository, RepositoryError, ScanCursorLease,
    },
    domain::{BlockHash, ChainBlockRef},
    services::{
        orders::Clock,
        payment_windows::PaymentWindowLookup,
        payments::{PaymentMatchPage, PaymentMatcher, PaymentMatchingConfig, PaymentMatchingError},
    },
    transfer_log_store::{
        LogPageToken, StreamId, TransferLogCursor, TransferLogReader, TransferLogStoreError,
    },
};

const DEFAULT_CONFIRMATION_SWEEP_LIMIT: usize = 1_000;

#[async_trait]
pub trait PaymentPageMatcher: Send + Sync {
    fn config(&self) -> PaymentMatchingConfig;

    async fn match_next_payment_page(
        &self,
        after: Option<LogPageToken>,
    ) -> Result<PaymentMatchPage, PaymentMatchingError>;
}

#[async_trait]
impl<L, W, H> PaymentPageMatcher for PaymentMatcher<L, W, H>
where
    L: TransferLogReader,
    W: PaymentWindowLookup,
    H: ChainHeaderReader,
{
    fn config(&self) -> PaymentMatchingConfig {
        self.config()
    }

    async fn match_next_payment_page(
        &self,
        after: Option<LogPageToken>,
    ) -> Result<PaymentMatchPage, PaymentMatchingError> {
        self.match_next_page(after).await
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaymentScannerConfig {
    pub worker_id: String,
    pub stream: StreamId,
    pub lease_duration: Duration,
    pub confirmation_sweep_limit: usize,
}

impl PaymentScannerConfig {
    pub fn new(worker_id: impl Into<String>, stream: StreamId, lease_duration: Duration) -> Self {
        Self {
            worker_id: worker_id.into(),
            stream,
            lease_duration,
            confirmation_sweep_limit: DEFAULT_CONFIRMATION_SWEEP_LIMIT,
        }
    }

    pub const fn with_confirmation_sweep_limit(mut self, limit: usize) -> Self {
        self.confirmation_sweep_limit = limit;
        self
    }

    fn validate(&self, matcher_config: PaymentMatchingConfig) -> Result<(), PaymentScannerError> {
        if self.worker_id.trim().is_empty() {
            return Err(PaymentScannerError::InvalidConfig {
                field: "worker_id",
                message: "must not be empty".to_string(),
            });
        }

        if !self.lease_duration.is_positive() {
            return Err(PaymentScannerError::InvalidConfig {
                field: "lease_duration",
                message: "must be positive".to_string(),
            });
        }

        if self.confirmation_sweep_limit == 0 {
            return Err(PaymentScannerError::InvalidConfig {
                field: "confirmation_sweep_limit",
                message: "must be greater than zero".to_string(),
            });
        }

        if self.stream != matcher_config.stream {
            return Err(PaymentScannerError::InvalidConfig {
                field: "stream",
                message: "scanner stream must match matcher stream".to_string(),
            });
        }

        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaymentScannerTickOutcome {
    LeaseHeld {
        stream: StreamId,
        worker_id: String,
    },
    KvReorgHandled {
        stream: StreamId,
        epoch: u64,
        last_reorg_from: u64,
    },
    Idle {
        stream: StreamId,
        last_scanned_block: u64,
        kv_completed_block: Option<u64>,
    },
    PageIncomplete {
        stream: StreamId,
        last_scanned_block: u64,
        next_token: Option<LogPageToken>,
    },
    Committed {
        stream: StreamId,
        complete_to_block: u64,
        matched_payments: usize,
        recompute_order_ids: Vec<Uuid>,
        records: Vec<PaymentRecord>,
    },
    ConfirmationsSwept {
        stream: StreamId,
        canonical_head: ChainBlockRef,
        confirmed_payments: usize,
        recompute_order_ids: Vec<Uuid>,
        records: Vec<PaymentRecord>,
    },
}

#[derive(Debug, Error)]
pub enum PaymentScannerError {
    #[error("invalid payment scanner config {field}: {message}")]
    InvalidConfig {
        field: &'static str,
        message: String,
    },

    #[error("lease timestamp overflow")]
    LeaseTimestampOverflow,

    #[error("KV reorg epoch {epoch} has no last_reorg_from")]
    MissingKvReorgBlock { epoch: u64 },

    #[error(
        "canonical block hash mismatch for payment block {block_number}: stored {stored_hash}, canonical {canonical_hash}"
    )]
    CanonicalBlockHashMismatch {
        block_number: u64,
        stored_hash: BlockHash,
        canonical_hash: BlockHash,
    },

    #[error(
        "KV reorg epoch regressed for {stream:?}: repository has {seen_epoch}, KVDB has {kv_epoch}"
    )]
    KvReorgEpochRegression {
        stream: StreamId,
        seen_epoch: u64,
        kv_epoch: u64,
    },

    #[error(transparent)]
    Repository(#[from] RepositoryError),

    #[error(transparent)]
    PaymentMatching(#[from] PaymentMatchingError),

    #[error(transparent)]
    Chain(#[from] ChainError),

    #[error(transparent)]
    TransferLogStore(#[from] TransferLogStoreError),
}

pub struct PaymentScannerWorker<R, M, L, H, C> {
    repository: R,
    matcher: M,
    log_reader: L,
    head_reader: H,
    clock: C,
    config: PaymentScannerConfig,
}

impl<R, M, L, H, C> PaymentScannerWorker<R, M, L, H, C> {
    pub const fn new(
        repository: R,
        matcher: M,
        log_reader: L,
        head_reader: H,
        clock: C,
        config: PaymentScannerConfig,
    ) -> Self {
        Self {
            repository,
            matcher,
            log_reader,
            head_reader,
            clock,
            config,
        }
    }
}

impl<R, M, L, H, C> PaymentScannerWorker<R, M, L, H, C>
where
    R: PaymentRepository,
    M: PaymentPageMatcher,
    L: TransferLogReader,
    H: ChainHeaderReader,
    C: Clock,
{
    pub async fn tick(&self) -> Result<PaymentScannerTickOutcome, PaymentScannerError> {
        let matcher_config = self.matcher.config();
        self.config.validate(matcher_config)?;

        let lease_until = self
            .clock
            .now()
            .checked_add(self.config.lease_duration)
            .ok_or(PaymentScannerError::LeaseTimestampOverflow)?;
        let stream = self.config.stream;
        let Some(lease) = self
            .repository
            .claim_scan_range(
                &self.config.worker_id,
                stream.chain_id,
                stream.token_address,
                lease_until,
            )
            .await?
        else {
            return Ok(PaymentScannerTickOutcome::LeaseHeld {
                stream,
                worker_id: self.config.worker_id.clone(),
            });
        };

        let cursor = self.log_reader.cursor(stream).await?;
        if cursor.reorg_epoch != lease.seen_kv_reorg_epoch {
            return self.handle_kv_reorg(lease, cursor).await;
        }

        let page = self
            .matcher
            .match_next_payment_page(after_token(lease.last_scanned_block))
            .await?;
        if page.kv_reorg_epoch != lease.seen_kv_reorg_epoch {
            let cursor = self.log_reader.cursor(stream).await?;
            return self.handle_kv_reorg(lease, cursor).await;
        }

        let Some(complete_to_block) =
            complete_to_block_for_commit(&page, &cursor, lease.last_scanned_block)
        else {
            return Ok(PaymentScannerTickOutcome::PageIncomplete {
                stream,
                last_scanned_block: lease.last_scanned_block,
                next_token: page.next_token,
            });
        };

        if complete_to_block <= lease.last_scanned_block {
            if let Some(outcome) = self.sweep_confirmations(matcher_config).await? {
                return Ok(outcome);
            }
            return Ok(PaymentScannerTickOutcome::Idle {
                stream,
                last_scanned_block: lease.last_scanned_block,
                kv_completed_block: cursor.last_completed_block,
            });
        }

        let matched_payments = page.matched_payments;
        let recompute_order_ids = recompute_order_ids(&matched_payments);
        let matched_payment_count = matched_payments.len();
        let records = self
            .repository
            .commit_scanned_batch(CommitScannedBatch {
                chain_id: stream.chain_id,
                token_address: stream.token_address,
                worker_id: self.config.worker_id.clone(),
                expected_last_scanned_block: lease.last_scanned_block,
                complete_to_block,
                expected_seen_kv_reorg_epoch: lease.seen_kv_reorg_epoch,
                seen_kv_reorg_epoch: page.kv_reorg_epoch,
                matched_payments,
                recompute_order_ids: recompute_order_ids.clone(),
            })
            .await?;

        Ok(PaymentScannerTickOutcome::Committed {
            stream,
            complete_to_block,
            matched_payments: matched_payment_count,
            recompute_order_ids,
            records,
        })
    }

    async fn handle_kv_reorg(
        &self,
        lease: ScanCursorLease,
        cursor: TransferLogCursor,
    ) -> Result<PaymentScannerTickOutcome, PaymentScannerError> {
        let stream = self.config.stream;
        if cursor.reorg_epoch < lease.seen_kv_reorg_epoch {
            return Err(PaymentScannerError::KvReorgEpochRegression {
                stream,
                seen_epoch: lease.seen_kv_reorg_epoch,
                kv_epoch: cursor.reorg_epoch,
            });
        }

        let last_reorg_from =
            cursor
                .last_reorg_from
                .ok_or(PaymentScannerError::MissingKvReorgBlock {
                    epoch: cursor.reorg_epoch,
                })?;

        self.repository
            .handle_kv_reorg_epoch(
                stream.chain_id,
                stream.token_address,
                cursor.reorg_epoch,
                last_reorg_from,
            )
            .await?;

        Ok(PaymentScannerTickOutcome::KvReorgHandled {
            stream,
            epoch: cursor.reorg_epoch,
            last_reorg_from,
        })
    }

    async fn sweep_confirmations(
        &self,
        matcher_config: PaymentMatchingConfig,
    ) -> Result<Option<PaymentScannerTickOutcome>, PaymentScannerError> {
        let stream = self.config.stream;
        let canonical_head = self.head_reader.latest_head().await?;
        let Some(max_block_number) =
            max_confirmable_block(canonical_head, matcher_config.min_confirmations)
        else {
            return Ok(None);
        };

        let candidates = self
            .repository
            .observed_payment_confirmation_candidates(
                stream.chain_id,
                stream.token_address,
                max_block_number,
                self.config.confirmation_sweep_limit,
            )
            .await?;
        if candidates.is_empty() {
            return Ok(None);
        }

        let payment_ids = self
            .canonical_confirmable_payment_ids(&candidates, canonical_head, matcher_config)
            .await?;
        if payment_ids.is_empty() {
            return Ok(None);
        }

        let records = self
            .repository
            .confirm_observed_payments(ConfirmObservedPaymentsBatch {
                chain_id: stream.chain_id,
                token_address: stream.token_address,
                payment_ids,
                canonical_head,
            })
            .await?;
        if records.is_empty() {
            return Ok(None);
        }

        let recompute_order_ids = recompute_order_ids_from_records(&records);
        Ok(Some(PaymentScannerTickOutcome::ConfirmationsSwept {
            stream,
            canonical_head,
            confirmed_payments: records.len(),
            recompute_order_ids,
            records,
        }))
    }

    async fn canonical_confirmable_payment_ids(
        &self,
        candidates: &[PaymentConfirmationCandidate],
        canonical_head: ChainBlockRef,
        matcher_config: PaymentMatchingConfig,
    ) -> Result<Vec<Uuid>, PaymentScannerError> {
        let mut canonical_blocks = BTreeMap::new();
        let mut payment_ids = Vec::new();

        for candidate in candidates {
            let canonical_block = match canonical_blocks.get(&candidate.block.number) {
                Some(block) => *block,
                None => {
                    let block = self
                        .head_reader
                        .block_by_number(candidate.block.number)
                        .await?
                        .block_ref();
                    canonical_blocks.insert(candidate.block.number, block);
                    block
                }
            };

            if canonical_block.hash != candidate.block.hash {
                return Err(PaymentScannerError::CanonicalBlockHashMismatch {
                    block_number: candidate.block.number,
                    stored_hash: candidate.block.hash,
                    canonical_hash: canonical_block.hash,
                });
            }

            if candidate
                .block
                .has_confirmations(canonical_head, matcher_config.min_confirmations)
            {
                payment_ids.push(candidate.payment_id);
            }
        }

        Ok(payment_ids)
    }

    async fn run_forever(self, poll_interval: StdDuration) {
        let mut interval = tokio::time::interval(poll_interval);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            interval.tick().await;
            match self.tick().await {
                Ok(outcome) => log_tick_outcome(&outcome),
                Err(error) => {
                    let stream = self.config.stream;
                    tracing::error!(
                        chain_id = stream.chain_id,
                        token_address = %stream.token_address,
                        worker_id = %self.config.worker_id,
                        error = %error,
                        "payment scanner tick failed"
                    );
                }
            }
        }
    }
}

pub fn spawn_payment_scanner_loop<R, M, L, H, C>(
    worker: PaymentScannerWorker<R, M, L, H, C>,
    poll_interval: StdDuration,
) -> Result<JoinHandle<()>, PaymentScannerError>
where
    R: PaymentRepository + 'static,
    M: PaymentPageMatcher + 'static,
    L: TransferLogReader + 'static,
    H: ChainHeaderReader + 'static,
    C: Clock + 'static,
{
    if poll_interval.is_zero() {
        return Err(PaymentScannerError::InvalidConfig {
            field: "poll_interval",
            message: "must be greater than zero".to_string(),
        });
    }

    Ok(tokio::spawn(worker.run_forever(poll_interval)))
}

fn log_tick_outcome(outcome: &PaymentScannerTickOutcome) {
    match outcome {
        PaymentScannerTickOutcome::LeaseHeld { stream, worker_id } => {
            tracing::debug!(
                chain_id = stream.chain_id,
                token_address = %stream.token_address,
                worker_id = %worker_id,
                "payment scanner lease held by another worker"
            );
        }
        PaymentScannerTickOutcome::KvReorgHandled {
            stream,
            epoch,
            last_reorg_from,
        } => {
            tracing::warn!(
                chain_id = stream.chain_id,
                token_address = %stream.token_address,
                epoch,
                last_reorg_from,
                "payment scanner handled KV reorg"
            );
        }
        PaymentScannerTickOutcome::Idle {
            stream,
            last_scanned_block,
            kv_completed_block,
        } => {
            tracing::debug!(
                chain_id = stream.chain_id,
                token_address = %stream.token_address,
                last_scanned_block,
                kv_completed_block = ?kv_completed_block,
                "payment scanner idle"
            );
        }
        PaymentScannerTickOutcome::PageIncomplete {
            stream,
            last_scanned_block,
            next_token,
        } => {
            tracing::debug!(
                chain_id = stream.chain_id,
                token_address = %stream.token_address,
                last_scanned_block,
                next_token_block = next_token.map(|token| token.block_number),
                next_token_log_index = next_token.map(|token| token.log_index),
                "payment scanner page incomplete"
            );
        }
        PaymentScannerTickOutcome::Committed {
            stream,
            complete_to_block,
            matched_payments,
            recompute_order_ids,
            records: _,
        } => {
            tracing::info!(
                chain_id = stream.chain_id,
                token_address = %stream.token_address,
                complete_to_block,
                matched_payments,
                recompute_order_count = recompute_order_ids.len(),
                "payment scanner committed batch"
            );
        }
        PaymentScannerTickOutcome::ConfirmationsSwept {
            stream,
            canonical_head,
            confirmed_payments,
            recompute_order_ids,
            records: _,
        } => {
            tracing::info!(
                chain_id = stream.chain_id,
                token_address = %stream.token_address,
                canonical_head_number = canonical_head.number,
                canonical_head_hash = %canonical_head.hash,
                confirmed_payments,
                recompute_order_count = recompute_order_ids.len(),
                "payment scanner swept payment confirmations"
            );
        }
    }
}

fn after_token(last_scanned_block: u64) -> Option<LogPageToken> {
    if last_scanned_block == 0 {
        None
    } else {
        Some(LogPageToken::new(last_scanned_block, u64::MAX))
    }
}

fn complete_to_block_for_commit(
    page: &PaymentMatchPage,
    cursor: &TransferLogCursor,
    last_scanned_block: u64,
) -> Option<u64> {
    let complete_to_block = match page.complete_to_block {
        Some(block) => block,
        None if page.matched_payments.is_empty() && page.rejected.is_empty() => {
            cursor.last_completed_block?
        }
        None => return None,
    };

    let complete_to_block = cursor
        .last_completed_block
        .map(|kv_completed| complete_to_block.min(kv_completed))
        .unwrap_or(complete_to_block);
    if complete_to_block < last_scanned_block {
        None
    } else {
        Some(complete_to_block)
    }
}

fn recompute_order_ids(payments: &[crate::db::repositories::MatchedPaymentInput]) -> Vec<Uuid> {
    payments
        .iter()
        .map(|payment| payment.order_id)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn recompute_order_ids_from_records(records: &[PaymentRecord]) -> Vec<Uuid> {
    records
        .iter()
        .map(|payment| payment.order_id)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn max_confirmable_block(canonical_head: ChainBlockRef, min_confirmations: u64) -> Option<u64> {
    if min_confirmations == 0 {
        Some(canonical_head.number)
    } else {
        canonical_head.number.checked_sub(min_confirmations - 1)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        sync::{Arc, Mutex},
    };

    use async_trait::async_trait;
    use time::{OffsetDateTime, macros::datetime};

    use super::*;
    use crate::{
        chain::ChainBlock,
        db::repositories::{MatchedPaymentInput, PaymentWindowCandidate},
        domain::{
            BlockHash, ChainBlockRef, EvmAddress, PaymentChainStatus, PaymentMatchStatus,
            RawAmount, TxHash,
        },
        services::orders::Clock,
        transfer_log_store::{
            LogsPage, ScanTargetMode, StoredBlockHeader, StoredTransferLog, TransferLogStreamConfig,
        },
    };

    #[tokio::test]
    async fn tick_claims_matches_and_commits_batch() {
        let repo = FakePaymentRepository::with_lease(lease(10, 7));
        let reader = FakeLogReader::new(cursor(20, 7, None));
        let matcher = FakeMatcher::with_pages(vec![Ok(match_page(
            vec![matched_payment(order_id(1), 12)],
            Some(12),
            Some(LogPageToken::new(12, 0)),
            7,
        ))]);
        let worker = worker(repo.clone(), matcher.clone(), reader);

        let outcome = worker.tick().await.unwrap();

        assert_eq!(matcher.calls(), vec![Some(LogPageToken::new(10, u64::MAX))]);
        assert_eq!(
            outcome,
            PaymentScannerTickOutcome::Committed {
                stream: stream(),
                complete_to_block: 12,
                matched_payments: 1,
                recompute_order_ids: vec![order_id(1)],
                records: Vec::new(),
            }
        );
        let commits = repo.commits();
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].expected_last_scanned_block, 10);
        assert_eq!(commits[0].complete_to_block, 12);
        assert_eq!(commits[0].expected_seen_kv_reorg_epoch, 7);
        assert_eq!(commits[0].matched_payments.len(), 1);
    }

    #[tokio::test]
    async fn tick_exits_when_lease_is_held_by_another_worker() {
        let repo = FakePaymentRepository::without_lease();
        let reader = FakeLogReader::new(cursor(20, 7, None));
        let matcher = FakeMatcher::with_pages(vec![Ok(match_page(Vec::new(), Some(20), None, 7))]);
        let worker = worker(repo.clone(), matcher.clone(), reader);

        let outcome = worker.tick().await.unwrap();

        assert_eq!(
            outcome,
            PaymentScannerTickOutcome::LeaseHeld {
                stream: stream(),
                worker_id: "scanner-1".to_string(),
            }
        );
        assert!(matcher.calls().is_empty());
        assert!(repo.commits().is_empty());
    }

    #[tokio::test]
    async fn tick_handles_kv_reorg_before_matching() {
        let repo = FakePaymentRepository::with_lease(lease(10, 7));
        let reader = FakeLogReader::new(cursor(20, 8, Some(9)));
        let matcher = FakeMatcher::with_pages(vec![Ok(match_page(Vec::new(), Some(20), None, 8))]);
        let worker = worker(repo.clone(), matcher.clone(), reader);

        let outcome = worker.tick().await.unwrap();

        assert_eq!(
            outcome,
            PaymentScannerTickOutcome::KvReorgHandled {
                stream: stream(),
                epoch: 8,
                last_reorg_from: 9,
            }
        );
        assert_eq!(repo.reorgs(), vec![(8, 9)]);
        assert!(matcher.calls().is_empty());
        assert!(repo.commits().is_empty());
    }

    #[tokio::test]
    async fn empty_page_advances_to_kv_completed_block() {
        let repo = FakePaymentRepository::with_lease(lease(10, 7));
        let reader = FakeLogReader::new(cursor(15, 7, None));
        let matcher = FakeMatcher::with_pages(vec![Ok(match_page(Vec::new(), None, None, 7))]);
        let worker = worker(repo.clone(), matcher, reader);

        let outcome = worker.tick().await.unwrap();

        assert!(matches!(
            outcome,
            PaymentScannerTickOutcome::Committed {
                complete_to_block: 15,
                matched_payments: 0,
                ..
            }
        ));
        assert_eq!(repo.commits()[0].complete_to_block, 15);
        assert!(repo.commits()[0].matched_payments.is_empty());
    }

    #[tokio::test]
    async fn page_that_does_not_complete_a_new_block_does_not_commit() {
        let repo = FakePaymentRepository::with_lease(lease(10, 7));
        let reader = FakeLogReader::new(cursor(12, 7, None));
        let matcher = FakeMatcher::with_pages(vec![Ok(match_page(
            vec![matched_payment(order_id(1), 11)],
            Some(10),
            Some(LogPageToken::new(11, 0)),
            7,
        ))]);
        let worker = worker(repo.clone(), matcher, reader);

        let outcome = worker.tick().await.unwrap();

        assert_eq!(
            outcome,
            PaymentScannerTickOutcome::Idle {
                stream: stream(),
                last_scanned_block: 10,
                kv_completed_block: Some(12),
            }
        );
        assert!(repo.commits().is_empty());
    }

    #[tokio::test]
    async fn idle_tick_sweeps_observed_payment_confirmations() {
        let payment_id = payment_id(9);
        let order_id = order_id(9);
        let repo = FakePaymentRepository::with_lease(lease(10, 7))
            .with_confirmation_candidates(vec![confirmation_candidate(
                payment_id,
                order_id,
                block_ref(9),
            )])
            .with_confirmation_records(vec![payment_record(
                payment_id,
                order_id,
                9,
                PaymentChainStatus::Confirmed,
            )]);
        let reader = FakeLogReader::new(cursor(10, 7, None));
        let matcher = FakeMatcher::with_pages(vec![Ok(match_page(Vec::new(), Some(10), None, 7))]);
        let head = FakeHeadReader::new(block_ref(10));
        let worker = worker_with_head(repo.clone(), matcher, reader, head);

        let outcome = worker.tick().await.unwrap();

        assert_eq!(
            outcome,
            PaymentScannerTickOutcome::ConfirmationsSwept {
                stream: stream(),
                canonical_head: block_ref(10),
                confirmed_payments: 1,
                recompute_order_ids: vec![order_id],
                records: vec![payment_record(
                    payment_id,
                    order_id,
                    9,
                    PaymentChainStatus::Confirmed
                )],
            }
        );
        assert_eq!(
            repo.confirmation_candidate_calls(),
            vec![(stream().chain_id, stream().token_address, 9, 1_000)]
        );
        assert_eq!(repo.confirmed_payment_ids(), vec![vec![payment_id]]);
    }

    #[tokio::test]
    async fn confirmation_sweep_fails_closed_on_canonical_block_hash_mismatch() {
        let repo =
            FakePaymentRepository::with_lease(lease(10, 7)).with_confirmation_candidates(vec![
                confirmation_candidate(payment_id(9), order_id(9), block_ref(9)),
            ]);
        let reader = FakeLogReader::new(cursor(10, 7, None));
        let matcher = FakeMatcher::with_pages(vec![Ok(match_page(Vec::new(), Some(10), None, 7))]);
        let head = FakeHeadReader::new(block_ref(10)).with_block(9, block_hash(0xee));
        let worker = worker_with_head(repo.clone(), matcher, reader, head);

        let error = worker.tick().await.unwrap_err();

        assert!(matches!(
            error,
            PaymentScannerError::CanonicalBlockHashMismatch {
                block_number: 9,
                stored_hash,
                canonical_hash,
            } if stored_hash == block_hash(9) && canonical_hash == block_hash(0xee)
        ));
        assert!(repo.confirmed_payment_ids().is_empty());
    }

    #[tokio::test]
    async fn commit_cas_mismatch_bubbles_without_retrying_inside_tick() {
        let repo = FakePaymentRepository::with_lease(lease(10, 7)).with_commit_error(
            RepositoryError::CursorCasMismatch {
                chain_id: stream().chain_id,
                token_address: stream().token_address,
                worker_id: "scanner-1".to_string(),
                expected_last_scanned_block: 10,
                actual_last_scanned_block: 11,
                expected_seen_kv_reorg_epoch: 7,
                actual_seen_kv_reorg_epoch: 7,
                actual_lease_owner: Some("scanner-2".to_string()),
            },
        );
        let reader = FakeLogReader::new(cursor(12, 7, None));
        let matcher = FakeMatcher::with_pages(vec![Ok(match_page(Vec::new(), Some(12), None, 7))]);
        let worker = worker(repo, matcher, reader);

        let error = worker.tick().await.unwrap_err();

        assert!(matches!(
            error,
            PaymentScannerError::Repository(RepositoryError::CursorCasMismatch { .. })
        ));
    }

    #[test]
    fn spawn_loop_rejects_zero_poll_interval() {
        let repo = FakePaymentRepository::without_lease();
        let reader = FakeLogReader::new(cursor(20, 7, None));
        let matcher = FakeMatcher::with_pages(Vec::new());
        let worker = worker(repo, matcher, reader);

        let error = spawn_payment_scanner_loop(worker, StdDuration::ZERO).unwrap_err();

        assert!(matches!(
            error,
            PaymentScannerError::InvalidConfig {
                field: "poll_interval",
                ..
            }
        ));
    }

    fn worker(
        repo: FakePaymentRepository,
        matcher: FakeMatcher,
        reader: FakeLogReader,
    ) -> PaymentScannerWorker<
        FakePaymentRepository,
        FakeMatcher,
        FakeLogReader,
        FakeHeadReader,
        FixedClock,
    > {
        worker_with_head(repo, matcher, reader, FakeHeadReader::new(block_ref(100)))
    }

    fn worker_with_head(
        repo: FakePaymentRepository,
        matcher: FakeMatcher,
        reader: FakeLogReader,
        head: FakeHeadReader,
    ) -> PaymentScannerWorker<
        FakePaymentRepository,
        FakeMatcher,
        FakeLogReader,
        FakeHeadReader,
        FixedClock,
    > {
        PaymentScannerWorker::new(
            repo,
            matcher,
            reader,
            head,
            FixedClock,
            PaymentScannerConfig::new("scanner-1", stream(), Duration::seconds(30)),
        )
    }

    #[derive(Clone, Copy, Debug)]
    struct FixedClock;

    impl Clock for FixedClock {
        fn now(&self) -> OffsetDateTime {
            now()
        }
    }

    #[derive(Clone)]
    struct FakePaymentRepository {
        state: Arc<Mutex<FakePaymentRepositoryState>>,
    }

    #[derive(Debug)]
    struct FakePaymentRepositoryState {
        claim_result: Option<ScanCursorLease>,
        commit_error: Option<RepositoryError>,
        commits: Vec<CommitScannedBatch>,
        confirmation_candidates: Vec<PaymentConfirmationCandidate>,
        confirmation_candidate_calls: Vec<(u64, EvmAddress, u64, usize)>,
        confirmation_records: Vec<PaymentRecord>,
        confirmation_batches: Vec<ConfirmObservedPaymentsBatch>,
        reorgs: Vec<(u64, u64)>,
    }

    impl FakePaymentRepository {
        fn with_lease(lease: ScanCursorLease) -> Self {
            Self {
                state: Arc::new(Mutex::new(FakePaymentRepositoryState {
                    claim_result: Some(lease),
                    commit_error: None,
                    commits: Vec::new(),
                    confirmation_candidates: Vec::new(),
                    confirmation_candidate_calls: Vec::new(),
                    confirmation_records: Vec::new(),
                    confirmation_batches: Vec::new(),
                    reorgs: Vec::new(),
                })),
            }
        }

        fn without_lease() -> Self {
            Self {
                state: Arc::new(Mutex::new(FakePaymentRepositoryState {
                    claim_result: None,
                    commit_error: None,
                    commits: Vec::new(),
                    confirmation_candidates: Vec::new(),
                    confirmation_candidate_calls: Vec::new(),
                    confirmation_records: Vec::new(),
                    confirmation_batches: Vec::new(),
                    reorgs: Vec::new(),
                })),
            }
        }

        fn with_commit_error(self, error: RepositoryError) -> Self {
            self.state
                .lock()
                .expect("fake repo lock poisoned")
                .commit_error = Some(error);
            self
        }

        fn with_confirmation_candidates(
            self,
            candidates: Vec<PaymentConfirmationCandidate>,
        ) -> Self {
            self.state
                .lock()
                .expect("fake repo lock poisoned")
                .confirmation_candidates = candidates;
            self
        }

        fn with_confirmation_records(self, records: Vec<PaymentRecord>) -> Self {
            self.state
                .lock()
                .expect("fake repo lock poisoned")
                .confirmation_records = records;
            self
        }

        fn commits(&self) -> Vec<CommitScannedBatch> {
            self.state
                .lock()
                .expect("fake repo lock poisoned")
                .commits
                .clone()
        }

        fn reorgs(&self) -> Vec<(u64, u64)> {
            self.state
                .lock()
                .expect("fake repo lock poisoned")
                .reorgs
                .clone()
        }

        fn confirmation_candidate_calls(&self) -> Vec<(u64, EvmAddress, u64, usize)> {
            self.state
                .lock()
                .expect("fake repo lock poisoned")
                .confirmation_candidate_calls
                .clone()
        }

        fn confirmed_payment_ids(&self) -> Vec<Vec<Uuid>> {
            self.state
                .lock()
                .expect("fake repo lock poisoned")
                .confirmation_batches
                .iter()
                .map(|batch| batch.payment_ids.clone())
                .collect()
        }
    }

    #[async_trait]
    impl PaymentRepository for FakePaymentRepository {
        async fn claim_scan_range(
            &self,
            _worker_id: &str,
            _chain_id: u64,
            _token_address: EvmAddress,
            _lease_until: OffsetDateTime,
        ) -> Result<Option<ScanCursorLease>, RepositoryError> {
            Ok(self
                .state
                .lock()
                .expect("fake repo lock poisoned")
                .claim_result
                .clone())
        }

        async fn commit_scanned_batch(
            &self,
            batch: CommitScannedBatch,
        ) -> Result<Vec<PaymentRecord>, RepositoryError> {
            let mut state = self.state.lock().expect("fake repo lock poisoned");
            state.commits.push(batch);
            if let Some(error) = state.commit_error.take() {
                return Err(error);
            }
            Ok(Vec::new())
        }

        async fn observed_payment_confirmation_candidates(
            &self,
            chain_id: u64,
            token_address: EvmAddress,
            max_block_number: u64,
            limit: usize,
        ) -> Result<Vec<PaymentConfirmationCandidate>, RepositoryError> {
            let mut state = self.state.lock().expect("fake repo lock poisoned");
            state.confirmation_candidate_calls.push((
                chain_id,
                token_address,
                max_block_number,
                limit,
            ));
            Ok(state.confirmation_candidates.clone())
        }

        async fn confirm_observed_payments(
            &self,
            batch: ConfirmObservedPaymentsBatch,
        ) -> Result<Vec<PaymentRecord>, RepositoryError> {
            let mut state = self.state.lock().expect("fake repo lock poisoned");
            state.confirmation_batches.push(batch);
            Ok(state.confirmation_records.clone())
        }

        async fn recompute_orders(&self, _order_ids: Vec<Uuid>) -> Result<(), RepositoryError> {
            Ok(())
        }

        async fn handle_kv_reorg_epoch(
            &self,
            _chain_id: u64,
            _token_address: EvmAddress,
            epoch: u64,
            last_reorg_from: u64,
        ) -> Result<(), RepositoryError> {
            self.state
                .lock()
                .expect("fake repo lock poisoned")
                .reorgs
                .push((epoch, last_reorg_from));
            Ok(())
        }
    }

    #[derive(Clone, Debug)]
    struct FakeHeadReader {
        latest: ChainBlockRef,
        blocks: Arc<Mutex<std::collections::BTreeMap<u64, BlockHash>>>,
    }

    impl FakeHeadReader {
        fn new(latest: ChainBlockRef) -> Self {
            let mut blocks = std::collections::BTreeMap::new();
            blocks.insert(latest.number, latest.hash);
            Self {
                latest,
                blocks: Arc::new(Mutex::new(blocks)),
            }
        }

        fn with_block(self, number: u64, hash: BlockHash) -> Self {
            self.blocks
                .lock()
                .expect("fake head lock poisoned")
                .insert(number, hash);
            self
        }
    }

    #[async_trait]
    impl ChainHeaderReader for FakeHeadReader {
        async fn latest_head(&self) -> Result<ChainBlockRef, ChainError> {
            Ok(self.latest)
        }

        async fn safe_head(&self) -> Result<ChainBlockRef, ChainError> {
            Ok(self.latest)
        }

        async fn finalized_head(&self) -> Result<ChainBlockRef, ChainError> {
            Ok(self.latest)
        }

        async fn block_by_number(&self, number: u64) -> Result<ChainBlock, ChainError> {
            let hash = self
                .blocks
                .lock()
                .expect("fake head lock poisoned")
                .get(&number)
                .copied()
                .unwrap_or_else(|| block_hash(number as u8));
            Ok(ChainBlock::new(
                number,
                hash,
                block_hash(number.saturating_sub(1) as u8),
                now(),
            ))
        }
    }

    #[derive(Clone)]
    struct FakeMatcher {
        state: Arc<Mutex<FakeMatcherState>>,
    }

    struct FakeMatcherState {
        pages: VecDeque<Result<PaymentMatchPage, PaymentMatchingError>>,
        calls: Vec<Option<LogPageToken>>,
    }

    impl FakeMatcher {
        fn with_pages(pages: Vec<Result<PaymentMatchPage, PaymentMatchingError>>) -> Self {
            Self {
                state: Arc::new(Mutex::new(FakeMatcherState {
                    pages: pages.into(),
                    calls: Vec::new(),
                })),
            }
        }

        fn calls(&self) -> Vec<Option<LogPageToken>> {
            self.state
                .lock()
                .expect("fake matcher lock poisoned")
                .calls
                .clone()
        }
    }

    #[async_trait]
    impl PaymentPageMatcher for FakeMatcher {
        fn config(&self) -> PaymentMatchingConfig {
            PaymentMatchingConfig {
                stream: stream(),
                min_confirmations: 2,
                page_limit: 100,
                max_unique_to_addresses_per_batch: 100,
            }
        }

        async fn match_next_payment_page(
            &self,
            after: Option<LogPageToken>,
        ) -> Result<PaymentMatchPage, PaymentMatchingError> {
            let mut state = self.state.lock().expect("fake matcher lock poisoned");
            state.calls.push(after);
            state
                .pages
                .pop_front()
                .expect("fake matcher page should be queued")
        }
    }

    #[derive(Clone)]
    struct FakeLogReader {
        cursor: TransferLogCursor,
    }

    impl FakeLogReader {
        const fn new(cursor: TransferLogCursor) -> Self {
            Self { cursor }
        }
    }

    #[async_trait]
    impl TransferLogReader for FakeLogReader {
        async fn cursor(
            &self,
            _stream: StreamId,
        ) -> Result<TransferLogCursor, TransferLogStoreError> {
            Ok(self.cursor.clone())
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
            _stream: StreamId,
            _after: Option<LogPageToken>,
            _limit: usize,
        ) -> Result<LogsPage, TransferLogStoreError> {
            Ok(LogsPage::new(stream(), Vec::new(), None, None))
        }
    }

    fn lease(last_scanned_block: u64, seen_kv_reorg_epoch: u64) -> ScanCursorLease {
        ScanCursorLease {
            chain_id: stream().chain_id,
            token_address: stream().token_address,
            lease_owner: "scanner-1".to_string(),
            lease_until: now() + Duration::seconds(30),
            last_scanned_block,
            seen_kv_reorg_epoch,
        }
    }

    fn cursor(
        last_completed_block: u64,
        reorg_epoch: u64,
        last_reorg_from: Option<u64>,
    ) -> TransferLogCursor {
        TransferLogCursor {
            stream: stream(),
            start_block: 1,
            next_block: last_completed_block + 1,
            last_completed_block: Some(last_completed_block),
            last_completed_hash: Some(BlockHash::ZERO),
            target_mode: ScanTargetMode::SafeTag,
            reorg_epoch,
            last_reorg_from,
            last_reorg_at: last_reorg_from.map(|_| now()),
            writer_epoch: 1,
            updated_at: now(),
        }
    }

    fn match_page(
        matched_payments: Vec<MatchedPaymentInput>,
        complete_to_block: Option<u64>,
        next_token: Option<LogPageToken>,
        kv_reorg_epoch: u64,
    ) -> PaymentMatchPage {
        PaymentMatchPage {
            matched_payments,
            rejected: Vec::new(),
            next_token,
            complete_to_block,
            kv_reorg_epoch,
        }
    }

    fn matched_payment(order_id: Uuid, block_number: u64) -> MatchedPaymentInput {
        MatchedPaymentInput {
            id: payment_id(block_number),
            order_id,
            child_account_id: child_account_id(1),
            chain_id: stream().chain_id,
            token_address: stream().token_address,
            tx_hash: tx_hash(block_number),
            log_index: 0,
            from_address: address(0x10),
            to_address: address(0x20),
            amount_raw: RawAmount::from(100),
            block_number,
            block_hash: BlockHash::ZERO,
            block_time: now(),
            confirmations: 3,
            match_status: PaymentMatchStatus::OnTime,
            chain_status: PaymentChainStatus::Confirmed,
        }
    }

    fn confirmation_candidate(
        payment_id: Uuid,
        order_id: Uuid,
        block: ChainBlockRef,
    ) -> PaymentConfirmationCandidate {
        PaymentConfirmationCandidate {
            payment_id,
            order_id,
            block,
            confirmations: 1,
        }
    }

    fn payment_record(
        id: Uuid,
        order_id: Uuid,
        block_number: u64,
        chain_status: PaymentChainStatus,
    ) -> PaymentRecord {
        PaymentRecord {
            id,
            order_id,
            child_account_id: child_account_id(1),
            chain_id: stream().chain_id,
            token_address: stream().token_address,
            tx_hash: tx_hash(block_number),
            log_index: 0,
            from_address: address(0x10),
            to_address: address(0x20),
            amount_raw: RawAmount::from(100),
            block_number,
            block_hash: block_hash(block_number as u8),
            block_time: now(),
            confirmations: 2,
            match_status: PaymentMatchStatus::OnTime,
            chain_status,
            created_at: now(),
            updated_at: now(),
        }
    }

    fn block_ref(number: u64) -> ChainBlockRef {
        ChainBlockRef::new(number, block_hash(number as u8))
    }

    fn block_hash(byte: u8) -> BlockHash {
        BlockHash::from_bytes([byte; 32])
    }

    fn stream() -> StreamId {
        StreamId::new(31337, address(1))
    }

    fn now() -> OffsetDateTime {
        datetime!(2026-05-03 12:00 UTC)
    }

    fn order_id(value: u128) -> Uuid {
        Uuid::from_u128(value)
    }

    fn child_account_id(value: u128) -> Uuid {
        Uuid::from_u128(1000 + value)
    }

    fn payment_id(value: u64) -> Uuid {
        Uuid::from_u128(2000 + u128::from(value))
    }

    fn address(byte: u8) -> EvmAddress {
        format!("0x{byte:040x}").parse().unwrap()
    }

    fn tx_hash(value: u64) -> TxHash {
        format!("0x{value:064x}").parse().unwrap()
    }

    #[allow(dead_code)]
    fn candidate(order_id: Uuid) -> PaymentWindowCandidate {
        PaymentWindowCandidate {
            order_id,
            child_account_id: child_account_id(1),
            receive_address: address(0x20),
            chain_id: stream().chain_id,
            token_address: stream().token_address,
            expected_amount_raw: RawAmount::from(100),
            paid_amount_raw: RawAmount::ZERO,
            order_status: crate::domain::OrderStatus::Pending,
            window_from: now(),
            window_from_block: ChainBlockRef::new(1, BlockHash::ZERO),
            expires_at: now() + Duration::hours(1),
            monitor_until: now() + Duration::hours(2),
        }
    }

    #[allow(dead_code)]
    fn stream_config() -> TransferLogStreamConfig {
        TransferLogStreamConfig {
            chain_id: stream().chain_id,
            token_address: stream().token_address,
            start_block: 1,
            poll_interval_ms: 1000,
            batch_size_blocks: 10,
            max_batch_size_blocks: 10,
            max_logs_per_page: 100,
            max_unique_to_addresses_per_batch: 100,
            max_db_fallback_addresses: 100,
            capacity_probe_blocks: 10,
            reorg_lookback_blocks: 8,
            target_mode: ScanTargetMode::SafeTag,
            rpc_max_retries: 3,
            log_source: crate::transfer_log_store::LogSourceKind::RpcRange,
        }
    }
}
