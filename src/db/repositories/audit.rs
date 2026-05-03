use async_trait::async_trait;
use sqlx::{PgPool, Row, postgres::PgRow};
use time::OffsetDateTime;

use crate::domain::TxHash;

use super::{
    error::RepositoryError,
    types::{AuditEventInput, AuditEventRecord},
};

#[async_trait]
pub trait AuditRepository: Send + Sync {
    async fn append_audit_event(
        &self,
        event: AuditEventInput,
    ) -> Result<AuditEventRecord, RepositoryError>;
}

#[derive(Clone)]
pub struct PgAuditRepository {
    pool: PgPool,
}

impl PgAuditRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl AuditRepository for PgAuditRepository {
    async fn append_audit_event(
        &self,
        event: AuditEventInput,
    ) -> Result<AuditEventRecord, RepositoryError> {
        let row = sqlx::query(
            r#"
            INSERT INTO audit_events (
                id,
                event_type,
                request_id,
                principal_sub,
                scopes,
                order_id,
                collection_id,
                tx_hash,
                payload
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            RETURNING
                id,
                event_type,
                request_id,
                principal_sub,
                scopes,
                order_id,
                collection_id,
                tx_hash,
                payload,
                created_at
            "#,
        )
        .bind(event.id)
        .bind(event.event_type)
        .bind(event.request_id)
        .bind(event.principal_sub)
        .bind(event.scopes)
        .bind(event.order_id)
        .bind(event.collection_id)
        .bind(event.tx_hash.map(|tx_hash| tx_hash.to_string()))
        .bind(event.payload)
        .fetch_one(&self.pool)
        .await?;

        audit_event_from_row(&row)
    }
}

fn audit_event_from_row(row: &PgRow) -> Result<AuditEventRecord, RepositoryError> {
    Ok(AuditEventRecord {
        id: row.try_get("id")?,
        event_type: row.try_get("event_type")?,
        request_id: row.try_get("request_id")?,
        principal_sub: row.try_get("principal_sub")?,
        scopes: row.try_get("scopes")?,
        order_id: row.try_get("order_id")?,
        collection_id: row.try_get("collection_id")?,
        tx_hash: optional_tx_hash(row.try_get("tx_hash")?)?,
        payload: row.try_get("payload")?,
        created_at: row.try_get::<OffsetDateTime, _>("created_at")?,
    })
}

fn optional_tx_hash(value: Option<String>) -> Result<Option<TxHash>, RepositoryError> {
    value
        .map(|value| {
            value.parse().map_err(|error| {
                RepositoryError::invalid_db_value(
                    "audit_events.tx_hash",
                    value,
                    format!("invalid tx hash: {error}"),
                )
            })
        })
        .transpose()
}
