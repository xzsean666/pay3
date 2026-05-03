use async_trait::async_trait;
use axum::{
    Json,
    extract::{Path, State},
    http::HeaderMap,
};
use serde::Serialize;
use thiserror::Error;
use uuid::Uuid;

use crate::{auth::ORDERS_VERIFY_SCOPE, domain::RawAmount, error::ApiError};

use super::{ApiState, parse_uuid, require_scope};

#[async_trait]
pub trait OrderVerifyApiService: Send + Sync {
    async fn verify_order(&self, order_id: Uuid) -> Result<OrderVerifyResult, OrderVerifyError>;
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct OrderVerifyResult {
    pub order_id: Uuid,
    pub status: OrderVerifyStatus,
    pub matched_payments: u64,
    pub paid_amount_raw: RawAmount,
    pub confirmations: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub complete_to_block: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderVerifyStatus {
    Confirmed,
    Confirming,
    NotFound,
    NoCoverage,
}

#[derive(Debug, Error)]
pub enum OrderVerifyError {
    #[error("order not found")]
    NotFound,

    #[error("transfer log coverage is insufficient")]
    CoverageInsufficient,

    #[error("verify dependency unavailable: {0}")]
    DependencyUnavailable(String),
}

pub(super) async fn verify_order(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<OrderVerifyResult>, ApiError> {
    require_scope(&state, &headers, ORDERS_VERIFY_SCOPE)?;
    let id = parse_uuid(&id)?;
    let result = state
        .order_verify()?
        .verify_order(id)
        .await
        .map_err(order_verify_error_to_api)?;

    Ok(Json(result))
}

fn order_verify_error_to_api(error: OrderVerifyError) -> ApiError {
    match error {
        OrderVerifyError::NotFound => ApiError::not_found("order not found"),
        OrderVerifyError::CoverageInsufficient => {
            ApiError::service_unavailable("log_store_coverage_insufficient", error.to_string())
        }
        OrderVerifyError::DependencyUnavailable(_) => {
            ApiError::service_unavailable("order_verify_unavailable", error.to_string())
        }
    }
}
