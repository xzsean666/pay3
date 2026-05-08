#[path = "support/manual_verify/mod.rs"]
mod manual_verify;

use manual_verify::{
    FakeLogReader, FakeOrderRepository, FakeRecorder, LogReaderCall, address, order_id, order_view,
    receive_address, service, service_with_config, stored_log, stream,
};
use pay3::{
    domain::{BlockHash, PaymentChainStatus, PaymentMatchStatus, RawAmount},
    services::verify::{ManualVerifyConfig, ManualVerifyError, ManualVerifyStatus},
};

#[tokio::test]
async fn manual_verify_returns_not_found_before_touching_kv_or_recorder() {
    let reader = FakeLogReader::new(stream(), Vec::new(), Some(20));
    let recorder = FakeRecorder::default();

    let error = service(
        FakeOrderRepository::missing(),
        recorder.clone(),
        reader.clone(),
        20,
        2,
    )
    .verify_order(order_id())
    .await
    .unwrap_err();

    assert!(matches!(error, ManualVerifyError::OrderNotFound { .. }));
    assert!(reader.calls().is_empty());
    assert!(recorder.calls().is_empty());
}

#[tokio::test]
async fn manual_verify_requires_kv_coverage_for_the_order_window_start() {
    let reader = FakeLogReader::new(stream(), Vec::new(), Some(9));
    let recorder = FakeRecorder::default();

    let error = service(
        FakeOrderRepository::with_view(order_view(RawAmount::from(100))),
        recorder.clone(),
        reader.clone(),
        20,
        2,
    )
    .verify_order(order_id())
    .await
    .unwrap_err();

    assert!(matches!(
        error,
        ManualVerifyError::CoverageInsufficient { .. }
    ));
    assert_eq!(reader.calls(), vec![LogReaderCall::Cursor(stream())]);
    assert!(recorder.calls().is_empty());
}

#[tokio::test]
async fn manual_verify_matches_order_logs_records_payments_and_reports_confirmed() {
    let logs = vec![
        stored_log(10, 0, receive_address(), RawAmount::from(100), 10),
        stored_log(10, 1, address(0xbb), RawAmount::from(100), 10),
    ];
    let reader = FakeLogReader::new(stream(), logs, Some(12));
    let recorder = FakeRecorder::default();

    let result = service(
        FakeOrderRepository::with_view(order_view(RawAmount::from(100))),
        recorder.clone(),
        reader.clone(),
        12,
        3,
    )
    .verify_order(order_id())
    .await
    .unwrap();

    assert_eq!(result.status, ManualVerifyStatus::Confirmed);
    assert_eq!(result.matched_payments, 1);
    assert_eq!(result.paid_amount_raw, RawAmount::from(100));
    assert_eq!(result.confirmations, 3);
    assert_eq!(result.complete_to_block, Some(12));
    assert_eq!(
        reader.calls(),
        vec![
            LogReaderCall::Cursor(stream()),
            LogReaderCall::LogsInRange {
                stream: stream(),
                from: 10,
                to: 12,
                max_logs: 21,
            },
        ]
    );

    let calls = recorder.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].order_id, order_id());
    assert_eq!(calls[0].payments.len(), 1);
    assert_eq!(
        calls[0].payments[0].match_status,
        PaymentMatchStatus::OnTime
    );
    assert_eq!(
        calls[0].payments[0].chain_status,
        PaymentChainStatus::Confirmed
    );
}

#[tokio::test]
async fn manual_verify_reports_confirming_until_confirmations_are_sufficient() {
    let logs = vec![stored_log(
        10,
        0,
        receive_address(),
        RawAmount::from(100),
        10,
    )];

    let result = service(
        FakeOrderRepository::with_view(order_view(RawAmount::from(100))),
        FakeRecorder::default(),
        FakeLogReader::new(stream(), logs, Some(10)),
        11,
        3,
    )
    .verify_order(order_id())
    .await
    .unwrap();

    assert_eq!(result.status, ManualVerifyStatus::Confirming);
    assert_eq!(result.matched_payments, 1);
    assert_eq!(result.paid_amount_raw, RawAmount::ZERO);
    assert_eq!(result.confirmations, 2);
}

#[tokio::test]
async fn manual_verify_fails_closed_when_matched_payment_block_is_not_canonical() {
    let mut log = stored_log(10, 0, receive_address(), RawAmount::from(100), 10);
    log.block_hash = BlockHash::from_bytes([0xee; 32]);
    let logs = vec![log];
    let recorder = FakeRecorder::default();

    let error = service(
        FakeOrderRepository::with_view(order_view(RawAmount::from(100))),
        recorder.clone(),
        FakeLogReader::new(stream(), logs, Some(10)),
        12,
        1,
    )
    .verify_order(order_id())
    .await
    .unwrap_err();

    assert!(matches!(
        error,
        ManualVerifyError::CanonicalBlockMismatch {
            block_number: 10,
            ..
        }
    ));
    assert!(recorder.calls().is_empty());
}

#[tokio::test]
async fn manual_verify_fails_closed_when_order_window_log_limit_is_hit() {
    let logs = vec![
        stored_log(10, 0, receive_address(), RawAmount::from(1), 10),
        stored_log(10, 1, receive_address(), RawAmount::from(1), 10),
    ];
    let recorder = FakeRecorder::default();

    let error = service_with_config(
        FakeOrderRepository::with_view(order_view(RawAmount::from(100))),
        recorder.clone(),
        FakeLogReader::new(stream(), logs, Some(10)),
        11,
        ManualVerifyConfig::new(1, 1),
    )
    .verify_order(order_id())
    .await
    .unwrap_err();

    assert!(matches!(error, ManualVerifyError::LogLimitExceeded { .. }));
    assert!(recorder.calls().is_empty());
}
