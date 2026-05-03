# Pay3 MVP Runbook

## 当前状态

这是 MVP 生产候选必须随代码一起完成和演练的 runbook。当前仓库还没有 Rust 实现，所以演练状态为未开始。

## 告警阈值

- `pay3_log_ingestor_lag_blocks > MIN_CONFIRMATIONS * 2` 持续 5 分钟：warning。
- `pay3_payment_scanner_lag_blocks > REORG_LOOKBACK_BLOCKS` 持续 5 分钟：critical。
- RPC error rate > 5% 持续 5 分钟：warning。
- RPC primary/secondary safe/finalized head hash 冲突：critical，暂停 cursor 推进。
- KVDB single-writer fencing epoch 冲突：critical，暂停 log ingestor。
- collection `transferring/confirming` 超过 30 分钟：warning。
- collection failed、dropped 或 replacement 失败：critical，需要人工确认 tx hash 和 nonce。
- signer 连续失败超过 3 次：critical，暂停 collector。
- prefunded gas 余额低于 3 次 collect 估算 gas：warning。
- late/outside_window payment 出现：warning，进入人工对账。

## RPC Provider 故障

1. 查看 provider health、chain_id、safe/finalized head、block hash、429/error rate。
2. 如果 provider hash 冲突，暂停 `pay3-log-ingestor` 和 `pay3-scanner` 推进 cursor。
3. 切到健康 provider，但不得跳过 KV cursor 或 PostgreSQL cursor。
4. 从 `transfer_log_store.next_block - REORG_LOOKBACK_BLOCKS` 开始校验 block hash。
5. 确认一致后恢复 log ingestor。
6. 等 KVDB logs 覆盖 scanner 所需区间后，再恢复 scanner lease。
7. 记录切换时间、受影响 block range、最终 `chain_cursors.last_scanned_block`。

Pass 标准：cursor 不跳跃，`pay3_log_ingestor_lag_blocks` 和 `pay3_payment_scanner_lag_blocks` 恢复到阈值内，无重复 collect/outbound tx。

## Reorg

1. 暂停 log ingestor 和 payment scanner 推进。
2. 从 RPC 重拉最近 `REORG_LOOKBACK_BLOCKS` 的 block hash；KVDB block header cache 只能加速，不能作为唯一依据。
3. 与 PostgreSQL matched payments 的 `block_number/block_hash` 对比。
4. 调用 `transfer_log_store.rewind_to(reorg_safe_block)`；KV cursor 必须增加 `reorg_epoch` 并写 `last_reorg_from`。
5. scanner 发现新 KV epoch 后，回退 `chain_cursors.last_scanned_block`，更新 `seen_kv_reorg_epoch`。
6. 标记受影响 payments 为 `orphaned`。
7. 按 order id 排序重算订单状态。
8. 从 reorg-safe block 继续扫描并推进 PostgreSQL `chain_cursors`。

Pass 标准：orphaned payments 不再计入 paid，reorg 后新分支同区间 Pay3 payment 能被 overlap 重扫发现。

## KVDB Rebuild

触发场景：redb 损坏、丢失、retention 误清理、schema migration 失败。

1. 暂停 `pay3-scanner`。
2. 读取 PostgreSQL `chain_cursors.last_scanned_block` 和最早未结订单 `payment_windows.window_from_block`。
3. 计算 `rebuild_from = min(last_scanned_block - REORG_LOOKBACK_BLOCKS, earliest_unsettled_window_from_block)`。
4. 清理或隔离旧 redb 文件。
5. 调用 `transfer_log_store.ensure_stream(chain_id, token_address, start_block)`。
6. 调用 `rewind_to(rebuild_from)` 并从该块重扫 raw Transfer logs。
7. 等 KVDB 覆盖 `[rebuild_from, target]` 后恢复 scanner。
8. scanner 用幂等 upsert 重扫，只写 matched Pay3 payments。

