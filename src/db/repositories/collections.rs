use std::{str::FromStr, time::Duration};

use async_trait::async_trait;
use bigdecimal::BigDecimal;
use sqlx::{PgPool, Postgres, Row, Transaction, postgres::PgRow};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::domain::{DerivationSegment, EvmAddress, RawAmount};

use super::{
    error::RepositoryError,
    types::{CollectionJob, CollectionRecord, CollectionRecordStatus, CreateCollectionCommand},
};

const DEFAULT_CLAIM_LEASE_SECONDS: u64 = 60;

const COLLECTION_COLUMNS: &str = r#"
    id,
    order_id,
    idempotency_key,
    request_hash,
    child_account_id,
    chain_id,
    token_address,
    from_address,
    to_address,
    amount_raw,
    status,
    outbound_tx_id,
    attempt_count,
    locked_by,
    locked_until,
    error,
    created_at,
    updated_at
"#;

#[async_trait]
pub trait CollectionRepository: Send + Sync {
    async fn create_collection_idempotent(
        &self,
        command: CreateCollectionCommand,
    ) -> Result<CollectionRecord, RepositoryError>;

    async fn get_collection(&self, id: Uuid) -> Result<Option<CollectionRecord>, RepositoryError>;

    async fn get_collection_job(&self, id: Uuid) -> Result<Option<CollectionJob>, RepositoryError>;

    async fn claim_collection_job(
        &self,
        worker_id: &str,
    ) -> Result<Option<CollectionJob>, RepositoryError>;

    async fn attach_outbound_tx(
        &self,
        collection_id: Uuid,
        outbound_tx_id: Uuid,
        resolved_amount_raw: RawAmount,
    ) -> Result<CollectionRecord, RepositoryError>;
}

#[derive(Clone)]
pub struct PgCollectionRepository {
    pool: PgPool,
    claim_lease_seconds: u64,
}

impl PgCollectionRepository {
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
impl CollectionRepository for PgCollectionRepository {
    async fn create_collection_idempotent(
        &self,
        command: CreateCollectionCommand,
    ) -> Result<CollectionRecord, RepositoryError> {
        let mut tx = self.pool.begin().await?;

        if let Some(existing) =
            select_collection_by_idempotency_key(&mut tx, &command.idempotency_key).await?
        {
            ensure_same_collection_request(&existing, &command)?;
            tx.commit().await?;
            return Ok(existing);
        }

        lock_paid_order_for_collection(&mut tx, &command).await?;
        let inserted = insert_collection(&mut tx, &command).await?;

        let collection = match inserted {
            Some(collection) => collection,
            None => {
                let existing =
                    select_collection_by_idempotency_key(&mut tx, &command.idempotency_key)
                        .await?
                        .ok_or_else(|| {
                            protocol_error("collection idempotency conflict did not return a row")
                        })?;
                ensure_same_collection_request(&existing, &command)?;
                existing
            }
        };

        tx.commit().await?;
        Ok(collection)
    }

    async fn get_collection(&self, id: Uuid) -> Result<Option<CollectionRecord>, RepositoryError> {
        let sql = format!(
            r#"
            SELECT {COLLECTION_COLUMNS}
            FROM collections
            WHERE id = $1
            "#
        );

        sqlx::query(&sql)
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .map(|row| collection_record_from_row(&row))
            .transpose()
    }

    async fn get_collection_job(&self, id: Uuid) -> Result<Option<CollectionJob>, RepositoryError> {
        let sql = format!(
            r#"
            SELECT {COLLECTION_COLUMNS}
            FROM collections
            WHERE id = $1
            FOR UPDATE
            "#
        );

        let mut tx = self.pool.begin().await?;
        let row = sqlx::query(&sql).bind(id).fetch_optional(&mut *tx).await?;

        let job = match row {
            Some(row) => {
                let collection = collection_record_from_row(&row)?;
                Some(collection_job_from_record(&mut tx, collection).await?)
            }
            None => None,
        };

        tx.commit().await?;
        Ok(job)
    }

