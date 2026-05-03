//! Collection creation and prefunded collection job preparation.

use std::num::{ParseIntError, TryFromIntError};

use alloy_primitives::keccak256;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    chain::{ChainError, Erc20ChainClient},
    db::repositories::{
        AuditEventInput, AuditRepository, CollectionJob, CollectionRecord, CollectionRepository,
        CreateCollectionCommand, NewSignedOutboundTx, OrderRepository, OrderView,
        OutboundRepository, OutboundTxPurpose, OutboundTxRecord, RepositoryError,
    },
    domain::{CollectionFees, EvmAddress, OrderStatus, RawAmount},
    services::orders::{IdGenerator, RandomIdGenerator},
    signer::{SignedTx, SignerError, SignerProvider, UnsignedTx},
};

const ERC20_TRANSFER_SELECTOR: [u8; 4] = [0xa9, 0x05, 0x9c, 0xbb];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CollectionServiceConfig {
    pub chain_id: u64,
    pub token_address: EvmAddress,
    pub treasury_address: EvmAddress,
    pub fees: CollectionFees,
}

impl CollectionServiceConfig {
    pub const fn new(
        chain_id: u64,
        token_address: EvmAddress,
        treasury_address: EvmAddress,
        fees: CollectionFees,
    ) -> Self {
        Self {
            chain_id,
            token_address,
            treasury_address,
            fees,
        }
    }

