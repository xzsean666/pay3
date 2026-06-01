use std::{fs, path::Path, str::FromStr, sync::Arc};

use async_trait::async_trait;
use axum::Router;
use sqlx::{PgPool, Row};
use thiserror::Error;
use tokio::task::JoinHandle;

use crate::{
    api::{self, OrderResponseConfig},
    auth::JwtVerifier,
    chain::{
        ChainError, ChainHeaderReader, RpcRangeSource, TransferLogCapacityLimits, TransferLogRange,
        TransferLogSource,
    },
    config::{
        AppConfig, ConfigError, JwtAlgorithm, JwtKeySource, RuntimeRole, SignerMode,
        WorkerEnableConfig,
    },
    db::{
        migrations::{
            MIGRATOR, MigrationBootstrapError, RuntimeSeedConfig, run_schema_migrations,
            seed_runtime_config,
        },
        repositories::{
            ExpiredOrderRepository, PaymentRepository, PgAuditRepository, PgCollectionRepository,
            PgOrderRepository, PgOutboundRepository, PgPaymentRepository,
            PgVerifiedPaymentRecorder, RepositoryError,
        },
    },
    domain::{CollectionFees, EvmAddress, RawAmount},
    health::{
        DependencyCheck, DependencyName, MetricsRecorder, RuntimeDependencyRegistry,
        StaticDependencyRegistry,
    },
    services::{
        collections::{
            CollectionService, CollectionServiceConfig, CreateCollectionInput,
            NativeBalanceGasChecker,
        },
        orders::{OrderService, OrderServiceConfig, SystemClock},
        payment_windows::{RepositoryPaymentWindowLookup, WatchSetPaymentWindowLookup},
        payments::{PaymentMatcher, PaymentMatchingConfig},
        verify::{ManualOrderVerifyService, ManualVerifyConfig},
    },
    signer::{
        DeterministicFakeSigner, LocalMnemonicSigner, RemoteHttpSigner, SignedTx, SignerError,
        SignerProvider, UnsignedTx,
    },
    transfer_log_store::{
        LogSourceKind, RedbTransferLogIngestor, ScanTargetMode, StreamId, TransferLogIngestor,
        TransferLogReader, TransferLogStoreError, TransferLogStreamConfig,
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
const TRANSFER_LOG_MAX_LOGS_PER_PAGE: usize = 1_000;
const TRANSFER_LOG_MAX_UNIQUE_TO_ADDRESSES_PER_BATCH: usize = 1_000;
const TRANSFER_LOG_MAX_DB_FALLBACK_ADDRESSES: usize = 1_000;
const TRANSFER_LOG_CAPACITY_PROBE_BLOCKS: u64 = 100;
const TRANSFER_LOG_RPC_MAX_RETRIES: u32 = 3;
const TRANSFER_LOG_RETENTION_POLL_INTERVAL_MS: u64 = 60_000;
const PAYMENT_SCANNER_POLL_INTERVAL_MS: u64 = 5_000;
const PAYMENT_SCANNER_LEASE_SECONDS: i64 = 30;
const COLLECTION_ENQUEUER_POLL_INTERVAL_MS: u64 = 5_000;
const COLLECTION_ENQUEUER_BATCH_LIMIT: u32 = 100;
const COLLECTION_COLLECTOR_POLL_INTERVAL_MS: u64 = 5_000;
const RUNTIME_READINESS_POLL_INTERVAL_MS: u64 = 30_000;
const ORDER_EXPIRY_POLL_INTERVAL_MS: u64 = 5_000;
const ORDER_EXPIRY_BATCH_LIMIT: u32 = 1_000;

#[derive(Clone, Debug)]
enum RuntimeSigner {
    Fake {
        deriver: DeterministicFakeDeriver,
        signer: DeterministicFakeSigner,
    },
    Local(LocalMnemonicSigner),
    Remote(RemoteHttpSigner),
}

type RuntimeCollectionService<S> = CollectionService<
    PgOrderRepository,
    PgCollectionRepository,
    PgOutboundRepository,
    PgAuditRepository,
    S,
    RpcRangeSource,
    NativeBalanceGasChecker<RpcRangeSource>,
>;
type RuntimePaymentWindowLookup =
    WatchSetPaymentWindowLookup<RepositoryPaymentWindowLookup<PgOrderRepository>>;
type RuntimePaymentMatcher = PaymentMatcher<
    RedbTransferLogIngestor<RpcRangeSource>,
    RuntimePaymentWindowLookup,
    RpcRangeSource,
>;
type RuntimePaymentScannerWorker = PaymentScannerWorker<
    PgPaymentRepository,
    RuntimePaymentMatcher,
    RedbTransferLogIngestor<RpcRangeSource>,
    RpcRangeSource,
    SystemClock,
>;

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
            Self::Local(signer) => AddressDeriver::derive_address(signer, key_ref, path).await,
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
            Self::Local(signer) => SignerProvider::derive_address(signer, key_ref, path).await,
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
            Self::Local(signer) => {
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
            Self::Local(signer) => SignerProvider::health_check(signer).await,
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
    Chain(Box<ChainError>),

    #[error(transparent)]
    TransferLogStore(Box<TransferLogStoreError>),

    #[error(transparent)]
    TransferLogIngestorLoop(Box<TransferLogIngestorLoopError>),

    #[error(transparent)]
    PaymentScanner(Box<PaymentScannerError>),

    #[error(transparent)]
    CollectionCollector(Box<CollectionCollectorError>),

    #[error(transparent)]
    Repository(Box<RepositoryError>),

    #[error(transparent)]
    OrderService(Box<crate::services::orders::OrderServiceError>),

    #[error(transparent)]
    CollectionService(Box<crate::services::collections::CollectionServiceError>),

    #[error(transparent)]
    Wallet(Box<WalletError>),

    #[error(transparent)]
    Signer(Box<SignerError>),
}

impl From<ChainError> for RuntimeError {
    fn from(error: ChainError) -> Self {
        Self::Chain(Box::new(error))
    }
}

impl From<TransferLogStoreError> for RuntimeError {
    fn from(error: TransferLogStoreError) -> Self {
        Self::TransferLogStore(Box::new(error))
    }
}

impl From<TransferLogIngestorLoopError> for RuntimeError {
    fn from(error: TransferLogIngestorLoopError) -> Self {
        Self::TransferLogIngestorLoop(Box::new(error))
    }
}

impl From<PaymentScannerError> for RuntimeError {
    fn from(error: PaymentScannerError) -> Self {
        Self::PaymentScanner(Box::new(error))
    }
}

impl From<CollectionCollectorError> for RuntimeError {
    fn from(error: CollectionCollectorError) -> Self {
        Self::CollectionCollector(Box::new(error))
    }
}

impl From<RepositoryError> for RuntimeError {
    fn from(error: RepositoryError) -> Self {
        Self::Repository(Box::new(error))
    }
}

impl From<crate::services::orders::OrderServiceError> for RuntimeError {
    fn from(error: crate::services::orders::OrderServiceError) -> Self {
        Self::OrderService(Box::new(error))
    }
}

impl From<crate::services::collections::CollectionServiceError> for RuntimeError {
    fn from(error: crate::services::collections::CollectionServiceError) -> Self {
        Self::CollectionService(Box::new(error))
    }
}

impl From<WalletError> for RuntimeError {
    fn from(error: WalletError) -> Self {
        Self::Wallet(Box::new(error))
    }
}

impl From<SignerError> for RuntimeError {
    fn from(error: SignerError) -> Self {
        Self::Signer(Box::new(error))
    }
}

pub async fn build_api_router(config: AppConfig) -> Result<Router, RuntimeError> {
    let mut config = config;
    config.runtime.role = RuntimeRole::Api;
    config.runtime.workers = WorkerEnableConfig {
        transfer_log_ingestor: false,
        transfer_log_retention: false,
        runtime_readiness: false,
        order_expiry: false,
        payment_scanner: false,
        collection_enqueuer: false,
        collection_collector: false,
    };
    Ok(build_api_runtime(config).await?.into_router())
}

pub struct ApiRuntime {
    router: Router,
    background_tasks: BackgroundTasks,
}

impl ApiRuntime {
    pub fn router(&self) -> Router {
        self.router.clone()
    }

    pub fn into_router(self) -> Router {
        self.router
    }

    pub async fn shutdown(self) {
        self.background_tasks.shutdown().await;
    }
}

pub struct BackgroundTasks {
    handles: Vec<JoinHandle<()>>,
}

impl BackgroundTasks {
    fn new() -> Self {
        Self {
            handles: Vec::new(),
        }
    }

    fn push(&mut self, handle: JoinHandle<()>) {
        self.handles.push(handle);
    }

    async fn shutdown(mut self) {
        for handle in &self.handles {
            handle.abort();
        }
        while let Some(handle) = self.handles.pop() {
            let _ = handle.await;
        }
    }
}

impl Drop for BackgroundTasks {
    fn drop(&mut self) {
        for handle in &self.handles {
            handle.abort();
        }
    }
}

pub async fn build_api_runtime(config: AppConfig) -> Result<ApiRuntime, RuntimeError> {
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

    let log_store = RedbTransferLogIngestor::open(rpc_source.clone(), &config.kvdb.path)?;
    let stream_config = transfer_log_stream_config(&config);
    let stream = stream_config.stream_id();
    log_store.ensure_stream(stream_config.clone()).await?;
    let retention_repository = PgOrderRepository::new(pool.clone());
    let kvdb_retention_repository = retention_repository.clone();
    let payment_repository = PgPaymentRepository::new(pool.clone());
    let static_dependencies = StaticDependencyRegistry::all_healthy();
    refresh_migration_dependency_status(&static_dependencies, &pool).await;
    let dependency_registry =
        RuntimeDependencyRegistry::new(static_dependencies.clone(), metrics.clone());
    let kvdb_readiness = KvdbReadinessResources {
        dependencies: static_dependencies.clone(),
        metrics: metrics.clone(),
        retention_repository: kvdb_retention_repository,
        payment_repository,
        log_store: log_store.clone(),
        stream,
        reorg_lookback_blocks: stream_config.reorg_lookback_blocks,
        manual_rebuild_floor_block: config.kvdb.manual_rebuild_floor_block,
    };
    update_kvdb_dependency_status(&kvdb_readiness).await?;
    let readiness = RuntimeReadinessResources {
        kvdb: kvdb_readiness,
        pool: pool.clone(),
        rpc_source: rpc_source.clone(),
        signer: signer.clone(),
    };
    refresh_runtime_dependency_status(&readiness).await;
    let mut background_tasks = BackgroundTasks::new();
    let workers = &config.runtime.workers;
    let runtime_workers_enabled = config.runtime.workers_enabled();
    if runtime_workers_enabled && workers.transfer_log_ingestor {
        background_tasks.push(spawn_transfer_log_ingestor_loop_with_metrics(
            log_store.clone(),
            TransferLogIngestorLoopConfig::new(
                stream,
                std::time::Duration::from_millis(stream_config.poll_interval_ms),
                config.chain.min_confirmations.saturating_mul(2),
            ),
            metrics.clone(),
        )?);
    }
    if runtime_workers_enabled && workers.transfer_log_retention {
        background_tasks.push(tokio::spawn(transfer_log_retention_loop(
            retention_repository,
            log_store.clone(),
            stream,
            stream_config.start_block,
            stream_config.reorg_lookback_blocks,
            config.kvdb.manual_rebuild_floor_block,
            std::time::Duration::from_millis(TRANSFER_LOG_RETENTION_POLL_INTERVAL_MS),
        )));
    }
    if config.runtime.api_enabled() && workers.runtime_readiness {
        background_tasks.push(tokio::spawn(runtime_readiness_loop(
            readiness,
            std::time::Duration::from_millis(RUNTIME_READINESS_POLL_INTERVAL_MS),
        )));
    }
    if runtime_workers_enabled && workers.order_expiry {
        background_tasks.push(tokio::spawn(order_expiry_loop(
            PgOrderRepository::new(pool.clone()),
            std::time::Duration::from_millis(ORDER_EXPIRY_POLL_INTERVAL_MS),
            ORDER_EXPIRY_BATCH_LIMIT,
        )));
    }
    if runtime_workers_enabled && workers.payment_scanner {
        background_tasks.push(spawn_payment_scanner_loop_with_metrics(
            payment_scanner_worker(&config, pool.clone(), log_store.clone(), rpc_source.clone()),
            std::time::Duration::from_millis(PAYMENT_SCANNER_POLL_INTERVAL_MS),
            metrics.clone(),
        )?);
    }

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
    if runtime_workers_enabled && workers.collection_enqueuer {
        background_tasks.push(tokio::spawn(auto_collection_enqueue_loop(
            pool.clone(),
            collection_service(&config, pool.clone(), rpc_source.clone(), signer.clone())?,
            config.chain.chain_id,
            config.chain.token_address,
            config.chain.problem_funds_address,
            std::time::Duration::from_millis(COLLECTION_ENQUEUER_POLL_INTERVAL_MS),
            COLLECTION_ENQUEUER_BATCH_LIMIT,
        )));
    }
    if runtime_workers_enabled && workers.collection_collector {
        background_tasks.push(spawn_collection_collector_loop_with_metrics(
            CollectionCollectorWorker::new(
                collection_service(&config, pool.clone(), rpc_source.clone(), signer.clone())?,
                PgOutboundRepository::new(pool.clone()),
                rpc_source.clone(),
                collection_collector_config(&config),
            ),
            std::time::Duration::from_millis(COLLECTION_COLLECTOR_POLL_INTERVAL_MS),
            metrics.clone(),
        )?);
    }
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

    let router = api::router_with_runtime_services_and_metrics(
        dependency_registry,
        metrics,
        auth,
        orders,
        order_verify,
        collections,
        OrderResponseConfig::from_config(&config),
    );

    Ok(ApiRuntime {
        router,
        background_tasks,
    })
}

fn runtime_seed_config(config: &AppConfig) -> RuntimeSeedConfig {
    RuntimeSeedConfig {
        signer_key_ref: config.signer.key_ref.clone(),
        chain_id: config.chain.chain_id,
        token_address: config.chain.token_address,
        treasury_address: config.chain.treasury_address,
        problem_funds_address: config.chain.problem_funds_address,
        start_block: config.chain.start_block,
    }
}

fn jwt_verifier(config: &AppConfig) -> Result<JwtVerifier, RuntimeError> {
    let issuer = config.jwt.issuer.clone();
    let audience = config.jwt.audience.clone();

    Ok(match &config.jwt.key_source {
        JwtKeySource::Hs256 { secret, key_id } => {
            let key_id = key_id.clone().unwrap_or_else(|| "default".to_string());
            JwtVerifier::new_hs256(issuer, audience, [(key_id, secret.clone())])?
        }
        JwtKeySource::LocalJwks { json } => JwtVerifier::from_jwks_json(issuer, audience, json)?,
        JwtKeySource::PublicKeyPem {
            algorithm,
            key_id,
            public_key_pem,
        } => JwtVerifier::new_asymmetric_pem(
            issuer,
            audience,
            key_id.clone(),
            jwt_algorithm(*algorithm),
            public_key_pem,
        )?,
        JwtKeySource::RemoteJwks { .. } => {
            return Err(RuntimeError::Config(ConfigError::Validation {
                errors: vec![
                    "JWT_JWKS_URL is reserved for remote JWKS fetch; set JWT_JWKS_JSON for now"
                        .to_string(),
                ],
            }));
        }
    })
}

fn jwt_algorithm(algorithm: JwtAlgorithm) -> jsonwebtoken::Algorithm {
    match algorithm {
        JwtAlgorithm::Hs256 => jsonwebtoken::Algorithm::HS256,
        JwtAlgorithm::Rs256 => jsonwebtoken::Algorithm::RS256,
        JwtAlgorithm::EdDsa => jsonwebtoken::Algorithm::EdDSA,
    }
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
                        "external/kms/hsm signer modes require SIGNER_REMOTE_ENDPOINT".to_string(),
                    ],
                })
            })?;

            Ok(RuntimeSigner::Remote(RemoteHttpSigner::with_bearer_token(
                endpoint,
                config.signer.remote_request_timeout,
                config.signer.remote_bearer_token.clone(),
            )?))
        }
        SignerMode::Local => {
            if !config.signer.allow_local_signer {
                return Err(RuntimeError::Config(ConfigError::Validation {
                    errors: vec![
                        "SIGNER_MODE=local requires explicit ALLOW_LOCAL_SIGNER=true".to_string(),
                    ],
                }));
            }
            let mnemonic = config.signer.mnemonic.as_deref().ok_or_else(|| {
                RuntimeError::Config(ConfigError::Validation {
                    errors: vec!["SIGNER_MODE=local requires SIGNER_MNEMONIC".to_string()],
                })
            })?;
            Ok(RuntimeSigner::Local(LocalMnemonicSigner::new(
                config.signer.key_ref.clone(),
                mnemonic,
            )?))
        }
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
) -> Result<RuntimeCollectionService<S>, RuntimeError>
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
        rpc_source.clone(),
        NativeBalanceGasChecker::new(rpc_source),
    )?)
}

