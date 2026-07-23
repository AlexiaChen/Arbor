//! Consensus application boundary and explicitly development-only single-validator engine.
//!
//! This crate does not contain a production BFT implementation. Candidate BFT crates remain
//! isolated under `spikes/` until ADR-004's durable-before-sign gate passes.

#![forbid(unsafe_code)]

mod checkpoint;

use std::collections::{BTreeMap, BTreeSet};

use alloy_primitives::{Address, B256, Bytes, U256, keccak256};
use arbor_chain::{
    ChainError, ChainMachine, ConsensusBlock, DomainConfig, FinalizedChainGenesis,
    FinalizedChainState, MAX_CONSENSUS_BLOCK_GAS, ValidatedProposal, decode_consensus_block,
    encode_consensus_block,
};
use arbor_crypto::{ConsensusSigner, consensus_header_hash, validator_id, validator_set_hash};
use arbor_evm::{ExecutionState, GenesisAccount};
use arbor_mempool::{Mempool, MempoolConfig, MempoolError, PoolEntry, QueueStatus};
use arbor_primitives::{DomainBatch, DomainId, NetworkId, Validator, ValidatorSet};
use arbor_storage::{
    CommitBatch, CommitStats, Database, DomainStateCommit, FinalizedMarker, IndexedValue,
    StorageError,
};
use arbor_system::{CHAIN_REGISTRY_ADDRESS, root_registry_genesis_storage};
use thiserror::Error;
use tokio::sync::broadcast;

pub use checkpoint::DevelopmentCheckpoint;

const DEV_GENESIS_TAG: &[u8] = b"ARBOR_DEV_GENESIS_RECORD_V1";
const DEV_WAL_TAG: &[u8] = b"ARBOR_DEV_COMMIT_WAL_V1";
const KEY_DEV_GENESIS: &[u8] = b"m5:dev:genesis";
const KEY_DEV_WAL: &[u8] = b"m5:dev:wal";
const KEY_DEV_CHECKPOINT: &[u8] = b"m7:dev:checkpoint";
const PREFIX_BLOCK: &[u8] = b"m5:block:";
const PREFIX_RECEIPT: &[u8] = b"m5:receipt:";
const PREFIX_TX_LOCATION: &[u8] = b"m5:tx:";

/// Hard cap used by the M5 single-domain proposer before M6 adds fair multi-domain scheduling.
pub const MAX_DEV_PROPOSAL_TRANSACTIONS: usize = 10_000;
/// Honest-proposer cap per domain before another active domain gets a scheduling turn.
pub const MAX_FAIR_DOMAIN_TRANSACTIONS: usize = 1_024;

/// Explicit capability required to construct the non-BFT development engine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EngineMode {
    /// Immediate single-validator finality for local development only.
    DevValidator,
    /// Production assembly, for which M5 deliberately provides no engine.
    Production,
}

/// Node-local transaction-history projection; never an input to proposal execution or validity.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum DomainHistoryRetention {
    /// Persist receipt and transaction-location indexes for every domain.
    #[default]
    All,
    /// Persist those history indexes only for the listed domains.
    Selected(BTreeSet<DomainId>),
}

impl DomainHistoryRetention {
    /// Builds an explicit selected-domain projection.
    #[must_use]
    pub fn selected(domains: impl IntoIterator<Item = DomainId>) -> Self {
        Self::Selected(domains.into_iter().collect())
    }

