use std::{fs, path::Path, sync::Arc};

use async_trait::async_trait;
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
            PgPaymentRepository, PgVerifiedPaymentRecorder,
        },
    },
    domain::CollectionFees,
    health::{MetricsRecorder, RuntimeDependencyRegistry, StaticDependencyRegistry},
    services::{
        collections::{AssumePrefundedGas, CollectionService, CollectionServiceConfig},
        orders::{OrderService, OrderServiceConfig, SystemClock},
        payment_windows::{RepositoryPaymentWindowLookup, WatchSetPaymentWindowLookup},
        payments::{PaymentMatcher, PaymentMatchingConfig},
        verify::{ManualOrderVerifyService, ManualVerifyConfig},
    },
    signer::{
        DeterministicFakeSigner, RemoteHttpSigner, SignedTx, SignerError, SignerProvider,
        UnsignedTx,
    },
    transfer_log_store::{
        LogSourceKind, RedbTransferLogIngestor, ScanTargetMode, StreamId, TransferLogIngestor,
        TransferLogStoreError, TransferLogStreamConfig,
    },
    wallet::{AddressDeriver, DeterministicFakeDeriver, HdWallet, WalletError},
    workers::collector::{
        CollectionCollectorConfig, CollectionCollectorError, CollectionCollectorWorker,
        spawn_collection_collector_loop_with_metrics,
    },
    workers::scanner::{
        PaymentScannerConfig, PaymentScannerError, PaymentScannerWorker,
        spawn_payment_scanner_loop_with_metrics,
    },
    workers::transfer_log_ingestor::{
        TransferLogIngestorLoopConfig, TransferLogIngestorLoopError,
        spawn_transfer_log_ingestor_loop_with_metrics,
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
const PAYMENT_SCANNER_POLL_INTERVAL_MS: u64 = 5_000;
const PAYMENT_SCANNER_LEASE_SECONDS: i64 = 30;
const COLLECTION_COLLECTOR_POLL_INTERVAL_MS: u64 = 5_000;

#[derive(Clone, Debug)]
enum RuntimeSigner {
    Fake {
        deriver: DeterministicFakeDeriver,
        signer: DeterministicFakeSigner,
    },
    Remote(RemoteHttpSigner),
}

#[async_trait]
impl AddressDeriver for RuntimeSigner {
    async fn derive_address(
        &self,
        key_ref: &str,
        path: &str,
    ) -> Result<crate::domain::EvmAddress, WalletError> {
        match self {
            Self::Fake { deriver, .. } => {
                AddressDeriver::derive_address(deriver, key_ref, path).await
            }
            Self::Remote(signer) => AddressDeriver::derive_address(signer, key_ref, path).await,
        }
    }
}

#[async_trait]
impl SignerProvider for RuntimeSigner {
    async fn derive_address(
        &self,
        key_ref: &str,
        path: &str,
    ) -> Result<crate::domain::EvmAddress, SignerError> {
        match self {
            Self::Fake { signer, .. } => {
                SignerProvider::derive_address(signer, key_ref, path).await
            }
            Self::Remote(signer) => SignerProvider::derive_address(signer, key_ref, path).await,
        }
    }

    async fn sign_transaction(
        &self,
        key_ref: &str,
        path: &str,
        tx: UnsignedTx,
    ) -> Result<SignedTx, SignerError> {
        match self {
            Self::Fake { signer, .. } => {
                SignerProvider::sign_transaction(signer, key_ref, path, tx).await
            }
            Self::Remote(signer) => {
                SignerProvider::sign_transaction(signer, key_ref, path, tx).await
            }
        }
    }

    async fn health_check(&self) -> Result<(), SignerError> {
        match self {
            Self::Fake { signer, .. } => SignerProvider::health_check(signer).await,
            Self::Remote(signer) => SignerProvider::health_check(signer).await,
        }
    }
}

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
    PaymentScanner(#[from] PaymentScannerError),

    #[error(transparent)]
    CollectionCollector(#[from] CollectionCollectorError),

    #[error(transparent)]
    OrderService(#[from] crate::services::orders::OrderServiceError),

    #[error(transparent)]
    CollectionService(#[from] crate::services::collections::CollectionServiceError),

    #[error(transparent)]
    Wallet(#[from] WalletError),

    #[error(transparent)]
    Signer(#[from] SignerError),

    #[error("runtime signer mode {mode:?} is not supported; SIGNER_MODE=local remains disabled")]
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

    let signer = runtime_signer(&config)?;
    ensure_runtime_signer_health(&signer).await?;
    let metrics = MetricsRecorder::default();
    let dependency_registry =
        RuntimeDependencyRegistry::new(StaticDependencyRegistry::all_healthy(), metrics.clone());

    let log_store = RedbTransferLogIngestor::open(rpc_source.clone(), &config.kvdb.path)?;
    let stream_config = transfer_log_stream_config(&config);
    let stream = stream_config.stream_id();
    log_store.ensure_stream(stream_config.clone()).await?;
    let _log_ingestor_loop = spawn_transfer_log_ingestor_loop_with_metrics(
        log_store.clone(),
        TransferLogIngestorLoopConfig::new(
            stream,
            std::time::Duration::from_millis(stream_config.poll_interval_ms),
            config.chain.min_confirmations.saturating_mul(2),
        ),
        metrics.clone(),
    )?;
    let _payment_scanner_loop = spawn_payment_scanner_loop_with_metrics(
        payment_scanner_worker(&config, pool.clone(), log_store.clone(), rpc_source.clone()),
        std::time::Duration::from_millis(PAYMENT_SCANNER_POLL_INTERVAL_MS),
        metrics.clone(),
    )?;

    let auth = jwt_verifier(&config)?;
    let orders = Arc::new(order_service(
        &config,
        pool.clone(),
        rpc_source.clone(),
        signer.clone(),
    )?);
    let collections = Arc::new(collection_service(
        &config,
        pool.clone(),
        rpc_source.clone(),
        signer.clone(),
    )?);
    let _collection_collector_loop = spawn_collection_collector_loop_with_metrics(
        CollectionCollectorWorker::new(
            collection_service(&config, pool.clone(), rpc_source.clone(), signer.clone())?,
            PgOutboundRepository::new(pool.clone()),
            rpc_source.clone(),
            collection_collector_config(&config),
        ),
        std::time::Duration::from_millis(COLLECTION_COLLECTOR_POLL_INTERVAL_MS),
        metrics.clone(),
    )?;
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

    Ok(api::router_with_runtime_services_and_metrics(
        dependency_registry,
        metrics,
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

fn runtime_signer(config: &AppConfig) -> Result<RuntimeSigner, RuntimeError> {
    match &config.signer.mode {
        SignerMode::Fake => Ok(RuntimeSigner::Fake {
            deriver: DeterministicFakeDeriver::with_allowed_key_refs(
                "pay3-runtime-fake",
                [config.signer.key_ref.clone()],
            )?,
            signer: DeterministicFakeSigner::with_allowed_key_refs(
                "pay3-runtime-fake",
                [config.signer.key_ref.clone()],
            )?,
        }),
        SignerMode::External | SignerMode::Kms | SignerMode::Hsm => {
            let endpoint = config.signer.remote_endpoint.as_deref().ok_or_else(|| {
                RuntimeError::Config(ConfigError::Validation {
                    errors: vec![
                        "non-fake signer modes require SIGNER_REMOTE_ENDPOINT".to_string(),
                    ],
                })
            })?;

            Ok(RuntimeSigner::Remote(RemoteHttpSigner::new(
                endpoint,
                config.signer.remote_request_timeout,
            )?))
        }
        SignerMode::Local => Err(RuntimeError::UnsupportedSignerMode {
            mode: SignerMode::Local,
        }),
    }
}

fn order_service<D>(
    config: &AppConfig,
    pool: PgPool,
    rpc_source: RpcRangeSource,
    deriver: D,
) -> Result<OrderService<PgOrderRepository, D, RpcRangeSource>, RuntimeError>
where
    D: AddressDeriver,
{
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

fn collection_service<S>(
    config: &AppConfig,
    pool: PgPool,
    rpc_source: RpcRangeSource,
    signer: S,
) -> Result<
    CollectionService<
        PgOrderRepository,
        PgCollectionRepository,
        PgOutboundRepository,
        PgAuditRepository,
        S,
        RpcRangeSource,
        AssumePrefundedGas,
    >,
    RuntimeError,
>
where
    S: SignerProvider,
{
    Ok(CollectionService::new(
        collection_service_config(config),
        PgOrderRepository::new(pool.clone()),
        PgCollectionRepository::new(pool.clone()),
        PgOutboundRepository::new(pool.clone()),
        PgAuditRepository::new(pool),
        signer,
        rpc_source,
        AssumePrefundedGas,
    )?)
}

fn collection_service_config(config: &AppConfig) -> CollectionServiceConfig {
    CollectionServiceConfig::new(
        config.chain.chain_id,
        config.chain.token_address,
        config.chain.treasury_address,
        CollectionFees::new(
            config.collection.gas_limit,
            config.collection.max_fee_per_gas_wei,
            config.collection.max_priority_fee_per_gas_wei,
        ),
    )
}

fn collection_collector_config(config: &AppConfig) -> CollectionCollectorConfig {
    CollectionCollectorConfig::new(format!("collection-collector-{}", std::process::id()))
        .with_replacement_stuck_after(config.collector.replacement_stuck_after)
}

fn payment_scanner_worker(
    config: &AppConfig,
    pool: PgPool,
    log_store: RedbTransferLogIngestor<RpcRangeSource>,
    rpc_source: RpcRangeSource,
) -> PaymentScannerWorker<
    PgPaymentRepository,
    PaymentMatcher<
        RedbTransferLogIngestor<RpcRangeSource>,
        WatchSetPaymentWindowLookup<RepositoryPaymentWindowLookup<PgOrderRepository>>,
        RpcRangeSource,
    >,
    RedbTransferLogIngestor<RpcRangeSource>,
    RpcRangeSource,
    SystemClock,
> {
    let stream = StreamId::new(config.chain.chain_id, config.chain.token_address);
    let fallback = RepositoryPaymentWindowLookup::new(
        PgOrderRepository::new(pool.clone()),
        TRANSFER_LOG_MAX_DB_FALLBACK_ADDRESSES,
    );
    let matcher = PaymentMatcher::new(
        log_store.clone(),
        WatchSetPaymentWindowLookup::new(fallback),
        rpc_source.clone(),
        PaymentMatchingConfig {
            stream,
            min_confirmations: config.chain.min_confirmations,
            page_limit: TRANSFER_LOG_MAX_LOGS_PER_PAGE,
            max_unique_to_addresses_per_batch: TRANSFER_LOG_MAX_UNIQUE_TO_ADDRESSES_PER_BATCH,
        },
    );

    PaymentScannerWorker::new(
        PgPaymentRepository::new(pool),
        matcher,
        log_store,
        rpc_source,
        SystemClock,
        PaymentScannerConfig::new(
            format!("payment-scanner-{}", std::process::id()),
            stream,
            time::Duration::seconds(PAYMENT_SCANNER_LEASE_SECONDS),
        ),
    )
}

async fn ensure_runtime_signer_health<S>(signer: &S) -> Result<(), RuntimeError>
where
    S: SignerProvider,
{
    signer.health_check().await?;
    Ok(())
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

    use axum::{
        Router,
        extract::{Json, State},
        routing::{get, post},
    };
    use serde::Deserialize;
    use serde_json::json;
    use tokio::net::TcpListener;

    use super::*;
    use crate::config::AppConfig;
    use crate::domain::{DerivationSegment, EvmAddress, RawAmount, TxHash};
    use crate::wallet::DeriveAddressRequest;

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
    fn collection_service_config_uses_app_config_collection_fees() {
        let config = test_config(&[
            ("COLLECTION_GAS_LIMIT", "90000"),
            ("COLLECTION_MAX_FEE_PER_GAS_WEI", "60000000000"),
            ("COLLECTION_MAX_PRIORITY_FEE_PER_GAS_WEI", "3000000000"),
        ]);

        let service_config = collection_service_config(&config);

        assert_eq!(service_config.chain_id, config.chain.chain_id);
        assert_eq!(service_config.token_address, config.chain.token_address);
        assert_eq!(
            service_config.treasury_address,
            config.chain.treasury_address
        );
        assert_eq!(service_config.fees.gas_limit, 90_000);
        assert_eq!(
            service_config.fees.max_fee_per_gas,
            RawAmount::from(60_000_000_000)
        );
        assert_eq!(
            service_config.fees.max_priority_fee_per_gas,
            RawAmount::from(3_000_000_000)
        );
    }

    #[test]
    fn collection_collector_config_uses_app_config_timeout() {
        let config = test_config(&[("COLLECTION_REPLACEMENT_STUCK_AFTER_SECS", "120")]);

        let collector_config = collection_collector_config(&config);

        assert_eq!(
            collector_config.worker_id,
            format!("collection-collector-{}", std::process::id())
        );
        assert_eq!(
            collector_config.replacement_stuck_after,
            std::time::Duration::from_secs(120)
        );
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
    async fn runtime_signer_bootstraps_fake_mode() {
        let config = test_config(&[("SIGNER_MODE", "fake")]);
        let signer = runtime_signer(&config).expect("fake signer should bootstrap");

        assert!(matches!(&signer, RuntimeSigner::Fake { .. }));
        ensure_runtime_signer_health(&signer)
            .await
            .expect("fake signer health check");

        let wallet = HdWallet::new(signer.clone());
        let request = DeriveAddressRequest::new("pay3-master", 1, DerivationSegment::ZERO).unwrap();
        let derived = wallet.derive_child_address(request.clone()).await.unwrap();
        let derived_again = wallet.derive_child_address(request).await.unwrap();

        assert_eq!(derived.signer_key_ref, "pay3-master");
        assert_eq!(derived.derivation_path, "m/44'/60'/0'/0/0");
        assert_eq!(derived.address, derived_again.address);
    }

    #[tokio::test]
    async fn runtime_signer_bootstraps_remote_modes_and_reuses_same_client() {
        let state = RemoteTestState {
            expected_key_ref: "pay3-master".to_string(),
            expected_path: "m/44'/60'/7'/8/9".to_string(),
            address: EvmAddress::from_bytes([0x11; 20]),
            tx_hash: TxHash::from_bytes([0x22; 32]),
        };
        let (endpoint, handle) = spawn_remote_signer_server(state.clone()).await;

        for mode in ["external", "kms", "hsm"] {
            let config = test_config_owned(vec![
                ("SIGNER_MODE", mode.to_string()),
                ("SIGNER_REMOTE_ENDPOINT", endpoint.clone()),
                ("SIGNER_REMOTE_REQUEST_TIMEOUT_SECS", "2".to_string()),
            ]);
            let signer = runtime_signer(&config).expect("remote signer should bootstrap");

            assert!(matches!(&signer, RuntimeSigner::Remote(_)));
            ensure_runtime_signer_health(&signer)
                .await
                .expect("remote signer health check");

            let wallet = HdWallet::new(signer.clone());
            let request = DeriveAddressRequest::new(
                state.expected_key_ref.clone(),
                1,
                DerivationSegment::new(7, 8, 9).unwrap(),
            )
            .unwrap();
            let derived = wallet.derive_child_address(request).await.unwrap();
            assert_eq!(derived.address, state.address);
            assert_eq!(derived.derivation_path, state.expected_path);

            let unsigned = UnsignedTx::new(
                "request-1",
                31337,
                9,
                EvmAddress::from_bytes([0x33; 20]),
                RawAmount::from(1_000u64),
                80_000,
                RawAmount::from(50_000_000_000u64),
                RawAmount::from(2_000_000_000u64),
                vec![0xaa, 0xbb, 0xcc],
            )
            .unwrap();
            let signed = signer
                .sign_transaction(
                    &state.expected_key_ref,
                    &state.expected_path,
                    unsigned.clone(),
                )
                .await
                .unwrap();

            assert_eq!(signed.request_id, unsigned.request_id);
            assert_eq!(signed.from, state.address);
            assert_eq!(signed.to, unsigned.to);
            assert_eq!(signed.tx_hash, state.tx_hash);
            assert_eq!(signed.raw_tx, vec![0xde, 0xad, 0xbe, 0xef]);
        }

        handle.abort();
    }

    #[test]
    fn runtime_signer_rejects_local_mode() {
        let config = test_config_owned(vec![
            ("SIGNER_MODE", "local".to_string()),
            (
                "SIGNER_REMOTE_ENDPOINT",
                "http://localhost:8081".to_string(),
            ),
        ]);

        let error = runtime_signer(&config).expect_err("local mode should remain unsupported");

        assert!(matches!(
            error,
            RuntimeError::UnsupportedSignerMode {
                mode: SignerMode::Local
            }
        ));
    }

    #[derive(Clone)]
    struct RemoteTestState {
        expected_key_ref: String,
        expected_path: String,
        address: EvmAddress,
        tx_hash: TxHash,
    }

    #[derive(Deserialize)]
    struct RemoteDeriveRequest {
        key_ref: String,
        path: String,
    }

    #[derive(Deserialize)]
    struct RemoteSignRequest {
        key_ref: String,
        path: String,
        transaction: UnsignedTx,
    }

    async fn spawn_remote_signer_server(
        state: RemoteTestState,
    ) -> (String, tokio::task::JoinHandle<()>) {
        async fn healthz() -> Json<serde_json::Value> {
            Json(json!({ "status": "ok" }))
        }

        async fn derive_address(
            State(state): State<RemoteTestState>,
            Json(body): Json<RemoteDeriveRequest>,
        ) -> Json<EvmAddress> {
            assert_eq!(body.key_ref, state.expected_key_ref);
            assert_eq!(body.path, state.expected_path);
            Json(state.address)
        }

        async fn sign_transaction(
            State(state): State<RemoteTestState>,
            Json(body): Json<RemoteSignRequest>,
        ) -> Json<SignedTx> {
            assert_eq!(body.key_ref, state.expected_key_ref);
            assert_eq!(body.path, state.expected_path);
            assert_eq!(body.transaction.request_id, "request-1");
            assert_eq!(body.transaction.chain_id, 31337);
            assert_eq!(body.transaction.nonce, 9);
            Json(SignedTx {
                request_id: body.transaction.request_id,
                chain_id: body.transaction.chain_id,
                nonce: body.transaction.nonce,
                from: state.address,
                to: body.transaction.to,
                tx_hash: state.tx_hash,
                raw_tx: vec![0xde, 0xad, 0xbe, 0xef],
            })
        }

        let app = Router::new()
            .route("/healthz", get(healthz))
            .route("/v1/addresses/derive", post(derive_address))
            .route("/v1/transactions/sign", post(sign_transaction))
            .with_state(state);

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let addr = listener.local_addr().expect("listener addr");
        let handle = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve remote signer");
        });

        (format!("http://{addr}"), handle)
    }

    fn test_config_owned(overrides: Vec<(&'static str, String)>) -> AppConfig {
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

        for (key, value) in overrides {
            if let Some((_, existing)) = pairs
                .iter_mut()
                .find(|(existing_key, _)| *existing_key == key)
            {
                *existing = value;
            } else {
                pairs.push((key, value));
            }
        }

        AppConfig::from_pairs(pairs).expect("test config should parse")
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
