# Pay3 Agent Guide

## 项目定位

Pay3 是一个 Rust 编写的 ERC20 token 收款平台。MVP 只支持单一业务账号、单一 EVM 链、单一 ERC20 token，通过母账号按 HD derivation segment 无限派生子地址收款，并提供创建订单、付款验证、订单状态查询和 token 归集的完整闭环。

## 工作语言与原则

- 面向用户的文档和说明默认使用中文，代码、API 字段、配置名使用英文。
- 先保证 MVP 全流程跑通，再扩展多账号、多链、多 token、webhook、后台管理等能力。
- API 必须简单稳定，客户端只需要 JWT、订单金额、外部订单号，就能拿到收款地址并查询付款状态。
- PostgreSQL 是唯一资金状态真相来源，本地 KV 只能做可重建缓存。
- 可从 PostgreSQL/RPC 重建或回放的数据才允许进本地 KVDB；资金相关状态必须进 PostgreSQL。
- 每次实现前先看全局进度，优先补齐阻断全流程的模块。
- 必须按 `docs/MODULE_PLAN.md` 一个模块一个模块实现；模块要高内聚、接口契约稳定且强、实现低耦合，方便单测和联调。

## Git 与交接规范

- 每完成一个独立功能、bugfix 或模块切片，必须先跑对应格式化/检查/测试，再创建一个本地 git commit。不要把多个无关功能攒到一个 commit。
- 默认只做本地 commit，不 push；除非用户明确要求，否则不要执行 `git push`。
- commit 必须只包含本次任务相关文件。遇到工作区已有未提交或未跟踪文件时，先用 `git status --short` 识别边界，不要把用户或其他 Agent 的无关改动混进 commit。
- 如果当前仓库还没有能代表现状的基线 commit，不要随手把全仓一次性提交；先向用户说明需要建立 baseline，得到明确确认后再做。
- commit 前必须确认 `nextsession.md` 已更新当前状态、剩余缺口和最近验证结果，确保后续 AI 可以通过 commit history + `nextsession.md` 接上。
- commit message 使用简洁英文 conventional commit 风格，例如 `feat(scanner): add payment scanner tick`、`fix(db): share payment upsert helper`、`docs(agent): require handoff commits`。
- 如果测试因外部依赖缺失无法运行，仍可 commit，但必须在 commit message 或 `nextsession.md` 里记录未运行的测试、原因和风险。
- 不允许为了做 commit 而回退、覆盖或重排非本次任务的改动。需要清理历史或重写 commit 时，必须先得到用户明确确认。

## MVP 范围

必须包含：

- JWT 鉴权。JWT 校验通过并具备接口所需 scope 后，视为同一个默认账号。
- 配置单一链和单一 ERC20 token。
- 按母账号和 HD segment 无限派生子收款地址，路径模板为 `m/44'/60'/{account_index}'/{change_index}/{address_index}`。
- 创建订单时为订单分配一个全新的子收款地址。
- 同一个收款地址永久只能对应一个订单，不做地址复用。
- 通过链上 ERC20 `Transfer` 事件和确认数验证付款成功。
- 支持 token collect，从子地址把 token 归集到 treasury 地址。
- PostgreSQL 事务保证订单、支付窗口、付款、归集状态的一致性。
- 本地 KVDB 分两类：`transfer_log_store` 的 raw Transfer log 存储是 MVP 必需模块；通用 cache 后置，只能缓存活跃订单候选、地址映射、非权威扫描进度快照等可重建数据。
- reorg/finality、orphan payment 重算、scanner crash/resume 是 MVP 必做。
- collect nonce、signed transaction、replacement、崩溃恢复和审计日志是 MVP 必做。
- 统一错误模型、端点级 JWT scope、`/readyz`、`/metrics`、结构化日志、告警 dry-run 和 runbook 演练是 MVP 出口标准。
- production profile 必须使用外部 signer/KMS/HSM adapter，拒绝 local/fake signer 和明文 secret。
- RPC provider manager、TransferLogSource capacity gate、KVDB single-writer/fencing、KV reorg epoch 是 MVP 必做。

