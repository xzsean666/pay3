use std::{
    collections::BTreeMap,
    env,
    error::Error,
    fmt, fs,
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use alloy_consensus::{SignableTransaction, TxEip1559};
use alloy_primitives::{TxKind, U256};
use alloy_signer::SignerSync;
use alloy_signer_local::PrivateKeySigner;
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use pay3::{
    auth::{Audience, Claims},
    chain::{
        ChainHeaderReader, Eip1559FeeEstimator, Erc20ChainClient, NativeBalanceReader,
        PendingNonceReader, RpcRangeSource, TransactionStatus,
    },
    domain::{EvmAddress, RawAmount, TxHash},
    signer::{LocalMnemonicSigner, SignedTx, SignerProvider, UnsignedTx},
};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use uuid::Uuid;

type AnyError = Box<dyn Error + Send + Sync>;

const DEFAULT_ENV_FILE: &str = ".env.test";
const DEFAULT_ORDER_TTL_SECONDS: u64 = 3_600;
const DEFAULT_PAYMENT_AMOUNT_RAW: &str = "1";
const DEFAULT_PAYMENT_GAS_LIMIT: u64 = 120_000;
const DEFAULT_NATIVE_TRANSFER_GAS_LIMIT: u64 = 21_000;
const DEFAULT_RECEIPT_TIMEOUT_SECS: u64 = 180;
const DEFAULT_ORDER_PAID_TIMEOUT_SECS: u64 = 360;
const DEFAULT_COLLECTION_TIMEOUT_SECS: u64 = 600;
const DEFAULT_POLL_INTERVAL_SECS: u64 = 5;
const DEFAULT_PAYER_DERIVATION_PATH: &str = "m/44'/60'/0'/0/0";

#[tokio::test]
#[ignore = "hits deployed API and may spend testnet gas/token; run with PAY3_RUN_DEPLOYED_API_ACCEPTANCE=1"]
async fn deployed_api_order_payment_collection_flow() -> Result<(), AnyError> {
    if env::var("PAY3_RUN_DEPLOYED_API_ACCEPTANCE").ok().as_deref() != Some("1") {
        eprintln!("skipping deployed API acceptance test; set PAY3_RUN_DEPLOYED_API_ACCEPTANCE=1");
        return Ok(());
    }

    let env_file = env::var("PAY3_DEPLOYED_API_ENV_FILE")
        .or_else(|_| env::var("PAY3_API_ENV_FILE"))
        .or_else(|_| env::var("PAY3_E2E_ENV_FILE"))
        .unwrap_or_else(|_| DEFAULT_ENV_FILE.to_string());
    let values = EnvValues::load(&env_file)?;
    let settings = TestSettings::from_env(&values)?;
    let api = ApiClient::new(settings.api_base_url.clone(), settings.jwt.clone())?;

    eprintln!(
        "[deployed-api] testing {} with external_id={}",
        settings.api_base_url, settings.external_id
    );

    assert_healthz(&api).await?;
    assert_readyz(&api).await?;
    assert_metrics(&api).await?;
    assert_v1_requires_auth(&api).await?;

    let order_body = json!({
        "external_id": settings.external_id,
        "amount": settings.amount,
        "ttl_seconds": settings.order_ttl_seconds,
        "metadata": {
            "source": "deployed_api_acceptance",
            "run_id": settings.run_id.to_string()
        }
    });

    let created = api
        .json(Method::POST, "/v1/orders", Some(order_body.clone()), true)
        .await?;
    assert_status(&created, StatusCode::CREATED, "create order")?;
    let order_id = json_str(&created.body, "id")?.to_string();
    let receive_address = json_str(&created.body, "payment.receive_address")?.parse()?;
    let chain_id = json_u64(&created.body, "payment.chain_id")?;
    let token_address = json_str(&created.body, "payment.token_address")?.parse()?;
    assert_eq_json_string(
        &created.body,
        "payment.amount_raw",
        &settings.payment_amount_raw.to_string(),
    )?;
    eprintln!(
        "[deployed-api] order created order_id={} receive_address={} amount_raw={}",
        order_id, receive_address, settings.payment_amount_raw
    );

    let idempotent = api
        .json(Method::POST, "/v1/orders", Some(order_body), true)
        .await?;
    assert_status(&idempotent, StatusCode::OK, "idempotent create order")?;
    assert_eq_json_string(&idempotent.body, "id", &order_id)?;

    let by_id = api
        .json(Method::GET, &format!("/v1/orders/{order_id}"), None, true)
        .await?;
    assert_status(&by_id, StatusCode::OK, "get order by id")?;
    assert_eq_json_string(&by_id.body, "id", &order_id)?;

    let by_external_id = api
        .json(
            Method::GET,
            &format!(
                "/v1/orders/by-external-id/{}",
                encode_path_segment(&settings.external_id)
            ),
            None,
            true,
        )
        .await?;
    assert_status(&by_external_id, StatusCode::OK, "get order by external id")?;
    assert_eq_json_string(&by_external_id.body, "id", &order_id)?;

    if settings.skip_chain_payment {
        eprintln!(
            "[deployed-api] skipping chain payment and collection because PAY3_API_TEST_SKIP_CHAIN_PAYMENT is enabled"
        );
        return Ok(());
    }

    let rpc_source = RpcRangeSource::from_http_urls(chain_id, &settings.rpc_http_urls, 1)?;
    rpc_source.manager().validate_chain_ids().await?;
    let payer = PaymentWallet::from_env(&values)?;
    let payer_address = payer.address().await?;
    eprintln!("[deployed-api] payer address={payer_address}");

    ensure_token_balance(
        &rpc_source,
        token_address,
        payer_address,
        settings.payment_amount_raw,
        "payer",
    )
    .await?;
    ensure_native_balance_nonzero(&rpc_source, chain_id, payer_address, "payer").await?;

    let fee_estimate = rpc_source.estimate_eip1559_fees().await?;
    let payment_tx_hash = send_token_transfer(
        &payer,
        &rpc_source,
        TokenTransferRequest {
            chain_id,
            token_address,
            recipient: receive_address,
            amount: settings.payment_amount_raw,
            gas_limit: settings.payment_gas_limit,
            max_fee_per_gas: fee_estimate.max_fee_per_gas,
            max_priority_fee_per_gas: fee_estimate.max_priority_fee_per_gas,
        },
    )
    .await?;
    eprintln!(
        "[deployed-api] payment tx broadcast tx_hash={} receive_address={}",
        payment_tx_hash, receive_address
    );

    let payment_receipt =
        wait_for_successful_receipt(&rpc_source, payment_tx_hash, settings.receipt_timeout).await?;
    wait_for_confirmations(
        &rpc_source,
        payment_receipt.block,
        settings.min_confirmations,
        settings.receipt_timeout,
    )
    .await?;

    let paid_order = wait_for_order_paid(
        &api,
        &order_id,
        settings.order_paid_timeout,
        settings.poll_interval,
    )
    .await?;
    assert_eq_json_string(
        &paid_order,
        "payment.paid_amount_raw",
        &settings.payment_amount_raw.to_string(),
    )?;
    eprintln!("[deployed-api] order reached paid status order_id={order_id}");

    if settings.skip_collection {
        eprintln!(
            "[deployed-api] skipping collection because PAY3_API_TEST_SKIP_COLLECTION is enabled"
        );
        return Ok(());
    }

    ensure_collection_native_balance(
        &payer,
        &rpc_source,
        CollectionTopUpRequest {
            chain_id,
            receive_address,
            max_fee_per_gas: fee_estimate.max_fee_per_gas,
            max_priority_fee_per_gas: fee_estimate.max_priority_fee_per_gas,
            collection_gas_limit: settings.collection_gas_limit,
            receipt_timeout: settings.receipt_timeout,
        },
    )
    .await?;

    let collection_body = json!({
        "order_id": order_id,
        "amount": "max",
        "idempotency_key": settings.collection_idempotency_key,
    });
    let collection = api
        .json(
            Method::POST,
            "/v1/collections",
            Some(collection_body.clone()),
            true,
        )
        .await?;
    assert_status(&collection, StatusCode::CREATED, "create collection")?;
    let collection_id = json_str(&collection.body, "id")?.to_string();
    eprintln!(
        "[deployed-api] collection created collection_id={} status={}",
        collection_id,
        json_str(&collection.body, "status")?
    );

    let collection_idempotent = api
        .json(Method::POST, "/v1/collections", Some(collection_body), true)
        .await?;
    assert_status(
        &collection_idempotent,
        StatusCode::OK,
        "idempotent create collection",
    )?;
    assert_eq_json_string(&collection_idempotent.body, "id", &collection_id)?;

    let collection_view = api
        .json(
            Method::GET,
            &format!("/v1/collections/{collection_id}"),
            None,
            true,
        )
        .await?;
    assert_status(&collection_view, StatusCode::OK, "get collection")?;
    assert_eq_json_string(&collection_view.body, "id", &collection_id)?;

    wait_for_collection_confirmed(
        &api,
        &collection_id,
        settings.collection_timeout,
        settings.poll_interval,
    )
    .await?;
    eprintln!("[deployed-api] collection confirmed collection_id={collection_id}");

    Ok(())
}

struct TestSettings {
    api_base_url: String,
    jwt: String,
    run_id: Uuid,
    external_id: String,
    collection_idempotency_key: String,
    amount: String,
    payment_amount_raw: RawAmount,
    order_ttl_seconds: u64,
    rpc_http_urls: Vec<String>,
    payment_gas_limit: u64,
    collection_gas_limit: u64,
    min_confirmations: u64,
    receipt_timeout: Duration,
    order_paid_timeout: Duration,
    collection_timeout: Duration,
    poll_interval: Duration,
    skip_chain_payment: bool,
    skip_collection: bool,
}

impl TestSettings {
    fn from_env(values: &EnvValues) -> Result<Self, AnyError> {
        let run_id = Uuid::new_v4();
        let payment_amount_raw = values
            .optional_any(&[
                "PAY3_API_TEST_PAYMENT_AMOUNT_RAW",
                "PAY3_E2E_PAYMENT_AMOUNT_RAW",
            ])
            .unwrap_or(DEFAULT_PAYMENT_AMOUNT_RAW)
            .parse::<RawAmount>()?;
        let token_decimals = parse_required_u8(values, "TOKEN_DECIMALS")?;
        let amount = values
            .optional("PAY3_API_TEST_AMOUNT")
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| raw_to_decimal_string(payment_amount_raw, token_decimals));

        Ok(Self {
            api_base_url: values
                .required_any(&[
                    "PAY3_API_BASE_URL",
                    "PAY3_DEPLOYED_API_BASE_URL",
                    "API_BASE_URL",
                ])?
                .trim_end_matches('/')
                .to_string(),
            jwt: jwt_from_env(values)?,
            run_id,
            external_id: values
                .optional("PAY3_API_TEST_EXTERNAL_ID")
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| format!("deployed-api-acceptance-order-{run_id}")),
            collection_idempotency_key: values
                .optional("PAY3_API_TEST_COLLECTION_IDEMPOTENCY_KEY")
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| format!("deployed-api-acceptance-collect-{run_id}")),
            amount,
            payment_amount_raw,
            order_ttl_seconds: parse_optional_u64(
                values,
                "PAY3_API_TEST_ORDER_TTL_SECONDS",
                DEFAULT_ORDER_TTL_SECONDS,
            )?,
            rpc_http_urls: parse_required_list(values, "RPC_HTTP_URLS")?,
            payment_gas_limit: parse_optional_u64(
                values,
                "PAY3_API_TEST_PAYMENT_GAS_LIMIT",
                DEFAULT_PAYMENT_GAS_LIMIT,
            )?,
            collection_gas_limit: parse_optional_u64(
                values,
                "PAY3_API_TEST_COLLECTION_GAS_LIMIT",
                values
                    .optional("COLLECTION_GAS_LIMIT")
                    .unwrap_or("120000")
                    .parse::<u64>()?,
            )?,
            min_confirmations: parse_optional_u64(values, "MIN_CONFIRMATIONS", 1)?,
            receipt_timeout: Duration::from_secs(parse_optional_u64(
                values,
                "PAY3_API_TEST_RECEIPT_TIMEOUT_SECS",
                DEFAULT_RECEIPT_TIMEOUT_SECS,
            )?),
            order_paid_timeout: Duration::from_secs(parse_optional_u64(
                values,
                "PAY3_API_TEST_ORDER_PAID_TIMEOUT_SECS",
                values
                    .optional("PAY3_E2E_CONFIRMATION_TIMEOUT_SECS")
                    .unwrap_or("360")
                    .parse()
                    .unwrap_or(DEFAULT_ORDER_PAID_TIMEOUT_SECS),
            )?),
            collection_timeout: Duration::from_secs(parse_optional_u64(
                values,
                "PAY3_API_TEST_COLLECTION_TIMEOUT_SECS",
                DEFAULT_COLLECTION_TIMEOUT_SECS,
            )?),
            poll_interval: Duration::from_secs(
                parse_optional_u64(
                    values,
                    "PAY3_API_TEST_POLL_INTERVAL_SECS",
                    DEFAULT_POLL_INTERVAL_SECS,
                )?
                .max(1),
            ),
            skip_chain_payment: parse_optional_bool(
                values,
                "PAY3_API_TEST_SKIP_CHAIN_PAYMENT",
                false,
            )?,
            skip_collection: parse_optional_bool(values, "PAY3_API_TEST_SKIP_COLLECTION", false)?,
        })
    }
}

