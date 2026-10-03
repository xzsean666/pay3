use std::{
    str::FromStr,
    sync::Arc,
};

use axum::{
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode},
};
use pay3::{
    api::{
        build_multi_token_router, ApiState, MultiTokenRouteEntry, OrderApiService,
        OrderResponseConfig, TokenSummary,
    },
    auth::JwtVerifier,
    config::AppConfig,
    db::repositories::{OrderPaymentDiagnostics, OrderView},
    domain::EvmAddress,
    health::{MetricsRecorder, StaticDependencyRegistry},
    services::orders::{CreateOrderInput, CreateOrderResult, OrderServiceError},
    wallet::{AddressDeriver, DeterministicFakeDeriver},
};
use serde_json::Value;
use tower::ServiceExt;
use uuid::Uuid;

const VALID_MULTI_TOKEN_YAML: &str = r#"
server:
  bind_addr: "127.0.0.1:8080"
  profile: "development"
database:
  url: "postgres://pay3:pay3@127.0.0.1:5432/pay3"
signer:
  mode: "fake"
  key_ref: "master-key"
auth:
  issuer: "https://auth.example.com"
  audience: "pay3-api"
  jwt_secret: "12345678901234567890123456789012"
tokens:
  - chain_id: 1
    token_address: "0xdAC17F958D2ee523a2206206994597C13D831ec7"
    token_symbol: "USDT"
    token_decimals: 6
    treasury_address: "0x1111111111111111111111111111111111111111"
    problem_funds_address: "0x2222222222222222222222222222222222222222"
    rpc_http_urls:
      - "http://127.0.0.1:8545"
    start_block: 1000
    min_confirmations: 12
    collection_method: "standard"
  - chain_id: 137
    token_address: "0x3c499c542cEF5E3811e1192ce70d8cC03d5c3359"
    token_symbol: "USDC"
    token_decimals: 6
    treasury_address: "0x3333333333333333333333333333333333333333"
    problem_funds_address: "0x4444444444444444444444444444444444444444"
    rpc_http_urls:
      - "http://127.0.0.1:8546"
    start_block: 2000
    min_confirmations: 20
    collection_method: "eip3009"
    token_name: "USD Coin"
    token_version: "2"
"#;

#[derive(Clone, Default)]
struct MockOrderService;

#[async_trait::async_trait]
impl OrderApiService for MockOrderService {
    async fn create_order(
        &self,
        _owner_sub: &str,
        _input: CreateOrderInput,
    ) -> Result<CreateOrderResult, OrderServiceError> {
        unimplemented!()
    }
    async fn get_order(
        &self,
        _owner_sub: &str,
        _id: Uuid,
    ) -> Result<Option<OrderView>, OrderServiceError> {
        unimplemented!()
    }
    async fn get_order_by_external_id(
        &self,
        _owner_sub: &str,
        _external_id: &str,
    ) -> Result<Option<OrderView>, OrderServiceError> {
        unimplemented!()
    }
    async fn accept_problem_payment(
        &self,
        _owner_sub: &str,
        _id: Uuid,
        _reason: &str,
        _note: Option<&str>,
    ) -> Result<Option<OrderView>, OrderServiceError> {
        unimplemented!()
    }
    async fn get_order_payment_diagnostics(
        &self,
        _owner_sub: &str,
        _id: Uuid,
        _problem_funds_address: EvmAddress,
    ) -> Result<Option<OrderPaymentDiagnostics>, OrderServiceError> {
        unimplemented!()
    }
}

