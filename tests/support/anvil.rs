use std::{
    error::Error,
    fmt, fs,
    net::TcpListener,
    process::{Child, Command, Stdio},
    time::Duration,
};

use alloy_primitives::{U256, keccak256};
use async_trait::async_trait;
use reqwest::Client;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::time::sleep;

use pay3::{
    domain::{EvmAddress, RawAmount, TxHash},
    signer::{SignedTx, SignerError, SignerProvider, UnsignedTx},
};

pub type AnyError = Box<dyn Error + Send + Sync>;

pub const DEFAULT_ANVIL_MNEMONIC: &str =
    "test test test test test test test test test test test junk";
pub const DEFAULT_CHAIN_ID: u64 = 31_337;
pub const CHILD_PATH: &str = "m/44'/60'/0'/0/0";
pub const DEPLOYER_PATH: &str = "m/44'/60'/0'/0/1";
pub const TREASURY_PATH: &str = "m/44'/60'/0'/0/2";

#[derive(Debug)]
struct HelperError(String);

impl fmt::Display for HelperError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Error for HelperError {}

fn helper_error(message: impl Into<String>) -> AnyError {
    Box::new(HelperError(message.into()))
}

async fn rpc_request(rpc_url: &str, method: &str, params: Value) -> Result<Value, AnyError> {
    let client = Client::new();
    let response = client
        .post(rpc_url)
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        }))
        .send()
        .await
        .map_err(|error| helper_error(format!("{method} request failed: {error}")))?;

    if !response.status().is_success() {
        return Err(helper_error(format!(
            "{method} returned HTTP {}",
            response.status()
        )));
    }

    let payload = response
        .json::<Value>()
        .await
        .map_err(|error| helper_error(format!("{method} json: {error}")))?;
    if let Some(error) = payload.get("error") {
        return Err(helper_error(format!(
            "{method} returned RPC error: {error}"
        )));
    }

    Ok(payload)
}

#[derive(Debug)]
pub struct AnvilHarness {
    child: Child,
    rpc_url: String,
    chain_id: u64,
    mnemonic: String,
}

impl AnvilHarness {
    pub async fn start() -> Result<Self, AnyError> {
        let port = pick_free_port()?;
        let rpc_url = format!("http://127.0.0.1:{port}");
        let mut command = Command::new("anvil");
        command
            .arg("--mnemonic")
            .arg(DEFAULT_ANVIL_MNEMONIC)
            .arg("--chain-id")
            .arg(DEFAULT_CHAIN_ID.to_string())
            .arg("--port")
            .arg(port.to_string())
            .arg("--accounts")
            .arg("10")
            .arg("--balance")
            .arg("10000")
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        let child = command
            .spawn()
            .map_err(|error| helper_error(format!("failed to spawn anvil: {error}")))?;

        let harness = Self {
            child,
            rpc_url,
            chain_id: DEFAULT_CHAIN_ID,
            mnemonic: DEFAULT_ANVIL_MNEMONIC.to_string(),
        };
        harness.wait_until_ready().await?;
        Ok(harness)
    }

    pub fn rpc_url(&self) -> &str {
        &self.rpc_url
    }

    pub fn chain_id(&self) -> u64 {
        self.chain_id
    }

    pub fn mnemonic(&self) -> &str {
        &self.mnemonic
    }

    pub async fn set_automine(&self, automine: bool) -> Result<(), AnyError> {
        let _ = rpc_request(&self.rpc_url, "evm_setAutomine", json!([automine])).await?;
        Ok(())
    }

    pub async fn mine_block(&self) -> Result<(), AnyError> {
        let _ = rpc_request(&self.rpc_url, "evm_mine", json!([])).await?;
        Ok(())
    }

    pub async fn derive_address(&self, derivation_path: &str) -> Result<EvmAddress, AnyError> {
        derive_address_from_mnemonic(&self.mnemonic, derivation_path)
            .await
            .map_err(|error| helper_error(format!("failed to derive address: {error}")))
    }