fn jwt_from_env(values: &EnvValues) -> Result<String, AnyError> {
    if let Some(token) = values.optional_any(&["PAY3_API_JWT", "PAY3_TEST_JWT"]) {
        return Ok(token.to_string());
    }

    let secret = values.required("JWT_SECRET").map_err(|_| {
        helper_error(
            "set PAY3_API_JWT/PAY3_TEST_JWT for asymmetric JWT environments, or provide JWT_SECRET for HS256 test signing",
        )
    })?;
    let issuer = values.required("JWT_ISSUER")?;
    let audience = values.required("JWT_AUDIENCE")?;
    let key_id = values
        .optional_any(&["JWT_KEY_ID", "JWT_KID"])
        .unwrap_or("pay3-deployed-api-test");
    let subject = values
        .optional_any(&["PAY3_API_TEST_SUBJECT", "JWT_SUBJECT"])
        .unwrap_or("deployed-api-acceptance");
    let scopes = values
        .optional_any(&["PAY3_API_TEST_JWT_SCOPES", "JWT_SCOPES"])
        .unwrap_or("orders:create orders:read orders:verify collections:create collections:read");
    let ttl_seconds = parse_optional_u64(values, "PAY3_API_TEST_JWT_TTL_SECONDS", 24 * 60 * 60)?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let claims = Claims {
        exp: now + ttl_seconds,
        nbf: now.saturating_sub(60),
        iat: now,
        iss: issuer.to_string(),
        aud: Audience::One(audience.to_string()),
        sub: subject.to_string(),
        scope: Some(scopes.to_string()),
        scopes: None,
        scp: None,
    };
    let mut header = Header::new(Algorithm::HS256);
    header.kid = Some(key_id.to_string());

    Ok(encode(
        &header,
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )?)
}

