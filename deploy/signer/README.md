# Pay3 生产级外部 Signer 部署与参考规范

本文档为 Pay3 网关的独立签名服务（`RemoteHttpSigner`）提供参考实现、部署方式与生产安全加固规范。

---

## 1. 架构定位与安全边界

- **职责单向隔离**：Pay3 网关主服务仅负责订单创建、链上交易扫描、付款核销与归集调度，**绝不直接持有私钥或助记词**。
- **外部 Signer**：私钥由专用签名服务、硬件安全模块 (HSM) 或云端托管密匙管理服务 (AWS KMS, GCP Cloud KMS, HashiCorp Vault) 管理。
- **网络隔离原则**：
  - 生产环境下，Signer 服务必须部署在严格受限的专用 VPC / 私网子网中。
  - **严禁**将 Signer 服务的监听端口（默认 8088）直接向公网开放。
  - Pay3 与 Signer 服务之间的通信必须通过内网服务发现或安全网关（如带有双向 TLS 的 Envoy / NGINX 反向代理）。

---

## 2. API 协议契约 (Contract Specification)

所有请求若配置了认证 Token，必须携带 HTTP Header：
```http
Authorization: Bearer <SIGNER_AUTH_TOKEN>
```

### 2.1 健康探测 (`GET /healthz`)
- **Response 200 OK**:
  ```json
  {
    "status": "ok"
  }
  ```
- 若内部 KMS 故障或无法解密主密钥，返回 500/503。

### 2.2 派生子收款地址 (`POST /v1/addresses/derive`)
- **Request Body**:
  ```json
  {
    "key_ref": "pay3-master",
    "path": "m/44'/60'/0'/0/42"
  }
  ```
- **Response 200 OK**:
  ```json
  "0x8b230151fc5d135bcd0f65d23febda4585da8a93"
  ```

### 2.3 签名归集交易 (`POST /v1/transactions/sign`)
- **Request Body**:
  ```json
  {
    "key_ref": "pay3-master",
    "path": "m/44'/60'/0'/0/1",
    "transaction": {
      "request_id": "collection-job-12",
      "chain_id": 1,
      "nonce": 12,
      "to": "0x2222222222222222222222222222222222222222",
      "value": "0",
      "gas_limit": 65000,
      "max_fee_per_gas": "30000000000",
      "max_priority_fee_per_gas": "1500000000",
      "data": "0xa9059cbb..."
    }
  }
  ```
- **Response 200 OK**:
  ```json
  {
    "request_id": "collection-job-12",
    "chain_id": 1,
    "nonce": 12,
    "from": "0x50330afc91b0457036a17efef9da963ce9477a5a",
    "to": "0x2222222222222222222222222222222222222222",
    "raw_tx": [2, ...],
    "tx_hash": "0x1234..."
  }
  ```

---

## 3. 快速部署 (Docker Compose)

在当前目录下启动参考签名器：

```bash
# 1. 设置安全 Token
export SIGNER_AUTH_TOKEN="your-secure-random-token-here"

# 2. 启动服务
docker compose up -d

# 3. 验证健康状态
curl -H "Authorization: Bearer ${SIGNER_AUTH_TOKEN}" http://127.0.0.1:8088/healthz
```

---

## 4. Pay3 网关配置接入

在 Pay3 网关的生产环境环境变量中配置：

```bash
# 指定 Signer 模式为 external (或 kms / hsm)
SIGNER_MODE=external
SIGNER_KEY_REF=pay3-master

# 生产环境强制使用 HTTPS 端点 (除非显式配置 ALLOW_INSECURE_REMOTE_SIGNER=true)
SIGNER_REMOTE_ENDPOINT=https://signer.internal.example.com
SIGNER_REMOTE_BEARER_TOKEN=your-secure-random-token-here
SIGNER_REMOTE_REQUEST_TIMEOUT_SECS=5
```
