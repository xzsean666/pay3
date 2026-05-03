# Pay3 Next Session

更新时间：2026-05-03

## 当前状态

仓库已经从纯文档进入 Rust 实现阶段。当前完成 Phase 1 基础模块，已完成 Phase 2 的 M4 `db/migrations` 和 M5 `db/repositories` 初版，已完成 Phase 3 的 M6 `wallet`、订单创建 service 初版和订单 API route contract，已推进 Phase 4 的 signer、chain、RPC provider manager、transfer log store、付款匹配 service 和手动 verify API route contract，并完成 Phase 5 `workers/scanner` tick contract 初版。

已完成：

- Rust 项目骨架：`Cargo.toml`、`Cargo.lock`、`src/main.rs`、`src/lib.rs`、基础模块目录。
- `src/config.rs`: typed env config，production profile guard，token decimals/symbol，敏感配置 Debug 脱敏。
- `src/domain/**`: amount/address/hash 规范化、derivation segment rollover、订单/付款/归集状态规则（含 `partial`）、KV reorg epoch、collection replacement invariant。
- `src/auth/**`: JWT HS256 bearer verifier、`exp/nbf/iat/iss/aud/sub/kid/alg` 校验、scope/principal，已包含 `orders:create`、`orders:read`、`orders:verify`、`collections:create`。
- `src/api/mod.rs`、`src/health.rs`、`src/error.rs`: `/healthz`、`/readyz`、`/metrics`、统一错误响应、订单 API route contract。
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
  - 支持 `balanceOf`、receipt 查询、signed tx broadcast 的 RPC adapter 初版；尚未做 Anvil ERC20 集成测试和真实 runtime wiring。
- `src/db/migrations/20260502000100_initial_schema.sql`: PostgreSQL 初始 schema。
- `src/db/migrations.rs`: sqlx migrator 和运行时 seed helper，用于初始化 `wallet_cursors`、`chain_cursors`、`treasury_addresses`。
- `src/db/repositories/**`: Order/Payment/Collection/Outbound/Audit repository trait 和 PostgreSQL 实现初版。
  - `OrderRepository::get_order_view` 已补齐，用于读回 order + child_account + payment_window。
  - `payment_records` / `payment_recompute` 已抽成共享 helper，`PaymentRepository` 和 `PgVerifiedPaymentRecorder` 复用同一套 matched payment upsert + order recompute 逻辑。
- `src/wallet/mod.rs`: HD wallet address derivation boundary，`HdWallet`、`AddressDeriver` trait、deterministic fake deriver、稳定地址/rollover/key_ref/path 负例测试。
- `src/signer/mod.rs`: `SignerProvider` contract、`UnsignedTx`/`SignedTx` DTO、deterministic fake signer；fake signer 派生地址与 deterministic wallet 保持一致。
- `src/services/orders.rs`: 订单创建/query service 初版，负责 canonical request_hash、idempotency pre-check、当前链头窗口下界、wallet cursor 分配、地址派生、order/payment_window command 拼装。
- `src/services/payment_windows.rs`: watch-set + 批量 fallback 的 `PaymentWindowLookup`，miss 走批量查询接口，不做逐地址 fallback。
- `src/services/payments.rs`: 纯付款匹配 service contract，使用 `TransferLogReader::logs_page`、`PaymentWindowLookup` 和 `ChainHeaderReader`，输出 `MatchedPaymentInput`、`next_token`、`complete_to_block`、`kv_reorg_epoch`。
- `src/services/verify/**`: manual verify service contract，按订单窗口读取 bounded KV logs，复用 `services::payments::match_stored_transfer_logs`，通过 `VerifiedPaymentRecorder` 小 trait 与具体 Pg 写入解耦。
- `src/services/mod.rs`: services 模块入口。
- `src/workers/scanner.rs`: payment scanner tick contract 初版：
  - 通过 `PaymentRepository::claim_scan_range` 获取 PostgreSQL cursor lease。
  - 检测 KV `reorg_epoch` 变化并调用 `handle_kv_reorg_epoch` 回退业务 cursor / orphan affected payments。
  - 只通过 `PaymentPageMatcher` / `TransferLogReader::logs_page` 跑 paged payment matching，不直接扫 RPC。
  - 使用 `commit_scanned_batch` 同事务提交 matched Pay3 payments、订单重算和 `chain_cursors.last_scanned_block` 推进。
  - 空 page 可推进到 KV `last_completed_block`；page limit 未覆盖完整新区块时不推进 cursor。
- 订单 API 初版：
  - `POST /v1/orders`: `orders:create` scope，解析 decimal `amount` -> raw amount，返回 `201 created` 或幂等 `200 ok`。
  - `GET /v1/orders/{id}`: `orders:read` scope。
  - `GET /v1/orders/by-external-id/{external_id}`: `orders:read` scope。
  - `POST /v1/orders/{id}/verify`: `orders:verify` scope，通过 `OrderVerifyApiService` trait 注入；已有 manual verify service adapter，真实 runtime wiring 未完成。
  - API 通过 `OrderApiService` trait 注入 service；真实 `router(config)` 的 DB/RPC/ChainHeadReader 组装仍未接入。
