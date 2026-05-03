CREATE TABLE wallet_cursors (
    id text PRIMARY KEY DEFAULT 'default',
    signer_key_ref text NOT NULL,
    derivation_version integer NOT NULL DEFAULT 1,
    account_index bigint NOT NULL,
    change_index bigint NOT NULL,
    next_address_index bigint NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now(),
    CHECK (id <> ''),
    CHECK (signer_key_ref <> ''),
    CHECK (derivation_version > 0),
    CHECK (account_index BETWEEN 0 AND 2147483647),
    CHECK (change_index BETWEEN 0 AND 2147483647),
    CHECK (next_address_index BETWEEN 0 AND 2147483647)
);

INSERT INTO wallet_cursors (
    id,
    signer_key_ref,
    derivation_version,
    account_index,
    change_index,
    next_address_index
) VALUES (
    'default',
    'unconfigured',
    1,
    0,
    0,
    0
) ON CONFLICT (id) DO NOTHING;

CREATE TABLE child_accounts (
    id uuid PRIMARY KEY,
    signer_key_ref text NOT NULL,
    derivation_version integer NOT NULL DEFAULT 1,
    account_index bigint NOT NULL,
    change_index bigint NOT NULL,
    address_index bigint NOT NULL,
    derivation_path text NOT NULL,
    address text NOT NULL UNIQUE,
    last_used_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    CHECK (signer_key_ref <> ''),
    CHECK (derivation_version > 0),
    CHECK (account_index BETWEEN 0 AND 2147483647),
    CHECK (change_index BETWEEN 0 AND 2147483647),
    CHECK (address_index BETWEEN 0 AND 2147483647),
    CHECK (derivation_path ~ '^m/44''/60''/[0-9]+''/[0-9]+/[0-9]+$'),
    CHECK (address ~ '^0x[0-9a-f]{40}$'),
    UNIQUE (
        signer_key_ref,
        derivation_version,
        account_index,
        change_index,
        address_index
    ),
    UNIQUE (id, address)
);

CREATE TABLE orders (
    id uuid PRIMARY KEY,
    external_id text NOT NULL UNIQUE,
    request_hash text NOT NULL,
    child_account_id uuid NOT NULL REFERENCES child_accounts(id),
    receive_address text NOT NULL UNIQUE,
    chain_id bigint NOT NULL,
    token_address text NOT NULL,
    expected_amount_raw numeric(78, 0) NOT NULL,
    paid_amount_raw numeric(78, 0) NOT NULL DEFAULT 0,
    status text NOT NULL,
    expires_at timestamptz NOT NULL,
    monitor_until timestamptz NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CHECK (external_id <> ''),
    CHECK (request_hash <> ''),
    CHECK (receive_address ~ '^0x[0-9a-f]{40}$'),
    CHECK (chain_id >= 0),
    CHECK (token_address ~ '^0x[0-9a-f]{40}$'),
    CHECK (expected_amount_raw > 0),
    CHECK (paid_amount_raw >= 0),
    CHECK (monitor_until >= expires_at),
    CHECK (status IN ('pending', 'partial', 'confirming', 'paid', 'expired')),
    FOREIGN KEY (child_account_id, receive_address) REFERENCES child_accounts(id, address),
    UNIQUE (id, child_account_id, receive_address),
    UNIQUE (id, chain_id, token_address, child_account_id, receive_address)
);

CREATE INDEX orders_chain_token_address_idx
    ON orders(chain_id, token_address, receive_address);

CREATE TABLE payment_windows (
    id uuid PRIMARY KEY,
    order_id uuid NOT NULL UNIQUE REFERENCES orders(id),
    child_account_id uuid NOT NULL REFERENCES child_accounts(id),
    receive_address text NOT NULL,
    window_from timestamptz NOT NULL,
    window_from_block bigint NOT NULL,
    window_from_block_hash text NOT NULL,
    expires_at timestamptz NOT NULL,
    monitor_until timestamptz NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    CHECK (receive_address ~ '^0x[0-9a-f]{40}$'),
    CHECK (window_from_block >= 0),
    CHECK (window_from_block_hash ~ '^0x[0-9a-f]{64}$'),
    CHECK (expires_at > window_from),
    CHECK (monitor_until >= expires_at),
    FOREIGN KEY (child_account_id, receive_address) REFERENCES child_accounts(id, address),
    FOREIGN KEY (order_id, child_account_id, receive_address)
        REFERENCES orders(id, child_account_id, receive_address)
);

CREATE INDEX payment_windows_address_window_idx
    ON payment_windows(receive_address, window_from_block, expires_at, monitor_until);

