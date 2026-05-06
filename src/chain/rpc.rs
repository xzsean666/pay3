use std::{collections::BTreeMap, fmt, sync::Arc, time::Duration};

use alloy_primitives::U256;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{
    ChainBlock, ChainError, ChainHeaderReader, Erc20ChainClient, NativeBalanceReader,
    TransactionStatus, TransferLog, TransferLogCapacityLimits, TransferLogCapacityReport,
    TransferLogRange, TransferLogSource, TxReceipt,
};
use crate::domain::{BlockHash, ChainBlockRef, EvmAddress, RawAmount, TxHash};

pub const ERC20_TRANSFER_TOPIC: &str =
    "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef";

pub type SharedJsonRpcProvider = Arc<dyn JsonRpcProvider>;

#[async_trait]
pub trait JsonRpcProvider: Send + Sync {
    fn provider_id(&self) -> &str;

    async fn request(&self, method: &str, params: Value) -> Result<Value, ChainError>;
}

#[derive(Clone, Debug)]
pub struct HttpJsonRpcProvider {
    id: String,
    url: String,
    client: reqwest::Client,
}

impl HttpJsonRpcProvider {
    pub fn new(id: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            url: url.into(),
            client: reqwest::Client::new(),
        }
    }

    pub fn with_timeout(
        id: impl Into<String>,
        url: impl Into<String>,
        timeout: Duration,
    ) -> Result<Self, ChainError> {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|error| ChainError::rpc_unavailable(error.to_string()))?;
        Ok(Self {
            id: id.into(),
            url: url.into(),
            client,
        })
    }

    pub fn url(&self) -> &str {
        &self.url
    }
}

#[async_trait]
impl JsonRpcProvider for HttpJsonRpcProvider {
    fn provider_id(&self) -> &str {
        &self.id
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value, ChainError> {
        let response = self
            .client
            .post(&self.url)
            .json(&JsonRpcRequest {
                jsonrpc: "2.0",
                id: 1,
                method,
                params,
            })
            .send()
            .await
            .map_err(|error| {
                ChainError::rpc_unavailable(format!(
                    "provider {} request {method} failed: {error}",
                    self.id
                ))
            })?;

        let status = response.status();
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(ChainError::rpc_unavailable(format!(
                "provider {} rate limited request {method}",
                self.id
            )));
        }
        if !status.is_success() {
            return Err(ChainError::rpc_unavailable(format!(
                "provider {} returned HTTP {} for {method}",
                self.id, status
            )));
        }

        let payload = response
            .json::<JsonRpcResponse>()
            .await
            .map_err(|error| ChainError::malformed_rpc_response(error.to_string()))?;
        if let Some(error) = payload.error {
            return Err(ChainError::rpc_unavailable(format!(
                "provider {} JSON-RPC error {} for {method}: {}",
                self.id, error.code, error.message
            )));
        }
        payload.result.ok_or_else(|| {
            ChainError::malformed_rpc_response(format!(
                "provider {} omitted result for {method}",
                self.id
            ))
        })
    }
}

#[derive(Debug, Serialize)]
struct JsonRpcRequest<'a> {
    jsonrpc: &'static str,
    id: u64,
    method: &'a str,
    params: Value,
}

#[derive(Debug, Deserialize)]
struct JsonRpcResponse {
    result: Option<Value>,
    error: Option<JsonRpcError>,
}

#[derive(Debug, Deserialize)]
struct JsonRpcError {
    code: i64,
    message: String,
}

#[derive(Clone)]
pub struct RpcProviderManager {
    expected_chain_id: u64,
    min_provider_count: usize,
    providers: Vec<SharedJsonRpcProvider>,
}

impl fmt::Debug for RpcProviderManager {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RpcProviderManager")
            .field("expected_chain_id", &self.expected_chain_id)
            .field("min_provider_count", &self.min_provider_count)
            .field("provider_count", &self.providers.len())
            .finish()
    }
}

impl RpcProviderManager {
    pub fn new(
        expected_chain_id: u64,
        providers: Vec<SharedJsonRpcProvider>,
    ) -> Result<Self, ChainError> {
        Self::with_min_provider_count(expected_chain_id, providers, 1)
    }

    pub fn with_min_provider_count(
        expected_chain_id: u64,
        providers: Vec<SharedJsonRpcProvider>,
        min_provider_count: usize,
    ) -> Result<Self, ChainError> {
        if providers.len() < min_provider_count {
            return Err(ChainError::rpc_unavailable(format!(
                "configured {} RPC providers, expected at least {min_provider_count}",
                providers.len()
            )));
        }
        if providers.is_empty() {
            return Err(ChainError::rpc_unavailable("no RPC providers configured"));
        }

        Ok(Self {
            expected_chain_id,
            min_provider_count,
            providers,
        })
    }

    pub fn from_http_urls(
        expected_chain_id: u64,
        urls: &[String],
        min_provider_count: usize,
    ) -> Result<Self, ChainError> {
        let timeout = Duration::from_secs(10);
        let providers = urls
            .iter()
            .enumerate()
            .map(|(index, url)| {
                let provider = HttpJsonRpcProvider::with_timeout(
                    format!("rpc-{}", index + 1),
                    url.clone(),
                    timeout,
                )?;
                Ok(Arc::new(provider) as SharedJsonRpcProvider)
            })
            .collect::<Result<Vec<_>, ChainError>>()?;
        Self::with_min_provider_count(expected_chain_id, providers, min_provider_count)
    }

