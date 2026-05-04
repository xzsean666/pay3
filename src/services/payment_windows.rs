//! Payment-window lookup support for scanned transfer matching.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::RwLock,
};

use async_trait::async_trait;
use thiserror::Error;

use crate::{
    db::repositories::{PaymentWindowCandidate, PaymentWindowCandidateRepository, RepositoryError},
    domain::EvmAddress,
};

#[derive(Debug, Error)]
pub enum PaymentWindowLookupError {
    #[error("invalid payment window lookup config {field}: {message}")]
    InvalidConfig {
        field: &'static str,
        message: String,
    },

    #[error(transparent)]
    Repository(#[from] RepositoryError),
}

#[async_trait]
pub trait PaymentWindowLookup: Send + Sync {
    async fn lookup_batch(
        &self,
        chain_id: u64,
        token_address: EvmAddress,
        to_addresses: &[EvmAddress],
    ) -> Result<Vec<PaymentWindowCandidate>, PaymentWindowLookupError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct PaymentWindowKey {
    chain_id: u64,
    token_address: EvmAddress,
    receive_address: EvmAddress,
}

impl PaymentWindowKey {
    const fn new(chain_id: u64, token_address: EvmAddress, receive_address: EvmAddress) -> Self {
        Self {
            chain_id,
            token_address,
            receive_address,
        }
    }

    const fn from_candidate(candidate: &PaymentWindowCandidate) -> Self {
        Self::new(
            candidate.chain_id,
            candidate.token_address,
            candidate.receive_address,
        )
    }
}

#[derive(Debug)]
pub struct WatchSetPaymentWindowLookup<F> {
    watch_set: RwLock<BTreeMap<PaymentWindowKey, Vec<PaymentWindowCandidate>>>,
    fallback: F,
}

impl<F> WatchSetPaymentWindowLookup<F> {
    pub const fn new(fallback: F) -> Self {
        Self {
            watch_set: RwLock::new(BTreeMap::new()),
            fallback,
        }
    }

    pub fn fallback(&self) -> &F {
        &self.fallback
    }

    pub fn replace_watch_set<I>(&self, candidates: I)
    where
        I: IntoIterator<Item = PaymentWindowCandidate>,
    {
        let mut watch_set = self.watch_set.write().expect("watch set lock poisoned");
        watch_set.clear();
        for candidate in candidates {
            watch_set
                .entry(PaymentWindowKey::from_candidate(&candidate))
                .or_default()
                .push(candidate);
        }
    }

    pub fn insert_candidate(&self, candidate: PaymentWindowCandidate) {
        self.watch_set
            .write()
            .expect("watch set lock poisoned")
            .entry(PaymentWindowKey::from_candidate(&candidate))
            .or_default()
            .push(candidate);
    }

    pub fn remove_address(
        &self,
        chain_id: u64,
        token_address: EvmAddress,
        receive_address: EvmAddress,
    ) -> Vec<PaymentWindowCandidate> {
        self.watch_set
            .write()
            .expect("watch set lock poisoned")
            .remove(&PaymentWindowKey::new(
                chain_id,
                token_address,
                receive_address,
            ))
            .unwrap_or_default()
    }

