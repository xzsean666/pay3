# Pay3 Next Session

更新时间：2026-05-04

## 当前状态

仓库已经从纯文档进入 Rust 实现阶段。当前完成 Phase 1 基础模块，已完成 Phase 2 的 M4 `db/migrations` 和 M5 `db/repositories` 初版，已完成 Phase 3 的 M6 `wallet`、订单创建 service 初版和订单 API route contract，已推进 Phase 4 的 signer、chain、RPC provider manager、transfer log store、付款匹配 service 和手动 verify API route contract，已完成 Phase 5 `workers/scanner` tick contract、常驻 loop、confirmation sweep、rolling lookback rescan 和 lag/readiness 初版，已完成 `services/collections` prefunded 初版、collector broadcast tick 初版、collector 常驻 loop 初版、collector broadcast 前/后崩溃恢复与 receipt sweep tick 初版、collect replacement 初版、collection fee/collector timeout 配置化初版、worker tick metrics/readyz 初版和 collection create/read API route contract 初版，并新增真实 API 启动路径的 runtime composition 初版、真实 PostgreSQL 集成测试 `tests/collection_db_integration.rs` 和 Anvil+mock ERC20 e2e `tests/anvil_e2e.rs`，其中补了 collect replacement 回归。

已完成：

- Rust 项目骨架：`Cargo.toml`、`Cargo.lock`、`src/main.rs`、`src/lib.rs`、基础模块目录。
- `src/config.rs`: typed env config，production profile guard，token decimals/symbol，敏感配置 Debug 脱敏。
- `src/domain/**`: amount/address/hash 规范化、derivation segment rollover、订单/付款/归集状态规则（含 `partial`）、KV reorg epoch、collection replacement invariant。
- `src/auth/**`: JWT HS256 bearer verifier、`exp/nbf/iat/iss/aud/sub/kid/alg` 校验、scope/principal，已包含 `orders:create`、`orders:read`、`orders:verify`、`collections:create`、`collections:read`。
- `src/api/mod.rs`、`src/health.rs`、`src/error.rs`: `/healthz`、`/readyz`、`/metrics`、worker tick metrics/readyz、统一错误响应、订单 API route contract。
- `src/chain/mod.rs`: 标准化链数据契约和 fake：
  - `ChainHeaderReader`、`TransferLogSource`、`Erc20ChainClient` traits。
  - `ChainBlock`、`TransferLog`、`TxReceipt`、capacity probe DTO。
  - `FakeErc20ChainClient` 可控制 range logs、RPC failure、reorg block replacement、balances、receipts、broadcast tx hash。
  - 已桥接 `OrderChainHeadReader`，后续订单 service 可用真实 chain head reader。
- `src/chain/rpc.rs`: RPC provider manager / `RpcRangeSource` 初版：
  - `JsonRpcProvider` trait、`HttpJsonRpcProvider`、`RpcProviderManager`、`RpcRangeSource`。
  - provider count gate、`eth_chainId` 校验、latest/safe/finalized head 读取、同高度 block hash mismatch fail-closed。
  - HTTP JSON-RPC 429/timeout/error 会 failover 到下一个 provider；readiness probe 可校验 chain id 和 heads。
  - `RpcRangeSource` 实现 `ChainHeaderReader`、`TransferLogSource`、`Erc20ChainClient`。
  - 支持 ERC20 `Transfer` `eth_getLogs` range filter、topic/data 解析、block header 校验、capacity probe 和 `ensure_capacity` gate。
  - 支持 `balanceOf`、receipt 查询、signed tx broadcast 的 RPC adapter 初版；Anvil ERC20 e2e 已补，生产 readiness 动态探针仍待补。
- `src/db/migrations/20260502000100_initial_schema.sql`: PostgreSQL 初始 schema。
- `src/db/migrations.rs`: sqlx migrator 和运行时 seed helper，用于初始化 `wallet_cursors`、`chain_cursors`、`treasury_addresses`。
- `src/db/repositories/**`: Order/Payment/Collection/Outbound/Audit repository trait 和 PostgreSQL 实现初版。
  - `OrderRepository::get_order_view` 已补齐，用于读回 order + child_account + payment_window。
  - `CollectionRepository::get_collection` 已补齐，用于 collection read API 读回 collection 状态/outbound 引用/错误信息。
  - `payment_records` / `payment_recompute` 已抽成共享 helper，`PaymentRepository` 和 `PgVerifiedPaymentRecorder` 复用同一套 matched payment upsert + order recompute 逻辑。
  - `PaymentRepository` 新增 observed payment confirmation candidates 和 confirm observed payments 批量方法；scanner 先做 canonical block hash 校验，再写 `confirmed` 并重算订单。
- `src/wallet/mod.rs`: HD wallet address derivation boundary，`HdWallet`、`AddressDeriver` trait、deterministic fake deriver、稳定地址/rollover/key_ref/path 负例测试。
- `src/signer/mod.rs`: `SignerProvider` contract、`UnsignedTx`/`SignedTx` DTO、deterministic fake signer；fake signer 派生地址与 deterministic wallet 保持一致。
- `src/services/orders.rs`: 订单创建/query service 初版，负责 canonical request_hash、idempotency pre-check、当前链头窗口下界、wallet cursor 分配、地址派生、order/payment_window command 拼装。
- `src/services/payment_windows.rs`: watch-set + 批量 fallback 的 `PaymentWindowLookup`，miss 走批量查询接口，不做逐地址 fallback；lookup contract 已改为返回 `Result`，真实 PostgreSQL fallback 失败时 scanner 会 fail closed，不会把候选当空集继续推进 cursor。
  - 新增 `RepositoryPaymentWindowLookup`，通过 `PaymentWindowCandidateRepository` 批量查 `payment_windows JOIN orders`，带 `chain_id/token_address` 约束和地址批大小 gate。
