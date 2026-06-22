use pay3::db::migrations::MIGRATOR;
use sqlx::{Connection, Executor, PgConnection};
use std::{env, error::Error, process, time::SystemTime};

const INITIAL_SCHEMA: &str = include_str!("../src/db/migrations/20260502000100_initial_schema.sql");
const AUTO_COLLECTION_INDEXES: &str =
    include_str!("../src/db/migrations/20260507000100_auto_collection_indexes.sql");
const ORDER_OWNER_SUB: &str =
    include_str!("../src/db/migrations/20260507000200_order_owner_sub.sql");
const COLLECTION_OWNER_SUB: &str =
    include_str!("../src/db/migrations/20260507000300_collection_owner_sub.sql");
const ORDER_PAYMENT_OVERRIDES: &str =
    include_str!("../src/db/migrations/20260622000100_order_payment_overrides.sql");

#[test]
fn migrator_embeds_initial_schema() {
    let versions = MIGRATOR
        .iter()
        .map(|migration| migration.version)
        .collect::<Vec<_>>();
    assert_eq!(
        versions,
        vec![
            20260502000100,
            20260507000100,
            20260507000200,
            20260507000300,
            20260622000100
        ]
    );
}

#[test]
fn initial_schema_contains_required_tables() {
    for table in [
        "wallet_cursors",
        "child_accounts",
        "orders",
        "payment_windows",
        "payments",
        "chain_cursors",
        "treasury_addresses",
        "collections",
        "account_nonces",
        "outbound_transactions",
        "audit_events",
    ] {
        assert!(
            INITIAL_SCHEMA.contains(&format!("CREATE TABLE {table}")),
            "missing table {table}"
        );
    }
}

#[test]
fn initial_schema_enforces_address_and_hash_shapes() {
    for check in [
        "receive_address ~ '^0x[0-9a-f]{40}$'",
        "token_address ~ '^0x[0-9a-f]{40}$'",
        "from_address ~ '^0x[0-9a-f]{40}$'",
        "to_address ~ '^0x[0-9a-f]{40}$'",
        "treasury_address ~ '^0x[0-9a-f]{40}$'",
        "tx_hash ~ '^0x[0-9a-f]{64}$'",
        "block_hash ~ '^0x[0-9a-f]{64}$'",
        "window_from_block_hash ~ '^0x[0-9a-f]{64}$'",
    ] {
        assert!(INITIAL_SCHEMA.contains(check), "missing CHECK {check}");
    }
}

#[test]
fn initial_schema_contains_funds_safety_constraints() {
    for fragment in [
        "UNIQUE (id, address)",
        "receive_address text NOT NULL UNIQUE",
        "REFERENCES orders(id, chain_id, token_address, child_account_id, receive_address)",
        "REFERENCES treasury_addresses(chain_id, token_address, treasury_address)",
        "CREATE UNIQUE INDEX one_active_collection_per_child",
        "WHERE status IN ('queued', 'transferring', 'confirming')",
        "CREATE UNIQUE INDEX outbound_active_nonce_idx",
        "WHERE status IN ('signed', 'broadcast', 'confirmed')",
        "CREATE TRIGGER collections_outbound_tx_invariant",
        "CREATE TABLE audit_events",
    ] {
        assert!(INITIAL_SCHEMA.contains(fragment), "missing {fragment}");
    }
}

#[test]
fn auto_collection_migration_adds_background_scan_indexes() {
    for fragment in [
        "CREATE INDEX orders_paid_auto_collection_idx",
        "WHERE status = 'paid'",
        "CREATE INDEX collections_order_idx",
        "ON collections(order_id)",
    ] {
        assert!(
            AUTO_COLLECTION_INDEXES.contains(fragment),
            "missing {fragment}"
        );
    }
}

#[test]
fn order_owner_migration_scopes_external_ids_by_owner() {
    for fragment in [
        "ADD COLUMN owner_sub text",
        "SET owner_sub = 'legacy'",
        "ALTER COLUMN owner_sub SET NOT NULL",
        "ADD CONSTRAINT orders_owner_sub_not_empty CHECK (owner_sub <> '')",
        "DROP CONSTRAINT IF EXISTS orders_external_id_key",
        "CREATE UNIQUE INDEX orders_owner_external_id_idx",
        "ON orders(owner_sub, external_id)",
    ] {
        assert!(ORDER_OWNER_SUB.contains(fragment), "missing {fragment}");
    }
}

