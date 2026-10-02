# Pay3 关键架构决策记录 (Architecture Decisions)

本文档记录 Pay3 项目在演进过程中制定的核心技术与架构决策（ADR）。任何后续的重构或功能扩展不得轻易推翻以下既定决策。

---

## ADR-001: 核心技术栈选型

- **状态**: 已采纳 (Accepted)
- **上下文**: 需要一个具备极致性能、内存安全、原生并发、强一致性保证以及完备 EVM 生态支持的服务端技术栈。
- **决策**:
  - Web 框架: `axum` (基于 Tower/Hyper，模块化中间件，强类型路由)
  - 异步运行时: `tokio`
  - 数据库与 ORM: `sqlx` + PostgreSQL (纯编译期 SQL 检查，利用强类型事务、行级锁、复合约束保证资金绝对一致性)
  - 区块链交互: `alloy` (Rust 生态现代化 EVM/RPC/类型安全互操作库)
  - 本地 KV 引擎: `redb` (纯 Rust 实现的 ACID 嵌入式键值存储，单文件高性能，适合持久化存储海量不可篡改的链上事件日志)
  - 鉴权体系: `jsonwebtoken` (端点级 Scope 校验，支持 JWKS/PEM)
- **影响**: 代码无任何 GC 停顿，内存极度安全，编译期拦截绝大多数 SQL 与类型错误。

---

## ADR-002: 绝对禁止地址复用 (Strict No Address Reuse)

- **状态**: 已采纳 (Accepted)
- **上下文**: ERC20 标准的 `Transfer` 事件只包含 `from`、`to`、`value`，不包含业务订单号或附言（memo）。在传统支付场景中，如果一个收款地址完成支付后被释放给新订单使用，一旦前序用户由于网络拥堵、提币延迟等原因发起了迟到付款，链上数据将无法确定这笔钱是属于老订单还是新订单。
- **决策**:
  - 每一个新订单创建时，从 HD 钱包无限派生一个全新的子地址。
  - 收款地址与该订单在数据库中永久强绑定（`orders.receive_address` 具备唯一性约束）。
  - 迟到转账（超出 `expires_at` 但在 `monitor_until` 内）依然精确关联到原订单，进入人工对账或退款流程，绝不与任何其他订单混淆。
- **影响**: 彻底消除链上付款归属的模糊性，消除了由于地址复用引发的资金纠纷与双重入账安全隐患。

---

## ADR-003: 双存储架构 (PostgreSQL 资金主账 + redb 可重建事件缓存)

- **状态**: 已采纳 (Accepted)
- **上下文**: 热门 ERC20 代币（如 USDT、USDC）每日产生数十万甚至上百万条链上转账记录。绝大多数转账属于其他链上用户，与 Pay3 无关。如果直接将所有原始扫描数据写入 PostgreSQL，会导致数据库连接池耗尽、WAL 爆炸、表膨胀和高昂的运维成本；但如果不落盘原始日志，又会导致 scanner 和 verify 频繁发起 RPC 请求被限流。
- **决策**:
  - **PostgreSQL**: 作为唯一资金权威真相源。只存储归属于 Pay3 的 orders、child_accounts、payment_windows、matched payments、collections、nonces、outbound transactions。
  - **redb**: 作为本地高性能、可重建的原始日志与区块头存储（`transfer_log_store`）。保存连续区块范围内的 raw Transfer logs、block headers、range manifests 和 KV 游标。
- **影响**: 即使 redb 损坏或被彻底删除，也可从链上重新扫链重建，完全不影响已有资金账目；同时极大降低了 PostgreSQL 的写入负载。

---

## ADR-004: HD 分层无限派生与三级 Rollover

- **状态**: 已采纳 (Accepted)
- **上下文**: 每一个订单都需要独立的新地址，单层 BIP44 的 `address_index` 为 31 位无符号整数（硬化前最大约 20 亿）。为防止单层溢出，并保持规范的 HD 钱包层级。
- **决策**:
  - 采用标准派生路径模板：`m/44'/60'/{account_index}'/{change_index}/{address_index}`。
  - 实现三级自动 Rollover 机制：`address_index` 满后向 `change_index` 进位，`change_index` 满后向 `account_index` 进位。
  - 在数据库 `wallet_cursors` 中通过悲观锁（`SELECT FOR UPDATE`）安全原子分配下一个派生坐标。
