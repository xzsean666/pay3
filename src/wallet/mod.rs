//! HD wallet address derivation boundary.

use std::collections::BTreeSet;

use alloy_primitives::keccak256;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::domain::{DerivationSegment, EvmAddress, MAX_DERIVATION_INDEX};
use crate::signer::{LocalMnemonicSigner, RemoteHttpSigner, SignerError, SignerProvider};

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WalletError {
    #[error("signer key ref must not be empty")]
    EmptySignerKeyRef,

    #[error("derivation version must be greater than zero, got {version}")]
    InvalidDerivationVersion { version: u32 },

    #[error("derivation path is invalid: {path}")]
    InvalidDerivationPath { path: String },

    #[error("fake wallet namespace must not be empty")]
    InvalidFakeNamespace,

    #[error("signer key ref not found: {key_ref}")]
    UnknownSignerKeyRef { key_ref: String },

    #[error("remote signer call failed: {message}")]
    RemoteSignerCallFailed { message: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeriveAddressRequest {
    pub signer_key_ref: String,
    pub derivation_version: u32,
    pub segment: DerivationSegment,
}

impl DeriveAddressRequest {
    pub fn new(
        signer_key_ref: impl Into<String>,
        derivation_version: u32,
        segment: DerivationSegment,
    ) -> Result<Self, WalletError> {
        if derivation_version == 0 {
            return Err(WalletError::InvalidDerivationVersion {
                version: derivation_version,
            });
        }

        Ok(Self {
            signer_key_ref: normalize_signer_key_ref(signer_key_ref)?,
            derivation_version,
            segment,
        })
    }

    pub fn derivation_path(&self) -> String {
        self.segment.derivation_path()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DerivedChildAddress {
    pub signer_key_ref: String,
    pub derivation_version: u32,
    pub segment: DerivationSegment,
    pub derivation_path: String,
    pub address: EvmAddress,
}

#[async_trait]
pub trait AddressDeriver: Send + Sync {
    async fn derive_address(&self, key_ref: &str, path: &str) -> Result<EvmAddress, WalletError>;
}

#[derive(Clone, Debug)]
pub struct HdWallet<D> {
    deriver: D,
}

impl<D> HdWallet<D> {
    pub const fn new(deriver: D) -> Self {
        Self { deriver }
    }

    pub const fn deriver(&self) -> &D {
        &self.deriver
    }
}

impl<D> HdWallet<D>
where
    D: AddressDeriver,
{
    pub async fn derive_child_address(
        &self,
        request: DeriveAddressRequest,
    ) -> Result<DerivedChildAddress, WalletError> {
        let derivation_path = request.derivation_path();
        let address = self
            .deriver
            .derive_address(&request.signer_key_ref, &derivation_path)
            .await?;

        Ok(DerivedChildAddress {
            signer_key_ref: request.signer_key_ref,
            derivation_version: request.derivation_version,
            segment: request.segment,
            derivation_path,
            address,
        })
    }
}

#[async_trait]
impl AddressDeriver for RemoteHttpSigner {
    async fn derive_address(&self, key_ref: &str, path: &str) -> Result<EvmAddress, WalletError> {
        SignerProvider::derive_address(self, key_ref, path)
            .await
            .map_err(map_signer_error)
    }
}

#[async_trait]
impl AddressDeriver for LocalMnemonicSigner {
    async fn derive_address(&self, key_ref: &str, path: &str) -> Result<EvmAddress, WalletError> {
        SignerProvider::derive_address(self, key_ref, path)
            .await
            .map_err(map_signer_error)
    }
}

fn map_signer_error(error: SignerError) -> WalletError {
    match error {
        SignerError::EmptySignerKeyRef => WalletError::EmptySignerKeyRef,
        SignerError::InvalidDerivationPath { path } => WalletError::InvalidDerivationPath { path },
        SignerError::InvalidFakeNamespace => WalletError::RemoteSignerCallFailed {
            message: "signer reported invalid fake namespace".to_string(),
        },
        SignerError::EmptyRemoteSignerEndpoint => WalletError::RemoteSignerCallFailed {
            message: "remote signer endpoint must not be empty".to_string(),
        },
        SignerError::EmptyLocalSignerMnemonic => WalletError::RemoteSignerCallFailed {
            message: "local signer mnemonic must not be empty".to_string(),
        },
        SignerError::UnknownSignerKeyRef { key_ref } => {
            WalletError::UnknownSignerKeyRef { key_ref }
        }
        SignerError::HealthCheckFailed { message } => WalletError::RemoteSignerCallFailed {
            message: format!("health check failed: {message}"),
        },
        SignerError::RemoteTransport { operation, message } => {
            WalletError::RemoteSignerCallFailed {
                message: format!("{operation} transport error: {message}"),
            }
        }
        SignerError::RemoteHttpStatus {
            operation,
            status,
            body,
        } => WalletError::RemoteSignerCallFailed {
            message: format!("{operation} returned status {status}: {body}"),
        },
        SignerError::RemoteJson { operation, message } => WalletError::RemoteSignerCallFailed {
            message: format!("{operation} returned invalid json: {message}"),
        },
        SignerError::EmptyRequestId => WalletError::RemoteSignerCallFailed {
            message: "signer rejected empty request id".to_string(),
        },
        SignerError::LocalSigner { operation, message } => WalletError::RemoteSignerCallFailed {
            message: format!("{operation} local signer error: {message}"),
        },
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeterministicFakeDeriver {
    namespace: String,
    allowed_key_refs: BTreeSet<String>,
}

impl Default for DeterministicFakeDeriver {
    fn default() -> Self {
        Self {
            namespace: "pay3-test".to_string(),
            allowed_key_refs: BTreeSet::new(),
        }
    }
}

impl DeterministicFakeDeriver {
    pub fn new(namespace: impl Into<String>) -> Result<Self, WalletError> {
        let namespace = namespace.into();
        let namespace = namespace.trim();
        if namespace.is_empty() {
            return Err(WalletError::InvalidFakeNamespace);
        }

        Ok(Self {
            namespace: namespace.to_string(),
            allowed_key_refs: BTreeSet::new(),
        })
    }

    pub fn with_allowed_key_refs<I, S>(
        namespace: impl Into<String>,
        key_refs: I,
    ) -> Result<Self, WalletError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut deriver = Self::new(namespace)?;
        for key_ref in key_refs {
            deriver
                .allowed_key_refs
                .insert(normalize_signer_key_ref(key_ref)?);
        }
        Ok(deriver)
    }

    pub fn allow_key_ref(mut self, key_ref: impl Into<String>) -> Result<Self, WalletError> {
        self.allowed_key_refs
            .insert(normalize_signer_key_ref(key_ref)?);
        Ok(self)
    }

    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    pub fn allowed_key_refs(&self) -> &BTreeSet<String> {
        &self.allowed_key_refs
    }
}

#[async_trait]
impl AddressDeriver for DeterministicFakeDeriver {
    async fn derive_address(&self, key_ref: &str, path: &str) -> Result<EvmAddress, WalletError> {
        let key_ref = normalize_signer_key_ref(key_ref)?;
        validate_derivation_path(path)?;

        if !self.allowed_key_refs.is_empty() && !self.allowed_key_refs.contains(&key_ref) {
            return Err(WalletError::UnknownSignerKeyRef { key_ref });
        }

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
}

fn normalize_signer_key_ref(value: impl Into<String>) -> Result<String, WalletError> {
    let value = value.into();
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(WalletError::EmptySignerKeyRef);
    }
    Ok(trimmed.to_string())
}

fn validate_derivation_path(path: &str) -> Result<(), WalletError> {
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
        Err(WalletError::InvalidDerivationPath {
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

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::time::Duration;

    use axum::{
        Router,
        extract::{Json, State},
        http::StatusCode,
        routing::{get, post},
    };
    use serde::Deserialize;
    use serde_json::json;
    use tokio::net::TcpListener;

    use crate::domain::{RawAmount, TxHash};
    use crate::signer::{SignedTx, UnsignedTx};

    use super::*;

    #[derive(Clone)]
    struct RemoteTestState {
        expected_key_ref: String,
        expected_path: String,
        address: EvmAddress,
        tx_hash: TxHash,
    }

    #[derive(Deserialize)]
    struct RemoteDeriveRequest {
        key_ref: String,
        path: String,
    }

    #[derive(Deserialize)]
    struct RemoteSignRequest {
        key_ref: String,
        path: String,
        transaction: UnsignedTx,
    }

    async fn spawn_remote_signer_server(
        state: RemoteTestState,
    ) -> (String, tokio::task::JoinHandle<()>) {
        async fn healthz() -> Json<serde_json::Value> {
            Json(json!({ "status": "ok" }))
        }

        async fn derive_address(
            State(state): State<RemoteTestState>,
            Json(body): Json<RemoteDeriveRequest>,
        ) -> Json<EvmAddress> {
            assert_eq!(body.key_ref, state.expected_key_ref);
            assert_eq!(body.path, state.expected_path);
            Json(state.address)
        }

        async fn sign_transaction(
            State(state): State<RemoteTestState>,
            Json(body): Json<RemoteSignRequest>,
        ) -> Json<SignedTx> {
            assert_eq!(body.key_ref, state.expected_key_ref);
            assert_eq!(body.path, state.expected_path);
            assert_eq!(body.transaction.request_id, "request-1");
            assert_eq!(body.transaction.chain_id, 31337);
            assert_eq!(body.transaction.nonce, 9);
            Json(SignedTx {
                request_id: body.transaction.request_id,
                chain_id: body.transaction.chain_id,
                nonce: body.transaction.nonce,
                from: state.address,
                to: body.transaction.to,
                tx_hash: state.tx_hash,
                raw_tx: vec![0xde, 0xad, 0xbe, 0xef],
            })
        }

        let app = Router::new()
            .route("/healthz", get(healthz))
            .route("/v1/addresses/derive", post(derive_address))
            .route("/v1/transactions/sign", post(sign_transaction))
            .with_state(state);

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let addr: SocketAddr = listener.local_addr().expect("listener addr");
        let handle = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve remote signer");
        });

        (format!("http://{addr}"), handle)
    }

    #[tokio::test]
    async fn derives_child_address_from_segment_path() {
        let wallet = HdWallet::new(
            DeterministicFakeDeriver::default()
                .allow_key_ref("pay3-master")
                .unwrap(),
        );
        let request =
            DeriveAddressRequest::new("pay3-master", 1, DerivationSegment::new(7, 8, 9).unwrap())
                .unwrap();

        let derived = wallet.derive_child_address(request).await.unwrap();

        assert_eq!(derived.signer_key_ref, "pay3-master");
        assert_eq!(derived.derivation_version, 1);
        assert_eq!(derived.segment, DerivationSegment::new(7, 8, 9).unwrap());
        assert_eq!(derived.derivation_path, "m/44'/60'/7'/8/9");
        assert_eq!(
            derived.address.to_lower_hex(),
            "0x0db486831dd0dd9148fbc7ef9b086a8c9f6044c7"
        );
    }

    #[tokio::test]
    async fn deterministic_fake_deriver_is_stable() {
        let deriver =
            DeterministicFakeDeriver::with_allowed_key_refs("pay3-wallet-tests", ["pay3-master"])
                .unwrap();
        let path = "m/44'/60'/0'/0/42";

        let first = deriver.derive_address("pay3-master", path).await.unwrap();
        let second = deriver.derive_address("pay3-master", path).await.unwrap();

        assert_eq!(first, second);
        assert_eq!(
            first.to_lower_hex(),
            "0x890d2a4b6d3f89babec862ca17d466aad7ff831f"
        );
    }

    #[tokio::test]
    async fn different_segments_derive_different_addresses() {
        let wallet = HdWallet::new(DeterministicFakeDeriver::default());
        let first = wallet
            .derive_child_address(
                DeriveAddressRequest::new("pay3-master", 1, DerivationSegment::ZERO).unwrap(),
            )
            .await
            .unwrap();
        let second = wallet
            .derive_child_address(
                DeriveAddressRequest::new(
                    "pay3-master",
                    1,
                    DerivationSegment::ZERO.next().unwrap(),
                )
                .unwrap(),
            )
            .await
            .unwrap();

        assert_ne!(first.address, second.address);
    }

    #[tokio::test]
    async fn rollover_segment_uses_next_change_path() {
        let wallet = HdWallet::new(DeterministicFakeDeriver::default());
        let segment = DerivationSegment::new(0, 0, MAX_DERIVATION_INDEX)
            .unwrap()
            .next()
            .unwrap();

        let derived = wallet
            .derive_child_address(DeriveAddressRequest::new("pay3-master", 1, segment).unwrap())
            .await
            .unwrap();

        assert_eq!(derived.segment, DerivationSegment::new(0, 1, 0).unwrap());
        assert_eq!(derived.derivation_path, "m/44'/60'/0'/1/0");
    }

    #[test]
    fn invalid_request_is_rejected() {
        assert!(matches!(
            DeriveAddressRequest::new(" ", 1, DerivationSegment::ZERO),
            Err(WalletError::EmptySignerKeyRef)
        ));
        assert!(matches!(
            DeriveAddressRequest::new("pay3-master", 0, DerivationSegment::ZERO),
            Err(WalletError::InvalidDerivationVersion { version: 0 })
        ));
    }

    #[tokio::test]
    async fn fake_deriver_can_reject_unknown_key_ref() {
        let deriver =
            DeterministicFakeDeriver::with_allowed_key_refs("pay3-wallet-tests", ["pay3-master"])
                .unwrap();

        let error = deriver
            .derive_address("rotated-key", "m/44'/60'/0'/0/0")
            .await
            .unwrap_err();

        assert_eq!(
            error,
            WalletError::UnknownSignerKeyRef {
                key_ref: "rotated-key".to_string()
            }
        );
    }

    #[tokio::test]
    async fn fake_deriver_rejects_malformed_paths() {
        let deriver = DeterministicFakeDeriver::default();

        let error = deriver
            .derive_address("pay3-master", "m/44'/60'/0'/0")
            .await
            .unwrap_err();

        assert_eq!(
            error,
            WalletError::InvalidDerivationPath {
                path: "m/44'/60'/0'/0".to_string()
            }
        );
    }

    #[test]
    fn fake_deriver_rejects_empty_namespace() {
        assert_eq!(
            DeterministicFakeDeriver::new("  ").unwrap_err(),
            WalletError::InvalidFakeNamespace
        );
    }

    #[tokio::test]
    async fn remote_http_signer_uses_http_contract_for_health_derivation_and_signing() {
        let state = RemoteTestState {
            expected_key_ref: "pay3-master".to_string(),
            expected_path: "m/44'/60'/7'/8/9".to_string(),
            address: EvmAddress::from_bytes([0x11; 20]),
            tx_hash: TxHash::from_bytes([0x22; 32]),
        };
        let (endpoint, handle) = spawn_remote_signer_server(state.clone()).await;
        let client = RemoteHttpSigner::new(endpoint, Duration::from_secs(2)).unwrap();

        client.health_check().await.expect("health check");

        let derived =
            AddressDeriver::derive_address(&client, &state.expected_key_ref, &state.expected_path)
                .await
                .expect("remote derivation");
        assert_eq!(derived, state.address);

        let unsigned = UnsignedTx::new(
            "request-1",
            31337,
            9,
            EvmAddress::from_bytes([0x33; 20]),
            RawAmount::from(1_000u64),
            80_000,
            RawAmount::from(50_000_000_000u64),
            RawAmount::from(2_000_000_000u64),
            vec![0xaa, 0xbb, 0xcc],
        )
        .expect("unsigned tx");
        let signed = client
            .sign_transaction(
                &state.expected_key_ref,
                &state.expected_path,
                unsigned.clone(),
            )
            .await
            .expect("remote signing");

        assert_eq!(signed.request_id, "request-1");
        assert_eq!(signed.chain_id, unsigned.chain_id);
        assert_eq!(signed.nonce, unsigned.nonce);
        assert_eq!(signed.from, state.address);
        assert_eq!(signed.to, unsigned.to);
        assert_eq!(signed.tx_hash, state.tx_hash);
        assert_eq!(signed.raw_tx, vec![0xde, 0xad, 0xbe, 0xef]);

        handle.abort();
    }

    #[tokio::test]
    async fn remote_http_signer_maps_http_failures_to_wallet_error() {
        async fn healthz() -> StatusCode {
            StatusCode::OK
        }

        async fn not_found() -> StatusCode {
            StatusCode::NOT_FOUND
        }

        let app = Router::new()
            .route("/healthz", get(healthz))
            .route("/v1/addresses/derive", post(not_found))
            .route("/v1/transactions/sign", post(not_found));
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let addr = listener.local_addr().expect("listener addr");
        let handle = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve remote signer");
        });

        let client = RemoteHttpSigner::new(format!("http://{addr}"), Duration::from_secs(2))
            .expect("remote signer client");
        let error = AddressDeriver::derive_address(&client, "pay3-master", "m/44'/60'/0'/0/0")
            .await
            .expect_err("404 should map to remote signer failure");

        assert!(matches!(
            error,
            WalletError::RemoteSignerCallFailed { message }
                if message.contains("status 404")
        ));

        handle.abort();
    }
}
