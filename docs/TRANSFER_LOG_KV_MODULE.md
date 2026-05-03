# ERC20 Transfer Log KV 模块

## 目标

`transfer_log_store` 是独立模块，只负责把指定 ERC20 token 的 `Transfer` events 从链上连续收集到本地 KVDB。业务付款匹配、订单状态、归集和 PostgreSQL 都不在这个模块里。

调用方传入：

- `chain_id`
- `token_address`
- `start_block`

模块从 `start_block` 开始扫描，之后按本地 cursor 从下一个未完成区块继续轮询。它保存 raw/normalized Transfer logs、区块头缓存、range manifest、reorg epoch 和非权威扫描 cursor 到 KVDB。PostgreSQL 不保存这些 raw scan 数据。

## 成熟方案参考

设计参考以下公开规范和成熟实现经验：

- Ethereum JSON-RPC `eth_getLogs` 支持 `fromBlock`、`toBlock`、`address`、`topics` 过滤，topic 顺序有明确语义，并支持 `"safe"`、`"finalized"` block tag。见 ethereum.org JSON-RPC 文档。
- Geth 的 `eth_subscribe("logs")` 文档明确：reorg 时旧链 logs 会以 `removed=true` 重新发送，新链 logs 也会发送，同一交易可能出现多次。即使本模块主路径用 HTTP polling，也要按这个模型设计重组处理。
- EIP-234 说明了单靠 filters/subscriptions 在断线和 reorg 下会失步，建议客户端维护最近 N 个区块的内部模型；`blockHash` filter 可用于按具体 block hash 拉取 logs。
- `redb` 是本地嵌入式 KV，支持 ACID transaction。模块必须把“写 logs/headers/range manifest”和“推进 KV cursor”放进同一个 KV transaction。

参考链接：

- https://ethereum.org/developers/docs/apis/json-rpc/#eth_getlogs
- https://ethereum.org/developers/docs/apis/json-rpc/#eth_newfilter
- https://geth.ethereum.org/docs/interacting-with-geth/rpc/pubsub#logs
- https://eips.ethereum.org/EIPS/eip-234
- https://docs.rs/redb/latest/redb/

## 模块边界

只做：

- 通过 `TransferLogSource` 获取 token contract + block range 的 Transfer logs；默认 source 是 HTTP `eth_getLogs`。
- 只过滤 `topics[0] = Transfer(address,address,uint256)`。
- 解析 `from`、`to`、`value`、`tx_hash`、`log_index`、`block_number`、`block_hash`。
- 拉取并缓存区块头：`block_number/block_hash/parent_hash/timestamp`。
- 将 logs 和 headers 写入 KVDB。
- 维护本地扫描 cursor：`next_block`。
- 处理 RPC 限流、空块、重复日志、reorg、崩溃恢复。
- 暴露 capacity gate，防止热门 token 超过当前 source 能力时仍然推进 cursor。

不做：

- 不判断 `to_address` 是否属于 Pay3。
- 不查 PostgreSQL。
- 不更新订单状态。
- 不调用 `balanceOf`。
- 不发 webhook。
- 不把 raw logs 写进 PostgreSQL。
- 不用 KVDB cursor 作为资金状态真相。

上游模块依赖它的 reader trait 即可：

```text
payment matcher -> TransferLogReader -> KVDB transfer logs
```

## Rust 模块位置

建议目录：

```text
src/
  transfer_log_store/
    mod.rs
    config.rs
    cursor.rs
    kv.rs
    rpc.rs
    ingestor.rs
    reader.rs
    types.rs
    reorg.rs
```

`chain` 模块只提供 RPC trait；`transfer_log_store` 调用 RPC trait 并写 KVDB。`services/payments` 只读 `TransferLogReader`。

## Public API

### 配置

