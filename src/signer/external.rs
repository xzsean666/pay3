//! Remote HTTP signer adapter.
//!
//! Contract:
//! - `GET /healthz` returns JSON `{ "status": "ok" }`.
//! - `POST /v1/addresses/derive` accepts `{ "key_ref": "...", "path": "m/44'/60'/0'/0/1" }`
//!   and returns an `EvmAddress` JSON string.
//! - `POST /v1/transactions/sign` accepts `{ "key_ref": "...", "path": "...", "transaction": UnsignedTx }`
//!   and returns a `SignedTx` JSON object.

use std::time::Duration;

use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::domain::EvmAddress;

use super::{
    SignedTx, SignerError, SignerProvider, UnsignedTx, normalize_request_id,
    normalize_signer_key_ref, validate_derivation_path,
};

const HEALTHZ_PATH: &str = "/healthz";
const DERIVE_ADDRESS_PATH: &str = "/v1/addresses/derive";
const SIGN_TRANSACTION_PATH: &str = "/v1/transactions/sign";
const HEALTHY_STATUS: &str = "ok";

#[derive(Clone, Debug)]
pub struct RemoteHttpSigner {
    endpoint: String,
    timeout: Duration,
    client: Client,
}

impl RemoteHttpSigner {
    pub fn new(endpoint: impl Into<String>, timeout: Duration) -> Result<Self, SignerError> {
        let endpoint = endpoint.into();
        let endpoint = endpoint.trim().trim_end_matches('/');
        if endpoint.is_empty() {
            return Err(SignerError::EmptyRemoteSignerEndpoint);
        }

        Ok(Self {
            endpoint: endpoint.to_string(),
            timeout,
            client: Client::new(),
        })
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    fn url(&self, path: &str) -> String {
        format!("{}/{}", self.endpoint, path.trim_start_matches('/'))
    }

    async fn request_json<T>(
        &self,
        request: reqwest::RequestBuilder,
        operation: &'static str,
    ) -> Result<T, SignerError>
    where
        T: DeserializeOwned,
    {
        tokio::time::timeout(self.timeout, async {
            let response = request
                .send()
                .await
                .map_err(|error| SignerError::RemoteTransport {
                    operation,
                    message: error.to_string(),
                })?;

            let status = response.status();
            let body = response
                .bytes()
                .await
                .map_err(|error| SignerError::RemoteTransport {
                    operation,
                    message: error.to_string(),
                })?;

            if !status.is_success() {
                return Err(SignerError::RemoteHttpStatus {
                    operation,
                    status: status.as_u16(),
                    body: render_body(&body),
                });
            }

            serde_json::from_slice(&body).map_err(|error| SignerError::RemoteJson {
                operation,
                message: error.to_string(),
            })
        })
        .await
        .map_err(|_| SignerError::RemoteTransport {
            operation,
            message: format!("request timed out after {:?}", self.timeout),
        })?
    }
}

#[derive(Debug, Serialize)]
struct DeriveAddressRequest<'a> {
    key_ref: &'a str,
    path: &'a str,
}

#[derive(Debug, Serialize)]
struct SignTransactionRequest<'a> {
    key_ref: &'a str,
    path: &'a str,
    transaction: &'a UnsignedTx,
}

#[derive(Debug, Deserialize)]
struct HealthzResponse {
    status: String,
}

fn render_body(bytes: &[u8]) -> String {
    let body = String::from_utf8_lossy(bytes).trim().to_string();
    if body.is_empty() {
        "<empty>".to_string()
    } else {
        body
    }
}

#[async_trait]
impl SignerProvider for RemoteHttpSigner {
    async fn derive_address(&self, key_ref: &str, path: &str) -> Result<EvmAddress, SignerError> {
        let key_ref = normalize_signer_key_ref(key_ref)?;
        validate_derivation_path(path)?;

        let request = DeriveAddressRequest {
            key_ref: &key_ref,
            path,
        };
        self.request_json(
            self.client
                .post(self.url(DERIVE_ADDRESS_PATH))
                .json(&request),
            "derive_address",
        )
        .await
    }

    async fn sign_transaction(
        &self,
        key_ref: &str,
        path: &str,
        tx: UnsignedTx,
    ) -> Result<SignedTx, SignerError> {
        let key_ref = normalize_signer_key_ref(key_ref)?;
        validate_derivation_path(path)?;

        let UnsignedTx {
            request_id,
            chain_id,
            nonce,
            to,
            value,
            gas_limit,
            max_fee_per_gas,
            max_priority_fee_per_gas,
            data,
        } = tx;
        let request_id = normalize_request_id(request_id)?;
        let tx = UnsignedTx {
            request_id,
            chain_id,
            nonce,
            to,
            value,
            gas_limit,
            max_fee_per_gas,
            max_priority_fee_per_gas,
            data,
        };

        let request = SignTransactionRequest {
            key_ref: &key_ref,
            path,
            transaction: &tx,
        };
        self.request_json(
            self.client
                .post(self.url(SIGN_TRANSACTION_PATH))
                .json(&request),
            "sign_transaction",
        )
        .await
    }

    async fn health_check(&self) -> Result<(), SignerError> {
        let health = self
            .request_json::<HealthzResponse>(
                self.client.get(self.url(HEALTHZ_PATH)),
                "health_check",
            )
            .await?;

        if health.status == HEALTHY_STATUS {
            Ok(())
        } else {
            Err(SignerError::HealthCheckFailed {
                message: format!("unexpected healthz status: {}", health.status),
            })
        }
    }
}