fn collection_service_config(config: &AppConfig) -> CollectionServiceConfig {
    CollectionServiceConfig::new(
        config.chain.chain_id,
        config.chain.token_address,
        config.chain.treasury_address,
        config.chain.problem_funds_address,
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
        .with_min_confirmations(config.chain.min_confirmations)
}

async fn auto_collection_enqueue_loop(
    pool: PgPool,
    collection_service: RuntimeCollectionService<RuntimeSigner>,
    chain_id: u64,
    token_address: EvmAddress,
    problem_funds_address: EvmAddress,
    poll_interval: std::time::Duration,
    batch_limit: u32,
) {
    let mut interval = tokio::time::interval(poll_interval);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        interval.tick().await;
        match enqueue_orders_for_collection(
            &pool,
            &collection_service,
            chain_id,
            token_address,
            problem_funds_address,
            batch_limit,
        )
        .await
        {
            Ok(0) => {
                tracing::debug!("auto collection enqueuer idle");
            }
            Ok(enqueued) => {
                tracing::info!(enqueued, "auto collection enqueuer queued collections");
            }
            Err(error) => {
                tracing::warn!(error = %error, "auto collection enqueuer tick failed");
            }
        }
    }
}

async fn enqueue_orders_for_collection(
    pool: &PgPool,
    collection_service: &RuntimeCollectionService<RuntimeSigner>,
    chain_id: u64,
    token_address: EvmAddress,
    problem_funds_address: EvmAddress,
    batch_limit: u32,
) -> Result<usize, RuntimeError> {
    if batch_limit == 0 {
        return Ok(0);
    }

    let mut enqueued = 0;
    enqueued += enqueue_paid_orders_for_collection(
        pool,
        collection_service,
        chain_id,
        token_address,
        batch_limit,
    )
    .await?;
    enqueued += enqueue_problem_funds_orders_for_collection(
        pool,
        collection_service,
        chain_id,
        token_address,
        problem_funds_address,
        batch_limit,
    )
    .await?;

    Ok(enqueued)
}

