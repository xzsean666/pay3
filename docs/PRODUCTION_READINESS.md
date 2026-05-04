# Pay3 MVP 生产验收审计

## 结论

当前仓库不能用于生产环境，因为虽然 Rust 实现、migration 和大部分测试已经有了，但部署工件、告警 dry-run 和演练记录还没有闭环。

但本文列出的内容不再是“生产前再补”的后续项。它们全部是 MVP 出口验收标准：只有实现、测试、部署演练全部通过后，Pay3 MVP 才能作为单链、单 token、单默认账号的生产候选版本接真实资金。

## MVP 阻塞验收项

### 地址不复用

ERC20 转账没有订单号或 memo。用户如果在地址被复用后才付款，链上无法证明这笔转账属于旧订单还是新订单。

MVP 必须：

- 不实现任何地址复用配置或分支。
- 每个订单必须派生新地址。
- `receive_address` 在数据库中全局唯一，并永久绑定原订单。
- late/outside_window payment 进入原订单对账或人工处理，不分配给新订单。

### 固定 Treasury 归集

`POST /v1/collections` 不得接受任意 `to_address` 作为最终归集地址。

MVP 必须：

- 归集目标固定为 `TREASURY_ADDRESS`。
- API request schema 不包含 `to_address`，传入也必须拒绝。
- repository 拒绝非 treasury `to_address`。
- DB 通过 `treasury_addresses` FK 或 trigger 拒绝直接 SQL 插入非 treasury collection。
- collection API 使用独立 scope：`collections:create`。
- 所有归集 job 写审计日志：调用方、request id、from address、to address、amount、nonce、tx hash。

### 外部 Signer 和密钥边界

生产 profile 不能把 mnemonic、子私钥或 gas wallet private key 明文放在 env、日志、配置文件、PostgreSQL 或 redb。

MVP 必须：

- 通过 `SignerProvider` trait 隔离签名。
- production profile 使用 KMS/HSM/外部签名服务，禁止 local/fake signer。
- 开发/测试 profile 如需本地 mnemonic，只能在独立 test profile。
- config guard 拒绝 local signer、mnemonic、private key、明文 gas key。
- signer contract test 覆盖 health、timeout、key_ref 不存在、签名结果 tx_hash 校验、审计 request_id。
- 日志、错误、panic report 脱敏 private key、mnemonic、JWT、DB URL。
- signer outage runbook 演练通过。

### Reorg 和 Finality

仅用确认数不足以让资金状态生产安全。系统必须能检测 canonical chain 变化并回滚孤块日志。

MVP 必须：

- `transfer_log_store` 把 raw Transfer logs、block headers、range manifest 写入 KVDB。
- PostgreSQL 不保存全量区块头表或 raw logs，只保存 matched Pay3 payments 的 `block_number/block_hash/log_index`。
- `transfer_log_store` 每轮滚动校验 block hash，发现 reorg 时 rewind KV cursor 并增加 `reorg_epoch`。
- KV cursor 保存 `reorg_epoch/last_reorg_from/last_reorg_at`。
- PostgreSQL `chain_cursors` 保存 `seen_kv_reorg_epoch`。
- payment scanner 发现 KV epoch 变化时回退业务 cursor、标记 orphaned、重算订单。
- payment scanner 只通过 `TransferLogReader` 读 logs，禁止直接 `eth_getLogs`。
- payment scanner 可通过 `ChainHeaderReader` 读取 latest/safe/finalized head 和 block hash，用于 confirmation sweep。
- 超过 `REORG_LOOKBACK_BLOCKS` 的深度 reorg 必须暂停 cursor 推进，人工确认后从指定 block 重扫。

### Transfer Log KVDB

Raw scan data 属于 KVDB，不属于 PostgreSQL。

MVP 必须：

- 输入 `chain_id/token_address/start_block`，第一次从 `start_block` 扫，之后从 KV `next_block` 扫。
- 空 logs 区块也保存 header/range manifest 并推进 KV cursor。
- KV transaction 同时写 logs/headers/range manifest 和 cursor。
- reader 主路径使用 `logs_page(limit)`，禁止无界 `logs_in_range`。
- `StoredTransferLog` 或配套 header reader 提供 block timestamp，付款窗口不能用本地观测时间判断。
- retention floor 覆盖 reorg lookback、活跃窗口、manual verify/rebuild 需求。
- scanner 启动前验证 KVDB 覆盖 `[last_scanned_block - lookback, target]`，不足则暂停并重建。
- 单个 `(chain_id, token_address)` 只有一个 writer；禁止 NFS/多机共享 redb 写路径。
- 外部 lease 必须有 fencing token/epoch，写入 range manifest。

### 大日志量和 RPC/Indexer Gate

订单量增长不能让 RPC 请求按订单数线性增长，但热门 token 的全量 Transfer logs 可能超过普通 RPC 能力。

MVP 必须：

- `chain` 模块实现 RPC provider manager：至少两个 HTTP provider、chain_id 校验、safe/finalized head/hash 检查、429/timeout failover。
- provider block hash 冲突时停止推进 cursor 并告警。
- `transfer_log_store` 提供 `TransferLogSource` trait。
- 默认 `RpcRangeSource` 必须有 capacity probe。
- 如果当前 token 最近 N 块日志量、单块日志量或 provider cap 超阈值，log ingestor readiness fail，不推进 cursor。
- 需要 indexer 或 `topics[2]` 分片时，必须实现兼容 `TransferLogSource` 的 source，输出同一 normalized log/header，并继续走 KV cursor 和 reorg 校验。
- payment matcher 对每批 unique `to_address` 使用临时表/批量 join，设置 `max_unique_to_addresses_per_batch` 和 `max_db_fallback_addresses`；超阈值不推进 cursor。

