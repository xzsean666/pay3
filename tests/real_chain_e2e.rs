use std::{
    collections::BTreeMap,
    env,
    error::Error,
    fmt, fs,
    future::Future,
    path::Path,
    process,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration as StdDuration, SystemTime, UNIX_EPOCH},
};

use alloy_consensus::{SignableTransaction, TxEip1559};
use alloy_primitives::{TxKind, U256};
use alloy_signer::SignerSync;
use alloy_signer_local::PrivateKeySigner;
use pay3::{
    chain::{
        ChainHeaderReader, Eip1559FeeEstimator, Erc20ChainClient, NativeBalanceReader,
        RpcRangeSource, TransactionStatus,
    },
    config::{AppConfig, SignerMode},
    db::{
        migrations::{RuntimeSeedConfig, run_schema_migrations, seed_runtime_config},
        repositories::{
            CollectionRecord, CollectionRecordStatus, PgAuditRepository, PgCollectionRepository,
            PgOrderRepository, PgOutboundRepository, PgPaymentRepository,
        },
    },
    domain::{CollectionFees, EvmAddress, OrderStatus, RawAmount, TxHash},
    services::{
        collections::{
            CollectionAmount, CollectionService, CollectionServiceConfig, CreateCollectionInput,
            CreateCollectionOutcome, NativeBalanceGasChecker,
        },
        orders::{
            CreateOrderInput, CreateOrderServiceOutcome, OrderService, OrderServiceConfig,
            SystemClock,
        },
        payment_windows::RepositoryPaymentWindowLookup,
        payments::{PaymentMatcher, PaymentMatchingConfig},
    },
    signer::{LocalMnemonicSigner, SignedTx, SignerProvider, UnsignedTx},
    transfer_log_store::{
        LogSourceKind, PollOutcome, RedbTransferLogIngestor, ScanTargetMode, StreamId,
        TransferLogIngestor, TransferLogReader, TransferLogStreamConfig,
    },
    wallet::HdWallet,
    workers::{
        collector::{
            CollectionCollectorConfig, CollectionCollectorTickOutcome, CollectionCollectorWorker,
        },
        scanner::{PaymentScannerConfig, PaymentScannerTickOutcome, PaymentScannerWorker},
    },
};
use serde_json::{Value, json};
use sqlx::{Connection, Executor, PgConnection, PgPool, postgres::PgPoolOptions};
use tempfile::TempDir;
use time::Duration as TimeDuration;
use tokio::{sync::Mutex, task::JoinSet};
use uuid::Uuid;

type AnyError = Box<dyn Error + Send + Sync>;
type RealChainOrderService = OrderService<PgOrderRepository, LocalMnemonicSigner, RpcRangeSource>;
type RealChainCollectionService = CollectionService<
    PgOrderRepository,
    PgCollectionRepository,
    PgOutboundRepository,
    PgAuditRepository,
    LocalMnemonicSigner,
    RpcRangeSource,
    NativeBalanceGasChecker<RpcRangeSource>,
>;
type RealChainScanner = PaymentScannerWorker<
    PgPaymentRepository,
    PaymentMatcher<
        RedbTransferLogIngestor<RpcRangeSource>,
        RepositoryPaymentWindowLookup<PgOrderRepository>,
        RpcRangeSource,
    >,
    RedbTransferLogIngestor<RpcRangeSource>,
    RpcRangeSource,
    SystemClock,
>;
type RealChainCollector =
    CollectionCollectorWorker<RealChainCollectionService, PgOutboundRepository, RpcRangeSource>;

const DEFAULT_ENV_FILE: &str = ".env.test";
const DEFAULT_PAYER_DERIVATION_PATH: &str = "m/44'/60'/0'/0/0";
const DEFAULT_ORDER_TTL_SECONDS: u64 = 3_600;
const DEFAULT_BATCH_SIZE_BLOCKS: u64 = 100;
const DEFAULT_RECEIPT_TIMEOUT_SECS: u64 = 180;
const DEFAULT_CONFIRMATION_TIMEOUT_SECS: u64 = 360;
const DEFAULT_COLLECTION_GAS_LIMIT: u64 = 120_000;
const DEFAULT_E2E_CONCURRENCY: usize = 1;
const MAX_RANDOM_PAYMENT_RAW_DIGITS: u32 = 18;