struct ApiClient {
    base_url: String,
    token: String,
    client: reqwest::Client,
}

struct JsonHttpResponse {
    status: StatusCode,
    body: Value,
}

impl ApiClient {
    fn new(base_url: String, token: String) -> Result<Self, AnyError> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()?;
        Ok(Self {
            base_url,
            token,
            client,
        })
    }

    async fn json(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
        auth: bool,
    ) -> Result<JsonHttpResponse, AnyError> {
        let mut request = self
            .client
            .request(method, self.url(path))
            .header(reqwest::header::ACCEPT, "application/json")
            .header("x-request-id", format!("deployed-api-{}", Uuid::new_v4()));
        if auth {
            request = request.bearer_auth(&self.token);
        }
        if let Some(body) = body {
            request = request.json(&body);
        }

        let response = request.send().await?;
        let status = response.status();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let bytes = response.bytes().await?;
        let body = if content_type.contains("application/json") {
            serde_json::from_slice(&bytes)?
        } else {
            Value::String(String::from_utf8_lossy(&bytes).into_owned())
        };

        Ok(JsonHttpResponse { status, body })
    }

    async fn text(&self, path: &str) -> Result<(StatusCode, String), AnyError> {
        let response = self.client.get(self.url(path)).send().await?;
        let status = response.status();
        let body = response.text().await?;
        Ok((status, body))
    }

    fn url(&self, path: &str) -> String {
        format!(
            "{}/{}",
            self.base_url.trim_end_matches('/'),
            path.trim_start_matches('/')
        )
    }
}

