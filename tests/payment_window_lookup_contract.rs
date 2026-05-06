pub mod db {
    pub use pay3::db::*;
}

pub mod domain {
    pub use pay3::domain::*;
}

#[allow(dead_code)]
#[path = "../src/services/payment_windows.rs"]
mod payment_windows;

use async_trait::async_trait;
use pay3::{
    db::repositories::PaymentWindowCandidate,
    domain::{BlockHash, ChainBlockRef, EvmAddress, OrderStatus, RawAmount},
};
use payment_windows::{PaymentWindowLookup, PaymentWindowLookupError, WatchSetPaymentWindowLookup};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

type LookupCall = (u64, EvmAddress, Vec<EvmAddress>);

#[derive(Clone, Debug, Default)]
struct ContractFallback {
    candidates: BTreeMap<(u64, EvmAddress, EvmAddress), Vec<PaymentWindowCandidate>>,
    calls: Arc<Mutex<Vec<LookupCall>>>,
}

impl ContractFallback {
    fn insert(&mut self, candidate: PaymentWindowCandidate) {
        self.candidates
            .entry((
                candidate.chain_id,
                candidate.token_address,
                candidate.receive_address,
            ))
            .or_default()
            .push(candidate);
    }

    fn calls(&self) -> Vec<LookupCall> {
        self.calls.lock().expect("calls lock poisoned").clone()
    }
}

#[async_trait]
impl PaymentWindowLookup for ContractFallback {
    async fn lookup_batch(
        &self,
        chain_id: u64,
        token_address: EvmAddress,
        to_addresses: &[EvmAddress],
    ) -> Result<Vec<PaymentWindowCandidate>, PaymentWindowLookupError> {
        self.calls.lock().expect("calls lock poisoned").push((
            chain_id,
            token_address,
            to_addresses.to_vec(),
        ));

        Ok(to_addresses
            .iter()
            .flat_map(|receive_address| {
                self.candidates
                    .get(&(chain_id, token_address, *receive_address))
                    .into_iter()
                    .flatten()
                    .cloned()
            })
            .collect())
    }
}

#[tokio::test]
async fn watch_set_lookup_batches_only_deduplicated_misses() {
    let chain_id = 1;
    let token = address(9);
    let hit = candidate(1, chain_id, token, address(10));
    let fallback_candidate = candidate(2, chain_id, token, address(20));

    let mut fallback = ContractFallback::default();
    fallback.insert(fallback_candidate.clone());
    let fallback_observer = fallback.clone();

    let lookup = WatchSetPaymentWindowLookup::new(fallback);
    lookup.insert_candidate(hit.clone());

    let candidates = lookup
        .lookup_batch(
            chain_id,
            token,
            &[address(10), address(20), address(20), address(10)],
        )
        .await
        .unwrap();

    assert_eq!(candidates, vec![hit, fallback_candidate]);
    assert_eq!(
        fallback_observer.calls(),
        vec![(chain_id, token, vec![address(20)])],
        "fallback must be called once for the deduplicated miss set"
    );
}

fn candidate(
    seed: u8,
    chain_id: u64,
    token_address: EvmAddress,
    receive_address: EvmAddress,
) -> PaymentWindowCandidate {
    let now = OffsetDateTime::UNIX_EPOCH + Duration::seconds(i64::from(seed));

    PaymentWindowCandidate {
        order_id: Uuid::from_u128(u128::from(seed)),
        child_account_id: Uuid::from_u128(u128::from(seed) + 100),
        receive_address,
        chain_id,
        token_address,
        expected_amount_raw: RawAmount::from(1000 + u64::from(seed)),
        paid_amount_raw: RawAmount::ZERO,
        order_status: OrderStatus::Pending,
        window_from: now,
        window_from_block: ChainBlockRef::new(u64::from(seed), BlockHash::from_bytes([seed; 32])),
        expires_at: now + Duration::minutes(15),
        monitor_until: now + Duration::minutes(30),
    }
}

const fn address(seed: u8) -> EvmAddress {
    EvmAddress::from_bytes([seed; 20])
}
