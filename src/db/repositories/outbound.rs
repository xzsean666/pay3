use std::{str::FromStr, time::Duration};

use async_trait::async_trait;
use bigdecimal::BigDecimal;
use sqlx::{PgPool, Postgres, Row, Transaction, postgres::PgRow};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::domain::{BlockHash, ChainBlockRef, EvmAddress, RawAmount, TxHash};

use super::{
    error::RepositoryError,
    types::{
        BroadcastableOutboundTx, InsertSignedCollectTxResult, NewSignedOutboundTx,
        OutboundTxPurpose, OutboundTxRecord, OutboundTxStatus, ReceiptCheckableOutboundTx,
        ReservedNonce,
    },
};

const DEFAULT_CLAIM_LEASE_SECONDS: u64 = 60;

const OUTBOUND_COLUMNS: &str = r#"
    id,
    chain_id,
    purpose,
    from_address,
    to_address,
    nonce,
    tx_hash,
    signed_tx,
    status,
    replacement_of,
    replacement_reason,
    broadcast_count,
    last_broadcast_at,
    receipt_block_number,
    receipt_block_hash,
    error,
    created_at,
    updated_at
"#;

#[async_trait]
pub trait OutboundRepository: Send + Sync {
    async fn reserve_nonce(
        &self,
        chain_id: u64,
        from_address: EvmAddress,
        pending_nonce: RawAmount,
    ) -> Result<ReservedNonce, RepositoryError>;

    async fn insert_signed_tx(
        &self,
        tx: NewSignedOutboundTx,
    ) -> Result<OutboundTxRecord, RepositoryError>;

    async fn insert_signed_collect_tx(
        &self,
        collection_id: Uuid,
        tx: NewSignedOutboundTx,
        resolved_amount_raw: RawAmount,
    ) -> Result<InsertSignedCollectTxResult, RepositoryError>;

    async fn replace_signed_tx(
        &self,
        old_tx_id: Uuid,
        replacement_tx: NewSignedOutboundTx,
    ) -> Result<OutboundTxRecord, RepositoryError>;

    async fn claim_signed_collect_tx_for_broadcast(
        &self,
        worker_id: &str,
    ) -> Result<Option<BroadcastableOutboundTx>, RepositoryError>;

    async fn claim_broadcast_collect_tx_for_receipt(
        &self,
        worker_id: &str,
    ) -> Result<Option<ReceiptCheckableOutboundTx>, RepositoryError>;

    async fn mark_broadcast(&self, tx_id: Uuid) -> Result<OutboundTxRecord, RepositoryError>;

    async fn mark_confirmed(
        &self,
        tx_id: Uuid,
        receipt_block: ChainBlockRef,
    ) -> Result<OutboundTxRecord, RepositoryError>;

    async fn mark_failed(
        &self,
        tx_id: Uuid,
        error: &str,
    ) -> Result<OutboundTxRecord, RepositoryError>;
}

#[derive(Clone)]
pub struct PgOutboundRepository {
    pool: PgPool,
    claim_lease_seconds: u64,
}

impl PgOutboundRepository {
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            claim_lease_seconds: DEFAULT_CLAIM_LEASE_SECONDS,
        }
    }

    pub fn with_claim_lease(pool: PgPool, claim_lease: Duration) -> Self {
        Self {
            pool,
            claim_lease_seconds: claim_lease.as_secs().max(1),
        }
    }
}