暂不包含：

- 多商户账号体系。
- 多链、多 token。
- 复杂风控、对账后台、退款、发票、汇率。
- webhook 必达保证和消息队列。
- 用户托管提现。

## 核心不变量

- 一个订单只绑定一个 `child_account_id`、完整 `derivation_path` 和一个 `receive_address`。
- 一个 `receive_address` 永久只能属于一个订单。
- 订单金额入库必须使用 token raw amount，不用浮点数。
- 链上付款以 `(tx_hash, log_index)` 幂等入库。
- `paid` 状态只能在达到配置确认数并通过 canonical chain/reorg 校验后写入。
- cache miss 必须回退 PostgreSQL，不允许因为缓存丢失影响资金判断；cache hit 也不能单独决定资金归属。
- KVDB 写失败不能回滚 PostgreSQL 事务，KVDB 丢失不能影响收款、付款确认、归集恢复。
- 付款归属必须以 PostgreSQL `orders/payment_windows` 为准。
- 扫链原始数据不能作为 PostgreSQL 主数据保存；raw RPC response、raw Transfer logs、非 Pay3 logs、block header cache 只能进 KVDB 或内存。
- PostgreSQL `chain_cursors` 是业务处理游标唯一真相源；KVDB cursor 只是 raw scan cache 的局部快照，不能用于恢复资金状态。
- payments 必须拆分 `match_status` 和 `chain_status`，订单金额只累计 canonical confirmed on-time payments。
- collect 目标地址必须固定为 treasury，不允许 API 调用方传目标地址。
- collect 发送链上交易前必须有幂等 job 和 outbound transaction 记录，并同时持久化 `chain_id/from_address/nonce/signed_tx/tx_hash`，以支持崩溃恢复和同一交易重播。
- JWT 必须校验 `exp/nbf/iat/iss/aud/sub`，归集等资金操作必须有独立 scope。

## 推荐技术栈

- HTTP: `axum`
- async runtime: `tokio`
- PostgreSQL: `sqlx`
- EVM/ERC20: `alloy`
- JWT: `jsonwebtoken`
- 本地 KV: `redb`，`transfer_log_store` 必需；通用 cache 后置
- 配置: `config` 或 `figment`，MVP 可直接用环境变量加 typed config
- 日志和 tracing: `tracing`, `tracing-subscriber`, `tower-http`
- API 文档: `utoipa` 或等价 OpenAPI 输出，MVP 出口必须覆盖公开 API 和错误模型
- 集成测试: PostgreSQL 用 `testcontainers-rs`，链用 Anvil，本地部署 mock ERC20

## 代码结构目标

```text
src/
  main.rs
  config.rs
  api/
    mod.rs
    routes.rs
    dto.rs
  auth/
    mod.rs
    jwt.rs
  domain/
    mod.rs
    amount.rs
    order.rs
    payment.rs
    collection.rs
  db/
    mod.rs
    migrations/
    repositories/
  wallet/
    mod.rs
    hd.rs
  signer/
    mod.rs
    external.rs
  chain/
    mod.rs
    erc20.rs
    alloy_client.rs
  transfer_log_store/
    mod.rs
    config.rs
    cursor.rs
    kv.rs
    rpc.rs
    ingestor.rs
    reader.rs
    types.rs
    reorg.rs
  outbound/
    mod.rs
    nonce.rs
    transactions.rs
  services/
    orders.rs
    payments.rs
    collections.rs
  workers/
    scanner.rs
    collector.rs
  cache/
    mod.rs
    redb_store.rs
```

模块依赖方向：

```text
api -> services -> repositories/transfer_log_store/chain/wallet/signer/outbound/cache
services -> domain
repositories -> domain
chain -> domain
transfer_log_store -> chain + cache/KVDB + domain
cache -> domain snapshots only
```

不要让 `api` 直接写 SQL，不要让 `db` 直接调用链，不要让 `chain` 模块知道 HTTP DTO。`services/payments` 不能直接 `eth_getLogs`，只能依赖 `transfer_log_store::TransferLogReader`；`transfer_log_store` 不能查询 PostgreSQL 或订单。

