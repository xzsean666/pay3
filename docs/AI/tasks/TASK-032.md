# TASK-032: 原生多链多 Token 统一调度引擎 (Native Multi-Token Runtime Engine)

## 1. 任务背景与目标

在实际商用支付网关场景中，单一商户往往需要同时支持多种区块链（如 Ethereum、Polygon、Arbitrum、Optimism、Base）以及多个 ERC20 代币（如 USDT、USDC）。
为了在**“单一套 PostgreSQL 数据库”**和**“单一套母私钥 / 外部 Signer”**的前提下，实现最佳的部署体验、最高的资金安全性和系统稳定性，本任务将系统从单一 Token 运行时无缝升级为**原生多链多 Token 统一调度引擎**。

核心目标：
1. **统一声明式配置 (`pay3.yaml`)**：支持通过 YAML 文件定义全局服务、数据库、Signer 及 `tokens` 列表，同时 100% 向后兼容现有单代币 `.env` 配置。
2. **多实例安全数据库初始化 (Multi-Token DB Seeding)**：在启动时并发/批量种子化所有代币的 `chain_cursors`、`treasury_addresses`、`relayer_addresses`，并共享单一 `wallet_cursors` 游标，确保行级锁安全并发派生。
3. **单母私钥子 Relayer 派生隔离**：基于同一 Master Signer，按规则为各个 Token 派生唯一的 Relayer 子地址（如 `m/44'/60'/99'/0/{index}`），从根本上避免同链多代币归集时的 Nonce 竞争。
4. **运行时多 Worker 调度与隔离 (TokenRuntimeRegistry)**：为每个代币启动独立的 `RpcProviderManager`（独立 CD 熔断与 Round-Robin）、`PaymentScannerWorker` 和 `CollectionCollectorWorker`，实现故障与异常零爆炸半径。
5. **Axum 原生统一路由**：支持 `/{token_address}/v1/...` 与 `/{chain_id}/{token_address}/v1/...` 动态请求分发，并提供全局 `/v1/tokens` 元数据大盘及打标的 Prometheus `/metrics`。

---

## 2. 影响文件与修改范围

- `Cargo.toml`: 引入 `serde_yaml`。
- `src/config.rs`: 扩展多代币配置模型 `MultiTokenAppConfig`、`TokenConfig`，支持从 YAML 文件或现有环境变量加载并做生产门禁校验。
- `src/db/migrations.rs`: 扩展 `RuntimeSeedConfig`，支持多代币批量初始化。
- `src/runtime.rs`: 抽象 `TokenInstanceContext` 与 `MultiTokenRegistry`，调度多实例 Worker。
- `src/api/mod.rs` & 路由模块: 支持按 Token 合约地址和链 ID 动态分发 API 请求，添加 `GET /v1/tokens` 端点。
- `tests/`: 编写多 Token 并发运行、地址唯一性与独立 RPC 隔离测试。

---

## 3. 验收标准与验证方案

1. `cargo check --all-targets` 编译 0 错误 0 警告。
2. 保持原有 230 个测试全部通过（向后兼容性验证）。
3. 新增多 Token 核心测试：
   - 验证 YAML 解析与单币/多币兼容加载。
   - 验证多代币共享单数据库与单私钥时，订单地址派生绝对唯一无冲突。
   - 验证动态路由正确分发至指定 Token 实例，并拦截未支持的代币。
   - 验证独立 RPC 故障隔离。

---

## 4. 完成状态与验证结果

- **状态**: `DONE` (已完成)
- **完成项**:
  1. `Cargo.toml`: 引入 `serde_yaml = "0.9"`。
  2. `src/config.rs`:
     - 扩展 `TokenInstanceConfig`、`MultiTokenConfigFile`、`TokenYamlConfig` 等数据模型。
     - 实现 `AppConfig::from_yaml_str`、`AppConfig::from_yaml_file`、`AppConfig::from_yaml_or_env`。
     - 扩展 `validate_profile`，全面遍历各代币的生产级多 RPC、起始区块、地址隔离以及 `(chain_id, token_address)` 实例防重检查。
  3. `src/db/migrations.rs`:
     - 实现 `seed_multi_runtime_config`，在同一强一致性事务内安全初始化母账号 `wallet_cursors` 及各个代币的 `chain_cursors`、`treasury_addresses`、`relayer_addresses`。
  4. `src/runtime.rs`:
     - 实现多代币 Worker 与调度核心：为各代币初始化专属 `RpcRangeSource`（独立 CD 熔断）、`PaymentScannerWorker` 和 `CollectionCollectorWorker`。
     - 按照 `m/44'/60'/99'/0/{index}` 为每个代币派生唯一 Relayer 地址，避免链上 Nonce 冲突。
  5. `src/api/mod.rs`:
     - 实现 `build_multi_token_router`，原生挂载 `GET /v1/tokens` 查询大盘，并将各代币路由动态挂载至 `/{token_address}/v1/...`、`/{chain_id}/{token_address}/v1/...`。
     - 在单代币模式下自动保持根路径 `/v1/orders` 100% 向后兼容。
  6. `src/main.rs`: 接入 `AppConfig::from_yaml_or_env()?`，支持 `--verify-readiness` 和 `--check-config` 命令行检查。
  7. `tests/multi_token_runtime.rs`: 5 个多 Token 场景集成测试全部通过。
  8. `pay3.example.yaml`: 提供了完整的多链多 Token 编排模版。
  9. **回归与验收**:
     - 全量测试通过：236 passed, 0 failed, 2 ignored。
     - `scripts/verify_production_readiness.sh`: 21 项门禁检查 100% 通过。

