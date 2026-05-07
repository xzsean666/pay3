use std::fmt;

use alloy_consensus::{SignableTransaction, TxEip1559};
use alloy_primitives::TxKind;
use alloy_signer::SignerSync;
use alloy_signer_local::{MnemonicBuilder, coins_bip39::English};
use async_trait::async_trait;

use crate::domain::{EvmAddress, RawAmount, TxHash};

use super::{
    SignedTx, SignerError, SignerProvider, UnsignedTx, normalize_request_id,
    normalize_signer_key_ref, validate_derivation_path,
};

#[derive(Clone)]
pub struct LocalMnemonicSigner {
    key_ref: String,
    mnemonic: String,
}

impl fmt::Debug for LocalMnemonicSigner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LocalMnemonicSigner")
            .field("key_ref", &self.key_ref)
            .field("mnemonic", &"<redacted>")
            .finish()
    }
}

impl LocalMnemonicSigner {
    pub fn new(
        key_ref: impl Into<String>,
        mnemonic: impl Into<String>,
    ) -> Result<Self, SignerError> {
        let key_ref = normalize_signer_key_ref(key_ref)?;
        let mnemonic = mnemonic.into();
        let mnemonic = mnemonic.trim();
        if mnemonic.is_empty() {
            return Err(SignerError::EmptyLocalSignerMnemonic);
        }

        Ok(Self {
            key_ref,
            mnemonic: mnemonic.to_string(),
        })
    }

    pub fn key_ref(&self) -> &str {
        &self.key_ref
    }

    fn ensure_key_ref(&self, key_ref: &str) -> Result<(), SignerError> {
        let key_ref = normalize_signer_key_ref(key_ref)?;
        if key_ref == self.key_ref {
            Ok(())
        } else {
            Err(SignerError::UnknownSignerKeyRef { key_ref })
        }
    }

    fn signer_for_path(
        &self,
        path: &str,
    ) -> Result<alloy_signer_local::PrivateKeySigner, SignerError> {
        validate_derivation_path(path)?;
        MnemonicBuilder::<English>::default()
            .phrase(self.mnemonic.clone())
            .derivation_path(path)
            .map_err(|error| SignerError::LocalSigner {
                operation: "derive_address",
                message: error.to_string(),
            })?
            .build()
            .map_err(|error| SignerError::LocalSigner {
                operation: "derive_address",
                message: error.to_string(),
            })
    }
}

#[async_trait]
impl SignerProvider for LocalMnemonicSigner {
    async fn derive_address(&self, key_ref: &str, path: &str) -> Result<EvmAddress, SignerError> {
        self.ensure_key_ref(key_ref)?;
        let signer = self.signer_for_path(path)?;
        Ok(EvmAddress::from_alloy(signer.address()))
    }

    async fn sign_transaction(
        &self,
        key_ref: &str,
        path: &str,
        tx: UnsignedTx,
    ) -> Result<SignedTx, SignerError> {
        self.ensure_key_ref(key_ref)?;
        let request_id = normalize_request_id(&tx.request_id)?;
        let signer = self.signer_for_path(path)?;
        let from = EvmAddress::from_alloy(signer.address());
        if tx.max_priority_fee_per_gas > tx.max_fee_per_gas {
            return Err(SignerError::LocalSigner {
                operation: "sign_transaction",
                message: "max_priority_fee_per_gas exceeds max_fee_per_gas".to_string(),
            });
        }

        let eip1559 = TxEip1559 {
            chain_id: tx.chain_id,
            nonce: tx.nonce,
            gas_limit: tx.gas_limit,
            max_fee_per_gas: raw_amount_to_u128(tx.max_fee_per_gas, "max_fee_per_gas")?,
            max_priority_fee_per_gas: raw_amount_to_u128(
                tx.max_priority_fee_per_gas,
                "max_priority_fee_per_gas",
            )?,
            to: TxKind::Call(tx.to.into_alloy()),
            value: tx.value.value(),
            access_list: Default::default(),
            input: tx.data.clone().into(),
        };

        let signature = signer
            .sign_hash_sync(&eip1559.signature_hash())
            .map_err(|error| SignerError::LocalSigner {
                operation: "sign_transaction",
                message: error.to_string(),
            })?;
        let signed = eip1559.into_signed(signature);
        let tx_hash = TxHash::from_alloy(*signed.hash());
        let mut raw_tx = Vec::with_capacity(signed.eip2718_encoded_length());
        signed.eip2718_encode(&mut raw_tx);

        Ok(SignedTx {
            request_id,
            chain_id: tx.chain_id,
            nonce: tx.nonce,
            from,
            to: tx.to,
            tx_hash,
            raw_tx,
        })
    }

    async fn health_check(&self) -> Result<(), SignerError> {
        self.signer_for_path("m/44'/60'/0'/0/0")?;
        Ok(())
    }
}

fn raw_amount_to_u128(value: RawAmount, field: &'static str) -> Result<u128, SignerError> {
    let value = value.value();
    u128::try_from(value).map_err(|_| SignerError::LocalSigner {
        operation: "sign_transaction",
        message: format!("{field} exceeds u128"),
    })
}