    async fn claim_collection_job(
        &self,
        worker_id: &str,
    ) -> Result<Option<CollectionJob>, RepositoryError> {
        let lease_seconds = u64_to_i64(self.claim_lease_seconds, "claim_lease_seconds")?;
        let sql = format!(
            r#"
            WITH next_collection AS (
                SELECT id AS collection_id
                FROM collections
                WHERE status = 'queued'
                  AND (locked_until IS NULL OR locked_until <= now())
                ORDER BY created_at, id
                FOR UPDATE SKIP LOCKED
                LIMIT 1
            )
            UPDATE collections AS c
            SET locked_by = $1,
                locked_until = now() + ($2::bigint * interval '1 second'),
                attempt_count = c.attempt_count + 1,
                updated_at = now()
            FROM next_collection
            WHERE c.id = next_collection.collection_id
            RETURNING {COLLECTION_COLUMNS}
            "#
        );

        let mut tx = self.pool.begin().await?;
        let row = sqlx::query(&sql)
            .bind(worker_id)
            .bind(lease_seconds)
            .fetch_optional(&mut *tx)
            .await?;

        let job = match row {
            Some(row) => {
                let collection = collection_record_from_row(&row)?;
                Some(collection_job_from_record(&mut tx, collection).await?)
            }
            None => None,
        };

        tx.commit().await?;
        Ok(job)
    }

    async fn attach_outbound_tx(
        &self,
        collection_id: Uuid,
        outbound_tx_id: Uuid,
        resolved_amount_raw: RawAmount,
    ) -> Result<CollectionRecord, RepositoryError> {
        let sql = format!(
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
            RETURNING {COLLECTION_COLUMNS}
            "#
        );

        let row = sqlx::query(&sql)
            .bind(collection_id)
            .bind(outbound_tx_id)
            .bind(raw_amount_to_decimal(resolved_amount_raw)?)
            .fetch_optional(&self.pool)
            .await?;

        row.map(|row| collection_record_from_row(&row))
            .transpose()?
            .ok_or_else(|| protocol_error(format!("collection {collection_id} is not attachable")))
    }
}

async fn select_collection_by_idempotency_key(
    tx: &mut Transaction<'_, Postgres>,
    idempotency_key: &str,
) -> Result<Option<CollectionRecord>, RepositoryError> {
    let sql = format!(
        r#"
        SELECT {COLLECTION_COLUMNS}
        FROM collections
        WHERE idempotency_key = $1
        FOR UPDATE
        "#
    );

    sqlx::query(&sql)
        .bind(idempotency_key)
        .fetch_optional(&mut **tx)
        .await?
        .map(|row| collection_record_from_row(&row))
        .transpose()
}

async fn lock_paid_order_for_collection(
    tx: &mut Transaction<'_, Postgres>,
    command: &CreateCollectionCommand,
) -> Result<(), RepositoryError> {
    let row = sqlx::query(
        r#"
        SELECT status
        FROM orders
        WHERE id = $1
          AND child_account_id = $2
          AND chain_id = $3
          AND token_address = $4
          AND receive_address = $5
        FOR UPDATE
        "#,
    )
    .bind(command.order_id)
    .bind(command.child_account_id)
    .bind(u64_to_i64(command.chain_id, "collections.chain_id")?)
    .bind(command.token_address.to_lower_hex())
    .bind(command.from_address.to_lower_hex())
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| RepositoryError::not_found("orders", command.order_id.to_string()))?;

    let status: String = row.try_get("status")?;
    if status != "paid" {
        return Err(RepositoryError::invariant_violation(format!(
            "order {} must be paid before collection, got {status}",
            command.order_id
        )));
    }

    Ok(())
}

async fn insert_collection(
    tx: &mut Transaction<'_, Postgres>,
    command: &CreateCollectionCommand,
) -> Result<Option<CollectionRecord>, RepositoryError> {
    let sql = format!(
        r#"
        INSERT INTO collections (
            id,
            order_id,
            idempotency_key,
            request_hash,
            child_account_id,
            chain_id,
            token_address,
            from_address,
            to_address,
            amount_raw,
            status
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 'queued')
        ON CONFLICT (idempotency_key) DO NOTHING
        RETURNING {COLLECTION_COLUMNS}
        "#
    );

    sqlx::query(&sql)
        .bind(command.collection_id)
        .bind(command.order_id)
        .bind(&command.idempotency_key)
        .bind(&command.request_hash)
        .bind(command.child_account_id)
        .bind(u64_to_i64(command.chain_id, "collections.chain_id")?)
        .bind(command.token_address.to_lower_hex())
        .bind(command.from_address.to_lower_hex())
        .bind(command.to_address.to_lower_hex())
        .bind(optional_raw_amount_to_decimal(command.amount_raw)?)
        .fetch_optional(&mut **tx)
        .await?
        .map(|row| collection_record_from_row(&row))
        .transpose()
}

fn ensure_same_collection_request(
    existing: &CollectionRecord,
    command: &CreateCollectionCommand,
) -> Result<(), RepositoryError> {
    if existing.request_hash == command.request_hash {
        return Ok(());
    }

    Err(RepositoryError::idempotency_conflict(
        "collections.idempotency_key",
        command.idempotency_key.clone(),
        Some(existing.id),
    ))
}