- **影响**: 保证在高并发下派生地址的确定性与绝对唯一性，支持近乎无限的地址分配容量。

---

## ADR-005: 归集目的地址强制写死为 Treasury

- **状态**: 已采纳 (Accepted)
- **上下文**: 资金归集接口如果允许调用方随意传 `to_address`，一旦调用方凭证泄露或接口被越权调用，攻击者可以发起归集将热钱包资金转移至黑客控制的地址。
- **决策**:
  - `POST /v1/collections` 请求参数中严格禁止接收 `to_address`。
  - 归集目标地址仅从系统配置中读取并强校验数据库 `treasury_addresses` 表外键约束。
  - 数据库层增加 CHECK / Trigger 约束，直接 SQL 插入非 treasury 的归集记录也会被拒绝。
- **影响**: 从接口、业务逻辑到数据库约束三层设防，彻底杜绝归集地址被篡改的风险。

---

## ADR-006: Reorg Epoch 与双阶段确认机制

- **状态**: 已采纳 (Accepted)
- **上下文**: 公链偶发分叉与重组（Reorg）。如果在分叉发生时直接把确认数不足的支付标记为结算成功，可能发生资金损失；如果在分叉后不能自动回滚，会导致假支付留存在主账。
- **决策**:
  - `transfer_log_store` 滚动校验区块哈希。一旦发现区块 Hash 与规范链不一致，立即回滚 KV 游标并递增 `reorg_epoch`。
  - Scanner Worker 轮询检测到 KV 存储的 `reorg_epoch` 大于 PostgreSQL 记录的 `seen_kv_reorg_epoch` 时，自动将 PostgreSQL 游标回退至分叉前区块，并将受影响的支付更新为 `orphaned`，重新计算订单已支付金额。
  - 支付状态先为 `observed`，空闲周期通过 Confirmation Sweep 确认依然处于规范链且达到 `MIN_CONFIRMATIONS` 后，才推进为 `confirmed`。
- **影响**: 系统具备完整的链重组抵御能力，确保订单入账基于绝对安全的最终确定性（Finality）。

---

## ADR-007: Outbound 交易先落盘后广播与同 Nonce 幂等恢复

- **状态**: 已采纳 (Accepted)
- **上下文**: 在发起链上转账（归集）时，最危险的状态是“广播了交易但进程崩溃或网络超时”，此时如果重新发起并使用新 Nonce，会导致同一笔资金被重复归集两次；如果不再重试，则归集卡死。
- **决策**:
  - **先存后发**: 准备归集任务时，先锁定 Nonce，构造 Raw Signed Tx 并落盘至 `outbound_transactions`（状态为 `signed`）。
  - **崩溃优先重播**: Collector 启动或每个 Tick 开始时，优先捞取所有 `signed` 但尚未标记 `broadcast` 的交易，重播同一笔已签名交易，确保链上识别为同一笔 Tx。
  - **同 Nonce Replacement**: 当交易因 Gas 波动在链上卡池超过超时阈值时，保留原 outbound 记录并标记 `replaced`，生成同 Nonce 且 Gas 提高的新 Signed Tx 进行加速替换。
- **影响**: 完美解决归集过程中的网络抖动与宕机问题，杜绝双花与 Nonce 空洞。

---

## ADR-008: MVP 采用 Prefunded 原生代币 Gas 策略

- **状态**: 已采纳 (Accepted)
- **上下文**: 从子地址把 ERC20 代币转到 Treasury 时，子地址本身需要持有母链原生代币（如 ETH/MATIC）来支付转账 Gas。
- **决策**:
  - MVP 阶段采用 `prefunded` 模式：派生子地址前由运营方预先打入小额 Gas，或直接归集已具备 Gas 的地址。Service 层在准备归集时通过 `PrefundedGasChecker` 强制校验原生余额。
  - 复杂的 Gas Station / 代付 Paymaster 机制推迟到后续版本。