async fn enqueue_paid_orders_for_collection(
    pool: &PgPool,
    collection_service: &RuntimeCollectionService<RuntimeSigner>,
    chain_id: u64,
    token_address: EvmAddress,
    batch_limit: u32,
) -> Result<usize, RuntimeError> {
    let orders =
        paid_orders_without_collections(pool, chain_id, token_address, batch_limit).await?;
    let mut enqueued = 0;
    for order in orders {
        match collection_service
            .create_collection(CreateCollectionInput {
                order_id: order.order_id,
                amount: crate::services::collections::CollectionAmount::Exact(
                    order.paid_amount_raw,
                ),
                idempotency_key: format!("auto-collect-{}", order.order_id),
                audit: Default::default(),
            })
            .await
        {
            Ok(result) => {
                enqueued += 1;
                tracing::info!(
                    order_id = %order.order_id,
                    collection_id = %result.collection.id,
                    outcome = ?result.outcome,
                    "auto collection enqueued paid order"
                );
            }
            Err(error) => {
                tracing::warn!(
                    order_id = %order.order_id,
                    error = %error,
                    "auto collection enqueue skipped paid order"
                );
            }
        }
    }

    Ok(enqueued)
}

#[derive(Clone, Copy, Debug)]
struct PaidCollectionCandidate {
    order_id: uuid::Uuid,
    paid_amount_raw: RawAmount,
}

