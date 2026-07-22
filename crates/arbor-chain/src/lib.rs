//! Deterministic consensus/domain block construction and full execution validation.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};

use alloy_primitives::{Address, B256, Bloom, U256, keccak256};
use arbor_codec::{
    MAX_BATCHES_PER_BLOCK, MAX_CANONICAL_OBJECT_BYTES, decode_consensus_header,
    decode_domain_batch, decode_domain_header, encode_consensus_header, encode_domain_batch,
    encode_domain_header,
};
use arbor_crypto::{
    consensus_header_hash, derive_domain_id, domain_genesis_hash, domain_header_hash,
};
use arbor_evm::{DomainEnv, ExecutionState};
use arbor_executor::{
    ChainRegistryExecutionContext, CreatedDomain, DomainExecutionResult, ExecutorError,
    ExecutorService,
};
use arbor_primitives::{
    CANONICAL_CODEC_VERSION, ConsensusBlockHeader, ConsensusHeight, DomainBatch, DomainBlockHeader,
    DomainDescriptor, DomainGenesis, DomainId, DomainNumber, DomainStatus, NetworkId,
    PROTOCOL_VERSION,
};
use arbor_state::{DomainHead, DomainHeadProof, DomainHeadsCommitment};
use thiserror::Error;

/// Maximum execution gas consumed across all batches in one consensus block.
pub const MAX_CONSENSUS_BLOCK_GAS: u64 = 120_000_000;
/// Maximum gas limit of one domain block.
pub const MAX_DOMAIN_BLOCK_GAS: u64 = 30_000_000;
/// Maximum encoded consensus block body.
pub const MAX_CONSENSUS_BLOCK_BYTES: usize = 16 * 1024 * 1024;
/// Maximum timestamp increment relative to the finalized parent.
pub const MAX_TIMESTAMP_STEP_SECONDS: u64 = 30;

const BLOCK_BODY_TAG: &[u8] = b"ARBOR_CONSENSUS_BLOCK_BODY_V1";
const MERKLE_ROOT_TAG: &[u8] = b"ARBOR_MERKLE_ROOT_V1";
const MERKLE_BRANCH_TAG: &[u8] = b"ARBOR_MERKLE_BRANCH_V1";
const MERKLE_EMPTY_TAG: &[u8] = b"ARBOR_MERKLE_EMPTY_V1";
const BATCH_LEAF_TAG: &[u8] = b"ARBOR_BATCH_LEAF_V1";
const RESULT_LEAF_TAG: &[u8] = b"ARBOR_RESULT_LEAF_V1";
const PREVRANDAO_TAG: &[u8] = b"ARBOR_PREVRANDAO_V1";

/// Immutable execution parameters of one active domain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DomainConfig {
    /// Unique EVM chain ID.
    pub chain_id: u64,
    /// Versioned Arbor/EVM execution rules.
    pub protocol_revision: u32,
    /// Per-domain gas limit.
    pub gas_limit: u64,
    /// Base fee used by the genesis domain block.
    pub initial_base_fee_per_gas: u128,
}

impl DomainConfig {
    fn validate(self) -> Result<(), ChainError> {
        if self.chain_id == 0 {
            return Err(ChainError::InvalidDomainConfig("chain ID must be non-zero"));
        }
        if self.gas_limit == 0 || self.gas_limit > MAX_DOMAIN_BLOCK_GAS {
            return Err(ChainError::InvalidDomainConfig(
                "gas limit must be in 1..=30,000,000",
            ));
        }
        if self.initial_base_fee_per_gas > u128::from(u64::MAX) {
            return Err(ChainError::InvalidDomainConfig(
                "base fee exceeds the fixed EVM block field",
            ));
        }
        ExecutorService::new(self.protocol_revision)
            .map_err(|_| ChainError::InvalidDomainConfig("unknown protocol revision"))?;
        Ok(())
    }
}

/// Finalized executable head of one domain.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FinalizedDomain {
    /// Consensus parameters of the domain.
    pub config: DomainConfig,
    /// Finalized logical block header, including the genesis block at number zero.
    pub header: DomainBlockHeader,
    /// Hash of `header`.
    pub block_hash: B256,
    /// Authenticated state reachable from `header.state_root`.
    pub state: ExecutionState,
}

/// Genesis inputs for the finalized multi-domain application view.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FinalizedChainGenesis {
    /// Genesis-bound network identifier.
    pub network_id: NetworkId,
    /// Height-zero consensus hash.
    pub consensus_hash: B256,
    /// Height-zero consensus timestamp.
    pub timestamp: u64,
    /// Initial validator-set commitment.
    pub validator_set_hash: B256,
    /// Root EVM domain identifier.
    pub root_domain_id: DomainId,
    /// Genesis-bound root-governance executor.
    pub governance_address: Address,
    /// Root EVM execution parameters.
    pub config: DomainConfig,
    /// Root authenticated execution state.
    pub state: ExecutionState,
}

/// Finalized application view used as the only parent for proposal execution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FinalizedChainState {
    /// Genesis-bound network identifier.
    pub network_id: NetworkId,
    /// Latest finalized root-consensus height.
    pub height: ConsensusHeight,
    /// Latest finalized consensus-header hash, or the configured genesis hash at height zero.
    pub consensus_hash: B256,
    /// Latest finalized consensus timestamp.
    pub timestamp: u64,
    /// Current validator-set commitment.
    pub validator_set_hash: B256,
    /// Next validator-set commitment.
    pub next_validator_set_hash: B256,
    root_domain_id: DomainId,
    governance_address: Address,
    domains: BTreeMap<DomainId, FinalizedDomain>,
    descriptors: BTreeMap<DomainId, DomainDescriptor>,
}

impl FinalizedChainState {
    /// Creates height-zero state with one root EVM domain.
    ///
    /// # Errors
    ///
    /// Returns [`ChainError`] for invalid root-domain parameters or an inconsistent state root.
    pub fn genesis(genesis: FinalizedChainGenesis) -> Result<Self, ChainError> {
        let FinalizedChainGenesis {
            network_id,
            consensus_hash,
            timestamp,
            validator_set_hash,
            root_domain_id,
            governance_address,
            config,
            state,
        } = genesis;
        config.validate()?;
        let header = DomainBlockHeader {
            protocol_version: PROTOCOL_VERSION,
            domain_id: root_domain_id,
            number: DomainNumber(0),
            parent_hash: B256::ZERO,
            consensus_height: ConsensusHeight(0),
            transactions_root: empty_ordered_trie_root(),
            state_root: state.state_root(),
            receipts_root: empty_ordered_trie_root(),
            logs_bloom: Bloom::default(),
            gas_limit: config.gas_limit,
            gas_used: 0,
            base_fee_per_gas: config.initial_base_fee_per_gas,
        };
        let block_hash = domain_header_hash(&header).map_err(ChainError::hash)?;
        Ok(Self {
            network_id,
            height: ConsensusHeight(0),
            consensus_hash,
            timestamp,
            validator_set_hash,
            next_validator_set_hash: validator_set_hash,
            root_domain_id,
            governance_address,
            domains: BTreeMap::from([(
                root_domain_id,
                FinalizedDomain {
                    config,
                    header,
                    block_hash,
                    state,
                },
            )]),
            descriptors: BTreeMap::new(),
        })
    }