- **影响**: 降低系统初始复杂度，确保核心归集状态机稳定可测。

---

## ADR-009: 扫链容量门禁 (Capacity Probe) 与多 RPC Provider 容灾

- **状态**: 已采纳 (Accepted)
- **上下文**: 热门代币的 Transfer 事件极其密集，若 RPC 节点发生波动或返回超大 batch，会导致扫链堵塞或内存 OOM。
- **决策**:
  - 实现 `RpcProviderManager`，支持配置多个 RPC Provider，自动健康探测、Chain ID 校验、高度比对与故障转移（Failover）。
  - 实现 `CapacityProbe` 门禁机制：在批次拉取前探测日志密度，如果单块日志超出安全阈值，自动缩小批次或进入 Fail-Closed 保护并报警，防止压垮服务。
- **影响**: 保障了扫链服务在公链极端波动和节点抖动下的极高可用性与健壮性。

---

## ADR-010: 生产环境零明文私钥与端点级 Scope 隔离

- **状态**: 已采纳 (Accepted)
- **上下文**: 任何在生产配置文件或环境变量中存储明文助记词/私钥的行为都会带来巨大资金被盗风险；同时资金敏感接口必须防止越权。
- **决策**:
  - 生产 profile 强制校验：禁止 `SIGNER_MODE=fake` 或本地明文私钥，必须通过 `SignerProvider` 接入外部远程签名服务或 KMS/HSM。
  - 生产环境禁止使用对称密钥 `HS256`，强制使用非对称公钥 `RS256` 或 `EdDSA`（本地 JWKS/PEM 或远程 JWKS）。
  - API 强制端点级 Scope 校验：`orders:create`、`orders:read`、`orders:verify`、`collections:create`、`collections:read`，归集权限绝不授予普通调用方。
- **影响**: 达到生产级企业资金安全隔离与内控标准。

---

## ADR-011: 极简生产可用与单实例单 Token 架构 (Stateless Direct Scanner)

- **状态**: 已采纳 (Accepted)
- **上下文**: 原架构引入本地 `redb` KVDB 作为原始日志缓存，意图分流热门 Token 的海量 Transfer 事件。但实际生产中，99.9% 的链上转账与本平台无关，落盘到本地带来了单写者文件锁竞争、磁盘膨胀需要额外清理、双存储状态机对齐等严重运维包袱，妨碍了容器无状态水平部署。
- **决策**:
  - **单实例单一 Token**: 每个服务实例严格锁定单一 `CHAIN_ID` 与 `TOKEN_ADDRESS`，多币种采用轻量多容器部署，天然物理隔离。
  - **移除本地 KVDB 依赖**: 废弃 `transfer_log_store` 与 `redb`。扫链直接从 RPC 批量拉取日志，内存中比对本平台活跃订单收款地址池，未命中转账直接丢弃，命中的转账单事务写入 PostgreSQL 并推进区块游标。
  - **完全无状态部署**: 服务容器不再挂载持久化磁盘卷，支持秒级重启与任意节点调度漂移。
- **影响**: 代码与存储拓扑大幅精简，吞吐性能提升，容器化运维难度降为零。

---

## ADR-012: 多 RPC 原子轮询负载均衡 (Round-Robin) 与智能 CD 冷却熔断

- **状态**: 已采纳 (Accepted)
- **上下文**: 链上节点存在速率限制（HTTP 429 Too Many Requests）、瞬时网络抖动和单节点宕机。原有简单的按顺序尝试（Failover）会导致流量全部倾斜在首个可用节点，容易触发限流。
- **决策**:
  - **Round-Robin 负载均衡**: 使用原子计数器在当前健康节点池中轮询分发 RPC 请求，均匀分摊并发量与额度消耗。
  - **分级 CD 冷却隔离 (Circuit Breaker Cooldown)**: 当某个节点触发 429、超时或网络报错时，立即将其置入 CD 冷却黑名单（初次报错 CD 10s，连续报错阶梯上升至 30s/60s），期间其他请求不再打向该节点；CD 到期后自动进入半开试探恢复。
- **影响**: 消除单节点被薅崩的风险，最大化利用多 RPC 资源，确保高并发下的极致可用性。