- `src/services/payments.rs`: 纯付款匹配 service contract，使用 `TransferLogReader::logs_page`、`PaymentWindowLookup` 和 `ChainHeaderReader`，输出 `MatchedPaymentInput`、`next_token`、`complete_to_block`、`kv_reorg_epoch`。
- `src/services/collections.rs`: collection 创建和 prefunded collection job 准备 service 初版：
  - 创建 collection 时只使用配置 treasury，不接受任意 `to_address`。
  - 使用 `order_id/idempotency_key/amount/chain/token/treasury` 生成 canonical request_hash。
  - 要求订单为 `paid` 且 chain/token 匹配后才调用 `CollectionRepository::create_collection_idempotent`。
  - 处理 queued job 时检查 ERC20 token balance、prefunded gas gate、signer health，预留 nonce，构造 ERC20 `transfer(treasury, amount)` calldata，签名并持久化 outbound tx，再 attach collection outbound。
  - 写 `collection.create` / `collection.signed` audit event。
  - 注意：当前是 service contract 初版，broadcast/retry/receipt/replacement 仍需与 collector/outbound 的真实运行态持续联调，不过已有真实 PostgreSQL 集成测试和 Anvil replacement e2e 回归。
- `src/services/verify/**`: manual verify service contract，按订单窗口读取 bounded KV logs，复用 `services::payments::match_stored_transfer_logs`，通过 `VerifiedPaymentRecorder` 小 trait 与具体 Pg 写入解耦。
- `src/services/mod.rs`: services 模块入口，已导出 orders/payment_windows/payments/verify/collections。
- `src/workers/scanner.rs`: payment scanner tick contract 初版：
  - 通过 `PaymentRepository::claim_scan_range` 获取 PostgreSQL cursor lease。
  - 检测 KV `reorg_epoch` 变化并调用 `handle_kv_reorg_epoch` 回退业务 cursor / orphan affected payments。
  - 只通过 `PaymentPageMatcher` / `TransferLogReader::logs_page` 跑 paged payment matching，不直接扫 RPC。
  - 使用 `commit_scanned_batch` 同事务提交 matched Pay3 payments、订单重算和 `chain_cursors.last_scanned_block` 推进。
  - 空 page 可推进到 KV `last_completed_block`；page limit 未覆盖完整新区块时不推进 cursor。
  - 空闲 tick 会执行 confirmation sweep：查询 observed payments 候选，用 `ChainHeaderReader::block_by_number` 校验 stored block hash 仍 canonical，满足 `min_confirmations` 后批量确认并重算订单。
  - canonical block hash mismatch 时 fail closed，不确认 payment，等待 KV reorg path 回退/orphan。
  - 新增 `spawn_payment_scanner_loop` 常驻 loop 初版，按固定 interval 调用 tick，单次错误记录结构化 tracing 后继续运行；覆盖零 poll interval 配置校验。
- `src/workers/collector.rs`: collector worker broadcast tick contract 初版：
  - 通过 `CollectionJobPreparer` 调用 `services/collections` 准备 queued collection job。
  - 只在 service 已返回 persisted outbound + signed tx 后广播 raw signed tx。
  - 校验链返回的 tx hash 必须等于 signed/outbound tx hash；不一致时 fail closed，不调用 `mark_broadcast`。
  - 广播成功后调用 `OutboundRepository::mark_broadcast`，让 collections/outbound 状态推进到 confirming/broadcast。
  - tick 开始时会优先通过 `OutboundRepository::claim_signed_collect_tx_for_broadcast` claim 已持久化但未广播的 `transferring + signed` collect outbound，重播同一 `signed_tx`，覆盖 broadcast 前崩溃恢复。
  - 无待重播 signed outbound 时，会通过 `OutboundRepository::claim_broadcast_collect_tx_for_receipt` claim `confirming + broadcast` collect outbound，查询 receipt；success 标记 outbound/collection confirmed，reverted 标记 failed，receipt missing 保持 pending。
  - 新增 `spawn_collection_collector_loop` 常驻 loop 初版，按固定 interval 调用 tick，单次错误记录结构化 tracing 后继续运行；覆盖零 poll interval 配置校验。
  - 仍缺确认数/finality 策略和更完整的 finality/reorg 复测。
- `src/workers/transfer_log_ingestor.rs`: transfer log ingestor 常驻 poll loop 初版：
  - `TransferLogIngestorLoop` 包装 `TransferLogIngestor::poll_once`，校验非零 poll interval。
  - `spawn_transfer_log_ingestor_loop` 使用 `tokio::time::interval` 周期 poll，`Advanced/Rewound/Idle` 写结构化 tracing，单次错误记录后继续循环。
  - 单测覆盖配置校验、按 stream tick、poll error 传播。