CREATE TABLE payments (
    id uuid PRIMARY KEY,
    order_id uuid NOT NULL REFERENCES orders(id),
    child_account_id uuid NOT NULL REFERENCES child_accounts(id),
    chain_id bigint NOT NULL,
    token_address text NOT NULL,
    tx_hash text NOT NULL,
    log_index bigint NOT NULL,
    from_address text NOT NULL,
    to_address text NOT NULL,
    amount_raw numeric(78, 0) NOT NULL,
    block_number bigint NOT NULL,
    block_hash text NOT NULL,
    block_time timestamptz NOT NULL,
    confirmations bigint NOT NULL DEFAULT 0,
    match_status text NOT NULL,
    chain_status text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (chain_id, tx_hash, log_index),
    CHECK (chain_id >= 0),
    CHECK (token_address ~ '^0x[0-9a-f]{40}$'),
    CHECK (tx_hash ~ '^0x[0-9a-f]{64}$'),
    CHECK (log_index >= 0),
    CHECK (from_address ~ '^0x[0-9a-f]{40}$'),
    CHECK (to_address ~ '^0x[0-9a-f]{40}$'),
    CHECK (amount_raw > 0),
    CHECK (block_number >= 0),
    CHECK (block_hash ~ '^0x[0-9a-f]{64}$'),
    CHECK (confirmations >= 0),
    CHECK (match_status IN ('on_time', 'late', 'outside_window')),
    CHECK (chain_status IN ('observed', 'confirmed', 'orphaned')),
    FOREIGN KEY (order_id, chain_id, token_address, child_account_id, to_address)
        REFERENCES orders(id, chain_id, token_address, child_account_id, receive_address)
);

CREATE INDEX payments_order_idx ON payments(order_id);
CREATE INDEX payments_block_idx ON payments(chain_id, block_number);
CREATE INDEX payments_to_address_idx ON payments(to_address);

CREATE TABLE chain_cursors (
    chain_id bigint NOT NULL,
    token_address text NOT NULL,
    last_scanned_block bigint NOT NULL,
    seen_kv_reorg_epoch bigint NOT NULL DEFAULT 0,
    lease_owner text,
    lease_until timestamptz,
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (chain_id, token_address),
    CHECK (chain_id >= 0),
    CHECK (token_address ~ '^0x[0-9a-f]{40}$'),
    CHECK (last_scanned_block >= 0),
    CHECK (seen_kv_reorg_epoch >= 0),
    CHECK (
        (lease_owner IS NULL AND lease_until IS NULL)
        OR (lease_owner IS NOT NULL AND lease_until IS NOT NULL)
    )
);

CREATE TABLE treasury_addresses (
    chain_id bigint NOT NULL,
    token_address text NOT NULL,
    treasury_address text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (chain_id, token_address, treasury_address),
    CHECK (chain_id >= 0),
    CHECK (token_address ~ '^0x[0-9a-f]{40}$'),
    CHECK (treasury_address ~ '^0x[0-9a-f]{40}$')
);

CREATE TABLE collections (
    id uuid PRIMARY KEY,
    order_id uuid NOT NULL REFERENCES orders(id),
    idempotency_key text NOT NULL,
    request_hash text NOT NULL,
    child_account_id uuid NOT NULL REFERENCES child_accounts(id),
    chain_id bigint NOT NULL,
    token_address text NOT NULL,
    from_address text NOT NULL,
    to_address text NOT NULL,
    amount_raw numeric(78, 0),
    status text NOT NULL,
    outbound_tx_id uuid,
    attempt_count integer NOT NULL DEFAULT 0,
    locked_by text,
    locked_until timestamptz,
    error text,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CHECK (idempotency_key <> ''),
    CHECK (request_hash <> ''),
    CHECK (chain_id >= 0),
    CHECK (token_address ~ '^0x[0-9a-f]{40}$'),
    CHECK (from_address ~ '^0x[0-9a-f]{40}$'),
    CHECK (to_address ~ '^0x[0-9a-f]{40}$'),
    CHECK (amount_raw IS NULL OR amount_raw > 0),
    CHECK (attempt_count >= 0),
    CHECK (
        status IN (
            'queued',
            'transferring',
            'confirming',
            'confirmed',
            'failed',
            'dropped',
            'replacing',
            'replaced'
        )
    ),
    CHECK (
        (locked_by IS NULL AND locked_until IS NULL)
        OR (locked_by IS NOT NULL AND locked_until IS NOT NULL)
    ),
    FOREIGN KEY (child_account_id, from_address) REFERENCES child_accounts(id, address),
    FOREIGN KEY (order_id, chain_id, token_address, child_account_id, from_address)
        REFERENCES orders(id, chain_id, token_address, child_account_id, receive_address),
    FOREIGN KEY (chain_id, token_address, to_address)
        REFERENCES treasury_addresses(chain_id, token_address, treasury_address)
);