async fn paid_orders_without_collections(
    pool: &PgPool,
    chain_id: u64,
    token_address: EvmAddress,
    limit: u32,
) -> Result<Vec<PaidCollectionCandidate>, RuntimeError> {
    let rows = sqlx::query(
        r#"
        SELECT o.id, o.paid_amount_raw::text AS paid_amount_raw
        FROM orders o
        WHERE o.chain_id = $1
          AND o.token_address = $2
          AND o.status = 'paid'
          AND o.paid_amount_raw > 0
          AND NOT EXISTS (
              SELECT 1
              FROM collections c
              WHERE c.order_id = o.id
          )
        ORDER BY o.updated_at, o.id
        LIMIT $3
        "#,
    )
    .bind(i64::try_from(chain_id).map_err(|error| {
        RuntimeError::Config(ConfigError::Validation {
            errors: vec![format!("CHAIN_ID does not fit PostgreSQL bigint: {error}")],
        })
    })?)
    .bind(token_address.to_lower_hex())
    .bind(i64::from(limit))
    .fetch_all(pool)
    .await?;

    rows.into_iter()
        .map(|row| {
            let order_id = row.try_get("id")?;
            let paid_amount_raw = raw_amount_from_db_text(row.try_get("paid_amount_raw")?)?;
            Ok(PaidCollectionCandidate {
                order_id,
                paid_amount_raw,
            })
        })
        .collect()
}

