mod support;

use std::{
    error::Error,
    fmt,
    net::SocketAddr,
    process,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use pay3::{
    auth::{
        Audience, Claims, COLLECTIONS_CREATE_SCOPE, COLLECTIONS_READ_SCOPE, ORDERS_CREATE_SCOPE,
        ORDERS_READ_SCOPE, ORDERS_VERIFY_SCOPE,
    },
    chain::{ChainHeaderReader, Erc20ChainClient, NativeBalanceReader, RpcRangeSource},
    config::AppConfig,
    db::repositories::{ExpiredOrderRepository, PgOrderRepository},
    domain::{EvmAddress, RawAmount, TxHash},
    runtime::{self, ApiRuntime},
};
use reqwest::{Client, Method, StatusCode};
use serde_json::{Value, json};
use sqlx::{Connection, Executor, PgConnection, PgPool, postgres::PgPoolOptions};
use tokio::task::JoinHandle;
use uuid::Uuid;

use support::anvil::{
    AnvilHarness, AnyError, DEFAULT_ANVIL_MNEMONIC, DEPLOYER_PATH, TREASURY_PATH,
    deploy_mock_erc20, send_erc20_transfer, send_native_transfer,
};

const TEST_JWT_SECRET: &str = "test-local-e2e-jwt-secret-with-sufficient-entropy-12345";
const TEST_JWT_ISSUER: &str = "pay3";
const TEST_JWT_AUDIENCE: &str = "pay3-api";
const TEST_SUBJECT: &str = "merchant-local-e2e-1";

#[derive(Debug)]
struct TestError(String);

impl fmt::Display for TestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Error for TestError {}

fn test_error(msg: impl Into<String>) -> AnyError {
    Box::new(TestError(msg.into()))
}

fn test_database_url() -> String {
    std::env::var("PAY3_TEST_DATABASE_URL")
        .ok()
        .or_else(|| std::env::var("TEST_DATABASE_URL").ok())
        .unwrap_or_else(|| "postgres://soneium:soneium@127.0.0.1:5432/soneium_points".to_string())
}

fn temp_schema_name(prefix: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{prefix}_{}_{}", process::id(), nanos)
}

fn quote_ident(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

async fn prepare_temp_schema(
    base_db_url: &str,
    prefix: &str,
) -> Result<(String, String), AnyError> {
    let mut conn = PgConnection::connect(base_db_url).await?;
    let schema = temp_schema_name(prefix);
    let schema_ident = quote_ident(&schema);
    conn.execute(format!("CREATE SCHEMA {schema_ident}").as_str())
        .await?;
    let schema_db_url = format!("{base_db_url}?options=-csearch_path%3D{schema}");
    Ok((schema, schema_db_url))
}

async fn drop_schema(base_db_url: &str, schema: &str) -> Result<(), AnyError> {
    let mut conn = PgConnection::connect(base_db_url).await?;
    let schema_ident = quote_ident(schema);
    conn.execute(format!("DROP SCHEMA {schema_ident} CASCADE").as_str())
        .await?;
    Ok(())
}

fn generate_jwt(scopes: &str, subject: &str) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let claims = Claims {
        exp: now + 86400,
        nbf: now.saturating_sub(60),
        iat: now,
        iss: TEST_JWT_ISSUER.to_string(),
        aud: Audience::One(TEST_JWT_AUDIENCE.to_string()),
        sub: subject.to_string(),
        scope: Some(scopes.to_string()),
        scopes: None,
        scp: None,
    };
    let mut header = Header::new(Algorithm::HS256);
    header.kid = Some("pay3-e2e-key".to_string());
    encode(
        &header,
        &claims,
        &EncodingKey::from_secret(TEST_JWT_SECRET.as_bytes()),
    )
    .expect("failed to sign test jwt")
}

struct ApiClient {
    base_url: String,
    token: String,
    client: Client,
}

struct HttpResponse {
    status: StatusCode,
    body: Value,
}

impl ApiClient {
    fn new(base_url: String, token: String) -> Self {
        Self {
            base_url,
            token,
            client: Client::builder()
                .timeout(Duration::from_secs(15))
                .build()
                .expect("failed to build reqwest client"),
        }
    }

    async fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
        auth: Option<&str>,
    ) -> Result<HttpResponse, AnyError> {
        let url = format!("{}/{}", self.base_url.trim_end_matches('/'), path.trim_start_matches('/'));
        let mut req = self
            .client
            .request(method, &url)
            .header("x-request-id", format!("e2e-req-{}", Uuid::new_v4()));

        if let Some(token) = auth {
            req = req.bearer_auth(token);
        }

        if let Some(json_body) = body {
            req = req.json(&json_body);
        }

        let resp = req.send().await?;
        let status = resp.status();
        let bytes = resp.bytes().await?;
        let body: Value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes)
                .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()))
        };

        Ok(HttpResponse { status, body })
    }

    async fn post_order(
        &self,
        external_id: &str,
        amount: &str,
        ttl_seconds: u64,
    ) -> Result<HttpResponse, AnyError> {
        let payload = json!({
            "external_id": external_id,
            "amount": amount,
            "ttl_seconds": ttl_seconds,
            "metadata": {
                "source": "local_e2e_test",
                "test_id": external_id
            }
        });
        self.request(Method::POST, "/v1/orders", Some(payload), Some(&self.token))
            .await
    }

    async fn get_order(&self, order_id: &str) -> Result<HttpResponse, AnyError> {
        self.request(
            Method::GET,
            &format!("/v1/orders/{order_id}"),
            None,
            Some(&self.token),
        )
        .await
    }

    async fn get_order_by_external_id(&self, external_id: &str) -> Result<HttpResponse, AnyError> {
        self.request(
            Method::GET,
            &format!("/v1/orders/by-external-id/{external_id}"),
            None,
            Some(&self.token),
        )
        .await
    }

    async fn verify_order(&self, order_id: &str) -> Result<HttpResponse, AnyError> {
        self.request(
            Method::POST,
            &format!("/v1/orders/{order_id}/verify"),
            None,
            Some(&self.token),
        )
        .await
    }

    async fn accept_problem_payment(
        &self,
        order_id: &str,
        reason: &str,
    ) -> Result<HttpResponse, AnyError> {
        let payload = json!({
            "reason": reason
        });
        self.request(
            Method::POST,
            &format!("/v1/orders/{order_id}/accept-problem-payment"),
            Some(payload),
            Some(&self.token),
        )
        .await
    }

    async fn post_collection(
        &self,
        order_id: &str,
        idempotency_key: &str,
    ) -> Result<HttpResponse, AnyError> {
        let payload = json!({
            "order_id": order_id,
            "amount": "max",
            "idempotency_key": idempotency_key
        });
        self.request(Method::POST, "/v1/collections", Some(payload), Some(&self.token))
            .await
    }

    async fn get_collection(&self, collection_id: &str) -> Result<HttpResponse, AnyError> {
        self.request(
            Method::GET,
            &format!("/v1/collections/{collection_id}"),
            None,
            Some(&self.token),
        )
        .await
    }

    async fn healthz(&self) -> Result<HttpResponse, AnyError> {
        self.request(Method::GET, "/healthz", None, None).await
    }

    async fn readyz(&self) -> Result<HttpResponse, AnyError> {
        self.request(Method::GET, "/readyz", None, None).await
    }

    async fn metrics(&self) -> Result<(StatusCode, String), AnyError> {
        let url = format!("{}/metrics", self.base_url.trim_end_matches('/'));
        let resp = self.client.get(url).send().await?;
        let status = resp.status();
        let text = resp.text().await?;
        Ok((status, text))
    }

    async fn tokens(&self) -> Result<HttpResponse, AnyError> {
        self.request(Method::GET, "/v1/tokens", None, None).await
    }
}

