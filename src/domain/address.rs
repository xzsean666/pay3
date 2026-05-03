use std::{cmp::Ordering, fmt, str::FromStr};

use alloy_primitives::{Address, B256};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum HexParseError {
    #[error("{kind} must start with 0x")]
    MissingPrefix { kind: &'static str },
    #[error("{kind} must contain {expected} hex chars, got {actual}")]
    InvalidLength {
        kind: &'static str,
        expected: usize,
        actual: usize,
    },
    #[error("{kind} contains invalid hex at char {index}")]
    InvalidHex { kind: &'static str, index: usize },
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct EvmAddress(Address);

impl EvmAddress {
    pub const ZERO: Self = Self(Address::ZERO);

    pub const fn from_alloy(value: Address) -> Self {
        Self(value)
    }

    pub const fn from_bytes(bytes: [u8; 20]) -> Self {
        Self(Address::new(bytes))
    }

    pub const fn as_alloy(&self) -> &Address {
        &self.0
    }

    pub const fn into_alloy(self) -> Address {
        self.0
    }

    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_slice()
    }

    pub fn to_lower_hex(self) -> String {
        encode_lower_prefixed(self.as_bytes())
    }
}

impl FromStr for EvmAddress {
    type Err = HexParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self::from_bytes(decode_prefixed_fixed::<20>(
            s,
            "evm address",
        )?))
    }
}

impl fmt::Display for EvmAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_lower_hex())
    }
}

impl fmt::Debug for EvmAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl Ord for EvmAddress {
    fn cmp(&self, other: &Self) -> Ordering {
        self.as_bytes().cmp(other.as_bytes())
    }
}

impl PartialOrd for EvmAddress {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Serialize for EvmAddress {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_lower_hex())
    }
}

impl<'de> Deserialize<'de> for EvmAddress {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserialize_hex_string(deserializer)
    }
}

macro_rules! fixed_hash_type {
    ($name:ident, $kind:literal) => {
        #[derive(Clone, Copy, PartialEq, Eq, Hash)]
        pub struct $name(B256);

        impl $name {
            pub const ZERO: Self = Self(B256::ZERO);

            pub const fn from_alloy(value: B256) -> Self {
                Self(value)
            }

            pub const fn from_bytes(bytes: [u8; 32]) -> Self {
                Self(B256::new(bytes))
            }

            pub const fn as_alloy(&self) -> &B256 {
                &self.0
            }

            pub const fn into_alloy(self) -> B256 {
                self.0
            }

            pub fn as_bytes(&self) -> &[u8] {
                self.0.as_slice()
            }

            pub fn to_lower_hex(self) -> String {
                encode_lower_prefixed(self.as_bytes())
            }
        }

        impl FromStr for $name {
            type Err = HexParseError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Ok(Self::from_bytes(decode_prefixed_fixed::<32>(s, $kind)?))
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.to_lower_hex())
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(self, f)
            }
        }

        impl Ord for $name {
            fn cmp(&self, other: &Self) -> Ordering {
                self.as_bytes().cmp(other.as_bytes())
            }
        }

        impl PartialOrd for $name {
            fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
                Some(self.cmp(other))
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_str(&self.to_lower_hex())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                deserialize_hex_string(deserializer)
            }
        }
    };
}

fixed_hash_type!(TxHash, "tx hash");
fixed_hash_type!(BlockHash, "block hash");

fn deserialize_hex_string<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: FromStr,
    T::Err: fmt::Display,
{
    let value = String::deserialize(deserializer)?;
    value.parse().map_err(de::Error::custom)
}

fn decode_prefixed_fixed<const N: usize>(
    input: &str,
    kind: &'static str,
) -> Result<[u8; N], HexParseError> {
    let hex = input
        .strip_prefix("0x")
        .or_else(|| input.strip_prefix("0X"))
        .ok_or(HexParseError::MissingPrefix { kind })?;

    let expected = N * 2;
    if hex.len() != expected {
        return Err(HexParseError::InvalidLength {
            kind,
            expected,
            actual: hex.len(),
        });
    }

    let mut bytes = [0u8; N];
    for (index, chunk) in hex.as_bytes().chunks_exact(2).enumerate() {
        let high = hex_value(chunk[0]).ok_or(HexParseError::InvalidHex {
            kind,
            index: index * 2,
        })?;
        let low = hex_value(chunk[1]).ok_or(HexParseError::InvalidHex {
            kind,
            index: index * 2 + 1,
        })?;
        bytes[index] = (high << 4) | low;
    }
    Ok(bytes)
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn encode_lower_prefixed(bytes: &[u8]) -> String {
    const TABLE: &[u8; 16] = b"0123456789abcdef";

    let mut output = String::with_capacity(2 + bytes.len() * 2);
    output.push_str("0x");
    for byte in bytes {
        output.push(TABLE[(byte >> 4) as usize] as char);
        output.push(TABLE[(byte & 0x0f) as usize] as char);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evm_address_normalizes_to_lowercase_0x_hex() {
        let address: EvmAddress = "0XAbCdEf0000000000000000000000000000001234"
            .parse()
            .unwrap();
        assert_eq!(
            address.to_string(),
            "0xabcdef0000000000000000000000000000001234"
        );
    }

    #[test]
    fn tx_and_block_hash_normalize_to_lowercase_0x_hex() {
        let input = "0XABCDEF0000000000000000000000000000000000000000000000000000001234";
        let tx: TxHash = input.parse().unwrap();
        let block: BlockHash = input.parse().unwrap();

        assert_eq!(
            tx.to_string(),
            "0xabcdef0000000000000000000000000000000000000000000000000000001234"
        );
        assert_eq!(block.to_string(), tx.to_string());
    }

    #[test]
    fn hex_types_reject_missing_prefix_or_wrong_length() {
        assert!(matches!(
            "abcdef".parse::<EvmAddress>(),
            Err(HexParseError::MissingPrefix { .. })
        ));
        assert!(matches!(
            "0xabc".parse::<TxHash>(),
            Err(HexParseError::InvalidLength { .. })
        ));
    }

    #[test]
    fn evm_address_serializes_as_normalized_string() {
        let address: EvmAddress = "0xABCDEF0000000000000000000000000000001234"
            .parse()
            .unwrap();
        let json = serde_json::to_string(&address).unwrap();
        assert_eq!(json, "\"0xabcdef0000000000000000000000000000001234\"");

        let decoded: EvmAddress = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, address);
    }
}