    /// Returns one finalized domain head.
    #[must_use]
    pub fn domain(&self, domain_id: DomainId) -> Option<&FinalizedDomain> {
        self.domains.get(&domain_id)
    }

    /// Iterates finalized domains in canonical `domain_id` order.
    pub fn domains(&self) -> impl Iterator<Item = (DomainId, &FinalizedDomain)> {
        self.domains.iter().map(|(id, domain)| (*id, domain))
    }

    /// Returns the immutable root-domain identifier.
    #[must_use]
    pub const fn root_domain_id(&self) -> DomainId {
        self.root_domain_id
    }

    /// Returns the genesis-bound root-governance executor.
    #[must_use]
    pub const fn governance_address(&self) -> Address {
        self.governance_address
    }

    /// Returns a runtime-created domain descriptor from the root registry projection.
    #[must_use]
    pub fn domain_descriptor(&self, domain_id: DomainId) -> Option<&DomainDescriptor> {
        self.descriptors.get(&domain_id)
    }

    /// Iterates runtime-created descriptors in canonical domain-ID order.
    pub fn domain_descriptors(&self) -> impl Iterator<Item = (DomainId, &DomainDescriptor)> {
        self.descriptors
            .iter()
            .map(|(id, descriptor)| (*id, descriptor))
    }

    /// Returns the root-to-target genealogy, including the root and target IDs.
    ///
    /// # Errors
    ///
    /// Returns [`ChainError`] for an unknown target or a corrupt/cyclic registry projection.
    pub fn genealogy(&self, domain_id: DomainId) -> Result<Vec<DomainId>, ChainError> {
        if !self.domains.contains_key(&domain_id) {
            return Err(ChainError::UnknownDomain(domain_id));
        }
        let mut reversed = vec![domain_id];
        let mut current = domain_id;
        let mut seen = BTreeSet::from([domain_id]);
        while current != self.root_domain_id {
            let descriptor = self
                .descriptors
                .get(&current)
                .ok_or(ChainError::RegistryInvariant("missing parent descriptor"))?;
            current = descriptor.parent_domain_id;
            if !seen.insert(current) {
                return Err(ChainError::RegistryInvariant("cyclic domain genealogy"));
            }
            reversed.push(current);
        }
        reversed.reverse();
        Ok(reversed)
    }

    /// Builds a checkpoint proof for one current domain head.
    #[must_use]
    pub fn domain_head_proof(&self, domain_id: DomainId) -> Option<DomainHeadProof> {
        self.domains
            .contains_key(&domain_id)
            .then(|| heads_commitment(&self.domains).proof(domain_id))
    }

    /// Adds another height-zero domain while assembling deterministic genesis.
    ///
    /// This does not implement runtime domain creation; M6 must derive new domains from the root
    /// `ChainRegistry` transition. It exists so genesis and multi-domain validation share the same
    /// head representation.
    ///
    /// # Errors
    ///
    /// Returns [`ChainError`] for duplicate IDs, invalid parameters, or encoding failure.
    pub fn insert_genesis_domain(
        &mut self,
        domain_id: DomainId,
        config: DomainConfig,
        state: ExecutionState,
    ) -> Result<(), ChainError> {
        config.validate()?;
        if self.domains.contains_key(&domain_id) {
            return Err(ChainError::DuplicateGenesisDomain(domain_id));
        }
        let header = DomainBlockHeader {
            protocol_version: PROTOCOL_VERSION,
            domain_id,
            number: DomainNumber(0),
            parent_hash: B256::ZERO,
            consensus_height: ConsensusHeight(0),
            transactions_root: empty_ordered_trie_root(),
            state_root: state.state_root(),
            receipts_root: empty_ordered_trie_root(),
            logs_bloom: Bloom::default(),
            gas_limit: config.gas_limit,
            gas_used: 0,
            base_fee_per_gas: config.initial_base_fee_per_gas,
        };
        let block_hash = domain_header_hash(&header).map_err(ChainError::hash)?;
        self.domains.insert(
            domain_id,
            FinalizedDomain {
                config,
                header,
                block_hash,
                state,
            },
        );
        Ok(())
    }

    /// Computes the sparse commitment to all finalized domain heads.
    #[must_use]
    pub fn domain_heads_root(&self) -> B256 {
        heads_commitment(&self.domains).root()
    }

    fn insert_created_domain(
        &mut self,
        created: &CreatedDomain,
        height: ConsensusHeight,
    ) -> Result<(), ChainError> {
        let descriptor = &created.descriptor;
        if self.domains.contains_key(&descriptor.domain_id)
            || self.descriptors.contains_key(&descriptor.domain_id)
        {
            return Err(ChainError::DuplicateCreatedDomain(descriptor.domain_id));
        }
        if !self.domains.contains_key(&descriptor.parent_domain_id) {
            return Err(ChainError::RegistryInvariant(
                "created domain parent is absent",
            ));
        }
        if descriptor.status != DomainStatus::Active
            || derive_domain_id(
                self.network_id,
                descriptor.parent_domain_id,
                descriptor.create_tx_hash,
            ) != descriptor.domain_id
        {
            return Err(ChainError::RegistryInvariant(
                "created domain identity/status mismatch",
            ));
        }
        if self
            .domains
            .values()
            .any(|domain| domain.config.chain_id == descriptor.evm_chain_id)
        {
            return Err(ChainError::RegistryInvariant("duplicate EVM chain ID"));
        }
        let config = DomainConfig {
            chain_id: descriptor.evm_chain_id,
            protocol_revision: descriptor.protocol_revision,
            gas_limit: descriptor.gas_limit,
            initial_base_fee_per_gas: descriptor.initial_base_fee,
        };
        config.validate()?;
        validate_domain_genesis(descriptor, &created.genesis_state)?;
        let header = DomainBlockHeader {
            protocol_version: PROTOCOL_VERSION,
            domain_id: descriptor.domain_id,
            number: DomainNumber(0),
            parent_hash: B256::ZERO,
            consensus_height: height,
            transactions_root: empty_ordered_trie_root(),
            state_root: created.genesis_state.state_root(),
            receipts_root: empty_ordered_trie_root(),
            logs_bloom: Bloom::default(),
            gas_limit: descriptor.gas_limit,
            gas_used: 0,
            base_fee_per_gas: descriptor.initial_base_fee,
        };
        let block_hash = domain_header_hash(&header).map_err(ChainError::hash)?;
        self.domains.insert(
            descriptor.domain_id,
            FinalizedDomain {
                config,
                header,
                block_hash,
                state: created.genesis_state.clone(),
            },
        );
        self.descriptors
            .insert(descriptor.domain_id, descriptor.clone());
        Ok(())
    }
}

