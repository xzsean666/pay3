//! Pure domain value objects and state rules.

pub mod address;
pub mod amount;
pub mod chain;
pub mod collection;
pub mod kv;
pub mod order;
pub mod payment;
pub mod permit;
pub mod wallet;

pub use address::{BlockHash, EvmAddress, HexParseError, TxHash};
pub use amount::{AmountParseError, RawAmount, TokenAmount};
pub use chain::ChainBlockRef;
pub use collection::{
    CollectionFees, CollectionPurpose, CollectionReplacementError, CollectionStatus,
    CollectionTxPlan,
};
pub use kv::{KvReorgEpoch, KvReorgEpochError};
pub use order::{OrderStatus, OrderStatusDecision, OrderStatusError, recompute_order_status};
pub use payment::{PaymentChainStatus, PaymentFact, PaymentMatchStatus};
pub use permit::{
    eip2612_struct_hash, eip3009_struct_hash, eip712_digest, eip712_domain_separator,
    encode_eip2612_permit, encode_eip3009_transfer_with_authorization,
    encode_polygon_execute_meta_transaction, polygon_meta_tx_domain_separator,
    polygon_meta_tx_struct_hash, CollectionMethod, TypedSignature,
};
pub use wallet::{DerivationSegment, DerivationSegmentError, MAX_DERIVATION_INDEX};
