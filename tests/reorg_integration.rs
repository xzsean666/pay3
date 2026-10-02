use std::{
    env,
    error::Error,
    process,
    time::{SystemTime, UNIX_EPOCH},
};

use bigdecimal::BigDecimal;
use pay3::{
    db::migrations::MIGRATOR,
    domain::{
        OrderStatus, PaymentChainStatus, PaymentFact, PaymentMatchStatus, RawAmount,
        recompute_order_status,
    },
};
use sqlx::{Connection, Executor, PgConnection, Row};
use uuid::Uuid;

#[allow(dead_code)]
#[derive(Debug)]
struct TestOrderFixture {
    chain_id: i64,
    token_address: String,
    treasury_address: String,
    child_address: String,
    order_id: Uuid,
}

#[tokio::test]
async fn reorg_marks_payments_orphaned_and_rewinds_cursor_in_postgres() -> Result<(), Box<dyn Error>> {
    let Some(database_url) = test_database_url() else {
        eprintln!(
            "skipping reorg DB integration test; set PAY3_TEST_DATABASE_URL or TEST_DATABASE_URL"
        );
        return Ok(());
    };

    let (mut conn, schema) = prepare_temp_schema(&database_url, "pay3_reorg_test").await?;
    let schema_ident = quote_ident(&schema);

    let result = async {
        let fixture = seed_order_fixture(&mut conn).await?;

        // 1. Initial state: cursor is at block 100, epoch 0
        insert_cursor(&mut conn, fixture.chain_id, &fixture.token_address, 100, 0).await?;

        // 2. A payment of 100 arrives at block 105, which is matching the full order amount
        let payment_id = Uuid::new_v4();
        insert_payment(
            &mut conn,
            payment_id,
            fixture.order_id,
            fixture.chain_id,
            &fixture.token_address,
            &fixture.child_address,
            BigDecimal::from(100u32),
            105,
            &tx_hash(1),
            0,
            "confirmed",
        )
        .await?;

        // Update cursor to 105
        update_cursor(&mut conn, fixture.chain_id, &fixture.token_address, 105, 0).await?;

        // Verify order is paid
        let order_status: String = sqlx::query_scalar("SELECT status FROM orders WHERE id = $1")
            .bind(fixture.order_id)
            .fetch_one(&mut conn)
            .await?;
        assert_eq!(order_status, "paid", "order must be paid after valid payment");

        // 3. Reorg occurs from block 103! Epoch advances to 1
        // Simulate handle_kv_reorg_epoch logic
        simulate_handle_reorg(
            &mut conn,
            fixture.chain_id,
            &fixture.token_address,
            1,   // new epoch
            103, // last_reorg_from
        )
        .await?;

        // 4. Assertions after Reorg:
        // A. Payment at block 105 must be 'orphaned'
        let payment_status: String =
            sqlx::query_scalar("SELECT chain_status FROM payments WHERE id = $1")
                .bind(payment_id)
                .fetch_one(&mut conn)
                .await?;
        assert_eq!(
            payment_status, "orphaned",
            "payment in forked block must be marked orphaned"
        );

        // B. Cursor last_scanned_block must be rewound to 102 (103 - 1)
        let (last_scanned, seen_epoch): (i64, i64) = sqlx::query_as(
            "SELECT last_scanned_block, seen_kv_reorg_epoch FROM chain_cursors WHERE chain_id = $1 AND token_address = $2",
        )
        .bind(fixture.chain_id)
        .bind(&fixture.token_address)
        .fetch_one(&mut conn)
        .await?;
        assert_eq!(last_scanned, 102, "cursor must rewind to last_reorg_from - 1");
        assert_eq!(seen_epoch, 1, "cursor seen_epoch must update to new epoch");

        // C. Order status must roll back from 'paid' to 'pending'
        let updated_order_status: String =
            sqlx::query_scalar("SELECT status FROM orders WHERE id = $1")
                .bind(fixture.order_id)
                .fetch_one(&mut conn)
                .await?;
        assert_eq!(
            updated_order_status, "pending",
            "order must revert to pending after payment was orphaned"
        );

        Ok::<(), Box<dyn Error>>(())
    }
    .await;

    let cleanup_result = drop_schema(&mut conn, &schema_ident).await;
    result?;
    cleanup_result?;
    Ok(())
}

