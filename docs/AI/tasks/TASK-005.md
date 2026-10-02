# TASK-005: 数据库仓储层 (Repositories) 实现

## Objective
实现基于 SQLx 的仓储层，提供强事务保障的 Order、Payment、Collection、Outbound 和 Audit 操作接口，实现原子锁与并发 CAS。

## Scope
- `OrderRepository`：订单创建、幂等检查、过期订单批量重算。
- `PaymentRepository`：游标 Lease 申请与 CAS 提交、付款匹配 upsert 与重算。
- `CollectionRepository`：归集任务创建、状态更新与关联查询。
- `OutboundRepository`：Nonce 串行预留、已签名交易持久化、崩溃恢复捞取。
- `AuditRepository`：关键资金操作审计事件持久化。

## Allowed Files
- `src/db/repositories/**`
- `tests/repository_contract.rs`

## Dependencies
- TASK-004

## Inputs and Outputs
- **Inputs**: 业务领域命令与事务上下文。
- **Outputs**: 持久化的领域实体与强类型结果。

## Acceptance Criteria
- 唯一索引冲突能正确转换为领域层定义的 `DuplicateExternalId` 等语义化错误。
- 扫链游标更新支持 CAS 乐观锁防并发覆盖。
- 仓储契约测试全量通过。

## Verification Commands
```bash
cargo test --test repository_contract
```

## Risks and Assumptions
- 单测使用契约模拟，真实并发集成测试需配合 PostgreSQL 运行。

## Status
DONE
