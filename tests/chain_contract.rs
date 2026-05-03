const CHAIN_MODULE: &str = include_str!("../src/chain/mod.rs");

#[test]
fn chain_module_exposes_normalized_contracts_and_fake_controls() {
    for fragment in [
        "pub trait Erc20ChainClient",
        "pub trait ChainHeaderReader",
        "pub trait TransferLogSource",
        "pub struct TransferLog",
        "pub struct TxReceipt",
        "async fn transfer_logs(&self, range: TransferLogRange)",
        "async fn capacity_probe",
        "replace_block_for_reorg",
        "fail_next",
        "ChainIdMismatch",
        "CapacityExceeded",
    ] {
        assert!(CHAIN_MODULE.contains(fragment), "missing {fragment}");
    }
}

#[test]
fn chain_module_stays_out_of_http_db_and_order_business_state() {
    for forbidden in [
        "axum",
        "Router",
        "sqlx",
        "PgPool",
        "OrderRecord",
        "OrderStatus",
        "payment_windows",
        "orders o",
    ] {
        assert!(
            !CHAIN_MODULE.contains(forbidden),
            "chain module must not depend on HTTP, DB, or order business state: {forbidden}"
        );
    }
}
