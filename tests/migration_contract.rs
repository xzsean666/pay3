use pay3::db::migrations::MIGRATOR;
use sqlx::{Connection, Executor, PgConnection};
use std::{env, error::Error, process, time::SystemTime};

const INITIAL_SCHEMA: &str = include_str!("../src/db/migrations/20260502000100_initial_schema.sql");

#[test]
fn migrator_embeds_initial_schema() {
    assert_eq!(MIGRATOR.iter().count(), 1);
    assert_eq!(MIGRATOR.iter().next().unwrap().version, 20260502000100);
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
