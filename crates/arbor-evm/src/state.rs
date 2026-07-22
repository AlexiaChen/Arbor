use std::collections::BTreeMap;

use alloy_primitives::{Address, B256, Bytes, U256, keccak256};
use arbor_state::{
    Account, EMPTY_ROOT_HASH, EthereumStateCommitment, KECCAK_EMPTY, MemoryNodeStore, NodeStore,
    StateOverlay, TrieSnapshot, decode_account, decode_storage_trie_value, secure_account_key,
    secure_storage_key, storage_trie_value,
};
use revm::{
    Database,
    bytecode::Bytecode,
    database_interface::DBErrorMarker,
    primitives::{StorageKey, StorageValue},
    state::{AccountInfo, EvmState},
};
use thiserror::Error;

use crate::EvmError;

/// Genesis account allocation used to materialize the first authenticated root.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct GenesisAccount {
    /// Initial transaction nonce.
    pub nonce: u64,
    /// Initial native balance.
    pub balance: U256,
    /// Runtime bytecode.
    pub code: Bytes,
    /// Raw EVM storage slots and values; zero values are omitted.
    pub storage: BTreeMap<U256, U256>,
}

/// Account root plus every currently reachable storage node and contract body.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionState {
    accounts: TrieSnapshot,
    trie_nodes: BTreeMap<B256, Vec<u8>>,
    contract_code: BTreeMap<B256, Vec<u8>>,
    block_hashes: BTreeMap<u64, B256>,
}

impl ExecutionState {
    /// Builds a deterministic genesis execution state.
    ///
    /// # Errors
    ///
    /// Returns [`EvmError`] for malformed or inconsistent authenticated state.
    pub fn from_genesis(allocations: &BTreeMap<Address, GenesisAccount>) -> Result<Self, EvmError> {
        let mut account_leaves = BTreeMap::new();
        let mut trie_nodes = BTreeMap::new();
        let mut contract_code = BTreeMap::new();
        for (address, allocation) in allocations {
            let storage = build_storage_snapshot(&allocation.storage)?;
            trie_nodes.extend(storage.nodes().clone());
            let code_hash = if allocation.code.is_empty() {
                KECCAK_EMPTY
            } else {
                let hash = keccak256(&allocation.code);
                contract_code.insert(hash, allocation.code.to_vec());
                hash
            };
            let account = Account {
                nonce: allocation.nonce,
                balance: allocation.balance,
                storage_root: storage.root(),
                code_hash,
            };
            if !account.is_empty() {
                account_leaves.insert(
                    secure_account_key(*address),
                    arbor_state::encode_account(&account),
                );
            }
        }
        let mut accounts = EthereumStateCommitment::build(&account_leaves)?;
        trie_nodes.extend(accounts.nodes().clone());
        accounts.extend_nodes(trie_nodes.clone())?;
        Ok(Self {
            accounts,
            trie_nodes,
            contract_code,
            block_hashes: BTreeMap::new(),
        })
    }

    /// Reconstructs executable state from a persisted account root and trie/code stores.
    ///
    /// Code is loaded only for hashes referenced by current accounts.
    ///
    /// # Errors
    ///
    /// Returns [`EvmError`] for a missing/corrupt trie node or contract body.
    pub fn from_persisted<S, F>(root: B256, store: &S, mut load_code: F) -> Result<Self, EvmError>
    where
        S: NodeStore,
        F: FnMut(B256) -> Result<Option<Vec<u8>>, EvmError>,
    {
        let account_leaves = EthereumStateCommitment::collect_leaves(root, store)?;
        let mut accounts = EthereumStateCommitment::build(&account_leaves)?;
        if accounts.root() != root {
            return Err(EvmError::State(
                "persisted account root mismatch".to_owned(),
            ));
        }
        let mut trie_nodes = accounts.nodes().clone();
        let mut contract_code = BTreeMap::new();
        for encoded in account_leaves.values() {
            let account = decode_account(encoded)?;
            if account.storage_root != EMPTY_ROOT_HASH {
                let leaves = EthereumStateCommitment::collect_leaves(account.storage_root, store)?;
                let storage = EthereumStateCommitment::build(&leaves)?;
                if storage.root() != account.storage_root {
                    return Err(EvmError::State(
                        "persisted storage root mismatch".to_owned(),
                    ));
                }
                trie_nodes.extend(storage.nodes().clone());
            }
            if account.code_hash != KECCAK_EMPTY {
                let code = load_code(account.code_hash)?.ok_or_else(|| {
                    EvmError::State(format!("missing contract code {}", account.code_hash))
                })?;
                if keccak256(&code) != account.code_hash {
                    return Err(EvmError::State("contract code hash mismatch".to_owned()));
                }
                contract_code.insert(account.code_hash, code);
            }
        }
        accounts.extend_nodes(trie_nodes.clone())?;
        Ok(Self {
            accounts,
            trie_nodes,
            contract_code,
            block_hashes: BTreeMap::new(),
        })
    }

