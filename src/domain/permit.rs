//! EIP-712 domain separators, type hashes, and ABI calldata encoders for
//! gasless token collections (EIP-3009 transferWithAuthorization, Polygon
//! executeMetaTransaction, and EIP-2612 permit).

use alloy_primitives::{U256, keccak256};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::domain::{EvmAddress, RawAmount};

/// Contract call interface for lightweight on-chain contract probing.
#[async_trait]
pub trait ContractCallClient: Send + Sync {
    async fn call_contract(&self, to: EvmAddress, data: &[u8]) -> Result<Vec<u8>, String>;
}

/// ERC-20 `name()` selector: `0x06fdde03`
pub const ERC20_NAME_SELECTOR: [u8; 4] = [0x06, 0xfd, 0xde, 0x03];

/// EIP-712 `version()` selector: `0x54fd4d50`
pub const ERC20_VERSION_SELECTOR: [u8; 4] = [0x54, 0xfd, 0x4d, 0x50];

/// EIP-3009 `authorizationState(address,bytes32)` selector: `0xe94a0102`
pub const EIP3009_AUTHORIZATION_STATE_SELECTOR: [u8; 4] = [0xe9, 0x4a, 0x01, 0x02];

/// Polygon `getNonce(address)` selector: `0x2d0335ab`
pub const POLYGON_GET_NONCE_SELECTOR: [u8; 4] = [0x2d, 0x03, 0x35, 0xab];

/// EIP-2612 / EIP-712 `DOMAIN_SEPARATOR()` selector: `0x3644e515`
pub const EIP2612_DOMAIN_SEPARATOR_SELECTOR: [u8; 4] = [0x36, 0x44, 0xe5, 0x15];

/// EIP-2612 `nonces(address)` selector: `0x7ecebe00`
pub const EIP2612_NONCES_SELECTOR: [u8; 4] = [0x7e, 0xce, 0xbe, 0x00];

/// Generates probe calldata for EIP-3009 `authorizationState(address,bytes32)`.
pub fn eip3009_probe_calldata() -> Vec<u8> {
    let mut data = Vec::with_capacity(4 + 64);
    data.extend_from_slice(&EIP3009_AUTHORIZATION_STATE_SELECTOR);
    data.extend_from_slice(&[0u8; 64]);
    data
}

/// Generates probe calldata for Polygon `getNonce(address)`.
pub fn polygon_meta_tx_probe_calldata() -> Vec<u8> {
    let mut data = Vec::with_capacity(4 + 32);
    data.extend_from_slice(&POLYGON_GET_NONCE_SELECTOR);
    data.extend_from_slice(&[0u8; 32]);
    data
}

/// Generates probe calldata for EIP-2612 `DOMAIN_SEPARATOR()`.
pub fn domain_separator_probe_calldata() -> Vec<u8> {
    EIP2612_DOMAIN_SEPARATOR_SELECTOR.to_vec()
}

/// Generates probe calldata for EIP-2612 `nonces(address)`.
pub fn nonces_probe_calldata() -> Vec<u8> {
    let mut data = Vec::with_capacity(4 + 32);
    data.extend_from_slice(&EIP2612_NONCES_SELECTOR);
    data.extend_from_slice(&[0u8; 32]);
    data
}

/// Decodes a string returned from an ABI call (`name()` or `version()`).
/// Supports standard ABI dynamic strings (offset + length + bytes) as well as
/// legacy fixed 32-byte `bytes32` strings.
pub fn decode_abi_string(data: &[u8]) -> Option<String> {
    if data.len() >= 64 {
        let len_slice = &data[32..64];
        if let Some(len) = U256::try_from_be_slice(len_slice).and_then(|u| usize::try_from(u).ok()) {
            let start: usize = 64;
            if let Some(end) = start.checked_add(len) {
                if end <= data.len() {
                    let str_bytes = &data[start..end];
                    if let Ok(s) = std::str::from_utf8(str_bytes) {
                        let trimmed = s.trim();
                        if !trimmed.is_empty() {
                            return Some(trimmed.to_string());
                        }
                    }
                }
            }
        }
    }

    if data.len() == 32 {
        let trimmed_bytes = data.split(|&b| b == 0).next().unwrap_or(&[]);
        if !trimmed_bytes.is_empty() {
            if let Ok(s) = std::str::from_utf8(trimmed_bytes) {
                let trimmed = s.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
        }
    }

    None
}

/// Result of probing an on-chain token contract.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OnChainTokenCapabilities {
    pub method: CollectionMethod,
    pub token_name: Option<String>,
    pub token_version: Option<String>,
    pub reason: String,
}

/// Probes an on-chain token contract via static `eth_call` to discover its supported
/// collection interface (EIP-3009, Polygon MetaTx, EIP-2612) and name/version metadata.
pub async fn probe_on_chain_token_capabilities<C: ContractCallClient>(
    client: &C,
    token_address: EvmAddress,
) -> OnChainTokenCapabilities {
    let token_name = match client.call_contract(token_address, &ERC20_NAME_SELECTOR).await {
        Ok(data) => decode_abi_string(&data),
        Err(_) => None,
    };
    let token_version = match client.call_contract(token_address, &ERC20_VERSION_SELECTOR).await {
        Ok(data) => decode_abi_string(&data),
        Err(_) => None,
    };

    // 1. Probe EIP-3009: authorizationState(address,bytes32) -> bool
    if let Ok(res) = client.call_contract(token_address, &eip3009_probe_calldata()).await {
        if res.len() == 32 {
            return OnChainTokenCapabilities {
                method: CollectionMethod::Eip3009,
                token_name,
                token_version: token_version.or_else(|| Some("2".to_string())),
                reason: "On-chain contract probe confirmed EIP-3009 authorizationState support".to_string(),
            };
        }
    }

    // 2. Probe Polygon MetaTx: getNonce(address) -> uint256
    if let Ok(res) = client.call_contract(token_address, &polygon_meta_tx_probe_calldata()).await {
        if res.len() == 32 {
            return OnChainTokenCapabilities {
                method: CollectionMethod::PolygonMetaTx,
                token_name,
                token_version: token_version.or_else(|| Some("1".to_string())),
                reason: "On-chain contract probe confirmed Polygon NativeMetaTransaction (getNonce) support".to_string(),
            };
        }
    }

    // 3. Probe EIP-2612: DOMAIN_SEPARATOR() and nonces(address)
    let domain_res = client.call_contract(token_address, &domain_separator_probe_calldata()).await;
    let nonces_res = client.call_contract(token_address, &nonces_probe_calldata()).await;

    if let (Ok(dom), Ok(non)) = (domain_res, nonces_res) {
        if dom.len() == 32 && dom.iter().any(|&b| b != 0) && non.len() == 32 {
            return OnChainTokenCapabilities {
                method: CollectionMethod::Eip2612,
                token_name,
                token_version: token_version.or_else(|| Some("1".to_string())),
                reason: "On-chain contract probe confirmed EIP-2612 permit (DOMAIN_SEPARATOR & nonces) support".to_string(),
            };
        }
    }

    // 4. Default fallback to Standard
    OnChainTokenCapabilities {
        method: CollectionMethod::Standard,
        token_name,
        token_version,
        reason: "On-chain contract probe did not detect EIP-3009, MetaTx, or EIP-2612 interfaces, safely using standard transfer".to_string(),
    }
}