```rust
pub struct TransferLogStreamConfig {
    pub chain_id: u64,
    pub token_address: EvmAddress,
    pub start_block: u64,
    pub poll_interval_ms: u64,
    pub batch_size_blocks: u64,
    pub max_batch_size_blocks: u64,
    pub max_logs_per_page: usize,
    pub max_unique_to_addresses_per_batch: usize,
    pub max_db_fallback_addresses: usize,
    pub capacity_probe_blocks: u64,
    pub reorg_lookback_blocks: u64,
    pub target_mode: ScanTargetMode,
    pub rpc_max_retries: u32,
    pub log_source: LogSourceKind,
}

pub enum ScanTargetMode {
    SafeTag,
    FinalizedTag,
    LatestMinusConfirmations(u64),
}

pub enum LogSourceKind {
    RpcRange,
    Indexer,
}
```

规则：

- `start_block` 是 inclusive。
- 第一次启动时 `next_block = start_block`。
- 后续每次从 `next_block` 开始扫。
- 如果同一 `(chain_id, token_address)` 已存在 stream，重复传入相同 `start_block` 是幂等；传入不同 `start_block` 必须返回 `stream_config_conflict`，除非调用显式 rewind/reset API。
- 生产禁止 `start_block=0`，除非配置显式允许全历史回放。
- `max_logs_per_page/max_unique_to_addresses_per_batch/max_db_fallback_addresses` 是 MVP 必填安全阈值，不能无限大。
- readiness 必须先运行 capacity probe；如果最近 N 块的日志体量超过当前 `log_source` 能力，本 stream 不允许进入 ready。

### Ingestor

```rust
pub trait TransferLogIngestor {
    async fn ensure_stream(&self, config: TransferLogStreamConfig) -> Result<StreamState>;
    async fn poll_once(&self, stream: StreamId) -> Result<PollOutcome>;
    async fn rewind_to(&self, stream: StreamId, block: u64, reason: RewindReason) -> Result<()>;
}
```

### Reader

```rust
pub trait TransferLogReader {
    async fn cursor(&self, stream: StreamId) -> Result<TransferLogCursor>;
    async fn block_header(&self, stream: StreamId, block: u64) -> Result<Option<BlockHeaderSnapshot>>;
    async fn logs_in_range(&self, stream: StreamId, from: u64, to: u64, max_logs: usize) -> Result<Vec<StoredTransferLog>>;
    async fn logs_page(&self, stream: StreamId, after: Option<LogPageToken>, limit: usize) -> Result<LogPage>;
}
```

付款匹配模块只依赖 `TransferLogReader`，不直接调 RPC。

`logs_in_range` 只允许用于测试、恢复和小窗口 debug，必须带 `max_logs` 上限。scanner 主路径只能用 `logs_page`，禁止一次把大范围全量 logs 读入内存。

`LogPageToken` 必须包含 `(block_number, log_index)`，并定义为 exclusive cursor，避免 `limit` 截断同一区块多条 logs 时跳过数据。返回顺序固定为 `(block_number ASC, log_index ASC)`。`LogPage` 还必须返回 `complete_to_block`：只有完全覆盖到某个区块末尾时，业务 scanner 才能把 PostgreSQL block cursor 推进到该区块。

## KVDB Schema

KV key 必须包含 schema version，方便以后迁移。

```text
stream_config:
  key   = v1:stream:{chain_id}:{token_address}
  value = TransferLogStreamConfig + created_at + updated_at

cursor:
  key   = v1:cursor:{chain_id}:{token_address}
  value = { start_block, next_block, last_completed_block, last_completed_hash, target_mode, reorg_epoch, last_reorg_from, last_reorg_at, writer_epoch, updated_at }

block_header:
  key   = v1:block:{chain_id}:{token_address}:{block_number_be}
  value = { block_hash, parent_hash, timestamp, scan_status, scanned_at }

log_by_block:
  key   = v1:log:block:{chain_id}:{token_address}:{block_number_be}:{log_index_be}
  value = StoredTransferLog

log_by_tx:
  key   = v1:log:tx:{chain_id}:{token_address}:{tx_hash}:{log_index_be}
  value = { block_number, block_hash }

range_manifest:
  key   = v1:range:{chain_id}:{token_address}:{from_block_be}:{to_block_be}
  value = { block_count, log_count, log_source, rpc_provider, request_id, writer_epoch, completed_at }

dead_letter:
  key   = v1:dead:{chain_id}:{token_address}:{block_number_be}:{tx_hash}:{log_index_be}
  value = malformed raw log + error
```

