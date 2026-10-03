# TASK-031: 链上 RPC 动态探查器与自适应代币元数据提取 (On-Chain Contract Prober & Dynamic Token Metadata Introspection)

## Objective
在 Pay3 归集模块中实现链上智能动态探查器（On-Chain Contract Prober），通过静态只读 `eth_call` 自动探测代币合约支持的免 Gas 接口及提取链上元数据：
1. **链上元数据自省**: 调用 `name()` (`0x06fdde03`) 与 `version()` (`0x54fd4d50`) 动态解码 ABI 字符串，无需人工手动配置 `token_name` / `token_version`；
2. **EIP-3009 特征探查**: 调用 `authorizationState(address,bytes32)` (`0xe19a40d7`)，探测是否支持 USDC 原生单笔划转；
3. **Polygon PoS MetaTx 特征探查**: 调用 `getNonce(address)` (`0x2d0335ab`)，探测是否支持 `executeMetaTransaction`；
4. **EIP-2612 Permit 特征探查**: 调用 `DOMAIN_SEPARATOR()` (`0x3644e515`) 与 `nonces(address)` (`0x7ecebe00`)，探测是否支持标准 Permit；
5. **双引擎协同决策**:
   - 优先通过主流稳定币知识库快速路径秒级匹配；
   - 未命中知识库的新增代币、全新 L2 或测试网，自动执行链上动态探查，全自动适配最优免 Gas 归集方案。

## Scope
- `src/domain/permit.rs` (探查选择器定义、ABI 字符串解码器、`ContractCallClient` trait、`probe_on_chain_token_capabilities`)
- `src/domain/mod.rs` (导出相关结构与函数)
- `src/chain/rpc.rs` (`RpcRangeSource::call_contract` 实现与 `FakeRpcClient` 探查打桩扩展)
- `src/runtime.rs` (在 `collection_strategy_config` 中传入 `rpc_source` 执行自适应探查)
- 单元测试与集成测试

## Dependencies
- TASK-030

## Acceptance Criteria
1. 实现 ABI 动态字符串解码器 `decode_abi_string`，支持标准 64 字节+偏移动态字符串及 32 字节 bytes32 兼容解码；
2. 实现 `ContractCallClient` trait 与 `probe_on_chain_token_capabilities`；
3. `RpcRangeSource` 实现 `call_contract`，通过负载均衡 RPC 节点池发起只读 `eth_call`；
4. 当遇到未在预置知识库中登记的未知代币时，系统能够通过链上探查自动识别 EIP-3009、Polygon MetaTx、EIP-2612 并提取链上真实 `name`；
5. 若链上探查失败或均不匹配，安全平滑降级为 `Standard`；
6. 全量现有测试保持 100% 通过。

## Verification Commands
```bash
cargo check --all-targets
cargo test
bash scripts/verify_production_readiness.sh --env-file .env.production.example
```

## Status
DONE

## Implementation Summary
- `src/domain/permit.rs`:
  - 规范定义标准选择器：`ERC20_NAME_SELECTOR` (`0x06fdde03`)、`ERC20_VERSION_SELECTOR` (`0x54fd4d50`)、`EIP3009_AUTHORIZATION_STATE_SELECTOR` (`0xe94a0102`)、`POLYGON_GET_NONCE_SELECTOR` (`0x2d0335ab`)、`EIP2612_DOMAIN_SEPARATOR_SELECTOR` (`0x3644e515`)、`EIP2612_NONCES_SELECTOR` (`0x7ecebe00`)；
  - 实现通用 `decode_abi_string`，支持动态 ABI 字符串与定长 `bytes32`；
  - 抽象并导出 `ContractCallClient` 异步 trait 与 `OnChainTokenCapabilities`；
  - 实现 `probe_on_chain_token_capabilities` 与 `resolve_optimal_collection_strategy_with_probe`；
- `src/domain/mod.rs`: 完整重导出探查器相关接口；
- `src/chain/rpc.rs`:
  - `RpcRangeSource` 实现 `call_contract` 并实现 `ContractCallClient`；
  - `FakeRpcProvider` 增加 `contract_calls` 与 `with_contract_call` 支持模拟 `eth_call` 测试桩；
- `src/runtime.rs`:
  - `collection_strategy_config` 接入 `resolve_optimal_collection_strategy_with_probe`，在服务启动时自动探测未知代币能力与元数据。
- 完整单元测试覆盖全部分支，全套测试（211 lib tests + 19 integration tests）100% 通过，生产验收脚本全部通过。

