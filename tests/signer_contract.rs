mod domain {
    pub use pay3::domain::*;
}

#[path = "../src/signer/mod.rs"]
mod signer;

use alloy_primitives::keccak256;
use pay3::{
    domain::{EvmAddress, RawAmount, TxHash},
    wallet::{AddressDeriver, DeterministicFakeDeriver},
};
use signer::{DeterministicFakeSigner, SignerError, SignerProvider, UnsignedTx};

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

fn unsigned_tx(nonce: u64) -> UnsignedTx {
    UnsignedTx::new(
        format!("collection-job-{nonce}"),
        1,
        nonce,
        EvmAddress::from_bytes([0x22; 20]),
        RawAmount::ZERO,
        65_000,
        RawAmount::from(30_000_000_000),
        RawAmount::from(1_500_000_000),
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
