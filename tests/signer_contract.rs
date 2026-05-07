mod domain {
    pub use pay3::domain::*;
}

#[path = "../src/signer/mod.rs"]
mod signer;

use alloy_primitives::keccak256;
use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use pay3::{
    domain::{EvmAddress, RawAmount, TxHash},
    wallet::{AddressDeriver, DeterministicFakeDeriver},
};
use serde::{Deserialize, Serialize};
use signer::{
    DeterministicFakeSigner, LocalMnemonicSigner, RemoteHttpSigner, SignerError, SignerProvider,
    UnsignedTx,
};
use std::time::Duration;
use tokio::net::TcpListener;

#[tokio::test]
async fn fake_signer_derives_same_address_as_deterministic_wallet() {
    let key_ref = "pay3-master";
    let path = "m/44'/60'/7'/0/42";
    let signer =
        DeterministicFakeSigner::with_allowed_key_refs("pay3-signer-tests", [key_ref]).unwrap();
    let wallet =
        DeterministicFakeDeriver::with_allowed_key_refs("pay3-signer-tests", [key_ref]).unwrap();
    assert_eq!(signer.namespace(), "pay3-signer-tests");
    assert!(signer.allowed_key_refs().contains(key_ref));

    let signer_address = signer.derive_address(key_ref, path).await.unwrap();
    let wallet_address = wallet.derive_address(key_ref, path).await.unwrap();

    assert_eq!(signer_address, wallet_address);
    assert_eq!(
        signer_address.to_lower_hex(),
        "0x8b230151fc5d135bcd0f65d23febda4585da8a93"
    );
}

#[tokio::test]
async fn fake_signer_produces_stable_raw_tx_and_hash() {
    let key_ref = "pay3-master";
    let path = "m/44'/60'/0'/0/1";
    let signer =
        DeterministicFakeSigner::with_allowed_key_refs("pay3-signer-tests", [key_ref]).unwrap();
    let tx = unsigned_tx(12);

    let first = signer
        .sign_transaction(key_ref, path, tx.clone())
        .await
        .unwrap();
    let second = signer.sign_transaction(key_ref, path, tx).await.unwrap();

    assert_eq!(first, second);
    assert_eq!(first.request_id, "collection-job-12");
    assert_eq!(first.chain_id, 1);
    assert_eq!(first.nonce, 12);
    assert_eq!(first.raw_tx.len(), b"pay3-fake-signed-tx-v1:".len() + 32);
    assert_eq!(first.tx_hash, hash_raw_tx(&first.raw_tx));
}

#[tokio::test]
async fn fake_signer_changes_signature_when_tx_request_changes() {
    let key_ref = "pay3-master";
    let path = "m/44'/60'/0'/0/1";
    let signer =
        DeterministicFakeSigner::with_allowed_key_refs("pay3-signer-tests", [key_ref]).unwrap();

    let first = signer
        .sign_transaction(key_ref, path, unsigned_tx(12))
        .await
        .unwrap();
    let second = signer
        .sign_transaction(key_ref, path, unsigned_tx(13))
        .await
        .unwrap();

    assert_ne!(first.raw_tx, second.raw_tx);
    assert_ne!(first.tx_hash, second.tx_hash);
}

#[tokio::test]
async fn fake_signer_rejects_unknown_key_ref() {
    let signer =
        DeterministicFakeSigner::with_allowed_key_refs("pay3-signer-tests", ["pay3-master"])
            .unwrap();

    let error = signer
        .derive_address("rotated-key", "m/44'/60'/0'/0/1")
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        SignerError::UnknownSignerKeyRef { key_ref } if key_ref == "rotated-key"
    ));
}