`block_number_be` 和 `log_index_be` 使用 big-endian bytes，保证 lexicographic order 等于数值顺序，方便 range scan。

## StoredTransferLog

```rust
pub struct StoredTransferLog {
    pub chain_id: u64,
    pub token_address: EvmAddress,
    pub block_number: u64,
    pub block_hash: BlockHash,
    pub block_timestamp: i64,
    pub tx_hash: TxHash,
    pub tx_index: Option<u64>,
    pub log_index: u64,
    pub from_address: EvmAddress,
    pub to_address: EvmAddress,
    pub amount_raw: U256,
    pub removed: bool,
    pub observed_at_ms: i64,
}
```

`removed` 默认 `false`。如果后续接 WebSocket subscription，收到 `removed=true` 只能作为提示，最终仍以 HTTP rolling rescan 和 block hash 校验为准。

## Poll 流程

```text
ensure_stream(config)

loop:
  poll_once(stream):
    1. read cursor from KVDB
    2. target = resolve ingest target block
    3. if target < cursor.next_block: return Idle
    4. from = cursor.next_block
    5. to = min(from + batch_size_blocks - 1, target)
    6. fetch block headers for [from, to]
    7. verify parent_hash continuity with last_completed_hash; first run may use start_block - 1 as anchor or skip when no previous hash exists
    8. TransferLogSource.transfer_logs(token, from, to)
    9. normalize logs, attach header timestamp as block_timestamp, verify log.block_hash == header[log.block_number].hash, and discard malformed non-Transfer data to dead_letter
    10. KV transaction:
        - upsert block_header for every block, including empty blocks
        - upsert log_by_block/log_by_tx
        - upsert range_manifest
        - set cursor.next_block = to + 1
        - set cursor.last_completed_block = to
        - set cursor.last_completed_hash = header[to].hash
    11. commit
```

空区块或没有 Transfer logs 的区块也必须写 `block_header` 和 `range_manifest`，然后推进 cursor。否则重启后无法区分“扫过但没有 logs”和“没扫过”。

`ingest target` 只决定 KVDB raw logs 采集到哪里，可以是 `latest`、`safe`、`finalized` 或 `latest - n`。是否把 payment 判为 `confirmed/paid` 由 payment scanner 按 `min_confirmations`、finality 和 canonical hash 决定，不由本模块决定。

## Log Source

MVP 必须有统一 source 抽象，避免热门 token 在 RPC 超限时只能人工改业务逻辑：

```rust
pub trait TransferLogSource {
    async fn capacity_probe(&self, stream: &TransferLogStreamConfig) -> Result<CapacityReport>;
    async fn transfer_logs(&self, token: EvmAddress, from: u64, to: u64) -> Result<Vec<TransferLog>>;
    async fn block_header(&self, block: u64) -> Result<BlockHeaderSnapshot>;
    async fn source_health(&self) -> Result<SourceHealth>;
}
```

MVP 默认实现 `RpcRangeSource`。如果 capacity probe 或运行中发现单块超限、provider cap、429/error rate 超阈值，本模块必须停止推进 cursor 并 readiness fail。接入 `IndexerSource` 时也必须输出同样的 normalized logs/header，并继续执行 block hash/reorg 校验；不能让 indexer 绕过 KV cursor 和 reorg 规则。

## RPC Filter

主路径：

- 不使用 `eth_newFilter` / `eth_getFilterChanges` 作为持久同步机制；节点 filter 是临时状态，进程重启、节点切换或断线后容易丢进度。
- 不把 WebSocket subscription 当唯一来源；WebSocket 只能做低延迟提示，最终以 HTTP `eth_getLogs` range replay 为准。
- 使用无状态 `eth_getLogs` 按 block range 拉取，cursor 由本模块自己持久化在 KVDB。

```json
{
  "address": "TOKEN_ADDRESS",
  "topics": ["0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"],
  "fromBlock": "0x...",
  "toBlock": "0x..."
}
```

说明：

