# Pay3 Module Plan

## 目标

本项目必须按模块逐步实现、测试和联调。每个模块都要满足：

- 高内聚：模块内部职责完整，能独立解释、独立测试。
- 低实现耦合：模块之间不能互相穿透实现细节。
- 强且稳定的接口契约：模块之间通过 trait、DTO、repository 或 service contract 连接。
- 可替换依赖：链、signer、DB、cache 都必须能用 fake/mock 做单测。
- 可组合联调：每完成一个模块，就有明确的上层组合测试。
- 可观测性同步交付：模块产生 worker、RPC、DB 或资金状态变化时，必须同时交付 metrics、结构化日志、审计事件和 readyz 检查。

如果后续说“做某个模块”，AI 只允许改该模块和必要的测试/契约文件，不做跨模块重构。默认每次只做一个模块；只有用户明确要求时才按一个 phase 推进，并逐项声明 phase 内模块边界。

## 模块依赖图

```text
api
  -> services
      -> domain
      -> db repositories
      -> chain client traits
      -> signer traits
      -> outbound tx
      -> cache traits

workers
  -> services
  -> db repositories
  -> chain client traits
  -> transfer_log_store

db repositories
  -> domain

transfer_log_store
  -> chain client traits
  -> cache/KVDB
  -> domain value types only

chain/signer/outbound/cache
  -> domain value types only
```

禁止依赖：

- `api` 不能直接写 SQL。
- `db` 不能调用 chain 或 signer。
- `chain` 不能知道 HTTP DTO。
- `services/payments` 不能直接 `eth_getLogs`；它只能依赖 `transfer_log_store::TransferLogReader`。
- `transfer_log_store` 不能查询 PostgreSQL、订单或 payment window。
- `signer` 不能知道订单业务状态。
- `cache` 不能成为资金状态真相源。
- `workers` 不能绕过 services 直接修改订单状态，除非该行为被 repository contract 明确定义。

## 模块清单

| 顺序 | 模块 | 职责 | 输入 | 输出 | 独立测试 |
| --- | --- | --- | --- | --- | --- |
| M1 | `config` | typed config、生产配置禁用项 | env/secret refs | `AppConfig` | env fixture |
| M2 | `domain` | amount、order、payment、collection 状态和规则 | 纯值对象 | 状态决策 | unit test |
| M3 | `auth` | JWT、scope、principal | bearer token | `Principal` | token fixture |
| M4 | `db/migrations` | PostgreSQL schema | SQL migrations | DB schema | migration test |
| M5 | `db/repositories` | 事务和查询 | domain commands | persisted records | testcontainers |
| M6 | `wallet` | derivation segment -> address | signer key ref/account/change/index | address/path | deterministic + rollover test |
| M7 | `signer` | 外部签名 trait | tx request/path | signed tx/hash | fake signer |
| M8 | `chain` | ERC20 logs/balance/receipt | RPC request | normalized chain data | fake + Anvil |
| M9 | `transfer_log_store` | ERC20 Transfer logs -> KVDB | start_block/token/tick | stored logs + KV cursor | fake RPC + temp redb |
| M10 | `outbound` | nonce 和 signed tx 持久化 | tx intent | outbound tx | DB integration |
| M11 | `services/orders` | 创建/查询订单 | request DTO-ish command | order view | fake repo/wallet |
| M12 | `services/payments` | 付款匹配、确认、reorg 重算 | KV Transfer logs | order status | fake repo/log reader |
| M13 | `services/collections` | 创建归集和推进状态 | order id/idempotency | collection view | fake signer/chain |
| M14 | `api` | HTTP routes、DTO、错误模型 | HTTP | JSON | axum router test |
| M15 | `workers/scanner` | payment match loop、cursor lease/CAS | KV logs tick | payments/orders | fake log reader/repo |
| M16 | `workers/collector` | collection job loop | queued jobs | outbound tx | fake signer/repo |
| M17 | `cache` | redb 通用后置优化 | DB snapshots | cache snapshots | temp dir test |