    async fn wait_until_ready(&self) -> Result<(), AnyError> {
        let client = Client::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(30);

        loop {
            if std::time::Instant::now() > deadline {
                return Err(helper_error("anvil did not become ready in time"));
            }

            let result = client
                .post(&self.rpc_url)
                .json(&json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "eth_chainId",
                    "params": [],
                }))
                .send()
                .await;

            if let Ok(response) = result {
                if response.status().is_success() {
                    let payload = response.json::<Value>().await;
                    if let Ok(payload) = payload {
                        if payload.get("result").and_then(Value::as_str).is_some() {
                            return Ok(());
                        }
                    }
                }
            }

            sleep(Duration::from_millis(100)).await;
        }
    }
}

impl Drop for AnvilHarness {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[derive(Clone, Debug)]
pub struct AnvilMnemonicDeriver {
    mnemonic: String,
}

impl AnvilMnemonicDeriver {
    pub fn new(mnemonic: impl Into<String>) -> Result<Self, AnyError> {
        let mnemonic = mnemonic.into();
        if mnemonic.trim().is_empty() {
            return Err(helper_error("mnemonic must not be empty"));
        }
        Ok(Self { mnemonic })
    }
}

#[async_trait]
impl pay3::wallet::AddressDeriver for AnvilMnemonicDeriver {
    async fn derive_address(
        &self,
        key_ref: &str,
        path: &str,
    ) -> Result<EvmAddress, pay3::wallet::WalletError> {
        if key_ref.trim().is_empty() {
            return Err(pay3::wallet::WalletError::EmptySignerKeyRef);
        }
        derive_address_from_mnemonic(&self.mnemonic, path)
            .await
            .map_err(|_| pay3::wallet::WalletError::InvalidDerivationPath {
                path: path.to_string(),
            })
    }
}

#[derive(Clone, Debug)]
pub struct AnvilMnemonicSigner {
    mnemonic: String,
    chain_id: u64,
}

impl AnvilMnemonicSigner {
    pub fn new(mnemonic: impl Into<String>, chain_id: u64) -> Result<Self, SignerError> {
        let mnemonic = mnemonic.into();
        if mnemonic.trim().is_empty() {
            return Err(SignerError::EmptySignerKeyRef);
        }
        if chain_id == 0 {
            return Err(SignerError::HealthCheckFailed {
                message: "chain id must be greater than zero".to_string(),
            });
        }
        Ok(Self { mnemonic, chain_id })
    }
}

#[async_trait]
impl SignerProvider for AnvilMnemonicSigner {
    async fn derive_address(&self, key_ref: &str, path: &str) -> Result<EvmAddress, SignerError> {
        if key_ref.trim().is_empty() {
            return Err(SignerError::EmptySignerKeyRef);
        }
        derive_address_from_mnemonic(&self.mnemonic, path)
            .await
            .map_err(|_| SignerError::InvalidDerivationPath {
                path: path.to_string(),
            })
    }

    async fn sign_transaction(
        &self,
        key_ref: &str,
        path: &str,
        tx: UnsignedTx,
    ) -> Result<SignedTx, SignerError> {
        if key_ref.trim().is_empty() {
            return Err(SignerError::EmptySignerKeyRef);
        }

        let from = self.derive_address(key_ref, path).await?;
        let private_key = private_key_for_mnemonic(&self.mnemonic, path)
            .await
            .map_err(|error| SignerError::HealthCheckFailed {
                message: format!("failed to derive signing key: {error}"),
            })?;
        let raw_tx = sign_unsigned_tx_with_cast(&private_key, self.chain_id, &tx)
            .await
            .map_err(|error| SignerError::HealthCheckFailed {
                message: format!("failed to sign transaction: {error}"),
            })?;
        let tx_hash = TxHash::from_alloy(keccak256(&raw_tx));

        Ok(SignedTx {
            request_id: tx.request_id,
            chain_id: tx.chain_id,
            nonce: tx.nonce,
            from,
            to: tx.to,
            tx_hash,
            raw_tx,
        })
    }