- `topics[0]` 是 ERC20 `Transfer(address,address,uint256)` event signature。
- `topics[1]` 是 indexed `from`。
- `topics[2]` 是 indexed `to`。
- `data` 是 `value`。
- 本模块默认不按 `to` 过滤，因为它是 token 全量 collector；如果某个 token 无法通过 RPC 全量 range source 的 capacity gate，必须显式配置兼容的 `IndexerSource` 或等价分片 source。分片 source 可以利用 `topics[2]`，但对外仍必须保持同一 stream cursor、同一 KV schema、同一 reorg 校验语义。

如果 provider 支持 `blockHash` filter，可在 reorg 修复或单块校验时使用：

```json
{
  "address": "TOKEN_ADDRESS",
  "topics": ["TRANSFER_TOPIC"],
  "blockHash": "0x..."
}
```

`blockHash` 与 `fromBlock/toBlock` 互斥。

## Reorg 处理

本模块保存 raw scan 数据，所以 reorg 在 KVDB 内部处理，不触碰 PostgreSQL。

每次 poll 前做轻量校验：

```text
1. 读取 last_completed_block / last_completed_hash
2. 从 RPC 获取 last_completed_block 的当前 block hash
3. 如果 hash 相同，继续 next_block
4. 如果 hash 不同，从 max(start_block, last_completed_block - reorg_lookback_blocks + 1) 开始向后找第一个 hash 不同的 block
5. 删除或标记该 block 之后的 KVDB headers/logs/range_manifest，并同步清理 `log_by_tx` 二级索引
6. `cursor.reorg_epoch += 1`
7. `cursor.last_reorg_from = first_diverged_block`
8. `cursor.last_reorg_at = now`
9. cursor.next_block = first_diverged_block
10. 下一轮从 first_diverged_block 重新扫描
```

超过 `reorg_lookback_blocks` 的深度重组：

- 停止推进 cursor。
- 返回 `DeepReorgDetected`。
- 需要人工调用 `rewind_to(block)`。

为什么要保存空块 header：

- 如果某个区块当时没有 Transfer logs，但 reorg 后新分支出现了 Transfer logs，只有保存并校验区块 hash，才能知道需要回退重扫。

## RPC 边界情况

必须覆盖：

- `target < next_block`: 没有新区块，返回 `Idle`。
- RPC 返回 `query returned more than ...`、timeout、payload too large：缩小 `batch_size_blocks` 重试，最小到 1 block。
- 单块仍超限：记录 `stalled_single_block_too_large`，告警并 backoff，不推进 cursor；readiness fail，必须切换到符合 `TransferLogSource` 契约的 indexer 或分片 source。
- RPC 429/rate limit：指数退避，不推进 cursor。
- RPC provider 返回的 `chain_id` 与配置不一致：拒绝启动。
- `token_address` 非 20 bytes 或非 lowercase canonical：启动失败。
- `eth_getLogs` 返回 log 缺字段、topic 数不足、data 不是 uint256：写 dead_letter，不影响其他 logs。
- log 没有 `blockNumber`、`blockHash`、`logIndex`：如果来自 pending，不入库。
- 同一 `(block_hash, log_index)` 或 `(tx_hash, log_index)` 重复：幂等覆盖。
- provider 短暂返回旧 head：不推进到小于 `next_block` 的 target。
- KVDB 写失败：本轮失败，不推进 cursor；下轮重试同一 range。
- 进程崩溃：因为 cursor 和 logs 同 KV transaction，重启后从未提交的 `next_block` 继续。
- capacity probe 失败或最近窗口日志量超阈值：readiness fail，不推进 cursor。
- `logs_page` 命中 limit 截断同一区块：返回 `complete_to_block` 为上一个完整区块；scanner 不得把业务 cursor 推过未完整处理的区块。

配置变更规则：

- 不可变：`chain_id`、`token_address`、`start_block`、schema version。
- 可热更新：`poll_interval_ms`、`batch_size_blocks`、`max_batch_size_blocks`、`rpc_max_retries`。
- 谨慎更新：`target_mode`、`reorg_lookback_blocks`，必须记录到 stream config history；降低 lookback 前要确认 KVDB retention 足够。

