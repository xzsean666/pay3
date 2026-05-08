ALTER TABLE orders
    ADD COLUMN owner_sub text;

UPDATE orders
SET owner_sub = 'legacy'
WHERE owner_sub IS NULL;

ALTER TABLE orders
    ALTER COLUMN owner_sub SET NOT NULL,
    ADD CONSTRAINT orders_owner_sub_not_empty CHECK (owner_sub <> '');

ALTER TABLE orders
    DROP CONSTRAINT IF EXISTS orders_external_id_key;

CREATE UNIQUE INDEX orders_owner_external_id_idx
    ON orders(owner_sub, external_id);
