# Pay3 当前会话状态 (Session State)

更新时间：2026-10-02

---

## 1. 当前上下文

- **当前 Goal**: 生产级极简架构重构与生产就绪硬化（单实例单 Token、多 RPC 负载均衡 + CD 熔断机制、无状态 Direct Scanner、远程 JWKS 动态拉取与轮换、远程 Signer 部署与鉴权加固）
- **当前 Task**: [TASK-019: 生产级外部 Signer 部署工件与鉴权加固](file:///ssd0/git/pay3/docs/AI/tasks/TASK-019.md)
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
   - 从 `src/runtime.rs` 中彻底移除 `RedbTransferLogIngestor`、`ensure_kvdb_parent`、数据保留清理循环和 KV 游标准备资源。
   - 应用容器实现 100% 无状态（Stateless），不再需要在本地磁盘挂载卷或写任何缓存数据。
   - 重构 `PaymentScannerWorker` 为 Direct 扫描器，直接通过带负载均衡和熔断机制的 `RpcRangeSource` 拉取区块日志，内存快速过滤，单事务提交 PostgreSQL，推进游标。
6. **完成 TASK-018：远程 JWKS 动态拉取与密钥轮换支持**:
   - 在 `src/auth/jwt.rs` 中设计 `RemoteJwksClient` 与 `JwksState`，使用 `Arc<RwLock<JwksState>>` 维护公钥集。
   - 支持通过 `JWT_JWKS_URL` 远程异步拉取 JWKS 公钥集，解析 HTTP `Cache-Control: max-age=...` 支持自适应 TTL（默认 300s）。
   - 实现按需密钥轮换与冷却保护（5s Cooldown），当收到携带未缓存 `kid` 的 Token 时自动触发最新 JWKS 拉取并动态更新缓存。
   - 降级保护：网络抖动或远端 5xx 故障时优雅降级至本地缓存公钥，不 panic。
   - `src/config.rs` 生产 Profile 校验放行合法 HTTPS `JWT_JWKS_URL`。
   - `src/runtime.rs` 在启动时装配远程 JWKS 并调度后台静默定时保鲜任务。
   - 完善单测覆盖：初次拉取验签、密钥轮换、网络异常降级、Cache-Control TTL 自适应解析。
7. **完成 TASK-019：生产级外部 Signer 部署工件与鉴权加固**:
   - 建立 `deploy/signer/` 独立生产部署工件：包含 `Dockerfile`、`docker-compose.yml`、`README.md` 与可执行的 `reference_signer.py` 参考签名服务。
   - 在 `tests/signer_contract.rs` 中增加 401 Unauthorized、500 Internal Server Error 与超时断言覆盖。
   - 全部 16 个签名契约测试均成功通过。

---

## 3. 修改、创建与移除的文件清单

### 创建文件
- `deploy/signer/reference_signer.py`
- `deploy/signer/Dockerfile`
- `deploy/signer/docker-compose.yml`
- `deploy/signer/README.md`

### 修改文件
- `tests/signer_contract.rs`
- `docs/AI/TASK_INDEX.md`
- `docs/AI/tasks/TASK-019.md`
- `docs/AI/SESSION_STATE.md`

---

## 4. 已运行的验证命令及结果

- `cargo check --all-targets`: **通过**。零错误，零警告。
- `cargo test --test signer_contract`: **通过**。全部 16 个单测绿色通过。
- `cargo test --tests`: **通过**。全部 257 个单元/契约/集成测试全部绿色通过。

---

## 5. 未解决问题与剩余工作

- 无遗留未解决缺陷。
- 下一步按 Backlog 推进 **TASK-020**：Prometheus 告警规则 Dry-Run 与指标补全。

---

## 6. 下一步任务与读取入口

- **下一步执行任务**: [TASK-020: Prometheus 告警规则 Dry-Run 与指标补全](file:///ssd0/git/pay3/docs/AI/tasks/TASK-020.md)
- **下一次 Session 应先读取的文件**:
  1. [AGENTS.md](file:///ssd0/git/pay3/AGENTS.md)
  2. [docs/AI/SESSION_STATE.md](file:///ssd0/git/pay3/docs/AI/SESSION_STATE.md)
  3. [docs/AI/TASK_INDEX.md](file:///ssd0/git/pay3/docs/AI/TASK_INDEX.md)
  4. [docs/AI/tasks/TASK-020.md](file:///ssd0/git/pay3/docs/AI/tasks/TASK-020.md)

