# Pay3 Deployment Notes

## 当前状态

本文是 MVP 生产候选部署要求，不代表当前项目已经可生产。当前仓库已经进入 Rust 实现阶段，并已提供 Docker/Compose dry-run 工件；生产级部署、告警 dry-run、备份恢复和 runbook 演练仍未闭环。

本地 dry-run 入口：

- `Dockerfile`: 多阶段 Rust release build，runtime 非 root 用户，100% 无状态运行，默认 healthcheck 调 `/readyz`。
- `docker-compose.yml`: 默认只启动 `pay3` 并要求传入 `DATABASE_URL`；内置 PostgreSQL 只在 `local-db` profile 下启用；暴露 `3000`。
- `.env.example`: 本地开发/staging dry-run 示例；production 必须改用真实 RPC、外部 signer、RS256/EdDSA JWT key source 和 secret manager。
- `deploy/prometheus/pay3-alerts.example.yml`: Prometheus 告警 dry-run 示例规则。

使用外部数据库的默认 dry-run：

```bash
DATABASE_URL=postgres://user:pass@db.example:5432/pay3 \
PAY3_ENV_FILE=.env.example \
docker compose --env-file .env.example up --build pay3
```

该默认配置要求宿主机 `8545` 已有 chain id `31337` 的 Anvil/local JSON-RPC；否则 runtime 会在 RPC chain id/readiness 校验处失败。只验证服务启动可以先用占位 `TOKEN_ADDRESS`，要跑 ERC20 转账和归集流程必须先部署 mock ERC20 并更新 `TOKEN_ADDRESS`。

`pay3` 必须显式传入 `DATABASE_URL`，可以来自 shell、项目根 `.env` 或 Compose `--env-file`。`.env.example` 里的默认值只配合 `local-db` profile 使用；本地 Postgres 容器的初始化账号、密码和库名是 `docker-compose.yml` 内部固定的开发值，普通使用不需要再维护拆开的 `POSTGRES_USER`/`POSTGRES_PASSWORD`/`POSTGRES_DB`。

归集交易 fee 默认按当前链 RPC 动态估算，collector 签名前会综合 `eth_feeHistory`、`eth_maxPriorityFeePerGas` 和 `eth_gasPrice`。`COLLECTION_MAX_FEE_PER_GAS_WEI` / `COLLECTION_MAX_PRIORITY_FEE_PER_GAS_WEI` 只作为可选下限，不需要为不同链硬编码。

Polygon Amoy 这类本地真实链测试可以用 `SIGNER_MODE=local` 直接从 env 读取 `SIGNER_MNEMONIC`，但必须同时设置 `ALLOW_LOCAL_SIGNER=true`。这条路径只用于受控测试环境；mnemonic 会进入进程/容器环境，production profile 会拒绝 local signer 以及任何 mnemonic/private key/xprv 环境变量。

生成新的测试助记词可以直接用脚本：

```bash
scripts/generate-mnemonic.sh --words 12 --accounts 1
scripts/generate-mnemonic.sh --write-env --env-file .env
```

需要本地覆盖配置时，复制 `.env.example` 为 `.env`，然后显式选择 env file：

```bash
PAY3_ENV_FILE=.env docker compose --env-file .env up --build
```

如果使用 compose 内置本地 Postgres：

```bash
PAY3_ENV_FILE=.env.example docker compose --env-file .env.example --profile local-db up --build
```

如果使用非 `.env` 文件，必须同时传给 Compose 插值和 `pay3` service 的 `env_file`：

```bash
PAY3_ENV_FILE=staging.env docker compose --env-file staging.env up --build
```

Compose/Dockerfile healthcheck 使用 `/readyz`，用于暴露 DB、RPC、signer、KVDB 和 worker readiness；`/healthz` 只代表进程存活。

## 预编译二进制 Docker 入口

默认 `Dockerfile` 和 `docker-compose.yml` 不变，仍然是在 Docker build 里编译 Rust。需要先在宿主机或 CI 里编译，再让 Docker 镜像只打包二进制时，使用新增的预编译入口：

```bash
scripts/build-prebuilt-binary.sh
```

该命令会执行 `cargo build --release --locked`，然后把可执行文件复制到 `deploy/prebuilt/pay3`。这个二进制不会提交到 git。构建出来的二进制必须匹配运行镜像平台，例如在 Linux amd64 服务器上运行，就要产出 Linux amd64 的 `pay3`。

启动时直接使用完整的 `docker-compose.prebuilt.yml`。它和默认 compose 的 env、端口、volume、healthcheck、可选 `local-db` profile 保持一致，只是 `pay3` 镜像改为打包预编译二进制：