    pub fn expected_chain_id(&self) -> u64 {
        self.expected_chain_id
    }

    pub fn provider_count(&self) -> usize {
        self.providers.len()
    }

    pub fn min_provider_count(&self) -> usize {
        self.min_provider_count
    }

    pub async fn validate_chain_ids(&self) -> Result<Vec<RpcProviderChainStatus>, ChainError> {
        let mut statuses = Vec::new();
        let mut errors = Vec::new();
        for provider in &self.providers {
            match self.provider_chain_id(provider.as_ref()).await {
                Ok(actual_chain_id) if actual_chain_id == self.expected_chain_id => {
                    statuses.push(RpcProviderChainStatus {
                        provider_id: provider.provider_id().to_string(),
                        chain_id: actual_chain_id,
                    });
                }
                Ok(actual_chain_id) => {
                    return Err(ChainError::ChainIdMismatch {
                        expected: self.expected_chain_id,
                        actual: actual_chain_id,
                    });
                }
                Err(error) => {
                    errors.push(format!("{}: {error}", provider.provider_id()));
                }
            }
        }

        if statuses.len() != self.providers.len() {
            return Err(ChainError::rpc_unavailable(format!(
                "RPC chain id validation failed: {}",
                errors.join("; ")
            )));
        }

        Ok(statuses)
    }

    pub async fn readiness_probe(&self) -> Result<RpcProviderReadiness, ChainError> {
        let providers = self.validate_chain_ids().await?;
        let latest_head = self.head_by_tag("latest").await?;
        let safe_head = self.head_by_tag("safe").await?;
        let finalized_head = self.head_by_tag("finalized").await?;
        Ok(RpcProviderReadiness {
            expected_chain_id: self.expected_chain_id,
            providers,
            latest_head,
            safe_head,
            finalized_head,
        })
    }

    async fn provider_chain_id(&self, provider: &dyn JsonRpcProvider) -> Result<u64, ChainError> {
        let value = provider.request("eth_chainId", json!([])).await?;
        let chain_id = value.as_str().ok_or_else(|| {
            ChainError::malformed_rpc_response(format!(
                "provider {} returned non-string eth_chainId",
                provider.provider_id()
            ))
        })?;
        parse_hex_u64(chain_id, "eth_chainId")
    }

    async fn head_by_tag(&self, tag: &'static str) -> Result<ChainBlockRef, ChainError> {
        let blocks = self.blocks_by_tag(tag).await?;
        Ok(select_conservative_head(&blocks).block.block_ref())
    }

    async fn blocks_by_tag(&self, tag: &'static str) -> Result<Vec<ProviderBlock>, ChainError> {
        self.request_blocks(
            format!("block tag {tag}"),
            json!([tag, false]),
            ChainError::BlockNotFound { number: u64::MAX },
        )
        .await
    }

    async fn block_by_number(&self, number: u64) -> Result<ChainBlock, ChainError> {
        let blocks = self
            .request_blocks(
                format!("block {number}"),
                json!([quantity_hex(number), false]),
                ChainError::BlockNotFound { number },
            )
            .await?;
        Ok(select_conservative_head(&blocks).block)
    }

    async fn request_blocks(
        &self,
        context: String,
        params: Value,
        not_found: ChainError,
    ) -> Result<Vec<ProviderBlock>, ChainError> {
        let mut blocks = Vec::new();
        let mut errors = Vec::new();
        let mut not_found_count = 0usize;
        for provider in &self.providers {
            match provider
                .request("eth_getBlockByNumber", params.clone())
                .await
            {
                Ok(Value::Null) => {
                    not_found_count += 1;
                    errors.push(format!("{}: block not found", provider.provider_id()))
                }
                Ok(value) => match parse_block(value, provider.provider_id()) {
                    Ok(block) => blocks.push(block),
                    Err(error) => errors.push(format!("{}: {error}", provider.provider_id())),
                },
                Err(ChainError::BlockNotFound { .. }) => return Err(not_found.clone()),
                Err(error) => errors.push(format!("{}: {error}", provider.provider_id())),
            }
        }

        if blocks.is_empty() {
            if not_found_count == self.providers.len() {
                return Err(not_found);
            }
            if errors.is_empty() {
                return Err(ChainError::rpc_unavailable(format!(
                    "no RPC provider returned {context}"
                )));
            }
            return Err(ChainError::rpc_unavailable(format!(
                "no RPC provider returned {context}: {}",
                errors.join("; ")
            )));
        }

        ensure_consistent_block_hashes(&context, &blocks)?;
        Ok(blocks)
    }

