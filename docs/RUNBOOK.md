# Pay3 生产运维与灾难恢复手册 (Operations & Disaster Recovery Runbook)

本文档是 Pay3 支付网关生产环境故障排查、应急响应、灾难恢复与实操演练的事实指导手册。

---

## 1. 系统核心不变量与运维原则

1. **单实例单一 Token 架构 (One Instance, One Token)**:
   - 每个网关实例只绑定一个 `CHAIN_ID` 与单一 `TOKEN_ADDRESS`。多币种跨链采用独立实例隔离部署，杜绝跨链交叉污染。
2. **绝对禁止地址复用 (Strict No Address Reuse)**:
   - 每个订单唯一绑定派生的子收款地址（`orders.receive_address` UNIQUE）。迟到付款必须留在原订单进入对账，绝不分配给新订单。
3. **PostgreSQL 是唯一真相源，扫描器 100% 纯无状态 (Stateless Direct Scanner)**:
   - 订单、付款、游标、归集、nonce 均在 PostgreSQL 强事务一致性保护下。
   - 无本地 redb 或 KV 文件存储，无磁盘写放大与文件锁风险，容器重启随时按 PostgreSQL 游标自动续扫。
4. **多 RPC 负载均衡与智能 CD 冷却熔断 (Multi-RPC Load Balancing & Cooldown)**:
   - 请求通过原子计数器在配置的多节点间 Round-Robin 分发。遇 429 或超时自动进入阶梯 CD（5s -> 10s -> 60s），健康节点承接流量，冷却到期自动半开探测恢复。
5. **同 Nonce 幂等与崩溃优先重播**:
   - 广播链上交易前必须持久化落盘 `(chain_id, from_address, nonce, signed_tx, tx_hash)`。
   - 崩溃恢复必须优先重播同一已签名交易，严禁发放新 nonce 导致资金重出。

---

## 2. 核心监控指标与告警阈值

| 告警规则 | 触发条件 | 级别 | 处置动作 |
| :--- | :--- | :--- | :--- |
| **Pay3NotReady** | `pay3_readyz_status == 0` 持续 2m | `critical` | 访问 `/readyz` 查看失败依赖（DB、RPC、Signer、Workers）并查看日志 |
| **Pay3DependencyUnhealthy** | `pay3_readyz_dependency_status == 0` 持续 3m | `critical` | 排查指定依赖连通性（PostgreSQL 连接池、RPC 节点响应、Signer Token） |
| **Pay3WorkerConsecutiveFailures** | `pay3_worker_consecutive_failures > 5` 持续 2m | `critical` | 检查对应 Worker 结构化错误日志，排查网络阻塞或数据库锁冲突 |
| **Pay3PaymentScannerHighLag** | `pay3_payment_scanner_lag_blocks > 50` 持续 5m | `warning` | 检查 RPC 节点出块与网络延迟，核对 `chain_cursors` 推进状态 |
| **Pay3RpcHighErrorRate** | `increase(pay3_rpc_errors_total[5m]) > 10` 持续 2m | `warning` | 检查 RPC 供应商配额、429 限制，必要时增加备用 RPC 节点 |
| **Pay3SignerFailure** | `increase(pay3_signer_errors_total[5m]) > 0` 持续 1m | `critical` | 检查外部 Signer 服务健康状态、网络连通性及 Bearer Token 是否过期 |
| **Pay3HighHttpLatency** | `pay3_http_request_latency_seconds_max > 2` 持续 5m | `warning` | 检查数据库慢查询或外网网络抖动 |

---

## 3. 常见故障排查与应急操作规程

### 3.1 多 RPC 节点故障与网络中断

**现象**：
- 日志中频繁出现 `RPC node entered cooldown`、`HTTP 429 Too Many Requests` 或 `Request timeout`。
- `pay3_rpc_errors_total` 指标上升。

**系统自愈行为**：
- 系统自动将故障节点隔离冷却，流量自动无缝切换到其他配置的健康 RPC 节点。
- CD 冷却时间采用指数退避：第 1 次 5 秒，第 2 次 10 秒，第 3 次及以上最高 60 秒。
- 冷却期满后，节点自动进入半开状态，成功处理请求即恢复健康。

**人工介入步骤**：
1. 若所有节点均进入冷却，检查外部网络或供应商服务状态。
2. 紧急增加备用 RPC 节点：
   ```bash
   # 更新环境变量 CHAIN_RPC_URLS
   export CHAIN_RPC_URLS="https://rpc1.example.com,https://rpc2.example.com,https://backup-rpc.example.com"
   # 平滑重启服务实例
   ```
