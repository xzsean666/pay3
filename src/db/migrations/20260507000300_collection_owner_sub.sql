ALTER TABLE collections
    ADD COLUMN owner_sub text;

UPDATE collections c
SET owner_sub = o.owner_sub
FROM orders o
WHERE c.order_id = o.id
  AND c.owner_sub IS NULL;

ALTER TABLE collections
    ALTER COLUMN owner_sub SET NOT NULL,
    ADD CONSTRAINT collections_owner_sub_not_empty CHECK (owner_sub <> '');

CREATE UNIQUE INDEX orders_id_owner_sub_idx
    ON orders(id, owner_sub);

ALTER TABLE collections
    ADD CONSTRAINT collections_order_owner_sub_fk
    FOREIGN KEY (order_id, owner_sub) REFERENCES orders(id, owner_sub);

DROP INDEX IF EXISTS collections_idempotency_key_idx;

CREATE UNIQUE INDEX collections_owner_idempotency_key_idx
    ON collections(owner_sub, idempotency_key);

CREATE INDEX collections_owner_idx
    ON collections(owner_sub);
