use std::{
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use axum::{
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode, header},
};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use pay3::{
    api::{
        router_with_order_verify_service,
        verify::{OrderVerifyApiService, OrderVerifyError, OrderVerifyResult, OrderVerifyStatus},
    },
    auth::{Audience, Claims, JwtVerifier, ORDERS_READ_SCOPE, ORDERS_VERIFY_SCOPE},
    domain::RawAmount,
    health::StaticDependencyRegistry,
};
use serde_json::Value;
use tower::ServiceExt;
use uuid::Uuid;

const ISSUER: &str = "pay3-test-issuer";
const AUDIENCE: &str = "pay3-api";
const KID: &str = "test-key";
const SECRET: &str = "test-secret-with-enough-entropy";

#[tokio::test]
async fn verify_rejects_missing_token_before_service_call() {
    let service = Arc::new(FakeVerifyService::ok(success_result()));

    let response = request_verify(service.clone(), valid_order_path(), None).await;

    assert_eq!(response.status, StatusCode::UNAUTHORIZED);
    assert_eq!(response.body["error"]["code"], "unauthorized");
    assert_eq!(service.calls(), Vec::<Uuid>::new());
}

#[tokio::test]
async fn verify_rejects_token_without_scope_before_service_call() {
    let service = Arc::new(FakeVerifyService::ok(success_result()));

    let response = request_verify(
        service.clone(),
        valid_order_path(),
        Some(token_without_scope()),
    )
    .await;

    assert_eq!(response.status, StatusCode::FORBIDDEN);
    assert_eq!(response.body["error"]["code"], "forbidden");
    assert_eq!(service.calls(), Vec::<Uuid>::new());
}

#[tokio::test]
async fn verify_rejects_insufficient_scope_before_service_call() {
    let service = Arc::new(FakeVerifyService::ok(success_result()));

    let response = request_verify(
        service.clone(),
        valid_order_path(),
        Some(token(ORDERS_READ_SCOPE)),
    )
    .await;

    assert_eq!(response.status, StatusCode::FORBIDDEN);
    assert_eq!(response.body["error"]["code"], "forbidden");
    assert_eq!(service.calls(), Vec::<Uuid>::new());
}

#[tokio::test]
async fn verify_with_valid_scope_calls_service_and_returns_json() {
    let service = Arc::new(FakeVerifyService::ok(success_result()));

    let response = request_verify(
        service.clone(),
        valid_order_path(),
        Some(token(ORDERS_VERIFY_SCOPE)),
    )
    .await;

    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.body["order_id"], order_id().to_string());
    assert_eq!(response.body["status"], "confirmed");
    assert_eq!(response.body["matched_payments"], 2);
    assert_eq!(response.body["paid_amount_raw"], "12340000");
    assert_eq!(response.body["confirmations"], 12);
    assert_eq!(response.body["complete_to_block"], 123456);
    assert_eq!(service.calls(), vec![order_id()]);
}

#[tokio::test]
async fn verify_rejects_invalid_order_id_before_service_call() {
    let service = Arc::new(FakeVerifyService::ok(success_result()));

    let response = request_verify(
        service.clone(),
        "/v1/orders/not-a-uuid/verify",
        Some(token(ORDERS_VERIFY_SCOPE)),
    )
    .await;

    assert_eq!(response.status, StatusCode::BAD_REQUEST);
    assert_eq!(response.body["error"]["code"], "invalid_order_id");
    assert_eq!(service.calls(), Vec::<Uuid>::new());
}

#[tokio::test]
async fn verify_maps_service_not_found_to_404() {
    let service = Arc::new(FakeVerifyService::err(OrderVerifyError::NotFound));

    let response = request_verify(
        service,
        valid_order_path(),
        Some(token(ORDERS_VERIFY_SCOPE)),
    )
    .await;

    assert_eq!(response.status, StatusCode::NOT_FOUND);
    assert_eq!(response.body["error"]["code"], "not_found");
}

