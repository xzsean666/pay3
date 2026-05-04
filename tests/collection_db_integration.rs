use bigdecimal::BigDecimal;
use pay3::db::migrations::MIGRATOR;
use sqlx::{Connection, Executor, PgConnection};
use std::{
    env,
    error::Error,
    process,
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

const CHAIN_ID: i64 = 1;

#[derive(Debug, Clone)]
struct CollectionFixture {
    chain_id: i64,
    token_address: String,
    child_account_id: Uuid,
    child_address: String,
    treasury_address: String,
    order_id: Uuid,
}

#[tokio::test]
async fn collections_outbound_invariants_are_enforced_in_postgres() -> Result<(), Box<dyn Error>> {
    let Some(database_url) = test_database_url() else {
        eprintln!(
            "skipping collection DB integration test; set PAY3_TEST_DATABASE_URL or TEST_DATABASE_URL"
        );
        return Ok(());
    };

    let (mut conn, schema) =
        prepare_temp_schema(&database_url, "pay3_collection_db_trigger").await?;
    let schema_ident = quote_ident(&schema);

    let result = async {
        let fixture = seed_collection_fixture(&mut conn).await?;
        let amount = BigDecimal::from(10u32);

        let valid_outbound_id = Uuid::new_v4();
        let valid_outbound = insert_outbound(
            &mut conn,
            NewOutbound {
                id: valid_outbound_id,
                chain_id: fixture.chain_id,
                purpose: "collect",
                from_address: &fixture.child_address,
                to_address: &fixture.treasury_address,
                nonce: BigDecimal::from(1u32),
                tx_hash: &tx_hash(1),
                signed_tx: vec![1, 2, 3],
                status: "signed",
                replacement_of: None,
                replacement_reason: None,
            },
        )
        .await?;
        assert_eq!(valid_outbound.rows_affected(), 1);

        let valid_collection = insert_collection(
            &mut conn,
            NewCollection {
                id: Uuid::new_v4(),
                idempotency_key: "collection-valid",
                request_hash: "collection-valid-request",
                amount_raw: &amount,
                status: "confirmed",
                outbound_tx_id: valid_outbound_id,
            },
            &fixture,
        )
        .await?;
        assert_eq!(valid_collection.rows_affected(), 1);

        let chain_mismatch_outbound_id = Uuid::new_v4();
        let chain_mismatch_outbound = insert_outbound(
            &mut conn,
            NewOutbound {
                id: chain_mismatch_outbound_id,
                chain_id: fixture.chain_id + 1,
                purpose: "collect",
                from_address: &fixture.child_address,
                to_address: &fixture.treasury_address,
                nonce: BigDecimal::from(2u32),
                tx_hash: &tx_hash(2),
                signed_tx: vec![4, 5, 6],
                status: "signed",
                replacement_of: None,
                replacement_reason: None,
            },
        )
        .await?;
        assert_eq!(chain_mismatch_outbound.rows_affected(), 1);

        assert_db_error(
            insert_collection(
                &mut conn,
                NewCollection {
                    id: Uuid::new_v4(),
                    idempotency_key: "collection-chain-mismatch",
                    request_hash: "collection-chain-mismatch-request",
                    amount_raw: &amount,
                    status: "confirmed",
                    outbound_tx_id: chain_mismatch_outbound_id,
                },
                &fixture,
            )
            .await,
            "23514",
            "collection outbound transaction invariant violation",
            "chain mismatch",
        )?;

        let from_mismatch_outbound_id = Uuid::new_v4();
        let from_mismatch_outbound = insert_outbound(
            &mut conn,
            NewOutbound {
                id: from_mismatch_outbound_id,
                chain_id: fixture.chain_id,
                purpose: "collect",
                from_address: &address(0x44),
                to_address: &fixture.treasury_address,
                nonce: BigDecimal::from(3u32),
                tx_hash: &tx_hash(3),
                signed_tx: vec![7, 8, 9],
                status: "signed",
                replacement_of: None,
                replacement_reason: None,
            },
        )
        .await?;
        assert_eq!(from_mismatch_outbound.rows_affected(), 1);

        assert_db_error(
            insert_collection(
                &mut conn,
                NewCollection {
                    id: Uuid::new_v4(),
                    idempotency_key: "collection-from-mismatch",
                    request_hash: "collection-from-mismatch-request",
                    amount_raw: &amount,
                    status: "confirmed",
                    outbound_tx_id: from_mismatch_outbound_id,
                },
                &fixture,
            )
            .await,
            "23514",
            "collection outbound transaction invariant violation",
            "from mismatch",
        )?;

        let to_mismatch_outbound_id = Uuid::new_v4();
        let to_mismatch_outbound = insert_outbound(
            &mut conn,
            NewOutbound {
                id: to_mismatch_outbound_id,
                chain_id: fixture.chain_id,
                purpose: "collect",
                from_address: &fixture.child_address,
                to_address: &address(0x55),
                nonce: BigDecimal::from(4u32),
                tx_hash: &tx_hash(4),
                signed_tx: vec![10, 11, 12],
                status: "signed",
                replacement_of: None,
                replacement_reason: None,
            },
        )
        .await?;
        assert_eq!(to_mismatch_outbound.rows_affected(), 1);

        assert_db_error(
            insert_collection(
                &mut conn,
                NewCollection {
                    id: Uuid::new_v4(),
                    idempotency_key: "collection-to-mismatch",
                    request_hash: "collection-to-mismatch-request",
                    amount_raw: &amount,
                    status: "confirmed",
                    outbound_tx_id: to_mismatch_outbound_id,
                },
                &fixture,
            )
            .await,
            "23514",
            "collection outbound transaction invariant violation",
            "to mismatch",
        )?;

        // The schema keeps outbound purpose collect-only with a CHECK, so purpose mismatch
        // is enforced one layer earlier than the collection trigger.
        assert_db_error(
            insert_outbound(
                &mut conn,
                NewOutbound {
                    id: Uuid::new_v4(),
                    chain_id: fixture.chain_id,
                    purpose: "transfer",
                    from_address: &fixture.child_address,
                    to_address: &fixture.treasury_address,
                    nonce: BigDecimal::from(5u32),
                    tx_hash: &tx_hash(5),
                    signed_tx: vec![13, 14, 15],
                    status: "signed",
                    replacement_of: None,
                    replacement_reason: None,
                },
            )
            .await,
            "23514",
            "violates check constraint",
            "collect-only purpose check",
        )?;

        Ok::<(), Box<dyn Error>>(())
    }
    .await;

    let cleanup_result = drop_schema(&mut conn, &schema_ident).await;

    match result {
        Ok(()) => {
            cleanup_result?;
            Ok(())
        }
        Err(err) => {
            if let Err(cleanup_err) = cleanup_result {
                eprintln!("failed to drop temp schema {schema}: {cleanup_err}");
            }
            Err(err)
        }
    }
}

#[tokio::test]
async fn outbound_active_nonce_index_blocks_duplicate_active_nonce_and_allows_replacement()
-> Result<(), Box<dyn Error>> {
    let Some(database_url) = test_database_url() else {
        eprintln!(
            "skipping collection DB integration test; set PAY3_TEST_DATABASE_URL or TEST_DATABASE_URL"
        );
        return Ok(());
    };

    let (mut conn, schema) = prepare_temp_schema(&database_url, "pay3_collection_db_nonce").await?;
    let schema_ident = quote_ident(&schema);

    let result = async {
        let fixture = seed_collection_fixture(&mut conn).await?;
        let nonce = BigDecimal::from(7u32);

        let original_id = Uuid::new_v4();
        let original = insert_outbound(
            &mut conn,
            NewOutbound {
                id: original_id,
                chain_id: fixture.chain_id,
                purpose: "collect",
                from_address: &fixture.child_address,
                to_address: &fixture.treasury_address,
                nonce: nonce.clone(),
                tx_hash: &tx_hash(11),
                signed_tx: vec![1, 1, 1],
                status: "signed",
                replacement_of: None,
                replacement_reason: None,
            },
        )
        .await?;
        assert_eq!(original.rows_affected(), 1);

        assert_db_error(
            insert_outbound(
                &mut conn,
                NewOutbound {
                    id: Uuid::new_v4(),
                    chain_id: fixture.chain_id,
                    purpose: "collect",
                    from_address: &fixture.child_address,
                    to_address: &fixture.treasury_address,
                    nonce: nonce.clone(),
                    tx_hash: &tx_hash(12),
                    signed_tx: vec![2, 2, 2],
                    status: "signed",
                    replacement_of: None,
                    replacement_reason: None,
                },
            )
            .await,
            "23505",
            "outbound_active_nonce_idx",
            "duplicate active outbound nonce",
        )?;

        let replaced = sqlx::query(
            r#"
            UPDATE outbound_transactions
            SET status = 'replaced',
                updated_at = now()
            WHERE id = $1
            "#,
        )
        .bind(original_id)
        .execute(&mut conn)
        .await?;
        assert_eq!(replaced.rows_affected(), 1);

        let replacement_id = Uuid::new_v4();
        let replacement = insert_outbound(
            &mut conn,
            NewOutbound {
                id: replacement_id,
                chain_id: fixture.chain_id,
                purpose: "collect",
                from_address: &fixture.child_address,
                to_address: &fixture.treasury_address,
                nonce: nonce.clone(),
                tx_hash: &tx_hash(13),
                signed_tx: vec![3, 3, 3],
                status: "signed",
                replacement_of: Some(original_id),
                replacement_reason: Some("gas bump"),
            },
        )
        .await?;
        assert_eq!(replacement.rows_affected(), 1);

        let row_count: i64 = sqlx::query_scalar(
            r#"
            SELECT count(*)::bigint
            FROM outbound_transactions
            WHERE chain_id = $1
              AND from_address = $2
              AND nonce = $3
            "#,
        )
        .bind(fixture.chain_id)
        .bind(&fixture.child_address)
        .bind(nonce)
        .fetch_one(&mut conn)
        .await?;
        assert_eq!(row_count, 2);

        Ok::<(), Box<dyn Error>>(())
    }
    .await;

    let cleanup_result = drop_schema(&mut conn, &schema_ident).await;

    match result {
        Ok(()) => {
            cleanup_result?;
            Ok(())
        }
        Err(err) => {
            if let Err(cleanup_err) = cleanup_result {
                eprintln!("failed to drop temp schema {schema}: {cleanup_err}");
            }
            Err(err)
        }
    }
}

async fn seed_collection_fixture(
    conn: &mut PgConnection,
) -> Result<CollectionFixture, Box<dyn Error>> {
    let fixture = CollectionFixture {
        chain_id: CHAIN_ID,
        token_address: address(0x66),
        child_account_id: Uuid::new_v4(),
        child_address: address(0x22),
        treasury_address: address(0x33),
        order_id: Uuid::new_v4(),
    };

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
        ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
        "#,
    )
    .bind(fixture.child_account_id)
    .bind("fixture-signer")
    .bind(1i32)
    .bind(0i64)
    .bind(0i64)
    .bind(0i64)
    .bind("m/44'/60'/0'/0/0")
    .bind(&fixture.child_address)
    .execute(&mut *conn)
    .await?;

    sqlx::query(
        r#"
        INSERT INTO treasury_addresses (
            chain_id,
            token_address,
            treasury_address
        ) VALUES ($1, $2, $3)
        "#,
    )
    .bind(fixture.chain_id)
    .bind(&fixture.token_address)
    .bind(&fixture.treasury_address)
    .execute(&mut *conn)
    .await?;

    sqlx::query(
        r#"
        INSERT INTO orders (
            id,
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
        ) VALUES (
            $1,
            $2,
            $3,
            $4,
            $5,
            $6,
            $7,
            $8,
            $9,
            'paid',
            now() + interval '1 hour',
            now() + interval '2 hours'
        )
        "#,
    )
    .bind(fixture.order_id)
    .bind(format!("external-{}", fixture.order_id))
    .bind(format!("request-{}", fixture.order_id))
    .bind(fixture.child_account_id)
    .bind(&fixture.child_address)
    .bind(fixture.chain_id)
    .bind(&fixture.token_address)
    .bind(BigDecimal::from(10u32))
    .bind(BigDecimal::from(10u32))
    .execute(&mut *conn)
    .await?;

    Ok(fixture)
}

