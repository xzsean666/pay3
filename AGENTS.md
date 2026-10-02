# AGENTS.md - Pay3 工程开发代理规则 (Agent Working Rules)

本文档是 AI 代理在 `pay3` 仓库工作的事实来源和最高指导规则之一。所有在此仓库工作的 AI 代理必须严格遵守以下规则。

---

## 1. 事实来源与目录规范

所有工作必须依据规范文档，不可凭空假设或跳过文档：
- **开发代理总指导提示词**: [docs/AI_AGENT_PROMPT.md](file:///ssd0/git/pay3/docs/AI_AGENT_PROMPT.md)
- **项目总目标**: [docs/AI/GOAL.md](file:///ssd0/git/pay3/docs/AI/GOAL.md)
- **任务索引表**: [docs/AI/TASK_INDEX.md](file:///ssd0/git/pay3/docs/AI/TASK_INDEX.md)
- **当前会话状态**: [docs/AI/SESSION_STATE.md](file:///ssd0/git/pay3/docs/AI/SESSION_STATE.md)
- **细粒度任务卡**: `docs/AI/tasks/TASK-xxx.md`
- **系统架构说明**: [docs/AI/ARCHITECTURE.md](file:///ssd0/git/pay3/docs/AI/ARCHITECTURE.md)
- **核心架构决策**: [docs/AI/DECISIONS.md](file:///ssd0/git/pay3/docs/AI/DECISIONS.md)
- **生产部署说明**: [docs/DEPLOYMENT.md](file:///ssd0/git/pay3/docs/DEPLOYMENT.md)
- **生产验收与审计**: [docs/PRODUCTION_READINESS.md](file:///ssd0/git/pay3/docs/PRODUCTION_READINESS.md)
- **运维与故障处理**: [docs/RUNBOOK.md](file:///ssd0/git/pay3/docs/RUNBOOK.md)

---

## 2. GitHub 账户与认证路由规范 (强制)

本机器配置了多个 GitHub 账号：
- **当前仓库路径**: `/ssd0/git/pay3` (属于 `/ssd0/git` 目录体系)
- **指定 GitHub 账号**: `xzsean666`
- **执行规范**:
  - 在本仓库进行任何 GitHub CLI (`gh`) 命令（如 `gh pr`、`gh repo` 等）或需要认证的 Git 操作前，必须确保 `gh` 活跃账号为 `xzsean666`。
  - 若当前账号不是 `xzsean666`，必须在执行操作前切换：
    ```bash
    gh auth switch --user xzsean666
    ```

---

## 3. 项目定位与核心不变量

Pay3 是一个由 Rust 编写的高可靠 ERC20 token 收款网关。
MVP 严格限制为：单一业务账号、单一 EVM 链、单一 ERC20 token，通过母账号按 HD derivation segment 无限派生子地址收款，并提供创建订单、付款验证、订单状态查询和 token 归集的完整闭环。

### 核心不变量与工程准则 (绝对禁止违背)

1. **单实例单一 Token 架构 (One Instance, One Token)**:
   - 单个服务实例固定只配置一个 `CHAIN_ID` 与单一 `TOKEN_ADDRESS`。
   - 杜绝多币种交叉污染与多游标管理开销，跨链或跨 Token 采用轻量多实例容器部署。
2. **绝对禁止地址复用 (Strict No Address Reuse)**:
   - 每个订单必须派生一个全新的子收款地址。
   - 一个收款地址永久唯一绑定原订单，数据库全局唯一约束（`orders.receive_address` UNIQUE）。
   - 迟到/窗口外付款（late/outside_window）必须留在原订单进入对账，绝不分配给新订单。
3. **PostgreSQL 是唯一真相源，扫链采用无状态过滤 (Stateless Direct Scanner)**:
   - `orders`、`child_accounts`、`payment_windows`、`payments`、`chain_cursors`、`collections`、`account_nonces`、`outbound_transactions`、`audit_events` 均在 PostgreSQL 强事务一致性保护下。
   - 链上 RPC 是最权威的只读归档；应用层采用纯无状态设计，无需任何本地 KVDB 文件存储，未命中本平台的转账日志直接在内存中丢弃，杜绝磁盘 IO 浪费与文件锁开销。
4. **多 RPC 负载均衡与智能 CD 冷却机制 (Multi-RPC Load Balancing & Cooldown)**:
   - 系统支持接入多个 RPC 节点，请求采用 Round-Robin 原子轮询负载均衡，平摊调用配额。
   - 当节点遇到 429 (Rate Limit)、超时或网络故障时，必须自动进入阶梯式 CD 冷却隔离，流量自动切换到健康节点；CD 到期后自动半开恢复。
5. **归集目标地址固定为 Treasury**:
   - `POST /v1/collections` 绝对不允许外部调用方指定任意 `to_address`。目标地址在数据库与服务层强制校验为配置的 `TREASURY_ADDRESS`。
6. **Outbound 交易崩溃恢复与同 Nonce 幂等**:
   - 广播链上交易前必须落盘 `chain_id/from_address/nonce/signed_tx/tx_hash`。
   - 崩溃恢复必须优先重播同一已签名交易，禁止重发新 nonce 造成资金重出。
   - dropped/stuck 交易 replacement 必须保留原 outbound 记录并标记 `replaced`，同 nonce 提高 gas，不得修改转账目标或金额。
7. **生产环境安全边界**:
   - production profile 严格禁止明文助记词/私钥、禁止 local/fake signer，必须接入外部 Signer/KMS/HSM。
   - 生产环境禁止使用 `HS256` 明文 JWT Secret，必须使用 RS256/EdDSA (JWKS/PEM)。
   - 日志与错误报告严禁泄漏 mnemonic、private key、JWT、数据库密码。

---

## 4. 工作原则与 Git 提交规范

1. **一次只做一个 Task**:
   - 严格按照 [docs/AI/TASK_INDEX.md](file:///ssd0/git/pay3/docs/AI/TASK_INDEX.md) 和当前任务卡推进。
   - 不修改与当前任务无关的文件，不进行大规模全局重构。
2. **修改前输出计划**:
   - 严格按照 [docs/AI_AGENT_PROMPT.md](file:///ssd0/git/pay3/docs/AI_AGENT_PROMPT.md) 第 7 节格式输出计划，确认边界后再做改动。
3. **验证驱动**:
   - 没有实际运行过的测试不得声称通过。
   - 修改完成后必须运行对应测试和静态检查（`cargo clippy`、`cargo fmt`、`cargo test`）。
4. **Git 提交规范**:
   - 每完成一个独立任务或 bugfix，先跑通测试，再创建本地 git commit（默认不 `git push`）。
   - Commit message 遵循 Conventional Commits（例如 `feat(scanner): ...`、`fix(db): ...`、`docs(ai): ...`）。
   - Commit 前必须更新 [docs/AI/SESSION_STATE.md](file:///ssd0/git/pay3/docs/AI/SESSION_STATE.md)。
5. **严禁破坏性操作**:
   - 严禁 `git reset --hard`、`git checkout -f` 或未授权删除用户未提交修改。
