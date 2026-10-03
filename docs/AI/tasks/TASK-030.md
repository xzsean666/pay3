# TASK-030: 智能自动推导与选择最优归集方案 (Auto Collection Strategy Resolution)

## Objective
在 Pay3 资金归集系统中引入智能自动推导与自适应选择机制（`COLLECTION_METHOD=auto`），使系统在启动和运行时能够根据配置的 `CHAIN_ID`、`TOKEN_ADDRESS` 以及 Relayer 状态，自动推导出最优、最省 Gas、最安全的归集策略：
- **EIP-3009** (USDC 极致单笔免授权划转)
- **PolygonMetaTx** (Polygon USDT 原生元交易代付)
- **EIP-2612** (标准 Permit 签名)
- **Standard** (不支持 permit 或未配置 Relayer 时的安全降级)

免除运维与集成人员人工记忆各链各币种具体 EIP 编号的负担。

## Scope
- `src/domain/permit.rs` (扩展 `CollectionMethod::Auto`，内置主流稳定币知识库 Preset 与 `resolve_optimal_collection_strategy` 决策算法)
- `src/config.rs` (支持 `COLLECTION_METHOD=auto` 解析，更新默认值，在 `auto` 模式下智能宽容 Relayer 缺失时的降级)
- `src/runtime.rs` (在 `collection_strategy_config` 中接入自动决策，启动时输出最优归集决策日志)
- 单元测试与集成测试补齐

## Dependencies
- TASK-029

## Acceptance Criteria
1. `CollectionMethod` 支持 `Auto`（解析 `"auto" | "default" | "optimal"`），并作为默认推荐配置；
2. 当配置了 Relayer 时：
   - Polygon PoS 官方 USDT (`0xc2132D...`) 自动选定 `PolygonMetaTx`，自动填入 Salt Domain；
   - 以太坊、Polygon、Arbitrum、Base、Optimism、Avalanche 上的 Circle 原生 USDC 自动选定 `Eip3009`，自动填入 `token_name="USD Coin"`，`token_version="2"`；
   - Arbitrum、Optimism、Avalanche 上的官方 USDT 自动选定 `Eip2612`；
   - 以太坊 USDT、BSC USDT 等不支持 permit 的代币安全自动降级为 `Standard`；
3. 当未配置 Relayer 时：`Auto` 模式安全自动降级为 `Standard`，无需报错；
4. 环境变量若显式指定（如 `COLLECTION_METHOD=eip3009`）则优先严格遵从人工显式配置；
5. 全量 203+ 单元测试、契约测试与集成测试保持 100% 通过。

## Verification Commands
```bash
cargo check --all-targets
cargo test
bash scripts/verify_production_readiness.sh --env-file .env.production.example
```

## Implementation Summary
1. **策略枚举与自适应扩展 (`src/domain/permit.rs`)**:
   - `CollectionMethod` 增加 `Auto` 变体（解析 `"auto" | "default" | "optimal"`），并作为默认推荐配置；
   - 增加 `OptimalStrategyResolution` 数据结构。
2. **主流稳定币预置知识库 (Preset Matrix)**:
   - 覆盖以太坊、Polygon、Arbitrum、Optimism、Base、Avalanche 等主流 EVM 链官方 USDT / USDC 规范地址；
   - 支持根据 `token_symbol` 进行安全启发式兜底匹配；
   - 自动适配各代币的标准 `token_name` 与 `token_version`（如 USDC 对应 `"USD Coin"` / `"2"`，Polygon USDT 对应 `"(PoS) Tether USD"` / `"1"`，Arbitrum USDT 对应 `"Tether USD"` / `"1"`）。
3. **决策算法与自动降级 (`resolve_optimal_collection_strategy`)**:
   - 若未配置 Relayer 钱包，安全平滑降级为 `Standard`；
   - 若代币不支持 permit（如以太坊主网 USDT、BSC USDT 等），自动安全降级为 `Standard`；
   - 用户显式指定的配置或自定义 `token_name` / `token_version` 优先尊重；
4. **运行时装配接入 (`src/runtime.rs`, `src/config.rs`)**:
   - `collection_strategy_config` 在服务启动时自动解析决策并打印结构化 trace 日志；
   - `DEFAULT_COLLECTION_METHOD` 设置为 `Auto`。
5. **单元测试与全量回归**:
   - 新增 `resolve_optimal_strategy_presets_and_fallbacks` 涵盖 11 个核心场景的断言；
   - 全量 204+ 个单元、契约和集成测试全部通过。

## Status
DONE