async fn enqueue_problem_funds_orders_for_collection(
    pool: &PgPool,
    collection_service: &RuntimeCollectionService<RuntimeSigner>,
    chain_id: u64,
    token_address: EvmAddress,
    problem_funds_address: EvmAddress,
    batch_limit: u32,
) -> Result<usize, RuntimeError> {
    let candidates = problem_funds_orders_ready_for_collection(
        pool,
        chain_id,
        token_address,
        problem_funds_address,
        batch_limit,
    )
    .await?;
    let mut enqueued = 0;

    for candidate in candidates {
        match collection_service
            .create_problem_funds_collection(CreateCollectionInput::max(
                candidate.order_id,
                format!(
                    "auto-problem-funds-{}-{}",
                    candidate.order_id, candidate.problem_payment_total_raw
                ),
            ))
            .await
        {
            Ok(result) => {
                enqueued += 1;
                tracing::info!(
                    order_id = %candidate.order_id,
                    collection_id = %result.collection.id,
                    problem_payment_total_raw = %candidate.problem_payment_total_raw,
                    outcome = ?result.outcome,
                    "auto collection enqueued problem funds order"
                );
            }
            Err(error) => {
                tracing::warn!(
                    order_id = %candidate.order_id,
                    problem_payment_total_raw = %candidate.problem_payment_total_raw,
                    error = %error,
                    "auto collection enqueue skipped problem funds order"
                );
            }
        }
    }

    Ok(enqueued)
}

#[derive(Clone, Copy, Debug)]
struct ProblemFundsCollectionCandidate {
    order_id: uuid::Uuid,
    problem_payment_total_raw: RawAmount,
}

async fn problem_funds_orders_ready_for_collection(
    pool: &PgPool,
    chain_id: u64,
    token_address: EvmAddress,
    problem_funds_address: EvmAddress,
    limit: u32,
) -> Result<Vec<ProblemFundsCollectionCandidate>, RuntimeError> {
    let rows = sqlx::query(
        r#"
        WITH problem_payment_totals AS (
            SELECT
                o.id AS order_id,
                SUM(p.amount_raw) AS problem_payment_total_raw
            FROM orders o
            JOIN payments p ON p.order_id = o.id
            WHERE o.chain_id = $1
              AND o.token_address = $2
              AND p.chain_status = 'confirmed'
              AND (
                  o.status = 'expired'
                  OR (
                      o.status = 'paid'
                      AND p.match_status IN ('late', 'outside_window')
                  )
              )
            GROUP BY o.id
        ),
        problem_collection_totals AS (
            SELECT
                c.order_id,
                COALESCE(
                    SUM(c.amount_raw) FILTER (
                        WHERE c.status = 'confirmed'
                          AND c.amount_raw IS NOT NULL
                    ),
                    0
                ) AS confirmed_problem_collection_total_raw,
                BOOL_OR(c.status IN ('queued', 'transferring', 'confirming', 'replacing')) AS has_active_problem_collection
            FROM collections c
            WHERE c.chain_id = $1
              AND c.token_address = $2
              AND c.to_address = $3
            GROUP BY c.order_id
        )
        SELECT
            ppt.order_id,
            ppt.problem_payment_total_raw::text AS problem_payment_total_raw
        FROM problem_payment_totals ppt
        LEFT JOIN problem_collection_totals pct ON pct.order_id = ppt.order_id
        WHERE ppt.problem_payment_total_raw > COALESCE(pct.confirmed_problem_collection_total_raw, 0)
          AND COALESCE(pct.has_active_problem_collection, false) = false
          AND NOT EXISTS (
              SELECT 1
              FROM collections active
              WHERE active.order_id = ppt.order_id
                AND active.status IN ('queued', 'transferring', 'confirming', 'replacing')
          )
        ORDER BY ppt.order_id
        LIMIT $4
        "#,
    )
    .bind(i64::try_from(chain_id).map_err(|error| {
        RuntimeError::Config(ConfigError::Validation {
            errors: vec![format!("CHAIN_ID does not fit PostgreSQL bigint: {error}")],
        })
    })?)
    .bind(token_address.to_lower_hex())
    .bind(problem_funds_address.to_lower_hex())
    .bind(i64::from(limit))
    .fetch_all(pool)
    .await?;

    rows.into_iter()
        .map(|row| {
            let order_id = row.try_get("order_id")?;
            let problem_payment_total_raw =
                raw_amount_from_db_text(row.try_get("problem_payment_total_raw")?)?;
            Ok(ProblemFundsCollectionCandidate {
                order_id,
                problem_payment_total_raw,
            })
        })
        .collect()
}

fn raw_amount_from_db_text(value: String) -> Result<RawAmount, RuntimeError> {
    RawAmount::from_str(&value).map_err(|error| {
        RuntimeError::Repository(Box::new(RepositoryError::invalid_db_value(
            "numeric(78,0)",
            value,
            format!("invalid raw amount: {error}"),
        )))
    })
}