通用 `cache` 不是首个 MVP 阻塞项；但 `transfer_log_store` 的 KVDB raw log 存储是付款验证路径的必需模块。首个 MVP 的资金真相仍必须只在 PostgreSQL，raw scan data 只在 KVDB。

本地 KVDB 分工：可重建、可从 PostgreSQL/RPC 回放的数据才进 `redb`。raw RPC response、raw Transfer logs、非 Pay3 logs、block header cache、raw scan batches 都属于 KVDB 或内存数据，不能作为 PostgreSQL 主数据。资金状态、幂等、nonce、signed tx、payment matcher cursor 的权威值都必须在 PostgreSQL。

每个模块完成后，必须按对应 phase 的测试清单做组合验收；不能只交孤立单测。

## 模块契约

### `domain`

必须先实现，其他模块只能引用 domain value types。

核心类型：

- `RawAmount`
- `TokenAmount`
- `OrderStatus`
- `PaymentMatchStatus`: `on_time | late | outside_window`
- `PaymentChainStatus`: `observed | confirmed | orphaned`
- `CollectionStatus`
- `ChainBlockRef`
- `KvReorgEpoch`
- `DerivationSegment`
- `EvmAddress`
- `TxHash`

完成标准：

- 金额解析不使用浮点数。
- address/hash 规范化为 lowercase hex。
- `DerivationSegment` 支持 rollover：`address_index` 达到最大值后递增 `change_index`，`change_index` 达到最大值后递增 `account_index`。
- 订单状态由 canonical confirmed on-time payments 重算。
- reorg 可以让 `paid/confirming` 回退。
- scanner 延迟发现按时付款可以让 `expired` 回到 `confirming/paid`。
- collection replacement 不能改变 from/to/amount/purpose，只能替换同 nonce 交易费用。

### `db/repositories`

Repository 是业务一致性的边界。service 不拼 SQL。

必须提供：

- `OrderRepository`
  - `create_order_idempotent(command)`
  - `get_order(id)`
  - `get_order_by_external_id(external_id)`
  - `PaymentRepository`
    - `claim_scan_range(worker_id)`
    - `commit_scanned_batch(batch)`，只提交 matched Pay3 payments、订单重算和 PostgreSQL cursor，不提交 raw logs
    - `recompute_orders(order_ids)`
    - `handle_kv_reorg_epoch(epoch, last_reorg_from)`
- `CollectionRepository`
  - `create_collection_idempotent(command)`
  - `claim_collection_job(worker_id)`
  - `attach_outbound_tx(collection_id, outbound_tx_id)`
  - `OutboundRepository`
    - `reserve_nonce(chain_id, from_address)`
    - `insert_signed_tx(tx)`
    - `replace_signed_tx(old_tx_id, replacement_tx)`
    - `claim_signed_collect_tx_for_broadcast(worker_id)`，只 claim 已持久化且尚未 broadcast 的 collect signed tx，用于 broadcast 前崩溃恢复
    - `claim_broadcast_collect_tx_for_receipt(worker_id)`，只 claim 已 broadcast 且尚未确认的 collect tx，用于 broadcast 后崩溃恢复和 receipt sweep
    - `mark_broadcast/confirmed/failed`
  - `AuditRepository`
    - `append_audit_event(event)`

完成标准：

- 幂等冲突返回 typed error。
- 创建订单对 `external_id` 使用 advisory lock 或等价机制。
- `wallet_cursors` 用原子 `UPDATE ... RETURNING` 分配 derivation segment，并测试 `address_index -> change_index -> account_index` rollover。
- payment matcher cursor 使用 PostgreSQL lease + CAS。
- payment matcher cursor 保存 `seen_kv_reorg_epoch`，KV epoch 变化时必须回退业务 cursor 并标记 orphaned。
- 多订单重算按 `order_id` 排序锁定。
- collect 使用 `account_nonces` 串行 nonce。
- migration 必须包含 address/hash CHECK、payments 地址归属 FK、collections treasury FK/trigger、active collection partial unique、outbound active nonce unique、audit_events。