Pass 标准：KVDB 重建期间 PostgreSQL 资金状态不丢失；恢复后 scanner 不漏、不重复推进 cursor。

## Hot Token / Capacity Gate

1. 查看 `TransferLogSource.capacity_probe` 报告：最近 N 块 logs 数、单块最大 logs、provider cap、429/error rate。
2. 如果 `RpcRangeSource` 超阈值，保持 log ingestor not ready，不推进 cursor。
3. 配置兼容 `TransferLogSource` 的 indexer/分片 source。
4. 在 staging 对同一区间比对 RPC/indexer normalized logs、headers、block hash。
5. 比对通过后切换 source，并记录 source version、起始 block、校验 hash。

Pass 标准：切换 source 不改变 KV schema、不绕过 reorg 校验、不产生 PostgreSQL raw logs 表。

## Stuck Collection

1. 查询 `collections.outbound_tx_id`。
2. 查询 `outbound_transactions.tx_hash` receipt。
3. 如果未广播，重播同一 `signed_tx`。
4. 如果 pending 超过阈值，进入 replacement 流程。
5. replacement 必须使用同一 `from_address/to_address/nonce/purpose/token transfer calldata`，只提高费用。
6. 在一个 DB 事务内把旧 outbound 标记 `replaced`，插入新 outbound，更新 collection `outbound_tx_id`。
7. 广播新 signed tx。
8. 如果旧 tx 和新 tx 出现异常 receipt，人工核对 treasury token balance 和两个 tx receipt，再更新状态。

Pass 标准：同一 nonce 只有一个 active outbound；所有 replacement 有 `replacement_of` 链路；不允许重新构造未关联原 job 的交易。

## Signer 故障

1. 暂停 collector 领取新 job。
2. 检查 signer health、key_ref、rate limit、审计日志。
3. 对已经 `signed/broadcast` 的 outbound 优先查 receipt 或重播原 signed tx。
4. signer 恢复后先跑 health check 和小额签名 contract test。
5. 恢复 collector。

Pass 标准：故障期间不生成未落库 signed tx；恢复后 nonce 连续，审计日志完整。

## DB 恢复

1. 使用 PITR 恢复到目标时间。
2. 校验 migration version。
3. 校验 PostgreSQL `chain_cursors` 和 matched payments 的最近 block refs。
4. 从 `last_scanned_block - REORG_LOOKBACK_BLOCKS` 通过 `transfer_log_store` 重拉 headers 和 Transfer logs，重建 KVDB scan cache。
5. 幂等重扫该窗口，只把 matched Pay3 payments 写回 PostgreSQL。
6. collector 恢复前先对所有 pending outbound tx 查 receipt。
7. 记录 RTO、RPO、恢复点、最后校验 block。

Pass 标准：PITR 后订单、payments、nonces、signed_tx 和 audit_events 一致；collector 不重复发送新 nonce。

## 人工对账

触发：late/outside_window payment、reorg 深度超过 lookback、replacement 异常、treasury balance 对不上。

记录内容：

- order_id、receive_address、tx_hash、log_index、block_number、block_hash。
- match_status、chain_status、expected_amount、paid_amount。
- scanner cursor、KV reorg_epoch、处理人、处理结论。

## 上线前演练

必须完成并记录：

- migration dry-run 和 rollback。
- DB PITR 恢复。
- RPC provider 切换。
- KVDB rebuild。
- log source capacity gate / indexer source 切换。
- scanner crash/resume。
- collector 广播前/广播后崩溃恢复。
- dropped/stuck collection replacement。
- signer 故障。
- reorg/orphan payment 重算。
- readyz 依赖失败。
- metrics/alert dry-run。

## 演练记录模板

```text
Drill:
Date:
Environment:
Operator:
Start time:
End time:
RTO:
RPO:
Initial state:
Steps executed:
Expected result:
Actual result:
Metrics checked:
Logs/audit events checked:
Pass/Fail:
Follow-up:
```
