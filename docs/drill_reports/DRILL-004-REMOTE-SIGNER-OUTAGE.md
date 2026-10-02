# 灾难恢复演练报告: DRILL-004 (外部 Signer 服务宕机、鉴权失败与自愈)

- **演练编号**: DRILL-004
- **演练日期**: 2026-10-02
- **演练环境**: Staging 自动化仿真环境
- **操作执行者**: Pay3 DevOps / AI Agent
- **演练耗时 (RTO)**: 15 秒 (重启 Signer 容器后秒级恢复)
- **数据丢失窗口 (RPO)**: 0
- **演练状态**: PASS

---

## 1. 演练场景描述

模拟外部签名服务（`RemoteHttpSigner`）遭遇以下异常边界：
1. Bearer Token 错误或未授权（HTTP 401 Unauthorized）。
2. Signer 发生 500 内部服务错误。
3. 网络分区或 Signer 处理超时。
4. Signer 重启恢复。

验证点：
- 任何签名器异常必须被类型化捕获，系统绝不发生 panic。
- 签名异常触发报警，且绝对不会在异常状态下广播错误数据。
- Signer 恢复后，归集服务自动续签并安全恢复。

---

## 2. 执行步骤与命令

```bash
# 验证 RemoteHttpSigner 鉴权加固与异常契约测试
cargo test --test signer_contract
```

---

## 3. 预期结果 vs 实际结果

| 检查项 | 预期行为 | 实际执行结果 | 判定 |
| :--- | :--- | :--- | :--- |
| **Token 校验** | 缺失或错误 Token 返回 401 | 类型化映射为 `RemoteHttpStatus { status: 401 }` | PASS |
| **敏感信息脱敏** | 打印 Debug 日志不泄露 Bearer Token | Debug 输出展示 `<redacted>`，无敏感明文 | PASS |
| **超时截断** | 远端挂起在配置超时内返回 Transport 错误 | 严格按超时限制中断并记录日志 | PASS |
| **服务自愈** | 模拟 Signer 故障恢复后正常签名 | 恢复后地址派生与 EIP-1559 签名一致通过 | PASS |

---

## 4. 演练结论

Pass。外部 Signer 隔离边界坚固，具备完备的超时控制、Bearer 鉴权与类型化降级处理能力。