    async fn request_first_success(
        &self,
        method: &'static str,
        params: Value,
    ) -> Result<ProviderValue, ChainError> {
        let mut errors = Vec::new();
        for provider in &self.providers {
            match provider.request(method, params.clone()).await {
                Ok(value) => {
                    return Ok(ProviderValue {
                        provider_id: provider.provider_id().to_string(),
                        value,
                    });
                }
                Err(error) => {
                    errors.push(format!("{}: {error}", provider.provider_id()));
                }
            }
        }

        Err(ChainError::rpc_unavailable(format!(
            "all RPC providers failed {method}: {}",
            errors.join("; ")
        )))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RpcProviderChainStatus {
    pub provider_id: String,
    pub chain_id: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RpcProviderReadiness {
    pub expected_chain_id: u64,
    pub providers: Vec<RpcProviderChainStatus>,
    pub latest_head: ChainBlockRef,
    pub safe_head: ChainBlockRef,
    pub finalized_head: ChainBlockRef,
}

#[derive(Clone, Debug)]
struct ProviderValue {
    provider_id: String,
    value: Value,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ProviderBlock {
    provider_id: String,
    block: ChainBlock,
}

#[derive(Clone)]
pub struct RpcRangeSource {
    manager: RpcProviderManager,
}

impl fmt::Debug for RpcRangeSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RpcRangeSource")
            .field("manager", &self.manager)
            .finish()
    }
}

impl RpcRangeSource {
    pub fn new(manager: RpcProviderManager) -> Self {
        Self { manager }
    }

    pub fn from_http_urls(
        expected_chain_id: u64,
        urls: &[String],
        min_provider_count: usize,
    ) -> Result<Self, ChainError> {
        Ok(Self::new(RpcProviderManager::from_http_urls(
            expected_chain_id,
            urls,
            min_provider_count,
        )?))
    }

    pub fn manager(&self) -> &RpcProviderManager {
        &self.manager
    }

    pub async fn readiness_probe(&self) -> Result<RpcProviderReadiness, ChainError> {
        self.manager.readiness_probe().await
    }

    pub async fn ensure_capacity(
        &self,
        range: TransferLogRange,
        limits: TransferLogCapacityLimits,
    ) -> Result<TransferLogCapacityReport, ChainError> {
        let report = self.capacity_probe(range, limits).await?;
        report.ensure_within_limits()?;
        Ok(report)
    }
}

#[async_trait]
impl ChainHeaderReader for RpcRangeSource {
    async fn latest_head(&self) -> Result<ChainBlockRef, ChainError> {
        self.manager.head_by_tag("latest").await
    }

    async fn safe_head(&self) -> Result<ChainBlockRef, ChainError> {
        self.manager.head_by_tag("safe").await
    }

    async fn finalized_head(&self) -> Result<ChainBlockRef, ChainError> {
        self.manager.head_by_tag("finalized").await
    }

    async fn block_by_number(&self, number: u64) -> Result<ChainBlock, ChainError> {
        self.manager.block_by_number(number).await
    }
}

#[async_trait]
impl TransferLogSource for RpcRangeSource {
    async fn transfer_logs(&self, range: TransferLogRange) -> Result<Vec<TransferLog>, ChainError> {
        range.validate()?;
        if range.chain_id != self.manager.expected_chain_id() {
            return Err(ChainError::ChainIdMismatch {
                expected: range.chain_id,
                actual: self.manager.expected_chain_id(),
            });
        }
        let ProviderValue { provider_id, value } = self
            .manager
            .request_first_success("eth_getLogs", transfer_filter(range))
            .await?;
        let raw_logs = value.as_array().ok_or_else(|| {
            ChainError::malformed_rpc_response("eth_getLogs result must be an array")
        })?;

        let mut parsed_logs = Vec::new();
        for raw_log in raw_logs {
            if raw_log.get("removed").and_then(Value::as_bool) == Some(true) {
                continue;
            }
            let parsed = parse_transfer_log(raw_log.clone())?;
            if parsed.token_address != range.token_address {
                continue;
            }
            parsed_logs.push(parsed);
        }

        let mut headers = BTreeMap::new();
        for block_number in parsed_logs.iter().map(|log| log.block_number) {
            if headers.contains_key(&block_number) {
                continue;
            }
            headers.insert(
                block_number,
                self.manager.block_by_number(block_number).await?,
            );
        }

        let mut logs = Vec::with_capacity(parsed_logs.len());
        for parsed in parsed_logs {
            let header = headers.get(&parsed.block_number).ok_or_else(|| {
                ChainError::malformed_rpc_response(format!(
                    "missing header for log block {}",
                    parsed.block_number
                ))
            })?;
            if header.hash != parsed.block_hash {
                return Err(ChainError::ProviderHashMismatch {
                    context: format!("eth_getLogs block {}", parsed.block_number).into(),
                    left_provider: provider_id.clone().into(),
                    left_hash: parsed.block_hash,
                    right_provider: "header_quorum".into(),
                    right_hash: header.hash,
                });
            }
            logs.push(TransferLog {
                chain_id: range.chain_id,
                token_address: parsed.token_address,
                block: *header,
                tx_hash: parsed.tx_hash,
                log_index: parsed.log_index,
                from_address: parsed.from_address,
                to_address: parsed.to_address,
                amount_raw: parsed.amount_raw,
            });
        }

        logs.sort_by_key(TransferLog::position);
        Ok(logs)
    }

    async fn capacity_probe(
        &self,
        range: TransferLogRange,
        limits: TransferLogCapacityLimits,
    ) -> Result<TransferLogCapacityReport, ChainError> {
        let logs = self.transfer_logs(range).await?;
        let mut block_counts = BTreeMap::<u64, usize>::new();
        for log in &logs {
            *block_counts.entry(log.block.number).or_default() += 1;
        }

        Ok(TransferLogCapacityReport {
            range,
            log_count: logs.len(),
            max_logs_in_single_block: block_counts.values().copied().max().unwrap_or_default(),
            limits,
        })
    }
}

#[async_trait]
impl NativeBalanceReader for RpcRangeSource {
    async fn native_balance(
        &self,
        chain_id: u64,
        owner: EvmAddress,
    ) -> Result<RawAmount, ChainError> {
        if chain_id != self.manager.expected_chain_id() {
            return Err(ChainError::ChainIdMismatch {
                expected: chain_id,
                actual: self.manager.expected_chain_id(),
            });
        }

        let ProviderValue { value, .. } = self
            .manager
            .request_first_success("eth_getBalance", json!([owner.to_string(), "latest"]))
            .await?;
        let value = value.as_str().ok_or_else(|| {
            ChainError::malformed_rpc_response("eth_getBalance result must be a hex string")
        })?;
        parse_hex_u256(value, "eth_getBalance result").map(RawAmount::new)
    }
}

#[async_trait]
impl Erc20ChainClient for RpcRangeSource {
    async fn token_balance(
        &self,
        token: EvmAddress,
        owner: EvmAddress,
    ) -> Result<RawAmount, ChainError> {
        let ProviderValue { value, .. } = self
            .manager
            .request_first_success("eth_call", balance_of_call(token, owner))
            .await?;
        let value = value.as_str().ok_or_else(|| {
            ChainError::malformed_rpc_response("eth_call balanceOf result must be a hex string")
        })?;
        parse_hex_u256(value, "eth_call balanceOf result").map(RawAmount::new)
    }

    async fn transaction_receipt(&self, tx: TxHash) -> Result<Option<TxReceipt>, ChainError> {
        let ProviderValue { value, .. } = self
            .manager
            .request_first_success("eth_getTransactionReceipt", json!([tx.to_string()]))
            .await?;
        if value.is_null() {
            return Ok(None);
        }
        parse_receipt(value).map(Some)
    }

    async fn broadcast_signed_tx(&self, signed_tx: Vec<u8>) -> Result<TxHash, ChainError> {
        let ProviderValue { value, .. } = self
            .manager
            .request_first_success("eth_sendRawTransaction", json!([encode_hex(&signed_tx)]))
            .await?;
        let tx = value.as_str().ok_or_else(|| {
            ChainError::malformed_rpc_response("eth_sendRawTransaction result must be a tx hash")
        })?;
        parse_tx_hash(tx, "eth_sendRawTransaction result")
    }
}

fn transfer_filter(range: TransferLogRange) -> Value {
    json!([{
        "address": range.token_address.to_string(),
        "fromBlock": quantity_hex(range.from_block),
        "toBlock": quantity_hex(range.to_block),
        "topics": [ERC20_TRANSFER_TOPIC],
    }])
}

fn balance_of_call(token: EvmAddress, owner: EvmAddress) -> Value {
    let mut data = String::from("0x70a08231");
    data.push_str("000000000000000000000000");
    data.push_str(owner.to_lower_hex().trim_start_matches("0x"));
    json!([{
        "to": token.to_string(),
        "data": data,
    }, "latest"])
}

fn select_conservative_head(blocks: &[ProviderBlock]) -> ProviderBlock {
    blocks
        .iter()
        .min_by_key(|provider_block| provider_block.block.number)
        .expect("block list must not be empty")
        .clone()
}

fn ensure_consistent_block_hashes(
    context: &str,
    blocks: &[ProviderBlock],
) -> Result<(), ChainError> {
    let mut hashes = BTreeMap::<u64, ProviderBlock>::new();
    for block in blocks {
        if let Some(existing) = hashes.get(&block.block.number) {
            if existing.block.hash != block.block.hash {
                return Err(ChainError::ProviderHashMismatch {
                    context: context.into(),
                    left_provider: existing.provider_id.clone().into(),
                    left_hash: existing.block.hash,
                    right_provider: block.provider_id.clone().into(),
                    right_hash: block.block.hash,
                });
            }
        } else {
            hashes.insert(block.block.number, block.clone());
        }
    }
    Ok(())
}

fn parse_block(value: Value, provider_id: &str) -> Result<ProviderBlock, ChainError> {
    let block: RpcBlock = serde_json::from_value(value)
        .map_err(|error| ChainError::malformed_rpc_response(error.to_string()))?;
    let number = parse_required_hex_u64(block.number.as_deref(), "block.number")?;
    let hash = parse_required_block_hash(block.hash.as_deref(), "block.hash")?;
    let parent_hash = parse_required_block_hash(block.parent_hash.as_deref(), "block.parentHash")?;
    let timestamp = parse_required_hex_u64(block.timestamp.as_deref(), "block.timestamp")?;
    let timestamp = time::OffsetDateTime::from_unix_timestamp(timestamp as i64)
        .map_err(|error| ChainError::malformed_rpc_response(error.to_string()))?;
    Ok(ProviderBlock {
        provider_id: provider_id.to_string(),
        block: ChainBlock::new(number, hash, parent_hash, timestamp),
    })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RpcBlock {
    number: Option<String>,
    hash: Option<String>,
    parent_hash: Option<String>,
    timestamp: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ParsedTransferLog {
    token_address: EvmAddress,
    block_number: u64,
    block_hash: BlockHash,
    tx_hash: TxHash,
    log_index: u64,
    from_address: EvmAddress,
    to_address: EvmAddress,
    amount_raw: RawAmount,
}

fn parse_transfer_log(value: Value) -> Result<ParsedTransferLog, ChainError> {
    let log: RpcLog = serde_json::from_value(value)
        .map_err(|error| ChainError::malformed_rpc_response(error.to_string()))?;
    let token_address = parse_address(&log.address, "log.address")?;
    let block_number = parse_required_hex_u64(log.block_number.as_deref(), "log.blockNumber")?;
    let block_hash = parse_required_block_hash(log.block_hash.as_deref(), "log.blockHash")?;
    let tx_hash = parse_required_tx_hash(log.transaction_hash.as_deref(), "log.transactionHash")?;
    let log_index = parse_required_hex_u64(log.log_index.as_deref(), "log.logIndex")?;
    let topics = log.topics;
    if topics.len() < 3 {
        return Err(ChainError::malformed_rpc_response(format!(
            "ERC20 Transfer log has {} topics",
            topics.len()
        )));
    }
    if !topics[0].eq_ignore_ascii_case(ERC20_TRANSFER_TOPIC) {
        return Err(ChainError::malformed_rpc_response(
            "eth_getLogs returned a non-Transfer event",
        ));
    }

    Ok(ParsedTransferLog {
        token_address,
        block_number,
        block_hash,
        tx_hash,
        log_index,
        from_address: parse_topic_address(&topics[1], "log.topics[1]")?,
        to_address: parse_topic_address(&topics[2], "log.topics[2]")?,
        amount_raw: RawAmount::new(parse_hex_u256(&log.data, "log.data")?),
    })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RpcLog {
    address: String,
    topics: Vec<String>,
    data: String,
    block_number: Option<String>,
    block_hash: Option<String>,
    transaction_hash: Option<String>,
    log_index: Option<String>,
}

fn parse_receipt(value: Value) -> Result<TxReceipt, ChainError> {
    let receipt: RpcReceipt = serde_json::from_value(value)
        .map_err(|error| ChainError::malformed_rpc_response(error.to_string()))?;
    let tx_hash = parse_tx_hash(&receipt.transaction_hash, "receipt.transactionHash")?;
    let block_number =
        parse_required_hex_u64(receipt.block_number.as_deref(), "receipt.blockNumber")?;
    let block_hash = parse_required_block_hash(receipt.block_hash.as_deref(), "receipt.blockHash")?;
    let status = match receipt.status.as_deref() {
        Some("0x1") | Some("0X1") => TransactionStatus::Success,
        Some("0x0") | Some("0X0") => TransactionStatus::Reverted,
        Some(value) => {
            return Err(ChainError::malformed_rpc_response(format!(
                "unknown receipt.status {value}"
            )));
        }
        None => {
            return Err(ChainError::malformed_rpc_response(
                "receipt.status is missing",
            ));
        }
    };
    let gas_used = match receipt.gas_used.as_deref() {
        Some(value) => Some(parse_hex_u64(value, "receipt.gasUsed")?),
        None => None,
    };

    Ok(TxReceipt {
        tx_hash,
        block: ChainBlockRef::new(block_number, block_hash),
        status,
        gas_used,
    })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RpcReceipt {
    transaction_hash: String,
    block_number: Option<String>,
    block_hash: Option<String>,
    status: Option<String>,
    gas_used: Option<String>,
}

fn quantity_hex(value: u64) -> String {
    format!("0x{value:x}")
}

fn parse_required_hex_u64(value: Option<&str>, field: &'static str) -> Result<u64, ChainError> {
    let value = value.ok_or_else(|| {
        ChainError::malformed_rpc_response(format!("{field} is missing from RPC response"))
    })?;
    parse_hex_u64(value, field)
}

fn parse_hex_u64(value: &str, field: &'static str) -> Result<u64, ChainError> {
    let hex = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"));
    let hex = hex.ok_or_else(|| {
        ChainError::malformed_rpc_response(format!("{field} must be a 0x-prefixed hex quantity"))
    })?;
    if hex.is_empty() {
        return Ok(0);
    }
    if hex.len() > 16 {
        return Err(ChainError::malformed_rpc_response(format!(
            "{field} overflows u64"
        )));
    }
    u64::from_str_radix(hex, 16)
        .map_err(|_| ChainError::malformed_rpc_response(format!("{field} contains invalid hex")))
}

fn parse_hex_u256(value: &str, field: &'static str) -> Result<U256, ChainError> {
    let bytes = decode_prefixed_hex(value, field, true)?;
    if bytes.len() > 32 {
        return Err(ChainError::malformed_rpc_response(format!(
            "{field} overflows uint256"
        )));
    }
    U256::try_from_be_slice(&bytes)
        .ok_or_else(|| ChainError::malformed_rpc_response(format!("{field} overflows uint256")))
}

fn parse_required_block_hash(
    value: Option<&str>,
    field: &'static str,
) -> Result<BlockHash, ChainError> {
    let value = value.ok_or_else(|| {
        ChainError::malformed_rpc_response(format!("{field} is missing from RPC response"))
    })?;
    parse_block_hash(value, field)
}

fn parse_block_hash(value: &str, field: &'static str) -> Result<BlockHash, ChainError> {
    value
        .parse::<BlockHash>()
        .map_err(|error| ChainError::malformed_rpc_response(format!("{field}: {error}")))
}

fn parse_required_tx_hash(value: Option<&str>, field: &'static str) -> Result<TxHash, ChainError> {
    let value = value.ok_or_else(|| {
        ChainError::malformed_rpc_response(format!("{field} is missing from RPC response"))
    })?;
    parse_tx_hash(value, field)
}

fn parse_tx_hash(value: &str, field: &'static str) -> Result<TxHash, ChainError> {
    value
        .parse::<TxHash>()
        .map_err(|error| ChainError::malformed_rpc_response(format!("{field}: {error}")))
}

fn parse_address(value: &str, field: &'static str) -> Result<EvmAddress, ChainError> {
    value
        .parse::<EvmAddress>()
        .map_err(|error| ChainError::malformed_rpc_response(format!("{field}: {error}")))
}

fn parse_topic_address(value: &str, field: &'static str) -> Result<EvmAddress, ChainError> {
    let hex = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"));
    let hex = hex.ok_or_else(|| {
        ChainError::malformed_rpc_response(format!("{field} must be a 0x-prefixed topic"))
    })?;
    if hex.len() != 64 {
        return Err(ChainError::malformed_rpc_response(format!(
            "{field} must contain 64 hex chars"
        )));
    }
    parse_address(&format!("0x{}", &hex[24..]), field)
}

fn decode_prefixed_hex(
    value: &str,
    field: &'static str,
    allow_odd_nibbles: bool,
) -> Result<Vec<u8>, ChainError> {
    let hex = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"));
    let mut hex = hex
        .ok_or_else(|| {
            ChainError::malformed_rpc_response(format!("{field} must be 0x-prefixed hex"))
        })?
        .to_string();
    if hex.is_empty() {
        return Ok(Vec::new());
    }
    if hex.len() % 2 != 0 {
        if allow_odd_nibbles {
            hex.insert(0, '0');
        } else {
            return Err(ChainError::malformed_rpc_response(format!(
                "{field} has odd-length hex data"
            )));
        }
    }

    let mut bytes = Vec::with_capacity(hex.len() / 2);
    for (index, chunk) in hex.as_bytes().chunks_exact(2).enumerate() {
        let high = hex_value(chunk[0]).ok_or_else(|| {
            ChainError::malformed_rpc_response(format!(
                "{field} contains invalid hex at char {}",
                index * 2
            ))
        })?;
        let low = hex_value(chunk[1]).ok_or_else(|| {
            ChainError::malformed_rpc_response(format!(
                "{field} contains invalid hex at char {}",
                index * 2 + 1
            ))
        })?;
        bytes.push((high << 4) | low);
    }
    Ok(bytes)
}

fn encode_hex(bytes: &[u8]) -> String {
    const TABLE: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(2 + bytes.len() * 2);
    output.push_str("0x");
    for byte in bytes {
        output.push(TABLE[(byte >> 4) as usize] as char);
        output.push(TABLE[(byte & 0x0f) as usize] as char);
    }
    output
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeMap, BTreeSet},
        sync::{Arc, Mutex},
    };

    use serde_json::json;
    use time::macros::datetime;

    use super::*;

    #[tokio::test]
    async fn provider_manager_validates_chain_ids_and_detects_hash_mismatch() {
        let p1 = Arc::new(FakeRpcProvider::new("provider-1", 1).with_block(block(10, 0xaa)));
        let p2 = Arc::new(FakeRpcProvider::new("provider-2", 1).with_block(block(10, 0xbb)));
        let manager = RpcProviderManager::with_min_provider_count(1, vec![p1, p2], 2).unwrap();

        assert_eq!(manager.validate_chain_ids().await.unwrap().len(), 2);
        assert!(matches!(
            manager.block_by_number(10).await,
            Err(ChainError::ProviderHashMismatch { .. })
        ));

        let bad_chain = Arc::new(FakeRpcProvider::new("provider-3", 2).with_block(block(10, 0xaa)));
        let manager = RpcProviderManager::new(1, vec![bad_chain]).unwrap();
        assert!(matches!(
            manager.validate_chain_ids().await,
            Err(ChainError::ChainIdMismatch {
                expected: 1,
                actual: 2
            })
        ));

        let good_chain =
            Arc::new(FakeRpcProvider::new("provider-4", 1).with_block(block(10, 0xaa)));
        let source = RpcRangeSource::new(RpcProviderManager::new(1, vec![good_chain]).unwrap());
        assert!(matches!(
            source
                .transfer_logs(TransferLogRange::new(2, address(0x11), 10, 10))
                .await,
            Err(ChainError::ChainIdMismatch {
                expected: 2,
                actual: 1
            })
        ));
    }

    #[tokio::test]
    async fn rpc_range_source_parses_transfer_logs_and_capacity_gate() {
        let token = address(0x11);
        let from = address(0x22);
        let to = address(0x33);
        let p1 = Arc::new(
            FakeRpcProvider::new("provider-1", 1)
                .with_block(block(10, 0xaa))
                .with_log(rpc_log(token, from, to, 10, 0xaa, 0, 42))
                .with_log(rpc_log(token, from, to, 10, 0xaa, 1, 43)),
        );
        let p2 = Arc::new(FakeRpcProvider::new("provider-2", 1).with_block(block(10, 0xaa)));
        let source = source(vec![p1, p2]);

        let logs = source
            .transfer_logs(TransferLogRange::new(1, token, 10, 10))
            .await
            .unwrap();

        assert_eq!(logs.len(), 2);
        assert_eq!(logs[0].from_address, from);
        assert_eq!(logs[0].to_address, to);
        assert_eq!(logs[0].amount_raw, RawAmount::from(42));
        assert_eq!(logs[0].block.hash, block_hash(0xaa));

        let report = source
            .capacity_probe(
                TransferLogRange::new(1, token, 10, 10),
                TransferLogCapacityLimits {
                    max_logs: 10,
                    max_logs_per_block: 1,
                },
            )
            .await
            .unwrap();
        assert_eq!(report.log_count, 2);
        assert_eq!(report.max_logs_in_single_block, 2);
        assert!(
            source
                .ensure_capacity(
                    TransferLogRange::new(1, token, 10, 10),
                    TransferLogCapacityLimits {
                        max_logs: 10,
                        max_logs_per_block: 1,
                    },
                )
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn rpc_range_source_fails_over_provider_errors() {
        let token = address(0x11);
        let p1 = Arc::new(
            FakeRpcProvider::new("provider-1", 1)
                .with_block(block(10, 0xaa))
                .fail_method("eth_getLogs"),
        );
        let p2 = Arc::new(
            FakeRpcProvider::new("provider-2", 1)
                .with_block(block(10, 0xaa))
                .with_log(rpc_log(token, address(0x22), address(0x33), 10, 0xaa, 0, 7)),
        );
        let source = source(vec![p1.clone(), p2.clone()]);

        let logs = source
            .transfer_logs(TransferLogRange::new(1, token, 10, 10))
            .await
            .unwrap();

        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].amount_raw, RawAmount::from(7));
        assert!(p1.calls().iter().any(|call| call == "eth_getLogs"));
        assert!(p2.calls().iter().any(|call| call == "eth_getLogs"));
    }

    #[tokio::test]
    async fn rpc_range_source_supports_balance_receipt_and_broadcast() {
        let token = address(0x11);
        let owner = address(0x22);
        let tx = tx_hash(7);
        let p1 = Arc::new(
            FakeRpcProvider::new("provider-1", 1)
                .with_native_balance(1_000_000)
                .with_balance(99)
                .with_receipt(receipt_json(tx, 10, 0xaa, "0x1", 21_000))
                .with_broadcast(tx_hash(8)),
        );
        let p2 = Arc::new(FakeRpcProvider::new("provider-2", 1));
        let source = source(vec![p1, p2]);

        assert_eq!(
            source.native_balance(1, owner).await.unwrap(),
            RawAmount::from(1_000_000)
        );
        assert_eq!(
            source.token_balance(token, owner).await.unwrap(),
            RawAmount::from(99)
        );
        assert_eq!(
            source.transaction_receipt(tx).await.unwrap(),
            Some(TxReceipt {
                tx_hash: tx,
                block: ChainBlockRef::new(10, block_hash(0xaa)),
                status: TransactionStatus::Success,
                gas_used: Some(21_000),
            })
        );
        assert_eq!(
            source.broadcast_signed_tx(vec![0xde, 0xad]).await.unwrap(),
            tx_hash(8)
        );
    }

    fn source(providers: Vec<Arc<FakeRpcProvider>>) -> RpcRangeSource {
        let providers = providers
            .into_iter()
            .map(|provider| provider as SharedJsonRpcProvider)
            .collect();
        RpcRangeSource::new(RpcProviderManager::with_min_provider_count(1, providers, 2).unwrap())
    }

    #[derive(Debug)]
    struct FakeRpcProvider {
        id: String,
        chain_id: u64,
        blocks: BTreeMap<u64, ChainBlock>,
        logs: Vec<Value>,
        native_balance: Option<Value>,
        balance: Option<Value>,
        receipt: Option<Value>,
        broadcast: Option<Value>,
        failures: BTreeSet<&'static str>,
        calls: Mutex<Vec<String>>,
    }

    impl FakeRpcProvider {
        fn new(id: &str, chain_id: u64) -> Self {
            Self {
                id: id.to_string(),
                chain_id,
                blocks: BTreeMap::new(),
                logs: Vec::new(),
                native_balance: None,
                balance: None,
                receipt: None,
                broadcast: None,
                failures: BTreeSet::new(),
                calls: Mutex::new(Vec::new()),
            }
        }

        fn with_block(mut self, block: ChainBlock) -> Self {
            self.blocks.insert(block.number, block);
            self
        }

        fn with_log(mut self, log: Value) -> Self {
            self.logs.push(log);
            self
        }

        fn with_balance(mut self, balance: u64) -> Self {
            self.balance = Some(Value::String(uint256_hex(balance)));
            self
        }

        fn with_native_balance(mut self, balance: u64) -> Self {
            self.native_balance = Some(Value::String(uint256_hex(balance)));
            self
        }

        fn with_receipt(mut self, receipt: Value) -> Self {
            self.receipt = Some(receipt);
            self
        }

        fn with_broadcast(mut self, tx_hash: TxHash) -> Self {
            self.broadcast = Some(Value::String(tx_hash.to_string()));
            self
        }

        fn fail_method(mut self, method: &'static str) -> Self {
            self.failures.insert(method);
            self
        }

        fn calls(&self) -> Vec<String> {
            self.calls
                .lock()
                .expect("fake RPC calls lock poisoned")
                .clone()
        }
    }

    #[async_trait]
    impl JsonRpcProvider for FakeRpcProvider {
        fn provider_id(&self) -> &str {
            &self.id
        }

        async fn request(&self, method: &str, params: Value) -> Result<Value, ChainError> {
            self.calls
                .lock()
                .expect("fake RPC calls lock poisoned")
                .push(method.to_string());
            if self.failures.contains(method) {
                return Err(ChainError::rpc_unavailable(format!("{method} failed")));
            }

            match method {
                "eth_chainId" => Ok(Value::String(quantity_hex(self.chain_id))),
                "eth_getBlockByNumber" => {
                    let tag = params
                        .as_array()
                        .and_then(|params| params.first())
                        .and_then(Value::as_str)
                        .ok_or_else(|| ChainError::malformed_rpc_response("missing block tag"))?;
                    let block = match tag {
                        "latest" | "safe" | "finalized" => self.blocks.values().last().copied(),
                        value => {
                            let number = parse_hex_u64(value, "block number")?;
                            self.blocks.get(&number).copied()
                        }
                    };
                    Ok(block.map(block_json).unwrap_or(Value::Null))
                }
                "eth_getLogs" => Ok(Value::Array(self.logs.clone())),
                "eth_getBalance" => Ok(self
                    .native_balance
                    .clone()
                    .unwrap_or_else(|| Value::String("0x0".to_string()))),
                "eth_call" => Ok(self
                    .balance
                    .clone()
                    .unwrap_or_else(|| Value::String("0x0".to_string()))),
                "eth_getTransactionReceipt" => Ok(self.receipt.clone().unwrap_or(Value::Null)),
                "eth_sendRawTransaction" => self
                    .broadcast
                    .clone()
                    .ok_or_else(|| ChainError::rpc_unavailable("missing fake broadcast result")),
                other => Err(ChainError::rpc_unavailable(format!(
                    "unsupported fake method {other}"
                ))),
            }
        }
    }

    fn block_json(block: ChainBlock) -> Value {
        json!({
            "number": quantity_hex(block.number),
            "hash": block.hash.to_string(),
            "parentHash": block.parent_hash.to_string(),
            "timestamp": quantity_hex(block.timestamp.unix_timestamp() as u64),
        })
    }

    fn rpc_log(
        token: EvmAddress,
        from: EvmAddress,
        to: EvmAddress,
        block_number: u64,
        block_hash_byte: u8,
        log_index: u64,
        amount: u64,
    ) -> Value {
        json!({
            "address": token.to_string(),
            "topics": [
                ERC20_TRANSFER_TOPIC,
                topic_address(from),
                topic_address(to),
            ],
            "data": uint256_hex(amount),
            "blockNumber": quantity_hex(block_number),
            "blockHash": block_hash(block_hash_byte).to_string(),
            "transactionHash": tx_hash(block_number * 100 + log_index).to_string(),
            "logIndex": quantity_hex(log_index),
            "removed": false,
        })
    }

    fn receipt_json(
        tx_hash: TxHash,
        block_number: u64,
        block_hash_byte: u8,
        status: &str,
        gas_used: u64,
    ) -> Value {
        json!({
            "transactionHash": tx_hash.to_string(),
            "blockNumber": quantity_hex(block_number),
            "blockHash": block_hash(block_hash_byte).to_string(),
            "status": status,
            "gasUsed": quantity_hex(gas_used),
        })
    }

    fn topic_address(address: EvmAddress) -> String {
        format!(
            "0x000000000000000000000000{}",
            address.to_lower_hex().trim_start_matches("0x")
        )
    }

    fn uint256_hex(value: u64) -> String {
        format!("0x{value:064x}")
    }

    fn block(number: u64, hash_byte: u8) -> ChainBlock {
        ChainBlock::new(
            number,
            block_hash(hash_byte),
            block_hash(hash_byte.saturating_sub(1)),
            datetime!(2026-05-03 00:00:00 UTC) + time::Duration::seconds(number as i64),
        )
    }

    fn address(byte: u8) -> EvmAddress {
        EvmAddress::from_bytes([byte; 20])
    }

    fn tx_hash(value: u64) -> TxHash {
        let mut bytes = [0u8; 32];
        bytes[24..].copy_from_slice(&value.to_be_bytes());
        TxHash::from_bytes(bytes)
    }

    fn block_hash(byte: u8) -> BlockHash {
        BlockHash::from_bytes([byte; 32])
    }
}
