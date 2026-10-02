# TASK-002: Domain 核心值对象与状态机

## Objective
定义 Pay3 核心业务领域对象、金额无损解析、链地址与哈希规范化、HD 派生进位计算以及订单/支付/归集状态机转换规则。

## Scope
- `RawAmount` 与 `TokenAmount` 精确计算与字符串解析（杜绝浮点数）。
- 地址与哈希严格规范化（EVM Checksum、0x 前缀规范化）。
- 状态机定义：`OrderStatus`、`PaymentMatchStatus`、`PaymentChainStatus`、`CollectionStatus`。
- `KvReorgEpoch` 与 Nonce Replacement 核心规则。

## Allowed Files
- `src/domain/mod.rs`
- `src/domain/amount.rs`
- `src/domain/address.rs`
- `src/domain/order.rs`
- `src/domain/payment.rs`
- `src/domain/collection.rs`
- `src/domain/kv.rs`
- `src/domain/wallet.rs`
- `src/domain/chain.rs`

## Dependencies
- TASK-001

## Inputs and Outputs
- **Inputs**: 金额字符串、链上原始 Hex、状态迁移指令。
- **Outputs**: 强类型不可变值对象与状态机判定结果。

## Acceptance Criteria
- 杜绝任何浮点数金额转换，溢出安全检查。
- 非法以太坊地址与哈希拒绝构建并返回类型化错误。
- 状态转换单向受限，禁止非法逆向跃迁。

## Verification Commands
```bash
cargo test domain::
```

## Risks and Assumptions
- 假设 ERC20 精度最高不超过 18 位。

## Status
DONE
