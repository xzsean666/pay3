# TASK-012: 手动付款验证服务与 API 路由

## Objective
提供主动触发的手动付款验证接口，在后台扫链延迟或客户端急需结果时，通过指定窗口读取 KV 日志并立即执行重算与入库。

## Scope
- `ManualOrderVerifyService`：按订单支付窗口拉取 KV 日志并复用纯匹配函数。
- `VerifiedPaymentRecorder`：将验证到的匹配记录原子写入 PostgreSQL 并更新订单状态。
- API Route：`POST /v1/orders/{id}/verify`，绑定 `orders:verify` Scope。

## Allowed Files
- `src/services/verify/**`
- `src/api/verify.rs`
- `src/api/verify_service.rs`
- `tests/manual_verify_service_contract.rs`
- `tests/order_verify_api_contract.rs`

## Dependencies
- TASK-005
- TASK-009
- TASK-010

## Inputs and Outputs
- **Inputs**: HTTP POST 订单 ID。
- **Outputs**: 验证后的最新订单视图与匹配付款明细。

## Acceptance Criteria
- 严格限定仅扫描对应订单的有效支付窗口区块范围，禁止全表/全链扫。
- 幂等执行，已记录的付款不会重复累计。
- 契约与 API 路由测试全部通过。

## Verification Commands
```bash
cargo test --test manual_verify_service_contract
cargo test --test order_verify_api_contract
```

## Risks and Assumptions
- 若 KV 覆盖范围滞后于订单创建高度，应返回明确的状态提示。

## Status
DONE