#[tokio::test]
async fn reorg_partial_payment_recomputation_preserves_pre_reorg_payments(
) -> Result<(), Box<dyn Error>> {
    let Some(database_url) = test_database_url() else {
        return Ok(());
    };

    let (mut conn, schema) = prepare_temp_schema(&database_url, "pay3_reorg_partial").await?;
    let schema_ident = quote_ident(&schema);

    let result = async {
        let fixture = seed_order_fixture(&mut conn).await?;

        // Payment 1: Block 101, amount 40
        let p1_id = Uuid::new_v4();
        insert_payment(
            &mut conn,
            p1_id,
            fixture.order_id,
            fixture.chain_id,
            &fixture.token_address,
            &fixture.child_address,
            BigDecimal::from(40u32),
            101,
            &tx_hash(1),
            0,
            "confirmed",
        )
        .await?;

        // Payment 2: Block 106, amount 60
        let p2_id = Uuid::new_v4();
        insert_payment(
            &mut conn,
            p2_id,
            fixture.order_id,
            fixture.chain_id,
            &fixture.token_address,
            &fixture.child_address,
            BigDecimal::from(60u32),
            106,
            &tx_hash(2),
            0,
            "confirmed",
        )
        .await?;

        // Order total is 100 -> status is paid
        let order_status: String = sqlx::query_scalar("SELECT status FROM orders WHERE id = $1")
            .bind(fixture.order_id)
            .fetch_one(&mut conn)
            .await?;
        assert_eq!(order_status, "paid");

        // Reorg from block 105 (only affects Payment 2 at block 106)
        simulate_handle_reorg(
            &mut conn,
            fixture.chain_id,
            &fixture.token_address,
            1,
            105,
        )
        .await?;

        // Payment 1 must remain confirmed
        let p1_status: String =
            sqlx::query_scalar("SELECT chain_status FROM payments WHERE id = $1")
                .bind(p1_id)
                .fetch_one(&mut conn)
                .await?;
        assert_eq!(p1_status, "confirmed");

        // Payment 2 must become orphaned
        let p2_status: String =
            sqlx::query_scalar("SELECT chain_status FROM payments WHERE id = $1")
                .bind(p2_id)
                .fetch_one(&mut conn)
                .await?;
        assert_eq!(p2_status, "orphaned");

        // Order must now be 'partial' because 40 of 100 remains
        let updated_order_status: String =
            sqlx::query_scalar("SELECT status FROM orders WHERE id = $1")
                .bind(fixture.order_id)
                .fetch_one(&mut conn)
                .await?;
        assert_eq!(updated_order_status, "partial");

        Ok::<(), Box<dyn Error>>(())
    }
    .await;

    let cleanup_result = drop_schema(&mut conn, &schema_ident).await;
    result?;
    cleanup_result?;
    Ok(())
}

#[test]
fn domain_payment_status_orphaned_invariants() {
    let payment = PaymentFact::new(
        RawAmount::from(100),
        PaymentMatchStatus::OnTime,
        PaymentChainStatus::Orphaned,
    );
    assert!(!payment.is_confirmed_on_time());
    assert!(!payment.is_non_orphaned_on_time());

    let decision = recompute_order_status(RawAmount::from(100), [payment], false).unwrap();
    assert_eq!(decision.status, OrderStatus::Pending);
    assert_eq!(decision.confirmed_on_time_total, RawAmount::ZERO);
}


async fn simulate_handle_reorg(
    conn: &mut PgConnection,
    chain_id: i64,
    token_address: &str,
    epoch: i64,
    last_reorg_from: i64,
) -> Result<(), Box<dyn Error>> {
    let rewind_to = (last_reorg_from - 1).max(0);

    // 1. Mark payments as orphaned
    let affected_rows = sqlx::query(
        r#"
        UPDATE payments
        SET chain_status = 'orphaned',
            updated_at = now()
        WHERE chain_id = $1
          AND token_address = $2
          AND block_number >= $3
          AND chain_status <> 'orphaned'
        RETURNING order_id
        "#,
    )
    .bind(chain_id)
    .bind(token_address)
    .bind(last_reorg_from)
    .fetch_all(&mut *conn)
    .await?;

    let mut affected_order_ids: Vec<Uuid> = Vec::new();
    for row in affected_rows {
        affected_order_ids.push(row.try_get("order_id")?);
    }

    // 2. Rewind cursor
    sqlx::query(
        r#"
        UPDATE chain_cursors
        SET last_scanned_block = LEAST(last_scanned_block, $3),
            seen_kv_reorg_epoch = $4,
            lease_owner = NULL,
            lease_until = NULL,
            updated_at = now()
        WHERE chain_id = $1 AND token_address = $2
        "#,
    )
    .bind(chain_id)
    .bind(token_address)
    .bind(rewind_to)
    .bind(epoch)
    .execute(&mut *conn)
    .await?;

    // 3. Recompute affected orders
    for order_id in affected_order_ids {
        recompute_order(conn, order_id).await?;
    }

    Ok(())
}