3. 服务重启后会自动从 PostgreSQL `chain_cursors` 当前高度无缝续扫，不会跳块或重复记账。

---

### 3.2 深度链分叉 (Deep Reorg) 处置

**现象**：
- 链发生超过配置 `REORG_LOOKBACK_BLOCKS` 的深度分叉，孤块产生。
- 扫描器检测到区块哈希不连续。

**处理规程**：
1. 暂停 Scanner Worker 推进。
2. 确认最新的 Canonical 链权威分叉高度（`fork_block_number`）。
3. 执行游标回退与孤块重算（系统内部自动处理，或手动触发）：
   ```sql
   -- 1. 将分叉高度及以后的付款标记为 orphaned
   UPDATE payments
   SET chain_status = 'orphaned', updated_at = now()
   WHERE chain_id = $CHAIN_ID AND token_address = $TOKEN_ADDRESS AND block_number >= $FORK_BLOCK
     AND chain_status <> 'orphaned';

   -- 2. 将游标回退至分叉前一个安全区块
   UPDATE chain_cursors
   SET last_scanned_block = $FORK_BLOCK - 1,
       lease_owner = NULL,
       lease_until = NULL,
       updated_at = now()
   WHERE chain_id = $CHAIN_ID AND token_address = $TOKEN_ADDRESS;
   ```
4. 数据库内的订单金额计算引擎会自动将孤块金额剔除，原 `paid` 订单安全倒退为 `pending` 或 `partial`。
5. 重启 Scanner，从回退高度继续沿新主链重扫入库。

---

### 3.3 外部 Signer 服务宕机或鉴权失效

**现象**：
- `/readyz` 报告 `signer` 依赖 unhealthy。
- Collector 报告 `RemoteHttpStatus { status: 401 }` 或 `RemoteTransport timeout`。

**处理规程**：
1. 检查 Signer 服务容器与网络健康：
   ```bash
   curl -H "Authorization: Bearer $SIGNER_AUTH_TOKEN" http://signer-host:8088/healthz
   ```
2. 若返回 401：
   - 检查环境变量 `SIGNER_REMOTE_BEARER_TOKEN` 与 Signer 端配置是否一致。
   - 更新匹配的 Token 并热重载。
3. 若服务进程退出：
   - 检查 Signer 容器日志，重启服务：
   ```bash
   docker compose -f deploy/signer/docker-compose.yml restart
   ```
4. 恢复后：Collector 自动唤醒，无需任何人工对账，继续处理队列中的归集作业。

---

### 3.4 归集交易卡死与同 Nonce 加速替换 (Stuck Collection Replacement)

**现象**：
- 归集交易在链上长时间处于 pending 状态（Gas Price 剧烈上涨导致）。
- 达到 `replacement_stuck_after` 超时阈值。

**处理规程**：
- Collector Worker 自动进入同 Nonce Replacement 流程：
  1. 读取原卡死交易记录与 Nonce。
  2. 使用同一 `(chain_id, from_address, nonce, to_address)` 构造新交易，提高 `max_fee_per_gas` 与 `max_priority_fee_per_gas`。
  3. 请求 Signer 完成重签。
  4. 数据库事务内将旧记录标记为 `replaced`，落盘新记录为 `signed`。
  5. 广播新签名交易，推进状态为 `broadcast`。
- **数据库强约束保证**：PostgreSQL Partial Unique Index 绝不允许出现两个处于活跃状态的同 Nonce 记录，彻底杜绝双花或资金重出。

---

### 3.5 PostgreSQL 灾难恢复 (PITR)

**规程**：
1. 使用云厂商或物理备份将 PostgreSQL 数据库实例 PITR 恢复至指定安全时间点 $T$。
2. 校验数据库约束与最新迁移版本：
   ```bash
   cargo test --test migration_contract
   ```
3. 核对 `chain_cursors.last_scanned_block`：
   - 由于 PITR 可能回退几分钟数据，扫描器启动后会自动从 PITR 后的 `last_scanned_block` 继续向最新区块拉取日志。
   - 写入 `payments` 采用 `ON CONFLICT (chain_id, tx_hash, log_index) DO NOTHING` 幂等插入，完全幂等且无重复数据。
4. Collector 启动后优先检查所有未决 Outbound 交易在链上的实际 Receipt，确认真实执行状态，防止重复广播。