详细模块交付顺序、接口契约、fake/mock 和联调测试要求见 `docs/MODULE_PLAN.md`。后续实现时必须先声明当前模块和测试边界，再开始改代码。

## 测试策略

- `domain`: 纯单元测试，覆盖金额解析、订单状态机、确认数判断。
- `wallet`: 用固定 test signer 测试 segment rollover 和地址派生稳定性。
- `db`: 用 PostgreSQL 集成测试覆盖事务、唯一约束、每地址只绑定一个订单。
- `chain`: 用 fake trait 测业务逻辑，用 Anvil 测真实 ERC20 事件和 transfer。
- `api`: 用 axum router 测 JWT、请求/响应和错误码。
- `e2e`: 创建订单 -> mock token 转账 -> scanner/verify -> paid -> collect -> collection confirmed。

## 实现优先级

1. Rust 项目骨架、配置、health endpoint。
2. domain types。
3. JWT middleware 和端点级 scope。
4. PostgreSQL migrations 和 repository contract。
5. 创建订单和支付窗口。
6. 独立 `transfer_log_store`：从 `start_block/token_address` 收集 ERC20 Transfer logs 到 KVDB。
7. 手动 verify endpoint，先从 `TransferLogReader` 读日志，不依赖后台 worker。
8. scanner worker，自动推进付款状态。
9. collect job 和 collector worker。
10. metrics/readyz/alerts/runbook drill。
11. Anvil + mock ERC20 的 e2e 测试。

## 安全边界

- 代码必须通过 signer trait 隔离；生产环境必须使用 KMS/HSM/外部签名服务，不允许明文 mnemonic/private key 作为常规部署配置。
- 日志不得输出 mnemonic、private key、JWT、数据库密码。
- 收款地址归集 ERC20 时需要原生币 gas。MVP 先只支持 `prefunded` 策略；后续 gas station 也必须通过生产 signer，不允许明文 gas private key。
- 任何链上交易都必须先写 job、nonce reservation 和 outbound transaction，再广播交易；重试时必须优先重播同一 signed transaction，避免进程崩溃后重复发送。
- `transfer_log_store` 必须处理 raw log reorg/rewind；payment scanner 必须处理 orphan payment，不能只依赖单次 RPC/KV 读取结果。
- redb 不保存任何 secret，不保存不可重建的资金状态。

## MVP 出口判断

当前项目已经进入 Rust 实现阶段，并具备 migration、runtime worker 初版、Docker/Compose dry-run、真实 PostgreSQL 集成测试和 Anvil+mock ERC20 e2e；但 production signer 服务、远程 JWKS 拉取、告警 dry-run、备份恢复/runbook 演练和 collect finality/reorg 完整复测还没有闭环，不可用于生产接真实资金。

MVP 完成并进入生产候选至少需要：

- 完整 Rust 实现、migration、workers、e2e 测试。
- reorg/finality、scanner resume、late/outside_window payment 处理。
- collect nonce、signed transaction 持久化和崩溃恢复。
- replacement/gas bump、outbound tx 审计轨迹。
- JWT claims/scope、统一错误模型、限流和审计日志。
- 外部 signer/KMS contract test、DB 备份恢复、metrics/alert/runbook 演练。
- RPC provider manager、capacity gate、KVDB reorg epoch 和 single-writer fencing。
- 生产配置检查必须禁止地址复用。
- 生产配置检查必须禁止明文 JWT secret、单 RPC provider、local signer、`SCAN_FROM_BLOCK=0`。

## 当前文档入口

- MVP 架构文档: `docs/MVP_ARCHITECTURE.md`
- 模块实施规范: `docs/MODULE_PLAN.md`
- ERC20 Transfer log KVDB 采集模块: `docs/TRANSFER_LOG_KV_MODULE.md`
- MVP 生产验收审计: `docs/PRODUCTION_READINESS.md`
- 部署要求: `docs/DEPLOYMENT.md`
- MVP runbook: `docs/RUNBOOK.md`
- 端到端流程: `docs/END_TO_END_FLOW.md`
- 下一 session 交接: `nextsession.md`