#[tokio::test]
#[ignore = "spends real testnet gas/token; run with PAY3_RUN_REAL_CHAIN_E2E=1"]
async fn real_chain_order_payment_collection_flow() -> Result<(), AnyError> {
    if env::var("PAY3_RUN_REAL_CHAIN_E2E").ok().as_deref() != Some("1") {
        eprintln!("skipping real chain e2e; set PAY3_RUN_REAL_CHAIN_E2E=1");
        return Ok(());
    }

    let env_file = env::var("PAY3_E2E_ENV_FILE").unwrap_or_else(|_| DEFAULT_ENV_FILE.to_string());
    let env_values = EnvValues::load(&env_file)?;
    let config = AppConfig::from_pairs(env_values.pairs())?;
    config.validate_profile()?;

    let signer = local_signer_from_config(&config)?;
    let payer = Arc::new(PaymentWallet::from_env(&env_values, &config)?);
    let payment_amount =
        payment_amount_from_env_or_random(&env_values, config.chain.token_decimals)?;
    let concurrency = concurrency_from_env(&env_values)?;
    let total_payment_amount = scale_raw_amount(payment_amount, concurrency)?;
    let pool_connections = pool_connections_for_concurrency(concurrency);
    eprintln!(
        "real chain e2e payment amount raw: {payment_amount} total_raw={total_payment_amount} concurrency={concurrency} (token decimals: {})",
        config.chain.token_decimals
    );
    let receive_address_index = env_values
        .optional("PAY3_E2E_RECEIVE_ADDRESS_INDEX")
        .unwrap_or("0")
        .parse::<u32>()?;
    let batch_size_blocks = env_values
        .optional("PAY3_E2E_BATCH_SIZE_BLOCKS")
        .unwrap_or("100")
        .parse::<u64>()?
        .max(1);
    let receipt_timeout = StdDuration::from_secs(
        env_values
            .optional("PAY3_E2E_RECEIPT_TIMEOUT_SECS")
            .unwrap_or("180")
            .parse::<u64>()?
            .max(DEFAULT_RECEIPT_TIMEOUT_SECS),
    );
    let confirmation_timeout = StdDuration::from_secs(
        env_values
            .optional("PAY3_E2E_CONFIRMATION_TIMEOUT_SECS")
            .unwrap_or("360")
            .parse::<u64>()?
            .max(DEFAULT_CONFIRMATION_TIMEOUT_SECS),
    );

    let rpc_source =
        RpcRangeSource::from_http_urls(config.chain.chain_id, &config.chain.rpc_http_urls, 1)?;
    retry_transient_external(
        "validate RPC providers",
        StdDuration::from_secs(30),
        || async {
            rpc_source
                .manager()
                .validate_chain_ids()
                .await
                .map(|_| ())
                .map_err(|error| Box::new(error) as AnyError)
        },
    )
    .await?;

    let payer_address = payer.address().await?;
    retry_transient_external(
        "check payer token balance",
        StdDuration::from_secs(30),
        || async {
            ensure_token_balance(
                &rpc_source,
                config.chain.token_address,
                payer_address,
                total_payment_amount,
                "payer",
            )
            .await
        },
    )
    .await?;
    retry_transient_external(
        "check payer native balance",
        StdDuration::from_secs(30),
        || async {
            ensure_native_balance_nonzero(
                &rpc_source,
                config.chain.chain_id,
                payer_address,
                "payer",
            )
            .await
        },
    )
    .await?;

    let latest_head =
        retry_transient_external("read latest head", StdDuration::from_secs(30), || async {
            rpc_source
                .latest_head()
                .await
                .map_err(|error| Box::new(error) as AnyError)
        })
        .await?;
    let start_block = latest_head
        .number
        .checked_add(1)
        .ok_or_else(|| helper_error("latest head overflowed when computing start block"))?;
    eprintln!(
        "[real-chain-e2e] rpc latest head={} -> start_block={}",
        latest_head.number, start_block
    );

    let (pool, schema) = prepare_temp_schema_pool(
        &config.database.url,
        "pay3_real_chain_e2e",
        pool_connections,
    )
    .await?;
    let schema_ident = quote_ident(&schema);

    let result = async {
        let stream = StreamId::new(config.chain.chain_id, config.chain.token_address);
        let order_repo = PgOrderRepository::new(pool.clone());
        let collection_repo = PgCollectionRepository::new(pool.clone());
        let outbound_repo = PgOutboundRepository::new(pool.clone());
        let audit_repo = PgAuditRepository::new(pool.clone());
        let payment_repo = PgPaymentRepository::new(pool.clone());

        run_schema_migrations(&pool).await?;
        seed_runtime_config(
            &pool,
            &RuntimeSeedConfig {
                signer_key_ref: config.signer.key_ref.clone(),
                chain_id: config.chain.chain_id,
                token_address: config.chain.token_address,
                treasury_address: config.chain.treasury_address,
                start_block,
            },
        )
        .await?;
        set_wallet_cursor_address_index(&pool, receive_address_index).await?;

        let kvdb_dir = TempDir::new()?;
        let kvdb_path = kvdb_dir.path().join("real-chain-transfer-log.redb");
        let log_store = RedbTransferLogIngestor::open(rpc_source.clone(), &kvdb_path)?;
        let stream_config = TransferLogStreamConfig {
            chain_id: config.chain.chain_id,
            token_address: config.chain.token_address,
            start_block,
            poll_interval_ms: 1_000,
            batch_size_blocks,
            max_batch_size_blocks: batch_size_blocks.max(DEFAULT_BATCH_SIZE_BLOCKS),
            max_logs_per_page: 1_000,
            max_unique_to_addresses_per_batch: 1_000,
            max_db_fallback_addresses: 1_000,
            capacity_probe_blocks: 1,
            reorg_lookback_blocks: config.chain.min_confirmations.max(1),
            target_mode: ScanTargetMode::LatestMinusConfirmations(0),
            rpc_max_retries: 3,
            log_source: LogSourceKind::RpcRange,
        };
        log_store.ensure_stream(stream_config).await?;

        let order_service = OrderService::new(
            OrderServiceConfig::new(
                config.chain.chain_id,
                config.chain.token_address,
                24 * 60 * 60,
            ),
            order_repo.clone(),
            HdWallet::new(signer.clone()),
            rpc_source.clone(),
        )?;

        let payment_matcher = PaymentMatcher::new(
            log_store.clone(),
            RepositoryPaymentWindowLookup::new(order_repo.clone(), 1_000),
            rpc_source.clone(),
            PaymentMatchingConfig {
                stream,
                min_confirmations: config.chain.min_confirmations,
                page_limit: 1_000,
                max_unique_to_addresses_per_batch: 1_000,
            },
        );
        let scanner = Arc::new(PaymentScannerWorker::new(
            payment_repo,
            payment_matcher,
            log_store.clone(),
            rpc_source.clone(),
            SystemClock,
            PaymentScannerConfig::new("real-chain-scanner-e2e", stream, TimeDuration::seconds(60))
                .with_confirmation_sweep_limit(1_000),
        ));

        let gas_limit = env_values
            .optional("PAY3_E2E_COLLECTION_GAS_LIMIT")
            .unwrap_or("120000")
            .parse::<u64>()?
            .max(DEFAULT_COLLECTION_GAS_LIMIT);
        let fee_estimate = retry_transient_external(
            "estimate collection fees",
            StdDuration::from_secs(30),
            || async {
                rpc_source
                    .estimate_eip1559_fees()
                    .await
                    .map_err(|error| Box::new(error) as AnyError)
            },
        )
        .await?;
        let collection_fees = CollectionFees::new(
            gas_limit,
            fee_estimate.max_fee_per_gas,
            fee_estimate.max_priority_fee_per_gas,
        );
        let collection_config = CollectionServiceConfig::new(
            config.chain.chain_id,
            config.chain.token_address,
            config.chain.treasury_address,
            collection_fees,
        );
        let collection_config_for_concurrent = collection_config.clone();
        let collection_service = CollectionService::new(
            collection_config.clone(),
            order_repo.clone(),
            collection_repo.clone(),
            outbound_repo.clone(),
            audit_repo.clone(),
            signer.clone(),
            rpc_source.clone(),
            NativeBalanceGasChecker::new(rpc_source.clone()),
        )?;
        let collector_collection_service = CollectionService::new(
            collection_config,
            order_repo.clone(),
            collection_repo.clone(),
            outbound_repo.clone(),
            audit_repo.clone(),
            signer.clone(),
            rpc_source.clone(),
            NativeBalanceGasChecker::new(rpc_source.clone()),
        )?;
        let collector: RealChainCollector = CollectionCollectorWorker::new(
            collector_collection_service,
            outbound_repo.clone(),
            rpc_source.clone(),
            CollectionCollectorConfig::new("real-chain-collector-e2e")
                .with_replacement_stuck_after(StdDuration::from_secs(30 * 60)),
        );

        if concurrency > 1 {
            eprintln!(
                "[real-chain-e2e] running concurrent real-chain flow with concurrency={concurrency}"
            );
            return run_real_chain_concurrent_flow(ConcurrentRealChainContext {
                concurrency,
                order_service: order_service.clone(),
                scanner: scanner.clone(),
                log_store: log_store.clone(),
                rpc_source: rpc_source.clone(),
                pool: pool.clone(),
                order_repo: order_repo.clone(),
                collection_repo: collection_repo.clone(),
                outbound_repo: outbound_repo.clone(),
                audit_repo: audit_repo.clone(),
                collection_config: collection_config_for_concurrent.clone(),
                signer: signer.clone(),
                payer: payer.clone(),
                payment_amount,
                total_payment_amount,
                collection_fees,
                stream,
                chain_id: config.chain.chain_id,
                token_address: config.chain.token_address,
                treasury_address: config.chain.treasury_address,
                rpc_url: config.chain.rpc_http_urls[0].clone(),
                receipt_timeout,
                confirmation_timeout,
                payer_address,
                min_confirmations: config.chain.min_confirmations,
            })
            .await;
        }

        let order_external_id = format!("real-chain-e2e-order-{}", Uuid::new_v4());
        eprintln!(
            "[real-chain-e2e] creating order external_id={} amount_raw={} receive_address_index={}",
            order_external_id, payment_amount, receive_address_index
        );
        let order_result = order_service
            .create_order(CreateOrderInput::new(
                order_external_id,
                payment_amount,
                DEFAULT_ORDER_TTL_SECONDS,
            ))
            .await?;
        assert_eq!(order_result.outcome, CreateOrderServiceOutcome::Created);
        assert_eq!(
            order_result.view.child_account.address,
            order_result.view.order.receive_address
        );
        eprintln!(
            "[real-chain-e2e] order created order_id={} external_id={} status={:?} receive_address={} child_account={} paid_amount_raw={}",
            order_result.view.order.id,
            order_result.view.order.external_id,
            order_result.view.order.status,
            order_result.view.order.receive_address,
            order_result.view.child_account.address,
            order_result.view.order.paid_amount_raw
        );

        let receive_address = order_result.view.order.receive_address;
        if receive_address == config.chain.treasury_address {
            return Err(helper_error(format!(
                "receive address {receive_address} equals treasury address; set TREASURY_ADDRESS to a separate collection recipient or set PAY3_E2E_RECEIVE_ADDRESS_INDEX to a funded signer child"
            )));
        }
        ensure_native_balance_nonzero(
            &rpc_source,
            config.chain.chain_id,
            receive_address,
            "receive/collect address",
        )
        .await?;

        let treasury_before = rpc_source
            .token_balance(config.chain.token_address, config.chain.treasury_address)
            .await?;
        eprintln!(
            "[real-chain-e2e] treasury balance before payment={} token_address={}",
            treasury_before, config.chain.token_address
        );

        eprintln!(
            "[real-chain-e2e] broadcasting payment tx payer={} receive_address={} amount_raw={}",
            payer_address, receive_address, payment_amount
        );
        let payment_tx_hash = retry_transient_external(
            "broadcast payment transaction",
            StdDuration::from_secs(30),
            || async {
                send_payment(
                    &payer,
                    &rpc_source,
                    &config.chain.rpc_http_urls[0],
                    TokenPaymentRequest {
                        chain_id: config.chain.chain_id,
                        token_address: config.chain.token_address,
                        recipient: receive_address,
                        amount: payment_amount,
                        gas_limit: collection_fees.gas_limit,
                        max_fee_per_gas: collection_fees.max_fee_per_gas,
                        max_priority_fee_per_gas: collection_fees.max_priority_fee_per_gas,
                    },
                )
                .await
            },
        )
        .await?;
        eprintln!(
            "[real-chain-e2e] payment tx broadcast tx_hash={}",
            payment_tx_hash
        );
        let payment_receipt =
            wait_for_successful_receipt(&rpc_source, payment_tx_hash, receipt_timeout).await?;
        eprintln!(
            "[real-chain-e2e] payment mined tx_hash={} block={:?} status={:?}",
            payment_tx_hash, payment_receipt.block, payment_receipt.status
        );
        wait_for_confirmations(
            &rpc_source,
            payment_receipt.block,
            config.chain.min_confirmations,
            confirmation_timeout,
        )
        .await?;
        eprintln!(
            "[real-chain-e2e] payment confirmed tx_hash={} block_number={} min_confirmations={}",
            payment_tx_hash, payment_receipt.block.number, config.chain.min_confirmations
        );

        eprintln!(
            "[real-chain-e2e] waiting for transfer log store through block {}",
            payment_receipt.block.number
        );
        poll_log_store_until(
            &log_store,
            stream,
            payment_receipt.block.number,
            confirmation_timeout,
        )
        .await?;
        eprintln!(
            "[real-chain-e2e] transfer log store caught up through block {}",
            payment_receipt.block.number
        );
        eprintln!(
            "[real-chain-e2e] scanning until order {} becomes paid",
            order_result.view.order.id
        );
        tick_scanner_until_paid(
            &scanner,
            &order_service,
            order_result.view.order.id,
            confirmation_timeout,
        )
        .await?;
        eprintln!(
            "[real-chain-e2e] order paid order_id={} status=paid paid_amount_raw={}",
            order_result.view.order.id, payment_amount
        );

        eprintln!(
            "[real-chain-e2e] syncing collection nonce receive_address={}",
            receive_address
        );
        retry_transient_external("sync collection nonce", StdDuration::from_secs(30), || async {
            sync_account_nonce(
                &pool,
                &config.chain.rpc_http_urls[0],
                config.chain.chain_id,
                receive_address,
            )
            .await
        })
        .await?;
        eprintln!(
            "[real-chain-e2e] collection nonce synced receive_address={}",
            receive_address
        );

        eprintln!(
            "[real-chain-e2e] creating collection order_id={} amount_raw={} treasury={}",
            order_result.view.order.id, payment_amount, config.chain.treasury_address
        );
        let collection_result = collection_service
            .create_collection(CreateCollectionInput {
                order_id: order_result.view.order.id,
                amount: CollectionAmount::Exact(payment_amount),
                idempotency_key: format!("real-chain-e2e-collect-{}", Uuid::new_v4()),
                audit: Default::default(),
            })
            .await?;
        assert_eq!(collection_result.outcome, CreateCollectionOutcome::Created);
        assert_eq!(
            collection_result.collection.status,
            CollectionRecordStatus::Queued
        );
        eprintln!(
            "[real-chain-e2e] collection created collection_id={} status={:?} outbound_tx_id={:?}",
            collection_result.collection.id,
            collection_result.collection.status,
            collection_result.collection.outbound_tx_id
        );

        let collection_id = collection_result.collection.id;
        eprintln!(
            "[real-chain-e2e] collector tick expecting broadcast collection_id={}",
            collection_id
        );
        let collect_tx_hash = match collector.tick().await? {
            CollectionCollectorTickOutcome::Broadcast {
                collection_id: actual_collection_id,
                outbound,
            } => {
                assert_eq!(actual_collection_id, collection_id);
                eprintln!(
                    "[real-chain-e2e] collection broadcast collection_id={} outbound_tx_id={} tx_hash={} nonce={} status={}",
                    actual_collection_id,
                    outbound.id,
                    outbound.tx_hash,
                    outbound.nonce,
                    outbound.status.as_db_str()
                );
                outbound.tx_hash
            }
            other => panic!("expected broadcast collection tick, got {other:?}"),
        };

        let collect_receipt =
            wait_for_successful_receipt(&rpc_source, collect_tx_hash, receipt_timeout).await?;
        eprintln!(
            "[real-chain-e2e] collection mined tx_hash={} block={:?} status={:?}",
            collect_tx_hash, collect_receipt.block, collect_receipt.status
        );
        eprintln!(
            "[real-chain-e2e] collector tick expecting confirmation collection_id={}",
            collection_id
        );
        match collector.tick().await? {
            CollectionCollectorTickOutcome::Confirmed {
                collection_id: actual_collection_id,
                outbound,
            } => {
                assert_eq!(actual_collection_id, collection_id);
                assert_eq!(outbound.status.as_db_str(), "confirmed");
                eprintln!(
                    "[real-chain-e2e] collection confirmed collection_id={} outbound_tx_id={} tx_hash={} status={}",
                    actual_collection_id,
                    outbound.id,
                    outbound.tx_hash,
                    outbound.status.as_db_str()
                );
            }
            other => panic!("expected confirmed collection tick, got {other:?}"),
        }

        let final_collection = collection_service
            .get_collection(collection_id)
            .await?
            .expect("collection must be readable after confirmation");
        assert_eq!(final_collection.status, CollectionRecordStatus::Confirmed);
        eprintln!(
            "[real-chain-e2e] final collection state collection_id={} status={:?} outbound_tx_id={:?}",
            final_collection.id,
            final_collection.status,
            final_collection.outbound_tx_id
        );

        let paid_order = order_service
            .get_order(order_result.view.order.id)
            .await?
            .expect("order must remain readable");
        assert_eq!(paid_order.order.status, OrderStatus::Paid);
        assert_eq!(paid_order.order.paid_amount_raw, payment_amount);
        eprintln!(
            "[real-chain-e2e] final order state order_id={} status={:?} paid_amount_raw={} receive_address={}",
            paid_order.order.id,
            paid_order.order.status,
            paid_order.order.paid_amount_raw,
            paid_order.order.receive_address
        );

        let treasury_after = rpc_source
            .token_balance(config.chain.token_address, config.chain.treasury_address)
            .await?;
        let expected_treasury_delta = if payer_address == config.chain.treasury_address {
            RawAmount::ZERO
        } else {
            payment_amount
        };
        assert_eq!(
            treasury_after.checked_sub(treasury_before),
            Some(expected_treasury_delta),
            "treasury token delta should match net payment and collection movement"
        );
        eprintln!(
            "[real-chain-e2e] treasury balance after collection before={} after={} delta={}",
            treasury_before,
            treasury_after,
            treasury_after.checked_sub(treasury_before).unwrap_or(RawAmount::ZERO)
        );

        Ok::<(), AnyError>(())
    }
    .await;

    pool.close().await;
    let cleanup_result = drop_schema(&config.database.url, &schema_ident).await;
    match result {
        Ok(()) => {
            cleanup_result?;
            Ok(())
        }
        Err(error) => {
            if let Err(cleanup_error) = cleanup_result {
                eprintln!("failed to drop temp schema {schema}: {cleanup_error}");
            }
            Err(error)
        }
    }
}

