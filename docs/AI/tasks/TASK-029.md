# TASK-029: 全功能可插拔免 Gas (Permit / Meta-Tx) 资金归集架构

## Objective
扩展 Pay3 资金归集模块，引入统一可插拔的 `CollectionStrategy` / `CollectionMethod` 机制，支持通过 EIP-712 链下签名 + Relayer（代付人）广播模式实现子地址 **0 原生 Gas** 归集：
1. **EIP-3009 (`transferWithAuthorization`)**: 适配以太坊、Polygon、Arbitrum、Base、Optimism 等多链官方 Native USDC，由子地址签署授权转账，Relayer 单笔交易直接划转至 Treasury，无需 approve；
2. **Polygon PoS Native Meta-Transaction (`executeMetaTransaction`)**: 适配 Polygon PoS 官方 USDT (`ChildERC20` 合约 `0xc2132D05D31c914a87C6611C10748AEb04B58e8F`)，由子地址签署元交易，Relayer 广播并由合约代扣执行；
3. **EIP-2612 (`permit`)**: 适配支持标准 permit 的 ERC-20 代币，签署 permit 后由 Relayer 执行 `transferFrom`；
4. **标准转账回退 (`standard`)**: 保持现有子地址直接调用 `transfer(treasury, amount)` 的兼容能力；
5. **独立 Relayer 代付钱包隔离**: 引入 `COLLECTION_RELAYER_KEY_REF` 与 `COLLECTION_RELAYER_DERIVATION_PATH`，代付 Gas 的钱包与 Treasury 资金库物理分离，子地址无需预充任何 ETH/POL。

## Scope
- `src/domain/permit.rs` (新建：EIP-712 类型定义、Domain Separator 计算与 Calldata ABI 编码)
- `src/domain/mod.rs`
- `src/domain/collection.rs`
- `src/signer/mod.rs` (扩展 `SignerProvider::sign_digest` 与 `TypedSignature`)
- `src/signer/local.rs` (实现本地私钥/助记词 `sign_digest`)
- `src/signer/external.rs` (实现远程 HTTP Signer `/v1/digests/sign`)
- `deploy/signer/reference_signer.py` (增加参考签名服务 `/v1/digests/sign` 接口)
- `src/services/collections.rs` (接入策略模式、Relayer Gas 校验、免 Gas 出站交易构建与 Replacement)
- `src/config.rs` (增加 `COLLECTION_METHOD` 与 Relayer 相关环境变量解析)
- `tests/signer_contract.rs`
- `tests/collector_recovery_integration.rs`

## Dependencies
- TASK-028

## Acceptance Criteria
1. `SignerProvider` 统一支持 `sign_digest`，覆盖 `DeterministicFakeSigner`、`LocalMnemonicSigner` 和 `RemoteHttpSigner`；
2. EIP-3009、Polygon MetaTx 与 EIP-2612 的 EIP-712 签名哈希计算与 ABI 编码完全符合 EVM 规范；
3. 免 Gas 模式下，`ensure_prefunded_gas` 校验 Relayer 钱包的原生币余额，子地址余额为 0 时依然能够顺利归集；
4. 出站交易表 `outbound_transactions` 准确记录 Relayer 地址与 Nonce，同时严格保证归集记录 `collections` 归属与 Treasury 固定不变；
5. 原有标准转账模式（Standard Transfer）100% 向后兼容；
6. 全量单元测试、集成测试与契约测试全部通过。

## Verification Commands
```bash
cargo check --all-targets
cargo test
bash scripts/verify_production_readiness.sh --env-file .env.production.example
```

## Implementation Summary
- **EIP-712 & ABI 模块 (`src/domain/permit.rs`)**:
  - 实现 EIP-712 标准 Domain Separator 与 Struct Hash 计算（包括 EIP-3009 `TransferWithAuthorization`、Polygon PoS 特殊 Salt Domain Separator 与 `MetaTransaction`、EIP-2612 `Permit`）。
  - 实现各免 Gas 方法规范 ABI Calldata 编码器（4-byte selector、右对齐参数、动态 bytes 长度与 32 字节补齐）。
- **统一签名抽象与适配器 (`src/signer/mod.rs`, `local.rs`, `external.rs`, `reference_signer.py`)**:
  - `SignerProvider` trait 扩展 `sign_digest(&self, key_ref, digest) -> Result<TypedSignature, WalletError>`。
  - `LocalMnemonicSigner`、`DeterministicFakeSigner`、`RemoteHttpSigner` 及参考签名服务均支持 `sign_digest`，覆盖 19 个签名契约测试。
- **数据库白名单与安全约束 (`20261003000200_collection_relayers.sql`)**:
  - 新建 `relayer_addresses (chain_id, relayer_address)` 表。
  - 增强 `enforce_collection_outbound_tx()` 触发器：白名单放行注册的 Relayer 作为 `from_address`（用于免 Gas 归集代付原生 Gas），非白名单地址抛出 23514 约束错误。
- **归集策略与服务编排 (`src/services/collections.rs`)**:
  - 支持 `CollectionStrategyConfig` (`Standard`, `Eip3009`, `PolygonMetaTx`, `Eip2612`)。
  - 免 Gas 归集时：由子地址离线签署转账/授权，Payer 自动切换为 `relayer.address`；
  - `gas_checker.ensure_prefunded_gas` 校验 Relayer 钱包的原生 Gas 余额，子收款地址 0 原生 Gas 也可顺利归集；
  - `account_nonces` 分配锁定 Relayer 的 Nonce；
  - `outbound_transactions` 严格落盘 `from_address = relayer.address`，`to_address = treasury`，广播交易签名由 Relayer 完成；
  - 幂等同 Nonce Replacement 支持免 Gas 重构与手续费上浮重新广播。
- **运行时装配 (`src/runtime.rs`, `src/config.rs`)**:
  - 支持环境变量 `COLLECTION_METHOD` (`standard` | `eip3009` | `polygon_meta_tx` | `eip2612`)，`COLLECTION_RELAYER_KEY_REF`、`COLLECTION_RELAYER_DERIVATION_PATH`、`COLLECTION_TOKEN_NAME`、`COLLECTION_TOKEN_VERSION`。
  - 服务启动时自动派生 Relayer 地址并注册进 `relayer_addresses` 表。

## Status
DONE

