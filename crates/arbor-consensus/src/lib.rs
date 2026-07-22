//! Consensus application boundary and explicitly development-only single-validator engine.
//!
//! This crate does not contain a production BFT implementation. Candidate BFT crates remain
//! isolated under `spikes/` until ADR-004's durable-before-sign gate passes.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};

use alloy_primitives::{Address, B256, Bytes, U256, keccak256};
use arbor_chain::{
    ChainError, ChainMachine, ConsensusBlock, DomainConfig, FinalizedChainState, ValidatedProposal,
    decode_consensus_block, encode_consensus_block,
};
use arbor_crypto::{ConsensusSigner, consensus_header_hash, validator_id, validator_set_hash};
use arbor_evm::{ExecutionState, GenesisAccount};
use arbor_mempool::{Mempool, MempoolConfig, MempoolError, PoolEntry, QueueStatus};
use arbor_primitives::{DomainBatch, DomainId, NetworkId, Validator, ValidatorSet};
use arbor_storage::{
    CommitBatch, CommitStats, Database, DomainStateCommit, FinalizedMarker, IndexedValue,
    StorageError,
};
use thiserror::Error;
use tokio::sync::broadcast;

const DEV_GENESIS_TAG: &[u8] = b"ARBOR_DEV_GENESIS_RECORD_V1";
const DEV_WAL_TAG: &[u8] = b"ARBOR_DEV_COMMIT_WAL_V1";
const KEY_DEV_GENESIS: &[u8] = b"m5:dev:genesis";
const KEY_DEV_WAL: &[u8] = b"m5:dev:wal";
const PREFIX_BLOCK: &[u8] = b"m5:block:";
const PREFIX_RECEIPT: &[u8] = b"m5:receipt:";
const PREFIX_TX_LOCATION: &[u8] = b"m5:tx:";

/// Hard cap used by the M5 single-domain proposer before M6 adds fair multi-domain scheduling.
pub const MAX_DEV_PROPOSAL_TRANSACTIONS: usize = 10_000;

/// Explicit capability required to construct the non-BFT development engine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EngineMode {
    /// Immediate single-validator finality for local development only.
    DevValidator,
    /// Production assembly, for which M5 deliberately provides no engine.
    Production,
}

/// Deterministic root-domain and validator configuration for a development database.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DevGenesis {
    /// Network identity already bound to the parity-db instance.
    pub network_id: NetworkId,
    /// Height-zero consensus hash.
    pub genesis_hash: B256,
    /// Height-zero timestamp.
    pub timestamp: u64,
    /// Root EVM domain.
    pub root_domain_id: DomainId,
    /// Root execution parameters.
    pub root_config: DomainConfig,
    /// Exactly one consensus validator. It does not make this engine production BFT.
    pub validator_set: ValidatorSet,
    /// Address receiving EIP-1559 priority fees in every included domain.
    pub reward_address: Address,
    /// Root-domain genesis account allocation, including any validator funding.
    pub allocations: BTreeMap<Address, GenesisAccount>,
}

impl DevGenesis {
    /// Builds the deterministic local-development genesis used by `arbor node init --dev`.
    ///
    /// The fixed consensus secret is public test material and must never be accepted by a
    /// production assembly. The funded account corresponds to the M4 fixture key `[7; 32]`.
    ///
    /// # Errors
    ///
    /// Returns [`ConsensusError`] only if the embedded development key becomes invalid.
    pub fn local_default(
        network_id: NetworkId,
        genesis_hash: B256,
    ) -> Result<Self, ConsensusError> {
        let signer = ConsensusSigner::from_secret_bytes(&[9_u8; 32])
            .map_err(|error| ConsensusError::InvalidGenesisOwned(error.to_string()))?;
        let public_key = signer.public_key();
        let mut reward_bytes = [0_u8; 20];
        reward_bytes[18..].copy_from_slice(&[0x0f, 0xee]);
        let reward_address = Address::from(reward_bytes);
        let funded_address = Address::from([
            0x4a, 0x62, 0x31, 0x66, 0x23, 0xad, 0x45, 0x7f, 0x02, 0xcd, 0xc5, 0xd9, 0x97, 0xde,
            0xd6, 0x7a, 0x38, 0x3e, 0xc5, 0x69,
        ]);
        Ok(Self {
            network_id,
            genesis_hash,
            timestamp: 1_700_000_000,
            root_domain_id: DomainId(keccak256(b"ARBOR_DEV_ROOT_DOMAIN_V1")),
            root_config: DomainConfig {
                chain_id: 2_048,
                protocol_revision: 1,
                gas_limit: 30_000_000,
                initial_base_fee_per_gas: 1_000_000_000,
            },
            validator_set: ValidatorSet {
                epoch: 0,
                validators: vec![Validator {
                    id: validator_id(public_key),
                    public_key,
                    power: 1,
                }],
            },
            reward_address,
            allocations: BTreeMap::from([(
                funded_address,
                GenesisAccount {
                    balance: U256::from(10_u128.pow(24)),
                    ..GenesisAccount::default()
                },
            )]),
        })
    }

