# Pay3 端到端流程

## 总览

Pay3 MVP 的主链路分成五条相互独立但可组合的流程：

```text
JWT/API
  -> 创建订单和派生地址(PostgreSQL)
  -> ERC20 Transfer log 采集(KVDB)
  -> Pay3 付款匹配和订单状态(PostgreSQL)
  -> token collect(PostgreSQL outbound + signer)
  -> treasury 到账
```

核心分工：

- PostgreSQL 是资金和业务状态真相。
- KVDB 只保存可从 RPC 回放的 raw Transfer logs、block headers、range manifest、dead letter 和可重建 cache。
- `transfer_log_store` 只采集 ERC20 `Transfer` logs，不知道订单。
- payment scanner 只做业务匹配，不直接 `eth_getLogs`。
- collector 只发送已创建的 collection job，不接受任意目标地址。

## 0. 启动

1. 读取 typed config：单链、单 token、JWT、PostgreSQL、KVDB、HD derivation、signer、treasury、确认数。
2. 运行 PostgreSQL migrations，确保 `wallet_cursors`、`orders`、`payment_windows`、`payments`、`chain_cursors`、`treasury_addresses`、`collections`、`account_nonces`、`outbound_transactions`、`audit_events` 存在。
3. 初始化 `wallet_cursors(id='default')`。
4. 初始化 `chain_cursors(chain_id, token_address)`，作为 payment matcher 的业务 cursor。
5. 初始化 RPC provider manager，校验至少两个 HTTP provider、chain_id、safe/finalized head/hash。
6. 初始化 `/metrics`、`/readyz` dependency checks 和结构化日志。
7. 调用 `transfer_log_store.ensure_stream(chain_id, token_address, start_block)`：
   - KVDB 没有 cursor 时，`next_block = start_block`。
   - 已存在相同配置时幂等返回。
   - 同一 stream 传入不同 `start_block` 返回配置冲突，除非显式 rewind/reset。
   - capacity probe 失败时 log ingestor not ready，不推进 cursor。
8. scanner 启动前验证 KVDB 覆盖 `[chain_cursors.last_scanned_block - REORG_LOOKBACK_BLOCKS, target]`；不覆盖则先重建 KVDB。

## 1. 创建订单

入口：`POST /v1/orders`，需要 JWT scope `orders:create`。

事务流程：

```text
BEGIN
  validate JWT/scope
  parse amount -> raw amount
  compute canonical request_hash
  lock external_id idempotency key
  if same external_id + same request_hash: return existing order
  if same external_id + different request_hash: return 409
  fetch current chain head as window_from_block/window_from_block_hash
  lock wallet_cursors
  allocate next derivation segment
  derive child receive_address
  insert child_accounts
  insert orders
  insert payment_windows
  update wallet_cursors
COMMIT
after commit: update optional watch_set/order snapshot cache
```

结果：

- 每个订单拿到一个全新的 `receive_address`。
- 地址永久只绑定该订单，不复用。
- `payment_windows` 保存 `window_from_block/window_from_block_hash/expires_at/monitor_until`。

如果创建订单时 RPC 无法提供当前 head，MVP 应直接失败，避免支付窗口下界不确定。

## 2. 用户付款

用户向 `receive_address` 转 ERC20 token。

系统禁止：

- 禁止每个 pending 订单逐个 RPC 轮询。
- 禁止定时对每个地址 `balanceOf`。
- 禁止 payment scanner 直接 `eth_getLogs`。

付款可见性完全来自 `transfer_log_store` 写入 KVDB 的 ERC20 `Transfer` logs。

## 3. Transfer Log 采集

模块：`transfer_log_store`。

输入：

- `chain_id`
- `token_address`
- `start_block`

轮询流程：

```text
poll_once:
  read KV cursor
  target = resolve ingest target block
  if target < next_block: Idle
  from = next_block
  to = min(from + batch_size - 1, target)
  fetch block headers [from, to]
  verify header continuity
  TransferLogSource.transfer_logs(token, from, to)
  normalize logs and attach block_timestamp
  verify log.block_hash == header[log.block_number].hash
  KV transaction:
    upsert block headers, including empty blocks
    upsert logs by block/log_index
    upsert logs by tx/log_index secondary index
    upsert range manifest
    write dead letters
    cursor.next_block = to + 1
    cursor.last_completed_block = to
    cursor.last_completed_hash = header[to].hash
  commit
```

