---
name: pay3
description: "生产级、无状态 ERC20 加密货币收款后端服务。支持 HD 钱包派生唯一子收款地址、直连 RPC 批量扫链匹配付款、按时付款状态判定、JWT 作用域权限校验与国库归集。"
---

# pay3 AI Skill & Integration Guide

> 本文档专供 AI 编程助手与开发者查阅，系统化涵盖 `pay3` 的架构定位、REST API 端点规范、JWT 鉴权机制、请求/响应 DTO 结构、TypeScript/Cloudflare Worker 客户端集成用例以及避坑指南。

---

## 1. 模块定位与核心能力

- **模块定位**：面向生产环境的高性能、100% 无状态 ERC20 订单收款后端服务。系统为每个业务订单派生一个唯一的 EVM 收款地址，通过链上 `Transfer` 日志自动确认付款，杜绝地址复用与虚假充值。
- **源码根路径**：`/ssd0/git/pay3`
- **托管账号**：`xzsean666`
- **Git 远程仓库**：`https://github.com/xzsean666/pay3.git`
- **服务形态**：Rust 实现的 HTTP RESTful 微服务（Axum 框架，基于 PostgreSQL 真相源），对外提供标准 JSON 契约。
- **调用方式**：业务系统通过 HTTP(S) 配合带有细粒度 Scope 的 JWT Bearer Token 进行鉴权调用。

### 核心能力特性：
1. **HD 地址无限派生与隔离**：单订单单地址（BIP-44 派生规则），从根源杜绝地址冲突与多用户充值撞单。
2. **规范链匹配与时效判定**：自动区分 `on_time`（有效按时）、`late`（过期后付款）、`outside_window`（窗口外转账），防止过期订单恶意充值诈骗。
3. **支持多笔转账累加**：支持用户分笔转入指定 Token，仅当累计按时金额达标并达到足够安全区块确认数后才将订单标记为 `paid`。
4. **资金国库自动归集 (Treasury Sweep)**：后台异步 Worker 处理子地址资金收集至统一国库，具备 Nonce 预留、崩溃重播与卡单替换（Gas Replacement）能力。
5. **手动即时重算验证 (`verify`)**：支持用户端或后端主动发起 `POST /v1/orders/{id}/verify` 直连 RPC 扫链，无需漫长等待定时轮询。

---

## 2. 鉴权与 JWT 作用域规范 (Authentication & Scopes)

所有对外业务接口均要求在 HTTP 请求头中携带 Bearer JWT：
```http
Authorization: Bearer <JWT_TOKEN>
```

### 核心 Scope 清单：
- `orders:create`: 允许调用 `POST /v1/orders` 创建收款订单。
- `orders:read`: 允许调用 `GET /v1/orders/{id}` 及 `GET /v1/orders/by-external-id/{external_id}` 查询订单状态与付款明细。
- `orders:verify`: 允许调用 `POST /v1/orders/{id}/verify` 手动触发链上扫链与状态重算。
- `collections:create`: 允许调用 `POST /v1/collections` 发起国库资金归集。
- `collections:read`: 允许调用 `GET /v1/collections/{id}` 查询归集交易进度。

### 客户端 JWT Claims 结构：
```json
{
  "iss": "pay3",
  "aud": "pay3-api",
  "sub": "gochina-portal",
  "scope": "orders:create orders:read orders:verify",
  "iat": 1729344000,
  "exp": 1729347600
}
```

---

## 3. REST API 端点规范与契约 (API Reference)

### 3.1 创建收款订单 (`POST /v1/orders`)
- **权限要求**：`orders:create`
- **请求体 (Request JSON)**：
  ```json
  {
    "external_id": "order_gochina_20261019_001",
    "amount": "15.00",
    "ttl_seconds": 3600,
    "metadata": {
      "user_id": "usr_987654321",
      "plan_id": "plan_standard_monthly"
    }
  }
  ```
  - `external_id` (string, 必需)：调用方的全局唯一业务订单号，支持幂等重入。
  - `amount` (string, 必需)：需支付的代币金额（人类可读十进制格式，如 `"15.00"`）。
  - `ttl_seconds` (number, 必需)：订单支付有效窗口秒数（通常 1800 - 7200）。
  - `metadata` (object, 可选)：附加业务上下文 JSON 对象。

- **响应体 (201 Created / 200 OK 幂等)**：
  ```json
  {
    "id": "e838f3a3-674a-4d7a-8b83-8a3b5a9ef001",
    "external_id": "order_gochina_20261019_001",
    "status": "pending",
    "payment": {
      "chain_id": 80002,
      "token_address": "0x1234567890123456789012345678901234567890",
      "token_symbol": "USDT",
      "token_decimals": 6,
      "amount": "15.00",
      "amount_raw": "15000000",
      "paid_amount_raw": "0",
      "receive_address": "0x9876543210987654321098765432109876543210",
      "child_account_id": "b345f8a1-3e2b-4d56-a1c2-9876543210ab",
      "derivation_path": "m/44'/60'/0'/0/42",
      "expires_at": "2026-10-19T22:01:00Z",
      "monitor_until": "2026-10-19T23:01:00Z"
    },
    "diagnostics": {
      "state": "awaiting_payment",
      "problem_payment_count": 0,
      "problem_payment_total_raw": "0",
      "late_payment_count": 0,
      "late_payment_total_raw": "0",
      "outside_window_payment_count": 0,
      "outside_window_payment_total_raw": "0",
      "latest_problem_payment": null,
      "problem_collection": null,
      "manual_acceptance": null
    }
  }
  ```

