use std::{collections::BTreeSet, str::FromStr};

use async_trait::async_trait;
use bigdecimal::BigDecimal;
use sqlx::{PgPool, Postgres, Row, Transaction, postgres::PgRow};
use uuid::Uuid;

use crate::domain::{
    ChainBlockRef, DerivationSegment, DerivationSegmentError, EvmAddress, MAX_DERIVATION_INDEX,
    OrderStatus, RawAmount,
};

use super::{
    error::RepositoryError,
    types::{
        ChildAccountRecord, CreateOrderCommand, OrderRecord, OrderView, PaymentWindowCandidate,
        PaymentWindowRecord,
    },
};

const ORDER_SELECT: &str = r#"
SELECT
    o.id,
    o.external_id,
    o.request_hash,
    o.child_account_id,
    o.receive_address,
    o.chain_id,
    o.token_address,
    o.expected_amount_raw,
    o.paid_amount_raw,
    o.status,
    o.expires_at,
    o.monitor_until,
    o.created_at,
    o.updated_at
FROM orders o
"#;

const ORDER_VIEW_SELECT: &str = r#"
SELECT
    o.id,
    o.external_id,
    o.request_hash,
    o.child_account_id,
    o.receive_address,
    o.chain_id,
    o.token_address,
    o.expected_amount_raw,
    o.paid_amount_raw,
    o.status,
    o.expires_at,
    o.monitor_until,
    o.created_at,
    o.updated_at,
    ca.id AS ca_id,
    ca.signer_key_ref AS ca_signer_key_ref,
    ca.derivation_version AS ca_derivation_version,
    ca.account_index AS ca_account_index,
    ca.change_index AS ca_change_index,
    ca.address_index AS ca_address_index,
    ca.derivation_path AS ca_derivation_path,
    ca.address AS ca_address,
    ca.last_used_at AS ca_last_used_at,
    ca.created_at AS ca_created_at,
    pw.id AS pw_id,
    pw.order_id AS pw_order_id,
    pw.child_account_id AS pw_child_account_id,
    pw.receive_address AS pw_receive_address,
    pw.window_from AS pw_window_from,
    pw.window_from_block AS pw_window_from_block,
    pw.window_from_block_hash AS pw_window_from_block_hash,
    pw.expires_at AS pw_expires_at,
    pw.monitor_until AS pw_monitor_until,
    pw.created_at AS pw_created_at
FROM orders o
JOIN child_accounts ca ON ca.id = o.child_account_id
JOIN payment_windows pw ON pw.order_id = o.id
"#;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AllocatedDerivation {
    pub signer_key_ref: String,
    pub derivation_version: u32,
    pub segment: DerivationSegment,
    pub derivation_path: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CreateOrderOutcome {
    Created(OrderRecord),
    Existing(OrderRecord),
}

#[async_trait]
pub trait OrderRepository: Send + Sync {
    async fn allocate_derivation_segment(
        &self,
        cursor_id: &str,
    ) -> Result<AllocatedDerivation, RepositoryError>;

    async fn create_order_idempotent(
        &self,
        command: CreateOrderCommand,
    ) -> Result<CreateOrderOutcome, RepositoryError>;

    async fn get_order(&self, id: Uuid) -> Result<Option<OrderRecord>, RepositoryError>;

    async fn get_order_view(&self, id: Uuid) -> Result<Option<OrderView>, RepositoryError>;

    async fn get_order_by_external_id(
        &self,
        external_id: &str,
    ) -> Result<Option<OrderRecord>, RepositoryError>;
}

#[async_trait]
pub trait PaymentWindowCandidateRepository: Send + Sync {
    async fn lookup_payment_window_candidates(
        &self,
        chain_id: u64,
        token_address: EvmAddress,
        to_addresses: &[EvmAddress],
    ) -> Result<Vec<PaymentWindowCandidate>, RepositoryError>;
}

#[derive(Clone, Debug)]
pub struct PgOrderRepository {
    pub pool: PgPool,
}

impl PgOrderRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