### `chain`

只负责标准化链上数据，不决定订单状态。

Trait:

```rust
trait Erc20ChainClient {
    async fn safe_head(&self) -> Result<BlockRef>;
    async fn finalized_head(&self) -> Result<BlockRef>;
    async fn block_by_number(&self, number: u64) -> Result<BlockRef>;
    async fn transfer_logs(&self, token: EvmAddress, from: u64, to: u64) -> Result<Vec<TransferLog>>;
    async fn token_balance(&self, token: EvmAddress, owner: EvmAddress) -> Result<RawAmount>;
    async fn transaction_receipt(&self, tx: TxHash) -> Result<Option<TxReceipt>>;
    async fn broadcast_signed_tx(&self, signed_tx: Vec<u8>) -> Result<TxHash>;
}
```

完成标准：

- fake client 可控制 reorg、RPC failure、confirmations。
- Anvil 测标准 ERC20 `Transfer`。
- `transfer_logs(token, from, to)` 必须按 block range 批量返回 logs，禁止按订单逐个 RPC 查询。
- 不支持 fee-on-transfer/rebasing token，除非单独验收。
- MVP 必须实现 RPC provider manager：至少两个 provider、chain_id 校验、safe/finalized head hash 检查、429/timeout failover、hash mismatch 暂停 cursor。
- `services/payments` 禁止 `eth_getLogs`，但允许通过 `ChainHeaderReader` 读取 head 和 block hash 计算 confirmations。

### `transfer_log_store`

独立模块，详细设计见 `docs/TRANSFER_LOG_KV_MODULE.md`。它把指定 token 的 ERC20 `Transfer` logs 从 `start_block` 起连续写入 KVDB。

必须提供：

- `ensure_stream(chain_id, token_address, start_block)`
- `poll_once(stream_id)`
- `rewind_to(stream_id, block, reason)`
- `TransferLogReader::logs_in_range(stream_id, from, to, max_logs)`
- `TransferLogReader::logs_page(stream_id, after_page_token, limit)`
- `TransferLogSource::capacity_probe`

完成标准：

- 第一次从 `start_block` 扫，之后从 KVDB `next_block` 扫。
- 空 logs 区块也保存 block header/range manifest 并推进 KV cursor。
- raw logs、raw block headers、range manifest、dead letter 只在 KVDB。
- KVDB transaction 必须同时写 logs/headers/range manifest 和推进 cursor。
- reorg 时 rewind KV cursor 并重扫，不碰 PostgreSQL。
- RPC 超限时自动缩小 batch，不推进 cursor。
- `services/payments` 只能通过 `TransferLogReader` 读日志。
- reader 分页必须使用 `(block_number, log_index)` exclusive page token，不能只用 block number。
- `logs_in_range` 必须有上限，只能用于测试/恢复/debug；scanner 主路径使用 `logs_page(limit)`。
- cursor 必须包含 `reorg_epoch/last_reorg_from/last_reorg_at`。
- `StoredTransferLog` 或配套 header reader 必须提供 block timestamp，禁止用本地观测时间判断付款窗口。
- capacity probe 超阈值时 readiness fail，不推进 cursor。
- 生产部署禁止多机器共享 redb 写路径；single writer 必须有 fencing epoch。

### `PaymentWindowLookup`

付款匹配用它做候选查询，但它不拥有资金归属判断。

Trait:

```rust
trait PaymentWindowLookup {
    async fn lookup_batch(
        &self,
        chain_id: u64,
        token_address: EvmAddress,
        to_addresses: Vec<EvmAddress>,
    ) -> Result<Vec<PaymentWindowCandidate>>;
}
```

完成标准：

- MVP 至少提供内存 watch set + PostgreSQL 批量 fallback 实现。
- fallback 必须是批量 join 或临时表 join，禁止逐地址 SQL。
- 查询必须限定 `chain_id/token_address`。
- watch set stale 时必须走受限 PostgreSQL join，不允许直接丢弃 miss。
- cache hit 只能作为候选，最终仍由 PostgreSQL `orders/payment_windows` 确认。