    async fn health_check(&self) -> Result<(), SignerError> {
        Ok(())
    }
}

pub async fn derive_address_from_mnemonic(
    mnemonic: &str,
    derivation_path: &str,
) -> Result<EvmAddress, AnyError> {
    let output = run_cast_command([
        "wallet",
        "address",
        "-q",
        "--mnemonic",
        mnemonic,
        "--mnemonic-derivation-path",
        derivation_path,
    ])
    .await?;
    output
        .trim()
        .parse::<EvmAddress>()
        .map_err(|error| helper_error(format!("invalid derived address {output:?}: {error}")))
}

pub async fn deploy_mock_erc20(
    rpc_url: &str,
    deployer_address: EvmAddress,
    initial_holder: EvmAddress,
    initial_supply: RawAmount,
) -> Result<EvmAddress, AnyError> {
    let project = TempDir::new().map_err(|error| helper_error(format!("tempdir: {error}")))?;
    let source_dir = project.path().join("src");
    fs::create_dir_all(&source_dir)
        .map_err(|error| helper_error(format!("create source dir: {error}")))?;
    fs::write(project.path().join("foundry.toml"), foundry_toml())
        .map_err(|error| helper_error(format!("write foundry.toml: {error}")))?;
    fs::write(source_dir.join("MockERC20.sol"), mock_erc20_source())
        .map_err(|error| helper_error(format!("write mock erc20: {error}")))?;

    let output = Command::new("forge")
        .arg("create")
        .arg("--json")
        .arg("--broadcast")
        .arg("--unlocked")
        .arg("--rpc-url")
        .arg(rpc_url)
        .arg("--from")
        .arg(deployer_address.to_string())
        .arg("--root")
        .arg(project.path())
        .arg("--contracts")
        .arg("src")
        .arg("src/MockERC20.sol:MockERC20")
        .arg("--legacy")
        .arg("--gas-price")
        .arg("10gwei")
        .arg("--constructor-args")
        .arg(initial_holder.to_string())
        .arg(initial_supply.to_string())
        .output()
        .map_err(|error| helper_error(format!("failed to spawn forge create: {error}")))?;

    if !output.status.success() {
        return Err(helper_error(format!(
            "forge create failed: {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )));
    }

    let payload = String::from_utf8(output.stdout)
        .map_err(|error| helper_error(format!("forge stdout utf8: {error}")))?;
    let json: Value = serde_json::from_str(&payload)
        .map_err(|error| helper_error(format!("forge json output: {error}\n{payload}")))?;
    let deployed_to = json
        .get("deployedTo")
        .and_then(Value::as_str)
        .or_else(|| json.get("deployed_to").and_then(Value::as_str))
        .ok_or_else(|| {
            helper_error(format!(
                "forge create output did not include deployedTo: {payload}"
            ))
        })?;
    deployed_to
        .parse::<EvmAddress>()
        .map_err(|error| helper_error(format!("invalid deployed address {deployed_to}: {error}")))
}

pub async fn send_erc20_transfer(
    rpc_url: &str,
    mnemonic: &str,
    from_path: &str,
    token_address: EvmAddress,
    recipient: EvmAddress,
    amount: RawAmount,
    gas_limit: u64,
    gas_price: RawAmount,
) -> Result<TxHash, AnyError> {
    let from = derive_address_from_mnemonic(mnemonic, from_path).await?;
    let nonce = current_nonce(rpc_url, from).await?;
    let private_key = private_key_for_mnemonic(mnemonic, from_path).await?;
    let raw_tx = sign_erc20_transfer_with_cast(
        &private_key,
        DEFAULT_CHAIN_ID,
        token_address,
        recipient,
        amount,
        gas_limit,
        gas_price,
        nonce,
    )
    .await?;
    let tx_hash = send_raw_transaction(rpc_url, &raw_tx).await?;
    Ok(tx_hash)
}

async fn sign_unsigned_tx_with_cast(
    private_key: &str,
    chain_id: u64,
    tx: &UnsignedTx,
) -> Result<Vec<u8>, AnyError> {
    let call = decode_erc20_transfer_call(&tx.data)?;
    sign_erc20_transfer_with_cast(
        private_key,
        chain_id,
        tx.to,
        call.recipient,
        call.amount,
        tx.gas_limit,
        tx.max_fee_per_gas,
        tx.nonce,
    )
    .await
}

struct DecodedTransferCall {
    recipient: EvmAddress,
    amount: RawAmount,
}

fn decode_erc20_transfer_call(data: &[u8]) -> Result<DecodedTransferCall, AnyError> {
    if data.len() != 68 {
        return Err(helper_error(format!(
            "unsupported calldata length {}, expected ERC20 transfer",
            data.len()
        )));
    }
    if data[..4] != [0xa9, 0x05, 0x9c, 0xbb] {
        return Err(helper_error("unsupported calldata selector"));
    }

    let mut recipient = [0u8; 20];
    recipient.copy_from_slice(&data[16..36]);
    let amount = U256::try_from_be_slice(&data[36..68])
        .ok_or_else(|| helper_error("failed to parse ERC20 transfer amount from calldata"))?;

    Ok(DecodedTransferCall {
        recipient: EvmAddress::from_bytes(recipient),
        amount: RawAmount::new(amount),
    })
}

async fn sign_erc20_transfer_with_cast(
    private_key: &str,
    chain_id: u64,
    token_address: EvmAddress,
    recipient: EvmAddress,
    amount: RawAmount,
    gas_limit: u64,
    gas_price: RawAmount,
    nonce: u64,
) -> Result<Vec<u8>, AnyError> {
    let chain_id = chain_id.to_string();
    let token_address = token_address.to_string();
    let recipient = recipient.to_string();
    let amount = amount.to_string();
    let nonce = nonce.to_string();
    let gas_price = gas_price.to_string();
    let gas_limit = gas_limit.to_string();
    let output = run_cast_command([
        "mktx",
        "-q",
        "--legacy",
        "--chain",
        &chain_id,
        "--private-key",
        private_key,
        &token_address,
        "transfer(address,uint256)",
        &recipient,
        &amount,
        "--nonce",
        &nonce,
        "--gas-price",
        &gas_price,
        "--gas-limit",
        &gas_limit,
        "--value",
        "0",
    ])
    .await?;
    decode_prefixed_hex(output.trim())
}

async fn current_nonce(rpc_url: &str, address: EvmAddress) -> Result<u64, AnyError> {
    let client = Client::new();
    let response = client
        .post(rpc_url)
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "eth_getTransactionCount",
            "params": [address.to_string(), "latest"],
        }))
        .send()
        .await
        .map_err(|error| {
            helper_error(format!("eth_getTransactionCount request failed: {error}"))
        })?;

    if !response.status().is_success() {
        return Err(helper_error(format!(
            "eth_getTransactionCount returned HTTP {}",
            response.status()
        )));
    }

    let payload = response
        .json::<Value>()
        .await
        .map_err(|error| helper_error(format!("eth_getTransactionCount json: {error}")))?;
    let nonce = payload
        .get("result")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            helper_error(format!("eth_getTransactionCount missing result: {payload}"))
        })?;
    parse_hex_u64(nonce)
}