fn validate_domain_genesis(
    descriptor: &DomainDescriptor,
    state: &ExecutionState,
) -> Result<(), ChainError> {
    let owner_balance = state
        .account(descriptor.owner)
        .map_err(ChainError::hash)?
        .map_or(U256::ZERO, |account| account.balance);
    if owner_balance != descriptor.initial_supply {
        return Err(ChainError::RegistryInvariant(
            "owner allocation differs from initial supply",
        ));
    }
    let genesis = DomainGenesis {
        domain_id: descriptor.domain_id,
        parent_domain_id: descriptor.parent_domain_id,
        joint_domain_block_hash: descriptor.joint_domain_block_hash,
        create_tx_hash: descriptor.create_tx_hash,
        name: descriptor.name.clone(),
        symbol: descriptor.symbol.clone(),
        evm_chain_id: descriptor.evm_chain_id,
        owner: descriptor.owner,
        protocol_revision: descriptor.protocol_revision,
        gas_limit: descriptor.gas_limit,
        initial_base_fee: descriptor.initial_base_fee,
        initial_supply: descriptor.initial_supply,
        initial_state_root: state.state_root(),
    };
    if domain_genesis_hash(&genesis).map_err(ChainError::hash)? != descriptor.origin_hash {
        return Err(ChainError::RegistryInvariant("domain origin hash mismatch"));
    }
    Ok(())
}

/// Body finalized under a root-consensus header.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConsensusBlock {
    /// Header hash preimage.
    pub header: ConsensusBlockHeader,
    /// Canonically sorted domain batches.
    pub batches: Vec<DomainBatch>,
    /// Domain headers in the exact same domain order as `batches`.
    pub domain_blocks: Vec<DomainBlockHeader>,
}

impl ConsensusBlock {
    /// Returns the canonical consensus-header hash.
    ///
    /// # Errors
    ///
    /// Returns [`ChainError`] if header encoding unexpectedly exceeds its budget.
    pub fn hash(&self) -> Result<B256, ChainError> {
        consensus_header_hash(&self.header).map_err(ChainError::hash)
    }

    /// Builds a domain-result inclusion proof for a batch executed in this block.
    ///
    /// # Errors
    ///
    /// Returns [`ChainError`] if the result header cannot be canonically encoded.
    pub fn domain_result_proof(
        &self,
        domain_id: DomainId,
    ) -> Result<Option<DomainResultProof>, ChainError> {
        let Some(index) = self
            .domain_blocks
            .iter()
            .position(|header| header.domain_id == domain_id)
        else {
            return Ok(None);
        };
        let values = self
            .domain_blocks
            .iter()
            .map(encode_domain_header)
            .collect::<Result<Vec<_>, _>>()
            .map_err(ChainError::hash)?;
        let count = u32::try_from(values.len())
            .map_err(|_| ChainError::Encoding("domain result count".to_owned()))?;
        let mut level = values
            .iter()
            .enumerate()
            .map(|(position, value)| collection_leaf(RESULT_LEAF_TAG, count, position, value))
            .collect::<Vec<_>>();
        let mut cursor = index;
        let mut depth = 0_u16;
        let mut siblings = Vec::new();
        while level.len() > 1 {
            let sibling = if cursor.is_multiple_of(2) {
                level
                    .get(cursor + 1)
                    .copied()
                    .unwrap_or_else(|| merkle_empty(RESULT_LEAF_TAG, depth))
            } else {
                level[cursor - 1]
            };
            siblings.push(sibling);
            level = level
                .chunks(2)
                .map(|pair| {
                    collection_branch(
                        RESULT_LEAF_TAG,
                        depth,
                        pair[0],
                        pair.get(1)
                            .copied()
                            .unwrap_or_else(|| merkle_empty(RESULT_LEAF_TAG, depth)),
                    )
                })
                .collect();
            cursor /= 2;
            depth = depth.saturating_add(1);
        }
        Ok(Some(DomainResultProof {
            root: self.header.domain_results_root,
            result: self.domain_blocks[index].clone(),
            index: u32::try_from(index)
                .map_err(|_| ChainError::Encoding("domain result index".to_owned()))?,
            count,
            siblings,
        }))
    }
}

/// Binary Merkle inclusion proof for one domain result in a finalized consensus block.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DomainResultProof {
    /// `domain_results_root` from the consensus header.
    pub root: B256,
    /// Included domain block header.
    pub result: DomainBlockHeader,
    /// Zero-based canonical result position.
    pub index: u32,
    /// Total committed result count.
    pub count: u32,
    /// Siblings from leaf level upward.
    pub siblings: Vec<B256>,
}

impl DomainResultProof {
    /// Verifies leaf position, tree shape, and root binding.
    ///
    /// # Errors
    ///
    /// Returns [`ChainError`] for malformed shape, encoding failure, or a root mismatch.
    pub fn verify(&self) -> Result<(), ChainError> {
        if self.count == 0 || self.index >= self.count {
            return Err(ChainError::InvalidResultProof("index/count"));
        }
        let mut expected_levels = 0_usize;
        let mut width = self.count as usize;
        while width > 1 {
            expected_levels += 1;
            width = width.div_ceil(2);
        }
        if self.siblings.len() != expected_levels {
            return Err(ChainError::InvalidResultProof("depth"));
        }
        let encoded = encode_domain_header(&self.result).map_err(ChainError::hash)?;
        let mut hash = collection_leaf(RESULT_LEAF_TAG, self.count, self.index as usize, &encoded);
        let mut cursor = self.index as usize;
        for (depth, sibling) in self.siblings.iter().enumerate() {
            let depth = u16::try_from(depth)
                .map_err(|_| ChainError::InvalidResultProof("depth overflow"))?;
            hash = if cursor.is_multiple_of(2) {
                collection_branch(RESULT_LEAF_TAG, depth, hash, *sibling)
            } else {
                collection_branch(RESULT_LEAF_TAG, depth, *sibling, hash)
            };
            cursor /= 2;
        }
        let actual = collection_root(RESULT_LEAF_TAG, self.count, hash);
        if actual != self.root {
            return Err(ChainError::InvalidResultProof("root mismatch"));
        }
        Ok(())
    }
}

/// A fully executed proposal overlay. It is not finalized until durably committed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatedProposal {
    block: ConsensusBlock,
    resulting_state: FinalizedChainState,
    executions: Vec<DomainExecutionResult>,
}

impl ValidatedProposal {
    /// Returns the candidate block.
    #[must_use]
    pub const fn block(&self) -> &ConsensusBlock {
        &self.block
    }

    /// Returns the post-execution overlay, which must not be exposed as finalized state.
    #[must_use]
    pub const fn resulting_state(&self) -> &FinalizedChainState {
        &self.resulting_state
    }

    /// Returns execution outputs aligned with domain blocks.
    #[must_use]
    pub fn executions(&self) -> &[DomainExecutionResult] {
        &self.executions
    }

