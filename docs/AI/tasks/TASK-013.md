# TASK-013: 异步扫链 Worker 与确认数推进

## Objective
实现后台自动扫链引擎 `workers/scanner`，获取游标租约，消费 KV 日志推进订单付款状态，并在空闲 Tick 运行 Confirmation Sweep 完成最终确认。

## Scope
- 游标 Lease 申请与并发 CAS 保护。
- 联动 `reorg_epoch`：检测到 KV 发生分叉时回退 PostgreSQL 游标并标记孤块重算。
- 分页匹配消费 KV Transfer 日志并单事务落盘。
- Confirmation Sweep：校验观察到的支付区块 Hash 是否依然在规范链上，达到最小确认数后转为 `confirmed`。
- `spawn_payment_scanner_loop` 常驻后台循环。

## Allowed Files
- `src/workers/scanner.rs`
- `src/workers/mod.rs`

## Dependencies
- TASK-005
- TASK-009
- TASK-010

## Inputs and Outputs
- **Inputs**: KV Transfer 日志与 PostgreSQL 游标状态。
- **Outputs**: 推进后的业务游标、更新后的订单支付状态。

## Acceptance Criteria
- 单次 Tick 发生异常不导致整个进程 Panic，输出结构化日志并重试。
- 遇链分叉必须能自动纠偏与回滚，不能将孤块交易判定为已确认。
- 覆盖 Lease 抢占、CAS 冲突、分页不完整等核心单测场景。

## Verification Commands
```bash
cargo test workers::scanner
```

## Risks and Assumptions
- 假设单实例或基于数据库 Lease 保证同一链币种仅有一个 Active Scanner。

## Status
DONE
