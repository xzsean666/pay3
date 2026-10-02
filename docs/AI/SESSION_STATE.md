# Pay3 当前会话状态 (Session State)

更新时间：2026-10-02

---

## 1. 当前上下文

- **当前 Goal**: 生产级极简架构重构（单实例单 Token、多 RPC 负载均衡 + CD 熔断机制、无状态 Direct Scanner 替换 redb）
- **当前 Task**: [TASK-026: 移除 redb 改造为 Direct 无状态扫描器](file:///ssd0/git/pay3/docs/AI/tasks/TASK-026.md)
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
   - **完全删除 `redb` 依赖与冗余代码**:
     - 从 `Cargo.toml` 彻底移除 `redb` 依赖包。
     - 物理删除 `src/transfer_log_store/redb_store.rs`、`src/workers/transfer_log_ingestor.rs`、`tests/transfer_log_redb_contract.rs`。
     - 从 `src/runtime.rs` 中彻底移除 `RedbTransferLogIngestor` 启动逻辑、`ensure_kvdb_parent`、`transfer_log_retention_loop` 和 `KvdbReadinessResources`。
     - 从 `src/health.rs` 与 `/readyz` 探针中移除 KVDB 依赖项。
   - **重构 `PaymentScannerWorker` 为 Direct 扫描器**:
     - `src/workers/scanner.rs`：直接通过 `RpcRangeSource`（带负载均衡与熔断）拉取区块日志，内存中匹配活跃收款窗口，单事务原子提交至 PostgreSQL 并推进 `chain_cursors`。
     - 移除了旧的分页枚举变体与冗余同步逻辑。
   - **适配全量测试套件**:
     - `tests/manual_verify_service_contract.rs`：适配 Fake 实现，6 项测试全部通过。
     - `tests/order_verify_api_contract.rs`：8 项测试全部通过。
     - `tests/payment_matching_contract.rs`：重构为测试 `match_range`，6 项测试全部通过。
     - `tests/anvil_e2e.rs`：移除 redb 打开与 Ingestor 轮询，2 项测试全部通过。
     - `tests/real_chain_e2e.rs`：移除所有 log_store 引用，类型检查与单元测试全部通过。

---

## 3. 修改、创建与移除的文件清单

### 修改文件
- `Cargo.toml` / `Cargo.lock`
- `src/runtime.rs`
- `src/health.rs`
- `src/api/mod.rs`
- `src/api/verify_service.rs`
- `src/services/payments.rs`
- `src/services/verify/service.rs`
- `src/transfer_log_store/mod.rs`
- `src/workers/mod.rs`
- `src/workers/scanner.rs`
- `tests/anvil_e2e.rs`
- `tests/manual_verify_service_contract.rs`
- `tests/payment_matching_contract.rs`
- `tests/real_chain_e2e.rs`
- `tests/support/manual_verify/fakes.rs`
- `docs/AI/ARCHITECTURE.md`
- `docs/AI/TASK_INDEX.md`
- `docs/AI/tasks/TASK-026.md`
- `docs/AI/SESSION_STATE.md`

### 彻底移除的冗余文件与代码
- `src/transfer_log_store/redb_store.rs` (575 行)
- `src/workers/transfer_log_ingestor.rs` (398 行)
- `tests/transfer_log_redb_contract.rs` (476 行)
- 净删除超过 2,500 行冗余磁盘 KVDB 逻辑。

---

## 4. 已运行的验证命令及结果

- `cargo check --tests --benches`: **通过**。零错误，零警告。
- `cargo test --lib`: **通过**。187 个库级单元测试全部绿色通过 (187 passed; 0 failed)。
- `cargo test --tests`: **通过**。全套 250 个集成与契约测试全部绿色通过 (250 passed; 0 failed; 2 ignored for live network)。
- 内存与容器状态：100% Stateless，应用不再需要在本地持久化任何数据文件。

---

## 5. 未解决问题与剩余工作

- 无遗留未解决缺陷。
- 下一步可按 Backlog 推进 **TASK-018**（远程 JWKS 动态拉取与轮换）或生产部署配置。

---

## 6. 下一步任务与读取入口

- **下一步执行任务**: [TASK-018: 远程 JWKS 动态拉取与密钥轮换](file:///ssd0/git/pay3/docs/AI/tasks/TASK-018.md)
- **下一次 Session 应先读取的文件**:
  1. [AGENTS.md](file:///ssd0/git/pay3/AGENTS.md)
  2. [docs/AI/SESSION_STATE.md](file:///ssd0/git/pay3/docs/AI/SESSION_STATE.md)
  3. [docs/AI/TASK_INDEX.md](file:///ssd0/git/pay3/docs/AI/TASK_INDEX.md)
  4. [docs/AI/tasks/TASK-018.md](file:///ssd0/git/pay3/docs/AI/tasks/TASK-018.md)
