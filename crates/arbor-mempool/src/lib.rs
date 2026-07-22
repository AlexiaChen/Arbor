//! Bounded per-domain EIP-1559 nonce queues and deterministic replacement policy.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};

use arbor_codec::{MAX_TRANSACTION_ENVELOPE_BYTES, decode_eip1559};
use arbor_crypto::{eip1559_transaction_hash, recover_eip1559_sender};
use arbor_primitives::{Address, B256, Bytes, DomainId, Eip1559Transaction};
use thiserror::Error;

/// Local policy. It affects admission/eviction only and never block validity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MempoolConfig {
    /// Total transactions across all domains.
    pub max_transactions: usize,
    /// Transactions retained for one `(domain, sender)`.
    pub max_transactions_per_sender: usize,
    /// Maximum accepted envelope bytes, capped by the protocol codec limit.
    pub max_transaction_bytes: usize,
    /// Required increase of both EIP-1559 fee caps for same-nonce replacement.
    pub replacement_bump_percent: u8,
}

impl Default for MempoolConfig {
    fn default() -> Self {
        Self {
            max_transactions: 50_000,
            max_transactions_per_sender: 128,
            max_transaction_bytes: MAX_TRANSACTION_ENVELOPE_BYTES,
            replacement_bump_percent: 10,
        }
    }
}

/// Position of an admitted transaction relative to current sender nonce.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QueueStatus {
    /// Every nonce from current state through this transaction is present.
    Pending,
    /// At least one lower nonce is missing.
    Queued,
    /// An existing same-nonce transaction was replaced.
    Replaced,
}

/// One decoded and signature-verified local pool entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PoolEntry {
    /// Target logical domain.
    pub domain_id: DomainId,
    /// Recovered sender.
    pub sender: Address,
    /// Standard Ethereum transaction hash.
    pub transaction_hash: B256,
    /// Exact typed envelope propagated to peers and blocks.
    pub envelope: Bytes,
    /// Decoded fields used for ordering and replacement.
    pub transaction: Eip1559Transaction,
}

/// Admission or queue failure.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum MempoolError {
    /// Domain must be registered with one unique EVM chain ID.
    #[error("unknown mempool domain")]
    UnknownDomain,
    /// EVM chain IDs are non-zero protocol identifiers.
    #[error("EVM chain ID must be non-zero")]
    InvalidChainId,
    /// A domain's EVM chain ID is immutable after registration.
    #[error("domain is already registered with another EVM chain ID")]
    DomainAlreadyRegistered,
    /// One chain ID cannot route to multiple domains.
    #[error("EVM chain ID is already registered to another domain")]
    DuplicateChainId,
    /// The envelope exceeds local/protocol byte limits.
    #[error("transaction envelope is {actual} bytes; limit is {limit}")]
    EnvelopeTooLarge {
        /// Effective limit.
        limit: usize,
        /// Observed bytes.
        actual: usize,
    },
    /// Typed RLP or signature validation failed.
    #[error("invalid EIP-1559 transaction: {0}")]
    InvalidTransaction(String),
    /// Envelope chain ID does not target the selected domain.
    #[error("transaction chain ID {actual} does not match domain chain ID {expected}")]
    WrongChainId {
        /// Registered chain ID.
        expected: u64,
        /// Encoded chain ID.
        actual: u64,
    },
    /// Transaction nonce is already finalized for this sender.
    #[error("transaction nonce {transaction} is lower than state nonce {state}")]
    NonceTooLow {
        /// Finalized state nonce.
        state: u64,
        /// Transaction nonce.
        transaction: u64,
    },
    /// Same-nonce replacement did not bump both fee caps enough.
    #[error("replacement fee caps are underpriced")]
    ReplacementUnderpriced,
    /// Global local capacity is full.
    #[error("mempool transaction capacity reached")]
    Capacity,
    /// One sender reached its local anti-abuse quota.
    #[error("per-sender mempool capacity reached")]
    SenderCapacity,
}

/// Domain-isolated local transaction pool.
#[derive(Clone, Debug)]
pub struct Mempool {
    config: MempoolConfig,
    chain_ids: BTreeMap<DomainId, u64>,
    used_chain_ids: BTreeSet<u64>,
    entries: BTreeMap<(DomainId, Address, u64), PoolEntry>,
    sender_counts: BTreeMap<(DomainId, Address), usize>,
}

impl Mempool {
    /// Creates an empty bounded pool.
    #[must_use]
    pub fn new(mut config: MempoolConfig) -> Self {
        config.max_transaction_bytes = config
            .max_transaction_bytes
            .min(MAX_TRANSACTION_ENVELOPE_BYTES);
        Self {
            config,
            chain_ids: BTreeMap::new(),
            used_chain_ids: BTreeSet::new(),
            entries: BTreeMap::new(),
            sender_counts: BTreeMap::new(),
        }
    }

