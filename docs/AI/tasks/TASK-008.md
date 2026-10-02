# TASK-008: 链客户端契约、RPC 管理器与容量门禁

## Objective
抽象区块链交互层，实现多节点容灾的 `RpcProviderManager`，提供 ERC20 日志范围拉取、链头读取、余额与收据查询，并集成容量门禁（Capacity Probe）。

## Scope
- `ChainHeaderReader`、`TransferLogSource`、`Erc20ChainClient` traits。
- `JsonRpcProvider` 与 `HttpJsonRpcProvider`。
- `RpcProviderManager`：多节点轮询、Chain ID 检查、同高 Hash 冲突保护、429/超时自动 Failover。
- `RpcRangeSource`：集成 `CapacityProbe` 门禁，防止单块日志暴增打垮系统。

## Allowed Files
- `src/chain/mod.rs`
- `src/chain/rpc.rs`
- `tests/chain_contract.rs`

## Dependencies
- TASK-002

## Inputs and Outputs
- **Inputs**: 区块高度范围、RPC URL 列表、Token 地址。
- **Outputs**: 标准化的区块头、ERC20 Transfer 日志、交易收据、余额。

## Acceptance Criteria
- 节点返回错误或限流时能无缝 Failover 到备用节点。
- 区块 Hash 冲突时主动 Fail-Closed 停止推进。
- 日志密度超出配置容量时拒绝拉取并报错。

## Verification Commands
```bash
cargo test --test chain_contract
```

## Risks and Assumptions
- 生产环境要求至少配置 2 个独立的 RPC Provider。

## Status
DONE
