//! Pure domain value objects and state rules.

pub mod address;
pub mod amount;
pub mod chain;
pub mod collection;
pub mod kv;
pub mod order;
pub mod payment;
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
pub use wallet::{DerivationSegment, DerivationSegmentError, MAX_DERIVATION_INDEX};