#[derive(Clone, Debug)]
struct EnvValues {
    values: BTreeMap<String, String>,
}

impl EnvValues {
    fn load(path: impl AsRef<Path>) -> Result<Self, AnyError> {
        let mut values = BTreeMap::new();
        let path = path.as_ref();
        let contents = fs::read_to_string(path)
            .map_err(|error| helper_error(format!("failed to read {}: {error}", path.display())))?;
        for line in contents.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let line = line.strip_prefix("export ").unwrap_or(line);
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let key = key.trim();
            if key.is_empty() {
                continue;
            }
            values.insert(key.to_string(), unquote_env_value(value.trim()));
        }

        for (key, value) in env::vars() {
            values.insert(key, value);
        }

        Ok(Self { values })
    }

    fn optional(&self, key: &str) -> Option<&str> {
        self.values
            .get(key)
            .map(String::as_str)
            .filter(|value| !value.trim().is_empty())
    }

    fn pairs(&self) -> impl Iterator<Item = (String, String)> + '_ {
        self.values
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
    }
}

fn unquote_env_value(value: &str) -> String {
    let value = value.trim();
    if value.len() >= 2 {
        let bytes = value.as_bytes();
        if (bytes[0] == b'"' && bytes[value.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[value.len() - 1] == b'\'')
        {
            return value[1..value.len() - 1].to_string();
        }
    }
    value.to_string()
}

fn payment_amount_from_env_or_random(
    values: &EnvValues,
    token_decimals: u8,
) -> Result<RawAmount, AnyError> {
    if let Some(amount) = values.optional("PAY3_E2E_PAYMENT_AMOUNT_RAW") {
        let amount = amount.parse::<RawAmount>()?;
        let minimum = minimum_visible_payment_amount(token_decimals);
        if amount >= minimum {
            return Ok(amount);
        }
        eprintln!(
            "PAY3_E2E_PAYMENT_AMOUNT_RAW={amount} is below visible floor {minimum}; using a randomized visible amount instead"
        );
    }

    Ok(random_visible_payment_amount(token_decimals))
}

fn minimum_visible_payment_amount(token_decimals: u8) -> RawAmount {
    RawAmount::from(minimum_visible_payment_amount_u64(token_decimals))
}

fn random_visible_payment_amount(token_decimals: u8) -> RawAmount {
    let raw_digits = random_payment_raw_digits(token_decimals);
    let lower = minimum_visible_payment_amount_u64(token_decimals);
    let upper = 10u64.pow(raw_digits);
    let random_offset = (Uuid::new_v4().as_u128() % u128::from(upper - lower)) as u64;
    RawAmount::from(lower + random_offset)
}

fn minimum_visible_payment_amount_u64(token_decimals: u8) -> u64 {
    let raw_digits = random_payment_raw_digits(token_decimals);
    10u64.pow(raw_digits.saturating_sub(1))
}

fn random_payment_raw_digits(token_decimals: u8) -> u32 {
    u32::from(token_decimals).clamp(1, MAX_RANDOM_PAYMENT_RAW_DIGITS)
}

#[test]
fn default_real_chain_payment_amount_is_visible_for_18_decimals() {
    for _ in 0..16 {
        let amount = random_visible_payment_amount(18);

        assert_eq!(amount.to_string().len(), 18);
        assert_ne!(amount, RawAmount::from(1));
    }
}

#[test]
fn tiny_real_chain_payment_amount_env_is_promoted_to_visible_random_amount() {
    let values = EnvValues {
        values: BTreeMap::from([("PAY3_E2E_PAYMENT_AMOUNT_RAW".to_string(), "1".to_string())]),
    };

    let amount = payment_amount_from_env_or_random(&values, 18).unwrap();

    assert_eq!(amount.to_string().len(), 18);
    assert_ne!(amount, RawAmount::from(1));
}

fn concurrency_from_env(values: &EnvValues) -> Result<usize, AnyError> {
    let concurrency = match values.optional("PAY3_E2E_CONCURRENCY") {
        Some(value) => value.parse::<usize>()?,
        None => DEFAULT_E2E_CONCURRENCY,
    }
    .max(1);
    Ok(concurrency)
}

fn pool_connections_for_concurrency(concurrency: usize) -> u32 {
    let connections = concurrency.saturating_mul(2).max(4);
    u32::try_from(connections).unwrap_or(u32::MAX)
}

fn scale_raw_amount(amount: RawAmount, factor: usize) -> Result<RawAmount, AnyError> {
    let factor = u64::try_from(factor)?;
    let scaled = amount
        .value()
        .checked_mul(U256::from(factor))
        .ok_or_else(|| helper_error("raw amount overflow while scaling by concurrency"))?;
    Ok(RawAmount::new(scaled))
}

#[derive(Debug)]
struct NonceAllocator {
    next: AtomicU64,
}

impl NonceAllocator {
    fn new(seed: u64) -> Self {
        Self {
            next: AtomicU64::new(seed),
        }
    }

    fn reserve(&self) -> u64 {
        self.next.fetch_add(1, Ordering::SeqCst)
    }
}

#[derive(Clone)]
struct ConcurrentRealChainContext {
    concurrency: usize,
    order_service: RealChainOrderService,
    scanner: Arc<RealChainScanner>,
    log_store: RedbTransferLogIngestor<RpcRangeSource>,
    rpc_source: RpcRangeSource,
    pool: PgPool,
    order_repo: PgOrderRepository,
    collection_repo: PgCollectionRepository,
    outbound_repo: PgOutboundRepository,
    audit_repo: PgAuditRepository,
    collection_config: CollectionServiceConfig,
    signer: LocalMnemonicSigner,
    payer: Arc<PaymentWallet>,
    payment_amount: RawAmount,
    total_payment_amount: RawAmount,
    collection_fees: CollectionFees,
    stream: StreamId,
    chain_id: u64,
    token_address: EvmAddress,
    treasury_address: EvmAddress,
    rpc_url: String,
    receipt_timeout: StdDuration,
    confirmation_timeout: StdDuration,
    payer_address: EvmAddress,
    min_confirmations: u64,
}

#[derive(Debug)]
struct ConcurrentFlowResult {
    collection_id: Uuid,
}

fn local_signer_from_config(config: &AppConfig) -> Result<LocalMnemonicSigner, AnyError> {
    if !matches!(config.signer.mode, SignerMode::Local) {
        return Err(helper_error(
            "real-chain e2e currently requires SIGNER_MODE=local so the test can derive and sign collection transactions",
        ));
    }
    let mnemonic = config
        .signer
        .mnemonic
        .as_deref()
        .ok_or_else(|| helper_error("SIGNER_MODE=local requires SIGNER_MNEMONIC"))?;
    Ok(LocalMnemonicSigner::new(
        config.signer.key_ref.clone(),
        mnemonic,
    )?)
}

enum PaymentWallet {
    Mnemonic {
        signer: LocalMnemonicSigner,
        key_ref: String,
        path: String,
    },
    PrivateKey {
        signer: PrivateKeySigner,
    },
}

impl PaymentWallet {
    fn from_env(values: &EnvValues, config: &AppConfig) -> Result<Self, AnyError> {
        if let Some(private_key) = values
            .optional("PAY3_E2E_PAYER_PRIVATE_KEY")
            .or_else(|| values.optional("DEPLOYER_PRIVATE_KEY"))
        {
            let private_key = private_key
                .trim()
                .strip_prefix("0x")
                .or_else(|| private_key.trim().strip_prefix("0X"))
                .unwrap_or_else(|| private_key.trim());
            let signer = private_key.parse::<PrivateKeySigner>()?;
            return Ok(Self::PrivateKey { signer });
        }

        let mnemonic = values
            .optional("PAY3_E2E_PAYER_MNEMONIC")
            .or(config.signer.mnemonic.as_deref())
            .ok_or_else(|| {
                helper_error(
                    "configure PAY3_E2E_PAYER_PRIVATE_KEY, DEPLOYER_PRIVATE_KEY, PAY3_E2E_PAYER_MNEMONIC, or SIGNER_MNEMONIC for the payment sender",
                )
            })?;
        let path = values
            .optional("PAY3_E2E_PAYER_DERIVATION_PATH")
            .unwrap_or(DEFAULT_PAYER_DERIVATION_PATH)
            .to_string();
        let key_ref = values
            .optional("PAY3_E2E_PAYER_KEY_REF")
            .unwrap_or(&config.signer.key_ref)
            .to_string();
        let signer = LocalMnemonicSigner::new(key_ref.clone(), mnemonic)?;
        Ok(Self::Mnemonic {
            signer,
            key_ref,
            path,
        })
    }

    async fn address(&self) -> Result<EvmAddress, AnyError> {
        match self {
            Self::Mnemonic {
                signer,
                key_ref,
                path,
            } => Ok(SignerProvider::derive_address(signer, key_ref, path).await?),
            Self::PrivateKey { signer } => Ok(EvmAddress::from_alloy(signer.address())),
        }
    }

    async fn sign_transaction(&self, tx: UnsignedTx) -> Result<SignedTx, AnyError> {
        match self {
            Self::Mnemonic {
                signer,
                key_ref,
                path,
            } => Ok(signer.sign_transaction(key_ref, path, tx).await?),
            Self::PrivateKey { signer } => sign_with_private_key(signer, tx),
        }
    }
}

struct TokenPaymentRequest {
    chain_id: u64,
    token_address: EvmAddress,
    recipient: EvmAddress,
    amount: RawAmount,
    gas_limit: u64,
    max_fee_per_gas: RawAmount,
    max_priority_fee_per_gas: RawAmount,
}

struct NativeTransferRequest {
    chain_id: u64,
    recipient: EvmAddress,
    amount: RawAmount,
    gas_limit: u64,
    max_fee_per_gas: RawAmount,
    max_priority_fee_per_gas: RawAmount,
}

async fn send_payment(
    payer: &PaymentWallet,
    rpc_source: &RpcRangeSource,
    rpc_url: &str,
    request: TokenPaymentRequest,
) -> Result<TxHash, AnyError> {
    let payer_address = payer.address().await?;
    let nonce = current_nonce(rpc_url, payer_address).await?;
    send_payment_with_nonce(payer, rpc_source, request, nonce).await
}

async fn send_payment_with_nonce(
    payer: &PaymentWallet,
    rpc_source: &RpcRangeSource,
    request: TokenPaymentRequest,
    nonce: u64,
) -> Result<TxHash, AnyError> {
    let payer_address = payer.address().await?;
    let unsigned = UnsignedTx::new(
        format!("real-chain-e2e-payment-{}", Uuid::new_v4()),
        request.chain_id,
        nonce,
        request.token_address,
        RawAmount::ZERO,
        request.gas_limit,
        request.max_fee_per_gas,
        request.max_priority_fee_per_gas,
        erc20_transfer_data(request.recipient, request.amount),
    )?;
    let signed = payer.sign_transaction(unsigned).await?;
    if signed.from != payer_address {
        return Err(helper_error(format!(
            "payer signer produced from address {}, expected {}",
            signed.from, payer_address
        )));
    }
    let broadcast_hash = rpc_source.broadcast_signed_tx(signed.raw_tx).await?;
    if broadcast_hash != signed.tx_hash {
        return Err(helper_error(format!(
            "payment broadcast hash mismatch: signed {}, broadcast {}",
            signed.tx_hash, broadcast_hash
        )));
    }
    Ok(broadcast_hash)
}

async fn send_native_transfer_with_nonce(
    payer: &PaymentWallet,
    rpc_source: &RpcRangeSource,
    request: NativeTransferRequest,
    nonce: u64,
) -> Result<TxHash, AnyError> {
    let payer_address = payer.address().await?;
    let unsigned = UnsignedTx::new(
        format!("real-chain-e2e-native-topup-{}", Uuid::new_v4()),
        request.chain_id,
        nonce,
        request.recipient,
        request.amount,
        request.gas_limit,
        request.max_fee_per_gas,
        request.max_priority_fee_per_gas,
        Vec::new(),
    )?;
    let signed = payer.sign_transaction(unsigned).await?;
    if signed.from != payer_address {
        return Err(helper_error(format!(
            "payer signer produced from address {}, expected {}",
            signed.from, payer_address
        )));
    }
    let broadcast_hash = rpc_source.broadcast_signed_tx(signed.raw_tx).await?;
    if broadcast_hash != signed.tx_hash {
        return Err(helper_error(format!(
            "native transfer broadcast hash mismatch: signed {}, broadcast {}",
            signed.tx_hash, broadcast_hash
        )));
    }

    Ok(broadcast_hash)
}

fn sign_with_private_key(signer: &PrivateKeySigner, tx: UnsignedTx) -> Result<SignedTx, AnyError> {
    let request_id = tx.request_id.clone();
    let eip1559 = TxEip1559 {
        chain_id: tx.chain_id,
        nonce: tx.nonce,
        gas_limit: tx.gas_limit,
        max_fee_per_gas: raw_amount_to_u128(tx.max_fee_per_gas, "max_fee_per_gas")?,
        max_priority_fee_per_gas: raw_amount_to_u128(
            tx.max_priority_fee_per_gas,
            "max_priority_fee_per_gas",
        )?,
        to: TxKind::Call(tx.to.into_alloy()),
        value: tx.value.value(),
        access_list: Default::default(),
        input: tx.data.clone().into(),
    };
    let signature = signer.sign_hash_sync(&eip1559.signature_hash())?;
    let signed = eip1559.into_signed(signature);
    let tx_hash = TxHash::from_alloy(*signed.hash());
    let mut raw_tx = Vec::with_capacity(signed.eip2718_encoded_length());
    signed.eip2718_encode(&mut raw_tx);

    Ok(SignedTx {
        request_id,
        chain_id: tx.chain_id,
        nonce: tx.nonce,
        from: EvmAddress::from_alloy(signer.address()),
        to: tx.to,
        tx_hash,
        raw_tx,
    })
}

fn raw_amount_to_u128(value: RawAmount, field: &'static str) -> Result<u128, AnyError> {
    u128::try_from(value.value()).map_err(|_| helper_error(format!("{field} exceeds u128")))
}

fn erc20_transfer_data(recipient: EvmAddress, amount: RawAmount) -> Vec<u8> {
    let mut data = Vec::with_capacity(68);
    data.extend_from_slice(&[0xa9, 0x05, 0x9c, 0xbb]);
    data.extend_from_slice(&[0u8; 12]);
    data.extend_from_slice(recipient.as_bytes());
    let amount = amount.value().to_be_bytes::<32>();
    data.extend_from_slice(&amount);
    data
}

async fn ensure_token_balance(
    rpc_source: &RpcRangeSource,
    token: EvmAddress,
    owner: EvmAddress,
    required: RawAmount,
    label: &str,
) -> Result<(), AnyError> {
    let balance = rpc_source.token_balance(token, owner).await?;
    if balance < required {
        return Err(helper_error(format!(
            "{label} address {owner} token balance {balance} is below required payment amount {required}; fund it or set PAY3_E2E_PAYER_PRIVATE_KEY/PAY3_E2E_PAYER_MNEMONIC to a funded payer"
        )));
    }
    Ok(())
}

async fn ensure_native_balance_nonzero(
    rpc_source: &RpcRangeSource,
    chain_id: u64,
    owner: EvmAddress,
    label: &str,
) -> Result<(), AnyError> {
    let balance = rpc_source.native_balance(chain_id, owner).await?;
    if balance.is_zero() {
        return Err(helper_error(format!(
            "{label} address {owner} has zero native gas balance"
        )));
    }
    Ok(())
}

async fn wait_for_successful_receipt(
    rpc_source: &RpcRangeSource,
    tx_hash: TxHash,
    timeout: StdDuration,
) -> Result<pay3::chain::TxReceipt, AnyError> {
    let deadline = std::time::Instant::now() + timeout;
    let mut last_transient_error = None;
    eprintln!(
        "[real-chain-e2e] waiting for receipt tx_hash={} timeout_secs={}",
        tx_hash,
        timeout.as_secs()
    );
    loop {
        if std::time::Instant::now() > deadline {
            let suffix = last_transient_error
                .as_ref()
                .map(|error| format!("; last transient error: {error}"))
                .unwrap_or_default();
            return Err(helper_error(format!(
                "timed out waiting for receipt of {tx_hash}{suffix}"
            )));
        }
        match rpc_source.transaction_receipt(tx_hash).await {
            Ok(Some(receipt)) => {
                if receipt.status != TransactionStatus::Success {
                    return Err(helper_error(format!("transaction {tx_hash} reverted")));
                }
                return Ok(receipt);
            }
            Ok(None) => {}
            Err(error) if is_transient_external_error(&error) => {
                last_transient_error = Some(error.to_string());
                eprintln!("temporary receipt lookup failure for {tx_hash}: {error}");
            }
            Err(error) => return Err(Box::new(error)),
        }
        tokio::time::sleep(StdDuration::from_secs(2)).await;
    }
}

async fn wait_for_confirmations(
    rpc_source: &RpcRangeSource,
    block: pay3::domain::ChainBlockRef,
    min_confirmations: u64,
    timeout: StdDuration,
) -> Result<(), AnyError> {
    let deadline = std::time::Instant::now() + timeout;
    let mut last_transient_error = None;
    eprintln!(
        "[real-chain-e2e] waiting for {} confirmations for block {}",
        min_confirmations, block.number
    );
    loop {
        match rpc_source.latest_head().await {
            Ok(head) => {
                if block.has_confirmations(head, min_confirmations) {
                    return Ok(());
                }
            }
            Err(error) if is_transient_external_error(&error) => {
                last_transient_error = Some(error.to_string());
                eprintln!(
                    "temporary head lookup failure while waiting for {min_confirmations} confirmations for block {}: {error}",
                    block.number
                );
            }
            Err(error) => return Err(Box::new(error)),
        }
        if std::time::Instant::now() > deadline {
            let suffix = last_transient_error
                .as_ref()
                .map(|error| format!("; last transient error: {error}"))
                .unwrap_or_default();
            return Err(helper_error(format!(
                "timed out waiting for {min_confirmations} confirmations for block {}{suffix}",
                block.number
            )));
        }
        tokio::time::sleep(StdDuration::from_secs(2)).await;
    }
}

async fn poll_log_store_until(
    log_store: &RedbTransferLogIngestor<RpcRangeSource>,
    stream: StreamId,
    target_block: u64,
    timeout: StdDuration,
) -> Result<(), AnyError> {
    let deadline = std::time::Instant::now() + timeout;
    eprintln!(
        "[real-chain-e2e] polling transfer log store stream={:?} until block {}",
        stream, target_block
    );
    loop {
        let cursor = log_store.cursor(stream).await?;
        if cursor
            .last_completed_block
            .is_some_and(|completed| completed >= target_block)
        {
            return Ok(());
        }
        match log_store.poll_once(stream).await {
            Ok(PollOutcome::Advanced { .. } | PollOutcome::Rewound { .. }) => {}
            Ok(PollOutcome::Idle { .. }) => tokio::time::sleep(StdDuration::from_secs(2)).await,
            Err(error) if is_transient_external_error(&error) => {
                eprintln!("temporary log store poll failure for stream {stream:?}: {error}");
                tokio::time::sleep(StdDuration::from_secs(2)).await;
            }
            Err(error) => return Err(Box::new(error)),
        }
        if std::time::Instant::now() > deadline {
            return Err(helper_error(format!(
                "timed out waiting for transfer log store to cover block {target_block}"
            )));
        }
    }
}

async fn tick_scanner_until_paid<D, H>(
    scanner: &RealChainScanner,
    order_service: &OrderService<PgOrderRepository, D, H>,
    order_id: Uuid,
    timeout: StdDuration,
) -> Result<(), AnyError>
where
    D: pay3::wallet::AddressDeriver,
    H: pay3::services::orders::OrderChainHeadReader,
{
    tick_scanner_until_paid_inner(scanner, None, order_service, order_id, timeout).await
}

async fn tick_scanner_until_paid_serialized<D, H>(
    scanner: &RealChainScanner,
    scanner_lock: &Mutex<()>,
    order_service: &OrderService<PgOrderRepository, D, H>,
    order_id: Uuid,
    timeout: StdDuration,
) -> Result<(), AnyError>
where
    D: pay3::wallet::AddressDeriver,
    H: pay3::services::orders::OrderChainHeadReader,
{
    tick_scanner_until_paid_inner(
        scanner,
        Some(scanner_lock),
        order_service,
        order_id,
        timeout,
    )
    .await
}

async fn tick_scanner_until_paid_inner<D, H>(
    scanner: &RealChainScanner,
    scanner_lock: Option<&Mutex<()>>,
    order_service: &OrderService<PgOrderRepository, D, H>,
    order_id: Uuid,
    timeout: StdDuration,
) -> Result<(), AnyError>
where
    D: pay3::wallet::AddressDeriver,
    H: pay3::services::orders::OrderChainHeadReader,
{
    let deadline = std::time::Instant::now() + timeout;
    let mut last_status = None;
    loop {
        let tick_result = match scanner_lock {
            Some(lock) => {
                let _guard = lock.lock().await;
                scanner.tick().await
            }
            None => scanner.tick().await,
        };
        match tick_result {
            Ok(
                PaymentScannerTickOutcome::Committed { .. }
                | PaymentScannerTickOutcome::ConfirmationsSwept { .. }
                | PaymentScannerTickOutcome::Idle { .. }
                | PaymentScannerTickOutcome::PageIncomplete { .. }
                | PaymentScannerTickOutcome::LeaseHeld { .. }
                | PaymentScannerTickOutcome::KvReorgHandled { .. },
            ) => {}
            Err(error) if is_transient_external_error(&error) => {
                eprintln!("temporary scanner tick failure for order {order_id}: {error}");
                tokio::time::sleep(StdDuration::from_secs(2)).await;
            }
            Err(error) => return Err(Box::new(error)),
        }
        let order = order_service
            .get_order(order_id)
            .await?
            .ok_or_else(|| helper_error(format!("order disappeared: {order_id}")))?;
        if last_status != Some(order.order.status) {
            eprintln!(
                "[real-chain-e2e] order status update order_id={} status={:?} paid_amount_raw={}",
                order.order.id, order.order.status, order.order.paid_amount_raw
            );
            last_status = Some(order.order.status);
        }
        if order.order.status == OrderStatus::Paid {
            eprintln!(
                "[real-chain-e2e] order reached paid status order_id={} paid_amount_raw={}",
                order.order.id, order.order.paid_amount_raw
            );
            return Ok(());
        }
        if std::time::Instant::now() > deadline {
            return Err(helper_error(format!(
                "timed out waiting for order {order_id} to become paid; last status {:?}",
                order.order.status
            )));
        }
        tokio::time::sleep(StdDuration::from_secs(2)).await;
    }
}

async fn wait_for_collection_confirmed(
    collection_service: &RealChainCollectionService,
    collection_id: Uuid,
    timeout: StdDuration,
) -> Result<CollectionRecord, AnyError> {
    let deadline = std::time::Instant::now() + timeout;
    let mut last_status = None;
    loop {
        let collection = collection_service
            .get_collection(collection_id)
            .await?
            .ok_or_else(|| helper_error(format!("collection disappeared: {collection_id}")))?;
        if last_status != Some(collection.status) {
            eprintln!(
                "[real-chain-e2e] collection status update collection_id={} status={:?} outbound_tx_id={:?}",
                collection.id, collection.status, collection.outbound_tx_id
            );
            last_status = Some(collection.status);
        }
        if collection.status == CollectionRecordStatus::Confirmed {
            eprintln!(
                "[real-chain-e2e] collection reached confirmed status collection_id={} outbound_tx_id={:?}",
                collection.id, collection.outbound_tx_id
            );
            return Ok(collection);
        }
        if collection.status == CollectionRecordStatus::Failed {
            return Err(helper_error(format!(
                "collection {collection_id} entered failed status"
            )));
        }
        if std::time::Instant::now() > deadline {
            return Err(helper_error(format!(
                "timed out waiting for collection {collection_id} to become confirmed; last status {:?}",
                collection.status
            )));
        }
        tokio::time::sleep(StdDuration::from_secs(2)).await;
    }
}

async fn ensure_collection_address_prefunded(
    payer: &Arc<PaymentWallet>,
    rpc_source: &RpcRangeSource,
    chain_id: u64,
    receive_address: EvmAddress,
    collection_fees: CollectionFees,
    nonce_allocator: &NonceAllocator,
    receipt_timeout: StdDuration,
) -> Result<(), AnyError> {
    let required = required_native_gas_for_collection(collection_fees)?;
    let balance = rpc_source.native_balance(chain_id, receive_address).await?;
    if balance >= required {
        eprintln!(
            "[real-chain-e2e] collection address already funded receive_address={} balance={} required={}",
            receive_address, balance, required
        );
        return Ok(());
    }

    let top_up = required
        .checked_sub(balance)
        .ok_or_else(|| helper_error("native balance underflow while computing top-up amount"))?;
    if top_up.is_zero() {
        return Ok(());
    }

    let nonce = nonce_allocator.reserve();
    eprintln!(
        "[real-chain-e2e] funding collection address receive_address={} balance={} required={} top_up={} nonce={}",
        receive_address, balance, required, top_up, nonce
    );
    let tx_hash = send_native_transfer_with_nonce(
        payer,
        rpc_source,
        NativeTransferRequest {
            chain_id,
            recipient: receive_address,
            amount: top_up,
            gas_limit: 21_000,
            max_fee_per_gas: collection_fees.max_fee_per_gas,
            max_priority_fee_per_gas: collection_fees.max_priority_fee_per_gas,
        },
        nonce,
    )
    .await?;
    let receipt = wait_for_successful_receipt(rpc_source, tx_hash, receipt_timeout).await?;
    eprintln!(
        "[real-chain-e2e] funded collection address receive_address={} tx_hash={} block={:?}",
        receive_address, tx_hash, receipt.block
    );

    let after = rpc_source.native_balance(chain_id, receive_address).await?;
    if after < required {
        return Err(helper_error(format!(
            "receive address {receive_address} native balance {after} is still below required {required} after top-up"
        )));
    }

    Ok(())
}

fn required_native_gas_for_collection(
    collection_fees: CollectionFees,
) -> Result<RawAmount, AnyError> {
    let required = collection_fees
        .max_fee_per_gas
        .value()
        .checked_mul(U256::from(collection_fees.gas_limit))
        .ok_or_else(|| {
            helper_error("required native gas overflowed while computing collection top-up")
        })?;
    Ok(RawAmount::new(required))
}

async fn run_real_chain_concurrent_flow(ctx: ConcurrentRealChainContext) -> Result<(), AnyError> {
    let ConcurrentRealChainContext {
        concurrency,
        order_service,
        scanner,
        log_store,
        rpc_source,
        pool,
        order_repo,
        collection_repo,
        outbound_repo,
        audit_repo,
        collection_config,
        signer,
        payer,
        payment_amount,
        total_payment_amount,
        collection_fees,
        stream,
        chain_id,
        token_address,
        treasury_address,
        rpc_url,
        receipt_timeout,
        confirmation_timeout,
        payer_address,
        min_confirmations,
    } = ctx;

    let treasury_before = rpc_source
        .token_balance(token_address, treasury_address)
        .await?;
    eprintln!(
        "[real-chain-e2e] treasury balance before concurrent paid-order run={} total_payment_amount={}",
        treasury_before, total_payment_amount
    );

    let nonce_seed = current_nonce(&rpc_url, payer_address).await?;
    let nonce_allocator = Arc::new(NonceAllocator::new(nonce_seed));
    let scanner_lock = Arc::new(Mutex::new(()));
    let collector_parallelism = concurrency.clamp(1, 4);
    let collector_shutdown = Arc::new(AtomicBool::new(false));
    let mut collector_handles: Vec<tokio::task::JoinHandle<Result<(), AnyError>>> =
        Vec::with_capacity(collector_parallelism);

    for worker_index in 0..collector_parallelism {
        let collector_service = CollectionService::new(
            collection_config.clone(),
            order_repo.clone(),
            collection_repo.clone(),
            outbound_repo.clone(),
            audit_repo.clone(),
            signer.clone(),
            rpc_source.clone(),
            NativeBalanceGasChecker::new(rpc_source.clone()),
        )?;
        let collector = CollectionCollectorWorker::new(
            collector_service,
            outbound_repo.clone(),
            rpc_source.clone(),
            CollectionCollectorConfig::new(format!("real-chain-collector-e2e-{worker_index}"))
                .with_replacement_stuck_after(StdDuration::from_secs(30 * 60)),
        );
        let shutdown = collector_shutdown.clone();
        collector_handles.push(tokio::spawn(async move {
            loop {
                if shutdown.load(Ordering::Relaxed) {
                    break;
                }
                match collector.tick().await {
                    Ok(CollectionCollectorTickOutcome::Broadcast {
                        collection_id,
                        outbound,
                    }) => {
                        eprintln!(
                            "[real-chain-e2e] collector worker={worker_index} broadcast collection_id={} outbound_tx_id={} tx_hash={} nonce={} status={}",
                            collection_id,
                            outbound.id,
                            outbound.tx_hash,
                            outbound.nonce,
                            outbound.status.as_db_str()
                        );
                    }
                    Ok(CollectionCollectorTickOutcome::Confirmed {
                        collection_id,
                        outbound,
                    }) => {
                        eprintln!(
                            "[real-chain-e2e] collector worker={worker_index} confirmed collection_id={} outbound_tx_id={} tx_hash={} status={}",
                            collection_id,
                            outbound.id,
                            outbound.tx_hash,
                            outbound.status.as_db_str()
                        );
                    }
                    Ok(CollectionCollectorTickOutcome::Failed {
                        collection_id,
                        outbound,
                    }) => {
                        eprintln!(
                            "[real-chain-e2e] collector worker={worker_index} failed collection_id={} outbound_tx_id={} tx_hash={} status={}",
                            collection_id,
                            outbound.id,
                            outbound.tx_hash,
                            outbound.status.as_db_str()
                        );
                    }
                    Ok(CollectionCollectorTickOutcome::ReceiptPending { .. })
                    | Ok(CollectionCollectorTickOutcome::NoJob) => {}
                    Err(error) if is_transient_external_error(&error) => {
                        eprintln!(
                            "[real-chain-e2e] collector worker={worker_index} transient tick failure: {error}"
                        );
                    }
                    Err(error) => return Err(Box::new(error) as AnyError),
                }
                tokio::time::sleep(StdDuration::from_secs(1)).await;
            }
            Ok::<(), AnyError>(())
        }));
    }

    let mut flows = JoinSet::new();
    for flow_index in 0..concurrency {
        let order_service = order_service.clone();
        let scanner = scanner.clone();
        let scanner_lock = scanner_lock.clone();
        let log_store = log_store.clone();
        let rpc_source = rpc_source.clone();
        let pool = pool.clone();
        let order_repo = order_repo.clone();
        let collection_repo = collection_repo.clone();
        let outbound_repo = outbound_repo.clone();
        let audit_repo = audit_repo.clone();
        let collection_config = collection_config.clone();
        let signer = signer.clone();
        let payer = payer.clone();
        let nonce_allocator = nonce_allocator.clone();
        let rpc_url = rpc_url.clone();
        flows.spawn(async move {
            let flow_label = format!("flow-{flow_index}");
            let external_id = format!("real-chain-e2e-order-{flow_index}-{}", Uuid::new_v4());
            eprintln!(
                "[real-chain-e2e] {flow_label} creating order external_id={} amount_raw={}",
                external_id, payment_amount
            );
            let order_result = order_service
                .create_order(CreateOrderInput::new(
                    external_id,
                    payment_amount,
                    DEFAULT_ORDER_TTL_SECONDS,
                ))
                .await?;
            assert_eq!(order_result.outcome, CreateOrderServiceOutcome::Created);
            assert_eq!(
                order_result.view.child_account.address,
                order_result.view.order.receive_address
            );
            eprintln!(
                "[real-chain-e2e] {flow_label} order created order_id={} external_id={} status={:?} receive_address={} child_account={} paid_amount_raw={}",
                order_result.view.order.id,
                order_result.view.order.external_id,
                order_result.view.order.status,
                order_result.view.order.receive_address,
                order_result.view.child_account.address,
                order_result.view.order.paid_amount_raw
            );

            let receive_address = order_result.view.order.receive_address;
            if receive_address == treasury_address {
                return Err(helper_error(format!(
                    "flow {flow_label} receive address {receive_address} equals treasury address"
                )));
            }
            let payment_nonce = nonce_allocator.reserve();
            eprintln!(
                "[real-chain-e2e] {flow_label} broadcasting payment payer={} receive_address={} amount_raw={} nonce={}",
                payer_address, receive_address, payment_amount, payment_nonce
            );
            let payment_tx_hash = retry_transient_external(
                "broadcast payment transaction",
                StdDuration::from_secs(30),
                || async {
                    send_payment_with_nonce(
                        &payer,
                        &rpc_source,
                        TokenPaymentRequest {
                            chain_id,
                            token_address,
                            recipient: receive_address,
                            amount: payment_amount,
                            gas_limit: collection_fees.gas_limit,
                            max_fee_per_gas: collection_fees.max_fee_per_gas,
                            max_priority_fee_per_gas: collection_fees.max_priority_fee_per_gas,
                        },
                        payment_nonce,
                    )
                    .await
                },
            )
            .await?;
            eprintln!(
                "[real-chain-e2e] {flow_label} payment tx broadcast tx_hash={}",
                payment_tx_hash
            );
            let payment_receipt =
                wait_for_successful_receipt(&rpc_source, payment_tx_hash, receipt_timeout).await?;
            eprintln!(
                "[real-chain-e2e] {flow_label} payment mined tx_hash={} block={:?} status={:?}",
                payment_tx_hash, payment_receipt.block, payment_receipt.status
            );
            wait_for_confirmations(
                &rpc_source,
                payment_receipt.block,
                min_confirmations,
                confirmation_timeout,
            )
            .await?;
            eprintln!(
                "[real-chain-e2e] {flow_label} payment confirmed tx_hash={} block_number={} min_confirmations={}",
                payment_tx_hash, payment_receipt.block.number, min_confirmations
            );

            eprintln!(
                "[real-chain-e2e] {flow_label} waiting for transfer log store through block {}",
                payment_receipt.block.number
            );
            poll_log_store_until(
                &log_store,
                stream,
                payment_receipt.block.number,
                confirmation_timeout,
            )
            .await?;
            eprintln!(
                "[real-chain-e2e] {flow_label} transfer log store caught up through block {}",
                payment_receipt.block.number
            );
            eprintln!(
                "[real-chain-e2e] {flow_label} scanning until order {} becomes paid",
                order_result.view.order.id
            );
            tick_scanner_until_paid_serialized(
                scanner.as_ref(),
                scanner_lock.as_ref(),
                &order_service,
                order_result.view.order.id,
                confirmation_timeout,
            )
            .await?;
            eprintln!(
                "[real-chain-e2e] {flow_label} order paid order_id={} status=paid paid_amount_raw={}",
                order_result.view.order.id, payment_amount
            );

            let final_order = order_service
                .get_order(order_result.view.order.id)
                .await?
                .ok_or_else(|| {
                    helper_error(format!(
                        "order disappeared: {}",
                        order_result.view.order.id
                    ))
                })?;
            assert_eq!(final_order.order.status, OrderStatus::Paid);
            assert_eq!(final_order.order.paid_amount_raw, payment_amount);
            eprintln!(
                "[real-chain-e2e] {flow_label} final order state order_id={} status={:?} paid_amount_raw={} receive_address={}",
                final_order.order.id,
                final_order.order.status,
                final_order.order.paid_amount_raw,
                final_order.order.receive_address
            );

            ensure_collection_address_prefunded(
                &payer,
                &rpc_source,
                chain_id,
                receive_address,
                collection_fees,
                &nonce_allocator,
                receipt_timeout,
            )
            .await?;
            eprintln!(
                "[real-chain-e2e] {flow_label} syncing collection nonce receive_address={}",
                receive_address
            );
            retry_transient_external("sync collection nonce", StdDuration::from_secs(30), || async {
                sync_account_nonce(&pool, &rpc_url, chain_id, receive_address).await
            })
            .await?;
            eprintln!(
                "[real-chain-e2e] {flow_label} collection nonce synced receive_address={}",
                receive_address
            );

            let collection_service = CollectionService::new(
                collection_config.clone(),
                order_repo.clone(),
                collection_repo.clone(),
                outbound_repo.clone(),
                audit_repo.clone(),
                signer.clone(),
                rpc_source.clone(),
                NativeBalanceGasChecker::new(rpc_source.clone()),
            )?;
            eprintln!(
                "[real-chain-e2e] {flow_label} creating collection order_id={} amount_raw={} treasury={}",
                order_result.view.order.id, payment_amount, treasury_address
            );
            let collection_result = collection_service
                .create_collection(CreateCollectionInput {
                    order_id: order_result.view.order.id,
                    amount: CollectionAmount::Exact(payment_amount),
                    idempotency_key: format!("real-chain-e2e-collect-{flow_index}-{}", Uuid::new_v4()),
                    audit: Default::default(),
                })
                .await?;
            assert_eq!(collection_result.outcome, CreateCollectionOutcome::Created);
            assert_eq!(
                collection_result.collection.status,
                CollectionRecordStatus::Queued
            );
            eprintln!(
                "[real-chain-e2e] {flow_label} collection queued collection_id={} status={:?} outbound_tx_id={:?}; not waiting for async collection confirmation",
                collection_result.collection.id,
                collection_result.collection.status,
                collection_result.collection.outbound_tx_id
            );

            Ok::<ConcurrentFlowResult, AnyError>(ConcurrentFlowResult {
                collection_id: collection_result.collection.id,
            })
        });
    }

    let mut flow_error: Option<AnyError> = None;
    let mut flow_results = Vec::with_capacity(concurrency);
    while let Some(result) = flows.join_next().await {
        match result {
            Ok(Ok(flow_result)) => flow_results.push(flow_result),
            Ok(Err(error)) => {
                flow_error = Some(error);
                break;
            }
            Err(join_error) => {
                flow_error = Some(Box::new(join_error) as AnyError);
                break;
            }
        }
    }

    if let Some(error) = flow_error {
        collector_shutdown.store(true, Ordering::SeqCst);
        flows.abort_all();
        for handle in &collector_handles {
            handle.abort();
        }
        for handle in collector_handles {
            let _ = handle.await;
        }
        return Err(error);
    }

    let collection_status_service = CollectionService::new(
        collection_config.clone(),
        order_repo.clone(),
        collection_repo.clone(),
        outbound_repo.clone(),
        audit_repo.clone(),
        signer.clone(),
        rpc_source.clone(),
        NativeBalanceGasChecker::new(rpc_source.clone()),
    )?;
    let collection_wait_result = async {
        for flow_result in &flow_results {
            let collection_id = flow_result.collection_id;
            let final_collection = wait_for_collection_confirmed(
                &collection_status_service,
                collection_id,
                confirmation_timeout,
            )
            .await?;
            let outbound_tx_id = final_collection.outbound_tx_id.ok_or_else(|| {
                helper_error(format!("collection {collection_id} missing outbound tx id"))
            })?;
            let collect_tx_hash: TxHash =
                sqlx::query_scalar::<_, String>("SELECT tx_hash FROM outbound_transactions WHERE id = $1")
                    .bind(outbound_tx_id)
                    .fetch_one(&pool)
                    .await?
                    .parse()?;
            eprintln!(
                "[real-chain-e2e] async collection confirmed collection_id={} outbound_tx_id={} tx_hash={}",
                collection_id, outbound_tx_id, collect_tx_hash
            );
        }
        Ok::<(), AnyError>(())
    }
    .await;

    collector_shutdown.store(true, Ordering::SeqCst);
    let mut collector_error: Option<AnyError> = None;
    for handle in collector_handles {
        match handle.await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                collector_error = Some(error);
                break;
            }
            Err(join_error) => {
                collector_error = Some(Box::new(join_error));
                break;
            }
        }
    }
    collection_wait_result?;
    if let Some(error) = collector_error {
        return Err(error);
    }

    let treasury_after = rpc_source
        .token_balance(token_address, treasury_address)
        .await?;
    let expected_treasury_delta = if payer_address == treasury_address {
        RawAmount::ZERO
    } else {
        total_payment_amount
    };
    assert_eq!(
        treasury_after.checked_sub(treasury_before),
        Some(expected_treasury_delta),
        "treasury token delta should match net payment and async collection movement"
    );
    eprintln!(
        "[real-chain-e2e] treasury balance after async collections before={} after={} delta={}",
        treasury_before,
        treasury_after,
        treasury_after
            .checked_sub(treasury_before)
            .unwrap_or(RawAmount::ZERO)
    );

    Ok(())
}

