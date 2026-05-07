const CHAIN_MODULE: &str = include_str!("../src/chain/mod.rs");
const CHAIN_RPC_MODULE: &str = include_str!("../src/chain/rpc.rs");

#[test]
fn chain_module_exposes_normalized_contracts_and_fake_controls() {
    for fragment in [
        "pub trait Erc20ChainClient",
        "pub trait ChainHeaderReader",
        "pub trait TransferLogSource",
        "pub trait NativeBalanceReader",
        "pub trait Eip1559FeeEstimator",
        "pub trait JsonRpcProvider",
        "pub struct RpcProviderManager",
        "pub struct RpcRangeSource",
        "pub struct HttpJsonRpcProvider",
        "pub struct TransferLog",
        "pub struct TxReceipt",
        "async fn transfer_logs(&self, range: TransferLogRange)",
        "async fn capacity_probe",
        "async fn native_balance",
        "eth_getBalance",
        "eth_feeHistory",
        "eth_maxPriorityFeePerGas",
        "eth_gasPrice",
        "set_native_balance",
        "set_fee_estimate",
        "ProviderHashMismatch",
        "ERC20_TRANSFER_TOPIC",
        "replace_block_for_reorg",
        "fail_next",
        "ChainIdMismatch",
        "CapacityExceeded",
    ] {
        assert!(
            CHAIN_MODULE.contains(fragment) || CHAIN_RPC_MODULE.contains(fragment),
            "missing {fragment}"
        );
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
            !CHAIN_MODULE.contains(forbidden) && !CHAIN_RPC_MODULE.contains(forbidden),
            "chain module must not depend on API, DB, or order business state: {forbidden}"
        );
    }
}