    /// Current Ethereum account-state root.
    #[must_use]
    pub const fn state_root(&self) -> B256 {
        self.accounts.root()
    }

    /// Fully materialized account snapshot carrying reachable storage nodes.
    #[must_use]
    pub const fn snapshot(&self) -> &TrieSnapshot {
        &self.accounts
    }

    /// Contract bodies referenced or created while executing this state.
    #[must_use]
    pub const fn contract_code(&self) -> &BTreeMap<B256, Vec<u8>> {
        &self.contract_code
    }

    /// Adds a consensus-verified historical block hash for `BLOCKHASH`.
    pub fn insert_block_hash(&mut self, number: u64, hash: B256) {
        self.block_hashes.insert(number, hash);
    }

    /// Reads one account by its address preimage.
    ///
    /// # Errors
    ///
    /// Returns [`EvmError`] for a malformed authenticated account leaf.
    pub fn account(&self, address: Address) -> Result<Option<Account>, EvmError> {
        self.accounts
            .leaves()
            .get(&secure_account_key(address))
            .map(|bytes| decode_account(bytes).map_err(EvmError::from))
            .transpose()
    }

    /// Reads one EVM storage slot from the account's authenticated storage root.
    ///
    /// # Errors
    ///
    /// Returns [`EvmError`] for a malformed/missing storage trie.
    pub fn storage(&self, address: Address, index: U256) -> Result<U256, EvmError> {
        let Some(account) = self.account(address)? else {
            return Ok(U256::ZERO);
        };
        if account.storage_root == EMPTY_ROOT_HASH {
            return Ok(U256::ZERO);
        }
        let store = MemoryNodeStore::new(self.trie_nodes.clone());
        let leaves = EthereumStateCommitment::collect_leaves(account.storage_root, &store)?;
        let key = secure_storage_key(index);
        leaves
            .get(&key)
            .map(|value| decode_storage_trie_value(value).map_err(EvmError::from))
            .transpose()
            .map(Option::unwrap_or_default)
    }