### `signer`

只负责派生地址和签名。

Trait:

```rust
trait SignerProvider {
    async fn derive_address(&self, key_ref: &str, path: &str) -> Result<EvmAddress>;
    async fn sign_transaction(&self, key_ref: &str, path: &str, tx: UnsignedTx) -> Result<SignedTx>;
    async fn health_check(&self) -> Result<()>;
}
```

完成标准：

- 生产实现为 external signer/KMS/HSM adapter。
- 测试实现为 deterministic fake signer。
- 不在日志、DB、cache 中存 mnemonic/private key。

### `outbound`

所有链上发送交易都走 outbound。

完成标准：

- 广播前必须保存 `chain_id/from_address/nonce/signed_tx/tx_hash`。
- 重试只能重播同一 `signed_tx`。
- active outbound 的 `(chain_id, from_address, nonce)` 唯一；replacement 历史允许同 nonce 多条记录，但同一时间只能一条 active。
- dropped/replaced/confirmed 都可恢复。
- replacement 插入新 outbound 前必须把旧 outbound 标记为 `replaced`，并保留 `replacement_of` 轨迹。
- collection outbound 必须用 DB 约束或 trigger 校验 purpose/from/to/chain 一致。

## 分阶段交付

### Phase 1: 基础骨架

交付模块：`config`、`domain`、`auth`、health/ready、metrics 基础、统一错误模型。

测试：

- `cargo test domain`
- JWT scope test
- config production guard test
- JWT `exp/nbf/iat/iss/aud/sub/kid/alg` 校验测试
- scope 缺失、scope 不足、`collections:create` 与普通订单 scope 隔离测试
- `/healthz` 只测进程活性
- `/readyz` 覆盖 DB down、migration mismatch、RPC chain_id mismatch、KVDB open failure、signer down、worker lease unreadable
- `/metrics` 至少暴露 build info、request latency、readyz dependency status
- 统一错误响应契约测试

### Phase 2: 数据库闭环

交付模块：migrations、repositories。

测试：

- migration up test
- 并发 `external_id` 创建测试
- wallet cursor 并发测试
- derivation segment rollover 测试
- composite FK 测试
- payment matcher cursor lease/CAS 测试
- address/hash CHECK 负例测试
- payments 地址归属 FK 负例测试
- collections treasury FK/trigger 负例测试
- active collection partial unique 测试
- outbound active nonce/replacement unique 测试
- audit_events 写入测试

### Phase 3: 创建订单 API

交付模块：`wallet`、`services/orders`、订单 API。

测试：

- `POST /v1/orders`
- `GET /v1/orders/{id}`
- `GET /v1/orders/by-external-id/{external_id}`
- idempotency conflict
- 每单新地址且地址不复用

### Phase 4: 付款验证

交付模块：`chain`、`transfer_log_store`、`services/payments`、manual verify。

测试：

- `transfer_log_store` 从指定 `start_block/token_address` 开始写 KVDB，第二轮从 `next_block` 继续。
- 批量 KV Transfer logs -> 本地/KVDB 候选过滤 + PostgreSQL 批量 fallback -> 仅 matched Pay3 logs 批量 upsert payments；非 Pay3 logs 保留在 KVDB raw log store。
- 1000 个 pending order、0 个 payment 时不触发 per-order RPC
- 本地 watch set hit/miss 测试；miss 必须批量 fallback PostgreSQL
- 本批 1000 个不同 `to_address` 只允许批量查询，不允许逐个 SQL/RPC
- fallback SQL 必须限定 `chain_id/token_address`
- DB fallback 超阈值时本轮失败且不推进 cursor
- scanner 只使用 `logs_page(limit)`，不使用无界 `logs_in_range`
- page limit 截断同一区块时不跳过剩余 logs
- block timestamp 来自 KV header，缺 header 时不推进 cursor
- rolling lookback rescan 能发现 reorg 后新分支里出现的 Pay3 payment
- KV reorg epoch 变化后 business cursor 回退并 orphan 受影响 payments
- PostgreSQL 不存在 raw logs/full block headers 表，raw scan data 只能在 KVDB fake 中断言
- payment scanner 只推进到当前分页完整覆盖的 `complete_to_block`，不能跳过 KVDB 尚未采集区块或同区块剩余 logs
- observed/confirming payment confirmation sweep 能在没有新 logs 时推进订单到 `paid`
- Anvil ERC20 transfer -> `confirming`
- 达到 safe head/finality -> `paid`
- late payment -> `match_status=late`
- reorg -> payment `orphaned`，订单重算
- expired 后发现按时付款 -> 回到 `confirming/paid`