```bash
PAY3_ENV_FILE=.env.test docker compose \
  --env-file .env.test \
  -f docker-compose.prebuilt.yml \
  up -d --build pay3
```

如果使用 compose 内置 PostgreSQL：

```bash
PAY3_ENV_FILE=.env.test docker compose \
  --env-file .env.test \
  --profile local-db \
  -f docker-compose.prebuilt.yml \
  up -d --build
```

只构建镜像不启动容器时：

```bash
docker build -f Dockerfile.prebuilt -t pay3:prebuilt .
```

## 自动化部署与无损升级脚本 (scripts/deploy.sh)

Pay3 提供了开箱即用的自动化部署与无损升级脚本 `scripts/deploy.sh`，支持本地直接部署及通过 SSH + `rsync` 部署到远程服务器。

### 核心特性

1. **本地与远程统一**:
   - 本地部署模式无需指定目录，默认直接部署在当前仓库根目录；
   - 远程 SSH 模式接收目标父级目录（例如 `-d /opt`），自动在远端创建 `/opt/pay3` 目录并部署。
2. **支持三种部署形态**:
   - **默认网关模式 (`--service pay3`)**: 仅部署 Pay3 网关（+ 可选 `local-db` 数据库）。
   - **一体化全量模式 (`--with-signer` / `--service all`)**: 一键同时部署 Pay3 与独立 Signer 签名机（+ 可选 `local-db` 数据库），容器间通过 Docker 内部虚拟局域网通信，满足生产网络隔离同时实现单机一次性部署。
   - **独立签名机模式 (`--signer-only` / `--service signer`)**: 仅部署独立 Signer 签名机服务，适用于将签名机单独部署在专用隔离物理机的场景。
3. **配置与机密自动化**:
   - 支持传入指定环境文件（`-e .env.prod`）与业务配置文件（`-c pay3.yaml`）；
   - 自动设置 `.env` 为 `600` 权限安全保护；
   - 若传入了 `pay3.yaml`，脚本会自动维护 `docker-compose.override.yml` 将其只读挂载到容器 `/var/lib/pay3/pay3.yaml`。
4. **无损升级保证 (Upgrade Without Data Loss)**:
   - 基于 `rsync` 同步代码，严格排除 `./data/` 目录；
   - 容器启动采用 `docker compose up -d --build` 原地滚动更新，严禁清空 `./data/postgres`；
   - 容器启动后自动执行 SQLx 数据库 schema 迁移，确保历史订单、支付、归集及交易数据 100% 完整保留；
   - 再次运行脚本即为版本升级，无需手动停止或删除数据。
5. **服务就绪深度校验**:
   - 部署后自动针对目标服务进行健康检查（Signer 检查 `/healthz`，Pay3 深度检查 `/readyz`）；
   - 若超时未就绪，自动打印对应容器尾部日志协助定位故障。

### 常用命令示例

```bash
# 1. 本地一键部署当前仓库 (默认仅 pay3，免输入目录)
scripts/deploy.sh -e .env.prod -c pay3.prod.yaml

# 2. 本地/远程一键一体化全量部署 (Pay3 + Signer + 数据库 一次性全部部署完)
scripts/deploy.sh --with-signer -e .env.prod -c pay3.prod.yaml
# 或通过远程 SSH 一体化部署:
scripts/deploy.sh --remote root@192.168.1.100 -d /opt --with-signer -e .env.production -c pay3.yaml

# 3. 仅部署独立签名机服务 (部署到专用内网物理机)
scripts/deploy.sh --remote root@signer.internal -d /opt --signer-only -e .env.signer

# 4. 本地指定目录部署/升级 (将在 /data/services/pay3 下部署并保留历史数据)
scripts/deploy.sh --mode local -d /data/services -e .env.prod -c pay3.prod.yaml

# 5. 远程 SSH 首次部署 Pay3 网关 (自动在远端创建 /opt/pay3 目录并同步配置与代码)
scripts/deploy.sh --remote root@192.168.1.100 -d /opt -e .env.production -c pay3.yaml

# 6. 远程 SSH 快速升级 (远端已存有 .env 与 pay3.yaml，仅更新代码和构建)
scripts/deploy.sh --remote root@192.168.1.100 -d /opt

# 7. 指定 SSH 端口与密钥部署
scripts/deploy.sh --remote ubuntu@server.example.com -p 2222 -i ~/.ssh/id_rsa -d /data/apps -e .env.prod -c pay3.yaml

# 8. 使用国内镜像加速 (预编译二进制 + 阿里云/DaoCloud 镜像)
scripts/deploy.sh --remote root@192.168.1.100 -d /opt --cn -e .env.production -c pay3.yaml

# 9. 部署演练 (Dry-run，仅打印部署动作不执行真实操作)
scripts/deploy.sh --remote root@192.168.1.100 -d /opt --with-signer -e .env.prod -c pay3.yaml --dry-run
```

