use std::collections::BTreeSet;

use async_trait::async_trait;
use sqlx::{PgPool, Row};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::domain::EvmAddress;

use super::{
    error::RepositoryError,
    payment_recompute::recompute_orders_in_tx,
    payment_records::{i64_to_u64, u64_to_i64, upsert_matched_payment_tx},
    types::{CommitScannedBatch, PaymentRecord, ScanCursorLease},
};

#[async_trait]
pub trait PaymentRepository: Send + Sync {
    async fn claim_scan_range(
        &self,
        worker_id: &str,
        chain_id: u64,
        token_address: EvmAddress,
        lease_until: OffsetDateTime,
    ) -> Result<Option<ScanCursorLease>, RepositoryError>;

    async fn commit_scanned_batch(
        &self,
        batch: CommitScannedBatch,
    ) -> Result<Vec<PaymentRecord>, RepositoryError>;

    async fn recompute_orders(&self, order_ids: Vec<Uuid>) -> Result<(), RepositoryError>;

    async fn handle_kv_reorg_epoch(
        &self,
        chain_id: u64,
        token_address: EvmAddress,
        epoch: u64,
        last_reorg_from: u64,
    ) -> Result<(), RepositoryError>;
}

#[derive(Clone)]
pub struct PgPaymentRepository {
    pub pool: PgPool,
}

