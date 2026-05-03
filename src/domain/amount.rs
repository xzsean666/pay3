use std::{fmt, str::FromStr};

use alloy_primitives::U256;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AmountParseError {
    #[error("amount is empty")]
    Empty,
    #[error("amount must not be negative")]
    Negative,
    #[error("amount has invalid decimal format")]
    InvalidFormat,
    #[error("amount contains non-decimal digits")]
    InvalidDigits,
    #[error("fractional precision {actual} exceeds token decimals {decimals}")]
    TooManyFractionalDigits { actual: usize, decimals: u8 },
    #[error("amount overflows uint256")]
    Overflow,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RawAmount(U256);

impl RawAmount {
    pub const ZERO: Self = Self(U256::ZERO);

    pub const fn new(value: U256) -> Self {
        Self(value)
    }

    pub const fn value(self) -> U256 {
        self.0
    }

    pub fn parse_dec_str(input: &str) -> Result<Self, AmountParseError> {
        let s = input.trim();
        if s.is_empty() {
            return Err(AmountParseError::Empty);
        }
        if s.starts_with('-') {
            return Err(AmountParseError::Negative);
        }
        if s.starts_with('+') || s.contains('.') {
            return Err(AmountParseError::InvalidFormat);
        }
        parse_decimal_u256(s).map(Self)
    }

    pub fn checked_add(self, rhs: Self) -> Option<Self> {
        self.0.checked_add(rhs.0).map(Self)
    }

    pub fn checked_sub(self, rhs: Self) -> Option<Self> {
        self.0.checked_sub(rhs.0).map(Self)
    }

    pub fn is_zero(self) -> bool {
        self.0 == U256::ZERO
    }
}

impl From<u64> for RawAmount {
    fn from(value: u64) -> Self {
        Self(U256::from(value))
    }
}

impl FromStr for RawAmount {
    type Err = AmountParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse_dec_str(s)
    }
}

impl fmt::Display for RawAmount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Serialize for RawAmount {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for RawAmount {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct RawAmountVisitor;

        impl de::Visitor<'_> for RawAmountVisitor {
            type Value = RawAmount;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a decimal raw token amount string")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                RawAmount::parse_dec_str(value).map_err(E::custom)
            }

            fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                self.visit_str(&value)
            }

            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(RawAmount::from(value))
            }
        }

        deserializer.deserialize_any(RawAmountVisitor)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TokenAmount {
    pub raw: RawAmount,
    pub decimals: u8,
}

impl TokenAmount {
    pub const fn from_raw(raw: RawAmount, decimals: u8) -> Self {
        Self { raw, decimals }
    }

    pub fn parse(input: &str, decimals: u8) -> Result<Self, AmountParseError> {
        Ok(Self {
            raw: parse_token_units(input, decimals)?,
            decimals,
        })
    }

    pub fn to_decimal_string(self) -> String {
        let raw = self.raw.to_string();
        let decimals = usize::from(self.decimals);
        if decimals == 0 {
            return raw;
        }

        let mut value = if raw.len() <= decimals {
            let zeros = "0".repeat(decimals - raw.len());
            format!("0.{zeros}{raw}")
        } else {
            let split_at = raw.len() - decimals;
            format!("{}.{}", &raw[..split_at], &raw[split_at..])
        };

        while value.ends_with('0') {
            value.pop();
        }
        if value.ends_with('.') {
            value.pop();
        }
        value
    }
}

impl fmt::Display for TokenAmount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_decimal_string())
    }
}

