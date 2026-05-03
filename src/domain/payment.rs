use serde::{Deserialize, Serialize};

use super::RawAmount;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaymentMatchStatus {
    OnTime,
    Late,
    OutsideWindow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaymentChainStatus {
    Observed,
    Confirmed,
    Orphaned,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PaymentFact {
    pub amount: RawAmount,
    pub match_status: PaymentMatchStatus,
    pub chain_status: PaymentChainStatus,
}

impl PaymentFact {
    pub const fn new(
        amount: RawAmount,
        match_status: PaymentMatchStatus,
        chain_status: PaymentChainStatus,
    ) -> Self {
        Self {
            amount,
            match_status,
            chain_status,
        }
    }

    pub fn is_confirmed_on_time(self) -> bool {
        self.match_status == PaymentMatchStatus::OnTime
            && self.chain_status == PaymentChainStatus::Confirmed
    }

    pub fn is_non_orphaned_on_time(self) -> bool {
        self.match_status == PaymentMatchStatus::OnTime
            && self.chain_status != PaymentChainStatus::Orphaned
    }
}
