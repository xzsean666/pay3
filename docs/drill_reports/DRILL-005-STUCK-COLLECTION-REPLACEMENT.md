# 灾难恢复演练报告: DRILL-005 (卡死归集同 Nonce 加速替换与崩溃重播)

- **演练编号**: DRILL-005
- **演练日期**: 2026-10-02
- **演练环境**: Staging 自动化仿真环境
- **操作执行者**: Pay3 DevOps / AI Agent
- **演练耗时 (RTO)**: 5 秒 (下一个 Worker tick 自动重播或替换)
- **数据丢失窗口 (RPO)**: 0
- **演练状态**: PASS

---

## 1. 演练场景描述

模拟归集流程在最苛刻的崩溃与网络卡顿场景下的自愈：
1. **落盘后广播前崩溃**：归集交易已调用 Signer 完成签名并在数据库落盘为 `signed`，但广播网络请求发出前操作系统崩溃（SIGKILL / 掉电）。
   - 重启后，Collector 必须 100% 优先恢复该 `signed` 记录重播同一 Signed Tx，禁止申请新 Nonce。
2. **交易卡死与同 Nonce Replacement**：已广播交易因链上 Gas 暴涨卡在 Mempool。
   - 超过 `replacement_stuck_after` 后，自动发起同 Nonce 加速替换。
   - 原记录标记为 `replaced`，新记录以更高 Gas Price 广播。
   - 全生命周期中，PostgreSQL Partial Unique Index 绝不允许出现两个同 Nonce 的活跃 Outbound。

---

## 2. 执行步骤与命令

```bash
# 验证 Collector 崩溃恢复与同 Nonce 幂等集成测试
cargo test --test collector_recovery_integration
```

---

## 3. 预期结果 vs 实际结果

| 检查项 | 预期行为 | 实际执行结果 | 判定 |
| :--- | :--- | :--- | :--- |
| **未广播崩溃恢复** | 优先捞起 Signed 记录重播 | 成功重播原交易并标记为 `broadcast` | PASS |
| **Nonce 幂等保护** | 绝不消耗新 Nonce | 严格保持原 Nonce，不造成资金重出 | PASS |
| **并发唯一约束** | 阻止多笔同 Nonce 活跃记录 | 尝试重复插入活跃 Nonce 触发 `23505` 唯一索引异常 | PASS |
| **同 Nonce 替换** | 旧记录标记 replaced，新记录广播 | 替换链条完整，链上最终确认该 Nonce | PASS |

---

## 4. 演练结论

Pass。归集核心状态机和数据库 Partial Unique Index 完美防御了资金并发与崩溃重出的极端风险。
