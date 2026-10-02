# TASK-015: 归集 Worker、崩溃重播与 Replacement

## Objective
实现异步归集调度执行引擎 `workers/collector`，包含广播前/后崩溃自愈重播、收据状态推进以及超时卡死交易的同 Nonce Replacement。

## Scope
- **崩溃优先重播**：Tick 开始时优先捞取 `transferring + signed` 但未确认广播的 Outbound，重播同一 Raw Tx。
- **交易广播**：广播已持久化的交易并严格核对 Tx Hash 一致性。
- **收据轮询**：轮询 Receipt 状态，成功标记为 `confirmed`，失败标记为 `failed`。
- **同 Nonce Replacement**：交易超时卡池未上链时，保留旧记录并标记 `replaced`，同 Nonce 提高 Gas 发送替换交易。
- `spawn_collection_collector_loop` 常驻后台循环。

## Allowed Files
- `src/workers/collector.rs`
- `src/workers/mod.rs`
- `tests/collection_db_integration.rs`

## Dependencies
- TASK-005
- TASK-008
- TASK-014

## Inputs and Outputs
- **Inputs**: 数据库待处理与待轮询的 Outbound 交易列表。
- **Outputs**: 链上广播成功的交易与更新后的归集状态。

## Acceptance Criteria
- 进程在广播前后任意时点被杀，重启后必须能恢复并幂等推进。
- Replacement 绝不允许修改收款方（必须保持 Treasury）、Token 或业务金额。
- 归集数据库集成测试通过。

## Verification Commands
```bash
cargo test workers::collector
cargo test --test collection_db_integration
```

## Risks and Assumptions
- 节点支持符合 EIP-1559 或传统 Legacy 交易替换规范。

## Status
DONE
