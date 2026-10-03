# Pay3 任务索引表 (Task Index)

本文档是 Pay3 项目所有细粒度任务的中央索引。每个任务对应 `docs/AI/tasks/TASK-xxx.md`，记录目标、范围、依赖、验收标准与状态。

---

## 1. 任务流转规则

状态流转严格遵循：
```text
TODO -> IN_PROGRESS -> REVIEW -> DONE
                    \-> BLOCKED
```

- **一次只执行一个任务**: 当前 session 只能激活一个处于 `IN_PROGRESS` 的任务。
- **依赖优先**: 只有前置依赖全部为 `DONE` 的 `TODO` 任务才允许启动。
- **完成准入**: 只有验收标准全部满足、验证命令实际运行通过且文档更新后，才可标记为 `DONE`。

---

## 2. 任务总览矩阵

### 已完成基石任务 (Phase 1 ~ Phase 5: MVP 核心功能实现)

| 任务编号 | 任务名称 | 核心范围 | 依赖项 | 状态 | 关联任务卡 |
| :--- | :--- | :--- | :--- | :--- | :--- |
| **TASK-001** | Rust 基础骨架、配置与健康检查 | Cargo 配置、Typed Config、`/healthz`、`/readyz`、`/metrics`、统一错误模型 | 无 | `DONE` | [TASK-001.md](file:///ssd0/git/pay3/docs/AI/tasks/TASK-001.md) |
| **TASK-002** | Domain 核心值对象与状态机 | Amount、Address、Order/Payment/Collection 状态机、Rollover 计算 | TASK-001 | `DONE` | [TASK-002.md](file:///ssd0/git/pay3/docs/AI/tasks/TASK-002.md) |
| **TASK-003** | JWT 鉴权与端点级 Scope 门禁 | JWT Claims/Verifier、端点级 Scope 校验、生产 Profile 密钥门禁 | TASK-001, TASK-002 | `DONE` | [TASK-003.md](file:///ssd0/git/pay3/docs/AI/tasks/TASK-003.md) |
| **TASK-004** | PostgreSQL 数据库迁移与约束 | SQLx 迁移、唯一约束、外键约束、CHECK 校验、初始种子数据 | TASK-001, TASK-002 | `DONE` | [TASK-004.md](file:///ssd0/git/pay3/docs/AI/tasks/TASK-004.md) |
| **TASK-005** | 数据库仓储层 (Repositories) 实现 | Order/Payment/Collection/Outbound/Audit Repositories 及事务控制 | TASK-004 | `DONE` | [TASK-005.md](file:///ssd0/git/pay3/docs/AI/tasks/TASK-005.md) |
| **TASK-006** | HD Wallet 地址无限派生与 Rollover | BIP44 模板、自动进位 Rollover、Deterministic Fake Deriver | TASK-002 | `DONE` | [TASK-006.md](file:///ssd0/git/pay3/docs/AI/tasks/TASK-006.md) |
| **TASK-007** | 签名契约与 Remote HTTP 适配器 | SignerProvider Trait、Fake Signer、Remote HTTP Signer 适配器 | TASK-002 | `DONE` | [TASK-007.md](file:///ssd0/git/pay3/docs/AI/tasks/TASK-007.md) |
| **TASK-008** | 链客户端契约、RPC 管理器与容量门禁 | JsonRpcProvider、RPC 故障转移、TransferLogSource、Capacity Probe | TASK-002 | `DONE` | [TASK-008.md](file:///ssd0/git/pay3/docs/AI/tasks/TASK-008.md) |
| **TASK-009** | 独立 Transfer 日志 KV 存储 (redb) | 原始日志/区块头落盘、分页读取、Reorg Rewind、Ingestor 循环 | TASK-008 | `DONE` | [TASK-009.md](file:///ssd0/git/pay3/docs/AI/tasks/TASK-009.md) |
| **TASK-010** | 支付窗口查找与纯支付匹配服务 | PaymentWindowLookup、时效分类匹配、多块与分页匹配计算 | TASK-002, TASK-009 | `DONE` | [TASK-010.md](file:///ssd0/git/pay3/docs/AI/tasks/TASK-010.md) |
| **TASK-011** | 订单创建与查询服务及 API 路由 | OrderService、幂等 Hash、地址分配、API Routes (`/v1/orders`) | TASK-003, TASK-005, TASK-006 | `DONE` | [TASK-011.md](file:///ssd0/git/pay3/docs/AI/tasks/TASK-011.md) |
| **TASK-012** | 手动付款验证服务与 API 路由 | ManualOrderVerifyService、KV 范围读取、重算入库、验证 API | TASK-005, TASK-009, TASK-010 | `DONE` | [TASK-012.md](file:///ssd0/git/pay3/docs/AI/tasks/TASK-012.md) |
| **TASK-013** | 异步扫链 Worker 与确认数推进 | Scanner Loop、Lease/CAS 锁、KV Reorg Epoch 联动、Confirmation Sweep | TASK-005, TASK-009, TASK-010 | `DONE` | [TASK-013.md](file:///ssd0/git/pay3/docs/AI/tasks/TASK-013.md) |
| **TASK-014** | 资金归集服务与 Outbound 准备 | CollectionService、Treasury 约束、Prefunded Gas 校验、签名持久化 | TASK-005, TASK-007, TASK-008 | `DONE` | [TASK-014.md](file:///ssd0/git/pay3/docs/AI/tasks/TASK-014.md) |
| **TASK-015** | 归集 Worker、崩溃重播与 Replacement | Collector Loop、崩溃优先重播、收据轮询、同 Nonce Replacement | TASK-005, TASK-008, TASK-014 | `DONE` | [TASK-015.md](file:///ssd0/git/pay3/docs/AI/tasks/TASK-015.md) |
| **TASK-016** | 完整运行时装配与生命周期管理 | Runtime Composition、启动依赖探测、后台 Worker 调度、健康刷新 | TASK-001 ~ TASK-015 | `DONE` | [TASK-016.md](file:///ssd0/git/pay3/docs/AI/tasks/TASK-016.md) |
| **TASK-017** | 端到端与集成测试套件 | Anvil E2E 测试、数据库集成测试、契约测试、黑盒验收测试框架 | TASK-016 | `DONE` | [TASK-017.md](file:///ssd0/git/pay3/docs/AI/tasks/TASK-017.md) |

---

### 待执行生产就绪与硬化任务 (Phase 6: Production Hardening Backlog)

当前所有功能基石已完成，下一步需按依赖顺序推进生产验收项：

| 任务编号 | 任务名称 | 核心范围 | 依赖项 | 状态 | 关联任务卡 |
| :--- | :--- | :--- | :--- | :--- | :--- |
| **TASK-025** | 多 RPC 负载均衡与智能 CD 冷却升级 | 原子 Round-Robin 分发、429/超时分级 CD 隔离、健康自动探测 | TASK-008 | `DONE` | [TASK-025.md](file:///ssd0/git/pay3/docs/AI/tasks/TASK-025.md) |
| **TASK-026** | 移除 redb 改造为 Direct 无状态扫描器 | 废弃 KVDB、直接 RPC 拉取 + 内存地址过滤 + PostgreSQL 事务写入 | TASK-025 | `DONE` | [TASK-026.md](file:///ssd0/git/pay3/docs/AI/tasks/TASK-026.md) |
| **TASK-018** | 远程 JWKS 动态拉取与密钥轮换 | 实现 `JWT_JWKS_URL` 异步后台刷新、缓存与故障降级 | TASK-003, TASK-016 | `DONE` | [TASK-018.md](file:///ssd0/git/pay3/docs/AI/tasks/TASK-018.md) |
| **TASK-019** | 生产级远程 Signer 部署与网络隔离契约 | Remote Signer 服务鉴权、mTLS/HMAC 签名校验、独立部署配置 | TASK-007, TASK-016 | `DONE` | [TASK-019.md](file:///ssd0/git/pay3/docs/AI/tasks/TASK-019.md) |
| **TASK-020** | Prometheus 告警规则 Dry-Run 与指标补全 | 核心告警规则配置验证、指标模拟上报、Grafana 仪表盘配置 | TASK-016 | `DONE` | [TASK-020.md](file:///ssd0/git/pay3/docs/AI/tasks/TASK-020.md) |
| **TASK-021** | 深度 Reorg 与孤块付款真实 DB 回归 | 编写多块深度分叉场景下 Scanner 游标回退与孤块重算的集成测试 | TASK-013, TASK-017 | `DONE` | [TASK-021.md](file:///ssd0/git/pay3/docs/AI/tasks/TASK-021.md) |
| **TASK-022** | 归集崩溃恢复与 Finality/Reorg 深度回归 | 模拟广播前/后强行中断进程、链重组下的归集状态机完整回归 | TASK-015, TASK-017 | `DONE` | [TASK-022.md](file:///ssd0/git/pay3/docs/AI/tasks/TASK-022.md) |
| **TASK-023** | 灾难恢复 Runbook 实操演练与记录 | DB PITR、KV 重建、RPC 切换、Signer 故障、卡死归集实操演练 | TASK-016, TASK-017 | `DONE` | [TASK-023.md](file:///ssd0/git/pay3/docs/AI/tasks/TASK-023.md) |
| **TASK-024** | 生产上线审计门禁自动化验证脚本 | 编写一键 Pre-flight Check 脚本，自动检查所有生产准入禁项 | TASK-018 ~ TASK-023 | `DONE` | [TASK-024.md](file:///ssd0/git/pay3/docs/AI/tasks/TASK-024.md) |
| **TASK-027** | 参考 evm-call 优化多 RPC 连接池 | 并发启动探测、故障快速漂移与按需法定多数，适配公开 RPC 池 | TASK-025, TASK-026 | `DONE` | [TASK-027.md](file:///ssd0/git/pay3/docs/AI/tasks/TASK-027.md) |
| **TASK-028** | 全面审计缺陷修复与生产架构优化 | 修复卡死归集 Gas 底线计算、核验接口租户隔离、Topic 零填充校验及清理 KVDB 遗留 | TASK-027 | `DONE` | [TASK-028.md](file:///ssd0/git/pay3/docs/AI/tasks/TASK-028.md) |
| **TASK-029** | 全功能可插拔免 Gas (Permit / Meta-Tx) 资金归集架构 | 支持 EIP-3009 (原生 USDC)、Polygon MetaTx (USDT) 与 EIP-2612，Relayer 独立代付原生 Gas | TASK-028 | `DONE` | [TASK-029.md](file:///ssd0/git/pay3/docs/AI/tasks/TASK-029.md) |
| **TASK-030** | 智能自动推导与选择最优归集方案 | 支持 `COLLECTION_METHOD=auto`，基于链与代币知识库自适应匹配 EIP-3009/MetaTx/Permit/Standard | TASK-029 | `DONE` | [TASK-030.md](file:///ssd0/git/pay3/docs/AI/tasks/TASK-030.md) |
