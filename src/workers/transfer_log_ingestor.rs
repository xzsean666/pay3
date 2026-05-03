//! Background transfer-log ingestor loop.

use std::time::Duration;

use async_trait::async_trait;
use thiserror::Error;
use tokio::task::JoinHandle;

use crate::transfer_log_store::{
    PollOutcome, StreamId, TransferLogIngestor, TransferLogStoreError, TransferLogStoreResult,
};

#[async_trait]
pub trait TransferLogPoller: Send + Sync {
    async fn poll_once(&self, stream: StreamId) -> TransferLogStoreResult<PollOutcome>;
}

#[async_trait]
impl<T> TransferLogPoller for T
where
    T: TransferLogIngestor,
{
    async fn poll_once(&self, stream: StreamId) -> TransferLogStoreResult<PollOutcome> {
        TransferLogIngestor::poll_once(self, stream).await
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TransferLogIngestorLoopConfig {
    pub stream: StreamId,
    pub poll_interval: Duration,
}

impl TransferLogIngestorLoopConfig {
    pub const fn new(stream: StreamId, poll_interval: Duration) -> Self {
        Self {
            stream,
            poll_interval,
        }
    }

    fn validate(&self) -> Result<(), TransferLogIngestorLoopError> {
        if self.poll_interval.is_zero() {
            return Err(TransferLogIngestorLoopError::InvalidConfig {
                field: "poll_interval",
                message: "must be greater than zero".to_string(),
            });
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum TransferLogIngestorLoopError {
    #[error("invalid transfer log ingestor loop config {field}: {message}")]
    InvalidConfig {
        field: &'static str,
        message: String,
    },

    #[error(transparent)]
    TransferLogStore(#[from] TransferLogStoreError),
}

pub struct TransferLogIngestorLoop<P> {
    poller: P,
    config: TransferLogIngestorLoopConfig,
}

impl<P> TransferLogIngestorLoop<P> {
    pub fn new(
        poller: P,
        config: TransferLogIngestorLoopConfig,
    ) -> Result<Self, TransferLogIngestorLoopError> {
        config.validate()?;
        Ok(Self { poller, config })
    }
}

impl<P> TransferLogIngestorLoop<P>
where
    P: TransferLogPoller,
{
    pub async fn tick(&self) -> Result<PollOutcome, TransferLogIngestorLoopError> {
        Ok(self.poller.poll_once(self.config.stream).await?)
    }

    async fn run_forever(self) {
        let mut interval = tokio::time::interval(self.config.poll_interval);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            interval.tick().await;
            match self.tick().await {
                Ok(outcome) => log_poll_outcome(&outcome),
                Err(error) => {
                    let stream = self.config.stream;
                    tracing::error!(
                        chain_id = stream.chain_id,
                        token_address = %stream.token_address,
                        error = %error,
                        "transfer log ingestor tick failed"
                    );
                }
            }
        }
    }
}

pub fn spawn_transfer_log_ingestor_loop<P>(
    poller: P,
    config: TransferLogIngestorLoopConfig,
) -> Result<JoinHandle<()>, TransferLogIngestorLoopError>
where
    P: TransferLogPoller + 'static,
{
    let worker = TransferLogIngestorLoop::new(poller, config)?;
    Ok(tokio::spawn(worker.run_forever()))
}

fn log_poll_outcome(outcome: &PollOutcome) {
    match outcome {
        PollOutcome::Idle { cursor } => {
            let stream = cursor.stream;
            tracing::debug!(
                chain_id = stream.chain_id,
                token_address = %stream.token_address,
                next_block = cursor.next_block,
                last_completed_block = cursor.last_completed_block,
                reorg_epoch = cursor.reorg_epoch,
                "transfer log ingestor idle"
            );
        }
        PollOutcome::Advanced {
            stream,
            from_block,
            to_block,
            log_count,
            cursor,
        } => {
            tracing::info!(
                chain_id = stream.chain_id,
                token_address = %stream.token_address,
                from_block,
                to_block,
                log_count,
                next_block = cursor.next_block,
                reorg_epoch = cursor.reorg_epoch,
                "transfer log ingestor advanced"
            );
        }
        PollOutcome::Rewound {
            stream,
            from_block,
            cursor,
        } => {
            tracing::warn!(
                chain_id = stream.chain_id,
                token_address = %stream.token_address,
                from_block,
                next_block = cursor.next_block,
                reorg_epoch = cursor.reorg_epoch,
                "transfer log ingestor rewound after reorg"
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
    use time::OffsetDateTime;

    use super::*;
    use crate::{
        domain::{BlockHash, EvmAddress},
        transfer_log_store::{ScanTargetMode, TransferLogCursor},
    };

    #[tokio::test]
    async fn tick_polls_configured_stream() {
        let cursor = cursor(10);
        let poller = FakePoller::with_outcomes(vec![Ok(PollOutcome::Idle {
            cursor: cursor.clone(),
        })]);
        let worker = TransferLogIngestorLoop::new(poller.clone(), config()).unwrap();

        let outcome = worker.tick().await.unwrap();

        assert_eq!(outcome, PollOutcome::Idle { cursor });
        assert_eq!(poller.calls(), vec![stream()]);
    }

    #[tokio::test]
    async fn tick_propagates_poll_errors() {
        let poller = FakePoller::with_outcomes(vec![Err(TransferLogStoreError::NotReady {
            reason: "capacity probe failed".to_string(),
        })]);
        let worker = TransferLogIngestorLoop::new(poller, config()).unwrap();

        let error = worker.tick().await.unwrap_err();

        assert!(matches!(
            error,
            TransferLogIngestorLoopError::TransferLogStore(TransferLogStoreError::NotReady { .. })
        ));
    }

    #[test]
    fn zero_poll_interval_is_rejected() {
        let error = TransferLogIngestorLoop::new(
            FakePoller::with_outcomes(Vec::new()),
            TransferLogIngestorLoopConfig::new(stream(), Duration::ZERO),
        )
        .err()
        .expect("zero interval should be invalid");

        assert!(matches!(
            error,
            TransferLogIngestorLoopError::InvalidConfig {
                field: "poll_interval",
                ..
            }
        ));
    }

    #[derive(Clone, Debug)]
    struct FakePoller {
        state: Arc<Mutex<FakePollerState>>,
    }

    #[derive(Debug)]
    struct FakePollerState {
        outcomes: VecDeque<TransferLogStoreResult<PollOutcome>>,
        calls: Vec<StreamId>,
    }

    impl FakePoller {
        fn with_outcomes(outcomes: Vec<TransferLogStoreResult<PollOutcome>>) -> Self {
            Self {
                state: Arc::new(Mutex::new(FakePollerState {
                    outcomes: VecDeque::from(outcomes),
                    calls: Vec::new(),
                })),
            }
        }

        fn calls(&self) -> Vec<StreamId> {
            self.state
                .lock()
                .expect("fake poller lock poisoned")
                .calls
                .clone()
        }
    }

    #[async_trait]
    impl TransferLogPoller for FakePoller {
        async fn poll_once(&self, stream: StreamId) -> TransferLogStoreResult<PollOutcome> {
            let mut state = self.state.lock().expect("fake poller lock poisoned");
            state.calls.push(stream);
            state
                .outcomes
                .pop_front()
                .expect("fake poller outcome missing")
        }
    }

    fn config() -> TransferLogIngestorLoopConfig {
        TransferLogIngestorLoopConfig::new(stream(), Duration::from_millis(50))
    }

    fn cursor(next_block: u64) -> TransferLogCursor {
        TransferLogCursor {
            stream: stream(),
            start_block: 1,
            next_block,
            last_completed_block: next_block.checked_sub(1),
            last_completed_hash: Some(BlockHash::from_bytes([0x11; 32])),
            target_mode: ScanTargetMode::LatestMinusConfirmations(12),
            reorg_epoch: 0,
            last_reorg_from: None,
            last_reorg_at: None,
            writer_epoch: 1,
            updated_at: OffsetDateTime::from_unix_timestamp(1_777_777_777).unwrap(),
        }
    }

    fn stream() -> StreamId {
        StreamId::new(1, EvmAddress::from_bytes([0x11; 20]))
    }
}