async fn send_raw_transaction(rpc_url: &str, raw_tx: &[u8]) -> Result<TxHash, AnyError> {
    let client = Client::new();
    let response = client
        .post(rpc_url)
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "eth_sendRawTransaction",
            "params": [format!("0x{}", encode_hex(raw_tx))],
        }))
        .send()
        .await
        .map_err(|error| helper_error(format!("eth_sendRawTransaction request failed: {error}")))?;

    if !response.status().is_success() {
        return Err(helper_error(format!(
            "eth_sendRawTransaction returned HTTP {}",
            response.status()
        )));
    }

    let payload = response
        .json::<Value>()
        .await
        .map_err(|error| helper_error(format!("eth_sendRawTransaction json: {error}")))?;
    let tx_hash = payload
        .get("result")
        .and_then(Value::as_str)
        .ok_or_else(|| helper_error(format!("eth_sendRawTransaction missing result: {payload}")))?;
    tx_hash
        .parse::<TxHash>()
        .map_err(|error| helper_error(format!("invalid tx hash {tx_hash}: {error}")))
}

async fn private_key_for_mnemonic(
    mnemonic: &str,
    derivation_path: &str,
) -> Result<String, AnyError> {
    run_cast_command([
        "wallet",
        "private-key",
        "-q",
        "--mnemonic",
        mnemonic,
        "--mnemonic-derivation-path",
        derivation_path,
    ])
    .await
}

