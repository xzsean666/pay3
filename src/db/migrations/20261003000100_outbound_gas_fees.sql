ALTER TABLE outbound_transactions
    ADD COLUMN gas_limit bigint NOT NULL DEFAULT 80000,
    ADD COLUMN max_fee_per_gas numeric(78, 0) NOT NULL DEFAULT 0,
    ADD COLUMN max_priority_fee_per_gas numeric(78, 0) NOT NULL DEFAULT 0;

ALTER TABLE outbound_transactions
    ADD CONSTRAINT outbound_gas_limit_positive CHECK (gas_limit > 0),
    ADD CONSTRAINT outbound_max_fee_per_gas_non_negative CHECK (max_fee_per_gas >= 0),
    ADD CONSTRAINT outbound_max_priority_fee_per_gas_non_negative CHECK (max_priority_fee_per_gas >= 0),
    ADD CONSTRAINT outbound_priority_fee_le_max_fee CHECK (max_priority_fee_per_gas <= max_fee_per_gas);