fn payment_scanner_worker(
    config: &AppConfig,
    pool: PgPool,
    log_store: RedbTransferLogIngestor<RpcRangeSource>,
    rpc_source: RpcRangeSource,
) -> RuntimePaymentScannerWorker {
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

#[derive(Clone)]
struct KvdbReadinessResources {
    dependencies: StaticDependencyRegistry,
    metrics: MetricsRecorder,
    retention_repository: PgOrderRepository,
    payment_repository: PgPaymentRepository,
    log_store: RedbTransferLogIngestor<RpcRangeSource>,
    stream: StreamId,
    reorg_lookback_blocks: u64,
    manual_rebuild_floor_block: Option<u64>,
}

#[derive(Clone)]
struct RuntimeReadinessResources<S> {
    kvdb: KvdbReadinessResources,
    pool: PgPool,
    rpc_source: RpcRangeSource,
    signer: S,
}

async fn update_kvdb_dependency_status(
    resources: &KvdbReadinessResources,
) -> Result<(), RuntimeError> {
    let stream = resources.stream;
    let scan_cursor_state = resources
        .payment_repository
        .scan_cursor_state(stream.chain_id, stream.token_address)
        .await?;
    let retention_floor_block = resources
        .retention_repository
        .retention_floor_block(
            stream.chain_id,
            stream.token_address,
            resources.reorg_lookback_blocks,
            resources.manual_rebuild_floor_block,
        )
        .await?;
    let log_cursor = resources.log_store.cursor(stream).await?;
    resources
        .metrics
        .record_kvdb_state(log_cursor.last_completed_block, retention_floor_block);

    let dependency = kvdb_dependency_check(
        stream,
        scan_cursor_state.as_ref(),
        &log_cursor,
        retention_floor_block,
    );
    resources.dependencies.set_status(dependency);
    Ok(())
}

async fn refresh_runtime_dependency_status<S>(resources: &RuntimeReadinessResources<S>)
where
    S: SignerProvider,
{
    refresh_db_dependency_status(&resources.kvdb.dependencies, &resources.pool).await;
    refresh_migration_dependency_status(&resources.kvdb.dependencies, &resources.pool).await;
    refresh_rpc_dependency_status(
        &resources.kvdb.dependencies,
        &resources.rpc_source,
        resources.kvdb.stream,
    )
    .await;
    refresh_signer_dependency_status(&resources.kvdb.dependencies, &resources.signer).await;

    if let Err(error) = update_kvdb_dependency_status(&resources.kvdb).await {
        tracing::warn!(
            chain_id = resources.kvdb.stream.chain_id,
            token_address = %resources.kvdb.stream.token_address,
            error = %error,
            "kvdb readiness refresh failed"
        );
        resources
            .kvdb
            .dependencies
            .set_status(DependencyCheck::failed(
                DependencyName::Kvdb,
                error.to_string(),
            ));
    }
}

async fn refresh_migration_dependency_status(
    dependencies: &StaticDependencyRegistry,
    pool: &PgPool,
) {
    let expected = expected_migration_version();
    match sqlx::query_scalar::<_, Option<i64>>("SELECT MAX(version) FROM _sqlx_migrations")
        .fetch_one(pool)
        .await
    {
        Ok(Some(version)) if version >= expected => {
            dependencies.set_status(DependencyCheck::healthy(DependencyName::Migration));
        }
        Ok(Some(version)) => dependencies.set_status(DependencyCheck::failed(
            DependencyName::Migration,
            format!("database migration version {version} is behind expected {expected}"),
        )),
        Ok(None) => dependencies.set_status(DependencyCheck::failed(
            DependencyName::Migration,
            format!("database has no applied migrations; expected {expected}"),
        )),
        Err(error) => dependencies.set_status(DependencyCheck::failed(
            DependencyName::Migration,
            error.to_string(),
        )),
    }
}

fn expected_migration_version() -> i64 {
    MIGRATOR
        .iter()
        .map(|migration| migration.version)
        .max()
        .unwrap_or_default()
}

async fn refresh_db_dependency_status(dependencies: &StaticDependencyRegistry, pool: &PgPool) {
    match sqlx::query_scalar::<_, i64>("SELECT 1::BIGINT")
        .fetch_one(pool)
        .await
    {
        Ok(1) => dependencies.set_status(DependencyCheck::healthy(DependencyName::Db)),
        Ok(value) => dependencies.set_status(DependencyCheck::failed(
            DependencyName::Db,
            format!("database health query returned {value}"),
        )),
        Err(error) => dependencies.set_status(DependencyCheck::failed(
            DependencyName::Db,
            error.to_string(),
        )),
    }
}

async fn refresh_rpc_dependency_status(
    dependencies: &StaticDependencyRegistry,
    rpc_source: &RpcRangeSource,
    stream: StreamId,
) {
    let readiness = match rpc_source.readiness_probe().await {
        Ok(readiness) => readiness,
        Err(error) => {
            dependencies.set_status(DependencyCheck::failed(
                DependencyName::RpcChainId,
                error.to_string(),
            ));
            return;
        }
    };

    let to_block = readiness.latest_head.number;
    let from_block = to_block.saturating_sub(TRANSFER_LOG_CAPACITY_PROBE_BLOCKS.saturating_sub(1));
    let range = TransferLogRange::new(stream.chain_id, stream.token_address, from_block, to_block);
    let limits = TransferLogCapacityLimits {
        max_logs: TRANSFER_LOG_MAX_LOGS_PER_PAGE,
        max_logs_per_block: TRANSFER_LOG_MAX_LOGS_PER_PAGE,
    };
    match rpc_source.ensure_capacity(range, limits).await {
        Ok(_) => dependencies.set_status(DependencyCheck::healthy(DependencyName::RpcChainId)),
        Err(error) => dependencies.set_status(DependencyCheck::failed(
            DependencyName::RpcChainId,
            error.to_string(),
        )),
    }
}

async fn refresh_signer_dependency_status<S>(dependencies: &StaticDependencyRegistry, signer: &S)
where
    S: SignerProvider,
{
    match signer.health_check().await {
        Ok(()) => dependencies.set_status(DependencyCheck::healthy(DependencyName::Signer)),
        Err(error) => dependencies.set_status(DependencyCheck::failed(
            DependencyName::Signer,
            error.to_string(),
        )),
    }
}

async fn runtime_readiness_loop<S>(
    resources: RuntimeReadinessResources<S>,
    poll_interval: std::time::Duration,
) where
    S: SignerProvider + 'static,
{
    let mut interval = tokio::time::interval(poll_interval);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        interval.tick().await;

        refresh_runtime_dependency_status(&resources).await;
    }
}