async fn recompute_order(conn: &mut PgConnection, order_id: Uuid) -> Result<(), Box<dyn Error>> {
    // Sum only non-orphaned payments
    let paid_sum: Option<BigDecimal> = sqlx::query_scalar(
        r#"
        SELECT COALESCE(SUM(amount_raw), 0)
        FROM payments
        WHERE order_id = $1
          AND chain_status <> 'orphaned'
        "#,
    )
    .bind(order_id)
    .fetch_one(&mut *conn)
    .await?;

    let paid_sum = paid_sum.unwrap_or_else(|| BigDecimal::from(0));

    let (expected_amount, expires_at): (BigDecimal, time::OffsetDateTime) = sqlx::query_as(
        r#"
        SELECT expected_amount_raw, expires_at
        FROM orders
        WHERE id = $1
        "#,
    )
    .bind(order_id)
    .fetch_one(&mut *conn)
    .await?;

    let now = time::OffsetDateTime::now_utc();
    let new_status = if paid_sum >= expected_amount {
        "paid"
    } else if paid_sum > BigDecimal::from(0) {
        if expires_at <= now {
            "expired"
        } else {
            "partial"
        }
    } else if expires_at <= now {
        "expired"
    } else {
        "pending"
    };

    sqlx::query("UPDATE orders SET status = $1, updated_at = now() WHERE id = $2")
        .bind(new_status)
        .bind(order_id)
        .execute(&mut *conn)
        .await?;

    Ok(())
}

async fn seed_order_fixture(conn: &mut PgConnection) -> Result<TestOrderFixture, Box<dyn Error>> {
    let chain_id = 1i64;
    let token_address = address(1);
    let treasury_address = address(2);
    let child_address = address(3);
    let order_id = Uuid::new_v4();

    sqlx::query(
        r#"
        INSERT INTO chain_configs (chain_id, chain_name, rpc_urls, required_confirmations)
        VALUES ($1, 'test', '["http://localhost:8545"]', 1)
        ON CONFLICT (chain_id) DO NOTHING
        "#,
    )
    .bind(chain_id)
    .execute(&mut *conn)
    .await?;

    sqlx::query(
        r#"
        INSERT INTO treasury_addresses (chain_id, token_address, treasury_address, label)
        VALUES ($1, $2, $3, 'default')
        ON CONFLICT (chain_id, token_address) DO NOTHING
        "#,
    )
    .bind(chain_id)
    .bind(&token_address)
    .bind(&treasury_address)
    .execute(&mut *conn)
    .await?;

    sqlx::query(
        r#"
        INSERT INTO child_accounts (chain_id, derivation_index, address)
        VALUES ($1, 1, $2)
        ON CONFLICT (chain_id, address) DO NOTHING
        "#,
    )
    .bind(chain_id)
    .bind(&child_address)
    .execute(&mut *conn)
    .await?;

    let now = time::OffsetDateTime::now_utc();
    let expires = now + time::Duration::hours(2);

    sqlx::query(
        r#"
        INSERT INTO orders (
            id, owner_sub, external_id, chain_id, token_address,
            receive_address, expected_amount_raw, status, expires_at, created_at, updated_at
        )
        VALUES ($1, 'test-owner', $2, $3, $4, $5, 100, 'pending', $6, $7, $7)
        "#,
    )
    .bind(order_id)
    .bind(format!("ext-{}", order_id))
    .bind(chain_id)
    .bind(&token_address)
    .bind(&child_address)
    .bind(expires)
    .bind(now)
    .execute(&mut *conn)
    .await?;

    Ok(TestOrderFixture {
        chain_id,
        token_address,
        treasury_address,
        child_address,
        order_id,
    })
}

