use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const MAX_DERIVATION_INDEX: u32 = (1u32 << 31) - 1;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DerivationSegmentError {
    #[error("{field} index {value} exceeds max derivation index {max}")]
    IndexOutOfRange {
        field: &'static str,
        value: u32,
        max: u32,
    },
    #[error("derivation segment exhausted")]
    Exhausted,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DerivationSegment {
    pub account_index: u32,
    pub change_index: u32,
    pub address_index: u32,
}

impl DerivationSegment {
    pub const ZERO: Self = Self {
        account_index: 0,
        change_index: 0,
        address_index: 0,
    };

    pub fn new(
        account_index: u32,
        change_index: u32,
        address_index: u32,
    ) -> Result<Self, DerivationSegmentError> {
        validate_index("account", account_index)?;
        validate_index("change", change_index)?;
        validate_index("address", address_index)?;
        Ok(Self {
            account_index,
            change_index,
            address_index,
        })
    }

    pub fn derivation_path(self) -> String {
        format!(
            "m/44'/60'/{}'/{}/{}",
            self.account_index, self.change_index, self.address_index
        )
    }

    pub fn next(self) -> Result<Self, DerivationSegmentError> {
        if self.address_index < MAX_DERIVATION_INDEX {
            return Self::new(
                self.account_index,
                self.change_index,
                self.address_index + 1,
            );
        }

        if self.change_index < MAX_DERIVATION_INDEX {
            return Self::new(self.account_index, self.change_index + 1, 0);
        }

        if self.account_index < MAX_DERIVATION_INDEX {
            return Self::new(self.account_index + 1, 0, 0);
        }

        Err(DerivationSegmentError::Exhausted)
    }

    pub fn take_current_and_advance(&mut self) -> Result<Self, DerivationSegmentError> {
        let current = *self;
        *self = self.next()?;
        Ok(current)
    }
}

fn validate_index(field: &'static str, value: u32) -> Result<(), DerivationSegmentError> {
    if value > MAX_DERIVATION_INDEX {
        return Err(DerivationSegmentError::IndexOutOfRange {
            field,
            value,
            max: MAX_DERIVATION_INDEX,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derivation_path_matches_mvp_template() {
        let segment = DerivationSegment::new(7, 8, 9).unwrap();
        assert_eq!(segment.derivation_path(), "m/44'/60'/7'/8/9");
    }

    #[test]
    fn address_index_rolls_over_to_change_index() {
        let segment = DerivationSegment::new(0, 0, MAX_DERIVATION_INDEX).unwrap();
        assert_eq!(
            segment.next().unwrap(),
            DerivationSegment::new(0, 1, 0).unwrap()
        );
    }

    #[test]
    fn change_index_rolls_over_to_account_index() {
        let segment =
            DerivationSegment::new(0, MAX_DERIVATION_INDEX, MAX_DERIVATION_INDEX).unwrap();
        assert_eq!(
            segment.next().unwrap(),
            DerivationSegment::new(1, 0, 0).unwrap()
        );
    }

    #[test]
    fn segment_exhaustion_is_reported() {
        let segment = DerivationSegment::new(
            MAX_DERIVATION_INDEX,
            MAX_DERIVATION_INDEX,
            MAX_DERIVATION_INDEX,
        )
        .unwrap();
        assert_eq!(segment.next(), Err(DerivationSegmentError::Exhausted));
    }

    #[test]
    fn invalid_bip32_public_index_is_rejected() {
        assert!(matches!(
            DerivationSegment::new(0, 0, MAX_DERIVATION_INDEX + 1),
            Err(DerivationSegmentError::IndexOutOfRange {
                field: "address",
                ..
            })
        ));
    }
}
