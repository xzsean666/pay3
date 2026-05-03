# Pay3 MVP 架构设计

## 目标

Pay3 MVP 要跑通一个最小但真实可用的 ERC20 收款闭环：

```text
JWT 鉴权 -> 创建订单 -> 分配派生收款地址 -> 用户转账 ERC20
-> 验证付款和确认数 -> 订单 paid -> token collect -> treasury 到账
```

MVP 只做单一默认账号、单一链、单一 ERC20 token。账号体系、多链、多 token、复杂 webhook 和后台运营面板后置。

## 当前生产可用性结论

当前仓库只有文档，没有 Rust 实现、migration、测试和部署工件，所以不能接真实资金。

本文定义的是生产优先 MVP：所有会导致真实资金丢失、误判、不可恢复或不可观测的能力都属于 MVP 出口标准，不再作为“上线前再补”的后续项。MVP 代码完成时必须同时具备：

- reorg/finality、orphan payment 重算、scanner crash/resume。
- collect nonce、signed transaction、replacement 和崩溃恢复。
- JWT claims/scope、统一错误模型、端点级权限隔离和审计日志。
- PostgreSQL 强约束、DB 级 treasury 保护、直接 SQL 破坏性插入负例测试。
- `transfer_log_store` KVDB 单写者、reorg epoch、retention/rebuild、分页读取和大日志量熔断。
- RPC provider pool、hash mismatch 暂停、RPC/indexer capacity gate。
- `/readyz`、`/metrics`、结构化日志、告警 dry-run、DB 备份恢复和 runbook 演练记录。

## 核心方案

### 地址派生

- 使用母账号通过 HD wallet 分段派生子地址，对业务 API 表现为无限地址池。
- 基础路径模板：`m/44'/60'/{account_index}'/{change_index}/{address_index}`。
- `address_index` 使用非 hardened 范围 `0..2^31-1`；当前段用完后递增 `change_index`，`change_index` 用完后递增 `account_index`。
- 每个订单绑定一个 `derivation_id`、完整 `derivation_path` 和一个 `receive_address`。
- 收款地址永不复用。每个订单分配新的 derivation path，历史地址永久保留给原订单用于 late/outside_window payment 识别。
- 如果只在单个 account/change 段内收款，account 级 xpub 足够派生地址；如果要跨 hardened `account_index` rollover，必须由 signer/root key 授权生成新 account 段。只要要 collect，就必须能为子地址签名，所以生产实现必须通过 signer trait 接入 KMS/HSM/外部签名服务。

无限派生说明：

- 单个 account/change 段约有 21.47 亿个 `address_index`。
- 系统不把单段容量当上限；`wallet_cursors` 保存当前 segment，自动 rollover 到下一段。
- `child_accounts` 保存完整 path 和 segment 字段，任何地址都能追溯到 signer key、path version 和具体 index。
- 如果 signer/key 轮换，新增 `signer_key_ref + derivation_version`，旧地址继续可识别和归集。

### 订单和支付窗口

生产优先 MVP 直接砍掉地址复用：同一个 `receive_address` 永久只能对应一个订单。这样 ERC20 没有 memo 的迟到付款也不会被误归属给新订单。

MVP 仍保留 `payment_windows` 表，用来描述订单有效付款窗口和 scanner 查询范围，但它不用于地址复用：

```sql
CREATE UNIQUE INDEX one_order_per_receive_address
ON orders(receive_address);
```

订单的 `expires_at` 是业务支付截止时间。`monitor_until` 是继续扫描迟到付款的结束时间，用于对账和人工处理，不用于地址复用。

付款匹配规则：

- `block_number >= window_from_block` 且 `block_time <= expires_at`: 可计入订单付款。
- `block_number >= window_from_block` 且 `expires_at < block_time <= monitor_until`: `match_status=late`，不自动判定订单成功。
- `block_number < window_from_block` 或 `block_time > monitor_until`: `match_status=outside_window`，表示命中 Pay3 历史地址但不在订单窗口内，进入人工对账队列。
- 订单状态必须由当前 canonical payments 重算；如果订单已过期但后来发现满足 `block_number >= window_from_block` 且 `block_time <= expires_at` 的 confirmed payment，必须允许从 `expired` 回到 `confirming/paid`。

### 金额规则

- API 接收用户金额字符串，例如 `"12.34"`。
- 服务端按配置的 token decimals 转成 raw amount。
- 数据库存 `numeric(78,0)` 或字符串，不使用浮点数。
- 支付可以允许 `amount_paid >= expected_amount` 判定成功。
- 多笔转账可累计，未达到金额前保持 `pending` 或 `partial`。

### 付款验证

主要依据 ERC20 `Transfer(address indexed from, address indexed to, uint256 value)` 事件：

- 只扫描配置的 token contract。
- 只关心 `to` 属于已分配收款地址的日志。
- 以 `(tx_hash, log_index)` 幂等入库。
- 达到 `min_confirmations` 且通过 canonical chain 校验后，订单状态才能进入 `paid`。
- verify endpoint 可以手动扫描某个订单关联窗口，scanner worker 负责后台持续扫描。
- 付款归属必须查询 PostgreSQL `orders/payment_windows`，按 `to_address` 找唯一订单并按链上 block number 和 block time 判定 `match_status=on_time/late/outside_window`；redb 缓存只能提供候选，不允许单独决定订单归属。

补充校验：

- 对已付款或待确认地址可调用 `balanceOf(address)` 做轻量对账。
- payment scanner 中断后，以 PostgreSQL 的 `chain_cursors` 决定从哪里恢复；raw scan cache 只从 KVDB/RPC 重建，不从 PostgreSQL 读取。
- PostgreSQL 只保存命中 Pay3 地址后的 payment block ref；raw block headers 和 raw logs 如需保留只能进 KVDB。发现 reorg 时把相关 payment 标记为 `orphaned` 并重算订单状态。

### 批量 Logs 处理

系统禁止对 pending 订单逐个轮询 RPC，也禁止对每个订单定时调用 `balanceOf`。

链上原始日志收集由独立 `transfer_log_store` 模块负责，详细设计见 `docs/TRANSFER_LOG_KV_MODULE.md`。它只按区块批量扫描单一 token contract 的 `Transfer` logs，并把 raw/normalized logs 写入 KVDB：

```text
eth_getLogs(
  address = TOKEN_ADDRESS,
  topics[0] = ERC20 Transfer topic,
  fromBlock = transfer_log_store.cursor.next_block,
  toBlock = target_ingest_block
)
```

`transfer_log_store` 采集流程：

```text
1. ensure_stream(chain_id, token_address, start_block)
2. 如果 KVDB cursor 不存在，next_block = start_block
3. 如果 cursor 已存在，fromBlock = cursor.next_block
4. 按 SCAN_BATCH_SIZE 切分 [fromBlock, target_ingest_block]
5. 一次 RPC 拉取该范围内所有 TOKEN_ADDRESS Transfer logs
6. 原始 block headers / raw logs 先写 KVDB scan cache 或在内存处理后丢弃
7. 空 logs 区块也写 block header 和 range manifest
8. KVDB 同事务写 headers/logs/range manifest，并推进 KVDB cursor.next_block = toBlock + 1
```

