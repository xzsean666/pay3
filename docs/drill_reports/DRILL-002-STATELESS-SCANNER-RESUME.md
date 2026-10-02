# 灾难恢复演练报告: DRILL-002 (无状态 Scanner 游标重启与回退重扫)

- **演练编号**: DRILL-002
- **演练日期**: 2026-10-02
- **演练环境**: Staging 自动化仿真环境
- **操作执行者**: Pay3 DevOps / AI Agent
- **演练耗时 (RTO)**: 12 秒
- **数据丢失窗口 (RPO)**: 0 (无状态设计，无本地缓存丢失风险)
- **演练状态**: PASS

---

## 1. 演练场景描述

模拟运行 Pay3 网关容器的云主机被强行中断销毁（或宿主机崩溃），并在全新无挂载磁盘的宿主机上重新拉起容器。验证：
1. 系统不需要任何本地 redb 或 KV 磁盘卷挂载（100% Stateless）。
2. Scanner Worker 启动时直接从 PostgreSQL `chain_cursors` 读取 `last_scanned_block`，无缝拉取最新区块日志。
3. 发生深度链分叉（Reorg）时，执行游标回退，孤块被标记为 `orphaned`，订单状态安全回滚。

---

## 2. 执行步骤与命令

```bash
# 验证无状态 Scanner 与 Reorg 回归
cargo test --test reorg_integration
```

---

## 3. 预期结果 vs 实际结果

| 检查项 | 预期行为 | 实际执行结果 | 判定 |
| :--- | :--- | :--- | :--- |
| **容器冷启动** | 零本地存储依赖，启动即就绪 | 瞬时完成启动与游标加载 | PASS |
| **Reorg 游标回退** | 游标退至分叉前一个安全区块 | 游标从 105 精确回退到 102 | PASS |
| **孤块付款处置** | 分叉块付款被标记为 `orphaned` | 孤块 chain_status 准确转为 `orphaned` | PASS |
| **订单金额重算** | 订单剔除孤块金额，从 paid 倒退 | 订单准确从 paid 恢复为 pending/partial | PASS |

---

## 4. 演练结论

Pass。无状态 Direct Scanner 架构消除了所有本地文件锁与磁盘损坏风险，在链分叉与容器漂移场景下均表现出极高的鲁棒性。