    /// Registers one domain/chain-ID routing boundary.
    ///
    /// # Errors
    ///
    /// Returns an error if the ID is zero, the domain is already bound to another ID,
    /// or another domain owns the ID.
    pub fn register_domain(
        &mut self,
        domain_id: DomainId,
        chain_id: u64,
    ) -> Result<(), MempoolError> {
        if chain_id == 0 {
            return Err(MempoolError::InvalidChainId);
        }
        if let Some(&registered_chain_id) = self.chain_ids.get(&domain_id) {
            return if registered_chain_id == chain_id {
                Ok(())
            } else {
                Err(MempoolError::DomainAlreadyRegistered)
            };
        }
        if self.used_chain_ids.contains(&chain_id) {
            return Err(MempoolError::DuplicateChainId);
        }
        self.chain_ids.insert(domain_id, chain_id);
        self.used_chain_ids.insert(chain_id);
        Ok(())
    }

    /// Admits one signed type-2 envelope using the latest finalized sender nonce.
    ///
    /// # Errors
    ///
    /// Returns [`MempoolError`] for routing, signature, nonce, replacement, or quota failures.
    pub fn insert(
        &mut self,
        domain_id: DomainId,
        envelope: Bytes,
        state_nonce: u64,
    ) -> Result<QueueStatus, MempoolError> {
        let Some(&chain_id) = self.chain_ids.get(&domain_id) else {
            return Err(MempoolError::UnknownDomain);
        };
        if envelope.len() > self.config.max_transaction_bytes {
            return Err(MempoolError::EnvelopeTooLarge {
                limit: self.config.max_transaction_bytes,
                actual: envelope.len(),
            });
        }
        let transaction = decode_eip1559(&envelope)
            .map_err(|error| MempoolError::InvalidTransaction(error.to_string()))?;
        if transaction.chain_id != chain_id {
            return Err(MempoolError::WrongChainId {
                expected: chain_id,
                actual: transaction.chain_id,
            });
        }
        if transaction.nonce < state_nonce {
            return Err(MempoolError::NonceTooLow {
                state: state_nonce,
                transaction: transaction.nonce,
            });
        }
        let sender = recover_eip1559_sender(&transaction)
            .map_err(|error| MempoolError::InvalidTransaction(error.to_string()))?;
        let transaction_hash = eip1559_transaction_hash(&transaction)
            .map_err(|error| MempoolError::InvalidTransaction(error.to_string()))?;
        let key = (domain_id, sender, transaction.nonce);
        if let Some(existing) = self.entries.get(&key) {
            if !replacement_priced(
                &existing.transaction,
                &transaction,
                self.config.replacement_bump_percent,
            ) {
                return Err(MempoolError::ReplacementUnderpriced);
            }
            self.entries.insert(
                key,
                PoolEntry {
                    domain_id,
                    sender,
                    transaction_hash,
                    envelope,
                    transaction,
                },
            );
            return Ok(QueueStatus::Replaced);
        }
        if self.entries.len() >= self.config.max_transactions {
            return Err(MempoolError::Capacity);
        }
        let sender_key = (domain_id, sender);
        let sender_count = self.sender_counts.get(&sender_key).copied().unwrap_or(0);
        if sender_count >= self.config.max_transactions_per_sender {
            return Err(MempoolError::SenderCapacity);
        }
        let nonce = transaction.nonce;
        self.entries.insert(
            key,
            PoolEntry {
                domain_id,
                sender,
                transaction_hash,
                envelope,
                transaction,
            },
        );
        self.sender_counts.insert(sender_key, sender_count + 1);
        Ok(
            if self.nonce_is_contiguous(domain_id, sender, state_nonce, nonce) {
                QueueStatus::Pending
            } else {
                QueueStatus::Queued
            },
        )
    }

    /// Returns the contiguous executable prefix for one sender.
    #[must_use]
    pub fn ready(&self, domain_id: DomainId, sender: Address, state_nonce: u64) -> Vec<&PoolEntry> {
        let mut ready = Vec::new();
        let mut nonce = state_nonce;
        while let Some(entry) = self.entries.get(&(domain_id, sender, nonce)) {
            ready.push(entry);
            let Some(next) = nonce.checked_add(1) else {
                break;
            };
            nonce = next;
        }
        ready
    }

    /// Removes finalized/stale nonces for one sender and returns the number removed.
    pub fn advance_nonce(
        &mut self,
        domain_id: DomainId,
        sender: Address,
        state_nonce: u64,
    ) -> usize {
        let keys: Vec<_> = self
            .entries
            .range((domain_id, sender, 0)..(domain_id, sender, state_nonce))
            .map(|(key, _)| *key)
            .collect();
        for key in &keys {
            self.entries.remove(key);
        }
        if let Some(count) = self.sender_counts.get_mut(&(domain_id, sender)) {
            *count -= keys.len();
            if *count == 0 {
                self.sender_counts.remove(&(domain_id, sender));
            }
        }
        keys.len()
    }

    /// Number of retained local transactions.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns whether no transactions are retained.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn nonce_is_contiguous(
        &self,
        domain_id: DomainId,
        sender: Address,
        state_nonce: u64,
        target: u64,
    ) -> bool {
        (state_nonce..=target).all(|nonce| self.entries.contains_key(&(domain_id, sender, nonce)))
    }
}