/// Supported fund collection methods.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CollectionMethod {
    /// Automatically resolve the best collection strategy based on the chain ID,
    /// token address/symbol, and whether a relayer is configured.
    #[default]
    Auto,

    /// Standard ERC-20 `transfer(treasury, amount)`.
    /// Sent directly by the child address, requiring native gas on child address.
    Standard,

    /// EIP-3009 `transferWithAuthorization(from, to, value, validAfter, validBefore, nonce, v, r, s)`.
    /// Native USDC on Ethereum, Polygon, Arbitrum, Base, Optimism, Avalanche, etc.
    /// Child signs off-chain EIP-712 authorization, outer transaction sent and paid by Relayer.
    Eip3009,

    /// Polygon PoS Native Meta-Transaction `executeMetaTransaction(userAddress, functionSignature, sigR, sigS, sigV)`.
    /// Polygon PoS bridged USDT (`ChildERC20` contract `0xc2132D05D31c914a87C6611C10748AEb04B58e8F`).
    /// Child signs off-chain EIP-712 meta transaction, outer transaction sent and paid by Relayer.
    PolygonMetaTx,

    /// EIP-2612 `permit(owner, spender, value, deadline, v, r, s)`.
    /// Child signs off-chain EIP-712 permit, outer transaction sent and paid by Relayer.
    Eip2612,
}

impl CollectionMethod {
    pub const fn is_gasless(self) -> bool {
        matches!(self, Self::Eip3009 | Self::PolygonMetaTx | Self::Eip2612)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Standard => "standard",
            Self::Eip3009 => "eip3009",
            Self::PolygonMetaTx => "polygon_meta_tx",
            Self::Eip2612 => "eip2612",
        }
    }

    pub fn parse(value: &str) -> Result<Self, String> {
        match value.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "auto" | "default" | "optimal" => Ok(Self::Auto),
            "standard" | "direct" | "erc20" => Ok(Self::Standard),
            "eip3009" | "eip_3009" | "usdc" => Ok(Self::Eip3009),
            "polygon_meta_tx" | "polygon" | "meta_tx" | "pos_meta_tx" => Ok(Self::PolygonMetaTx),
            "eip2612" | "eip_2612" | "permit" => Ok(Self::Eip2612),
            _ => Err(format!(
                "expected one of auto, standard, eip3009, polygon_meta_tx, eip2612; got '{value}'"
            )),
        }
    }
}

/// Resolved optimal collection strategy outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OptimalStrategyResolution {
    pub method: CollectionMethod,
    pub token_name: String,
    pub token_version: String,
    pub reason: &'static str,
}

#[derive(Clone, Copy, Debug)]
pub struct PresetTokenInfo {
    pub method: CollectionMethod,
    pub token_name: &'static str,
    pub token_version: &'static str,
    pub reason: &'static str,
}

