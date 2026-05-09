# Pay3

Pay3 是一个用 Rust 实现的 ERC20 收款后端。它面向“订单收款”场景：系统为每个订单派生一个独立收款地址，用户向该地址转入指定 ERC20 token，后台通过链上 `Transfer` 日志确认付款，再把收款地址中的 token 归集到固定 treasury 地址。

当前项目仍处于 MVP / dry-run 阶段，不应直接接入真实资金。生产环境还需要完成外部 signer、远程 JWKS、监控告警、备份恢复、RPC 切换和 runbook 演练等闭环。

## 核心流程

```text
创建订单
  -> 派生唯一收款地址
  -> 用户 ERC20 转账
  -> 采集 Transfer logs
  -> 匹配订单付款
  -> 验证付款状态
  -> token collect 到 treasury
```

关键约束：

- 单链、单 token、固定 treasury。
- 每个订单使用一个新派生地址，地址不复用。
- PostgreSQL 保存订单、付款、归集、审计和业务 cursor。
- redb/KVDB 保存可从 RPC 重放的 raw Transfer logs、区块头和采集 cursor。
- payment scanner 只读 KVDB，不直接对每个订单轮询 RPC。
- production profile 不允许使用 mnemonic、private key 或本地 signer。

## 主要模块

- `src/api`: Axum HTTP API，包含 health、readiness、metrics、订单、付款验证和归集接口。
- `src/services`: 订单创建、付款匹配、手动验证、归集业务逻辑。
- `src/db`: PostgreSQL migrations 和 repository 层。
- `src/transfer_log_store`: redb-backed ERC20 Transfer log 采集与读取。
- `src/workers`: log ingestor、payment scanner、collector 等后台循环。
- `src/chain`: JSON-RPC provider、区块头和 Transfer log 数据源。
- `src/signer`: fake、本地 mnemonic、远程 HTTP signer 抽象。
- `src/wallet`: HD wallet 地址派生。
- `frontend-test`: 用于联调的 Cloudflare Pages/Worker 前端测试页。

## API 概览

基础接口：

- `GET /healthz`: 进程存活检查。
- `GET /readyz`: 数据库、RPC、signer、KVDB、worker 等依赖 readiness。
- `GET /metrics`: 简单文本指标。

订单接口：

- `POST /v1/orders`: 创建订单，需要 JWT scope `orders:create`。
- `GET /v1/orders/{id}`: 查询订单，需要 JWT scope `orders:read`。
- `GET /v1/orders/by-external-id/{external_id}`: 按业务外部 ID 查询订单，需要 JWT scope `orders:read`。
- `POST /v1/orders/{id}/verify`: 手动验证订单付款状态，需要 JWT scope `orders:verify`。

归集接口：

- `POST /v1/collections`: 创建归集任务，需要 JWT scope `collections:create`。
- `GET /v1/collections/{id}`: 查询归集任务，需要 JWT scope `collections:read`。

## 本地运行

准备环境：

```bash
cp .env.example .env
```

默认配置面向本地 dry-run：

- API 端口：`3000`
- chain id：`31337`
- RPC：`http://host.docker.internal:8545`
- signer：`fake`
- 数据库：Docker Compose `local-db` profile 内置 PostgreSQL

使用 Docker Compose 启动内置 PostgreSQL 和 Pay3：

```bash
PAY3_ENV_FILE=.env docker compose --env-file .env --profile local-db up --build
```

如果使用外部 PostgreSQL，设置 `DATABASE_URL` 后启动：

```bash
PAY3_ENV_FILE=.env docker compose --env-file .env up --build pay3
```

本地直接运行 Rust 服务：

```bash
cargo run
```

## 常用开发命令

```bash
cargo fmt -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --locked
cargo build --release --locked
```

生成本地 mnemonic：

```bash
scripts/generate-mnemonic.sh --write-env --env-file .env
```

构建预编译二进制镜像工件：

```bash
scripts/build-prebuilt-binary.sh
```

## 配置入口

项目通过环境变量配置，主要配置项见 `.env.example`。

常用变量：

- `APP_PROFILE`: `development`、`test`、`staging` 或 `production`。
- `RUN_ROLE`: `api`、`worker` 或 `all`。
- `DATABASE_URL`: PostgreSQL 连接串。
- `CHAIN_ID`: 目标链 ID。
- `TOKEN_ADDRESS`: ERC20 token 合约地址。
- `TOKEN_DECIMALS`: token 精度。
- `TOKEN_SYMBOL`: token 符号。
- `TREASURY_ADDRESS`: 归集目标地址。
- `RPC_HTTP_URLS`: 逗号分隔的 RPC HTTP provider 列表。
- `START_BLOCK`: Transfer log 采集起始区块。
- `MIN_CONFIRMATIONS`: 付款确认数。
- `SIGNER_MODE`: `fake`、`local` 或 `external`。
- `JWT_ISSUER`、`JWT_AUDIENCE`、`JWT_SECRET`、`JWT_JWKS_JSON`、`JWT_PUBLIC_KEY_PEM`: JWT 验证配置。

## 生产注意事项

当前仓库已经包含较完整的 runtime、contract tests、Anvil e2e、Docker dry-run 和生产验收文档，但默认配置不代表生产可用。

生产接入前至少需要确认：

- 使用外部 signer，不在 Pay3 进程内保存 mnemonic/private key。
- 使用强 JWT key material，生产环境不使用 `JWT_SECRET` dry-run 配置。
- 配置至少两个可靠 RPC provider，并完成 provider 切换演练。
- 为 PostgreSQL 配置备份、PITR 和 migration rollback 流程。
- 为 KVDB rebuild、reorg、collection 卡住、RPC 异常准备 runbook。
- 完成 `/readyz`、`/metrics`、日志、告警和链上资金流 e2e 演练。

## 文档

- `docs/MVP_ARCHITECTURE.md`: MVP 架构、API、数据库、模块和测试设计。
- `docs/END_TO_END_FLOW.md`: 从订单创建到付款匹配、归集的完整流程。
- `docs/MODULE_PLAN.md`: 模块实现、测试和联调计划。
- `docs/TRANSFER_LOG_KV_MODULE.md`: ERC20 Transfer log KVDB 采集模块设计。
- `docs/PRODUCTION_READINESS.md`: 生产验收审计和上线清单。
- `docs/DEPLOYMENT.md`: 部署拓扑、worker 锁、RPC provider 和 readiness 要求。
- `docs/RUNBOOK.md`: RPC、reorg、collection、KVDB rebuild、DB 恢复等操作手册。
- `frontend-test/README.md`: 前端测试页说明。