    pub(crate) fn apply_changes(&mut self, changes: EvmState) -> Result<(), EvmError> {
        let original_store = MemoryNodeStore::new(self.trie_nodes.clone());
        let mut account_overlay = StateOverlay::from_snapshot(&self.accounts);
        let mut added_storage_nodes = BTreeMap::new();

        for (address, changed) in changes {
            if !changed.is_touched() {
                continue;
            }
            if changed.is_selfdestructed() {
                account_overlay.delete_account(address);
                continue;
            }

            let old = self.account(address)?.unwrap_or_default();
            let mut storage_leaves = if changed.is_created() || old.storage_root == EMPTY_ROOT_HASH
            {
                BTreeMap::new()
            } else {
                EthereumStateCommitment::collect_leaves(old.storage_root, &original_store)?
            };
            for (slot, value) in changed.changed_storage_slots() {
                let key = secure_storage_key(*slot);
                match storage_trie_value(value.present_value()) {
                    Some(value) => {
                        storage_leaves.insert(key, value);
                    }
                    None => {
                        storage_leaves.remove(&key);
                    }
                }
            }
            let storage = EthereumStateCommitment::build(&storage_leaves)?;
            added_storage_nodes.extend(storage.nodes().clone());

            let code_hash = changed.info.code_hash;
            if code_hash != KECCAK_EMPTY
                && let Some(code) = changed.info.code.as_ref()
            {
                let bytes = code.original_bytes();
                if !bytes.is_empty() {
                    if keccak256(&bytes) != code_hash {
                        return Err(EvmError::State(
                            "revm returned mismatched code hash".to_owned(),
                        ));
                    }
                    self.contract_code.insert(code_hash, bytes.to_vec());
                }
            }
            account_overlay.set_account(
                address,
                Account {
                    nonce: changed.info.nonce,
                    balance: changed.info.balance,
                    storage_root: storage.root(),
                    code_hash,
                },
            );
        }

        let mut accounts = account_overlay.commit()?;
        let mut candidate_nodes = self.trie_nodes.clone();
        candidate_nodes.extend(added_storage_nodes);
        candidate_nodes.extend(accounts.nodes().clone());
        let candidate_store = MemoryNodeStore::new(candidate_nodes);
        let mut reachable = accounts.nodes().clone();
        for encoded in accounts.leaves().values() {
            let account = decode_account(encoded)?;
            if account.storage_root != EMPTY_ROOT_HASH {
                let leaves = EthereumStateCommitment::collect_leaves(
                    account.storage_root,
                    &candidate_store,
                )?;
                let storage = EthereumStateCommitment::build(&leaves)?;
                reachable.extend(storage.nodes().clone());
            }
        }
        accounts.extend_nodes(reachable.clone())?;
        self.accounts = accounts;
        self.trie_nodes = reachable;
        Ok(())
    }
}

fn build_storage_snapshot(storage: &BTreeMap<U256, U256>) -> Result<TrieSnapshot, EvmError> {
    let leaves = storage
        .iter()
        .filter_map(|(slot, value)| {
            storage_trie_value(*value).map(|value| (secure_storage_key(*slot), value))
        })
        .collect();
    Ok(EthereumStateCommitment::build(&leaves)?)
}

#[derive(Clone, Debug, Error)]
pub(crate) enum ExecutionDatabaseError {
    #[error("{0}")]
    State(String),
}

impl DBErrorMarker for ExecutionDatabaseError {}

pub(crate) struct ExecutionDatabase<'a> {
    state: &'a ExecutionState,
}

impl<'a> ExecutionDatabase<'a> {
    pub(crate) const fn new(state: &'a ExecutionState) -> Self {
        Self { state }
    }
}

impl Database for ExecutionDatabase<'_> {
    type Error = ExecutionDatabaseError;

    fn basic(&mut self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        self.state
            .account(address)
            .map(|account| {
                account.map(|account| AccountInfo {
                    balance: account.balance,
                    nonce: account.nonce,
                    code_hash: account.code_hash,
                    account_id: None,
                    code: (account.code_hash == KECCAK_EMPTY).then(Bytecode::new),
                })
            })
            .map_err(|error| ExecutionDatabaseError::State(error.to_string()))
    }

    fn code_by_hash(&mut self, code_hash: B256) -> Result<Bytecode, Self::Error> {
        if code_hash == KECCAK_EMPTY {
            return Ok(Bytecode::new());
        }
        let code = self.state.contract_code.get(&code_hash).ok_or_else(|| {
            ExecutionDatabaseError::State(format!("missing contract code {code_hash}"))
        })?;
        Bytecode::new_raw_checked(Bytes::copy_from_slice(code))
            .map_err(|error| ExecutionDatabaseError::State(error.to_string()))
    }

    fn storage(
        &mut self,
        address: Address,
        index: StorageKey,
    ) -> Result<StorageValue, Self::Error> {
        self.state
            .storage(address, index)
            .map_err(|error| ExecutionDatabaseError::State(error.to_string()))
    }

    fn block_hash(&mut self, number: u64) -> Result<B256, Self::Error> {
        Ok(self
            .state
            .block_hashes
            .get(&number)
            .copied()
            .unwrap_or_default())
    }
}
