# Pay3 当前会话状态 (Session State)

更新时间：2026-10-02

---

## 1. 当前上下文

- **当前 Goal**: 生产级极简架构重构（单实例单 Token、多 RPC 负载均衡 + CD 熔断机制、无状态 Direct Scanner 替换 redb）
- **当前 Task**: [TASK-025: 多 RPC 负载均衡与智能 CD 冷却熔断机制升级](file:///ssd0/git/pay3/docs/AI/tasks/TASK-025.md)
- **当前状态**: `DONE`

---

## 2. 已完成内容

1. **AI Agent 指引与项目规则落地**:
   - 创建 [docs/AI_AGENT_PROMPT.md](file:///ssd0/git/pay3/docs/AI_AGENT_PROMPT.md)：完整收录 AI 代理 12 大工作准则、任务拆分、状态流转与交接规范。
   - 创建 [AGENTS.md](file:///ssd0/git/pay3/AGENTS.md)：建立最高规则事实来源，明确 `/ssd0/git` 对应 GitHub 账号 `xzsean666`，严格确立“单实例单 Token”、“禁止地址复用”、“PostgreSQL 资金真相源”、“多 RPC 负载均衡”、“固定 Treasury 归集”等不变量。
2. **总目标与架构文档标准化**:
   - 创建 [docs/AI/GOAL.md](file:///ssd0/git/pay3/docs/AI/GOAL.md)：系统阐述 Pay3 核心愿景、MVP In-Scope / Out-of-Scope、核心设计红线与准出标准。
   - 创建 [docs/AI/ARCHITECTURE.md](file:///ssd0/git/pay3/docs/AI/ARCHITECTURE.md)：提供 Mermaid 拓扑图、单向依赖图、核心 API 契约与数据库表模型。
3. **架构决策沉淀**:
   - 创建 [docs/AI/DECISIONS.md](file:///ssd0/git/pay3/docs/AI/DECISIONS.md)：结构化记录 ADR-001 至 ADR-012 架构决策，新增 ADR-011（极简无状态单 Token）与 ADR-012（多 RPC 原子 Round-Robin 与 CD 熔断）。
4. **任务索引与细粒度任务拆解**:
   - 创建 [docs/AI/TASK_INDEX.md](file:///ssd0/git/pay3/docs/AI/TASK_INDEX.md)：完整映射 17 个已完成功能基石任务（TASK-001 ~ TASK-017）和最新生产就绪任务。
   - 创建 `docs/AI/tasks/TASK-001.md` 至 `docs/AI/tasks/TASK-026.md`。
5. **清理干扰 AI 的冗余文件**:
   - 彻底删除根目录下陈旧重叠的 `Agent.md`、`nextsession.md` 和非标准临时文件 `cmd`。
   - 彻底删除 `docs/` 下 5 份历史冗余草稿（`RUST_DOCKER_DEPLOYMENT_AI_GUIDE.md`、`MODULE_PLAN.md`、`TRANSFER_LOG_KV_MODULE.md`、`END_TO_END_FLOW.md`、`MVP_ARCHITECTURE.md`）。
6. **完成 TASK-025：多 RPC 负载均衡与智能 CD 冷却升级**:
   - 在 `src/chain/rpc.rs` 中为 `RpcProviderManager` 接入原子计数器 `round_robin_counter: Arc<AtomicUsize>`，实现请求在可用 Provider 池中的严格 Round-Robin 均匀轮询分发。
   - 增强阶梯式 CD 冷却隔离（初次 5s、二次 10s、三次及以上最高 60s），并在错误触发与冷却恢复时输出结构化 Tracing 日志。
   - 新增针对性轮询分发单测 `rpc_provider_manager_distributes_requests_round_robin` 并全绿通过。

---

## 3. 修改、创建与移除的文件清单

### 修改文件
- `src/chain/rpc.rs`
- `AGENTS.md`
- `docs/AI/DECISIONS.md`
- `docs/AI/TASK_INDEX.md`
- `docs/AI/tasks/TASK-025.md`
- `docs/AI/SESSION_STATE.md`

### 新建任务卡
- `docs/AI/tasks/TASK-025.md`
- `docs/AI/tasks/TASK-026.md`

### 彻底移除的干扰文件
- `Agent.md`
- `nextsession.md`
- `cmd`

---

## 4. 已运行的验证命令及结果

- `cargo test chain::rpc`: **通过**。9 项单测全过（含新增的 Round-Robin 均衡分发测试）。
- `cargo test --quiet`: **通过**。196 单元测试 + 65 契约/集成测试 + 2 Anvil E2E 测试全绿通过。
- `cargo check`: **通过**。编译耗时 4.7s，零错误。
- `git status --short`: **通过**。变更边界清晰。

---

## 5. 未解决问题与剩余工作

- 下一步实施 **TASK-026**：移除本地 `redb` 依赖与残留的清理循环，将 Scanner Worker 重构为 Direct-to-Postgres 无状态直连模式。

---

## 6. 下一步任务与读取入口

- **下一步执行任务**: [TASK-026: 移除 redb 改造为 Direct 无状态扫描器](file:///ssd0/git/pay3/docs/AI/tasks/TASK-026.md)
- **下一次 Session 应先读取的文件**:
  1. [AGENTS.md](file:///ssd0/git/pay3/AGENTS.md)
  2. [docs/AI/SESSION_STATE.md](file:///ssd0/git/pay3/docs/AI/SESSION_STATE.md)
  3. [docs/AI/tasks/TASK-026.md](file:///ssd0/git/pay3/docs/AI/tasks/TASK-026.md)
  4. [docs/AI/ARCHITECTURE.md](file:///ssd0/git/pay3/docs/AI/ARCHITECTURE.md)

