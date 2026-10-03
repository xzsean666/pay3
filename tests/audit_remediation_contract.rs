use pay3::{
    chain::{ChainBlock, TransferLog, TransferLogRange},
    domain::{
        BlockHash, CollectionMethod, EvmAddress, RawAmount, TxHash,
        resolve_optimal_collection_strategy,
    },
};
use time::OffsetDateTime;

#[test]
fn test_eip2612_auto_resolves_fallback_to_standard() {
    let arb_usdt: EvmAddress = "0xfd086bc7cd5c481dcc9c85ebe478a1c0b69fcbb9".parse().unwrap();
    let res = resolve_optimal_collection_strategy(
        42161,
        arb_usdt,
        "USDT",
        true,
        CollectionMethod::Auto,
        None,
        None,
    );
    assert_eq!(res.method, CollectionMethod::Standard);
    assert!(res.reason.contains("forwarder transfer capability"));

    let eth_dai: EvmAddress = "0x6b175474e89094c44da98b954eedeac495271d0f".parse().unwrap();
    let res_dai = resolve_optimal_collection_strategy(
        1,
        eth_dai,
        "DAI",
        true,
        CollectionMethod::Auto,
        None,
        None,
    );
    assert_eq!(res_dai.method, CollectionMethod::Standard);
    assert!(res_dai.reason.contains("forwarder transfer capability"));
}

#[test]
fn test_transfer_log_range_topic2_filter() {
    let recipient: EvmAddress = "0x0000000000000000000000000000000000000042".parse().unwrap();
    let other: EvmAddress = "0x0000000000000000000000000000000000000099".parse().unwrap();
    let token: EvmAddress = "0x1111111111111111111111111111111111111111".parse().unwrap();

    let range_with_recipient = TransferLogRange::new(1, token, 100, 200).with_recipient(recipient);
    assert_eq!(range_with_recipient.recipient, Some(recipient));

    let block = ChainBlock::new(150, BlockHash::ZERO, BlockHash::ZERO, OffsetDateTime::now_utc());
    let log_matching = TransferLog {
        chain_id: 1,
        token_address: token,
        block,
        tx_hash: TxHash::ZERO,
        log_index: 0,
        from_address: other,
        to_address: recipient,
        amount_raw: RawAmount::from(1000u64),
    };

    let log_non_matching = TransferLog {
        chain_id: 1,
        token_address: token,
        block,
        tx_hash: TxHash::ZERO,
        log_index: 1,
        from_address: recipient,
        to_address: other,
        amount_raw: RawAmount::from(1000u64),
    };

    assert!(range_with_recipient.recipient.is_none_or(|r| log_matching.to_address == r));
    assert!(!range_with_recipient.recipient.is_none_or(|r| log_non_matching.to_address == r));
}

#[test]
fn test_internal_error_sanitization_does_not_leak_raw_database_strings() {
    let raw_db_error = pay3::services::orders::OrderServiceError::Repository(
        pay3::db::repositories::RepositoryError::Database(
            sqlx::Error::Configuration("FATAL: password authentication failed for user pay3_app".into()),
        ),
    );

    let api_err = pay3::api::order_service_error_to_api_for_test(raw_db_error);
    assert_eq!(api_err.code(), "internal_error");
    assert_eq!(api_err.message(), "internal server error");
    assert!(!api_err.message().contains("password"));
    assert!(!api_err.message().contains("authentication"));
}