impl From<PgPool> for PgOrderRepository {
    fn from(pool: PgPool) -> Self {
        Self::new(pool)
    }
}

#[async_trait]
impl OrderRepository for PgOrderRepository {
    async fn allocate_derivation_segment(
        &self,
        cursor_id: &str,
    ) -> Result<AllocatedDerivation, RepositoryError> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        let allocated = allocate_derivation_segment_tx(&mut tx, cursor_id).await?;
        tx.commit().await.map_err(db_error)?;
        Ok(allocated)
    }

    async fn create_order_idempotent(
        &self,
        command: CreateOrderCommand,
    ) -> Result<CreateOrderOutcome, RepositoryError> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;

        sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1)::bigint)")
            .bind(&command.external_id)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;

        if let Some(existing) = fetch_order_by_external_id_tx(&mut tx, &command.external_id).await?
        {
            if existing.request_hash == command.request_hash {
                tx.commit().await.map_err(db_error)?;
                return Ok(CreateOrderOutcome::Existing(existing));
            }

            tx.rollback().await.map_err(db_error)?;
            return Err(idempotency_conflict(&command.external_id, existing.id));
        }

        let order = insert_order_tx(&mut tx, command).await?;
        tx.commit().await.map_err(db_error)?;
        Ok(CreateOrderOutcome::Created(order))
    }

    async fn get_order(&self, id: Uuid) -> Result<Option<OrderRecord>, RepositoryError> {
        let sql = format!("{ORDER_SELECT} WHERE o.id = $1");
        let row = sqlx::query(&sql)
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(db_error)?;

        row.map(|row| row_to_order_record(&row)).transpose()
    }

    async fn get_order_view(&self, id: Uuid) -> Result<Option<OrderView>, RepositoryError> {
        let sql = format!("{ORDER_VIEW_SELECT} WHERE o.id = $1");
        let row = sqlx::query(&sql)
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(db_error)?;

        row.map(row_to_order_view).transpose()
    }

    async fn get_order_by_external_id(
        &self,
        external_id: &str,
    ) -> Result<Option<OrderRecord>, RepositoryError> {
        let sql = format!("{ORDER_SELECT} WHERE o.external_id = $1");
        let row = sqlx::query(&sql)
            .bind(external_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(db_error)?;

        row.map(|row| row_to_order_record(&row)).transpose()
    }
}

#[async_trait]
impl PaymentWindowCandidateRepository for PgOrderRepository {
    async fn lookup_payment_window_candidates(
        &self,
        chain_id: u64,
        token_address: EvmAddress,
        to_addresses: &[EvmAddress],
    ) -> Result<Vec<PaymentWindowCandidate>, RepositoryError> {
        if to_addresses.is_empty() {
            return Ok(Vec::new());
        }

        let chain_id_i64 = u64_to_i64(chain_id, "chain_id")?;
        let token_address_hex = token_address.to_lower_hex();
        let address_hexes = to_addresses
            .iter()
            .map(|address| address.to_lower_hex())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();

        let rows = sqlx::query(
            r#"
            SELECT
                o.id AS order_id,
                o.child_account_id,
                pw.receive_address,
                o.chain_id,
                o.token_address,
                o.expected_amount_raw,
                o.paid_amount_raw,
                o.status AS order_status,
                pw.window_from,
                pw.window_from_block,
                pw.window_from_block_hash,
                pw.expires_at,
                pw.monitor_until
            FROM payment_windows pw
            JOIN orders o ON o.id = pw.order_id
            WHERE o.chain_id = $1
              AND o.token_address = $2
              AND pw.receive_address = ANY($3::text[])
            ORDER BY pw.receive_address, pw.window_from_block, o.id
            "#,
        )
        .bind(chain_id_i64)
        .bind(&token_address_hex)
        .bind(&address_hexes)
        .fetch_all(&self.pool)
        .await
        .map_err(db_error)?;

        rows.iter().map(row_to_payment_window_candidate).collect()
    }
}