fn replacement_priced(
    old: &Eip1559Transaction,
    new: &Eip1559Transaction,
    bump_percent: u8,
) -> bool {
    new.max_fee_per_gas >= replacement_min(old.max_fee_per_gas, bump_percent)
        && new.max_priority_fee_per_gas
            >= replacement_min(old.max_priority_fee_per_gas, bump_percent)
}

fn replacement_min(value: u128, bump_percent: u8) -> u128 {
    let quotient = value / 100;
    let remainder = value % 100;
    let bump = quotient
        .saturating_mul(u128::from(bump_percent))
        .saturating_add(
            remainder
                .saturating_mul(u128::from(bump_percent))
                .div_ceil(100),
        );
    value.saturating_add(bump)
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{U256, address};
    use arbor_codec::{encode_eip1559, encode_eip1559_signing_payload};
    use k256::ecdsa::SigningKey;

    use super::*;

    fn domain(byte: u8) -> DomainId {
        DomainId(B256::repeat_byte(byte))
    }

    fn signed(chain_id: u64, nonce: u64, tip: u128, fee: u128) -> Bytes {
        let mut transaction = Eip1559Transaction {
            chain_id,
            nonce,
            max_priority_fee_per_gas: tip,
            max_fee_per_gas: fee,
            gas_limit: 21_000,
            to: Some(address!("00000000000000000000000000000000000000aa")),
            value: U256::from(1),
            input: Bytes::new(),
            access_list: Vec::new(),
            y_parity: false,
            r: U256::ZERO,
            s: U256::ZERO,
        };
        let payload = encode_eip1559_signing_payload(&transaction).unwrap();
        let digest = alloy_primitives::keccak256(payload);
        let key = SigningKey::from_bytes((&[7_u8; 32]).into()).unwrap();
        let (signature, recovery_id) = key.sign_prehash_recoverable(digest.as_slice()).unwrap();
        let bytes = signature.to_bytes();
        transaction.r = U256::from_be_slice(&bytes[..32]);
        transaction.s = U256::from_be_slice(&bytes[32..]);
        transaction.y_parity = recovery_id.is_y_odd();
        encode_eip1559(&transaction).unwrap().into()
    }

    #[test]
    fn domains_nonce_gaps_and_replacement_are_isolated() {
        let mut pool = Mempool::new(MempoolConfig::default());
        pool.register_domain(domain(1), 1).unwrap();
        pool.register_domain(domain(2), 2).unwrap();

        assert_eq!(
            pool.insert(domain(1), signed(1, 1, 10, 100), 0).unwrap(),
            QueueStatus::Queued
        );
        assert_eq!(
            pool.insert(domain(1), signed(1, 0, 10, 100), 0).unwrap(),
            QueueStatus::Pending
        );
        let sender =
            recover_eip1559_sender(&decode_eip1559(&signed(1, 0, 10, 100)).unwrap()).unwrap();
        assert_eq!(pool.ready(domain(1), sender, 0).len(), 2);
        assert_eq!(
            pool.insert(domain(1), signed(1, 0, 10, 109), 0),
            Err(MempoolError::ReplacementUnderpriced)
        );
        assert_eq!(
            pool.insert(domain(1), signed(1, 0, 11, 110), 0).unwrap(),
            QueueStatus::Replaced
        );
        assert!(matches!(
            pool.insert(domain(2), signed(1, 0, 10, 100), 0),
            Err(MempoolError::WrongChainId { .. })
        ));
        assert_eq!(pool.advance_nonce(domain(1), sender, 1), 1);
    }

    #[test]
    fn domain_chain_id_registration_is_unique_nonzero_and_immutable() {
        let mut pool = Mempool::new(MempoolConfig::default());

        assert_eq!(
            pool.register_domain(domain(1), 0),
            Err(MempoolError::InvalidChainId)
        );
        pool.register_domain(domain(1), 1).unwrap();
        pool.register_domain(domain(1), 1).unwrap();
        assert_eq!(
            pool.register_domain(domain(1), 2),
            Err(MempoolError::DomainAlreadyRegistered)
        );
        assert_eq!(
            pool.register_domain(domain(2), 1),
            Err(MempoolError::DuplicateChainId)
        );
    }

    #[test]
    fn global_and_sender_limits_are_enforced() {
        let mut pool = Mempool::new(MempoolConfig {
            max_transactions: 2,
            max_transactions_per_sender: 1,
            ..MempoolConfig::default()
        });
        pool.register_domain(domain(1), 1).unwrap();
        pool.insert(domain(1), signed(1, 0, 1, 10), 0).unwrap();
        assert_eq!(
            pool.insert(domain(1), signed(1, 1, 1, 10), 0),
            Err(MempoolError::SenderCapacity)
        );
        pool.register_domain(domain(2), 2).unwrap();
        pool.insert(domain(2), signed(2, 0, 1, 10), 0).unwrap();
        pool.register_domain(domain(3), 3).unwrap();
        assert_eq!(
            pool.insert(domain(3), signed(3, 0, 1, 10), 0),
            Err(MempoolError::Capacity)
        );
    }
}
