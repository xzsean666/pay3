# TASK-011: 订单创建与查询服务及 API 路由

## Objective
实现订单业务服务与 HTTP 接口，包括订单请求 Hash 计算、幂等检查、HD 地址分配、支付窗口写入以及按 ID 和外部订单号查询接口。

## Scope
- `OrderService`：标准化 `request_hash`、原子获取钱包游标、地址派生、开启事务落盘订单与支付窗口。
- API Routes：`POST /v1/orders`、`GET /v1/orders/{id}`、`GET /v1/orders/by-external-id/{external_id}`。
- 绑定 `orders:create` 与 `orders:read` JWT Scope 校验。
- 订单请求与响应 DTO，金额字符串高精度解析。

## Allowed Files
- `src/services/orders.rs`
- `src/api/mod.rs`
- `src/api/routes.rs` (若有)
- `src/api/dto.rs` (若有)

## Dependencies
- TASK-003
- TASK-005
- TASK-006

## Inputs and Outputs
- **Inputs**: HTTP POST 订单创建 JSON 载荷（`amount`、`external_id` 等）。
- **Outputs**: 包含收款地址、过期时间、订单状态的 JSON 响应，成功创建返回 `201`，幂等返回 `200`，冲突返回 `409`。

## Acceptance Criteria
- 相同 `external_id` 重复请求返回幂等结果。
- 相同 `external_id` 传递不同参数返回 `409 Conflict`。
- 每个新订单必须分配全局唯一的子收款地址。

## Verification Commands
```bash
cargo test services::orders
cargo test api::
```

## Risks and Assumptions
- 外部订单号 `external_id` 假定长度不超过 128 字符。

## Status
DONE