#[test]
fn yaml_config_parses_multiple_tokens_correctly() {
    let config = AppConfig::from_yaml_str(VALID_MULTI_TOKEN_YAML)
        .expect("valid YAML should parse successfully");

    assert_eq!(config.tokens.len(), 2);

    // Token 0: ETH USDT
    let t0 = &config.tokens[0];
    assert_eq!(t0.chain.chain_id, 1);
    assert_eq!(t0.chain.token_symbol, "USDT");
    assert_eq!(t0.chain.token_decimals, 6);
    assert_eq!(
        t0.chain.token_address,
        EvmAddress::from_str("0xdAC17F958D2ee523a2206206994597C13D831ec7").unwrap()
    );
    assert_eq!(t0.chain.start_block, 1000);
    assert_eq!(t0.chain.min_confirmations, 12);
    assert_eq!(t0.collection.method.as_str(), "standard");
    assert_eq!(
        t0.collection.relayer_derivation_path,
        "m/44'/60'/99'/0/0"
    );

    // Token 1: Polygon USDC
    let t1 = &config.tokens[1];
    assert_eq!(t1.chain.chain_id, 137);
    assert_eq!(t1.chain.token_symbol, "USDC");
    assert_eq!(t1.chain.token_decimals, 6);
    assert_eq!(
        t1.chain.token_address,
        EvmAddress::from_str("0x3c499c542cEF5E3811e1192ce70d8cC03d5c3359").unwrap()
    );
    assert_eq!(t1.chain.start_block, 2000);
    assert_eq!(t1.chain.min_confirmations, 20);
    assert_eq!(t1.collection.method.as_str(), "eip3009");
    assert_eq!(
        t1.collection.relayer_derivation_path,
        "m/44'/60'/99'/0/1"
    );

    // Backward compatible fields match token 0
    assert_eq!(config.chain.chain_id, 1);
    assert_eq!(config.chain.token_symbol, "USDT");

    // Profile validation succeeds
    config.validate_profile().expect("profile validation should pass");
}

#[test]
fn yaml_config_rejects_duplicate_tokens_on_same_chain() {
    let duplicate_yaml = r#"
server:
  bind_addr: "127.0.0.1:8080"
database:
  url: "postgres://pay3:pay3@127.0.0.1:5432/pay3"
signer:
  mode: "fake"
  key_ref: "master-key"
auth:
  issuer: "https://auth.example.com"
  audience: "pay3-api"
  jwt_secret: "12345678901234567890123456789012"
tokens:
  - chain_id: 1
    token_address: "0xdAC17F958D2ee523a2206206994597C13D831ec7"
    treasury_address: "0x1111111111111111111111111111111111111111"
    problem_funds_address: "0x2222222222222222222222222222222222222222"
    rpc_http_urls:
      - "http://127.0.0.1:8545"
  - chain_id: 1
    token_address: "0xdAC17F958D2ee523a2206206994597C13D831ec7"
    treasury_address: "0x3333333333333333333333333333333333333333"
    problem_funds_address: "0x4444444444444444444444444444444444444444"
    rpc_http_urls:
      - "http://127.0.0.1:8545"
"#;

    let config = AppConfig::from_yaml_str(duplicate_yaml).expect("should parse");
    let err = config.validate_profile().expect_err("should reject duplicate token");
    assert!(
        err.to_string().contains("duplicate token instance"),
        "error message was: {err}"
    );
}

#[tokio::test]
async fn multi_token_relayer_derivation_paths_are_unique() {
    let deriver = DeterministicFakeDeriver::with_allowed_key_refs(
        "pay3-test",
        ["master-key".to_string()],
    )
    .expect("deriver");

    let config = AppConfig::from_yaml_str(VALID_MULTI_TOKEN_YAML).unwrap();

    let relayer_0 = deriver
        .derive_address("master-key", &config.tokens[0].collection.relayer_derivation_path)
        .await
        .unwrap();

    let relayer_1 = deriver
        .derive_address("master-key", &config.tokens[1].collection.relayer_derivation_path)
        .await
        .unwrap();

    assert_ne!(
        relayer_0, relayer_1,
        "Tokens MUST have distinct relayer addresses to avoid nonce collisions"
    );
}