### Collect 崩溃恢复

如果进程在广播交易成功后、写入 tx hash 前崩溃，简单重试可能重复发送交易。

MVP 必须：

- 广播前持久化 `chain_id/from_address/nonce/signed_tx/tx_hash`。
- 重试优先查 receipt 或重播同一 signed transaction。
- 每个发送地址串行 nonce lock。
- `account_nonces` 和 `outbound_transactions` 是必需表。
- dropped/stuck tx replacement 必须保留原 outbound row，把旧 row 标记 `replaced`，再插入同 nonce 新 signed tx。
- replacement 不能改变 from/to/treasury/purpose/业务金额。
- collector 广播前、广播后崩溃恢复测试通过。

### API 幂等、错误模型和 JWT

MVP API 必须让调用方安全重试，并能明确区分认证、权限、幂等和依赖故障。

MVP 必须：

- `POST /v1/orders` 使用 `external_id + request_hash` 幂等。
- `POST /v1/collections` 使用 `idempotency_key + request_hash` 幂等。
- 所有错误统一返回 `code/message/request_id/retryable/details`。
- JWT 强制校验 `exp/nbf/iat/iss/aud/sub/kid/alg`。
- 所有 `/v1/*` endpoint 强制 endpoint scope。
- `collections:create` 不给普通业务 JWT。
- 401/403/409/422/429/503 typed error 测试通过。
- production profile 使用 RS256/EdDSA + JWKS + `kid`；HS256 只允许 dev/test profile。

## DB 强约束

MVP migration 必须让直接 SQL 破坏性插入失败，而不只依赖 service 代码：

- `child_accounts` 有 path 唯一约束和 `(id,address)` 组合唯一。
- `orders.receive_address` 全局唯一。
- `orders/payment_windows/payments/collections/outbound_transactions` 对 address/hash 有 regex `CHECK`。
- `orders` 和 `payment_windows` 使用 composite FK 保证 `child_account_id/address` 一致。
- `payments` 带 `child_account_id`，并用 `(order_id, chain_id, token_address, child_account_id, to_address)` FK 到订单收款地址。
- `collections` 带 `chain_id/token_address`，并 FK 到 `treasury_addresses`。
- 同一 `child_account_id` 只能有一个 active collection。
- `outbound_transactions` 对 active nonce 建 partial unique index，允许 replacement 轨迹但禁止两个 active 同 nonce。
- trigger 或等价约束保证 collection outbound 的 `purpose/from/to/chain` 与 collection 一致。

## 可观测性和演练

MVP 必须具备：

- `/metrics` Prometheus 指标。
- `/readyz` 严格依赖检查。
- 结构化日志：`request_id`、`order_id`、`external_id`、`chain_id`、`block_number`、`tx_hash`、`worker`、`duration_ms`。
- `audit_events` 资金审计表。
- dashboard 和告警 dry-run。
- runbook 演练记录。

核心告警：

| Alert | 指标 | 阈值 | Runbook |
| --- | --- | --- | --- |
| LogIngestorLag | `pay3_log_ingestor_lag_blocks` | `> MIN_CONFIRMATIONS * 2` 5m | `docs/RUNBOOK.md#rpc-provider-故障` |
| PaymentScannerLag | `pay3_payment_scanner_lag_blocks` | `> REORG_LOOKBACK_BLOCKS` 5m | `docs/RUNBOOK.md#reorg` |
| RpcErrorRate | `pay3_rpc_errors_total` rate | `> 5%` 5m | `docs/RUNBOOK.md#rpc-provider-故障` |
| ProviderHashMismatch | provider head/hash health | any mismatch | `docs/RUNBOOK.md#rpc-provider-故障` |
| CollectionStuck | `pay3_collections_by_status` | transferring/confirming > 30m | `docs/RUNBOOK.md#stuck-collection` |
| SignerFailures | `pay3_signer_errors_total` | 3 consecutive failures | `docs/RUNBOOK.md#signer-故障` |
| PrefundedGasLow | `pay3_prefunded_gas_low_total` | gas < 3 collect tx | `docs/RUNBOOK.md#stuck-collection` |
| LateOrOutsideWindowPayment | `pay3_payment_events_total` | any late/outside_window | `docs/RUNBOOK.md#人工对账` |

## MVP 生产候选清单

全部满足后，才可以评估接真实资金：

- Rust 服务实现完成，所有 endpoint 和 worker 可运行。
- `docs/MODULE_PLAN.md` 每个 phase 的独立测试和组合测试完成。
- PostgreSQL migrations 完成，并通过并发一致性和 DB 负例约束测试。
- Anvil e2e 覆盖创建订单、付款、确认、归集。
- reorg/orphan payment 测试通过。
- KV reorg epoch -> PG cursor rewind 测试通过。
- scanner crash/resume 测试通过。
- collect crash/replay/replacement 测试通过。
- JWT claims/scope、签名轮换、错误模型测试通过。
- `/readyz` 依赖失败测试通过。
- `/metrics`、结构化日志、告警 dry-run 完成。
- external signer/KMS contract test 通过，明文 mnemonic 禁用。
- RPC provider manager 和 capacity gate 通过。
- DB PITR 恢复、migration rollback、RPC 切换、signer 故障、stuck collection、KVDB rebuild 演练完成并记录。
- production config guard 通过：禁止明文 JWT secret、单 RPC provider、local signer、`SCAN_FROM_BLOCK=0`、地址复用、非 treasury collect。

## 当前建议

下一步做 Rust MVP 代码时，必须按本文验收项实现和测试。不要先做多商户、多链、多 token、后台或前端；先把单链单币真实资金闭环做成可恢复、可观测、可审计。
