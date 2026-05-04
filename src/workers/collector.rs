//! Collection collector worker tick.

use std::time::{Duration, Instant};

use async_trait::async_trait;
use thiserror::Error;
use time::{Duration as TimeDuration, OffsetDateTime};
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::{
    chain::{ChainError, TransactionStatus, TxReceipt},
    db::repositories::{
        BroadcastableOutboundTx, OutboundRepository, OutboundTxRecord, ReceiptCheckableOutboundTx,
        RepositoryError,
    },
    domain::TxHash,
    health::{MetricsRecorder, WorkerName},
    services::{
        collections::{CollectionService, CollectionServiceError, PrepareCollectionJobOutcome},
        orders::IdGenerator,
    },
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CollectionCollectorConfig {
    pub worker_id: String,
    pub replacement_stuck_after: Duration,
}

impl CollectionCollectorConfig {
    pub fn new(worker_id: impl Into<String>) -> Self {
        Self {
            worker_id: worker_id.into(),
            replacement_stuck_after: Duration::ZERO,
        }
    }

    pub fn with_replacement_stuck_after(mut self, replacement_stuck_after: Duration) -> Self {
        self.replacement_stuck_after = replacement_stuck_after;
        self
    }

    fn validate(&self) -> Result<(), CollectionCollectorError> {
        if self.worker_id.trim().is_empty() {
            return Err(CollectionCollectorError::InvalidConfig {
                field: "worker_id",
                message: "must not be empty".to_string(),
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CollectionCollectorTickOutcome {
    NoJob,
    Broadcast {
        collection_id: Uuid,
        outbound: OutboundTxRecord,
    },
    ReceiptPending {
        collection_id: Uuid,
        outbound: OutboundTxRecord,
    },
    Confirmed {
        collection_id: Uuid,
        outbound: OutboundTxRecord,
    },
    Failed {
        collection_id: Uuid,
        outbound: OutboundTxRecord,
    },
}

#[derive(Debug, Error)]
pub enum CollectionCollectorError {
    #[error("invalid collection collector config {field}: {message}")]
    InvalidConfig {
        field: &'static str,
        message: String,
    },

    #[error(
        "broadcast tx hash mismatch for outbound {outbound_tx_id}: signed {expected_tx_hash}, broadcast returned {actual_tx_hash}"
    )]
    BroadcastHashMismatch {
        outbound_tx_id: Uuid,
        expected_tx_hash: TxHash,
        actual_tx_hash: TxHash,
    },

    #[error(
        "receipt tx hash mismatch for outbound {outbound_tx_id}: expected {expected_tx_hash}, receipt returned {actual_tx_hash}"
    )]
    ReceiptHashMismatch {
        outbound_tx_id: Uuid,
        expected_tx_hash: TxHash,
        actual_tx_hash: TxHash,
    },

    #[error(transparent)]
    CollectionService(#[from] CollectionServiceError),

    #[error(transparent)]
    Repository(#[from] RepositoryError),

    #[error(transparent)]
    Chain(#[from] ChainError),
}

#[async_trait]
pub trait CollectionJobPreparer: Send + Sync {
    async fn prepare_next_collection_job(
        &self,
        worker_id: &str,
    ) -> Result<PrepareCollectionJobOutcome, CollectionServiceError>;
}

#[async_trait]
impl<O, C, B, A, S, H, G, I> CollectionJobPreparer for CollectionService<O, C, B, A, S, H, G, I>
where
    O: crate::db::repositories::OrderRepository,
    C: crate::db::repositories::CollectionRepository,
    B: OutboundRepository,
    A: crate::db::repositories::AuditRepository,
    S: crate::signer::SignerProvider,
    H: crate::chain::Erc20ChainClient,
    G: crate::services::collections::PrefundedGasChecker,
    I: IdGenerator,
{
    async fn prepare_next_collection_job(
        &self,
        worker_id: &str,
    ) -> Result<PrepareCollectionJobOutcome, CollectionServiceError> {
        CollectionService::prepare_next_collection_job(self, worker_id).await
    }
}

#[async_trait]
pub trait CollectionJobReplacer: Send + Sync {
    async fn replace_stuck_collection_job(
        &self,
        worker_id: &str,
        job: ReceiptCheckableOutboundTx,
        replacement_reason: &str,
    ) -> Result<PrepareCollectionJobOutcome, CollectionServiceError>;
}

#[async_trait]
impl<O, C, B, A, S, H, G, I> CollectionJobReplacer for CollectionService<O, C, B, A, S, H, G, I>
where
    O: crate::db::repositories::OrderRepository,
    C: crate::db::repositories::CollectionRepository,
    B: OutboundRepository,
    A: crate::db::repositories::AuditRepository,
    S: crate::signer::SignerProvider,
    H: crate::chain::Erc20ChainClient,
    G: crate::services::collections::PrefundedGasChecker,
    I: IdGenerator,
{
    async fn replace_stuck_collection_job(
        &self,
        worker_id: &str,
        job: ReceiptCheckableOutboundTx,
        replacement_reason: &str,
    ) -> Result<PrepareCollectionJobOutcome, CollectionServiceError> {
        CollectionService::replace_collection_job(self, worker_id, job, replacement_reason).await
    }
}

#[async_trait]
pub trait SignedTxBroadcaster: Send + Sync {
    async fn broadcast_signed_tx(&self, signed_tx: Vec<u8>) -> Result<TxHash, ChainError>;
}

#[async_trait]
impl<T> SignedTxBroadcaster for T
where
    T: crate::chain::Erc20ChainClient,
{
    async fn broadcast_signed_tx(&self, signed_tx: Vec<u8>) -> Result<TxHash, ChainError> {
        crate::chain::Erc20ChainClient::broadcast_signed_tx(self, signed_tx).await
    }
}

#[async_trait]
pub trait TxReceiptReader: Send + Sync {
    async fn transaction_receipt(&self, tx_hash: TxHash) -> Result<Option<TxReceipt>, ChainError>;
}

#[async_trait]
impl<T> TxReceiptReader for T
where
    T: crate::chain::Erc20ChainClient,
{
    async fn transaction_receipt(&self, tx_hash: TxHash) -> Result<Option<TxReceipt>, ChainError> {
        crate::chain::Erc20ChainClient::transaction_receipt(self, tx_hash).await
    }
}

pub struct CollectionCollectorWorker<P, O, B> {
    preparer: P,
    outbound: O,
    broadcaster: B,
    config: CollectionCollectorConfig,
}

impl<P, O, B> CollectionCollectorWorker<P, O, B> {
    pub const fn new(
        preparer: P,
        outbound: O,
        broadcaster: B,
        config: CollectionCollectorConfig,
    ) -> Self {
        Self {
            preparer,
            outbound,
            broadcaster,
            config,
        }
    }
}

impl<P, O, B> CollectionCollectorWorker<P, O, B>
where
    P: CollectionJobPreparer + CollectionJobReplacer,
    O: OutboundRepository,
    B: SignedTxBroadcaster + TxReceiptReader,
{
    pub async fn tick(&self) -> Result<CollectionCollectorTickOutcome, CollectionCollectorError> {
        self.config.validate()?;
        let worker_id = self.config.worker_id.trim();

        if let Some(recoverable) = self
            .outbound
            .claim_signed_collect_tx_for_broadcast(worker_id)
            .await?
        {
            return self.broadcast_outbound(recoverable).await;
        }

        if let Some(receipt_checkable) = self
            .outbound
            .claim_broadcast_collect_tx_for_receipt(worker_id)
            .await?
        {
            return self
                .check_outbound_receipt(worker_id, receipt_checkable)
                .await;
        }

        let outcome = self.preparer.prepare_next_collection_job(worker_id).await?;
        let PrepareCollectionJobOutcome::Prepared {
            collection,
            outbound,
            signed_tx: _,
        } = outcome
        else {
            return Ok(CollectionCollectorTickOutcome::NoJob);
        };

        self.broadcast_outbound(BroadcastableOutboundTx {
            collection_id: collection.id,
            outbound,
        })
        .await
    }

    async fn broadcast_outbound(
        &self,
        recoverable: BroadcastableOutboundTx,
    ) -> Result<CollectionCollectorTickOutcome, CollectionCollectorError> {
        let broadcast_tx_hash = self
            .broadcaster
            .broadcast_signed_tx(recoverable.outbound.signed_tx.clone())
            .await?;
        if broadcast_tx_hash != recoverable.outbound.tx_hash {
            return Err(CollectionCollectorError::BroadcastHashMismatch {
                outbound_tx_id: recoverable.outbound.id,
                expected_tx_hash: recoverable.outbound.tx_hash,
                actual_tx_hash: broadcast_tx_hash,
            });
        }

        let outbound = self
            .outbound
            .mark_broadcast(recoverable.outbound.id)
            .await?;
        Ok(CollectionCollectorTickOutcome::Broadcast {
            collection_id: recoverable.collection_id,
            outbound,
        })
    }

    async fn check_outbound_receipt(
        &self,
        worker_id: &str,
        checkable: ReceiptCheckableOutboundTx,
    ) -> Result<CollectionCollectorTickOutcome, CollectionCollectorError> {
        let Some(receipt) = self
            .broadcaster
            .transaction_receipt(checkable.outbound.tx_hash)
            .await?
        else {
            if self.outbound_replacement_due(&checkable.outbound) {
                let outcome = self
                    .preparer
                    .replace_stuck_collection_job(
                        worker_id,
                        checkable.clone(),
                        "receipt missing beyond replacement threshold",
                    )
                    .await?;
                return match outcome {
                    PrepareCollectionJobOutcome::NoJob => {
                        Ok(CollectionCollectorTickOutcome::ReceiptPending {
                            collection_id: checkable.collection_id,
                            outbound: checkable.outbound,
                        })
                    }
                    PrepareCollectionJobOutcome::Prepared {
                        collection,
                        outbound,
                        signed_tx: _,
                    } => {
                        self.broadcast_outbound(BroadcastableOutboundTx {
                            collection_id: collection.id,
                            outbound,
                        })
                        .await
                    }
                };
            }
            return Ok(CollectionCollectorTickOutcome::ReceiptPending {
                collection_id: checkable.collection_id,
                outbound: checkable.outbound,
            });
        };

        if receipt.tx_hash != checkable.outbound.tx_hash {
            return Err(CollectionCollectorError::ReceiptHashMismatch {
                outbound_tx_id: checkable.outbound.id,
                expected_tx_hash: checkable.outbound.tx_hash,
                actual_tx_hash: receipt.tx_hash,
            });
        }

        match receipt.status {
            TransactionStatus::Success => {
                let outbound = self
                    .outbound
                    .mark_confirmed(checkable.outbound.id, receipt.block)
                    .await?;
                Ok(CollectionCollectorTickOutcome::Confirmed {
                    collection_id: checkable.collection_id,
                    outbound,
                })
            }
            TransactionStatus::Reverted => {
                let outbound = self
                    .outbound
                    .mark_failed(checkable.outbound.id, "collect transaction reverted")
                    .await?;
                Ok(CollectionCollectorTickOutcome::Failed {
                    collection_id: checkable.collection_id,
                    outbound,
                })
            }
        }
    }

    fn outbound_replacement_due(&self, outbound: &OutboundTxRecord) -> bool {
        let Some(last_broadcast_at) = outbound.last_broadcast_at else {
            return false;
        };

        let cutoff = OffsetDateTime::now_utc()
            - TimeDuration::seconds(self.config.replacement_stuck_after.as_secs() as i64);
        last_broadcast_at <= cutoff
    }

    async fn run_forever(self, poll_interval: Duration, metrics: Option<MetricsRecorder>) {
        let mut interval = tokio::time::interval(poll_interval);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            interval.tick().await;
            let started_at = Instant::now();
            match self.tick().await {
                Ok(outcome) => {
                    if let Some(metrics) = &metrics {
                        metrics.record_worker_success(
                            WorkerName::CollectionCollector,
                            started_at.elapsed(),
                        );
                    }
                    log_tick_outcome(&outcome);
                }
                Err(error) => {
                    if let Some(metrics) = &metrics {
                        metrics.record_worker_error(
                            WorkerName::CollectionCollector,
                            started_at.elapsed(),
                            error.to_string(),
                        );
                    }
                    tracing::error!(
                        worker_id = %self.config.worker_id,
                        error = %error,
                        "collection collector tick failed"
                    );
                }
            }
        }
    }
}

pub fn spawn_collection_collector_loop<P, O, B>(
    worker: CollectionCollectorWorker<P, O, B>,
    poll_interval: Duration,
) -> Result<JoinHandle<()>, CollectionCollectorError>
where
    P: CollectionJobPreparer + CollectionJobReplacer + 'static,
    O: OutboundRepository + 'static,
    B: SignedTxBroadcaster + TxReceiptReader + 'static,
{
    spawn_collection_collector_loop_with_optional_metrics(worker, poll_interval, None)
}

pub fn spawn_collection_collector_loop_with_metrics<P, O, B>(
    worker: CollectionCollectorWorker<P, O, B>,
    poll_interval: Duration,
    metrics: MetricsRecorder,
) -> Result<JoinHandle<()>, CollectionCollectorError>
where
    P: CollectionJobPreparer + CollectionJobReplacer + 'static,
    O: OutboundRepository + 'static,
    B: SignedTxBroadcaster + TxReceiptReader + 'static,
{
    spawn_collection_collector_loop_with_optional_metrics(worker, poll_interval, Some(metrics))
}

fn spawn_collection_collector_loop_with_optional_metrics<P, O, B>(
    worker: CollectionCollectorWorker<P, O, B>,
    poll_interval: Duration,
    metrics: Option<MetricsRecorder>,
) -> Result<JoinHandle<()>, CollectionCollectorError>
where
    P: CollectionJobPreparer + CollectionJobReplacer + 'static,
    O: OutboundRepository + 'static,
    B: SignedTxBroadcaster + TxReceiptReader + 'static,
{
    if poll_interval.is_zero() {
        return Err(CollectionCollectorError::InvalidConfig {
            field: "poll_interval",
            message: "must be greater than zero".to_string(),
        });
    }

    Ok(tokio::spawn(worker.run_forever(poll_interval, metrics)))
}

fn log_tick_outcome(outcome: &CollectionCollectorTickOutcome) {
    match outcome {
        CollectionCollectorTickOutcome::NoJob => {
            tracing::debug!("collection collector idle");
        }
        CollectionCollectorTickOutcome::Broadcast {
            collection_id,
            outbound,
        } => {
            tracing::info!(
                collection_id = %collection_id,
                outbound_tx_id = %outbound.id,
                tx_hash = %outbound.tx_hash,
                replacement_of = ?outbound.replacement_of,
                "collection collector broadcast outbound tx"
            );
        }
        CollectionCollectorTickOutcome::ReceiptPending {
            collection_id,
            outbound,
        } => {
            tracing::debug!(
                collection_id = %collection_id,
                outbound_tx_id = %outbound.id,
                tx_hash = %outbound.tx_hash,
                "collection collector receipt pending"
            );
        }
        CollectionCollectorTickOutcome::Confirmed {
            collection_id,
            outbound,
        } => {
            tracing::info!(
                collection_id = %collection_id,
                outbound_tx_id = %outbound.id,
                tx_hash = %outbound.tx_hash,
                receipt_block = ?outbound.receipt_block,
                "collection collector confirmed outbound tx"
            );
        }
        CollectionCollectorTickOutcome::Failed {
            collection_id,
            outbound,
        } => {
            tracing::warn!(
                collection_id = %collection_id,
                outbound_tx_id = %outbound.id,
                tx_hash = %outbound.tx_hash,
                error = ?outbound.error,
                "collection collector failed outbound tx"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        sync::{Arc, Mutex},
    };

    use async_trait::async_trait;
    use time::{Duration as TimeDuration, OffsetDateTime, macros::datetime};

    use super::*;
    use crate::{
        db::repositories::{NewSignedOutboundTx, OutboundTxPurpose, OutboundTxStatus},
        domain::{BlockHash, ChainBlockRef, EvmAddress, RawAmount},
        signer::SignedTx,
    };

    #[tokio::test]
    async fn tick_prepares_broadcasts_and_marks_outbound_broadcast() {
        let outbound = outbound_record(11, OutboundTxStatus::Signed);
        let worker = worker(
            FakePreparer::with_outcomes(vec![Ok(prepared_outcome(outbound.clone()))]),
            FakeOutboundRepository::default(),
            FakeBroadcaster::returning(outbound.tx_hash),
        );

        let outcome = worker.tick().await.unwrap();

        assert_eq!(
            outcome,
            CollectionCollectorTickOutcome::Broadcast {
                collection_id: collection_id(),
                outbound: outbound_record(11, OutboundTxStatus::Broadcast),
            }
        );
        assert_eq!(worker.preparer.calls(), vec!["collector-1".to_string()]);
        assert_eq!(worker.broadcaster.broadcasts(), vec![signed_tx().raw_tx]);
        assert_eq!(worker.outbound.marked(), vec![outbound.id]);
    }

    #[tokio::test]
    async fn tick_recovers_signed_outbound_before_preparing_new_job() {
        let outbound = outbound_record(12, OutboundTxStatus::Signed);
        let worker = worker(
            FakePreparer::with_outcomes(vec![Ok(PrepareCollectionJobOutcome::NoJob)]),
            FakeOutboundRepository::with_recoverable(BroadcastableOutboundTx {
                collection_id: collection_id(),
                outbound: outbound.clone(),
            }),
            FakeBroadcaster::returning(outbound.tx_hash),
        );

        let outcome = worker.tick().await.unwrap();

        assert_eq!(
            outcome,
            CollectionCollectorTickOutcome::Broadcast {
                collection_id: collection_id(),
                outbound: outbound_record(12, OutboundTxStatus::Broadcast),
            }
        );
        assert!(worker.preparer.calls().is_empty());
        assert_eq!(
            worker.outbound.claim_calls(),
            vec!["collector-1".to_string()]
        );
        assert_eq!(worker.broadcaster.broadcasts(), vec![outbound.signed_tx]);
        assert_eq!(worker.outbound.marked(), vec![outbound.id]);
    }

    #[tokio::test]
    async fn tick_returns_no_job_without_broadcasting() {
        let worker = worker(
            FakePreparer::with_outcomes(vec![Ok(PrepareCollectionJobOutcome::NoJob)]),
            FakeOutboundRepository::default(),
            FakeBroadcaster::returning(tx_hash(0xaa)),
        );

        let outcome = worker.tick().await.unwrap();

        assert_eq!(outcome, CollectionCollectorTickOutcome::NoJob);
        assert!(worker.broadcaster.broadcasts().is_empty());
        assert!(worker.outbound.marked().is_empty());
    }

    #[tokio::test]
    async fn tick_marks_broadcast_outbound_confirmed_when_success_receipt_exists() {
        let outbound = outbound_record(13, OutboundTxStatus::Broadcast);
        let receipt = receipt(outbound.tx_hash, TransactionStatus::Success, block_ref(90));
        let worker = worker(
            FakePreparer::with_outcomes(vec![Ok(PrepareCollectionJobOutcome::NoJob)]),
            FakeOutboundRepository::with_receipt_checkable(ReceiptCheckableOutboundTx {
                collection_id: collection_id(),
                outbound: outbound.clone(),
            }),
            FakeBroadcaster::returning(tx_hash(0xaa)).with_receipt(Some(receipt)),
        );

        let outcome = worker.tick().await.unwrap();

        assert_eq!(
            outcome,
            CollectionCollectorTickOutcome::Confirmed {
                collection_id: collection_id(),
                outbound: outbound_record_with_receipt(
                    13,
                    OutboundTxStatus::Confirmed,
                    Some(block_ref(90)),
                    None,
                ),
            }
        );
        assert!(worker.preparer.calls().is_empty());
        assert_eq!(worker.broadcaster.receipt_queries(), vec![outbound.tx_hash]);
        assert_eq!(
            worker.outbound.confirmed(),
            vec![(outbound.id, block_ref(90))]
        );
    }

    #[tokio::test]
    async fn tick_marks_broadcast_outbound_failed_when_receipt_reverted() {
        let outbound = outbound_record(14, OutboundTxStatus::Broadcast);
        let receipt = receipt(outbound.tx_hash, TransactionStatus::Reverted, block_ref(91));
        let worker = worker(
            FakePreparer::with_outcomes(vec![Ok(PrepareCollectionJobOutcome::NoJob)]),
            FakeOutboundRepository::with_receipt_checkable(ReceiptCheckableOutboundTx {
                collection_id: collection_id(),
                outbound: outbound.clone(),
            }),
            FakeBroadcaster::returning(tx_hash(0xaa)).with_receipt(Some(receipt)),
        );

        let outcome = worker.tick().await.unwrap();

        assert_eq!(
            outcome,
            CollectionCollectorTickOutcome::Failed {
                collection_id: collection_id(),
                outbound: outbound_record_with_receipt(
                    14,
                    OutboundTxStatus::Failed,
                    None,
                    Some("collect transaction reverted".to_string()),
                ),
            }
        );
        assert!(worker.preparer.calls().is_empty());
        assert_eq!(
            worker.outbound.failed(),
            vec![(outbound.id, "collect transaction reverted".to_string())]
        );
    }

    #[tokio::test]
    async fn tick_keeps_broadcast_outbound_pending_when_receipt_is_missing() {
        let outbound = outbound_record(15, OutboundTxStatus::Broadcast);
        let worker = worker(
            FakePreparer::with_outcomes(vec![Ok(PrepareCollectionJobOutcome::NoJob)]),
            FakeOutboundRepository::with_receipt_checkable(ReceiptCheckableOutboundTx {
                collection_id: collection_id(),
                outbound: outbound.clone(),
            }),
            FakeBroadcaster::returning(tx_hash(0xaa)).with_receipt(None),
        );

        let outcome = worker.tick().await.unwrap();

        assert_eq!(
            outcome,
            CollectionCollectorTickOutcome::ReceiptPending {
                collection_id: collection_id(),
                outbound,
            }
        );
        assert!(worker.preparer.calls().is_empty());
        assert!(worker.outbound.confirmed().is_empty());
        assert!(worker.outbound.failed().is_empty());
    }

    #[tokio::test]
    async fn tick_replaces_stale_broadcast_outbound_when_receipt_is_missing() {
        let mut outbound = outbound_record(16, OutboundTxStatus::Broadcast);
        outbound.last_broadcast_at =
            Some(OffsetDateTime::now_utc() - TimeDuration::seconds(10 * 60 + 1));
        let replacement = outbound_record(17, OutboundTxStatus::Signed);
        let worker = CollectionCollectorWorker::new(
            FakePreparer::with_replacement_outcomes(
                vec![Ok(PrepareCollectionJobOutcome::NoJob)],
                vec![Ok(prepared_outcome(replacement.clone()))],
            ),
            FakeOutboundRepository::with_receipt_checkable(ReceiptCheckableOutboundTx {
                collection_id: collection_id(),
                outbound: outbound.clone(),
            }),
            FakeBroadcaster::returning(replacement.tx_hash),
            CollectionCollectorConfig::new("collector-1")
                .with_replacement_stuck_after(Duration::from_secs(1)),
        );

        let outcome = worker.tick().await.unwrap();

        assert_eq!(
            outcome,
            CollectionCollectorTickOutcome::Broadcast {
                collection_id: collection_id(),
                outbound: outbound_record(17, OutboundTxStatus::Broadcast),
            }
        );
        assert_eq!(
            worker.preparer.replacement_calls(),
            vec![("collector-1".to_string(), collection_id(), outbound.id)]
        );
        assert_eq!(worker.broadcaster.broadcasts(), vec![replacement.signed_tx]);
        assert_eq!(worker.outbound.marked(), vec![replacement.id]);
    }

    #[tokio::test]
    async fn tick_fails_closed_when_broadcast_hash_differs_from_signed_hash() {
        let outbound = outbound_record(11, OutboundTxStatus::Signed);
        let worker = worker(
            FakePreparer::with_outcomes(vec![Ok(prepared_outcome(outbound.clone()))]),
            FakeOutboundRepository::default(),
            FakeBroadcaster::returning(tx_hash(0xbb)),
        );

        let error = worker.tick().await.unwrap_err();

        assert!(matches!(
            error,
            CollectionCollectorError::BroadcastHashMismatch {
                expected_tx_hash,
                actual_tx_hash,
                ..
            } if expected_tx_hash == tx_hash(0xaa) && actual_tx_hash == tx_hash(0xbb)
        ));
        assert_eq!(worker.broadcaster.broadcasts(), vec![signed_tx().raw_tx]);
        assert!(worker.outbound.marked().is_empty());
    }

    #[tokio::test]
    async fn invalid_worker_id_stops_before_claiming() {
        let worker = CollectionCollectorWorker::new(
            FakePreparer::with_outcomes(vec![Ok(PrepareCollectionJobOutcome::NoJob)]),
            FakeOutboundRepository::default(),
            FakeBroadcaster::returning(tx_hash(0xaa)),
            CollectionCollectorConfig::new(" "),
        );

        let error = worker.tick().await.unwrap_err();

        assert!(matches!(
            error,
            CollectionCollectorError::InvalidConfig {
                field: "worker_id",
                ..
            }
        ));
        assert!(worker.preparer.calls().is_empty());
    }

    #[test]
    fn spawn_loop_rejects_zero_poll_interval() {
        let worker = worker(
            FakePreparer::with_outcomes(vec![Ok(PrepareCollectionJobOutcome::NoJob)]),
            FakeOutboundRepository::default(),
            FakeBroadcaster::returning(tx_hash(0xaa)),
        );

        let error = spawn_collection_collector_loop(worker, Duration::ZERO).unwrap_err();

        assert!(matches!(
            error,
            CollectionCollectorError::InvalidConfig {
                field: "poll_interval",
                ..
            }
        ));
    }

    fn worker(
        preparer: FakePreparer,
        outbound: FakeOutboundRepository,
        broadcaster: FakeBroadcaster,
    ) -> CollectionCollectorWorker<FakePreparer, FakeOutboundRepository, FakeBroadcaster> {
        worker_with_config(
            preparer,
            outbound,
            broadcaster,
            CollectionCollectorConfig::new("collector-1")
                .with_replacement_stuck_after(Duration::from_secs(365 * 24 * 60 * 60)),
        )
    }

    fn worker_with_config(
        preparer: FakePreparer,
        outbound: FakeOutboundRepository,
        broadcaster: FakeBroadcaster,
        config: CollectionCollectorConfig,
    ) -> CollectionCollectorWorker<FakePreparer, FakeOutboundRepository, FakeBroadcaster> {
        CollectionCollectorWorker::new(preparer, outbound, broadcaster, config)
    }

    #[derive(Clone, Debug)]
    struct FakePreparer {
        state: Arc<Mutex<FakePreparerState>>,
    }

    #[derive(Debug)]
    struct FakePreparerState {
        outcomes: VecDeque<Result<PrepareCollectionJobOutcome, CollectionServiceError>>,
        replacement_outcomes: VecDeque<Result<PrepareCollectionJobOutcome, CollectionServiceError>>,
        calls: Vec<String>,
        replacement_calls: Vec<(String, Uuid, Uuid)>,
    }

    impl FakePreparer {
        fn with_outcomes(
            outcomes: Vec<Result<PrepareCollectionJobOutcome, CollectionServiceError>>,
        ) -> Self {
            Self {
                state: Arc::new(Mutex::new(FakePreparerState {
                    outcomes: VecDeque::from(outcomes),
                    replacement_outcomes: VecDeque::new(),
                    calls: Vec::new(),
                    replacement_calls: Vec::new(),
                })),
            }
        }

        fn with_replacement_outcomes(
            outcomes: Vec<Result<PrepareCollectionJobOutcome, CollectionServiceError>>,
            replacement_outcomes: Vec<Result<PrepareCollectionJobOutcome, CollectionServiceError>>,
        ) -> Self {
            Self {
                state: Arc::new(Mutex::new(FakePreparerState {
                    outcomes: VecDeque::from(outcomes),
                    replacement_outcomes: VecDeque::from(replacement_outcomes),
                    calls: Vec::new(),
                    replacement_calls: Vec::new(),
                })),
            }
        }

        fn calls(&self) -> Vec<String> {
            self.state
                .lock()
                .expect("fake preparer mutex poisoned")
                .calls
                .clone()
        }

        fn replacement_calls(&self) -> Vec<(String, Uuid, Uuid)> {
            self.state
                .lock()
                .expect("fake preparer mutex poisoned")
                .replacement_calls
                .clone()
        }
    }

    #[async_trait]
    impl CollectionJobPreparer for FakePreparer {
        async fn prepare_next_collection_job(
            &self,
            worker_id: &str,
        ) -> Result<PrepareCollectionJobOutcome, CollectionServiceError> {
            let mut state = self.state.lock().expect("fake preparer mutex poisoned");
            state.calls.push(worker_id.to_string());
            state
                .outcomes
                .pop_front()
                .expect("fake preparer outcomes exhausted")
        }
    }

    #[async_trait]
    impl CollectionJobReplacer for FakePreparer {
        async fn replace_stuck_collection_job(
            &self,
            worker_id: &str,
            job: ReceiptCheckableOutboundTx,
            _replacement_reason: &str,
        ) -> Result<PrepareCollectionJobOutcome, CollectionServiceError> {
            let mut state = self.state.lock().expect("fake preparer mutex poisoned");
            state.replacement_calls.push((
                worker_id.to_string(),
                job.collection_id,
                job.outbound.id,
            ));
            state
                .replacement_outcomes
                .pop_front()
                .expect("fake preparer replacement outcomes exhausted")
        }
    }

    #[derive(Clone, Debug, Default)]
    struct FakeOutboundRepository {
        state: Arc<Mutex<FakeOutboundState>>,
    }

    #[derive(Debug, Default)]
    struct FakeOutboundState {
        recoverable: Option<BroadcastableOutboundTx>,
        receipt_checkable: Option<ReceiptCheckableOutboundTx>,
        claim_calls: Vec<String>,
        receipt_claim_calls: Vec<String>,
        marked: Vec<Uuid>,
        confirmed: Vec<(Uuid, ChainBlockRef)>,
        failed: Vec<(Uuid, String)>,
    }

    impl FakeOutboundRepository {
        fn with_recoverable(recoverable: BroadcastableOutboundTx) -> Self {
            Self {
                state: Arc::new(Mutex::new(FakeOutboundState {
                    recoverable: Some(recoverable),
                    ..FakeOutboundState::default()
                })),
            }
        }

        fn with_receipt_checkable(receipt_checkable: ReceiptCheckableOutboundTx) -> Self {
            Self {
                state: Arc::new(Mutex::new(FakeOutboundState {
                    receipt_checkable: Some(receipt_checkable),
                    ..FakeOutboundState::default()
                })),
            }
        }

        fn claim_calls(&self) -> Vec<String> {
            self.state
                .lock()
                .expect("fake outbound mutex poisoned")
                .claim_calls
                .clone()
        }

        fn _receipt_claim_calls(&self) -> Vec<String> {
            self.state
                .lock()
                .expect("fake outbound mutex poisoned")
                .receipt_claim_calls
                .clone()
        }

        fn marked(&self) -> Vec<Uuid> {
            self.state
                .lock()
                .expect("fake outbound mutex poisoned")
                .marked
                .clone()
        }

        fn confirmed(&self) -> Vec<(Uuid, ChainBlockRef)> {
            self.state
                .lock()
                .expect("fake outbound mutex poisoned")
                .confirmed
                .clone()
        }

        fn failed(&self) -> Vec<(Uuid, String)> {
            self.state
                .lock()
                .expect("fake outbound mutex poisoned")
                .failed
                .clone()
        }
    }

    #[async_trait]
    impl OutboundRepository for FakeOutboundRepository {
        async fn reserve_nonce(
            &self,
            _chain_id: u64,
            _from_address: EvmAddress,
        ) -> Result<crate::db::repositories::ReservedNonce, RepositoryError> {
            unimplemented!("collector worker does not reserve nonce directly")
        }

        async fn insert_signed_tx(
            &self,
            _tx: NewSignedOutboundTx,
        ) -> Result<OutboundTxRecord, RepositoryError> {
            unimplemented!("collector worker does not insert signed tx directly")
        }

        async fn replace_signed_tx(
            &self,
            _old_tx_id: Uuid,
            _replacement_tx: NewSignedOutboundTx,
        ) -> Result<OutboundTxRecord, RepositoryError> {
            unimplemented!("collector worker does not replace signed txs yet")
        }

        async fn claim_signed_collect_tx_for_broadcast(
            &self,
            worker_id: &str,
        ) -> Result<Option<BroadcastableOutboundTx>, RepositoryError> {
            let mut state = self.state.lock().expect("fake outbound mutex poisoned");
            state.claim_calls.push(worker_id.to_string());
            Ok(state.recoverable.take())
        }

        async fn claim_broadcast_collect_tx_for_receipt(
            &self,
            worker_id: &str,
        ) -> Result<Option<ReceiptCheckableOutboundTx>, RepositoryError> {
            let mut state = self.state.lock().expect("fake outbound mutex poisoned");
            state.receipt_claim_calls.push(worker_id.to_string());
            Ok(state.receipt_checkable.take())
        }

        async fn mark_broadcast(&self, tx_id: Uuid) -> Result<OutboundTxRecord, RepositoryError> {
            self.state
                .lock()
                .expect("fake outbound mutex poisoned")
                .marked
                .push(tx_id);
            Ok(outbound_record(
                tx_id.as_u128(),
                OutboundTxStatus::Broadcast,
            ))
        }

        async fn mark_confirmed(
            &self,
            tx_id: Uuid,
            receipt_block: ChainBlockRef,
        ) -> Result<OutboundTxRecord, RepositoryError> {
            self.state
                .lock()
                .expect("fake outbound mutex poisoned")
                .confirmed
                .push((tx_id, receipt_block));
            Ok(outbound_record_with_receipt(
                tx_id.as_u128(),
                OutboundTxStatus::Confirmed,
                Some(receipt_block),
                None,
            ))
        }

        async fn mark_failed(
            &self,
            tx_id: Uuid,
            error: &str,
        ) -> Result<OutboundTxRecord, RepositoryError> {
            self.state
                .lock()
                .expect("fake outbound mutex poisoned")
                .failed
                .push((tx_id, error.to_string()));
            Ok(outbound_record_with_receipt(
                tx_id.as_u128(),
                OutboundTxStatus::Failed,
                None,
                Some(error.to_string()),
            ))
        }
    }

    #[derive(Clone, Debug)]
    struct FakeBroadcaster {
        returned_hash: TxHash,
        receipt: Option<TxReceipt>,
        broadcasts: Arc<Mutex<Vec<Vec<u8>>>>,
        receipt_queries: Arc<Mutex<Vec<TxHash>>>,
    }

    impl FakeBroadcaster {
        fn returning(returned_hash: TxHash) -> Self {
            Self {
                returned_hash,
                receipt: None,
                broadcasts: Arc::new(Mutex::new(Vec::new())),
                receipt_queries: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn with_receipt(mut self, receipt: Option<TxReceipt>) -> Self {
            self.receipt = receipt;
            self
        }

        fn broadcasts(&self) -> Vec<Vec<u8>> {
            self.broadcasts
                .lock()
                .expect("fake broadcaster mutex poisoned")
                .clone()
        }

        fn receipt_queries(&self) -> Vec<TxHash> {
            self.receipt_queries
                .lock()
                .expect("fake broadcaster mutex poisoned")
                .clone()
        }
    }

    #[async_trait]
    impl SignedTxBroadcaster for FakeBroadcaster {
        async fn broadcast_signed_tx(&self, signed_tx: Vec<u8>) -> Result<TxHash, ChainError> {
            self.broadcasts
                .lock()
                .expect("fake broadcaster mutex poisoned")
                .push(signed_tx);
            Ok(self.returned_hash)
        }
    }

    #[async_trait]
    impl TxReceiptReader for FakeBroadcaster {
        async fn transaction_receipt(
            &self,
            tx_hash: TxHash,
        ) -> Result<Option<TxReceipt>, ChainError> {
            self.receipt_queries
                .lock()
                .expect("fake broadcaster mutex poisoned")
                .push(tx_hash);
            Ok(self.receipt.clone())
        }
    }

    fn prepared_outcome(outbound: OutboundTxRecord) -> PrepareCollectionJobOutcome {
        PrepareCollectionJobOutcome::Prepared {
            collection: collection_record(Some(outbound.id)),
            outbound,
            signed_tx: signed_tx(),
        }
    }

    fn signed_tx() -> SignedTx {
        SignedTx {
            request_id: "collection-2-nonce-7".to_string(),
            chain_id: 1,
            nonce: 7,
            from: address(0x33),
            to: address(0x11),
            tx_hash: tx_hash(0xaa),
            raw_tx: b"signed-raw-transaction".to_vec(),
        }
    }

    fn collection_record(
        outbound_tx_id: Option<Uuid>,
    ) -> crate::db::repositories::CollectionRecord {
        crate::db::repositories::CollectionRecord {
            id: collection_id(),
            order_id: Uuid::from_u128(1),
            idempotency_key: "collect-1".to_string(),
            request_hash: "request-hash".to_string(),
            child_account_id: Uuid::from_u128(3),
            chain_id: 1,
            token_address: address(0x11),
            from_address: address(0x33),
            to_address: address(0x22),
            amount_raw: None,
            status: crate::db::repositories::CollectionRecordStatus::Transferring,
            outbound_tx_id,
            attempt_count: 1,
            locked_by: None,
            locked_until: None,
            error: None,
            created_at: now(),
            updated_at: now(),
        }
    }

    fn outbound_record(seed: u128, status: OutboundTxStatus) -> OutboundTxRecord {
        outbound_record_with_receipt(seed, status, None, None)
    }

    fn outbound_record_with_receipt(
        seed: u128,
        status: OutboundTxStatus,
        receipt_block: Option<ChainBlockRef>,
        error: Option<String>,
    ) -> OutboundTxRecord {
        OutboundTxRecord {
            id: Uuid::from_u128(seed),
            chain_id: 1,
            purpose: OutboundTxPurpose::Collect,
            from_address: address(0x33),
            to_address: address(0x22),
            nonce: RawAmount::from(7),
            tx_hash: tx_hash(0xaa),
            signed_tx: signed_tx().raw_tx,
            status,
            replacement_of: None,
            replacement_reason: None,
            broadcast_count: u32::from(matches!(status, OutboundTxStatus::Broadcast)),
            last_broadcast_at: matches!(status, OutboundTxStatus::Broadcast).then_some(now()),
            receipt_block,
            error,
            created_at: now(),
            updated_at: now(),
        }
    }

    fn collection_id() -> Uuid {
        Uuid::from_u128(2)
    }

    fn now() -> OffsetDateTime {
        datetime!(2026-05-03 12:00 UTC)
    }

    fn address(byte: u8) -> EvmAddress {
        EvmAddress::from_bytes([byte; 20])
    }

    fn tx_hash(byte: u8) -> TxHash {
        TxHash::from_bytes([byte; 32])
    }

    fn receipt(tx_hash: TxHash, status: TransactionStatus, block: ChainBlockRef) -> TxReceipt {
        TxReceipt {
            tx_hash,
            block,
            status,
            gas_used: Some(51_000),
        }
    }

    fn block_ref(number: u64) -> ChainBlockRef {
        ChainBlockRef::new(number, _block_hash(number as u8))
    }

    fn _block_hash(byte: u8) -> BlockHash {
        BlockHash::from_bytes([byte; 32])
    }
}
