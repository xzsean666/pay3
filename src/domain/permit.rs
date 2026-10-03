//! EIP-712 domain separators, type hashes, and ABI calldata encoders for
//! gasless token collections (EIP-3009 transferWithAuthorization, Polygon
//! executeMetaTransaction, and EIP-2612 permit).

use alloy_primitives::{U256, keccak256};
use serde::{Deserialize, Serialize};

use crate::domain::{EvmAddress, RawAmount};

/// Supported fund collection methods.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CollectionMethod {
    /// Standard ERC-20 `transfer(treasury, amount)`.
    /// Sent directly by the child address, requiring native gas on child address.
    #[default]
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
        !matches!(self, Self::Standard)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::Eip3009 => "eip3009",
            Self::PolygonMetaTx => "polygon_meta_tx",
            Self::Eip2612 => "eip2612",
        }
    }

    pub fn parse(value: &str) -> Result<Self, String> {
        match value.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "standard" | "direct" | "erc20" => Ok(Self::Standard),
            "eip3009" | "eip_3009" | "usdc" => Ok(Self::Eip3009),
            "polygon_meta_tx" | "polygon" | "meta_tx" | "pos_meta_tx" => Ok(Self::PolygonMetaTx),
            "eip2612" | "eip_2612" | "permit" => Ok(Self::Eip2612),
            _ => Err(format!(
                "expected one of standard, eip3009, polygon_meta_tx, eip2612; got '{value}'"
            )),
        }
    }
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
}