/// Fast-path preset lookup for standard tokens across major chains.
pub fn lookup_preset(chain_id: u64, token_address: EvmAddress) -> Option<PresetTokenInfo> {
    let addr_lower = token_address.to_lower_hex();
    match (chain_id, addr_lower.as_str()) {
        // Ethereum Mainnet (1)
        (1, "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48") => Some(PresetTokenInfo {
            method: CollectionMethod::Eip3009,
            token_name: "USD Coin",
            token_version: "2",
            reason: "Ethereum Mainnet Native USDC supports EIP-3009 transferWithAuthorization",
        }),
        (1, "0xdac17f958d2ee523a2206206994597c13d831ec7") => Some(PresetTokenInfo {
            method: CollectionMethod::Standard,
            token_name: "Tether USD",
            token_version: "1",
            reason: "Ethereum Mainnet legacy USDT lacks permit/meta-tx support, falling back to standard transfer",
        }),
        (1, "0x6b175474e89094c44da98b954eedeac495271d0f") => Some(PresetTokenInfo {
            method: CollectionMethod::Eip2612,
            token_name: "Dai Stablecoin",
            token_version: "1",
            reason: "Ethereum Mainnet DAI supports EIP-2612 permit",
        }),

        // Polygon PoS (137)
        (137, "0xc2132d05d31c914a87c6611c10748aeb04b58e8f") => Some(PresetTokenInfo {
            method: CollectionMethod::PolygonMetaTx,
            token_name: "(PoS) Tether USD",
            token_version: "1",
            reason: "Polygon PoS Native USDT supports executeMetaTransaction",
        }),
        (137, "0x3c499c542cef5e3811e1192ce70d8cc03d5c3359") => Some(PresetTokenInfo {
            method: CollectionMethod::Eip3009,
            token_name: "USD Coin",
            token_version: "2",
            reason: "Polygon PoS Native USDC supports EIP-3009 transferWithAuthorization",
        }),
        (137, "0x2791bca1f2de4661ed88a30c99a7a9449aa84174") => Some(PresetTokenInfo {
            method: CollectionMethod::Eip3009,
            token_name: "USD Coin (PoS)",
            token_version: "1",
            reason: "Polygon PoS Bridged USDC.e supports EIP-3009 transferWithAuthorization",
        }),
        (137, "0x8f3cf7ad23cd3cadbd9735aff958023239c6a063") => Some(PresetTokenInfo {
            method: CollectionMethod::Eip2612,
            token_name: "(PoS) Dai Stablecoin",
            token_version: "1",
            reason: "Polygon PoS DAI supports EIP-2612 permit",
        }),

        // Arbitrum One (42161)
        (42161, "0xaf88d065e77c8cc2239327c5edb3a432268e5831") => Some(PresetTokenInfo {
            method: CollectionMethod::Eip3009,
            token_name: "USD Coin",
            token_version: "2",
            reason: "Arbitrum One Native USDC supports EIP-3009 transferWithAuthorization",
        }),
        (42161, "0xfd086bc7cd5c481dcc9c85ebe478a1c0b69fcbb9") => Some(PresetTokenInfo {
            method: CollectionMethod::Eip2612,
            token_name: "Tether USD",
            token_version: "1",
            reason: "Arbitrum One Native USDT supports EIP-2612 permit",
        }),
        (42161, "0xff970a61a04b1ca14834a43f5de4533ebddb5cc8") => Some(PresetTokenInfo {
            method: CollectionMethod::Standard,
            token_name: "Bridged USDC",
            token_version: "1",
            reason: "Arbitrum One legacy USDC.e lacks permit support, falling back to standard transfer",
        }),

        // Optimism (10)
        (10, "0x0b2c639c533813f4aa9d7837caf62653d097ff85") => Some(PresetTokenInfo {
            method: CollectionMethod::Eip3009,
            token_name: "USD Coin",
            token_version: "2",
            reason: "Optimism Native USDC supports EIP-3009 transferWithAuthorization",
        }),
        (10, "0x94b008aa00579c1307b0ef2c499ad98a8ce58e58") => Some(PresetTokenInfo {
            method: CollectionMethod::Eip2612,
            token_name: "Tether USD",
            token_version: "1",
            reason: "Optimism Native USDT supports EIP-2612 permit",
        }),

        // Base (8453)
        (8453, "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913") => Some(PresetTokenInfo {
            method: CollectionMethod::Eip3009,
            token_name: "USD Coin",
            token_version: "2",
            reason: "Base Native USDC supports EIP-3009 transferWithAuthorization",
        }),

        // Avalanche C-Chain (43114)
        (43114, "0xb97ef9ef8734c71904d8002f8b6bc66dd9c48a6e") => Some(PresetTokenInfo {
            method: CollectionMethod::Eip3009,
            token_name: "USD Coin",
            token_version: "2",
            reason: "Avalanche Native USDC supports EIP-3009 transferWithAuthorization",
        }),
        (43114, "0x9702230a8ea53601f5cd2dc00fdbc13d4df4a8c7") => Some(PresetTokenInfo {
            method: CollectionMethod::Eip2612,
            token_name: "Tether USDt",
            token_version: "1",
            reason: "Avalanche Native USDT supports EIP-2612 permit",
        }),

        // BNB Smart Chain (56)
        (56, "0x55d398326f99059ff775485246999027b3197955") => Some(PresetTokenInfo {
            method: CollectionMethod::Standard,
            token_name: "Tether USD",
            token_version: "1",
            reason: "BNB Smart Chain BEP-20 USDT lacks native permit, falling back to standard transfer",
        }),
        (56, "0x8ac76a51cc950d9822d68b83fe1ad97b32cd580d") => Some(PresetTokenInfo {
            method: CollectionMethod::Standard,
            token_name: "Binance-Peg USD Coin",
            token_version: "1",
            reason: "BNB Smart Chain BEP-20 USDC lacks native permit, falling back to standard transfer",
        }),

        _ => None,
    }
}

/// Fallback heuristic matching for symbols.
pub fn lookup_heuristic(chain_id: u64, token_symbol: &str) -> PresetTokenInfo {
    let sym = token_symbol.trim();
    if sym.eq_ignore_ascii_case("USDC") || sym.eq_ignore_ascii_case("USDC.e") {
        if chain_id == 56 {
            PresetTokenInfo {
                method: CollectionMethod::Standard,
                token_name: "Binance-Peg USD Coin",
                token_version: "1",
                reason: "BNB Smart Chain USDC lacks native permit, falling back to standard transfer",
            }
        } else {
            PresetTokenInfo {
                method: CollectionMethod::Eip3009,
                token_name: "USD Coin",
                token_version: "2",
                reason: "Symbol matches Circle Native USDC standard, auto-selecting EIP-3009",
            }
        }
    } else if sym.eq_ignore_ascii_case("USDT") {
        if chain_id == 137 {
            PresetTokenInfo {
                method: CollectionMethod::PolygonMetaTx,
                token_name: "(PoS) Tether USD",
                token_version: "1",
                reason: "Polygon PoS USDT matches NativeMetaTransaction, auto-selecting PolygonMetaTx",
            }
        } else if chain_id == 42161 || chain_id == 10 || chain_id == 43114 {
            PresetTokenInfo {
                method: CollectionMethod::Eip2612,
                token_name: "Tether USD",
                token_version: "1",
                reason: "L2/Alt-L1 USDT matches EIP-2612 permit standard, auto-selecting Eip2612",
            }
        } else {
            PresetTokenInfo {
                method: CollectionMethod::Standard,
                token_name: "Tether USD",
                token_version: "1",
                reason: "Token lacks confirmed permit standard on this chain, falling back to standard transfer",
            }
        }
    } else {
        PresetTokenInfo {
            method: CollectionMethod::Standard,
            token_name: "ERC20",
            token_version: "1",
            reason: "Unrecognized token without permit configuration, safely falling back to standard transfer",
        }
    }
}

fn lookup_preset_or_heuristic(
    chain_id: u64,
    token_address: EvmAddress,
    token_symbol: &str,
) -> PresetTokenInfo {
    lookup_preset(chain_id, token_address).unwrap_or_else(|| lookup_heuristic(chain_id, token_symbol))
}

/// Resolves the optimal collection strategy based on the chain, token, relayer status,
/// and configured preferences.
pub fn resolve_optimal_collection_strategy(
    chain_id: u64,
    token_address: EvmAddress,
    token_symbol: &str,
    has_relayer: bool,
    configured_method: CollectionMethod,
    custom_token_name: Option<String>,
    custom_token_version: Option<String>,
) -> OptimalStrategyResolution {
    let preset = lookup_preset_or_heuristic(chain_id, token_address, token_symbol);

    let (effective_method, reason) = match configured_method {
        CollectionMethod::Auto => {
            if !has_relayer {
                (
                    CollectionMethod::Standard,
                    "No relayer configured for gasless collection, using standard transfer",
                )
            } else {
                (preset.method, preset.reason)
            }
        }
        explicit => {
            let reason = match explicit {
                CollectionMethod::Auto => unreachable!(),
                CollectionMethod::Standard => "Explicitly configured to standard transfer",
                CollectionMethod::Eip3009 => {
                    "Explicitly configured to EIP-3009 transferWithAuthorization"
                }
                CollectionMethod::PolygonMetaTx => {
                    "Explicitly configured to Polygon executeMetaTransaction"
                }
                CollectionMethod::Eip2612 => "Explicitly configured to EIP-2612 permit",
            };
            (explicit, reason)
        }
    };

    let token_name = custom_token_name.unwrap_or_else(|| preset.token_name.to_string());
    let token_version = custom_token_version.unwrap_or_else(|| preset.token_version.to_string());

    OptimalStrategyResolution {
        method: effective_method,
        token_name,
        token_version,
        reason,
    }
}

