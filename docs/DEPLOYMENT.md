# Pay3 Deployment Notes

## 当前状态

本文是 MVP 生产候选部署要求，不代表当前项目已经可生产。当前仓库仍处于规划阶段，没有 Rust 实现和部署工件。

## 组件拓扑

- `pay3-api`: HTTP API，只处理鉴权、DTO、service 调用。
- `pay3-log-ingestor`: 独立 ERC20 Transfer log KVDB 采集 worker，按 `(chain_id, token_address)` 从配置的 `start_block` 开始轮询并写本地 KVDB；每个 stream 只能一个 writer。
- `pay3-scanner`: 业务付款匹配 worker，读取 KVDB Transfer logs，按 `(chain_id, token_address)` 通过 PostgreSQL cursor lease 保证单活。
- `pay3-collector`: 归集 worker，可多副本；必须通过 `collections FOR UPDATE SKIP LOCKED` 抢 job，并通过 `account_nonces` 对每个 from address 串行 nonce。
- `postgres`: 资金状态唯一真相源，必须启用 PITR/WAL 备份。
- `redb/KVDB`: log ingestor 本地可重建缓存，只保存 raw scan batches、block header cache、Transfer logs、非 Pay3 logs、watch set snapshot；可重建，不参与资金恢复真相。
- `signer`: KMS/HSM/外部签名服务，生产不得使用本地 mnemonic/private key。

## Worker 锁

- scanner 使用 `chain_cursors.lease_owner/lease_until`，claim 后链外扫描，提交前 CAS 校验 cursor 未变化。
- scanner 还必须校验 `seen_kv_reorg_epoch`，KV epoch 变化时先回退业务 cursor。
- log ingestor 的原始扫描结果如需落盘只能写本地 KVDB；PostgreSQL 只接收 matched Pay3 payments、订单重算和 payment matcher cursor 推进。
- scanner 不直接调用 `eth_getLogs`，只读取 `transfer_log_store::TransferLogReader`。
- log ingestor 多副本必须使用外部 lease + fencing token；`writer_epoch` 写入 range manifest。lease 丢失立即停止写。
- collector 使用 job lock + `account_nonces` 行锁。
- 任何 worker graceful shutdown 必须停止领取新任务，完成或释放当前 lease。

## KVDB 部署边界

- redb 写路径只能使用本地 RWO volume。
- 禁止 NFS/多机器共享同一个 redb 文件作为写路径。
- 多进程读取优先通过 `pay3-log-ingestor` 本机 IPC/HTTP reader service，不依赖共享文件。
- `/readyz` 必须检查 redb 可读写、schema version、stream config、磁盘空间、single-writer/fencing epoch。
- KVDB 丢失后，先按 runbook 从 `min(last_scanned_block - lookback, earliest_unsettled_window_from_block)` 重建，再恢复 scanner。

## RPC Provider

- 生产必须配置至少两个 HTTP RPC provider。
- WebSocket 只做低延迟提示，断线后必须用 HTTP 按 PostgreSQL cursor 补扫。
- provider 健康检查至少包含 chain id、latest/safe/finalized head、block hash、latency、429/error rate。
- provider block hash 不一致时，log ingestor 和 scanner readiness 必须 fail，并停止推进 cursor。
- RPC range source 必须通过 capacity probe；未通过时拒绝为该 token 进入 ready，除非配置了兼容 `TransferLogSource` 的 indexer/分片 source。

## 发布和迁移

- migration 必须先在 staging dry-run。
- DB schema 变更必须兼容当前 API/worker 旧版本，支持滚动发布。
- 发布顺序：migration -> API -> log-ingestor -> scanner -> collector。
- 回滚前确认新旧版本对 `outbound_transactions`、`payments` 状态语义兼容。
- 回滚前确认 KV schema version、reorg epoch、writer_epoch 和 `chain_cursors.seen_kv_reorg_epoch` 兼容。

## Readiness

`/readyz` 只能内网访问，必须检查：

- DB 可连接。
- migration version 正确。
- chain id 和 token contract 匹配配置。
- signer 可用。
- RPC provider 配置数量 >= 2，至少一个可服务，主备 chain_id 一致，safe/finalized head/hash 在允许窗口内一致。
- RPC capacity gate 通过。
- KVDB 可读写、schema version 匹配、stream config 与当前配置一致、磁盘空间满足阈值、single-writer/fencing epoch 有效。
- log ingestor lag 和 payment scanner lag 未超过阈值。
- worker lease 状态可读。
- signer 可用，且 production profile 不允许 local/fake signer。

## 生产配置禁用项

- 禁止明文 JWT secret。
- 禁止单 RPC provider。
- 禁止 local signer。
- 禁止 mnemonic/private key。
- 禁止 `SCAN_FROM_BLOCK=0`。
- 禁止 treasury 为空、零地址或等于任一 child address。
- 禁止 API collection request 接受 `to_address`。
- 禁止 redb 多机共享写路径。

## DB 备份和恢复

- PostgreSQL 必须启用 WAL archiving/PITR。
- dashboard 必须显示最近 base backup 时间、WAL archive 延迟、备份失败计数。
- staging 必须完成 PITR restore drill，并记录 RTO/RPO。
- migration 必须有 dry-run、rollback 验证和旧版本兼容说明。
- 备份必须加密并限制访问权限。