- `src/transfer_log_store/types.rs`: transfer log stream/cursor/log/header/page token canonical primitives。
- `src/transfer_log_store/mod.rs`: in-memory `TransferLogIngestor`/`TransferLogReader`，以及 redb-backed `RedbTransferLogIngestor` runtime 初版；覆盖连续扫描、空块 header、exclusive page token、reorg rewind、capacity gate 和持久化读路径。
- `src/transfer_log_store/redb_store.rs`: redb persistence layer，支持 config/cursor/header/log/range manifest 持久化、stream config + cursor 原子初始化、atomic batch write、bounded `logs_in_range`、exclusive `logs_page`、rewind delete。
- `tests/migration_contract.rs`: migration contract 测试；有 `PAY3_TEST_DATABASE_URL`/`TEST_DATABASE_URL` 时会实际 apply 到临时 schema，否则跳过 DB apply 分支。
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
- `cargo check`: 通过。
- `cargo test`: 通过，101 个库测试 + 56 个 integration/contract 测试：
  - chain 2、manual verify service 5、migration 6、order verify API 8、payment matching 9、payment window lookup 4、repository 5、signer 5、transfer log redb 7、transfer log store types 5。

## 多 Agent 审计结论

当前项目仍不可用于生产接真实资金。虽然 Phase 1、M4 migration、M5 repository 初版、M6 wallet、M7 signer contract/fake、订单创建 service、订单 API route contract、M8 chain 纯契约/fake、RPC provider manager/RpcRangeSource 初版、M9 transfer log store redb-backed runtime 初版、M12 付款匹配纯 service、手动 verify service/API route contract 和 scanner worker tick contract 初版已经完成并通过编译/静态 contract 测试，但还没有真实 DB/RPC API wiring、真实 DB 集成测试、Anvil ERC20 集成测试、scanner runtime loop/confirmation sweep/readiness、collect worker、部署工件和演练记录。

原因：

- 仓库还没有订单 API 的真实 DB/RPC runtime wiring、manual verify 的真实 runtime wiring、scanner runtime loop/confirmation sweep/metrics/readiness、collect worker、真实 DB 集成测试、Anvil ERC20 集成测试、部署和 runbook 演练记录。
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
   - 幂等冲突 typed error、external_id advisory lock、wallet cursor 原子分配、payment matcher cursor lease/CAS、nonce 串行和审计写入
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
   - 注意：真实 `router(config)` 暂未挂订单 service，因为还缺 DB pool + real ChainHeadReader/RPC provider wiring；不能把 fake head 用于生产。
11. 实现 `chain` 纯契约/fake。已完成初版：
   - `ChainHeaderReader` / `TransferLogSource` / `Erc20ChainClient`。
   - `TransferLogRange`、`TransferLogCapacityLimits`、`TransferLogCapacityReport`。
   - fake 覆盖 block range 批量 logs、token/range filtering、canonical block hash filtering、RPC failure、chain_id mismatch、reorg replacement、capacity probe、balance/receipt/broadcast。
   - 真实 `RpcRangeSource` / provider manager 已完成初版；注意还没有 Anvil ERC20 集成测试、runtime wiring 和生产 readiness 接入，不能用于生产 RPC。
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
   - 仍需 retention floor cleanup、runtime loop/readiness/metrics wiring、KVDB coverage/retention 对外状态和 Anvil ERC20 集成测试。
13. 实现 `PaymentWindowLookup`。已完成：
   - memory watch set + 批量 fallback trait。
   - fallback miss 去重并一次批量查询；不做 per-address fallback。
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
   - Pg `VerifiedPaymentRecorder` 已实现并复用 shared payment upsert/recompute helper；真实 runtime wiring 未完成。
17. 实现 scanner worker tick contract。已完成初版：
   - `src/workers/scanner.rs` 通过 PostgreSQL payment matcher cursor lease/CAS 驱动 paged matcher。
   - KV `reorg_epoch` 变化时调用 repository 回退业务 cursor 并 orphan 受影响 payments。
   - raw scan batches 仍由 `transfer_log_store` 管理，scanner 不直接 `eth_getLogs`。
   - 已覆盖 lease held、claim+commit、KV reorg、空 page 推进、page incomplete 不提交、CAS mismatch。
   - 仍需 runtime loop、confirmation sweep、rolling lookback/coverage gate、metrics/readiness、真实 DB 集成测试。
18. 实现 RPC provider manager、`ChainHeaderReader`、`TransferLogSource` capacity gate。已完成初版：
   - `JsonRpcProvider` / `HttpJsonRpcProvider` / `RpcProviderManager` / `RpcRangeSource`。
   - provider count gate、`eth_chainId` 校验、latest/safe/finalized head、同高度 hash mismatch fail-closed。
   - `eth_getLogs` ERC20 Transfer range source、capacity probe、`ensure_capacity` gate、429/timeout/error failover。
   - 仍需 runtime wiring、metrics/readyz 接入和 Anvil ERC20 集成测试。