async fn insert_cursor(
    conn: &mut PgConnection,
    chain_id: i64,
    token_address: &str,
    last_scanned: i64,
    seen_epoch: i64,
) -> Result<(), Box<dyn Error>> {
    sqlx::query(
        r#"
        INSERT INTO chain_cursors (
            chain_id, token_address, last_scanned_block, seen_kv_reorg_epoch, updated_at
        )
        VALUES ($1, $2, $3, $4, now())
        ON CONFLICT (chain_id, token_address) DO UPDATE
        SET last_scanned_block = EXCLUDED.last_scanned_block,
            seen_kv_reorg_epoch = EXCLUDED.seen_kv_reorg_epoch,
            updated_at = now()
        "#,
    )
    .bind(chain_id)
    .bind(token_address)
    .bind(last_scanned)
    .bind(seen_epoch)
    .execute(conn)
    .await?;
    Ok(())
}

async fn update_cursor(
    conn: &mut PgConnection,
    chain_id: i64,
    token_address: &str,
    last_scanned: i64,
    seen_epoch: i64,
) -> Result<(), Box<dyn Error>> {
    insert_cursor(conn, chain_id, token_address, last_scanned, seen_epoch).await
}

async fn insert_payment(
    conn: &mut PgConnection,
    id: Uuid,
    order_id: Uuid,
    chain_id: i64,
    token_address: &str,
    to_address: &str,
    amount: BigDecimal,
    block_number: i64,
    tx_hash: &str,
    log_index: i64,
    status: &str,
) -> Result<(), Box<dyn Error>> {
    sqlx::query(
        r#"
        INSERT INTO payments (
            id, order_id, chain_id, token_address, from_address, to_address,
            amount_raw, block_number, block_hash, tx_hash, log_index,
            block_timestamp, classification, chain_status, created_at, updated_at
        )
        VALUES (
            $1, $2, $3, $4, $5, $6,
            $7, $8, $9, $10, $11,
            now(), 'on_time', $12, now(), now()
        )
        "#,
    )
    .bind(id)
    .bind(order_id)
    .bind(chain_id)
    .bind(token_address)
    .bind(address(99))
    .bind(to_address)
    .bind(amount)
    .bind(block_number)
    .bind(tx_hash)
    .bind(tx_hash)
    .bind(log_index)
    .bind(status)
    .execute(&mut *conn)
    .await?;

    recompute_order(conn, order_id).await?;
    Ok(())
}

async fn prepare_temp_schema(
    database_url: &str,
    prefix: &str,
) -> Result<(PgConnection, String), Box<dyn Error>> {
    let mut conn = PgConnection::connect(database_url).await?;
    let schema = temp_schema_name(prefix)?;
    let schema_ident = quote_ident(&schema);

    if let Err(error) = async {
        conn.execute(format!("CREATE SCHEMA {schema_ident}").as_str())
            .await?;
        conn.execute(format!("SET search_path TO {schema_ident}").as_str())
            .await?;
        MIGRATOR.run_direct(&mut conn).await?;
        Ok::<(), Box<dyn Error>>(())
    }
    .await
    {
        let _ = conn
            .execute(format!("DROP SCHEMA {schema_ident} CASCADE").as_str())
            .await;
        return Err(error);
    }

    Ok((conn, schema))
}

async fn drop_schema(conn: &mut PgConnection, schema_ident: &str) -> Result<(), Box<dyn Error>> {
    conn.execute(format!("DROP SCHEMA {schema_ident} CASCADE").as_str())
        .await?;
    Ok(())
}

fn test_database_url() -> Option<String> {
    env::var("PAY3_TEST_DATABASE_URL")
        .ok()
        .or_else(|| env::var("TEST_DATABASE_URL").ok())
}

fn temp_schema_name(prefix: &str) -> Result<String, Box<dyn Error>> {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    Ok(format!("{prefix}_{}_{}", process::id(), nanos))
}

fn quote_ident(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

fn address(byte: u8) -> String {
    format!("0x{:040x}", byte)
}

fn tx_hash(byte: u8) -> String {
    format!("0x{:064x}", byte)
}