    fn fingerprint(&self) -> Result<B256, ConsensusError> {
        if self.validator_set.validators.len() != 1 {
            return Err(ConsensusError::InvalidGenesis(
                "dev validator set must contain exactly one validator",
            ));
        }
        let validator_hash = validator_set_hash(&self.validator_set)
            .map_err(|error| ConsensusError::InvalidGenesisOwned(error.to_string()))?;
        let state = ExecutionState::from_genesis(&self.allocations)
            .map_err(|error| ConsensusError::InvalidGenesisOwned(error.to_string()))?;
        let mut bytes = Vec::with_capacity(DEV_GENESIS_TAG.len() + 256);
        bytes.extend_from_slice(DEV_GENESIS_TAG);
        bytes.extend_from_slice(self.network_id.0.as_slice());
        bytes.extend_from_slice(self.genesis_hash.as_slice());
        bytes.extend_from_slice(&self.timestamp.to_be_bytes());
        bytes.extend_from_slice(self.root_domain_id.0.as_slice());
        bytes.extend_from_slice(&self.root_config.chain_id.to_be_bytes());
        bytes.extend_from_slice(&self.root_config.protocol_revision.to_be_bytes());
        bytes.extend_from_slice(&self.root_config.gas_limit.to_be_bytes());
        bytes.extend_from_slice(&self.root_config.initial_base_fee_per_gas.to_be_bytes());
        bytes.extend_from_slice(validator_hash.as_slice());
        bytes.extend_from_slice(self.reward_address.as_slice());
        bytes.extend_from_slice(state.state_root().as_slice());
        Ok(keccak256(bytes))
    }

    fn chain_state(&self) -> Result<FinalizedChainState, ConsensusError> {
        let validator_hash = validator_set_hash(&self.validator_set)
            .map_err(|error| ConsensusError::InvalidGenesisOwned(error.to_string()))?;
        let state = ExecutionState::from_genesis(&self.allocations)
            .map_err(|error| ConsensusError::InvalidGenesisOwned(error.to_string()))?;
        FinalizedChainState::genesis(
            self.network_id,
            self.genesis_hash,
            self.timestamp,
            validator_hash,
            self.root_domain_id,
            self.root_config,
            state,
        )
        .map_err(ConsensusError::from)
    }
}

/// Finalized event consumed by M6 scheduling and M7 block announcement/sync.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommitEvent {
    /// Finalized root-consensus height.
    pub height: u64,
    /// Finalized consensus hash.
    pub consensus_hash: B256,
    /// Sparse commitment to every current domain head.
    pub domain_heads_root: B256,
}

/// Opaque identity of the one pending development proposal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProposalId(B256);

impl ProposalId {
    /// Returns the candidate consensus hash.
    #[must_use]
    pub const fn hash(self) -> B256 {
        self.0
    }
}

struct PendingProposal {
    id: ProposalId,
    proposal: ValidatedProposal,
    reserved: Vec<PoolEntry>,
}

