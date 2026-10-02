# TASK-001: Rust 基础骨架、配置与健康检查

## Objective
搭建 Pay3 Rust 项目基础工程框架，引入强类型配置管理、结构化日志与追踪、统一错误模型，并提供标准的 `/healthz`、`/readyz`、`/metrics` 端点。

## Scope
- Cargo 工作区或单 Package 配置（`Cargo.toml`）。
- `src/config.rs`：环境变量解析、生产 Profile 校验与敏感信息脱敏。
- `src/error.rs`：统一 API 错误结构（包含 `code`、`message`、`request_id`、`retryable`、`details`）。
- `src/health.rs` 与 API 路由：基础运行状态、依赖探针与 Prometheus 指标输出。

## Allowed Files
- `Cargo.toml`
- `src/main.rs`
- `src/lib.rs`
- `src/config.rs`
- `src/error.rs`
- `src/health.rs`
- `src/api/mod.rs`

## Dependencies
无

## Inputs and Outputs
- **Inputs**: 环境变量（`APP_PROFILE`、`SERVER_PORT`、`DATABASE_URL` 等）。
- **Outputs**: HTTP 服务监听、`/healthz` 响应 `200 OK`、Prometheus 指标流。

## Acceptance Criteria
- 编译无警告（`cargo check`）。
- 环境变量能正确反序列化并校验必填项。
- 生产 Profile 下禁止不安全配置（如调试端口或弱秘钥）。
- 错误响应满足统一 JSON 契约。

## Verification Commands
```bash
cargo check
cargo test config
curl -i http://localhost:8080/healthz
```

## Risks and Assumptions
- 假设底层运行在支持 Tokio 的 64 位 Linux 环境。

## Status
DONE
