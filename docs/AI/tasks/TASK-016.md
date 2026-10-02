# TASK-016: 完整运行时装配与生命周期管理

## Objective
在 `src/runtime.rs` 中完成生产级组件组装：连接数据库、执行 Migration 与 Seed、初始化 redb、校验 RPC Chain ID、启动各后台 Worker 并挂载 HTTP API 路由与健康探针。

## Scope
- `runtime::build_api_router` 完整生命周期装配。
- 启动并托管后台任务：`transfer_log_ingestor_loop`、`payment_scanner_loop`、`collection_collector_loop`、`order_expiry_loop`、`retention_cleanup_loop`。
- 动态周期刷新依赖健康状态（DB、RPC、Signer、KVDB）。
- 优雅停机与错误熔断。

## Allowed Files
- `src/runtime.rs`
- `src/main.rs`
- `src/api/mod.rs`

## Dependencies
- TASK-001 ~ TASK-015

## Inputs and Outputs
- **Inputs**: 应用配置对象 `AppConfig`。
- **Outputs**: 具备完整路由与活跃后台 Worker 的 Axum HTTP Server。

## Acceptance Criteria
- 任何核心依赖（如 DB 连接失败或 RPC 链 ID 不匹配）在启动时直接报错退出，避免带病运行。
- 各 Worker Loop 在发生单次瞬时异常时仅记录日志而不导致进程崩溃。
- `/readyz` 准确反映所有后台依赖的实时可用性。

## Verification Commands
```bash
cargo check
cargo test runtime
```

## Risks and Assumptions
- 启动顺序要求 PostgreSQL 与 RPC 节点优先就绪。

## Status
DONE