- `src/runtime.rs`、`src/main.rs`、`src/api/mod.rs`: API runtime composition 初版：
  - `main.rs` 现在通过 `runtime::build_api_router(config)` 异步启动，不再直接使用健康检查壳子。
  - runtime 会校验 profile、连接 `PgPool`、运行 schema migration、seed `wallet_cursors/chain_cursors/treasury_addresses`。
  - runtime 会创建 KVDB 父目录、打开 redb-backed `RedbTransferLogIngestor`，并为配置链/token 初始化 stream。
  - runtime 会 spawn redb-backed transfer log ingestor poll loop，持续把 ERC20 `Transfer` raw logs 写入 KVDB，供 manual verify 和后续 scanner 读取。
  - runtime 会创建 `RpcRangeSource`，启动时校验 RPC `eth_chainId`。
  - runtime 会创建 JWT verifier，并把 `PgOrderRepository + HdWallet<DeterministicFakeDeriver> + RpcRangeSource` 组装进真实 `OrderService`。
  - runtime 会把 `ManualOrderVerifyService` 接到 API verify route，依赖 `PgOrderRepository`、`PgVerifiedPaymentRecorder`、redb log reader 和 `RpcRangeSource`。
  - runtime 会把 `CollectionService` 接到 API collection route，依赖 `PgOrderRepository`、`PgCollectionRepository`、`PgOutboundRepository`、`PgAuditRepository`、`DeterministicFakeSigner`、`RpcRangeSource` 和 `AssumePrefundedGas`；collection fee 和 collector replacement timeout 现在由 `AppConfig` 驱动，不再是代码常量。
  - runtime 会组装并 spawn payment scanner loop，依赖 `PgPaymentRepository`、`RedbTransferLogIngestor` reader、`WatchSetPaymentWindowLookup<RepositoryPaymentWindowLookup<PgOrderRepository>>` 和 `RpcRangeSource` head reader，并把 worker tick metrics 接入 `/readyz` 和 `/metrics`。
  - runtime 会组装并 spawn collection collector loop，依赖 `CollectionService`、`PgOutboundRepository` 和 `RpcRangeSource`，同样记录 worker tick metrics。
  - `api::router_with_runtime_services` 可同时挂订单 create/read、manual verify 和 collection create/read；旧同步 `api::router(config)` 不再伪装 ready，会返回未 bootstrap 的 dependency failure。
  - 注意：当前 runtime 只允许 non-production `SIGNER_MODE=fake` 跑通开发/联调路径；external/KMS/HSM signer adapter 仍未实现，production 仍不可用。
- 订单 API 初版：
  - `POST /v1/orders`: `orders:create` scope，解析 decimal `amount` -> raw amount，返回 `201 created` 或幂等 `200 ok`。
  - `GET /v1/orders/{id}`: `orders:read` scope。
  - `GET /v1/orders/by-external-id/{external_id}`: `orders:read` scope。
  - `POST /v1/orders/{id}/verify`: `orders:verify` scope，通过 `OrderVerifyApiService` trait 注入；已有 manual verify service adapter，并已接入 API runtime composition 初版。
  - API 通过 `OrderApiService` trait 注入 service；真实启动路径已接 Pg/RPC/redb/JWT。
- 归集 API 初版：
  - `POST /v1/collections`: `collections:create` scope。
  - `GET /v1/collections/{id}`: `collections:read` scope。
  - request 只接受 `order_id`、`amount`、`idempotency_key`，显式拒绝未知字段，避免客户端传 `to_address`。
  - MVP API 只接受 `amount=max`，不会暴露任意 exact amount 或 treasury override；具体归集金额仍由 service 根据子地址 token balance 解析。
  - 返回 collection id/order/chain/token/status/from/to/amount/outbound/attempt/error/时间字段，`Created` -> `201`、`Existing` -> `200`。
  - 已接入真实 runtime composition 初版；仍缺 collector recovery loop、receipt/retry/replacement 和 e2e。