async fn assert_healthz(api: &ApiClient) -> Result<(), AnyError> {
    let response = api.json(Method::GET, "/healthz", None, false).await?;
    assert_status(&response, StatusCode::OK, "GET /healthz")?;
    assert_eq_json_string(&response.body, "status", "ok")?;
    Ok(())
}

async fn assert_readyz(api: &ApiClient) -> Result<(), AnyError> {
    let response = api.json(Method::GET, "/readyz", None, false).await?;
    assert_status(&response, StatusCode::OK, "GET /readyz")?;
    assert_eq_json_string(&response.body, "status", "ok")?;
    Ok(())
}

async fn assert_metrics(api: &ApiClient) -> Result<(), AnyError> {
    let (status, body) = api.text("/metrics").await?;
    if status != StatusCode::OK {
        return Err(helper_error(format!(
            "GET /metrics returned {status}: {body}"
        )));
    }
    if !body.contains("pay3_build_info") {
        return Err(helper_error("GET /metrics did not expose pay3_build_info"));
    }
    Ok(())
}

async fn assert_v1_requires_auth(api: &ApiClient) -> Result<(), AnyError> {
    let response = api
        .json(
            Method::GET,
            "/v1/orders/00000000-0000-0000-0000-000000000001",
            None,
            false,
        )
        .await?;
    assert_status(
        &response,
        StatusCode::UNAUTHORIZED,
        "GET /v1/orders without auth",
    )?;
    Ok(())
}