## 服务器 Docker Compose 手动测试部署

下面流程适合在一台测试服务器上用 Docker Compose 跑 Pay3 API + worker 的 combined 进程。它仍是测试/staging dry-run，不是生产上线流程；生产必须满足本文后面的外部 signer、多 RPC、备份、告警和 runbook 要求。

### 1. 准备服务器

服务器需要安装 Docker Engine 和 Docker Compose plugin：

```bash
docker version
docker compose version
```

建议把仓库放在固定目录，例如：

```bash
sudo mkdir -p /opt/pay3
sudo chown "$USER":"$USER" /opt/pay3
git clone <repo-url> /opt/pay3
cd /opt/pay3
```

如果服务器已经有代码，发布新版本时：

```bash
cd /opt/pay3
git pull --ff-only
```

### 2. 准备 env 文件

不要直接使用 `.env.example` 接真实测试网。复制一个独立 env 文件，例如：

```bash
cp .env.example .env.test
chmod 600 .env.test
```

如果使用项目里的 `.env.test` 作为 Amoy 测试环境配置，至少确认：

```bash
PAY3_ENV_FILE=.env.test
APP_PROFILE=development
CHAIN_ID=80002
RPC_HTTP_URLS=https://...
DATABASE_URL=postgres://...
TOKEN_ADDRESS=0x...
TREASURY_ADDRESS=0x...
START_BLOCK=<token 起始扫描区块>
MIN_CONFIRMATIONS=12
SIGNER_MODE=local
ALLOW_LOCAL_SIGNER=true
SIGNER_MNEMONIC=<测试网助记词>
```

`PAY3_ENV_FILE` 很重要：`docker-compose.yml` 会用它决定 service 的 `env_file`。如果文件名是 `.env.test`，就把文件内也写成 `PAY3_ENV_FILE=.env.test`，或者每次命令前显式加 `PAY3_ENV_FILE=.env.test`。

如果服务器前面有 Nginx/Caddy 反代，建议只绑定本机端口：

```bash
PAY3_PORT=127.0.0.1:3000
```

如果要让 Docker 直接暴露到公网端口，可以用：

```bash
PAY3_PORT=3000
```

但必须用安全组/防火墙限制访问，尤其是 `/readyz`、`/metrics` 和所有 `/v1/*` 接口。

### 3. 检查 Compose 渲染结果

先看最终 Compose 配置。输出会包含 env 值，不要贴到公开渠道：

```bash
PAY3_ENV_FILE=.env.test docker compose --env-file .env.test config
```

检查重点：

- `pay3.environment.DATABASE_URL` 是目标 PostgreSQL，不是空值。
- `pay3.environment.APP_BIND` 在 Compose 下是 `0.0.0.0:3000`。
- `pay3.ports` 是否符合预期，例如 `127.0.0.1:3000:3000` 或 `3000:3000`。
- 本地 PostgreSQL 数据通过 `./data/postgres` 目录挂载持久化。

### 4. 启动

使用外部 PostgreSQL 时，只启动 `pay3`：

```bash
PAY3_ENV_FILE=.env.test docker compose --env-file .env.test up -d --build pay3
```

如果只是本机 dry-run 并使用 Compose 内置 PostgreSQL，把 `DATABASE_URL` 改成 `postgres://pay3:pay3_dev_password@postgres:5432/pay3`，然后启用 `local-db` profile：

```bash
PAY3_ENV_FILE=.env.test docker compose --env-file .env.test --profile local-db up -d --build
```

查看日志：

```bash
docker compose --env-file .env.test logs -f pay3
```

查看容器状态：

```bash
docker compose --env-file .env.test ps
```

### 5. 健康检查

在服务器上检查：

```bash
curl -fsS http://127.0.0.1:3000/healthz
curl -fsS http://127.0.0.1:3000/readyz
```

`/healthz` 只说明进程还活着。`/readyz` 会检查 DB、migration、RPC、signer、KVDB 和 worker readiness；如果 RPC、DB、token 地址、signer 或扫描状态不满足要求，`/readyz` 会返回非 200，这是预期的失败信号。

### 6. 已部署 API 验收测试

