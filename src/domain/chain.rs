use serde::{Deserialize, Serialize};

use super::BlockHash;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ChainBlockRef {
    pub number: u64,
    pub hash: BlockHash,
}

impl ChainBlockRef {
    pub const fn new(number: u64, hash: BlockHash) -> Self {
        Self { number, hash }
    }

    pub fn confirmations_against(self, canonical_head: Self) -> Option<u64> {
        if canonical_head.number < self.number {
            return None;
        }
        Some(canonical_head.number - self.number + 1)
    }

    pub fn has_confirmations(self, canonical_head: Self, required_confirmations: u64) -> bool {
        required_confirmations == 0
            || self
                .confirmations_against(canonical_head)
                .is_some_and(|confirmations| confirmations >= required_confirmations)
    }

    pub fn same_block(self, other: Self) -> bool {
        self.number == other.number && self.hash == other.hash
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(number: u64) -> ChainBlockRef {
        ChainBlockRef::new(number, BlockHash::from_bytes([number as u8; 32]))
    }

    #[test]
    fn confirmations_include_the_block_itself() {
        assert_eq!(block(10).confirmations_against(block(10)), Some(1));
        assert_eq!(block(10).confirmations_against(block(12)), Some(3));
        assert!(block(10).has_confirmations(block(12), 3));
        assert!(!block(10).has_confirmations(block(12), 4));
    }

    #[test]
    fn future_block_has_no_confirmations_against_older_head() {
        assert_eq!(block(12).confirmations_against(block(10)), None);
    }
}
