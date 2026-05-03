const ORDERS_REPOSITORY: &str = include_str!("../src/db/repositories/orders.rs");
const PAYMENTS_REPOSITORY: &str = include_str!("../src/db/repositories/payments.rs");
const PAYMENT_RECORDS_REPOSITORY: &str = include_str!("../src/db/repositories/payment_records.rs");
const COLLECTIONS_REPOSITORY: &str = include_str!("../src/db/repositories/collections.rs");
const OUTBOUND_REPOSITORY: &str = include_str!("../src/db/repositories/outbound.rs");
const AUDIT_REPOSITORY: &str = include_str!("../src/db/repositories/audit.rs");

#[test]
fn order_repository_uses_external_id_lock_and_idempotency() {
    for fragment in [
        "pg_advisory_xact_lock(hashtext($1)::bigint)",
        "fetch_order_by_external_id_tx",
        "idempotency_conflict",
        "INSERT INTO child_accounts",
        "INSERT INTO orders",
        "INSERT INTO payment_windows",
        "get_order_view",
        "JOIN child_accounts",
        "JOIN payment_windows",
        "FOR UPDATE",
    ] {
        assert!(ORDERS_REPOSITORY.contains(fragment), "missing {fragment}");
    }
}

#[test]
fn payment_repository_uses_cursor_lease_cas_and_matched_payments_only() {
    let payment_write_path = format!("{PAYMENTS_REPOSITORY}\n{PAYMENT_RECORDS_REPOSITORY}");

    for fragment in [
        "lease_owner",
        "lease_until",
        "expected_last_scanned_block",
        "expected_seen_kv_reorg_epoch",
        "CursorCasMismatch",
        "INSERT INTO payments",
        "ON CONFLICT (chain_id, tx_hash, log_index)",
        "chain_status = 'orphaned'",
        "UPDATE chain_cursors",
        "recompute_orders_in_tx",
    ] {
        assert!(payment_write_path.contains(fragment), "missing {fragment}");
    }

    for forbidden in ["raw_logs", "raw_transfer_logs", "raw_rpc", "range_manifest"] {
        assert!(
            !payment_write_path.contains(forbidden),
            "payment repository must not persist raw scan data: {forbidden}"
        );
    }
}

#[test]
fn collection_repository_uses_job_lock_and_treasury_backed_insert() {
    for fragment in [
        "FOR UPDATE SKIP LOCKED",
        "status = 'queued'",
        "idempotency_key",
        "request_hash",
        "status != \"paid\"",
        "INSERT INTO collections",
        "attach_outbound_tx",
    ] {
        assert!(
            COLLECTIONS_REPOSITORY.contains(fragment),
            "missing {fragment}"
        );
    }

    assert!(
        !COLLECTIONS_REPOSITORY.contains("INSERT INTO treasury_addresses"),
        "collection repository must rely on configured treasury rows, not create treasury addresses"
    );
}

#[test]
fn outbound_repository_serializes_nonce_and_preserves_replacement_invariants() {
    for fragment in [
        "INSERT INTO account_nonces",
        "FOR UPDATE",
        "SET next_nonce = next_nonce + 1",
        "INSERT INTO outbound_transactions",
        "status = 'replaced'",
        "ensure_replacement_invariants",
        "old.chain_id != replacement.chain_id",
        "old.purpose != replacement.purpose",
        "old.from_address != replacement.from_address",
        "old.to_address != replacement.to_address",
        "old.nonce != replacement.nonce",
    ] {
        assert!(OUTBOUND_REPOSITORY.contains(fragment), "missing {fragment}");
    }
}

#[test]
fn audit_repository_appends_audit_events() {
    for fragment in [
        "INSERT INTO audit_events",
        "event_type",
        "request_id",
        "principal_sub",
        "collection_id",
        "payload",
    ] {
        assert!(AUDIT_REPOSITORY.contains(fragment), "missing {fragment}");
    }
}