未付款订单不会触发 RPC 查询。订单量增长主要增加 PostgreSQL 订单写入和索引查询压力，不会让 RPC 请求按订单数线性增长。

这意味着 RPC 查询目标是“token contract + block range”，不是“订单列表”。在热门主网 token 上，全量 `Transfer` logs 可能很大，所以 MVP 必须有 capacity gate：

- `chain` 模块提供 `TransferLogSource` trait，默认实现是 `RpcRangeSource`。
- 启动和 readiness 必须探测最近 `LOG_SOURCE_CAPACITY_PROBE_BLOCKS` 的 token Transfer log 体量、provider 单次返回上限、429/error rate 和单块最大 logs。
- 如果 RPC range source 无法稳定覆盖配置 token，`pay3-log-ingestor` 必须 readiness fail 并拒绝推进 cursor；该 token 只能在配置兼容的 `IndexerSource` 或等价分片 source 后运行。
- 无论 source 是 RPC 还是 indexer，输出必须写入同一个 `transfer_log_store` KV schema，并走同一套 block hash/reorg 校验。
- 仍禁止逐订单 `balanceOf` 轮询。

付款匹配流程只依赖 `TransferLogReader`，不直接调 RPC：

```text
1. 从 PostgreSQL 读取业务 matcher cursor
2. 从 TransferLogReader 读取 KVDB 中已采集的 Transfer logs
3. 从 logs 提取所有 to_address，并去重
4. 这些 to_address 是全网收款地址，绝大多数不是 Pay3 地址
5. 先用本地内存/KVDB watch set 做 membership 候选匹配
6. 对 KVDB miss 的地址，用 PostgreSQL 批量 fallback 查询
7. 非 Pay3 地址不写 PostgreSQL payments，保留在 KVDB raw log store
8. 命中的 Pay3 地址才批量 upsert PostgreSQL payments
9. 对 overlap 范围内已存在的 Pay3 payments 校验 block_hash，必要时标记 orphaned
10. 只重算本批 logs 命中的 order_id 和 orphaned 影响到的 order_id
11. 同事务推进 PostgreSQL 业务 matcher cursor 到已处理 KV block
```

PostgreSQL 不保存某段区块内的全量 Transfer logs，也不保存非 Pay3 地址的 logs。全量扫描数据属于 KVDB 扫描缓存：

- raw RPC response
- block headers cache
- raw Transfer logs cache
- 非 Pay3 地址 logs
- 本批去重后的 `to_address` 临时集合

这些数据如果持久化，只能放 KVDB，建议按 block range 和 TTL 分段保存；也可以在内存处理完直接丢弃。KVDB 丢失后用 stream 的 `start_block` 或人工指定的 `rebuild_from` 重新 RPC 扫描即可恢复；业务 matcher 的 PostgreSQL cursor 需要回退到已重建 KV logs 覆盖的安全区间。

`to_address` 匹配不是范围查询。EVM address 是 hash-like 值，不能靠大小范围判断是不是 Pay3 地址。Pay3 使用集合 membership：

```text
watch_set[address] -> {order_id, window_from, expires_at, monitor_until}
```

匹配策略：

```text
1. scanner 启动时构建 `PaymentWindowLookup`，最小实现可以是内存 watch set + PostgreSQL 批量 join
2. 新订单 PostgreSQL commit 后可以异步更新 watch set，但 watch set 不参与资金最终判定
3. 扫到 logs 后，对本批去重 to_address 做本地 HashMap/KVDB 查询
4. KVDB hit 只代表候选命中，仍按 PostgreSQL/payment_window 规则确认
5. KVDB miss 不能直接丢弃，必须把 miss 地址打包批量查 PostgreSQL
6. PostgreSQL 也查不到，才确认不是 Pay3 地址并丢弃
7. 如果 watch set 没有明确的 `snapshot_version/loaded_until_order_created_at`，scanner 必须降级为受限批量 PostgreSQL join，不允许只靠 stale watch set 丢日志
```

PostgreSQL fallback 查询必须是批量的，不允许循环逐个查：

```sql
SELECT o.id, o.receive_address, pw.window_from_block, pw.window_from, pw.expires_at, pw.monitor_until
FROM unnest($1::text[]) AS batch(to_address)
JOIN orders o ON o.receive_address = batch.to_address
JOIN payment_windows pw ON pw.order_id = o.id
WHERE o.chain_id = $2
  AND o.token_address = $3;
```

当本批 `to_address` 很大时，可以使用临时表或 `COPY` 后 join，避免超大 SQL 参数：

```text
CREATE TEMP TABLE batch_to_addresses(address text PRIMARY KEY) ON COMMIT DROP
COPY/insert unique to_address
JOIN orders/payment_windows
```

KVDB 的 watch set 是性能优化，不是资金真相。KVDB miss 必须 fallback PostgreSQL；KVDB 丢失后从 PostgreSQL 重建。MVP 必须限制每批 `unique to_address`、DB fallback 地址数和 SQL 参数大小；超过阈值时本轮失败并不推进 cursor，而不是退化成逐地址查询。

扫描 cursor 分两层：

- PostgreSQL `chain_cursors.last_scanned_block` 是业务处理真相，表示到该区块为止的 canonical Pay3 付款匹配、订单重算和 cursor 推进已经同事务提交。scanner 读取 KVDB 必须使用分页 API，不能一次把大范围全量 logs 读进内存；block-level cursor 只有在当前页覆盖完整区块后才能推进。
- KVDB `transfer_log_store` cursor 只是本地 raw scan batches 的完整性快照，例如某段 raw logs 是否已经拉过、是否命中过 RPC 限流；它可丢、可重建、不能用于判断订单状态。

防漏原则：payment scanner 每轮都滚动重读最近 `REORG_LOOKBACK_BLOCKS` 的 KVDB logs，而不是只读 `last_scanned_block + 1` 之后的新块。这样即使之前某个区块没有 Pay3 payment，reorg 后新分支在同一区间出现 Pay3 payment，也会在 overlap 重扫中被发现。超过 lookback 的深度 reorg 必须暂停 cursor，由人工指定 rewind block 重扫。

KVDB rewind 必须通过 epoch 协议通知业务层：

- KV cursor 保存 `reorg_epoch`、`last_reorg_from`、`last_reorg_at`。
- PostgreSQL `chain_cursors` 保存 `seen_kv_reorg_epoch`。
- scanner 发现 epoch 变化时，必须在事务内把业务 cursor 回退到 `min(last_reorg_from, last_scanned_block - REORG_LOOKBACK_BLOCKS + 1)`，标记受影响 payments 为 `orphaned`，然后重扫。
- 如果 KVDB 覆盖范围不足以支撑回退，scanner readiness fail，先按 runbook 重建 KVDB。

`balanceOf(receive_address)` 只允许用于：

- collect 前确认 token balance。
- manual verify 的限流辅助校验。
- 对账抽样或异常排查。

`balanceOf` 不能作为 pending 订单轮询机制，也不能替代 `Transfer` logs 和 PostgreSQL `payments` 记录。

### 双数据库分工

Pay3 使用两个数据库，但职责必须严格分开：