#[async_trait]
impl OutboundRepository for PgOutboundRepository {
    async fn reserve_nonce(
        &self,
        chain_id: u64,
        from_address: EvmAddress,
        pending_nonce: RawAmount,
    ) -> Result<ReservedNonce, RepositoryError> {
        let chain_id_i64 = u64_to_i64(chain_id, "chain_id")?;
        let from_address_hex = from_address.to_lower_hex();
        let pending_nonce_decimal = raw_amount_to_decimal(pending_nonce)?;
        let mut tx = self.pool.begin().await?;

        sqlx::query(
            r#"
            INSERT INTO account_nonces (
                chain_id,
                address,
                next_nonce
            )
            VALUES ($1, $2, $3)
            ON CONFLICT (chain_id, address) DO UPDATE
            SET next_nonce = GREATEST(account_nonces.next_nonce, EXCLUDED.next_nonce),
                updated_at = now()
            "#,
        )
        .bind(chain_id_i64)
        .bind(&from_address_hex)
        .bind(pending_nonce_decimal)
        .execute(&mut *tx)
        .await?;

        let reserved_nonce: BigDecimal = sqlx::query_scalar(
            r#"
            SELECT next_nonce
            FROM account_nonces
            WHERE chain_id = $1
              AND address = $2
            FOR UPDATE
            "#,
        )
        .bind(chain_id_i64)
        .bind(&from_address_hex)
        .fetch_one(&mut *tx)
        .await?;

        sqlx::query(
            r#"
            UPDATE account_nonces
            SET next_nonce = next_nonce + 1,
                updated_at = now()
            WHERE chain_id = $1
              AND address = $2
            "#,
        )
        .bind(chain_id_i64)
        .bind(&from_address_hex)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;

        Ok(ReservedNonce {
            chain_id,
            address: from_address,
            nonce: decimal_to_raw_amount(reserved_nonce, "account_nonces.next_nonce")?,
        })
    }

    async fn insert_signed_tx(
        &self,
        tx: NewSignedOutboundTx,
    ) -> Result<OutboundTxRecord, RepositoryError> {
        let mut db_tx = self.pool.begin().await?;
        let record = insert_signed_tx_row(&mut db_tx, &tx, None).await?;
        db_tx.commit().await?;
        Ok(record)
    }

    async fn insert_signed_collect_tx(
        &self,
        collection_id: Uuid,
        tx: NewSignedOutboundTx,
        resolved_amount_raw: RawAmount,
    ) -> Result<InsertSignedCollectTxResult, RepositoryError> {
        let mut db_tx = self.pool.begin().await?;
        let record = insert_signed_tx_row(&mut db_tx, &tx, None).await?;

        let update = sqlx::query(
            r#"
            UPDATE collections
            SET outbound_tx_id = $2,
                amount_raw = COALESCE(amount_raw, $3),
                status = 'transferring',
                locked_by = NULL,
                locked_until = NULL,
                error = NULL,
                updated_at = now()
            WHERE id = $1
              AND status IN ('queued', 'transferring', 'replacing')
            "#,
        )
        .bind(collection_id)
        .bind(record.id)
        .bind(raw_amount_to_decimal(resolved_amount_raw)?)
        .execute(&mut *db_tx)
        .await?;

        if update.rows_affected() != 1 {
            return Err(protocol_error(format!(
                "collection {collection_id} is not attachable"
            )));
        }

        db_tx.commit().await?;
        Ok(InsertSignedCollectTxResult {
            collection_id,
            outbound: record,
        })
    }

    async fn replace_signed_tx(
        &self,
        old_tx_id: Uuid,
        replacement_tx: NewSignedOutboundTx,
    ) -> Result<OutboundTxRecord, RepositoryError> {
        let mut tx = self.pool.begin().await?;
        let old = select_outbound_for_update(&mut tx, old_tx_id).await?;

        ensure_replaceable(&old)?;
        ensure_replacement_invariants(&old, &replacement_tx)?;
        lock_nonce_row(&mut tx, old.chain_id, old.from_address).await?;

        sqlx::query(
            r#"
            UPDATE outbound_transactions
            SET status = 'replaced',
                replacement_reason = COALESCE($2, replacement_reason),
                updated_at = now()
            WHERE id = $1
            "#,
        )
        .bind(old_tx_id)
        .bind(replacement_tx.replacement_reason.as_deref())
        .execute(&mut *tx)
        .await?;

        let replacement = insert_signed_tx_row(&mut tx, &replacement_tx, Some(old_tx_id)).await?;

        sqlx::query(
            r#"
            UPDATE collections
            SET outbound_tx_id = $2,
                status = 'transferring',
                attempt_count = attempt_count + 1,
                locked_by = NULL,
                locked_until = NULL,
                error = NULL,
                updated_at = now()
            WHERE outbound_tx_id = $1
            "#,
        )
        .bind(old_tx_id)
        .bind(replacement.id)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(replacement)
    }

