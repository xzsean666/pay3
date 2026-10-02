# TASK-014: 资金归集服务与 Outbound 准备

## Objective
实现资金归集 Service 与 API 路由，确保归集目标地址仅限固定 Treasury，执行 Prefunded Gas 校验，预留 Nonce 并持久化已签名交易。

## Scope
- `CollectionService`：校验订单已支付状态、检查子地址原生 Gas 与 ERC20 余额。
- 归集地址固定：仅允许从配置读取 `TREASURY_ADDRESS`，拒绝外部注入。
- Nonce 预留与 Calldata 拼装，请求签名器生成 Raw Signed Tx。
- 广播前落盘：写入 `outbound_transactions` 表，记录审计日志。
- API Routes：`POST /v1/collections`、`GET /v1/collections/{id}`，绑定 `collections:create` / `collections:read` Scope。

## Allowed Files
- `src/services/collections.rs`
- `src/api/mod.rs`

## Dependencies
- TASK-005
- TASK-007
- TASK-008

## Inputs and Outputs
- **Inputs**: HTTP POST 订单 ID 与归集模式（MVP 仅支持 `amount=max`）。
- **Outputs**: 创建的归集单详情、Outbound 交易引用，状态进入 `transferring`。

## Acceptance Criteria
- 任何试图指定外部 `to_address` 的请求必须被直接拒绝。
- 未付款的订单禁止发起归集。
- 必须先完成签名与数据库落盘，不得在落盘前调用 RPC 广播。

## Verification Commands
```bash
cargo test services::collections
```

## Risks and Assumptions
- MVP 假设子地址已提前预置了足够的原生代币支付 Gas。

## Status
DONE
