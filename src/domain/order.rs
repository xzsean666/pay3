use std::borrow::Borrow;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{PaymentFact, RawAmount};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderStatus {
    Pending,
    Partial,
    Confirming,
    Paid,
    Expired,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct OrderStatusDecision {
    pub status: OrderStatus,
    pub confirmed_on_time_total: RawAmount,
    pub non_orphaned_on_time_total: RawAmount,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum OrderStatusError {
    #[error("payment amount total overflows uint256")]
    AmountOverflow,
}

pub fn recompute_order_status<I>(
    required_amount: RawAmount,
    payments: I,
    is_expired: bool,
) -> Result<OrderStatusDecision, OrderStatusError>
where
    I: IntoIterator,
    I::Item: Borrow<PaymentFact>,
{
    let mut confirmed_on_time_total = RawAmount::ZERO;
    let mut non_orphaned_on_time_total = RawAmount::ZERO;

    for payment in payments {
        let payment = *payment.borrow();
        if payment.is_confirmed_on_time() {
            confirmed_on_time_total = confirmed_on_time_total
                .checked_add(payment.amount)
                .ok_or(OrderStatusError::AmountOverflow)?;
        }
        if payment.is_non_orphaned_on_time() {
            non_orphaned_on_time_total = non_orphaned_on_time_total
                .checked_add(payment.amount)
                .ok_or(OrderStatusError::AmountOverflow)?;
        }
    }

    let status = if confirmed_on_time_total >= required_amount {
        OrderStatus::Paid
    } else if non_orphaned_on_time_total >= required_amount {
        OrderStatus::Confirming
    } else if !non_orphaned_on_time_total.is_zero() && !is_expired {
        OrderStatus::Partial
    } else if is_expired {
        OrderStatus::Expired
    } else {
        OrderStatus::Pending
    };

    Ok(OrderStatusDecision {
        status,
        confirmed_on_time_total,
        non_orphaned_on_time_total,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{PaymentChainStatus, PaymentMatchStatus};

    fn payment(
        amount: u64,
        match_status: PaymentMatchStatus,
        chain_status: PaymentChainStatus,
    ) -> PaymentFact {
        PaymentFact::new(RawAmount::from(amount), match_status, chain_status)
    }

    #[test]
    fn order_totals_use_only_confirmed_on_time_payments() {
        let payments = [
            payment(
                99,
                PaymentMatchStatus::OnTime,
                PaymentChainStatus::Confirmed,
            ),
            payment(10, PaymentMatchStatus::Late, PaymentChainStatus::Confirmed),
            payment(
                10,
                PaymentMatchStatus::OutsideWindow,
                PaymentChainStatus::Confirmed,
            ),
        ];

        let decision = recompute_order_status(RawAmount::from(100), payments, false).unwrap();
        assert_eq!(decision.status, OrderStatus::Partial);
        assert_eq!(decision.confirmed_on_time_total, RawAmount::from(99));
    }

    #[test]
    fn observed_on_time_full_amount_is_confirming_until_confirmed() {
        let payments = [payment(
            100,
            PaymentMatchStatus::OnTime,
            PaymentChainStatus::Observed,
        )];

        let decision = recompute_order_status(RawAmount::from(100), payments, false).unwrap();
        assert_eq!(decision.status, OrderStatus::Confirming);
        assert_eq!(decision.confirmed_on_time_total, RawAmount::ZERO);
    }

    #[test]
    fn confirmed_on_time_full_amount_is_paid() {
        let payments = [payment(
            100,
            PaymentMatchStatus::OnTime,
            PaymentChainStatus::Confirmed,
        )];

        let decision = recompute_order_status(RawAmount::from(100), payments, true).unwrap();
        assert_eq!(decision.status, OrderStatus::Paid);
    }

    #[test]
    fn partial_on_time_amount_is_partial_before_expiry() {
        let payments = [payment(
            40,
            PaymentMatchStatus::OnTime,
            PaymentChainStatus::Confirmed,
        )];

        let decision = recompute_order_status(RawAmount::from(100), payments, false).unwrap();
        assert_eq!(decision.status, OrderStatus::Partial);
        assert_eq!(decision.confirmed_on_time_total, RawAmount::from(40));
    }

    #[test]
    fn reorg_can_roll_paid_order_back() {
        let before = [payment(
            100,
            PaymentMatchStatus::OnTime,
            PaymentChainStatus::Confirmed,
        )];
        assert_eq!(
            recompute_order_status(RawAmount::from(100), before, false)
                .unwrap()
                .status,
            OrderStatus::Paid
        );

        let after = [payment(
            100,
            PaymentMatchStatus::OnTime,
            PaymentChainStatus::Orphaned,
        )];
        assert_eq!(
            recompute_order_status(RawAmount::from(100), after, false)
                .unwrap()
                .status,
            OrderStatus::Pending
        );
    }

    #[test]
    fn expired_order_recovers_when_on_time_payment_is_discovered_late() {
        assert_eq!(
            recompute_order_status(RawAmount::from(100), Vec::<PaymentFact>::new(), true)
                .unwrap()
                .status,
            OrderStatus::Expired
        );

        let observed = [payment(
            100,
            PaymentMatchStatus::OnTime,
            PaymentChainStatus::Observed,
        )];
        assert_eq!(
            recompute_order_status(RawAmount::from(100), observed, true)
                .unwrap()
                .status,
            OrderStatus::Confirming
        );

        let confirmed = [payment(
            100,
            PaymentMatchStatus::OnTime,
            PaymentChainStatus::Confirmed,
        )];
        assert_eq!(
            recompute_order_status(RawAmount::from(100), confirmed, true)
                .unwrap()
                .status,
            OrderStatus::Paid
        );
    }
}