    /// Consumes the overlay after durable application commit.
    #[must_use]
    pub fn into_parts(
        self,
    ) -> (
        ConsensusBlock,
        FinalizedChainState,
        Vec<DomainExecutionResult>,
    ) {
        (self.block, self.resulting_state, self.executions)
    }
}

/// Consensus commitment whose mismatch made a proposal invalid.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RootField {
    /// Header commitment to canonical batches.
    Batches,
    /// One domain header's transaction root.
    Transactions,
    /// One domain header's post-state root.
    State,
    /// One domain header's receipt root.
    Receipts,
    /// Header commitment to domain results.
    DomainResults,
    /// Header sparse commitment to all current domain heads.
    DomainHeads,
}

/// Invalid block, resource, execution, or body encoding error.
#[derive(Debug, Error)]
pub enum ChainError {
    /// Root domain or protocol configuration is invalid.
    #[error("invalid domain configuration: {0}")]
    InvalidDomainConfig(&'static str),
    /// Genesis assembly attempted to register the same domain twice.
    #[error("duplicate genesis domain {0:?}")]
    DuplicateGenesisDomain(DomainId),
    /// A proposal attempted to publish the same runtime domain twice.
    #[error("duplicate created domain {0:?}")]
    DuplicateCreatedDomain(DomainId),
    /// Root-registry projection disagrees with authenticated execution output.
    #[error("ChainRegistry invariant failed: {0}")]
    RegistryInvariant(&'static str),
    /// Proposal does not extend the current finalized consensus head.
    #[error("consensus parent/height does not extend the finalized head")]
    WrongConsensusParent,
    /// Timestamp must advance deterministically within the protocol window.
    #[error("timestamp {actual} must be in {minimum}..={maximum}")]
    InvalidTimestamp {
        /// Smallest valid timestamp.
        minimum: u64,
        /// Largest valid timestamp.
        maximum: u64,
        /// Proposed timestamp.
        actual: u64,
    },
    /// Batch collection is not strictly sorted or repeats a domain.
    #[error("domain batches must be strictly sorted by domain ID and unique")]
    NonCanonicalBatches,
    /// A batch references an inactive or unknown domain.
    #[error("unknown domain {0:?}")]
    UnknownDomain(DomainId),
    /// Batch parent differs from the finalized domain head.
    #[error("batch parent does not match finalized head for domain {0:?}")]
    WrongDomainParent(DomainId),
    /// Domain result count/order does not match batch count/order.
    #[error("domain result list does not align with the batch list")]
    ResultAlignment,
    /// A domain block number/consensus height/parent is inconsistent.
    #[error("domain header ancestry is invalid for domain {0:?}")]
    WrongDomainNumber(DomainId),
    /// A consensus-sensitive root does not match deterministic replay.
    #[error("{field:?} root mismatch for domain {domain_id:?}")]
    RootMismatch {
        /// Commitment category.
        field: RootField,
        /// Domain for a local root; `None` for consensus-wide commitments.
        domain_id: Option<DomainId>,
    },
    /// Header metadata not covered by another typed condition differs from replay.
    #[error("consensus or domain header fields differ from deterministic replay")]
    HeaderMismatch,
    /// Too many domain batches are included.
    #[error("consensus block has {actual} batches; limit is {limit}")]
    TooManyBatches {
        /// Protocol limit.
        limit: usize,
        /// Observed count.
        actual: usize,
    },
    /// Actual aggregate gas exceeded the root block limit.
    #[error("consensus block gas {actual} exceeds limit {limit}")]
    AggregateGas {
        /// Protocol limit.
        limit: u64,
        /// Observed gas.
        actual: u64,
    },
    /// Encoded body exceeded the consensus network/storage budget.
    #[error("encoded consensus block is {actual} bytes; limit is {limit}")]
    BlockBytes {
        /// Protocol limit.
        limit: usize,
        /// Observed bytes.
        actual: usize,
    },
    /// Domain execution rejected a block-invalid transaction.
    #[error("domain {domain_id:?} execution failed: {source}")]
    Execution {
        /// Domain being executed.
        domain_id: DomainId,
        /// Executor failure.
        #[source]
        source: ExecutorError,
    },
    /// Canonical encoding or hashing failed.
    #[error("canonical block encoding failed: {0}")]
    Encoding(String),
    /// Persisted/network block body is malformed.
    #[error("invalid consensus block body: {0}")]
    Decode(&'static str),
    /// A domain-result inclusion proof has an invalid shape or root.
    #[error("invalid domain result proof: {0}")]
    InvalidResultProof(&'static str),
}

impl ChainError {
    fn hash(error: impl std::fmt::Display) -> Self {
        Self::Encoding(error.to_string())
    }
}

/// Stateless deterministic block builder/validator over an explicit finalized parent.
#[derive(Clone, Copy, Debug, Default)]
pub struct ChainMachine;

impl ChainMachine {
    /// Builds a canonical proposal, sorting batches by domain ID before execution.
    ///
    /// # Errors
    ///
    /// Returns [`ChainError`] for duplicates, ancestry/resource violations, or block-invalid
    /// execution.
    pub fn build_proposal(
        &self,
        parent: &FinalizedChainState,
        mut batches: Vec<DomainBatch>,
        timestamp: u64,
        proposer: Address,
    ) -> Result<ValidatedProposal, ChainError> {
        batches.sort_by_key(|batch| batch.domain_id);
        ensure_batch_order(&batches)?;
        Self::execute(parent, batches, timestamp, proposer)
    }

    /// Fully validates a proposal by checking ancestry/order/limits and replaying every batch.
    ///
    /// # Errors
    ///
    /// Returns a typed [`ChainError`] for the first invalid field or execution failure.
    pub fn validate_proposal(
        &self,
        parent: &FinalizedChainState,
        candidate: &ConsensusBlock,
    ) -> Result<ValidatedProposal, ChainError> {
        if candidate.header.protocol_version != PROTOCOL_VERSION
            || candidate.header.network_id != parent.network_id
            || candidate.header.height.0 != parent.height.0.saturating_add(1)
            || candidate.header.parent_hash != parent.consensus_hash
        {
            return Err(ChainError::WrongConsensusParent);
        }
        ensure_batch_order(&candidate.batches)?;
        if candidate.domain_blocks.len() != candidate.batches.len()
            || candidate
                .batches
                .iter()
                .zip(&candidate.domain_blocks)
                .any(|(batch, result)| batch.domain_id != result.domain_id)
        {
            return Err(ChainError::ResultAlignment);
        }
        let expected = Self::execute(
            parent,
            candidate.batches.clone(),
            candidate.header.timestamp,
            candidate.header.proposer,
        )?;
        compare_candidate(candidate, expected.block())?;
        Ok(expected)
    }

    fn execute(
        parent: &FinalizedChainState,
        batches: Vec<DomainBatch>,
        timestamp: u64,
        proposer: Address,
    ) -> Result<ValidatedProposal, ChainError> {
        validate_timestamp(parent.timestamp, timestamp)?;
        if batches.len() > MAX_BATCHES_PER_BLOCK {
            return Err(ChainError::TooManyBatches {
                limit: MAX_BATCHES_PER_BLOCK,
                actual: batches.len(),
            });
        }
        let height = ConsensusHeight(
            parent
                .height
                .0
                .checked_add(1)
                .ok_or(ChainError::WrongConsensusParent)?,
        );
        let mut resulting_state = parent.clone();
        let mut domain_blocks = Vec::with_capacity(batches.len());
        let mut executions = Vec::with_capacity(batches.len());
        let mut aggregate_gas = 0_u64;
        let finalized_heads = parent
            .domains
            .iter()
            .map(|(domain_id, domain)| (*domain_id, domain.block_hash))
            .collect::<BTreeMap<_, _>>();

        for batch in &batches {
            let finalized = parent
                .domains
                .get(&batch.domain_id)
                .ok_or(ChainError::UnknownDomain(batch.domain_id))?;
            if batch.parent_domain_block_hash != finalized.block_hash {
                return Err(ChainError::WrongDomainParent(batch.domain_id));
            }
            let (header, execution, next_domain) = execute_domain(
                parent.consensus_hash,
                finalized,
                batch,
                height,
                timestamp,
                proposer,
                &ChainRegistryExecutionContext {
                    network_id: parent.network_id,
                    root_domain_id: parent.root_domain_id,
                    executing_domain_id: batch.domain_id,
                    creation_height: height.0,
                    governance_address: parent.governance_address,
                    finalized_heads: finalized_heads.clone(),
                },
            )?;
            aggregate_gas =
                aggregate_gas
                    .checked_add(execution.gas_used)
                    .ok_or(ChainError::AggregateGas {
                        limit: MAX_CONSENSUS_BLOCK_GAS,
                        actual: u64::MAX,
                    })?;
            validate_aggregate_gas(aggregate_gas)?;
            resulting_state.domains.insert(batch.domain_id, next_domain);
            for created in &execution.created_domains {
                resulting_state.insert_created_domain(created, height)?;
            }
            domain_blocks.push(header);
            executions.push(execution);
        }

        let batches_root = batches_root(&batches)?;
        let domain_results_root = domain_results_root(&domain_blocks)?;
        let domain_heads_root = resulting_state.domain_heads_root();
        let header = ConsensusBlockHeader {
            protocol_version: PROTOCOL_VERSION,
            network_id: parent.network_id,
            height,
            parent_hash: parent.consensus_hash,
            timestamp,
            batches_root,
            domain_results_root,
            domain_heads_root,
            validator_set_hash: parent.next_validator_set_hash,
            next_validator_set_hash: parent.next_validator_set_hash,
            proposer,
        };
        resulting_state.validator_set_hash = header.validator_set_hash;
        resulting_state.next_validator_set_hash = header.next_validator_set_hash;
        let block = ConsensusBlock {
            header,
            batches,
            domain_blocks,
        };
        let encoded_len = encode_consensus_block(&block)?.len();
        if encoded_len > MAX_CONSENSUS_BLOCK_BYTES {
            return Err(ChainError::BlockBytes {
                limit: MAX_CONSENSUS_BLOCK_BYTES,
                actual: encoded_len,
            });
        }
        resulting_state.height = height;
        resulting_state.consensus_hash = block.hash()?;
        resulting_state.timestamp = timestamp;
        Ok(ValidatedProposal {
            block,
            resulting_state,
            executions,
        })
    }
}

fn execute_domain(
    parent_consensus_hash: B256,
    finalized: &FinalizedDomain,
    batch: &DomainBatch,
    height: ConsensusHeight,
    timestamp: u64,
    proposer: Address,
    registry_context: &ChainRegistryExecutionContext,
) -> Result<(DomainBlockHeader, DomainExecutionResult, FinalizedDomain), ChainError> {
    let number = finalized
        .header
        .number
        .0
        .checked_add(1)
        .ok_or(ChainError::WrongDomainNumber(batch.domain_id))?;
    let base_fee = next_base_fee(&finalized.header);
    let env = DomainEnv {
        chain_id: finalized.config.chain_id,
        block_number: number,
        timestamp,
        beneficiary: proposer,
        gas_limit: finalized.config.gas_limit,
        base_fee_per_gas: base_fee,
        prevrandao: derived_prevrandao(parent_consensus_hash, height),
    };
    let service =
        ExecutorService::new(finalized.config.protocol_revision).map_err(ChainError::hash)?;
    let execution = service
        .execute_batch_with_registry(env, &finalized.state, &batch.transactions, registry_context)
        .map_err(|source| ChainError::Execution {
            domain_id: batch.domain_id,
            source,
        })?;
    let header = DomainBlockHeader {
        protocol_version: PROTOCOL_VERSION,
        domain_id: batch.domain_id,
        number: DomainNumber(number),
        parent_hash: finalized.block_hash,
        consensus_height: height,
        transactions_root: execution.transactions_root,
        state_root: execution.state.state_root(),
        receipts_root: execution.receipts_root,
        logs_bloom: execution.logs_bloom,
        gas_limit: finalized.config.gas_limit,
        gas_used: execution.gas_used,
        base_fee_per_gas: base_fee,
    };
    let block_hash = domain_header_hash(&header).map_err(ChainError::hash)?;
    let next_domain = FinalizedDomain {
        config: finalized.config,
        header: header.clone(),
        block_hash,
        state: execution.state.clone(),
    };
    Ok((header, execution, next_domain))
}

fn ensure_batch_order(batches: &[DomainBatch]) -> Result<(), ChainError> {
    if batches.len() < 2
        || batches
            .windows(2)
            .all(|pair| pair[0].domain_id < pair[1].domain_id)
    {
        Ok(())
    } else {
        Err(ChainError::NonCanonicalBatches)
    }
}

fn validate_timestamp(parent: u64, actual: u64) -> Result<(), ChainError> {
    let minimum = parent.checked_add(1).ok_or(ChainError::InvalidTimestamp {
        minimum: u64::MAX,
        maximum: u64::MAX,
        actual,
    })?;
    let maximum = parent.saturating_add(MAX_TIMESTAMP_STEP_SECONDS);
    if actual < minimum || actual > maximum {
        Err(ChainError::InvalidTimestamp {
            minimum,
            maximum,
            actual,
        })
    } else {
        Ok(())
    }
}

fn validate_aggregate_gas(actual: u64) -> Result<(), ChainError> {
    if actual > MAX_CONSENSUS_BLOCK_GAS {
        Err(ChainError::AggregateGas {
            limit: MAX_CONSENSUS_BLOCK_GAS,
            actual,
        })
    } else {
        Ok(())
    }
}

fn compare_candidate(actual: &ConsensusBlock, expected: &ConsensusBlock) -> Result<(), ChainError> {
    if actual.header.batches_root != expected.header.batches_root {
        return Err(ChainError::RootMismatch {
            field: RootField::Batches,
            domain_id: None,
        });
    }
    for (actual, expected) in actual.domain_blocks.iter().zip(&expected.domain_blocks) {
        if actual.number != expected.number
            || actual.parent_hash != expected.parent_hash
            || actual.consensus_height != expected.consensus_height
        {
            return Err(ChainError::WrongDomainNumber(actual.domain_id));
        }
        for (field, differs) in [
            (
                RootField::Transactions,
                actual.transactions_root != expected.transactions_root,
            ),
            (RootField::State, actual.state_root != expected.state_root),
            (
                RootField::Receipts,
                actual.receipts_root != expected.receipts_root,
            ),
        ] {
            if differs {
                return Err(ChainError::RootMismatch {
                    field,
                    domain_id: Some(actual.domain_id),
                });
            }
        }
        if actual != expected {
            return Err(ChainError::HeaderMismatch);
        }
    }
    if actual.header.domain_results_root != expected.header.domain_results_root {
        return Err(ChainError::RootMismatch {
            field: RootField::DomainResults,
            domain_id: None,
        });
    }
    if actual.header.domain_heads_root != expected.header.domain_heads_root {
        return Err(ChainError::RootMismatch {
            field: RootField::DomainHeads,
            domain_id: None,
        });
    }
    if actual.header != expected.header {
        return Err(ChainError::HeaderMismatch);
    }
    Ok(())
}

fn heads_commitment(domains: &BTreeMap<DomainId, FinalizedDomain>) -> DomainHeadsCommitment {
    let mut heads = DomainHeadsCommitment::new();
    for (domain_id, domain) in domains {
        heads.insert(
            *domain_id,
            DomainHead {
                domain_block_hash: domain.block_hash,
                state_root: domain.header.state_root,
            },
        );
    }
    heads
}

/// Computes the v1 batch collection root from canonical batch bytes.
///
/// # Errors
///
/// Returns [`ChainError`] if a batch violates canonical codec limits.
pub fn batches_root(batches: &[DomainBatch]) -> Result<B256, ChainError> {
    let values = batches
        .iter()
        .map(encode_domain_batch)
        .collect::<Result<Vec<_>, _>>()
        .map_err(ChainError::hash)?;
    Ok(merkle_collection_root(BATCH_LEAF_TAG, &values))
}

/// Computes the v1 result collection root from canonical domain-header bytes.
///
/// # Errors
///
/// Returns [`ChainError`] if a header violates canonical codec limits.
pub fn domain_results_root(results: &[DomainBlockHeader]) -> Result<B256, ChainError> {
    let values = results
        .iter()
        .map(encode_domain_header)
        .collect::<Result<Vec<_>, _>>()
        .map_err(ChainError::hash)?;
    Ok(merkle_collection_root(RESULT_LEAF_TAG, &values))
}

fn merkle_collection_root(leaf_tag: &[u8], values: &[Vec<u8>]) -> B256 {
    let count = u32::try_from(values.len()).expect("protocol collection count fits u32");
    let mut level: Vec<B256> = values
        .iter()
        .enumerate()
        .map(|(index, value)| collection_leaf(leaf_tag, count, index, value))
        .collect();
    if level.is_empty() {
        level.push(merkle_empty(leaf_tag, 0));
    }
    let mut depth = 0_u16;
    while level.len() > 1 {
        let mut next = Vec::with_capacity(level.len().div_ceil(2));
        for pair in level.chunks(2) {
            let right = pair
                .get(1)
                .copied()
                .unwrap_or_else(|| merkle_empty(leaf_tag, depth));
            next.push(collection_branch(leaf_tag, depth, pair[0], right));
        }
        level = next;
        depth = depth.saturating_add(1);
    }
    collection_root(leaf_tag, count, level[0])
}

fn collection_leaf(leaf_tag: &[u8], count: u32, index: usize, value: &[u8]) -> B256 {
    let mut bytes = Vec::with_capacity(leaf_tag.len() + value.len() + 13);
    bytes.extend_from_slice(leaf_tag);
    bytes.push(CANONICAL_CODEC_VERSION);
    bytes.extend_from_slice(&count.to_be_bytes());
    bytes.extend_from_slice(
        &u32::try_from(index)
            .expect("protocol collection index fits u32")
            .to_be_bytes(),
    );
    bytes.extend_from_slice(
        &u32::try_from(value.len())
            .expect("canonical object size fits u32")
            .to_be_bytes(),
    );
    bytes.extend_from_slice(value);
    keccak256(bytes)
}

fn collection_branch(leaf_tag: &[u8], depth: u16, left: B256, right: B256) -> B256 {
    let mut bytes = Vec::with_capacity(MERKLE_BRANCH_TAG.len() + leaf_tag.len() + 68);
    bytes.extend_from_slice(MERKLE_BRANCH_TAG);
    bytes.push(CANONICAL_CODEC_VERSION);
    bytes.extend_from_slice(leaf_tag);
    bytes.extend_from_slice(&depth.to_be_bytes());
    bytes.extend_from_slice(left.as_slice());
    bytes.extend_from_slice(right.as_slice());
    keccak256(bytes)
}

fn collection_root(leaf_tag: &[u8], count: u32, tree_hash: B256) -> B256 {
    let mut root = Vec::with_capacity(MERKLE_ROOT_TAG.len() + leaf_tag.len() + 38);
    root.extend_from_slice(MERKLE_ROOT_TAG);
    root.push(CANONICAL_CODEC_VERSION);
    root.extend_from_slice(leaf_tag);
    root.extend_from_slice(&count.to_be_bytes());
    root.extend_from_slice(tree_hash.as_slice());
    keccak256(root)
}

fn merkle_empty(leaf_tag: &[u8], depth: u16) -> B256 {
    let mut bytes = Vec::with_capacity(MERKLE_EMPTY_TAG.len() + leaf_tag.len() + 3);
    bytes.extend_from_slice(MERKLE_EMPTY_TAG);
    bytes.push(CANONICAL_CODEC_VERSION);
    bytes.extend_from_slice(leaf_tag);
    bytes.extend_from_slice(&depth.to_be_bytes());
    keccak256(bytes)
}

fn derived_prevrandao(parent_hash: B256, height: ConsensusHeight) -> B256 {
    let mut bytes = Vec::with_capacity(PREVRANDAO_TAG.len() + 41);
    bytes.extend_from_slice(PREVRANDAO_TAG);
    bytes.push(CANONICAL_CODEC_VERSION);
    bytes.extend_from_slice(parent_hash.as_slice());
    bytes.extend_from_slice(&height.0.to_be_bytes());
    keccak256(bytes)
}

/// Computes the EIP-1559 base fee for a domain's next non-vacant block.
#[must_use]
pub fn next_base_fee(parent: &DomainBlockHeader) -> u128 {
    let target = parent.gas_limit / 2;
    if target == 0 || parent.gas_used == target {
        return parent.base_fee_per_gas;
    }
    let target = u128::from(target);
    let parent_fee = parent.base_fee_per_gas;
    if parent.gas_used > parent.gas_limit / 2 {
        let gas_delta = u128::from(parent.gas_used - parent.gas_limit / 2);
        let increase = parent_fee
            .saturating_mul(gas_delta)
            .checked_div(target)
            .unwrap_or_default()
            .checked_div(8)
            .unwrap_or_default()
            .max(1);
        parent_fee.saturating_add(increase)
    } else {
        let gas_delta = u128::from(parent.gas_limit / 2 - parent.gas_used);
        let decrease = parent_fee
            .saturating_mul(gas_delta)
            .checked_div(target)
            .unwrap_or_default()
            .checked_div(8)
            .unwrap_or_default();
        parent_fee.saturating_sub(decrease)
    }
}

fn empty_ordered_trie_root() -> B256 {
    arbor_executor::transactions_root(&[])
}

/// Encodes a complete finalized block body for durable storage and future networking.
///
/// The header remains the only block-hash preimage; this body codec binds exact ordered batches
/// and results for replay.
///
/// # Errors
///
/// Returns [`ChainError`] for codec or size-limit violations.
pub fn encode_consensus_block(block: &ConsensusBlock) -> Result<Vec<u8>, ChainError> {
    if block.batches.len() != block.domain_blocks.len() {
        return Err(ChainError::ResultAlignment);
    }
    let header = encode_consensus_header(&block.header).map_err(ChainError::hash)?;
    let mut out = Vec::with_capacity(header.len() + 128);
    out.extend_from_slice(BLOCK_BODY_TAG);
    out.push(CANONICAL_CODEC_VERSION);
    put_bytes(&mut out, &header)?;
    put_u32(&mut out, block.batches.len())?;
    for batch in &block.batches {
        put_bytes(
            &mut out,
            &encode_domain_batch(batch).map_err(ChainError::hash)?,
        )?;
    }
    put_u32(&mut out, block.domain_blocks.len())?;
    for header in &block.domain_blocks {
        put_bytes(
            &mut out,
            &encode_domain_header(header).map_err(ChainError::hash)?,
        )?;
    }
    if out.len() > MAX_CONSENSUS_BLOCK_BYTES {
        return Err(ChainError::BlockBytes {
            limit: MAX_CONSENSUS_BLOCK_BYTES,
            actual: out.len(),
        });
    }
    Ok(out)
}

/// Decodes one exact bounded consensus block body.
///
/// # Errors
///
/// Returns [`ChainError`] for malformed, oversized, or trailing input.
pub fn decode_consensus_block(input: &[u8]) -> Result<ConsensusBlock, ChainError> {
    if input.len() > MAX_CONSENSUS_BLOCK_BYTES {
        return Err(ChainError::BlockBytes {
            limit: MAX_CONSENSUS_BLOCK_BYTES,
            actual: input.len(),
        });
    }
    let mut cursor = 0_usize;
    if take(input, &mut cursor, BLOCK_BODY_TAG.len())? != BLOCK_BODY_TAG
        || take(input, &mut cursor, 1)? != [CANONICAL_CODEC_VERSION]
    {
        return Err(ChainError::Decode("block body tag/version"));
    }
    let header = decode_consensus_header(take_bytes(input, &mut cursor)?)
        .map_err(|_| ChainError::Decode("consensus header"))?;
    let batch_count = take_u32(input, &mut cursor)?;
    if batch_count > MAX_BATCHES_PER_BLOCK {
        return Err(ChainError::TooManyBatches {
            limit: MAX_BATCHES_PER_BLOCK,
            actual: batch_count,
        });
    }
    let mut batches = Vec::with_capacity(batch_count);
    for _ in 0..batch_count {
        batches.push(
            decode_domain_batch(take_bytes(input, &mut cursor)?)
                .map_err(|_| ChainError::Decode("domain batch"))?,
        );
    }
    let result_count = take_u32(input, &mut cursor)?;
    if result_count != batch_count {
        return Err(ChainError::ResultAlignment);
    }
    let mut domain_blocks = Vec::with_capacity(result_count);
    for _ in 0..result_count {
        domain_blocks.push(
            decode_domain_header(take_bytes(input, &mut cursor)?)
                .map_err(|_| ChainError::Decode("domain header"))?,
        );
    }
    if cursor != input.len() {
        return Err(ChainError::Decode("trailing bytes"));
    }
    Ok(ConsensusBlock {
        header,
        batches,
        domain_blocks,
    })
}

fn put_u32(out: &mut Vec<u8>, value: usize) -> Result<(), ChainError> {
    let value = u32::try_from(value).map_err(|_| ChainError::Encoding("u32 length".to_owned()))?;
    out.extend_from_slice(&value.to_be_bytes());
    Ok(())
}

fn put_bytes(out: &mut Vec<u8>, value: &[u8]) -> Result<(), ChainError> {
    put_u32(out, value.len())?;
    out.extend_from_slice(value);
    Ok(())
}

fn take<'a>(input: &'a [u8], cursor: &mut usize, length: usize) -> Result<&'a [u8], ChainError> {
    let end = cursor
        .checked_add(length)
        .ok_or(ChainError::Decode("length overflow"))?;
    let value = input
        .get(*cursor..end)
        .ok_or(ChainError::Decode("unexpected end"))?;
    *cursor = end;
    Ok(value)
}

fn take_u32(input: &[u8], cursor: &mut usize) -> Result<usize, ChainError> {
    let bytes: [u8; 4] = take(input, cursor, 4)?
        .try_into()
        .map_err(|_| ChainError::Decode("u32"))?;
    Ok(u32::from_be_bytes(bytes) as usize)
}

fn take_bytes<'a>(input: &'a [u8], cursor: &mut usize) -> Result<&'a [u8], ChainError> {
    let length = take_u32(input, cursor)?;
    if length > MAX_CANONICAL_OBJECT_BYTES {
        return Err(ChainError::Decode("nested object too large"));
    }
    take(input, cursor, length)
}

