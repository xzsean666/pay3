# TASK-019: 生产级外部 Signer 部署工件与鉴权加固

## Objective
为生产环境 `RemoteHttpSigner` 补充独立的轻量签名服务参考实现或部署工件（Docker/Compose）、双向 TLS (mTLS) 或请求签名校验，确保生产部署边界闭环。

## Scope
- `deploy/signer/`：编写独立的生产签名器参考部署配置与 Dockerfile。
- 增强 `src/signer/external.rs`：支持签名请求带上授权 Header（Bearer Token 或 HMAC 签名），防止内部网络被探测利用。
- 完善外部 Signer 超时、重试、健康探测与指标导出。

## Allowed Files
- `src/signer/external.rs`
- `src/signer/mod.rs`
- `src/config.rs`
- `deploy/signer/**`
- `tests/signer_contract.rs`

## Dependencies
- TASK-007
- TASK-016

## Inputs and Outputs
- **Inputs**: 部署配置与带鉴权的 Remote Signer 端点。
- **Outputs**: 安全隔离的签名交互通道与自动化集成校验。

## Acceptance Criteria
- 生产环境配置 Remote Signer 并携带认证 Token 时能完成归集交易签名。
- 模拟签名器返回 401/500/超时能被类型化感知并记录审计告警。

## Verification Commands
```bash
cargo test --test signer_contract
cargo clippy --all-targets -- -D warnings
```

## Risks and Assumptions
- 生产环境中 Signer 服务应部署在受限的专用 VPC 或私有网络中。

## Status
DONE

