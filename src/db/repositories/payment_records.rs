use std::str::FromStr;

use bigdecimal::BigDecimal;
use sqlx::{Postgres, Row, Transaction, postgres::PgRow};
use uuid::Uuid;

use crate::domain::{
    BlockHash, EvmAddress, PaymentChainStatus, PaymentMatchStatus, RawAmount, TxHash,
};

use super::{RepositoryError, types::MatchedPaymentInput, types::PaymentRecord};

pub async fn upsert_matched_payment_tx(
    tx: &mut Transaction<'_, Postgres>,
    chain_id: u64,
    token_address: EvmAddress,
    payment: &MatchedPaymentInput,
) -> Result<PaymentRecord, RepositoryError> {
    let chain_id_i64 = u64_to_i64(chain_id, "chain_id")?;
    let token_address_hex = token_address.to_lower_hex();
    let tx_hash_hex = payment.tx_hash.to_lower_hex();
    let log_index_i64 = u64_to_i64(payment.log_index, "log_index")?;
    let from_address_hex = payment.from_address.to_lower_hex();
    let to_address_hex = payment.to_address.to_lower_hex();
    let amount_raw = raw_amount_to_numeric(payment.amount_raw)?;
    let block_number_i64 = u64_to_i64(payment.block_number, "block_number")?;
    let block_hash_hex = payment.block_hash.to_lower_hex();
    let confirmations_i64 = u64_to_i64(payment.confirmations, "confirmations")?;
    let chain_status = payment_chain_status_str(payment.chain_status);

    let row = sqlx::query(
        r#"
        INSERT INTO payments (
            id,
            order_id,
            child_account_id,
            chain_id,
            token_address,
            tx_hash,
            log_index,
            from_address,
            to_address,
            amount_raw,
            block_number,
            block_hash,
            block_time,
            confirmations,
            match_status,
            chain_status
        )
        SELECT
            $1,
            o.id,
            o.child_account_id,
            o.chain_id,
            o.token_address,
            $5,
            $6,
            $7,
            $8,
            $9,
            $10,
            $11,
            $12,
            $13,
            CASE
                WHEN $10 >= pw.window_from_block AND $12 <= pw.expires_at THEN 'on_time'
                WHEN $10 >= pw.window_from_block AND $12 <= pw.monitor_until THEN 'late'
                ELSE 'outside_window'
            END,
            $14
        FROM orders o
        JOIN payment_windows pw ON pw.order_id = o.id
        WHERE o.id = $2
          AND o.chain_id = $3
          AND o.token_address = $4
          AND o.receive_address = $8
        ON CONFLICT (chain_id, tx_hash, log_index)
        DO UPDATE SET
            order_id = EXCLUDED.order_id,
            child_account_id = EXCLUDED.child_account_id,
            token_address = EXCLUDED.token_address,
            from_address = EXCLUDED.from_address,
            to_address = EXCLUDED.to_address,
            amount_raw = EXCLUDED.amount_raw,
            block_number = EXCLUDED.block_number,
            block_hash = EXCLUDED.block_hash,
            block_time = EXCLUDED.block_time,
            confirmations = EXCLUDED.confirmations,
            match_status = EXCLUDED.match_status,
            chain_status = EXCLUDED.chain_status,
            updated_at = now()
        WHERE payments.order_id = EXCLUDED.order_id
          AND payments.chain_id = EXCLUDED.chain_id
          AND payments.token_address = EXCLUDED.token_address
          AND payments.to_address = EXCLUDED.to_address
        RETURNING
            id,
            order_id,
            child_account_id,
            chain_id,
            token_address,
            tx_hash,
            log_index,
            from_address,
            to_address,
            amount_raw,
            block_number,
            block_hash,
            block_time,
            confirmations,
            match_status,
            chain_status,
            created_at,
            updated_at
        "#,
    )
    .bind(payment.id)
    .bind(payment.order_id)
    .bind(chain_id_i64)
    .bind(&token_address_hex)
    .bind(&tx_hash_hex)
    .bind(log_index_i64)
    .bind(&from_address_hex)
    .bind(&to_address_hex)
    .bind(amount_raw)
    .bind(block_number_i64)
    .bind(&block_hash_hex)
    .bind(payment.block_time)
    .bind(confirmations_i64)
    .bind(chain_status)
    .fetch_optional(&mut **tx)
    .await
    .map_err(RepositoryError::Database)?
    .ok_or_else(|| RepositoryError::StalePaymentMatch {
        order_id: payment.order_id,
        tx_hash: payment.tx_hash,
        log_index: payment.log_index,
    })?;

    payment_record_from_row(&row)
}

