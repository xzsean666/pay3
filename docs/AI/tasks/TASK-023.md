# TASK-023: 灾难恢复 Runbook 实操演练与记录

## Objective
按照 [docs/RUNBOOK.md](file:///ssd0/git/pay3/docs/RUNBOOK.md) 中规划的操作指南，对关键故障场景进行全流程实操演练，并生成结构化演练执行报告。

## Scope
- 演练场景 1：**PostgreSQL PITR 点对点恢复**与数据一致性核对。
- 演练场景 2：**本地 redb 损坏与从链上完全重建**（Rebuild from chain）。
- 演练场景 3：**主 RPC 节点宕机与多节点切换**演练。
- 演练场景 4：**Signer 外部服务宕机降级与恢复**演练。
- 演练场景 5：**卡死归集（Stuck Collection）同 Nonce 加速替换**演练。

## Allowed Files
- `docs/RUNBOOK.md`
- `docs/drill_reports/**`
- `scripts/**`

## Dependencies
- TASK-016
- TASK-017

## Inputs and Outputs
- **Inputs**: Staging 演练环境与 Runbook 操作手册。
- **Outputs**: 包含命令、输出日志、RTO/RPO 记录的演练审计报告 Markdown 文件。

## Acceptance Criteria
- 每一项演练必须具备完整的执行命令输出日志。
- redb 在被直接 `rm -f` 删除后，重启服务能自动重新初始化并从起始高度或保留高度扫齐缺失日志。
- 演练中未发现阻塞性未定义行为。

## Verification Commands
```bash
bash scripts/drill_kv_rebuild.sh # 示例演练脚本
```

## Risks and Assumptions
- 演练需在专用的 Staging 或隔离测试环境中进行，不可在生产真实环境直接演练。

## Status
TODO