impl PgPaymentRepository {
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl PaymentRepository for PgPaymentRepository {
    async fn claim_scan_range(
        &self,
        worker_id: &str,
        chain_id: u64,
        token_address: EvmAddress,
        lease_until: OffsetDateTime,
    ) -> Result<Option<ScanCursorLease>, RepositoryError> {
        let chain_id_i64 = u64_to_i64(chain_id, "chain_id")?;
        let token_address_hex = token_address.to_lower_hex();
        let now = OffsetDateTime::now_utc();

        let mut tx = self.pool.begin().await.map_err(RepositoryError::Database)?;
        let row = sqlx::query(
            r#"
            SELECT last_scanned_block, seen_kv_reorg_epoch, lease_owner, lease_until
            FROM chain_cursors
            WHERE chain_id = $1 AND token_address = $2
            FOR UPDATE
            "#,
        )
        .bind(chain_id_i64)
        .bind(&token_address_hex)
        .fetch_optional(&mut *tx)
        .await
        .map_err(RepositoryError::Database)?
        .ok_or_else(|| cursor_not_found(chain_id, token_address))?;

        let current_owner: Option<String> = row
            .try_get("lease_owner")
            .map_err(RepositoryError::Database)?;
        let current_lease_until: Option<OffsetDateTime> = row
            .try_get("lease_until")
            .map_err(RepositoryError::Database)?;

        let held_by_other_live_worker = current_owner
            .as_deref()
            .is_some_and(|owner| owner != worker_id)
            && current_lease_until.is_some_and(|until| until > now);

        if held_by_other_live_worker {
            tx.commit().await.map_err(RepositoryError::Database)?;
            return Ok(None);
        }

        let row = sqlx::query(
            r#"
            UPDATE chain_cursors
            SET lease_owner = $3,
                lease_until = $4,
                updated_at = now()
            WHERE chain_id = $1 AND token_address = $2
            RETURNING last_scanned_block, seen_kv_reorg_epoch
            "#,
        )
        .bind(chain_id_i64)
        .bind(&token_address_hex)
        .bind(worker_id)
        .bind(lease_until)
        .fetch_one(&mut *tx)
        .await
        .map_err(RepositoryError::Database)?;

        let last_scanned_block = i64_to_u64(
            row.try_get("last_scanned_block")
                .map_err(RepositoryError::Database)?,
            "last_scanned_block",
        )?;
        let seen_kv_reorg_epoch = i64_to_u64(
            row.try_get("seen_kv_reorg_epoch")
                .map_err(RepositoryError::Database)?,
            "seen_kv_reorg_epoch",
        )?;

        tx.commit().await.map_err(RepositoryError::Database)?;

        Ok(Some(ScanCursorLease {
            chain_id,
            token_address,
            lease_owner: worker_id.to_owned(),
            lease_until,
            last_scanned_block,
            seen_kv_reorg_epoch,
        }))
    }

    async fn commit_scanned_batch(
        &self,
        batch: CommitScannedBatch,
    ) -> Result<Vec<PaymentRecord>, RepositoryError> {
        if batch.complete_to_block < batch.expected_last_scanned_block {
            return Err(invalid_argument(
                "complete_to_block",
                "complete_to_block must not be lower than expected_last_scanned_block",
            ));
        }

        let chain_id_i64 = u64_to_i64(batch.chain_id, "chain_id")?;
        let token_address_hex = batch.token_address.to_lower_hex();
        let expected_last_scanned_block_i64 = u64_to_i64(
            batch.expected_last_scanned_block,
            "expected_last_scanned_block",
        )?;
        let expected_seen_kv_reorg_epoch_i64 = u64_to_i64(
            batch.expected_seen_kv_reorg_epoch,
            "expected_seen_kv_reorg_epoch",
        )?;
        let complete_to_block_i64 = u64_to_i64(batch.complete_to_block, "complete_to_block")?;

        let mut tx = self.pool.begin().await.map_err(RepositoryError::Database)?;
        let cursor = sqlx::query(
            r#"
            SELECT last_scanned_block, seen_kv_reorg_epoch, lease_owner
            FROM chain_cursors
            WHERE chain_id = $1 AND token_address = $2
            FOR UPDATE
            "#,
        )
        .bind(chain_id_i64)
        .bind(&token_address_hex)
        .fetch_optional(&mut *tx)
        .await
        .map_err(RepositoryError::Database)?
        .ok_or_else(|| cursor_not_found(batch.chain_id, batch.token_address))?;

        let actual_last_scanned_block_i64: i64 = cursor
            .try_get("last_scanned_block")
            .map_err(RepositoryError::Database)?;
        let actual_seen_kv_reorg_epoch_i64: i64 = cursor
            .try_get("seen_kv_reorg_epoch")
            .map_err(RepositoryError::Database)?;
        let actual_lease_owner: Option<String> = cursor
            .try_get("lease_owner")
            .map_err(RepositoryError::Database)?;

        if actual_lease_owner.as_deref() != Some(batch.worker_id.as_str())
            || actual_last_scanned_block_i64 != expected_last_scanned_block_i64
            || actual_seen_kv_reorg_epoch_i64 != expected_seen_kv_reorg_epoch_i64
        {
            let actual_last_scanned_block =
                i64_to_u64(actual_last_scanned_block_i64, "last_scanned_block")?;
            let actual_seen_kv_reorg_epoch =
                i64_to_u64(actual_seen_kv_reorg_epoch_i64, "seen_kv_reorg_epoch")?;

            return Err(RepositoryError::CursorCasMismatch {
                chain_id: batch.chain_id,
                token_address: batch.token_address,
                worker_id: batch.worker_id,
                expected_last_scanned_block: batch.expected_last_scanned_block,
                actual_last_scanned_block,
                expected_seen_kv_reorg_epoch: batch.expected_seen_kv_reorg_epoch,
                actual_seen_kv_reorg_epoch,
                actual_lease_owner,
            });
        }

        let mut affected_order_ids = BTreeSet::new();
        let mut records = Vec::with_capacity(batch.matched_payments.len());

        for payment in &batch.matched_payments {
            let record =
                upsert_matched_payment_tx(&mut tx, batch.chain_id, batch.token_address, payment)
                    .await?;
            affected_order_ids.insert(record.order_id);
            records.push(record);
        }

        recompute_orders_in_tx(&mut tx, affected_order_ids).await?;

        sqlx::query(
            r#"
            UPDATE chain_cursors
            SET last_scanned_block = $3,
                lease_owner = NULL,
                lease_until = NULL,
                updated_at = now()
            WHERE chain_id = $1
              AND token_address = $2
              AND lease_owner = $4
              AND last_scanned_block = $5
              AND seen_kv_reorg_epoch = $6
            "#,
        )
        .bind(chain_id_i64)
        .bind(&token_address_hex)
        .bind(complete_to_block_i64)
        .bind(&batch.worker_id)
        .bind(expected_last_scanned_block_i64)
        .bind(expected_seen_kv_reorg_epoch_i64)
        .execute(&mut *tx)
        .await
        .map_err(RepositoryError::Database)?;

        tx.commit().await.map_err(RepositoryError::Database)?;
        Ok(records)
    }

    async fn recompute_orders(&self, order_ids: Vec<Uuid>) -> Result<(), RepositoryError> {
        let mut tx = self.pool.begin().await.map_err(RepositoryError::Database)?;
        recompute_orders_in_tx(&mut tx, order_ids.into_iter().collect()).await?;
        tx.commit().await.map_err(RepositoryError::Database)
    }

    async fn handle_kv_reorg_epoch(
        &self,
        chain_id: u64,
        token_address: EvmAddress,
        epoch: u64,
        last_reorg_from: u64,
    ) -> Result<(), RepositoryError> {
        let chain_id_i64 = u64_to_i64(chain_id, "chain_id")?;
        let token_address_hex = token_address.to_lower_hex();
        let epoch_i64 = u64_to_i64(epoch, "epoch")?;
        let last_reorg_from_i64 = u64_to_i64(last_reorg_from, "last_reorg_from")?;
        let rewind_to_i64 = u64_to_i64(last_reorg_from.saturating_sub(1), "rewind_to")?;

        let mut tx = self.pool.begin().await.map_err(RepositoryError::Database)?;
        let cursor = sqlx::query(
            r#"
            SELECT last_scanned_block, seen_kv_reorg_epoch
            FROM chain_cursors
            WHERE chain_id = $1 AND token_address = $2
            FOR UPDATE
            "#,
        )
        .bind(chain_id_i64)
        .bind(&token_address_hex)
        .fetch_optional(&mut *tx)
        .await
        .map_err(RepositoryError::Database)?
        .ok_or_else(|| cursor_not_found(chain_id, token_address))?;

        let seen_epoch: i64 = cursor
            .try_get("seen_kv_reorg_epoch")
            .map_err(RepositoryError::Database)?;
        if seen_epoch >= epoch_i64 {
            tx.commit().await.map_err(RepositoryError::Database)?;
            return Ok(());
        }

        let affected_rows = sqlx::query(
            r#"
            UPDATE payments
            SET chain_status = 'orphaned',
                updated_at = now()
            WHERE chain_id = $1
              AND token_address = $2
              AND block_number >= $3
              AND chain_status <> 'orphaned'
            RETURNING order_id
            "#,
        )
        .bind(chain_id_i64)
        .bind(&token_address_hex)
        .bind(last_reorg_from_i64)
        .fetch_all(&mut *tx)
        .await
        .map_err(RepositoryError::Database)?;

        let affected_order_ids = affected_rows
            .into_iter()
            .map(|row| row.try_get("order_id").map_err(RepositoryError::Database))
            .collect::<Result<BTreeSet<Uuid>, RepositoryError>>()?;

        sqlx::query(
            r#"
            UPDATE chain_cursors
            SET last_scanned_block = LEAST(last_scanned_block, $3),
                seen_kv_reorg_epoch = $4,
                lease_owner = NULL,
                lease_until = NULL,
                updated_at = now()
            WHERE chain_id = $1 AND token_address = $2
            "#,
        )
        .bind(chain_id_i64)
        .bind(&token_address_hex)
        .bind(rewind_to_i64)
        .bind(epoch_i64)
        .execute(&mut *tx)
        .await
        .map_err(RepositoryError::Database)?;

        recompute_orders_in_tx(&mut tx, affected_order_ids).await?;
        tx.commit().await.map_err(RepositoryError::Database)
    }
}

fn cursor_not_found(chain_id: u64, token_address: EvmAddress) -> RepositoryError {
    RepositoryError::CursorNotFound {
        chain_id,
        token_address,
    }
}

fn invalid_argument(field: &'static str, message: impl Into<String>) -> RepositoryError {
    RepositoryError::InvalidArgument {
        field,
        message: message.into(),
    }
}
