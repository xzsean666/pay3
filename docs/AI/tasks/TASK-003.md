# TASK-003: JWT 鉴权体系、Scope 权限与生产密钥门禁

## Objective
实现端点级 JWT 鉴权中间件，支持 Claim 全量校验、细粒度 Scope 访问控制，并在生产 Profile 下强制使用非对称密钥（RS256/EdDSA）。

## Scope
- JWT Claim 校验：`exp`、`nbf`、`iat`、`iss`、`aud`、`sub`、`kid`、`alg`。
- Scope 判定：`orders:create`、`orders:read`、`orders:verify`、`collections:create`、`collections:read`。
- 生产环境门禁：禁止 `HS256` 与明文密钥，支持 Local JWKS JSON 与 PEM 公钥源。

## Allowed Files
- `src/auth/mod.rs`
- `src/auth/jwt.rs`
- `src/auth/scope.rs`
- `src/api/mod.rs`

## Dependencies
- TASK-001
- TASK-002

## Inputs and Outputs
- **Inputs**: HTTP `Authorization: Bearer <token>` 请求头。
- **Outputs**: 鉴权通过的 `Principal` 扩展对象，或统一的 `401 Unauthorized` / `403 Forbidden` 错误。

## Acceptance Criteria
- 缺失或格式错误的 Token 返回 `401`。
- Scope 不满足目标端点要求的 Token 返回 `403`。
- 生产环境配置了 HS256 时启动直接 panic fail-closed。

## Verification Commands
```bash
cargo test auth::
```

## Risks and Assumptions
- 开发与测试环境允许使用临时配置的对称密钥进行单测。

## Status
DONE