部署后可以从本仓库对目标 HTTP API 跑黑盒验收测试。该测试不会启动本地 Pay3 服务，会调用 `PAY3_API_BASE_URL`，并在未跳过链上步骤时用 payer 账号发送真实测试网 ERC20 转账，再通过 API 轮询订单付款和归集状态。

```bash
PAY3_RUN_DEPLOYED_API_ACCEPTANCE=1 \
PAY3_DEPLOYED_API_ENV_FILE=.env.test \
PAY3_API_BASE_URL=https://your-pay3-api.example \
cargo test --test deployed_api_acceptance -- --ignored --nocapture
```

env 文件或 shell 需要包含链和测试账号配置：

- `RPC_HTTP_URLS`、`TOKEN_DECIMALS`、`MIN_CONFIRMATIONS`。
- `PAY3_API_JWT` / `PAY3_TEST_JWT`；如果测试环境仍使用 HS256，也可以只提供 `JWT_SECRET`、`JWT_ISSUER`、`JWT_AUDIENCE`、`JWT_KEY_ID`，测试会现场签发带 `orders:create orders:read orders:verify collections:create collections:read` 的 JWT。
- `PAY3_API_TEST_PAYER_PRIVATE_KEY`、`PAY3_E2E_PAYER_PRIVATE_KEY`、`DEPLOYER_PRIVATE_KEY` 之一；或 `PAY3_API_TEST_PAYER_MNEMONIC` / `PAY3_E2E_PAYER_MNEMONIC` / `SIGNER_MNEMONIC`。

常用覆盖项：

- `PAY3_API_TEST_PAYMENT_AMOUNT_RAW=1`: 付款 raw amount，默认 `1`。
- `PAY3_API_TEST_SKIP_CHAIN_PAYMENT=1`: 只测健康检查、鉴权、订单创建和查询，不发链上交易。
- `PAY3_API_TEST_SKIP_COLLECTION=1`: 测到订单付款为止，不创建归集。
- `PAY3_API_TEST_ORDER_PAID_TIMEOUT_SECS=360`、`PAY3_API_TEST_COLLECTION_TIMEOUT_SECS=600`: 调整 worker 轮询等待时间。

该测试会花测试网 gas/token，不能对有真实资金风险的环境随意运行。

### 7. 停止、重启和更新

停止：

```bash
docker compose --env-file .env.test down
```

重启：

```bash
PAY3_ENV_FILE=.env.test docker compose --env-file .env.test up -d --build pay3
```

更新代码后重新构建：

```bash
git pull --ff-only
PAY3_ENV_FILE=.env.test docker compose --env-file .env.test up -d --build pay3
docker compose --env-file .env.test logs -f pay3
```

本地 PostgreSQL 数据保存在 `./data/postgres` 目录下。测试环境如需彻底清空本地数据库：

```bash
docker compose --env-file .env.test down
rm -rf ./data/postgres
```

不要在有真实资金风险的环境随意删除 `./data/postgres`。

### `.env.test` 使用结论

当前 `.env.test` 是 Polygon Amoy 测试网配置，可以用于受控测试服务器 dry-run，但不能用于生产部署。主要原因：

- `APP_PROFILE=development`。
- 使用 `JWT_SECRET` / HS256，只适合开发和测试。
- `SIGNER_MODE=local` 且包含 `SIGNER_MNEMONIC`，助记词会进入容器环境变量。
- `ALLOW_LOCAL_SIGNER=true`，production profile 会拒绝。
- `RPC_HTTP_URLS` 只有一个 provider，不满足生产至少两个 RPC provider 的要求。
- 文件里包含 e2e/deployer 相关私钥变量时，不应该传入长期运行的 server container。

用于测试服务器前，建议删除或注释掉运行时不需要的私钥变量，例如 `PAY3_E2E_PAYER_PRIVATE_KEY`、`DEPLOYER_PRIVATE_KEY`；这些变量不属于 Pay3 runtime 必需配置。

## 生产目标组件拓扑

当前 Docker/Compose dry-run 是单个 `pay3` 进程同时启动 API、log ingestor、scanner 和 collector loop；下面是生产候选拆分部署的目标边界。

