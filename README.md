# Pay3

Rust ERC20 token 收款平台。MVP 目标是用单一链、单一 token、单一默认账号跑通完整收款流程：

```text
创建订单 -> 派生收款地址 -> 用户 ERC20 转账 -> 验证付款成功 -> token collect
```

当前仓库已经进入 Rust 实现阶段，已完成基础配置/health/JWT/domain、PostgreSQL migration/repository 初版、HD wallet 地址派生边界、signer contract/fake、订单创建 service 初版、订单 API route contract、chain 纯契约/fake、transfer log store 原型、付款匹配纯 service、手动 verify service/API route contract，以及 scanner worker tick contract 初版。

生产可用性：当前仓库仍不可用于生产接真实资金。真实 RPC provider manager、订单 API 的真实 DB/RPC runtime wiring、完整 redb-backed ingestor runtime、manual verify 真实 runtime wiring、scanner runtime loop/confirmation sweep/readiness、collect worker、真实 DB 集成测试、部署工件和 runbook 演练还没有完成。MVP 出口标准已经包含 reorg/finality、collect 崩溃恢复、外部 signer、监控告警、备份恢复、runbook 演练和 e2e 测试；这些不是后续补项。

生产优先约束：每个订单使用新收款地址且永不复用；归集目标固定为 treasury；production profile 不包含 mnemonic/private key/local signer；RPC provider 至少两个；KVDB raw log store 单写者。

地址派生：使用 `account_index/change_index/address_index` 分段 rollover，对 API 表现为无限派生地址池。

扫链数据分工：PostgreSQL 只保存订单、派生地址、matched Pay3 payments、业务 cursor、归集和 outbound tx；raw Transfer logs、非 Pay3 logs、raw scan batches、block header cache 只进 KVDB 或内存。

当前验证：`cargo fmt -- --check`、`cargo check`、`cargo test` 均通过。最近一次全量测试为 97 个库测试 + 53 个 integration/contract 测试。

文档入口：

- `Agent.md`: AI/开发协作规则和 MVP 边界。
- `docs/MVP_ARCHITECTURE.md`: MVP 架构、API、数据库、模块和测试设计。
- `docs/END_TO_END_FLOW.md`: 从订单到付款、归集的整体流程。
- `docs/MODULE_PLAN.md`: 按模块实现、测试和联调的实施规范。
- `docs/TRANSFER_LOG_KV_MODULE.md`: 独立 ERC20 Transfer log KVDB 采集模块。
- `docs/PRODUCTION_READINESS.md`: MVP 生产验收审计和上线清单。
- `docs/DEPLOYMENT.md`: 部署拓扑、worker 锁、RPC provider 和 readiness 要求。
- `docs/RUNBOOK.md`: RPC、reorg、collection、KVDB rebuild、DB 恢复等 MVP runbook。
- `nextsession.md`: 下一次 session 的任务交接和全局进度。