目标区块分两层：

- `ingest_target`: KVDB 可以采集到的最新 raw logs，可配置为 `latest`、`safe`、`finalized` 或 `latest - n`。
- `finality_target`: payment scanner 能把 payment 判成 `confirmed/paid` 的区块边界，由 `min_confirmations` 和 canonical hash 校验决定。

这样可以同时支持快速显示 `confirming` 和最终 `paid`。

## 4. KVDB Reorg

`transfer_log_store` 每轮 poll 前校验最近 block hash。

```text
if last_completed_hash == rpc.block_hash(last_completed_block):
  continue next_block
else:
  find first_diverged_block within reorg_lookback
  KV transaction:
    delete/mark block_header/log_by_block/log_by_tx/range_manifest from diverged block onward
    cursor.reorg_epoch += 1
    cursor.last_reorg_from = first_diverged_block
    cursor.next_block = first_diverged_block
    cursor.last_completed_block = first_diverged_block - 1
  next poll replays from first_diverged_block
```

超过 lookback 的深 reorg：

- 停止 log ingestor。
- 暴露 `DeepReorgDetected` 状态。
- 人工指定 `rewind_to(block)`。

如果 KV rewind 到 PostgreSQL payment matcher cursor 之前，payment scanner 必须感知 `kv_reorg_from`，回退业务 cursor 并把受影响 payments 标记为 `orphaned`。

具体契约：

- KV cursor 保存 `reorg_epoch/last_reorg_from/last_reorg_at`。
- PostgreSQL `chain_cursors` 保存 `seen_kv_reorg_epoch`。
- scanner 发现 epoch 变化后，先回退业务 cursor，再读取 KV logs。

## 5. 付款匹配

模块：payment scanner / `services/payments`。

它只读 `TransferLogReader`，不直接访问 RPC logs。

```text
BEGIN
  lock chain_cursors row
  claim lease
  read last_scanned_block
COMMIT

outside transaction:
  kv_completed = TransferLogReader.cursor().last_completed_block
  processed_from = max(start_block, last_scanned_block - REORG_LOOKBACK_BLOCKS + 1)
  processed_to = min(kv_completed, target_block_for_matching)
  if processed_to < processed_from: Idle
  read KV logs by logs_page(limit) within [processed_from, processed_to]
  read KV block headers and use header.timestamp as payment block_time
  extract unique to_address
  watch_set candidate lookup
  batch fallback PostgreSQL for misses
  build matched Pay3 payment batch
  detect orphan candidates by block_hash mismatch

  BEGIN
  lock chain_cursors row
  verify lease, last_scanned_block, and seen_kv_reorg_epoch unchanged
  upsert matched payments by (chain_id, tx_hash, log_index)
  mark orphaned payments in changed blocks
  lock affected orders in sorted order
  recompute paid_amount_raw/status
  update chain_cursors.last_scanned_block = complete_to_block
  release/extend lease
COMMIT
```

关键点：

- `last_scanned_block` 只能推进到当前分页完整覆盖的 `complete_to_block`，不能直接写目标 safe/finalized block。
- 每轮必须 overlap 最近 `REORG_LOOKBACK_BLOCKS`，靠 payment 幂等 upsert 去重。
- 非 Pay3 logs 不写 PostgreSQL。
- KVDB hit 只是候选，最终归属必须由 PostgreSQL `orders/payment_windows` 确认。
- KVDB miss 必须批量 fallback PostgreSQL，不能逐地址查。
- 如果 page limit 截断同一区块，scanner 不能把 cursor 推过该区块剩余 logs。
- 如果 DB fallback 地址数超过阈值，本轮失败并不推进 cursor。

## 6. 付款状态

`payments` 只保存命中 Pay3 地址后的 payment。

匹配语义：

- `on_time`: `block_number >= window_from_block` 且 `block_time <= expires_at`。
- `late`: `block_number >= window_from_block` 且 `expires_at < block_time <= monitor_until`。
- `outside_window`: 命中 Pay3 历史地址，但不在支付/监控窗口内，进入人工对账。