#[tokio::test]
async fn multi_token_router_serves_tokens_list_and_routes_to_endpoints() {
    let verifier = Arc::new(
        JwtVerifier::new_hs256(
            "https://auth.example.com",
            "pay3-api",
            [("default", "12345678901234567890123456789012".to_string())],
        )
        .expect("verifier"),
    );

    let token0_addr = EvmAddress::from_str("0xdAC17F958D2ee523a2206206994597C13D831ec7").unwrap();
    let token1_addr = EvmAddress::from_str("0x3c499c542cEF5E3811e1192ce70d8cC03d5c3359").unwrap();
    let relayer_addr = EvmAddress::from_str("0x9999999999999999999999999999999999999999").unwrap();

    let static_deps = Arc::new(StaticDependencyRegistry::all_healthy());
    let metrics = MetricsRecorder::default();

    let mock_orders: Arc<dyn OrderApiService> = Arc::new(MockOrderService);

    let token0_response_config = OrderResponseConfig {
        token_decimals: 6,
        token_symbol: "USDT".to_string(),
        problem_funds_address: EvmAddress::from_str("0x2222222222222222222222222222222222222222").unwrap(),
    };
    let token1_response_config = OrderResponseConfig {
        token_decimals: 6,
        token_symbol: "USDC".to_string(),
        problem_funds_address: EvmAddress::from_str("0x4444444444444444444444444444444444444444").unwrap(),
    };

    let token0_state = ApiState::new_with_metrics(static_deps.clone(), metrics.clone())
        .with_orders(verifier.clone(), mock_orders.clone(), token0_response_config);
    let token1_state = ApiState::new_with_metrics(static_deps.clone(), metrics.clone())
        .with_orders(verifier.clone(), mock_orders.clone(), token1_response_config);

    let route_entries = vec![
        MultiTokenRouteEntry {
            chain_id: 1,
            token_address: token0_addr,
            summary: TokenSummary {
                chain_id: 1,
                token_address: token0_addr,
                token_symbol: "USDT".to_string(),
                token_decimals: 6,
                treasury_address: EvmAddress::from_str("0x1111111111111111111111111111111111111111").unwrap(),
                problem_funds_address: EvmAddress::from_str("0x2222222222222222222222222222222222222222").unwrap(),
                relayer_address: Some(relayer_addr),
                collection_method: "standard".to_string(),
                start_block: 1000,
                min_confirmations: 12,
            },
            state: token0_state,
        },
        MultiTokenRouteEntry {
            chain_id: 137,
            token_address: token1_addr,
            summary: TokenSummary {
                chain_id: 137,
                token_address: token1_addr,
                token_symbol: "USDC".to_string(),
                token_decimals: 6,
                treasury_address: EvmAddress::from_str("0x3333333333333333333333333333333333333333").unwrap(),
                problem_funds_address: EvmAddress::from_str("0x4444444444444444444444444444444444444444").unwrap(),
                relayer_address: Some(relayer_addr),
                collection_method: "eip3009".to_string(),
                start_block: 2000,
                min_confirmations: 20,
            },
            state: token1_state,
        },
    ];

    let global_state = ApiState::new_with_metrics(static_deps, metrics);
    let app = build_multi_token_router(global_state, route_entries);

    // 1. GET /v1/tokens should return the array of 2 tokens
    let req = Request::builder()
        .method(Method::GET)
        .uri("/v1/tokens")
        .body(Body::empty())
        .unwrap();

    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();
    let tokens = json["tokens"].as_array().expect("tokens should be an array");
    assert_eq!(tokens.len(), 2);
    assert_eq!(tokens[0]["chain_id"], 1);
    assert_eq!(tokens[0]["token_symbol"], "USDT");
    assert_eq!(tokens[1]["chain_id"], 137);
    assert_eq!(tokens[1]["token_symbol"], "USDC");

    // 2. GET /healthz and /readyz should work at root
    let req = Request::builder()
        .method(Method::GET)
        .uri("/healthz")
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    // 3. Requesting an unconfigured token returns 404
    let req = Request::builder()
        .method(Method::POST)
        .uri("/0x0000000000000000000000000000000000000000/v1/orders")
        .header("content-type", "application/json")
        .body(Body::from("{}"))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    // 4. Requesting configured token route without auth hits auth middleware (401 Unauthorized)
    let req = Request::builder()
        .method(Method::POST)
        .uri(format!("/{}/v1/orders", token0_addr))
        .header("content-type", "application/json")
        .body(Body::from("{}"))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    // 5. Requesting with chain_id prefix also hits auth middleware (401 Unauthorized)
    let req = Request::builder()
        .method(Method::POST)
        .uri(format!("/137/{}/v1/orders", token1_addr))
        .header("content-type", "application/json")
        .body(Body::from("{}"))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn single_token_router_preserves_root_compatibility() {
    let verifier = Arc::new(
        JwtVerifier::new_hs256(
            "https://auth.example.com",
            "pay3-api",
            [("default", "12345678901234567890123456789012".to_string())],
        )
        .expect("verifier"),
    );

    let token_addr = EvmAddress::from_str("0xdAC17F958D2ee523a2206206994597C13D831ec7").unwrap();
    let static_deps = Arc::new(StaticDependencyRegistry::all_healthy());
    let metrics = MetricsRecorder::default();
    let mock_orders: Arc<dyn OrderApiService> = Arc::new(MockOrderService);

    let token_response_config = OrderResponseConfig {
        token_decimals: 6,
        token_symbol: "USDT".to_string(),
        problem_funds_address: EvmAddress::from_str("0x2222222222222222222222222222222222222222").unwrap(),
    };

    let token_state = ApiState::new_with_metrics(static_deps.clone(), metrics.clone())
        .with_orders(verifier.clone(), mock_orders, token_response_config);

    let route_entries = vec![MultiTokenRouteEntry {
        chain_id: 1,
        token_address: token_addr,
        summary: TokenSummary {
            chain_id: 1,
            token_address: token_addr,
            token_symbol: "USDT".to_string(),
            token_decimals: 6,
            treasury_address: EvmAddress::from_str("0x1111111111111111111111111111111111111111").unwrap(),
            problem_funds_address: EvmAddress::from_str("0x2222222222222222222222222222222222222222").unwrap(),
            relayer_address: None,
            collection_method: "standard".to_string(),
            start_block: 1000,
            min_confirmations: 12,
        },
        state: token_state,
    }];

    let global_state = ApiState::new_with_metrics(static_deps, metrics);
    let app = build_multi_token_router(global_state, route_entries);

    // Single token router allows /v1/orders directly at root (hits auth -> 401)
    let req = Request::builder()
        .method(Method::POST)
        .uri("/v1/orders")
        .header("content-type", "application/json")
        .body(Body::from("{}"))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    // And also allows /{token}/v1/orders (hits auth -> 401)
    let req = Request::builder()
        .method(Method::POST)
        .uri(format!("/{}/v1/orders", token_addr))
        .header("content-type", "application/json")
        .body(Body::from("{}"))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[test]
fn yaml_interpolates_env_variables_and_defaults() {
    unsafe {
        std::env::set_var("TEST_PAY3_DB_URL", "postgres://user:pass@127.0.0.1:5432/injected_db");
        std::env::set_var("TEST_PAY3_SIGNER_TOKEN", "injected-secret-vault-token");
    }

    let yaml = r#"
server:
  bind_addr: "127.0.0.1:${TEST_PORT:-8080}"
database:
  url: "${TEST_PAY3_DB_URL}"
signer:
  mode: "external"
  key_ref: "master-key"
  endpoint: "https://vault.internal:8443"
  bearer_token: "${TEST_PAY3_SIGNER_TOKEN}"
auth:
  jwks_url: "https://auth.company.internal/.well-known/jwks.json"
tokens:
  - chain_id: 137
    token_address: "0xc2132D05D31c914a87C6611C10748AEb04B58e8F"
    treasury_address: "0x71C7656EC7ab88b098defB751B7401B5f6d8976F"
    rpc_http_urls:
      - "http://127.0.0.1:8545"
"#;

    let config = AppConfig::from_yaml_str(yaml).expect("should parse with interpolated env vars");
    assert_eq!(config.database.url, "postgres://user:pass@127.0.0.1:5432/injected_db");
    assert_eq!(
        config.signer.remote_bearer_token.as_deref(),
        Some("injected-secret-vault-token")
    );
    assert_eq!(config.http.bind_addr.port(), 8080);
}

#[test]
fn yaml_omitted_secrets_auto_fall_back_to_env() {
    unsafe {
        std::env::set_var("DATABASE_URL", "postgres://fallback_user:secret@127.0.0.1:5432/fallback_db");
        std::env::set_var("SIGNER_REMOTE_BEARER_TOKEN", "fallback-secret-bearer-token");
    }

    let yaml = r#"
server:
  bind_addr: "127.0.0.1:8080"
# database is completely omitted in YAML!
signer:
  mode: "external"
  key_ref: "master-key"
  endpoint: "https://vault.internal:8443"
  # bearer_token is completely omitted in YAML!
auth:
  jwks_url: "https://auth.company.internal/.well-known/jwks.json"
tokens:
  - chain_id: 137
    token_address: "0xc2132D05D31c914a87C6611C10748AEb04B58e8F"
    treasury_address: "0x71C7656EC7ab88b098defB751B7401B5f6d8976F"
    rpc_http_urls:
      - "http://127.0.0.1:8545"
"#;

    let config = AppConfig::from_yaml_str(yaml).expect("should fall back to env vars");
    assert_eq!(config.database.url, "postgres://fallback_user:secret@127.0.0.1:5432/fallback_db");
    assert_eq!(
        config.signer.remote_bearer_token.as_deref(),
        Some("fallback-secret-bearer-token")
    );
}