- `src/transfer_log_store/types.rs`: transfer log stream/cursor/log/header/page token canonical primitives。
- `src/transfer_log_store/mod.rs`: in-memory `TransferLogIngestor`/`TransferLogReader`，以及 redb-backed `RedbTransferLogIngestor` runtime 初版；覆盖连续扫描、空块 header、exclusive page token、reorg rewind、capacity gate 和持久化读路径。
- `src/transfer_log_store/redb_store.rs`: redb persistence layer，支持 config/cursor/header/log/range manifest 持久化、stream config + cursor 原子初始化、atomic batch write、bounded `logs_in_range`、exclusive `logs_page`、rewind delete。
- `tests/migration_contract.rs`: migration contract 测试；有 `PAY3_TEST_DATABASE_URL`/`TEST_DATABASE_URL` 时会实际 apply 到临时 schema，否则跳过 DB apply 分支。
- `tests/collection_db_integration.rs`: 真实 PostgreSQL 集成测试，临时 schema + migrations 之后直接用 SQL 验证 collection/outbound trigger、collect-only purpose check 和 outbound active nonce replacement trajectory；无数据库 URL 时自动跳过。
- `tests/anvil_e2e.rs`: 真实 Anvil + mock ERC20 e2e，覆盖订单创建、Transfer log 采集、scanner 付款确认、collection 广播/确认、stuck replacement 和 treasury 余额断言。
- `tests/support/anvil.rs`: Anvil/Foundry 测试支撑层，封装 mnemonic 派生、mock ERC20 部署和 ERC20 转账广播。
- `tests/repository_contract.rs`: repository SQL/contract 静态测试，覆盖幂等、lease/CAS、matched-only payments、collection job lock、nonce/replacement、audit insert。
- `tests/chain_contract.rs`: chain 模块静态 contract 测试，确保暴露必要 trait/fake/RPC 控制点，且不依赖 axum API DTO、DB 或订单业务状态。
- `tests/signer_contract.rs`: signer fake/contract 测试。
- `tests/payment_window_lookup_contract.rs`: watch-set hit/miss 和批量 fallback contract 测试。
- `tests/payment_matching_contract.rs`: 付款匹配分页、候选过滤、窗口判断、确认数和歧义拒绝 contract 测试。
- `workers::scanner` 单元测试：覆盖 lease held、claim+commit、KV reorg epoch、空 page 推进、page incomplete 不提交、CAS mismatch 冒泡。
- `tests/manual_verify_service_contract.rs` + `tests/support/manual_verify/**`: manual verify service contract 测试和可复用 fake/fixture。
- `tests/order_verify_api_contract.rs`: 手动 verify API scope/error/response route contract 测试。
- `tests/transfer_log_store_types_contract.rs`、`tests/transfer_log_redb_contract.rs`: transfer log type/redb persistence contract 测试。
- `Agent.md`: 项目协作规则、MVP 边界、模块边界、测试策略。
- `docs/MVP_ARCHITECTURE.md`: ERC20 收款平台 MVP 架构、API、数据库、事务、缓存、归集设计。
- `docs/END_TO_END_FLOW.md`: 端到端整体流程，覆盖订单、KVDB 扫链、付款匹配、确认、归集和恢复。
- `docs/MODULE_PLAN.md`: 一个模块一个模块实现、测试和联调的规范。
- `docs/TRANSFER_LOG_KV_MODULE.md`: 独立 ERC20 Transfer log KVDB 采集模块设计。
- `docs/PRODUCTION_READINESS.md`: 多 Agent 审计后的 MVP 生产验收项和上线清单。
- `docs/DEPLOYMENT.md`: 部署拓扑、worker 锁、RPC provider 和 readiness 要求。
- `docs/RUNBOOK.md`: RPC、reorg、collection、KVDB rebuild、DB 恢复等 MVP runbook。
- `nextsession.md`: 当前交接文档。

最近验证：

- `cargo fmt -- --check`: 通过。
- `cargo test --test anvil_e2e`: 通过，2 个 Anvil e2e 测试。
- `cargo test`: 上次全量通过，144 个库测试 + 58 个 integration/contract 测试；当时 Anvil e2e 只有 1 个测试，本轮新增 replacement 场景后尚未重新全量复跑：
  - chain 2、collection_db integration 2、manual verify service 5、migration 6、order verify API 8、payment matching 9、payment window lookup 4、repository 5、signer 5、transfer log redb 7、transfer log store types 5。

## 多 Agent 审计结论

当前项目仍不可用于生产接真实资金。虽然 Phase 1、M4 migration、M5 repository 初版、M6 wallet、M7 signer contract/fake、订单创建 service、订单 API route contract、M8 chain 纯契约/fake、RPC provider manager/RpcRangeSource 初版、M9 transfer log store redb-backed runtime 初版和常驻 poll loop、M12 付款匹配纯 service、手动 verify service/API route contract、collection create/read API route contract、API runtime composition 初版、scanner worker tick + 常驻 loop + confirmation sweep + rolling lookback rescan + lag/readiness 初版、`services/collections` prefunded 初版、collector broadcast tick + 常驻 loop 初版和 broadcast 前/后崩溃恢复与 receipt sweep tick 初版、collect replacement 初版、collection fee/collector timeout 配置化初版、worker tick metrics/readyz 初版已经完成并通过编译/静态 contract 测试，且已经补了一组真实 PostgreSQL 集成测试和 Anvil ERC20 e2e（包含 collect replacement 回归），但部署工件和演练记录仍未补齐。

原因：

  - 仓库已有订单 API、manual verify、collection create/read、transfer log ingestor poll loop、payment scanner loop/confirmation sweep 和 collector loop 的真实启动组装初版，也已有 collector broadcast 前/后崩溃恢复和 receipt sweep tick 初版、collect replacement 初版、collection fee/collector timeout 配置化初版、worker tick metrics/readyz 初版、scanner lag/readiness 初版；已经新增 collection/outbound 的真实 PostgreSQL 集成测试和 Anvil ERC20 e2e（含 replacement 回归），但部署和 runbook 演练记录仍未补齐。
- runtime 目前只支持 non-production fake signer 作为开发/联调桥接；外部 signer/KMS/HSM adapter 未实现前不能 production。
- 地址复用在 ERC20 场景无法绝对消除迟到付款歧义，已从 MVP 砍掉。
- collect 不能允许任意 `to_address`，必须固定 treasury。
- 需要 reorg/finality 处理，否则可能把孤块付款标为 `paid`。
- collect 必须持久化 nonce/signed transaction，支持广播后崩溃恢复。
- JWT 需要 scope、过期校验、轮换和限流，归集权限不能给普通业务调用方。
- KVDB/redb 现在是 `transfer_log_store` 的 MVP 依赖，用来存 raw Transfer logs；通用 order snapshot/watch set cache 后置，不阻塞资金闭环。
- 可从 PostgreSQL/RPC 重建或回放的数据才进本地 KVDB。
- 扫链原始数据不进 PostgreSQL：raw Transfer logs、非 Pay3 logs、raw RPC response、block header cache、raw scan batches 只能进 KVDB 或内存。
- PostgreSQL 只保存业务真相：orders、child_accounts、payment_windows、matched Pay3 payments、chain_cursors、collections、nonces、outbound transactions。
- 统一错误模型、metrics、告警、备份恢复、RPC 切换、外部 signer、KVDB single-writer、reorg epoch、capacity gate 都已经纳入 MVP 出口标准，不能后置。

