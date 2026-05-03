use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::domain::{
    BlockHash, ChainBlockRef, DerivationSegment, EvmAddress, KvReorgEpoch, OrderStatus,
    PaymentChainStatus, PaymentMatchStatus, RawAmount, TxHash,
};

use super::error::RepositoryError;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewChildAccount {
    pub id: Uuid,
    pub signer_key_ref: String,
    pub derivation_version: u32,
    pub derivation_segment: DerivationSegment,
    pub derivation_path: String,
    pub address: EvmAddress,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChildAccountRecord {
    pub id: Uuid,
    pub signer_key_ref: String,
    pub derivation_version: u32,
    pub derivation_segment: DerivationSegment,
    pub derivation_path: String,
    pub address: EvmAddress,
    pub last_used_at: Option<OffsetDateTime>,
    pub created_at: OffsetDateTime,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewPaymentWindow {
    pub id: Uuid,
    pub order_id: Uuid,
    pub child_account_id: Uuid,
    pub receive_address: EvmAddress,
    pub window_from: OffsetDateTime,
    pub window_from_block: ChainBlockRef,
    pub expires_at: OffsetDateTime,
    pub monitor_until: OffsetDateTime,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaymentWindowRecord {
    pub id: Uuid,
    pub order_id: Uuid,
    pub child_account_id: Uuid,
    pub receive_address: EvmAddress,
    pub window_from: OffsetDateTime,
    pub window_from_block: ChainBlockRef,
    pub expires_at: OffsetDateTime,
    pub monitor_until: OffsetDateTime,
    pub created_at: OffsetDateTime,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateOrderCommand {
    pub order_id: Uuid,
    pub external_id: String,
    pub request_hash: String,
    pub child_account: NewChildAccount,
    pub chain_id: u64,
    pub token_address: EvmAddress,
    pub expected_amount_raw: RawAmount,
    pub payment_window: NewPaymentWindow,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderRecord {
    pub id: Uuid,
    pub external_id: String,
    pub request_hash: String,
    pub child_account_id: Uuid,
    pub receive_address: EvmAddress,
    pub chain_id: u64,
    pub token_address: EvmAddress,
    pub expected_amount_raw: RawAmount,
    pub paid_amount_raw: RawAmount,
    pub status: OrderStatus,
    pub expires_at: OffsetDateTime,
    pub monitor_until: OffsetDateTime,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderView {
    pub order: OrderRecord,
    pub child_account: ChildAccountRecord,
    pub payment_window: PaymentWindowRecord,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaymentWindowCandidate {
    pub order_id: Uuid,
    pub child_account_id: Uuid,
    pub receive_address: EvmAddress,
    pub chain_id: u64,
    pub token_address: EvmAddress,
    pub expected_amount_raw: RawAmount,
    pub paid_amount_raw: RawAmount,
    pub order_status: OrderStatus,
    pub window_from: OffsetDateTime,
    pub window_from_block: ChainBlockRef,
    pub expires_at: OffsetDateTime,
    pub monitor_until: OffsetDateTime,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaymentRecord {
    pub id: Uuid,
    pub order_id: Uuid,
    pub child_account_id: Uuid,
    pub chain_id: u64,
    pub token_address: EvmAddress,
    pub tx_hash: TxHash,
    pub log_index: u64,
    pub from_address: EvmAddress,
    pub to_address: EvmAddress,
    pub amount_raw: RawAmount,
    pub block_number: u64,
    pub block_hash: BlockHash,
    pub block_time: OffsetDateTime,
    pub confirmations: u64,
    pub match_status: PaymentMatchStatus,
    pub chain_status: PaymentChainStatus,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatchedPaymentInput {
    pub id: Uuid,
    pub order_id: Uuid,
    pub child_account_id: Uuid,
    pub chain_id: u64,
    pub token_address: EvmAddress,
    pub tx_hash: TxHash,
    pub log_index: u64,
    pub from_address: EvmAddress,
    pub to_address: EvmAddress,
    pub amount_raw: RawAmount,
    pub block_number: u64,
    pub block_hash: BlockHash,
    pub block_time: OffsetDateTime,
    pub confirmations: u64,
    pub match_status: PaymentMatchStatus,
    pub chain_status: PaymentChainStatus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanBlockRange {
    pub from_block: u64,
    pub to_block: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanCursorLease {
    pub chain_id: u64,
    pub token_address: EvmAddress,
    pub lease_owner: String,
    pub lease_until: OffsetDateTime,
    pub last_scanned_block: u64,
    pub seen_kv_reorg_epoch: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitScannedBatch {
    pub chain_id: u64,
    pub token_address: EvmAddress,
    pub worker_id: String,
    pub expected_last_scanned_block: u64,
    pub complete_to_block: u64,
    pub expected_seen_kv_reorg_epoch: u64,
    pub seen_kv_reorg_epoch: u64,
    pub matched_payments: Vec<MatchedPaymentInput>,
    pub recompute_order_ids: Vec<Uuid>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KvReorgCursorUpdate {
    pub chain_id: u64,
    pub token_address: EvmAddress,
    pub epoch: KvReorgEpoch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CollectionRecordStatus {
    Queued,
    Transferring,
    Confirming,
    Confirmed,
    Failed,
    Dropped,
    Replacing,
    Replaced,
}

impl CollectionRecordStatus {
    pub const fn as_db_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Transferring => "transferring",
            Self::Confirming => "confirming",
            Self::Confirmed => "confirmed",
            Self::Failed => "failed",
            Self::Dropped => "dropped",
            Self::Replacing => "replacing",
            Self::Replaced => "replaced",
        }
    }
}

impl TryFrom<&str> for CollectionRecordStatus {
    type Error = RepositoryError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "queued" => Ok(Self::Queued),
            "transferring" => Ok(Self::Transferring),
            "confirming" => Ok(Self::Confirming),
            "confirmed" => Ok(Self::Confirmed),
            "failed" => Ok(Self::Failed),
            "dropped" => Ok(Self::Dropped),
            "replacing" => Ok(Self::Replacing),
            "replaced" => Ok(Self::Replaced),
            _ => Err(RepositoryError::invalid_db_value(
                "collections.status",
                value,
                "unknown collection status",
            )),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateCollectionCommand {
    pub collection_id: Uuid,
    pub order_id: Uuid,
    pub idempotency_key: String,
    pub request_hash: String,
    pub child_account_id: Uuid,
    pub chain_id: u64,
    pub token_address: EvmAddress,
    pub from_address: EvmAddress,
    pub to_address: EvmAddress,
    pub amount_raw: Option<RawAmount>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectionRecord {
    pub id: Uuid,
    pub order_id: Uuid,
    pub idempotency_key: String,
    pub request_hash: String,
    pub child_account_id: Uuid,
    pub chain_id: u64,
    pub token_address: EvmAddress,
    pub from_address: EvmAddress,
    pub to_address: EvmAddress,
    pub amount_raw: Option<RawAmount>,
    pub status: CollectionRecordStatus,
    pub outbound_tx_id: Option<Uuid>,
    pub attempt_count: u32,
    pub locked_by: Option<String>,
    pub locked_until: Option<OffsetDateTime>,
    pub error: Option<String>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectionJob {
    pub collection: CollectionRecord,
    pub signer_key_ref: String,
    pub derivation_version: u32,
    pub derivation_segment: DerivationSegment,
    pub derivation_path: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutboundTxPurpose {
    Collect,
}

impl OutboundTxPurpose {
    pub const fn as_db_str(self) -> &'static str {
        match self {
            Self::Collect => "collect",
        }
    }
}

impl TryFrom<&str> for OutboundTxPurpose {
    type Error = RepositoryError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "collect" => Ok(Self::Collect),
            _ => Err(RepositoryError::invalid_db_value(
                "outbound_transactions.purpose",
                value,
                "unknown outbound transaction purpose",
            )),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutboundTxStatus {
    Signed,
    Broadcast,
    Confirmed,
    Failed,
    Dropped,
    Replaced,
}

impl OutboundTxStatus {
    pub const fn as_db_str(self) -> &'static str {
        match self {
            Self::Signed => "signed",
            Self::Broadcast => "broadcast",
            Self::Confirmed => "confirmed",
            Self::Failed => "failed",
            Self::Dropped => "dropped",
            Self::Replaced => "replaced",
        }
    }
}

impl TryFrom<&str> for OutboundTxStatus {
    type Error = RepositoryError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "signed" => Ok(Self::Signed),
            "broadcast" => Ok(Self::Broadcast),
            "confirmed" => Ok(Self::Confirmed),
            "failed" => Ok(Self::Failed),
            "dropped" => Ok(Self::Dropped),
            "replaced" => Ok(Self::Replaced),
            _ => Err(RepositoryError::invalid_db_value(
                "outbound_transactions.status",
                value,
                "unknown outbound transaction status",
            )),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReservedNonce {
    pub chain_id: u64,
    pub address: EvmAddress,
    pub nonce: RawAmount,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewSignedOutboundTx {
    pub id: Uuid,
    pub chain_id: u64,
    pub purpose: OutboundTxPurpose,
    pub from_address: EvmAddress,
    pub to_address: EvmAddress,
    pub nonce: RawAmount,
    pub tx_hash: TxHash,
    pub signed_tx: Vec<u8>,
    pub replacement_of: Option<Uuid>,
    pub replacement_reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboundTxRecord {
    pub id: Uuid,
    pub chain_id: u64,
    pub purpose: OutboundTxPurpose,
    pub from_address: EvmAddress,
    pub to_address: EvmAddress,
    pub nonce: RawAmount,
    pub tx_hash: TxHash,
    pub signed_tx: Vec<u8>,
    pub status: OutboundTxStatus,
    pub replacement_of: Option<Uuid>,
    pub replacement_reason: Option<String>,
    pub broadcast_count: u32,
    pub last_broadcast_at: Option<OffsetDateTime>,
    pub receipt_block: Option<ChainBlockRef>,
    pub error: Option<String>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BroadcastableOutboundTx {
    pub collection_id: Uuid,
    pub outbound: OutboundTxRecord,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiptCheckableOutboundTx {
    pub collection_id: Uuid,
    pub outbound: OutboundTxRecord,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEventInput {
    pub id: Uuid,
    pub event_type: String,
    pub request_id: Option<String>,
    pub principal_sub: Option<String>,
    pub scopes: Option<String>,
    pub order_id: Option<Uuid>,
    pub collection_id: Option<Uuid>,
    pub tx_hash: Option<TxHash>,
    pub payload: Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEventRecord {
    pub id: Uuid,
    pub event_type: String,
    pub request_id: Option<String>,
    pub principal_sub: Option<String>,
    pub scopes: Option<String>,
    pub order_id: Option<Uuid>,
    pub collection_id: Option<Uuid>,
    pub tx_hash: Option<TxHash>,
    pub payload: Value,
    pub created_at: OffsetDateTime,
}

pub fn order_status_as_db_str(status: OrderStatus) -> &'static str {
    match status {
        OrderStatus::Pending => "pending",
        OrderStatus::Partial => "partial",
        OrderStatus::Confirming => "confirming",
        OrderStatus::Paid => "paid",
        OrderStatus::Expired => "expired",
    }
}

pub fn parse_order_status(value: &str) -> Result<OrderStatus, RepositoryError> {
    match value {
        "pending" => Ok(OrderStatus::Pending),
        "partial" => Ok(OrderStatus::Partial),
        "confirming" => Ok(OrderStatus::Confirming),
        "paid" => Ok(OrderStatus::Paid),
        "expired" => Ok(OrderStatus::Expired),
        _ => Err(RepositoryError::invalid_db_value(
            "orders.status",
            value,
            "unknown order status",
        )),
    }
}

pub fn payment_match_status_as_db_str(status: PaymentMatchStatus) -> &'static str {
    match status {
        PaymentMatchStatus::OnTime => "on_time",
        PaymentMatchStatus::Late => "late",
        PaymentMatchStatus::OutsideWindow => "outside_window",
    }
}

pub fn parse_payment_match_status(value: &str) -> Result<PaymentMatchStatus, RepositoryError> {
    match value {
        "on_time" => Ok(PaymentMatchStatus::OnTime),
        "late" => Ok(PaymentMatchStatus::Late),
        "outside_window" => Ok(PaymentMatchStatus::OutsideWindow),
        _ => Err(RepositoryError::invalid_db_value(
            "payments.match_status",
            value,
            "unknown payment match status",
        )),
    }
}

pub fn payment_chain_status_as_db_str(status: PaymentChainStatus) -> &'static str {
    match status {
        PaymentChainStatus::Observed => "observed",
        PaymentChainStatus::Confirmed => "confirmed",
        PaymentChainStatus::Orphaned => "orphaned",
    }
}

pub fn parse_payment_chain_status(value: &str) -> Result<PaymentChainStatus, RepositoryError> {
    match value {
        "observed" => Ok(PaymentChainStatus::Observed),
        "confirmed" => Ok(PaymentChainStatus::Confirmed),
        "orphaned" => Ok(PaymentChainStatus::Orphaned),
        _ => Err(RepositoryError::invalid_db_value(
            "payments.chain_status",
            value,
            "unknown payment chain status",
        )),
    }
}

pub fn block_ref(number: u64, hash: BlockHash) -> ChainBlockRef {
    ChainBlockRef { number, hash }
}
