pub mod verify;
mod verify_service;

use std::{
    env,
    sync::{Arc, Mutex},
    time::{Duration as StdDuration, Instant},
};

use async_trait::async_trait;
use axum::{
    Json, Router,
    body::Body,
    extract::{Path, Request, State, rejection::JsonRejection},
    http::{HeaderMap, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::{
    auth::{
        AuthError, COLLECTIONS_CREATE_SCOPE, COLLECTIONS_READ_SCOPE, JwtVerifier,
        ORDERS_CREATE_SCOPE, ORDERS_READ_SCOPE, Principal,
    },
    config::AppConfig,
    db::repositories::{CollectionRecord, CollectionRecordStatus, OrderView, RepositoryError},
    domain::{OrderStatus, TokenAmount},
    error::ApiError,
    health::{
        DependencyCheck, DependencyName, DependencyRegistry, HealthzResponse, MetricsRecorder,
        ReadinessReport, SharedDependencyRegistry, StaticDependencyRegistry,
    },
    services::{
        collections::{
            AuditContext, CollectionAmount, CollectionService, CollectionServiceError,
            CreateCollectionInput, CreateCollectionOutcome, CreateCollectionResult,
        },
        orders::{
            CreateOrderInput, CreateOrderResult, CreateOrderServiceOutcome, OrderService,
            OrderServiceError,
        },
    },
};

#[derive(Clone)]
struct ApiState {
    dependencies: SharedDependencyRegistry,
    metrics: MetricsRecorder,
    auth: Option<Arc<JwtVerifier>>,
    orders: Option<Arc<dyn OrderApiService>>,
    order_verify: Option<Arc<dyn verify::OrderVerifyApiService>>,
    collections: Option<Arc<dyn CollectionApiService>>,
    order_response_config: Option<OrderResponseConfig>,
    rate_limiter: Option<Arc<FixedWindowRateLimiter>>,
}

impl ApiState {
    fn new(dependencies: SharedDependencyRegistry) -> Self {
        Self::new_with_metrics(dependencies, MetricsRecorder::default())
    }

    fn new_with_metrics(dependencies: SharedDependencyRegistry, metrics: MetricsRecorder) -> Self {
        Self {
            dependencies,
            metrics,
            auth: None,
            orders: None,
            order_verify: None,
            collections: None,
            order_response_config: None,
            rate_limiter: FixedWindowRateLimiter::from_env().map(Arc::new),
        }
    }

    fn readiness(&self) -> ReadinessReport {
        self.dependencies.readiness()
    }

    fn with_orders(
        mut self,
        auth: Arc<JwtVerifier>,
        orders: Arc<dyn OrderApiService>,
        order_response_config: OrderResponseConfig,
    ) -> Self {
        self.auth = Some(auth);
        self.orders = Some(orders);
        self.order_response_config = Some(order_response_config);
        self
    }

    fn with_order_verify(
        mut self,
        auth: Arc<JwtVerifier>,
        order_verify: Arc<dyn verify::OrderVerifyApiService>,
    ) -> Self {
        self.auth = Some(auth);
        self.order_verify = Some(order_verify);
        self
    }

    fn with_collections(
        mut self,
        auth: Arc<JwtVerifier>,
        collections: Arc<dyn CollectionApiService>,
    ) -> Self {
        self.auth = Some(auth);
        self.collections = Some(collections);
        self
    }

    #[cfg(test)]
    fn with_rate_limit_per_minute(mut self, limit: Option<u32>) -> Self {
        self.rate_limiter = limit.map(FixedWindowRateLimiter::per_minute).map(Arc::new);
        self
    }

    fn auth(&self) -> Result<&JwtVerifier, ApiError> {
        self.auth
            .as_deref()
            .ok_or_else(|| ApiError::internal("auth verifier is not configured"))
    }

    fn orders(&self) -> Result<&dyn OrderApiService, ApiError> {
        self.orders.as_deref().ok_or_else(|| {
            ApiError::service_unavailable("orders_unavailable", "orders service is not configured")
        })
    }

    fn order_response_config(&self) -> Result<&OrderResponseConfig, ApiError> {
        self.order_response_config
            .as_ref()
            .ok_or_else(|| ApiError::internal("order response config is not configured"))
    }

    fn order_verify(&self) -> Result<&dyn verify::OrderVerifyApiService, ApiError> {
        self.order_verify.as_deref().ok_or_else(|| {
            ApiError::service_unavailable(
                "order_verify_unavailable",
                "order verify service is not configured",
            )
        })
    }

    fn collections(&self) -> Result<&dyn CollectionApiService, ApiError> {
        self.collections.as_deref().ok_or_else(|| {
            ApiError::service_unavailable(
                "collections_unavailable",
                "collections service is not configured",
            )
        })
    }
}

#[derive(Debug)]
struct FixedWindowRateLimiter {
    limit_per_minute: u32,
    state: Mutex<FixedWindowState>,
}

#[derive(Debug)]
struct FixedWindowState {
    window_started_at: Instant,
    used: u32,
}

impl FixedWindowRateLimiter {
    fn per_minute(limit_per_minute: u32) -> Self {
        Self {
            limit_per_minute,
            state: Mutex::new(FixedWindowState {
                window_started_at: Instant::now(),
                used: 0,
            }),
        }
    }

    fn from_env() -> Option<Self> {
        let value = env::var("API_RATE_LIMIT_PER_MINUTE")
            .ok()
            .or_else(|| env::var("PAY3_API_RATE_LIMIT_PER_MINUTE").ok())?;
        let limit = value.trim().parse::<u32>().ok()?;
        (limit > 0).then(|| Self::per_minute(limit))
    }

    fn allow(&self) -> bool {
        if self.limit_per_minute == 0 {
            return false;
        }

        let mut state = self.state.lock().expect("api rate limiter mutex poisoned");
        if state.window_started_at.elapsed() >= StdDuration::from_secs(60) {
            state.window_started_at = Instant::now();
            state.used = 0;
        }

        if state.used >= self.limit_per_minute {
            return false;
        }

        state.used = state.used.saturating_add(1);
        true
    }

    fn limit_per_minute(&self) -> u32 {
        self.limit_per_minute
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrderResponseConfig {
    pub token_decimals: u8,
    pub token_symbol: String,
}

impl OrderResponseConfig {
    pub fn from_config(config: &AppConfig) -> Self {
        Self {
            token_decimals: config.chain.token_decimals,
            token_symbol: config.chain.token_symbol.clone(),
        }
    }
}

#[async_trait]
pub trait OrderApiService: Send + Sync {
    async fn create_order(
        &self,
        input: CreateOrderInput,
    ) -> Result<CreateOrderResult, OrderServiceError>;

    async fn get_order(&self, id: Uuid) -> Result<Option<OrderView>, OrderServiceError>;

    async fn get_order_by_external_id(
        &self,
        external_id: &str,
    ) -> Result<Option<OrderView>, OrderServiceError>;
}

#[async_trait]
pub trait CollectionApiService: Send + Sync {
    async fn create_collection(
        &self,
        input: CreateCollectionInput,
    ) -> Result<CreateCollectionResult, CollectionServiceError>;

    async fn get_collection(
        &self,
        id: Uuid,
    ) -> Result<Option<CollectionRecord>, CollectionServiceError>;
}

#[async_trait]
impl<R, D, H, C, I> OrderApiService for OrderService<R, D, H, C, I>
where
    R: crate::db::repositories::OrderRepository,
    D: crate::wallet::AddressDeriver,
    H: crate::services::orders::OrderChainHeadReader,
    C: crate::services::orders::Clock,
    I: crate::services::orders::IdGenerator,
{
    async fn create_order(
        &self,
        input: CreateOrderInput,
    ) -> Result<CreateOrderResult, OrderServiceError> {
        OrderService::create_order(self, input).await
    }

    async fn get_order(&self, id: Uuid) -> Result<Option<OrderView>, OrderServiceError> {
        OrderService::get_order(self, id).await
    }

    async fn get_order_by_external_id(
        &self,
        external_id: &str,
    ) -> Result<Option<OrderView>, OrderServiceError> {
        OrderService::get_order_by_external_id(self, external_id).await
    }
}

#[async_trait]
impl<O, C, B, A, S, H, G, I> CollectionApiService for CollectionService<O, C, B, A, S, H, G, I>
where
    O: crate::db::repositories::OrderRepository,
    C: crate::db::repositories::CollectionRepository,
    B: crate::db::repositories::OutboundRepository,
    A: crate::db::repositories::AuditRepository,
    S: crate::signer::SignerProvider,
    H: crate::chain::Erc20ChainClient + crate::chain::Eip1559FeeEstimator,
    G: crate::services::collections::PrefundedGasChecker,
    I: crate::services::orders::IdGenerator,
{
    async fn create_collection(
        &self,
        input: CreateCollectionInput,
    ) -> Result<CreateCollectionResult, CollectionServiceError> {
        CollectionService::create_collection(self, input).await
    }

    async fn get_collection(
        &self,
        id: Uuid,
    ) -> Result<Option<CollectionRecord>, CollectionServiceError> {
        CollectionService::get_collection(self, id).await
    }
}

pub fn router(config: AppConfig) -> Router {
    drop(config);
    router_with_registry(StaticDependencyRegistry::from_checks(vec![
        DependencyCheck::healthy(DependencyName::WorkerLease),
        DependencyCheck::failed(
            DependencyName::Db,
            "runtime services were not bootstrapped; use runtime::build_api_router",
        ),
        DependencyCheck::failed(
            DependencyName::Migration,
            "runtime services were not bootstrapped; use runtime::build_api_router",
        ),
        DependencyCheck::failed(
            DependencyName::RpcChainId,
            "runtime services were not bootstrapped; use runtime::build_api_router",
        ),
        DependencyCheck::failed(
            DependencyName::Kvdb,
            "runtime services were not bootstrapped; use runtime::build_api_router",
        ),
        DependencyCheck::failed(
            DependencyName::Signer,
            "runtime services were not bootstrapped; use runtime::build_api_router",
        ),
    ]))
}

pub fn router_with_registry<R>(registry: R) -> Router
where
    R: DependencyRegistry,
{
    router_with_shared_registry(Arc::new(registry))
}

pub fn router_with_shared_registry(registry: SharedDependencyRegistry) -> Router {
    let state = ApiState::new(registry);
    router_from_state(state)
}

pub fn router_with_order_service<R>(
    registry: R,
    auth: JwtVerifier,
    orders: Arc<dyn OrderApiService>,
    order_response_config: OrderResponseConfig,
) -> Router
where
    R: DependencyRegistry,
{
    let state = ApiState::new(Arc::new(registry)).with_orders(
        Arc::new(auth),
        orders,
        order_response_config,
    );
    router_from_state(state)
}

pub fn router_with_order_verify_service<R>(
    registry: R,
    auth: JwtVerifier,
    order_verify: Arc<dyn verify::OrderVerifyApiService>,
) -> Router
where
    R: DependencyRegistry,
{
    let state = ApiState::new(Arc::new(registry)).with_order_verify(Arc::new(auth), order_verify);
    router_from_state(state)
}

pub fn router_with_collection_service<R>(
    registry: R,
    auth: JwtVerifier,
    collections: Arc<dyn CollectionApiService>,
) -> Router
where
    R: DependencyRegistry,
{
    let state = ApiState::new(Arc::new(registry)).with_collections(Arc::new(auth), collections);
    router_from_state(state)
}

pub fn router_with_runtime_services<R>(
    registry: R,
    auth: JwtVerifier,
    orders: Arc<dyn OrderApiService>,
    order_verify: Arc<dyn verify::OrderVerifyApiService>,
    collections: Arc<dyn CollectionApiService>,
    order_response_config: OrderResponseConfig,
) -> Router
where
    R: DependencyRegistry,
{
    router_with_runtime_services_and_metrics(
        registry,
        MetricsRecorder::default(),
        auth,
        orders,
        order_verify,
        collections,
        order_response_config,
    )
}

pub fn router_with_runtime_services_and_metrics<R>(
    registry: R,
    metrics: MetricsRecorder,
    auth: JwtVerifier,
    orders: Arc<dyn OrderApiService>,
    order_verify: Arc<dyn verify::OrderVerifyApiService>,
    collections: Arc<dyn CollectionApiService>,
    order_response_config: OrderResponseConfig,
) -> Router
where
    R: DependencyRegistry,
{
    let auth = Arc::new(auth);
    let state = ApiState::new_with_metrics(Arc::new(registry), metrics)
        .with_orders(auth.clone(), orders, order_response_config)
        .with_order_verify(auth.clone(), order_verify)
        .with_collections(auth, collections);
    router_from_state(state)
}

fn router_from_state(state: ApiState) -> Router {
    let include_order_routes = state.orders.is_some();
    let include_order_verify_route = state.order_verify.is_some();
    let include_collection_routes = state.collections.is_some();
    let mut router = Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/metrics", get(metrics))
        .fallback(not_found);

    if include_order_routes {
        router = router
            .route("/v1/orders", post(create_order))
            .route(
                "/v1/orders/by-external-id/{external_id}",
                get(get_order_by_external_id),
            )
            .route("/v1/orders/{id}", get(get_order));
    }

    if include_order_verify_route {
        router = router.route("/v1/orders/{id}/verify", post(verify::verify_order));
    }

    if include_collection_routes {
        router = router
            .route("/v1/collections", post(create_collection))
            .route("/v1/collections/{id}", get(get_collection));
    }

    router
        .layer(middleware::from_fn_with_state(
            state.clone(),
            enforce_api_rate_limit,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            record_request_latency,
        ))
        .with_state(state)
}

async fn healthz() -> Json<HealthzResponse> {
    Json(HealthzResponse::default())
}

async fn readyz(State(state): State<ApiState>) -> Response {
    let readiness = state.readiness();
    let status = if readiness.is_ready() {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };

    (status, Json(readiness)).into_response()
}

async fn metrics(State(state): State<ApiState>) -> Response {
    let readiness = state.readiness();
    let body = state.metrics.render_prometheus(&readiness);

    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        body,
    )
        .into_response()
}

#[derive(Debug, Deserialize)]
struct CreateOrderRequest {
    external_id: String,
    amount: String,
    ttl_seconds: u64,
    #[serde(default)]
    metadata: Option<Value>,
}

#[derive(Debug, Serialize)]
struct OrderResponse {
    id: Uuid,
    external_id: String,
    status: OrderStatus,
    payment: OrderPaymentResponse,
}

#[derive(Debug, Serialize)]
struct OrderPaymentResponse {
    chain_id: u64,
    token_address: String,
    token_symbol: String,
    token_decimals: u8,
    amount: String,
    amount_raw: String,
    paid_amount_raw: String,
    receive_address: String,
    child_account_id: Uuid,
    derivation_path: String,
    expires_at: time::OffsetDateTime,
    monitor_until: time::OffsetDateTime,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateCollectionRequest {
    order_id: String,
    amount: String,
    idempotency_key: String,
}

#[derive(Debug, Serialize)]
struct CollectionResponse {
    id: Uuid,
    order_id: Uuid,
    chain_id: u64,
    token_address: String,
    status: CollectionRecordStatus,
    from_address: String,
    to_address: String,
    amount_raw: Option<String>,
    outbound_tx_id: Option<Uuid>,
    attempt_count: u32,
    error: Option<String>,
    created_at: time::OffsetDateTime,
    updated_at: time::OffsetDateTime,
}

async fn create_order(
    State(state): State<ApiState>,
    headers: HeaderMap,
    payload: Result<Json<CreateOrderRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<OrderResponse>), ApiError> {
    require_scope(&state, &headers, ORDERS_CREATE_SCOPE)?;
    let Json(payload) = payload.map_err(json_rejection)?;
    let config = state.order_response_config()?;

    let amount = TokenAmount::parse(&payload.amount, config.token_decimals)
        .map_err(|error| ApiError::bad_request("invalid_amount", error.to_string()))?;
    let metadata = match payload.metadata {
        Some(Value::Object(map)) => Value::Object(map),
        Some(Value::Null) | None => Value::Object(Map::new()),
        Some(_) => {
            return Err(ApiError::bad_request(
                "invalid_metadata",
                "metadata must be a JSON object when provided",
            ));
        }
    };

    let input = CreateOrderInput::new(payload.external_id, amount.raw, payload.ttl_seconds)
        .with_metadata(metadata);
    let result = state
        .orders()?
        .create_order(input)
        .await
        .map_err(order_service_error_to_api)?;
    let status = match result.outcome {
        CreateOrderServiceOutcome::Created => StatusCode::CREATED,
        CreateOrderServiceOutcome::Existing => StatusCode::OK,
    };
    let response = order_response(result.view, config);

    Ok((status, Json(response)))
}

async fn get_order(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<OrderResponse>, ApiError> {
    require_scope(&state, &headers, ORDERS_READ_SCOPE)?;
    let id = parse_order_id(&id)?;
    let config = state.order_response_config()?;
    let Some(view) = state
        .orders()?
        .get_order(id)
        .await
        .map_err(order_service_error_to_api)?
    else {
        return Err(ApiError::not_found("order not found"));
    };

    Ok(Json(order_response(view, config)))
}

async fn get_order_by_external_id(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(external_id): Path<String>,
) -> Result<Json<OrderResponse>, ApiError> {
    require_scope(&state, &headers, ORDERS_READ_SCOPE)?;
    let config = state.order_response_config()?;
    let Some(view) = state
        .orders()?
        .get_order_by_external_id(&external_id)
        .await
        .map_err(order_service_error_to_api)?
    else {
        return Err(ApiError::not_found("order not found"));
    };

    Ok(Json(order_response(view, config)))
}

async fn create_collection(
    State(state): State<ApiState>,
    headers: HeaderMap,
    payload: Result<Json<CreateCollectionRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<CollectionResponse>), ApiError> {
    let principal = require_scope(&state, &headers, COLLECTIONS_CREATE_SCOPE)?;
    let Json(payload) = payload.map_err(json_rejection)?;
    let order_id = parse_order_id(&payload.order_id)?;
    let amount = parse_collection_amount(&payload.amount)?;
    let audit = AuditContext {
        request_id: request_id_from_headers(&headers),
        principal_sub: Some(principal.subject),
        scopes: principal
            .scopes
            .iter()
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>(),
    };

    let result = state
        .collections()?
        .create_collection(CreateCollectionInput {
            order_id,
            amount,
            idempotency_key: payload.idempotency_key,
            audit,
        })
        .await
        .map_err(collection_service_error_to_api)?;
    let status = match result.outcome {
        CreateCollectionOutcome::Created => StatusCode::CREATED,
        CreateCollectionOutcome::Existing => StatusCode::OK,
    };

    Ok((status, Json(collection_response(result.collection))))
}

async fn get_collection(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<CollectionResponse>, ApiError> {
    require_scope(&state, &headers, COLLECTIONS_READ_SCOPE)?;
    let id = parse_collection_id(&id)?;
    let Some(collection) = state
        .collections()?
        .get_collection(id)
        .await
        .map_err(collection_service_error_to_api)?
    else {
        return Err(ApiError::not_found("collection not found"));
    };

    Ok(Json(collection_response(collection)))
}

async fn not_found() -> ApiError {
    ApiError::not_found("route not found")
}

fn order_response(view: OrderView, config: &OrderResponseConfig) -> OrderResponse {
    let amount = TokenAmount::from_raw(view.order.expected_amount_raw, config.token_decimals);
    OrderResponse {
        id: view.order.id,
        external_id: view.order.external_id,
        status: view.order.status,
        payment: OrderPaymentResponse {
            chain_id: view.order.chain_id,
            token_address: view.order.token_address.to_lower_hex(),
            token_symbol: config.token_symbol.clone(),
            token_decimals: config.token_decimals,
            amount: amount.to_decimal_string(),
            amount_raw: view.order.expected_amount_raw.to_string(),
            paid_amount_raw: view.order.paid_amount_raw.to_string(),
            receive_address: view.order.receive_address.to_lower_hex(),
            child_account_id: view.order.child_account_id,
            derivation_path: view.child_account.derivation_path,
            expires_at: view.order.expires_at,
            monitor_until: view.order.monitor_until,
        },
    }
}

fn collection_response(collection: CollectionRecord) -> CollectionResponse {
    CollectionResponse {
        id: collection.id,
        order_id: collection.order_id,
        chain_id: collection.chain_id,
        token_address: collection.token_address.to_lower_hex(),
        status: collection.status,
        from_address: collection.from_address.to_lower_hex(),
        to_address: collection.to_address.to_lower_hex(),
        amount_raw: collection.amount_raw.map(|amount| amount.to_string()),
        outbound_tx_id: collection.outbound_tx_id,
        attempt_count: collection.attempt_count,
        error: collection.error,
        created_at: collection.created_at,
        updated_at: collection.updated_at,
    }
}

fn parse_collection_amount(value: &str) -> Result<CollectionAmount, ApiError> {
    if value.trim().eq_ignore_ascii_case("max") {
        Ok(CollectionAmount::Max)
    } else {
        Err(ApiError::bad_request(
            "invalid_collection_amount",
            "collection amount must be \"max\" for MVP",
        ))
    }
}

fn require_scope(
    state: &ApiState,
    headers: &HeaderMap,
    required_scope: &str,
) -> Result<Principal, ApiError> {
    let authorization = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok());
    state
        .auth()?
        .verify_bearer_with_scope(authorization, required_scope)
        .map_err(auth_error_to_api)
}

fn auth_error_to_api(error: AuthError) -> ApiError {
    match error {
        AuthError::MissingScope(_) | AuthError::InsufficientScope(_) => {
            ApiError::forbidden(error.to_string())
        }
        _ => ApiError::unauthorized(error.to_string()),
    }
}

fn order_service_error_to_api(error: OrderServiceError) -> ApiError {
    match error {
        OrderServiceError::InvalidArgument { field, message } => {
            ApiError::bad_request("invalid_order", format!("{field}: {message}"))
        }
        OrderServiceError::Repository(RepositoryError::IdempotencyConflict { .. }) => {
            ApiError::conflict("idempotency_conflict", error.to_string())
        }
        OrderServiceError::Repository(RepositoryError::NotFound { .. })
        | OrderServiceError::OrderViewMissing { .. } => ApiError::not_found("order not found"),
        OrderServiceError::ChainHeadUnavailable { .. } => {
            ApiError::service_unavailable("chain_head_unavailable", error.to_string())
        }
        _ => ApiError::internal(error.to_string()),
    }
}

fn collection_service_error_to_api(error: CollectionServiceError) -> ApiError {
    let message = error.to_string();
    match &error {
        CollectionServiceError::InvalidArgument { field, message } => {
            ApiError::bad_request("invalid_collection", format!("{field}: {message}"))
        }
        CollectionServiceError::OrderNotFound { .. } => ApiError::not_found("order not found"),
        CollectionServiceError::Repository(error)
            if matches!(error.as_ref(), RepositoryError::NotFound { .. }) =>
        {
            ApiError::not_found("order not found")
        }
        CollectionServiceError::Repository(error)
            if matches!(error.as_ref(), RepositoryError::IdempotencyConflict { .. }) =>
        {
            ApiError::conflict("idempotency_conflict", message)
        }
        CollectionServiceError::OrderNotCollectable { .. }
        | CollectionServiceError::OrderStreamMismatch { .. }
        | CollectionServiceError::ZeroCollectionAmount { .. }
        | CollectionServiceError::InsufficientTokenBalance { .. } => {
            ApiError::conflict("collection_not_allowed", message)
        }
        CollectionServiceError::Chain(_)
        | CollectionServiceError::Signer(_)
        | CollectionServiceError::GasFunding(_) => {
            ApiError::service_unavailable("collection_dependency_unavailable", message)
        }
        _ => ApiError::internal(message),
    }
}

fn json_rejection(rejection: JsonRejection) -> ApiError {
    ApiError::bad_request("invalid_json", rejection.to_string())
}

fn request_id_from_headers(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

pub(super) fn parse_order_id(value: &str) -> Result<Uuid, ApiError> {
    parse_uuid(value, "invalid_order_id", "invalid order id")
}

fn parse_collection_id(value: &str) -> Result<Uuid, ApiError> {
    parse_uuid(value, "invalid_collection_id", "invalid collection id")
}

fn parse_uuid(value: &str, code: &'static str, message: &'static str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(value).map_err(|_| ApiError::bad_request(code, message))
}

async fn record_request_latency(
    State(state): State<ApiState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let started_at = Instant::now();
    let response = next.run(request).await;
    state.metrics.record_request(started_at.elapsed());
    response
}

async fn enforce_api_rate_limit(
    State(state): State<ApiState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let path = request.uri().path().to_string();
    if !path.starts_with("/v1/") {
        return next.run(request).await;
    }

    let Some(limiter) = &state.rate_limiter else {
        return next.run(request).await;
    };

    if limiter.allow() {
        return next.run(request).await;
    }

    let mut error = ApiError::too_many_requests("rate_limited", "API rate limit exceeded")
        .with_detail("limit_per_minute", u64::from(limiter.limit_per_minute()));
    if let Some(request_id) = request_id_from_headers(request.headers()) {
        error = error.with_request_id(request_id);
    }
    error.into_response()
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        sync::{Arc, Mutex},
        time::{SystemTime, UNIX_EPOCH},
    };

    use async_trait::async_trait;
    use axum::{
        body::{Body, to_bytes},
        http::{Method, Request, StatusCode, header},
    };
    use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
    use serde_json::{Value, json};
    use time::{Duration, OffsetDateTime};
    use tower::ServiceExt;
    use uuid::Uuid;

    use super::{
        ApiState, CollectionApiService, OrderApiService, OrderResponseConfig, router_from_state,
        router_with_collection_service, router_with_order_service, router_with_registry,
    };
    use crate::{
        auth::{
            Audience, COLLECTIONS_CREATE_SCOPE, COLLECTIONS_READ_SCOPE, Claims, JwtVerifier,
            ORDERS_CREATE_SCOPE, ORDERS_READ_SCOPE,
        },
        db::repositories::{
            ChildAccountRecord, CollectionRecord, CollectionRecordStatus, OrderRecord, OrderView,
            PaymentWindowRecord, RepositoryError,
        },
        domain::{BlockHash, ChainBlockRef, DerivationSegment, EvmAddress, OrderStatus, RawAmount},
        health::{DependencyCheck, DependencyName, StaticDependencyRegistry},
        services::collections::{
            CollectionAmount, CollectionServiceError, CreateCollectionInput,
            CreateCollectionOutcome, CreateCollectionResult,
        },
        services::orders::{
            CreateOrderInput, CreateOrderResult, CreateOrderServiceOutcome, OrderServiceError,
        },
    };

    const ISSUER: &str = "pay3-test-issuer";
    const AUDIENCE: &str = "pay3-api";
    const KID: &str = "test-key";
    const SECRET: &str = "test-secret-with-enough-entropy";

    #[tokio::test]
    async fn healthz_returns_only_process_liveness() {
        let response = request_json("/healthz", StaticDependencyRegistry::all_healthy()).await;

        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(response.body["status"], "ok");
        assert!(response.body.get("dependencies").is_none());
    }

    #[tokio::test]
    async fn readyz_returns_success_dependency_report() {
        let response = request_json("/readyz", StaticDependencyRegistry::all_healthy()).await;

        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(response.body["status"], "ok");

        for dependency in [
            "db",
            "migration",
            "rpc_chain_id",
            "kvdb",
            "signer",
            "worker_lease",
        ] {
            assert!(dependency_status(&response.body, dependency).is_some());
            assert_eq!(dependency_status(&response.body, dependency), Some("ok"));
        }
    }

    #[tokio::test]
    async fn readyz_returns_failure_dependency_report() {
        let registry = StaticDependencyRegistry::all_healthy();
        registry.set_status(DependencyCheck::failed(
            DependencyName::RpcChainId,
            "configured chain id does not match RPC",
        ));
        registry.set_status(DependencyCheck::failed(
            DependencyName::WorkerLease,
            "worker lease unreadable",
        ));

        let response = request_json("/readyz", registry).await;

        assert_eq!(response.status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response.body["status"], "failed");
        assert_eq!(
            dependency_status(&response.body, "rpc_chain_id"),
            Some("failed")
        );
        assert_eq!(
            dependency_status(&response.body, "worker_lease"),
            Some("failed")
        );
    }

    #[tokio::test]
    async fn metrics_exposes_build_latency_and_readyz_dependency_status() {
        let registry = StaticDependencyRegistry::all_healthy();
        registry.set_status(DependencyCheck::failed(
            DependencyName::Kvdb,
            "kvdb open failure",
        ));

        let app = router_with_registry(registry);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();

        assert!(body.contains("pay3_build_info"));
        assert!(body.contains("pay3_http_request_latency_seconds_count"));
        assert!(body.contains("pay3_readyz_dependency_status{dependency=\"db\"} 1"));
        assert!(body.contains("pay3_readyz_dependency_status{dependency=\"kvdb\"} 0"));
    }

    #[tokio::test]
    async fn not_found_uses_unified_error_contract() {
        let response = request_json("/missing", StaticDependencyRegistry::all_healthy()).await;

        assert_eq!(response.status, StatusCode::NOT_FOUND);
        assert_eq!(response.body["error"]["code"], "not_found");
        assert_eq!(response.body["error"]["message"], "route not found");
        assert!(response.body["error"]["request_id"].as_str().is_some());
        assert_eq!(response.body["error"]["retryable"], false);
        assert_eq!(response.body["error"]["details"], json!({}));
    }

    #[tokio::test]
    async fn rate_limit_returns_429_for_v1_routes() {
        let service = Arc::new(FakeOrderApiService::with_order(order_view(
            Uuid::from_u128(3),
            "merchant-order-3",
            RawAmount::from(1_000_000),
        )));
        let state = ApiState::new(Arc::new(StaticDependencyRegistry::all_healthy()))
            .with_orders(
                Arc::new(verifier()),
                service,
                OrderResponseConfig {
                    token_decimals: 6,
                    token_symbol: "USDT".to_string(),
                },
            )
            .with_rate_limit_per_minute(Some(1));
        let app = router_from_state(state);

        let first = request_json_with_app(
            app.clone(),
            Method::GET,
            "/v1/orders/00000000-0000-0000-0000-000000000003",
            Value::Null,
            Some(token(ORDERS_READ_SCOPE)),
        )
        .await;
        assert_eq!(first.status, StatusCode::OK);

        let second = request_json_with_app(
            app,
            Method::GET,
            "/v1/orders/00000000-0000-0000-0000-000000000003",
            Value::Null,
            Some(token(ORDERS_READ_SCOPE)),
        )
        .await;

        assert_eq!(second.status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(second.body["error"]["code"], "rate_limited");
        assert_eq!(second.body["error"]["retryable"], true);
        assert_eq!(second.body["error"]["details"]["limit_per_minute"], 1);
    }

    #[tokio::test]
    async fn post_orders_requires_orders_create_scope() {
        let service = Arc::new(FakeOrderApiService::default());
        let response = request_json_with_app(
            orders_app(service.clone()),
            Method::POST,
            "/v1/orders",
            json!({"external_id": "merchant-order-1", "amount": "12.34", "ttl_seconds": 900}),
            Some(token(ORDERS_READ_SCOPE)),
        )
        .await;

        assert_eq!(response.status, StatusCode::FORBIDDEN);
        assert_eq!(response.body["error"]["code"], "forbidden");
        assert_eq!(service.calls.lock().unwrap().create_inputs.len(), 0);
    }

    #[tokio::test]
    async fn post_orders_creates_order_and_returns_payment_details() {
        let view = order_view(
            Uuid::from_u128(1),
            "merchant-order-1",
            RawAmount::from(12_340_000),
        );
        let service = Arc::new(FakeOrderApiService::with_create(CreateOrderResult {
            outcome: CreateOrderServiceOutcome::Created,
            view,
        }));

        let response = request_json_with_app(
            orders_app(service.clone()),
            Method::POST,
            "/v1/orders",
            json!({
                "external_id": "merchant-order-1",
                "amount": "12.34",
                "ttl_seconds": 900,
                "metadata": {"note": "optional"}
            }),
            Some(token(ORDERS_CREATE_SCOPE)),
        )
        .await;

        assert_eq!(response.status, StatusCode::CREATED);
        assert_eq!(response.body["id"], Uuid::from_u128(1).to_string());
        assert_eq!(response.body["status"], "pending");
        assert_eq!(response.body["payment"]["token_symbol"], "USDT");
        assert_eq!(response.body["payment"]["token_decimals"], 6);
        assert_eq!(response.body["payment"]["amount"], "12.34");
        assert_eq!(response.body["payment"]["amount_raw"], "12340000");
        assert_eq!(
            response.body["payment"]["derivation_path"],
            "m/44'/60'/0'/0/42"
        );

        let calls = service.calls.lock().unwrap();
        assert_eq!(calls.create_inputs.len(), 1);
        assert_eq!(calls.create_inputs[0].external_id, "merchant-order-1");
        assert_eq!(
            calls.create_inputs[0].expected_amount_raw,
            RawAmount::from(12_340_000)
        );
        assert_eq!(calls.create_inputs[0].ttl_seconds, 900);
        assert_eq!(calls.create_inputs[0].metadata["note"], "optional");
    }

    #[tokio::test]
    async fn post_orders_existing_idempotent_result_uses_ok_status() {
        let view = order_view(Uuid::from_u128(2), "merchant-order-2", RawAmount::from(42));
        let service = Arc::new(FakeOrderApiService::with_create(CreateOrderResult {
            outcome: CreateOrderServiceOutcome::Existing,
            view,
        }));

        let response = request_json_with_app(
            orders_app(service),
            Method::POST,
            "/v1/orders",
            json!({"external_id": "merchant-order-2", "amount": "0.000042", "ttl_seconds": 900}),
            Some(token(ORDERS_CREATE_SCOPE)),
        )
        .await;

        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(response.body["id"], Uuid::from_u128(2).to_string());
    }

    #[tokio::test]
    async fn post_orders_maps_idempotency_conflict_to_409() {
        let service = Arc::new(FakeOrderApiService::with_create_error(
            OrderServiceError::Repository(RepositoryError::idempotency_conflict(
                "orders",
                "merchant-order-1",
                Some(Uuid::from_u128(1)),
            )),
        ));

        let response = request_json_with_app(
            orders_app(service),
            Method::POST,
            "/v1/orders",
            json!({"external_id": "merchant-order-1", "amount": "12.34", "ttl_seconds": 900}),
            Some(token(ORDERS_CREATE_SCOPE)),
        )
        .await;

        assert_eq!(response.status, StatusCode::CONFLICT);
        assert_eq!(response.body["error"]["code"], "idempotency_conflict");
    }

    #[tokio::test]
    async fn post_orders_rejects_invalid_amount_before_service_call() {
        let service = Arc::new(FakeOrderApiService::default());

        let response = request_json_with_app(
            orders_app(service.clone()),
            Method::POST,
            "/v1/orders",
            json!({"external_id": "merchant-order-1", "amount": "12.3456789", "ttl_seconds": 900}),
            Some(token(ORDERS_CREATE_SCOPE)),
        )
        .await;

        assert_eq!(response.status, StatusCode::BAD_REQUEST);
        assert_eq!(response.body["error"]["code"], "invalid_amount");
        assert_eq!(service.calls.lock().unwrap().create_inputs.len(), 0);
    }

    #[tokio::test]
    async fn get_order_returns_order_view_with_read_scope() {
        let view = order_view(
            Uuid::from_u128(3),
            "merchant-order-3",
            RawAmount::from(1_000_000),
        );
        let service = Arc::new(FakeOrderApiService::with_order(view));

        let response = request_json_with_app(
            orders_app(service),
            Method::GET,
            "/v1/orders/00000000-0000-0000-0000-000000000003",
            Value::Null,
            Some(token(ORDERS_READ_SCOPE)),
        )
        .await;

        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(response.body["external_id"], "merchant-order-3");
        assert_eq!(response.body["payment"]["amount"], "1");
        assert_eq!(
            response.body["payment"]["receive_address"],
            evm_address(0x77).to_string()
        );
    }

    #[tokio::test]
    async fn get_order_by_external_id_returns_order_view() {
        let view = order_view(
            Uuid::from_u128(4),
            "merchant-order-4",
            RawAmount::from(2_500_000),
        );
        let service = Arc::new(FakeOrderApiService::with_order(view));

        let response = request_json_with_app(
            orders_app(service),
            Method::GET,
            "/v1/orders/by-external-id/merchant-order-4",
            Value::Null,
            Some(token(ORDERS_READ_SCOPE)),
        )
        .await;

        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(response.body["id"], Uuid::from_u128(4).to_string());
        assert_eq!(response.body["payment"]["amount"], "2.5");
    }

    #[tokio::test]
    async fn post_collections_requires_collections_create_scope() {
        let service = Arc::new(FakeCollectionApiService::default());
        let response = request_json_with_app(
            collections_app(service.clone()),
            Method::POST,
            "/v1/collections",
            json!({
                "order_id": Uuid::from_u128(10).to_string(),
                "amount": "max",
                "idempotency_key": "collect-1"
            }),
            Some(token(ORDERS_READ_SCOPE)),
        )
        .await;

        assert_eq!(response.status, StatusCode::FORBIDDEN);
        assert_eq!(response.body["error"]["code"], "forbidden");
        assert_eq!(service.calls.lock().unwrap().create_inputs.len(), 0);
    }

    #[tokio::test]
    async fn post_collections_creates_max_collection() {
        let record = collection_record(
            Uuid::from_u128(11),
            Uuid::from_u128(10),
            CollectionRecordStatus::Queued,
        );
        let service = Arc::new(FakeCollectionApiService::with_create(
            CreateCollectionResult {
                outcome: CreateCollectionOutcome::Created,
                collection: record,
            },
        ));

        let response = request_json_with_app(
            collections_app(service.clone()),
            Method::POST,
            "/v1/collections",
            json!({
                "order_id": Uuid::from_u128(10).to_string(),
                "amount": "max",
                "idempotency_key": "collect-1"
            }),
            Some(token(COLLECTIONS_CREATE_SCOPE)),
        )
        .await;

        assert_eq!(response.status, StatusCode::CREATED);
        assert_eq!(response.body["id"], Uuid::from_u128(11).to_string());
        assert_eq!(response.body["order_id"], Uuid::from_u128(10).to_string());
        assert_eq!(response.body["status"], "queued");
        assert_eq!(response.body["amount_raw"], Value::Null);
        assert_eq!(response.body["from_address"], evm_address(0x77).to_string());
        assert_eq!(response.body["to_address"], evm_address(0x99).to_string());

        let calls = service.calls.lock().unwrap();
        assert_eq!(calls.create_inputs.len(), 1);
        assert_eq!(calls.create_inputs[0].order_id, Uuid::from_u128(10));
        assert_eq!(calls.create_inputs[0].amount, CollectionAmount::Max);
        assert_eq!(calls.create_inputs[0].idempotency_key, "collect-1");
        assert_eq!(
            calls.create_inputs[0].audit.principal_sub.as_deref(),
            Some("merchant-1")
        );
        assert_eq!(calls.create_inputs[0].audit.request_id, None);
        assert_eq!(
            calls.create_inputs[0].audit.scopes,
            vec![COLLECTIONS_CREATE_SCOPE]
        );
    }

    #[tokio::test]
    async fn post_collections_passes_request_id_to_audit_context() {
        let record = collection_record(
            Uuid::from_u128(11),
            Uuid::from_u128(10),
            CollectionRecordStatus::Queued,
        );
        let service = Arc::new(FakeCollectionApiService::with_create(
            CreateCollectionResult {
                outcome: CreateCollectionOutcome::Created,
                collection: record,
            },
        ));
        let app = collections_app(service.clone());
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/v1/collections")
                    .header(
                        header::AUTHORIZATION,
                        format!("Bearer {}", token(COLLECTIONS_CREATE_SCOPE)),
                    )
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("x-request-id", "req-collection-1")
                    .body(Body::from(
                        serde_json::to_vec(&json!({
                            "order_id": Uuid::from_u128(10).to_string(),
                            "amount": "max",
                            "idempotency_key": "collect-1"
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CREATED);
        assert_eq!(
            service.calls.lock().unwrap().create_inputs[0]
                .audit
                .request_id,
            Some("req-collection-1".to_string())
        );
    }

    #[tokio::test]
    async fn post_collections_existing_idempotent_result_uses_ok_status() {
        let record = collection_record(
            Uuid::from_u128(12),
            Uuid::from_u128(10),
            CollectionRecordStatus::Queued,
        );
        let service = Arc::new(FakeCollectionApiService::with_create(
            CreateCollectionResult {
                outcome: CreateCollectionOutcome::Existing,
                collection: record,
            },
        ));

        let response = request_json_with_app(
            collections_app(service),
            Method::POST,
            "/v1/collections",
            json!({
                "order_id": Uuid::from_u128(10).to_string(),
                "amount": "MAX",
                "idempotency_key": "collect-1"
            }),
            Some(token(COLLECTIONS_CREATE_SCOPE)),
        )
        .await;

        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(response.body["id"], Uuid::from_u128(12).to_string());
    }

    #[tokio::test]
    async fn post_collections_rejects_unknown_to_address_field() {
        let service = Arc::new(FakeCollectionApiService::default());
        let response = request_json_with_app(
            collections_app(service.clone()),
            Method::POST,
            "/v1/collections",
            json!({
                "order_id": Uuid::from_u128(10).to_string(),
                "amount": "max",
                "idempotency_key": "collect-1",
                "to_address": evm_address(0x55).to_string()
            }),
            Some(token(COLLECTIONS_CREATE_SCOPE)),
        )
        .await;

        assert_eq!(response.status, StatusCode::BAD_REQUEST);
        assert_eq!(response.body["error"]["code"], "invalid_json");
        assert_eq!(service.calls.lock().unwrap().create_inputs.len(), 0);
    }

    #[tokio::test]
    async fn post_collections_rejects_exact_amount_in_mvp_api() {
        let service = Arc::new(FakeCollectionApiService::default());
        let response = request_json_with_app(
            collections_app(service.clone()),
            Method::POST,
            "/v1/collections",
            json!({
                "order_id": Uuid::from_u128(10).to_string(),
                "amount": "1000",
                "idempotency_key": "collect-1"
            }),
            Some(token(COLLECTIONS_CREATE_SCOPE)),
        )
        .await;

        assert_eq!(response.status, StatusCode::BAD_REQUEST);
        assert_eq!(response.body["error"]["code"], "invalid_collection_amount");
        assert_eq!(service.calls.lock().unwrap().create_inputs.len(), 0);
    }

    #[tokio::test]
    async fn post_collections_maps_uncollectable_order_to_409() {
        let service = Arc::new(FakeCollectionApiService::with_create_error(
            CollectionServiceError::OrderNotCollectable {
                order_id: Uuid::from_u128(10),
                status: OrderStatus::Confirming,
            },
        ));

        let response = request_json_with_app(
            collections_app(service),
            Method::POST,
            "/v1/collections",
            json!({
                "order_id": Uuid::from_u128(10).to_string(),
                "amount": "max",
                "idempotency_key": "collect-1"
            }),
            Some(token(COLLECTIONS_CREATE_SCOPE)),
        )
        .await;

        assert_eq!(response.status, StatusCode::CONFLICT);
        assert_eq!(response.body["error"]["code"], "collection_not_allowed");
    }

    #[tokio::test]
    async fn get_collections_requires_collections_read_scope() {
        let service = Arc::new(FakeCollectionApiService::with_collection(
            collection_record(
                Uuid::from_u128(13),
                Uuid::from_u128(10),
                CollectionRecordStatus::Queued,
            ),
        ));

        let response = request_json_with_app(
            collections_app(service.clone()),
            Method::GET,
            "/v1/collections/00000000-0000-0000-0000-000000000013",
            Value::Null,
            Some(token(COLLECTIONS_CREATE_SCOPE)),
        )
        .await;

        assert_eq!(response.status, StatusCode::FORBIDDEN);
        assert_eq!(response.body["error"]["code"], "forbidden");
        assert_eq!(service.calls.lock().unwrap().get_ids.len(), 0);
    }

    #[tokio::test]
    async fn get_collections_returns_collection_with_read_scope() {
        let mut record = collection_record(
            Uuid::from_u128(14),
            Uuid::from_u128(10),
            CollectionRecordStatus::Confirming,
        );
        record.amount_raw = Some(RawAmount::from(1_000_000));
        record.outbound_tx_id = Some(Uuid::from_u128(99));
        record.attempt_count = 2;
        record.error = Some("receipt pending".to_string());
        let service = Arc::new(FakeCollectionApiService::with_collection(record));

        let response = request_json_with_app(
            collections_app(service.clone()),
            Method::GET,
            "/v1/collections/00000000-0000-0000-0000-00000000000e",
            Value::Null,
            Some(token(COLLECTIONS_READ_SCOPE)),
        )
        .await;

        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(response.body["id"], Uuid::from_u128(14).to_string());
        assert_eq!(response.body["order_id"], Uuid::from_u128(10).to_string());
        assert_eq!(response.body["chain_id"], 1);
        assert_eq!(
            response.body["token_address"],
            evm_address(0x11).to_string()
        );
        assert_eq!(response.body["status"], "confirming");
        assert_eq!(response.body["amount_raw"], "1000000");
        assert_eq!(
            response.body["outbound_tx_id"],
            Uuid::from_u128(99).to_string()
        );
        assert_eq!(response.body["attempt_count"], 2);
        assert_eq!(response.body["error"], "receipt pending");

        assert_eq!(
            service.calls.lock().unwrap().get_ids,
            vec![Uuid::from_u128(14)]
        );
    }

    #[tokio::test]
    async fn get_collections_returns_404_when_missing() {
        let service = Arc::new(FakeCollectionApiService::default());

        let response = request_json_with_app(
            collections_app(service),
            Method::GET,
            "/v1/collections/00000000-0000-0000-0000-000000000015",
            Value::Null,
            Some(token(COLLECTIONS_READ_SCOPE)),
        )
        .await;

        assert_eq!(response.status, StatusCode::NOT_FOUND);
        assert_eq!(response.body["error"]["message"], "collection not found");
    }

    struct JsonResponse {
        status: StatusCode,
        body: Value,
    }

    async fn request_json(uri: &str, registry: StaticDependencyRegistry) -> JsonResponse {
        let app = router_with_registry(registry);
        let response = app
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let body = serde_json::from_slice(&body).unwrap();

        JsonResponse { status, body }
    }

    async fn request_json_with_app(
        app: axum::Router,
        method: Method,
        uri: &str,
        body: Value,
        authorization: Option<String>,
    ) -> JsonResponse {
        let mut builder = Request::builder().method(method).uri(uri);
        if let Some(authorization) = authorization {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {authorization}"));
        }

        let body = if body.is_null() {
            Body::empty()
        } else {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
            Body::from(serde_json::to_vec(&body).unwrap())
        };
        let response = app.oneshot(builder.body(body).unwrap()).await.unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let body = serde_json::from_slice(&body).unwrap();

        JsonResponse { status, body }
    }

    fn orders_app(service: Arc<FakeOrderApiService>) -> axum::Router {
        router_with_order_service(
            StaticDependencyRegistry::all_healthy(),
            verifier(),
            service,
            OrderResponseConfig {
                token_decimals: 6,
                token_symbol: "USDT".to_string(),
            },
        )
    }

    fn collections_app(service: Arc<FakeCollectionApiService>) -> axum::Router {
        router_with_collection_service(StaticDependencyRegistry::all_healthy(), verifier(), service)
    }

    fn verifier() -> JwtVerifier {
        JwtVerifier::new_hs256(ISSUER, AUDIENCE, [(KID, SECRET)]).unwrap()
    }

    fn token(scopes: &str) -> String {
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
            scope: Some(scopes.to_string()),
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

    fn order_view(id: Uuid, external_id: &str, expected_amount_raw: RawAmount) -> OrderView {
        let child_account_id = Uuid::from_u128(id.as_u128() + 100);
        let receive_address = evm_address(0x77);
        let token_address = evm_address(0x11);
        let now = OffsetDateTime::from_unix_timestamp(1_777_777_777).unwrap();
        let expires_at = now + Duration::seconds(900);
        let monitor_until = expires_at + Duration::seconds(86_400);
        let derivation_segment = DerivationSegment::new(0, 0, 42).unwrap();

        OrderView {
            order: OrderRecord {
                id,
                external_id: external_id.to_string(),
                request_hash: "0xrequest".to_string(),
                child_account_id,
                receive_address,
                chain_id: 1,
                token_address,
                expected_amount_raw,
                paid_amount_raw: RawAmount::ZERO,
                status: OrderStatus::Pending,
                expires_at,
                monitor_until,
                created_at: now,
                updated_at: now,
            },
            child_account: ChildAccountRecord {
                id: child_account_id,
                signer_key_ref: "pay3-master".to_string(),
                derivation_version: 1,
                derivation_segment,
                derivation_path: derivation_segment.derivation_path(),
                address: receive_address,
                last_used_at: Some(now),
                created_at: now,
            },
            payment_window: PaymentWindowRecord {
                id: Uuid::from_u128(id.as_u128() + 200),
                order_id: id,
                child_account_id,
                receive_address,
                window_from: now,
                window_from_block: ChainBlockRef::new(123, BlockHash::from_bytes([0x12; 32])),
                expires_at,
                monitor_until,
                created_at: now,
            },
        }
    }

    fn collection_record(
        id: Uuid,
        order_id: Uuid,
        status: CollectionRecordStatus,
    ) -> CollectionRecord {
        let now = OffsetDateTime::from_unix_timestamp(1_777_777_777).unwrap();
        CollectionRecord {
            id,
            order_id,
            idempotency_key: "collect-1".to_string(),
            request_hash: "0xcollection-request".to_string(),
            child_account_id: Uuid::from_u128(order_id.as_u128() + 100),
            chain_id: 1,
            token_address: evm_address(0x11),
            from_address: evm_address(0x77),
            to_address: evm_address(0x99),
            amount_raw: None,
            status,
            outbound_tx_id: None,
            attempt_count: 0,
            locked_by: None,
            locked_until: None,
            error: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn evm_address(byte: u8) -> EvmAddress {
        EvmAddress::from_bytes([byte; 20])
    }

    #[derive(Default)]
    struct FakeOrderApiService {
        create_result: Mutex<Option<Result<CreateOrderResult, OrderServiceError>>>,
        orders: Mutex<BTreeMap<Uuid, OrderView>>,
        orders_by_external_id: Mutex<BTreeMap<String, Uuid>>,
        calls: Mutex<FakeOrderApiCalls>,
    }

    impl FakeOrderApiService {
        fn with_create(result: CreateOrderResult) -> Self {
            Self {
                create_result: Mutex::new(Some(Ok(result))),
                ..Self::default()
            }
        }

        fn with_create_error(error: OrderServiceError) -> Self {
            Self {
                create_result: Mutex::new(Some(Err(error))),
                ..Self::default()
            }
        }

        fn with_order(view: OrderView) -> Self {
            let mut orders = BTreeMap::new();
            let mut orders_by_external_id = BTreeMap::new();
            orders_by_external_id.insert(view.order.external_id.clone(), view.order.id);
            orders.insert(view.order.id, view);
            Self {
                orders: Mutex::new(orders),
                orders_by_external_id: Mutex::new(orders_by_external_id),
                ..Self::default()
            }
        }
    }

    #[derive(Default)]
    struct FakeOrderApiCalls {
        create_inputs: Vec<CreateOrderInput>,
        get_ids: Vec<Uuid>,
        get_external_ids: Vec<String>,
    }

    #[async_trait]
    impl OrderApiService for FakeOrderApiService {
        async fn create_order(
            &self,
            input: CreateOrderInput,
        ) -> Result<CreateOrderResult, OrderServiceError> {
            self.calls.lock().unwrap().create_inputs.push(input);
            match self.create_result.lock().unwrap().take() {
                Some(Ok(result)) => Ok(result),
                Some(Err(error)) => Err(error),
                None => Err(OrderServiceError::invalid_argument(
                    "fake_error",
                    "missing fake create result",
                )),
            }
        }

        async fn get_order(&self, id: Uuid) -> Result<Option<OrderView>, OrderServiceError> {
            self.calls.lock().unwrap().get_ids.push(id);
            Ok(self.orders.lock().unwrap().get(&id).cloned())
        }

        async fn get_order_by_external_id(
            &self,
            external_id: &str,
        ) -> Result<Option<OrderView>, OrderServiceError> {
            self.calls
                .lock()
                .unwrap()
                .get_external_ids
                .push(external_id.to_string());
            let Some(id) = self
                .orders_by_external_id
                .lock()
                .unwrap()
                .get(external_id)
                .copied()
            else {
                return Ok(None);
            };
            Ok(self.orders.lock().unwrap().get(&id).cloned())
        }
    }

    #[derive(Default)]
    struct FakeCollectionApiService {
        create_result: Mutex<Option<Result<CreateCollectionResult, CollectionServiceError>>>,
        collections: Mutex<BTreeMap<Uuid, CollectionRecord>>,
        calls: Mutex<FakeCollectionApiCalls>,
    }

    impl FakeCollectionApiService {
        fn with_create(result: CreateCollectionResult) -> Self {
            Self {
                create_result: Mutex::new(Some(Ok(result))),
                ..Self::default()
            }
        }

        fn with_create_error(error: CollectionServiceError) -> Self {
            Self {
                create_result: Mutex::new(Some(Err(error))),
                ..Self::default()
            }
        }

        fn with_collection(collection: CollectionRecord) -> Self {
            let mut collections = BTreeMap::new();
            collections.insert(collection.id, collection);
            Self {
                collections: Mutex::new(collections),
                ..Self::default()
            }
        }
    }

    #[derive(Default)]
    struct FakeCollectionApiCalls {
        create_inputs: Vec<CreateCollectionInput>,
        get_ids: Vec<Uuid>,
    }

    #[async_trait]
    impl CollectionApiService for FakeCollectionApiService {
        async fn create_collection(
            &self,
            input: CreateCollectionInput,
        ) -> Result<CreateCollectionResult, CollectionServiceError> {
            self.calls.lock().unwrap().create_inputs.push(input);
            match self.create_result.lock().unwrap().take() {
                Some(Ok(result)) => Ok(result),
                Some(Err(error)) => Err(error),
                None => Err(CollectionServiceError::InvalidArgument {
                    field: "fake_error",
                    message: "missing fake create result".to_string(),
                }),
            }
        }

        async fn get_collection(
            &self,
            id: Uuid,
        ) -> Result<Option<CollectionRecord>, CollectionServiceError> {
            self.calls.lock().unwrap().get_ids.push(id);
            Ok(self.collections.lock().unwrap().get(&id).cloned())
        }
    }

    fn dependency_status<'a>(body: &'a Value, name: &str) -> Option<&'a str> {
        body["dependencies"]
            .as_array()?
            .iter()
            .find(|dependency| dependency["name"].as_str() == Some(name))
            .and_then(|dependency| dependency["status"].as_str())
    }
}
