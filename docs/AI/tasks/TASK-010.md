# TASK-010: 支付窗口查找与纯支付匹配服务

## Objective
实现高性能的地址窗口查找机制与无副作用的纯支付匹配计算逻辑，对 Transfer 日志与待收款订单进行高效对账与时效分类。

## Scope
- `PaymentWindowLookup`：内存活跃 Watch-Set 与 PostgreSQL 批量 Fallback 机制（拒绝逐地址查询）。
- `match_stored_transfer_logs` 纯函数：根据区块时间戳划分 `on_time`、`late`、`outside_window`。
- 输出 `MatchedPaymentInput`、`next_token` 与 `complete_to_block`。

## Allowed Files
- `src/services/payment_windows.rs`
- `src/services/payments.rs`
- `tests/payment_window_lookup_contract.rs`
- `tests/payment_matching_contract.rs`

## Dependencies
- TASK-002
- TASK-009

## Inputs and Outputs
- **Inputs**: KV Transfer Logs 列表与窗口查找适配器。
- **Outputs**: 匹配的支付记录集合与分页断点。

## Acceptance Criteria
- 窗口查找 Miss 时必须走批量 Fallback，禁止 N 次串行单条 SQL。
- 支付时效判定基于区块时间戳而非系统本地时间。
- 纯匹配逻辑契约测试 100% 通过。

## Verification Commands
```bash
cargo test --test payment_window_lookup_contract
cargo test --test payment_matching_contract
```

## Risks and Assumptions
- 假设链上区块时间戳单调递增。

## Status
DONE