/// Resolves the optimal collection strategy with optional on-chain dynamic probing.
/// If the token is not present in the hardcoded preset registry, this will query
/// the contract directly via `client` to determine if it supports EIP-3009,
/// Polygon MetaTx, or EIP-2612, and extract the token's on-chain name/version.
pub async fn resolve_optimal_collection_strategy_with_probe<C: ContractCallClient>(
    chain_id: u64,
    token_address: EvmAddress,
    token_symbol: &str,
    has_relayer: bool,
    configured_method: CollectionMethod,
    custom_token_name: Option<String>,
    custom_token_version: Option<String>,
    client: Option<&C>,
) -> OptimalStrategyResolution {
    if configured_method != CollectionMethod::Auto {
        return resolve_optimal_collection_strategy(
            chain_id,
            token_address,
            token_symbol,
            has_relayer,
            configured_method,
            custom_token_name,
            custom_token_version,
        );
    }

    if !has_relayer {
        return OptimalStrategyResolution {
            method: CollectionMethod::Standard,
            token_name: custom_token_name.unwrap_or_else(|| token_symbol.to_string()),
            token_version: custom_token_version.unwrap_or_else(|| "1".to_string()),
            reason: "No relayer configured for gasless collection, using standard transfer",
        };
    }

    // 1. Fast path: check exact preset registry
    if let Some(preset) = lookup_preset(chain_id, token_address) {
        let token_name = custom_token_name.unwrap_or_else(|| preset.token_name.to_string());
        let token_version =
            custom_token_version.unwrap_or_else(|| preset.token_version.to_string());
        return OptimalStrategyResolution {
            method: preset.method,
            token_name,
            token_version,
            reason: preset.reason,
        };
    }

    // 2. Dynamic on-chain probe for unknown tokens
    if let Some(client) = client {
        let probed = probe_on_chain_token_capabilities(client, token_address).await;
        if probed.method != CollectionMethod::Standard {
            let token_name = custom_token_name
                .or(probed.token_name)
                .unwrap_or_else(|| token_symbol.to_string());
            let token_version = custom_token_version
                .or(probed.token_version)
                .unwrap_or_else(|| "1".to_string());
            return OptimalStrategyResolution {
                method: probed.method,
                token_name,
                token_version,
                reason: Box::leak(probed.reason.into_boxed_str()),
            };
        }
    }

    // 3. Fallback: heuristic
    resolve_optimal_collection_strategy(
        chain_id,
        token_address,
        token_symbol,
        has_relayer,
        configured_method,
        custom_token_name,
        custom_token_version,
    )
}

impl std::str::FromStr for CollectionMethod {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

/// An ECDSA (v, r, s) signature produced from an EIP-712 digest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypedSignature {
    pub v: u8,
    #[serde(with = "hex_bytes_32")]
    pub r: [u8; 32],
    #[serde(with = "hex_bytes_32")]
    pub s: [u8; 32],
}

impl TypedSignature {
    pub const fn new(v: u8, r: [u8; 32], s: [u8; 32]) -> Self {
        Self { v, r, s }
    }
}

mod hex_bytes_32 {
    use serde::{Deserialize, Deserializer, Serializer, de};
    use crate::domain::address::{decode_prefixed_fixed, encode_lower_prefixed};

    pub fn serialize<S>(value: &[u8; 32], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&encode_lower_prefixed(value))
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<[u8; 32], D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        decode_prefixed_fixed::<32>(&s, "signature_component").map_err(de::Error::custom)
    }
}

/// Standard EIP-712 domain separator:
/// `EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)`
pub fn eip712_domain_separator(
    name: &str,
    version: &str,
    chain_id: u64,
    verifying_contract: EvmAddress,
) -> [u8; 32] {
    let type_hash = keccak256(
        b"EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)",
    );
    let name_hash = keccak256(name.as_bytes());
    let version_hash = keccak256(version.as_bytes());
    let chain_id_bytes = U256::from(chain_id).to_be_bytes::<32>();
    let mut contract_bytes = [0u8; 32];
    contract_bytes[12..].copy_from_slice(verifying_contract.as_bytes());

    let mut encoded = Vec::with_capacity(32 * 5);
    encoded.extend_from_slice(type_hash.as_slice());
    encoded.extend_from_slice(name_hash.as_slice());
    encoded.extend_from_slice(version_hash.as_slice());
    encoded.extend_from_slice(&chain_id_bytes);
    encoded.extend_from_slice(&contract_bytes);

    keccak256(&encoded).into()
}

/// Polygon PoS Native Meta-Transaction domain separator:
/// `EIP712Domain(string name,string version,address verifyingContract,bytes32 salt)`
/// where `salt = bytes32(chainId)`.
pub fn polygon_meta_tx_domain_separator(
    name: &str,
    version: &str,
    chain_id: u64,
    verifying_contract: EvmAddress,
) -> [u8; 32] {
    let type_hash = keccak256(
        b"EIP712Domain(string name,string version,address verifyingContract,bytes32 salt)",
    );
    let name_hash = keccak256(name.as_bytes());
    let version_hash = keccak256(version.as_bytes());
    let mut contract_bytes = [0u8; 32];
    contract_bytes[12..].copy_from_slice(verifying_contract.as_bytes());
    let salt = U256::from(chain_id).to_be_bytes::<32>();

    let mut encoded = Vec::with_capacity(32 * 5);
    encoded.extend_from_slice(type_hash.as_slice());
    encoded.extend_from_slice(name_hash.as_slice());
    encoded.extend_from_slice(version_hash.as_slice());
    encoded.extend_from_slice(&contract_bytes);
    encoded.extend_from_slice(&salt);

    keccak256(&encoded).into()
}

/// Combines `domain_separator` and `struct_hash` into the standard EIP-712 signing digest:
/// `keccak256("\x19\x01" || domainSeparator || structHash)`
pub fn eip712_digest(domain_separator: [u8; 32], struct_hash: [u8; 32]) -> [u8; 32] {
    let mut input = [0u8; 2 + 32 + 32];
    input[0] = 0x19;
    input[1] = 0x01;
    input[2..34].copy_from_slice(&domain_separator);
    input[34..66].copy_from_slice(&struct_hash);
    keccak256(&input).into()
}