保留策略：

- raw logs 和 block headers 至少保留到 `retention_floor_block` 之后。
- `retention_floor_block = min(chain_cursors.last_scanned_block - reorg_lookback, earliest active/monitor_until window_from_block, manual_rebuild_floor)`，其中 PostgreSQL 值由调用方传入清理任务。
- 不允许在 payment matcher 可能 overlap 读取的区间内清理 raw logs。
- 清理必须同时处理 `log_by_block`、`log_by_tx`、`range_manifest` 和 dead letter 关联数据。
- scanner 启动前必须验证 KVDB 覆盖 `[chain_cursors.last_scanned_block - reorg_lookback, target]`；不覆盖则暂停 scanner，先按 runbook 重建 KVDB。

## 并发和部署边界

KVDB 是本地库，不适合多机器共享写同一个 stream。

MVP 规则：

- 同一个 `(chain_id, token_address)` 只允许一个本地 ingestor 写。
- 生产部署禁止多机器共享同一个 redb 文件写入，禁止 NFS/多机共享 redb 作为写路径。
- 推荐拓扑是 `pay3-log-ingestor` 持有本地 RWO volume，并通过同进程 reader、本机 IPC 或本地 HTTP reader service 给 scanner 读取。
- 如果 API、scanner、matcher 是多个进程，不推荐共享 redb 文件；优先通过 reader service 读。
- 生产多副本要么只运行一个 ingestor，要么增加外部 lease。外部 lease 必须有 fencing token/epoch，并把 `writer_epoch` 写入 range manifest；lease 丢失后当前进程必须立即停止写 KVDB。

## 与付款匹配模块的关系

付款匹配模块不再直接 `eth_getLogs`，而是：

```text
1. 读取 TransferLogReader.cursor()
2. 按上次业务消费 offset 读取 `logs_page(limit)`
3. 提取 to_address 去重
4. watch_set 候选过滤
5. KVDB miss 批量 fallback PostgreSQL
6. 仅 matched Pay3 logs 写 PostgreSQL payments
```

业务消费 offset 可以在 PostgreSQL 保存，因为它代表“业务匹配处理到哪里”；raw logs 的存储 cursor 仍在 KVDB。

业务 matcher 还必须读取每个 log 对应 block header 的 `timestamp`，用它写 `payments.block_time` 并判断 `on_time/late/outside_window`。缺 header 时本批失败，不推进 PostgreSQL cursor。

## 测试清单

单元测试：

- 初始化 stream 后 `next_block = start_block`。
- `poll_once` 第一次从 `start_block` 扫。
- 第二次从上次 `to + 1` 扫。
- 空 logs 也推进 cursor 并保存 headers。
- 重复 logs 幂等覆盖。
- malformed logs 写 dead_letter。
- KV transaction 失败不推进 cursor。
- start_block 配置冲突报错。
- `cursor.reorg_epoch/last_reorg_from` 在 rewind 后正确更新。
- `logs_page` limit 截断时不跳过同一区块剩余 logs。
- capacity probe 超阈值时 readiness fail。

Fake RPC 测试：

- RPC timeout/backoff。
- batch 太大自动减半。
- 单块超限不推进。
- provider head 回退不推进。
- reorg: block hash 改变后 rewind 并重扫。
- reorg 后空块变成有 Transfer log，能被重扫发现。
- writer lease/fencing token 变化后旧 writer 停止写入。

Anvil 测试：

- 部署 mock ERC20。
- 从指定 block 开始收集 Transfer events。
- 多笔 Transfer 同块、多块、无 logs 区块。
- 重启进程后继续从 `next_block` 扫。

集成测试：

- `transfer_log_store` 写 KVDB。
- `services/payments` 只通过 `TransferLogReader` 读取，不直接调 RPC。
- 删除 KVDB 后能从 `start_block` 或人工 rewind block 重建 raw logs。
- scanner 发现 KV reorg epoch 变化后回退 PostgreSQL business cursor。
- KVDB 覆盖不足时 scanner readiness fail。