### Phase 5: Scanner Worker

交付模块：`workers/scanner`。

测试：

- payment matcher cursor crash/resume；raw log cursor crash/resume 由 `transfer_log_store` 覆盖
- worker lease 单活
- RPC provider manager failure/backoff/hash mismatch
- WS 断线后 HTTP 补扫
- KVDB 覆盖不足时 scanner readiness fail
- watch set stale 时走受限 PostgreSQL join，不漏新订单付款
- log ingestor lag 和 payment scanner lag metrics/告警 dry-run

### Phase 6: Collect

交付模块：`signer`、`outbound`、`services/collections`、collector。

测试：

- create collection idempotent
- prefunded gas check
- signed tx persisted before broadcast
- broadcast 前崩溃恢复
- broadcast 后崩溃恢复
- same from address concurrent collect nonce serialization
- request schema 不接受 `to_address`
- repository/DB 直接写非 treasury collection 失败
- 无 `collections:create` scope 返回 403
- signer contract test：health、timeout、key_ref 不存在、签名 tx_hash 校验、审计 request_id
- production profile 拒绝 fake/local signer
- dropped/stuck tx replacement 保留同 nonce 轨迹并写审计事件
- signer/collection/outbound metrics 和告警 dry-run

### Phase 7: E2E

全流程：

```text
create order -> transfer ERC20 -> scanner/verify -> paid
-> create collection -> outbound tx -> treasury receives token
```

必须在 Anvil + mock ERC20 上通过。

同时必须通过：

- `/readyz` 依赖失败场景。
- `/metrics` 指标存在性。
- 告警 dry-run。
- runbook 演练记录模板填写。

### Phase 8: 通用 Cache 后置

通用 redb cache 只在完整 DB 版本和 `transfer_log_store` 稳定后接入。`transfer_log_store` 的 raw log KVDB 已在 Phase 4 实现，不属于本阶段的可选项。

测试：

- 删除通用 cache 后系统从 PostgreSQL 恢复；删除 `transfer_log_store` KVDB 后必须从 `start_block` 或人工 `rewind_to` 通过 RPC 重建 raw logs。
- redb 写失败不影响订单/付款/归集。
- scanner 不能从 redb cursor 恢复。
- 通用 redb 只能保存候选地址映射、snapshot、metadata、RPC health、非权威扫描进度快照等可重建数据；raw scan batches 和 block header cache 归 `transfer_log_store` 管。

## AI 单模块工作规则

默认每次只做一个模块。只有用户明确要求时才做一个 phase；如果做 phase，必须逐项声明 phase 内每个模块边界。开始前必须写清：

- 本次模块名。
- 会改哪些文件。
- 依赖哪些已完成模块。
- 接口契约是否新增或变更。
- 本模块 fake/mock 怎么写。
- 本模块完成后跑哪些测试。

完成后必须更新：

- `nextsession.md` 全局进度板。
- 对应模块测试。
- 如果接口契约变更，更新 `docs/MODULE_PLAN.md` 和 `docs/MVP_ARCHITECTURE.md`。

禁止：

- 为了当前模块绕过 domain 类型。
- 为了当前测试直接改别的模块内部实现。
- 在 API 层写 SQL。
- 在 worker 里绕过 service/repository contract。
- 把 redb 当资金状态真相源。
- 把 raw Transfer logs、非 Pay3 logs 或全量 block headers 放进 PostgreSQL。
