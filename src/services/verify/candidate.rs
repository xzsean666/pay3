use crate::{
    db::repositories::{OrderView, PaymentWindowCandidate},
    domain::OrderStatus,
};

pub fn candidate_from_order_view(view: &OrderView) -> PaymentWindowCandidate {
    PaymentWindowCandidate {
        order_id: view.order.id,
        child_account_id: view.order.child_account_id,
        receive_address: view.order.receive_address,
        chain_id: view.order.chain_id,
        token_address: view.order.token_address,
        expected_amount_raw: view.order.expected_amount_raw,
        paid_amount_raw: view.order.paid_amount_raw,
        order_status: view.order.status,
        window_from: view.payment_window.window_from,
        window_from_block: view.payment_window.window_from_block,
        expires_at: view.payment_window.expires_at,
        monitor_until: view.payment_window.monitor_until,
    }
}

pub fn manual_status_from_order_status(status: OrderStatus) -> super::ManualVerifyStatus {
    match status {
        OrderStatus::Paid => super::ManualVerifyStatus::Confirmed,
        OrderStatus::Pending
        | OrderStatus::Partial
        | OrderStatus::Confirming
        | OrderStatus::Expired => super::ManualVerifyStatus::Confirming,
    }
}
