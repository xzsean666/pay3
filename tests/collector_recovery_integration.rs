use std::{
    env,
    error::Error,
    process,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use bigdecimal::BigDecimal;
use pay3::{
    chain::{ChainBlock, ChainError, ChainHeaderReader, TransactionStatus, TxReceipt},
    db::{
        migrations::MIGRATOR,
        repositories::{
            BroadcastableOutboundTx, InsertSignedCollectTxResult, NewSignedOutboundTx,
            OutboundRepository, OutboundTxPurpose, OutboundTxRecord, OutboundTxStatus,
            ReceiptCheckableOutboundTx, RepositoryError, ReservedNonce,
        },
    },
    domain::{BlockHash, ChainBlockRef, EvmAddress, RawAmount, TxHash},
    services::collections::{CollectionServiceError, PrepareCollectionJobOutcome},
    workers::collector::{
        CollectionCollectorConfig, CollectionCollectorTickOutcome, CollectionCollectorWorker,
        CollectionJobPreparer, CollectionJobReplacer, SignedTxBroadcaster, TxReceiptReader,
    },
};
use sqlx::{Connection, Executor, PgConnection};
use time::OffsetDateTime;
use uuid::Uuid;

#[allow(dead_code)]
#[derive(Debug)]
struct TestCollectionFixture {
    chain_id: i64,
    token_address: String,
    treasury_address: String,
    child_address: String,
    order_id: Uuid,
    collection_id: Uuid,
}

#[tokio::test]
async fn crash_before_broadcast_recovery_in_postgres() -> Result<(), Box<dyn Error>> {
    let Some(database_url) = test_database_url() else {
        eprintln!(
            "skipping collector recovery DB integration test; set PAY3_TEST_DATABASE_URL or TEST_DATABASE_URL"
        );
        return Ok(());
    };

    let (mut conn, schema) =
        prepare_temp_schema(&database_url, "pay3_collector_recovery_crash").await?;
    let schema_ident = quote_ident(&schema);

    let result = async {
        let fixture = seed_collection_fixture(&mut conn).await?;
        let nonce = BigDecimal::from(42u32);
        let outbound_id = Uuid::new_v4();
        let original_signed_bytes = vec![0x02, 0xAA, 0xBB, 0xCC];
        let original_tx_hash = tx_hash(42);

        // 1. Simulate crash: outbound transaction is saved with status 'signed', but process was killed before broadcast
        insert_outbound(
            &mut conn,
            outbound_id,
            fixture.chain_id,
            &fixture.child_address,
            &fixture.treasury_address,
            nonce.clone(),
            &original_tx_hash,
            &original_signed_bytes,
            "signed",
            None,
        )
        .await?;

        // Attach outbound to collection
        sqlx::query(
            "UPDATE collections SET outbound_tx_id = $1, status = 'signing' WHERE id = $2",
        )
        .bind(outbound_id)
        .bind(fixture.collection_id)
        .execute(&mut conn)
        .await?;

        // 2. Restart recovery: verify query for recoverable signed tx finds this exact job
        let recoverable: Option<(Uuid, Uuid, Vec<u8>, String)> = sqlx::query_as(
            r#"
            SELECT c.id, o.id, o.signed_tx, o.tx_hash
            FROM outbound_transactions o
            JOIN collections c ON c.outbound_tx_id = o.id
            WHERE o.status = 'signed'
              AND o.chain_id = $1
            FOR UPDATE SKIP LOCKED
            LIMIT 1
            "#,
        )
        .bind(fixture.chain_id)
        .fetch_optional(&mut conn)
        .await?;

        assert!(recoverable.is_some(), "collector must recover signed tx");
        let (rec_col_id, rec_out_id, rec_signed_tx, rec_tx_hash) = recoverable.unwrap();
        assert_eq!(rec_col_id, fixture.collection_id);
        assert_eq!(rec_out_id, outbound_id);
        assert_eq!(rec_signed_tx, original_signed_bytes, "must replay exact signed bytes");
        assert_eq!(rec_tx_hash, original_tx_hash, "must retain exact tx hash");

        // 3. Mark broadcast after replaying
        sqlx::query(
            "UPDATE outbound_transactions SET status = 'broadcast', broadcast_at = now() WHERE id = $1",
        )
        .bind(outbound_id)
        .execute(&mut conn)
        .await?;

        sqlx::query("UPDATE collections SET status = 'broadcast' WHERE id = $1")
            .bind(fixture.collection_id)
            .execute(&mut conn)
            .await?;

        // Verify status is now broadcast
        let status: String =
            sqlx::query_scalar("SELECT status FROM outbound_transactions WHERE id = $1")
                .bind(outbound_id)
                .fetch_one(&mut conn)
                .await?;
        assert_eq!(status, "broadcast");

        Ok::<(), Box<dyn Error>>(())
    }
    .await;

    let cleanup_result = drop_schema(&mut conn, &schema_ident).await;
    result?;
    cleanup_result?;
    Ok(())
}

#[tokio::test]
async fn stuck_transaction_replacement_enforces_nonce_invariants_in_postgres(
) -> Result<(), Box<dyn Error>> {
    let Some(database_url) = test_database_url() else {
        return Ok(());
    };

    let (mut conn, schema) =
        prepare_temp_schema(&database_url, "pay3_collector_recovery_nonce").await?;
    let schema_ident = quote_ident(&schema);

    let result = async {
        let fixture = seed_collection_fixture(&mut conn).await?;
        let nonce = BigDecimal::from(99u32);
        let orig_id = Uuid::new_v4();

        // 1. Insert original active outbound
        insert_outbound(
            &mut conn,
            orig_id,
            fixture.chain_id,
            &fixture.child_address,
            &fixture.treasury_address,
            nonce.clone(),
            &tx_hash(91),
            &[1, 2, 3],
            "broadcast",
            None,
        )
        .await?;

        // 2. Attempt to insert duplicate active outbound with SAME nonce -> MUST FAIL (Partial Unique Index)
        let dup_attempt = insert_outbound(
            &mut conn,
            Uuid::new_v4(),
            fixture.chain_id,
            &fixture.child_address,
            &fixture.treasury_address,
            nonce.clone(),
            &tx_hash(92),
            &[4, 5, 6],
            "signed",
            None,
        )
        .await;

        assert!(
            dup_attempt.is_err(),
            "inserting duplicate active nonce must violate unique constraint"
        );

        // 3. Mark original as 'replaced'
        sqlx::query("UPDATE outbound_transactions SET status = 'replaced' WHERE id = $1")
            .bind(orig_id)
            .execute(&mut conn)
            .await?;

        // 4. Now inserting the replacement outbound with the SAME nonce succeeds!
        let repl_id = Uuid::new_v4();
        insert_outbound(
            &mut conn,
            repl_id,
            fixture.chain_id,
            &fixture.child_address,
            &fixture.treasury_address,
            nonce.clone(),
            &tx_hash(93),
            &[7, 8, 9],
            "signed",
            Some(orig_id),
        )
        .await?;

        let (orig_status, repl_status): (String, String) = sqlx::query_as(
            r#"
            SELECT
                (SELECT status FROM outbound_transactions WHERE id = $1),
                (SELECT status FROM outbound_transactions WHERE id = $2)
            "#,
        )
        .bind(orig_id)
        .bind(repl_id)
        .fetch_one(&mut conn)
        .await?;

        assert_eq!(orig_status, "replaced");
        assert_eq!(repl_status, "signed");

        Ok::<(), Box<dyn Error>>(())
    }
    .await;

    let cleanup_result = drop_schema(&mut conn, &schema_ident).await;
    result?;
    cleanup_result?;
    Ok(())
}

#[tokio::test]
async fn collector_worker_replays_signed_tx_on_startup() {
    let original_outbound = test_outbound_record(10, OutboundTxStatus::Signed);
    let signed_bytes = original_outbound.signed_tx.clone();
    let tx_hash = original_outbound.tx_hash;
    let col_id = Uuid::new_v4();

    let preparer = MockPreparer::default();
    let outbound_repo = MockOutboundRepo::with_recoverable(BroadcastableOutboundTx {
        collection_id: col_id,
        outbound: original_outbound.clone(),
    });
    let broadcaster = MockBroadcaster::returning(tx_hash);

    let worker = CollectionCollectorWorker::new(
        preparer.clone(),
        outbound_repo.clone(),
        broadcaster.clone(),
        CollectionCollectorConfig::new("test-collector-worker"),
    );

    // Tick must pick up the signed tx, broadcast it, and mark it broadcast
    let outcome = worker.tick().await.unwrap();

    match outcome {
        CollectionCollectorTickOutcome::Broadcast {
            collection_id,
            outbound,
        } => {
            assert_eq!(collection_id, col_id);
            assert_eq!(outbound.id, original_outbound.id);
            assert_eq!(outbound.status, OutboundTxStatus::Broadcast);
        }
        other => panic!("unexpected tick outcome: {other:?}"),
    }

    // Preparer should not have been called because recovery took precedence
    assert_eq!(preparer.calls(), 0);
    // Broadcaster broadcasted original signed tx bytes
    assert_eq!(broadcaster.broadcasted(), vec![signed_bytes]);
    // Outbound was marked broadcast
    assert_eq!(outbound_repo.marked_broadcast(), vec![original_outbound.id]);
}

#[tokio::test]
async fn collector_worker_confirms_receipt_after_confirmations_met() {
    let outbound = test_outbound_record(20, OutboundTxStatus::Broadcast);
    let col_id = Uuid::new_v4();
    let tx_hash = outbound.tx_hash;

    let receipt_block = ChainBlockRef {
        number: 100,
        hash: BlockHash::from_bytes([100u8; 32]),
    };

    let preparer = MockPreparer::default();
    let outbound_repo = MockOutboundRepo::with_receipt_checkable(ReceiptCheckableOutboundTx {
        collection_id: col_id,
        outbound: outbound.clone(),
    });
    let broadcaster = MockBroadcaster::with_receipt(
        tx_hash,
        TxReceipt {
            tx_hash,
            block: receipt_block,
            status: TransactionStatus::Success,
            gas_used: Some(21_000),
        },
        105, // canonical head is 105 (5 confirmations > 0 required)
    );

    let worker = CollectionCollectorWorker::new(
        preparer,
        outbound_repo.clone(),
        broadcaster,
        CollectionCollectorConfig::new("test-collector-worker"),
    );

    let outcome = worker.tick().await.unwrap();

    match outcome {
        CollectionCollectorTickOutcome::Confirmed {
            collection_id,
            outbound: confirmed_outbound,
        } => {
            assert_eq!(collection_id, col_id);
            assert_eq!(confirmed_outbound.status, OutboundTxStatus::Confirmed);
        }
        other => panic!("expected Confirmed tick outcome, got: {other:?}"),
    }

    assert_eq!(outbound_repo.marked_confirmed(), vec![outbound.id]);
}

// ---------------- Helper Mock Types ----------------

#[derive(Clone, Default)]
struct MockPreparer {
    calls: Arc<Mutex<usize>>,
}

impl MockPreparer {
    fn calls(&self) -> usize {
        *self.calls.lock().unwrap()
    }
}

#[async_trait::async_trait]
impl CollectionJobPreparer for MockPreparer {
    async fn prepare_next_collection_job(
        &self,
        _worker_id: &str,
    ) -> Result<PrepareCollectionJobOutcome, CollectionServiceError> {
        let mut count = self.calls.lock().unwrap();
        *count += 1;
        Ok(PrepareCollectionJobOutcome::NoJob)
    }
}

#[async_trait::async_trait]
impl CollectionJobReplacer for MockPreparer {
    async fn replace_stuck_collection_job(
        &self,
        _worker_id: &str,
        _job: ReceiptCheckableOutboundTx,
        _replacement_reason: &str,
    ) -> Result<PrepareCollectionJobOutcome, CollectionServiceError> {
        Ok(PrepareCollectionJobOutcome::NoJob)
    }
}

#[derive(Clone, Default)]
struct MockOutboundRepo {
    recoverable: Arc<Mutex<Option<BroadcastableOutboundTx>>>,
    receipt_checkable: Arc<Mutex<Option<ReceiptCheckableOutboundTx>>>,
    marked_broadcast: Arc<Mutex<Vec<Uuid>>>,
    marked_confirmed: Arc<Mutex<Vec<Uuid>>>,
}

impl MockOutboundRepo {
    fn with_recoverable(tx: BroadcastableOutboundTx) -> Self {
        Self {
            recoverable: Arc::new(Mutex::new(Some(tx))),
            ..Default::default()
        }
    }

    fn with_receipt_checkable(tx: ReceiptCheckableOutboundTx) -> Self {
        Self {
            receipt_checkable: Arc::new(Mutex::new(Some(tx))),
            ..Default::default()
        }
    }

    fn marked_broadcast(&self) -> Vec<Uuid> {
        self.marked_broadcast.lock().unwrap().clone()
    }

    fn marked_confirmed(&self) -> Vec<Uuid> {
        self.marked_confirmed.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl OutboundRepository for MockOutboundRepo {
    async fn reserve_nonce(
        &self,
        _chain_id: u64,
        _from_address: EvmAddress,
        _pending_nonce: RawAmount,
    ) -> Result<ReservedNonce, RepositoryError> {
        unimplemented!()
    }

    async fn insert_signed_tx(
        &self,
        _tx: NewSignedOutboundTx,
    ) -> Result<OutboundTxRecord, RepositoryError> {
        unimplemented!()
    }

    async fn insert_signed_collect_tx(
        &self,
        _collection_id: Uuid,
        _tx: NewSignedOutboundTx,
        _resolved_amount_raw: RawAmount,
    ) -> Result<InsertSignedCollectTxResult, RepositoryError> {
        unimplemented!()
    }

    async fn replace_signed_tx(
        &self,
        _old_tx_id: Uuid,
        _replacement_tx: NewSignedOutboundTx,
    ) -> Result<OutboundTxRecord, RepositoryError> {
        unimplemented!()
    }

    async fn claim_signed_collect_tx_for_broadcast(
        &self,
        _worker_id: &str,
    ) -> Result<Option<BroadcastableOutboundTx>, RepositoryError> {
        let mut guard = self.recoverable.lock().unwrap();
        Ok(guard.take())
    }

    async fn claim_broadcast_collect_tx_for_receipt(
        &self,
        _worker_id: &str,
    ) -> Result<Option<ReceiptCheckableOutboundTx>, RepositoryError> {
        let mut guard = self.receipt_checkable.lock().unwrap();
        Ok(guard.take())
    }

    async fn mark_broadcast(&self, tx_id: Uuid) -> Result<OutboundTxRecord, RepositoryError> {
        self.marked_broadcast.lock().unwrap().push(tx_id);
        let mut rec = test_outbound_record(1, OutboundTxStatus::Broadcast);
        rec.id = tx_id;
        Ok(rec)
    }

    async fn mark_confirmed(
        &self,
        tx_id: Uuid,
        receipt_block: ChainBlockRef,
    ) -> Result<OutboundTxRecord, RepositoryError> {
        self.marked_confirmed.lock().unwrap().push(tx_id);
        let mut rec = test_outbound_record(1, OutboundTxStatus::Confirmed);
        rec.id = tx_id;
        rec.receipt_block = Some(receipt_block);
        Ok(rec)
    }

    async fn mark_failed(
        &self,
        tx_id: Uuid,
        error: &str,
    ) -> Result<OutboundTxRecord, RepositoryError> {
        let mut rec = test_outbound_record(1, OutboundTxStatus::Failed);
        rec.id = tx_id;
        rec.error = Some(error.to_string());
        Ok(rec)
    }
}

#[derive(Clone)]
struct MockBroadcaster {
    expected_hash: TxHash,
    broadcasted: Arc<Mutex<Vec<Vec<u8>>>>,
    receipt: Option<TxReceipt>,
    head_number: u64,
}

impl MockBroadcaster {
    fn returning(tx_hash: TxHash) -> Self {
        Self {
            expected_hash: tx_hash,
            broadcasted: Arc::new(Mutex::new(Vec::new())),
            receipt: None,
            head_number: 100,
        }
    }

    fn with_receipt(tx_hash: TxHash, receipt: TxReceipt, head_number: u64) -> Self {
        Self {
            expected_hash: tx_hash,
            broadcasted: Arc::new(Mutex::new(Vec::new())),
            receipt: Some(receipt),
            head_number,
        }
    }

    fn broadcasted(&self) -> Vec<Vec<u8>> {
        self.broadcasted.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl SignedTxBroadcaster for MockBroadcaster {
    async fn broadcast_signed_tx(&self, signed_tx: Vec<u8>) -> Result<TxHash, ChainError> {
        self.broadcasted.lock().unwrap().push(signed_tx);
        Ok(self.expected_hash)
    }
}

#[async_trait::async_trait]
impl TxReceiptReader for MockBroadcaster {
    async fn transaction_receipt(&self, _tx_hash: TxHash) -> Result<Option<TxReceipt>, ChainError> {
        Ok(self.receipt.clone())
    }
}

#[async_trait::async_trait]
impl ChainHeaderReader for MockBroadcaster {
    async fn latest_head(&self) -> Result<ChainBlockRef, ChainError> {
        Ok(ChainBlockRef {
            number: self.head_number,
            hash: BlockHash::from_bytes([0x88; 32]),
        })
    }

    async fn safe_head(&self) -> Result<ChainBlockRef, ChainError> {
        self.latest_head().await
    }

    async fn finalized_head(&self) -> Result<ChainBlockRef, ChainError> {
        self.latest_head().await
    }

    async fn block_by_number(&self, number: u64) -> Result<ChainBlock, ChainError> {
        Ok(ChainBlock::new(
            number,
            BlockHash::from_bytes([number as u8; 32]),
            BlockHash::from_bytes([number.saturating_sub(1) as u8; 32]),
            OffsetDateTime::now_utc(),
        ))
    }
}

fn test_outbound_record(nonce: u64, status: OutboundTxStatus) -> OutboundTxRecord {
    let now = OffsetDateTime::now_utc();
    OutboundTxRecord {
        id: Uuid::new_v4(),
        chain_id: 1,
        purpose: OutboundTxPurpose::Collect,
        from_address: EvmAddress::from_bytes([0x11; 20]),
        to_address: EvmAddress::from_bytes([0x22; 20]),
        nonce: RawAmount::from(nonce),
        gas_limit: 80_000,
        max_fee_per_gas: RawAmount::from(30_000_000_000u64),
        max_priority_fee_per_gas: RawAmount::from(2_000_000_000u64),
        tx_hash: tx_hash_from_byte(nonce as u8),
        signed_tx: vec![0x02, nonce as u8, 0xAA],
        status,
        replacement_of: None,
        replacement_reason: None,
        broadcast_count: u32::from(matches!(status, OutboundTxStatus::Broadcast)),
        last_broadcast_at: matches!(status, OutboundTxStatus::Broadcast).then_some(now),
        receipt_block: None,
        error: None,
        created_at: now,
        updated_at: now,
    }
}

fn tx_hash_from_byte(b: u8) -> TxHash {
    let mut bytes = [0u8; 32];
    bytes[0] = b;
    TxHash::from_bytes(bytes)
}

// ---------------- Database Fixture Helpers ----------------

async fn seed_collection_fixture(
    conn: &mut PgConnection,
) -> Result<TestCollectionFixture, Box<dyn Error>> {
    let chain_id = 1i64;
    let token_address = address(1);
    let treasury_address = address(2);
    let child_address = address(3);
    let order_id = Uuid::new_v4();
    let collection_id = Uuid::new_v4();

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

    let now = OffsetDateTime::now_utc();
    let expires = now + time::Duration::hours(2);

    sqlx::query(
        r#"
        INSERT INTO orders (
            id, owner_sub, external_id, chain_id, token_address,
            receive_address, expected_amount_raw, status, expires_at, created_at, updated_at
        )
        VALUES ($1, 'test-owner', $2, $3, $4, $5, 100, 'paid', $6, $7, $7)
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

    sqlx::query(
        r#"
        INSERT INTO collections (
            id, owner_sub, idempotency_key, request_hash, chain_id, token_address,
            from_address, to_address, amount_raw, order_id, order_requirement, status,
            created_at, updated_at
        )
        VALUES (
            $1, 'test-owner', $2, $3, $4, $5,
            $6, $7, 100, $8, 'paid', 'queued',
            $9, $9
        )
        "#,
    )
    .bind(collection_id)
    .bind(format!("idem-{}", collection_id))
    .bind(format!("req-{}", collection_id))
    .bind(chain_id)
    .bind(&token_address)
    .bind(&child_address)
    .bind(&treasury_address)
    .bind(order_id)
    .bind(now)
    .execute(&mut *conn)
    .await?;

    Ok(TestCollectionFixture {
        chain_id,
        token_address,
        treasury_address,
        child_address,
        order_id,
        collection_id,
    })
}

async fn insert_outbound(
    conn: &mut PgConnection,
    id: Uuid,
    chain_id: i64,
    from: &str,
    to: &str,
    nonce: BigDecimal,
    tx_hash: &str,
    signed_tx: &[u8],
    status: &str,
    replacement_of: Option<Uuid>,
) -> Result<(), Box<dyn Error>> {
    sqlx::query(
        r#"
        INSERT INTO outbound_transactions (
            id, chain_id, purpose, from_address, to_address, nonce,
            tx_hash, signed_tx, status, replacement_of, created_at, updated_at
        )
        VALUES ($1, $2, 'collect', $3, $4, $5, $6, $7, $8, $9, now(), now())
        "#,
    )
    .bind(id)
    .bind(chain_id)
    .bind(from)
    .bind(to)
    .bind(nonce)
    .bind(tx_hash)
    .bind(signed_tx)
    .bind(status)
    .bind(replacement_of)
    .execute(conn)
    .await?;
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