fn parse_token_units(input: &str, decimals: u8) -> Result<RawAmount, AmountParseError> {
    let s = input.trim();
    if s.is_empty() {
        return Err(AmountParseError::Empty);
    }
    if s.starts_with('-') {
        return Err(AmountParseError::Negative);
    }
    if s.starts_with('+') {
        return Err(AmountParseError::InvalidFormat);
    }

    let mut parts = s.split('.');
    let whole = parts.next().unwrap_or_default();
    let fraction = parts.next();
    if parts.next().is_some() {
        return Err(AmountParseError::InvalidFormat);
    }

    if whole.is_empty() && fraction.unwrap_or_default().is_empty() {
        return Err(AmountParseError::Empty);
    }
    if !whole.is_empty() && !whole.bytes().all(|b| b.is_ascii_digit()) {
        return Err(AmountParseError::InvalidDigits);
    }

    let fraction = fraction.unwrap_or_default();
    if !fraction.bytes().all(|b| b.is_ascii_digit()) {
        return Err(AmountParseError::InvalidDigits);
    }
    if fraction.len() > usize::from(decimals) {
        return Err(AmountParseError::TooManyFractionalDigits {
            actual: fraction.len(),
            decimals,
        });
    }

    let scale = pow10_u256(decimals)?;
    let whole_raw = if whole.is_empty() {
        U256::ZERO
    } else {
        parse_decimal_u256(whole)?
    };
    let mut raw = whole_raw
        .checked_mul(scale)
        .ok_or(AmountParseError::Overflow)?;

    if !fraction.is_empty() {
        let fractional_raw = parse_decimal_u256(fraction)?;
        let fractional_scale = pow10_u256(decimals - fraction.len() as u8)?;
        raw = raw
            .checked_add(
                fractional_raw
                    .checked_mul(fractional_scale)
                    .ok_or(AmountParseError::Overflow)?,
            )
            .ok_or(AmountParseError::Overflow)?;
    }

    Ok(RawAmount::new(raw))
}

fn parse_decimal_u256(s: &str) -> Result<U256, AmountParseError> {
    if s.is_empty() {
        return Err(AmountParseError::Empty);
    }
    if !s.bytes().all(|b| b.is_ascii_digit()) {
        return Err(AmountParseError::InvalidDigits);
    }
    U256::from_str_radix(s, 10).map_err(|_| AmountParseError::Overflow)
}

fn pow10_u256(decimals: u8) -> Result<U256, AmountParseError> {
    let mut value = U256::from(1u8);
    for _ in 0..decimals {
        value = value
            .checked_mul(U256::from(10u8))
            .ok_or(AmountParseError::Overflow)?;
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_amount_parses_decimal_integer_without_float_syntax() {
        assert_eq!("123456".parse::<RawAmount>().unwrap().to_string(), "123456");
        assert!("1.0".parse::<RawAmount>().is_err());
        assert!("1e6".parse::<RawAmount>().is_err());
        assert!("-1".parse::<RawAmount>().is_err());
    }

    #[test]
    fn token_amount_parses_decimal_units_without_float_math() {
        let amount = TokenAmount::parse("12.345600", 6).unwrap();
        assert_eq!(amount.raw, RawAmount::from(12_345_600));
        assert_eq!(amount.to_decimal_string(), "12.3456");

        let subunit = TokenAmount::parse(".000001", 6).unwrap();
        assert_eq!(subunit.raw, RawAmount::from(1));
    }

    #[test]
    fn token_amount_rejects_excess_fractional_precision() {
        let err = TokenAmount::parse("1.001", 2).unwrap_err();
        assert_eq!(
            err,
            AmountParseError::TooManyFractionalDigits {
                actual: 3,
                decimals: 2
            }
        );
    }

    #[test]
    fn token_amount_detects_uint256_overflow() {
        let err = TokenAmount::parse("2", 78).unwrap_err();
        assert_eq!(err, AmountParseError::Overflow);
    }

    #[test]
    fn raw_amount_serializes_as_decimal_string() {
        let json = serde_json::to_string(&RawAmount::from(42)).unwrap();
        assert_eq!(json, "\"42\"");
        let decoded: RawAmount = serde_json::from_str("\"42\"").unwrap();
        assert_eq!(decoded, RawAmount::from(42));
    }
}