### 3.2 订单状态枚举 (`OrderStatus`)
- `pending`: 待支付，尚未收到足额按时链上付款，且订单在有效期内。
- `partial`: 部分支付，已检测到部分付款但未足额。
- `confirming`: 已收到全额转账，正在等待区块链区块确认数（`min_confirmations`）。
- `paid`: 支付成功！已足额且已完成区块安全确认，业务系统可安全交付权益。
- `expired`: 订单已过期，未能完成支付。

### 3.3 查询订单详情 (`GET /v1/orders/{id}`) 或 (`GET /v1/orders/by-external-id/{external_id}`)
- **权限要求**：`orders:read`
- **返回**：标准 `OrderResponse`（同上述创建响应）。

### 3.4 手动触发链上校验 (`POST /v1/orders/{id}/verify`)
- **权限要求**：`orders:verify`
- **场景**：用户在前端点击“我已完成付款”，客户端触发即时重算。
- **返回**：重算后的最新 `OrderResponse`，立即返回最新链上匹配状态。

---

## 4. TypeScript / Cloudflare Worker 快速接入用例

在 Cloudflare Worker 或 Node.js 环境中封装轻量 Pay3 客户端：

```typescript
import { SignJWT } from "jose";

export interface Pay3ClientConfig {
  baseUrl: string;
  jwtSecret: string;
  jwtIssuer?: string;
  jwtAudience?: string;
  jwtKeyId?: string;
}

export interface CreateOrderParams {
  externalId: string;
  amount: string;
  ttlSeconds: number;
  metadata?: Record<string, unknown>;
}

export class Pay3Client {
  constructor(private config: Pay3ClientConfig) {}

  private async generateToken(scopes: string[]): Promise<string> {
    const secretKey = new TextEncoder().encode(this.config.jwtSecret);
    return await new SignJWT({
      scope: scopes.join(" "),
    })
      .setProtectedHeader({ alg: "HS256", kid: this.config.jwtKeyId || "pay3-key-1" })
      .setIssuedAt()
      .setIssuer(this.config.jwtIssuer || "pay3")
      .setAudience(this.config.jwtAudience || "pay3-api")
      .setExpirationTime("1h")
      .sign(secretKey);
  }

  async createOrder(params: CreateOrderParams) {
    const token = await this.generateToken(["orders:create", "orders:read"]);
    const resp = await fetch(`${this.config.baseUrl}/v1/orders`, {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        "Authorization": `Bearer ${token}`,
      },
      body: JSON.stringify({
        external_id: params.externalId,
        amount: params.amount,
        ttl_seconds: params.ttlSeconds,
        metadata: params.metadata,
      }),
    });

    if (!resp.ok) {
      const err = await resp.json().catch(() => ({}));
      throw new Error(`Pay3 createOrder failed: ${resp.status} - ${JSON.stringify(err)}`);
    }
    return await resp.json();
  }

  async getOrder(orderId: string) {
    const token = await this.generateToken(["orders:read"]);
    const resp = await fetch(`${this.config.baseUrl}/v1/orders/${orderId}`, {
      headers: { "Authorization": `Bearer ${token}` },
    });
    if (!resp.ok) throw new Error(`Pay3 getOrder failed: ${resp.status}`);
    return await resp.json();
  }

  async verifyOrder(orderId: string) {
    const token = await this.generateToken(["orders:verify", "orders:read"]);
    const resp = await fetch(`${this.config.baseUrl}/v1/orders/${orderId}/verify`, {
      method: "POST",
      headers: { "Authorization": `Bearer ${token}` },
    });
    if (!resp.ok) throw new Error(`Pay3 verifyOrder failed: ${resp.status}`);
    return await resp.json();
  }
}
```

---

## 5. 常见陷阱与最佳实践 (Pitfalls & Best Practices)

1. **`external_id` 幂等设计**：
   - 必须使用业务全局唯一的订单主键作为 `external_id`。如果用户重复提交或网络重试，Pay3 会幂等返回相同的订单与收款地址，切忌为同一次用户结算请求生成多个不同的 `external_id`。
2. **精度与金额格式**：
   - `amount` 字段必须传纯文本十进制字符串（如 `"15.00"`、`"9.9"`），严禁使用浮点数，以避免序列化时的浮点精度漂移。
3. **订单时效性控制**：
   - 默认推荐 `ttl_seconds: 3600`（1小时）。展示层应提供动态倒计时。
   - 若用户在倒计时结束后才转账，该笔转账将分类为 `late`（逾期转账），订单不会自动变为 `paid`。业务系统可通过 `diagnostics.late_payment_count` 识别并在工单系统中人工处理。
4. **前端轮询与手动触发机制**：
   - 支付页面建议以 3~5 秒间隔轮询订单状态；
   - 用户点击“已完成转账”按钮时，调用 `POST /v1/orders/{id}/verify` 触发即时链上扫链，极大提升用户体验。
5. **安全与生产约束**：
   - 生产环境中 Pay3 容器应当部署在私有内网或通过 Cloudflare Access / 专用网络互联，仅供 Portal 后端或 Pages Functions 代理访问，绝不能将具备写权限的 JWT 凭据下发至公开浏览器。