CREATE UNIQUE INDEX one_active_collection_per_child
    ON collections(child_account_id)
    WHERE status IN ('queued', 'transferring', 'confirming');

CREATE UNIQUE INDEX collections_idempotency_key_idx
    ON collections(idempotency_key);

CREATE INDEX collections_claim_idx
    ON collections(status, locked_until, created_at);

CREATE TABLE account_nonces (
    chain_id bigint NOT NULL,
    address text NOT NULL,
    next_nonce numeric(78, 0) NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (chain_id, address),
    CHECK (chain_id >= 0),
    CHECK (address ~ '^0x[0-9a-f]{40}$'),
    CHECK (next_nonce >= 0)
);

CREATE TABLE outbound_transactions (
    id uuid PRIMARY KEY,
    chain_id bigint NOT NULL,
    purpose text NOT NULL,
    from_address text NOT NULL,
    to_address text NOT NULL,
    nonce numeric(78, 0) NOT NULL,
    tx_hash text NOT NULL,
    signed_tx bytea NOT NULL,
    status text NOT NULL,
    replacement_of uuid REFERENCES outbound_transactions(id),
    replacement_reason text,
    broadcast_count integer NOT NULL DEFAULT 0,
    last_broadcast_at timestamptz,
    receipt_block_number bigint,
    receipt_block_hash text,
    error text,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (chain_id, tx_hash),
    CHECK (chain_id >= 0),
    CHECK (purpose IN ('collect')),
    CHECK (from_address ~ '^0x[0-9a-f]{40}$'),
    CHECK (to_address ~ '^0x[0-9a-f]{40}$'),
    CHECK (nonce >= 0),
    CHECK (tx_hash ~ '^0x[0-9a-f]{64}$'),
    CHECK (octet_length(signed_tx) > 0),
    CHECK (
        status IN (
            'signed',
            'broadcast',
            'confirmed',
            'failed',
            'dropped',
            'replaced'
        )
    ),
    CHECK (broadcast_count >= 0),
    CHECK (receipt_block_number IS NULL OR receipt_block_number >= 0),
    CHECK (receipt_block_hash IS NULL OR receipt_block_hash ~ '^0x[0-9a-f]{64}$')
);

CREATE UNIQUE INDEX outbound_active_nonce_idx
    ON outbound_transactions(chain_id, from_address, nonce)
    WHERE status IN ('signed', 'broadcast', 'confirmed');

CREATE UNIQUE INDEX outbound_collect_composite_idx
    ON outbound_transactions(id, purpose, chain_id, from_address, to_address);

ALTER TABLE collections
    ADD CONSTRAINT collections_outbound_tx_fk
    FOREIGN KEY (outbound_tx_id) REFERENCES outbound_transactions(id);

CREATE OR REPLACE FUNCTION enforce_collection_outbound_tx()
RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    outbound outbound_transactions%ROWTYPE;
BEGIN
    IF NEW.outbound_tx_id IS NULL THEN
        RETURN NEW;
    END IF;

    SELECT *
    INTO outbound
    FROM outbound_transactions
    WHERE id = NEW.outbound_tx_id;

    IF NOT FOUND THEN
        RAISE EXCEPTION 'collection outbound transaction % does not exist', NEW.outbound_tx_id
            USING ERRCODE = '23503';
    END IF;

    IF outbound.purpose <> 'collect'
        OR outbound.chain_id <> NEW.chain_id
        OR outbound.from_address <> NEW.from_address
        OR outbound.to_address <> NEW.to_address THEN
        RAISE EXCEPTION 'collection outbound transaction invariant violation'
            USING ERRCODE = '23514';
    END IF;

    RETURN NEW;
END;
$$;

CREATE TRIGGER collections_outbound_tx_invariant
    BEFORE INSERT OR UPDATE OF outbound_tx_id, chain_id, from_address, to_address
    ON collections
    FOR EACH ROW
    EXECUTE FUNCTION enforce_collection_outbound_tx();

CREATE TABLE audit_events (
    id uuid PRIMARY KEY,
    event_type text NOT NULL,
    request_id text,
    principal_sub text,
    scopes text,
    order_id uuid REFERENCES orders(id),
    collection_id uuid REFERENCES collections(id),
    tx_hash text,
    payload jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    CHECK (event_type <> ''),
    CHECK (tx_hash IS NULL OR tx_hash ~ '^0x[0-9a-f]{64}$')
);