    pub fn watch_set_len(&self) -> usize {
        self.watch_set
            .read()
            .expect("watch set lock poisoned")
            .values()
            .map(Vec::len)
            .sum()
    }
}

#[async_trait]
impl<F> PaymentWindowLookup for WatchSetPaymentWindowLookup<F>
where
    F: PaymentWindowLookup,
{
    async fn lookup_batch(
        &self,
        chain_id: u64,
        token_address: EvmAddress,
        to_addresses: &[EvmAddress],
    ) -> Result<Vec<PaymentWindowCandidate>, PaymentWindowLookupError> {
        let to_addresses = deduplicate_addresses(to_addresses);
        if to_addresses.is_empty() {
            return Ok(Vec::new());
        }

        let mut candidates = Vec::new();
        let mut misses = Vec::new();

        {
            let watch_set = self.watch_set.read().expect("watch set lock poisoned");
            for receive_address in to_addresses {
                let key = PaymentWindowKey::new(chain_id, token_address, receive_address);
                let Some(hits) = watch_set.get(&key) else {
                    misses.push(receive_address);
                    continue;
                };

                let before_len = candidates.len();
                candidates.extend(
                    hits.iter()
                        .filter(|candidate| candidate_matches(candidate, key))
                        .cloned(),
                );

                if candidates.len() == before_len {
                    misses.push(receive_address);
                }
            }
        }

        if !misses.is_empty() {
            candidates.extend(
                self.fallback
                    .lookup_batch(chain_id, token_address, &misses)
                    .await?
                    .into_iter()
                    .filter(|candidate| {
                        candidate_is_for_lookup(candidate, chain_id, token_address)
                    }),
            );
        }

        Ok(candidates)
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct EmptyPaymentWindowLookup;

#[async_trait]
impl PaymentWindowLookup for EmptyPaymentWindowLookup {
    async fn lookup_batch(
        &self,
        _chain_id: u64,
        _token_address: EvmAddress,
        _to_addresses: &[EvmAddress],
    ) -> Result<Vec<PaymentWindowCandidate>, PaymentWindowLookupError> {
        Ok(Vec::new())
    }
}

#[derive(Clone, Debug)]
pub struct RepositoryPaymentWindowLookup<R> {
    repository: R,
    max_addresses: usize,
}

impl<R> RepositoryPaymentWindowLookup<R> {
    pub const fn new(repository: R, max_addresses: usize) -> Self {
        Self {
            repository,
            max_addresses,
        }
    }

    pub const fn max_addresses(&self) -> usize {
        self.max_addresses
    }
}

#[async_trait]
impl<R> PaymentWindowLookup for RepositoryPaymentWindowLookup<R>
where
    R: PaymentWindowCandidateRepository,
{
    async fn lookup_batch(
        &self,
        chain_id: u64,
        token_address: EvmAddress,
        to_addresses: &[EvmAddress],
    ) -> Result<Vec<PaymentWindowCandidate>, PaymentWindowLookupError> {
        if self.max_addresses == 0 {
            return Err(PaymentWindowLookupError::InvalidConfig {
                field: "max_addresses",
                message: "must be greater than zero".to_string(),
            });
        }

        let to_addresses = deduplicate_addresses(to_addresses);
        if to_addresses.len() > self.max_addresses {
            return Err(PaymentWindowLookupError::InvalidConfig {
                field: "to_addresses",
                message: format!(
                    "received {} addresses, max {}",
                    to_addresses.len(),
                    self.max_addresses
                ),
            });
        }

        Ok(self
            .repository
            .lookup_payment_window_candidates(chain_id, token_address, &to_addresses)
            .await?)
    }
}

fn deduplicate_addresses(to_addresses: &[EvmAddress]) -> Vec<EvmAddress> {
    let mut seen = BTreeSet::new();
    let mut deduped = Vec::new();

    for address in to_addresses {
        if seen.insert(*address) {
            deduped.push(*address);
        }
    }

    deduped
}

fn candidate_matches(candidate: &PaymentWindowCandidate, key: PaymentWindowKey) -> bool {
    candidate.chain_id == key.chain_id
        && candidate.token_address == key.token_address
        && candidate.receive_address == key.receive_address
}

fn candidate_is_for_lookup(
    candidate: &PaymentWindowCandidate,
    chain_id: u64,
    token_address: EvmAddress,
) -> bool {
    candidate.chain_id == chain_id && candidate.token_address == token_address
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::domain::{BlockHash, ChainBlockRef, OrderStatus, RawAmount};
    use time::{Duration, OffsetDateTime};
    use uuid::Uuid;

    #[derive(Debug, Default)]
    struct FakeFallbackPaymentWindowLookup {
        candidates: BTreeMap<PaymentWindowKey, Vec<PaymentWindowCandidate>>,
        calls: Mutex<Vec<(u64, EvmAddress, Vec<EvmAddress>)>>,
    }

    impl FakeFallbackPaymentWindowLookup {
        fn insert(&mut self, candidate: PaymentWindowCandidate) {
            self.candidates
                .entry(PaymentWindowKey::from_candidate(&candidate))
                .or_default()
                .push(candidate);
        }

        fn calls(&self) -> Vec<(u64, EvmAddress, Vec<EvmAddress>)> {
            self.calls.lock().expect("calls lock poisoned").clone()
        }
    }

    #[async_trait]
    impl PaymentWindowLookup for FakeFallbackPaymentWindowLookup {
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

            let mut candidates = Vec::new();
            for receive_address in to_addresses {
                if let Some(hits) = self.candidates.get(&PaymentWindowKey::new(
                    chain_id,
                    token_address,
                    *receive_address,
                )) {
                    candidates.extend(hits.iter().cloned());
                }
            }
            Ok(candidates)
        }
    }

    #[tokio::test]
    async fn watch_set_hits_are_returned_and_misses_fall_back_in_one_batch() {
        let chain_id = 1;
        let token = address(9);
        let hit = candidate(1, chain_id, token, address(10));
        let fallback_one = candidate(2, chain_id, token, address(20));
        let fallback_two = candidate(3, chain_id, token, address(30));

        let mut fallback = FakeFallbackPaymentWindowLookup::default();
        fallback.insert(fallback_one.clone());
        fallback.insert(fallback_two.clone());

        let lookup = WatchSetPaymentWindowLookup::new(fallback);
        lookup.insert_candidate(hit.clone());

        let candidates = lookup
            .lookup_batch(
                chain_id,
                token,
                &[
                    address(10),
                    address(20),
                    address(20),
                    address(30),
                    address(10),
                ],
            )
            .await
            .unwrap();

        assert_eq!(candidates, vec![hit, fallback_one, fallback_two]);
        assert_eq!(
            lookup.fallback().calls(),
            vec![(chain_id, token, vec![address(20), address(30)])],
            "fallback must be called once with the deduplicated misses, not once per address"
        );
    }

    #[tokio::test]
    async fn lookup_preserves_chain_and_token_constraints() {
        let requested_chain = 1;
        let other_chain = 2;
        let requested_token = address(9);
        let other_token = address(8);
        let receive_address = address(10);
        let fallback_candidate = candidate(3, requested_chain, requested_token, receive_address);

        let mut fallback = FakeFallbackPaymentWindowLookup::default();
        fallback.insert(fallback_candidate.clone());

        let lookup = WatchSetPaymentWindowLookup::new(fallback);
        lookup.insert_candidate(candidate(1, other_chain, requested_token, receive_address));
        lookup.insert_candidate(candidate(2, requested_chain, other_token, receive_address));

        let candidates = lookup
            .lookup_batch(requested_chain, requested_token, &[receive_address])
            .await
            .unwrap();

        assert_eq!(candidates, vec![fallback_candidate]);
        assert_eq!(
            lookup.fallback().calls(),
            vec![(requested_chain, requested_token, vec![receive_address])]
        );
    }

    #[tokio::test]
    async fn empty_lookup_returns_no_candidates() {
        let candidates = EmptyPaymentWindowLookup
            .lookup_batch(1, address(9), &[address(10)])
            .await
            .unwrap();

        assert!(candidates.is_empty());
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
            window_from_block: ChainBlockRef::new(
                u64::from(seed),
                BlockHash::from_bytes([seed; 32]),
            ),
            expires_at: now + Duration::minutes(15),
            monitor_until: now + Duration::minutes(30),
        }
    }

    const fn address(seed: u8) -> EvmAddress {
        EvmAddress::from_bytes([seed; 20])
    }
}
