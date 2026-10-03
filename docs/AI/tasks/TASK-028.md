# TASK-028: 全面审计缺陷修复与生产架构优化 (Audit Findings Remediation & Codebase Cleanup)

## Objective
基于全面生产审计结论，系统修复与优化 4 项安全、业务与整洁度缺陷：
1. **[Medium] 卡死归集交易替换时的 Gas 费用计算潜在 Bug**:
   - 在 `outbound_transactions` 表中增加 `gas_limit`、`max_fee_per_gas`、`max_priority_fee_per_gas` 持久化字段；
   - 改造 `replace_collection_job`：确保新替换交易的费率同时满足：(a) 至少高于上一笔卡死交易费率的 110%；(b) 不低于当前网络预估费率；严格杜绝网络 Gas 回落时触发 `replacement transaction underpriced` 报错；
   - 在 `CollectionTxPlan::assert_replacement_allowed` 中增加费率严格上浮断言。
2. **[Low/Medium] `POST /v1/orders/{id}/verify` 增加租户归属校验**:
   - 在 `OrderVerifyApiService` 与 `ManualOrderVerifyService` 中引入 `owner_sub` 租户归属约束；
   - API 端点提取 JWT `principal.subject` 传递校验，杜绝跨租户越权核验他人物流与支付信息。
3. **[Low] EVM ERC-20 Topic Address 解析高位 12 字节补零校验**:
   - 在 `parse_topic_address` 中严格校验前 12 字节（24 个字符）必须全为 `'0'`，防止恶意非对齐 ABI 注入。
4. **[Informational] 彻底清理移除 `redb` 后的遗留 KVDB 结构体与指标代码**:
   - 清理 `health.rs` 中的 `DependencyName::Kvdb`、KVDB 遗留 Prometheus 导出项以及 `api/mod.rs` 中的硬编码遗留。

## Scope
- `src/db/migrations/20261003000100_outbound_gas_fees.sql` (新建迁移)
- `src/db/repositories/types.rs`
- `src/db/repositories/outbound.rs`
- `src/domain/collection.rs`
- `src/services/collections.rs`
- `src/api/verify.rs`
- `src/api/verify_service.rs`
- `src/services/verify/service.rs`
- `src/chain/rpc.rs`
- `src/health.rs`
- `src/api/mod.rs`
- `tests/order_verify_api_contract.rs`
- `tests/migration_contract.rs`
- `tests/collector_recovery_integration.rs`

## Allowed Files
- 所有 Scope 清单内的文件及相关文档。

## Dependencies
- TASK-027

## Acceptance Criteria
1. `replace_collection_job` 产生的替换交易，其 `max_fee_per_gas` 与 `max_priority_fee_per_gas` 均严格 $\ge 110\%$ 原交易费率，且 $\ge$ 当前网络费率；
2. 非当前商户调用 `POST /v1/orders/{id}/verify` 时返回 404 Not Found，实现多租户严密隔离；
3. `parse_topic_address` 对高位非 0 的异常 Topic 抛出 `ChainError::malformed_rpc_response`；
4. KVDB 遗留死代码与空 Gauge 指标彻底移除；
5. 全量单元测试、集成测试与生产门禁脚本 100% 通过。

## Verification Commands
```bash
cargo check --all-targets
cargo test
bash scripts/verify_production_readiness.sh --env-file .env.production.example
```

## Status
DONE

## Completion Summary
- [x] Finding 1: 增加了数据库迁移 `20261003000100_outbound_gas_fees.sql`，在 `outbound_transactions` 表中持久化保存 `gas_limit`、`max_fee_per_gas`、`max_priority_fee_per_gas`；在 `replace_collection_job` 中严格计算 $\max(1.1 \times \text{previous\_fees}, \text{current\_network\_fees})$，并在 `CollectionTxPlan::assert_replacement_allowed` 中强制校验费率严格上浮，彻底杜绝 EIP-1559 替换交易因网络 Gas 回落而被节点拒绝 (`underpriced`) 的问题；
- [x] Finding 2: `POST /v1/orders/{id}/verify` 端点与底层服务增加 `owner_sub` 多租户隔离校验，禁止越权核验他人物流与订单，非法跨租户请求统一安全返回 404 Not Found；
- [x] Finding 3: `parse_topic_address` 增加了高位 12 字节全零校验，杜绝非标准 ABI Topic 导致错误解析；
- [x] Finding 4: 清理了移除 KVDB 后遗留在 `health.rs`、`api/mod.rs`、`workers/scanner.rs` 中的死代码、无用指标和错误变体；
- [x] 全量回归验证：`cargo test` 194/194 全部通过，`verify_production_readiness.sh` 生产审计门禁通过（0 failed gates）。
