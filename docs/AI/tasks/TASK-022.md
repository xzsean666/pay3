# TASK-022: 归集崩溃恢复与 Finality/Reorg 深度回归

## Objective
对资金归集全链路进行深度混沌测试，模拟在“签名后未广播”、“广播后未存收据”、“链重组撤销归集交易”等各种极端异常边界下，系统的自愈恢复与同 Nonce 幂等保证。

## Scope
- 编写专项测试 `tests/collector_recovery_integration.rs`。
- 模拟进程在 `insert_signed_tx` 之后、`broadcast` 之前强行终止并重启，验证 Collector 自动捞起重播。
- 模拟链上 Reorg 导致已确认的归集交易 Receipt 消失或 Revert，验证状态机安全回退或告警。
- 验证高并发同 Nonce Replacement 时不会产生多笔 Active 交易。

## Allowed Files
- `tests/collector_recovery_integration.rs`
- `tests/support/**`
- `src/workers/collector.rs`
- `src/services/collections.rs`

## Dependencies
- TASK-015
- TASK-017

## Inputs and Outputs
- **Inputs**: 真实 PostgreSQL 事务与模拟崩溃注入。
- **Outputs**: 强韧自愈测试日志与断言结果。

## Acceptance Criteria
- 广播前崩溃恢复测试 100% 优先重播原有 Signed Tx。
- 数据库 Partial Unique Index 绝不允许出现两个同 Nonce 的活跃 Outbound 记录。
- 归集恢复全流程测试通过。

## Verification Commands
```bash
cargo test --test collector_recovery_integration
```

## Risks and Assumptions
- 需要模拟故障注入机制。

## Status
TODO