#[tokio::test]
async fn verify_maps_coverage_failure_to_503() {
    let service = Arc::new(FakeVerifyService::err(
        OrderVerifyError::CoverageInsufficient,
    ));

    let response = request_verify(
        service,
        valid_order_path(),
        Some(token(ORDERS_VERIFY_SCOPE)),
    )
    .await;

    assert_eq!(response.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        response.body["error"]["code"],
        "log_store_coverage_insufficient"
    );
}

#[tokio::test]
async fn verify_maps_dependency_unavailable_to_503() {
    let service = Arc::new(FakeVerifyService::err(
        OrderVerifyError::DependencyUnavailable("kvdb unavailable".to_string()),
    ));

    let response = request_verify(
        service,
        valid_order_path(),
        Some(token(ORDERS_VERIFY_SCOPE)),
    )
    .await;

    assert_eq!(response.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(response.body["error"]["code"], "order_verify_unavailable");
}

struct JsonResponse {
    status: StatusCode,
    body: Value,
}

async fn request_verify(
    service: Arc<FakeVerifyService>,
    uri: &str,
    authorization: Option<String>,
) -> JsonResponse {
    let app = router_with_order_verify_service(
        StaticDependencyRegistry::all_healthy(),
        verifier(),
        service,
    );
    let mut builder = Request::builder().method(Method::POST).uri(uri);
    if let Some(authorization) = authorization {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {authorization}"));
    }

    let response = app
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    let body = serde_json::from_slice(&body).unwrap();

    JsonResponse { status, body }
}

fn verifier() -> JwtVerifier {
    JwtVerifier::new_hs256(ISSUER, AUDIENCE, [(KID, SECRET)]).unwrap()
}

fn token(scopes: &str) -> String {
    token_with_scope_claim(Some(scopes.to_string()))
}

fn token_without_scope() -> String {
    token_with_scope_claim(None)
}

fn token_with_scope_claim(scope: Option<String>) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let claims = Claims {
        exp: now + 3600,
        nbf: now.saturating_sub(10),
        iat: now,
        iss: ISSUER.to_string(),
        aud: Audience::One(AUDIENCE.to_string()),
        sub: "merchant-1".to_string(),
        scope,
        scopes: None,
        scp: None,
    };
    let mut header = Header::new(Algorithm::HS256);
    header.kid = Some(KID.to_string());
    encode(
        &header,
        &claims,
        &EncodingKey::from_secret(SECRET.as_bytes()),
    )
    .unwrap()
}

fn valid_order_path() -> &'static str {
    "/v1/orders/00000000-0000-0000-0000-00000000002a/verify"
}

fn order_id() -> Uuid {
    Uuid::from_u128(42)
}

fn success_result() -> OrderVerifyResult {
    OrderVerifyResult {
        order_id: order_id(),
        status: OrderVerifyStatus::Confirmed,
        matched_payments: 2,
        paid_amount_raw: RawAmount::from(12_340_000),
        confirmations: 12,
        complete_to_block: Some(123456),
    }
}

struct FakeVerifyService {
    result: Mutex<Option<Result<OrderVerifyResult, OrderVerifyError>>>,
    calls: Mutex<Vec<Uuid>>,
}

impl FakeVerifyService {
    fn ok(result: OrderVerifyResult) -> Self {
        Self {
            result: Mutex::new(Some(Ok(result))),
            calls: Mutex::new(Vec::new()),
        }
    }

    fn err(error: OrderVerifyError) -> Self {
        Self {
            result: Mutex::new(Some(Err(error))),
            calls: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<Uuid> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl OrderVerifyApiService for FakeVerifyService {
    async fn verify_order(&self, order_id: Uuid) -> Result<OrderVerifyResult, OrderVerifyError> {
        self.calls.lock().unwrap().push(order_id);
        self.result
            .lock()
            .unwrap()
            .take()
            .expect("fake verify result must be configured")
    }
}