## 用户需要先过目的关键决策

- 技术栈是否接受：`axum + sqlx + alloy + jsonwebtoken + redb`；其中 redb 先用于独立 Transfer log KVDB 模块。
- HD 无限派生模板是否接受：`m/44'/60'/{account_index}'/{change_index}/{address_index}`。
- MVP collect 先只做 `prefunded`：子地址预先有 gas，后续 gas station 不进入 MVP。
- 热门 token 支持策略：默认 `RpcRangeSource` 必须通过 capacity probe；否则 log ingestor not ready，必须配置兼容 `TransferLogSource` 的 indexer/分片 source。
- 订单地址策略已经固定：
  - 每个订单派生新地址。
  - 地址永久绑定原订单，不复用。
  - `expires_at` 是业务支付截止。
  - `monitor_until` 用于继续扫描迟到付款并进入人工对账。
- API 是否足够简单：创建订单、查订单、手动 verify、创建 collect、查 collect。

## 下一步实现顺序

实现时必须按 `docs/MODULE_PLAN.md` 的 phase 顺序推进。默认每次只做一个模块；只有用户明确要求时才做一个 phase。每个模块完成独立测试和对应组合联调后再进入下一个模块。

1. 创建 Rust 项目骨架：`Cargo.toml`、`src/main.rs`、模块目录、`.gitignore`。已完成。
2. 加 typed config、tracing、health endpoints。已完成。
   - 同时加 `/metrics`、严格 `/readyz` dependency status、统一错误模型。
3. 实现 domain types。已完成：
   - token amount parser
   - address/hash normalization
   - derivation segment rollover
   - order status machine
   - payment confirmation calculation
   - collection status machine
   - kv reorg epoch
   - outbound replacement invariant
4. 加 JWT verifier/scope 基础。已完成；后续 API 路由接入时仍需把所有 `/v1/*` 端点挂 scope middleware。
5. 写 PostgreSQL migration。已完成：
   - `wallet_cursors`
   - `child_accounts`
   - `orders`
   - `payment_windows`
   - `payments`
   - `chain_cursors`
   - `collections`
   - `account_nonces`
   - `outbound_transactions`
   - `treasury_addresses`
   - `audit_events`
   - address/hash CHECK、payments 地址归属 FK、collections treasury FK/trigger、outbound active nonce partial unique
6. 实现 db repositories。已完成初版：
   - `OrderRepository`
   - `PaymentRepository`
   - `CollectionRepository`
   - `OutboundRepository`
   - `AuditRepository`
   - 幂等冲突 typed error、external_id advisory lock、wallet cursor 原子分配、payment matcher cursor lease/CAS、nonce 串行、signed/broadcast collect outbound recovery claim 和审计写入
7. 实现 `wallet` 模块。已完成：
   - derivation segment -> path/address
   - deterministic fake deriver for tests
   - rollover test 和稳定地址测试
8. 实现 `signer` 模块。已完成 contract/fake：
   - `SignerProvider` trait。
   - deterministic fake signer。
   - production profile 仍禁止 fake/local signer。
9. 实现订单创建 service。已完成初版：
   - 对 `external_id` 使用事务级 advisory lock 或等价幂等预占。
   - 直接锁 `wallet_cursors` 并分配下一个 derivation segment。
   - 实现 `address_index -> change_index -> account_index` rollover。
   - 插入 order 和 payment window。
   - canonical request_hash 使用 `external_id`、raw amount、effective ttl、canonical JSON metadata，不包含当前时间、order id 或派生地址。
   - 创建订单时依赖 `OrderChainHeadReader` 提供当前 head；失败时不分配 wallet segment。
   - 通过 fake repo/fake head/deterministic wallet 覆盖创建、幂等复用、冲突、链头失败、输入校验。
10. 实现订单 API route contract。已完成初版：
   - `POST /v1/orders`、`GET /v1/orders/{id}`、`GET /v1/orders/by-external-id/{external_id}`。
   - `orders:create` / `orders:read` JWT scope 校验。
   - request/response DTO、decimal amount parser、统一错误映射、幂等 conflict `409`。
   - fake `OrderApiService` route tests 覆盖 create、existing、conflict、invalid amount、read scope。
   - 真实启动路径已通过 `runtime::build_api_router` 接入 Pg/RPC/JWT/redb 和 `OrderService`；同步 `api::router(config)` 仅保留为未 bootstrap 壳子。
11. 实现 `chain` 纯契约/fake。已完成初版：
   - `ChainHeaderReader` / `TransferLogSource` / `Erc20ChainClient`。
   - `TransferLogRange`、`TransferLogCapacityLimits`、`TransferLogCapacityReport`。
   - fake 覆盖 block range 批量 logs、token/range filtering、canonical block hash filtering、RPC failure、chain_id mismatch、reorg replacement、capacity probe、balance/receipt/broadcast。
   - 真实 `RpcRangeSource` / provider manager 已完成初版并接入 API runtime 初版；Anvil ERC20 e2e 已补，但动态 readiness 和生产 signer 接入仍待补，不能用于生产 RPC。