pub async fn payment_records_for_order_tx(
    tx: &mut Transaction<'_, Postgres>,
    order_id: Uuid,
) -> Result<Vec<PaymentRecord>, RepositoryError> {
    let rows = sqlx::query(
        r#"
        SELECT
            id,
            order_id,
            child_account_id,
            chain_id,
            token_address,
            tx_hash,
            log_index,
            from_address,
            to_address,
            amount_raw,
            block_number,
            block_hash,
            block_time,
            confirmations,
            match_status,
            chain_status,
            created_at,
            updated_at
        FROM payments
        WHERE order_id = $1
        ORDER BY block_number, log_index
        "#,
    )
    .bind(order_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(RepositoryError::Database)?;

    rows.iter().map(payment_record_from_row).collect()
}

pub fn payment_record_from_row(row: &PgRow) -> Result<PaymentRecord, RepositoryError> {
    let token_address: String = row
        .try_get("token_address")
        .map_err(RepositoryError::Database)?;
    let tx_hash: String = row.try_get("tx_hash").map_err(RepositoryError::Database)?;
    let from_address: String = row
        .try_get("from_address")
        .map_err(RepositoryError::Database)?;
    let to_address: String = row
        .try_get("to_address")
        .map_err(RepositoryError::Database)?;
    let block_hash: String = row
        .try_get("block_hash")
        .map_err(RepositoryError::Database)?;
    let amount_raw: BigDecimal = row
        .try_get("amount_raw")
        .map_err(RepositoryError::Database)?;
    let match_status: String = row
        .try_get("match_status")
        .map_err(RepositoryError::Database)?;
    let chain_status: String = row
        .try_get("chain_status")
        .map_err(RepositoryError::Database)?;

    Ok(PaymentRecord {
        id: row.try_get("id").map_err(RepositoryError::Database)?,
        order_id: row.try_get("order_id").map_err(RepositoryError::Database)?,
        child_account_id: row
            .try_get("child_account_id")
            .map_err(RepositoryError::Database)?,
        chain_id: i64_to_u64(
            row.try_get("chain_id").map_err(RepositoryError::Database)?,
            "chain_id",
        )?,
        token_address: parse_evm_address(&token_address, "token_address")?,
        tx_hash: parse_tx_hash(&tx_hash)?,
        log_index: i64_to_u64(
            row.try_get("log_index")
                .map_err(RepositoryError::Database)?,
            "log_index",
        )?,
        from_address: parse_evm_address(&from_address, "from_address")?,
        to_address: parse_evm_address(&to_address, "to_address")?,
        amount_raw: raw_amount_from_numeric(amount_raw)?,
        block_number: i64_to_u64(
            row.try_get("block_number")
                .map_err(RepositoryError::Database)?,
            "block_number",
        )?,
        block_hash: parse_block_hash(&block_hash)?,
        block_time: row
            .try_get("block_time")
            .map_err(RepositoryError::Database)?,
        confirmations: i64_to_u64(
            row.try_get("confirmations")
                .map_err(RepositoryError::Database)?,
            "confirmations",
        )?,
        match_status: parse_payment_match_status(&match_status)?,
        chain_status: parse_payment_chain_status(&chain_status)?,
        created_at: row
            .try_get("created_at")
            .map_err(RepositoryError::Database)?,
        updated_at: row
            .try_get("updated_at")
            .map_err(RepositoryError::Database)?,
    })
}

pub fn u64_to_i64(value: u64, field: &'static str) -> Result<i64, RepositoryError> {
    i64::try_from(value).map_err(|_| {
        RepositoryError::invalid_argument(field, "value exceeds PostgreSQL bigint range")
    })
}

pub fn i64_to_u64(value: i64, field: &'static str) -> Result<u64, RepositoryError> {
    u64::try_from(value)
        .map_err(|_| RepositoryError::invalid_persisted_state(format!("{field} is negative")))
}

fn raw_amount_to_numeric(amount: RawAmount) -> Result<BigDecimal, RepositoryError> {
    BigDecimal::from_str(&amount.to_string()).map_err(|error| {
        RepositoryError::invalid_persisted_state(format!(
            "raw amount cannot be encoded as numeric: {error}"
        ))
    })
}

fn raw_amount_from_numeric(value: BigDecimal) -> Result<RawAmount, RepositoryError> {
    RawAmount::parse_dec_str(&value.to_string()).map_err(|error| {
        RepositoryError::invalid_persisted_state(format!(
            "database amount_raw is not uint256-compatible: {error}"
        ))
    })
}

fn parse_evm_address(value: &str, field: &'static str) -> Result<EvmAddress, RepositoryError> {
    value.parse().map_err(|error| {
        RepositoryError::invalid_persisted_state(format!("invalid {field}: {error}"))
    })
}

fn parse_tx_hash(value: &str) -> Result<TxHash, RepositoryError> {
    value.parse().map_err(|error| {
        RepositoryError::invalid_persisted_state(format!("invalid tx_hash: {error}"))
    })
}

fn parse_block_hash(value: &str) -> Result<BlockHash, RepositoryError> {
    value.parse().map_err(|error| {
        RepositoryError::invalid_persisted_state(format!("invalid block_hash: {error}"))
    })
}

fn parse_payment_match_status(value: &str) -> Result<PaymentMatchStatus, RepositoryError> {
    match value {
        "on_time" => Ok(PaymentMatchStatus::OnTime),
        "late" => Ok(PaymentMatchStatus::Late),
        "outside_window" => Ok(PaymentMatchStatus::OutsideWindow),
        _ => Err(RepositoryError::invalid_persisted_state(format!(
            "invalid payment match_status: {value}"
        ))),
    }
}

fn parse_payment_chain_status(value: &str) -> Result<PaymentChainStatus, RepositoryError> {
    match value {
        "observed" => Ok(PaymentChainStatus::Observed),
        "confirmed" => Ok(PaymentChainStatus::Confirmed),
        "orphaned" => Ok(PaymentChainStatus::Orphaned),
        _ => Err(RepositoryError::invalid_persisted_state(format!(
            "invalid payment chain_status: {value}"
        ))),
    }
}

fn payment_chain_status_str(status: PaymentChainStatus) -> &'static str {
    match status {
        PaymentChainStatus::Observed => "observed",
        PaymentChainStatus::Confirmed => "confirmed",
        PaymentChainStatus::Orphaned => "orphaned",
    }
}
