# TASK-026: 移除 redb 依赖，改造为 Direct 无状态扫描器

## Objective
对扫链与存储架构进行瘦身，完全废弃本地 `redb` 嵌入式存储与 `transfer_log_store`，将扫链 Worker 重构为 Direct-to-Postgres 极简无状态扫描器，让应用容器实现 100% 无状态、轻量级部署。

## Scope
- 废弃 `src/transfer_log_store/` 中的本地 KV 文件读写逻辑。
- 重构 `workers/scanner.rs`：
  - 直接调用 `RpcRangeSource::logs_page` 或批量 `eth_getLogs` 拉取区块日志。
  - 内存中快速比对平台活跃收款地址集合（`PaymentWindowLookup`）。
  - 命中的交易单事务内原子写入 PostgreSQL `payments`，更新订单状态，并推进 `chain_cursors.last_scanned_block`。
  - 未命中的非平台交易在内存中直接 GC 丢弃，不再落盘。
- 移除运行时与生命周期中冗余的 redb 文件初始化、保留清理循环（Retention cleanup loop）与 KV Reorg Epoch 联动。
- 更新集成测试与 Anvil E2E 测试。

## Allowed Files
- `src/workers/scanner.rs`
- `src/runtime.rs`
- `src/config.rs`
- `src/transfer_log_store/**`
- `src/services/verify/**`
- `tests/**`

## Dependencies
- TASK-025

## Inputs and Outputs
- **Inputs**: 链上 RPC 事件流与 PostgreSQL 数据库。
- **Outputs**: 纯无状态的高性能扫链支付推进，无任何本地磁盘 IO 负担。

## Acceptance Criteria
- 编译通过且彻底移除对 redb 库的运行时写文件依赖。
- 订单支付流程在 Anvil E2E 环境下顺畅跑通。
- 应用程序容器不再需要挂载任何主机磁盘卷。

## Verification Commands
```bash
cargo check
cargo test
cargo test --test anvil_e2e
```

## Risks and Assumptions
- 极端大日志量通过 RPC 批大小（`SCAN_BATCH_SIZE`）与并发量控制，避免单次拉取超限。

## Status
DONE