fn collection_record_from_row(row: &PgRow) -> Result<CollectionRecord, RepositoryError> {
    let status: String = row.try_get("status")?;

    Ok(CollectionRecord {
        id: row.try_get("id")?,
        order_id: row.try_get("order_id")?,
        idempotency_key: row.try_get("idempotency_key")?,
        request_hash: row.try_get("request_hash")?,
        child_account_id: row.try_get("child_account_id")?,
        chain_id: i64_to_u64(row.try_get("chain_id")?, "collections.chain_id")?,
        token_address: parse_address(row.try_get::<String, _>("token_address")?, "token_address")?,
        from_address: parse_address(row.try_get::<String, _>("from_address")?, "from_address")?,
        to_address: parse_address(row.try_get::<String, _>("to_address")?, "to_address")?,
        amount_raw: optional_decimal_to_raw_amount(row.try_get("amount_raw")?)?,
        status: CollectionRecordStatus::try_from(status.as_str())?,
        outbound_tx_id: row.try_get("outbound_tx_id")?,
        attempt_count: i32_to_u32(row.try_get("attempt_count")?, "collections.attempt_count")?,
        locked_by: row.try_get("locked_by")?,
        locked_until: row.try_get::<Option<OffsetDateTime>, _>("locked_until")?,
        error: row.try_get("error")?,
        created_at: row.try_get::<OffsetDateTime, _>("created_at")?,
        updated_at: row.try_get::<OffsetDateTime, _>("updated_at")?,
    })
}

async fn collection_job_from_record(
    tx: &mut Transaction<'_, Postgres>,
    collection: CollectionRecord,
) -> Result<CollectionJob, RepositoryError> {
    let row = sqlx::query(
        r#"
        SELECT signer_key_ref,
               derivation_version,
               account_index,
               change_index,
               address_index,
               derivation_path
        FROM child_accounts
        WHERE id = $1
        "#,
    )
    .bind(collection.child_account_id)
    .fetch_one(&mut **tx)
    .await?;

    let account_index = i64_to_u32(
        row.try_get("account_index")?,
        "child_accounts.account_index",
    )?;
    let change_index = i64_to_u32(row.try_get("change_index")?, "child_accounts.change_index")?;
    let address_index = i64_to_u32(
        row.try_get("address_index")?,
        "child_accounts.address_index",
    )?;

    Ok(CollectionJob {
        collection,
        signer_key_ref: row.try_get("signer_key_ref")?,
        derivation_version: i32_to_u32(
            row.try_get("derivation_version")?,
            "child_accounts.derivation_version",
        )?,
        derivation_segment: DerivationSegment::new(account_index, change_index, address_index)?,
        derivation_path: row.try_get("derivation_path")?,
    })
}

fn parse_address(value: String, field: &'static str) -> Result<EvmAddress, RepositoryError> {
    value.parse().map_err(|error| {
        RepositoryError::invalid_db_value(field, value, format!("invalid EVM address: {error}"))
    })
}

fn optional_raw_amount_to_decimal(
    value: Option<RawAmount>,
) -> Result<Option<BigDecimal>, RepositoryError> {
    value.map(raw_amount_to_decimal).transpose()
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

fn optional_decimal_to_raw_amount(
    value: Option<BigDecimal>,
) -> Result<Option<RawAmount>, RepositoryError> {
    value.map(decimal_to_raw_amount).transpose()
}

fn decimal_to_raw_amount(value: BigDecimal) -> Result<RawAmount, RepositoryError> {
    let raw = value.with_scale(0).to_string();
    RawAmount::from_str(&raw).map_err(|error| {
        RepositoryError::invalid_db_value(
            "numeric(78,0)",
            raw,
            format!("invalid raw amount: {error}"),
        )
    })
}

fn i64_to_u64(value: i64, field: &'static str) -> Result<u64, RepositoryError> {
    u64::try_from(value).map_err(|source| RepositoryError::integer_out_of_range(field, source))
}

fn u64_to_i64(value: u64, field: &'static str) -> Result<i64, RepositoryError> {
    i64::try_from(value).map_err(|source| RepositoryError::integer_out_of_range(field, source))
}

fn i64_to_u32(value: i64, field: &'static str) -> Result<u32, RepositoryError> {
    u32::try_from(value).map_err(|source| RepositoryError::integer_out_of_range(field, source))
}

fn i32_to_u32(value: i32, field: &'static str) -> Result<u32, RepositoryError> {
    u32::try_from(value).map_err(|source| RepositoryError::integer_out_of_range(field, source))
}

fn protocol_error(message: impl Into<String>) -> RepositoryError {
    RepositoryError::invariant_violation(message)
}