- `pay3-api`: HTTP API，只处理鉴权、DTO、service 调用。
- `pay3-log-ingestor`: 独立 ERC20 Transfer log KVDB 采集 worker，按 `(chain_id, token_address)` 从配置的 `start_block` 开始轮询并写本地 KVDB；每个 stream 只能一个 writer。
- `pay3-scanner`: 业务付款匹配 worker，读取 KVDB Transfer logs，按 `(chain_id, token_address)` 通过 PostgreSQL cursor lease 保证单活。
- `pay3-collector`: 归集 worker，可多副本；必须通过 `collections FOR UPDATE SKIP LOCKED` 抢 job，并通过 `account_nonces` 对每个 from address 串行 nonce。
- `postgres`: 资金状态唯一真相源，必须启用 PITR/WAL 备份。
- `redb/KVDB`: log ingestor 本地可重建缓存，只保存 raw scan batches、block header cache、Transfer logs、非 Pay3 logs、watch set snapshot；可重建，不参与资金恢复真相。
- `signer`: KMS/HSM/外部签名服务，生产不得使用本地 mnemonic/private key。

## Worker 锁

- scanner 使用 `chain_cursors.lease_owner/lease_until`，claim 后链外扫描，提交前 CAS 校验 cursor 未变化。
- scanner 还必须校验 `seen_kv_reorg_epoch`，KV epoch 变化时先回退业务 cursor。
- log ingestor 的原始扫描结果如需落盘只能写本地 KVDB；PostgreSQL 只接收 matched Pay3 payments、订单重算和 payment matcher cursor 推进。
- scanner 不直接调用 `eth_getLogs`，只读取 `transfer_log_store::TransferLogReader`。
- log ingestor 多副本必须使用外部 lease + fencing token；`writer_epoch` 写入 range manifest。lease 丢失立即停止写。
- collector 使用 job lock + `account_nonces` 行锁。
- 任何 worker graceful shutdown 必须停止领取新任务，完成或释放当前 lease。

## KVDB 部署边界

- redb 写路径只能使用本地 RWO volume。
- 禁止 NFS/多机器共享同一个 redb 文件作为写路径。
- 多进程读取优先通过 `pay3-log-ingestor` 本机 IPC/HTTP reader service，不依赖共享文件。
- `/readyz` 必须检查 redb 可读写、schema version、stream config、磁盘空间、single-writer/fencing epoch。
- KVDB 丢失后，先按 runbook 从 `min(last_scanned_block - lookback, earliest_unsettled_window_from_block)` 重建，再恢复 scanner。

## RPC Provider

- 生产必须配置至少两个 HTTP RPC provider。
- WebSocket 只做低延迟提示，断线后必须用 HTTP 按 PostgreSQL cursor 补扫。
- provider 健康检查至少包含 chain id、latest/safe/finalized head、block hash、latency、429/error rate。
- provider block hash 不一致时，log ingestor 和 scanner readiness 必须 fail，并停止推进 cursor。
- RPC range source 必须通过 capacity probe；未通过时拒绝为该 token 进入 ready，除非配置了兼容 `TransferLogSource` 的 indexer/分片 source。

## 发布和迁移

- migration 必须先在 staging dry-run。
- DB schema 变更必须兼容当前 API/worker 旧版本，支持滚动发布。
- 发布顺序：migration -> API -> log-ingestor -> scanner -> collector。
- 回滚前确认新旧版本对 `outbound_transactions`、`payments` 状态语义兼容。
- 回滚前确认 KV schema version、reorg epoch、writer_epoch 和 `chain_cursors.seen_kv_reorg_epoch` 兼容。

## Readiness

`/readyz` 只能内网访问，必须检查：

- DB 可连接。
- migration version 正确。
- chain id 和 token contract 匹配配置。
- signer 可用。
- RPC provider 配置数量 >= 2，至少一个可服务，主备 chain_id 一致，safe/finalized head/hash 在允许窗口内一致。
- RPC capacity gate 通过。
- KVDB 可读写、schema version 匹配、stream config 与当前配置一致、磁盘空间满足阈值、single-writer/fencing epoch 有效。
- log ingestor lag 和 payment scanner lag 未超过阈值。
- worker lease 状态可读。
- signer 可用，且 production profile 不允许 local/fake signer。

## 生产配置禁用项

- 禁止明文 JWT secret。
- 禁止单 RPC provider。
- 禁止 local signer。
- 禁止 mnemonic/private key。
- 禁止 `SCAN_FROM_BLOCK=0`。
- 禁止 treasury 为空、零地址或等于任一 child address。
- 禁止 API collection request 接受 `to_address`。
- 禁止 redb 多机共享写路径。

## DB 备份和恢复

- PostgreSQL 必须启用 WAL archiving/PITR。
- dashboard 必须显示最近 base backup 时间、WAL archive 延迟、备份失败计数。
- staging 必须完成 PITR restore drill，并记录 RTO/RPO。
- migration 必须有 dry-run、rollback 验证和旧版本兼容说明。
- 备份必须加密并限制访问权限。
