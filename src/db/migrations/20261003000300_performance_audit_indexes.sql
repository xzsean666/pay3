-- Performance and concurrency audit indexes

-- 1. Accelerates order_expiry_loop:
-- SELECT id FROM orders WHERE status IN ('pending', 'partial') AND expires_at <= now() ORDER BY expires_at, id
CREATE INDEX IF NOT EXISTS idx_orders_pending_expires_at
    ON orders (expires_at, id)
    WHERE status IN ('pending', 'partial');

-- 2. Accelerates collections foreign key checks and outbound transaction status updates:
-- UPDATE collections SET status = 'confirming'/'confirmed' WHERE outbound_tx_id = $1
CREATE INDEX IF NOT EXISTS idx_collections_outbound_tx_id
    ON collections (outbound_tx_id)
    WHERE outbound_tx_id IS NOT NULL;

-- 3. Accelerates observed_payment_confirmation_candidates during confirmation sweeps:
-- SELECT id ... FROM payments WHERE chain_id = $1 AND token_address = $2 AND chain_status = 'observed' AND block_number <= $3 ORDER BY block_number, log_index, id
CREATE INDEX IF NOT EXISTS idx_payments_observed_candidates
    ON payments (chain_id, token_address, block_number, log_index, id)
    WHERE chain_status = 'observed';

-- 4. Accelerates multi-token worker collection job claiming:
CREATE INDEX IF NOT EXISTS idx_collections_multi_token_claim
    ON collections (chain_id, token_address, status, locked_until, created_at);