/// Returns all transaction hashes from execution output in canonical block order.
#[must_use]
pub fn executed_transaction_hashes(proposal: &ValidatedProposal) -> BTreeSet<B256> {
    proposal
        .executions
        .iter()
        .flat_map(|execution| {
            execution
                .transactions
                .iter()
                .map(|transaction| transaction.transaction_hash)
        })
        .collect()
}

/// Utility for tests and dev genesis allocations.
#[must_use]
pub fn balance(state: &FinalizedChainState, domain_id: DomainId, address: Address) -> Option<U256> {
    state
        .domain(domain_id)
        .and_then(|domain| domain.state.account(address).ok().flatten())
        .map(|account| account.balance)
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{Bytes, U256, address};
    use arbor_codec::{encode_eip1559, encode_eip1559_signing_payload};
    use arbor_evm::GenesisAccount;
    use arbor_primitives::Eip1559Transaction;
    use k256::ecdsa::SigningKey;

    use super::*;

    const ROOT: DomainId = DomainId(B256::repeat_byte(0x44));
    const SENDER: Address = address!("4a62316623ad457f02cdc5d997ded67a383ec569");

    fn state() -> ExecutionState {
        ExecutionState::from_genesis(&BTreeMap::from([(
            SENDER,
            GenesisAccount {
                balance: U256::from(10_u128.pow(18)),
                ..GenesisAccount::default()
            },
        )]))
        .unwrap()
    }

    fn config() -> DomainConfig {
        DomainConfig {
            chain_id: 2_048,
            protocol_revision: 1,
            gas_limit: 30_000_000,
            initial_base_fee_per_gas: 10,
        }
    }

    fn genesis() -> FinalizedChainState {
        FinalizedChainState::genesis(FinalizedChainGenesis {
            network_id: NetworkId(B256::repeat_byte(0x11)),
            consensus_hash: B256::repeat_byte(0x22),
            timestamp: 1_000,
            validator_set_hash: B256::repeat_byte(0x33),
            root_domain_id: ROOT,
            governance_address: address!("0000000000000000000000000000000000000fee"),
            config: config(),
            state: state(),
        })
        .unwrap()
    }

    fn signed_transfer() -> Bytes {
        let mut transaction = Eip1559Transaction {
            chain_id: 2_048,
            nonce: 0,
            max_priority_fee_per_gas: 2,
            max_fee_per_gas: 20,
            gas_limit: 21_000,
            to: Some(address!("00000000000000000000000000000000000000aa")),
            value: U256::from(1),
            input: Bytes::new(),
            access_list: Vec::new(),
            y_parity: false,
            r: U256::ZERO,
            s: U256::ZERO,
        };
        let digest = keccak256(encode_eip1559_signing_payload(&transaction).unwrap());
        let key = SigningKey::from_bytes((&[7_u8; 32]).into()).unwrap();
        let (signature, recovery_id) = key.sign_prehash_recoverable(digest.as_slice()).unwrap();
        let bytes = signature.to_bytes();
        transaction.r = U256::from_be_slice(&bytes[..32]);
        transaction.s = U256::from_be_slice(&bytes[32..]);
        transaction.y_parity = recovery_id.is_y_odd();
        encode_eip1559(&transaction).unwrap().into()
    }

    fn proposal() -> (FinalizedChainState, ValidatedProposal) {
        let parent = genesis();
        let proposal = ChainMachine
            .build_proposal(
                &parent,
                vec![DomainBatch {
                    domain_id: ROOT,
                    parent_domain_block_hash: parent.domain(ROOT).unwrap().block_hash,
                    transactions: vec![signed_transfer()],
                }],
                1_001,
                address!("0000000000000000000000000000000000000fee"),
            )
            .unwrap();
        (parent, proposal)
    }

    #[test]
    fn block_body_roundtrips_and_replays() {
        let (parent, proposal) = proposal();
        let proof = proposal.block().domain_result_proof(ROOT).unwrap().unwrap();
        proof.verify().unwrap();
        let mut tampered = proof;
        tampered.result.state_root = B256::repeat_byte(0xee);
        assert!(tampered.verify().is_err());
        let encoded = encode_consensus_block(proposal.block()).unwrap();
        let decoded = decode_consensus_block(&encoded).unwrap();
        assert_eq!(&decoded, proposal.block());
        let replayed = ChainMachine.validate_proposal(&parent, &decoded).unwrap();
        assert_eq!(replayed.resulting_state(), proposal.resulting_state());
    }

    #[test]
    fn every_committed_root_is_checked_independently() {
        let (parent, proposal) = proposal();
        for (field, mutate) in [
            (RootField::Batches, 0_u8),
            (RootField::Transactions, 1),
            (RootField::State, 2),
            (RootField::Receipts, 3),
            (RootField::DomainResults, 4),
            (RootField::DomainHeads, 5),
        ] {
            let mut block = proposal.block().clone();
            match mutate {
                0 => block.header.batches_root = B256::repeat_byte(0xa0),
                1 => block.domain_blocks[0].transactions_root = B256::repeat_byte(0xa1),
                2 => block.domain_blocks[0].state_root = B256::repeat_byte(0xa2),
                3 => block.domain_blocks[0].receipts_root = B256::repeat_byte(0xa3),
                4 => block.header.domain_results_root = B256::repeat_byte(0xa4),
                5 => block.header.domain_heads_root = B256::repeat_byte(0xa5),
                _ => unreachable!(),
            }
            assert!(matches!(
                ChainMachine.validate_proposal(&parent, &block),
                Err(ChainError::RootMismatch { field: actual, .. }) if actual == field
            ));
        }
    }

    #[test]
    fn duplicate_wrong_parent_number_and_resource_limits_are_rejected() {
        let parent = genesis();
        let batch = DomainBatch {
            domain_id: ROOT,
            parent_domain_block_hash: parent.domain(ROOT).unwrap().block_hash,
            transactions: vec![signed_transfer()],
        };
        assert!(matches!(
            ChainMachine.build_proposal(
                &parent,
                vec![batch.clone(), batch.clone()],
                1_001,
                Address::ZERO
            ),
            Err(ChainError::NonCanonicalBatches)
        ));
        let mut wrong_parent = batch;
        wrong_parent.parent_domain_block_hash = B256::ZERO;
        assert!(matches!(
            ChainMachine.build_proposal(&parent, vec![wrong_parent], 1_001, Address::ZERO),
            Err(ChainError::WrongDomainParent(ROOT))
        ));

        let (_, proposal) = proposal();
        let mut wrong_number = proposal.block().clone();
        wrong_number.domain_blocks[0].number.0 += 1;
        assert!(matches!(
            ChainMachine.validate_proposal(&parent, &wrong_number),
            Err(ChainError::WrongDomainNumber(ROOT))
        ));
        assert!(matches!(
            validate_aggregate_gas(MAX_CONSENSUS_BLOCK_GAS + 1),
            Err(ChainError::AggregateGas { .. })
        ));
        assert!(matches!(
            validate_timestamp(parent.timestamp, parent.timestamp + 31),
            Err(ChainError::InvalidTimestamp { .. })
        ));
    }

    #[test]
    fn builder_sorts_distinct_domain_batches() {
        let mut parent = genesis();
        let second = DomainId(B256::repeat_byte(0x55));
        parent
            .insert_genesis_domain(
                second,
                DomainConfig {
                    chain_id: 2_049,
                    ..config()
                },
                state(),
            )
            .unwrap();
        let proposal = ChainMachine
            .build_proposal(
                &parent,
                vec![
                    DomainBatch {
                        domain_id: second,
                        parent_domain_block_hash: parent.domain(second).unwrap().block_hash,
                        transactions: Vec::new(),
                    },
                    DomainBatch {
                        domain_id: ROOT,
                        parent_domain_block_hash: parent.domain(ROOT).unwrap().block_hash,
                        transactions: Vec::new(),
                    },
                ],
                1_001,
                Address::ZERO,
            )
            .unwrap();
        assert_eq!(proposal.block().batches[0].domain_id, ROOT);
        assert_eq!(proposal.block().batches[1].domain_id, second);
    }
}
