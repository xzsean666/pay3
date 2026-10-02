# TASK-009: 独立 Transfer 日志 KV 存储 (redb) 与 Ingestor

## Objective
实现基于嵌入式 KV 引擎 `redb` 的独立 Transfer 日志存储层，提供原始日志持久化、空块索引、独占分页读取、Reorg 回滚与后台轮询拉取循环。

## Scope
- 数据表设计：Stream 配置、KV 游标、区块头、Transfer Logs、Range Manifests。
- `RedbTransferLogIngestor`：实现日志拉取落盘与游标推进。
- `RedbTransferLogReader`：实现有界 `logs_in_range` 与独占分页 `logs_page`。
- 分叉回滚（Rewind）：删除分叉块及后续记录，递增 `reorg_epoch`。
- `workers/transfer_log_ingestor` 后台轮询调度。

## Allowed Files
- `src/transfer_log_store/**`
- `src/workers/transfer_log_ingestor.rs`
- `tests/transfer_log_redb_contract.rs`
- `tests/transfer_log_store_types_contract.rs`

## Dependencies
- TASK-008

## Inputs and Outputs
- **Inputs**: 链上 Transfer Logs 与区块头信息。
- **Outputs**: 本地持久化的 redb 数据库文件及有序日志流。

## Acceptance Criteria
- 严格禁止无界日志读取，主路径必须采用限定大小的 `logs_page(limit)`。
- Reorg 发生时能准确清理分叉记录并更新 `reorg_epoch`。
- 数据库意外关闭重开后能正确恢复游标。

## Verification Commands
```bash
cargo test --test transfer_log_redb_contract
cargo test --test transfer_log_store_types_contract
```

## Risks and Assumptions
- 单个 `(chain_id, token_address)` 仅允许单个写进程访问 redb 文件。

## Status
DONE
