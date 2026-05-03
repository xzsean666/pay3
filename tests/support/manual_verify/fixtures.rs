use pay3::{
    db::repositories::{
        ChildAccountRecord, MatchedPaymentInput, OrderRecord, OrderView, PaymentRecord,
        PaymentWindowRecord,
    },
    domain::{
        BlockHash, ChainBlockRef, DerivationSegment, EvmAddress, OrderStatus, RawAmount, TxHash,
    },
    transfer_log_store::{StoredTransferLog, StreamId},
};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

pub fn order_view(expected_amount_raw: RawAmount) -> OrderView {
    let segment = DerivationSegment::new(0, 0, 42).unwrap();
    let child_account_id = Uuid::from_u128(100);

    OrderView {
        order: OrderRecord {
            id: order_id(),
            external_id: "merchant-order-1".to_string(),
            request_hash: "0xrequest".to_string(),
            child_account_id,
            receive_address: receive_address(),
            chain_id: stream().chain_id,
            token_address: stream().token_address,
            expected_amount_raw,
            paid_amount_raw: RawAmount::ZERO,
            status: OrderStatus::Pending,
            expires_at: now() + Duration::seconds(15),
            monitor_until: now() + Duration::seconds(30),
            created_at: now(),
            updated_at: now(),
        },
        child_account: ChildAccountRecord {
            id: child_account_id,
            signer_key_ref: "pay3-master".to_string(),
            derivation_version: 1,
            derivation_segment: segment,
            derivation_path: segment.derivation_path(),
            address: receive_address(),
            last_used_at: Some(now()),
            created_at: now(),
        },
        payment_window: PaymentWindowRecord {
            id: Uuid::from_u128(200),
            order_id: order_id(),
            child_account_id,
            receive_address: receive_address(),
            window_from: now(),
            window_from_block: ChainBlockRef::new(10, block_hash(10)),
            expires_at: now() + Duration::seconds(15),
            monitor_until: now() + Duration::seconds(30),
            created_at: now(),
        },
    }
}

pub fn payment_record_from_match(payment: MatchedPaymentInput) -> PaymentRecord {
    PaymentRecord {
        id: payment.id,
        order_id: payment.order_id,
        child_account_id: payment.child_account_id,
        chain_id: payment.chain_id,
        token_address: payment.token_address,
        tx_hash: payment.tx_hash,
        log_index: payment.log_index,
        from_address: payment.from_address,
        to_address: payment.to_address,
        amount_raw: payment.amount_raw,
        block_number: payment.block_number,
        block_hash: payment.block_hash,
        block_time: payment.block_time,
        confirmations: payment.confirmations,
        match_status: payment.match_status,
        chain_status: payment.chain_status,
        created_at: now(),
        updated_at: now(),
    }
}

pub fn stored_log(
    block_number: u64,
    log_index: u64,
    to_address: EvmAddress,
    amount_raw: RawAmount,
    block_second: i64,
) -> StoredTransferLog {
    StoredTransferLog {
        chain_id: stream().chain_id,
        token_address: stream().token_address,
        block_number,
        block_hash: block_hash(block_number as u8),
        block_timestamp: now() + Duration::seconds(block_second),
        tx_hash: tx_hash(block_number, log_index),
        tx_index: Some(0),
        log_index,
        from_address: address(0x01),
        to_address,
        amount_raw,
        removed: false,
        observed_at: now() + Duration::seconds(99),
    }
}

pub fn stream() -> StreamId {
    StreamId::new(1, address(0x11))
}

pub fn order_id() -> Uuid {
    Uuid::from_u128(42)
}

pub fn receive_address() -> EvmAddress {
    address(0xaa)
}

pub fn now() -> OffsetDateTime {
    OffsetDateTime::UNIX_EPOCH
}

pub const fn address(byte: u8) -> EvmAddress {
    EvmAddress::from_bytes([byte; 20])
}

pub const fn block_hash(byte: u8) -> BlockHash {
    BlockHash::from_bytes([byte; 32])
}

fn tx_hash(block: u64, index: u64) -> TxHash {
    let mut bytes = [0; 32];
    bytes[..8].copy_from_slice(&block.to_be_bytes());
    bytes[8..16].copy_from_slice(&index.to_be_bytes());
    TxHash::from_bytes(bytes)
}
