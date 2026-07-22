use std::collections::BTreeMap;

use alloy_primitives::{Address, B256};

use crate::{
    Account, EthereumStateCommitment, StateError, TrieSnapshot, decode_account, encode_account,
    secure_account_key,
};

/// Read-only account-state boundary used by execution and RPC layers.
pub trait StateView {
    /// Returns an account at the current root.
    ///
    /// # Errors
    ///
    /// Returns [`StateError`] if the authenticated leaf is malformed.
    fn account(&self, address: Address) -> Result<Option<Account>, StateError>;

    /// Returns an exact value by secure trie key.
    fn secure_leaf(&self, key: B256) -> Option<&[u8]>;
}

/// Copy-on-write proposal overlay over authenticated secure leaves.
///
/// Dropping or calling [`Self::discard`] cannot mutate the base snapshot.
#[derive(Clone, Debug)]
pub struct StateOverlay {
    base: BTreeMap<B256, Vec<u8>>,
    writes: BTreeMap<B256, Option<Vec<u8>>>,
}

impl StateOverlay {
    /// Starts an overlay over exact secure trie leaves.
    #[must_use]
    pub fn new(base: BTreeMap<B256, Vec<u8>>) -> Self {
        Self {
            base,
            writes: BTreeMap::new(),
        }
    }

    /// Starts an overlay from a materialized snapshot.
    #[must_use]
    pub fn from_snapshot(snapshot: &TrieSnapshot) -> Self {
        Self::new(snapshot.leaves().clone())
    }

    /// Inserts or updates an account; empty accounts are deleted per EIP-161.
    pub fn set_account(&mut self, address: Address, account: Account) {
        let key = secure_account_key(address);
        if account.is_empty() {
            self.writes.insert(key, None);
        } else {
            self.writes.insert(key, Some(encode_account(&account)));
        }
    }

    /// Deletes an account explicitly.
    pub fn delete_account(&mut self, address: Address) {
        self.writes.insert(secure_account_key(address), None);
    }

    /// Inserts or deletes a generic secure storage leaf.
    pub fn set_secure_leaf(&mut self, key: B256, value: Option<Vec<u8>>) {
        self.writes.insert(key, value);
    }

    /// Returns the number of distinct changed secure keys.
    #[must_use]
    pub fn changed_keys(&self) -> usize {
        self.writes.len()
    }

    /// Materializes the writes and builds a new immutable authenticated snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`StateError`] when the trie cannot be constructed.
    pub fn commit(mut self) -> Result<TrieSnapshot, StateError> {
        for (key, value) in self.writes {
            if let Some(value) = value {
                self.base.insert(key, value);
            } else {
                self.base.remove(&key);
            }
        }
        EthereumStateCommitment::build(&self.base)
    }

    /// Discards all writes and returns the unchanged base leaves.
    #[must_use]
    pub fn discard(self) -> BTreeMap<B256, Vec<u8>> {
        self.base
    }
}

impl StateView for StateOverlay {
    fn account(&self, address: Address) -> Result<Option<Account>, StateError> {
        self.secure_leaf(secure_account_key(address))
            .map(decode_account)
            .transpose()
    }

    fn secure_leaf(&self, key: B256) -> Option<&[u8]> {
        match self.writes.get(&key) {
            Some(Some(value)) => Some(value),
            Some(None) => None,
            None => self.base.get(&key).map(Vec::as_slice),
        }
    }
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{U256, address};

    use super::*;

    #[test]
    fn commit_discard_and_write_order_are_deterministic() {
        let alice = address!("0000000000000000000000000000000000000001");
        let bob = address!("0000000000000000000000000000000000000002");
        let account = |balance| Account {
            balance: U256::from(balance),
            ..Account::default()
        };

        let mut first = StateOverlay::new(BTreeMap::new());
        first.set_account(alice, account(1));
        first.set_account(bob, account(2));
        let first = first.commit().unwrap();

        let mut second = StateOverlay::new(BTreeMap::new());
        second.set_account(bob, account(2));
        second.set_account(alice, account(1));
        let second = second.commit().unwrap();
        assert_eq!(first.root(), second.root());

        let mut discarded = StateOverlay::from_snapshot(&first);
        discarded.delete_account(alice);
        assert_eq!(discarded.discard(), first.leaves().clone());
    }

    #[test]
    fn empty_accounts_are_removed() {
        let alice = address!("0000000000000000000000000000000000000001");
        let mut overlay = StateOverlay::new(BTreeMap::new());
        overlay.set_account(
            alice,
            Account {
                balance: U256::from(1),
                ..Account::default()
            },
        );
        let snapshot = overlay.commit().unwrap();
        let mut overlay = StateOverlay::from_snapshot(&snapshot);
        overlay.set_account(alice, Account::default());
        assert_eq!(overlay.commit().unwrap().root(), crate::EMPTY_ROOT_HASH);
    }
}
