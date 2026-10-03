CREATE TABLE relayer_addresses (
    chain_id bigint NOT NULL,
    relayer_address text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (chain_id, relayer_address),
    CHECK (chain_id >= 0),
    CHECK (relayer_address ~ '^0x[0-9a-f]{40}$')
);

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
        OR (
            outbound.from_address <> NEW.from_address
            AND NOT EXISTS (
                SELECT 1
                FROM relayer_addresses r
                WHERE r.chain_id = NEW.chain_id
                  AND r.relayer_address = outbound.from_address
            )
        )
        OR outbound.to_address <> NEW.to_address THEN
        RAISE EXCEPTION 'collection outbound transaction invariant violation'
            USING ERRCODE = '23514';
    END IF;

    RETURN NEW;
END;
$$;
