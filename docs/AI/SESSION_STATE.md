# Pay3 当前会话状态 (Session State)

更新时间：2026-10-03

---

## 1. 当前上下文

- **当前 Goal**: 原生多链多 Token 统一调度引擎（单库单私钥下支持 YAML 编排多 Token 矩阵、独立 RPC/Worker 隔离、动态 Axum 路由分发）
- **当前 Task**: [TASK-032: 原生多链多 Token 统一调度引擎](file:///ssd0/git/pay3/docs/AI/tasks/TASK-032.md)
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
   - 创建 [docs/AI/DECISIONS.md](file:///ssd0/git/pay3/docs/AI/DECISIONS.md)：结构化记录 ADR-001 至 ADR-012 架构决策，新增 ADR-011（极简无状态单 Token）与 ADR-012（多 RPC 原子 Round-Robin 与 CD 熔断）。
4. **完成 TASK-025：多 RPC 负载均衡与智能 CD 冷却升级**:
   - 在 `src/chain/rpc.rs` 中为 `RpcProviderManager` 接入原子计数器 `round_robin_counter: Arc<AtomicUsize>`，实现请求在可用 Provider 池中的严格 Round-Robin 均匀轮询分发。
   - 增强阶梯式 CD 冷却隔离（初次 5s、二次 10s、三次及以上最高 60s），并在错误触发与冷却恢复时输出结构化 Tracing 日志。
5. **完成 TASK-026：彻底移除 redb，改造为 Direct-to-Postgres 无状态扫描器**:
   - 从 `Cargo.toml` 彻底移除 `redb` 依赖包，物理删除本地 KV 存储实现及测试。
   - 重构 `PaymentScannerWorker` 为 Direct 扫描器，直接通过带负载均衡和熔断机制的 `RpcRangeSource` 拉取区块日志，内存快速过滤，单事务提交 PostgreSQL，推进游标。
6. **完成 TASK-018 ~ TASK-024 生产就绪与硬化**:
   - 远程 JWKS 动态拉取、生产级外部 Signer 工件、Prometheus 告警规则 Dry-Run、深度 Reorg 回归、归集崩溃恢复与 Nonce 替换回归、Runbook 灾难实操演练、生产门禁一键检查脚本全部完成。
7. **完成 TASK-027 ~ TASK-031 免 Gas 归集与动态链上探查**:
   - 参考 evm-call 优化多 RPC 连接池（并发探测、故障快速漂移与按需法定多数）；
   - 全面审计缺陷修复与生产架构优化；
   - 全功能可插拔免 Gas (Permit / Meta-Tx) 资金归集架构（USDC EIP-3009、Polygon USDT MetaTx、L2 USDT EIP-2612）；
   - 智能自动推导最优归集方案 (`COLLECTION_METHOD=auto`)；
   - 链上 RPC 动态探查器与自适应代币元数据提取。
8. **完成 TASK-032：原生多链多 Token 统一调度引擎 (方案 B 原生落地)**:
   - **声明式 YAML 编排 (`pay3.yaml` / `pay3.example.yaml`)**:
     - 引入 `serde_yaml = "0.9"` 依赖；
     - 扩展 `AppConfig`、`TokenInstanceConfig`、`MultiTokenConfigFile`、`TokenYamlConfig` 模型；
     - 支持从 `pay3.yaml`、`pay3.yml`、`PAY3_CONFIG_FILE` 路径或环境变量无缝加载；
     - 保持 100% 向后兼容单币 `.env` 配置；
     - `validate_profile` 深度检验各 Token 的独立 RPC 数量、起始区块、地址有效性及 `(chain_id, token_address)` 实例防重。
   - **单数据库共享与多 Token 幂等初始化**:
     - 在 `src/db/migrations.rs` 中实现 `seed_multi_runtime_config`；
     - 单一事务内初始化母账号共享的 `wallet_cursors`，并批量安全幂等初始化各代币的 `chain_cursors`、`treasury_addresses`、`relayer_addresses`。
   - **单 Master Signer 派生独立 Relayer 地址**:
     - 遵循 `m/44'/60'/99'/0/{index}` BIP-44 规范，为每个代币派生独立的 Relayer 链上地址并注册入 DB 白名单，彻底杜绝同链多代币归集时的 Nonce 冲突。
   - **运行时多 Worker 并发调度与隔离**:
     - 在 `src/runtime.rs` 中为每个 Token 实例独立分配 `RpcRangeSource`（拥有独立的 Provider 池、原子 Round-Robin 计数器与独立的 CD 熔断隔离，互不干扰）；
     - 为每个 Token 独立调度 `PaymentScannerWorker` 和 `CollectionCollectorWorker`；
     - 统一保全全局后台任务池与优雅关机信号。
   - **Axum 动态统一路由网关**:
     - 实现 `build_multi_token_router`；
     - 原生挂载 `GET /v1/tokens` 查询大盘，返回所有已配置代币的实时元数据；
     - 支持基于 Token 地址的动态路由 `/{token_address}/v1/orders`、`/{token_address}/v1/collections`（大小写与 Checksum 地址自适应）；
     - 支持带链 ID 的显式多链前缀路由 `/{chain_id}/{token_address}/v1/...`；
     - 在单代币模式下自动保持根路径 `/v1/orders` 100% 向后兼容；
     - 统一 404 错误响应完全符合全局 API 错误契约。
   - **验收与回归**:
     - 编写 `tests/multi_token_runtime.rs` 包含 5 项核心集成测试全部通过；
     - 全量 236 项测试无一失败；
     - `scripts/verify_production_readiness.sh` 21 项门禁检查 100% 通过。

---

## 3. 修改、创建与移除的文件清单

### 创建文件
- `docs/AI/tasks/TASK-032.md`
- `pay3.example.yaml`
- `.env.secrets.example`
- `tests/multi_token_runtime.rs`

### 修改文件
- `Cargo.lock`
- `Cargo.toml`
- `docs/AI/SESSION_STATE.md`
- `docs/AI/TASK_INDEX.md`
- `src/api/mod.rs`
- `src/config.rs`
- `src/db/migrations.rs`
- `src/main.rs`
- `src/runtime.rs`

---

## 4. 已运行的验证命令及结果

- `cargo check --all-targets`: **通过**。零错误，零警告。
- `cargo test --all-targets`: **通过**。全量 238 个测试全部绿色通过（含 7 项多 Token 与环境变量插值/回退测试）。
- `DATABASE_URL="..." SIGNER_REMOTE_BEARER_TOKEN="..." PAY3_CONFIG_FILE=pay3.example.yaml cargo run --bin pay3 -- --verify-readiness`: **通过**。机密注入与 3 币矩阵验证全部通过。
- `bash scripts/verify_production_readiness.sh --env-file .env.production.example`: **通过**。21 checks passed, 0 failed.

---

## 5. 未解决问题与剩余工作

- **无未解决问题**。
- `docs/AI/TASK_INDEX.md` 中所有 32 个任务卡（TASK-001 ~ TASK-032）全部处于 `DONE` 状态。
- 系统已完整支持原生多链多 Token 统一调度引擎与声明式 YAML 编排。

---

## 6. 下一步任务与读取入口

- **项目状态**: **100% 生产就绪、多链多 Token 原生调度引擎落地 (Production Ready, Audited & Native Multi-Chain Multi-Token Engine)**
- **读取入口**:
  1. [pay3.example.yaml](file:///ssd0/git/pay3/pay3.example.yaml) (多链多 Token YAML 编排示例)
  2. [docs/AI/tasks/TASK-032.md](file:///ssd0/git/pay3/docs/AI/tasks/TASK-032.md) (原生多链多 Token 引擎任务卡)
  3. [docs/PRODUCTION_READINESS.md](file:///ssd0/git/pay3/docs/PRODUCTION_READINESS.md) (生产验收结论与上线命令)
  4. [docs/RUNBOOK.md](file:///ssd0/git/pay3/docs/RUNBOOK.md) (故障排查与运维指南)
  5. [docs/DEPLOYMENT.md](file:///ssd0/git/pay3/docs/DEPLOYMENT.md) (生产部署架构)
  6. [docs/AI/TASK_INDEX.md](file:///ssd0/git/pay3/docs/AI/TASK_INDEX.md) (全量任务卡索引)
