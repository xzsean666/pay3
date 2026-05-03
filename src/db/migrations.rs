use std::num::TryFromIntError;

use sqlx::{PgPool, Postgres, Transaction};
use thiserror::Error;

use crate::domain::EvmAddress;

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("src/db/migrations");

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeSeedConfig {
    pub signer_key_ref: String,
    pub chain_id: u64,
    pub token_address: EvmAddress,
    pub treasury_address: EvmAddress,
    pub start_block: u64,
}

#[derive(Debug, Error)]
pub enum MigrationBootstrapError {
    #[error("migration failed: {0}")]
    Migration(#[from] sqlx::migrate::MigrateError),
    #[error("database query failed: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("numeric config value does not fit PostgreSQL bigint")]
    IntegerOutOfRange(#[from] TryFromIntError),
}

pub async fn run_schema_migrations(pool: &PgPool) -> Result<(), MigrationBootstrapError> {
    MIGRATOR.run(pool).await?;
    Ok(())
}

pub async fn seed_runtime_config(
    pool: &PgPool,
    config: &RuntimeSeedConfig,
) -> Result<(), MigrationBootstrapError> {
    let mut tx = pool.begin().await?;
    seed_wallet_cursor(&mut tx, config).await?;
    seed_chain_cursor(&mut tx, config).await?;
    seed_treasury_address(&mut tx, config).await?;
    tx.commit().await?;
    Ok(())
}

async fn seed_wallet_cursor(
    tx: &mut Transaction<'_, Postgres>,
    config: &RuntimeSeedConfig,
) -> Result<(), MigrationBootstrapError> {
    let result = sqlx::query(
        r#"
        INSERT INTO wallet_cursors (
            id,
            signer_key_ref,
            derivation_version,
            account_index,
            change_index,
            next_address_index
        )
        VALUES ('default', $1, 1, 0, 0, 0)
        ON CONFLICT (id) DO UPDATE
        SET signer_key_ref = EXCLUDED.signer_key_ref,
            updated_at = now()
        WHERE wallet_cursors.signer_key_ref IN ('unconfigured', EXCLUDED.signer_key_ref)
        "#,
    )
    .bind(&config.signer_key_ref)
    .execute(&mut **tx)
    .await?;

    if result.rows_affected() == 0 {
        return Err(sqlx::Error::Protocol(
            "wallet cursor signer_key_ref differs from runtime config".to_string(),
        )
        .into());
    }

    Ok(())
}

async fn seed_chain_cursor(
    tx: &mut Transaction<'_, Postgres>,
    config: &RuntimeSeedConfig,
) -> Result<(), MigrationBootstrapError> {
    let chain_id = i64::try_from(config.chain_id)?;
    let start_block = i64::try_from(config.start_block)?;
    let last_scanned_block = start_block.saturating_sub(1);

    sqlx::query(
        r#"
        INSERT INTO chain_cursors (
            chain_id,
            token_address,
            last_scanned_block,
            seen_kv_reorg_epoch
        )
        VALUES ($1, $2, $3, 0)
        ON CONFLICT (chain_id, token_address) DO NOTHING
        "#,
    )
    .bind(chain_id)
    .bind(config.token_address.to_string())
    .bind(last_scanned_block)
    .execute(&mut **tx)
    .await?;

    Ok(())
}

async fn seed_treasury_address(
    tx: &mut Transaction<'_, Postgres>,
    config: &RuntimeSeedConfig,
) -> Result<(), MigrationBootstrapError> {
    sqlx::query(
        r#"
        INSERT INTO treasury_addresses (
            chain_id,
            token_address,
            treasury_address
        )
        VALUES ($1, $2, $3)
        ON CONFLICT (chain_id, token_address, treasury_address) DO NOTHING
        "#,
    )
    .bind(i64::try_from(config.chain_id)?)
    .bind(config.token_address.to_string())
    .bind(config.treasury_address.to_string())
    .execute(&mut **tx)
    .await?;

    Ok(())
}
