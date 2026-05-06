use std::{collections::BTreeMap, num::TryFromIntError};

use alloy_primitives::keccak256;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::{
    db::repositories::{
        AllocatedDerivation, CreateOrderCommand, CreateOrderOutcome, NewChildAccount,
        NewPaymentWindow, OrderRepository, OrderView, RepositoryError,
    },
    domain::{ChainBlockRef, EvmAddress, RawAmount},
    wallet::{AddressDeriver, DeriveAddressRequest, HdWallet, WalletError},
};

pub const DEFAULT_WALLET_CURSOR_ID: &str = "default";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrderServiceConfig {
    pub chain_id: u64,
    pub token_address: EvmAddress,
    pub wallet_cursor_id: String,
    pub late_payment_monitor_seconds: u64,
}

impl OrderServiceConfig {
    pub fn new(
        chain_id: u64,
        token_address: EvmAddress,
        late_payment_monitor_seconds: u64,
    ) -> Self {
        Self {
            chain_id,
            token_address,
            wallet_cursor_id: DEFAULT_WALLET_CURSOR_ID.to_string(),
            late_payment_monitor_seconds,
        }
    }

    pub fn with_wallet_cursor_id(mut self, wallet_cursor_id: impl Into<String>) -> Self {
        self.wallet_cursor_id = wallet_cursor_id.into();
        self
    }