#[tokio::test]
async fn fake_signer_can_simulate_health_failure_and_recovery() {
    let signer = DeterministicFakeSigner::default();
    assert!(signer.health_check().await.is_ok());

    signer.fail_health_check("kms timeout");
    assert!(matches!(
        signer.health_check().await,
        Err(SignerError::HealthCheckFailed { message }) if message == "kms timeout"
    ));

    signer.recover_health_check();
    assert!(signer.health_check().await.is_ok());
}

#[tokio::test]
async fn local_mnemonic_signer_derives_and_signs_eip1559_transactions() {
    let signer = LocalMnemonicSigner::new(
        TEST_KEY_REF,
        "test test test test test test test test test test test junk",
    )
    .unwrap();

    assert_eq!(signer.key_ref(), TEST_KEY_REF);
    signer.health_check().await.unwrap();

    let address = signer
        .derive_address(TEST_KEY_REF, TEST_PATH)
        .await
        .unwrap();
    let next_address = signer
        .derive_address(TEST_KEY_REF, "m/44'/60'/7'/0/43")
        .await
        .unwrap();
    let signed = signer
        .sign_transaction(TEST_KEY_REF, TEST_PATH, unsigned_tx(12))
        .await
        .unwrap();

    assert_eq!(
        address.to_lower_hex(),
        "0x50330afc91b0457036a17efef9da963ce9477a5a"
    );
    assert_eq!(
        next_address.to_lower_hex(),
        "0xd5d191d7a552e683e26afb2b6d64a5cb88a2aea4"
    );
    assert_ne!(address, next_address);
    assert_eq!(signed.from, address);
    assert_eq!(signed.raw_tx.first(), Some(&0x02));
    assert_eq!(signed.tx_hash, hash_raw_tx(&signed.raw_tx));
    assert_ne!(signed.tx_hash, TxHash::ZERO);
}

#[tokio::test]
async fn local_mnemonic_signer_rejects_invalid_inputs() {
    let signer = LocalMnemonicSigner::new(
        TEST_KEY_REF,
        "test test test test test test test test test test test junk",
    )
    .unwrap();

    assert!(matches!(
        signer
            .derive_address("rotated-key", TEST_PATH)
            .await
            .unwrap_err(),
        SignerError::UnknownSignerKeyRef { key_ref } if key_ref == "rotated-key"
    ));
    assert!(matches!(
        signer
            .derive_address(TEST_KEY_REF, "m/44'/60'/0'/0")
            .await
            .unwrap_err(),
        SignerError::InvalidDerivationPath { .. }
    ));
    let mut empty_request_id_tx = unsigned_tx(12);
    empty_request_id_tx.request_id.clear();
    assert!(matches!(
        signer
            .sign_transaction(TEST_KEY_REF, TEST_PATH, empty_request_id_tx)
            .await
            .unwrap_err(),
        SignerError::EmptyRequestId
    ));
    assert!(matches!(
        signer
            .sign_transaction(
                TEST_KEY_REF,
                TEST_PATH,
                unsigned_tx_with_fees(1_500_000_000, 1_500_000_001),
            )
            .await
            .unwrap_err(),
        SignerError::LocalSigner {
            operation: "sign_transaction",
            ..
        }
    ));

    let invalid_mnemonic = LocalMnemonicSigner::new(TEST_KEY_REF, "not a valid mnemonic").unwrap();
    assert!(matches!(
        invalid_mnemonic.health_check().await.unwrap_err(),
        SignerError::LocalSigner {
            operation: "derive_address",
            ..
        }
    ));
}

