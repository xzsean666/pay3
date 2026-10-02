# Pay3 项目总目标 (Project Goal)

## 1. 项目愿景与定位

Pay3 是一个使用 Rust 语言构建的高性能、高可靠性 ERC20 Token 收款平台网关。
其核心定位是为去中心化与 Web3 支付场景提供具备**高并发处理能力、确定性对账、零资金混淆、全链路崩溃恢复**能力的链上支付与资金归集闭环系统。

---

## 2. MVP 目标范围 (Scope)

为确保工程质量与核心资产安全，MVP 阶段专注于单一核心闭环，严格控制复杂性。

### 2.1 包含范围 (In-Scope)
- **单一业务主账号**: JWT 鉴权通过并具备所需 scope 即视为同一默认主体。
- **单一 EVM 链与单一 ERC20 Token**: 系统配置固定的 `CHAIN_ID` 与 `TOKEN_ADDRESS`。
- **HD 分层确定性子地址派生**:
  - 路径规范：`m/44'/60'/{account_index}'/{change_index}/{address_index}`。
  - 支持 `address_index -> change_index -> account_index` 无限 Rollover 派生。
- **严格地址单次使用 (No Address Reuse)**:
  - 每个订单创建时分配一个全新的子收款地址。
  - 收款地址与订单全局唯一绑定，永久有效且永不复用。
- **链上付款验证与扫描闭环**:
  - `transfer_log_store` 将 ERC20 `Transfer` 事件 raw logs、block headers、range manifests 录入本地 redb KVDB。
  - `workers/scanner` 分页拉取 KV logs，结合 PostgreSQL 订单支付窗口进行高效匹配与确认。
  - 支持手动支付验证接口 (`POST /v1/orders/{id}/verify`)。
- **自动/半自动 Token 归集 (Collect)**:
  - 仅支持归集到系统预设的 `TREASURY_ADDRESS`。
  - MVP 采用 `prefunded` 原生代币 Gas 模式。
  - 广播前持久化 Signed Tx 与 Nonce，提供完整的崩溃恢复与同 Nonce Replacement 支持。
- **高韧性状态机与异常恢复**:
  - 链重组（Reorg）感知与回退（KV `reorg_epoch` 驱动 PostgreSQL 游标回退与孤块重算）。
  - 进程任意点崩溃恢复（包含扫链游标、归集广播前后、收据轮询）。
- **生产级可观测性与安全门禁**:
  - `/healthz`、`/readyz`、`/metrics`。
  - 端点级 JWT Scope 校验，生产环境禁止明文密钥与 Local Signer。

### 2.2 明确暂不包含范围 (Out-of-Scope)
- 多租户/多商户隔离系统。
- 多链路由与多币种自动兑换。
- 复杂的后台管理系统、退款流水线、法币汇率计算。
- 基于消息队列（如 Kafka/RabbitMQ）的 Webhook 必达重试保障。
- 用户资产托管与自主提现（仅支持资金归集至 Treasury）。
- 归集代付 Gas Station / Paymaster 抽象（后续版本扩展）。

---

## 3. 核心不变量与设计红线 (Core Invariants)

| 不变量 | 规则要求 | 破坏后果 |
| :--- | :--- | :--- |
| **禁止地址复用** | 每个订单派生唯一新地址；`orders.receive_address` 必须全局唯一，迟到转账永久归属原订单。 | 迟到转账发生时产生不可逆的订单归属歧义与资金纠纷。 |
| **资金真相单一源** | PostgreSQL 是所有订单、支付、归集、游标与 Nonce 的唯一权威真相来源。 | KV 缓存损坏导致资产丢失或对账逻辑混乱。 |
| **Raw 数据隔离** | 原始 RPC 响应、全量区块头、raw Transfer logs 仅允许进 redb 或内存，禁止直接作为 PostgreSQL 主表保存。 | 热门代币海量事件撑爆关系型数据库事务与存储。 |
| **固定归集目标** | 归集目的地址强制写死为 Treasury，API/Service 拒绝任何动态外部传入的目标地址。 | 攻击者或配置错误将归集资金转入恶意外部地址。 |
| **Outbound 先存后发** | 任何归集交易必须先落盘 Nonce、已签名 Raw Tx，校验一致后才允许通过 RPC 广播。 | 进程在广播与落盘之间崩溃导致重复发交易或 Nonce 混乱。 |
| **Reorg 联动重算** | KV 发生分叉 rewind 必须递增 `reorg_epoch`，Scanner 消费到新 epoch 必须回滚游标并重算订单。 | 孤块虚假支付被误判为最终已付款。 |
| **生产零明文私钥** | 生产 profile 严禁明文助记词/私钥，强制依赖外部 Signer/KMS 适配器。 | 私钥泄漏引发全盘资产失窃。 |

---

## 4. MVP 出口验收标准 (Exit Criteria)

满足以下全部条件后，Pay3 MVP 方可声明达到生产候选（Production Ready）：

1. **核心服务与 Worker 运行闭环**:
   - API 服务与 Transfer Log Ingestor、Payment Scanner、Collector、Order Expiry、Retention 循环稳定运行。
2. **测试矩阵全线通过**:
   - 单元测试、契约测试、PostgreSQL 事务与约束测试、Anvil+Mock ERC20 端到端测试通过。
3. **关键容灾与安全硬化**:
   - `JWT_JWKS_URL` 远程 JWKS 轮换支持或生产 PEM 校验。
   - 外部生产 Signer 服务接入契约校验。
   - 深度 Reorg、孤块付款重算及扫描崩溃恢复真实测试覆盖。
   - Collector 广播前、广播后崩溃恢复及 Replacement 回归测试。
4. **运维 Runbook 演练完成**:
   - 数据库 PITR 恢复、Migration 回滚、RPC 故障切换、Signer 宕机恢复、KVDB 重建演练记录完备。
   - Prometheus 核心告警规则完成 dry-run。
