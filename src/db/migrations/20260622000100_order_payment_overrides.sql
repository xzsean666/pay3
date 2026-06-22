CREATE TABLE order_payment_overrides (
    order_id uuid PRIMARY KEY REFERENCES orders(id),
    accepted_problem_payment_raw numeric(78, 0) NOT NULL,
    accepted_by text NOT NULL,
    reason text,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CHECK (accepted_problem_payment_raw > 0),
    CHECK (accepted_by <> ''),
    CHECK (reason IS NULL OR reason <> '')
);

CREATE INDEX order_payment_overrides_updated_idx
    ON order_payment_overrides(updated_at);