    async fn claim_signed_collect_tx_for_broadcast(
        &self,
        worker_id: &str,
    ) -> Result<Option<BroadcastableOutboundTx>, RepositoryError> {
        let lease_seconds = u64_to_i64(self.claim_lease_seconds, "claim_lease_seconds")?;
        let sql = r#"
            WITH next_outbound AS (
                SELECT c.id AS collection_id,
                       c.outbound_tx_id
                FROM collections c
                JOIN outbound_transactions o
                  ON o.id = c.outbound_tx_id
                WHERE c.status = 'transferring'
                  AND o.status = 'signed'
                  AND (c.locked_until IS NULL OR c.locked_until <= now())
                ORDER BY c.updated_at, c.id
                FOR UPDATE OF c SKIP LOCKED
                LIMIT 1
            ),
            claimed AS (
                UPDATE collections AS c
                SET locked_by = $1,
                    locked_until = now() + ($2::bigint * interval '1 second'),
                    updated_at = now()
                FROM next_outbound
                WHERE c.id = next_outbound.collection_id
                RETURNING c.id AS collection_id,
                          c.outbound_tx_id
            )
            SELECT claimed.collection_id,
                   o.id,
                   o.chain_id,
                   o.purpose,
                   o.from_address,
                   o.to_address,
                   o.nonce,
                   o.tx_hash,
                   o.signed_tx,
                   o.status,
                   o.replacement_of,
                   o.replacement_reason,
                   o.broadcast_count,
                   o.last_broadcast_at,
                   o.receipt_block_number,
                   o.receipt_block_hash,
                   o.error,
                   o.created_at,
                   o.updated_at
            FROM claimed
            JOIN outbound_transactions o
              ON o.id = claimed.outbound_tx_id
            "#;

        let row = sqlx::query(sql)
            .bind(worker_id)
            .bind(lease_seconds)
            .fetch_optional(&self.pool)
            .await?;

        row.map(|row| {
            Ok(BroadcastableOutboundTx {
                collection_id: row.try_get("collection_id")?,
                outbound: outbound_record_from_row(&row)?,
            })
        })
        .transpose()
    }

    async fn claim_broadcast_collect_tx_for_receipt(
        &self,
        worker_id: &str,
    ) -> Result<Option<ReceiptCheckableOutboundTx>, RepositoryError> {
        let lease_seconds = u64_to_i64(self.claim_lease_seconds, "claim_lease_seconds")?;
        let sql = r#"
            WITH next_outbound AS (
                SELECT c.id AS collection_id,
                       c.outbound_tx_id
                FROM collections c
                JOIN outbound_transactions o
                  ON o.id = c.outbound_tx_id
                WHERE c.status = 'confirming'
                  AND o.status = 'broadcast'
                  AND (c.locked_until IS NULL OR c.locked_until <= now())
                ORDER BY c.updated_at, c.id
                FOR UPDATE OF c SKIP LOCKED
                LIMIT 1
            ),
            claimed AS (
                UPDATE collections AS c
                SET locked_by = $1,
                    locked_until = now() + ($2::bigint * interval '1 second'),
                    updated_at = now()
                FROM next_outbound
                WHERE c.id = next_outbound.collection_id
                RETURNING c.id AS collection_id,
                          c.outbound_tx_id
            )
            SELECT claimed.collection_id,
                   o.id,
                   o.chain_id,
                   o.purpose,
                   o.from_address,
                   o.to_address,
                   o.nonce,
                   o.tx_hash,
                   o.signed_tx,
                   o.status,
                   o.replacement_of,
                   o.replacement_reason,
                   o.broadcast_count,
                   o.last_broadcast_at,
                   o.receipt_block_number,
                   o.receipt_block_hash,
                   o.error,
                   o.created_at,
                   o.updated_at
            FROM claimed
            JOIN outbound_transactions o
              ON o.id = claimed.outbound_tx_id
            "#;

        let row = sqlx::query(sql)
            .bind(worker_id)
            .bind(lease_seconds)
            .fetch_optional(&self.pool)
            .await?;

        row.map(|row| {
            Ok(ReceiptCheckableOutboundTx {
                collection_id: row.try_get("collection_id")?,
                outbound: outbound_record_from_row(&row)?,
            })
        })
        .transpose()
    }