async fn wait_for_order_paid(
    api: &ApiClient,
    order_id: &str,
    timeout: Duration,
    poll_interval: Duration,
) -> Result<Value, AnyError> {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut last_order: Value;
    let mut last_verify: Value;

    loop {
        let order = api
            .json(Method::GET, &format!("/v1/orders/{order_id}"), None, true)
            .await?;
        assert_status(&order, StatusCode::OK, "poll order")?;
        let order_body = order.body;
        if order_body.get("status").and_then(Value::as_str) == Some("paid") {
            return Ok(order_body);
        }
        last_order = order_body;

        let verify = api
            .json(
                Method::POST,
                &format!("/v1/orders/{order_id}/verify"),
                None,
                true,
            )
            .await?;
        if verify.status.is_success() || verify.status == StatusCode::SERVICE_UNAVAILABLE {
            last_verify = verify.body;
        } else {
            return Err(helper_error(format!(
                "verify order returned {}: {}",
                verify.status, verify.body
            )));
        }

        if tokio::time::Instant::now() >= deadline {
            return Err(helper_error(format!(
                "timed out waiting for order {order_id} to become paid; last_order={last_order}; last_verify={last_verify}"
            )));
        }
        tokio::time::sleep(poll_interval).await;
    }
}

async fn wait_for_collection_confirmed(
    api: &ApiClient,
    collection_id: &str,
    timeout: Duration,
    poll_interval: Duration,
) -> Result<Value, AnyError> {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut last_collection: Value;

    loop {
        let response = api
            .json(
                Method::GET,
                &format!("/v1/collections/{collection_id}"),
                None,
                true,
            )
            .await?;
        assert_status(&response, StatusCode::OK, "poll collection")?;
        let collection_body = response.body;
        match collection_body.get("status").and_then(Value::as_str) {
            Some("confirmed") => return Ok(collection_body),
            Some("failed" | "dropped" | "replaced") => {
                return Err(helper_error(format!(
                    "collection {collection_id} reached terminal failure: {collection_body}"
                )));
            }
            _ => {}
        }
        last_collection = collection_body;

        if tokio::time::Instant::now() >= deadline {
            return Err(helper_error(format!(
                "timed out waiting for collection {collection_id} to confirm; last_collection={last_collection}"
            )));
        }
        tokio::time::sleep(poll_interval).await;
    }
}

