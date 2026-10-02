# TASK-021: 深度 Reorg 与孤块付款真实 DB 回归

## Objective
在真实 PostgreSQL 数据库与 Anvil/Fake 链环境下，构建深度链分叉（Reorg）场景，完整复现并回归测试 Scanner 消费到新 `reorg_epoch` 后的游标回退、孤块支付标为 `orphaned` 及订单金额回滚重算。

## Scope
- 编写专项测试 `tests/reorg_integration.rs`。
- 模拟场景：
  1. 订单已匹配并在分叉块中记录付款。
  2. 链发生 3~5 个块的深度分叉，原交易被抛弃或替换为不同收款人。
  3. KV 触发 Rewind 并递增 `reorg_epoch`。
  4. Scanner 识别并执行 PostgreSQL 游标回退。
  5. 断言原付款状态变为 `orphaned`，订单状态从 `paid` 回退到 `pending` 或 `partial`。

## Allowed Files
- `tests/reorg_integration.rs`
- `tests/support/**`
- `src/workers/scanner.rs`
- `src/db/repositories/payments.rs`

## Dependencies
- TASK-013
- TASK-017

## Inputs and Outputs
- **Inputs**: 真实 PostgreSQL 连接与受控分叉事件源。
- **Outputs**: 自动化的 Reorg 回归测试用例与验证断言。

## Acceptance Criteria
- 分叉后的孤块支付绝不能残留为 `confirmed`。
- 订单金额在分叉后必须精确重新计算并扣减孤块金额。
- 测试能够稳定重复执行，无残留状态污染。

## Verification Commands
```bash
cargo test --test reorg_integration
```

## Risks and Assumptions
- 需要本地或 CI 环境提供可连接的 PostgreSQL 测试实例。

## Status
TODO
