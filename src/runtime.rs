use std::{fs, path::Path, sync::Arc};

use axum::Router;
use sqlx::PgPool;
use thiserror::Error;

use crate::{
    api::{self, OrderResponseConfig},
    auth::JwtVerifier,
    chain::{ChainError, RpcRangeSource},
    config::{AppConfig, ConfigError, SignerMode},
    db::{
        migrations::{
            MigrationBootstrapError, RuntimeSeedConfig, run_schema_migrations, seed_runtime_config,
        },
        repositories::{
            PgAuditRepository, PgCollectionRepository, PgOrderRepository, PgOutboundRepository,
            PgVerifiedPaymentRecorder,
        },
    },
    domain::{CollectionFees, RawAmount},
    health::StaticDependencyRegistry,
    services::{
        collections::{AssumePrefundedGas, CollectionService, CollectionServiceConfig},
        orders::{OrderService, OrderServiceConfig, SystemClock},
        verify::{ManualOrderVerifyService, ManualVerifyConfig},
    },
    signer::{DeterministicFakeSigner, SignerError, SignerProvider},
    transfer_log_store::{
        LogSourceKind, RedbTransferLogIngestor, ScanTargetMode, TransferLogIngestor,
        TransferLogStoreError, TransferLogStreamConfig,
    },
    wallet::{DeterministicFakeDeriver, HdWallet, WalletError},
    workers::transfer_log_ingestor::{
        TransferLogIngestorLoopConfig, TransferLogIngestorLoopError,
        spawn_transfer_log_ingestor_loop,
    },
};

