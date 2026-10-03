# TASK-033: 全面安全审计缺陷修复与高并发性能优化

状态: `DONE`
依赖: [TASK-032](file:///ssd0/git/pay3/docs/AI/tasks/TASK-032.md)
完成时间: 2026-10-03
完成情况:
- [x] [CRITICAL-01] EIP-2612 归集安全回退至 Standard 转账，防止无 forwarder 合约时资金滞留在子地址；
- [x] [CRITICAL-02] 归集与 Outbound 认领严格基于 chain_id 与 token_address 隔离，防止多链混淆与向错误链广播；
- [x] [HIGH-01] 扫链器 Worker 实现 Reorg 自动自愈，检测到区块哈希分叉时自动触发游标回退并标记孤立支付；
- [x] [HIGH-02] TransferLogRange 引入 recipient (Topic2) 过滤，并在手动订单核验服务中过滤目标地址，彻底解决热点代币日志溢出问题；
- [x] [MED-01] 新增 20261003000300_performance_audit_indexes 数据库部分索引，消除超时监控与归集关联合并查询的全表扫描；
- [x] [MED-02] Remote JWKS 引入单飞并发排队与双重检查锁，彻底阻断恶意/未知 kid 的缓存击穿与雪崩；
- [x] [MED-03] FixedWindowRateLimiter 改造为纯无锁原子窗口计数器，消除全局 Mutex 竞争；
- [x] [MED-04] API 统一拦截底层数据库错误并输出脱敏报错，内部记录完整 tracing 日志；
- [x] [PERF-01] RpcRangeSource::transfer_logs 缺失区块头并发获取，大幅降低 RPC 批量延迟；
- [x] [LOW-01] LocalMnemonicSigner 缓存按路径派生的私钥 Signer，消除高频 PBKDF2 重复开销。

---

## 1. 任务目标

根据全面安全与性能审计结果，系统性修复全部严重缺陷与性能瓶颈：
1. **[CRITICAL-01] EIP-2612 归集安全防护**: 防止仅执行 approve/permit 的虚假 confirmed 归集，自适应策略安全回退至标准转账，防止资金滞留；
2. **[CRITICAL-02] 多链多 Token Worker 强隔离**: 为 `claim_collection_job` 与 `outbound_transactions` 认领查询增加 `chain_id` 与 `token_address` 过滤，杜绝跨链抢单与向错误链广播；
3. **[HIGH-01] 扫链器 Reorg 自动自愈**: 在检测到 `CanonicalBlockHashMismatch` 时自动调用 `handle_reorg` 回退游标并孤块重算，杜绝死循环卡死；
4. **[HIGH-02] 精准日志过滤**: 扩展 `TransferLogRange` 支持指定 `to_address` (Topic2) 过滤，解决手动核验与扫链在高频公网代币下的 1000 地址/日志溢出 DoS 问题；
5. **[MED-01] 数据库关键部分索引**: 增加 `orders(expires_at, id)`、`collections(outbound_tx_id)`、`payments(chain_id, token_address, block_number, log_index, id)` 针对性索引，彻底消除轮询全表扫描；
6. **[PERF-01] 区块头并发拉取**: 将 `RpcRangeSource::transfer_logs` 中的区块头顺序拉取改造为并发批处理，大幅降低扫链延迟；
7. **[MED-04] 内部错误脱敏**: 隐藏内部 SQL/DB 详细报错，防止暴露表结构与系统实现细节。

---

## 2. 验收标准

1. `tests/audit_remediation_contract.rs` 覆盖所有修复项并全部通过；
2. `cargo check --all-targets` 编译零错误零警告；
3. `cargo test --all-targets` 全量测试套件 100% 绿色通过。