12. 实现独立 `transfer_log_store`。已完成 redb-backed runtime 初版/contract：
   - 输入 `chain_id/token_address/start_block`。
   - in-memory ingestor 第一次从 `start_block` 扫，之后从 cursor `next_block` 继续轮询。
   - redb-backed `RedbTransferLogIngestor` 第一次从 `start_block` 初始化 config/cursor，之后从 redb cursor `next_block` 继续轮询。
   - raw Transfer logs、block header cache、range manifest 保持在 memory/redb 层，不写 PostgreSQL。
   - 空 logs 区块也保存 header 并推进 cursor。
   - reader 主路径使用 `logs_page(limit)`，禁止无界读。
   - cursor 保存 `reorg_epoch/last_reorg_from/last_reorg_at/writer_epoch`。
   - redb layer 已有 config/cursor/header/log/range manifest 持久化、stream config + cursor 原子初始化、atomic batch write、bounded `logs_in_range`、exclusive `logs_page`、rewind delete。
   - redb runtime poll 前运行 `TransferLogSource::capacity_probe`；可缩小 batch，单块超阈值时返回 not ready 且不推进 cursor。
   - redb runtime reorg 检测会 rewind KV cursor 并删除分叉块及之后的 headers/logs/range manifest，不触碰 PostgreSQL。
   - redb-backed ingestor poll loop 已接入 API runtime 启动路径，启动后周期执行 `poll_once`。
   - 仍需 retention floor cleanup、readiness/metrics wiring、KVDB coverage/retention 对外状态和真实生产 readiness 回归。
13. 实现 `PaymentWindowLookup`。已完成：
   - memory watch set + 批量 fallback trait。
   - fallback miss 去重并一次批量查询；不做 per-address fallback。
   - lookup contract 返回 `Result`；新增 PostgreSQL fallback adapter，批量 join `payment_windows`/`orders`，并把 DB 错误传给 matcher/scanner fail closed。
14. 实现 `services/payments`。已完成纯 service contract：
   - 只使用 `TransferLogReader`、`PaymentWindowLookup`、`ChainHeaderReader`。
   - 使用 KV log `block_timestamp` 判定 `on_time/late/outside_window`。
   - 输出 `MatchedPaymentInput`、`complete_to_block`、`next_token`、`kv_reorg_epoch`。
   - 已抽出 `match_stored_transfer_logs` 供 manual verify 和后续 scanner 复用。
15. 实现 `POST /v1/orders/{id}/verify`。已完成 route contract：
   - `orders:verify` scope。
   - API 通过 `OrderVerifyApiService` trait 注入。
   - 已有 `api::verify_service` adapter 可把 `ManualOrderVerifyService` 接到 API trait。
16. 实现 manual verify service。已完成 contract：
   - 读取订单/payment window。
   - 检查 KV coverage。
   - bounded `logs_in_range` 只读订单窗口，复用 payment matching pure function。
   - 通过 `VerifiedPaymentRecorder` 返回订单 canonical payment set 后用 domain 状态机重算 verify 响应。
   - Pg `VerifiedPaymentRecorder` 已实现并复用 shared payment upsert/recompute helper；API runtime composition 初版已接入。
17. 实现 scanner worker tick contract。已完成初版：
   - `src/workers/scanner.rs` 通过 PostgreSQL payment matcher cursor lease/CAS 驱动 paged matcher。
   - KV `reorg_epoch` 变化时调用 repository 回退业务 cursor 并 orphan 受影响 payments。
   - raw scan batches 仍由 `transfer_log_store` 管理，scanner 不直接 `eth_getLogs`。
   - 空闲 tick 已执行 observed payment confirmation sweep，通过 canonical block hash 校验后批量确认 payment 并重算订单；hash mismatch fail closed。
   - 常驻 loop 初版已完成，单次 tick 错误记录结构化 tracing 后继续运行，并已接入 API runtime composition 初版。
   - 已覆盖 lease held、claim+commit、KV reorg、空 page 推进、page incomplete 不提交、CAS mismatch、confirmation sweep、canonical block mismatch、零 poll interval 配置校验。
   - rolling lookback rescan 和 lag/readiness 初版已完成；仍需真实 DB 集成测试。
18. 实现 RPC provider manager、`ChainHeaderReader`、`TransferLogSource` capacity gate。已完成初版：
   - `JsonRpcProvider` / `HttpJsonRpcProvider` / `RpcProviderManager` / `RpcRangeSource`。
   - provider count gate、`eth_chainId` 校验、latest/safe/finalized head、同高度 hash mismatch fail-closed。
   - `eth_getLogs` ERC20 Transfer range source、capacity probe、`ensure_capacity` gate、429/timeout/error failover。
   - 已接入 API runtime composition 初版；仍需动态 metrics/readyz 和后台 loop 回归。
19. 实现完整 redb-backed `TransferLogIngestor` runtime（或把现有 in-memory ingestor 抽到可替换 storage），替换当前原型路径。已完成初版：
   - `RedbTransferLogIngestor<S>` 实现 `TransferLogIngestor` 和 `TransferLogReader`。
   - 支持 `ensure_stream`、`poll_once`、`rewind_to`、`cursor`、`block_header`、bounded `logs_in_range`、exclusive `logs_page`。
   - poll batch 通过 capacity probe gate，超限 batch 自动缩小；单块超限 fail closed，不写入 headers/logs/cursor。
   - redb contract 测试覆盖 reopen 后读状态、空块 header 持久化、reorg rewind+rescan、capacity gate 不推进 cursor。
   - `workers/transfer_log_ingestor` 已提供常驻 poll loop 并接入 `runtime::build_api_router`，启动后持续写 KV raw logs。