#[tokio::test]
async fn remote_http_signer_round_trip_is_deterministic() {
    let signer =
        DeterministicFakeSigner::with_allowed_key_refs(TEST_NAMESPACE, [TEST_KEY_REF]).unwrap();
    let server = spawn_remote_signer_server(ServerMode::Happy, signer.clone()).await;
    let remote = RemoteHttpSigner::new(server.base_url.clone(), TEST_TIMEOUT).unwrap();

    assert_eq!(remote.endpoint(), server.base_url);
    assert_eq!(remote.timeout(), TEST_TIMEOUT);
    remote.health_check().await.unwrap();

    let derived_first = remote
        .derive_address(TEST_KEY_REF, TEST_PATH)
        .await
        .unwrap();
    let derived_second = remote
        .derive_address(TEST_KEY_REF, TEST_PATH)
        .await
        .unwrap();
    let expected_derived = signer
        .derive_address(TEST_KEY_REF, TEST_PATH)
        .await
        .unwrap();
    assert_eq!(derived_first, expected_derived);
    assert_eq!(derived_first, derived_second);

    let tx = unsigned_tx(12);
    let signed_first = remote
        .sign_transaction(TEST_KEY_REF, TEST_PATH, tx.clone())
        .await
        .unwrap();
    let signed_second = remote
        .sign_transaction(TEST_KEY_REF, TEST_PATH, tx.clone())
        .await
        .unwrap();
    let expected_signed = signer
        .sign_transaction(TEST_KEY_REF, TEST_PATH, tx)
        .await
        .unwrap();
    assert_eq!(signed_first, expected_signed);
    assert_eq!(signed_first, signed_second);
}

#[tokio::test]
async fn remote_http_signer_reports_transport_failures() {
    let remote = RemoteHttpSigner::new(closed_endpoint().await, TEST_TIMEOUT).unwrap();

    let error = remote.health_check().await.unwrap_err();
    assert!(matches!(
        error,
        SignerError::RemoteTransport { operation, .. } if operation == "health_check"
    ));
}

#[tokio::test]
async fn remote_http_signer_reports_non_success_status_codes() {
    let signer =
        DeterministicFakeSigner::with_allowed_key_refs(TEST_NAMESPACE, [TEST_KEY_REF]).unwrap();
    let server = spawn_remote_signer_server(ServerMode::DeriveRejected, signer).await;
    let remote = RemoteHttpSigner::new(server.base_url.clone(), TEST_TIMEOUT).unwrap();

    let error = remote
        .derive_address(TEST_KEY_REF, TEST_PATH)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        SignerError::RemoteHttpStatus {
            operation,
            status,
            body,
        } if operation == "derive_address"
            && status == StatusCode::SERVICE_UNAVAILABLE.as_u16()
            && body.contains("derivation unavailable")
    ));
}

#[tokio::test]
async fn remote_http_signer_reports_malformed_json_payloads() {
    let signer =
        DeterministicFakeSigner::with_allowed_key_refs(TEST_NAMESPACE, [TEST_KEY_REF]).unwrap();
    let server = spawn_remote_signer_server(ServerMode::SignMalformed, signer).await;
    let remote = RemoteHttpSigner::new(server.base_url.clone(), TEST_TIMEOUT).unwrap();

    let error = remote
        .sign_transaction(TEST_KEY_REF, TEST_PATH, unsigned_tx(12))
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        SignerError::RemoteJson { operation, .. } if operation == "sign_transaction"
    ));
}

fn unsigned_tx(nonce: u64) -> UnsignedTx {
    unsigned_tx_with_request_id_and_fees(
        format!("collection-job-{nonce}"),
        nonce,
        30_000_000_000,
        1_500_000_000,
    )
}

fn unsigned_tx_with_fees(max_fee_per_gas: u64, max_priority_fee_per_gas: u64) -> UnsignedTx {
    unsigned_tx_with_request_id_and_fees(
        "collection-job-12",
        12,
        max_fee_per_gas,
        max_priority_fee_per_gas,
    )
}

fn unsigned_tx_with_request_id_and_fees(
    request_id: impl Into<String>,
    nonce: u64,
    max_fee_per_gas: u64,
    max_priority_fee_per_gas: u64,
) -> UnsignedTx {
    UnsignedTx::new(
        request_id,
        1,
        nonce,
        EvmAddress::from_bytes([0x22; 20]),
        RawAmount::ZERO,
        65_000,
        RawAmount::from(max_fee_per_gas),
        RawAmount::from(max_priority_fee_per_gas),
        erc20_transfer_data(EvmAddress::from_bytes([0x33; 20]), RawAmount::from(1_000)),
    )
    .unwrap()
}