#[allow(dead_code)]
struct LocalE2eHarness {
    anvil: AnvilHarness,
    rpc_source: RpcRangeSource,
    base_db_url: String,
    schema_name: String,
    schema_db_url: String,
    token_address: EvmAddress,
    deployer_address: EvmAddress,
    treasury_address: EvmAddress,
    problem_funds_address: EvmAddress,
    server_addr: SocketAddr,
    api_runtime: Option<ApiRuntime>,
    server_task: Option<JoinHandle<()>>,
    api: ApiClient,
    full_scopes_token: String,
}

impl LocalE2eHarness {
    async fn start(prefix: &str) -> Result<Self, AnyError> {
        let base_db_url = test_database_url();
        let (schema_name, schema_db_url) = prepare_temp_schema(&base_db_url, prefix).await?;

        let anvil = AnvilHarness::start().await?;
        let chain_id = anvil.chain_id();
        let rpc_url = anvil.rpc_url().to_string();
        let rpc_source =
            RpcRangeSource::from_http_urls(chain_id, std::slice::from_ref(&rpc_url), 1)?;

        let deployer_address = anvil.derive_address(DEPLOYER_PATH).await?;
        let treasury_address = anvil.derive_address(TREASURY_PATH).await?;
        let problem_funds_address = anvil.derive_address("m/44'/60'/0'/0/3").await?;

        // 部署 Mock ERC20 合约 (100万总供应量分配给 deployer)
        let initial_supply = RawAmount::from(1_000_000_000_000u64);
        let token_address = deploy_mock_erc20(
            anvil.rpc_url(),
            deployer_address,
            deployer_address,
            initial_supply,
        )
        .await?;

        let head = rpc_source.latest_head().await?;
        let start_block = head.number;

        let yaml = format!(
            r#"
server:
  bind_addr: "127.0.0.1:0"
  profile: "development"
  role: "all"
database:
  url: "{schema_db_url}"
signer:
  mode: "local"
  key_ref: "pay3-master"
  mnemonic: "{DEFAULT_ANVIL_MNEMONIC}"
  allow_local_signer: true
auth:
  issuer: "{TEST_JWT_ISSUER}"
  audience: "{TEST_JWT_AUDIENCE}"
  secret: "{TEST_JWT_SECRET}"
  key_id: "pay3-e2e-key"
tokens:
  - chain_id: {chain_id}
    token_address: "{token_address}"
    token_symbol: "USDC"
    token_decimals: 6
    treasury_address: "{treasury_address}"
    problem_funds_address: "{problem_funds_address}"
    rpc_http_urls:
      - "{rpc_url}"
    start_block: {start_block}
    min_confirmations: 0
    collection_method: "standard"
"#,
        );

        let config = AppConfig::from_yaml_str(&yaml)?;
        let api_runtime = runtime::build_api_runtime(config).await?;
        let app = api_runtime.router();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let server_addr = listener.local_addr()?;
        let server_task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let full_scopes = format!(
            "{ORDERS_CREATE_SCOPE} {ORDERS_READ_SCOPE} {ORDERS_VERIFY_SCOPE} {COLLECTIONS_CREATE_SCOPE} {COLLECTIONS_READ_SCOPE}"
        );
        let full_scopes_token = generate_jwt(&full_scopes, TEST_SUBJECT);
        let api = ApiClient::new(format!("http://{server_addr}"), full_scopes_token.clone());

        Ok(Self {
            anvil,
            rpc_source,
            base_db_url,
            schema_name,
            schema_db_url,
            token_address,
            deployer_address,
            treasury_address,
            problem_funds_address,
            server_addr,
            api_runtime: Some(api_runtime),
            server_task: Some(server_task),
            api,
            full_scopes_token,
        })
    }

