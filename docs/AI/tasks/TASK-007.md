# TASK-007: 签名契约与 Remote HTTP 适配器

## Objective
抽象隔离外部区块链交易签名能力，实现 `SignerProvider` Trait，提供用于测试的 Fake 签名器和用于生产环境的 Remote HTTP Signer 适配器。

## Scope
- `SignerProvider` trait：支持交易签名、健康探测与公共地址读取。
- `UnsignedTx` 与 `SignedTx` 契约对象。
- `DeterministicFakeSigner` 实现。
- `RemoteHttpSigner` 客户端实现。
- 生产环境禁止 Fake Signer 的防御规则。

## Allowed Files
- `src/signer/mod.rs`
- `src/signer/local.rs`
- `src/signer/external.rs`
- `tests/signer_contract.rs`

## Dependencies
- TASK-002

## Inputs and Outputs
- **Inputs**: 原始待签名交易 DTO 与 Key 引用。
- **Outputs**: 已签名的 RLP Raw Hex 与 Tx Hash。

## Acceptance Criteria
- 签名结果必须通过 Tx Hash 校验。
- 外部 Signer 服务超时或网络异常时具备清晰的类型化报错。
- 生产 Profile 显式拒绝 Local/Fake Signer。

## Verification Commands
```bash
cargo test --test signer_contract
```

## Risks and Assumptions
- 外部签名器假设提供兼容的 HTTP 签名端点。

## Status
DONE
