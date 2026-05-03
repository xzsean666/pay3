use std::collections::BTreeSet;

use async_trait::async_trait;
use sqlx::PgPool;
use uuid::Uuid;

use super::{
    RepositoryError,
    payment_recompute::recompute_orders_in_tx,
    payment_records::{payment_records_for_order_tx, upsert_matched_payment_tx},
    types::{MatchedPaymentInput, PaymentRecord},
};

#[async_trait]
pub trait VerifiedPaymentRecorder: Send + Sync {
    /// Persists newly matched payments and returns the canonical payment set
    /// for the order after the write.
    async fn record_verified_payments(
        &self,
        order_id: Uuid,
        matched_payments: Vec<MatchedPaymentInput>,
    ) -> Result<Vec<PaymentRecord>, RepositoryError>;
}

#[derive(Clone)]
pub struct PgVerifiedPaymentRecorder {
    pub pool: PgPool,
}

impl PgVerifiedPaymentRecorder {
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl VerifiedPaymentRecorder for PgVerifiedPaymentRecorder {
    async fn record_verified_payments(
        &self,
        order_id: Uuid,
        matched_payments: Vec<MatchedPaymentInput>,
    ) -> Result<Vec<PaymentRecord>, RepositoryError> {
        if matched_payments
            .iter()
            .any(|payment| payment.order_id != order_id)
        {
            return Err(RepositoryError::invariant_violation(
                "manual verify recorder received payment for a different order",
            ));
        }

        let mut tx = self.pool.begin().await.map_err(RepositoryError::Database)?;
        for payment in &matched_payments {
            upsert_matched_payment_tx(&mut tx, payment.chain_id, payment.token_address, payment)
                .await?;
        }

        recompute_orders_in_tx(&mut tx, BTreeSet::from([order_id])).await?;
        let records = payment_records_for_order_tx(&mut tx, order_id).await?;
        tx.commit().await.map_err(RepositoryError::Database)?;

        Ok(records)
    }
}
