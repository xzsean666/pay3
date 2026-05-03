use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pay3::{
    chain::{ChainBlock, ChainError, ChainHeaderReader},
    db::repositories::{
        AllocatedDerivation, CreateOrderCommand, CreateOrderOutcome, MatchedPaymentInput,
        OrderRecord, OrderRepository, OrderView, PaymentRecord, RepositoryError,
    },
    domain::ChainBlockRef,
    services::{orders::Clock, verify::VerifiedPaymentRecorder},
    transfer_log_store::{
        LogPageToken, LogsPage, ScanTargetMode, StoredBlockHeader, StoredTransferLog, StreamId,
        TransferLogCursor, TransferLogReader, TransferLogStoreError,
    },
};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use super::fixtures::{block_hash, now, payment_record_from_match};

#[derive(Clone)]
pub struct FakeOrderRepository {
    view: Arc<Mutex<Option<OrderView>>>,
}

impl FakeOrderRepository {
    pub fn missing() -> Self {
        Self {
            view: Arc::new(Mutex::new(None)),
        }
    }

    pub fn with_view(view: OrderView) -> Self {
        Self {
            view: Arc::new(Mutex::new(Some(view))),
        }
    }
}

#[async_trait]
impl OrderRepository for FakeOrderRepository {
    async fn allocate_derivation_segment(
        &self,
        _cursor_id: &str,
    ) -> Result<AllocatedDerivation, RepositoryError> {
        unimplemented!("manual verify does not allocate wallet segments")
    }

    async fn create_order_idempotent(
        &self,
        _command: CreateOrderCommand,
    ) -> Result<CreateOrderOutcome, RepositoryError> {
        unimplemented!("manual verify does not create orders")
    }

    async fn get_order(&self, _id: Uuid) -> Result<Option<OrderRecord>, RepositoryError> {
        Ok(self
            .view
            .lock()
            .unwrap()
            .as_ref()
            .map(|view| view.order.clone()))
    }

    async fn get_order_view(&self, id: Uuid) -> Result<Option<OrderView>, RepositoryError> {
        Ok(self
            .view
            .lock()
            .unwrap()
            .as_ref()
            .filter(|view| view.order.id == id)
            .cloned())
    }

    async fn get_order_by_external_id(
        &self,
        _external_id: &str,
    ) -> Result<Option<OrderRecord>, RepositoryError> {
        unimplemented!("manual verify does not query external ids")
    }
}

#[derive(Clone, Default)]
pub struct FakeRecorder {
    calls: Arc<Mutex<Vec<RecorderCall>>>,
}

impl FakeRecorder {
    pub fn calls(&self) -> Vec<RecorderCall> {
        self.calls.lock().unwrap().clone()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecorderCall {
    pub order_id: Uuid,
    pub payments: Vec<MatchedPaymentInput>,
}

#[async_trait]
impl VerifiedPaymentRecorder for FakeRecorder {
    async fn record_verified_payments(
        &self,
        order_id: Uuid,
        matched_payments: Vec<MatchedPaymentInput>,
    ) -> Result<Vec<PaymentRecord>, RepositoryError> {
        self.calls.lock().unwrap().push(RecorderCall {
            order_id,
            payments: matched_payments.clone(),
        });

        Ok(matched_payments
            .into_iter()
            .map(payment_record_from_match)
            .collect())
    }
}

#[derive(Clone)]
pub struct FakeLogReader {
    stream: StreamId,
    logs: Arc<Vec<StoredTransferLog>>,
    last_completed_block: Option<u64>,
    calls: Arc<Mutex<Vec<LogReaderCall>>>,
}

impl FakeLogReader {
    pub fn new(
        stream: StreamId,
        logs: Vec<StoredTransferLog>,
        last_completed_block: Option<u64>,
    ) -> Self {
        Self {
            stream,
            logs: Arc::new(logs),
            last_completed_block,
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn calls(&self) -> Vec<LogReaderCall> {
        self.calls.lock().unwrap().clone()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LogReaderCall {
    Cursor(StreamId),
    LogsInRange {
        stream: StreamId,
        from: u64,
        to: u64,
        max_logs: usize,
    },
}

#[async_trait]
impl TransferLogReader for FakeLogReader {
    async fn cursor(&self, stream: StreamId) -> Result<TransferLogCursor, TransferLogStoreError> {
        self.calls
            .lock()
            .unwrap()
            .push(LogReaderCall::Cursor(stream));
        Ok(TransferLogCursor {
            stream: self.stream,
            start_block: 1,
            next_block: self
                .last_completed_block
                .map(|block| block.saturating_add(1))
                .unwrap_or(1),
            last_completed_block: self.last_completed_block,
            last_completed_hash: None,
            target_mode: ScanTargetMode::SafeTag,
            reorg_epoch: 0,
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
        stream: StreamId,
        from: u64,
        to: u64,
        max_logs: usize,
    ) -> Result<Vec<StoredTransferLog>, TransferLogStoreError> {
        self.calls.lock().unwrap().push(LogReaderCall::LogsInRange {
            stream,
            from,
            to,
            max_logs,
        });
        Ok(self
            .logs
            .iter()
            .filter(|log| log.block_number >= from && log.block_number <= to)
            .take(max_logs)
            .cloned()
            .collect())
    }

    async fn logs_page(
        &self,
        _stream: StreamId,
        _after: Option<LogPageToken>,
        _limit: usize,
    ) -> Result<LogsPage, TransferLogStoreError> {
        unimplemented!("manual verify reads a bounded order window, not scanner pages")
    }
}

#[derive(Clone, Copy)]
pub struct FakeHeadReader {
    pub head_number: u64,
}

#[async_trait]
impl ChainHeaderReader for FakeHeadReader {
    async fn latest_head(&self) -> Result<ChainBlockRef, ChainError> {
        Ok(ChainBlockRef::new(self.head_number, block_hash(0xf0)))
    }

    async fn safe_head(&self) -> Result<ChainBlockRef, ChainError> {
        self.latest_head().await
    }

    async fn finalized_head(&self) -> Result<ChainBlockRef, ChainError> {
        self.latest_head().await
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

#[derive(Clone, Copy)]
pub struct TestClock;

impl Clock for TestClock {
    fn now(&self) -> OffsetDateTime {
        now() + Duration::seconds(20)
    }
}
