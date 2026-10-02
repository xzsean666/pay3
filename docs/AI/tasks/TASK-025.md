# TASK-025: 多 RPC 负载均衡与智能 CD 冷却熔断机制升级

## Objective
在 `src/chain/rpc.rs` 中为 `RpcProviderManager` 升级请求调度器：从原来的“单向顺序尝试 (Failover-only)”升级为“原子轮询负载均衡 (Round-Robin) + 智能分级 CD 冷却隔离 (Circuit Breaker Cooldown)”，使得多个配置的 RPC Provider 能均匀分摊请求流量，并在遇到 429、超时、网络错误时自动进入冷却隔离。

## Scope
- `src/chain/rpc.rs`：
  - 增加原子轮询指针 `round_robin_counter: AtomicUsize`。
  - `request_first_success` 与数据查询方法改为从当前轮询游标开始遍历可用的健康 Candidate，实现真正的 Round-Robin 流量分摊。
  - 增强 `ManagedRpcProvider` 的 CD 冷却逻辑：
    - 针对 429 (Rate Limit)、超时或连接异常，记录连续失败次数并触发阶梯式 CD 冷却（初次 5s，二次 15s，多次 60s）。
    - 冷却期内 `is_available(now)` 返回 `false`，流量自动平滑切至其他健康 Provider。
    - 冷却期满后允许请求通行（半开试探）；请求成功则重置失败计数恢复为健康状态。
- `tests/chain_contract.rs`：
  - 增加单测验证多个 RPC Provider 下请求呈 Round-Robin 轮询分布。
  - 增加单测验证 Provider 报错后立即进入 CD，且不再接收后续请求，直到冷却期过。

## Allowed Files
- `src/chain/rpc.rs`
- `tests/chain_contract.rs`

## Dependencies
- TASK-008

## Inputs and Outputs
- **Inputs**: 多个配置的 HTTP RPC Provider。
- **Outputs**: 具备负载均衡与自动熔断退避的 RPC 管理器。

## Acceptance Criteria
- 多个可用 Provider 存在时，请求按 Round-Robin 均匀分发，不再倾斜于首个节点。
- 节点返回 429、超时或连接失败时，自动进入冷却期并记录结构化告警日志。
- 当且仅当所有节点都在冷却期时才降级报错，避免单点限流导致服务不可用。
- 相关单元测试与契约测试通过。

## Verification Commands
```bash
cargo test chain::rpc
cargo test --test chain_contract
cargo clippy --all-targets -- -D warnings
```

## Risks and Assumptions
- 各 RPC 节点需配置为相同 Chain ID（初始化时自动校验）。

## Status
DONE