订单状态由当前 canonical payments 重算：

- `pending`: 无有效付款。
- `partial`: 有 on-time canonical payment，但不足额。
- `confirming`: 足额但确认数或 finality 不足。
- `paid`: 足额，达到 `min_confirmations`，且 canonical 校验通过。
- `expired`: 超时未足额。

订单状态允许回退：

- reorg 可让 `paid/confirming` 回退。
- scanner 延迟发现按时付款，可让 `expired` 回到 `confirming/paid`。

确认数推进：

- payment scanner 每轮处理新 KV logs。
- 同一个 worker 还必须周期性 sweep PostgreSQL 中 `observed/confirming` payments/orders。
- 即使没有新 Transfer log，head 增长后也要重新计算 confirmations，并把满足条件的订单推进到 `paid`。
- sweep 可通过 `ChainHeaderReader` 读取 head/block hash，但仍禁止 `eth_getLogs`。

## 7. Manual Verify

入口：`POST /v1/orders/{id}/verify`，需要 `orders:verify`。

语义：

- verify 不直接调用 `eth_getLogs`。
- verify 只触发一次 payment matcher 对该订单窗口对应 KVDB logs 的读取和重算。
- 如果 `transfer_log_store` 尚未采集到订单窗口所需区块，返回当前订单状态，并带 `verification_status=log_store_lagging`。
- 如果 KVDB 已覆盖窗口但确认数不足，返回 `confirming`。
- 只有 matched payment 达到确认数并 canonical 校验通过，才返回/写入 `paid`。

## 8. Collect

入口：`POST /v1/collections`，需要 `collections:create`。

创建 collection：

```text
BEGIN
  lock order/child_account
  require order.status = paid
  compute request_hash(idempotency_key, order_id, amount=max)
  if same idempotency_key + same request_hash: return existing
  if same idempotency_key + different request_hash: return 409
  set to_address = TREASURY_ADDRESS
  ensure no active collection for child_account
  insert collection queued
COMMIT
```

发送 collection：

```text
BEGIN
  pick queued collection FOR UPDATE SKIP LOCKED
  check token balance
  resolve amount=max at execution time
  check prefunded gas
  lock account_nonces row
  if nonce row missing:
    fetch pending nonce from RPC
    insert account_nonces
  reserve nonce and increment next_nonce
  build ERC20 transfer(treasury, amount)
  sign via signer trait
  insert outbound_transactions(chain_id, from, nonce, signed_tx, tx_hash, status=signed)
  write audit event
  update collection status=transferring, outbound_tx_id
COMMIT

outside transaction:
  broadcast signed_tx
  on retry: check receipt or rebroadcast same signed_tx
```

确认 collection：

- collector 轮询 `outbound_transactions.tx_hash` receipt。
- 达到确认/finality 后，`outbound_transactions.status=confirmed`，`collections.status=confirmed`。
- dropped/replaced/failed 必须保留原 outbound record，不能丢失 nonce 轨迹。
- stuck/dropped 时只允许同 nonce replacement；replacement 不能改变 from/to/treasury/token transfer calldata/业务金额。

## 9. 数据库分工

PostgreSQL 保存：

- `orders`
- `child_accounts`
- `payment_windows`
- matched Pay3 `payments`
- payment matcher `chain_cursors`
- `collections`
- `account_nonces`
- `outbound_transactions`
- idempotency key / request hash
- worker lease
- `audit_events`
- `treasury_addresses`

KVDB 保存：

- raw Transfer logs
- block headers
- range manifest
- dead letter
- non-Pay3 logs
- `transfer_log_store` cursor
- watch set / snapshots / metadata cache
- KV `reorg_epoch/last_reorg_from/writer_epoch`

不能反过来：

- raw logs 不进 PostgreSQL。
- orders/payments/nonces/signed_tx 不以 KVDB 为唯一存储。

## 10. 当前实现状态

当前仓库仍是文档阶段，没有 Rust 代码、migration、worker、测试和部署工件，所以不能用于真实资金。MVP 实现必须按本文跑通全流程，并通过 `docs/PRODUCTION_READINESS.md`、`docs/DEPLOYMENT.md`、`docs/RUNBOOK.md` 的验收。