async fn run_cast_command<const N: usize>(args: [&str; N]) -> Result<String, AnyError> {
    let output = Command::new("cast")
        .args(args)
        .output()
        .map_err(|error| helper_error(format!("failed to spawn cast: {error}")))?;

    if !output.status.success() {
        return Err(helper_error(format!(
            "cast command failed: {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )));
    }

    String::from_utf8(output.stdout)
        .map(|value| value.trim().to_string())
        .map_err(|error| helper_error(format!("cast stdout utf8: {error}")))
}

fn decode_prefixed_hex(value: &str) -> Result<Vec<u8>, AnyError> {
    let value = value.trim();
    let hex = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .ok_or_else(|| helper_error(format!("hex string must start with 0x: {value}")))?;

    if hex.len() % 2 != 0 {
        return Err(helper_error(format!("hex string has odd length: {value}")));
    }

    let mut bytes = Vec::with_capacity(hex.len() / 2);
    for chunk in hex.as_bytes().chunks_exact(2) {
        let high = hex_value(chunk[0]).ok_or_else(|| helper_error("invalid hex digit"))?;
        let low = hex_value(chunk[1]).ok_or_else(|| helper_error("invalid hex digit"))?;
        bytes.push((high << 4) | low);
    }
    Ok(bytes)
}

fn encode_hex(bytes: &[u8]) -> String {
    const TABLE: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(TABLE[(byte >> 4) as usize] as char);
        output.push(TABLE[(byte & 0x0f) as usize] as char);
    }
    output
}

fn parse_hex_u64(value: &str) -> Result<u64, AnyError> {
    let value = value.trim();
    let hex = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .ok_or_else(|| helper_error(format!("hex quantity must start with 0x: {value}")))?;
    u64::from_str_radix(hex, 16)
        .map_err(|error| helper_error(format!("invalid hex quantity {value}: {error}")))
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn pick_free_port() -> Result<u16, AnyError> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .map_err(|error| helper_error(format!("failed to bind free port: {error}")))?;
    let port = listener
        .local_addr()
        .map_err(|error| helper_error(format!("failed to inspect free port: {error}")))?
        .port();
    drop(listener);
    Ok(port)
}

fn foundry_toml() -> String {
    r#"[profile.default]
src = "src"
out = "out"
libs = []
solc_version = "0.8.24"
"#
    .to_string()
}

fn mock_erc20_source() -> String {
    r#"// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

contract MockERC20 {
    string public name = "Pay3 Mock Token";
    string public symbol = "PMOCK";
    uint8 public decimals = 18;

    mapping(address => uint256) public balanceOf;

    event Transfer(address indexed from, address indexed to, uint256 value);

    constructor(address initialHolder, uint256 initialSupply) {
        balanceOf[initialHolder] = initialSupply;
        emit Transfer(address(0), initialHolder, initialSupply);
    }

    function transfer(address to, uint256 value) external returns (bool) {
        require(balanceOf[msg.sender] >= value, "insufficient balance");
        unchecked {
            balanceOf[msg.sender] -= value;
            balanceOf[to] += value;
        }
        emit Transfer(msg.sender, to, value);
        return true;
    }
}
"#
    .to_string()
}
