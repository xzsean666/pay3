# Pay3

Rust ERC20 token 收款平台。MVP 目标是用单一链、单一 token、单一默认账号跑通完整收款流程：

```text
创建订单 -> 派生收款地址 -> 用户 ERC20 转账 -> 验证付款成功 -> token collect
```

当前仓库已经进入 Rust 实现阶段，已完成基础配置/health/JWT/domain、PostgreSQL migration/repository 初版、HD wallet 地址派生边界、signer contract/fake、订单创建 service 初版、订单 API route contract、chain 纯契约/fake 与 RPC provider manager/RpcRangeSource 初版、transfer log store redb-backed runtime 初版与 retention cleanup loop、付款匹配纯 service、手动 verify service/API route contract、scanner worker tick + runtime loop + confirmation sweep + rolling lookback/lag readiness 初版、collector tick + runtime loop 初版、collect replacement 初版、collection fee/collector timeout 配置化初版、worker tick metrics/readyz 初版、Docker/Compose dry-run 工件、production JWT local JWKS/PEM guard、真实 native gas prefund check、运行时 readiness refresh、订单过期重算 loop，以及统一错误模型/429 rate limit 初版。

生产可用性：当前仓库仍不可用于生产接真实资金。Docker/Compose dry-run、真实 PostgreSQL 集成测试和 Anvil+mock ERC20 e2e 已补，但 production remote signer 服务、远程 JWKS 拉取、告警 dry-run、DB PITR/migration rollback/RPC 切换/KVDB rebuild/runbook 演练仍未闭环，collect finality/reorg 仍需更完整复测。MVP 出口标准已经包含 reorg/finality、collect 崩溃恢复、外部 signer、监控告警、备份恢复、runbook 演练和 e2e 测试；这些不是后续补项。

生产优先约束：每个订单使用新收款地址且永不复用；归集目标固定为 treasury；production profile 不包含 mnemonic/private key/local signer；RPC provider 至少两个；KVDB raw log store 单写者。

地址派生：使用 `account_index/change_index/address_index` 分段 rollover，对 API 表现为无限派生地址池。

扫链数据分工：PostgreSQL 只保存订单、派生地址、matched Pay3 payments、业务 cursor、归集和 outbound tx；raw Transfer logs、非 Pay3 logs、raw scan batches、block header cache 只进 KVDB 或内存。

当前验证：`cargo fmt -- --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`cargo test --locked`、`cargo audit`、`cargo build --release --locked`、`DATABASE_URL=... PAY3_ENV_FILE=.env.example docker compose --env-file .env.example config`、`PAY3_ENV_FILE=.env.example docker compose --env-file .env.example --profile local-db config` 均通过。最近一次全量测试为 180 个库测试 + 75 个 integration/contract 测试，其中包含 2 个 Anvil e2e；真实链 e2e 仍为显式手动 gate。

本地 Docker dry-run：默认需要传入 `DATABASE_URL`，并需要宿主机 `8545` 上有 chain id `31337` 的 Anvil/local JSON-RPC；需要跑 token 转账/归集流程时，先部署 mock ERC20 并覆盖 `TOKEN_ADDRESS`。外部数据库可用 `DATABASE_URL=... PAY3_ENV_FILE=.env.example docker compose --env-file .env.example up --build pay3`；如需 compose 内置 Postgres，使用 `PAY3_ENV_FILE=.env.example docker compose --env-file .env.example --profile local-db up --build`。本地覆盖配置可复制为 `.env` 后用 `PAY3_ENV_FILE=.env docker compose --env-file .env up --build pay3`。该 Compose 入口只用于 development/staging dry-run，不代表生产 signer、RPC、JWT key 和备份演练已经闭环。

文档入口：

- `Agent.md`: AI/开发协作规则和 MVP 边界。
- `docs/MVP_ARCHITECTURE.md`: MVP 架构、API、数据库、模块和测试设计。
- `docs/END_TO_END_FLOW.md`: 从订单到付款、归集的整体流程。
- `docs/MODULE_PLAN.md`: 按模块实现、测试和联调的实施规范。
- `docs/TRANSFER_LOG_KV_MODULE.md`: 独立 ERC20 Transfer log KVDB 采集模块。
- `docs/PRODUCTION_READINESS.md`: MVP 生产验收审计和上线清单。
- `docs/DEPLOYMENT.md`: 部署拓扑、worker 锁、RPC provider 和 readiness 要求。
- `docs/RUNBOOK.md`: RPC、reorg、collection、KVDB rebuild、DB 恢复等 MVP runbook。
- `nextsession.md`: 下一次 session 的任务交接和全局进度。