// ---------------------------------------------------------------------------
// EIP-3009: TransferWithAuthorization (Native USDC)
// ---------------------------------------------------------------------------

/// Computes the EIP-712 struct hash for EIP-3009 `TransferWithAuthorization`:
/// `keccak256("TransferWithAuthorization(address from,address to,uint256 value,uint256 validAfter,uint256 validBefore,bytes32 nonce)")`
pub fn eip3009_struct_hash(
    from: EvmAddress,
    to: EvmAddress,
    value: RawAmount,
    valid_after: u64,
    valid_before: u64,
    nonce: [u8; 32],
) -> [u8; 32] {
    let type_hash = keccak256(
        b"TransferWithAuthorization(address from,address to,uint256 value,uint256 validAfter,uint256 validBefore,bytes32 nonce)",
    );
    let mut from_bytes = [0u8; 32];
    from_bytes[12..].copy_from_slice(from.as_bytes());
    let mut to_bytes = [0u8; 32];
    to_bytes[12..].copy_from_slice(to.as_bytes());
    let value_bytes = value.value().to_be_bytes::<32>();
    let valid_after_bytes = U256::from(valid_after).to_be_bytes::<32>();
    let valid_before_bytes = U256::from(valid_before).to_be_bytes::<32>();

    let mut encoded = Vec::with_capacity(32 * 7);
    encoded.extend_from_slice(type_hash.as_slice());
    encoded.extend_from_slice(&from_bytes);
    encoded.extend_from_slice(&to_bytes);
    encoded.extend_from_slice(&value_bytes);
    encoded.extend_from_slice(&valid_after_bytes);
    encoded.extend_from_slice(&valid_before_bytes);
    encoded.extend_from_slice(&nonce);

    keccak256(&encoded).into()
}

/// Selector for `transferWithAuthorization(address,address,uint256,uint256,uint256,bytes32,uint8,bytes32,bytes32)`:
/// `0xe3ee160e`
pub const EIP3009_TRANSFER_WITH_AUTHORIZATION_SELECTOR: [u8; 4] = [0xe3, 0xee, 0x16, 0x0e];

/// Encodes ABI calldata for `transferWithAuthorization(from, to, value, validAfter, validBefore, nonce, v, r, s)`.
pub fn encode_eip3009_transfer_with_authorization(
    from: EvmAddress,
    to: EvmAddress,
    value: RawAmount,
    valid_after: u64,
    valid_before: u64,
    nonce: [u8; 32],
    sig: TypedSignature,
) -> Vec<u8> {
    let mut data = Vec::with_capacity(4 + 32 * 9);
    data.extend_from_slice(&EIP3009_TRANSFER_WITH_AUTHORIZATION_SELECTOR);

    // from (address)
    data.extend_from_slice(&[0u8; 12]);
    data.extend_from_slice(from.as_bytes());

    // to (address)
    data.extend_from_slice(&[0u8; 12]);
    data.extend_from_slice(to.as_bytes());

    // value (uint256)
    data.extend_from_slice(&value.value().to_be_bytes::<32>());

    // validAfter (uint256)
    data.extend_from_slice(&U256::from(valid_after).to_be_bytes::<32>());

    // validBefore (uint256)
    data.extend_from_slice(&U256::from(valid_before).to_be_bytes::<32>());

    // nonce (bytes32)
    data.extend_from_slice(&nonce);

    // v (uint8)
    data.extend_from_slice(&[0u8; 31]);
    data.push(sig.v);

    // r (bytes32)
    data.extend_from_slice(&sig.r);

    // s (bytes32)
    data.extend_from_slice(&sig.s);

    data
}

// ---------------------------------------------------------------------------
// Polygon PoS: NativeMetaTransaction (executeMetaTransaction)
// ---------------------------------------------------------------------------

/// Computes the EIP-712 struct hash for Polygon `MetaTransaction`:
/// `keccak256("MetaTransaction(uint256 nonce,address from,bytes functionSignature)")`
pub fn polygon_meta_tx_struct_hash(
    nonce: u64,
    from: EvmAddress,
    function_signature: &[u8],
) -> [u8; 32] {
    let type_hash = keccak256(b"MetaTransaction(uint256 nonce,address from,bytes functionSignature)");
    let nonce_bytes = U256::from(nonce).to_be_bytes::<32>();
    let mut from_bytes = [0u8; 32];
    from_bytes[12..].copy_from_slice(from.as_bytes());
    let fn_sig_hash = keccak256(function_signature);

    let mut encoded = Vec::with_capacity(32 * 4);
    encoded.extend_from_slice(type_hash.as_slice());
    encoded.extend_from_slice(&nonce_bytes);
    encoded.extend_from_slice(&from_bytes);
    encoded.extend_from_slice(fn_sig_hash.as_slice());

    keccak256(&encoded).into()
}

/// Selector for `executeMetaTransaction(address,bytes,bytes32,bytes32,uint8)`:
/// `0x0c53c51c`
pub const POLYGON_EXECUTE_META_TRANSACTION_SELECTOR: [u8; 4] = [0x0c, 0x53, 0xc5, 0x1c];

/// Encodes ABI calldata for `executeMetaTransaction(userAddress, functionSignature, sigR, sigS, sigV)`.
/// Arguments: `(address, bytes, bytes32, bytes32, uint8)`.
pub fn encode_polygon_execute_meta_transaction(
    user_address: EvmAddress,
    function_signature: &[u8],
    sig: TypedSignature,
) -> Vec<u8> {
    let mut data = Vec::with_capacity(4 + 32 * 6 + function_signature.len() + 32);
    data.extend_from_slice(&POLYGON_EXECUTE_META_TRANSACTION_SELECTOR);

    // 1. userAddress (offset 0x00)
    data.extend_from_slice(&[0u8; 12]);
    data.extend_from_slice(user_address.as_bytes());

    // 2. offset to functionSignature (offset 0x20):
    // 5 head parameters * 32 bytes = 160 (0xa0)
    data.extend_from_slice(&U256::from(160).to_be_bytes::<32>());

    // 3. sigR (offset 0x40)
    data.extend_from_slice(&sig.r);

    // 4. sigS (offset 0x60)
    data.extend_from_slice(&sig.s);

    // 5. sigV (offset 0x80)
    data.extend_from_slice(&[0u8; 31]);
    data.push(sig.v);

    // Dynamic tail: functionSignature
    // length (offset 0xa0)
    data.extend_from_slice(&U256::from(function_signature.len()).to_be_bytes::<32>());

    // data bytes
    data.extend_from_slice(function_signature);

    // pad to 32-byte boundary
    let remainder = function_signature.len() % 32;
    if remainder != 0 {
        let padding = 32 - remainder;
        data.extend_from_slice(&vec![0u8; padding]);
    }

    data
}