async fn ensure_collection_native_balance(
    payer: &PaymentWallet,
    rpc_source: &RpcRangeSource,
    request: CollectionTopUpRequest,
) -> Result<(), AnyError> {
    let required = raw_mul_u64(request.max_fee_per_gas, request.collection_gas_limit)?;
    let balance = rpc_source
        .native_balance(request.chain_id, request.receive_address)
        .await?;
    if balance >= required {
        eprintln!(
            "[deployed-api] collection address has native gas balance={} required={}",
            balance, required
        );
        return Ok(());
    }

    let top_up = required
        .checked_sub(balance)
        .ok_or_else(|| helper_error("collection native top-up underflow"))?;
    eprintln!(
        "[deployed-api] topping up collection address={} balance={} required={} top_up={}",
        request.receive_address, balance, required, top_up
    );
    let tx_hash = send_native_transfer(
        payer,
        rpc_source,
        NativeTransferRequest {
            chain_id: request.chain_id,
            recipient: request.receive_address,
            amount: top_up,
            gas_limit: DEFAULT_NATIVE_TRANSFER_GAS_LIMIT,
            max_fee_per_gas: request.max_fee_per_gas,
            max_priority_fee_per_gas: request.max_priority_fee_per_gas,
        },
    )
    .await?;
    wait_for_successful_receipt(rpc_source, tx_hash, request.receipt_timeout).await?;
    Ok(())
}

struct CollectionTopUpRequest {
    chain_id: u64,
    receive_address: EvmAddress,
    max_fee_per_gas: RawAmount,
    max_priority_fee_per_gas: RawAmount,
    collection_gas_limit: u64,
    receipt_timeout: Duration,
}

enum PaymentWallet {
    PrivateKey {
        signer: PrivateKeySigner,
    },
    Mnemonic {
        signer: LocalMnemonicSigner,
        key_ref: String,
        path: String,
    },
}

impl PaymentWallet {
    fn from_env(values: &EnvValues) -> Result<Self, AnyError> {
        if let Some(key) = values.optional_any(&[
            "PAY3_API_TEST_PAYER_PRIVATE_KEY",
            "PAY3_E2E_PAYER_PRIVATE_KEY",
            "DEPLOYER_PRIVATE_KEY",
        ]) {
            return Ok(Self::PrivateKey {
                signer: key.parse()?,
            });
        }

        let mnemonic = values
            .optional_any(&[
                "PAY3_API_TEST_PAYER_MNEMONIC",
                "PAY3_E2E_PAYER_MNEMONIC",
                "SIGNER_MNEMONIC",
            ])
            .ok_or_else(|| {
                helper_error(
                    "configure PAY3_API_TEST_PAYER_PRIVATE_KEY, PAY3_E2E_PAYER_PRIVATE_KEY, PAY3_API_TEST_PAYER_MNEMONIC, PAY3_E2E_PAYER_MNEMONIC, or SIGNER_MNEMONIC for the payment sender",
                )
            })?;
        let key_ref = values
            .optional_any(&["PAY3_API_TEST_PAYER_KEY_REF", "PAY3_E2E_PAYER_KEY_REF"])
            .unwrap_or("deployed-api-acceptance-payer")
            .to_string();
        let path = values
            .optional_any(&[
                "PAY3_API_TEST_PAYER_DERIVATION_PATH",
                "PAY3_E2E_PAYER_DERIVATION_PATH",
            ])
            .unwrap_or(DEFAULT_PAYER_DERIVATION_PATH)
            .to_string();

        Ok(Self::Mnemonic {
            signer: LocalMnemonicSigner::new(key_ref.clone(), mnemonic)?,
            key_ref,
            path,
        })
    }

    async fn address(&self) -> Result<EvmAddress, AnyError> {
        match self {
            Self::PrivateKey { signer } => Ok(EvmAddress::from_alloy(signer.address())),
            Self::Mnemonic {
                signer,
                key_ref,
                path,
            } => Ok(signer.derive_address(key_ref, path).await?),
        }
    }