```text
PostgreSQL = 资金真相、强一致、不可丢状态
本地 KVDB(redb) = 可重建、可从 RPC 回放的 raw scan/cache 数据
```

PostgreSQL 必须保存：

- orders
- child_accounts
- payment_windows
- 命中 Pay3 地址后的 payments
- chain_cursors
- collections
- account_nonces
- outbound_transactions
- 幂等 request_hash / idempotency_key
- worker lease / payment matcher cursor
- matched payment 的 block_number/block_hash/log_index

本地 KVDB 只允许保存：

- `active_payment_windows` 的查询缓存
- `receive_address -> order_id` 的候选缓存
- order/collection 的只读 snapshot
- token metadata 的本地副本
- RPC provider health 的本地临时状态
- raw scan batches: `chain_id/token/from_block/to_block -> raw logs`
- raw block headers / raw Transfer logs 辅助缓存
- 非 Pay3 地址 logs
- 最近处理过的 block/log 去重辅助缓存
- UI/API 快速返回用的非权威数据

KVDB 规则：

- KVDB 丢失后，系统必须能从 PostgreSQL 和 RPC 重新构建。
- KVDB 写失败不能回滚 PostgreSQL 事务。
- KVDB 命中只能作为候选提示，不能直接决定订单 paid、payment match、collection 状态。
- payment matcher cursor、nonce、signed transaction、命中 Pay3 地址后的 payments、orders 这些资金相关状态不能以 KVDB 为唯一存储。
- PostgreSQL 不做全量链上日志仓库；扫描原始数据进 KVDB 或直接丢弃。
- 扫描缓存按区块 cursor 组织，但这个 cursor 只是 KVDB scan cache 的局部键，不是恢复真相；恢复真相仍是 PostgreSQL `chain_cursors`。
- 可从 PostgreSQL/RPC 重建或回放的数据才进 KVDB。

### Token Collect

ERC20 从子地址归集到 treasury 必须由子地址支付原生币 gas，这是实现上最容易被忽略的点。

MVP 只保留生产可接受的签名抽象，不在生产配置中暴露私钥：

- `prefunded`: 子地址已经有足够原生币，collector 直接调用 ERC20 `transfer(treasury, amount)`。
- gas station 不进入 MVP；后续如需 gas station，也必须通过同一个生产 signer/outbound transaction 模型实现，不能使用明文 gas private key。
- Anvil/单元测试 profile 可以使用 deterministic local signer，但 production profile 必须通过 config guard 拒绝 local signer、mnemonic 和 private key。

collect 必须是 job 模型：

```text
create collection job -> check token balance -> check prefunded gas
-> send ERC20 transfer -> wait confirmations -> mark confirmed
```

安全约束：

- MVP 不允许客户端传归集目标地址；目标固定为 `TREASURY_ADDRESS`。
- repository contract 必须拒绝任何非 `TREASURY_ADDRESS` 的 collection `to_address`；MVP migration 也必须用 `treasury_addresses` FK 或 DB trigger 拒绝直接 SQL 写入非 treasury。
- collection API 必须使用独立 JWT scope，例如 `collections:create`。
- 发送交易前必须持久化 `chain_id/from_address/nonce/signed_tx/tx_hash`。进程崩溃后必须重播同一 signed transaction，而不是重新构造新交易。
- 同一个 from address 的 collect 必须串行 nonce lock。
- replacement 必须保留原 outbound row，把旧 row 标记为 `replaced` 后再插入同 nonce 的新 signed tx；不允许丢失 nonce 轨迹。
- 所有归集创建、签名、广播、replacement 和确认结果必须写审计事件，至少包含 `request_id/principal_sub/scope/order_id/collection_id/from_address/to_address/amount_raw/nonce/tx_hash`。

## 模块划分

具体实施必须遵循 `docs/MODULE_PLAN.md`：一个模块一个模块做，每个模块先有 contract/fake/mock 和独立测试，再进入组合联调。

| 模块 | 职责 | 可独立测试方式 |
| --- | --- | --- |
| `config` | 加载链、token、DB、JWT、HD wallet、确认数配置 | env fixture 单元测试 |
| `auth` | JWT decode/validate，生成默认 principal | 无 HTTP 的 token 单元测试 |
| `domain` | 金额、订单、付款、归集状态机 | 纯单元测试 |
| `wallet` | 根据母账号和 index 派生地址/签名器 | 固定 mnemonic 测地址 |
| `db` | migrations、repository、事务边界 | PostgreSQL 集成测试 |
| `cache` | redb 通用缓存，活跃地址候选、只读 snapshot、metadata | 临时目录单元测试 |
| `chain` | ERC20 ABI、日志扫描、balanceOf、transfer | fake client + Anvil 测试 |
| `transfer_log_store` | 从 start_block/token_address 收集 ERC20 Transfer logs 到 KVDB | fake RPC + 临时 redb + Anvil |
| `services/orders` | 创建订单、支付窗口分配、订单查询 | fake wallet + repo 测试 |
| `services/payments` | 读取 KV Transfer logs、付款匹配、确认数、状态推进 | fake TransferLogReader + repo 测试 |
| `services/collections` | 创建归集任务和推进状态 | fake chain/signer 测试 |
| `api` | axum routes、DTO、错误响应 | router 集成测试 |
| `workers` | scanner 和 collector 循环 | fake service 测 tick，Anvil 测 e2e |

组合测试从小到大：

```text
domain unit -> service with fake repo/chain -> db integration
-> api integration -> anvil e2e
```

## HTTP API MVP

所有 `/v1/*` 接口需要：

```http
Authorization: Bearer <jwt>
```

JWT 必须校验 `exp`、`nbf`、`iat`、`iss`、`aud`、`sub`。MVP 是单默认账号，但 `scope` 是必需字段，所有 `/v1/*` endpoint 都必须强制校验：

- `orders:create`
- `orders:read`
- `orders:verify`
- `collections:create`
- `collections:read`

端点必须强制 scope，不是预留字段：

| Endpoint | Scope |
| --- | --- |
| `POST /v1/orders` | `orders:create` |
| `GET /v1/orders/{id}` | `orders:read` |
| `GET /v1/orders/by-external-id/{external_id}` | `orders:read` |
| `POST /v1/orders/{id}/verify` | `orders:verify` |
| `POST /v1/collections` | `collections:create` |
| `GET /v1/collections/{id}` | `collections:read` |

`scope` claim 使用 OAuth2 风格空格分隔字符串。每个 endpoint 校验所需 scope 是否存在。MVP production profile 必须使用 RS256/EdDSA + JWKS + `kid`；HS256 只允许本地开发/test profile。

### `POST /v1/orders`

创建订单。`external_id` 用于业务方幂等，服务端保存 `request_hash`。相同 `external_id` 且请求体一致时返回已有订单；相同 `external_id` 但金额、TTL 等字段不同，返回 `409 idempotency_conflict`。

请求：

```json
{
  "external_id": "merchant-order-10001",
  "amount": "12.34",
  "ttl_seconds": 900,
  "metadata": {
    "note": "optional"
  }
}
```

响应：