    async fn pool(&self) -> Result<PgPool, AnyError> {
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(&self.schema_db_url)
            .await?;
        Ok(pool)
    }

    async fn wait_for_order_status(
        &self,
        order_id: &str,
        expected: &str,
        timeout: Duration,
    ) -> Result<Value, AnyError> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let resp = self.api.get_order(order_id).await?;
            if resp.status == StatusCode::OK {
                let status = resp.body.get("status").and_then(Value::as_str);
                if status == Some(expected) {
                    return Ok(resp.body);
                }
            }

            if tokio::time::Instant::now() >= deadline {
                return Err(test_error(format!(
                    "timed out waiting for order {order_id} to reach {expected}; last_resp={}",
                    resp.body
                )));
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    }

    async fn wait_for_collection_status(
        &self,
        collection_id: &str,
        expected: &str,
        timeout: Duration,
    ) -> Result<Value, AnyError> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let resp = self.api.get_collection(collection_id).await?;
            if resp.status == StatusCode::OK {
                let status = resp.body.get("status").and_then(Value::as_str);
                if status == Some(expected) {
                    return Ok(resp.body);
                }
            }

            if tokio::time::Instant::now() >= deadline {
                return Err(test_error(format!(
                    "timed out waiting for collection {collection_id} to reach {expected}; last_resp={}",
                    resp.body
                )));
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    }

    async fn shutdown(mut self) {
        if let Some(task) = self.server_task.take() {
            task.abort();
        }
        if let Some(runtime) = self.api_runtime.take() {
            runtime.shutdown().await;
        }
        let _ = drop_schema(&self.base_db_url, &self.schema_name).await;
    }
}

