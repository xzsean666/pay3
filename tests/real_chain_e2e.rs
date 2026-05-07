use std::{
    collections::BTreeMap,
    env,
    error::Error,
    fmt, fs,
    path::Path,
    process,
    time::{Duration as StdDuration, SystemTime, UNIX_EPOCH},
};

use alloy_consensus::{SignableTransaction, TxEip1559};
use alloy_primitives::TxKind;
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
            CollectionRecordStatus, PgAuditRepository, PgCollectionRepository, PgOrderRepository,
            PgOutboundRepository, PgPaymentRepository,
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
use uuid::Uuid;

type AnyError = Box<dyn Error + Send + Sync>;

const DEFAULT_ENV_FILE: &str = ".env.test";
const DEFAULT_PAYER_DERIVATION_PATH: &str = "m/44'/60'/0'/0/0";
const DEFAULT_ORDER_TTL_SECONDS: u64 = 3_600;
const DEFAULT_BATCH_SIZE_BLOCKS: u64 = 100;
const DEFAULT_RECEIPT_TIMEOUT_SECS: u64 = 180;
const DEFAULT_CONFIRMATION_TIMEOUT_SECS: u64 = 360;
const DEFAULT_COLLECTION_GAS_LIMIT: u64 = 120_000;
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
    let payer = PaymentWallet::from_env(&env_values, &config)?;
    let payment_amount =
        payment_amount_from_env_or_random(&env_values, config.chain.token_decimals)?;
    eprintln!(
        "real chain e2e payment amount raw: {payment_amount} (token decimals: {})",
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
    rpc_source.manager().validate_chain_ids().await?;

    let payer_address = payer.address().await?;
    ensure_token_balance(
        &rpc_source,
        config.chain.token_address,
        payer_address,
        payment_amount,
        "payer",
    )
    .await?;
    ensure_native_balance_nonzero(&rpc_source, config.chain.chain_id, payer_address, "payer")
        .await?;

    let latest_head = rpc_source.latest_head().await?;
    let start_block = latest_head
        .number
        .checked_add(1)
        .ok_or_else(|| helper_error("latest head overflowed when computing start block"))?;

    let (pool, schema) =
        prepare_temp_schema_pool(&config.database.url, "pay3_real_chain_e2e").await?;
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
        let scanner = PaymentScannerWorker::new(
            payment_repo,
            payment_matcher,
            log_store.clone(),
            rpc_source.clone(),
            SystemClock,
            PaymentScannerConfig::new("real-chain-scanner-e2e", stream, TimeDuration::seconds(60))
                .with_confirmation_sweep_limit(1_000),
        );

        let gas_limit = env_values
            .optional("PAY3_E2E_COLLECTION_GAS_LIMIT")
            .unwrap_or("120000")
            .parse::<u64>()?
            .max(DEFAULT_COLLECTION_GAS_LIMIT);
        let fee_estimate = rpc_source.estimate_eip1559_fees().await?;
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
            collection_repo,
            outbound_repo.clone(),
            audit_repo,
            signer.clone(),
            rpc_source.clone(),
            NativeBalanceGasChecker::new(rpc_source.clone()),
        )?;
        let collector = CollectionCollectorWorker::new(
            collector_collection_service,
            outbound_repo,
            rpc_source.clone(),
            CollectionCollectorConfig::new("real-chain-collector-e2e")
                .with_replacement_stuck_after(StdDuration::from_secs(30 * 60)),
        );

        let order_external_id = format!("real-chain-e2e-order-{}", Uuid::new_v4());
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

        let payment_tx_hash = send_payment(
            &payer,
            &rpc_source,
            &config.chain.rpc_http_urls[0],
            config.chain.chain_id,
            config.chain.token_address,
            receive_address,
            payment_amount,
            collection_fees.gas_limit,
            collection_fees.max_fee_per_gas,
            collection_fees.max_priority_fee_per_gas,
        )
        .await?;
        let payment_receipt =
            wait_for_successful_receipt(&rpc_source, payment_tx_hash, receipt_timeout).await?;
        wait_for_confirmations(
            &rpc_source,
            payment_receipt.block,
            config.chain.min_confirmations,
            confirmation_timeout,
        )
        .await?;

        poll_log_store_until(
            &log_store,
            stream,
            payment_receipt.block.number,
            confirmation_timeout,
        )
        .await?;
        tick_scanner_until_paid(
            &scanner,
            &order_service,
            order_result.view.order.id,
            confirmation_timeout,
        )
        .await?;

        sync_account_nonce(
            &pool,
            &config.chain.rpc_http_urls[0],
            config.chain.chain_id,
            receive_address,
        )
        .await?;

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

        let collection_id = collection_result.collection.id;
        let collect_tx_hash = match collector.tick().await? {
            CollectionCollectorTickOutcome::Broadcast {
                collection_id: actual_collection_id,
                outbound,
            } => {
                assert_eq!(actual_collection_id, collection_id);
                outbound.tx_hash
            }
            other => panic!("expected broadcast collection tick, got {other:?}"),
        };

        wait_for_successful_receipt(&rpc_source, collect_tx_hash, receipt_timeout).await?;
        match collector.tick().await? {
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

        let paid_order = order_service
            .get_order(order_result.view.order.id)
            .await?
            .expect("order must remain readable");
        assert_eq!(paid_order.order.status, OrderStatus::Paid);
        assert_eq!(paid_order.order.paid_amount_raw, payment_amount);

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

async fn send_payment(
    payer: &PaymentWallet,
    rpc_source: &RpcRangeSource,
    rpc_url: &str,
    chain_id: u64,
    token_address: EvmAddress,
    recipient: EvmAddress,
    amount: RawAmount,
    gas_limit: u64,
    max_fee_per_gas: RawAmount,
    max_priority_fee_per_gas: RawAmount,
) -> Result<TxHash, AnyError> {
    let payer_address = payer.address().await?;
    let nonce = current_nonce(rpc_url, payer_address).await?;
    let unsigned = UnsignedTx::new(
        format!("real-chain-e2e-payment-{}", Uuid::new_v4()),
        chain_id,
        nonce,
        token_address,
        RawAmount::ZERO,
        gas_limit,
        max_fee_per_gas,
        max_priority_fee_per_gas,
        erc20_transfer_data(recipient, amount),
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
    loop {
        if std::time::Instant::now() > deadline {
            return Err(helper_error(format!(
                "timed out waiting for receipt of {tx_hash}"
            )));
        }
        if let Some(receipt) = rpc_source.transaction_receipt(tx_hash).await? {
            if receipt.status != TransactionStatus::Success {
                return Err(helper_error(format!("transaction {tx_hash} reverted")));
            }
            return Ok(receipt);
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
    loop {
        let head = rpc_source.latest_head().await?;
        if block.has_confirmations(head, min_confirmations) {
            return Ok(());
        }
        if std::time::Instant::now() > deadline {
            return Err(helper_error(format!(
                "timed out waiting for {min_confirmations} confirmations for block {}",
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
    loop {
        let cursor = log_store.cursor(stream).await?;
        if cursor
            .last_completed_block
            .is_some_and(|completed| completed >= target_block)
        {
            return Ok(());
        }
        match log_store.poll_once(stream).await? {
            PollOutcome::Advanced { .. } | PollOutcome::Rewound { .. } => {}
            PollOutcome::Idle { .. } => tokio::time::sleep(StdDuration::from_secs(2)).await,
        }
        if std::time::Instant::now() > deadline {
            return Err(helper_error(format!(
                "timed out waiting for transfer log store to cover block {target_block}"
            )));
        }
    }
}

async fn tick_scanner_until_paid<D, H>(
    scanner: &PaymentScannerWorker<
        PgPaymentRepository,
        PaymentMatcher<
            RedbTransferLogIngestor<RpcRangeSource>,
            RepositoryPaymentWindowLookup<PgOrderRepository>,
            RpcRangeSource,
        >,
        RedbTransferLogIngestor<RpcRangeSource>,
        RpcRangeSource,
        SystemClock,
    >,
    order_service: &OrderService<PgOrderRepository, D, H>,
    order_id: Uuid,
    timeout: StdDuration,
) -> Result<(), AnyError>
where
    D: pay3::wallet::AddressDeriver,
    H: pay3::services::orders::OrderChainHeadReader,
{
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match scanner.tick().await? {
            PaymentScannerTickOutcome::Committed { .. }
            | PaymentScannerTickOutcome::ConfirmationsSwept { .. }
            | PaymentScannerTickOutcome::Idle { .. }
            | PaymentScannerTickOutcome::PageIncomplete { .. }
            | PaymentScannerTickOutcome::LeaseHeld { .. }
            | PaymentScannerTickOutcome::KvReorgHandled { .. } => {}
        }
        let order = order_service
            .get_order(order_id)
            .await?
            .ok_or_else(|| helper_error(format!("order disappeared: {order_id}")))?;
        if order.order.status == OrderStatus::Paid {
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