// ---------------------------------------------------------------------------
// EIP-2612: Permit
// ---------------------------------------------------------------------------

/// Computes the EIP-712 struct hash for EIP-2612 `Permit`:
/// `keccak256("Permit(address owner,address spender,uint256 value,uint256 nonce,uint256 deadline)")`
pub fn eip2612_struct_hash(
    owner: EvmAddress,
    spender: EvmAddress,
    value: RawAmount,
    nonce: u64,
    deadline: u64,
) -> [u8; 32] {
    let type_hash = keccak256(
        b"Permit(address owner,address spender,uint256 value,uint256 nonce,uint256 deadline)",
    );
    let mut owner_bytes = [0u8; 32];
    owner_bytes[12..].copy_from_slice(owner.as_bytes());
    let mut spender_bytes = [0u8; 32];
    spender_bytes[12..].copy_from_slice(spender.as_bytes());
    let value_bytes = value.value().to_be_bytes::<32>();
    let nonce_bytes = U256::from(nonce).to_be_bytes::<32>();
    let deadline_bytes = U256::from(deadline).to_be_bytes::<32>();

    let mut encoded = Vec::with_capacity(32 * 6);
    encoded.extend_from_slice(type_hash.as_slice());
    encoded.extend_from_slice(&owner_bytes);
    encoded.extend_from_slice(&spender_bytes);
    encoded.extend_from_slice(&value_bytes);
    encoded.extend_from_slice(&nonce_bytes);
    encoded.extend_from_slice(&deadline_bytes);

    keccak256(&encoded).into()
}

/// Selector for `permit(address,address,uint256,uint256,uint8,bytes32,bytes32)`:
/// `0xd505accf`
pub const EIP2612_PERMIT_SELECTOR: [u8; 4] = [0xd5, 0x05, 0xac, 0xcf];