19. 实现完整 redb-backed `TransferLogIngestor` runtime（或把现有 in-memory ingestor 抽到可替换 storage），替换当前原型路径。已完成初版：
   - `RedbTransferLogIngestor<S>` 实现 `TransferLogIngestor` 和 `TransferLogReader`。
   - 支持 `ensure_stream`、`poll_once`、`rewind_to`、`cursor`、`block_header`、bounded `logs_in_range`、exclusive `logs_page`。
   - poll batch 通过 capacity probe gate，超限 batch 自动缩小；单块超限 fail closed，不写入 headers/logs/cursor。
   - redb contract 测试覆盖 reopen 后读状态、空块 header 持久化、reorg rewind+rescan、capacity gate 不推进 cursor。
20. 实现 collect job，先只做 `prefunded`，但必须持久化 nonce/signed transaction，支持同 nonce replacement 和审计事件。
21. 用 Anvil + mock ERC20 做全流程 e2e，并补并发、reorg、KVDB rebuild、RPC 切换、崩溃恢复、metrics/alert/runbook drill。

## 全局进度板

| 模块 | 状态 | 备注 |
| --- | --- | --- |
| 文档规划 | 完成 | 等用户过目 |
| 模块实施规范 | 完成 | 见 `docs/MODULE_PLAN.md` |
| MVP 生产验收审计 | 完成 | 结论：当前未完整闭环不可生产；验收项已纳入 MVP，见 `docs/PRODUCTION_READINESS.md` |
| Rust 项目骨架 | 完成 | `cargo check` 通过 |
| 配置、health、readyz、metrics | 完成 | typed config、production guard、`/healthz`、`/readyz`、`/metrics`、统一错误模型 |
| JWT 鉴权 | 完成基础 | verifier/scope 已完成；订单 API 已接 scope 校验，后续新 `/v1/*` 端点仍需逐个接入 |
| PostgreSQL migrations | 完成 | sqlx migrator、初始 schema、运行时 seed helper、migration contract 测试；真实 DB apply 需设置 `PAY3_TEST_DATABASE_URL` |
| db repositories | 完成初版 | traits + Pg repositories 已接入编译；repository contract 测试通过；还需有测试 DB 后补真实并发/负例集成测试 |
| domain 状态机 | 完成 | amount/address/hash、order/payment/collection、KV reorg epoch、derivation rollover |
| HD wallet 无限派生 | 完成 | `HdWallet` + `AddressDeriver` trait + deterministic fake deriver；稳定地址和 rollover 测试通过 |
| signer contract/fake | 完成 | `SignerProvider` + deterministic fake signer；production profile 仍禁止 fake/local signer |
| 创建订单 service | 完成初版 | canonical request_hash、链头窗口、wallet 派生、fake repo/head 测试通过；真实 DB 并发测试后补 |
| 创建订单 API | 完成 route contract | `POST /v1/orders` / `GET /v1/orders/{id}` / by-external-id，scope 和错误映射测试通过；真实 runtime wiring 等 chain/RPC/DB 组装 |
| chain 纯契约/fake | 完成初版 | traits + normalized logs/headers/receipt + fake range/reorg/failure/capacity 测试；真实 RPC provider manager 初版已完成，Anvil ERC20 测试未做 |
| PaymentWindowLookup | 完成 | watch set + 批量 fallback contract；禁止逐地址 fallback |
| transfer_log_store | 完成 M9 runtime 初版 | canonical types + in-memory ingestor/reader + redb-backed ingestor/reader + redb persistence contract；retention cleanup、readiness/metrics wiring、Anvil 测试未做 |
| services/payments | 完成纯 service contract | `TransferLogReader::logs_page` -> candidate lookup -> `MatchedPaymentInput`；`match_stored_transfer_logs` 已供 verify/scanner 复用 |
| RPC provider manager / LogSource | 完成初版 | `HttpJsonRpcProvider` + `RpcProviderManager` + `RpcRangeSource`，含 chain_id 校验、hash mismatch fail-closed、capacity gate、failover；runtime wiring/metrics/Anvil 测试未做 |
| 付款 verify | 完成 service + route contract | `POST /v1/orders/{id}/verify` + `orders:verify` scope；manual service 已复用 matcher；Pg recorder 已完成；真实 runtime wiring 未完成 |
| scanner worker | 部分完成 | tick contract 初版已完成：lease/CAS、KV reorg epoch、paged matcher、commit batch；runtime loop、confirmation sweep、rolling lookback/coverage gate、metrics/readiness 未完成 |
| redb/KVDB | 完成 transfer log KV 初版 | `transfer_log_store/redb_store.rs` + `RedbTransferLogIngestor` 通过 contract；通用 cache 后置 |
| collect | 未开始 | MVP 只做 prefunded；目标只能 treasury；必须有 outbound tx/replacement/audit |
| 外部 signer adapter | 未开始 | 已有 signer trait/fake；production 需要 KMS/HSM/external signer adapter |
| 可观测性/告警 | 未开始 | `/metrics`、结构化日志、alert dry-run |
| e2e 测试 | 未开始 | Anvil + mock ERC20 |
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