#[test]
fn collection_owner_migration_scopes_reads_and_idempotency_by_owner() {
    for fragment in [
        "ADD COLUMN owner_sub text",
        "SET owner_sub = o.owner_sub",
        "ALTER COLUMN owner_sub SET NOT NULL",
        "ADD CONSTRAINT collections_owner_sub_not_empty CHECK (owner_sub <> '')",
        "CREATE UNIQUE INDEX orders_id_owner_sub_idx",
        "FOREIGN KEY (order_id, owner_sub) REFERENCES orders(id, owner_sub)",
        "DROP INDEX IF EXISTS collections_idempotency_key_idx",
        "CREATE UNIQUE INDEX collections_owner_idempotency_key_idx",
        "ON collections(owner_sub, idempotency_key)",
        "CREATE INDEX collections_owner_idx",
    ] {
        assert!(
            COLLECTION_OWNER_SUB.contains(fragment),
            "missing {fragment}"
        );
    }
}

#[test]
fn order_payment_override_migration_records_manual_problem_payment_acceptance() {
    for fragment in [
        "CREATE TABLE order_payment_overrides",
        "order_id uuid PRIMARY KEY REFERENCES orders(id)",
        "accepted_problem_payment_raw numeric(78, 0) NOT NULL",
        "accepted_by text NOT NULL",
        "CHECK (accepted_problem_payment_raw > 0)",
        "CREATE INDEX order_payment_overrides_updated_idx",
    ] {
        assert!(
            ORDER_PAYMENT_OVERRIDES.contains(fragment),
            "missing {fragment}"
        );
    }
}

#[test]
fn initial_schema_does_not_store_raw_chain_logs_in_postgres() {
    for forbidden in [
        "raw_transfer_logs",
        "raw_logs",
        "raw_rpc",
        "block_headers",
        "range_manifest",
        "non_pay3_logs",
    ] {
        assert!(
            !INITIAL_SCHEMA.contains(forbidden),
            "raw scan data belongs in KVDB, found {forbidden}"
        );
    }
}

