# TASK-017: 端到端与集成测试套件

## Objective
构建高仿真端到端测试套件，结合 Anvil 本地节点与 Mock ERC20 合约，完整验证“创建订单 -> 链上转账 -> 日志采集 -> Scanner 扫链对账 -> 确认结算 -> 归集到 Treasury -> Replacement 替换”全业务闭环。

## Scope
- `tests/anvil_e2e.rs`：Anvil 自动化启停、部署 Mock ERC20、派生助记词、转账与完整资金闭环验证。
- `tests/deployed_api_acceptance.rs`：针对已部署运行态的黑盒 API 验收测试套件。
- 模拟测试卡死归集与同 Nonce Replacement。

## Allowed Files
- `tests/anvil_e2e.rs`
- `tests/deployed_api_acceptance.rs`
- `tests/support/**`

## Dependencies
- TASK-016

## Inputs and Outputs
- **Inputs**: 本地 Anvil 节点环境。
- **Outputs**: 绿色的端到端全链路测试报告。

## Acceptance Criteria
- 自动完成从订单创建到 Treasury 余额增加的资金对账验证。
- 覆盖卡死交易 Replacement 并确认 Treasury 最终正确入账。
- 所有 E2E 测试稳定通过，无随机 Flaky 现象。

## Verification Commands
```bash
cargo test --test anvil_e2e
```

## Risks and Assumptions
- 执行需要本地环境已安装 Foundry (Anvil) 或使用测试容器。

## Status
DONE
