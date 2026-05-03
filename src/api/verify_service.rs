use async_trait::async_trait;

use crate::{
    chain::ChainHeaderReader,
    db::repositories::OrderRepository,
    services::{
        orders::Clock,
        verify::{
            ManualOrderVerifyService, ManualVerifyError, ManualVerifyStatus,
            VerifiedPaymentRecorder,
        },
    },
    transfer_log_store::TransferLogReader,
};

use super::verify::{
    OrderVerifyApiService, OrderVerifyError, OrderVerifyResult, OrderVerifyStatus,
};

#[async_trait]
impl<O, R, L, H, C> OrderVerifyApiService for ManualOrderVerifyService<O, R, L, H, C>
where
    O: OrderRepository,
    R: VerifiedPaymentRecorder,
    L: TransferLogReader,
    H: ChainHeaderReader,
    C: Clock,
{
    async fn verify_order(
        &self,
        order_id: uuid::Uuid,
    ) -> Result<OrderVerifyResult, OrderVerifyError> {
        let result = ManualOrderVerifyService::verify_order(self, order_id)
            .await
            .map_err(order_verify_error)?;

        Ok(OrderVerifyResult {
            order_id: result.order_id,
            status: match result.status {
                ManualVerifyStatus::Confirmed => OrderVerifyStatus::Confirmed,
                ManualVerifyStatus::Confirming => OrderVerifyStatus::Confirming,
                ManualVerifyStatus::NoCoverage => OrderVerifyStatus::NoCoverage,
            },
            matched_payments: result.matched_payments,
            paid_amount_raw: result.paid_amount_raw,
            confirmations: result.confirmations,
            complete_to_block: result.complete_to_block,
        })
    }
}

fn order_verify_error(error: ManualVerifyError) -> OrderVerifyError {
    match error {
        ManualVerifyError::OrderNotFound { .. } => OrderVerifyError::NotFound,
        ManualVerifyError::CoverageInsufficient { .. } => OrderVerifyError::CoverageInsufficient,
        _ => OrderVerifyError::DependencyUnavailable(error.to_string()),
    }
}
