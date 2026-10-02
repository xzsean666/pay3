# TASK-004: PostgreSQL 数据库迁移与强约束 Schema

## Objective
编写完整的 PostgreSQL 初始迁移脚本与 Migrator，通过数据库级唯一索引、外键约束、CHECK 校验及 Trigger 保证资金状态不变量。

## Scope
- 核心表结构：`wallet_cursors`、`child_accounts`、`orders`、`payment_windows`、`payments`、`chain_cursors`、`collections`、`account_nonces`、`outbound_transactions`、`treasury_addresses`、`audit_events`。
- 数据库级约束：`orders.receive_address` 全局唯一、地址/哈希格式正则 CHECK、Outbound active nonce partial unique index、Treasury 外键关联。

## Allowed Files
- `src/db/migrations/**`
- `src/db/migrations.rs`
- `src/db/mod.rs`
- `tests/migration_contract.rs`

## Dependencies
- TASK-001
- TASK-002

## Inputs and Outputs
- **Inputs**: PostgreSQL 数据库连接。
- **Outputs**: 结构完整、带强约束的数据表及初始游标/配置。

## Acceptance Criteria
- Migration 可幂等重复执行。
- 直接通过 SQL 插入非法地址格式或非 Treasury 归集时必须报错。
- `tests/migration_contract.rs` 静态检查全部通过。

## Verification Commands
```bash
cargo test --test migration_contract
```

## Risks and Assumptions
- 依赖 PostgreSQL 14+ 版本。

## Status
DONE