    fn validate(&self) -> Result<(), CollectionServiceError> {
        if self.chain_id == 0 {
            return Err(CollectionServiceError::invalid_argument(
                "chain_id",
                "must be greater than zero",
            ));
        }
        if self.token_address == EvmAddress::ZERO {
            return Err(CollectionServiceError::invalid_argument(
                "token_address",
                "must not be zero",
            ));
        }
        if self.treasury_address == EvmAddress::ZERO {
            return Err(CollectionServiceError::invalid_argument(
                "treasury_address",
                "must not be zero",
            ));
        }
        if self.fees.gas_limit == 0 {
            return Err(CollectionServiceError::invalid_argument(
                "gas_limit",
                "must be greater than zero",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CollectionAmount {
    Max,
    Exact(RawAmount),
}

impl CollectionAmount {
    fn validate(&self) -> Result<(), CollectionServiceError> {
        match self {
            Self::Max => Ok(()),
            Self::Exact(amount) if amount.is_zero() => Err(
                CollectionServiceError::invalid_argument("amount_raw", "must be greater than zero"),
            ),
            Self::Exact(_) => Ok(()),
        }
    }

    fn as_optional_raw(&self) -> Option<RawAmount> {
        match self {
            Self::Max => None,
            Self::Exact(amount) => Some(*amount),
        }
    }

    fn canonical_value(&self) -> String {
        match self {
            Self::Max => "max".to_string(),
            Self::Exact(amount) => amount.to_string(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateCollectionInput {
    pub order_id: Uuid,
    pub amount: CollectionAmount,
    pub idempotency_key: String,
    #[serde(default)]
    pub audit: AuditContext,
}

impl CreateCollectionInput {
    pub fn max(order_id: Uuid, idempotency_key: impl Into<String>) -> Self {
        Self {
            order_id,
            amount: CollectionAmount::Max,
            idempotency_key: idempotency_key.into(),
            audit: AuditContext::default(),
        }
    }

    pub fn with_audit(mut self, audit: AuditContext) -> Self {
        self.audit = audit;
        self
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditContext {
    pub request_id: Option<String>,
    pub principal_sub: Option<String>,
    #[serde(default)]
    pub scopes: Vec<String>,
}

impl AuditContext {
    fn scopes_string(&self) -> Option<String> {
        if self.scopes.is_empty() {
            None
        } else {
            Some(self.scopes.join(" "))
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CreateCollectionOutcome {
    Created,
    Existing,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateCollectionResult {
    pub outcome: CreateCollectionOutcome,
    pub collection: CollectionRecord,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PrepareCollectionJobOutcome {
    NoJob,
    Prepared {
        collection: CollectionRecord,
        outbound: OutboundTxRecord,
        signed_tx: SignedTx,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PrefundedGasCheck {
    pub chain_id: u64,
    pub from_address: EvmAddress,
    pub gas_limit: u64,
    pub max_fee_per_gas: RawAmount,
    pub max_priority_fee_per_gas: RawAmount,
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum GasFundingError {
    #[error("native gas balance unavailable for {chain_id}/{address}: {message}")]
    Unavailable {
        chain_id: u64,
        address: EvmAddress,
        message: String,
    },

    #[error("native gas balance is insufficient for {chain_id}/{address}")]
    Insufficient { chain_id: u64, address: EvmAddress },
}

#[async_trait]
pub trait PrefundedGasChecker: Send + Sync {
    async fn ensure_prefunded_gas(&self, check: PrefundedGasCheck) -> Result<(), GasFundingError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct AssumePrefundedGas;

#[async_trait]
impl PrefundedGasChecker for AssumePrefundedGas {
    async fn ensure_prefunded_gas(&self, _check: PrefundedGasCheck) -> Result<(), GasFundingError> {
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum CollectionServiceError {
    #[error("invalid argument {field}: {message}")]
    InvalidArgument {
        field: &'static str,
        message: String,
    },

    #[error("order not found: {order_id}")]
    OrderNotFound { order_id: Uuid },

    #[error("order {order_id} is not paid and cannot be collected")]
    OrderNotCollectable { order_id: Uuid, status: OrderStatus },

    #[error("order {order_id} is for a different chain/token")]
    OrderStreamMismatch { order_id: Uuid },

    #[error("collection amount resolved to zero for collection {collection_id}")]
    ZeroCollectionAmount { collection_id: Uuid },

    #[error("insufficient token balance for collection {collection_id}")]
    InsufficientTokenBalance {
        collection_id: Uuid,
        balance: RawAmount,
        required: RawAmount,
    },

    #[error("reserved nonce {nonce} does not fit u64")]
    NonceOutOfRange {
        nonce: RawAmount,
        #[source]
        source: ParseIntError,
    },

    #[error("integer value out of range for {field}")]
    IntegerOutOfRange {
        field: &'static str,
        #[source]
        source: TryFromIntError,
    },

    #[error("signed transaction invariant violation: {message}")]
    SignedTxInvariant { message: String },

    #[error(transparent)]
    Repository(#[from] RepositoryError),

    #[error(transparent)]
    Chain(#[from] ChainError),

    #[error(transparent)]
    Signer(#[from] SignerError),

    #[error(transparent)]
    GasFunding(#[from] GasFundingError),

    #[error("canonical request serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
}

impl CollectionServiceError {
    fn invalid_argument(field: &'static str, message: impl Into<String>) -> Self {
        Self::InvalidArgument {
            field,
            message: message.into(),
        }
    }
}

pub struct CollectionService<O, C, B, A, S, H, G, I = RandomIdGenerator> {
    config: CollectionServiceConfig,
    orders: O,
    collections: C,
    outbound: B,
    audit: A,
    signer: S,
    chain: H,
    gas_checker: G,
    ids: I,
}

impl<O, C, B, A, S, H, G> CollectionService<O, C, B, A, S, H, G, RandomIdGenerator> {
    pub fn new(
        config: CollectionServiceConfig,
        orders: O,
        collections: C,
        outbound: B,
        audit: A,
        signer: S,
        chain: H,
        gas_checker: G,
    ) -> Result<Self, CollectionServiceError> {
        Self::with_id_generator(
            config,
            orders,
            collections,
            outbound,
            audit,
            signer,
            chain,
            gas_checker,
            RandomIdGenerator,
        )
    }
}

impl<O, C, B, A, S, H, G, I> CollectionService<O, C, B, A, S, H, G, I> {
    #[allow(clippy::too_many_arguments)]
    pub fn with_id_generator(
        config: CollectionServiceConfig,
        orders: O,
        collections: C,
        outbound: B,
        audit: A,
        signer: S,
        chain: H,
        gas_checker: G,
        ids: I,
    ) -> Result<Self, CollectionServiceError> {
        config.validate()?;
        Ok(Self {
            config,
            orders,
            collections,
            outbound,
            audit,
            signer,
            chain,
            gas_checker,
            ids,
        })
    }
}

impl<O, C, B, A, S, H, G, I> CollectionService<O, C, B, A, S, H, G, I>
where
    O: OrderRepository,
    C: CollectionRepository,
    B: OutboundRepository,
    A: AuditRepository,
    S: SignerProvider,
    H: Erc20ChainClient,
    G: PrefundedGasChecker,
    I: IdGenerator,
{
    pub async fn create_collection(
        &self,
        input: CreateCollectionInput,
    ) -> Result<CreateCollectionResult, CollectionServiceError> {
        let input = normalize_create_collection_input(input)?;
        let view = self.orders.get_order_view(input.order_id).await?.ok_or(
            CollectionServiceError::OrderNotFound {
                order_id: input.order_id,
            },
        )?;
        validate_collectable_order(&view, &self.config)?;

        let collection_id = self.ids.new_id();
        let request_hash = canonical_collection_request_hash(&input, &self.config)?;
        let command = CreateCollectionCommand {
            collection_id,
            order_id: view.order.id,
            idempotency_key: input.idempotency_key.clone(),
            request_hash,
            child_account_id: view.order.child_account_id,
            chain_id: self.config.chain_id,
            token_address: self.config.token_address,
            from_address: view.order.receive_address,
            to_address: self.config.treasury_address,
            amount_raw: input.amount.as_optional_raw(),
        };

        let collection = self
            .collections
            .create_collection_idempotent(command)
            .await?;
        let outcome = if collection.id == collection_id {
            CreateCollectionOutcome::Created
        } else {
            CreateCollectionOutcome::Existing
        };
        self.append_collection_audit(
            "collection.create",
            &input.audit,
            Some(collection.order_id),
            Some(collection.id),
            None,
            json!({
                "outcome": outcome,
                "from_address": collection.from_address,
                "to_address": collection.to_address,
                "amount_raw": collection.amount_raw.map(|amount| amount.to_string()),
            }),
        )
        .await?;

        Ok(CreateCollectionResult {
            outcome,
            collection,
        })
    }

    pub async fn prepare_next_collection_job(
        &self,
        worker_id: &str,
    ) -> Result<PrepareCollectionJobOutcome, CollectionServiceError> {
        let worker_id = worker_id.trim();
        if worker_id.is_empty() {
            return Err(CollectionServiceError::invalid_argument(
                "worker_id",
                "must not be empty",
            ));
        }

        let Some(job) = self.collections.claim_collection_job(worker_id).await? else {
            return Ok(PrepareCollectionJobOutcome::NoJob);
        };

        let amount = self.resolve_collection_amount(&job).await?;
        self.gas_checker
            .ensure_prefunded_gas(PrefundedGasCheck {
                chain_id: job.collection.chain_id,
                from_address: job.collection.from_address,
                gas_limit: self.config.fees.gas_limit,
                max_fee_per_gas: self.config.fees.max_fee_per_gas,
                max_priority_fee_per_gas: self.config.fees.max_priority_fee_per_gas,
            })
            .await?;
        self.signer.health_check().await?;

        let reserved_nonce = self
            .outbound
            .reserve_nonce(job.collection.chain_id, job.collection.from_address)
            .await?;
        let nonce = raw_amount_to_u64(reserved_nonce.nonce)?;
        let unsigned = UnsignedTx::new(
            format!("collection-{}-nonce-{nonce}", job.collection.id),
            job.collection.chain_id,
            nonce,
            job.collection.token_address,
            RawAmount::ZERO,
            self.config.fees.gas_limit,
            self.config.fees.max_fee_per_gas,
            self.config.fees.max_priority_fee_per_gas,
            erc20_transfer_data(job.collection.to_address, amount),
        )?;
        let signed = self
            .signer
            .sign_transaction(&job.signer_key_ref, &job.derivation_path, unsigned)
            .await?;
        ensure_signed_tx_matches_job(&job, &signed, nonce)?;

        let outbound = self
            .outbound
            .insert_signed_tx(NewSignedOutboundTx {
                id: self.ids.new_id(),
                chain_id: job.collection.chain_id,
                purpose: OutboundTxPurpose::Collect,
                from_address: job.collection.from_address,
                to_address: job.collection.to_address,
                nonce: reserved_nonce.nonce,
                tx_hash: signed.tx_hash,
                signed_tx: signed.raw_tx.clone(),
                replacement_of: None,
                replacement_reason: None,
            })
            .await?;
        let collection = self
            .collections
            .attach_outbound_tx(job.collection.id, outbound.id)
            .await?;
        self.append_collection_audit(
            "collection.signed",
            &AuditContext::default(),
            Some(collection.order_id),
            Some(collection.id),
            Some(outbound.tx_hash),
            json!({
                "worker_id": worker_id,
                "outbound_tx_id": outbound.id,
                "from_address": outbound.from_address,
                "to_address": outbound.to_address,
                "amount_raw": amount.to_string(),
                "nonce": outbound.nonce.to_string(),
                "tx_hash": outbound.tx_hash,
            }),
        )
        .await?;

        Ok(PrepareCollectionJobOutcome::Prepared {
            collection,
            outbound,
            signed_tx: signed,
        })
    }

    async fn resolve_collection_amount(
        &self,
        job: &CollectionJob,
    ) -> Result<RawAmount, CollectionServiceError> {
        let balance = self
            .chain
            .token_balance(job.collection.token_address, job.collection.from_address)
            .await?;
        let amount = match job.collection.amount_raw {
            Some(amount) if amount > balance => {
                return Err(CollectionServiceError::InsufficientTokenBalance {
                    collection_id: job.collection.id,
                    balance,
                    required: amount,
                });
            }
            Some(amount) => amount,
            None => balance,
        };
        if amount.is_zero() {
            return Err(CollectionServiceError::ZeroCollectionAmount {
                collection_id: job.collection.id,
            });
        }
        Ok(amount)
    }

    async fn append_collection_audit(
        &self,
        event_type: &str,
        context: &AuditContext,
        order_id: Option<Uuid>,
        collection_id: Option<Uuid>,
        tx_hash: Option<crate::domain::TxHash>,
        payload: Value,
    ) -> Result<(), CollectionServiceError> {
        self.audit
            .append_audit_event(AuditEventInput {
                id: self.ids.new_id(),
                event_type: event_type.to_string(),
                request_id: context.request_id.clone(),
                principal_sub: context.principal_sub.clone(),
                scopes: context.scopes_string(),
                order_id,
                collection_id,
                tx_hash,
                payload,
            })
            .await?;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct NormalizedCreateCollectionInput {
    order_id: Uuid,
    amount: CollectionAmount,
    idempotency_key: String,
    audit: AuditContext,
}

fn normalize_create_collection_input(
    input: CreateCollectionInput,
) -> Result<NormalizedCreateCollectionInput, CollectionServiceError> {
    input.amount.validate()?;
    let idempotency_key = input.idempotency_key.trim().to_string();
    if idempotency_key.is_empty() {
        return Err(CollectionServiceError::invalid_argument(
            "idempotency_key",
            "must not be empty",
        ));
    }
    Ok(NormalizedCreateCollectionInput {
        order_id: input.order_id,
        amount: input.amount,
        idempotency_key,
        audit: input.audit,
    })
}

fn validate_collectable_order(
    view: &OrderView,
    config: &CollectionServiceConfig,
) -> Result<(), CollectionServiceError> {
    if view.order.status != OrderStatus::Paid {
        return Err(CollectionServiceError::OrderNotCollectable {
            order_id: view.order.id,
            status: view.order.status,
        });
    }
    if view.order.chain_id != config.chain_id || view.order.token_address != config.token_address {
        return Err(CollectionServiceError::OrderStreamMismatch {
            order_id: view.order.id,
        });
    }
    Ok(())
}

fn canonical_collection_request_hash(
    input: &NormalizedCreateCollectionInput,
    config: &CollectionServiceConfig,
) -> Result<String, CollectionServiceError> {
    #[derive(Serialize)]
    struct CanonicalRequest<'a> {
        idempotency_key: &'a str,
        order_id: Uuid,
        amount: String,
        chain_id: u64,
        token_address: EvmAddress,
        treasury_address: EvmAddress,
    }

    let canonical = CanonicalRequest {
        idempotency_key: &input.idempotency_key,
        order_id: input.order_id,
        amount: input.amount.canonical_value(),
        chain_id: config.chain_id,
        token_address: config.token_address,
        treasury_address: config.treasury_address,
    };
    let bytes = serde_json::to_vec(&canonical)?;
    Ok(keccak256(bytes).to_string())
}

fn raw_amount_to_u64(nonce: RawAmount) -> Result<u64, CollectionServiceError> {
    nonce
        .to_string()
        .parse::<u64>()
        .map_err(|source| CollectionServiceError::NonceOutOfRange { nonce, source })
}

pub fn erc20_transfer_data(to: EvmAddress, amount: RawAmount) -> Vec<u8> {
    let mut data = Vec::with_capacity(68);
    data.extend_from_slice(&ERC20_TRANSFER_SELECTOR);
    data.extend_from_slice(&[0u8; 12]);
    data.extend_from_slice(to.as_bytes());
    data.extend_from_slice(&amount.value().to_be_bytes::<32>());
    data
}

fn ensure_signed_tx_matches_job(
    job: &CollectionJob,
    signed: &SignedTx,
    nonce: u64,
) -> Result<(), CollectionServiceError> {
    if signed.chain_id != job.collection.chain_id {
        return Err(signed_invariant(format!(
            "signed chain id {} did not match collection chain id {}",
            signed.chain_id, job.collection.chain_id
        )));
    }
    if signed.nonce != nonce {
        return Err(signed_invariant(format!(
            "signed nonce {} did not match reserved nonce {nonce}",
            signed.nonce
        )));
    }
    if signed.from != job.collection.from_address {
        return Err(signed_invariant(format!(
            "signed from {} did not match collection from {}",
            signed.from, job.collection.from_address
        )));
    }
    if signed.to != job.collection.token_address {
        return Err(signed_invariant(format!(
            "signed to {} did not match token contract {}",
            signed.to, job.collection.token_address
        )));
    }
    Ok(())
}

fn signed_invariant(message: impl Into<String>) -> CollectionServiceError {
    CollectionServiceError::SignedTxInvariant {
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        sync::{Arc, Mutex},
    };

    use async_trait::async_trait;
    use time::{OffsetDateTime, macros::datetime};

    use super::*;
    use crate::{
        chain::{
            ChainBlock, ChainHeaderReader, TransactionStatus, TransferLog,
            TransferLogCapacityLimits, TransferLogCapacityReport, TransferLogRange,
            TransferLogSource, TxReceipt,
        },
        db::repositories::{
            AllocatedDerivation, AuditEventRecord, ChildAccountRecord, CreateOrderCommand,
            CreateOrderOutcome, OrderRecord, OutboundTxStatus, PaymentWindowRecord, ReservedNonce,
        },
        domain::{BlockHash, ChainBlockRef, DerivationSegment, TxHash},
    };

    #[tokio::test]
    async fn create_collection_uses_treasury_and_canonical_request_hash() {
        let service = service(Fixture::default());
        let result = service
            .create_collection(CreateCollectionInput::max(order_id(), " collect-1 "))
            .await
            .unwrap();

        assert_eq!(result.outcome, CreateCollectionOutcome::Created);
        assert_eq!(result.collection.to_address, treasury());
        let commands = service.collections.commands();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].idempotency_key, "collect-1");
        assert_eq!(commands[0].order_id, order_id());
        assert_eq!(commands[0].from_address, child_address());
        assert_eq!(commands[0].to_address, treasury());
        assert_eq!(commands[0].amount_raw, None);
        assert_eq!(
            service.audit.event_types(),
            vec!["collection.create".to_string()]
        );
    }

    #[tokio::test]
    async fn create_collection_rejects_unpaid_order_before_repository_insert() {
        let service = service(Fixture {
            order_status: OrderStatus::Confirming,
            ..Fixture::default()
        });

        let error = service
            .create_collection(CreateCollectionInput::max(order_id(), "collect-1"))
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            CollectionServiceError::OrderNotCollectable { .. }
        ));
        assert!(service.collections.commands().is_empty());
    }

    #[tokio::test]
    async fn prepare_next_collection_job_checks_balance_signs_and_persists_outbound() {
        let service = service(Fixture::default());

        let outcome = service
            .prepare_next_collection_job("collector-1")
            .await
            .unwrap();
        let PrepareCollectionJobOutcome::Prepared {
            collection,
            outbound,
            signed_tx,
        } = outcome
        else {
            panic!("expected prepared collection job");
        };

        assert_eq!(collection.outbound_tx_id, Some(outbound.id));
        assert_eq!(outbound.purpose, OutboundTxPurpose::Collect);
        assert_eq!(outbound.from_address, child_address());
        assert_eq!(outbound.to_address, treasury());
        assert_eq!(outbound.nonce, RawAmount::from(7));
        assert_eq!(outbound.tx_hash, signed_tx.tx_hash);
        let signed_requests = service.signer.signed_requests();
        assert_eq!(signed_requests.len(), 1);
        assert_eq!(signed_requests[0].to, token());
        assert_eq!(signed_requests[0].value, RawAmount::ZERO);
        assert_eq!(signed_requests[0].nonce, 7);
        assert_eq!(
            signed_requests[0].data,
            erc20_transfer_data(treasury(), RawAmount::from(1_000))
        );
        assert_eq!(service.outbound.inserted().len(), 1);
        assert_eq!(
            service.collections.attached(),
            vec![(collection_id(), outbound.id)]
        );
        assert_eq!(
            service.audit.event_types(),
            vec!["collection.signed".to_string()]
        );
    }

    #[tokio::test]
    async fn prepare_next_collection_job_fails_gas_gate_before_nonce_or_signing() {
        let service = service(Fixture {
            gas_error: Some(GasFundingError::Insufficient {
                chain_id: 1,
                address: child_address(),
            }),
            ..Fixture::default()
        });

        let error = service
            .prepare_next_collection_job("collector-1")
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            CollectionServiceError::GasFunding(GasFundingError::Insufficient { .. })
        ));
        assert!(service.outbound.reserved().is_empty());
        assert!(service.signer.signed_requests().is_empty());
        assert!(service.collections.attached().is_empty());
    }

    #[tokio::test]
    async fn prepare_next_collection_job_rejects_exact_amount_above_balance() {
        let service = service(Fixture {
            job_amount: Some(RawAmount::from(2_000)),
            ..Fixture::default()
        });

        let error = service
            .prepare_next_collection_job("collector-1")
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            CollectionServiceError::InsufficientTokenBalance {
                balance,
                required,
                ..
            } if balance == RawAmount::from(1_000) && required == RawAmount::from(2_000)
        ));
        assert!(service.outbound.reserved().is_empty());
    }

    #[test]
    fn erc20_transfer_data_uses_standard_selector_and_abi_words() {
        let data = erc20_transfer_data(treasury(), RawAmount::from(100));

        assert_eq!(&data[..4], &[0xa9, 0x05, 0x9c, 0xbb]);
        assert_eq!(&data[4..16], &[0u8; 12]);
        assert_eq!(&data[16..36], treasury().as_bytes());
        assert_eq!(
            &data[36..68],
            &RawAmount::from(100).value().to_be_bytes::<32>()
        );
    }

    type TestService = CollectionService<
        FakeOrderRepository,
        FakeCollectionRepository,
        FakeOutboundRepository,
        FakeAuditRepository,
        FakeSigner,
        FakeChain,
        FakeGasChecker,
        FixedIds,
    >;

    fn service(fixture: Fixture) -> TestService {
        CollectionService::with_id_generator(
            config(),
            FakeOrderRepository::new(order_view(fixture.order_status)),
            FakeCollectionRepository::new(collection_job(fixture.job_amount)),
            FakeOutboundRepository::default(),
            FakeAuditRepository::default(),
            FakeSigner::default(),
            FakeChain::new(fixture.token_balance),
            FakeGasChecker::new(fixture.gas_error),
            FixedIds::new(vec![
                Uuid::from_u128(100),
                Uuid::from_u128(101),
                Uuid::from_u128(102),
                Uuid::from_u128(103),
            ]),
        )
        .unwrap()
    }

    #[derive(Clone, Debug)]
    struct Fixture {
        order_status: OrderStatus,
        token_balance: RawAmount,
        job_amount: Option<RawAmount>,
        gas_error: Option<GasFundingError>,
    }

    impl Default for Fixture {
        fn default() -> Self {
            Self {
                order_status: OrderStatus::Paid,
                token_balance: RawAmount::from(1_000),
                job_amount: None,
                gas_error: None,
            }
        }
    }

    #[derive(Clone, Debug)]
    struct FixedIds {
        ids: Arc<Mutex<VecDeque<Uuid>>>,
    }

    impl FixedIds {
        fn new(ids: Vec<Uuid>) -> Self {
            Self {
                ids: Arc::new(Mutex::new(VecDeque::from(ids))),
            }
        }
    }

    impl IdGenerator for FixedIds {
        fn new_id(&self) -> Uuid {
            self.ids
                .lock()
                .expect("fixed ids mutex poisoned")
                .pop_front()
                .expect("fixed id queue exhausted")
        }
    }

    #[derive(Clone, Debug)]
    struct FakeOrderRepository {
        view: OrderView,
    }

    impl FakeOrderRepository {
        fn new(view: OrderView) -> Self {
            Self { view }
        }
    }

    #[async_trait]
    impl OrderRepository for FakeOrderRepository {
        async fn allocate_derivation_segment(
            &self,
            _cursor_id: &str,
        ) -> Result<AllocatedDerivation, RepositoryError> {
            unimplemented!("collection service does not allocate wallet segments")
        }

        async fn create_order_idempotent(
            &self,
            _command: CreateOrderCommand,
        ) -> Result<CreateOrderOutcome, RepositoryError> {
            unimplemented!("collection service does not create orders")
        }

        async fn get_order(&self, _id: Uuid) -> Result<Option<OrderRecord>, RepositoryError> {
            Ok(Some(self.view.order.clone()))
        }

        async fn get_order_view(&self, id: Uuid) -> Result<Option<OrderView>, RepositoryError> {
            Ok((id == self.view.order.id).then_some(self.view.clone()))
        }

        async fn get_order_by_external_id(
            &self,
            _external_id: &str,
        ) -> Result<Option<OrderRecord>, RepositoryError> {
            Ok(None)
        }
    }

    #[derive(Clone, Debug)]
    struct FakeCollectionRepository {
        state: Arc<Mutex<FakeCollectionState>>,
    }

    #[derive(Clone, Debug)]
    struct FakeCollectionState {
        job: Option<CollectionJob>,
        commands: Vec<CreateCollectionCommand>,
        attached: Vec<(Uuid, Uuid)>,
    }

    impl FakeCollectionRepository {
        fn new(job: CollectionJob) -> Self {
            Self {
                state: Arc::new(Mutex::new(FakeCollectionState {
                    job: Some(job),
                    commands: Vec::new(),
                    attached: Vec::new(),
                })),
            }
        }

        fn commands(&self) -> Vec<CreateCollectionCommand> {
            self.state
                .lock()
                .expect("fake collection repo mutex poisoned")
                .commands
                .clone()
        }

        fn attached(&self) -> Vec<(Uuid, Uuid)> {
            self.state
                .lock()
                .expect("fake collection repo mutex poisoned")
                .attached
                .clone()
        }
    }

    #[async_trait]
    impl CollectionRepository for FakeCollectionRepository {
        async fn create_collection_idempotent(
            &self,
            command: CreateCollectionCommand,
        ) -> Result<CollectionRecord, RepositoryError> {
            self.state
                .lock()
                .expect("fake collection repo mutex poisoned")
                .commands
                .push(command.clone());
            Ok(collection_record(
                command.collection_id,
                command.amount_raw,
                None,
            ))
        }

        async fn claim_collection_job(
            &self,
            _worker_id: &str,
        ) -> Result<Option<CollectionJob>, RepositoryError> {
            Ok(self
                .state
                .lock()
                .expect("fake collection repo mutex poisoned")
                .job
                .take())
        }

        async fn attach_outbound_tx(
            &self,
            collection_id: Uuid,
            outbound_tx_id: Uuid,
        ) -> Result<CollectionRecord, RepositoryError> {
            self.state
                .lock()
                .expect("fake collection repo mutex poisoned")
                .attached
                .push((collection_id, outbound_tx_id));
            Ok(collection_record(collection_id, None, Some(outbound_tx_id)))
        }
    }

    #[derive(Clone, Debug, Default)]
    struct FakeOutboundRepository {
        state: Arc<Mutex<FakeOutboundState>>,
    }

    #[derive(Clone, Debug, Default)]
    struct FakeOutboundState {
        reserved: Vec<(u64, EvmAddress)>,
        inserted: Vec<NewSignedOutboundTx>,
    }

    impl FakeOutboundRepository {
        fn reserved(&self) -> Vec<(u64, EvmAddress)> {
            self.state
                .lock()
                .expect("fake outbound repo mutex poisoned")
                .reserved
                .clone()
        }

        fn inserted(&self) -> Vec<NewSignedOutboundTx> {
            self.state
                .lock()
                .expect("fake outbound repo mutex poisoned")
                .inserted
                .clone()
        }
    }

    #[async_trait]
    impl OutboundRepository for FakeOutboundRepository {
        async fn reserve_nonce(
            &self,
            chain_id: u64,
            from_address: EvmAddress,
        ) -> Result<ReservedNonce, RepositoryError> {
            self.state
                .lock()
                .expect("fake outbound repo mutex poisoned")
                .reserved
                .push((chain_id, from_address));
            Ok(ReservedNonce {
                chain_id,
                address: from_address,
                nonce: RawAmount::from(7),
            })
        }

        async fn insert_signed_tx(
            &self,
            tx: NewSignedOutboundTx,
        ) -> Result<OutboundTxRecord, RepositoryError> {
            self.state
                .lock()
                .expect("fake outbound repo mutex poisoned")
                .inserted
                .push(tx.clone());
            Ok(outbound_record(tx))
        }

        async fn replace_signed_tx(
            &self,
            _old_tx_id: Uuid,
            _replacement_tx: NewSignedOutboundTx,
        ) -> Result<OutboundTxRecord, RepositoryError> {
            unimplemented!("collection service does not replace txs yet")
        }

        async fn claim_signed_collect_tx_for_broadcast(
            &self,
            _worker_id: &str,
        ) -> Result<Option<crate::db::repositories::BroadcastableOutboundTx>, RepositoryError>
        {
            unimplemented!("collection service does not claim signed txs for broadcast")
        }

        async fn claim_broadcast_collect_tx_for_receipt(
            &self,
            _worker_id: &str,
        ) -> Result<Option<crate::db::repositories::ReceiptCheckableOutboundTx>, RepositoryError>
        {
            unimplemented!("collection service does not claim broadcast txs for receipt")
        }

        async fn mark_broadcast(&self, _tx_id: Uuid) -> Result<OutboundTxRecord, RepositoryError> {
            unimplemented!("collector worker owns broadcast state")
        }

        async fn mark_confirmed(
            &self,
            _tx_id: Uuid,
            _receipt_block: ChainBlockRef,
        ) -> Result<OutboundTxRecord, RepositoryError> {
            unimplemented!("collector worker owns confirmation state")
        }

        async fn mark_failed(
            &self,
            _tx_id: Uuid,
            _error: &str,
        ) -> Result<OutboundTxRecord, RepositoryError> {
            unimplemented!("collector worker owns failure state")
        }
    }

    #[derive(Clone, Debug, Default)]
    struct FakeAuditRepository {
        events: Arc<Mutex<Vec<AuditEventInput>>>,
    }

    impl FakeAuditRepository {
        fn event_types(&self) -> Vec<String> {
            self.events
                .lock()
                .expect("fake audit repo mutex poisoned")
                .iter()
                .map(|event| event.event_type.clone())
                .collect()
        }
    }

    #[async_trait]
    impl AuditRepository for FakeAuditRepository {
        async fn append_audit_event(
            &self,
            event: AuditEventInput,
        ) -> Result<AuditEventRecord, RepositoryError> {
            self.events
                .lock()
                .expect("fake audit repo mutex poisoned")
                .push(event.clone());
            Ok(AuditEventRecord {
                id: event.id,
                event_type: event.event_type,
                request_id: event.request_id,
                principal_sub: event.principal_sub,
                scopes: event.scopes,
                order_id: event.order_id,
                collection_id: event.collection_id,
                tx_hash: event.tx_hash,
                payload: event.payload,
                created_at: now(),
            })
        }
    }

    #[derive(Clone, Debug, Default)]
    struct FakeSigner {
        requests: Arc<Mutex<Vec<UnsignedTx>>>,
    }

    impl FakeSigner {
        fn signed_requests(&self) -> Vec<UnsignedTx> {
            self.requests
                .lock()
                .expect("fake signer mutex poisoned")
                .clone()
        }
    }

    #[async_trait]
    impl SignerProvider for FakeSigner {
        async fn derive_address(
            &self,
            _key_ref: &str,
            _path: &str,
        ) -> Result<EvmAddress, SignerError> {
            Ok(child_address())
        }

        async fn sign_transaction(
            &self,
            _key_ref: &str,
            _path: &str,
            tx: UnsignedTx,
        ) -> Result<SignedTx, SignerError> {
            self.requests
                .lock()
                .expect("fake signer mutex poisoned")
                .push(tx.clone());
            let raw_tx = format!("signed:{}", tx.request_id).into_bytes();
            Ok(SignedTx {
                request_id: tx.request_id,
                chain_id: tx.chain_id,
                nonce: tx.nonce,
                from: child_address(),
                to: tx.to,
                tx_hash: tx_hash(0xaa),
                raw_tx,
            })
        }

        async fn health_check(&self) -> Result<(), SignerError> {
            Ok(())
        }
    }

    #[derive(Clone, Debug)]
    struct FakeChain {
        token_balance: RawAmount,
    }

    impl FakeChain {
        fn new(token_balance: RawAmount) -> Self {
            Self { token_balance }
        }
    }

    #[async_trait]
    impl ChainHeaderReader for FakeChain {
        async fn latest_head(&self) -> Result<ChainBlockRef, ChainError> {
            Ok(ChainBlockRef::new(1, block_hash(1)))
        }

        async fn safe_head(&self) -> Result<ChainBlockRef, ChainError> {
            Ok(ChainBlockRef::new(1, block_hash(1)))
        }

        async fn finalized_head(&self) -> Result<ChainBlockRef, ChainError> {
            Ok(ChainBlockRef::new(1, block_hash(1)))
        }

        async fn block_by_number(&self, number: u64) -> Result<ChainBlock, ChainError> {
            Ok(ChainBlock::new(
                number,
                block_hash(number as u8),
                block_hash(number.saturating_sub(1) as u8),
                now(),
            ))
        }
    }

    #[async_trait]
    impl TransferLogSource for FakeChain {
        async fn transfer_logs(
            &self,
            _range: TransferLogRange,
        ) -> Result<Vec<TransferLog>, ChainError> {
            Ok(Vec::new())
        }

        async fn capacity_probe(
            &self,
            range: TransferLogRange,
            limits: TransferLogCapacityLimits,
        ) -> Result<TransferLogCapacityReport, ChainError> {
            Ok(TransferLogCapacityReport {
                range,
                log_count: 0,
                max_logs_in_single_block: 0,
                limits,
            })
        }
    }

    #[async_trait]
    impl Erc20ChainClient for FakeChain {
        async fn token_balance(
            &self,
            _token: EvmAddress,
            _owner: EvmAddress,
        ) -> Result<RawAmount, ChainError> {
            Ok(self.token_balance)
        }

        async fn transaction_receipt(&self, _tx: TxHash) -> Result<Option<TxReceipt>, ChainError> {
            Ok(Some(TxReceipt {
                tx_hash: tx_hash(0xaa),
                block: ChainBlockRef::new(1, block_hash(1)),
                status: TransactionStatus::Success,
                gas_used: Some(65_000),
            }))
        }

        async fn broadcast_signed_tx(&self, _signed_tx: Vec<u8>) -> Result<TxHash, ChainError> {
            Ok(tx_hash(0xaa))
        }
    }

    #[derive(Clone, Debug)]
    struct FakeGasChecker {
        error: Option<GasFundingError>,
    }

    impl FakeGasChecker {
        fn new(error: Option<GasFundingError>) -> Self {
            Self { error }
        }
    }

    #[async_trait]
    impl PrefundedGasChecker for FakeGasChecker {
        async fn ensure_prefunded_gas(
            &self,
            _check: PrefundedGasCheck,
        ) -> Result<(), GasFundingError> {
            match &self.error {
                Some(error) => Err(error.clone()),
                None => Ok(()),
            }
        }
    }

    fn config() -> CollectionServiceConfig {
        CollectionServiceConfig::new(
            1,
            token(),
            treasury(),
            CollectionFees::new(
                65_000,
                RawAmount::from(30_000_000_000),
                RawAmount::from(1_500_000_000),
            ),
        )
    }

    fn order_view(status: OrderStatus) -> OrderView {
        OrderView {
            order: OrderRecord {
                id: order_id(),
                external_id: "merchant-order-1".to_string(),
                request_hash: "request-hash".to_string(),
                child_account_id: child_account_id(),
                receive_address: child_address(),
                chain_id: 1,
                token_address: token(),
                expected_amount_raw: RawAmount::from(1_000),
                paid_amount_raw: RawAmount::from(1_000),
                status,
                expires_at: now(),
                monitor_until: now(),
                created_at: now(),
                updated_at: now(),
            },
            child_account: ChildAccountRecord {
                id: child_account_id(),
                signer_key_ref: "test-key".to_string(),
                derivation_version: 1,
                derivation_segment: DerivationSegment::new(0, 0, 7).unwrap(),
                derivation_path: "m/44'/60'/0'/0/7".to_string(),
                address: child_address(),
                last_used_at: Some(now()),
                created_at: now(),
            },
            payment_window: PaymentWindowRecord {
                id: Uuid::from_u128(901),
                order_id: order_id(),
                child_account_id: child_account_id(),
                receive_address: child_address(),
                window_from: now(),
                window_from_block: ChainBlockRef::new(1, block_hash(1)),
                expires_at: now(),
                monitor_until: now(),
                created_at: now(),
            },
        }
    }

    fn collection_job(amount_raw: Option<RawAmount>) -> CollectionJob {
        CollectionJob {
            collection: collection_record(collection_id(), amount_raw, None),
            signer_key_ref: "test-key".to_string(),
            derivation_version: 1,
            derivation_segment: DerivationSegment::new(0, 0, 7).unwrap(),
            derivation_path: "m/44'/60'/0'/0/7".to_string(),
        }
    }

    fn collection_record(
        id: Uuid,
        amount_raw: Option<RawAmount>,
        outbound_tx_id: Option<Uuid>,
    ) -> CollectionRecord {
        CollectionRecord {
            id,
            order_id: order_id(),
            idempotency_key: "collect-1".to_string(),
            request_hash: "collection-request-hash".to_string(),
            child_account_id: child_account_id(),
            chain_id: 1,
            token_address: token(),
            from_address: child_address(),
            to_address: treasury(),
            amount_raw,
            status: if outbound_tx_id.is_some() {
                crate::db::repositories::CollectionRecordStatus::Transferring
            } else {
                crate::db::repositories::CollectionRecordStatus::Queued
            },
            outbound_tx_id,
            attempt_count: 0,
            locked_by: None,
            locked_until: None,
            error: None,
            created_at: now(),
            updated_at: now(),
        }
    }

    fn outbound_record(tx: NewSignedOutboundTx) -> OutboundTxRecord {
        OutboundTxRecord {
            id: tx.id,
            chain_id: tx.chain_id,
            purpose: tx.purpose,
            from_address: tx.from_address,
            to_address: tx.to_address,
            nonce: tx.nonce,
            tx_hash: tx.tx_hash,
            signed_tx: tx.signed_tx,
            status: OutboundTxStatus::Signed,
            replacement_of: tx.replacement_of,
            replacement_reason: tx.replacement_reason,
            broadcast_count: 0,
            last_broadcast_at: None,
            receipt_block: None,
            error: None,
            created_at: now(),
            updated_at: now(),
        }
    }

    fn now() -> OffsetDateTime {
        datetime!(2026-05-03 12:00 UTC)
    }

    fn order_id() -> Uuid {
        Uuid::from_u128(1)
    }

    fn collection_id() -> Uuid {
        Uuid::from_u128(2)
    }

    fn child_account_id() -> Uuid {
        Uuid::from_u128(3)
    }

    fn token() -> EvmAddress {
        address(0x11)
    }

    fn treasury() -> EvmAddress {
        address(0x22)
    }

    fn child_address() -> EvmAddress {
        address(0x33)
    }

    fn address(byte: u8) -> EvmAddress {
        EvmAddress::from_bytes([byte; 20])
    }

    fn block_hash(byte: u8) -> BlockHash {
        BlockHash::from_bytes([byte; 32])
    }

    fn tx_hash(byte: u8) -> TxHash {
        TxHash::from_bytes([byte; 32])
    }
}
