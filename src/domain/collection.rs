use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{EvmAddress, RawAmount};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CollectionStatus {
    Pending,
    Signing,
    Signed,
    Broadcast,
    Confirmed,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CollectionPurpose {
    TreasurySweep,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CollectionFees {
    pub gas_limit: u64,
    pub max_fee_per_gas: RawAmount,
    pub max_priority_fee_per_gas: RawAmount,
}

impl CollectionFees {
    pub const fn new(
        gas_limit: u64,
        max_fee_per_gas: RawAmount,
        max_priority_fee_per_gas: RawAmount,
    ) -> Self {
        Self {
            gas_limit,
            max_fee_per_gas,
            max_priority_fee_per_gas,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CollectionTxPlan {
    pub chain_id: u64,
    pub nonce: u64,
    pub from: EvmAddress,
    pub to: EvmAddress,
    pub amount: RawAmount,
    pub purpose: CollectionPurpose,
    pub fees: CollectionFees,
}

impl CollectionTxPlan {
    pub const fn new(
        chain_id: u64,
        nonce: u64,
        from: EvmAddress,
        to: EvmAddress,
        amount: RawAmount,
        purpose: CollectionPurpose,
        fees: CollectionFees,
    ) -> Self {
        Self {
            chain_id,
            nonce,
            from,
            to,
            amount,
            purpose,
            fees,
        }
    }

    pub fn assert_replacement_allowed(
        self,
        replacement: Self,
    ) -> Result<(), CollectionReplacementError> {
        if self.chain_id != replacement.chain_id {
            return Err(CollectionReplacementError::ChainIdChanged {
                existing: self.chain_id,
                replacement: replacement.chain_id,
            });
        }
        if self.nonce != replacement.nonce {
            return Err(CollectionReplacementError::NonceChanged {
                existing: self.nonce,
                replacement: replacement.nonce,
            });
        }
        if self.from != replacement.from {
            return Err(CollectionReplacementError::FromChanged {
                existing: self.from,
                replacement: replacement.from,
            });
        }
        if self.to != replacement.to {
            return Err(CollectionReplacementError::ToChanged {
                existing: self.to,
                replacement: replacement.to,
            });
        }
        if self.amount != replacement.amount {
            return Err(CollectionReplacementError::AmountChanged {
                existing: self.amount,
                replacement: replacement.amount,
            });
        }
        if self.purpose != replacement.purpose {
            return Err(CollectionReplacementError::PurposeChanged {
                existing: self.purpose,
                replacement: replacement.purpose,
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CollectionReplacementError {
    #[error("collection replacement cannot change chain id ({existing} -> {replacement})")]
    ChainIdChanged { existing: u64, replacement: u64 },
    #[error("collection replacement must use the same nonce ({existing} -> {replacement})")]
    NonceChanged { existing: u64, replacement: u64 },
    #[error("collection replacement cannot change from address ({existing} -> {replacement})")]
    FromChanged {
        existing: EvmAddress,
        replacement: EvmAddress,
    },
    #[error("collection replacement cannot change to address ({existing} -> {replacement})")]
    ToChanged {
        existing: EvmAddress,
        replacement: EvmAddress,
    },
    #[error("collection replacement cannot change amount ({existing} -> {replacement})")]
    AmountChanged {
        existing: RawAmount,
        replacement: RawAmount,
    },
    #[error("collection replacement cannot change purpose ({existing:?} -> {replacement:?})")]
    PurposeChanged {
        existing: CollectionPurpose,
        replacement: CollectionPurpose,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn address(byte: u8) -> EvmAddress {
        EvmAddress::from_bytes([byte; 20])
    }

    fn fees(max_fee: u64) -> CollectionFees {
        CollectionFees::new(
            80_000,
            RawAmount::from(max_fee),
            RawAmount::from(max_fee / 10),
        )
    }

    fn plan() -> CollectionTxPlan {
        CollectionTxPlan::new(
            1,
            7,
            address(1),
            address(2),
            RawAmount::from(100),
            CollectionPurpose::TreasurySweep,
            fees(100),
        )
    }

    #[test]
    fn replacement_can_only_change_fees_for_the_same_nonce() {
        let original = plan();
        let replacement = CollectionTxPlan {
            fees: fees(200),
            ..original
        };

        assert_eq!(original.assert_replacement_allowed(replacement), Ok(()));
    }

    #[test]
    fn replacement_cannot_change_nonce_or_payment_invariants() {
        let original = plan();

        assert!(matches!(
            original.assert_replacement_allowed(CollectionTxPlan {
                nonce: 8,
                ..original
            }),
            Err(CollectionReplacementError::NonceChanged { .. })
        ));
        assert!(matches!(
            original.assert_replacement_allowed(CollectionTxPlan {
                from: address(3),
                ..original
            }),
            Err(CollectionReplacementError::FromChanged { .. })
        ));
        assert!(matches!(
            original.assert_replacement_allowed(CollectionTxPlan {
                to: address(4),
                ..original
            }),
            Err(CollectionReplacementError::ToChanged { .. })
        ));
        assert!(matches!(
            original.assert_replacement_allowed(CollectionTxPlan {
                amount: RawAmount::from(101),
                ..original
            }),
            Err(CollectionReplacementError::AmountChanged { .. })
        ));
    }
}
