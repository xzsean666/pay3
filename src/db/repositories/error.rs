use std::num::TryFromIntError;

use thiserror::Error;
use uuid::Uuid;

use crate::domain::{DerivationSegmentError, EvmAddress, TxHash};

pub type RepositoryResult<T> = Result<T, RepositoryError>;

#[derive(Debug, Error)]
pub enum RepositoryError {
    #[error("database query failed: {0}")]
    Database(#[from] sqlx::Error),

    #[error("invalid argument {field}: {message}")]
    InvalidArgument {
        field: &'static str,
        message: String,
    },

    #[error("{resource} idempotency conflict for key {key}")]
    IdempotencyConflict {
        resource: &'static str,
        key: String,
        existing_id: Option<Uuid>,
    },

    #[error("{entity} not found: {key}")]
    NotFound { entity: &'static str, key: String },

    #[error("scan cursor lease not acquired by {worker_id} for {chain_id}/{token_address}")]
    LeaseNotAcquired {
        chain_id: u64,
        token_address: EvmAddress,
        worker_id: String,
    },

    #[error(
        "scan cursor CAS mismatch for {chain_id}/{token_address}: expected block {expected_last_scanned_block} and kv epoch {expected_seen_kv_reorg_epoch}"
    )]
    CursorCasMismatch {
        chain_id: u64,
        token_address: EvmAddress,
        worker_id: String,
        expected_last_scanned_block: u64,
        actual_last_scanned_block: u64,
        expected_seen_kv_reorg_epoch: u64,
        actual_seen_kv_reorg_epoch: u64,
        actual_lease_owner: Option<String>,
    },

    #[error("scan cursor not found for {chain_id}/{token_address}")]
    CursorNotFound {
        chain_id: u64,
        token_address: EvmAddress,
    },

    #[error("stale payment match for order {order_id}, tx {tx_hash}, log_index {log_index}")]
    StalePaymentMatch {
        order_id: Uuid,
        tx_hash: TxHash,
        log_index: u64,
    },

    #[error("orders not found for recompute: {order_ids:?}")]
    OrderNotFoundForRecompute { order_ids: Vec<Uuid> },

    #[error("wallet derivation segment exhausted")]
    DerivationExhausted,

    #[error("integer value out of range for {field}")]
    IntegerOutOfRange {
        field: &'static str,
        #[source]
        source: TryFromIntError,
    },

    #[error("invalid database value in {column}: {value} ({reason})")]
    InvalidDbValue {
        column: &'static str,
        value: String,
        reason: String,
    },

    #[error("invalid persisted state: {message}")]
    InvalidPersistedState { message: String },

    #[error("repository invariant violation: {reason}")]
    InvariantViolation { reason: String },
}

impl RepositoryError {
    pub fn idempotency_conflict(
        resource: &'static str,
        key: impl Into<String>,
        existing_id: Option<Uuid>,
    ) -> Self {
        Self::IdempotencyConflict {
            resource,
            key: key.into(),
            existing_id,
        }
    }

    pub fn not_found(entity: &'static str, key: impl Into<String>) -> Self {
        Self::NotFound {
            entity,
            key: key.into(),
        }
    }

    pub fn integer_out_of_range(field: &'static str, source: TryFromIntError) -> Self {
        Self::IntegerOutOfRange { field, source }
    }

    pub fn invalid_argument(field: &'static str, message: impl Into<String>) -> Self {
        Self::InvalidArgument {
            field,
            message: message.into(),
        }
    }

    pub fn invalid_db_value(
        column: &'static str,
        value: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self::InvalidDbValue {
            column,
            value: value.into(),
            reason: reason.into(),
        }
    }

    pub fn invariant_violation(reason: impl Into<String>) -> Self {
        Self::InvariantViolation {
            reason: reason.into(),
        }
    }

    pub fn invalid_persisted_state(message: impl Into<String>) -> Self {
        Self::InvalidPersistedState {
            message: message.into(),
        }
    }
}

impl From<DerivationSegmentError> for RepositoryError {
    fn from(error: DerivationSegmentError) -> Self {
        match error {
            DerivationSegmentError::Exhausted => Self::DerivationExhausted,
            DerivationSegmentError::IndexOutOfRange { field, value, max } => {
                Self::invalid_db_value(
                    field,
                    value.to_string(),
                    format!("derivation index exceeds maximum {max}"),
                )
            }
        }
    }
}
