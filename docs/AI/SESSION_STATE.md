# Pay3 当前会话状态 (Session State)

更新时间：2026-10-04

---

## 1. 当前上下文

- **当前 Goal**: 统一本地与远程 SSH 自动化部署与无损升级体系
- **当前 Task**: [TASK-034: 统一本地与远程 SSH 自动化部署与无损升级脚本](file:///ssd0/git/pay3/docs/AI/tasks/TASK-034.md)
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
   - 包含部署后 `/readyz` 深度就绪检查与超时日志自动回显。

---

## 3. 修改、创建与移除的文件清单

### 新增文件
- `scripts/deploy.sh`
- `docs/AI/tasks/TASK-034.md`

### 修改文件
- `docs/DEPLOYMENT.md`
- `docs/AI/TASK_INDEX.md`
- `docs/AI/SESSION_STATE.md`

---

## 4. 已运行的验证命令及结果

- `bash -n scripts/deploy.sh`: **通过**。语法静态检查通过。
- `scripts/deploy.sh --help`: **通过**。帮助文档清晰输出。
- `scripts/deploy.sh -e .env.example -c pay3.example.yaml --dry-run`: **通过**。本地默认免目录部署演练通过。
- `scripts/deploy.sh --mode local -d /data -e .env.example --dry-run`: **通过**。本地指定目录自动拼接 `/data/pay3` 演练通过。
- `scripts/deploy.sh --remote root@192.168.1.100 -d /opt -e .env.example -c pay3.example.yaml --dry-run`: **通过**。远程 SSH 模式自动拼接 `/opt/pay3`、rsync 排除规则、override 生成、compose up 演练通过。
- 异常边界用例（缺少 `-d`、文件不存在、非法参数等）测试: **通过**。错误提示清晰规范。

---

## 5. 未解决问题与剩余工作

- 无遗留阻断项。