async fn retry_transient_external<T, F, Fut>(
    label: &str,
    timeout: StdDuration,
    mut op: F,
) -> Result<T, AnyError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, AnyError>>,
{
    let deadline = std::time::Instant::now() + timeout;
    let mut last_transient_error = None;
    loop {
        match op().await {
            Ok(value) => return Ok(value),
            Err(error) if is_transient_external_error(error.as_ref()) => {
                if std::time::Instant::now() > deadline {
                    let suffix = last_transient_error
                        .as_ref()
                        .map(|error| format!("; last transient error: {error}"))
                        .unwrap_or_default();
                    return Err(helper_error(format!(
                        "{label} timed out after transient errors{suffix}"
                    )));
                }
                last_transient_error = Some(error.to_string());
                eprintln!("{label} failed transiently: {error}; retrying");
                tokio::time::sleep(StdDuration::from_secs(2)).await;
            }
            Err(error) => return Err(error),
        }
    }
}

async fn prepare_temp_schema_pool(
    database_url: &str,
    prefix: &str,
    max_connections: u32,
) -> Result<(PgPool, String), AnyError> {
    let mut conn = PgConnection::connect(database_url).await?;
    let schema = temp_schema_name(prefix)?;
    let schema_ident = quote_ident(&schema);
    conn.execute(format!("CREATE SCHEMA {schema_ident}").as_str())
        .await?;

    let search_path_sql = format!("SET search_path TO {schema_ident}");
    let pool = PgPoolOptions::new()
        .max_connections(max_connections.max(1))
        .after_connect({
            let search_path_sql = search_path_sql.clone();
            move |conn, _meta| {
                let search_path_sql = search_path_sql.clone();
                Box::pin(async move {
                    conn.execute(search_path_sql.as_str()).await?;
                    Ok::<(), sqlx::Error>(())
                })
            }
        })
        .connect(database_url)
        .await?;

    Ok((pool, schema))
}

