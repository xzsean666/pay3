use crate::{
    chain::{ChainHeaderReader, TransferLogRange, TransferLogSource},
    db::repositories::{OrderRepository, PaymentRecord},
    domain::{PaymentFact, recompute_order_status},
    services::{
        orders::Clock,
        payments::match_transfer_logs,
    },
    transfer_log_store::StreamId,
};
use uuid::Uuid;

use super::{
    ManualVerifyConfig, ManualVerifyError, ManualVerifyResult, candidate_from_order_view,
    manual_status_from_order_status, recorder::VerifiedPaymentRecorder,
};

pub struct ManualOrderVerifyService<O, R, S, H, C> {
    orders: O,
    recorder: R,
    log_source: S,
    head_reader: H,
    clock: C,
    config: ManualVerifyConfig,
}

impl<O, R, S, H, C> ManualOrderVerifyService<O, R, S, H, C> {
    pub const fn new(
        orders: O,
        recorder: R,
        log_source: S,
        head_reader: H,
        clock: C,
        config: ManualVerifyConfig,
    ) -> Self {
        Self {
            orders,
            recorder,
            log_source,
            head_reader,
            clock,
            config,
        }
    }
}

impl<O, R, S, H, C> ManualOrderVerifyService<O, R, S, H, C>
where
    O: OrderRepository,
    R: VerifiedPaymentRecorder,
    S: TransferLogSource,
    H: ChainHeaderReader,
    C: Clock,
{
    pub async fn verify_order(
        &self,
        order_id: Uuid,
    ) -> Result<ManualVerifyResult, ManualVerifyError> {
        self.config.validate()?;

        let view = self
            .orders
            .get_order_view(order_id)
            .await?
            .ok_or(ManualVerifyError::OrderNotFound { order_id })?;
        let stream = StreamId::new(view.order.chain_id, view.order.token_address);
        let from_block = view.payment_window.window_from_block.number;
        let head = self.head_reader.latest_head().await?;
        if head.number < from_block {
            return Err(ManualVerifyError::CoverageInsufficient { order_id });
        }
        let complete_to_block = head.number;

        let range = TransferLogRange::new(
            stream.chain_id,
            stream.token_address,
            from_block,
            complete_to_block,
        );
        let logs = self.log_source.transfer_logs(range).await?;
        if logs.len() > self.config.max_logs_per_order {
            return Err(ManualVerifyError::LogLimitExceeded {
                order_id,
                limit: self.config.max_logs_per_order,
            });
        }

        let candidate = candidate_from_order_view(&view);
        let matched_payments = match_transfer_logs(
            stream,
            self.config.min_confirmations,
            logs,
            vec![candidate],
            head,
        );

        for payment in &matched_payments {
            let canonical_block = self
                .head_reader
                .block_by_number(payment.block_number)
                .await?;
            if canonical_block.hash != payment.block_hash {
                return Err(ManualVerifyError::CanonicalBlockMismatch {
                    block_number: payment.block_number,
                    stored_hash: payment.block_hash,
                    canonical_hash: canonical_block.hash,
                });
            }
        }
        let matched_count = matched_payments.len() as u64;
        let records = self
            .recorder
            .record_verified_payments(order_id, matched_payments)
            .await?;
        let decision = recompute_order_status(
            view.order.expected_amount_raw,
            payment_facts(&records),
            self.clock.now() >= view.order.expires_at,
        )?;

        Ok(ManualVerifyResult {
            order_id,
            status: manual_status_from_order_status(decision.status),
            matched_payments: matched_count,
            paid_amount_raw: decision.confirmed_on_time_total,
            confirmations: records
                .iter()
                .map(|payment| payment.confirmations)
                .max()
                .unwrap_or_default(),
            complete_to_block: Some(complete_to_block),
        })
    }
}

fn payment_facts(records: &[PaymentRecord]) -> Vec<PaymentFact> {
    records
        .iter()
        .map(|payment| {
            PaymentFact::new(
                payment.amount_raw,
                payment.match_status,
                payment.chain_status,
            )
        })
        .collect()
}
