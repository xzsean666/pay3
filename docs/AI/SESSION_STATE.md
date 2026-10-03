# Pay3 当前会话状态 (Session State)

更新时间：2026-10-02

---

## 1. 当前上下文

- **当前 Goal**: 链上 RPC 动态探查器与自适应代币元数据提取（支持通过静态只读 eth_call 探查任意代币的 EIP-3009/MetaTx/Permit 接口及元数据）
- **当前 Task**: [TASK-031: 链上 RPC 动态探查器与自适应代币元数据提取](file:///ssd0/git/pay3/docs/AI/tasks/TASK-031.md)
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
8. **完成 TASK-020：Prometheus 告警规则 Dry-Run 验证与指标补齐**:
   - 在 `src/health.rs` 中为 `MetricsRecorder` 补齐 `rpc_errors_total`、`signer_errors_total` 与 `payment_events_total` 指标导出及单测。
   - 编写 `deploy/prometheus/pay3-alerts.example.yml`，包含 8 条生产告警规则（Readiness、Dependency、Worker Consecutive Failures、Scanner High Lag、Signer Failure、RPC High Error Rate 等）。
   - 编写 `deploy/prometheus/rules_test.yml`，使用 `promtool test rules` 完成时序数据 Dry-Run 校验全部 SUCCESS。
   - 创建 `deploy/prometheus/grafana_dashboard.json` 导出大盘。
9. **完成 TASK-021：深度 Reorg 与孤块付款真实 DB 回归**:
   - 编写 `tests/reorg_integration.rs` 专项测试，构建分叉后孤块付款场景。
   - 验证分叉发生时：`payments` 孤块被标记为 `chain_status = 'orphaned'`，游标回退至 `last_reorg_from - 1`，订单金额实时重算，订单状态从 `paid` 安全倒退回 `pending` 或 `partial`。
   - 验证孤块金额永远无法参与确认数与归集计算的不变量。
10. **完成 TASK-022：归集崩溃恢复与 Finality/Reorg 深度回归**:
    - 编写 `tests/collector_recovery_integration.rs` 专项测试，模拟签名后落盘未广播崩溃、同 Nonce Replacement 唯一约束竞争与 Receipt 确认推进。
    - 验证进程崩溃恢复优先重播既有 Signed Tx 且不分配新 Nonce。
    - 验证 PostgreSQL Partial Unique Index `outbound_active_nonce_idx` 在同 Nonce 替换生命周期中全程阻止重复活跃记录。
11. **完成 TASK-023：灾难恢复 Runbook 实操演练与记录**:
    - 全面更新 `docs/RUNBOOK.md`，彻底移除已废弃的 redb 运维条目，对齐无状态 Direct Scanner 架构与多 RPC 故障排查手册。
    - 编制 5 份结构化演练执行报告（`docs/drill_reports/DRILL-001 ~ DRILL-005`）：包含 PostgreSQL PITR 恢复演练、无状态 Scanner 游标回退演练、多 RPC CD 隔离与故障转移演练、Remote Signer 宕机与鉴权加固演练、卡死归集同 Nonce 替换演练。
    - 编写自动化演练脚本 `scripts/run_drills.sh` 并实操执行通过。
12. **完成 TASK-024：生产上线审计门禁自动化验证脚本**:
    - 在 `src/main.rs` 中支持 `--verify-readiness` / `--check-config` 命令行选项，直接利用 Rust 内置 `AppConfig` profile 规则做确定性静态门禁放行校验。
    - 提供模板工件 `.env.production.example`，规范生产级单链单币安全变量。
    - 交付综合预检门禁脚本 `scripts/verify_production_readiness.sh`，覆盖 7 大门禁检验（敏感机密扫描、生产不变量审计、Rust AppConfig 验证、无状态架构合规、依赖健康探测、Prometheus 告警规则语法及时序单测校验、灾备工件审计）。
    - 提供 `--test-violations` 自动化负例回归测试，8/8 关键违规项（如明文助记词、弱 JWT、单 RPC、START_BLOCK=0、地址冲突等）100% 拦截。
    - 更新 `docs/PRODUCTION_READINESS.md` 签署通过结论并给出上线前执行步骤。
13. **完成 TASK-027：参考 evm-call 优化多 RPC 连接池（并发探测、故障快速漂移与按需法定多数）**:
    - 在 `src/chain/rpc.rs` 中为 `RpcProviderManager::validate_chain_ids` 接入 `tokio::task::JoinSet`，实现多个 RPC 节点并发启动探测，彻底消除单节点慢响应对系统初始化的累加阻塞。
    - 将 `request_blocks` 优化为“按需法定多数（Bounded Quorum）+ 快速漂移（Fast Failover）”：达到 `min_provider_count` 且区块哈希一致后即刻返回，不再对池内全部公开节点冗余全量扫遍。
    - 节点遭遇 429、5xx、超时时自动触发分级 CD 隔离并快速 failover 至下一个健康候选节点。
    - 增加两个专项单元测试，验证大节点池下的受限法定多数与故障自动隔离。
14. **完成 TASK-028：全面审计缺陷修复与生产架构优化**:
    - **Finding 1 (Medium - EIP-1559 替换交易费率底线)**: 增加数据库迁移 `20261003000100_outbound_gas_fees.sql`，持久化记录 `gas_limit`、`max_fee_per_gas`、`max_priority_fee_per_gas`；在 `replace_collection_job` 中计算 $\max(1.1 \times \text{previous\_fees}, \text{current\_network\_fees})$，严格避免因网络 Gas 下降导致节点拒绝替换交易；
    - **Finding 2 (Low/Medium - 租户隔离)**: 在 `POST /v1/orders/{id}/verify` 端点与服务层校验调用者的 `owner_sub`，禁止跨租户非法核验他人物流与订单信息，非法跨商户请求安全返回 404；
    - **Finding 3 (Low - 防护)**: 在 `parse_topic_address` 增加 EVM ABI 高位 12 字节（24 字符）全零校验，杜绝恶意伪造非对齐 Topic 地址；
    - **Finding 4 (Informational - 架构整洁度)**: 清理移除了 `health.rs`、`api/mod.rs`、`workers/scanner.rs` 中遗留的 KVDB 相关死代码、指标与未使用的错误枚举变体。
15. **完成 TASK-029：全功能可插拔免 Gas (Permit / Meta-Tx) 资金归集架构**:
    - **EIP-712 与 ABI 编码模块 (`src/domain/permit.rs`)**:
      - 抽象并实现 `CollectionMethod` (`Standard`, `Eip3009`, `PolygonMetaTx`, `Eip2612`)。
      - 实现标准 EIP-712 Domain Separator、Polygon PoS 特殊 Salt Domain Separator、EIP-3009 `TransferWithAuthorization`、Polygon `MetaTransaction`、EIP-2612 `Permit` 的 Struct Hash 及标准 EVM ABI Calldata 编码器。
    - **统一签名抽象与适配器扩展 (`src/signer/mod.rs`, `local.rs`, `external.rs`, `reference_signer.py`)**:
      - `SignerProvider` 增加 `sign_digest(&self, key_ref, digest) -> Result<TypedSignature, WalletError>`。
      - `LocalMnemonicSigner`、`DeterministicFakeSigner`、`RemoteHttpSigner` 及 Python 外部参考签名服务均已实现 digest 签名，覆盖 19 个签名契约测试。
    - **数据库安全约束与白名单 (`20261003000200_collection_relayers.sql`)**:
      - 新增 `relayer_addresses (chain_id, relayer_address)` 表。
      - 增强 `enforce_collection_outbound_tx()` 触发器：白名单放行注册的 Relayer 作为 `from_address`（代付主网原生 Gas），非白名单地址抛出 23514 约束错误。
    - **归集策略与服务编排 (`src/services/collections.rs`)**:
      - 实现 `CollectionStrategyConfig`。免 Gas 归集时由子地址离线签名，Payer 自动切换为 `relayer.address`；
      - `gas_checker.ensure_prefunded_gas` 校验 Relayer 钱包原生 Gas 余额，子收款地址 0 原生 Gas 也可顺利归集；
      - `account_nonces` 分配锁定 Relayer 的 Nonce；`outbound_transactions` 严格落盘 `from_address = relayer.address`，`to_address = treasury`；
      - 幂等同 Nonce Replacement 支持免 Gas 重构与手续费上浮重新广播。
    - **运行时装配 (`src/runtime.rs`, `src/config.rs`)**:
      - 环境变量配置解析 `COLLECTION_METHOD`、`COLLECTION_RELAYER_KEY_REF`、`COLLECTION_RELAYER_DERIVATION_PATH`、`COLLECTION_TOKEN_NAME`、`COLLECTION_TOKEN_VERSION`。
      - 启动时自动派生 Relayer 地址并注册进 DB `relayer_addresses` 表。
16. **完成 TASK-030：智能自动推导与选择最优归集方案 (`COLLECTION_METHOD=auto`)**:
    - **自适应策略解析器 (`src/domain/permit.rs`)**:
      - `CollectionMethod` 增加 `Auto` 变体并作为默认配置；
      - 内置覆盖以太坊、Polygon、Arbitrum、Optimism、Base、Avalanche 等主流 EVM 链的稳定币预置知识库（Preset Matrix）；
      - 实现 `resolve_optimal_collection_strategy(...)` 决策算法：优先 EIP-3009 > PolygonMetaTx > EIP-2612 > Standard；
      - 未配置 Relayer 或代币不支持 permit 时平滑安全降级为 `Standard`；
      - 支持用户自定义 `token_name` 与 `token_version` 参数覆盖。
    - **配置与运行时集成 (`src/config.rs`, `src/runtime.rs`, `src/domain/address.rs`)**:
      - `DEFAULT_COLLECTION_METHOD` 设置为 `CollectionMethod::Auto`；
      - `EvmAddress` 增加 `parse_hex` 解析辅助；
      - 启动时自动解析决策并打印结构化 trace 日志；
    - **测试回归**:
      - 增加 `resolve_optimal_strategy_presets_and_fallbacks` 单元测试，覆盖 11 种典型分支；
      - 全量 204+ 测试与预检门禁脚本 100% 验证通过。
17. **完成 TASK-031：链上 RPC 动态探查器与自适应代币元数据提取**:
    - **合约探查器与 ABI 解码 (`src/domain/permit.rs`)**:
      - 规范定义标准选择器：`ERC20_NAME_SELECTOR` (`0x06fdde03`)、`ERC20_VERSION_SELECTOR` (`0x54fd4d50`)、`EIP3009_AUTHORIZATION_STATE_SELECTOR` (`0xe94a0102`)、`POLYGON_GET_NONCE_SELECTOR` (`0x2d0335ab`)、`EIP2612_DOMAIN_SEPARATOR_SELECTOR` (`0x3644e515`)、`EIP2612_NONCES_SELECTOR` (`0x7ecebe00`)；
      - 实现通用 `decode_abi_string`，支持动态 ABI 字符串（偏移量+长度+内容）与定长 `bytes32`；
      - 抽象并导出 `ContractCallClient` 异步 trait 与 `OnChainTokenCapabilities`；
      - 实现 `probe_on_chain_token_capabilities`：通过静态只读 `eth_call` 自动探测合约接口，依次探测 EIP-3009 -> Polygon MetaTx -> EIP-2612，并动态读取真实 `name()` 和 `version()`；
      - 实现 `resolve_optimal_collection_strategy_with_probe`：主流代币走快速预置匹配（0 RPC 开销），未登记未知代币自动执行链上动态探查并绑定最优免 Gas 归集方案，探查失败或不支持时平滑安全降级为 `Standard`；
    - **RPC 契约与打桩测试扩展 (`src/chain/rpc.rs`)**:
      - `RpcRangeSource` 实现 `call_contract` 并实现 `ContractCallClient`；
      - `FakeRpcProvider` 增加 `contract_calls` 与 `with_contract_call` 支持模拟合约只读调用；
    - **服务装配 (`src/runtime.rs`)**:
      - `collection_strategy_config` 传入 `rpc_source`，在服务启动时自动探测未知代币能力与元数据并输出审计日志；
    - **测试验证**:
      - 补齐 ABI 解码、Canonical Selector、EIP-3009/Polygon MetaTx/EIP-2612 探测与未知链降级的全部单元测试与集成测试；全量 230 项测试与生产预检门禁脚本 100% 通过。

---

## 3. 修改、创建与移除的文件清单

### 创建文件
- `docs/AI/tasks/TASK-028.md`
- `docs/AI/tasks/TASK-029.md`
- `docs/AI/tasks/TASK-030.md`
- `docs/AI/tasks/TASK-031.md`
- `src/db/migrations/20261003000100_outbound_gas_fees.sql`
- `src/db/migrations/20261003000200_collection_relayers.sql`
- `src/domain/permit.rs`

### 修改文件
- `deploy/signer/reference_signer.py`
- `docs/AI/TASK_INDEX.md`
- `docs/AI/SESSION_STATE.md`
- `src/chain/rpc.rs`
- `src/config.rs`
- `src/db/migrations.rs`
- `src/domain/address.rs`
- `src/domain/mod.rs`
- `src/runtime.rs`
- `src/services/collections.rs`
- `src/signer/external.rs`
- `src/signer/local.rs`
- `src/signer/mod.rs`
- `tests/anvil_e2e.rs`
- `tests/migration_contract.rs`
- `tests/real_chain_e2e.rs`
- `tests/signer_contract.rs`
- `tests/support/anvil.rs`

---

## 4. 已运行的验证命令及结果

- `cargo check --all-targets`: **通过**。零错误，零警告。
- `cargo test`: **通过**。全量 230 个测试（211 lib tests + 19 integration tests）全部绿色通过。
- `bash scripts/verify_production_readiness.sh --env-file .env.production.example`: **通过**。21 checks passed, 0 failed.

---

## 5. 未解决问题与剩余工作

- **无未解决问题**。
- `docs/AI/TASK_INDEX.md` 中所有 31 个任务卡（TASK-001 ~ TASK-031）全部处于 `DONE` 状态。
- 系统已实现完全智能自适应链上探查与免 Gas 归集（USDC EIP-3009 / Polygon USDT MetaTx / L2 USDT EIP-2612 / 自动链上探查 / Standard 回退）。

---

## 6. 下一步任务与读取入口

- **项目状态**: **100% 生产就绪、审计加固、免 Gas 归集、智能知识库与链上动态探查落地 (Production Ready, Audited & On-chain Probed Auto Optimal Sweep)**
- **读取入口**:
  1. [docs/PRODUCTION_READINESS.md](file:///ssd0/git/pay3/docs/PRODUCTION_READINESS.md) (生产验收结论与上线命令)
  2. [docs/RUNBOOK.md](file:///ssd0/git/pay3/docs/RUNBOOK.md) (故障排查与运维指南)
  3. [docs/DEPLOYMENT.md](file:///ssd0/git/pay3/docs/DEPLOYMENT.md) (生产部署架构)
  4. [docs/AI/ARCHITECTURE.md](file:///ssd0/git/pay3/docs/AI/ARCHITECTURE.md) (系统架构)
  5. [docs/AI/TASK_INDEX.md](file:///ssd0/git/pay3/docs/AI/TASK_INDEX.md) (全量任务卡索引)
  6. [docs/AI/tasks/TASK-031.md](file:///ssd0/git/pay3/docs/AI/tasks/TASK-031.md) (链上动态探查器任务卡)








