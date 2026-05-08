use crate::{
    chain::ChainHeaderReader,
    db::repositories::{OrderRepository, PaymentRecord},
    domain::{PaymentFact, recompute_order_status},
    services::{
        orders::Clock,
        payments::{StoredPaymentMatchInput, match_stored_transfer_logs},
    },
    transfer_log_store::{StreamId, TransferLogReader},
};
use uuid::Uuid;

use super::{
    ManualVerifyConfig, ManualVerifyError, ManualVerifyResult, candidate_from_order_view,
    manual_status_from_order_status, recorder::VerifiedPaymentRecorder,
};

pub struct ManualOrderVerifyService<O, R, L, H, C> {
    orders: O,
    recorder: R,
    log_reader: L,
    head_reader: H,
    clock: C,
    config: ManualVerifyConfig,
}

impl<O, R, L, H, C> ManualOrderVerifyService<O, R, L, H, C> {
    pub const fn new(
        orders: O,
        recorder: R,
        log_reader: L,
        head_reader: H,
        clock: C,
        config: ManualVerifyConfig,
    ) -> Self {
        Self {
            orders,
            recorder,
            log_reader,
            head_reader,
            clock,
            config,
        }
    }
}

impl<O, R, L, H, C> ManualOrderVerifyService<O, R, L, H, C>
where
    O: OrderRepository,
    R: VerifiedPaymentRecorder,
    L: TransferLogReader,
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
        let cursor = self.log_reader.cursor(stream).await?;
        let from_block = view.payment_window.window_from_block.number;
        let complete_to_block = cursor
            .last_completed_block
            .filter(|block| *block >= from_block)
            .ok_or(ManualVerifyError::CoverageInsufficient { order_id })?;

        let read_limit = self.config.max_logs_per_order.saturating_add(1);
        let logs = self
            .log_reader
            .logs_in_range(stream, from_block, complete_to_block, read_limit)
            .await?;
        if logs.len() > self.config.max_logs_per_order {
            return Err(ManualVerifyError::LogLimitExceeded {
                order_id,
                limit: self.config.max_logs_per_order,
            });
        }

        let head = self.head_reader.latest_head().await?;
        let match_page = match_stored_transfer_logs(
            stream,
            self.config.min_confirmations,
            StoredPaymentMatchInput {
                logs,
                candidates: vec![candidate_from_order_view(&view)],
                head,
                next_token: None,
                complete_to_block: Some(complete_to_block),
                kv_reorg_epoch: cursor.reorg_epoch,
            },
        );
        for payment in &match_page.matched_payments {
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
        let matched_count = match_page.matched_payments.len() as u64;
        let records = self
            .recorder
            .record_verified_payments(order_id, match_page.matched_payments)
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
