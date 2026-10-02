# TASK-006: HD Wallet 地址无限派生与 Rollover

## Objective
封装 BIP44 分层确定性地址派生算法，支持按照三级索引进位实现无限子地址分配，并提供确定性 Fake 派生器用于快速单元测试。

## Scope
- HD 路径格式：`m/44'/60'/{account_index}'/{change_index}/{address_index}`。
- 三级进位 Rollover 逻辑（`address -> change -> account`）。
- `AddressDeriver` trait 与 `HdWallet` 抽象。
- `DeterministicFakeDeriver` 实现与派生稳定性测试。

## Allowed Files
- `src/wallet/mod.rs`

## Dependencies
- TASK-002

## Inputs and Outputs
- **Inputs**: 派生游标 `(account_index, change_index, address_index)`。
- **Outputs**: 派生路径字符串与 EVM 校验和地址。

## Acceptance Criteria
- 派生结果具备确定性与重复可验证性。
- 达到单层边界时正确触发进位。
- 派生逻辑单测 100% 覆盖。

## Verification Commands
```bash
cargo test wallet::
```

## Risks and Assumptions
- 生产环境仅依赖签名器提供公钥派生支持，不暴露私钥。

## Status
DONE