const LATE_PAYMENT_MONITOR_SECONDS: u64 = 7 * 24 * 60 * 60;
const TRANSFER_LOG_POLL_INTERVAL_MS: u64 = 5_000;
const TRANSFER_LOG_BATCH_SIZE_BLOCKS: u64 = 100;
const TRANSFER_LOG_MAX_BATCH_SIZE_BLOCKS: u64 = 1_000;
const TRANSFER_LOG_MAX_LOGS_PER_PAGE: usize = 1_000;
const TRANSFER_LOG_MAX_UNIQUE_TO_ADDRESSES_PER_BATCH: usize = 1_000;
const TRANSFER_LOG_MAX_DB_FALLBACK_ADDRESSES: usize = 1_000;
const TRANSFER_LOG_CAPACITY_PROBE_BLOCKS: u64 = 100;
const TRANSFER_LOG_RPC_MAX_RETRIES: u32 = 3;
const COLLECTION_GAS_LIMIT: u64 = 80_000;
const COLLECTION_MAX_FEE_PER_GAS_WEI: u64 = 50_000_000_000;
const COLLECTION_MAX_PRIORITY_FEE_PER_GAS_WEI: u64 = 2_000_000_000;

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error(transparent)]
    Config(#[from] ConfigError),

    #[error("database connection failed: {0}")]
    Database(#[from] sqlx::Error),

    #[error(transparent)]
    Migration(#[from] MigrationBootstrapError),

    #[error("runtime directory setup failed: {0}")]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Auth(#[from] crate::auth::AuthError),

    #[error(transparent)]
    Chain(#[from] ChainError),

    #[error(transparent)]
    TransferLogStore(#[from] TransferLogStoreError),

    #[error(transparent)]
    TransferLogIngestorLoop(#[from] TransferLogIngestorLoopError),

    #[error(transparent)]
    OrderService(#[from] crate::services::orders::OrderServiceError),

    #[error(transparent)]
    CollectionService(#[from] crate::services::collections::CollectionServiceError),

    #[error(transparent)]
    Wallet(#[from] WalletError),

    #[error(transparent)]
    Signer(#[from] SignerError),

    #[error(
        "runtime signer mode {mode:?} is not implemented yet; use SIGNER_MODE=fake only for development/test"
    )]
    UnsupportedSignerMode { mode: SignerMode },
}

pub async fn build_api_router(config: AppConfig) -> Result<Router, RuntimeError> {
    config.validate_profile()?;

    let pool = PgPool::connect(&config.database.url).await?;
    run_schema_migrations(&pool).await?;
    seed_runtime_config(&pool, &runtime_seed_config(&config)).await?;

    ensure_kvdb_parent(&config.kvdb.path)?;
    let rpc_source = RpcRangeSource::from_http_urls(
        config.chain.chain_id,
        &config.chain.rpc_http_urls,
        min_rpc_provider_count(&config),
    )?;
    rpc_source.manager().validate_chain_ids().await?;

    ensure_runtime_signer_health(&config).await?;

    let log_store = RedbTransferLogIngestor::open(rpc_source.clone(), &config.kvdb.path)?;
    let stream_config = transfer_log_stream_config(&config);
    let stream = stream_config.stream_id();
    log_store.ensure_stream(stream_config.clone()).await?;
    let _log_ingestor_loop = spawn_transfer_log_ingestor_loop(
        log_store.clone(),
        TransferLogIngestorLoopConfig::new(
            stream,
            std::time::Duration::from_millis(stream_config.poll_interval_ms),
        ),
    )?;

    let auth = jwt_verifier(&config)?;
    let orders = Arc::new(order_service(&config, pool.clone(), rpc_source.clone())?);
    let collections = Arc::new(collection_service(
        &config,
        pool.clone(),
        rpc_source.clone(),
    )?);
    let order_verify = Arc::new(ManualOrderVerifyService::new(
        PgOrderRepository::new(pool.clone()),
        PgVerifiedPaymentRecorder::new(pool),
        log_store,
        rpc_source,
        SystemClock,
        ManualVerifyConfig::new(
            TRANSFER_LOG_MAX_LOGS_PER_PAGE,
            config.chain.min_confirmations,
        ),
    ));

    Ok(api::router_with_runtime_services(
        StaticDependencyRegistry::all_healthy(),
        auth,
        orders,
        order_verify,
        collections,
        OrderResponseConfig::from_config(&config),
    ))
}

fn runtime_seed_config(config: &AppConfig) -> RuntimeSeedConfig {
    RuntimeSeedConfig {
        signer_key_ref: config.signer.key_ref.clone(),
        chain_id: config.chain.chain_id,
        token_address: config.chain.token_address,
        treasury_address: config.chain.treasury_address,
        start_block: config.chain.start_block,
    }
}

fn jwt_verifier(config: &AppConfig) -> Result<JwtVerifier, RuntimeError> {
    let key_id = config
        .jwt
        .key_id
        .clone()
        .unwrap_or_else(|| "default".to_string());
    Ok(JwtVerifier::new_hs256(
        config.jwt.issuer.clone(),
        config.jwt.audience.clone(),
        [(key_id, config.jwt.secret.clone())],
    )?)
}

fn order_service(
    config: &AppConfig,
    pool: PgPool,
    rpc_source: RpcRangeSource,
) -> Result<OrderService<PgOrderRepository, DeterministicFakeDeriver, RpcRangeSource>, RuntimeError>
{
    let deriver = match &config.signer.mode {
        SignerMode::Fake => DeterministicFakeDeriver::with_allowed_key_refs(
            "pay3-runtime-fake",
            [config.signer.key_ref.clone()],
        )?,
        mode => {
            return Err(RuntimeError::UnsupportedSignerMode { mode: mode.clone() });
        }
    };

    Ok(OrderService::new(
        OrderServiceConfig::new(
            config.chain.chain_id,
            config.chain.token_address,
            LATE_PAYMENT_MONITOR_SECONDS,
        ),
        PgOrderRepository::new(pool),
        HdWallet::new(deriver),
        rpc_source,
    )?)
}

fn collection_service(
    config: &AppConfig,
    pool: PgPool,
    rpc_source: RpcRangeSource,
) -> Result<
    CollectionService<
        PgOrderRepository,
        PgCollectionRepository,
        PgOutboundRepository,
        PgAuditRepository,
        DeterministicFakeSigner,
        RpcRangeSource,
        AssumePrefundedGas,
    >,
    RuntimeError,
> {
    let signer = match &config.signer.mode {
        SignerMode::Fake => DeterministicFakeSigner::with_allowed_key_refs(
            "pay3-runtime-fake",
            [config.signer.key_ref.clone()],
        )?,
        mode => {
            return Err(RuntimeError::UnsupportedSignerMode { mode: mode.clone() });
        }
    };

    Ok(CollectionService::new(
        CollectionServiceConfig::new(
            config.chain.chain_id,
            config.chain.token_address,
            config.chain.treasury_address,
            CollectionFees::new(
                COLLECTION_GAS_LIMIT,
                RawAmount::from(COLLECTION_MAX_FEE_PER_GAS_WEI),
                RawAmount::from(COLLECTION_MAX_PRIORITY_FEE_PER_GAS_WEI),
            ),
        ),
        PgOrderRepository::new(pool.clone()),
        PgCollectionRepository::new(pool.clone()),
        PgOutboundRepository::new(pool.clone()),
        PgAuditRepository::new(pool),
        signer,
        rpc_source,
        AssumePrefundedGas,
    )?)
}

async fn ensure_runtime_signer_health(config: &AppConfig) -> Result<(), RuntimeError> {
    match &config.signer.mode {
        SignerMode::Fake => {
            let signer = DeterministicFakeSigner::with_allowed_key_refs(
                "pay3-runtime-fake",
                [config.signer.key_ref.clone()],
            )?;
            signer.health_check().await?;
            Ok(())
        }
        mode => Err(RuntimeError::UnsupportedSignerMode { mode: mode.clone() }),
    }
}

fn transfer_log_stream_config(config: &AppConfig) -> TransferLogStreamConfig {
    TransferLogStreamConfig {
        chain_id: config.chain.chain_id,
        token_address: config.chain.token_address,
        start_block: config.chain.start_block,
        poll_interval_ms: TRANSFER_LOG_POLL_INTERVAL_MS,
        batch_size_blocks: TRANSFER_LOG_BATCH_SIZE_BLOCKS,
        max_batch_size_blocks: TRANSFER_LOG_MAX_BATCH_SIZE_BLOCKS,
        max_logs_per_page: TRANSFER_LOG_MAX_LOGS_PER_PAGE,
        max_unique_to_addresses_per_batch: TRANSFER_LOG_MAX_UNIQUE_TO_ADDRESSES_PER_BATCH,
        max_db_fallback_addresses: TRANSFER_LOG_MAX_DB_FALLBACK_ADDRESSES,
        capacity_probe_blocks: TRANSFER_LOG_CAPACITY_PROBE_BLOCKS,
        reorg_lookback_blocks: reorg_lookback_blocks(config.chain.min_confirmations),
        target_mode: ScanTargetMode::LatestMinusConfirmations(config.chain.min_confirmations),
        rpc_max_retries: TRANSFER_LOG_RPC_MAX_RETRIES,
        log_source: LogSourceKind::RpcRange,
    }
}

fn reorg_lookback_blocks(min_confirmations: u64) -> u64 {
    min_confirmations.saturating_mul(2).max(12)
}

fn min_rpc_provider_count(config: &AppConfig) -> usize {
    if config.profile.is_production() { 2 } else { 1 }
}

fn ensure_kvdb_parent(path: &Path) -> Result<(), RuntimeError> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    if parent.as_os_str().is_empty() {
        return Ok(());
    }
    fs::create_dir_all(parent)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::config::AppConfig;

    #[test]
    fn transfer_log_stream_config_uses_confirmed_target() {
        let config = test_config(&[
            ("MIN_CONFIRMATIONS", "12"),
            ("START_BLOCK", "42"),
            ("SIGNER_MODE", "fake"),
        ]);

        let stream = transfer_log_stream_config(&config);

        assert_eq!(stream.chain_id, 31337);
        assert_eq!(stream.token_address, config.chain.token_address);
        assert_eq!(stream.start_block, 42);
        assert_eq!(
            stream.target_mode,
            ScanTargetMode::LatestMinusConfirmations(12)
        );
        assert_eq!(stream.reorg_lookback_blocks, 24);
        assert_eq!(stream.log_source, LogSourceKind::RpcRange);
    }

    #[test]
    fn non_production_runtime_accepts_single_rpc_provider() {
        let config = test_config(&[
            ("APP_PROFILE", "development"),
            ("RPC_HTTP_URLS", "http://localhost:8545"),
            ("SIGNER_MODE", "fake"),
        ]);

        assert_eq!(min_rpc_provider_count(&config), 1);
    }

    #[tokio::test]
    async fn runtime_signer_health_is_explicitly_fake_only_for_now() {
        let fake = test_config(&[("SIGNER_MODE", "fake")]);
        ensure_runtime_signer_health(&fake)
            .await
            .expect("fake signer should bootstrap in development");

        let external = test_config(&[("SIGNER_MODE", "external")]);
        let error = ensure_runtime_signer_health(&external)
            .await
            .expect_err("external signer adapter is not implemented yet");

        assert!(matches!(
            error,
            RuntimeError::UnsupportedSignerMode {
                mode: SignerMode::External
            }
        ));
    }

    fn test_config(overrides: &[(&'static str, &'static str)]) -> AppConfig {
        let mut pairs = vec![
            ("APP_PROFILE", "development".to_string()),
            ("APP_BIND", "127.0.0.1:8080".to_string()),
            (
                "DATABASE_URL",
                "postgres://pay3:pay3@localhost:5432/pay3".to_string(),
            ),
            ("KVDB_PATH", "./target/test-pay3.redb".to_string()),
            ("JWT_ISSUER", "pay3".to_string()),
            ("JWT_AUDIENCE", "pay3-api".to_string()),
            ("JWT_SECRET", "0123456789abcdef0123456789abcdef".to_string()),
            ("JWT_KEY_ID", "pay3-key-1".to_string()),
            ("CHAIN_ID", "31337".to_string()),
            (
                "TOKEN_ADDRESS",
                "0x0000000000000000000000000000000000000001".to_string(),
            ),
            ("TOKEN_DECIMALS", "6".to_string()),
            ("TOKEN_SYMBOL", "USDT".to_string()),
            (
                "TREASURY_ADDRESS",
                "0x0000000000000000000000000000000000000002".to_string(),
            ),
            ("RPC_HTTP_URLS", "http://localhost:8545".to_string()),
            ("START_BLOCK", "1".to_string()),
            ("MIN_CONFIRMATIONS", "12".to_string()),
            ("SIGNER_MODE", "fake".to_string()),
            ("SIGNER_KEY_REF", "pay3-master".to_string()),
        ];

        for &(key, value) in overrides {
            if let Some((_, existing)) = pairs
                .iter_mut()
                .find(|(existing_key, _)| *existing_key == key)
            {
                *existing = value.to_string();
            } else {
                pairs.push((key, value.to_string()));
            }
        }

        AppConfig::from_pairs(pairs).expect("test config should parse")
    }

    #[test]
    fn ensure_kvdb_parent_creates_missing_directory() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("nested").join("pay3.redb");

        ensure_kvdb_parent(&PathBuf::from(&path)).expect("parent directory should be created");

        assert!(path.parent().expect("parent").exists());
    }
}
