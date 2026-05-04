mod support;

use std::{
    process,
    time::{Duration as StdDuration, SystemTime, UNIX_EPOCH},
};

use pay3::{
    chain::{ChainHeaderReader, Erc20ChainClient, RpcRangeSource},
    db::{
        migrations::{RuntimeSeedConfig, run_schema_migrations, seed_runtime_config},
        repositories::{
            CollectionRecordStatus, PgAuditRepository, PgCollectionRepository, PgOrderRepository,
            PgOutboundRepository, PgPaymentRepository,
        },
    },
    domain::{CollectionFees, OrderStatus, RawAmount, TxHash},
    services::{
        collections::{
            AssumePrefundedGas, CollectionService, CollectionServiceConfig, CreateCollectionInput,
            CreateCollectionOutcome,
        },
        orders::{
            CreateOrderInput, CreateOrderServiceOutcome, OrderService, OrderServiceConfig,
            SystemClock,
        },
        payment_windows::RepositoryPaymentWindowLookup,
        payments::{PaymentMatcher, PaymentMatchingConfig},
    },
    transfer_log_store::{
        LogSourceKind, PollOutcome, RedbTransferLogIngestor, ScanTargetMode, StreamId,
        TransferLogIngestor, TransferLogStreamConfig,
    },
    wallet::HdWallet,
    workers::{
        collector::{
            CollectionCollectorConfig, CollectionCollectorTickOutcome, CollectionCollectorWorker,
        },
        scanner::{PaymentScannerConfig, PaymentScannerTickOutcome, PaymentScannerWorker},
    },
};
use sqlx::{Connection, Executor, PgConnection, PgPool, postgres::PgPoolOptions};
use tempfile::TempDir;
use time::Duration as TimeDuration;
use uuid::Uuid;

use support::anvil::{
    AnvilHarness, AnvilMnemonicDeriver, AnvilMnemonicSigner, AnyError, CHILD_PATH,
    DEFAULT_ANVIL_MNEMONIC, DEPLOYER_PATH, TREASURY_PATH, deploy_mock_erc20, send_erc20_transfer,
};