    /// Returns whether derived transaction history is retained for `domain_id`.
    #[must_use]
    pub fn retains(&self, domain_id: DomainId) -> bool {
        match self {
            Self::All => true,
            Self::Selected(domains) => domains.contains(&domain_id),
        }
    }
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
    /// Root-governance executor authorized for native registry lifecycle calls.
    pub governance_address: Address,
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
        let mut governance_bytes = [0_u8; 20];
        governance_bytes[18..].copy_from_slice(&[0x0f, 0xef]);
        let governance_address = Address::from(governance_bytes);
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
            governance_address,
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
        let state = self.execution_state()?;
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
        bytes.extend_from_slice(self.governance_address.as_slice());
        bytes.extend_from_slice(state.state_root().as_slice());
        Ok(keccak256(bytes))
    }

    fn chain_state(&self) -> Result<FinalizedChainState, ConsensusError> {
        let validator_hash = validator_set_hash(&self.validator_set)
            .map_err(|error| ConsensusError::InvalidGenesisOwned(error.to_string()))?;
        let state = self.execution_state()?;
        FinalizedChainState::genesis(FinalizedChainGenesis {
            network_id: self.network_id,
            consensus_hash: self.genesis_hash,
            timestamp: self.timestamp,
            validator_set_hash: validator_hash,
            root_domain_id: self.root_domain_id,
            governance_address: self.governance_address,
            config: self.root_config,
            state,
        })
        .map_err(ConsensusError::from)
    }

    fn execution_state(&self) -> Result<ExecutionState, ConsensusError> {
        let mut allocations = self.allocations.clone();
        let registry = allocations.entry(CHAIN_REGISTRY_ADDRESS).or_default();
        if !registry.code.is_empty() {
            return Err(ConsensusError::InvalidGenesis(
                "native ChainRegistry address cannot contain bytecode",
            ));
        }
        if registry.nonce != 0 {
            return Err(ConsensusError::InvalidGenesis(
                "native ChainRegistry genesis nonce is reserved",
            ));
        }
        // EIP-161 emptiness ignores storage_root, so a storage-only native account would be
        // removed while materializing genesis. The fixed nonce keeps the registry account alive.
        registry.nonce = 1;
        for (slot, value) in
            root_registry_genesis_storage(self.root_domain_id, self.root_config.chain_id)
        {
            if registry.storage.insert(slot, value).is_some() {
                return Err(ConsensusError::InvalidGenesis(
                    "native ChainRegistry genesis slot collision",
                ));
            }
        }
        ExecutionState::from_genesis(&allocations)
            .map_err(|error| ConsensusError::InvalidGenesisOwned(error.to_string()))
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
    /// A transaction targets a domain absent from the finalized registry.
    #[error("transaction targets an unknown domain")]
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
    scheduler_cursor: usize,
    history_retention: DomainHistoryRetention,
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
        Self::open_with_history(
            mode,
            database,
            genesis,
            mempool_config,
            DomainHistoryRetention::All,
        )
    }

    /// Opens a development engine with a node-local derived-history projection.
    ///
    /// The projection is applied only while persisting receipt and transaction-location indexes;
    /// every domain is still executed and its latest authenticated state remains durable.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::open`].
    pub fn open_with_history(
        mode: EngineMode,
        database: Database,
        genesis: DevGenesis,
        mempool_config: MempoolConfig,
        history_retention: DomainHistoryRetention,
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
                if let Some(checkpoint) =
                    checkpoint::load_development_checkpoint(&database, &genesis)?
                {
                    if checkpoint.state.height.0 > marker.height {
                        return Err(ConsensusError::InconsistentStore(
                            "checkpoint height exceeds finalized marker",
                        ));
                    }
                    finalized = checkpoint.state;
                }
                for height in finalized.height.0.saturating_add(1)..=marker.height {
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
        for (domain_id, domain) in finalized.domains() {
            mempool.register_domain(domain_id, domain.config.chain_id)?;
        }
        let (events, _) = broadcast::channel(64);
        Ok(Self {
            database,
            genesis,
            finalized,
            mempool,
            pending: None,
            events,
            scheduler_cursor: 0,
            history_retention,
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

    /// Loads one locally finalized block body for M7 block sync serving.
    ///
    /// Height zero is represented by the configured genesis identity and has no encoded block
    /// body. A missing height inside the finalized range is treated as database inconsistency.
    ///
    /// # Errors
    ///
    /// Returns [`ConsensusError`] for corrupt storage or a malformed persisted block.
    pub fn finalized_block(&self, height: u64) -> Result<Option<ConsensusBlock>, ConsensusError> {
        if height == 0 || height > self.finalized.height.0 {
            return Ok(None);
        }
        let Some(bytes) = self.database.index(&block_key(height))? else {
            if checkpoint::checkpoint_height(&self.database)?.is_some_and(|base| height <= base) {
                return Ok(None);
            }
            return Err(ConsensusError::InconsistentStore(
                "missing finalized block body",
            ));
        };
        Ok(Some(decode_consensus_block(&bytes)?))
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
        let selected = self.select_transactions()?;
        let hashes = selected
            .iter()
            .map(|entry| entry.transaction_hash)
            .collect::<BTreeSet<_>>();
        let reserved = self.mempool.reserve_hashes(&hashes);
        let mut grouped = BTreeMap::<DomainId, Vec<Bytes>>::new();
        for entry in &reserved {
            grouped
                .entry(entry.domain_id)
                .or_default()
                .push(entry.envelope.clone());
        }
        let batches = grouped
            .into_iter()
            .map(|(domain_id, transactions)| {
                let parent = self
                    .finalized
                    .domain(domain_id)
                    .ok_or(ConsensusError::UnknownDomain)?;
                Ok(DomainBatch {
                    domain_id,
                    parent_domain_block_hash: parent.block_hash,
                    transactions,
                })
            })
            .collect::<Result<Vec<_>, ConsensusError>>()?;
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
        let stats = match self.commit_validated(&pending.proposal) {
            Ok(stats) => stats,
            Err(error) => {
                self.pending = Some(pending);
                return Err(error);
            }
        };
        Ok(stats)
    }

    /// Replays and durably imports one externally obtained next-height block in development mode.
    ///
    /// This method verifies every consensus header, collection root, domain parent, transaction,
    /// execution result, and resulting state root against the local finalized parent before the
    /// parity-db transaction is built. It does not prove production finality: the M7 sync adapter
    /// must first apply its selected finality verifier, and M8 must supply the accepted BFT proof.
    ///
    /// # Errors
    ///
    /// Returns a typed validation/storage error without publishing an invalid finalized view.
    pub fn import_development_finalized_block(
        &mut self,
        block: &ConsensusBlock,
    ) -> Result<CommitStats, ConsensusError> {
        if self.pending.is_some() {
            return Err(ConsensusError::ProposalPending);
        }
        let proposal = ChainMachine.validate_proposal(&self.finalized, block)?;
        self.commit_validated(&proposal)
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

    /// Serves a bounded node-local projection of finalized domain headers.
    ///
    /// This history is never an input to proposal validation. A node that did not retain the
    /// selected domain, or imported a checkpoint without older bodies, returns only locally
    /// available records.
    ///
    /// # Errors
    ///
    /// Returns a storage or canonical encoding error.
    pub fn domain_history(
        &self,
        domain_id: DomainId,
        start_number: u64,
        limit: usize,
    ) -> Result<Vec<Vec<u8>>, ConsensusError> {
        if limit == 0 || !self.history_retention.retains(domain_id) {
            return Ok(Vec::new());
        }
        let mut records = Vec::new();
        for height in 1..=self.finalized.height.0 {
            let Some(block) = self.finalized_block(height)? else {
                continue;
            };
            for header in block
                .domain_blocks
                .iter()
                .filter(|header| header.domain_id == domain_id && header.number.0 >= start_number)
            {
                records.push(
                    arbor_codec::encode_domain_header(header)
                        .map_err(|error| ConsensusError::InvalidGenesisOwned(error.to_string()))?,
                );
                if records.len() == limit {
                    return Ok(records);
                }
            }
        }
        Ok(records)
    }

    /// Number of unreserved local transactions.
    #[must_use]
    pub fn mempool_len(&self) -> usize {
        self.mempool.len()
    }

    fn commit_validated(
        &mut self,
        proposal: &ValidatedProposal,
    ) -> Result<CommitStats, ConsensusError> {
        let created_domains = proposal
            .executions()
            .iter()
            .flat_map(|execution| &execution.created_domains)
            .map(|created| {
                (
                    created.descriptor.domain_id,
                    created.descriptor.evm_chain_id,
                )
            })
            .collect::<Vec<_>>();
        let mut next_mempool = self.mempool.clone();
        for (domain_id, chain_id) in &created_domains {
            next_mempool.register_domain(*domain_id, *chain_id)?;
        }
        let included_hashes = proposal
            .executions()
            .iter()
            .flat_map(|execution| &execution.transactions)
            .map(|transaction| transaction.transaction_hash)
            .collect::<BTreeSet<_>>();
        drop(next_mempool.reserve_hashes(&included_hashes));
        let commit = proposal_commit_batch(proposal, &self.history_retention)?;
        let stats = self.database.commit(commit)?;
        self.finalized = proposal.resulting_state().clone();
        self.mempool = next_mempool;
        self.scheduler_cursor =
            self.scheduler_cursor.saturating_add(1) % self.finalized.domains().count().max(1);
        let event = CommitEvent {
            height: self.finalized.height.0,
            consensus_hash: self.finalized.consensus_hash,
            domain_heads_root: self.finalized.domain_heads_root(),
        };
        let _ = self.events.send(event);
        Ok(stats)
    }

    fn select_transactions(&self) -> Result<Vec<PoolEntry>, ConsensusError> {
        let mut ready_by_domain = Vec::<(DomainId, u64, Vec<PoolEntry>)>::new();
        for (domain_id, domain) in self.finalized.domains() {
            let mut ready = Vec::new();
            for sender in self.mempool.senders(domain_id) {
                let nonce = domain
                    .state
                    .account(sender)
                    .map_err(|error| ConsensusError::InvalidGenesisOwned(error.to_string()))?
                    .map_or(0, |account| account.nonce);
                ready.extend(
                    self.mempool
                        .ready(domain_id, sender, nonce)
                        .into_iter()
                        .cloned(),
                );
            }
            ready.sort_by_key(|entry| (entry.sender, entry.transaction.nonce));
            if !ready.is_empty() {
                ready_by_domain.push((domain_id, domain.config.gas_limit, ready));
            }
        }
        if ready_by_domain.is_empty() {
            return Ok(Vec::new());
        }
        let domain_count = ready_by_domain.len();
        let fair_share = MAX_CONSENSUS_BLOCK_GAS
            / u64::try_from(domain_count).expect("bounded domain count fits u64");
        ready_by_domain.rotate_left(self.scheduler_cursor % domain_count);
        let mut selected = Vec::new();
        let mut aggregate_gas = 0_u64;
        for (_, domain_limit, entries) in ready_by_domain {
            let quota = fair_share.min(domain_limit);
            let mut domain_gas = 0_u64;
            for entry in entries.into_iter().take(MAX_FAIR_DOMAIN_TRANSACTIONS) {
                let Some(next_domain_gas) = domain_gas.checked_add(entry.transaction.gas_limit)
                else {
                    break;
                };
                let Some(next_aggregate) = aggregate_gas.checked_add(entry.transaction.gas_limit)
                else {
                    break;
                };
                if next_domain_gas > domain_limit
                    || next_aggregate > MAX_CONSENSUS_BLOCK_GAS
                    || (next_domain_gas > quota && domain_gas != 0)
                    || selected.len() >= MAX_DEV_PROPOSAL_TRANSACTIONS
                {
                    break;
                }
                domain_gas = next_domain_gas;
                aggregate_gas = next_aggregate;
                selected.push(entry);
            }
        }
        selected.sort_by_key(|entry| (entry.domain_id, entry.sender, entry.transaction.nonce));
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
        consensus_height: domain.header.consensus_height.0,
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

fn proposal_commit_batch(
    proposal: &ValidatedProposal,
    history_retention: &DomainHistoryRetention,
) -> Result<CommitBatch, ConsensusError> {
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
            consensus_height: domain_header.consensus_height.0,
            snapshot: execution.state.snapshot().clone(),
        });
        commit
            .contract_code
            .extend(execution.state.contract_code().clone());
        if history_retention.retains(batch.domain_id) {
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
        for created in &execution.created_domains {
            commit.states.push(DomainStateCommit {
                domain_id: created.descriptor.domain_id,
                consensus_height: block.header.height.0,
                snapshot: created.genesis_state.snapshot().clone(),
            });
            commit
                .contract_code
                .extend(created.genesis_state.contract_code().clone());
            commit.domain_registry.push(IndexedValue {
                key: created.descriptor.domain_id.0.to_vec(),
                value: arbor_codec::encode_domain_descriptor(&created.descriptor)
                    .map_err(|error| ConsensusError::InvalidGenesisOwned(error.to_string()))?,
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
    use arbor_crypto::{ConsensusSigner, derive_domain_id, eip1559_transaction_hash, validator_id};
    use arbor_primitives::{Eip1559Transaction, Validator};
    use arbor_storage::{DatabaseIdentity, RetentionPolicy};
    use arbor_system::{
        CHAIN_REGISTRY_ADDRESS, CreateChainRequest, CreationDepositStatus, MIN_CREATION_DEPOSIT,
        deposit_slot, deposit_status_slot, descriptor_slot, encode_burn_deposit_call,
        encode_create_chain_call, encode_refund_deposit_call,
    };
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
            governance_address: address!("0000000000000000000000000000000000000fef"),
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
        sign(Eip1559Transaction {
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
        })
    }

    fn sign(transaction: Eip1559Transaction) -> Bytes {
        sign_with_secret(transaction, [7_u8; 32])
    }

    fn sign_with_secret(mut transaction: Eip1559Transaction, secret: [u8; 32]) -> Bytes {
        let payload = encode_eip1559_signing_payload(&transaction).unwrap();
        let digest = keccak256(payload);
        let key = SigningKey::from_bytes((&secret).into()).unwrap();
        let (signature, recovery_id) = key.sign_prehash_recoverable(digest.as_slice()).unwrap();
        let bytes = signature.to_bytes();
        transaction.r = U256::from_be_slice(&bytes[..32]);
        transaction.s = U256::from_be_slice(&bytes[32..]);
        transaction.y_parity = recovery_id.is_y_odd();
        encode_eip1559(&transaction).unwrap().into()
    }

    fn address_for_secret(secret: [u8; 32]) -> Address {
        let key = SigningKey::from_bytes((&secret).into()).unwrap();
        let public = key.verifying_key().to_encoded_point(false);
        Address::from_slice(&keccak256(&public.as_bytes()[1..]).as_slice()[12..])
    }

    fn lifecycle_transaction(nonce: u64, input: Bytes, secret: [u8; 32]) -> Bytes {
        sign_with_secret(
            Eip1559Transaction {
                chain_id: 2_048,
                nonce,
                max_priority_fee_per_gas: 2,
                max_fee_per_gas: 20,
                gas_limit: 250_000,
                to: Some(CHAIN_REGISTRY_ADDRESS),
                value: U256::ZERO,
                input,
                access_list: Vec::new(),
                y_parity: false,
                r: U256::ZERO,
                s: U256::ZERO,
            },
            secret,
        )
    }

    fn create_chain_transaction(
        nonce: u64,
        parent_domain_id: DomainId,
        evm_chain_id: u64,
        name: &str,
    ) -> Bytes {
        create_chain_transaction_with_value(
            nonce,
            parent_domain_id,
            evm_chain_id,
            name,
            MIN_CREATION_DEPOSIT,
        )
    }

    fn create_chain_transaction_with_value(
        nonce: u64,
        parent_domain_id: DomainId,
        evm_chain_id: u64,
        name: &str,
        value: U256,
    ) -> Bytes {
        sign(Eip1559Transaction {
            chain_id: 2_048,
            nonce,
            max_priority_fee_per_gas: 2,
            max_fee_per_gas: 20,
            gas_limit: 500_000,
            to: Some(CHAIN_REGISTRY_ADDRESS),
            value,
            input: encode_create_chain_call(&CreateChainRequest {
                parent_domain_id,
                name: name.to_owned(),
                symbol: "TST".to_owned(),
                evm_chain_id,
                owner: SENDER,
                gas_limit: 20_000_000,
                initial_base_fee: 10,
                initial_supply: U256::from(10_u128.pow(18)),
                protocol_revision: 1,
            })
            .unwrap(),
            access_list: Vec::new(),
            y_parity: false,
            r: U256::ZERO,
            s: U256::ZERO,
        })
    }

    fn domain_transfer(chain_id: u64, nonce: u64, value: u64) -> Bytes {
        domain_transaction(
            chain_id,
            nonce,
            Some(RECIPIENT),
            Bytes::new(),
            U256::from(value),
            21_000,
        )
    }

    fn domain_transaction(
        chain_id: u64,
        nonce: u64,
        to: Option<Address>,
        input: Bytes,
        value: U256,
        gas_limit: u64,
    ) -> Bytes {
        sign(Eip1559Transaction {
            chain_id,
            nonce,
            max_priority_fee_per_gas: 2,
            max_fee_per_gas: 20,
            gas_limit,
            to,
            value,
            input,
            access_list: Vec::new(),
            y_parity: false,
            r: U256::ZERO,
            s: U256::ZERO,
        })
    }

    fn initcode(runtime: &[u8]) -> Bytes {
        let offset = 12_u8;
        let length = u8::try_from(runtime.len()).unwrap();
        let mut code = vec![
            0x60, length, 0x60, offset, 0x60, 0x00, 0x39, 0x60, length, 0x60, 0x00, 0xf3,
        ];
        code.extend_from_slice(runtime);
        code.into()
    }

    fn m6_genesis() -> DevGenesis {
        let mut genesis = genesis();
        genesis.allocations.get_mut(&SENDER).unwrap().balance = U256::from(10_u128.pow(24));
        genesis
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

    fn open_m6(path: &std::path::Path) -> SingleValidatorEngine {
        let database = Database::open(path, identity(), RetentionPolicy::Archive).unwrap();
        SingleValidatorEngine::open(
            EngineMode::DevValidator,
            database,
            m6_genesis(),
            MempoolConfig::default(),
        )
        .unwrap()
    }

    fn open_m6_with_history(
        path: &std::path::Path,
        history: DomainHistoryRetention,
    ) -> SingleValidatorEngine {
        let database = Database::open(path, identity(), RetentionPolicy::Archive).unwrap();
        SingleValidatorEngine::open_with_history(
            EngineMode::DevValidator,
            database,
            m6_genesis(),
            MempoolConfig::default(),
            history,
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
    fn development_block_import_replays_before_durable_publication() {
        let source_dir = tempdir().unwrap();
        let target_dir = tempdir().unwrap();
        let mut source = open(source_dir.path());
        source
            .submit_raw(genesis().root_domain_id, signed_transfer())
            .unwrap();
        source.produce_block(1_001).unwrap();
        let block = source.finalized_block(1).unwrap().unwrap();

        let mut target = open(target_dir.path());
        target
            .submit_raw(genesis().root_domain_id, signed_transfer())
            .unwrap();
        assert_eq!(target.mempool_len(), 1);
        let mut invalid = block.clone();
        invalid.header.domain_heads_root.0[0] ^= 1;
        assert!(target.import_development_finalized_block(&invalid).is_err());
        assert_eq!(target.finalized_marker().unwrap().unwrap().height, 0);
        assert!(
            target
                .finalized_state()
                .domain(genesis().root_domain_id)
                .unwrap()
                .state
                .account(RECIPIENT)
                .unwrap()
                .is_none()
        );

        target.import_development_finalized_block(&block).unwrap();
        assert_eq!(target.mempool_len(), 0);
        assert_eq!(target.finalized_state(), source.finalized_state());
        assert_eq!(target.finalized_block(1).unwrap(), Some(block));
        drop(target);
        let reopened = open(target_dir.path());
        assert_eq!(reopened.finalized_state(), source.finalized_state());
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
    #[allow(clippy::too_many_lines)]
    fn chain_registry_creates_tree_domains_schedules_batches_and_replays() {
        let dir = tempdir().unwrap();
        let root = m6_genesis().root_domain_id;
        let mut engine = open_m6(dir.path());
        let root_joint = engine.finalized_state().domain(root).unwrap().block_hash;
        assert_ne!(
            engine
                .finalized_state()
                .domain(root)
                .unwrap()
                .state
                .storage(CHAIN_REGISTRY_ADDRESS, descriptor_slot(root))
                .unwrap(),
            U256::ZERO
        );

        let child_envelope = create_chain_transaction(0, root, 2_049, "Child");
        let child_transaction = arbor_codec::decode_eip1559(&child_envelope).unwrap();
        let child_tx_hash = eip1559_transaction_hash(&child_transaction).unwrap();
        let child_id = derive_domain_id(identity().network_id, root, child_tx_hash);
        engine.submit_raw(root, child_envelope).unwrap();
        let proposal = engine.build_proposal(1_001).unwrap();
        assert_eq!(
            engine.pending.as_ref().unwrap().proposal.executions()[0]
                .created_domains
                .len(),
            1,
            "registry execution: {:?}",
            engine.pending.as_ref().unwrap().proposal.executions()[0].transactions
        );
        assert!(engine.finalized_state().domain(child_id).is_none());
        engine.commit_proposal(proposal).unwrap();

        let child = engine.finalized_state().domain(child_id).unwrap();
        assert_eq!(child.header.number.0, 0);
        assert_eq!(
            child.state.account(SENDER).unwrap().unwrap().balance,
            U256::from(10_u128.pow(18))
        );
        let descriptor = engine
            .finalized_state()
            .domain_descriptor(child_id)
            .unwrap();
        let vectors = include_str!("../../../testdata/vectors/arbor-v1/m6-domain-roots.txt");
        let expected = |name: &str| {
            vectors
                .lines()
                .filter_map(|line| line.split_once('='))
                .find_map(|(key, value)| (key == name).then_some(value))
                .unwrap()
        };
        for (actual, name) in [
            (child_tx_hash, "create_tx_hash"),
            (child_id.0, "child_domain_id"),
            (descriptor.origin_hash, "child_origin_hash"),
            (descriptor.joint_domain_block_hash, "child_joint"),
            (child.block_hash, "child_genesis_block_hash"),
            (child.header.state_root, "child_genesis_state_root"),
            (
                engine.finalized_state().consensus_hash,
                "height_one_consensus_hash",
            ),
            (
                engine.finalized_state().domain_heads_root(),
                "height_one_domain_heads_root",
            ),
        ] {
            assert_eq!(actual.to_string(), expected(name));
        }
        assert_eq!(descriptor.parent_domain_id, root);
        assert_eq!(descriptor.joint_domain_block_hash, root_joint);
        assert_eq!(
            engine.finalized_state().genealogy(child_id).unwrap(),
            vec![root, child_id]
        );
        let proof = engine
            .finalized_state()
            .domain_head_proof(child_id)
            .unwrap();
        proof.verify().unwrap();

        // A conflicting chain ID is an ordinary status-zero EVM result: it consumes the root
        // nonce but cannot create another domain or alter the existing descriptor.
        let conflict = create_chain_transaction(1, root, 2_049, "Conflict");
        let conflict_tx = arbor_codec::decode_eip1559(&conflict).unwrap();
        let conflict_id = derive_domain_id(
            identity().network_id,
            root,
            eip1559_transaction_hash(&conflict_tx).unwrap(),
        );
        engine.submit_raw(root, conflict).unwrap();
        engine.produce_block(1_002).unwrap();
        assert!(engine.finalized_state().domain(conflict_id).is_none());

        // Execute a child batch in the same proposal that creates its grandchild. The joint must
        // remain the child head captured before either batch executes.
        let child_joint = engine
            .finalized_state()
            .domain(child_id)
            .unwrap()
            .block_hash;
        let grandchild_envelope = create_chain_transaction(2, child_id, 2_050, "Grandchild");
        let grandchild_tx = arbor_codec::decode_eip1559(&grandchild_envelope).unwrap();
        let grandchild_id = derive_domain_id(
            identity().network_id,
            child_id,
            eip1559_transaction_hash(&grandchild_tx).unwrap(),
        );
        engine.submit_raw(root, grandchild_envelope).unwrap();
        engine
            .submit_raw(child_id, domain_transfer(2_049, 0, 77))
            .unwrap();
        let proposal = engine.build_proposal(1_003).unwrap();
        assert_eq!(engine.pending_block(proposal).unwrap().batches.len(), 2);
        engine.commit_proposal(proposal).unwrap();
        assert_ne!(
            engine
                .finalized_state()
                .domain(child_id)
                .unwrap()
                .block_hash,
            child_joint
        );
        assert_eq!(
            engine
                .finalized_state()
                .domain_descriptor(grandchild_id)
                .unwrap()
                .joint_domain_block_hash,
            child_joint
        );
        assert_eq!(
            engine.finalized_state().genealogy(grandchild_id).unwrap(),
            vec![root, child_id, grandchild_id]
        );
        assert_eq!(
            engine
                .finalized_state()
                .domain(child_id)
                .unwrap()
                .state
                .account(RECIPIENT)
                .unwrap()
                .unwrap()
                .balance,
            U256::from(77)
        );

        // A newly finalized grandchild is admitted by the mempool only from the following height.
        engine
            .submit_raw(grandchild_id, domain_transfer(2_050, 0, 88))
            .unwrap();
        engine.produce_block(1_004).unwrap();
        assert_eq!(
            engine
                .finalized_state()
                .domain(grandchild_id)
                .unwrap()
                .state
                .account(RECIPIENT)
                .unwrap()
                .unwrap()
                .balance,
            U256::from(88)
        );
        let final_hash = engine.finalized_state().consensus_hash;
        drop(engine);

        let reopened = open_m6(dir.path());
        assert_eq!(reopened.finalized_state().consensus_hash, final_hash);
        assert_eq!(
            reopened.finalized_state().genealogy(grandchild_id).unwrap(),
            vec![root, child_id, grandchild_id]
        );
        let expected_descriptor = arbor_codec::encode_domain_descriptor(
            reopened
                .finalized_state()
                .domain_descriptor(grandchild_id)
                .unwrap(),
        )
        .unwrap();
        drop(reopened);
        let database = Database::open(dir.path(), identity(), RetentionPolicy::Archive).unwrap();
        assert_eq!(
            database.domain_registry(grandchild_id).unwrap().unwrap(),
            expected_descriptor
        );
    }

    #[test]
    fn chain_registry_allows_duplicate_names_and_reverts_rejected_creations() {
        let dir = tempdir().unwrap();
        let root = m6_genesis().root_domain_id;
        let mut engine = open_m6(dir.path());

        let first = create_chain_transaction(0, root, 2_049, "Repeated Name");
        let second = create_chain_transaction(1, root, 2_050, "Repeated Name");
        let first_hash =
            eip1559_transaction_hash(&arbor_codec::decode_eip1559(&first).unwrap()).unwrap();
        let second_hash =
            eip1559_transaction_hash(&arbor_codec::decode_eip1559(&second).unwrap()).unwrap();
        let first_id = derive_domain_id(identity().network_id, root, first_hash);
        let second_id = derive_domain_id(identity().network_id, root, second_hash);
        engine.submit_raw(root, first).unwrap();
        engine.submit_raw(root, second).unwrap();
        engine.produce_block(1_001).unwrap();

        assert_ne!(first_id, second_id);
        for domain_id in [first_id, second_id] {
            assert_eq!(
                engine
                    .finalized_state()
                    .domain_descriptor(domain_id)
                    .unwrap()
                    .name,
                "Repeated Name"
            );
        }

        let underfunded = create_chain_transaction_with_value(
            2,
            root,
            2_051,
            "Underfunded",
            MIN_CREATION_DEPOSIT - U256::from(1),
        );
        let unknown_parent = create_chain_transaction(
            3,
            DomainId(B256::repeat_byte(0x99)),
            2_052,
            "Unknown Parent",
        );
        let chain_id_conflict = create_chain_transaction(4, root, 2_049, "Chain ID Conflict");
        let rejected = [underfunded, unknown_parent, chain_id_conflict]
            .into_iter()
            .map(|envelope| {
                let transaction = arbor_codec::decode_eip1559(&envelope).unwrap();
                let hash = eip1559_transaction_hash(&transaction).unwrap();
                let request = arbor_system::decode_create_chain_call(&transaction.input).unwrap();
                let domain_id =
                    derive_domain_id(identity().network_id, request.parent_domain_id, hash);
                (envelope, hash, domain_id)
            })
            .collect::<Vec<_>>();
        for (envelope, _, _) in &rejected {
            engine.submit_raw(root, envelope.clone()).unwrap();
        }
        engine.produce_block(1_002).unwrap();

        for (_, hash, domain_id) in rejected {
            let receipt = engine.receipt(hash).unwrap().unwrap();
            assert!(
                !arbor_codec::decode_eip1559_receipt(&receipt)
                    .unwrap()
                    .status
            );
            assert!(engine.finalized_state().domain(domain_id).is_none());
        }
        assert_eq!(
            engine
                .finalized_state()
                .domain(root)
                .unwrap()
                .state
                .account(SENDER)
                .unwrap()
                .unwrap()
                .nonce,
            5
        );
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn parent_child_nonce_balance_code_and_storage_are_fully_isolated() {
        const STORAGE_RUNTIME: &[u8] = &[
            0x60, 0x00, 0x35, 0x80, 0x60, 0x00, 0x55, 0x60, 0x00, 0x52, 0x60, 0x20, 0x60, 0x00,
            0xf3,
        ];
        const PARENT_RUNTIME: &[u8] = &[0x00];
        const PARENT_ONLY: Address = address!("0000000000000000000000000000000000000cab");

        let dir = tempdir().unwrap();
        let mut genesis = m6_genesis();
        genesis.allocations.insert(
            PARENT_ONLY,
            GenesisAccount {
                nonce: 7,
                balance: U256::from(777),
                code: Bytes::from_static(PARENT_RUNTIME),
                storage: BTreeMap::from([(U256::ZERO, U256::from(99))]),
            },
        );
        let root = genesis.root_domain_id;
        let database = Database::open(dir.path(), identity(), RetentionPolicy::Archive).unwrap();
        let mut engine = SingleValidatorEngine::open(
            EngineMode::DevValidator,
            database,
            genesis,
            MempoolConfig::default(),
        )
        .unwrap();

        let creation = create_chain_transaction(0, root, 2_049, "Isolated");
        let creation_hash =
            eip1559_transaction_hash(&arbor_codec::decode_eip1559(&creation).unwrap()).unwrap();
        let child_id = derive_domain_id(identity().network_id, root, creation_hash);
        engine.submit_raw(root, creation).unwrap();
        engine.produce_block(1_001).unwrap();

        let parent_code_hash = keccak256(PARENT_RUNTIME);
        let root_state = &engine.finalized_state().domain(root).unwrap().state;
        let parent_account = root_state.account(PARENT_ONLY).unwrap().unwrap();
        assert_eq!(parent_account.nonce, 7);
        assert_eq!(parent_account.balance, U256::from(777));
        assert_eq!(parent_account.code_hash, parent_code_hash);
        assert_eq!(
            root_state.storage(PARENT_ONLY, U256::ZERO).unwrap(),
            U256::from(99)
        );

        let child_state = &engine.finalized_state().domain(child_id).unwrap().state;
        assert!(child_state.account(PARENT_ONLY).unwrap().is_none());
        assert_eq!(
            child_state.storage(PARENT_ONLY, U256::ZERO).unwrap(),
            U256::ZERO
        );
        assert!(!child_state.contract_code().contains_key(&parent_code_hash));

        let root_head_before_child_execution =
            engine.finalized_state().domain(root).unwrap().clone();
        let deploy = domain_transaction(
            2_049,
            0,
            None,
            initcode(STORAGE_RUNTIME),
            U256::ZERO,
            250_000,
        );
        engine.submit_raw(child_id, deploy).unwrap();
        let proposal = engine.build_proposal(1_002).unwrap();
        let contract = engine.pending.as_ref().unwrap().proposal.executions()[0].transactions[0]
            .contract_address
            .unwrap();
        engine.commit_proposal(proposal).unwrap();

        let mut word = [0_u8; 32];
        word[31] = 42;
        engine
            .submit_raw(
                child_id,
                domain_transaction(
                    2_049,
                    1,
                    Some(contract),
                    Bytes::copy_from_slice(&word),
                    U256::ZERO,
                    150_000,
                ),
            )
            .unwrap();
        engine.produce_block(1_003).unwrap();

        let child_state = &engine.finalized_state().domain(child_id).unwrap().state;
        let child_sender = child_state.account(SENDER).unwrap().unwrap();
        let child_contract = child_state.account(contract).unwrap().unwrap();
        let child_code_hash = keccak256(STORAGE_RUNTIME);
        assert_eq!(child_sender.nonce, 2);
        assert!(child_sender.balance < U256::from(10_u128.pow(18)));
        assert_eq!(child_contract.code_hash, child_code_hash);
        assert_eq!(
            child_state.storage(contract, U256::ZERO).unwrap(),
            U256::from(42)
        );
        assert!(child_state.contract_code().contains_key(&child_code_hash));

        let root_after_child_execution = engine.finalized_state().domain(root).unwrap();
        assert_eq!(
            root_after_child_execution,
            &root_head_before_child_execution
        );
        assert!(
            root_after_child_execution
                .state
                .account(contract)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            root_after_child_execution
                .state
                .storage(contract, U256::ZERO)
                .unwrap(),
            U256::ZERO
        );
        assert!(
            !root_after_child_execution
                .state
                .contract_code()
                .contains_key(&child_code_hash)
        );
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn idle_domains_do_not_advance_and_fair_scheduler_rotates_without_starvation() {
        let dir = tempdir().unwrap();
        let root = m6_genesis().root_domain_id;
        let mut engine = open_m6(dir.path());
        let mut children = Vec::new();
        for offset in 0_u64..7 {
            let chain_id = 2_049 + offset;
            let creation = create_chain_transaction(offset, root, chain_id, "Same Display Name");
            let hash =
                eip1559_transaction_hash(&arbor_codec::decode_eip1559(&creation).unwrap()).unwrap();
            children.push((
                derive_domain_id(identity().network_id, root, hash),
                chain_id,
            ));
            engine.submit_raw(root, creation).unwrap();
        }
        engine.produce_block(1_001).unwrap();
        assert_eq!(
            children
                .iter()
                .map(|(domain_id, _)| *domain_id)
                .collect::<BTreeSet<_>>()
                .len(),
            children.len()
        );
        for (domain_id, _) in &children {
            assert_eq!(
                engine
                    .finalized_state()
                    .domain_descriptor(*domain_id)
                    .unwrap()
                    .name,
                "Same Display Name"
            );
        }

        let idle_heads = children
            .iter()
            .map(|(domain_id, _)| {
                (
                    *domain_id,
                    engine.finalized_state().domain(*domain_id).unwrap().clone(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        engine
            .submit_raw(root, domain_transfer(2_048, 7, 1))
            .unwrap();
        engine.produce_block(1_002).unwrap();
        for (domain_id, idle) in &idle_heads {
            let after = engine.finalized_state().domain(*domain_id).unwrap();
            assert_eq!(after.header.number, idle.header.number);
            assert_eq!(after.header.base_fee_per_gas, idle.header.base_fee_per_gas);
            assert_eq!(after.block_hash, idle.block_hash);
            assert_eq!(after, idle);
        }

        let mut active = vec![(root, 2_048, 8_u64)];
        active.extend(
            children
                .iter()
                .map(|(domain_id, chain_id)| (*domain_id, *chain_id, 0)),
        );
        for (domain_id, chain_id, first_nonce) in &active {
            let gas_limit = engine
                .finalized_state()
                .domain(*domain_id)
                .unwrap()
                .config
                .gas_limit;
            for nonce in *first_nonce..(*first_nonce + 16) {
                engine
                    .submit_raw(
                        *domain_id,
                        domain_transaction(
                            *chain_id,
                            nonce,
                            Some(RECIPIENT),
                            Bytes::new(),
                            U256::from(1),
                            gas_limit,
                        ),
                    )
                    .unwrap();
            }
        }

        let expected = active
            .iter()
            .map(|(domain_id, _, _)| *domain_id)
            .collect::<BTreeSet<_>>();
        let mut seen = BTreeSet::new();
        let mut selected_sets = BTreeSet::new();
        for round in 0_u64..u64::try_from(active.len()).unwrap() {
            let proposal = engine.build_proposal(1_003 + round).unwrap();
            let block = engine.pending_block(proposal).unwrap();
            let selected = block
                .batches
                .iter()
                .map(|batch| {
                    assert_eq!(batch.transactions.len(), 1, "per-domain fair quota");
                    batch.domain_id
                })
                .collect::<BTreeSet<_>>();
            assert!(!selected.is_empty());
            seen.extend(selected.iter().copied());
            selected_sets.insert(selected);
            engine.commit_proposal(proposal).unwrap();
        }
        assert_eq!(seen, expected);
        assert!(
            selected_sets.len() > 1,
            "scheduler did not rotate its selected domain set"
        );
        for (domain_id, _, first_nonce) in active {
            assert!(
                engine
                    .finalized_state()
                    .domain(domain_id)
                    .unwrap()
                    .state
                    .account(SENDER)
                    .unwrap()
                    .unwrap()
                    .nonce
                    > first_nonce,
                "active domain {domain_id:?} was starved"
            );
        }
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn chain_registry_enforces_governance_burn_and_owner_refund() {
        let dir = tempdir().unwrap();
        let governance_secret = [8_u8; 32];
        let governance = address_for_secret(governance_secret);
        let mut genesis = m6_genesis();
        genesis.governance_address = governance;
        genesis.allocations.insert(
            governance,
            GenesisAccount {
                balance: U256::from(10_u128.pow(18)),
                ..GenesisAccount::default()
            },
        );
        let root = genesis.root_domain_id;
        let database = Database::open(dir.path(), identity(), RetentionPolicy::Archive).unwrap();
        let mut engine = SingleValidatorEngine::open(
            EngineMode::DevValidator,
            database,
            genesis,
            MempoolConfig::default(),
        )
        .unwrap();

        let burn_create = create_chain_transaction(0, root, 2_049, "Burned");
        let burn_tx = arbor_codec::decode_eip1559(&burn_create).unwrap();
        let burned_id = derive_domain_id(
            identity().network_id,
            root,
            eip1559_transaction_hash(&burn_tx).unwrap(),
        );
        let refund_create = create_chain_transaction(1, root, 2_050, "Refunded");
        let refund_tx = arbor_codec::decode_eip1559(&refund_create).unwrap();
        let refunded_id = derive_domain_id(
            identity().network_id,
            root,
            eip1559_transaction_hash(&refund_tx).unwrap(),
        );
        engine.submit_raw(root, burn_create).unwrap();
        engine.submit_raw(root, refund_create).unwrap();
        engine.produce_block(1_001).unwrap();

        // The owner cannot exercise the governance-only burn selector.
        let unauthorized_burn =
            lifecycle_transaction(2, encode_burn_deposit_call(burned_id), [7_u8; 32]);
        let unauthorized_burn_hash =
            eip1559_transaction_hash(&arbor_codec::decode_eip1559(&unauthorized_burn).unwrap())
                .unwrap();
        engine.submit_raw(root, unauthorized_burn).unwrap();
        engine.produce_block(1_002).unwrap();
        let receipt = engine.receipt(unauthorized_burn_hash).unwrap().unwrap();
        assert!(
            !arbor_codec::decode_eip1559_receipt(&receipt)
                .unwrap()
                .status
        );

        engine
            .submit_raw(
                root,
                lifecycle_transaction(0, encode_burn_deposit_call(burned_id), governance_secret),
            )
            .unwrap();
        engine.produce_block(1_003).unwrap();
        let root_state = &engine.finalized_state().domain(root).unwrap().state;
        assert_eq!(
            root_state
                .storage(CHAIN_REGISTRY_ADDRESS, deposit_slot(burned_id))
                .unwrap(),
            U256::ZERO
        );
        assert_eq!(
            root_state
                .storage(CHAIN_REGISTRY_ADDRESS, deposit_status_slot(burned_id))
                .unwrap(),
            U256::from(CreationDepositStatus::Burned as u8)
        );
        assert_eq!(
            root_state.account(Address::ZERO).unwrap().unwrap().balance,
            MIN_CREATION_DEPOSIT
        );

        // Creation at height one unlocks at height 101. Empty consensus blocks still advance
        // the protocol clock; owner refund before that boundary is not accepted.
        let early_refund =
            lifecycle_transaction(3, encode_refund_deposit_call(refunded_id), [7_u8; 32]);
        let early_refund_hash =
            eip1559_transaction_hash(&arbor_codec::decode_eip1559(&early_refund).unwrap()).unwrap();
        engine.submit_raw(root, early_refund).unwrap();
        engine.produce_block(1_004).unwrap();
        let receipt = engine.receipt(early_refund_hash).unwrap().unwrap();
        assert!(
            !arbor_codec::decode_eip1559_receipt(&receipt)
                .unwrap()
                .status
        );
        for height in 5..=100 {
            engine.produce_block(1_000 + height).unwrap();
        }
        engine
            .submit_raw(
                root,
                lifecycle_transaction(4, encode_refund_deposit_call(refunded_id), [7_u8; 32]),
            )
            .unwrap();
        engine.produce_block(1_101).unwrap();
        let root_state = &engine.finalized_state().domain(root).unwrap().state;
        assert_eq!(
            root_state
                .storage(CHAIN_REGISTRY_ADDRESS, deposit_slot(refunded_id))
                .unwrap(),
            U256::ZERO
        );
        assert_eq!(
            root_state
                .storage(CHAIN_REGISTRY_ADDRESS, deposit_status_slot(refunded_id))
                .unwrap(),
            U256::from(CreationDepositStatus::Refunded as u8)
        );
    }

    #[test]
    fn local_history_selection_does_not_change_domain_validity_or_roots() {
        let all_dir = tempdir().unwrap();
        let root_only_dir = tempdir().unwrap();
        let root = m6_genesis().root_domain_id;
        let mut all = open_m6_with_history(all_dir.path(), DomainHistoryRetention::All);
        let mut root_only = open_m6_with_history(
            root_only_dir.path(),
            DomainHistoryRetention::selected([root]),
        );

        let creation = create_chain_transaction(0, root, 2_049, "History");
        let creation_tx = arbor_codec::decode_eip1559(&creation).unwrap();
        let creation_hash = eip1559_transaction_hash(&creation_tx).unwrap();
        let child_id = derive_domain_id(identity().network_id, root, creation_hash);
        for engine in [&mut all, &mut root_only] {
            engine.submit_raw(root, creation.clone()).unwrap();
            engine.produce_block(1_001).unwrap();
        }
        assert_eq!(all.finalized_state(), root_only.finalized_state());
        assert!(all.receipt(creation_hash).unwrap().is_some());
        assert!(root_only.receipt(creation_hash).unwrap().is_some());

        let child_transfer = domain_transfer(2_049, 0, 99);
        let child_transfer_hash =
            eip1559_transaction_hash(&arbor_codec::decode_eip1559(&child_transfer).unwrap())
                .unwrap();
        for engine in [&mut all, &mut root_only] {
            engine.submit_raw(child_id, child_transfer.clone()).unwrap();
            engine.produce_block(1_002).unwrap();
        }
        assert_eq!(all.finalized_state(), root_only.finalized_state());
        assert_eq!(
            all.finalized_state().domain_heads_root(),
            root_only.finalized_state().domain_heads_root()
        );
        assert!(all.receipt(child_transfer_hash).unwrap().is_some());
        assert!(root_only.receipt(child_transfer_hash).unwrap().is_none());
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