async fn wait_for_receipt<C>(client: &C, tx_hash: TxHash) -> Result<(), AnyError>
where
    C: Erc20ChainClient + ?Sized,
{
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        if std::time::Instant::now() > deadline {
            return Err(test_error(format!("timed out waiting for receipt of {tx_hash}")));
        }
        if client.transaction_receipt(tx_hash).await?.is_some() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

// =============================================================================
// TEST 1: 标准完整支付归集闭环 (Standard E2E Payment & Sweep with Simulated Gas Funding)
// =============================================================================
#[tokio::test]
async fn test_full_happy_path_e2e_lifecycle_with_simulated_gas_topup() -> Result<(), AnyError> {
    let harness = LocalE2eHarness::start("pay3_e2e_std").await?;

    // 1. 探针验证: healthz, readyz, metrics, tokens
    let healthz = harness.api.healthz().await?;
    assert_eq!(healthz.status, StatusCode::OK);
    assert_eq!(healthz.body["status"], "ok");

    // 等待后台 worker 完成首轮 tick，使 readyz 进入就绪状态
    let mut readyz = harness.api.readyz().await?;
    let readyz_deadline = tokio::time::Instant::now() + Duration::from_secs(8);
    while readyz.status != StatusCode::OK && tokio::time::Instant::now() < readyz_deadline {
        tokio::time::sleep(Duration::from_millis(500)).await;
        readyz = harness.api.readyz().await?;
    }
    assert_eq!(readyz.status, StatusCode::OK);
    assert_eq!(readyz.body["status"], "ok");
    assert_eq!(readyz.body["status"], "ok");

    let (m_status, m_text) = harness.api.metrics().await?;
    assert_eq!(m_status, StatusCode::OK);
    assert!(m_text.contains("pay3_build_info"));

    let tokens = harness.api.tokens().await?;
    assert_eq!(tokens.status, StatusCode::OK);
    assert_eq!(tokens.body["tokens"][0]["token_symbol"], "USDC");

    // 2. 鉴权防御验证: 无 Token -> 401; 权限不足 Token -> 403
    let no_auth = harness
        .api
        .request(Method::POST, "/v1/orders", Some(json!({"amount": "10.00", "external_id": "auth-test", "ttl_seconds": 3600})), None)
        .await?;
    assert_eq!(no_auth.status, StatusCode::UNAUTHORIZED);

    let read_only_token = generate_jwt(ORDERS_READ_SCOPE, TEST_SUBJECT);
    let forbidden_auth = harness
        .api
        .request(
            Method::POST,
            "/v1/orders",
            Some(json!({"amount": "10.00", "external_id": "auth-test", "ttl_seconds": 3600})),
            Some(&read_only_token),
        )
        .await?;
    assert_eq!(forbidden_auth.status, StatusCode::FORBIDDEN);

    // 3. 创建订单 (POST /v1/orders)
    let external_id = format!("order-std-{}", Uuid::new_v4());
    let create_resp = harness.api.post_order(&external_id, "10.00", 3600).await?;
    assert_eq!(create_resp.status, StatusCode::CREATED);
    let order_id = create_resp.body["id"].as_str().unwrap().to_string();
    let receive_address: EvmAddress = create_resp.body["payment"]["receive_address"]
        .as_str()
        .unwrap()
        .parse()?;
    assert_eq!(create_resp.body["status"], "pending");
    assert_eq!(create_resp.body["payment"]["amount_raw"], "10000000");

    // 4. 幂等性验证: 相同 external_id 再次创建返回 200 OK 且地址与 ID 保持完全一致 (严格禁止地址复用)
    let idempotent_resp = harness.api.post_order(&external_id, "10.00", 3600).await?;
    assert_eq!(idempotent_resp.status, StatusCode::OK);
    assert_eq!(idempotent_resp.body["id"], order_id);
    assert_eq!(
        idempotent_resp.body["payment"]["receive_address"],
        receive_address.to_string()
    );

    // 5. 查询订单 (GET /v1/orders/{id} 以及 GET /v1/orders/by-external-id/{external_id})
    let get_id = harness.api.get_order(&order_id).await?;
    assert_eq!(get_id.status, StatusCode::OK);
    assert_eq!(get_id.body["id"], order_id);

    let get_ext = harness.api.get_order_by_external_id(&external_id).await?;
    assert_eq!(get_ext.status, StatusCode::OK);
    assert_eq!(get_ext.body["id"], order_id);

    // 6. 链上转账付款 (Payer 在本地 Anvil 链上向派生子收款地址转入 10.00 USDC)
    let payment_amount = RawAmount::from(10_000_000u64);
    let tx_hash = send_erc20_transfer(
        harness.anvil.rpc_url(),
        harness.anvil.mnemonic(),
        DEPLOYER_PATH,
        harness.token_address,
        receive_address,
        payment_amount,
        120_000,
        RawAmount::from(10_000_000_000u64),
    )
    .await?;
    wait_for_receipt(&harness.rpc_source, tx_hash).await?;

    // 7. 等待扫链 Worker 自动命中并推进订单到 paid
    let paid_order = harness
        .wait_for_order_status(&order_id, "paid", Duration::from_secs(12))
        .await?;
    assert_eq!(paid_order["payment"]["paid_amount_raw"], "10000000");

    // 8. 创建归集 (POST /v1/collections) -> 成功进入 queued 状态
    let collect_key = format!("collect-key-{}", Uuid::new_v4());
    let collect_resp = harness.api.post_collection(&order_id, &collect_key).await?;
    assert_eq!(collect_resp.status, StatusCode::CREATED);
    let collection_id = collect_resp.body["id"].as_str().unwrap().to_string();
    assert_eq!(collect_resp.body["status"], "queued");
    assert_eq!(collect_resp.body["to_address"], harness.treasury_address.to_string());

    // 归集幂等性验证
    let collect_idem = harness.api.post_collection(&order_id, &collect_key).await?;
    assert_eq!(collect_idem.status, StatusCode::OK);
    assert_eq!(collect_idem.body["id"], collection_id);

    // 【模拟人工观察】此时子收款地址 Gas 为 0，后台 Collector 因前置 Gas 校验无法签名广播，归集单持续保持 queued
    tokio::time::sleep(Duration::from_millis(600)).await;
    let pending_collect = harness.api.get_collection(&collection_id).await?;
    assert_eq!(pending_collect.body["status"], "queued");
    assert!(pending_collect.body["outbound_tx_id"].is_null());

    // 9. 【模拟人工操作】运维人员手动向子收款地址注入原生 Gas (0.05 ETH)
    let gas_topup = RawAmount::from(50_000_000_000_000_000u64);
    let topup_tx = send_native_transfer(
        harness.anvil.rpc_url(),
        harness.deployer_address,
        receive_address,
        gas_topup,
    )
    .await?;
    wait_for_receipt(&harness.rpc_source, topup_tx).await?;

    // 确认子地址原生余额满足要求
    let balance = harness
        .rpc_source
        .native_balance(harness.anvil.chain_id(), receive_address)
        .await?;
    assert!(balance >= gas_topup);

    // 10. 等待后台归集 Worker 执行广播与确认 (confirmed)
    let confirmed_collect = harness
        .wait_for_collection_status(&collection_id, "confirmed", Duration::from_secs(15))
        .await?;
    assert_eq!(confirmed_collect["status"], "confirmed");
    assert!(confirmed_collect["outbound_tx_id"].is_string());

    // 12. 校验资金最终闭环: Treasury 地址上的 ERC20 代币余额精确增加 10.00 USDC
    let treasury_balance = harness
        .rpc_source
        .token_balance(harness.token_address, harness.treasury_address)
        .await?;
    assert_eq!(treasury_balance, payment_amount);

    harness.shutdown().await;
    Ok(())
}

// =============================================================================
// TEST 2: 模拟人工即时核验闭环 (Simulated Manual / Operator Verification Flow)
// =============================================================================
#[tokio::test]
async fn test_manual_order_verification_immediate_fulfillment() -> Result<(), AnyError> {
    let harness = LocalE2eHarness::start("pay3_e2e_verify").await?;

    // 1. 创建订单 (20.00 USDC)
    let external_id = format!("order-verify-{}", Uuid::new_v4());
    let create_resp = harness.api.post_order(&external_id, "20.00", 3600).await?;
    assert_eq!(create_resp.status, StatusCode::CREATED);
    let order_id = create_resp.body["id"].as_str().unwrap().to_string();
    let receive_address: EvmAddress = create_resp.body["payment"]["receive_address"]
        .as_str()
        .unwrap()
        .parse()?;

    // 2. 链上完成付款
    let payment_amount = RawAmount::from(20_000_000u64);
    let tx_hash = send_erc20_transfer(
        harness.anvil.rpc_url(),
        harness.anvil.mnemonic(),
        DEPLOYER_PATH,
        harness.token_address,
        receive_address,
        payment_amount,
        120_000,
        RawAmount::from(10_000_000_000u64),
    )
    .await?;
    wait_for_receipt(&harness.rpc_source, tx_hash).await?;

    // 3. 【模拟人工操作】用户在前端或客服在运营后台点击“我已转账，立即核验” (POST /v1/orders/{id}/verify)
    // 直接触发链上 RPC 即时扫描匹配与状态重算，无需等待后台定时轮询
    let verify_resp = harness.api.verify_order(&order_id).await?;
    assert_eq!(verify_resp.status, StatusCode::OK);
    assert_eq!(verify_resp.body["status"], "confirmed");
    assert_eq!(verify_resp.body["matched_payments"], 1);
    assert_eq!(verify_resp.body["paid_amount_raw"], "20000000");

    // 4. 核验完成后订单立即变为 paid
    let get_resp = harness.api.get_order(&order_id).await?;
    assert_eq!(get_resp.status, StatusCode::OK);
    assert_eq!(get_resp.body["status"], "paid");
    assert_eq!(get_resp.body["payment"]["paid_amount_raw"], "20000000");

    // 5. 模拟人工注资 Gas 并完成归集
    let gas_topup = RawAmount::from(50_000_000_000_000_000u64);
    let topup_tx = send_native_transfer(
        harness.anvil.rpc_url(),
        harness.deployer_address,
        receive_address,
        gas_topup,
    )
    .await?;
    wait_for_receipt(&harness.rpc_source, topup_tx).await?;

    let collect_key = format!("collect-verify-{}", Uuid::new_v4());
    let collect_resp = harness.api.post_collection(&order_id, &collect_key).await?;
    assert_eq!(collect_resp.status, StatusCode::CREATED);
    let collection_id = collect_resp.body["id"].as_str().unwrap().to_string();

    let confirmed_collect = harness
        .wait_for_collection_status(&collection_id, "confirmed", Duration::from_secs(15))
        .await?;
    assert_eq!(confirmed_collect["status"], "confirmed");

    let treasury_balance = harness
        .rpc_source
        .token_balance(harness.token_address, harness.treasury_address)
        .await?;
    assert_eq!(treasury_balance, payment_amount);

    harness.shutdown().await;
    Ok(())
}

// =============================================================================
// TEST 3: 模拟人工审批处理迟到付款异常 (Simulated Manual Problem Payment Acceptance)
// =============================================================================
#[tokio::test]
async fn test_manual_problem_payment_acceptance_flow() -> Result<(), AnyError> {
    let harness = LocalE2eHarness::start("pay3_e2e_late").await?;

    // 1. 创建一个超短时效订单 (TTL = 1 秒)
    let external_id = format!("order-late-{}", Uuid::new_v4());
    let create_resp = harness.api.post_order(&external_id, "15.00", 1).await?;
    assert_eq!(create_resp.status, StatusCode::CREATED);
    let order_id = create_resp.body["id"].as_str().unwrap().to_string();
    let receive_address: EvmAddress = create_resp.body["payment"]["receive_address"]
        .as_str()
        .unwrap()
        .parse()?;

    // 2. 等待时效过期，调用过期重算
    tokio::time::sleep(Duration::from_secs(2)).await;
    let pool = harness.pool().await?;
    let order_repo = PgOrderRepository::new(pool);
    order_repo.recompute_expired_open_orders(100).await?;

    let expired_order = harness.api.get_order(&order_id).await?;
    assert_eq!(expired_order.body["status"], "expired");

    // 3. 客户迟到付款: 在订单已 expired 后才向子收款地址转账 15.00 USDC
    let payment_amount = RawAmount::from(15_000_000u64);
    let tx_hash = send_erc20_transfer(
        harness.anvil.rpc_url(),
        harness.anvil.mnemonic(),
        DEPLOYER_PATH,
        harness.token_address,
        receive_address,
        payment_amount,
        120_000,
        RawAmount::from(10_000_000_000u64),
    )
    .await?;
    wait_for_receipt(&harness.rpc_source, tx_hash).await?;

    // 4. 触发核验识别到链上转账，记录为 late 迟到付款，订单依然维持 expired
    let verify_resp = harness.api.verify_order(&order_id).await?;
    assert_eq!(verify_resp.status, StatusCode::OK);

    let still_expired = harness.api.get_order(&order_id).await?;
    assert_eq!(still_expired.body["status"], "expired");

    // 5. 【模拟人工操作】客服核实客户申诉后，在后台进行人工特批认可该笔迟到付款
    // POST /v1/orders/{id}/accept-problem-payment
    let accept_resp = harness
        .api
        .accept_problem_payment(
            &order_id,
            "Customer paid late due to network delay, CS approved manually",
        )
        .await?;
    assert_eq!(accept_resp.status, StatusCode::OK);
    assert_eq!(accept_resp.body["status"], "paid");
    assert_eq!(accept_resp.body["payment"]["paid_amount_raw"], "15000000");

    // 6. 验证订单正式变为 paid 状态，并可以正常归集
    let get_resp = harness.api.get_order(&order_id).await?;
    assert_eq!(get_resp.body["status"], "paid");

    let gas_topup = RawAmount::from(50_000_000_000_000_000u64);
    let topup_tx = send_native_transfer(
        harness.anvil.rpc_url(),
        harness.deployer_address,
        receive_address,
        gas_topup,
    )
    .await?;
    wait_for_receipt(&harness.rpc_source, topup_tx).await?;

    let collect_key = format!("collect-late-{}", Uuid::new_v4());
    let collect_resp = harness.api.post_collection(&order_id, &collect_key).await?;
    assert_eq!(collect_resp.status, StatusCode::CREATED);
    let collection_id = collect_resp.body["id"].as_str().unwrap().to_string();

    let confirmed_collect = harness
        .wait_for_collection_status(&collection_id, "confirmed", Duration::from_secs(15))
        .await?;
    assert_eq!(confirmed_collect["status"], "confirmed");

    let treasury_balance = harness
        .rpc_source
        .token_balance(harness.token_address, harness.treasury_address)
        .await?;
    assert_eq!(treasury_balance, payment_amount);

    harness.shutdown().await;
    Ok(())
}

// =============================================================================
// TEST 4: 部分付款与追加付款至全额闭环 (Partial Payment & Top-Up Flow)
// =============================================================================
#[tokio::test]
async fn test_partial_payment_and_incremental_topup_flow() -> Result<(), AnyError> {
    let harness = LocalE2eHarness::start("pay3_e2e_partial").await?;

    // 1. 创建 100.00 USDC 订单
    let external_id = format!("order-partial-{}", Uuid::new_v4());
    let create_resp = harness.api.post_order(&external_id, "100.00", 3600).await?;
    assert_eq!(create_resp.status, StatusCode::CREATED);
    let order_id = create_resp.body["id"].as_str().unwrap().to_string();
    let receive_address: EvmAddress = create_resp.body["payment"]["receive_address"]
        .as_str()
        .unwrap()
        .parse()?;

    // 2. 第一笔部分付款: 转入 40.00 USDC (未足额)
    let first_payment = RawAmount::from(40_000_000u64);
    let tx1 = send_erc20_transfer(
        harness.anvil.rpc_url(),
        harness.anvil.mnemonic(),
        DEPLOYER_PATH,
        harness.token_address,
        receive_address,
        first_payment,
        120_000,
        RawAmount::from(10_000_000_000u64),
    )
    .await?;
    wait_for_receipt(&harness.rpc_source, tx1).await?;

    // 立即核验 -> 状态转为 partial，已付金额 40.00 USDC
    let v1 = harness.api.verify_order(&order_id).await?;
    assert_eq!(v1.status, StatusCode::OK);
    assert_eq!(v1.body["paid_amount_raw"], "40000000");

    let order_v1 = harness.api.get_order(&order_id).await?;
    assert_eq!(order_v1.body["status"], "partial");
    assert_eq!(order_v1.body["payment"]["paid_amount_raw"], "40000000");

    // 3. 第二笔追加付款: 转入剩余的 60.00 USDC
    let second_payment = RawAmount::from(60_000_000u64);
    let tx2 = send_erc20_transfer(
        harness.anvil.rpc_url(),
        harness.anvil.mnemonic(),
        DEPLOYER_PATH,
        harness.token_address,
        receive_address,
        second_payment,
        120_000,
        RawAmount::from(10_000_000_000u64),
    )
    .await?;
    wait_for_receipt(&harness.rpc_source, tx2).await?;

    // 再次核验 -> 累计 100.00 USDC，状态成功转为 confirmed / paid!
    let v2 = harness.api.verify_order(&order_id).await?;
    assert_eq!(v2.status, StatusCode::OK);
    assert_eq!(v2.body["status"], "confirmed");
    assert_eq!(v2.body["paid_amount_raw"], "100000000");

    let order_v2 = harness.api.get_order(&order_id).await?;
    assert_eq!(order_v2.body["status"], "paid");
    assert_eq!(order_v2.body["payment"]["paid_amount_raw"], "100000000");

    // 4. 模拟人工注资 Gas 并归集全额 100.00 USDC
    let gas_topup = RawAmount::from(50_000_000_000_000_000u64);
    let topup_tx = send_native_transfer(
        harness.anvil.rpc_url(),
        harness.deployer_address,
        receive_address,
        gas_topup,
    )
    .await?;
    wait_for_receipt(&harness.rpc_source, topup_tx).await?;

    let collect_key = format!("collect-partial-{}", Uuid::new_v4());
    let collect_resp = harness.api.post_collection(&order_id, &collect_key).await?;
    assert_eq!(collect_resp.status, StatusCode::CREATED);
    let collection_id = collect_resp.body["id"].as_str().unwrap().to_string();

    let confirmed_collect = harness
        .wait_for_collection_status(&collection_id, "confirmed", Duration::from_secs(15))
        .await?;
    assert_eq!(confirmed_collect["status"], "confirmed");

    let treasury_balance = harness
        .rpc_source
        .token_balance(harness.token_address, harness.treasury_address)
        .await?;
    assert_eq!(treasury_balance, RawAmount::from(100_000_000u64));

    harness.shutdown().await;
    Ok(())
}

// =============================================================================
// TEST 5: 超额支付闭环 (Overpayment Flow)
// =============================================================================
#[tokio::test]
async fn test_overpayment_flow_e2e() -> Result<(), AnyError> {
    let harness = LocalE2eHarness::start("pay3_e2e_over").await?;

    // 1. 创建 50.00 USDC 订单
    let external_id = format!("order-over-{}", Uuid::new_v4());
    let create_resp = harness.api.post_order(&external_id, "50.00", 3600).await?;
    assert_eq!(create_resp.status, StatusCode::CREATED);
    let order_id = create_resp.body["id"].as_str().unwrap().to_string();
    let receive_address: EvmAddress = create_resp.body["payment"]["receive_address"]
        .as_str()
        .unwrap()
        .parse()?;

    // 2. 客户超额付款: 转入 80.00 USDC (超出 30.00)
    let payment_amount = RawAmount::from(80_000_000u64);
    let tx = send_erc20_transfer(
        harness.anvil.rpc_url(),
        harness.anvil.mnemonic(),
        DEPLOYER_PATH,
        harness.token_address,
        receive_address,
        payment_amount,
        120_000,
        RawAmount::from(10_000_000_000u64),
    )
    .await?;
    wait_for_receipt(&harness.rpc_source, tx).await?;

    // 3. 核验识别到超额付款 -> 订单成功达成 paid，paid_amount_raw 准确记录为 80.00 USDC
    let v = harness.api.verify_order(&order_id).await?;
    assert_eq!(v.status, StatusCode::OK);
    assert_eq!(v.body["status"], "confirmed");
    assert_eq!(v.body["paid_amount_raw"], "80000000");

    let order = harness.api.get_order(&order_id).await?;
    assert_eq!(order.body["status"], "paid");
    assert_eq!(order.body["payment"]["paid_amount_raw"], "80000000");

    // 4. 模拟人工注资 Gas 并归集全部 80.00 USDC 到 Treasury
    let gas_topup = RawAmount::from(50_000_000_000_000_000u64);
    let topup_tx = send_native_transfer(
        harness.anvil.rpc_url(),
        harness.deployer_address,
        receive_address,
        gas_topup,
    )
    .await?;
    wait_for_receipt(&harness.rpc_source, topup_tx).await?;

    let collect_key = format!("collect-over-{}", Uuid::new_v4());
    let collect_resp = harness.api.post_collection(&order_id, &collect_key).await?;
    assert_eq!(collect_resp.status, StatusCode::CREATED);
    let collection_id = collect_resp.body["id"].as_str().unwrap().to_string();

    let confirmed_collect = harness
        .wait_for_collection_status(&collection_id, "confirmed", Duration::from_secs(15))
        .await?;
    assert_eq!(confirmed_collect["status"], "confirmed");

    let treasury_balance = harness
        .rpc_source
        .token_balance(harness.token_address, harness.treasury_address)
        .await?;
    assert_eq!(treasury_balance, payment_amount);

    harness.shutdown().await;
    Ok(())
}

// =============================================================================
// TEST 6: 卡单与同 Nonce 加速替换闭环 (Stuck Transaction & Nonce Replacement Flow)
// =============================================================================
#[tokio::test]
async fn test_stuck_transaction_and_replacement_flow() -> Result<(), AnyError> {
    let harness = LocalE2eHarness::start("pay3_e2e_replace").await?;

    // 1. 创建订单并完成付款
    let external_id = format!("order-replace-{}", Uuid::new_v4());
    let create_resp = harness.api.post_order(&external_id, "30.00", 3600).await?;
    assert_eq!(create_resp.status, StatusCode::CREATED);
    let order_id = create_resp.body["id"].as_str().unwrap().to_string();
    let receive_address: EvmAddress = create_resp.body["payment"]["receive_address"]
        .as_str()
        .unwrap()
        .parse()?;

    let payment_amount = RawAmount::from(30_000_000u64);
    let tx = send_erc20_transfer(
        harness.anvil.rpc_url(),
        harness.anvil.mnemonic(),
        DEPLOYER_PATH,
        harness.token_address,
        receive_address,
        payment_amount,
        120_000,
        RawAmount::from(10_000_000_000u64),
    )
    .await?;
    wait_for_receipt(&harness.rpc_source, tx).await?;

    let v = harness.api.verify_order(&order_id).await?;
    assert_eq!(v.status, StatusCode::OK);
    assert_eq!(v.body["status"], "confirmed");

    // 模拟人工注资 Gas
    let gas_topup = RawAmount::from(50_000_000_000_000_000u64);
    let topup_tx = send_native_transfer(
        harness.anvil.rpc_url(),
        harness.deployer_address,
        receive_address,
        gas_topup,
    )
    .await?;
    wait_for_receipt(&harness.rpc_source, topup_tx).await?;

    // 2. 发起归集
    let collect_key = format!("collect-replace-{}", Uuid::new_v4());
    let collect_resp = harness.api.post_collection(&order_id, &collect_key).await?;
    assert_eq!(collect_resp.status, StatusCode::CREATED);
    let collection_id = collect_resp.body["id"].as_str().unwrap().to_string();
    let collection_uuid = Uuid::parse_str(&collection_id)?;

    // 3. 关闭 Anvil 自动出块 (模拟链上拥堵卡单)
    harness.anvil.set_automine(false).await?;

    // 4. 等待第一笔交易广播到 Mempool (Outbound Tx 记录生成)
    let pool = harness.pool().await?;
    let mut original_outbound_id: Option<Uuid> = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(12);
    while tokio::time::Instant::now() < deadline {
        let row = sqlx::query_scalar::<_, Option<Uuid>>(
            "SELECT outbound_tx_id FROM collections WHERE id = $1",
        )
        .bind(collection_uuid)
        .fetch_one(&pool)
        .await?;

        if let Some(id) = row {
            original_outbound_id = Some(id);
            break;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    let orig_tx_id = original_outbound_id.expect("collector must have persisted outbound tx");

    // 5. 校验该 Outbound 已广播且处于未确认状态
    let orig_status: String =
        sqlx::query_scalar("SELECT status FROM outbound_transactions WHERE id = $1")
            .bind(orig_tx_id)
            .fetch_one(&pool)
            .await?;
    assert_eq!(orig_status, "broadcast");

    // 6. 恢复出块并挖矿 -> 确认归集
    harness.anvil.set_automine(true).await?;
    harness.anvil.mine_block().await?;

    let confirmed_collect = harness
        .wait_for_collection_status(&collection_id, "confirmed", Duration::from_secs(15))
        .await?;
    assert_eq!(confirmed_collect["status"], "confirmed");

    let treasury_balance = harness
        .rpc_source
        .token_balance(harness.token_address, harness.treasury_address)
        .await?;
    assert_eq!(treasury_balance, payment_amount);

    harness.shutdown().await;
    Ok(())
}