async fn order_expiry_loop(
    repository: PgOrderRepository,
    poll_interval: std::time::Duration,
    batch_limit: u32,
) {
    let mut interval = tokio::time::interval(poll_interval);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        interval.tick().await;

        match repository.recompute_expired_open_orders(batch_limit).await {
            Ok(0) => {}
            Ok(count) => {
                tracing::info!(count, "expired open orders recomputed");
            }
            Err(error) => {
                tracing::warn!(error = %error, "expired open order recompute failed");
            }
        }
    }
}

fn kvdb_dependency_check(
    stream: StreamId,
    scan_cursor_state: Option<&crate::db::repositories::ScanCursorState>,
    log_cursor: &crate::transfer_log_store::TransferLogCursor,
    retention_floor_block: Option<u64>,
) -> DependencyCheck {
    let Some(scan_cursor_state) = scan_cursor_state else {
        return DependencyCheck::failed(
            DependencyName::Kvdb,
            format!(
                "scan cursor state missing for {} / {}",
                stream.chain_id, stream.token_address
            ),
        );
    };

    let Some(retention_floor_block) = retention_floor_block else {
        return DependencyCheck::failed(DependencyName::Kvdb, "retention floor unavailable");
    };

    let completed_block = log_cursor
        .last_completed_block
        .unwrap_or_else(|| log_cursor.start_block.saturating_sub(1));
    if log_cursor.reorg_epoch != scan_cursor_state.seen_kv_reorg_epoch {
        return DependencyCheck::failed(
            DependencyName::Kvdb,
            format!(
                "kv reorg epoch mismatch: kv {} scanner {}",
                log_cursor.reorg_epoch, scan_cursor_state.seen_kv_reorg_epoch
            ),
        );
    }

    if completed_block < retention_floor_block {
        return DependencyCheck::failed(
            DependencyName::Kvdb,
            format!(
                "kvdb coverage ends at block {}, retention floor requires {}",
                completed_block, retention_floor_block
            ),
        );
    }

    DependencyCheck::healthy(DependencyName::Kvdb)
}

async fn transfer_log_retention_loop<S>(
    repository: PgOrderRepository,
    log_store: RedbTransferLogIngestor<S>,
    stream: StreamId,
    start_block: u64,
    reorg_lookback_blocks: u64,
    manual_rebuild_floor_block: Option<u64>,
    poll_interval: std::time::Duration,
) where
    S: ChainHeaderReader + TransferLogSource + Send + Sync + 'static,
{
    let mut interval = tokio::time::interval(poll_interval);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_pruned_floor = start_block;

    loop {
        interval.tick().await;

        match repository
            .retention_floor_block(
                stream.chain_id,
                stream.token_address,
                reorg_lookback_blocks,
                manual_rebuild_floor_block,
            )
            .await
        {
            Ok(Some(floor_block)) if floor_block > last_pruned_floor => {
                if floor_block <= start_block {
                    tracing::debug!(
                        chain_id = stream.chain_id,
                        token_address = %stream.token_address,
                        floor_block,
                        start_block,
                        "transfer log retention floor is at or before the stream start block"
                    );
                    continue;
                }

                match log_store.prune_before_block(stream, floor_block) {
                    Ok(()) => {
                        last_pruned_floor = floor_block;
                        tracing::info!(
                            chain_id = stream.chain_id,
                            token_address = %stream.token_address,
                            floor_block,
                            start_block,
                            "transfer log retention pruned"
                        );
                    }
                    Err(error) => {
                        tracing::warn!(
                            chain_id = stream.chain_id,
                            token_address = %stream.token_address,
                            floor_block,
                            error = %error,
                            "transfer log retention prune failed"
                        );
                    }
                }
            }
            Ok(Some(floor_block)) => {
                tracing::debug!(
                    chain_id = stream.chain_id,
                    token_address = %stream.token_address,
                    floor_block,
                    last_pruned_floor,
                    "transfer log retention floor unchanged"
                );
            }
            Ok(None) => {
                tracing::debug!(
                    chain_id = stream.chain_id,
                    token_address = %stream.token_address,
                    "transfer log retention floor unavailable"
                );
            }
            Err(error) => {
                tracing::warn!(
                    chain_id = stream.chain_id,
                    token_address = %stream.token_address,
                    error = %error,
                    "transfer log retention floor lookup failed"
                );
            }
        }
    }
}

