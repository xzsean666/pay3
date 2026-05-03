//! Collection collector worker tick.

use async_trait::async_trait;
use thiserror::Error;
use uuid::Uuid;

use crate::{
    chain::ChainError,
    db::repositories::{OutboundRepository, OutboundTxRecord, RepositoryError},
    domain::TxHash,
    services::{
        collections::{CollectionService, CollectionServiceError, PrepareCollectionJobOutcome},
        orders::IdGenerator,
    },
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CollectionCollectorConfig {
    pub worker_id: String,
}

impl CollectionCollectorConfig {
    pub fn new(worker_id: impl Into<String>) -> Self {
        Self {
            worker_id: worker_id.into(),
        }
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
    P: CollectionJobPreparer,
    O: OutboundRepository,
    B: SignedTxBroadcaster,
{
    pub async fn tick(&self) -> Result<CollectionCollectorTickOutcome, CollectionCollectorError> {
        self.config.validate()?;

        let outcome = self
            .preparer
            .prepare_next_collection_job(self.config.worker_id.trim())
            .await?;
        let PrepareCollectionJobOutcome::Prepared {
            collection,
            outbound,
            signed_tx,
        } = outcome
        else {
            return Ok(CollectionCollectorTickOutcome::NoJob);
        };

        let broadcast_tx_hash = self
            .broadcaster
            .broadcast_signed_tx(signed_tx.raw_tx.clone())
            .await?;
        if broadcast_tx_hash != outbound.tx_hash {
            return Err(CollectionCollectorError::BroadcastHashMismatch {
                outbound_tx_id: outbound.id,
                expected_tx_hash: outbound.tx_hash,
                actual_tx_hash: broadcast_tx_hash,
            });
        }

        let outbound = self.outbound.mark_broadcast(outbound.id).await?;
        Ok(CollectionCollectorTickOutcome::Broadcast {
            collection_id: collection.id,
            outbound,
        })
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

    fn worker(
        preparer: FakePreparer,
        outbound: FakeOutboundRepository,
        broadcaster: FakeBroadcaster,
    ) -> CollectionCollectorWorker<FakePreparer, FakeOutboundRepository, FakeBroadcaster> {
        CollectionCollectorWorker::new(
            preparer,
            outbound,
            broadcaster,
            CollectionCollectorConfig::new("collector-1"),
        )
    }

    #[derive(Clone, Debug)]
    struct FakePreparer {
        state: Arc<Mutex<FakePreparerState>>,
    }

    #[derive(Debug)]
    struct FakePreparerState {
        outcomes: VecDeque<Result<PrepareCollectionJobOutcome, CollectionServiceError>>,
        calls: Vec<String>,
    }

    impl FakePreparer {
        fn with_outcomes(
            outcomes: Vec<Result<PrepareCollectionJobOutcome, CollectionServiceError>>,
        ) -> Self {
            Self {
                state: Arc::new(Mutex::new(FakePreparerState {
                    outcomes: VecDeque::from(outcomes),
                    calls: Vec::new(),
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

    #[derive(Clone, Debug, Default)]
    struct FakeOutboundRepository {
        marked: Arc<Mutex<Vec<Uuid>>>,
    }

    impl FakeOutboundRepository {
        fn marked(&self) -> Vec<Uuid> {
            self.marked
                .lock()
                .expect("fake outbound mutex poisoned")
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

        async fn mark_broadcast(&self, tx_id: Uuid) -> Result<OutboundTxRecord, RepositoryError> {
            self.marked
                .lock()
                .expect("fake outbound mutex poisoned")
                .push(tx_id);
            Ok(outbound_record(11, OutboundTxStatus::Broadcast))
        }

        async fn mark_confirmed(
            &self,
            _tx_id: Uuid,
            _receipt_block: ChainBlockRef,
        ) -> Result<OutboundTxRecord, RepositoryError> {
            unimplemented!("confirmation sweep is not part of this worker tick yet")
        }

        async fn mark_failed(
            &self,
            _tx_id: Uuid,
            _error: &str,
        ) -> Result<OutboundTxRecord, RepositoryError> {
            unimplemented!("failure handling is not part of this worker tick yet")
        }
    }

    #[derive(Clone, Debug)]
    struct FakeBroadcaster {
        returned_hash: TxHash,
        broadcasts: Arc<Mutex<Vec<Vec<u8>>>>,
    }

    impl FakeBroadcaster {
        fn returning(returned_hash: TxHash) -> Self {
            Self {
                returned_hash,
                broadcasts: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn broadcasts(&self) -> Vec<Vec<u8>> {
            self.broadcasts
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
            receipt_block: None,
            error: None,
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

    fn _block_hash(byte: u8) -> BlockHash {
        BlockHash::from_bytes([byte; 32])
    }
}
