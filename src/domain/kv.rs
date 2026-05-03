use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum KvReorgEpochError {
    #[error("kv reorg epoch overflow")]
    Overflow,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct KvReorgEpoch {
    pub epoch: u64,
    pub last_reorg_from: Option<u64>,
}

impl KvReorgEpoch {
    pub const ZERO: Self = Self {
        epoch: 0,
        last_reorg_from: None,
    };

    pub const fn new(epoch: u64, last_reorg_from: Option<u64>) -> Self {
        Self {
            epoch,
            last_reorg_from,
        }
    }

    pub fn bump(self, last_reorg_from: u64) -> Result<Self, KvReorgEpochError> {
        Ok(Self {
            epoch: self
                .epoch
                .checked_add(1)
                .ok_or(KvReorgEpochError::Overflow)?,
            last_reorg_from: Some(last_reorg_from),
        })
    }

    pub fn is_newer_than(self, seen_epoch: u64) -> bool {
        self.epoch > seen_epoch
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bump_increments_epoch_and_records_reorg_floor() {
        let bumped = KvReorgEpoch::ZERO.bump(42).unwrap();
        assert_eq!(bumped.epoch, 1);
        assert_eq!(bumped.last_reorg_from, Some(42));
        assert!(bumped.is_newer_than(0));
    }

    #[test]
    fn bump_detects_overflow() {
        let epoch = KvReorgEpoch::new(u64::MAX, None);
        assert_eq!(epoch.bump(0), Err(KvReorgEpochError::Overflow));
    }
}