20. 实现 collect job，先只做 `prefunded`，但必须持久化 nonce/signed transaction，支持同 nonce replacement 和审计事件。已完成 service 初版：
   - `services/collections.rs` 提供 `CollectionService`、`CreateCollectionInput`、`CollectionAmount::{Max, Exact}`、`PrefundedGasChecker`。
   - 创建 collection 固定写配置 treasury，要求订单 `paid` 且 chain/token 匹配，生成 canonical request_hash，调用 `CollectionRepository::create_collection_idempotent`。
   - prefunded job 准备流程：claim queued collection、检查 token balance、prefunded gas gate、signer health、reserve nonce、构造 ERC20 transfer calldata、sign、`insert_signed_tx`、`attach_outbound_tx`、audit。
   - 单测覆盖 treasury-only 创建、unpaid 拒绝、余额不足、gas gate 在 nonce/sign 前 fail closed、签名和 outbound 持久化、ERC20 calldata。
   - `workers/collector.rs` 已有 broadcast tick 初版：准备 job 后广播 signed raw tx、校验 tx hash、调用 `mark_broadcast`。
   - collector tick 会优先 claim `transferring + signed` collect outbound 并重播持久化 `signed_tx`，覆盖 broadcast 前崩溃恢复。
   - collector tick 还会 claim `confirming + broadcast` collect outbound 查询 receipt；success -> confirmed，reverted -> failed，missing -> pending。
   - collector 常驻 loop 初版已完成，单次 tick 错误记录结构化 tracing 后继续运行，并已接入 API runtime composition 初版。
   - collector 单测覆盖 no job、broadcast success、hash mismatch fail closed、不合法 worker id、broadcast 前崩溃恢复优先于新 job、receipt success/reverted/missing、零 poll interval 配置校验。
   - `POST /v1/collections` API route contract 初版已完成并接入 runtime：只允许 `amount=max`，拒绝未知字段/`to_address`，使用 `collections:create` scope，映射 created/existing/conflict/dependency 错误。
   - `GET /v1/collections/{id}` API route contract 初版已完成并接入 runtime：使用 `collections:read` scope，返回 collection 状态、outbound 引用、attempt/error 和时间字段，missing -> `404`。
  - 仍需确认数/finality 策略和更完整的真实 DB/Anvil 确认数回归。
21. 实现 API runtime composition。已完成初版：
   - `src/runtime.rs` 组装 PgPool、migration/seed、JWT、RPC chain id probe、redb stream、订单 service 和 manual verify service。
   - runtime 已组装 collection create/read API 所需的 Pg collection/outbound/audit repositories、fake signer、RPC chain client 和 prefunded gas checker。
   - runtime 已组装 payment scanner loop 所需的 Pg payment repository、redb log reader、PostgreSQL payment window batch fallback、RPC head reader，并在启动时 spawn 常驻 loop；worker tick metrics/readyz 和 scanner lag/readiness 初版已接入。
   - `src/main.rs` 已切到真实 runtime builder。
   - non-production 仅支持 `SIGNER_MODE=fake`，production signer adapter 仍未实现。
   - 仍需真实 DB/RPC/Anvil e2e。
22. 用 Anvil + mock ERC20 做全流程 e2e，并补并发、reorg、KVDB rebuild、RPC 切换、崩溃恢复、metrics/alert/runbook drill。

## 全局进度板