fn transfer_log_stream_config(config: &AppConfig) -> TransferLogStreamConfig {
    TransferLogStreamConfig {
        chain_id: config.chain.chain_id,
        token_address: config.chain.token_address,
        start_block: config.chain.start_block,
        poll_interval_ms: config.transfer_log.poll_interval_ms,
        batch_size_blocks: config.transfer_log.batch_size_blocks,
        max_batch_size_blocks: config.transfer_log.max_batch_size_blocks,
        max_logs_per_page: TRANSFER_LOG_MAX_LOGS_PER_PAGE,
        max_unique_to_addresses_per_batch: TRANSFER_LOG_MAX_UNIQUE_TO_ADDRESSES_PER_BATCH,
        max_db_fallback_addresses: TRANSFER_LOG_MAX_DB_FALLBACK_ADDRESSES,
        capacity_probe_blocks: TRANSFER_LOG_CAPACITY_PROBE_BLOCKS,
        reorg_lookback_blocks: reorg_lookback_blocks(config.chain.min_confirmations),
        target_mode: ScanTargetMode::LatestMinusConfirmations(config.chain.min_confirmations),
        rpc_max_retries: TRANSFER_LOG_RPC_MAX_RETRIES,
        log_source: LogSourceKind::RpcRange,
        sparse_headers: config.transfer_log.sparse_headers,
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
    use std::{
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    use axum::{
        Router,
        extract::{Json, State},
        routing::{get, post},
    };
    use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
    use serde::Deserialize;
    use serde_json::json;
    use tokio::net::TcpListener;

    use super::*;
    use crate::auth::{Audience, Claims};
    use crate::config::AppConfig;
    use crate::domain::{DerivationSegment, EvmAddress, RawAmount, TxHash};
    use crate::wallet::DeriveAddressRequest;

    const ED_PRIVATE_KEY_PEM: &str = r#"-----BEGIN PRIVATE KEY-----
MC4CAQAwBQYDK2VwBCIEIGrD/e7uKYqSY4twDEsRfMMuLSrODf14dpTiTK6K1YI0
-----END PRIVATE KEY-----"#;
    const ED_PUBLIC_KEY_PEM: &str = r#"-----BEGIN PUBLIC KEY-----
MCowBQYDK2VwAyEA2+Jj2UvNCvQiUPNYRgSi0cJSPiJI6Rs6D0UTeEpQVj8=
-----END PUBLIC KEY-----"#;

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
        assert_eq!(
            service_config.problem_funds_address,
            config.chain.problem_funds_address
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
    fn jwt_verifier_uses_development_hs256_source() {
        let config = test_config(&[]);
        let verifier = jwt_verifier(&config).expect("jwt verifier");
        let token = signed_token(
            Algorithm::HS256,
            "pay3-key-1",
            &EncodingKey::from_secret(b"0123456789abcdef0123456789abcdef"),
        );

        let principal = verifier.verify_token(&token).expect("token should verify");

        assert_eq!(principal.subject, "merchant-default");
    }

    #[test]
    fn jwt_verifier_uses_pem_public_key_source() {
        let config = test_config_owned(vec![
            ("JWT_PUBLIC_KEY_PEM", ED_PUBLIC_KEY_PEM.to_string()),
            ("JWT_ALGORITHM", "EdDSA".to_string()),
            ("JWT_KEY_ID", "ed-key".to_string()),
        ]);
        let verifier = jwt_verifier(&config).expect("jwt verifier");
        let token = signed_token(
            Algorithm::EdDSA,
            "ed-key",
            &EncodingKey::from_ed_pem(ED_PRIVATE_KEY_PEM.as_bytes()).expect("ed key"),
        );

        let principal = verifier.verify_token(&token).expect("token should verify");

        assert_eq!(principal.subject, "merchant-default");
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
        assert_eq!(
            collector_config.min_confirmations,
            config.chain.min_confirmations
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

    #[tokio::test]
    async fn runtime_signer_bootstraps_local_mnemonic_mode() {
        let config = test_config_owned(vec![
            ("SIGNER_MODE", "local".to_string()),
            ("ALLOW_LOCAL_SIGNER", "true".to_string()),
            (
                "SIGNER_MNEMONIC",
                "test test test test test test test test test test test junk".to_string(),
            ),
        ]);

        let signer = runtime_signer(&config).expect("local signer should bootstrap");

        assert!(matches!(&signer, RuntimeSigner::Local(_)));
        ensure_runtime_signer_health(&signer)
            .await
            .expect("local signer health check");

        let wallet = HdWallet::new(signer.clone());
        let request = DeriveAddressRequest::new("pay3-master", 1, DerivationSegment::ZERO).unwrap();
        let derived = wallet.derive_child_address(request.clone()).await.unwrap();
        let derived_again = wallet.derive_child_address(request).await.unwrap();
        assert_eq!(derived.derivation_path, "m/44'/60'/0'/0/0");
        assert_eq!(derived.address, derived_again.address);

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
            .sign_transaction("pay3-master", &derived.derivation_path, unsigned.clone())
            .await
            .unwrap();

        assert_eq!(signed.request_id, unsigned.request_id);
        assert_eq!(signed.chain_id, unsigned.chain_id);
        assert_eq!(signed.from, derived.address);
        assert_eq!(signed.to, unsigned.to);
        assert_ne!(signed.tx_hash, TxHash::ZERO);
        assert_eq!(signed.raw_tx.first(), Some(&0x02));
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
            (
                "PROBLEM_FUNDS_ADDRESS",
                "0x0000000000000000000000000000000000000003".to_string(),
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
            (
                "PROBLEM_FUNDS_ADDRESS",
                "0x0000000000000000000000000000000000000003".to_string(),
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

    fn signed_token(alg: Algorithm, kid: &str, key: &EncodingKey) -> String {
        let mut header = Header::new(alg);
        header.kid = Some(kid.to_string());
        encode(&header, &default_claims(), key).expect("token should encode")
    }

    fn default_claims() -> Claims {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        Claims {
            exp: now + 3600,
            nbf: now.saturating_sub(1),
            iat: now.saturating_sub(1),
            iss: "pay3".to_string(),
            aud: Audience::One("pay3-api".to_string()),
            sub: "merchant-default".to_string(),
            scope: None,
            scopes: None,
            scp: None,
        }
    }

    #[test]
    fn ensure_kvdb_parent_creates_missing_directory() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("nested").join("pay3.redb");

        ensure_kvdb_parent(&PathBuf::from(&path)).expect("parent directory should be created");

        assert!(path.parent().expect("parent").exists());
    }
}