struct NewCollection<'a> {
    id: Uuid,
    idempotency_key: &'a str,
    request_hash: &'a str,
    amount_raw: &'a BigDecimal,
    status: &'a str,
    outbound_tx_id: Uuid,
}

async fn insert_collection(
    conn: &mut PgConnection,
    collection: NewCollection<'_>,
    fixture: &CollectionFixture,
) -> Result<sqlx::postgres::PgQueryResult, sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO collections (
            id,
            order_id,
            idempotency_key,
            request_hash,
            child_account_id,
            chain_id,
            token_address,
            from_address,
            to_address,
            amount_raw,
            status,
            outbound_tx_id
        ) VALUES (
            $1,
            $2,
            $3,
            $4,
            $5,
            $6,
            $7,
            $8,
            $9,
            $10,
            $11,
            $12
        )
        "#,
    )
    .bind(collection.id)
    .bind(fixture.order_id)
    .bind(collection.idempotency_key)
    .bind(collection.request_hash)
    .bind(fixture.child_account_id)
    .bind(fixture.chain_id)
    .bind(&fixture.token_address)
    .bind(&fixture.child_address)
    .bind(&fixture.treasury_address)
    .bind(collection.amount_raw)
    .bind(collection.status)
    .bind(collection.outbound_tx_id)
    .execute(conn)
    .await
}