```json
{
  "id": "018f8d8b-9e5a-7d36-a92f-37c3f8f21b11",
  "external_id": "merchant-order-10001",
  "status": "pending",
  "payment": {
    "chain_id": 1,
    "token_address": "0x...",
    "token_symbol": "USDT",
    "token_decimals": 6,
    "amount": "12.34",
    "amount_raw": "12340000",
    "receive_address": "0x...",
    "child_account_id": "018f8d8b-b111-7d36-a92f-37c3f8f21b11",
    "derivation_path": "m/44'/60'/0'/0/42",
    "expires_at": "2026-05-01T12:15:00Z"
  }
}
```

### `GET /v1/orders/{id}`

查询订单状态、已观测付款、确认数、过期时间。

### `GET /v1/orders/by-external-id/{external_id}`

按业务方 `external_id` 查询订单，便于业务系统幂等重试后恢复订单信息。

### `POST /v1/orders/{id}/verify`

手动触发一次该订单窗口内的付款匹配重算。该接口属于 admin/debug 能力，必须限流和审计。第一版 verify 只能通过 `TransferLogReader` 读取 KVDB logs，不直接调用 `eth_getLogs`；如果 `transfer_log_store` 还没有覆盖订单窗口，返回当前订单状态并标记 `verification_status=log_store_lagging`。如果确认数或 canonical 校验不足，只能返回 `confirming`，不能写入 `paid`。

响应包含最新订单状态：

```json
{
  "id": "018f8d8b-9e5a-7d36-a92f-37c3f8f21b11",
  "status": "paid",
  "paid_amount_raw": "12340000",
  "confirmations": 12,
  "verification_status": "matched"
}
```

### `POST /v1/collections`

创建归集任务。MVP 默认归集到配置的 `TREASURY_ADDRESS`，不允许客户端传任意 `to_address`。请求必须包含 `idempotency_key`，服务端保存 collection `request_hash`；同 key 同请求返回已有任务，同 key 不同请求返回 `409 idempotency_conflict`。

请求：

```json
{
  "order_id": "018f8d8b-9e5a-7d36-a92f-37c3f8f21b11",
  "amount": "max",
  "idempotency_key": "collect-order-10001"
}
```

响应：

```json
{
  "id": "018f8d8b-a91c-7d58-ae8e-0ed5fc753111",
  "status": "queued",
  "from_address": "0xChild...",
  "to_address": "0xTreasury..."
}
```

### `GET /v1/collections/{id}`

查询归集状态、collect tx、错误信息。

### `GET /healthz`

进程活性检查，不需要 JWT。

### `GET /readyz`

检查 PostgreSQL、RPC provider pool、migration version、chain_id、token contract、KVDB、signer、worker lease、log ingestor lag、payment scanner lag 和告警依赖。该接口只能暴露在内网、负载均衡健康检查或网络 ACL 后，不允许公网访问。

`/readyz` 失败场景必须覆盖：

- DB down、migration mismatch。
- RPC provider 数量不足、chain_id mismatch、safe/finalized head hash 冲突。
- KVDB open failure、schema version mismatch、stream config conflict、磁盘空间不足、single-writer lease 丢失。
- signer down、worker lease 不可读。
- log ingestor lag 或 payment scanner lag 超阈值。

`/healthz` 只表示进程活着，不代表可以接流量或推进资金状态。

### 错误响应

所有错误统一返回：

```json
{
  "error": {
    "code": "idempotency_conflict",
    "message": "external_id already exists with a different request body",
    "request_id": "req_018f8d8b",
    "retryable": false,
    "details": {}
  }
}
```

HTTP 状态建议：

| HTTP | 场景 |
| --- | --- |
| 400 | JSON 格式错误或字段非法 |
| 401 | JWT 缺失或无效 |
| 403 | JWT scope 不足 |
| 404 | 资源不存在 |
| 409 | 幂等冲突、状态冲突、重复归集 |
| 422 | 金额精度超限、地址格式非法、业务规则不满足 |
| 429 | 限流 |
| 500 | 未预期内部错误 |
| 503 | DB/RPC/worker 依赖不可用 |

## 状态机

订单状态：

```text
pending -> partial -> confirming -> paid
pending -> expired
partial -> expired
expired -> confirming/paid  # discovered on-time canonical payment after scanner/KV lag
paid/confirming -> partial/pending  # only when reorg orphaned previously counted payments
```

说明：

- `pending`: 等待付款。
- `partial`: 已收到部分 token，但不足额。
- `confirming`: 已足额，但确认数不足。
- `paid`: 足额并达到确认数。
- `expired`: 到期未足额。
- late/outside_window payment 不改变订单为 paid，记录在 payments 中并进入人工对账。
- 订单状态不是只向前推进，必须由 canonical payments 重算。reorg 发生时，系统必须允许从 `paid/confirming` 回退；scanner 延迟发现按时付款时，必须允许从 `expired` 回到 `confirming/paid`。

归集状态：

```text
queued -> transferring -> confirming -> confirmed
queued/transferring -> failed
failed -> queued
transferring/confirming -> replacing -> confirming
```

MVP 只实现 `prefunded` 策略。replacement 只用于同 nonce 交易被 dropped 或长时间 pending 时的 gas bump/rebroadcast 恢复，不允许借 replacement 改 treasury、from address 或业务金额。

## PostgreSQL 数据模型

### 数据规范化

- EVM address 入库前统一转小写 hex，格式必须匹配 `^0x[0-9a-f]{40}$`。
- tx hash/block hash 入库前统一转小写 hex，格式必须匹配 `^0x[0-9a-f]{64}$`。
- token raw amount 在 Rust 中使用 `U256` 或专用 decimal wrapper，DB 使用 `numeric(78,0)`；禁止浮点数参与金额计算。
- ERC20 支持边界：MVP 只支持标准 ERC20 `Transfer`、`balanceOf`、`decimals` 行为；fee-on-transfer、rebasing、可升级代理异常、decimals/codehash 与配置不符时必须拒绝上线或单独验收。

MVP migration 必须增加 DB 级 `CHECK` 约束保护这些规范，不能只靠 Rust 校验：

```sql
CHECK (receive_address ~ '^0x[0-9a-f]{40}$')
CHECK (token_address ~ '^0x[0-9a-f]{40}$')
CHECK (from_address ~ '^0x[0-9a-f]{40}$')
CHECK (to_address ~ '^0x[0-9a-f]{40}$')
CHECK (tx_hash ~ '^0x[0-9a-f]{64}$')
CHECK (block_hash ~ '^0x[0-9a-f]{64}$')
```

### `wallet_cursors`

保存下一个可新生成的 derivation segment。MVP 只需要一行 `id='default'`，但结构支持无限 rollover。

```sql
CREATE TABLE wallet_cursors (
  id text PRIMARY KEY DEFAULT 'default',
  signer_key_ref text NOT NULL,
  derivation_version integer NOT NULL DEFAULT 1,
  account_index bigint NOT NULL,
  change_index bigint NOT NULL,
  next_address_index bigint NOT NULL,
  updated_at timestamptz NOT NULL DEFAULT now()
);
```

### `child_accounts`

保存派生地址和审计信息。地址创建后只绑定一个订单，不复用。