    async fn mark_broadcast(&self, tx_id: Uuid) -> Result<OutboundTxRecord, RepositoryError> {
        let mut tx = self.pool.begin().await?;
        let sql = format!(
            r#"
            UPDATE outbound_transactions
            SET status = 'broadcast',
                broadcast_count = broadcast_count + 1,
                last_broadcast_at = now(),
                error = NULL,
                updated_at = now()
            WHERE id = $1
              AND status IN ('signed', 'broadcast')
            RETURNING {OUTBOUND_COLUMNS}
            "#
        );

        let row = sqlx::query(&sql)
            .bind(tx_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| {
                protocol_error(format!("outbound transaction {tx_id} is not broadcastable"))
            })?;
        let record = outbound_record_from_row(&row)?;

        let update = sqlx::query(
            r#"
            UPDATE collections
            SET status = 'confirming',
                locked_by = NULL,
                locked_until = NULL,
                updated_at = now()
            WHERE outbound_tx_id = $1
              AND status IN ('transferring', 'confirming')
            "#,
        )
        .bind(tx_id)
        .execute(&mut *tx)
        .await?;
        if update.rows_affected() != 1 {
            return Err(protocol_error(format!(
                "outbound transaction {tx_id} is not attached to a broadcastable collection"
            )));
        }

        tx.commit().await?;
        Ok(record)
    }

    async fn mark_confirmed(
        &self,
        tx_id: Uuid,
        receipt_block: ChainBlockRef,
    ) -> Result<OutboundTxRecord, RepositoryError> {
        let mut tx = self.pool.begin().await?;
        let sql = format!(
            r#"
            UPDATE outbound_transactions
            SET status = 'confirmed',
                receipt_block_number = $2,
                receipt_block_hash = $3,
                error = NULL,
                updated_at = now()
            WHERE id = $1
              AND status IN ('signed', 'broadcast')
            RETURNING {OUTBOUND_COLUMNS}
            "#
        );

        let row = sqlx::query(&sql)
            .bind(tx_id)
            .bind(u64_to_i64(receipt_block.number, "receipt_block.number")?)
            .bind(receipt_block.hash.to_string())
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| {
                protocol_error(format!("outbound transaction {tx_id} is not confirmable"))
            })?;
        let record = outbound_record_from_row(&row)?;

        let update = sqlx::query(
            r#"
            UPDATE collections
            SET status = 'confirmed',
                locked_by = NULL,
                locked_until = NULL,
                error = NULL,
                updated_at = now()
            WHERE outbound_tx_id = $1
              AND status = 'confirming'
            "#,
        )
        .bind(tx_id)
        .execute(&mut *tx)
        .await?;
        if update.rows_affected() != 1 {
            return Err(protocol_error(format!(
                "outbound transaction {tx_id} is not attached to a confirmable collection"
            )));
        }

        tx.commit().await?;
        Ok(record)
    }

    async fn mark_failed(
        &self,
        tx_id: Uuid,
        error: &str,
    ) -> Result<OutboundTxRecord, RepositoryError> {
        let mut tx = self.pool.begin().await?;
        let sql = format!(
            r#"
            UPDATE outbound_transactions
            SET status = 'failed',
                error = $2,
                updated_at = now()
            WHERE id = $1
              AND status IN ('signed', 'broadcast')
            RETURNING {OUTBOUND_COLUMNS}
            "#
        );

        let row = sqlx::query(&sql)
            .bind(tx_id)
            .bind(error)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| {
                protocol_error(format!("outbound transaction {tx_id} is not failable"))
            })?;
        let record = outbound_record_from_row(&row)?;

        let update = sqlx::query(
            r#"
            UPDATE collections
            SET status = 'failed',
                locked_by = NULL,
                locked_until = NULL,
                error = $2,
                updated_at = now()
            WHERE outbound_tx_id = $1
              AND status = 'confirming'
            "#,
        )
        .bind(tx_id)
        .bind(error)
        .execute(&mut *tx)
        .await?;
        if update.rows_affected() != 1 {
            return Err(protocol_error(format!(
                "outbound transaction {tx_id} is not attached to a failable collection"
            )));
        }

        tx.commit().await?;
        Ok(record)
    }
}