async fn drop_schema(database_url: &str, schema_ident: &str) -> Result<(), AnyError> {
    let mut conn = PgConnection::connect(database_url).await?;
    conn.execute(format!("DROP SCHEMA {schema_ident} CASCADE").as_str())
        .await?;
    Ok(())
}

async fn set_wallet_cursor_address_index(pool: &PgPool, index: u32) -> Result<(), AnyError> {
    sqlx::query(
        r#"
        UPDATE wallet_cursors
        SET next_address_index = $1,
            updated_at = now()
        WHERE id = 'default'
        "#,
    )
    .bind(i64::from(index))
    .execute(pool)
    .await?;
    Ok(())
}

async fn sync_account_nonce(
    pool: &PgPool,
    rpc_url: &str,
    chain_id: u64,
    address: EvmAddress,
) -> Result<(), AnyError> {
    let nonce = current_nonce(rpc_url, address).await?;
    sqlx::query(
        r#"
        INSERT INTO account_nonces (
            chain_id,
            address,
            next_nonce
        )
        VALUES ($1, $2, $3)
        ON CONFLICT (chain_id, address) DO UPDATE
        SET next_nonce = EXCLUDED.next_nonce,
            updated_at = now()
        "#,
    )
    .bind(i64::try_from(chain_id)?)
    .bind(address.to_lower_hex())
    .bind(sqlx::types::BigDecimal::from(nonce))
    .execute(pool)
    .await?;
    Ok(())
}

