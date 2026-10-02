# TASK-020: Prometheus 告警规则 Dry-Run 验证与指标补齐

## Objective
验证 `deploy/prometheus/pay3-alerts.example.yml` 中定义的所有核心生产告警规则，对关键告警条件进行 Dry-Run 演练测试，并补齐指标覆盖。

## Scope
- 验证告警指标：`pay3_log_ingestor_lag_blocks`、`pay3_payment_scanner_lag_blocks`、`pay3_rpc_errors_total`、`pay3_collections_by_status`、`pay3_signer_errors_total`、`pay3_payment_events_total`。
- 使用 `promtool` 校验告警规则语法与触发条件测试用例。
- 完善 Grafana 监控大盘 JSON 导出文件。

## Allowed Files
- `deploy/prometheus/pay3-alerts.example.yml`
- `deploy/prometheus/rules_test.yml`
- `src/health.rs`
- `src/workers/**`

## Dependencies
- TASK-016

## Inputs and Outputs
- **Inputs**: 告警规则 YAML 文件与模拟指标时间序列数据。
- **Outputs**: `promtool` 语法校验通过报告与单元测试通过日志。

## Acceptance Criteria
- `promtool check rules` 检查无语法错误。
- 针对 Scanner Lag、Signer 故障、卡死归集等场景编写的告警单测全部触发通过。

## Verification Commands
```bash
promtool check rules deploy/prometheus/pay3-alerts.example.yml
```

## Risks and Assumptions
- 假设环境已安装 `promtool` 或提供预编译可执行工具。

## Status
TODO