async fn allocate_derivation_segment_tx(
    tx: &mut Transaction<'_, Postgres>,
    cursor_id: &str,
) -> Result<AllocatedDerivation, RepositoryError> {
    let row = sqlx::query(
        r#"
        SELECT
            signer_key_ref,
            derivation_version,
            account_index,
            change_index,
            next_address_index
        FROM wallet_cursors
        WHERE id = $1
        FOR UPDATE
        "#,
    )
    .bind(cursor_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(db_error)?;

    let Some(row) = row else {
        return Err(not_found("wallet_cursors", cursor_id));
    };

    let signer_key_ref: String = row.try_get("signer_key_ref").map_err(db_error)?;
    let derivation_version: i32 = row.try_get("derivation_version").map_err(db_error)?;
    let account_index = index_from_i64(row.try_get("account_index").map_err(db_error)?, "account")?;
    let change_index = index_from_i64(row.try_get("change_index").map_err(db_error)?, "change")?;
    let address_index = index_from_i64(
        row.try_get("next_address_index").map_err(db_error)?,
        "address",
    )?;

    let segment = DerivationSegment::new(account_index, change_index, address_index)
        .map_err(derivation_segment_data_error)?;
    let next_segment = match segment.next() {
        Ok(next) => next,
        Err(DerivationSegmentError::Exhausted) => {
            return Err(RepositoryError::DerivationExhausted);
        }
        Err(error) => return Err(derivation_segment_data_error(error)),
    };

    sqlx::query(
        r#"
        UPDATE wallet_cursors
        SET
            account_index = $2,
            change_index = $3,
            next_address_index = $4,
            updated_at = now()
        WHERE id = $1
        "#,
    )
    .bind(cursor_id)
    .bind(i64::from(next_segment.account_index))
    .bind(i64::from(next_segment.change_index))
    .bind(i64::from(next_segment.address_index))
    .execute(&mut **tx)
    .await
    .map_err(db_error)?;

    Ok(AllocatedDerivation {
        signer_key_ref,
        derivation_version: i32_to_u32(derivation_version, "wallet_cursors.derivation_version")?,
        segment,
        derivation_path: segment.derivation_path(),
    })
}

async fn insert_order_tx(
    tx: &mut Transaction<'_, Postgres>,
    command: CreateOrderCommand,
) -> Result<OrderRecord, RepositoryError> {
    validate_create_order_command(&command)?;

    let chain_id = u64_to_i64(command.chain_id, "chain_id")?;
    let child_account_id = command.child_account.id;
    let receive_address = command.child_account.address.to_lower_hex();
    let token_address = command.token_address.to_lower_hex();
    let expected_amount_raw = raw_amount_to_big_decimal(command.expected_amount_raw)?;
    let derivation_version = u32_to_i32(
        command.child_account.derivation_version,
        "child_accounts.derivation_version",
    )?;
    let window_from_block = u64_to_i64(
        command.payment_window.window_from_block.number,
        "window_from_block",
    )?;
    let window_from_block_hash = command.payment_window.window_from_block.hash.to_lower_hex();

    sqlx::query(
        r#"
        INSERT INTO child_accounts (
            id,
            signer_key_ref,
            derivation_version,
            account_index,
            change_index,
            address_index,
            derivation_path,
            address,
            last_used_at
        ) VALUES (
            $1, $2, $3, $4, $5, $6, $7, $8, now()
        )
        "#,
    )
    .bind(child_account_id)
    .bind(&command.child_account.signer_key_ref)
    .bind(derivation_version)
    .bind(i64::from(
        command.child_account.derivation_segment.account_index,
    ))
    .bind(i64::from(
        command.child_account.derivation_segment.change_index,
    ))
    .bind(i64::from(
        command.child_account.derivation_segment.address_index,
    ))
    .bind(&command.child_account.derivation_path)
    .bind(&receive_address)
    .execute(&mut **tx)
    .await
    .map_err(db_error)?;

    sqlx::query(
        r#"
        INSERT INTO orders (
            id,
            external_id,
            request_hash,
            child_account_id,
            receive_address,
            chain_id,
            token_address,
            expected_amount_raw,
            status,
            expires_at,
            monitor_until
        ) VALUES (
            $1, $2, $3, $4, $5, $6, $7, $8, 'pending', $9, $10
        )
        "#,
    )
    .bind(command.order_id)
    .bind(&command.external_id)
    .bind(&command.request_hash)
    .bind(child_account_id)
    .bind(&receive_address)
    .bind(chain_id)
    .bind(&token_address)
    .bind(expected_amount_raw)
    .bind(command.payment_window.expires_at)
    .bind(command.payment_window.monitor_until)
    .execute(&mut **tx)
    .await
    .map_err(db_error)?;

    sqlx::query(
        r#"
        INSERT INTO payment_windows (
            id,
            order_id,
            child_account_id,
            receive_address,
            window_from,
            window_from_block,
            window_from_block_hash,
            expires_at,
            monitor_until
        ) VALUES (
            $1, $2, $3, $4, $5, $6, $7, $8, $9
        )
        "#,
    )
    .bind(command.payment_window.id)
    .bind(command.order_id)
    .bind(child_account_id)
    .bind(&receive_address)
    .bind(command.payment_window.window_from)
    .bind(window_from_block)
    .bind(&window_from_block_hash)
    .bind(command.payment_window.expires_at)
    .bind(command.payment_window.monitor_until)
    .execute(&mut **tx)
    .await
    .map_err(db_error)?;

    fetch_order_by_id_tx(tx, command.order_id)
        .await?
        .ok_or_else(|| {
            invalid_persisted_state(format!(
                "created order {} was not readable",
                command.order_id
            ))
        })
}

async fn fetch_order_by_id_tx(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
) -> Result<Option<OrderRecord>, RepositoryError> {
    let sql = format!("{ORDER_SELECT} WHERE o.id = $1");
    let row = sqlx::query(&sql)
        .bind(id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(db_error)?;

    row.map(|row| row_to_order_record(&row)).transpose()
}

async fn fetch_order_by_external_id_tx(
    tx: &mut Transaction<'_, Postgres>,
    external_id: &str,
) -> Result<Option<OrderRecord>, RepositoryError> {
    let sql = format!("{ORDER_SELECT} WHERE o.external_id = $1");
    let row = sqlx::query(&sql)
        .bind(external_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(db_error)?;

    row.map(|row| row_to_order_record(&row)).transpose()
}

fn row_to_order_record(row: &PgRow) -> Result<OrderRecord, RepositoryError> {
    Ok(OrderRecord {
        id: row.try_get("id").map_err(db_error)?,
        external_id: row.try_get("external_id").map_err(db_error)?,
        request_hash: row.try_get("request_hash").map_err(db_error)?,
        child_account_id: row.try_get("child_account_id").map_err(db_error)?,
        receive_address: parse_evm_address(row.try_get("receive_address").map_err(db_error)?)?,
        chain_id: i64_to_u64(row.try_get("chain_id").map_err(db_error)?, "chain_id")?,
        token_address: parse_evm_address(row.try_get("token_address").map_err(db_error)?)?,
        expected_amount_raw: big_decimal_to_raw_amount(
            row.try_get("expected_amount_raw").map_err(db_error)?,
        )?,
        paid_amount_raw: big_decimal_to_raw_amount(
            row.try_get("paid_amount_raw").map_err(db_error)?,
        )?,
        status: parse_order_status(row.try_get("status").map_err(db_error)?)?,
        expires_at: row.try_get("expires_at").map_err(db_error)?,
        monitor_until: row.try_get("monitor_until").map_err(db_error)?,
        created_at: row.try_get("created_at").map_err(db_error)?,
        updated_at: row.try_get("updated_at").map_err(db_error)?,
    })
}

fn row_to_order_view(row: PgRow) -> Result<OrderView, RepositoryError> {
    let order = row_to_order_record(&row)?;
    let child_account = ChildAccountRecord {
        id: row.try_get("ca_id").map_err(db_error)?,
        signer_key_ref: row.try_get("ca_signer_key_ref").map_err(db_error)?,
        derivation_version: i32_to_u32(
            row.try_get("ca_derivation_version").map_err(db_error)?,
            "child_accounts.derivation_version",
        )?,
        derivation_segment: DerivationSegment::new(
            index_from_i64(
                row.try_get("ca_account_index").map_err(db_error)?,
                "account",
            )?,
            index_from_i64(row.try_get("ca_change_index").map_err(db_error)?, "change")?,
            index_from_i64(
                row.try_get("ca_address_index").map_err(db_error)?,
                "address",
            )?,
        )?,
        derivation_path: row.try_get("ca_derivation_path").map_err(db_error)?,
        address: parse_evm_address(row.try_get("ca_address").map_err(db_error)?)?,
        last_used_at: row.try_get("ca_last_used_at").map_err(db_error)?,
        created_at: row.try_get("ca_created_at").map_err(db_error)?,
    };
    let window_from_block = ChainBlockRef::new(
        i64_to_u64(
            row.try_get("pw_window_from_block").map_err(db_error)?,
            "payment_windows.window_from_block",
        )?,
        parse_block_hash(row.try_get("pw_window_from_block_hash").map_err(db_error)?)?,
    );
    let payment_window = PaymentWindowRecord {
        id: row.try_get("pw_id").map_err(db_error)?,
        order_id: row.try_get("pw_order_id").map_err(db_error)?,
        child_account_id: row.try_get("pw_child_account_id").map_err(db_error)?,
        receive_address: parse_evm_address(row.try_get("pw_receive_address").map_err(db_error)?)?,
        window_from: row.try_get("pw_window_from").map_err(db_error)?,
        window_from_block,
        expires_at: row.try_get("pw_expires_at").map_err(db_error)?,
        monitor_until: row.try_get("pw_monitor_until").map_err(db_error)?,
        created_at: row.try_get("pw_created_at").map_err(db_error)?,
    };

    Ok(OrderView {
        order,
        child_account,
        payment_window,
    })
}

fn row_to_payment_window_candidate(row: &PgRow) -> Result<PaymentWindowCandidate, RepositoryError> {
    let window_from_block = ChainBlockRef::new(
        i64_to_u64(
            row.try_get("window_from_block").map_err(db_error)?,
            "payment_windows.window_from_block",
        )?,
        parse_block_hash(row.try_get("window_from_block_hash").map_err(db_error)?)?,
    );

    Ok(PaymentWindowCandidate {
        order_id: row.try_get("order_id").map_err(db_error)?,
        child_account_id: row.try_get("child_account_id").map_err(db_error)?,
        receive_address: parse_evm_address(row.try_get("receive_address").map_err(db_error)?)?,
        chain_id: i64_to_u64(row.try_get("chain_id").map_err(db_error)?, "chain_id")?,
        token_address: parse_evm_address(row.try_get("token_address").map_err(db_error)?)?,
        expected_amount_raw: big_decimal_to_raw_amount(
            row.try_get("expected_amount_raw").map_err(db_error)?,
        )?,
        paid_amount_raw: big_decimal_to_raw_amount(
            row.try_get("paid_amount_raw").map_err(db_error)?,
        )?,
        order_status: parse_order_status(row.try_get("order_status").map_err(db_error)?)?,
        window_from: row.try_get("window_from").map_err(db_error)?,
        window_from_block,
        expires_at: row.try_get("expires_at").map_err(db_error)?,
        monitor_until: row.try_get("monitor_until").map_err(db_error)?,
    })
}

fn validate_create_order_command(command: &CreateOrderCommand) -> Result<(), RepositoryError> {
    if command.payment_window.order_id != command.order_id {
        return Err(invalid_persisted_state(
            "payment_window.order_id does not match command.order_id",
        ));
    }

    if command.payment_window.child_account_id != command.child_account.id {
        return Err(invalid_persisted_state(
            "payment_window.child_account_id does not match child_account.id",
        ));
    }

    if command.payment_window.receive_address != command.child_account.address {
        return Err(invalid_persisted_state(
            "payment_window.receive_address does not match child_account.address",
        ));
    }

    let expected_path = command.child_account.derivation_segment.derivation_path();
    if command.child_account.derivation_path != expected_path {
        return Err(invalid_persisted_state(format!(
            "child_account.derivation_path does not match segment: expected {expected_path}, got {}",
            command.child_account.derivation_path
        )));
    }

    Ok(())
}

fn raw_amount_to_big_decimal(amount: RawAmount) -> Result<BigDecimal, RepositoryError> {
    BigDecimal::from_str(&amount.to_string())
        .map_err(|error| invalid_persisted_state(format!("invalid raw amount {amount}: {error}")))
}

fn big_decimal_to_raw_amount(amount: BigDecimal) -> Result<RawAmount, RepositoryError> {
    let value = amount.with_scale(0).to_string();
    RawAmount::parse_dec_str(&value).map_err(|error| {
        invalid_persisted_state(format!("invalid persisted raw amount {value}: {error}"))
    })
}

fn parse_evm_address(value: String) -> Result<EvmAddress, RepositoryError> {
    EvmAddress::from_str(&value).map_err(|error| {
        RepositoryError::invalid_db_value("evm_address", value, format!("{error}"))
    })
}

fn parse_block_hash(value: String) -> Result<crate::domain::BlockHash, RepositoryError> {
    crate::domain::BlockHash::from_str(&value)
        .map_err(|error| RepositoryError::invalid_db_value("block_hash", value, format!("{error}")))
}

fn parse_order_status(value: String) -> Result<OrderStatus, RepositoryError> {
    match value.as_str() {
        "pending" => Ok(OrderStatus::Pending),
        "partial" => Ok(OrderStatus::Partial),
        "confirming" => Ok(OrderStatus::Confirming),
        "paid" => Ok(OrderStatus::Paid),
        "expired" => Ok(OrderStatus::Expired),
        _ => Err(RepositoryError::invalid_db_value(
            "orders.status",
            value,
            "unknown order status",
        )),
    }
}

fn index_from_i64(value: i64, field: &'static str) -> Result<u32, RepositoryError> {
    if value < 0 || value > i64::from(MAX_DERIVATION_INDEX) {
        return Err(invalid_persisted_state(format!(
            "{field} derivation index {value} outside 0..={MAX_DERIVATION_INDEX}"
        )));
    }

    Ok(value as u32)
}

fn u32_to_i32(value: u32, field: &'static str) -> Result<i32, RepositoryError> {
    i32::try_from(value).map_err(|source| RepositoryError::integer_out_of_range(field, source))
}

fn i32_to_u32(value: i32, field: &'static str) -> Result<u32, RepositoryError> {
    u32::try_from(value).map_err(|source| RepositoryError::integer_out_of_range(field, source))
}

fn u64_to_i64(value: u64, field: &'static str) -> Result<i64, RepositoryError> {
    i64::try_from(value).map_err(|source| RepositoryError::integer_out_of_range(field, source))
}

fn i64_to_u64(value: i64, field: &'static str) -> Result<u64, RepositoryError> {
    u64::try_from(value).map_err(|source| RepositoryError::integer_out_of_range(field, source))
}

fn derivation_segment_data_error(error: DerivationSegmentError) -> RepositoryError {
    error.into()
}

fn db_error(error: sqlx::Error) -> RepositoryError {
    error.into()
}

fn invalid_persisted_state(message: impl Into<String>) -> RepositoryError {
    RepositoryError::invariant_violation(message)
}

fn not_found(entity: &'static str, key: &str) -> RepositoryError {
    RepositoryError::not_found(entity, key)
}

fn idempotency_conflict(external_id: &str, existing_id: Uuid) -> RepositoryError {
    RepositoryError::idempotency_conflict("orders", external_id, Some(existing_id))
}