async fn current_nonce(rpc_url: &str, address: EvmAddress) -> Result<u64, AnyError> {
    let payload = rpc_request(
        rpc_url,
        "eth_getTransactionCount",
        json!([address.to_string(), "latest"]),
    )
    .await?;
    let nonce = payload
        .get("result")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            helper_error(format!("eth_getTransactionCount missing result: {payload}"))
        })?;
    parse_hex_u64(nonce)
}

async fn rpc_request(rpc_url: &str, method: &str, params: Value) -> Result<Value, AnyError> {
    let client = reqwest::Client::new();
    let response = client
        .post(rpc_url)
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        }))
        .send()
        .await
        .map_err(|error| helper_error(format!("{method} request failed: {error}")))?;
    if !response.status().is_success() {
        return Err(helper_error(format!(
            "{method} returned HTTP {}",
            response.status()
        )));
    }

    let payload = response
        .json::<Value>()
        .await
        .map_err(|error| helper_error(format!("{method} json: {error}")))?;
    if let Some(error) = payload.get("error") {
        return Err(helper_error(format!(
            "{method} returned RPC error: {error}"
        )));
    }
    Ok(payload)
}

fn parse_hex_u64(value: &str) -> Result<u64, AnyError> {
    let hex = value
        .trim()
        .strip_prefix("0x")
        .or_else(|| value.trim().strip_prefix("0X"))
        .ok_or_else(|| helper_error(format!("hex quantity must start with 0x: {value}")))?;
    if hex.is_empty() {
        return Ok(0);
    }
    u64::from_str_radix(hex, 16)
        .map_err(|error| helper_error(format!("invalid hex quantity {value}: {error}")))
}

fn temp_schema_name(prefix: &str) -> Result<String, AnyError> {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    Ok(format!("{prefix}_{}_{}", process::id(), nanos))
}

fn quote_ident(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

#[derive(Debug)]
struct HelperError(String);

impl fmt::Display for HelperError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Error for HelperError {}

fn helper_error(message: impl Into<String>) -> AnyError {
    Box::new(HelperError(message.into()))
}

fn is_transient_external_error(error: &(dyn Error + 'static)) -> bool {
    let text = error.to_string().to_ascii_lowercase();
    [
        "chain rpc unavailable",
        "rpc unavailable",
        "request failed",
        "returned http 429",
        "returned http 502",
        "returned http 503",
        "returned http 504",
        "timed out",
        "timeout",
        "temporarily unavailable",
        "connection refused",
        "connection reset",
        "broken pipe",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}
