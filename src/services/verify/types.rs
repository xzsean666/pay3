use thiserror::Error;
use uuid::Uuid;

use crate::{
    chain::ChainError,
    db::repositories::RepositoryError,
    domain::{OrderStatusError, RawAmount},
    services::payments::PaymentMatchingError,
    transfer_log_store::TransferLogStoreError,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ManualVerifyConfig {
    pub max_logs_per_order: usize,
    pub min_confirmations: u64,
}

impl ManualVerifyConfig {
    pub const fn new(max_logs_per_order: usize, min_confirmations: u64) -> Self {
        Self {
            max_logs_per_order,
            min_confirmations,
        }
    }

    pub fn validate(self) -> Result<(), ManualVerifyError> {
        if self.max_logs_per_order == 0 {
            Err(ManualVerifyError::InvalidConfig {
                field: "max_logs_per_order",
            })
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManualVerifyResult {
    pub order_id: Uuid,
    pub status: ManualVerifyStatus,
    pub matched_payments: u64,
    pub paid_amount_raw: RawAmount,
    pub confirmations: u64,
    pub complete_to_block: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManualVerifyStatus {
    Confirmed,
    Confirming,
    NoCoverage,
}

#[derive(Debug, Error)]
pub enum ManualVerifyError {
    #[error("invalid manual verify config: {field} must be greater than zero")]
    InvalidConfig { field: &'static str },

    #[error("order not found: {order_id}")]
    OrderNotFound { order_id: Uuid },

    #[error("transfer log coverage is insufficient for order {order_id}")]
    CoverageInsufficient { order_id: Uuid },

    #[error("too many transfer logs for order {order_id}: limit {limit}")]
    LogLimitExceeded { order_id: Uuid, limit: usize },

    #[error(transparent)]
    Repository(#[from] RepositoryError),

    #[error(transparent)]
    TransferLogStore(#[from] TransferLogStoreError),

    #[error(transparent)]
    Chain(#[from] ChainError),

    #[error(transparent)]
    PaymentMatching(#[from] PaymentMatchingError),

    #[error(transparent)]
    OrderStatus(#[from] OrderStatusError),
}