/// Development engine, storage, proposal, or recovery failure.
#[derive(Debug, Error)]
pub enum ConsensusError {
    /// This path is intentionally available only under an explicit dev mode.
    #[error("single-validator consensus requires explicit --dev-validator mode")]
    DevModeRequired,
    /// Development genesis violates a stable invariant.
    #[error("invalid development genesis: {0}")]
    InvalidGenesis(&'static str),
    /// Development genesis validation returned detailed owned context.
    #[error("invalid development genesis: {0}")]
    InvalidGenesisOwned(String),
    /// The database belongs to a different development genesis.
    #[error("development genesis fingerprint does not match the database")]
    GenesisMismatch,
    /// A proposal is already pending and must be committed or abandoned first.
    #[error("a development proposal is already pending")]
    ProposalPending,
    /// No pending proposal matches the supplied identifier.
    #[error("unknown pending proposal")]
    UnknownProposal,
    /// Durable head, block record, application state, or WAL disagree.
    #[error("inconsistent finalized database: {0}")]
    InconsistentStore(&'static str),
    /// Only the M5 root domain is registered before `ChainRegistry` lands in M6.
    #[error("M5 only accepts transactions for the configured root domain")]
    UnknownDomain,
    /// Canonical chain construction or validation failed.
    #[error(transparent)]
    Chain(#[from] ChainError),
    /// Local mempool admission failed.
    #[error(transparent)]
    Mempool(#[from] MempoolError),
    /// Durable parity-db operation failed.
    #[error(transparent)]
    Storage(#[from] StorageError),
}

/// Immediately-finalizing engine for `--dev-validator`; never a production consensus substitute.
pub struct SingleValidatorEngine {
    database: Database,
    genesis: DevGenesis,
    finalized: FinalizedChainState,
    mempool: Mempool,
    pending: Option<PendingProposal>,
    events: broadcast::Sender<CommitEvent>,
}

impl SingleValidatorEngine {
    /// Opens, initializes, or replays a development chain.
    ///
    /// # Errors
    ///
    /// Returns [`ConsensusError`] unless explicit development mode is supplied, or when genesis,
    /// storage, WAL, and deterministic replay do not agree.
    pub fn open(
        mode: EngineMode,
        database: Database,
        genesis: DevGenesis,
        mempool_config: MempoolConfig,
    ) -> Result<Self, ConsensusError> {
        if mode != EngineMode::DevValidator {
            return Err(ConsensusError::DevModeRequired);
        }
        let fingerprint = genesis.fingerprint()?;
        let mut finalized = genesis.chain_state()?;
        let marker = database.finalized_marker()?;
        match marker {
            None => initialize_database(&database, &genesis, &finalized, fingerprint)?,
            Some(marker) => {
                let stored =
                    database
                        .index(KEY_DEV_GENESIS)?
                        .ok_or(ConsensusError::InconsistentStore(
                            "missing dev genesis record",
                        ))?;
                if stored.as_slice() != fingerprint.as_slice() {
                    return Err(ConsensusError::GenesisMismatch);
                }
                for height in 1..=marker.height {
                    let bytes = database.index(&block_key(height))?.ok_or(
                        ConsensusError::InconsistentStore("missing finalized block body"),
                    )?;
                    let block = decode_consensus_block(&bytes)?;
                    let proposal = ChainMachine.validate_proposal(&finalized, &block)?;
                    finalized = proposal.into_parts().1;
                }
                verify_recovered(&database, &finalized, marker)?;
            }
        }
        let mut mempool = Mempool::new(mempool_config);
        mempool.register_domain(genesis.root_domain_id, genesis.root_config.chain_id)?;
        let (events, _) = broadcast::channel(64);
        Ok(Self {
            database,
            genesis,
            finalized,
            mempool,
            pending: None,
            events,
        })
    }

    /// Returns the finalized application view. Pending proposal state is never exposed here.
    #[must_use]
    pub const fn finalized_state(&self) -> &FinalizedChainState {
        &self.finalized
    }

    /// Returns the latest durable marker from storage.
    ///
    /// # Errors
    ///
    /// Returns [`ConsensusError`] if marker bytes are corrupt.
    pub fn finalized_marker(&self) -> Result<Option<FinalizedMarker>, ConsensusError> {
        Ok(self.database.finalized_marker()?)
    }

    /// Subscribes to stable finalized commit events.
    #[must_use]
    pub fn subscribe_commits(&self) -> broadcast::Receiver<CommitEvent> {
        self.events.subscribe()
    }

    /// Submits one signed EIP-1559 envelope against the latest finalized sender nonce.
    ///
    /// # Errors
    ///
    /// Returns [`ConsensusError`] for another domain or ordinary mempool rejection.
    pub fn submit_raw(
        &mut self,
        domain_id: DomainId,
        envelope: Bytes,
    ) -> Result<QueueStatus, ConsensusError> {
        if domain_id != self.genesis.root_domain_id {
            return Err(ConsensusError::UnknownDomain);
        }
        let transaction = arbor_codec_decode(&envelope)?;
        let sender = arbor_crypto::recover_eip1559_sender(&transaction)
            .map_err(|error| MempoolError::InvalidTransaction(error.to_string()))?;
        let state_nonce = self
            .finalized
            .domain(domain_id)
            .ok_or(ConsensusError::UnknownDomain)?
            .state
            .account(sender)
            .map_err(|error| ConsensusError::InvalidGenesisOwned(error.to_string()))?
            .map_or(0, |account| account.nonce);
        Ok(self.mempool.insert(domain_id, envelope, state_nonce)?)
    }

    /// Builds and locally validates one proposal without moving finalized state or its DB marker.
    ///
    /// Transactions selected from the pool remain reserved until commit or explicit abandonment.
    ///
    /// # Errors
    ///
    /// Returns [`ConsensusError`] for a pending proposal or invalid candidate execution.
    pub fn build_proposal(&mut self, timestamp: u64) -> Result<ProposalId, ConsensusError> {
        if self.pending.is_some() {
            return Err(ConsensusError::ProposalPending);
        }
        let selected = self.select_root_transactions()?;
        let hashes = selected
            .iter()
            .map(|entry| entry.transaction_hash)
            .collect::<BTreeSet<_>>();
        let reserved = self.mempool.reserve_hashes(&hashes);
        let batches = if reserved.is_empty() {
            Vec::new()
        } else {
            let parent = self
                .finalized
                .domain(self.genesis.root_domain_id)
                .ok_or(ConsensusError::UnknownDomain)?;
            vec![DomainBatch {
                domain_id: self.genesis.root_domain_id,
                parent_domain_block_hash: parent.block_hash,
                transactions: reserved
                    .iter()
                    .map(|entry| entry.envelope.clone())
                    .collect(),
            }]
        };
        let proposal = match ChainMachine.build_proposal(
            &self.finalized,
            batches,
            timestamp,
            self.genesis.reward_address,
        ) {
            Ok(proposal) => proposal,
            Err(error) => {
                self.mempool.restore_reserved(reserved);
                return Err(error.into());
            }
        };
        let id = ProposalId(proposal.block().hash()?);
        self.pending = Some(PendingProposal {
            id,
            proposal,
            reserved,
        });
        Ok(id)
    }

    /// Returns the pending block body without exposing its uncommitted state overlay.
    #[must_use]
    pub fn pending_block(&self, id: ProposalId) -> Option<&ConsensusBlock> {
        self.pending
            .as_ref()
            .filter(|pending| pending.id == id)
            .map(|pending| pending.proposal.block())
    }

    /// Discards a proposal and restores its reserved transactions to the local pool.
    ///
    /// # Errors
    ///
    /// Returns [`ConsensusError::UnknownProposal`] for a stale identifier.
    pub fn abandon_proposal(&mut self, id: ProposalId) -> Result<(), ConsensusError> {
        if self.pending.as_ref().is_none_or(|pending| pending.id != id) {
            return Err(ConsensusError::UnknownProposal);
        }
        let Some(pending) = self.pending.take() else {
            return Err(ConsensusError::UnknownProposal);
        };
        self.mempool.restore_reserved(pending.reserved);
        Ok(())
    }

    /// Durably commits the pending proposal in one synchronous parity-db transaction, then
    /// publishes finalized state and a commit event.
    ///
    /// # Errors
    ///
    /// Returns [`ConsensusError`] for a stale proposal or any pre/during-commit storage failure.
    pub fn commit_proposal(&mut self, id: ProposalId) -> Result<CommitStats, ConsensusError> {
        if self.pending.as_ref().is_none_or(|pending| pending.id != id) {
            return Err(ConsensusError::UnknownProposal);
        }
        let Some(pending) = self.pending.take() else {
            return Err(ConsensusError::UnknownProposal);
        };
        let commit = proposal_commit_batch(&pending.proposal)?;
        let stats = match self.database.commit(commit) {
            Ok(stats) => stats,
            Err(error) => {
                self.pending = Some(pending);
                return Err(error.into());
            }
        };
        let (_, finalized, _) = pending.proposal.into_parts();
        self.finalized = finalized;
        let event = CommitEvent {
            height: self.finalized.height.0,
            consensus_hash: self.finalized.consensus_hash,
            domain_heads_root: self.finalized.domain_heads_root(),
        };
        let _ = self.events.send(event);
        Ok(stats)
    }

    /// Builds and immediately commits one development block.
    ///
    /// # Errors
    ///
    /// Returns any proposal or durable commit failure.
    pub fn produce_block(&mut self, timestamp: u64) -> Result<CommitEvent, ConsensusError> {
        let id = self.build_proposal(timestamp)?;
        self.commit_proposal(id)?;
        Ok(CommitEvent {
            height: self.finalized.height.0,
            consensus_hash: self.finalized.consensus_hash,
            domain_heads_root: self.finalized.domain_heads_root(),
        })
    }

    /// Returns a finalized typed receipt by standard Ethereum transaction hash.
    ///
    /// # Errors
    ///
    /// Returns [`ConsensusError`] if storage cannot complete the lookup.
    pub fn receipt(&self, transaction_hash: B256) -> Result<Option<Vec<u8>>, ConsensusError> {
        Ok(self.database.receipt(&receipt_key(transaction_hash))?)
    }

    /// Number of unreserved local transactions.
    #[must_use]
    pub fn mempool_len(&self) -> usize {
        self.mempool.len()
    }

    fn select_root_transactions(&self) -> Result<Vec<PoolEntry>, ConsensusError> {
        let domain = self
            .finalized
            .domain(self.genesis.root_domain_id)
            .ok_or(ConsensusError::UnknownDomain)?;
        let mut selected = Vec::new();
        let mut reserved_gas = 0_u64;
        for sender in self.mempool.senders(self.genesis.root_domain_id) {
            let nonce = domain
                .state
                .account(sender)
                .map_err(|error| ConsensusError::InvalidGenesisOwned(error.to_string()))?
                .map_or(0, |account| account.nonce);
            for entry in self
                .mempool
                .ready(self.genesis.root_domain_id, sender, nonce)
            {
                let Some(next_gas) = reserved_gas.checked_add(entry.transaction.gas_limit) else {
                    break;
                };
                if next_gas > domain.config.gas_limit
                    || selected.len() >= MAX_DEV_PROPOSAL_TRANSACTIONS
                {
                    break;
                }
                reserved_gas = next_gas;
                selected.push(entry.clone());
            }
        }
        selected.sort_by_key(|entry| (entry.sender, entry.transaction.nonce));
        Ok(selected)
    }
}

fn initialize_database(
    database: &Database,
    genesis: &DevGenesis,
    state: &FinalizedChainState,
    fingerprint: B256,
) -> Result<(), ConsensusError> {
    let domain = state
        .domain(genesis.root_domain_id)
        .ok_or(ConsensusError::InvalidGenesis("missing root domain"))?;
    let marker = FinalizedMarker {
        height: 0,
        consensus_hash: genesis.genesis_hash,
        domain_heads_root: state.domain_heads_root(),
    };
    let mut commit = CommitBatch::new(marker);
    commit.states.push(DomainStateCommit {
        domain_id: genesis.root_domain_id,
        snapshot: domain.state.snapshot().clone(),
    });
    commit.contract_code = domain.state.contract_code().clone();
    commit.indexes.extend([
        IndexedValue {
            key: KEY_DEV_GENESIS.to_vec(),
            value: fingerprint.to_vec(),
        },
        IndexedValue {
            key: KEY_DEV_WAL.to_vec(),
            value: encode_dev_wal(marker),
        },
    ]);
    database.commit(commit)?;
    Ok(())
}

fn verify_recovered(
    database: &Database,
    state: &FinalizedChainState,
    marker: FinalizedMarker,
) -> Result<(), ConsensusError> {
    if marker.height != state.height.0
        || marker.consensus_hash != state.consensus_hash
        || marker.domain_heads_root != state.domain_heads_root()
    {
        return Err(ConsensusError::InconsistentStore("marker/head mismatch"));
    }
    let wal = database
        .index(KEY_DEV_WAL)?
        .ok_or(ConsensusError::InconsistentStore("missing dev commit WAL"))?;
    if wal != encode_dev_wal(marker) {
        return Err(ConsensusError::InconsistentStore("WAL/head mismatch"));
    }
    for (domain_id, domain) in state.domains() {
        let (height, root) =
            database
                .latest_head(domain_id)?
                .ok_or(ConsensusError::InconsistentStore(
                    "missing domain state head",
                ))?;
        if height != domain.header.consensus_height.0 || root != domain.header.state_root {
            return Err(ConsensusError::InconsistentStore(
                "domain state/head mismatch",
            ));
        }
    }
    Ok(())
}

fn proposal_commit_batch(proposal: &ValidatedProposal) -> Result<CommitBatch, ConsensusError> {
    let block = proposal.block();
    let hash = consensus_header_hash(&block.header)
        .map_err(|error| ConsensusError::InvalidGenesisOwned(error.to_string()))?;
    let marker = FinalizedMarker {
        height: block.header.height.0,
        consensus_hash: hash,
        domain_heads_root: block.header.domain_heads_root,
    };
    let mut commit = CommitBatch::new(marker);
    commit.indexes.push(IndexedValue {
        key: block_key(marker.height),
        value: encode_consensus_block(block)?,
    });
    commit.indexes.push(IndexedValue {
        key: KEY_DEV_WAL.to_vec(),
        value: encode_dev_wal(marker),
    });
    for ((domain_header, execution), batch) in block
        .domain_blocks
        .iter()
        .zip(proposal.executions())
        .zip(&block.batches)
    {
        commit.states.push(DomainStateCommit {
            domain_id: domain_header.domain_id,
            snapshot: execution.state.snapshot().clone(),
        });
        commit
            .contract_code
            .extend(execution.state.contract_code().clone());
        for (index, (transaction, receipt)) in execution
            .transactions
            .iter()
            .zip(&execution.encoded_receipts)
            .enumerate()
        {
            commit.receipts.push(IndexedValue {
                key: receipt_key(transaction.transaction_hash),
                value: receipt.clone(),
            });
            commit.indexes.push(IndexedValue {
                key: tx_location_key(transaction.transaction_hash),
                value: encode_tx_location(
                    marker.height,
                    batch.domain_id,
                    u32::try_from(index).map_err(|_| {
                        ConsensusError::InconsistentStore("transaction index overflow")
                    })?,
                ),
            });
        }
    }
    Ok(commit)
}

fn arbor_codec_decode(
    envelope: &[u8],
) -> Result<arbor_primitives::Eip1559Transaction, ConsensusError> {
    arbor_codec::decode_eip1559(envelope)
        .map_err(|error| MempoolError::InvalidTransaction(error.to_string()).into())
}

fn block_key(height: u64) -> Vec<u8> {
    [PREFIX_BLOCK, height.to_be_bytes().as_slice()].concat()
}

fn receipt_key(hash: B256) -> Vec<u8> {
    [PREFIX_RECEIPT, hash.as_slice()].concat()
}

fn tx_location_key(hash: B256) -> Vec<u8> {
    [PREFIX_TX_LOCATION, hash.as_slice()].concat()
}

fn encode_tx_location(height: u64, domain_id: DomainId, index: u32) -> Vec<u8> {
    [
        height.to_be_bytes().as_slice(),
        domain_id.0.as_slice(),
        index.to_be_bytes().as_slice(),
    ]
    .concat()
}

fn encode_dev_wal(marker: FinalizedMarker) -> Vec<u8> {
    [
        DEV_WAL_TAG,
        marker.height.to_be_bytes().as_slice(),
        marker.consensus_hash.as_slice(),
        marker.domain_heads_root.as_slice(),
    ]
    .concat()
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        process::Command,
        thread,
        time::{Duration, Instant},
    };

    use alloy_primitives::{U256, address};
    use arbor_codec::{encode_eip1559, encode_eip1559_signing_payload};
    use arbor_crypto::{ConsensusSigner, validator_id};
    use arbor_primitives::{Eip1559Transaction, Validator};
    use arbor_storage::{DatabaseIdentity, RetentionPolicy};
    use arbor_testkit::ProcessGuard;
    use k256::ecdsa::SigningKey;
    use tempfile::tempdir;

    use super::*;

    const SENDER: Address = address!("4a62316623ad457f02cdc5d997ded67a383ec569");
    const RECIPIENT: Address = address!("00000000000000000000000000000000000000aa");
    const REWARD: Address = address!("0000000000000000000000000000000000000fee");

    fn identity() -> DatabaseIdentity {
        DatabaseIdentity {
            network_id: NetworkId(B256::repeat_byte(0x11)),
            genesis_hash: B256::repeat_byte(0x22),
        }
    }

    fn genesis() -> DevGenesis {
        let signer = ConsensusSigner::from_secret_bytes(&[9_u8; 32]).unwrap();
        let public_key = signer.public_key();
        DevGenesis {
            network_id: identity().network_id,
            genesis_hash: identity().genesis_hash,
            timestamp: 1_000,
            root_domain_id: DomainId(B256::repeat_byte(0x44)),
            root_config: DomainConfig {
                chain_id: 2_048,
                protocol_revision: 1,
                gas_limit: 30_000_000,
                initial_base_fee_per_gas: 10,
            },
            validator_set: ValidatorSet {
                epoch: 0,
                validators: vec![Validator {
                    id: validator_id(public_key),
                    public_key,
                    power: 1,
                }],
            },
            reward_address: REWARD,
            allocations: BTreeMap::from([
                (
                    SENDER,
                    GenesisAccount {
                        balance: U256::from(10_u128.pow(18)),
                        ..GenesisAccount::default()
                    },
                ),
                (REWARD, GenesisAccount::default()),
            ]),
        }
    }

    fn signed_transfer() -> Bytes {
        let mut transaction = Eip1559Transaction {
            chain_id: 2_048,
            nonce: 0,
            max_priority_fee_per_gas: 2,
            max_fee_per_gas: 20,
            gas_limit: 21_000,
            to: Some(RECIPIENT),
            value: U256::from(123),
            input: Bytes::new(),
            access_list: Vec::new(),
            y_parity: false,
            r: U256::ZERO,
            s: U256::ZERO,
        };
        let payload = encode_eip1559_signing_payload(&transaction).unwrap();
        let digest = keccak256(payload);
        let key = SigningKey::from_bytes((&[7_u8; 32]).into()).unwrap();
        let (signature, recovery_id) = key.sign_prehash_recoverable(digest.as_slice()).unwrap();
        let bytes = signature.to_bytes();
        transaction.r = U256::from_be_slice(&bytes[..32]);
        transaction.s = U256::from_be_slice(&bytes[32..]);
        transaction.y_parity = recovery_id.is_y_odd();
        encode_eip1559(&transaction).unwrap().into()
    }

    fn open(path: &std::path::Path) -> SingleValidatorEngine {
        let database = Database::open(path, identity(), RetentionPolicy::Archive).unwrap();
        SingleValidatorEngine::open(
            EngineMode::DevValidator,
            database,
            genesis(),
            MempoolConfig::default(),
        )
        .unwrap()
    }

    fn assert_block_vectors(proposal: ProposalId, pending: &ConsensusBlock) {
        let vectors = include_str!("../../../testdata/vectors/arbor-v1/m5-block-roots.txt");
        let expected = |name: &str| {
            vectors
                .lines()
                .filter_map(|line| line.split_once('='))
                .find_map(|(key, value)| (key == name).then_some(value))
                .unwrap()
        };
        assert_eq!(proposal.hash().to_string(), expected("consensus_hash"));
        for (actual, name) in [
            (pending.header.batches_root, "batches_root"),
            (pending.header.domain_results_root, "domain_results_root"),
            (pending.header.domain_heads_root, "domain_heads_root"),
            (
                arbor_crypto::domain_header_hash(&pending.domain_blocks[0]).unwrap(),
                "domain_block_hash",
            ),
            (pending.domain_blocks[0].state_root, "state_root"),
            (
                pending.domain_blocks[0].transactions_root,
                "transactions_root",
            ),
            (pending.domain_blocks[0].receipts_root, "receipts_root"),
        ] {
            assert_eq!(actual.to_string(), expected(name));
        }
    }

    #[test]
    fn production_mode_is_a_hard_failure() {
        let dir = tempdir().unwrap();
        let database = Database::open(dir.path(), identity(), RetentionPolicy::Archive).unwrap();
        assert!(matches!(
            SingleValidatorEngine::open(
                EngineMode::Production,
                database,
                genesis(),
                MempoolConfig::default()
            ),
            Err(ConsensusError::DevModeRequired)
        ));
    }

    #[test]
    fn proposal_is_invisible_abandon_restores_and_commit_survives_restart() {
        let dir = tempdir().unwrap();
        let mut engine = open(dir.path());
        assert_eq!(engine.finalized_marker().unwrap().unwrap().height, 0);
        let envelope = signed_transfer();
        let transaction = arbor_codec::decode_eip1559(&envelope).unwrap();
        let transaction_hash = arbor_crypto::eip1559_transaction_hash(&transaction).unwrap();
        engine
            .submit_raw(genesis().root_domain_id, envelope.clone())
            .unwrap();

        let before = engine
            .finalized_state()
            .domain(genesis().root_domain_id)
            .unwrap()
            .state
            .state_root();
        let proposal = engine.build_proposal(1_001).unwrap();
        assert_eq!(engine.mempool_len(), 0);
        assert_eq!(engine.finalized_marker().unwrap().unwrap().height, 0);
        assert_eq!(
            engine
                .finalized_state()
                .domain(genesis().root_domain_id)
                .unwrap()
                .state
                .state_root(),
            before
        );
        assert!(engine.receipt(transaction_hash).unwrap().is_none());
        engine.abandon_proposal(proposal).unwrap();
        assert_eq!(engine.mempool_len(), 1);

        let mut events = engine.subscribe_commits();
        let proposal = engine.build_proposal(1_001).unwrap();
        let pending = engine.pending_block(proposal).unwrap();
        assert_block_vectors(proposal, pending);
        engine.commit_proposal(proposal).unwrap();
        let event = events.try_recv().unwrap();
        assert_eq!(event.height, 1);
        assert!(engine.receipt(transaction_hash).unwrap().is_some());
        assert_eq!(
            engine
                .finalized_state()
                .domain(genesis().root_domain_id)
                .unwrap()
                .state
                .account(RECIPIENT)
                .unwrap()
                .unwrap()
                .balance,
            U256::from(123)
        );
        let committed_hash = event.consensus_hash;
        drop(engine);

        let reopened = open(dir.path());
        let marker = reopened.finalized_marker().unwrap().unwrap();
        assert_eq!(marker.height, 1);
        assert_eq!(marker.consensus_hash, committed_hash);
        assert_eq!(
            reopened
                .finalized_state()
                .domain(genesis().root_domain_id)
                .unwrap()
                .state
                .account(RECIPIENT)
                .unwrap()
                .unwrap()
                .balance,
            U256::from(123)
        );
        assert!(reopened.receipt(transaction_hash).unwrap().is_some());
    }

    #[test]
    fn crash_before_commit_reopens_only_the_old_head() {
        let dir = tempdir().unwrap();
        {
            let mut engine = open(dir.path());
            engine
                .submit_raw(genesis().root_domain_id, signed_transfer())
                .unwrap();
            engine.build_proposal(1_001).unwrap();
            assert_eq!(engine.finalized_marker().unwrap().unwrap().height, 0);
        }
        let reopened = open(dir.path());
        assert_eq!(reopened.finalized_marker().unwrap().unwrap().height, 0);
        assert!(
            reopened
                .finalized_state()
                .domain(genesis().root_domain_id)
                .unwrap()
                .state
                .account(RECIPIENT)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn subprocess_kill_keeps_block_state_marker_and_wal_consistent() {
        if std::env::var_os("ARBOR_M5_CRASH_WORKER").is_some() {
            return;
        }
        for delay_ms in [0_u64, 1, 5, 20] {
            let dir = tempdir().unwrap();
            drop(open(dir.path()));
            let ready = dir.path().join("proposal-ready");
            let child = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "tests::crash_commit_worker", "--nocapture"])
                .env("ARBOR_M5_CRASH_WORKER", "1")
                .env("ARBOR_M5_CRASH_DB", dir.path())
                .env("ARBOR_M5_CRASH_READY", &ready)
                .spawn()
                .unwrap();
            let guard = ProcessGuard::new(child);
            let deadline = Instant::now() + Duration::from_secs(5);
            while !ready.exists() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(5));
            }
            assert!(ready.exists(), "crash worker did not reach commit boundary");
            thread::sleep(Duration::from_millis(delay_ms));
            guard.kill_and_wait().unwrap();

            let reopened = open(dir.path());
            let marker = reopened.finalized_marker().unwrap().unwrap();
            assert!(marker.height <= 1);
            let recipient = reopened
                .finalized_state()
                .domain(genesis().root_domain_id)
                .unwrap()
                .state
                .account(RECIPIENT)
                .unwrap();
            if marker.height == 0 {
                assert!(recipient.is_none());
            } else {
                assert_eq!(recipient.unwrap().balance, U256::from(123));
            }
        }
    }

    #[test]
    fn crash_commit_worker() {
        if std::env::var_os("ARBOR_M5_CRASH_WORKER").is_none() {
            return;
        }
        let path = std::env::var_os("ARBOR_M5_CRASH_DB").unwrap();
        let ready = std::env::var_os("ARBOR_M5_CRASH_READY").unwrap();
        let mut engine = open(std::path::Path::new(&path));
        engine
            .submit_raw(genesis().root_domain_id, signed_transfer())
            .unwrap();
        let proposal = engine.build_proposal(1_001).unwrap();
        fs::write(ready, b"ready").unwrap();
        engine.commit_proposal(proposal).unwrap();
    }
}
