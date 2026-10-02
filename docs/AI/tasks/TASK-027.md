# TASK-027: 参考 evm-call 优化多 RPC 连接池（并发探测、快速漂移与按需法定多数）

## Objective
参考 `/ssd0/git/evm-call` 的成熟 RPC 连接池实践（`RpcPool` 与 `JsonRpcBatchExecutor`），优化 Pay3 的 `RpcProviderManager`：
1. 启动校验 `validate_chain_ids` 采用并发请求（Concurrent Probing），消除单节点超时对初始化的串行阻塞；
2. `request_blocks` 优化为“按需法定多数（Bounded Quorum）+ 快速漂移（Fast Failover）”，达到 `min_provider_count` 且哈希一致后即刻返回，不再对池内全部 5~10 个公开节点冗余全量扫遍；单节点遇 429、5xx、超时立即记录失败进入 CD 隔离并自动尝试下一候选节点；
3. 补齐单元测试与回归测试，保证在多公开节点环境下的高吞吐与高容错性。

## Scope
- `src/chain/rpc.rs`：
  - 改造 `validate_chain_ids`：使用 `tokio::task::JoinSet` 并发检查所有 Provider 的 `eth_chainId`。
  - 改造 `request_blocks`：收集到满足 `min_provider_count` 的一致区块后立即提前返回，避免全池冗余调用；遇到失败节点自动记入 CD 并继续尝试下一个候选节点。
  - 增加对应单元测试。
- `docs/AI/TASK_INDEX.md`
- `docs/AI/SESSION_STATE.md`

## Allowed Files
- `src/chain/rpc.rs`
- `docs/AI/tasks/TASK-027.md`
- `docs/AI/TASK_INDEX.md`
- `docs/AI/SESSION_STATE.md`

## Dependencies
- TASK-025, TASK-026

## Acceptance Criteria
- `validate_chain_ids` 在多个 Provider 存在时并发执行，启动校验耗时由最慢节点决定而非串行累加；
- `request_blocks` 在满足 `min_provider_count` 且区块一致后提前退出，不再对后续 Provider 发送冗余查询；
- 当池中节点遇到网络失败或 429 时，无缝 failover 至下一个可用节点，并正确触发 CD 冷却；
- 所有现有测试保持 100% 绿色通过，`cargo check` 零警告。

## Verification Commands
```bash
cargo check --all-targets
cargo test --lib chain::rpc::tests
cargo test
bash scripts/verify_production_readiness.sh --env-file .env.production.example
```

## Status
DONE