    async fn sign_transaction(&self, tx: UnsignedTx) -> Result<SignedTx, AnyError> {
        match self {
            Self::PrivateKey { signer } => sign_with_private_key(signer, tx),
            Self::Mnemonic {
                signer,
                key_ref,
                path,
            } => Ok(signer.sign_transaction(key_ref, path, tx).await?),
        }
    }
}

struct TokenTransferRequest {
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

async fn send_token_transfer(
    payer: &PaymentWallet,
    rpc_source: &RpcRangeSource,
    request: TokenTransferRequest,
) -> Result<TxHash, AnyError> {
    let payer_address = payer.address().await?;
    let nonce = pending_nonce_u64(rpc_source, request.chain_id, payer_address).await?;
    let unsigned = UnsignedTx::new(
        format!("deployed-api-payment-{}", Uuid::new_v4()),
        request.chain_id,
        nonce,
        request.token_address,
        RawAmount::ZERO,
        request.gas_limit,
        request.max_fee_per_gas,
        request.max_priority_fee_per_gas,
        erc20_transfer_data(request.recipient, request.amount),
    )?;
    sign_and_broadcast(payer, rpc_source, payer_address, unsigned).await
}

async fn send_native_transfer(
    payer: &PaymentWallet,
    rpc_source: &RpcRangeSource,
    request: NativeTransferRequest,
) -> Result<TxHash, AnyError> {
    let payer_address = payer.address().await?;
    let nonce = pending_nonce_u64(rpc_source, request.chain_id, payer_address).await?;
    let unsigned = UnsignedTx::new(
        format!("deployed-api-native-topup-{}", Uuid::new_v4()),
        request.chain_id,
        nonce,
        request.recipient,
        request.amount,
        request.gas_limit,
        request.max_fee_per_gas,
        request.max_priority_fee_per_gas,
        Vec::new(),
    )?;
    sign_and_broadcast(payer, rpc_source, payer_address, unsigned).await
}

async fn sign_and_broadcast(
    payer: &PaymentWallet,
    rpc_source: &RpcRangeSource,
    payer_address: EvmAddress,
    unsigned: UnsignedTx,
) -> Result<TxHash, AnyError> {
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
            "broadcast hash mismatch: signed {}, broadcast {}",
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

async fn pending_nonce_u64(
    rpc_source: &RpcRangeSource,
    chain_id: u64,
    owner: EvmAddress,
) -> Result<u64, AnyError> {
    let nonce = rpc_source.pending_nonce(chain_id, owner).await?;
    u64::try_from(nonce.value()).map_err(|_| helper_error("pending nonce exceeds u64"))
}

fn raw_amount_to_u128(value: RawAmount, field: &'static str) -> Result<u128, AnyError> {
    u128::try_from(value.value()).map_err(|_| helper_error(format!("{field} exceeds u128")))
}

fn raw_mul_u64(value: RawAmount, multiplier: u64) -> Result<RawAmount, AnyError> {
    value
        .value()
        .checked_mul(U256::from(multiplier))
        .map(RawAmount::new)
        .ok_or_else(|| helper_error("raw amount multiplication overflowed"))
}

fn erc20_transfer_data(recipient: EvmAddress, amount: RawAmount) -> Vec<u8> {
    let mut data = Vec::with_capacity(68);
    data.extend_from_slice(&[0xa9, 0x05, 0x9c, 0xbb]);
    data.extend_from_slice(&[0u8; 12]);
    data.extend_from_slice(recipient.as_bytes());
    data.extend_from_slice(&amount.value().to_be_bytes::<32>());
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
            "{label} address {owner} token balance {balance} is below required payment amount {required}"
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
    timeout: Duration,
) -> Result<pay3::chain::TxReceipt, AnyError> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        match rpc_source.transaction_receipt(tx_hash).await? {
            Some(receipt) if receipt.status == TransactionStatus::Success => return Ok(receipt),
            Some(_) => return Err(helper_error(format!("transaction {tx_hash} reverted"))),
            None => {}
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(helper_error(format!(
                "timed out waiting for receipt of {tx_hash}"
            )));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

async fn wait_for_confirmations(
    rpc_source: &RpcRangeSource,
    block: pay3::domain::ChainBlockRef,
    min_confirmations: u64,
    timeout: Duration,
) -> Result<(), AnyError> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let head = rpc_source.latest_head().await?;
        if block.has_confirmations(head, min_confirmations) {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(helper_error(format!(
                "timed out waiting for {min_confirmations} confirmations for block {}",
                block.number
            )));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

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

    fn required(&self, key: &str) -> Result<&str, AnyError> {
        self.optional(key)
            .ok_or_else(|| helper_error(format!("missing required env var {key}")))
    }

    fn required_any(&self, keys: &[&str]) -> Result<&str, AnyError> {
        self.optional_any(keys).ok_or_else(|| {
            helper_error(format!(
                "missing required env var; set one of {}",
                keys.join(", ")
            ))
        })
    }

    fn optional(&self, key: &str) -> Option<&str> {
        self.values
            .get(key)
            .map(String::as_str)
            .filter(|value| !value.trim().is_empty())
    }

    fn optional_any(&self, keys: &[&str]) -> Option<&str> {
        keys.iter().find_map(|key| self.optional(key))
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

fn parse_required_u8(values: &EnvValues, key: &str) -> Result<u8, AnyError> {
    values
        .required(key)?
        .parse()
        .map_err(|error| helper_error(format!("{key} must be an unsigned 8-bit integer: {error}")))
}

fn parse_optional_u64(values: &EnvValues, key: &str, fallback: u64) -> Result<u64, AnyError> {
    let Some(value) = values.optional(key) else {
        return Ok(fallback);
    };
    value
        .parse()
        .map_err(|error| helper_error(format!("{key} must be an unsigned integer: {error}")))
}

fn parse_optional_bool(values: &EnvValues, key: &str, fallback: bool) -> Result<bool, AnyError> {
    let Some(value) = values.optional(key) else {
        return Ok(fallback);
    };
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(helper_error(format!("{key} must be a boolean value"))),
    }
}

fn parse_required_list(values: &EnvValues, key: &str) -> Result<Vec<String>, AnyError> {
    let values = values
        .required(key)?
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    if values.is_empty() {
        return Err(helper_error(format!("{key} must not be empty")));
    }
    Ok(values)
}

fn raw_to_decimal_string(raw: RawAmount, decimals: u8) -> String {
    let raw = raw.to_string();
    let decimals = usize::from(decimals);
    if decimals == 0 {
        return raw;
    }

    let value = if raw.len() <= decimals {
        let zeros = "0".repeat(decimals - raw.len());
        format!("0.{zeros}{raw}")
    } else {
        let split = raw.len() - decimals;
        format!("{}.{}", &raw[..split], &raw[split..])
    };

    value
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_string()
}

fn assert_status(
    response: &JsonHttpResponse,
    expected: StatusCode,
    label: &str,
) -> Result<(), AnyError> {
    if response.status != expected {
        return Err(helper_error(format!(
            "{label} returned {}, expected {}: {}",
            response.status, expected, response.body
        )));
    }
    Ok(())
}

fn assert_eq_json_string(body: &Value, path: &str, expected: &str) -> Result<(), AnyError> {
    let actual = json_str(body, path)?;
    if actual != expected {
        return Err(helper_error(format!(
            "{path} expected {expected}, got {actual}; body={body}"
        )));
    }
    Ok(())
}

fn json_str<'a>(body: &'a Value, path: &str) -> Result<&'a str, AnyError> {
    json_path(body, path)
        .and_then(Value::as_str)
        .ok_or_else(|| helper_error(format!("missing string field {path}: {body}")))
}

fn json_u64(body: &Value, path: &str) -> Result<u64, AnyError> {
    json_path(body, path)
        .and_then(Value::as_u64)
        .ok_or_else(|| helper_error(format!("missing u64 field {path}: {body}")))
}

fn json_path<'a>(body: &'a Value, path: &str) -> Option<&'a Value> {
    let mut value = body;
    for segment in path.split('.') {
        value = value.get(segment)?;
    }
    Some(value)
}

fn encode_path_segment(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char)
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
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