#[tokio::test]
async fn anvil_mock_erc20_end_to_end_flow() -> Result<(), AnyError> {
    let Some(database_url) = test_database_url() else {
        eprintln!("skipping anvil e2e test; set PAY3_TEST_DATABASE_URL or TEST_DATABASE_URL");
        return Ok(());
    };

    let anvil = AnvilHarness::start().await?;
    let chain_id = anvil.chain_id();
    let rpc_url = anvil.rpc_url().to_string();
    let rpc_source = RpcRangeSource::from_http_urls(chain_id, &[rpc_url.clone()], 1)?;

    let child_address = anvil.derive_address(CHILD_PATH).await?;
    let deployer_address = anvil.derive_address(DEPLOYER_PATH).await?;
    let treasury_address = anvil.derive_address(TREASURY_PATH).await?;

    let initial_supply = RawAmount::from(1_000_000u64);
    let payment_amount = RawAmount::from(12_345u64);
    let token_address = deploy_mock_erc20(
        anvil.rpc_url(),
        deployer_address,
        deployer_address,
        initial_supply,
    )
    .await?;

    let latest_head = rpc_source.latest_head().await?;
    let start_block = latest_head
        .number
        .checked_add(1)
        .ok_or_else(|| helper_error("latest head overflowed when computing start block"))?;

    let (pool, schema) = prepare_temp_schema_pool(&database_url, "pay3_anvil_e2e").await?;
    let schema_ident = quote_ident(&schema);

    let result = async {
        let stream = StreamId::new(chain_id, token_address);
        let order_repo = PgOrderRepository::new(pool.clone());
        let collection_repo = PgCollectionRepository::new(pool.clone());
        let outbound_repo = PgOutboundRepository::new(pool.clone());
        let audit_repo = PgAuditRepository::new(pool.clone());
        let payment_repo = PgPaymentRepository::new(pool.clone());

        run_schema_migrations(&pool).await?;
        seed_runtime_config(
            &pool,
            &RuntimeSeedConfig {
                signer_key_ref: "pay3-master".to_string(),
                chain_id,
                token_address,
                treasury_address,
                start_block,
            },
        )
        .await?;

        let kvdb_dir = TempDir::new()?;
        let kvdb_path = kvdb_dir.path().join("transfer-log.redb");
        let log_store = RedbTransferLogIngestor::open(rpc_source.clone(), &kvdb_path)?;
        let stream_config = TransferLogStreamConfig {
            chain_id,
            token_address,
            start_block,
            poll_interval_ms: 250,
            batch_size_blocks: 1,
            max_batch_size_blocks: 32,
            max_logs_per_page: 100,
            max_unique_to_addresses_per_batch: 100,
            max_db_fallback_addresses: 100,
            capacity_probe_blocks: 1,
            reorg_lookback_blocks: 1,
            target_mode: ScanTargetMode::LatestMinusConfirmations(0),
            rpc_max_retries: 3,
            log_source: LogSourceKind::RpcRange,
        };
        log_store.ensure_stream(stream_config.clone()).await?;

        let order_service = OrderService::new(
            OrderServiceConfig::new(chain_id, token_address, 24 * 60 * 60),
            order_repo.clone(),
            HdWallet::new(AnvilMnemonicDeriver::new(DEFAULT_ANVIL_MNEMONIC)?),
            rpc_source.clone(),
        )?;

        let payment_matcher = PaymentMatcher::new(
            log_store.clone(),
            RepositoryPaymentWindowLookup::new(order_repo.clone(), 100),
            rpc_source.clone(),
            PaymentMatchingConfig {
                stream,
                min_confirmations: 0,
                page_limit: 100,
                max_unique_to_addresses_per_batch: 100,
            },
        );
        let scanner = PaymentScannerWorker::new(
            payment_repo,
            payment_matcher,
            log_store.clone(),
            rpc_source.clone(),
            SystemClock,
            PaymentScannerConfig::new("scanner-e2e", stream, TimeDuration::seconds(30))
                .with_confirmation_sweep_limit(100),
        );

        let collection_service = CollectionService::new(
            CollectionServiceConfig::new(
                chain_id,
                token_address,
                treasury_address,
                CollectionFees::new(
                    120_000,
                    RawAmount::from(10_000_000_000u64),
                    RawAmount::from(1_000_000_000u64),
                ),
            ),
            order_repo.clone(),
            collection_repo.clone(),
            outbound_repo.clone(),
            audit_repo.clone(),
            AnvilMnemonicSigner::new(DEFAULT_ANVIL_MNEMONIC, chain_id)?,
            rpc_source.clone(),
            AssumePrefundedGas,
        )?;
        let collector_collection_service = CollectionService::new(
            CollectionServiceConfig::new(
                chain_id,
                token_address,
                treasury_address,
                CollectionFees::new(
                    120_000,
                    RawAmount::from(10_000_000_000u64),
                    RawAmount::from(1_000_000_000u64),
                ),
            ),
            order_repo.clone(),
            collection_repo,
            outbound_repo.clone(),
            audit_repo,
            AnvilMnemonicSigner::new(DEFAULT_ANVIL_MNEMONIC, chain_id)?,
            rpc_source.clone(),
            AssumePrefundedGas,
        )?;
        let collector = CollectionCollectorWorker::new(
            collector_collection_service,
            outbound_repo,
            rpc_source.clone(),
            CollectionCollectorConfig::new("collector-e2e")
                .with_replacement_stuck_after(StdDuration::from_secs(3600)),
        );

        let order_external_id = format!("anvil-e2e-order-{}", Uuid::new_v4());
        let order_result = order_service
            .create_order(CreateOrderInput::new(
                order_external_id,
                payment_amount,
                3_600,
            ))
            .await?;
        assert_eq!(order_result.outcome, CreateOrderServiceOutcome::Created);
        assert_eq!(order_result.view.order.receive_address, child_address);
        assert_eq!(order_result.view.child_account.address, child_address);

        let payment_tx_hash = send_erc20_transfer(
            anvil.rpc_url(),
            anvil.mnemonic(),
            DEPLOYER_PATH,
            token_address,
            child_address,
            payment_amount,
            100_000,
            RawAmount::from(10_000_000_000u64),
        )
        .await?;
        wait_for_receipt(&rpc_source, payment_tx_hash).await?;

        let poll_outcome = log_store.poll_once(stream).await?;
        match poll_outcome {
            PollOutcome::Advanced {
                stream: actual_stream,
                log_count,
                ..
            } => {
                assert_eq!(actual_stream, stream);
                assert_eq!(log_count, 1);
            }
            other => panic!("expected advanced poll outcome, got {other:?}"),
        }

        let scan_outcome = scanner.tick().await?;
        match scan_outcome {
            PaymentScannerTickOutcome::Committed {
                stream: actual_stream,
                matched_payments,
                ..
            } => {
                assert_eq!(actual_stream, stream);
                assert_eq!(matched_payments, 1);
            }
            other => panic!("expected committed scan outcome, got {other:?}"),
        }

        let paid_order = order_service
            .get_order(order_result.view.order.id)
            .await?
            .expect("order must be readable after scanner commit");
        assert_eq!(paid_order.order.status, OrderStatus::Paid);
        assert_eq!(paid_order.order.paid_amount_raw, payment_amount);
        assert_eq!(paid_order.order.receive_address, child_address);

        let collection_result = collection_service
            .create_collection(CreateCollectionInput::max(
                paid_order.order.id,
                format!("anvil-e2e-collect-{}", Uuid::new_v4()),
            ))
            .await?;
        assert_eq!(collection_result.outcome, CreateCollectionOutcome::Created);
        assert_eq!(
            collection_result.collection.status,
            CollectionRecordStatus::Queued
        );

        let first_tick = collector.tick().await?;
        let collection_id = collection_result.collection.id;
        let first_outbound_hash = match first_tick {
            CollectionCollectorTickOutcome::Broadcast {
                collection_id: actual_collection_id,
                outbound,
            } => {
                assert_eq!(actual_collection_id, collection_id);
                outbound.tx_hash
            }
            other => panic!("expected broadcast collection tick, got {other:?}"),
        };

        wait_for_receipt(&rpc_source, first_outbound_hash).await?;

        let confirming_collection = collection_service
            .get_collection(collection_id)
            .await?
            .expect("collection must be readable after broadcast");
        assert_eq!(
            confirming_collection.status,
            CollectionRecordStatus::Confirming
        );

        let second_tick = collector.tick().await?;
        match second_tick {
            CollectionCollectorTickOutcome::Confirmed {
                collection_id: actual_collection_id,
                outbound,
            } => {
                assert_eq!(actual_collection_id, collection_id);
                assert_eq!(outbound.status.as_db_str(), "confirmed");
            }
            other => panic!("expected confirmed collection tick, got {other:?}"),
        }

        let final_collection = collection_service
            .get_collection(collection_id)
            .await?
            .expect("collection must be readable after confirmation");
        assert_eq!(final_collection.status, CollectionRecordStatus::Confirmed);
        assert_eq!(final_collection.outbound_tx_id.is_some(), true);

        let outbound_tx_id = final_collection
            .outbound_tx_id
            .ok_or_else(|| helper_error("collection did not persist outbound tx id"))?;
        let outbound_status: String =
            sqlx::query_scalar("SELECT status FROM outbound_transactions WHERE id = $1")
                .bind(outbound_tx_id)
                .fetch_one(&pool)
                .await?;
        assert_eq!(outbound_status, "confirmed");

        let treasury_balance = rpc_source
            .token_balance(token_address, treasury_address)
            .await?;
        assert_eq!(treasury_balance, payment_amount);

        Ok::<(), AnyError>(())
    }
    .await;

    pool.close().await;
    drop_schema(&database_url, &schema_ident).await?;
    result
}

