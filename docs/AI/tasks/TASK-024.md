# TASK-024: 生产上线审计门禁自动化验证脚本

## Objective
编写一键式生产准入自动化预检脚本（Pre-Flight Verification Script），自动校验环境变量、配置项、安全策略、数据库迁移和依赖服务可用性，提供确定性的生产放行结论。

## Scope
- 编写脚本 `scripts/verify_production_readiness.sh` 或 Rust CLI 工具。
- 门禁规则检查：
  1. 生产配置校验：禁止 `APP_PROFILE=production` 下使用 `SIGNER_MODE=fake`、`HS256`、`JWT_SECRET`、`SCAN_FROM_BLOCK=0`。
  2. 依赖连通性校验：PostgreSQL 版本与迁移状态、至少 2 个活跃 RPC Provider、Signer 服务可用性。
  3. 数据库不变量检查：`treasury_addresses` 非空校验、地址唯一性索引校验。
  4. 磁盘与目录权限校验：redb 目录读写权限与排他锁。
- 输出符合 [docs/PRODUCTION_READINESS.md](file:///ssd0/git/pay3/docs/PRODUCTION_READINESS.md) 标准的格式化预检报告。

## Allowed Files
- `scripts/verify_production_readiness.sh`
- `src/main.rs` (若增加 `--verify-readiness` 子命令)
- `docs/PRODUCTION_READINESS.md`

## Dependencies
- TASK-018 ~ TASK-023

## Inputs and Outputs
- **Inputs**: 目标生产或预发环境配置文件（`--env-file`）。
- **Outputs**: 退出码 `0`（全部通过）或非零退出码（附详细未达标清单）。

## Acceptance Criteria
- 当检测到任何一项生产禁用配置（如明文私钥或弱 JWT）时，脚本立即阻断并退出。
- 预检通过后输出清晰的确认信息与签名校验。

## Verification Commands
```bash
bash scripts/verify_production_readiness.sh --env-file .env.production.example
```

## Risks and Assumptions
- 脚本执行环境具备 `curl`、`jq` 或对应 CLI 工具支持。

## Status
DONE