| 模块 | 状态 | 备注 |
| --- | --- | --- |
| 文档规划 | 完成 | 等用户过目 |
| 模块实施规范 | 完成 | 见 `docs/MODULE_PLAN.md` |
| MVP 生产验收审计 | 完成 | 结论：当前未完整闭环不可生产；验收项已纳入 MVP，见 `docs/PRODUCTION_READINESS.md` |
| Rust 项目骨架 | 完成 | `cargo check` 通过 |
| 配置、health、readyz、metrics | 完成 | typed config、production guard、`/healthz`、`/readyz`、`/metrics`、统一错误模型 |
| JWT 鉴权 | 完成基础 | verifier/scope 已完成；订单、verify、collection create/read API 已接 scope 校验，后续新 `/v1/*` 端点仍需逐个接入 |
| PostgreSQL migrations | 完成 | sqlx migrator、初始 schema、运行时 seed helper、migration contract 测试；真实 DB apply 需设置 `PAY3_TEST_DATABASE_URL` |
| db repositories | 完成初版 | traits + Pg repositories 已接入编译；repository contract 测试通过；还需有测试 DB 后补真实并发/负例集成测试 |
| domain 状态机 | 完成 | amount/address/hash、order/payment/collection、KV reorg epoch、derivation rollover |
| HD wallet 无限派生 | 完成 | `HdWallet` + `AddressDeriver` trait + deterministic fake deriver；稳定地址和 rollover 测试通过 |
| signer contract/fake | 完成 | `SignerProvider` + deterministic fake signer；production profile 仍禁止 fake/local signer |
| 创建订单 service | 完成初版 | canonical request_hash、链头窗口、wallet 派生、fake repo/head 测试通过；真实 DB 并发测试后补 |
| 创建订单 API | 完成 route + runtime 初版 | `POST /v1/orders` / `GET /v1/orders/{id}` / by-external-id，scope 和错误映射测试通过；真实启动路径已接 Pg/RPC/JWT/redb，真实 DB/Anvil e2e 未做 |
| chain 纯契约/fake | 完成初版 | traits + normalized logs/headers/receipt + fake range/reorg/failure/capacity 测试；真实 RPC provider manager 初版已完成，Anvil ERC20 测试未做 |
| PaymentWindowLookup | 完成 | watch set + 批量 fallback contract；新增 PostgreSQL `payment_windows JOIN orders` fallback adapter，lookup 错误会 fail closed；禁止逐地址 fallback |
| transfer_log_store | 完成 M9 runtime 初版 | canonical types + in-memory ingestor/reader + redb-backed ingestor/reader + runtime poll loop + redb persistence contract；retention cleanup、readiness/metrics wiring、Anvil 测试未做 |
| services/payments | 完成纯 service contract | `TransferLogReader::logs_page` -> candidate lookup -> `MatchedPaymentInput`；`match_stored_transfer_logs` 已供 verify/scanner 复用 |
| RPC provider manager / LogSource | 完成初版 | `HttpJsonRpcProvider` + `RpcProviderManager` + `RpcRangeSource`，含 chain_id 校验、hash mismatch fail-closed、capacity gate、failover；已接 API runtime 初版，metrics/Anvil 测试未做 |
| API runtime composition | 完成初版 | `runtime::build_api_router` 连接 Pg、跑 migration/seed、打开 redb、启动 transfer log poll loop、payment scanner loop 和 collector loop、校验 RPC chain id、挂订单、verify 和 collection create/read；仅支持 non-production fake signer；worker tick metrics/readyz 和 scanner lag/readiness 初版已接入 |
| 付款 verify | 完成 service + route + runtime 初版 | `POST /v1/orders/{id}/verify` + `orders:verify` scope；manual service 已复用 matcher；Pg recorder 已完成并接入 runtime；真实 DB/Anvil e2e 未做 |
| scanner worker | 完成初版 | tick contract + 常驻 loop + confirmation sweep + rolling lookback rescan 初版已完成：lease/CAS、KV reorg epoch、paged matcher、commit batch、canonical block hash 校验、结构化 tick 日志、worker tick metrics/readyz；真实 DB 集成测试未做 |
| redb/KVDB | 完成 transfer log KV 初版 | `transfer_log_store/redb_store.rs` + `RedbTransferLogIngestor` 通过 contract；通用 cache 后置 |
| services/collections | 完成初版 | treasury-only create + prefunded job prepare + signer/outbound/audit contract；collection create/read API route/runtime 已完成初版；broadcast 前/后崩溃恢复和 receipt sweep 已由 collector/outbound claim 覆盖；collection fee/collector timeout 已改为 AppConfig 驱动；已新增 collection/outbound 真实 PostgreSQL 集成测试和 Anvil replacement e2e |
| collect worker | 完成初版 | broadcast tick + 常驻 loop 初版已完成：优先重播 transferring+signed outbound，其次检查 confirming+broadcast receipt，receipt 长时间缺失时触发同 nonce replacement/gas bump 初版；replacement 路径已补 Anvil e2e，仍需确认数/finality 策略 |
| 外部 signer adapter | 未开始 | 已有 signer trait/fake；production 需要 KMS/HSM/external signer adapter |
| 可观测性/告警 | 部分完成 | `/metrics`、结构化日志已接入，alert dry-run 仍未开始 |
| e2e 测试 | 部分完成 | Anvil + mock ERC20 happy path + stuck collect replacement |
| 部署/runbook | 文档完成 | 见 `docs/DEPLOYMENT.md`、`docs/RUNBOOK.md`；演练未开始 |
| MVP 出口准入 | 未开始 | `docs/PRODUCTION_READINESS.md` 清单全部通过才可评估真实资金 |

## 不要偏离的约束

- 不要先做多账号、多链、多 token。
- 不要先做复杂后台或前端。
- 不要跳过模块边界；每次实现必须说明当前模块、依赖、接口契约是否变更、fake/mock 和测试。
- 不要让缓存成为资金状态来源。
- 不要把通用 cache 做成首个 MVP 阻塞项；但 `transfer_log_store` 的 KVDB raw log 存储是付款验证路径的必需模块。
- 不要把 orders/payments/chain_cursors/nonces/signed_tx 的权威状态放进 KVDB。
- 不要把 raw Transfer logs、非 Pay3 logs、raw RPC response、raw scan batches、全量 block headers 放进 PostgreSQL。
- 不要实现地址复用。
- 不要跳过 PostgreSQL 的每地址只绑定一个订单强约束。
- 不要允许 API 调用方传任意归集目标地址。
- 不要在没有 reorg/finality 处理时把订单用于真实资金。
- 不要在没有 nonce/signed transaction 持久化时上线 collect。
- 不要在没有 outbound_transactions/account_nonces 时实现 collect worker。
- 不要在没有 DB 级 treasury 约束时实现 collect。
- 不要实现 pending 订单逐个 RPC 轮询；付款验证必须按 token `Transfer` logs 批量扫描。
- 不要逐个 `to_address` 查询；本地 watch set 只做候选，miss 必须批量 fallback PostgreSQL，并加批大小、限流和熔断。
- 不要无界读取 KVDB logs；scanner 主路径必须使用 `logs_page(limit)`。
- 不要在 KVDB 覆盖不足或 reorg epoch 未同步时推进 PostgreSQL cursor。
- 不要让 `RpcRangeSource` 在 capacity probe 失败时继续 ready。
- 不要在没有 metrics/readyz/alert dry-run/runbook drill 时声明 MVP 完成。
- 不要在日志中输出 mnemonic、private key、JWT。
- 每次 session 都先看这个进度板，再补最影响全流程的缺口。
