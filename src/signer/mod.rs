//! Signer boundary, deterministic fake signer, and remote HTTP adapter.

use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
};

use alloy_primitives::keccak256;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::domain::{EvmAddress, MAX_DERIVATION_INDEX, RawAmount, TxHash};

mod external;
mod local;

pub use external::RemoteHttpSigner;
pub use local::LocalMnemonicSigner;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SignerError {
    #[error("signer key ref must not be empty")]
    EmptySignerKeyRef,

    #[error("derivation path is invalid: {path}")]
    InvalidDerivationPath { path: String },

    #[error("fake signer namespace must not be empty")]
    InvalidFakeNamespace,

    #[error("remote signer endpoint must not be empty")]
    EmptyRemoteSignerEndpoint,

    #[error("local signer mnemonic must not be empty")]
    EmptyLocalSignerMnemonic,

    #[error("signer key ref not found: {key_ref}")]
    UnknownSignerKeyRef { key_ref: String },

    #[error("signer health check failed: {message}")]
    HealthCheckFailed { message: String },

    #[error("remote signer transport error during {operation}: {message}")]
    RemoteTransport {
        operation: &'static str,
        message: String,
    },

    #[error(
        "remote signer returned non-success status during {operation}: status={status}, body={body}"
    )]
    RemoteHttpStatus {
        operation: &'static str,
        status: u16,
        body: String,
    },

    #[error("remote signer returned invalid json during {operation}: {message}")]
    RemoteJson {
        operation: &'static str,
        message: String,
    },

    #[error("transaction request id must not be empty")]
    EmptyRequestId,

    #[error("local signer failed during {operation}: {message}")]
    LocalSigner {
        operation: &'static str,
        message: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnsignedTx {
    pub request_id: String,
    pub chain_id: u64,
    pub nonce: u64,
    pub to: EvmAddress,
    pub value: RawAmount,
    pub gas_limit: u64,
    pub max_fee_per_gas: RawAmount,
    pub max_priority_fee_per_gas: RawAmount,
    pub data: Vec<u8>,
}

impl UnsignedTx {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        request_id: impl Into<String>,
        chain_id: u64,
        nonce: u64,
        to: EvmAddress,
        value: RawAmount,
        gas_limit: u64,
        max_fee_per_gas: RawAmount,
        max_priority_fee_per_gas: RawAmount,
        data: Vec<u8>,
    ) -> Result<Self, SignerError> {
        let request_id = normalize_request_id(request_id)?;
        Ok(Self {
            request_id,
            chain_id,
            nonce,
            to,
            value,
            gas_limit,
            max_fee_per_gas,
            max_priority_fee_per_gas,
            data,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedTx {
    pub request_id: String,
    pub chain_id: u64,
    pub nonce: u64,
    pub from: EvmAddress,
    pub to: EvmAddress,
    pub tx_hash: TxHash,
    pub raw_tx: Vec<u8>,
}

#[async_trait]
pub trait SignerProvider: Send + Sync {
    async fn derive_address(&self, key_ref: &str, path: &str) -> Result<EvmAddress, SignerError>;

    async fn sign_transaction(
        &self,
        key_ref: &str,
        path: &str,
        tx: UnsignedTx,
    ) -> Result<SignedTx, SignerError>;

    async fn health_check(&self) -> Result<(), SignerError>;
}

#[derive(Clone, Debug)]
pub struct DeterministicFakeSigner {
    namespace: String,
    state: Arc<Mutex<FakeSignerState>>,
}

impl Default for DeterministicFakeSigner {
    fn default() -> Self {
        Self {
            namespace: "pay3-test".to_string(),
            state: Arc::new(Mutex::new(FakeSignerState::default())),
        }
    }
}

impl DeterministicFakeSigner {
    pub fn new(namespace: impl Into<String>) -> Result<Self, SignerError> {
        let namespace = namespace.into();
        let namespace = namespace.trim();
        if namespace.is_empty() {
            return Err(SignerError::InvalidFakeNamespace);
        }

        Ok(Self {
            namespace: namespace.to_string(),
            state: Arc::new(Mutex::new(FakeSignerState::default())),
        })
    }

    pub fn with_allowed_key_refs<I, S>(
        namespace: impl Into<String>,
        key_refs: I,
    ) -> Result<Self, SignerError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let signer = Self::new(namespace)?;
        for key_ref in key_refs {
            signer.allow_key_ref(key_ref)?;
        }
        Ok(signer)
    }

    pub fn allow_key_ref(&self, key_ref: impl Into<String>) -> Result<Self, SignerError> {
        self.state
            .lock()
            .expect("fake signer mutex poisoned")
            .allowed_key_refs
            .insert(normalize_signer_key_ref(key_ref)?);
        Ok(self.clone())
    }

    pub fn fail_health_check(&self, message: impl Into<String>) -> Self {
        self.state
            .lock()
            .expect("fake signer mutex poisoned")
            .health_error = Some(message.into());
        self.clone()
    }

    pub fn recover_health_check(&self) -> Self {
        self.state
            .lock()
            .expect("fake signer mutex poisoned")
            .health_error = None;
        self.clone()
    }

    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    pub fn allowed_key_refs(&self) -> BTreeSet<String> {
        self.state
            .lock()
            .expect("fake signer mutex poisoned")
            .allowed_key_refs
            .clone()
    }
}

#[derive(Clone, Debug, Default)]
struct FakeSignerState {
    allowed_key_refs: BTreeSet<String>,
    health_error: Option<String>,
}

#[async_trait]
impl SignerProvider for DeterministicFakeSigner {
    async fn derive_address(&self, key_ref: &str, path: &str) -> Result<EvmAddress, SignerError> {
        let key_ref = normalize_signer_key_ref(key_ref)?;
        validate_derivation_path(path)?;
        self.ensure_allowed_key_ref(&key_ref)?;

        let preimage = format!(
            "pay3:deterministic-fake-wallet:v1:{}:{}:{}",
            self.namespace, key_ref, path
        );
        let digest = keccak256(preimage.as_bytes());
        let digest = digest.as_slice();
        let mut address = [0u8; 20];
        address.copy_from_slice(&digest[12..32]);
        Ok(EvmAddress::from_bytes(address))
    }

    async fn sign_transaction(
        &self,
        key_ref: &str,
        path: &str,
        tx: UnsignedTx,
    ) -> Result<SignedTx, SignerError> {
        let key_ref = normalize_signer_key_ref(key_ref)?;
        validate_derivation_path(path)?;
        normalize_request_id(&tx.request_id)?;
        self.ensure_allowed_key_ref(&key_ref)?;

        let from = self.derive_address(&key_ref, path).await?;
        let tx_json = serde_json::to_vec(&tx).expect("UnsignedTx serialization cannot fail");
        let signing_preimage = [
            b"pay3:deterministic-fake-signer:v1".as_slice(),
            self.namespace.as_bytes(),
            key_ref.as_bytes(),
            path.as_bytes(),
            from.as_bytes(),
            &tx_json,
        ]
        .concat();
        let signature_digest = keccak256(signing_preimage);

        let mut raw_tx = Vec::with_capacity(65);
        raw_tx.extend_from_slice(b"pay3-fake-signed-tx-v1:");
        raw_tx.extend_from_slice(signature_digest.as_slice());
        let tx_hash_digest = keccak256(&raw_tx);
        let mut tx_hash = [0u8; 32];
        tx_hash.copy_from_slice(tx_hash_digest.as_slice());

        Ok(SignedTx {
            request_id: tx.request_id,
            chain_id: tx.chain_id,
            nonce: tx.nonce,
            from,
            to: tx.to,
            tx_hash: TxHash::from_bytes(tx_hash),
            raw_tx,
        })
    }

    async fn health_check(&self) -> Result<(), SignerError> {
        let health_error = self
            .state
            .lock()
            .expect("fake signer mutex poisoned")
            .health_error
            .clone();
        match health_error {
            Some(message) => Err(SignerError::HealthCheckFailed { message }),
            None => Ok(()),
        }
    }
}

impl DeterministicFakeSigner {
    fn ensure_allowed_key_ref(&self, key_ref: &str) -> Result<(), SignerError> {
        let state = self.state.lock().expect("fake signer mutex poisoned");
        if !state.allowed_key_refs.is_empty() && !state.allowed_key_refs.contains(key_ref) {
            return Err(SignerError::UnknownSignerKeyRef {
                key_ref: key_ref.to_string(),
            });
        }
        Ok(())
    }
}

fn normalize_signer_key_ref(value: impl Into<String>) -> Result<String, SignerError> {
    let value = value.into();
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(SignerError::EmptySignerKeyRef);
    }
    Ok(trimmed.to_string())
}

fn normalize_request_id(value: impl Into<String>) -> Result<String, SignerError> {
    let value = value.into();
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(SignerError::EmptyRequestId);
    }
    Ok(trimmed.to_string())
}

fn validate_derivation_path(path: &str) -> Result<(), SignerError> {
    let mut parts = path.split('/');
    let valid = matches!(parts.next(), Some("m"))
        && matches!(parts.next(), Some("44'"))
        && matches!(parts.next(), Some("60'"))
        && valid_hardened_index(parts.next())
        && valid_plain_index(parts.next())
        && valid_plain_index(parts.next())
        && parts.next().is_none();

    if valid {
        Ok(())
    } else {
        Err(SignerError::InvalidDerivationPath {
            path: path.to_string(),
        })
    }
}

fn valid_hardened_index(part: Option<&str>) -> bool {
    let Some(part) = part else {
        return false;
    };
    let Some(index) = part.strip_suffix('\'') else {
        return false;
    };
    valid_decimal_index(index)
}

fn valid_plain_index(part: Option<&str>) -> bool {
    part.is_some_and(valid_decimal_index)
}

fn valid_decimal_index(value: &str) -> bool {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }

    let Ok(index) = value.parse::<u32>() else {
        return false;
    };
    index <= MAX_DERIVATION_INDEX
}