async fn insert_signed_tx_row(
    tx: &mut Transaction<'_, Postgres>,
    outbound: &NewSignedOutboundTx,
    replacement_of: Option<Uuid>,
) -> Result<OutboundTxRecord, RepositoryError> {
    let sql = format!(
        r#"
        INSERT INTO outbound_transactions (
            id,
            chain_id,
            purpose,
            from_address,
            to_address,
            nonce,
            tx_hash,
            signed_tx,
            status,
            replacement_of,
            replacement_reason
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'signed', $9, $10)
        RETURNING {OUTBOUND_COLUMNS}
        "#
    );

    let row = sqlx::query(&sql)
        .bind(outbound.id)
        .bind(u64_to_i64(outbound.chain_id, "outbound.chain_id")?)
        .bind(outbound.purpose.as_db_str())
        .bind(outbound.from_address.to_lower_hex())
        .bind(outbound.to_address.to_lower_hex())
        .bind(raw_amount_to_decimal(outbound.nonce)?)
        .bind(outbound.tx_hash.to_lower_hex())
        .bind(&outbound.signed_tx)
        .bind(replacement_of.or(outbound.replacement_of))
        .bind(outbound.replacement_reason.as_deref())
        .fetch_one(&mut **tx)
        .await?;

    outbound_record_from_row(&row)
}

async fn select_outbound_for_update(
    tx: &mut Transaction<'_, Postgres>,
    tx_id: Uuid,
) -> Result<OutboundTxRecord, RepositoryError> {
    let sql = format!(
        r#"
        SELECT {OUTBOUND_COLUMNS}
        FROM outbound_transactions
        WHERE id = $1
        FOR UPDATE
        "#
    );

    let row = sqlx::query(&sql)
        .bind(tx_id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| protocol_error(format!("outbound transaction {tx_id} not found")))?;

    outbound_record_from_row(&row)
}

async fn lock_nonce_row(
    tx: &mut Transaction<'_, Postgres>,
    chain_id: u64,
    from_address: EvmAddress,
) -> Result<(), RepositoryError> {
    sqlx::query(
        r#"
        SELECT next_nonce
        FROM account_nonces
        WHERE chain_id = $1
          AND address = $2
        FOR UPDATE
        "#,
    )
    .bind(u64_to_i64(chain_id, "chain_id")?)
    .bind(from_address.to_lower_hex())
    .fetch_optional(&mut **tx)
    .await?;

    Ok(())
}

fn ensure_replaceable(old: &OutboundTxRecord) -> Result<(), RepositoryError> {
    match old.status {
        OutboundTxStatus::Signed
        | OutboundTxStatus::Broadcast
        | OutboundTxStatus::Failed
        | OutboundTxStatus::Dropped => Ok(()),
        status => Err(RepositoryError::invariant_violation(format!(
            "outbound transaction {} with status {status:?} cannot be replaced",
            old.id
        ))),
    }
}

fn ensure_replacement_invariants(
    old: &OutboundTxRecord,
    replacement: &NewSignedOutboundTx,
) -> Result<(), RepositoryError> {
    if replacement
        .replacement_of
        .is_some_and(|replacement_of| replacement_of != old.id)
    {
        return Err(RepositoryError::invariant_violation(format!(
            "replacement for outbound transaction {} points at a different original transaction",
            old.id
        )));
    }

    if old.chain_id != replacement.chain_id
        || old.purpose != replacement.purpose
        || old.from_address != replacement.from_address
        || old.to_address != replacement.to_address
        || old.nonce != replacement.nonce
    {
        return Err(RepositoryError::invariant_violation(format!(
            "replacement for outbound transaction {} changed chain/purpose/from/to/nonce invariant",
            old.id
        )));
    }

    Ok(())
}

