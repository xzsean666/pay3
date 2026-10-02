# TASK-018: 远程 JWKS 动态拉取与密钥轮换支持

## Objective
完善生产环境 JWT 鉴权体系，实现通过 `JWT_JWKS_URL` 远程异步拉取 JWKS 公钥集、本地内存缓存、定时背景刷新与密钥无缝轮换，提升企业级身份认证集成能力。

## Scope
- `src/auth/jwt.rs`：新增基于 Reqwest/Tokio 的远程 JWKS 拉取器与缓存解析器。
- 支持 `Cache-Control` / 固定 TTL 缓存过期与按需刷新机制。
- 探测失败与网络分区时的降级保障（防止单次网络超时阻断认证）。
- 密钥轮换场景（Unknown `kid` 触发按需异步更新）单测覆盖。

## Allowed Files
- `src/auth/jwt.rs`
- `src/auth/mod.rs`
- `src/config.rs`
- `tests/auth_contract.rs` (若有)

## Dependencies
- TASK-003
- TASK-016

## Inputs and Outputs
- **Inputs**: 环境变量 `JWT_JWKS_URL`。
- **Outputs**: 动态解析的公钥集，用于验证请求携带的 JWT 签名。

## Acceptance Criteria
- 生产 Profile 下支持指定 `JWT_JWKS_URL` 并能成功启动。
- 模拟 JWKS 远端密钥轮换后，新旧合规 Token 均能正确完成签名认证。
- 远端 JWKS 短暂不可用时使用缓存公钥，不直接 Panic。

## Verification Commands
```bash
cargo test auth::jwt
cargo clippy --all-targets -- -D warnings
```

## Risks and Assumptions
- 依赖网络连接访问指定的 Identity Provider JWKS 端点。

## Status
DONE