```sql
CREATE TABLE child_accounts (
  id uuid PRIMARY KEY,
  signer_key_ref text NOT NULL,
  derivation_version integer NOT NULL DEFAULT 1,
  account_index bigint NOT NULL,
  change_index bigint NOT NULL,
  address_index bigint NOT NULL,
  derivation_path text NOT NULL,
  address text NOT NULL UNIQUE,
  last_used_at timestamptz,
  created_at timestamptz NOT NULL DEFAULT now(),
  UNIQUE (signer_key_ref, derivation_version, account_index, change_index, address_index),
  UNIQUE (id, address)
);
```

### `orders`

```sql
CREATE TABLE orders (
  id uuid PRIMARY KEY,
  external_id text NOT NULL UNIQUE,
  request_hash text NOT NULL,
  child_account_id uuid NOT NULL REFERENCES child_accounts(id),
  receive_address text NOT NULL UNIQUE,
  chain_id bigint NOT NULL,
  token_address text NOT NULL,
  expected_amount_raw numeric(78,0) NOT NULL,
  paid_amount_raw numeric(78,0) NOT NULL DEFAULT 0,
  status text NOT NULL,
  expires_at timestamptz NOT NULL,
  monitor_until timestamptz NOT NULL,
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now(),
  CHECK (expected_amount_raw > 0),
  CHECK (paid_amount_raw >= 0),
  CHECK (monitor_until >= expires_at),
  CHECK (status IN ('pending', 'partial', 'confirming', 'paid', 'expired')),
  FOREIGN KEY (child_account_id, receive_address) REFERENCES child_accounts(id, address),
  UNIQUE (id, child_account_id, receive_address),
  UNIQUE (id, chain_id, token_address, child_account_id, receive_address)
);
```

```sql
CREATE INDEX orders_chain_token_address_idx
ON orders(chain_id, token_address, receive_address);
```

### `payment_windows`

```sql
CREATE TABLE payment_windows (
  id uuid PRIMARY KEY,
  order_id uuid NOT NULL UNIQUE REFERENCES orders(id),
  child_account_id uuid NOT NULL REFERENCES child_accounts(id),
  receive_address text NOT NULL,
  window_from timestamptz NOT NULL,
  window_from_block bigint NOT NULL,
  window_from_block_hash text NOT NULL,
  expires_at timestamptz NOT NULL,
  monitor_until timestamptz NOT NULL,
  created_at timestamptz NOT NULL DEFAULT now(),
  CHECK (expires_at > window_from),
  CHECK (monitor_until >= expires_at),
  FOREIGN KEY (child_account_id, receive_address) REFERENCES child_accounts(id, address),
  FOREIGN KEY (order_id, child_account_id, receive_address) REFERENCES orders(id, child_account_id, receive_address)
);
```

每个 `receive_address` 只属于一个订单，靠 `orders.receive_address` 的唯一约束保证。

### `payments`

PostgreSQL 只保存命中 Pay3 地址后的 normalized payment，不保存全量 scanned logs。每条 payment 自带 `block_number/block_hash/log_index`，用于 reorg 后重算。

```sql
CREATE TABLE payments (
  id uuid PRIMARY KEY,
  order_id uuid NOT NULL REFERENCES orders(id),
  child_account_id uuid NOT NULL REFERENCES child_accounts(id),
  chain_id bigint NOT NULL,
  token_address text NOT NULL,
  tx_hash text NOT NULL,
  log_index bigint NOT NULL,
  from_address text NOT NULL,
  to_address text NOT NULL,
  amount_raw numeric(78,0) NOT NULL,
  block_number bigint NOT NULL,
  block_hash text NOT NULL,
  block_time timestamptz NOT NULL,
  confirmations bigint NOT NULL DEFAULT 0,
  match_status text NOT NULL,
  chain_status text NOT NULL,
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now(),
  UNIQUE (chain_id, tx_hash, log_index),
  CHECK (amount_raw > 0),
  CHECK (match_status IN ('on_time', 'late', 'outside_window')),
  CHECK (chain_status IN ('observed', 'confirmed', 'orphaned')),
  FOREIGN KEY (order_id, chain_id, token_address, child_account_id, to_address)
    REFERENCES orders(id, chain_id, token_address, child_account_id, receive_address)
);
```

### `chain_cursors`

```sql
CREATE TABLE chain_cursors (
  chain_id bigint NOT NULL,
  token_address text NOT NULL,
  last_scanned_block bigint NOT NULL,
  seen_kv_reorg_epoch bigint NOT NULL DEFAULT 0,
  lease_owner text,
  lease_until timestamptz,
  updated_at timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (chain_id, token_address)
);
```

`last_scanned_block` 表示 payment matcher 已完整处理的最高 KVDB log 区块，inclusive。scanner 不能长时间持 DB 锁读 KVDB 或做批量匹配；它必须先 claim lease，读取 `TransferLogReader` 分页后再重锁 cursor，校验 cursor/epoch 未变化，然后在同一事务内只写命中 Pay3 地址后的 payments/orders 并推进 cursor。原始区块头和 raw logs 不写 PostgreSQL。

### `treasury_addresses`

MVP 固定归集到 treasury，并用 DB 级约束兜底，防止绕过 repository 直接插入恶意 `to_address`。

```sql
CREATE TABLE treasury_addresses (
  chain_id bigint NOT NULL,
  token_address text NOT NULL,
  treasury_address text NOT NULL,
  created_at timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (chain_id, token_address, treasury_address),
  CHECK (treasury_address ~ '^0x[0-9a-f]{40}$')
);
```

migration 必须按当前配置 seed 一行 `(CHAIN_ID, TOKEN_ADDRESS, TREASURY_ADDRESS)`，并在 production profile 启动时校验 treasury 不是零地址、不是任一 child address。

### `collections`

```sql
CREATE TABLE collections (
  id uuid PRIMARY KEY,
  order_id uuid NOT NULL REFERENCES orders(id),
  idempotency_key text NOT NULL,
  request_hash text NOT NULL,
  child_account_id uuid NOT NULL REFERENCES child_accounts(id),
  chain_id bigint NOT NULL,
  token_address text NOT NULL,
  from_address text NOT NULL,
  to_address text NOT NULL,
  amount_raw numeric(78,0),
  status text NOT NULL,
  outbound_tx_id uuid,
  attempt_count integer NOT NULL DEFAULT 0,
  locked_by text,
  locked_until timestamptz,
  error text,
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now(),
  CHECK (status IN ('queued', 'transferring', 'confirming', 'confirmed', 'failed', 'dropped', 'replacing', 'replaced')),
  FOREIGN KEY (child_account_id, from_address) REFERENCES child_accounts(id, address),
  FOREIGN KEY (order_id, chain_id, token_address, child_account_id, from_address)
    REFERENCES orders(id, chain_id, token_address, child_account_id, receive_address),
  FOREIGN KEY (chain_id, token_address, to_address)
    REFERENCES treasury_addresses(chain_id, token_address, treasury_address)
);
```

需要增加 partial unique index，避免同一子地址同时存在多个 active collection：

```sql
CREATE UNIQUE INDEX one_active_collection_per_child
ON collections(child_account_id)
WHERE status IN ('queued', 'transferring', 'confirming');

CREATE UNIQUE INDEX collections_idempotency_key_idx
ON collections(idempotency_key);
```