async fn prepare_temp_schema_pool(
    database_url: &str,
    prefix: &str,
) -> Result<(PgPool, String), AnyError> {
    let mut conn = PgConnection::connect(database_url).await?;
    let schema = temp_schema_name(prefix)?;
    let schema_ident = quote_ident(&schema);
    conn.execute(format!("CREATE SCHEMA {schema_ident}").as_str())
        .await?;

    let search_path_sql = format!("SET search_path TO {schema_ident}");
    let pool = PgPoolOptions::new()
        .max_connections(4)
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

async fn wait_for_receipt<C>(client: &C, tx_hash: TxHash) -> Result<(), AnyError>
where
    C: Erc20ChainClient + ?Sized,
{
    let deadline = std::time::Instant::now() + StdDuration::from_secs(30);

    loop {
        if std::time::Instant::now() > deadline {
            return Err(helper_error(format!(
                "timed out waiting for receipt of {tx_hash}"
            )));
        }

        if client.transaction_receipt(tx_hash).await?.is_some() {
            return Ok(());
        }

        tokio::time::sleep(StdDuration::from_millis(100)).await;
    }
}

fn test_database_url() -> Option<String> {
    std::env::var("PAY3_TEST_DATABASE_URL")
        .ok()
        .or_else(|| std::env::var("TEST_DATABASE_URL").ok())
}

fn temp_schema_name(prefix: &str) -> Result<String, AnyError> {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    Ok(format!("{prefix}_{}_{}", process::id(), nanos))
}

fn quote_ident(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

fn helper_error(message: impl Into<String>) -> AnyError {
    Box::new(std::io::Error::other(message.into()))
}