fn outbound_record_from_row(row: &PgRow) -> Result<OutboundTxRecord, RepositoryError> {
    let purpose: String = row.try_get("purpose")?;
    let status: String = row.try_get("status")?;
    let receipt_block_number: Option<i64> = row.try_get("receipt_block_number")?;
    let receipt_block_hash: Option<String> = row.try_get("receipt_block_hash")?;

    Ok(OutboundTxRecord {
        id: row.try_get("id")?,
        chain_id: i64_to_u64(row.try_get("chain_id")?, "outbound_transactions.chain_id")?,
        purpose: OutboundTxPurpose::try_from(purpose.as_str())?,
        from_address: parse_address(row.try_get::<String, _>("from_address")?, "from_address")?,
        to_address: parse_address(row.try_get::<String, _>("to_address")?, "to_address")?,
        nonce: decimal_to_raw_amount(row.try_get("nonce")?, "outbound_transactions.nonce")?,
        tx_hash: parse_hash(row.try_get::<String, _>("tx_hash")?, "tx_hash")?,
        signed_tx: row.try_get("signed_tx")?,
        status: OutboundTxStatus::try_from(status.as_str())?,
        replacement_of: row.try_get("replacement_of")?,
        replacement_reason: row.try_get("replacement_reason")?,
        broadcast_count: i32_to_u32(row.try_get("broadcast_count")?, "broadcast_count")?,
        last_broadcast_at: row.try_get::<Option<OffsetDateTime>, _>("last_broadcast_at")?,
        receipt_block: optional_receipt_block(receipt_block_number, receipt_block_hash)?,
        error: row.try_get("error")?,
        created_at: row.try_get::<OffsetDateTime, _>("created_at")?,
        updated_at: row.try_get::<OffsetDateTime, _>("updated_at")?,
    })
}

fn parse_address(value: String, field: &'static str) -> Result<EvmAddress, RepositoryError> {
    value.parse().map_err(|error| {
        RepositoryError::invalid_db_value(field, value, format!("invalid EVM address: {error}"))
    })
}

fn parse_hash(value: String, field: &'static str) -> Result<TxHash, RepositoryError> {
    value.parse().map_err(|error| {
        RepositoryError::invalid_db_value(field, value, format!("invalid tx hash: {error}"))
    })
}

fn optional_receipt_block(
    number: Option<i64>,
    hash: Option<String>,
) -> Result<Option<ChainBlockRef>, RepositoryError> {
    match (number, hash) {
        (Some(number), Some(hash)) => Ok(Some(ChainBlockRef {
            number: i64_to_u64(number, "receipt_block_number")?,
            hash: parse_block_hash(hash, "receipt_block_hash")?,
        })),
        (None, None) => Ok(None),
        _ => Err(RepositoryError::invalid_db_value(
            "outbound_transactions.receipt_block",
            "partial receipt block",
            "receipt block number and hash must be set together",
        )),
    }
}

fn parse_block_hash(value: String, field: &'static str) -> Result<BlockHash, RepositoryError> {
    value.parse().map_err(|error| {
        RepositoryError::invalid_db_value(field, value, format!("invalid block hash: {error}"))
    })
}

fn u64_to_i64(value: u64, field: &'static str) -> Result<i64, RepositoryError> {
    i64::try_from(value).map_err(|source| RepositoryError::integer_out_of_range(field, source))
}

fn i64_to_u64(value: i64, field: &'static str) -> Result<u64, RepositoryError> {
    u64::try_from(value).map_err(|source| RepositoryError::integer_out_of_range(field, source))
}

fn i32_to_u32(value: i32, field: &'static str) -> Result<u32, RepositoryError> {
    u32::try_from(value).map_err(|source| RepositoryError::integer_out_of_range(field, source))
}

fn raw_amount_to_decimal(value: RawAmount) -> Result<BigDecimal, RepositoryError> {
    BigDecimal::from_str(&value.to_string()).map_err(|error| {
        RepositoryError::invalid_db_value(
            "raw_amount",
            value.to_string(),
            format!("invalid decimal: {error}"),
        )
    })
}

fn decimal_to_raw_amount(
    value: BigDecimal,
    field: &'static str,
) -> Result<RawAmount, RepositoryError> {
    let raw = value.with_scale(0).to_string();
    RawAmount::from_str(&raw).map_err(|error| {
        RepositoryError::invalid_db_value(field, raw, format!("invalid raw amount: {error}"))
    })
}

fn protocol_error(message: impl Into<String>) -> RepositoryError {
    RepositoryError::invariant_violation(message)
}
