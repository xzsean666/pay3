# Pay3 Polygon Amoy 测试币支付网关与 Cloudflare Pages 前端测试台对接指南

本文档记录了部署在测试服务器（`192.168.31.33`）上的 **Pay3 ERC20 支付网关（Polygon Amoy 测试网）**及托管在 Cloudflare 上的**前端测试收银台（`pay3-frontend-test.pages.dev`）**的全部配置细节、代币参数、鉴权方式、业务交互时序、API 接口规格与多语言对接示例。

后续任何业务系统、前端应用或第三方项目若需集成 Web3 ERC20（USDC / USDT）免 Gas 归接收款功能，均可直接参考本指南完成联调与测试。

---

## 目录

- [1. 部署架构与访问端点速查](#1-部署架构与访问端点速查)
- [2. 区块链网络与代币合约详情](#2-区块链网络与代币合约详情)
- [3. 测试代币领水方式 (Faucet)](#3-测试代币领水方式-faucet)
- [4. API 认证与鉴权 (JWT)](#4-api-认证与鉴权-jwt)
- [5. 业务对接流程与状态机](#5-业务对接流程与状态机)
- [6. API 接口详细规格与调用实战](#6-api-接口详细规格与调用实战)
  - [6.1 服务就绪与健康检查 (`GET /readyz`)](#61-服务就绪与健康检查-get-readyz)
  - [6.2 查询支持币种列表 (`GET /v1/tokens`)](#62-查询支持币种列表-get-v1tokens)
  - [6.3 创建收款订单 (`POST /v1/orders`)](#63-创建收款订单-post-v1orders)
  - [6.4 查询订单详情 (`GET /v1/orders/{id}`)](#64-查询订单详情-get-v1ordersid)
  - [6.5 按外部业务单号查询 (`GET /v1/orders/by-external-id/{external_id}`)](#65-按外部业务单号查询-get-v1ordersby-external-idexternal_id)
  - [6.6 即时链上核验 (`POST /v1/orders/{id}/verify`)](#66-即时链上核验-post-v1ordersidverify)
  - [6.7 人工接受问题付款 (`POST /v1/orders/{id}/accept-problem-payment`)](#67-人工接受问题付款-post-v1ordersidaccept-problem-payment)
- [7. 多语言对接代码示例](#7-多语言对接代码示例)
  - [7.1 Node.js / TypeScript 示例](#71-nodejs--typescript-示例)
  - [7.2 Python 示例](#72-python-示例)
  - [7.3 cURL 命令行快捷验证](#73-curl-命令行快捷验证)
- [8. 前端测试收银台使用指南 (Cloudflare Pages)](#8-前端测试收银台使用指南-cloudflare-pages)
- [9. 服务器部署结构与运维排错](#9-服务器部署结构与运维排错)

---

## 1. 部署架构与访问端点速查

系统采用 **前后端解耦架构**：
- **前端测试台**：由 **Cloudflare Pages** 全球边缘 CDN 托管，集成 Cloudflare Pages Functions 边缘 Worker，提供零配置免鉴权 API 代理。
- **后端支付网关**：部署在私有服务器 `192.168.31.33`，通过 Cloudflare Tunnel (`homecf`) 将 API 安全映射至公网域名 `https://home-10030.002788.xyz`，服务器端纯无状态运行，不挂载任何前端 Nginx 容器。

### 1.1 系统拓扑图

```mermaid
flowchart TD
    subgraph ClientProjects["接入方与客户端"]
        App["业务系统 / 后端服务"]
        Browser["用户浏览器 / Web3 钱包"]
    end

    subgraph CFPages["Cloudflare 全球边缘网络 (Pages & Functions)"]
        PagesUI["前端测试收银台 UI<br/>pay3-frontend-test.pages.dev"]
        EdgeProxy["Pages Functions 边缘代理<br/>/api/pay3/* 自动注入 Bearer Token"]
    end

    subgraph TestServer["测试服务器 (192.168.31.33)"]
        Tunnel["Cloudflare Tunnel 客户端 (homecf)"]

        subgraph Pay3Gateway["Pay3 支付网关核心 (pay3-pay3-1:10030)"]
            AxumAPI["Axum HTTP REST API"]
            Scanner["Direct-to-Postgres 链上扫块器"]
            Collector["免 Gas 资金归集器 (Collector)"]
            Signer["HD 钱包派生与本地签名机"]
        end

        subgraph DB["PostgreSQL 16 (postgresql-15430:15430)"]
            Postgres[("MyDB 数据库<br/>orders / payments / collections")]
        end
    end

    subgraph Blockchain["Polygon Amoy Testnet (Chain ID 80002)"]
        RPC["Polygon Amoy RPC 节点集群"]
        USDC_Contract["MockUSDC 合约 (EIP-3009)"]
        USDT_Contract["MockUSDT 合约 (Polygon Meta-Tx)"]
        TreasuryAcc["Treasury 归集总账钱包"]
    end

    Browser -->|1. 访问收银台 UI| PagesUI
    PagesUI -->|同源请求 /api/pay3/| EdgeProxy
    App -->|免鉴权代理调用 /api/pay3/| EdgeProxy
    EdgeProxy -->|携带内置测试 JWT| Tunnel
    
    App -->|直连 API 请求 (自备 JWT)| Tunnel
    Tunnel -->|转发本地 10030| AxumAPI
    
    AxumAPI <--> Postgres
    Scanner <--> Postgres
    Collector <--> Postgres
    
    Scanner -->|拉取区块与 Transfer Logs| RPC
    Collector -->|广播归集交易| RPC
    RPC --> USDC_Contract
    RPC --> USDT_Contract
    USDC_Contract -->|划转资金| TreasuryAcc
    USDT_Contract -->|划转资金| TreasuryAcc
```

### 1.2 端点速查表

| 服务名称 | 部署平台 / 环境 | 访问 URL / 端口 | 说明 |
| :--- | :--- | :--- | :--- |
| **前端测试台 UI** | Cloudflare Pages | [https://pay3-frontend-test.pages.dev](https://pay3-frontend-test.pages.dev) | 可视化收银台，支持领水、建单、扫码、轮询 |
| **免鉴权代理 API** | Cloudflare Pages 边缘函数 | `https://pay3-frontend-test.pages.dev/api/pay3` | **最简对接路径**：边缘函数自动注入 Bearer Token，调用方无需自签 JWT |
| **Pay3 后端 API (公网)** | 服务器 + Cloudflare 隧道 | `https://home-10030.002788.xyz` | 标准 REST API，直连调用需带 `Authorization: Bearer <JWT>` |
| **Pay3 后端 API (内网)** | 服务器局域网 HTTP | `http://192.168.31.33:10030` | 容器端口映射 `10030 -> 3000` |
| **PostgreSQL 数据库** | 服务器局域网 TCP | `192.168.31.33:15430` | 数据库 `MyDB`，用户名 `sean` |

---

## 2. 区块链网络与代币合约详情

本测试环境运行于 Polygon 官方测试网 **Amoy**，支持 **USDC** 与 **USDT** 双代币收款与全自动免 Gas 归集。

### 2.1 网络参数

- **网络名称**: Polygon Amoy Testnet
- **Chain ID**: `80002` (十六进制: `0x13882`)
- **原生代币**: `POL` (18 位精度，用于测试网 Gas 费用)
- **区块浏览器**: [https://amoy.polygonscan.com/](https://amoy.polygonscan.com/)
- **活跃 RPC 节点**（网关内部已配置智能轮询与冷却容灾）：
  - `https://polygon-amoy.drpc.org`
  - `https://polygon-amoy-bor-rpc.publicnode.com`
  - `https://polygon-amoy.gateway.tenderly.co`
  - `https://api.zan.top/polygon-amoy`

### 2.2 支持代币与合约配置

系统采用 **单实例统一调度引擎**，已部署并预置以下双代币支持：

| 币种 | 合约地址 | 精度 | EIP-712 名称 / 版本 | 归集协议 (Collection Method) | 归集 Gas 机制 |
| :--- | :--- | :---: | :--- | :--- | :--- |
| **USDC** | `0xc252a473DC466de3799149A2616ccc625832F40C` | 6 | `USD Coin` / `2` | `eip3009` (`receiveWithAuthorization`) | **用户与收款地址 0 原生 Gas**，Relayer 代发 |
| **USDT** | `0x2C2e569e3Df89c55Ba46Ce52248Ae180379d0B31` | 6 | `(PoS) Tether USD` / `1` | `polygon_meta_tx` (`executeMetaTransaction`) | **用户与收款地址 0 原生 Gas**，Relayer 代发 |

> [!NOTE]
> **免 Gas 归集原理**：
> 传统的 ERC20 归集需要从总账打入 POL 作为燃料给子地址，再由子地址发起 transfer，存在双倍 Gas 和链上阻塞痛点。
> Pay3 针对 USDC 采用 **EIP-3009 授权转账**，针对 USDT 采用 **Polygon 原生 MetaTransaction 元交易**。子地址只需生成签名，由具备极微量 POL 的 Relayer 作为代发人广播，即可将代币直接从子地址抽水到 Treasury，**收款子地址终生无需任何 POL 余额**。

### 2.3 关键系统地址

- **Treasury 归集总账钱包**:
  `0x55193778A808426Eb4024fADAd1aC3c0968E8AF2`
  *(所有订单成功支付后，资金会自动归集汇入此地址)*
- **Problem Funds 异常资金兜底地址**:
  `0x315d0eC4c9b7DeCEfCD91e45f9EBD8945e20C6De`
- **USDC Relayer 交易广播人**:
  `0x3ce91f5c429fcc9d928d06e5fc697cf0b5bdc1c1`
- **USDT Relayer 交易广播人**:
  `0xbd5cbf8ebef8c7439600a382b34ec97de8b4dcc0`
- **测试环境母助记词 (Master Mnemonic)**:
  `"picnic castle recall royal this repair champion play length fly behave only"`
  *(子收款地址派生路径规范: `m/44'/60'/0'/0/{index}`，每笔订单永久独占一个派生地址，严格禁止地址复用)*
- **测试自动付款钱包 (Payer Private Key)**:
  - 私钥: `0xa010561d0ee36587adb4c9d81f4d19d84dc8b2246a9b7b8fd0665d1f8b15d844`
  - 对应钱包地址: `0x285bd8C75C7647b7da1C1154776633804d4ff5eC`

---

## 3. 测试代币领水方式 (Faucet)

为了方便其他项目测试付款，测试网合约与前端均提供了简便的测试币获取途径。

### 方式 1：Cloudflare Pages 前端一键领水（推荐）

1. 在浏览器中打开 [https://pay3-frontend-test.pages.dev](https://pay3-frontend-test.pages.dev)
2. 点击顶部栏或导航栏中的 **「🚰 领水 (Faucet)」** 标签。
3. 点击 **「连接 Web3 钱包」**（支持 MetaMask、OKX Wallet、Rabby 等），页面会自动提示切换网络至 Polygon Amoy（Chain ID 80002）。
4. 点击 **「领取 1,000 USDC」** 或 **「领取 1,000 USDT」**，钱包签名广播后，1,000 枚测试代币将直接铸造到您的当前钱包中。
5. 可点击 **「添加到钱包」** 按钮，一键将代币合约和图标添加到钱包资产列表。

### 方式 2：调用合约 `faucet()` 方法领水

MockUSDC 与 MockUSDT 合约公开了免权限的 `faucet()` 函数，任何持有少量 POL（用于支付合约调用 Gas）的钱包均可直接调用：

- **调用方法**: `faucet()`
- **方法函数选择器 (Method ID)**: `0xde5f72fd`
- **单次领水额度**: 1,000 Token (即 raw 值 `1000000000`)

使用 Foundry `cast` 命令行调用示例：
```bash
# 领取 1,000 USDC
cast send 0xc252a473DC466de3799149A2616ccc625832F40C "faucet()" \
  --rpc-url https://polygon-amoy-bor-rpc.publicnode.com \
  --private-key <YOUR_PRIVATE_KEY>

# 领取 1,000 USDT
cast send 0x2C2e569e3Df89c55Ba46Ce52248Ae180379d0B31 "faucet()" \
  --rpc-url https://polygon-amoy-bor-rpc.publicnode.com \
  --private-key <YOUR_PRIVATE_KEY>
```

### 方式 3：服务器管理员脚本批量划拨

若您的测试钱包没有 POL 作为 Gas 费用，可以在服务器上直接执行预置的转账脚本：
```bash
ssh -i ~/ssh/sean -p 22 root@192.168.31.33
cd /home/apps/pay3
# 向指定地址赠送 1,000 USDC 与 1,000 USDT
./deploy/faucet_transfer.sh <YOUR_WALLET_ADDRESS> 1000
```

### 获取原生 POL Gas 水龙头

- [Polygon 官方水龙头](https://faucet.polygon.technology/)
- [Alchemy Amoy 水龙头](https://www.alchemy.com/faucets/polygon-amoy)

---

## 4. API 认证与鉴权 (JWT)

Pay3 API 采用标准 HTTP `Authorization: Bearer <JWT>` 鉴权，支持细粒度的权限范围控制（Scopes）。

### 4.1 对接模式选择

其他系统对接时可根据业务架构选择以下两种方式之一：

| 对接模式 | 目标 URL | 鉴权方式 | 适用场景 |
| :--- | :--- | :--- | :--- |
| **A. 免鉴权代理模式 (推荐前端/轻量测试)** | `https://pay3-frontend-test.pages.dev/api/pay3/...` | **无需配置 Header**（Cloudflare Pages Functions 边缘层自动注入合法 Bearer Token 并转发至后端） | 快速前端演示、测试脚本、本地 Demo |
| **B. 直连网关鉴权模式 (推荐后端服务)** | `https://home-10030.002788.xyz/...`<br/>或 `http://192.168.31.33:10030/...` | 请求头必须包含：<br/>`Authorization: Bearer <JWT_TOKEN>` | 正式后端对接、微服务通信、生产模拟 |

### 4.2 长期有效测试 JWT Token

测试环境内置了长期有效的开发 JWT Token（有效期至 **2027-10-17**），包含了订单创建、查询与链上核验所需的所有 scope：

```text
eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCIsImtpZCI6InBheTMtZGV2LWtleS0xIn0.eyJleHAiOjE4MjM3NDk4MjcsIm5iZiI6MTc5MjIxMzc2NywiaWF0IjoxNzkyMjEzODI3LCJpc3MiOiJwYXkzIiwiYXVkIjoicGF5My1hcGkiLCJzdWIiOiJmcm9udGVuZC10ZXN0Iiwic2NvcGUiOiJvcmRlcnM6Y3JlYXRlIG9yZGVyczpyZWFkIG9yZGVyczp2ZXJpZnkifQ.ukB5St4XmRDYdvumtUjjnr0kIhPg1haYVJWxjWTeyDc
```

Token 解密详情：
- **Header**: `{"alg": "HS256", "typ": "JWT", "kid": "pay3-dev-key-1"}`
- **Issuer (`iss`)**: `pay3`
- **Audience (`aud`)**: `pay3-api`
- **Subject (`sub`)**: `frontend-test` *(对应订单数据的归属者 tenant/user)*
- **Scopes (`scope`)**: `orders:create orders:read orders:verify`

### 4.3 自主签发 JWT Token（若接入方需要独立 `sub` 隔离）

如果接入方项目希望以独立的 `sub`（例如各自的业务系统标识 `shop-backend`）隔离订单数据，可使用测试环境密钥自主签名生成 JWT：

- **Secret**: `dev-dry-run-jwt-secret-000000000000666`
- **Key ID (`kid`)**: `pay3-dev-key-1`
- **Algorithm**: `HS256`

Node.js 快速生成脚本：
```javascript
import crypto from "node:crypto";

function createJwt(sub = "my-project", scopes = "orders:create orders:read orders:verify") {
  const secret = "dev-dry-run-jwt-secret-000000000000666";
  const header = { alg: "HS256", typ: "JWT", kid: "pay3-dev-key-1" };
  const now = Math.floor(Date.now() / 1000);
  const payload = {
    exp: now + 3600 * 24 * 365, // 1 年有效
    nbf: now - 10,
    iat: now,
    iss: "pay3",
    aud: "pay3-api",
    sub,
    scope: scopes,
  };
  const b64 = (obj) => Buffer.from(JSON.stringify(obj)).toString("base64url");
  const signInput = `${b64(header)}.${b64(payload)}`;
  const sig = crypto.createHmac("sha256", secret).update(signInput).digest("base64url");
  return `${signInput}.${sig}`;
}

console.log("Bearer " + createJwt());
```

---

## 5. 业务对接流程与状态机

### 5.1 完整支付交互时序

```mermaid
sequenceDiagram
    autonumber
    actor User as 付款用户 (Web3 钱包)
    participant Merchant as 商户业务系统
    participant Pay3 as Pay3 支付网关
    participant Chain as Polygon Amoy 区块链

    Merchant->>Pay3: 1. POST /v1/orders (external_id, amount, ttl_seconds)
    Pay3-->>Merchant: 2. 返回 201 Created (order_id, receive_address, expires_at)
    Merchant->>User: 3. 向用户展示收款地址与转账二维码
    User->>Chain: 4. 发起 ERC20 transfer (USDC/USDT -> receive_address)
    
    loop 自动扫链与确认数监听
        Pay3->>Chain: 5. 周期性拉取 Transfer 事件日志
        Chain-->>Pay3: 6. 发现匹配的收款入账
        Note over Pay3: 订单状态流转为 confirming<br/>累计确认区块数 (需 12 confirmations)
    end

    alt 商户轮询或用户页面主动核验
        Merchant->>Pay3: 7. GET /v1/orders/{id} 或 POST /v1/orders/{id}/verify
        Pay3-->>Merchant: 8. 返回最新状态 (status: paid, paid_amount_raw)
    end

    Note over Merchant: 9. 确认支付完成，向用户下发业务权益

    opt 后台异步免 Gas 归集
        Pay3->>Chain: 10. Relayer 发起 EIP-3009 或 Meta-Tx 归集划拨至 Treasury
        Chain-->>Pay3: 11. 归集交易确认落盘
    end
```

### 5.2 订单状态机流转

```mermaid
stateDiagram-v2
    [*] --> pending: 创建订单成功 (生成专属子地址)
    
    pending --> confirming: 链上检测到付款交易 (开始累计确认数)
    pending --> expired: 超过 ttl_seconds 未检测到付款
    
    confirming --> paid: 确认数达到 12 个区块且金额足额
    confirming --> expired: 窗口关闭且未能完成有效确认

    pending --> outside_window: 窗口关闭后迟到入账 / 金额不足
    outside_window --> paid: 人工复核通过 (POST /accept-problem-payment)
    
    paid --> [*]: 订单终态 (自动触发 Collector 归集)
    expired --> [*]: 订单终态
```

- `pending`: 待支付。订单已创建，后台扫链器持续监听该收款地址。
- `confirming`: 确认中。已在链上检测到 Transfer 事件，正在等待 12 个区块深度确认（Polygon 约 25~30 秒）。
- `paid`: 支付成功。确认数达标且金额足额，可交付业务。
- `expired`: 超时关闭。未在有效期内完成支付。

> [!IMPORTANT]
> **绝对不复用地址原则 (Strict No Address Reuse)**：
> 每一个订单派生的 `receive_address` 永久只绑定当前订单。即便订单超时，后续有迟到转账进入该地址，系统也只会将其记录在原订单的 `diagnostics` 问题支付中，**绝不会把该地址分配给任何新订单**，杜绝错账风险。

---

## 6. API 接口详细规格与调用实战

### 路由前缀规则

- **默认路由**：直接调用 `/v1/...` 时，默认使用系统中配置的第一个代币（本环境中为 **USDC**）。
- **多代币指定路由**：调用特定代币时，在 `/v1` 前加上代币地址（大小写不敏感）或 `/{chain_id}/{token_address}`：
  - USDC 接口：`POST /v1/orders` 或 `POST /0xc252a473DC466de3799149A2616ccc625832F40C/v1/orders`
  - USDT 接口：`POST /0x2C2e569e3Df89c55Ba46Ce52248Ae180379d0B31/v1/orders`

---

### 6.1 服务就绪与健康检查 (`GET /readyz`)

检查网关各子组件（数据库、RPC、签名机、扫块器、归集器）是否全部就绪。

- **URL**: `GET https://home-10030.002788.xyz/readyz`
- **无需鉴权**
- **响应示例 (HTTP 200)**:
```json
{
  "status": "ok",
  "dependencies": [
    { "name": "db", "status": "ok" },
    { "name": "migration", "status": "ok" },
    { "name": "rpc_chain_id", "status": "ok" },
    { "name": "signer", "status": "ok" },
    { "name": "worker_lease", "status": "ok" },
    { "name": "payment_scanner", "status": "ok" },
    { "name": "collection_collector", "status": "ok" }
  ]
}
```

---

### 6.2 查询支持币种列表 (`GET /v1/tokens`)

获取当前网关所支持的代币、精度、合约地址以及归集模式。

- **URL**: `GET https://home-10030.002788.xyz/v1/tokens`
- **无需鉴权**
- **响应示例 (HTTP 200)**:
```json
{
  "tokens": [
    {
      "chain_id": 80002,
      "collection_method": "eip3009",
      "min_confirmations": 12,
      "problem_funds_address": "0x315d0ec4c9b7decefcd91e45f9ebd8945e20c6de",
      "relayer_address": "0x3ce91f5c429fcc9d928d06e5fc697cf0b5bdc1c1",
      "start_block": 50391060,
      "token_address": "0xc252a473dc466de3799149a2616ccc625832f40c",
      "token_decimals": 6,
      "token_symbol": "USDC",
      "treasury_address": "0x55193778a808426eb4024fadad1ac3c0968e8af2"
    },
    {
      "chain_id": 80002,
      "collection_method": "polygon_meta_tx",
      "min_confirmations": 12,
      "problem_funds_address": "0x315d0ec4c9b7decefcd91e45f9ebd8945e20c6de",
      "relayer_address": "0xbd5cbf8ebef8c7439600a382b34ec97de8b4dcc0",
      "start_block": 50391060,
      "token_address": "0x2c2e569e3df89c55ba46ce52248ae180379d0b31",
      "token_decimals": 6,
      "token_symbol": "USDT",
      "treasury_address": "0x55193778a808426eb4024fadad1ac3c0968e8af2"
    }
  ]
}
```

---

### 6.3 创建收款订单 (`POST /v1/orders`)

商户系统调用此接口创建支付订单，系统会原子派生一个全新的收款子地址。

- **URL**:
  - 创建 **USDC** 订单: `POST https://home-10030.002788.xyz/v1/orders`
  - 创建 **USDT** 订单: `POST https://home-10030.002788.xyz/0x2C2e569e3Df89c55Ba46Ce52248Ae180379d0B31/v1/orders`
  - 或通过 Cloudflare Pages 免签代理: `POST https://pay3-frontend-test.pages.dev/api/pay3/v1/orders`
- **Headers**:
  - `Content-Type: application/json`
  - `Authorization: Bearer <TOKEN>` *(代理模式下可省略)*
- **请求参数 (JSON Body)**:

| 字段 | 类型 | 必填 | 说明 | 示例 |
| :--- | :--- | :---: | :--- | :--- |
| `external_id` | string | 是 | 接入方系统的唯一业务订单号（同一 `sub` 下唯一） | `"order-20261023-001"` |
| `amount` | string | 是 | 支付金额（人类可读十进制格式） | `"5.00"` |
| `ttl_seconds` | number | 是 | 订单有效倒计时（秒），过期后不再受理正常付款 | `900` (15 分钟) |
| `metadata` | object | 否 | 接入方自定义元数据（KV 键值对，原样回传） | `{"user_id": "10086", "plan": "pro"}` |

- **请求示例**:
```json
{
  "external_id": "shop-demo-10001",
  "amount": "2.5",
  "ttl_seconds": 900,
  "metadata": {
    "source": "my-cool-project",
    "customer": "alice"
  }
}
```

- **响应示例 (HTTP 201 Created)**:
```json
{
  "id": "ab21889b-39d9-43ae-8498-80cee7a3b5a3",
  "external_id": "shop-demo-10001",
  "status": "pending",
  "payment": {
    "chain_id": 80002,
    "token_address": "0xc252a473dc466de3799149a2616ccc625832f40c",
    "token_symbol": "USDC",
    "token_decimals": 6,
    "amount": "2.5",
    "amount_raw": "2500000",
    "paid_amount_raw": "0",
    "receive_address": "0xc5d63b69a493cfdeddd2e7c948c315d17a4e48ed",
    "child_account_id": "d82f37ee-2260-4a7b-907e-a55d82bae63e",
    "derivation_path": "m/44'/60'/0'/0/27",
    "expires_at": "2026-10-23T08:53:08.319029Z",
    "monitor_until": "2026-10-30T08:53:08.319029Z"
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

---

### 6.4 查询订单详情 (`GET /v1/orders/{id}`)

- **URL**: `GET https://home-10030.002788.xyz/v1/orders/{id}`
- **Headers**: `Authorization: Bearer <TOKEN>`
- **响应示例 (已支付状态 HTTP 200)**:
```json
{
  "id": "f5d0f19c-0940-4055-adf6-4283b8f73c44",
  "external_id": "fe-usdt-1792243066149",
  "status": "paid",
  "payment": {
    "chain_id": 80002,
    "token_address": "0x2c2e569e3df89c55ba46ce52248ae180379d0b31",
    "token_symbol": "USDT",
    "token_decimals": 6,
    "amount": "3",
    "amount_raw": "3000000",
    "paid_amount_raw": "3000000",
    "receive_address": "0xb8f856148bb6567c6188df94a147a9f8e29b916c",
    "child_account_id": "ebf41fe2-837f-418a-a02c-f92f903a7896",
    "derivation_path": "m/44'/60'/0'/0/25",
    "expires_at": "2026-10-20T06:36:16.313933Z",
    "monitor_until": "2026-10-27T06:36:16.313933Z"
  },
  "diagnostics": {
    "state": "settled",
    "problem_payment_count": 0,
    "problem_payment_total_raw": "0"
  }
}
```

---

### 6.5 按外部业务单号查询 (`GET /v1/orders/by-external-id/{external_id}`)

商户系统使用自身系统生成的 `external_id` 进行对账查询。

- **URL**: `GET https://home-10030.002788.xyz/v1/orders/by-external-id/{external_id}`
- **Headers**: `Authorization: Bearer <TOKEN>`
- **响应内容**: 与 `GET /v1/orders/{id}` 一致。

---

### 6.6 即时链上核验 (`POST /v1/orders/{id}/verify`)

用于前端页面在用户点击“我已经支付”或轮询时主动发起快速链上核验。网关会立即查询最新区块中与该订单 `receive_address` 匹配的 Transfer Logs，无需等待后台扫链定时器。

- **URL**: `POST https://home-10030.002788.xyz/v1/orders/{id}/verify`
- **Headers**: `Authorization: Bearer <TOKEN>`
- **响应示例 (确认中 HTTP 200)**:
```json
{
  "order_id": "ab21889b-39d9-43ae-8498-80cee7a3b5a3",
  "status": "confirming",
  "matched_payments": 1,
  "paid_amount_raw": "2500000",
  "confirmations": 5,
  "complete_to_block": 50922160
}
```

---

### 6.7 人工接受问题付款 (`POST /v1/orders/{id}/accept-problem-payment`)

若用户因网络拥堵导致付款交易在订单过期后才打包入账（产生迟到付款 `late` 或 `outside_window`），管理员或客服可调用此接口人工将订单复核为已支付并补单。

- **URL**: `POST https://home-10030.002788.xyz/v1/orders/{id}/accept-problem-payment`
- **Headers**: `Authorization: Bearer <TOKEN>`
- **请求体 (JSON Body)**:
```json
{
  "reason": "Customer payment arrived 2 mins late due to Polygon network spike, accepted manually"
}
```

---

## 7. 多语言对接代码示例

### 7.1 Node.js / TypeScript 示例

安装依赖：`npm install axios`

```typescript
import axios from "axios";

// 选项 A：走 Cloudflare Pages 免鉴权代理通道（最简）
const API_BASE = "https://pay3-frontend-test.pages.dev/api/pay3";
const AUTH_HEADER = {};

// 选项 B：走公网网关直连通道（需要填入 Bearer Token）
// const API_BASE = "https://home-10030.002788.xyz";
// const AUTH_HEADER = {
//   Authorization: "Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCIsImtpZCI6InBheTMtZGV2LWtleS0xIn0.eyJleHAiOjE4MjM3NDk4MjcsIm5iZiI6MTc5MjIxMzc2NywiaWF0IjoxNzkyMjEzODI3LCJpc3MiOiJwYXkzIiwiYXVkIjoicGF5My1hcGkiLCJzdWIiOiJmcm9udGVuZC10ZXN0Iiwic2NvcGUiOiJvcmRlcnM6Y3JlYXRlIG9yZGVyczpyZWFkIG9yZGVyczp2ZXJpZnkifQ.ukB5St4XmRDYdvumtUjjnr0kIhPg1haYVJWxjWTeyDc",
// };

async function createAndPollPayment() {
  const externalId = `shop-ts-${Date.now()}`;
  
  // 1. 创建订单（以 1.50 USDC 为例）
  console.log(`[1] 创建订单: ${externalId}...`);
  const createRes = await axios.post(
    `${API_BASE}/v1/orders`,
    {
      external_id: externalId,
      amount: "1.50",
      ttl_seconds: 600, // 10 分钟有效
      metadata: { project: "node-demo" },
    },
    { headers: { ...AUTH_HEADER, "Content-Type": "application/json" } }
  );

  const order = createRes.data;
  console.log(`[+] 订单创建成功!`);
  console.log(`    Order ID:        ${order.id}`);
  console.log(`    代币:            ${order.payment.token_symbol}`);
  console.log(`    收款地址:        ${order.payment.receive_address}`);
  console.log(`    应付金额:        ${order.payment.amount} (raw: ${order.payment.amount_raw})`);
  console.log(`    过期时间:        ${order.payment.expires_at}`);

  // 2. 轮询订单状态
  console.log(`[2] 开始监听付款入账状态...`);
  const interval = setInterval(async () => {
    try {
      const statusRes = await axios.get(`${API_BASE}/v1/orders/${order.id}`, {
        headers: AUTH_HEADER,
      });
      const current = statusRes.data;
      console.log(`    当前状态: [${current.status}] 已付: ${current.payment.paid_amount_raw}`);

      if (current.status === "paid") {
        console.log(`🎉 恭喜！订单已足额支付成功并确认！`);
        clearInterval(interval);
      } else if (current.status === "expired") {
        console.log(`❌ 订单已过期`);
        clearInterval(interval);
      }
    } catch (err: any) {
      console.error("轮询异常:", err.message);
    }
  }, 3000);
}

createAndPollPayment().catch(console.error);
```

### 7.2 Python 示例

安装依赖：`pip install requests`

```python
import time
import requests

API_BASE = "https://pay3-frontend-test.pages.dev/api/pay3"
HEADERS = {"Content-Type": "application/json"}

# 1. 创建订单 (以 2.00 USDT 为例，使用指定代币路径)
USDT_ADDRESS = "0x2C2e569e3Df89c55Ba46Ce52248Ae180379d0B31"
external_id = f"shop-py-{int(time.time())}"

create_payload = {
    "external_id": external_id,
    "amount": "2.00",
    "ttl_seconds": 900,
    "metadata": {"lang": "python"}
}

print(f"[*] 正在创建 USDT 收款单: {external_id}...")
resp = requests.post(f"{API_BASE}/{USDT_ADDRESS}/v1/orders", json=create_payload, headers=HEADERS)
resp.raise_for_status()
order_data = resp.json()

order_id = order_data["id"]
pay_info = order_data["payment"]
print(f"[+] 订单 ID: {order_id}")
print(f"[+] 收款地址: {pay_info['receive_address']}")
print(f"[+] 应付金额: {pay_info['amount']} {pay_info['token_symbol']}")

# 2. 轮询订单直到支付完成
print("[*] 正在等待入账...")
while True:
    time.sleep(3)
    check_resp = requests.get(f"{API_BASE}/v1/orders/{order_id}", headers=HEADERS)
    status_data = check_resp.json()
    status = status_data["status"]
    print(f"    订单状态: {status} (已收 raw: {status_data['payment']['paid_amount_raw']})")
    
    if status == "paid":
        print("🎉 支付已成功确认！")
        break
    elif status == "expired":
        print("[-] 订单已超时")
        break
```

### 7.3 cURL 命令行快捷验证

#### 快速建单 (USDC)
```bash
curl -sS -X POST "https://pay3-frontend-test.pages.dev/api/pay3/v1/orders" \
  -H "Content-Type: application/json" \
  -d '{
    "external_id": "test-curl-001",
    "amount": "1.00",
    "ttl_seconds": 900
  }' | jq
```

#### 快速建单 (USDT)
```bash
curl -sS -X POST "https://pay3-frontend-test.pages.dev/api/pay3/0x2C2e569e3Df89c55Ba46Ce52248Ae180379d0B31/v1/orders" \
  -H "Content-Type: application/json" \
  -d '{
    "external_id": "test-curl-usdt-001",
    "amount": "2.50",
    "ttl_seconds": 900
  }' | jq
```

#### 查询订单状态
```bash
curl -sS "https://pay3-frontend-test.pages.dev/api/pay3/v1/orders/by-external-id/test-curl-001" | jq
```

---

## 8. 前端测试收银台使用指南 (Cloudflare Pages)

前端收银台完全托管在 **Cloudflare Pages** 边缘网络上，天然具备全球低时延与高可用能力。

- **线上访问入口**: [https://pay3-frontend-test.pages.dev](https://pay3-frontend-test.pages.dev)
- **部署维护脚本**: `scripts/deploy-frontend-test-cloudflare.sh`
- **源码目录**: `frontend-test/`

```
┌─────────────────────────────────────────────────────────────────────────────┐
│ Pay3 前端测试台 (Cloudflare Pages)                         [🚰 领水] [代理模式]│
├─────────────────────────────────────────────────────────────────────────────┤
│ [连接] | [🚰 领水] | [创建订单] | [二维码] | [等待支付] | [人工处理] | [成功页] | [日志]│
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│  1. 领水: 连接钱包一键获取 1,000 USDC / 1,000 USDT 测试代币                     │
│  2. 建单: 选择币种、输入金额、自动派生子地址                                      │
│  3. 扫码: 渲染 EIP-681 转账 URI 二维码（手机钱包扫码可自动带出代币、金额与地址） │
│  4. 监控: 实时轮询区块确认数，达到 12 确认数自动切入「支付成功」页面               │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

### 核心功能面板说明

1. **连接设置 (Config)**:
   - 切换 **Worker 代理模式** 或 **浏览器直连模式**。
   - 自动拉取或填入内置开发 JWT，测试连接健康度。
2. **领水台 (Faucet)**:
   - 检查已连接钱包在 Polygon Amoy 上的 POL、USDC 与 USDT 余额。
   - 一键广播铸造 1,000 USDC / 1,000 USDT 测试币。
   - 一键将代币资产添加至 MetaMask 视图中。
3. **创建订单 (Create)**:
   - 币种下拉菜单：自动同步后端 `/v1/tokens`，支持 USDC（EIP-3009 免 Gas）与 USDT（Polygon MetaTx 免 Gas）。
   - 可自定义过期时间（TTL）、外部订单号及 JSON 扩展 Metadata。
4. **二维码收银台 (Pay / QR)**:
   - 自动生成符合 **EIP-681** 标准的 `ethereum:<token_address>@80002/transfer?address=<receive_address>&uint256=<raw_amount>` 支付二维码。
   - 支持切换纯地址二维码模式。
   - 提供收款地址一键复制及倒计时器。
5. **等待支付与轮询 (Wait)**:
   - 实时展示订单状态、已收金额、倒计时和最后轮询时间。
   - 提供「自动核验」开关和「手动 verify」按钮，触发秒级即时查块。
6. **人工处理 (Manual)**:
   - 支持按订单 ID 或 `external_id` 查询异常订单（例如少付、窗口外付款）。
   - 提供「接受问题付款」按钮，模拟客服对账特批。
7. **支付成功页 (Success)**:
   - 订单确认满 12 个区块后自动跳转，展示支付明细与跳转按钮。
8. **调试日志 (Logs)**:
   - 记录页面发起的所有 HTTP 请求、响应载荷、时延及异常日志，支持导出为 JSON。

### Cloudflare Pages 更新部署命令

如需更新前端静态资源或 Pages Functions 逻辑，在本地项目根目录运行：
```bash
./scripts/deploy-frontend-test-cloudflare.sh --api-base-url https://home-10030.002788.xyz
```

---

## 9. 服务器部署结构与运维排错

### 9.1 服务器登录与文件结构

- **SSH 访问命令**:
  ```bash
  ssh -i ~/ssh/sean -p 22 root@192.168.31.33
  ```
- **部署主目录**: `/home/apps/pay3`
  - `pay3.yaml`: 核心业务配置文件（配置代币、RPC 列表、Treasury 地址、扫块起始高度等）。
  - `.env`: 容器环境变量文件（包含数据库连接、JWT 秘钥等）。
  - `docker-compose.prebuilt.yml`: 后端 Docker Compose 编排文件。
  - `docker-compose.override.yml`: 卷挂载与配置文件覆盖。

### 9.2 Docker 容器状态（纯后端网关架构）

服务器上仅运行核心支付网关与数据库，无冗余静态服务容器：

| 容器名称 | 镜像 | 映射端口 | 容器作用 |
| :--- | :--- | :--- | :--- |
| `pay3-pay3-1` | `pay3:prebuilt` | `10030:3000` | Rust 编写的 Pay3 API 核心网关与扫链归集服务 |
| `postgresql-15430` | `48ebba8b80dc` | `15430:5432` | 独立 PostgreSQL 16 数据库实例 |
| `homecf` | `cloudflare/cloudflared` | host 模式 | 负责将 `home-10030.002788.xyz` 域名映射到本地端口 |

### 9.3 常用运维与巡检命令

#### 查看 Pay3 核心服务实时日志
```bash
docker logs -f --tail 100 pay3-pay3-1
```

#### 重启网关服务
```bash
cd /home/apps/pay3
docker compose -f docker-compose.prebuilt.yml -f docker-compose.override.yml restart pay3
```

#### 查询数据库扫链游标状态 (确保扫描器未挂起)
```bash
docker exec postgresql-15430 psql -U sean -d MyDB -c \
  "SELECT chain_id, token_address, last_scanned_block, updated_at FROM chain_cursors;"
```
*(正常情况下 `updated_at` 会在数秒内持续刷新，`last_scanned_block` 紧跟 Polygon Amoy 最新高度)*

#### 查看最近 5 笔订单及支付状态
```bash
docker exec postgresql-15430 psql -U sean -d MyDB -c \
  "SELECT id, external_id, status, paid_amount_raw, expected_amount_raw, created_at FROM orders ORDER BY created_at DESC LIMIT 5;"
```

#### 查看归集记录 (Collections)
```bash
docker exec postgresql-15430 psql -U sean -d MyDB -c \
  "SELECT id, order_id, status, amount_raw, to_address, outbound_tx_id, updated_at FROM collections ORDER BY created_at DESC LIMIT 5;"
```

---

### 9.4 常见问题排查 (Troubleshooting)

1. **付款后订单长时间处于 `pending`，没有变成 `confirming` 或 `paid`**：
   - 检查转账的目标地址是否与订单返回的 `payment.receive_address` 严格一致。
   - 检查转账的代币合约是否正确（USDC: `0xc252...F40C`，USDT: `0x2C2e...0B31`）。
   - 检查 Polygonscan 上该笔交易是否已经打包成功。
   - 调用 `POST /v1/orders/{id}/verify` 主动触发一次实时核验，观察返回的 `matched_payments`。
   - 检查数据库中 `chain_cursors` 是否持续推进。
2. **API 请求返回 `401 Unauthorized`**：
   - 确认请求头格式为 `Authorization: Bearer <TOKEN>`（注意 Bearer 后面有空格）。
   - 若自行生成 JWT，确认 `iss` 为 `pay3`，`aud` 为 `pay3-api`，`kid` 为 `pay3-dev-key-1`，算法为 `HS256`。
   - 可直接使用本文档第 4.2 节提供的内置长期 Token。
   - 或者直接将请求地址前缀替换为 `https://pay3-frontend-test.pages.dev/api/pay3`（Cloudflare 边缘免鉴权代理）。
3. **API 请求返回 `429 Too Many Requests`**：
   - 测试环境默认配置的接口限流速率为 `120 次/分钟`，请适当调整轮询间隔（推荐轮询间隔 `>= 2000ms`）。
4. **订单变为 `paid` 后，为什么归集记录（Collection）失败或未上链？**：
   - 归集交易是由 Relayer 广播的。
   - 检查对应币种的 Relayer 钱包是否有微量的 POL 原生代币用于支付 Gas。
   - USDC Relayer: `0x3ce91f5c429fcc9d928d06e5fc697cf0b5bdc1c1`
   - USDT Relayer: `0xbd5cbf8ebef8c7439600a382b34ec97de8b4dcc0`
   - 若余额为 0，只需向上述地址转入 0.1 ~ 0.5 POL 即可恢复。