fn erc20_transfer_data(to: EvmAddress, amount: RawAmount) -> Vec<u8> {
    let mut data = Vec::with_capacity(68);
    data.extend_from_slice(&[0xa9, 0x05, 0x9c, 0xbb]);
    data.extend_from_slice(&[0u8; 12]);
    data.extend_from_slice(to.as_bytes());
    let amount_word = amount.value().to_be_bytes::<32>();
    data.extend_from_slice(&amount_word);
    data
}

fn hash_raw_tx(raw_tx: &[u8]) -> TxHash {
    let digest = keccak256(raw_tx);
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(digest.as_slice());
    TxHash::from_bytes(bytes)
}

const TEST_NAMESPACE: &str = "pay3-http-signer-tests";
const TEST_KEY_REF: &str = "pay3-master";
const TEST_PATH: &str = "m/44'/60'/7'/0/42";
const TEST_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ServerMode {
    Happy,
    DeriveRejected,
    SignMalformed,
}

#[derive(Clone)]
struct ServerState {
    signer: DeterministicFakeSigner,
    mode: ServerMode,
    expected_key_ref: String,
    expected_path: String,
    expected_tx: UnsignedTx,
}

struct TestServer {
    base_url: String,
    handle: tokio::task::JoinHandle<()>,
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

#[derive(Clone, Deserialize)]
struct DeriveAddressRequest {
    key_ref: String,
    path: String,
}

#[derive(Clone, Deserialize)]
struct SignTransactionRequest {
    key_ref: String,
    path: String,
    transaction: UnsignedTx,
}

#[derive(Clone, Serialize)]
struct HealthzResponse {
    status: &'static str,
}

async fn spawn_remote_signer_server(
    mode: ServerMode,
    signer: DeterministicFakeSigner,
) -> TestServer {
    let state = ServerState {
        signer,
        mode,
        expected_key_ref: TEST_KEY_REF.to_string(),
        expected_path: TEST_PATH.to_string(),
        expected_tx: unsigned_tx(12),
    };

    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/v1/addresses/derive", post(derive_address))
        .route("/v1/transactions/sign", post(sign_transaction))
        .with_state(state);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app.into_make_service())
            .await
            .unwrap();
    });

    TestServer {
        base_url: format!("http://{addr}"),
        handle,
    }
}

async fn closed_endpoint() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    format!("http://{addr}")
}

async fn healthz() -> Json<HealthzResponse> {
    Json(HealthzResponse { status: "ok" })
}

async fn derive_address(
    State(state): State<ServerState>,
    Json(request): Json<DeriveAddressRequest>,
) -> Response {
    assert_eq!(request.key_ref, state.expected_key_ref);
    assert_eq!(request.path, state.expected_path);

    if state.mode == ServerMode::DeriveRejected {
        return (StatusCode::SERVICE_UNAVAILABLE, "derivation unavailable").into_response();
    }

    let address = state
        .signer
        .derive_address(&request.key_ref, &request.path)
        .await
        .unwrap();
    Json(address).into_response()
}

async fn sign_transaction(
    State(state): State<ServerState>,
    Json(request): Json<SignTransactionRequest>,
) -> Response {
    assert_eq!(request.key_ref, state.expected_key_ref);
    assert_eq!(request.path, state.expected_path);
    assert_eq!(request.transaction, state.expected_tx);

    if state.mode == ServerMode::SignMalformed {
        return (StatusCode::OK, "this is not valid json").into_response();
    }

    let signed_tx = state
        .signer
        .sign_transaction(&request.key_ref, &request.path, request.transaction)
        .await
        .unwrap();
    Json(signed_tx).into_response()
}