    fn validate(&self) -> Result<(), OrderServiceError> {
        if self.wallet_cursor_id.trim().is_empty() {
            return Err(OrderServiceError::invalid_argument(
                "wallet_cursor_id",
                "must not be empty",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CreateOrderInput {
    pub external_id: String,
    pub expected_amount_raw: RawAmount,
    pub ttl_seconds: u64,
    #[serde(default)]
    pub metadata: Value,
}

impl CreateOrderInput {
    pub fn new(
        external_id: impl Into<String>,
        expected_amount_raw: RawAmount,
        ttl_seconds: u64,
    ) -> Self {
        Self {
            external_id: external_id.into(),
            expected_amount_raw,
            ttl_seconds,
            metadata: Value::Object(Map::new()),
        }
    }

    pub fn with_metadata(mut self, metadata: Value) -> Self {
        self.metadata = metadata;
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CreateOrderServiceOutcome {
    Created,
    Existing,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateOrderResult {
    pub outcome: CreateOrderServiceOutcome,
    pub view: OrderView,
}

#[derive(Debug, Error)]
pub enum OrderServiceError {
    #[error("invalid argument {field}: {message}")]
    InvalidArgument {
        field: &'static str,
        message: String,
    },

    #[error("chain head unavailable: {message}")]
    ChainHeadUnavailable { message: String },

    #[error("created order view was not readable: {order_id}")]
    OrderViewMissing { order_id: Uuid },

    #[error("time calculation overflow for {field}")]
    TimeOverflow { field: &'static str },

    #[error("integer value out of range for {field}")]
    IntegerOutOfRange {
        field: &'static str,
        #[source]
        source: TryFromIntError,
    },

    #[error(transparent)]
    Repository(#[from] RepositoryError),

    #[error(transparent)]
    Wallet(#[from] WalletError),

    #[error("canonical request serialization failed: {0}")]
    CanonicalRequest(#[from] serde_json::Error),
}

impl OrderServiceError {
    pub fn invalid_argument(field: &'static str, message: impl Into<String>) -> Self {
        Self::InvalidArgument {
            field,
            message: message.into(),
        }
    }

    pub fn chain_head_unavailable(message: impl Into<String>) -> Self {
        Self::ChainHeadUnavailable {
            message: message.into(),
        }
    }
}

#[async_trait]
pub trait OrderChainHeadReader: Send + Sync {
    async fn current_head(&self) -> Result<ChainBlockRef, OrderServiceError>;
}

pub trait Clock: Send + Sync {
    fn now(&self) -> OffsetDateTime;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }
}

pub trait IdGenerator: Send + Sync {
    fn new_id(&self) -> Uuid;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct RandomIdGenerator;

impl IdGenerator for RandomIdGenerator {
    fn new_id(&self) -> Uuid {
        Uuid::new_v4()
    }
}

#[derive(Clone, Debug)]
pub struct OrderService<R, D, H, C = SystemClock, I = RandomIdGenerator> {
    config: OrderServiceConfig,
    repository: R,
    wallet: HdWallet<D>,
    head_reader: H,
    clock: C,
    ids: I,
}

impl<R, D, H> OrderService<R, D, H, SystemClock, RandomIdGenerator>
where
    R: OrderRepository,
    D: AddressDeriver,
    H: OrderChainHeadReader,
{
    pub fn new(
        config: OrderServiceConfig,
        repository: R,
        wallet: HdWallet<D>,
        head_reader: H,
    ) -> Result<Self, OrderServiceError> {
        Self::with_dependencies(
            config,
            repository,
            wallet,
            head_reader,
            SystemClock,
            RandomIdGenerator,
        )
    }
}

impl<R, D, H, C, I> OrderService<R, D, H, C, I>
where
    R: OrderRepository,
    D: AddressDeriver,
    H: OrderChainHeadReader,
    C: Clock,
    I: IdGenerator,
{
    pub fn with_dependencies(
        config: OrderServiceConfig,
        repository: R,
        wallet: HdWallet<D>,
        head_reader: H,
        clock: C,
        ids: I,
    ) -> Result<Self, OrderServiceError> {
        config.validate()?;
        Ok(Self {
            config,
            repository,
            wallet,
            head_reader,
            clock,
            ids,
        })
    }

    pub async fn create_order(
        &self,
        input: CreateOrderInput,
    ) -> Result<CreateOrderResult, OrderServiceError> {
        let input = normalize_create_order_input(input)?;
        let request_hash = canonical_order_request_hash(&input)?;

        if let Some(existing) = self
            .repository
            .get_order_by_external_id(&input.external_id)
            .await?
        {
            if existing.request_hash != request_hash {
                return Err(RepositoryError::idempotency_conflict(
                    "orders",
                    &input.external_id,
                    Some(existing.id),
                )
                .into());
            }

            let view = self.expect_order_view(existing.id).await?;
            return Ok(CreateOrderResult {
                outcome: CreateOrderServiceOutcome::Existing,
                view,
            });
        }

        let window_from_block = self.head_reader.current_head().await?;
        let allocated = self
            .repository
            .allocate_derivation_segment(self.config.wallet_cursor_id.trim())
            .await?;
        let derived = self.derive_address(&allocated).await?;

        let now = self.clock.now();
        let ttl = duration_from_secs(input.ttl_seconds, "ttl_seconds")?;
        let late_monitor = duration_from_secs(
            self.config.late_payment_monitor_seconds,
            "late_payment_monitor_seconds",
        )?;
        let expires_at = checked_add(now, ttl, "expires_at")?;
        let monitor_until = checked_add(expires_at, late_monitor, "monitor_until")?;

        let order_id = self.ids.new_id();
        let child_account_id = self.ids.new_id();
        let payment_window_id = self.ids.new_id();

        let command = CreateOrderCommand {
            order_id,
            external_id: input.external_id,
            request_hash,
            child_account: NewChildAccount {
                id: child_account_id,
                signer_key_ref: derived.signer_key_ref,
                derivation_version: derived.derivation_version,
                derivation_segment: derived.segment,
                derivation_path: derived.derivation_path,
                address: derived.address,
            },
            chain_id: self.config.chain_id,
            token_address: self.config.token_address,
            expected_amount_raw: input.expected_amount_raw,
            payment_window: NewPaymentWindow {
                id: payment_window_id,
                order_id,
                child_account_id,
                receive_address: derived.address,
                window_from: now,
                window_from_block,
                expires_at,
                monitor_until,
            },
        };

        let (outcome, order_id) = match self.repository.create_order_idempotent(command).await? {
            CreateOrderOutcome::Created(order) => (CreateOrderServiceOutcome::Created, order.id),
            CreateOrderOutcome::Existing(order) => (CreateOrderServiceOutcome::Existing, order.id),
        };
        let view = self.expect_order_view(order_id).await?;

        Ok(CreateOrderResult { outcome, view })
    }

    pub async fn get_order(&self, id: Uuid) -> Result<Option<OrderView>, OrderServiceError> {
        self.repository.get_order_view(id).await.map_err(Into::into)
    }

    pub async fn get_order_by_external_id(
        &self,
        external_id: &str,
    ) -> Result<Option<OrderView>, OrderServiceError> {
        let Some(order) = self
            .repository
            .get_order_by_external_id(external_id)
            .await?
        else {
            return Ok(None);
        };
        let view = self.expect_order_view(order.id).await?;
        Ok(Some(view))
    }

    async fn derive_address(
        &self,
        allocated: &AllocatedDerivation,
    ) -> Result<crate::wallet::DerivedChildAddress, OrderServiceError> {
        let request = DeriveAddressRequest::new(
            allocated.signer_key_ref.clone(),
            allocated.derivation_version,
            allocated.segment,
        )?;
        let derived = self.wallet.derive_child_address(request).await?;
        if derived.derivation_path != allocated.derivation_path {
            return Err(OrderServiceError::invalid_argument(
                "derivation_path",
                format!(
                    "allocated path {} did not match wallet path {}",
                    allocated.derivation_path, derived.derivation_path
                ),
            ));
        }
        Ok(derived)
    }

    async fn expect_order_view(&self, order_id: Uuid) -> Result<OrderView, OrderServiceError> {
        self.repository
            .get_order_view(order_id)
            .await?
            .ok_or(OrderServiceError::OrderViewMissing { order_id })
    }
}

#[derive(Clone, Debug, PartialEq)]
struct NormalizedCreateOrderInput {
    external_id: String,
    expected_amount_raw: RawAmount,
    ttl_seconds: u64,
    metadata: Value,
}

fn normalize_create_order_input(
    input: CreateOrderInput,
) -> Result<NormalizedCreateOrderInput, OrderServiceError> {
    let external_id = input.external_id.trim().to_string();
    if external_id.is_empty() {
        return Err(OrderServiceError::invalid_argument(
            "external_id",
            "must not be empty",
        ));
    }
    if input.expected_amount_raw.is_zero() {
        return Err(OrderServiceError::invalid_argument(
            "expected_amount_raw",
            "must be greater than zero",
        ));
    }
    if input.ttl_seconds == 0 {
        return Err(OrderServiceError::invalid_argument(
            "ttl_seconds",
            "must be greater than zero",
        ));
    }

    let metadata = canonical_metadata(input.metadata)?;
    Ok(NormalizedCreateOrderInput {
        external_id,
        expected_amount_raw: input.expected_amount_raw,
        ttl_seconds: input.ttl_seconds,
        metadata,
    })
}

fn canonical_order_request_hash(
    input: &NormalizedCreateOrderInput,
) -> Result<String, OrderServiceError> {
    #[derive(Serialize)]
    struct CanonicalRequest<'a> {
        external_id: &'a str,
        amount_raw: String,
        ttl_seconds: u64,
        metadata: &'a Value,
    }

    let canonical = CanonicalRequest {
        external_id: &input.external_id,
        amount_raw: input.expected_amount_raw.to_string(),
        ttl_seconds: input.ttl_seconds,
        metadata: &input.metadata,
    };
    let bytes = serde_json::to_vec(&canonical)?;
    Ok(keccak256(bytes).to_string())
}

fn canonical_metadata(value: Value) -> Result<Value, OrderServiceError> {
    match value {
        Value::Null => Ok(Value::Object(Map::new())),
        Value::Object(_) => Ok(sort_json_value(value)),
        _ => Err(OrderServiceError::invalid_argument(
            "metadata",
            "must be a JSON object when provided",
        )),
    }
}

fn sort_json_value(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let sorted = map
                .into_iter()
                .map(|(key, value)| (key, sort_json_value(value)))
                .collect::<BTreeMap<_, _>>();
            let mut map = Map::new();
            for (key, value) in sorted {
                map.insert(key, value);
            }
            Value::Object(map)
        }
        Value::Array(values) => Value::Array(values.into_iter().map(sort_json_value).collect()),
        value => value,
    }
}

fn duration_from_secs(seconds: u64, field: &'static str) -> Result<Duration, OrderServiceError> {
    let seconds = i64::try_from(seconds)
        .map_err(|source| OrderServiceError::IntegerOutOfRange { field, source })?;
    Ok(Duration::seconds(seconds))
}

fn checked_add(
    time: OffsetDateTime,
    duration: Duration,
    field: &'static str,
) -> Result<OffsetDateTime, OrderServiceError> {
    time.checked_add(duration)
        .ok_or(OrderServiceError::TimeOverflow { field })
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeMap, VecDeque},
        sync::Mutex,
    };

    use async_trait::async_trait;
    use serde_json::json;
    use time::macros::datetime;

    use super::*;
    use crate::{
        db::repositories::{AllocatedDerivation, OrderRecord},
        domain::{BlockHash, DerivationSegment, OrderStatus},
        wallet::DeterministicFakeDeriver,
    };

    #[tokio::test]
    async fn create_order_allocates_derives_and_persists_payment_window() {
        let order_id = Uuid::from_u128(1);
        let child_account_id = Uuid::from_u128(2);
        let payment_window_id = Uuid::from_u128(3);
        let repo = FakeOrderRepository::new(vec![allocated(7, 8, 9)]);
        let service = service(
            repo.clone(),
            FakeHeadReader::ok(block(123)),
            FixedClock(datetime!(2026-05-03 00:00:00 UTC)),
            SequenceIds::new([order_id, child_account_id, payment_window_id]),
        );

        let result = service
            .create_order(
                CreateOrderInput::new(" merchant-order-10001 ", RawAmount::from(12_345_600), 900)
                    .with_metadata(json!({"note": "optional"})),
            )
            .await
            .unwrap();

        assert_eq!(result.outcome, CreateOrderServiceOutcome::Created);
        assert_eq!(result.view.order.id, order_id);
        assert_eq!(result.view.order.external_id, "merchant-order-10001");
        assert_eq!(result.view.order.status, OrderStatus::Pending);
        assert_eq!(result.view.child_account.id, child_account_id);
        assert_eq!(
            result.view.child_account.derivation_path,
            "m/44'/60'/7'/8/9"
        );
        assert_eq!(
            result.view.child_account.address.to_lower_hex(),
            "0x0db486831dd0dd9148fbc7ef9b086a8c9f6044c7"
        );
        assert_eq!(result.view.payment_window.id, payment_window_id);
        assert_eq!(result.view.payment_window.window_from_block, block(123));
        assert_eq!(
            result.view.payment_window.expires_at,
            datetime!(2026-05-03 00:15:00 UTC)
        );
        assert_eq!(
            result.view.payment_window.monitor_until,
            datetime!(2026-05-04 00:15:00 UTC)
        );

        let state = repo.state.lock().unwrap();
        assert_eq!(state.allocate_calls, 1);
        assert_eq!(state.create_commands.len(), 1);
        let command = &state.create_commands[0];
        assert_eq!(command.external_id, "merchant-order-10001");
        assert_eq!(command.order_id, order_id);
        assert_eq!(
            command.payment_window.window_from,
            datetime!(2026-05-03 00:00:00 UTC)
        );
        assert_eq!(command.payment_window.window_from_block, block(123));
    }

    #[tokio::test]
    async fn existing_same_request_returns_without_head_or_wallet_allocation() {
        let input = CreateOrderInput::new("merchant-order-10001", RawAmount::from(42), 900)
            .with_metadata(json!({"note": "same"}));
        let request_hash = hash_for_test(input.clone());
        let existing = view_fixture(Uuid::from_u128(10), "merchant-order-10001", &request_hash);
        let repo = FakeOrderRepository::with_existing(existing.clone());
        let head_reader = FakeHeadReader::err("rpc down");
        let service = service(
            repo.clone(),
            head_reader.clone(),
            FixedClock(datetime!(2026-05-03 00:00:00 UTC)),
            SequenceIds::new([]),
        );

        let result = service.create_order(input).await.unwrap();

        assert_eq!(result.outcome, CreateOrderServiceOutcome::Existing);
        assert_eq!(result.view, existing);
        let state = repo.state.lock().unwrap();
        assert_eq!(state.allocate_calls, 0);
        assert!(state.create_commands.is_empty());
        assert_eq!(head_reader.calls(), 0);
    }

    #[tokio::test]
    async fn existing_different_request_returns_conflict_before_allocation() {
        let existing = view_fixture(Uuid::from_u128(10), "merchant-order-10001", "0xold");
        let repo = FakeOrderRepository::with_existing(existing);
        let service = service(
            repo.clone(),
            FakeHeadReader::ok(block(1)),
            FixedClock(datetime!(2026-05-03 00:00:00 UTC)),
            SequenceIds::new([]),
        );

        let error = service
            .create_order(CreateOrderInput::new(
                "merchant-order-10001",
                RawAmount::from(43),
                900,
            ))
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            OrderServiceError::Repository(RepositoryError::IdempotencyConflict {
                resource: "orders",
                ..
            })
        ));
        let state = repo.state.lock().unwrap();
        assert_eq!(state.allocate_calls, 0);
        assert!(state.create_commands.is_empty());
    }

    #[tokio::test]
    async fn chain_head_failure_stops_before_wallet_allocation() {
        let repo = FakeOrderRepository::new(vec![allocated(0, 0, 0)]);
        let service = service(
            repo.clone(),
            FakeHeadReader::err("rpc timeout"),
            FixedClock(datetime!(2026-05-03 00:00:00 UTC)),
            SequenceIds::new([]),
        );

        let error = service
            .create_order(CreateOrderInput::new(
                "merchant-order-10001",
                RawAmount::from(42),
                900,
            ))
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            OrderServiceError::ChainHeadUnavailable { .. }
        ));
        let state = repo.state.lock().unwrap();
        assert_eq!(state.allocate_calls, 0);
        assert!(state.create_commands.is_empty());
    }

    #[test]
    fn request_hash_is_canonical_and_excludes_generated_values() {
        let left = CreateOrderInput::new(" merchant-order-10001 ", RawAmount::from(42), 900)
            .with_metadata(json!({"b": 2, "a": {"d": 4, "c": 3}}));
        let right = CreateOrderInput::new("merchant-order-10001", RawAmount::from(42), 900)
            .with_metadata(json!({"a": {"c": 3, "d": 4}, "b": 2}));

        assert_eq!(hash_for_test(left), hash_for_test(right));
    }

    #[test]
    fn invalid_input_is_rejected() {
        for (input, field) in [
            (
                CreateOrderInput::new(" ", RawAmount::from(1), 900),
                "external_id",
            ),
            (
                CreateOrderInput::new("merchant-order-10001", RawAmount::ZERO, 900),
                "expected_amount_raw",
            ),
            (
                CreateOrderInput::new("merchant-order-10001", RawAmount::from(1), 0),
                "ttl_seconds",
            ),
            (
                CreateOrderInput::new("merchant-order-10001", RawAmount::from(1), 900)
                    .with_metadata(Value::String("bad".to_string())),
                "metadata",
            ),
        ] {
            let error = normalize_create_order_input(input).unwrap_err();
            assert!(matches!(
                error,
                OrderServiceError::InvalidArgument { field: actual, .. } if actual == field
            ));
        }
    }

    fn service(
        repo: FakeOrderRepository,
        head_reader: FakeHeadReader,
        clock: FixedClock,
        ids: SequenceIds,
    ) -> OrderService<
        FakeOrderRepository,
        DeterministicFakeDeriver,
        FakeHeadReader,
        FixedClock,
        SequenceIds,
    > {
        OrderService::with_dependencies(
            OrderServiceConfig::new(1, EvmAddress::from_bytes([0x11; 20]), 86_400),
            repo,
            HdWallet::new(
                DeterministicFakeDeriver::default()
                    .allow_key_ref("pay3-master")
                    .unwrap(),
            ),
            head_reader,
            clock,
            ids,
        )
        .unwrap()
    }

    fn hash_for_test(input: CreateOrderInput) -> String {
        let input = normalize_create_order_input(input).unwrap();
        canonical_order_request_hash(&input).unwrap()
    }

    fn allocated(account_index: u32, change_index: u32, address_index: u32) -> AllocatedDerivation {
        let segment = DerivationSegment::new(account_index, change_index, address_index).unwrap();
        AllocatedDerivation {
            signer_key_ref: "pay3-master".to_string(),
            derivation_version: 1,
            segment,
            derivation_path: segment.derivation_path(),
        }
    }

    fn block(number: u64) -> ChainBlockRef {
        ChainBlockRef::new(number, BlockHash::from_bytes([number as u8; 32]))
    }

    fn view_fixture(id: Uuid, external_id: &str, request_hash: &str) -> OrderView {
        let child_account_id = Uuid::from_u128(20);
        let receive_address = EvmAddress::from_bytes([0x77; 20]);
        let now = datetime!(2026-05-03 00:00:00 UTC);
        OrderView {
            order: OrderRecord {
                id,
                external_id: external_id.to_string(),
                request_hash: request_hash.to_string(),
                child_account_id,
                receive_address,
                chain_id: 1,
                token_address: EvmAddress::from_bytes([0x11; 20]),
                expected_amount_raw: RawAmount::from(42),
                paid_amount_raw: RawAmount::ZERO,
                status: OrderStatus::Pending,
                expires_at: now + Duration::seconds(900),
                monitor_until: now + Duration::seconds(900 + 86_400),
                created_at: now,
                updated_at: now,
            },
            child_account: crate::db::repositories::ChildAccountRecord {
                id: child_account_id,
                signer_key_ref: "pay3-master".to_string(),
                derivation_version: 1,
                derivation_segment: DerivationSegment::ZERO,
                derivation_path: DerivationSegment::ZERO.derivation_path(),
                address: receive_address,
                last_used_at: Some(now),
                created_at: now,
            },
            payment_window: crate::db::repositories::PaymentWindowRecord {
                id: Uuid::from_u128(21),
                order_id: id,
                child_account_id,
                receive_address,
                window_from: now,
                window_from_block: block(1),
                expires_at: now + Duration::seconds(900),
                monitor_until: now + Duration::seconds(900 + 86_400),
                created_at: now,
            },
        }
    }

    #[derive(Clone, Default)]
    struct FakeOrderRepository {
        state: std::sync::Arc<Mutex<FakeOrderRepositoryState>>,
    }

    impl FakeOrderRepository {
        fn new(allocations: Vec<AllocatedDerivation>) -> Self {
            Self {
                state: std::sync::Arc::new(Mutex::new(FakeOrderRepositoryState {
                    allocations: VecDeque::from(allocations),
                    ..FakeOrderRepositoryState::default()
                })),
            }
        }

        fn with_existing(view: OrderView) -> Self {
            let repo = Self::new(Vec::new());
            {
                let mut state = repo.state.lock().unwrap();
                state
                    .orders_by_external_id
                    .insert(view.order.external_id.clone(), view.order.clone());
                state.views.insert(view.order.id, view);
            }
            repo
        }
    }

    #[derive(Default)]
    struct FakeOrderRepositoryState {
        allocations: VecDeque<AllocatedDerivation>,
        allocate_calls: usize,
        create_commands: Vec<CreateOrderCommand>,
        orders_by_external_id: BTreeMap<String, OrderRecord>,
        views: BTreeMap<Uuid, OrderView>,
    }

    #[async_trait]
    impl OrderRepository for FakeOrderRepository {
        async fn allocate_derivation_segment(
            &self,
            _cursor_id: &str,
        ) -> Result<AllocatedDerivation, RepositoryError> {
            let mut state = self.state.lock().unwrap();
            state.allocate_calls += 1;
            state.allocations.pop_front().ok_or_else(|| {
                RepositoryError::not_found("wallet_cursors", DEFAULT_WALLET_CURSOR_ID)
            })
        }

        async fn create_order_idempotent(
            &self,
            command: CreateOrderCommand,
        ) -> Result<CreateOrderOutcome, RepositoryError> {
            let mut state = self.state.lock().unwrap();
            if let Some(existing) = state.orders_by_external_id.get(&command.external_id) {
                if existing.request_hash == command.request_hash {
                    return Ok(CreateOrderOutcome::Existing(existing.clone()));
                }
                return Err(RepositoryError::idempotency_conflict(
                    "orders",
                    &command.external_id,
                    Some(existing.id),
                ));
            }

            let order = OrderRecord {
                id: command.order_id,
                external_id: command.external_id.clone(),
                request_hash: command.request_hash.clone(),
                child_account_id: command.child_account.id,
                receive_address: command.child_account.address,
                chain_id: command.chain_id,
                token_address: command.token_address,
                expected_amount_raw: command.expected_amount_raw,
                paid_amount_raw: RawAmount::ZERO,
                status: OrderStatus::Pending,
                expires_at: command.payment_window.expires_at,
                monitor_until: command.payment_window.monitor_until,
                created_at: command.payment_window.window_from,
                updated_at: command.payment_window.window_from,
            };
            let view = OrderView {
                order: order.clone(),
                child_account: crate::db::repositories::ChildAccountRecord {
                    id: command.child_account.id,
                    signer_key_ref: command.child_account.signer_key_ref.clone(),
                    derivation_version: command.child_account.derivation_version,
                    derivation_segment: command.child_account.derivation_segment,
                    derivation_path: command.child_account.derivation_path.clone(),
                    address: command.child_account.address,
                    last_used_at: Some(command.payment_window.window_from),
                    created_at: command.payment_window.window_from,
                },
                payment_window: crate::db::repositories::PaymentWindowRecord {
                    id: command.payment_window.id,
                    order_id: command.payment_window.order_id,
                    child_account_id: command.payment_window.child_account_id,
                    receive_address: command.payment_window.receive_address,
                    window_from: command.payment_window.window_from,
                    window_from_block: command.payment_window.window_from_block,
                    expires_at: command.payment_window.expires_at,
                    monitor_until: command.payment_window.monitor_until,
                    created_at: command.payment_window.window_from,
                },
            };
            state.create_commands.push(command);
            state
                .orders_by_external_id
                .insert(order.external_id.clone(), order.clone());
            state.views.insert(order.id, view);
            Ok(CreateOrderOutcome::Created(order))
        }

        async fn get_order(&self, id: Uuid) -> Result<Option<OrderRecord>, RepositoryError> {
            let state = self.state.lock().unwrap();
            Ok(state.views.get(&id).map(|view| view.order.clone()))
        }

        async fn get_order_view(&self, id: Uuid) -> Result<Option<OrderView>, RepositoryError> {
            let state = self.state.lock().unwrap();
            Ok(state.views.get(&id).cloned())
        }

        async fn get_order_by_external_id(
            &self,
            external_id: &str,
        ) -> Result<Option<OrderRecord>, RepositoryError> {
            let state = self.state.lock().unwrap();
            Ok(state.orders_by_external_id.get(external_id).cloned())
        }
    }

    #[derive(Clone)]
    struct FakeHeadReader {
        state: std::sync::Arc<Mutex<FakeHeadReaderState>>,
    }

    impl FakeHeadReader {
        fn ok(head: ChainBlockRef) -> Self {
            Self {
                state: std::sync::Arc::new(Mutex::new(FakeHeadReaderState {
                    result: Ok(head),
                    calls: 0,
                })),
            }
        }

        fn err(message: impl Into<String>) -> Self {
            Self {
                state: std::sync::Arc::new(Mutex::new(FakeHeadReaderState {
                    result: Err(message.into()),
                    calls: 0,
                })),
            }
        }

        fn calls(&self) -> usize {
            self.state.lock().unwrap().calls
        }
    }

    struct FakeHeadReaderState {
        result: Result<ChainBlockRef, String>,
        calls: usize,
    }

    #[async_trait]
    impl OrderChainHeadReader for FakeHeadReader {
        async fn current_head(&self) -> Result<ChainBlockRef, OrderServiceError> {
            let mut state = self.state.lock().unwrap();
            state.calls += 1;
            state
                .result
                .clone()
                .map_err(OrderServiceError::chain_head_unavailable)
        }
    }

    #[derive(Clone, Copy)]
    struct FixedClock(OffsetDateTime);

    impl Clock for FixedClock {
        fn now(&self) -> OffsetDateTime {
            self.0
        }
    }

    #[derive(Default)]
    struct SequenceIds {
        ids: Mutex<VecDeque<Uuid>>,
    }

    impl SequenceIds {
        fn new<const N: usize>(ids: [Uuid; N]) -> Self {
            Self {
                ids: Mutex::new(VecDeque::from(ids)),
            }
        }
    }

    impl IdGenerator for SequenceIds {
        fn new_id(&self) -> Uuid {
            self.ids
                .lock()
                .unwrap()
                .pop_front()
                .expect("test id sequence exhausted")
        }
    }
}
