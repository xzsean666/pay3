CREATE INDEX orders_paid_auto_collection_idx
    ON orders(chain_id, token_address, updated_at, id)
    WHERE status = 'paid';

CREATE INDEX collections_order_idx
    ON collections(order_id);