/// Encodes ABI calldata for `permit(owner, spender, value, deadline, v, r, s)`.
pub fn encode_eip2612_permit(
    owner: EvmAddress,
    spender: EvmAddress,
    value: RawAmount,
    deadline: u64,
    sig: TypedSignature,
) -> Vec<u8> {
    let mut data = Vec::with_capacity(4 + 32 * 7);
    data.extend_from_slice(&EIP2612_PERMIT_SELECTOR);

    // owner (address)
    data.extend_from_slice(&[0u8; 12]);
    data.extend_from_slice(owner.as_bytes());

    // spender (address)
    data.extend_from_slice(&[0u8; 12]);
    data.extend_from_slice(spender.as_bytes());

    // value (uint256)
    data.extend_from_slice(&value.value().to_be_bytes::<32>());

    // deadline (uint256)
    data.extend_from_slice(&U256::from(deadline).to_be_bytes::<32>());

    // v (uint8)
    data.extend_from_slice(&[0u8; 31]);
    data.push(sig.v);

    // r (bytes32)
    data.extend_from_slice(&sig.r);

    // s (bytes32)
    data.extend_from_slice(&sig.s);

    data
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_address(byte: u8) -> EvmAddress {
        EvmAddress::from_bytes([byte; 20])
    }

    #[test]
    fn selectors_match_canonical_keccak() {
        assert_eq!(
            &keccak256(
                b"transferWithAuthorization(address,address,uint256,uint256,uint256,bytes32,uint8,bytes32,bytes32)"
            )[..4],
            &EIP3009_TRANSFER_WITH_AUTHORIZATION_SELECTOR
        );
        assert_eq!(
            &keccak256(b"executeMetaTransaction(address,bytes,bytes32,bytes32,uint8)")[..4],
            &POLYGON_EXECUTE_META_TRANSACTION_SELECTOR
        );
        assert_eq!(
            &keccak256(b"permit(address,address,uint256,uint256,uint8,bytes32,bytes32)")[..4],
            &EIP2612_PERMIT_SELECTOR
        );
        assert_eq!(&keccak256(b"name()")[..4], &ERC20_NAME_SELECTOR);
        assert_eq!(&keccak256(b"version()")[..4], &ERC20_VERSION_SELECTOR);
        assert_eq!(
            &keccak256(b"authorizationState(address,bytes32)")[..4],
            &EIP3009_AUTHORIZATION_STATE_SELECTOR
        );
        assert_eq!(
            &keccak256(b"getNonce(address)")[..4],
            &POLYGON_GET_NONCE_SELECTOR
        );
        assert_eq!(
            &keccak256(b"DOMAIN_SEPARATOR()")[..4],
            &EIP2612_DOMAIN_SEPARATOR_SELECTOR
        );
        assert_eq!(
            &keccak256(b"nonces(address)")[..4],
            &EIP2612_NONCES_SELECTOR
        );
    }

    #[test]
    fn eip712_digest_calculates_deterministic_hash() {
        let domain = eip712_domain_separator("USD Coin", "2", 137, test_address(0x11));
        let struct_hash = eip3009_struct_hash(
            test_address(0x22),
            test_address(0x33),
            RawAmount::from(1_000_000),
            0,
            1_800_000_000,
            [0xaa; 32],
        );
        let digest = eip712_digest(domain, struct_hash);
        assert_ne!(digest, [0u8; 32]);
    }

    #[test]
    fn eip3009_calldata_encoding_length_and_fields() {
        let from = test_address(0x22);
        let to = test_address(0x33);
        let sig = TypedSignature::new(27, [0x11; 32], [0x22; 32]);
        let calldata = encode_eip3009_transfer_with_authorization(
            from,
            to,
            RawAmount::from(50_000_000),
            0,
            u64::MAX,
            [0x99; 32],
            sig,
        );

        assert_eq!(calldata.len(), 4 + 32 * 9);
        assert_eq!(&calldata[..4], &EIP3009_TRANSFER_WITH_AUTHORIZATION_SELECTOR);
        assert_eq!(&calldata[16..36], from.as_bytes());
        assert_eq!(&calldata[48..68], to.as_bytes());
        assert_eq!(calldata[4 + 32 * 6 + 31], 27); // v
        assert_eq!(&calldata[4 + 32 * 7..4 + 32 * 8], &[0x11; 32]); // r
        assert_eq!(&calldata[4 + 32 * 8..4 + 32 * 9], &[0x22; 32]); // s
    }

    #[test]
    fn polygon_meta_tx_calldata_encoding_length_and_dynamic_bytes() {
        let user = test_address(0x44);
        let fn_data = vec![0xa9, 0x05, 0x9c, 0xbb, 0x01, 0x02]; // 6 bytes
        let sig = TypedSignature::new(28, [0x33; 32], [0x44; 32]);
        let calldata = encode_polygon_execute_meta_transaction(user, &fn_data, sig);

        // 4 selector + 5 head words (160) + 1 length word (32) + padded data (32) = 228
        assert_eq!(calldata.len(), 4 + 160 + 32 + 32);
        assert_eq!(&calldata[..4], &POLYGON_EXECUTE_META_TRANSACTION_SELECTOR);
        assert_eq!(&calldata[16..36], user.as_bytes());
        assert_eq!(calldata[4 + 32 * 4 + 31], 28); // v
        assert_eq!(&calldata[4 + 160 + 32..4 + 160 + 32 + 6], &fn_data);
    }

    #[test]
    fn eip2612_calldata_encoding_length_and_fields() {
        let owner = test_address(0x55);
        let spender = test_address(0x66);
        let sig = TypedSignature::new(27, [0x77; 32], [0x88; 32]);
        let calldata = encode_eip2612_permit(
            owner,
            spender,
            RawAmount::from(200_000_000),
            u64::MAX,
            sig,
        );

        // 4 selector + 7 words (owner, spender, value, deadline, v, r, s) = 4 + 32 * 7 = 228
        assert_eq!(calldata.len(), 4 + 32 * 7);
        assert_eq!(&calldata[..4], &EIP2612_PERMIT_SELECTOR);
        assert_eq!(&calldata[16..36], owner.as_bytes());
        assert_eq!(&calldata[48..68], spender.as_bytes());
        assert_eq!(calldata[4 + 32 * 4 + 31], 27); // v
        assert_eq!(&calldata[4 + 32 * 5..4 + 32 * 6], &[0x77; 32]); // r
        assert_eq!(&calldata[4 + 32 * 6..4 + 32 * 7], &[0x88; 32]); // s
    }

    #[test]
    fn resolve_optimal_strategy_presets_and_fallbacks() {
        let usdc_eth =
            EvmAddress::parse_hex("0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48").unwrap();
        let usdt_eth =
            EvmAddress::parse_hex("0xdac17f958d2ee523a2206206994597c13d831ec7").unwrap();
        let usdt_poly =
            EvmAddress::parse_hex("0xc2132d05d31c914a87c6611c10748aeb04b58e8f").unwrap();
        let usdc_poly =
            EvmAddress::parse_hex("0x3c499c542cef5e3811e1192ce70d8cc03d5c3359").unwrap();
        let usdt_arb =
            EvmAddress::parse_hex("0xfd086bc7cd5c481dcc9c85ebe478a1c0b69fcbb9").unwrap();
        let usdc_arb =
            EvmAddress::parse_hex("0xaf88d065e77c8cc2239327c5edb3a432268e5831").unwrap();
        let usdt_op =
            EvmAddress::parse_hex("0x94b008aa00579c1307b0ef2c499ad98a8ce58e58").unwrap();
        let usdt_bsc =
            EvmAddress::parse_hex("0x55d398326f99059ff775485246999027b3197955").unwrap();

        // 1. USDC on Ethereum -> Eip3009
        let res = resolve_optimal_collection_strategy(
            1,
            usdc_eth,
            "USDC",
            true,
            CollectionMethod::Auto,
            None,
            None,
        );
        assert_eq!(res.method, CollectionMethod::Eip3009);
        assert_eq!(res.token_name, "USD Coin");
        assert_eq!(res.token_version, "2");

        // 2. USDT on Ethereum -> Standard (lacks permit)
        let res = resolve_optimal_collection_strategy(
            1,
            usdt_eth,
            "USDT",
            true,
            CollectionMethod::Auto,
            None,
            None,
        );
        assert_eq!(res.method, CollectionMethod::Standard);

        // 3. USDT on Polygon -> PolygonMetaTx
        let res = resolve_optimal_collection_strategy(
            137,
            usdt_poly,
            "USDT",
            true,
            CollectionMethod::Auto,
            None,
            None,
        );
        assert_eq!(res.method, CollectionMethod::PolygonMetaTx);
        assert_eq!(res.token_name, "(PoS) Tether USD");
        assert_eq!(res.token_version, "1");

        // 4. USDC on Polygon -> Eip3009
        let res = resolve_optimal_collection_strategy(
            137,
            usdc_poly,
            "USDC",
            true,
            CollectionMethod::Auto,
            None,
            None,
        );
        assert_eq!(res.method, CollectionMethod::Eip3009);

        // 5. USDT on Arbitrum -> Eip2612
        let res = resolve_optimal_collection_strategy(
            42161,
            usdt_arb,
            "USDT",
            true,
            CollectionMethod::Auto,
            None,
            None,
        );
        assert_eq!(res.method, CollectionMethod::Eip2612);

        // 6. USDC on Arbitrum -> Eip3009
        let res = resolve_optimal_collection_strategy(
            42161,
            usdc_arb,
            "USDC",
            true,
            CollectionMethod::Auto,
            None,
            None,
        );
        assert_eq!(res.method, CollectionMethod::Eip3009);

        // 7. USDT on Optimism -> Eip2612
        let res = resolve_optimal_collection_strategy(
            10,
            usdt_op,
            "USDT",
            true,
            CollectionMethod::Auto,
            None,
            None,
        );
        assert_eq!(res.method, CollectionMethod::Eip2612);

        // 8. USDT on BSC -> Standard
        let res = resolve_optimal_collection_strategy(
            56,
            usdt_bsc,
            "USDT",
            true,
            CollectionMethod::Auto,
            None,
            None,
        );
        assert_eq!(res.method, CollectionMethod::Standard);

        // 9. When relayer is NOT configured -> falls back to Standard
        let res = resolve_optimal_collection_strategy(
            137,
            usdt_poly,
            "USDT",
            false,
            CollectionMethod::Auto,
            None,
            None,
        );
        assert_eq!(res.method, CollectionMethod::Standard);

        // 10. Explicit configuration override
        let res = resolve_optimal_collection_strategy(
            137,
            usdt_poly,
            "USDT",
            true,
            CollectionMethod::Standard,
            None,
            None,
        );
        assert_eq!(res.method, CollectionMethod::Standard);

        // 11. Custom token_name / version override
        let res = resolve_optimal_collection_strategy(
            137,
            usdt_poly,
            "USDT",
            true,
            CollectionMethod::Auto,
            Some("Custom USDT".to_string()),
            Some("99".to_string()),
        );
        assert_eq!(res.method, CollectionMethod::PolygonMetaTx);
        assert_eq!(res.token_name, "Custom USDT");
        assert_eq!(res.token_version, "99");
    }

    fn encode_abi_string(s: &str) -> Vec<u8> {
        let mut out = vec![0u8; 64];
        out[31] = 0x20;
        out[63] = s.len() as u8;
        out.extend_from_slice(s.as_bytes());
        while out.len() % 32 != 0 {
            out.push(0);
        }
        out
    }

    #[test]
    fn decode_abi_string_works_for_dynamic_and_bytes32() {
        // 1. Dynamic ABI string
        let dyn_bytes = encode_abi_string("USD Coin");
        assert_eq!(decode_abi_string(&dyn_bytes), Some("USD Coin".to_string()));

        // 2. Fixed 32-byte bytes32 with trailing nulls
        let mut fixed_bytes = [0u8; 32];
        let name = b"Tether USD";
        fixed_bytes[..name.len()].copy_from_slice(name);
        assert_eq!(decode_abi_string(&fixed_bytes), Some("Tether USD".to_string()));

        // 3. Empty string
        let empty_dyn = encode_abi_string("");
        assert_eq!(decode_abi_string(&empty_dyn), None);

        // 4. Invalid length / garbage
        assert_eq!(decode_abi_string(&[0u8; 10]), None);
        assert_eq!(decode_abi_string(&[]), None);
    }

    struct MockContractCaller {
        responses: std::collections::BTreeMap<Vec<u8>, Vec<u8>>,
    }

    #[async_trait]
    impl ContractCallClient for MockContractCaller {
        async fn call_contract(&self, _to: EvmAddress, data: &[u8]) -> Result<Vec<u8>, String> {
            for (prefix, resp) in &self.responses {
                if data.starts_with(prefix) {
                    return Ok(resp.clone());
                }
            }
            Err("call reverted".to_string())
        }
    }

    #[tokio::test]
    async fn probe_on_chain_capabilities_eip3009() {
        let mut caller = MockContractCaller {
            responses: std::collections::BTreeMap::new(),
        };
        caller
            .responses
            .insert(EIP3009_AUTHORIZATION_STATE_SELECTOR.to_vec(), vec![0u8; 32]);
        caller
            .responses
            .insert(ERC20_NAME_SELECTOR.to_vec(), encode_abi_string("USD Coin"));
        caller
            .responses
            .insert(ERC20_VERSION_SELECTOR.to_vec(), encode_abi_string("2"));

        let probe = probe_on_chain_token_capabilities(&caller, test_address(0x55)).await;
        assert_eq!(probe.method, CollectionMethod::Eip3009);
        assert_eq!(probe.token_name.as_deref(), Some("USD Coin"));
        assert_eq!(probe.token_version.as_deref(), Some("2"));
    }

    #[tokio::test]
    async fn probe_on_chain_capabilities_polygon_meta_tx() {
        let mut caller = MockContractCaller {
            responses: std::collections::BTreeMap::new(),
        };
        caller
            .responses
            .insert(POLYGON_GET_NONCE_SELECTOR.to_vec(), vec![0u8; 32]);
        caller.responses.insert(
            ERC20_NAME_SELECTOR.to_vec(),
            encode_abi_string("(PoS) Tether USD"),
        );

        let probe = probe_on_chain_token_capabilities(&caller, test_address(0x55)).await;
        assert_eq!(probe.method, CollectionMethod::PolygonMetaTx);
        assert_eq!(probe.token_name.as_deref(), Some("(PoS) Tether USD"));
        assert_eq!(probe.token_version.as_deref(), Some("1"));
    }

    #[tokio::test]
    async fn probe_on_chain_capabilities_eip2612() {
        let mut caller = MockContractCaller {
            responses: std::collections::BTreeMap::new(),
        };
        caller
            .responses
            .insert(EIP2612_DOMAIN_SEPARATOR_SELECTOR.to_vec(), vec![0x11; 32]);
        caller
            .responses
            .insert(EIP2612_NONCES_SELECTOR.to_vec(), vec![0u8; 32]);
        caller
            .responses
            .insert(ERC20_NAME_SELECTOR.to_vec(), encode_abi_string("DAI Token"));

        let probe = probe_on_chain_token_capabilities(&caller, test_address(0x55)).await;
        assert_eq!(probe.method, CollectionMethod::Eip2612);
        assert_eq!(probe.token_name.as_deref(), Some("DAI Token"));
        assert_eq!(probe.token_version.as_deref(), Some("1"));
    }

    #[tokio::test]
    async fn probe_on_chain_capabilities_fallback_to_standard() {
        let caller = MockContractCaller {
            responses: std::collections::BTreeMap::new(),
        };

        let probe = probe_on_chain_token_capabilities(&caller, test_address(0x55)).await;
        assert_eq!(probe.method, CollectionMethod::Standard);
        assert_eq!(probe.token_name, None);
        assert_eq!(probe.token_version, None);
    }

    #[tokio::test]
    async fn resolve_optimal_collection_strategy_with_probe_unknown_token() {
        let mut caller = MockContractCaller {
            responses: std::collections::BTreeMap::new(),
        };
        caller
            .responses
            .insert(EIP3009_AUTHORIZATION_STATE_SELECTOR.to_vec(), vec![0u8; 32]);
        caller.responses.insert(
            ERC20_NAME_SELECTOR.to_vec(),
            encode_abi_string("Custom Circle USDC"),
        );
        caller
            .responses
            .insert(ERC20_VERSION_SELECTOR.to_vec(), encode_abi_string("2"));

        let unknown_addr = test_address(0x77);
        let res = resolve_optimal_collection_strategy_with_probe(
            99999, // Unknown custom chain
            unknown_addr,
            "USDC",
            true,
            CollectionMethod::Auto,
            None,
            None,
            Some(&caller),
        )
        .await;

        assert_eq!(res.method, CollectionMethod::Eip3009);
        assert_eq!(res.token_name, "Custom Circle USDC");
        assert_eq!(res.token_version, "2");
        assert!(res.reason.contains("On-chain contract probe confirmed"));
    }
}