### 常用索引

```sql
CREATE INDEX payments_order_idx ON payments(order_id);
CREATE INDEX payments_block_idx ON payments(chain_id, block_number);
CREATE INDEX payments_to_address_idx ON payments(to_address);
CREATE INDEX payment_windows_address_window_idx
ON payment_windows(receive_address, window_from_block, expires_at, monitor_until);
CREATE INDEX collections_claim_idx
ON collections(status, locked_until, created_at);
```

### `account_nonces`

按发送地址串行保留 nonce。

```sql
CREATE TABLE account_nonces (
  chain_id bigint NOT NULL,
  address text NOT NULL,
  next_nonce numeric(78,0) NOT NULL,
  updated_at timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (chain_id, address)
);
```

### `outbound_transactions`

所有链上发送交易必须先落库再广播，collector 重试时只能重播同一 `signed_tx`。

```sql
CREATE TABLE outbound_transactions (
  id uuid PRIMARY KEY,
  chain_id bigint NOT NULL,
  purpose text NOT NULL,
  from_address text NOT NULL,
  to_address text NOT NULL,
  nonce numeric(78,0) NOT NULL,
  tx_hash text NOT NULL,
  signed_tx bytea NOT NULL,
  status text NOT NULL,
  replacement_of uuid REFERENCES outbound_transactions(id),
  replacement_reason text,
  broadcast_count integer NOT NULL DEFAULT 0,
  last_broadcast_at timestamptz,
  receipt_block_number bigint,
  receipt_block_hash text,
  error text,
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now(),
  UNIQUE (chain_id, tx_hash),
  CHECK (purpose IN ('collect')),
  CHECK (status IN ('signed', 'broadcast', 'confirmed', 'failed', 'dropped', 'replaced'))
);
```

同一个 nonce 可以有 replacement 轨迹，但只能有一个未终结 outbound：

```sql
CREATE UNIQUE INDEX outbound_active_nonce_idx
ON outbound_transactions(chain_id, from_address, nonce)
WHERE status IN ('signed', 'broadcast', 'confirmed');

CREATE UNIQUE INDEX outbound_collect_composite_idx
ON outbound_transactions(id, purpose, chain_id, from_address, to_address);
```

```sql
ALTER TABLE collections
ADD CONSTRAINT collections_outbound_tx_fk
FOREIGN KEY (outbound_tx_id) REFERENCES outbound_transactions(id);
```

MVP migration 必须增加 trigger 或等价 DB 约束，确保 `collections.outbound_tx_id` 指向的 outbound 满足：

- `purpose='collect'`
- `chain_id/from_address/to_address` 与 collection 完全一致
- replacement 不能改变 `from_address/to_address/chain_id/purpose`
- 直接 SQL 插入非 treasury collection 或错误 outbound 关联必须失败

### `audit_events`

资金相关动作必须落审计日志，便于恢复和追责。

```sql
CREATE TABLE audit_events (
  id uuid PRIMARY KEY,
  event_type text NOT NULL,
  request_id text,
  principal_sub text,
  scopes text,
  order_id uuid,
  collection_id uuid,
  tx_hash text,
  payload jsonb NOT NULL,
  created_at timestamptz NOT NULL DEFAULT now()
);
```

## 关键事务

### 创建订单事务

```text
BEGIN
  parse amount -> raw amount
  compute request_hash
  pg_advisory_xact_lock(hash(external_id))
  if external_id exists and request_hash matches, return existing order
  if external_id exists and request_hash differs, return 409 idempotency_conflict
  fetch current chain head and set window_from_block/window_from_block_hash
  allocate next derivation segment by atomic wallet_cursors UPDATE ... RETURNING
  rollover address_index/change_index/account_index when a segment reaches its max
  derive next child address from full derivation path
  insert child_accounts
  insert orders
  insert payment_windows
  update child_accounts.last_used_at
COMMIT
update redb cache after commit
```

`wallet_cursors` migration 必须 seed `id='default'`。订单创建只派生新地址，不实现地址复用分支。`request_hash` 使用 canonical request：`external_id`、raw amount、effective ttl、canonical JSON metadata；不能包含当前时间、生成的 order id 或派生地址。`window_from_block/window_from_block_hash` 使用创建订单时的当前 canonical/latest head；如果 RPC 不可用，MVP 直接拒绝创建订单，避免支付窗口下界不确定。

### 付款确认事务

```text
BEGIN
  lock order row FOR UPDATE
  upsert payment by (chain_id, tx_hash, log_index)
  verify payment belongs to exactly one order/payment_window
  set match_status and chain_status
  if previous row was orphaned and log reappears in canonical block, update block_hash/block_number/chain_status
  recalc order from confirmed + canonical + on_time payments only
  update order status
COMMIT
update redb cache after commit
```

scanner worker 更新 `chain_cursors` 时，必须和本批 payment/order 更新在同一个事务里提交。并发的 manual verify 和 scanner 必须通过 `orders FOR UPDATE` 串行同一订单状态重算；批量锁多个订单时按 `order_id` 排序。

### Scanner claim/CAS 算法

这里的 scanner 是 payment matcher，不是 raw log ingestor。raw ERC20 logs 已由 `transfer_log_store` 写入 KVDB。

```text
BEGIN
  lock chain_cursor FOR UPDATE
  if lease is held by another live worker, exit
  set lease_owner and lease_until
  read last_scanned_block
COMMIT

outside transaction:
  read TransferLogReader.cursor()
  if kv_cursor.reorg_epoch != chain_cursor.seen_kv_reorg_epoch:
    plan business rewind from kv_cursor.last_reorg_from
  processed_from = max(configured_start_block, last_scanned_block - REORG_LOOKBACK_BLOCKS + 1)
  processed_to = min(kv_completed_block, target_block_for_matching)
  if processed_to < processed_from, exit idle
  read KVDB Transfer logs by logs_page(limit) within [processed_from, processed_to]
  read KVDB block headers for every log block and use header.timestamp as payment block_time
  batch-match to_address against PaymentWindowLookup/PostgreSQL fallback

BEGIN
  lock chain_cursor FOR UPDATE
  verify lease_owner is self, last_scanned_block is unchanged, and seen_kv_reorg_epoch is unchanged
  if kv reorg epoch changed:
    rewind last_scanned_block to safe block
    update seen_kv_reorg_epoch
  upsert matched Pay3 payments only
  mark changed-block matched payments as orphaned
  lock affected orders in sorted order
  recalc order states
  update last_scanned_block = complete_to_block
  clear/extend lease
COMMIT
```

payment scanner 失败不推进 PostgreSQL cursor。下次 worker 从 PostgreSQL cursor 继续；`transfer_log_store` 的 KV cursor 只能说明 raw logs 采集进度，不能作为订单付款恢复依据。`last_scanned_block` 只能推进到当前分页已经完整覆盖的 `complete_to_block`，不能推进到 KVDB 尚未采集的目标区块，也不能在 page limit 截断同一区块时跳过剩余 logs。

### Confirmation Sweep