#[tokio::test]
async fn migrations_apply_to_postgres_when_test_database_is_configured()
-> Result<(), Box<dyn Error>> {
    let Some(database_url) = env::var("PAY3_TEST_DATABASE_URL")
        .ok()
        .or_else(|| env::var("TEST_DATABASE_URL").ok())
    else {
        eprintln!("skipping migration up test; set PAY3_TEST_DATABASE_URL");
        return Ok(());
    };

    let mut conn = PgConnection::connect(&database_url).await?;
    let schema = format!(
        "pay3_migration_test_{}_{}",
        process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)?
            .as_nanos()
    );
    let schema_ident = quote_ident(&schema);

    conn.execute(format!("CREATE SCHEMA {schema_ident}").as_str())
        .await?;

    let result = async {
        conn.execute(format!("SET search_path TO {schema_ident}").as_str())
            .await?;
        MIGRATOR.run_direct(&mut conn).await?;

        let table_count: i64 = sqlx::query_scalar(
            r#"
            SELECT count(*)::bigint
            FROM information_schema.tables
            WHERE table_schema = $1
              AND table_name IN (
                'wallet_cursors',
                'child_accounts',
                'orders',
                'payment_windows',
                'payments',
                'chain_cursors',
                'treasury_addresses',
                'collections',
                'account_nonces',
                'outbound_transactions',
                'audit_events'
              )
            "#,
        )
        .bind(&schema)
        .fetch_one(&mut conn)
        .await?;
        assert_eq!(table_count, 11);

        let seeded_wallet_cursor: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM wallet_cursors WHERE id = 'default'")
                .fetch_one(&mut conn)
                .await?;
        assert_eq!(seeded_wallet_cursor, 1);

        let order_owner_not_null: bool = sqlx::query_scalar(
            r#"
            SELECT is_nullable = 'NO'
            FROM information_schema.columns
            WHERE table_schema = $1
              AND table_name = 'orders'
              AND column_name = 'owner_sub'
            "#,
        )
        .bind(&schema)
        .fetch_one(&mut conn)
        .await?;
        assert!(order_owner_not_null);

        let collection_owner_not_null: bool = sqlx::query_scalar(
            r#"
            SELECT is_nullable = 'NO'
            FROM information_schema.columns
            WHERE table_schema = $1
              AND table_name = 'collections'
              AND column_name = 'owner_sub'
            "#,
        )
        .bind(&schema)
        .fetch_one(&mut conn)
        .await?;
        assert!(collection_owner_not_null);

        let owner_indexes: i64 = sqlx::query_scalar(
            r#"
            SELECT count(*)::bigint
            FROM pg_indexes
            WHERE schemaname = $1
              AND indexname IN (
                'orders_owner_external_id_idx',
                'collections_owner_idempotency_key_idx'
              )
            "#,
        )
        .bind(&schema)
        .fetch_one(&mut conn)
        .await?;
        assert_eq!(owner_indexes, 2);

        let global_collection_idempotency_index: i64 = sqlx::query_scalar(
            r#"
            SELECT count(*)::bigint
            FROM pg_indexes
            WHERE schemaname = $1
              AND indexname = 'collections_idempotency_key_idx'
            "#,
        )
        .bind(&schema)
        .fetch_one(&mut conn)
        .await?;
        assert_eq!(global_collection_idempotency_index, 0);

        sqlx::query(
            r#"
            INSERT INTO child_accounts (
                id,
                signer_key_ref,
                derivation_version,
                account_index,
                change_index,
                address_index,
                derivation_path,
                address
            ) VALUES
                (
                    '00000000-0000-0000-0000-000000000101',
                    'pay3-master',
                    1,
                    0,
                    0,
                    1,
                    'm/44''/60''/0''/0/1',
                    '0x1111111111111111111111111111111111111111'
                ),
                (
                    '00000000-0000-0000-0000-000000000102',
                    'pay3-master',
                    1,
                    0,
                    0,
                    2,
                    'm/44''/60''/0''/0/2',
                    '0x2222222222222222222222222222222222222222'
                )
            "#,
        )
        .execute(&mut conn)
        .await?;

        sqlx::query(
            r#"
            INSERT INTO treasury_addresses (
                chain_id,
                token_address,
                treasury_address
            ) VALUES (
                1,
                '0x9999999999999999999999999999999999999999',
                '0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'
            )
            "#,
        )
        .execute(&mut conn)
        .await?;

        sqlx::query(
            r#"
            INSERT INTO orders (
                id,
                owner_sub,
                external_id,
                request_hash,
                child_account_id,
                receive_address,
                chain_id,
                token_address,
                expected_amount_raw,
                paid_amount_raw,
                status,
                expires_at,
                monitor_until
            ) VALUES
                (
                    '00000000-0000-0000-0000-000000000201',
                    'merchant-a',
                    'shared-external-id',
                    'request-a',
                    '00000000-0000-0000-0000-000000000101',
                    '0x1111111111111111111111111111111111111111',
                    1,
                    '0x9999999999999999999999999999999999999999',
                    100,
                    100,
                    'paid',
                    now() + interval '1 hour',
                    now() + interval '2 hours'
                ),
                (
                    '00000000-0000-0000-0000-000000000202',
                    'merchant-b',
                    'shared-external-id',
                    'request-b',
                    '00000000-0000-0000-0000-000000000102',
                    '0x2222222222222222222222222222222222222222',
                    1,
                    '0x9999999999999999999999999999999999999999',
                    100,
                    100,
                    'paid',
                    now() + interval '1 hour',
                    now() + interval '2 hours'
                )
            "#,
        )
        .execute(&mut conn)
        .await?;

        sqlx::query(
            r#"
            INSERT INTO collections (
                id,
                owner_sub,
                order_id,
                idempotency_key,
                request_hash,
                child_account_id,
                chain_id,
                token_address,
                from_address,
                to_address,
                status
            ) VALUES
                (
                    '00000000-0000-0000-0000-000000000301',
                    'merchant-a',
                    '00000000-0000-0000-0000-000000000201',
                    'shared-collection-key',
                    'collection-request-a',
                    '00000000-0000-0000-0000-000000000101',
                    1,
                    '0x9999999999999999999999999999999999999999',
                    '0x1111111111111111111111111111111111111111',
                    '0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
                    'queued'
                ),
                (
                    '00000000-0000-0000-0000-000000000302',
                    'merchant-b',
                    '00000000-0000-0000-0000-000000000202',
                    'shared-collection-key',
                    'collection-request-b',
                    '00000000-0000-0000-0000-000000000102',
                    1,
                    '0x9999999999999999999999999999999999999999',
                    '0x2222222222222222222222222222222222222222',
                    '0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
                    'queued'
                )
            "#,
        )
        .execute(&mut conn)
        .await?;

        let duplicate_collection = sqlx::query(
            r#"
            INSERT INTO collections (
                id,
                owner_sub,
                order_id,
                idempotency_key,
                request_hash,
                child_account_id,
                chain_id,
                token_address,
                from_address,
                to_address,
                status
            ) VALUES (
                '00000000-0000-0000-0000-000000000303',
                'merchant-a',
                '00000000-0000-0000-0000-000000000201',
                'shared-collection-key',
                'collection-request-c',
                '00000000-0000-0000-0000-000000000101',
                1,
                '0x9999999999999999999999999999999999999999',
                '0x1111111111111111111111111111111111111111',
                '0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
                'confirmed'
            )
            "#,
        )
        .execute(&mut conn)
        .await;
        assert!(duplicate_collection.is_err());

        Ok::<(), Box<dyn Error>>(())
    }
    .await;

    conn.execute(format!("DROP SCHEMA {schema_ident} CASCADE").as_str())
        .await?;

    result
}

fn quote_ident(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}