struct NewOutbound<'a> {
    id: Uuid,
    chain_id: i64,
    purpose: &'a str,
    from_address: &'a str,
    to_address: &'a str,
    nonce: BigDecimal,
    tx_hash: &'a str,
    signed_tx: Vec<u8>,
    status: &'a str,
    replacement_of: Option<Uuid>,
    replacement_reason: Option<&'a str>,
}

async fn insert_outbound(
    conn: &mut PgConnection,
    outbound: NewOutbound<'_>,
) -> Result<sqlx::postgres::PgQueryResult, sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO outbound_transactions (
            id,
            chain_id,
            purpose,
            from_address,
            to_address,
            nonce,
            tx_hash,
            signed_tx,
            status,
            replacement_of,
            replacement_reason
        ) VALUES (
            $1,
            $2,
            $3,
            $4,
            $5,
            $6,
            $7,
            $8,
            $9,
            $10,
            $11
        )
        "#,
    )
    .bind(outbound.id)
    .bind(outbound.chain_id)
    .bind(outbound.purpose)
    .bind(outbound.from_address)
    .bind(outbound.to_address)
    .bind(outbound.nonce)
    .bind(outbound.tx_hash)
    .bind(outbound.signed_tx)
    .bind(outbound.status)
    .bind(outbound.replacement_of)
    .bind(outbound.replacement_reason)
    .execute(conn)
    .await
}

fn assert_db_error<T>(
    result: Result<T, sqlx::Error>,
    expected_code: &str,
    expected_message_fragment: &str,
    context: &str,
) -> Result<(), Box<dyn Error>> {
    let error = match result {
        Ok(_) => return Err(format!("{context}: expected a database error").into()),
        Err(error) => error,
    };

    let db_error = match error {
        sqlx::Error::Database(db_error) => db_error,
        other => {
            return Err(format!("{context}: expected database error, got {other:?}").into());
        }
    };

    let code = db_error.code();
    if code.as_deref() != Some(expected_code) {
        return Err(format!("{context}: expected sqlstate {expected_code}, got {code:?}").into());
    }

    let message = db_error.message();
    if !message.contains(expected_message_fragment) {
        return Err(
            format!(
                "{context}: expected error message to contain {expected_message_fragment:?}, got {message:?}"
            )
            .into(),
        );
    }

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