即使没有新的 Transfer log，head 增长后也可能让 `observed/confirming` payment 达到确认数。payment scanner 必须周期性执行 sweep：

```text
outside transaction:
  fetch current finality/latest head through ChainHeaderReader
  load observed/confirming payments and affected orders from PostgreSQL
  verify canonical block hash for each payment
  compute confirmations

BEGIN
  lock affected orders in sorted order
  update payment confirmations/chain_status
  mark orphaned if canonical hash changed
  recalc order states
COMMIT
```

`services/payments` 禁止直接 `eth_getLogs`，但允许依赖 `ChainHeaderReader` 读取 latest/safe/finalized head 和指定 block hash。sweep 不推进 raw log KV cursor；它只更新 PostgreSQL 中已匹配 Pay3 payments 的确认状态。

### Reorg 处理事务

```text
outside transaction:
  fetch current RPC block hashes for recent REORG_LOOKBACK_BLOCKS
  compare against payment block_hash and KVDB block header cache when available
  compute reorg_safe_block and affected payment/order ids

BEGIN
  lock chain_cursor FOR UPDATE
  mark payments in changed blocks as orphaned
  lock affected orders FOR UPDATE
  recalc paid_amount_raw and status
  rewind chain_cursor to reorg_safe_block
COMMIT
```

如果 KVDB block header cache 丢失，reorg 检测从 RPC 重新拉最近 `REORG_LOOKBACK_BLOCKS` 的 block hashes，并用 PostgreSQL `payments(block_number, block_hash)` 对已命中的 Pay3 payments 做校验。KVDB 只加速，不是 reorg 真相源。

因为 PostgreSQL 不保存全量区块头表，reorg 后“以前空块、新链出现 Pay3 payment”的情况靠 rolling overlap rescan 捕获；matched payment 的孤块回滚靠 PostgreSQL 中的 `block_number/block_hash/log_index` 与 RPC 当前 canonical hash 对比完成。

### 创建归集事务

```text
BEGIN
  lock order/child_account
  check order paid
  compute collection request_hash
  if idempotency_key exists and request_hash matches, return existing collection
  if idempotency_key exists and request_hash differs, return 409 idempotency_conflict
  set to_address from TREASURY_ADDRESS
  ensure no active collection for same child_index unless previous failed
  insert collection queued
COMMIT
```

### Collector 发送事务

```text
BEGIN
  pick queued collection FOR UPDATE SKIP LOCKED
  check token balance and resolve amount=max if needed
  lock account_nonces row for from_address
  if account_nonces row is missing, fetch pending nonce from RPC and insert it
  reserve nonce and increment next_nonce
  build and sign transaction
  insert outbound_transactions with chain_id, from_address, nonce, signed_tx, tx_hash, status=signed
  update collection outbound_tx_id and status=transferring
COMMIT
outside transaction: broadcast signed_tx
on retry: rebroadcast same signed_tx or check tx_hash receipt first
```

replacement/gas bump 流程：

```text
BEGIN
  lock collection and current outbound tx
  verify current tx is dropped or stuck past configured threshold
  lock account_nonces row for same from_address
  mark old outbound status=replaced
  build replacement tx with same nonce/from/to/purpose and higher fee
  sign via signer
  insert new outbound_transactions(replacement_of=old_id, same nonce, new tx_hash, status=signed)
  update collection.outbound_tx_id = new_id, status=transferring
  write audit event
COMMIT
outside transaction: broadcast new signed_tx
```

replacement 不能改变 treasury、from address、token transfer calldata 或业务金额。若旧 tx 后续也 confirmed，runbook 必须人工核验 treasury 收款，不能自动删除审计轨迹。

## 本地 KV 缓存

`redb` 分两类使用：

- `transfer_log_store` 的 raw log KVDB 是 MVP 付款验证路径的必需模块。
- 通用 cache，如 order snapshot、watch set snapshot、token metadata，本身是后置性能优化，不应该阻塞订单/付款/归集业务事实落 PostgreSQL。

无论哪一类，KVDB 数据都必须可从 PostgreSQL/RPC 重建，不能成为资金状态真相。

缓存表：

- `active_payment_windows`: `receive_address -> {order_id, window_from, expires_at, monitor_until}`
- `scan_batches`: `chain_id:token:from_block:to_block -> raw Transfer logs + rpc metadata`
- `block_header_cache`: `chain_id:block_number -> {block_hash, parent_hash, timestamp}`
- `non_pay3_log_cache`: 可选 TTL 缓存，用于调试和去重，不参与资金判定
- `order_snapshot`: `order_id -> compact json status`
- `token_metadata`: `token_address -> symbol/decimals`

缓存规则：

- `transfer_log_store` 的 raw scan tables 在 KVDB transaction 中独立提交：写 headers/logs/range manifest 后才能推进 KV cursor。
- order snapshot、watch set snapshot 这类业务 cache 只在 PostgreSQL commit 成功后写。
- 启动时从 DB 重建活跃支付窗口映射。
- cache miss 查询 DB。
- cache hit 只能作为候选提示，付款归属仍必须查询 PostgreSQL。
- PostgreSQL 的 `chain_cursors` 是 payment matcher cursor 唯一真相源；`transfer_log_store` 的 redb cursor 只是 raw log 采集进度，不决定资金状态。
- raw scan data 如果需要落盘，必须进 redb/KVDB，禁止为全量链上 logs 或 block headers 建 PostgreSQL 表。
- 除 `transfer_log_store` raw logs 外，只有可重建、可后置的数据才允许进入 redb。
- 不把 private key、mnemonic、JWT 放进 redb。

KVDB retention 水位：

```text
retention_floor_block =
  min(
    chain_cursors.last_scanned_block - REORG_LOOKBACK_BLOCKS,
    earliest active/monitor_until payment_windows.window_from_block,
    manual_rebuild_floor
  )
```

清理任务禁止删除 `retention_floor_block` 之后的数据。启动 scanner 前必须验证 KVDB 覆盖 `[last_scanned_block - REORG_LOOKBACK_BLOCKS, target]`；如果不覆盖，先从 `min(last_scanned_block - lookback, earliest_unsettled_window_from_block)` 重建 KVDB，再恢复业务 cursor。

## 配置项

