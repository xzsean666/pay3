# Pay3 当前会话状态 (Session State)

更新时间：2026-10-04

---

## 1. 当前上下文

- **当前 Goal**: 生产级自动化部署、Signer 独立与一体化极简部署
- **当前 Task**: [TASK-035: 支持 Signer 独立部署与 Pay3 + Signer 一体化极简部署](file:///ssd0/git/pay3/docs/AI/tasks/TASK-035.md)
- **当前状态**: `DONE`

---

## 2. 已完成内容

1. **AI Agent 指引与项目规则落地**:
   - 创建 [docs/AI_AGENT_PROMPT.md](file:///ssd0/git/pay3/docs/AI_AGENT_PROMPT.md)：完整收录 AI 代理 12 大工作准则、任务拆分、状态流转与交接规范。
   - 创建 [AGENTS.md](file:///ssd0/git/pay3/AGENTS.md)：建立最高规则事实来源，明确 `/ssd0/git` 对应 GitHub 账号 `xzsean666`，严格确立“单实例单 Token”、“禁止地址复用”、“PostgreSQL 资金真相源”、“多 RPC 负载均衡”、“固定 Treasury 归集”等不变量。
2. **总目标与架构文档标准化**:
   - 创建 [docs/AI/GOAL.md](file:///ssd0/git/pay3/docs/AI/GOAL.md)：系统阐述 Pay3 核心愿景、MVP In-Scope / Out-of-Scope、核心设计红线与准出标准。
   - 更新 [docs/AI/ARCHITECTURE.md](file:///ssd0/git/pay3/docs/AI/ARCHITECTURE.md)：提供 Direct-to-Postgres 纯无状态架构拓扑图、单向依赖图、核心 API 契约与数据库表模型。
3. **架构决策沉淀**:
   - 创建 [docs/AI/DECISIONS.md](file:///ssd0/git/pay3/docs/AI/DECISIONS.md)：结构化记录 ADR-001 至 ADR-012 架构决策。
4. **完成 TASK-025 ~ TASK-032 全部生产特性**:
   - 多 RPC 负载均衡与智能 CD 冷却升级；
   - 彻底移除 redb，改造为 Direct-to-Postgres 无状态扫描器；
   - 远程 JWKS 动态拉取、生产级外部 Signer 工件、Prometheus 告警规则 Dry-Run、深度 Reorg 回归、归集崩溃恢复与 Nonce 替换回归、Runbook 灾难实操演练、生产门禁一键检查脚本全部完成；
   - 全功能可插拔免 Gas (Permit / Meta-Tx) 资金归集架构、智能自动推导最优归集方案、链上 RPC 动态探查器；
   - 原生多链多 Token 统一调度引擎 (支持 YAML 编排多 Token 矩阵、独立 RPC/Worker 隔离、动态 Axum 路由分发)。
5. **完成 TASK-033：全面安全审计缺陷修复与高并发性能优化**:
   - **[CRITICAL-01] EIP-2612 归集安全防虚假 confirmed 与资金滞留**:
     - 识别出 ERC-2612 `permit` 在没有链上 Forwarder 合约执行 `transferFrom` 时仅修改 allowance、无法单笔交易直接划转资金的漏洞；
     - 在 `src/domain/permit.rs` 中将 Arbitrum USDT、Optimism USDT、Avalanche USDT、Ethereum DAI、Polygon DAI 的自动推荐策略及链上探测结果安全回退至 `CollectionMethod::Standard`，防止资金滞留；
   - **[CRITICAL-02] 跨链多 Token Worker 认领强隔离**:
     - 在 `CollectionRepository::claim_collection_job` 以及 `OutboundRepository::claim_signed_collect_tx_for_broadcast`、`claim_broadcast_collect_tx_for_receipt` 中新增 `(chain_id, token_address)` 强隔离查询参数；
     - 更新对应 SQL 绑定条件与数据库复合索引，彻底杜绝不同链或不同币种 Worker 间抢单以及跨链错发交易广播；
   - **[HIGH-01] 扫链器 Worker Reorg 自动自愈**:
     - 在 `PaymentScannerWorker::run_forever` 中捕获 `CanonicalBlockHashMismatch` 分叉错误，自动联动 `handle_reorg` 推进 reorg epoch 并回退 `chain_cursors.last_scanned_block`，标记孤块支付为 `orphaned` 并重算订单，杜绝无限死循环卡死；
   - **[HIGH-02] 精准日志过滤 (Topic2 Recipient Filter)**:
     - 扩展 `TransferLogRange` 支持可选 `recipient: Option<EvmAddress>`；
     - 在 `src/chain/rpc.rs:transfer_filter` 中根据 recipient 自动补全 Topic2 32-byte 零填充地址；
     - 在 `ManualOrderVerifyService` 中将待核验订单的 `receive_address` 传入日志范围过滤，彻底根治公网高频代币下日志溢出导致的 1000 条上限拒绝服务问题；
   - **[MED-01] 数据库关键高频查询部分索引**:
     - 增加数据库迁移 `20261003000300_performance_audit_indexes.sql`，创建针对 `orders(expires_at, id)`、`collections(outbound_tx_id)`、`payments(chain_id, token_address, block_number, ...)` 以及多币归集关联查询的高效部分索引，彻底消除定时任务全表扫描；
   - **[MED-02] Remote JWKS 单飞并发排队与双重检查锁**:
     - 在 `JwtVerifier` 中引入单飞互斥锁 `refresh_lock: Arc<tokio::sync::Mutex<()>>` 与双重检查锁模式，彻底杜绝恶意/未知 `kid` 触发的缓存击穿与远端雪崩；
   - **[MED-03] Lock-Free 无锁原子窗口限流器**:
     - 将 `FixedWindowRateLimiter` 内部的 `Mutex<FixedWindowState>` 改造为 `AtomicU64` 与 `AtomicU32` CAS 无锁循环，消除了每秒万级 API 请求时的全局锁争用瓶颈；
   - **[MED-04] API 内部数据库错误脱敏**:
     - 在 API 层统一拦截底层数据库异常，内部输出完整 `tracing::error!` 日志，对外响应统一脱敏为 `"internal server error"`，杜绝数据库表结构、字段名与连接细节外泄；
   - **[PERF-01] 区块头并发异步获取**:
     - 在 `RpcRangeSource::transfer_logs` 中引入 `futures_util::future::try_join_all`，将原本多区块头的串行网络 await 改为并发并行批量拉取，大幅降低扫链 RPC 时延；
   - **[LOW-01] LocalMnemonicSigner 路径私钥缓存**:
     - 为测试/开发环境 `LocalMnemonicSigner` 增加按路径缓存的 `signer_cache`，消除对同一子地址多次操作时重复执行 2048 轮 PBKDF2 的 CPU 开销。
6. **Docker 部署工件与配置中彻底清理 KVDB 遗留与卷挂载优化**:
   - 彻底移除 `docker-compose.yml`、`docker-compose.prebuilt.yml`、`docker-compose.prebuilt.cn.yml` 中的遗留 `KVDB_PATH` 环境变量与 `pay3-kvdb` 命名卷；
   - 移除 top-level `volumes:` 块，将 PostgreSQL 数据卷改为 `./data/postgres` 本地相对路径挂载；
   - 彻底移除 `Dockerfile`、`Dockerfile.prebuilt`、`Dockerfile.prebuilt.cn` 中的 `KVDB_PATH` 与匿名 `VOLUME ["/var/lib/pay3"]` 声明，保持 100% 容器无状态；
   - `src/config.rs` 中将 `KVDB_PATH` 改为可选注入，`.env.production.example` 移除无用占位符；
   - `.gitignore` 增加忽略 `data/` 本地持久化目录；更新 `docs/DEPLOYMENT.md` 相关说明。
7. **完成 TASK-034：统一本地与远程 SSH 自动化部署与无损升级脚本**:
   - 实现 `scripts/deploy.sh`，具备完整的模式选择（`--mode local|ssh`，提供 `--remote` 时自动设为 `ssh`）；
   - 支持部署目录自动规范化：远端或本地指定父级目录时，自动创建并部署至 `<dir>/pay3`；本地模式下默认使用当前仓库，无需输入文件夹；
   - 支持传入机密环境变量（`-e / --env-file`）与业务配置（`-c / --config-file` / `pay3.yaml`），自动设置 `chmod 600` 机密保护并通过 `docker-compose.override.yml` 挂载；
   - 保护数据库持久卷无损升级：`rsync` 增量同步排除 `data/` 目录；采用 `docker compose up -d --build` 原地重启，不执行 `down -v`，启动自动运行 SQLx schema migrations，彻底避免数据丢失；
8. **完成 TASK-035：支持 Signer 独立部署与 Pay3 + Signer 一体化极简部署**:
   - 在 `docker-compose.yml`、`docker-compose.prebuilt.yml`、`docker-compose.prebuilt.cn.yml` 中新增 `signer` 服务并分配 `signer` profile；
   - 优化 `DATABASE_URL` 插值在 standalone signer 模式下的兼容性；`deploy/signer/Dockerfile` 支持 `ARG PYTHON_IMAGE`；
   - 升级 `scripts/deploy.sh` 全面支持三种部署形态：
     - `--service pay3`（默认）：仅部署 Pay3 网关（+ 可选 `local-db`）；
     - `--with-signer`（或 `--service all`）：Pay3 + Signer (+ 可选 `local-db`) 一体化全量部署，通过内部 Docker 网络隔离；
     - `--signer-only`（或 `--service signer`）：仅部署独立 Signer 签名机服务，支持专用隔离物理机部署；
   - 实现双重差异化健康检查：`signer` 校验 `/healthz`，`pay3` 校验 `/readyz`。
9. **完成测试网 Polygon Amoy 免 Gas 代币部署、远程服务无损升级、测试前端更新与真实链 E2E 验证**:
   - **自动化代币部署**:
     - 编写 Foundry 合约：`deploy/tokens/src/MockUSDC.sol` (实现 EIP-3009 `receiveWithAuthorization` 完整免 Gas 归集与 Faucet) 与 `deploy/tokens/src/MockUSDT.sol` (实现 Polygon `executeMetaTransaction` NativeMetaTransaction 完整免 Gas 归集与 Faucet)；
     - 编写部署脚本 `deploy/deploy_test_tokens.sh` 并成功将合约部署至 Polygon Amoy 测试网（Chain ID 80002）：
       * `MockUSDC`: `0xc252a473DC466de3799149A2616ccc625832F40C`
       * `MockUSDT`: `0x2C2e569e3Df89c55Ba46Ce52248Ae180379d0B31`
     - 自动为测试付款钱包 `0x285bd8C75C7647b7da1C1154776633804d4ff5eC` 铸造各 1,000,000 代币，并自动为各 Relayer 分配充足原生 POL Gas；
   - **远程服务升级与 Bugfix**:
     - 修复 `src/health.rs`：移除已被无状态扫描器取代的过时 `TransferLogIngestor` Worker 依赖，消除 `/readyz` 503 报错；
     - 修复 `src/db/repositories/outbound.rs`：补全 `claim_signed_collect_tx_for_broadcast` 与 `claim_broadcast_collect_tx_for_receipt` 中缺失的 `o.gas_limit`、`o.max_fee_per_gas`、`o.max_priority_fee_per_gas` 字段查询；
     - 修复 `src/db/repositories/collections.rs`：在 `create_collection_idempotent` 中增加对子地址已存在活跃归集的优雅捕获并返回已有记录，避免被并发 auto-enqueuer 触发 `one_active_collection_per_child` 唯一键冲突；
     - 提高默认归集 Gas Limit 至 150,000，并在 `src/config.rs` 与 `pay3.yaml` 中支持按 Token 配置独立的 `gas_limit`；
     - 增强 `src/api/mod.rs`：多 Token 模式下默认将主代币路由自动 merge 挂载至根路径 `/v1/...`，同时保留各币种独立前缀路径；
     - 引入 `tower-http` permissive CORS 跨域支持，方便前端直接向网关跨域发起请求；
     - 升级 `scripts/deploy.sh`：自动检测并在存在 `docker-compose.override.yml` 时联动传递参数，实现 `pay3.yaml` 的零配置挂载。
   - **测试前端升级**:
     - 更新 `frontend-test/public/index.html` 与 `app.js`：添加 MockUSDC 和 MockUSDT 在 Polygonscan 上的链上快捷链接，增加币种动态切换下拉菜单；
     - 容器化配置 `frontend-test` Nginx 静态托管及反向代理，暴露端口 10010（通过 Cloudflare Tunnel `https://home-10010.002788.xyz` 访问）；
   - **全链路真实链 E2E 验证 (MockUSDC & MockUSDT 双验证)**:
     - 将归集目标 Treasury 地址修正为 `.env.key` 私钥对应的实际链上地址 `0x55193778A808426Eb4024fADAd1aC3c0968E8AF2`；
     - 更新 `pay3.yaml`、`.env.test`，并为 `tests/deployed_api_acceptance.rs` 增加归集到账地址强制校验与 `PAY3_API_TOKEN_PREFIX` 路由支持；
     - 使用 `scripts/deploy.sh` 远程增量热升级并重载远端配置，`/v1/tokens` 成功暴露新 Treasury 地址；
     - **MockUSDC (EIP-3009 免 Gas 归集) 真实链验证通过**:
       * 订单 ID: `9be13a64-d07d-45cc-a67e-dd48dc624171`
       * 付款 Tx: `0xeab4c08ba5fa1256762d26c81edaea1f5281868e6a10c620a23517f2d4fb1cef`
       * 扫链入账: 监听 12 确认数后标记 `paid`
       * 免 Gas 归集 Tx: `0x81d6140fa810c2d4a92a6159659b4d5bf686f602d3075951f870d98a1cb29c6b` (Block 50411875)
       * 归集结果: +6 USDC 准确划转至 Treasury `0x55193778A808426Eb4024fADAd1aC3c0968E8AF2` (余额: 10,000,006 USDC)；
     - **MockUSDT (Polygon Meta-Tx 免 Gas 归集) 真实链验证通过**:
       * 订单 ID: `310c7e1b-b56d-40ec-950c-36c1d9208b11`
       * 付款 Tx: `0xff2feb1dfa6bb99387a0e2157dafdf4746a88b0d8aa8ce8520fb9c726ebac2c6`
       * 扫链入账: 监听 12 确认数后标记 `paid`
       * 免 Gas 归集确认: 归集记录 `73ce93ad-2977-4f7f-a450-b07245983a10` 标记 `confirmed`
       * 归集结果: +6 USDT 准确划转至 Treasury `0x55193778A808426Eb4024fADAd1aC3c0968E8AF2` (余额: 10,000,006 USDT)；
   - **自动化无头浏览器（Headless Chrome）前端真实环境全链路实测**:
     - 编写基于 `puppeteer-core` 的端到端浏览器实测脚本，直连测试网前端 `https://home-10010.002788.xyz`；
     - **MockUSDC 完整 UI 支付闭环**:
       * 页面动态拉取代币列表并选中 USDC，提交创建订单 `fe-usdc-1792243048422` (金额 2.00 USDC)；
       * 页面正确渲染订单详情与转账二维码 URI，动态派生收款地址 `0x3d70619dadb7d48aef02ed4409a0e4e12773c62c`；
       * 链上实付 2 USDC (`0x3b5e20eddc40c7bc11b30f8c67d51a15e2a75ac2ebef89a4c315c4a5186647d3`)，前端进入“等待支付”页自动轮询；
       * 扫链入账后，页面在 12 秒内**自动跳入“成功页”**，显示绿色“支付成功”徽标与支付详情（订单已支付，paid_amount_raw: 2000000），截图保存为 `02_frontend_usdc_success.png`；
     - **MockUSDT 完整 UI 支付闭环**:
       * 点击“再创建一笔”，切换为 USDT 代币，提交创建订单 `fe-usdt-1792243066149` (金额 3.00 USDT)；
       * 动态派生独立收款地址 `0x21fadc5911d0054e41b16df808784ae60f901757`，截图保存为 `03_frontend_usdt_qr.png`；
       * 链上实付 3 USDT (`0x2233462b985de118da1805436d0c6e7f2197e5c4154e6a267e0b81c98f4970bc`)；
       * 前端自动轮询并在 11.9 秒内**自动跳入“成功页”**，状态显示“已支付”，截图保存为 `04_frontend_usdt_success.png`；
     - **排查并根治两大核心隐藏缺陷**:
       1. **路由大小写与 EIP-55 校验和匹配**: 为 `EvmAddress` 引入 `to_checksum_hex()`，Axum 路由同时挂载全小写与 Checksum 格式，根治前端或第三方传入大小写混合地址导致的 404；
       2. **高频出块链确认扫描饥饿 (Confirmation Sweep Starvation)**: 修复 `PaymentScannerWorker::tick`，使成熟观察支付的确认扫描（`sweep_confirmations`）在每个 tick 优先执行，彻底消除高频出块（如 Polygon 2s 出块）导致 `last_scanned_block >= target_block` 条件几乎无法满足的扫链延迟；
       3. **前端体验优化**: 前端控制台默认勾选 `autoVerify`，兼顾秒级主动链上探测与后台全自动无感知扫链双保险。

7. **完成 TASK-036：全流程闭环本地集成 E2E 测试套件与人工操作模拟**:
   - 编写完全本地化、全业务流程闭环的高可用端到端测试套件 `tests/local_e2e.rs`，100% 本地运行且零外部网络依赖；
   - 包含 Anvil 私有 EVM 链动态部署 MockERC20，结合 PostgreSQL 临时隔离 Schema 与真实 Axum HTTP Server 运行时；
   - 实现了 6 大闭环用例，覆盖标准支付流、模拟人工注资 Gas 垫付、模拟人工即时核验 (`POST /v1/orders/{id}/verify`)、模拟人工迟到付款特批 (`POST /v1/orders/{id}/accept-problem-payment`)、部分付款追加至全额、超额支付、卡单同 Nonce 替换等全部场景；
   - 修复 `tests/anvil_e2e.rs` 中 `CollectionCollectorConfig` 缺失 `with_chain` 的问题；
   - 扩展 `tests/support/anvil.rs` 中的 `send_native_transfer` 辅助工具；
   - 创建 `docs/AI/SKILL.md` 知识与集成指南。

---

## 3. 修改、创建与移除的文件清单

### 新增文件
- `deploy/tokens/src/MockUSDC.sol`
- `deploy/tokens/src/MockUSDT.sol`
- `deploy/tokens/test/MockTokens.t.sol`
- `deploy/deploy_test_tokens.sh`
- `deploy/tokens_amoy.env`
- `deploy/tokens_amoy.json`
- `docs/AI/SKILL.md`
- `examples/pay3.polygon-usdt.yaml`
- `frontend-test/nginx.conf`
- `pay3.yaml`
- `tests/local_e2e.rs`

### 修改文件
- `Cargo.toml`
- `src/main.rs`
- `src/api/mod.rs`
- `src/config.rs`
- `src/health.rs`
- `src/domain/address.rs`
- `src/workers/scanner.rs`
- `src/db/repositories/collections.rs`
- `src/db/repositories/outbound.rs`
- `scripts/deploy.sh`
- `deploy/prebuilt/pay3`
- `docker-compose.prebuilt.yml`
- `docker-compose.yml`
- `frontend-test/public/index.html`
- `frontend-test/public/app.js`
- `frontend-test/wrangler.toml`
- `tests/anvil_e2e.rs`
- `tests/deployed_api_acceptance.rs`
- `tests/support/anvil.rs`
- `docs/AI/SESSION_STATE.md`

---

## 4. 已运行的验证命令及结果

- `cargo test --test local_e2e -- --test-threads=1`: **6 passed, 0 failed**。全部本地 E2E 测试闭环通过。
- `cargo test --test anvil_e2e`: **2 passed, 0 failed**。Anvil 集成测试通过。
- `cargo test --lib`: **212 passed, 0 failed**。全量单元测试通过。
- `curl -fsS https://home-10030.002788.xyz/readyz`: **HTTP 200 OK**。所有组件健康。
- `curl -fsS https://home-10030.002788.xyz/v1/tokens`: **HTTP 200 OK**。正常返回 USDC 与 USDT 双代币配置，Treasury 为 `0x55193778a808426eb4024fadad1ac3c0968e8af2`。
- `curl -I https://home-10010.002788.xyz`: **HTTP 200 OK**。前端静态页面正常提供服务。
- `PAY3_RUN_DEPLOYED_API_ACCEPTANCE=1 cargo test --test deployed_api_acceptance -- --ignored --nocapture`: **1 passed, 0 failed** (MockUSDC 真实环境 E2E 验收通过，耗时 100.77s)。
- `PAY3_RUN_DEPLOYED_API_ACCEPTANCE=1 PAY3_API_TOKEN_PREFIX=0x2c2e569e3df89c55ba46ce52248ae180379d0b31 TOKEN_ADDRESS=0x2C2e569e3Df89c55Ba46Ce52248Ae180379d0B31 TOKEN_SYMBOL=USDT cargo test --test deployed_api_acceptance -- --ignored --nocapture`: **1 passed, 0 failed** (MockUSDT 真实环境 E2E 验收通过，耗时 101.96s)。
- `node scratch/test_frontend.js`: **ALL FRONTEND TESTS PASSED SUCCESSFULLY!** (自动化无头浏览器对前端测试台真实界面全流程交互、二维码渲染、链上付款、自动轮询及成功页跳转双币种验收全部通过)。
- `node scratch/run_frontend_test.js`: **ALL FRONTEND TESTS PASSED SUCCESSFULLY!** (自动化无头浏览器验证领水 Faucet UI 与跳转、MockUSDC 真实支付闭环 11.3s 自动跳入成功页、MockUSDT 真实支付闭环 14.6s 自动跳入成功页；并在链上实测 `faucet()` 与 `./deploy/faucet_transfer.sh` 划拨双币种均 100% 成功到账)。

---

## 5. 未解决问题与剩余工作

- 无遗留阻断项。全部目标均已达成。