```env
APP_BIND=0.0.0.0:8080
METRICS_BIND=0.0.0.0:9090
DATABASE_URL_SECRET_REF=pay3/database-url
REDB_PATH=./data/pay3.redb

JWT_ALLOWED_ALGS=RS256,EdDSA
JWT_JWKS_URL=https://auth.example.com/.well-known/jwks.json
JWT_ISSUER=pay3
JWT_AUDIENCE=pay3-api
JWT_SCOPE_CLAIM=scope

CHAIN_ID=1
RPC_HTTP_URLS=https://primary-rpc.example,https://secondary-rpc.example
RPC_WS_URLS=wss://primary-rpc.example,wss://secondary-rpc.example
RPC_PROVIDER_QUORUM_MODE=safe_head_hash_check
TOKEN_ADDRESS=0x...
TOKEN_SYMBOL=USDT
TOKEN_DECIMALS=6
MIN_CONFIRMATIONS=12
FINALITY_MODE=safe_head_with_reorg_lookback
REORG_LOOKBACK_BLOCKS=64
SCAN_FROM_BLOCK=DEPLOYMENT_SPECIFIC_BLOCK
SCAN_BATCH_SIZE=2000
MAX_LOGS_PER_SCAN_PAGE=5000
MAX_UNIQUE_TO_ADDRESSES_PER_BATCH=10000
MAX_DB_FALLBACK_ADDRESSES=10000
LOG_SOURCE=rpc_range
LOG_SOURCE_CAPACITY_PROBE_BLOCKS=128
MAX_VERIFY_SCAN_BLOCKS=5000

HD_DERIVATION_PATH_TEMPLATE=m/44'/60'/{account_index}'/{change_index}/{address_index}
HD_ADDRESS_INDEX_MAX=2147483647
HD_CHANGE_INDEX_MAX=2147483647
SIGNER_PROVIDER=external
SIGNER_KEY_REF=pay3-master
TREASURY_ADDRESS=0x...
COLLECT_STRATEGY=prefunded

DEFAULT_ORDER_TTL_SECONDS=900
MONITOR_LATE_PAYMENT_SECONDS=86400
```

生产配置不包含 mnemonic/private key，也不包含明文 JWT secret 或明文 DB URL。开发和测试如需 HS256、本地 DB、本地 mnemonic，只能放在单独的 test profile，不能进入生产配置模板。生产启动时必须拒绝 `SCAN_FROM_BLOCK=0`、空 treasury、treasury 等于子地址、local signer、明文 secret、单 RPC provider。

## MVP 可观测性

MVP 不是先把功能跑通再补监控。以下内容必须随模块落地：

- `/metrics` 暴露 Prometheus 指标。
- 所有 HTTP request、worker tick、RPC call、DB query、signer call 使用结构化日志，带 `request_id` 或 `worker_tick_id`。
- 资金动作写 `audit_events`，日志只记录 id，不泄露 secret。
- staging 必须触发告警 dry-run 并记录结果。

核心 metrics 名称：

| Metric | 含义 |
| --- | --- |
| `pay3_build_info` | build/profile/config version |
| `pay3_readyz_dependency_status` | readyz 依赖状态，label: dependency |
| `pay3_http_request_duration_seconds` | API latency |
| `pay3_db_query_duration_seconds` | DB latency |
| `pay3_rpc_request_duration_seconds` | RPC latency |
| `pay3_rpc_errors_total` | RPC error count |
| `pay3_log_ingestor_lag_blocks` | KV log ingestor lag |
| `pay3_payment_scanner_lag_blocks` | payment matcher lag |
| `pay3_transfer_logs_scanned_total` | scanned Transfer logs |
| `pay3_payment_events_total` | observed/confirmed/orphaned payments |
| `pay3_orders_by_status` | order status count |
| `pay3_collections_by_status` | collection status count |
| `pay3_outbound_transactions_by_status` | outbound tx status count |
| `pay3_signer_errors_total` | signer failures |
| `pay3_prefunded_gas_low_total` | gas balance low events |

最低告警：

- `pay3_log_ingestor_lag_blocks > MIN_CONFIRMATIONS * 2` 持续 5 分钟。
- `pay3_payment_scanner_lag_blocks > REORG_LOOKBACK_BLOCKS` 持续 5 分钟。
- RPC error rate > 5% 持续 5 分钟。
- provider safe/finalized head hash 冲突。
- signer 连续失败。
- collection `transferring/confirming` 超时。
- prefunded gas 低于 3 次 collect 估算 gas。
- late/outside_window payment 出现。

## 推荐实现路线

### M0 文档和项目骨架

- `Agent.md`
- `docs/MVP_ARCHITECTURE.md`
- `nextsession.md`
- `Cargo.toml`
- `src/main.rs`
- health endpoint

### M1 基础服务

- typed config
- tracing
- `/metrics`
- 严格 `/healthz` 和 `/readyz`
- domain value types
- amount parser
- order/payment/collection 状态机
- JWT middleware
- error type
- API DTO

### M2 数据库闭环

- migrations
- repositories
- payment matcher cursor lease/CAS

### M3 创建订单闭环

- HD 地址派生
- 创建订单事务，每单派生新地址，不复用
- `POST /v1/orders`
- `GET /v1/orders/{id}`

### M4 付款验证闭环

- ERC20 ABI
- RPC provider manager 和 `ChainHeaderReader`
- `TransferLogSource` trait + RPC range source capacity gate
- `transfer_log_store` 从 start_block/token_address 收集 Transfer logs 到 KVDB
- `POST /v1/orders/{id}/verify`
- 支付入库和确认数更新
- reorg 检测和 orphaned payment 重算

### M5 后台 worker

- PostgreSQL payment matcher cursor
- 定时读取 KVDB Transfer logs 并匹配 Pay3 订单
- 过期订单推进
- scanner crash/resume 和 cursor 原子推进

### M6 归集闭环

- collection job
- ERC20 transfer
- prefunded gas check
- nonce lock、signed transaction 持久化和重播
- replacement/gas bump 状态机和审计事件
- `POST /v1/collections`
- `GET /v1/collections/{id}`

### M7 E2E

- Anvil
- mock ERC20
- 创建订单、转账、verify、collect 全流程测试
- 并发创建、重复 external_id、late payment、reorg、RPC 中断、scanner resume、collect 崩溃恢复测试
- `/readyz` 依赖失败、`/metrics` 指标、告警 dry-run、runbook 演练记录

### M8 MVP 出口准入

- OpenAPI 输出和错误码示例完整。
- `docs/PRODUCTION_READINESS.md` 清单全部通过。
- `docs/DEPLOYMENT.md` 部署约束全部通过。
- `docs/RUNBOOK.md` 演练全部有 pass/fail 记录。
- config guard 自动拒绝明文 secret、单 RPC provider、local signer、`SCAN_FROM_BLOCK=0`、地址复用。

## 参考资料

- ERC20 标准: https://eips.ethereum.org/EIPS/eip-20
- Alloy Rust Ethereum toolkit: https://alloy.rs/introduction/getting-started/
- Alloy `sol!` contract bindings: https://alloy.rs/contract-interactions/using-sol!/
- axum middleware: https://docs.rs/axum/latest/axum/middleware/
- sqlx PostgreSQL pool/transaction: https://docs.rs/sqlx/latest/sqlx/struct.Pool.html
- jsonwebtoken decode/validation: https://docs.rs/jsonwebtoken/latest/jsonwebtoken/fn.decode.html
- redb embedded KV: https://docs.rs/redb/latest/redb/
- PostgreSQL constraints and exclusion constraints: https://www.postgresql.org/docs/current/ddl-constraints.html
- PostgreSQL transaction isolation: https://www.postgresql.org/docs/current/transaction-iso.html
- BIP44 HD wallet hierarchy: https://github.com/bitcoin/bips/blob/master/bip-0044.mediawiki
- Pay3 production readiness: `docs/PRODUCTION_READINESS.md`
- Pay3 module plan: `docs/MODULE_PLAN.md`
- Pay3 deployment notes: `docs/DEPLOYMENT.md`
- Pay3 runbook draft: `docs/RUNBOOK.md`
