use alloy_primitives::{Address, B256};
use arbor_chain::{
    DomainConfig, FinalizedChainCheckpoint, FinalizedChainState, FinalizedDomainCheckpoint,
};
use arbor_codec::{
    decode_domain_descriptor, decode_domain_header, decode_validator_set, encode_domain_descriptor,
    encode_domain_header, encode_validator_set,
};
use arbor_evm::{EvmError, ExecutionState};
use arbor_primitives::{ConsensusHeight, DomainId, NetworkId, ValidatorSet};
use arbor_storage::{CommitBatch, DomainStateCommit, FinalizedMarker, IndexedValue};

use super::{
    CommitEvent, ConsensusError, KEY_DEV_CHECKPOINT, KEY_DEV_WAL, SingleValidatorEngine,
    encode_dev_wal,
};

const CHECKPOINT_TAG: &[u8] = b"ARBOR_DEV_CHECKPOINT_V1";
const CHECKPOINT_VERSION: u8 = 1;
const MAX_CHECKPOINT_DOMAINS: usize = 65_536;
const MAX_CHECKPOINT_METADATA_BYTES: usize = 16 * 1024 * 1024;

/// Development-only finalized checkpoint accepted by the M7 state-sync boundary.
///
/// The empty proof rule is explicit and exists only because [`SingleValidatorEngine`] emits no
/// QC. Production state sync must replace this type at the M8 adapter boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DevelopmentCheckpoint {
    /// Fully reconstructed and authenticated application state.
    pub state: FinalizedChainState,
    /// Validator set bound by `state.validator_set_hash`.
    pub validator_set: ValidatorSet,
    /// Empty in development mode; reserved for production finality proof bytes.
    pub finality_proof: Vec<u8>,
}

impl SingleValidatorEngine {
    /// Produces a complete development checkpoint for M7 snapshot serving.
    #[must_use]
    pub fn development_checkpoint(&self) -> DevelopmentCheckpoint {
        DevelopmentCheckpoint {
            state: self.finalized.clone(),
            validator_set: self.genesis.validator_set.clone(),
            finality_proof: Vec::new(),
        }
    }

    /// Atomically imports a fully verified development checkpoint into a height-zero node.
    ///
    /// The caller must reconstruct authenticated trie/code state and the chain checkpoint before
    /// this method is invoked. This boundary rechecks genesis identity, the validator commitment,
    /// the explicit development finality rule, and writes all domain state, registry projection,
    /// checkpoint metadata, WAL, and finalized marker in one synchronous parity-db transaction.
    ///
    /// # Errors
    ///
    /// Returns without changing durable or in-memory finalized state on any validation failure.
    pub fn import_development_checkpoint(
        &mut self,
        checkpoint: DevelopmentCheckpoint,
    ) -> Result<(), ConsensusError> {
        if self.pending.is_some() {
            return Err(ConsensusError::ProposalPending);
        }
        if self.finalized.height.0 != 0 {
            return Err(ConsensusError::InconsistentStore(
                "checkpoint import requires height-zero state",
            ));
        }
        if !checkpoint.finality_proof.is_empty() {
            return Err(ConsensusError::InconsistentStore(
                "development checkpoint proof must be empty",
            ));
        }
        let validator_hash = arbor_crypto::validator_set_hash(&checkpoint.validator_set)
            .map_err(|error| ConsensusError::InvalidGenesisOwned(error.to_string()))?;
        if checkpoint.validator_set != self.genesis.validator_set
            || validator_hash != checkpoint.state.validator_set_hash
            || checkpoint.state.next_validator_set_hash != validator_hash
            || checkpoint.state.network_id != self.genesis.network_id
            || checkpoint.state.root_domain_id() != self.genesis.root_domain_id
            || checkpoint.state.governance_address() != self.genesis.governance_address
        {
            return Err(ConsensusError::InconsistentStore(
                "checkpoint genesis or validator-set mismatch",
            ));
        }
        let checkpoint = DevelopmentCheckpoint {
            state: FinalizedChainState::from_checkpoint(checkpoint.state.checkpoint())?,
            validator_set: checkpoint.validator_set,
            finality_proof: checkpoint.finality_proof,
        };
        let marker = FinalizedMarker {
            height: checkpoint.state.height.0,
            consensus_hash: checkpoint.state.consensus_hash,
            domain_heads_root: checkpoint.state.domain_heads_root(),
        };
        let mut commit = CommitBatch::new(marker);
        let mut next_mempool = self.mempool.clone();
        for (domain_id, domain) in checkpoint.state.domains() {
            commit.states.push(DomainStateCommit {
                domain_id,
                consensus_height: domain.header.consensus_height.0,
                snapshot: domain.state.snapshot().clone(),
            });
            commit
                .contract_code
                .extend(domain.state.contract_code().clone());
            next_mempool.register_domain(domain_id, domain.config.chain_id)?;
        }
        for (domain_id, descriptor) in checkpoint.state.domain_descriptors() {
            commit.domain_registry.push(IndexedValue {
                key: domain_id.0.to_vec(),
                value: encode_domain_descriptor(descriptor)
                    .map_err(|error| ConsensusError::InvalidGenesisOwned(error.to_string()))?,
            });
        }
        commit.indexes.extend([
            IndexedValue {
                key: KEY_DEV_CHECKPOINT.to_vec(),
                value: encode_checkpoint(&checkpoint)?,
            },
            IndexedValue {
                key: KEY_DEV_WAL.to_vec(),
                value: encode_dev_wal(marker),
            },
        ]);
        self.database.commit(commit)?;
        self.finalized = checkpoint.state;
        self.mempool = next_mempool;
        self.scheduler_cursor = 0;
        let _ = self.events.send(CommitEvent {
            height: marker.height,
            consensus_hash: marker.consensus_hash,
            domain_heads_root: marker.domain_heads_root,
        });
        Ok(())
    }
}

pub(super) fn checkpoint_height(
    database: &arbor_storage::Database,
) -> Result<Option<u64>, ConsensusError> {
    database
        .index(KEY_DEV_CHECKPOINT)?
        .map(|bytes| {
            let checkpoint = decode_checkpoint(database, &bytes)?;
            Ok(checkpoint.state.height.0)
        })
        .transpose()
}

pub(super) fn load_development_checkpoint(
    database: &arbor_storage::Database,
    genesis: &super::DevGenesis,
) -> Result<Option<DevelopmentCheckpoint>, ConsensusError> {
    let Some(bytes) = database.index(KEY_DEV_CHECKPOINT)? else {
        return Ok(None);
    };
    let checkpoint = decode_checkpoint(database, &bytes)?;
    let validator_hash = arbor_crypto::validator_set_hash(&checkpoint.validator_set)
        .map_err(|error| ConsensusError::InvalidGenesisOwned(error.to_string()))?;
    if checkpoint.validator_set != genesis.validator_set
        || checkpoint.state.network_id != genesis.network_id
        || checkpoint.state.root_domain_id() != genesis.root_domain_id
        || checkpoint.state.governance_address() != genesis.governance_address
        || checkpoint.state.validator_set_hash != validator_hash
        || checkpoint.state.next_validator_set_hash != validator_hash
        || !checkpoint.finality_proof.is_empty()
    {
        return Err(ConsensusError::InconsistentStore(
            "persisted checkpoint identity mismatch",
        ));
    }
    Ok(Some(checkpoint))
}

fn encode_checkpoint(checkpoint: &DevelopmentCheckpoint) -> Result<Vec<u8>, ConsensusError> {
    let state = &checkpoint.state;
    let domains = state.domains().collect::<Vec<_>>();
    let count = u32::try_from(domains.len())
        .map_err(|_| ConsensusError::InconsistentStore("checkpoint domain count overflow"))?;
    let validator_set = encode_validator_set(&checkpoint.validator_set)
        .map_err(|error| ConsensusError::InvalidGenesisOwned(error.to_string()))?;
    let mut output = Vec::new();
    output.extend_from_slice(CHECKPOINT_TAG);
    output.push(CHECKPOINT_VERSION);
    output.extend_from_slice(state.network_id.0.as_slice());
    output.extend_from_slice(&state.height.0.to_be_bytes());
    output.extend_from_slice(state.consensus_hash.as_slice());
    output.extend_from_slice(&state.timestamp.to_be_bytes());
    output.extend_from_slice(state.validator_set_hash.as_slice());
    output.extend_from_slice(state.next_validator_set_hash.as_slice());
    output.extend_from_slice(state.root_domain_id().0.as_slice());
    output.extend_from_slice(state.governance_address().as_slice());
    output.extend_from_slice(state.domain_heads_root().as_slice());
    put_bytes(&mut output, &validator_set)?;
    put_bytes(&mut output, &checkpoint.finality_proof)?;
    output.extend_from_slice(&count.to_be_bytes());
    for (domain_id, domain) in domains {
        output.extend_from_slice(domain_id.0.as_slice());
        output.extend_from_slice(&domain.config.chain_id.to_be_bytes());
        output.extend_from_slice(&domain.config.protocol_revision.to_be_bytes());
        output.extend_from_slice(&domain.config.gas_limit.to_be_bytes());
        output.extend_from_slice(&domain.config.initial_base_fee_per_gas.to_be_bytes());
        put_bytes(
            &mut output,
            &encode_domain_header(&domain.header)
                .map_err(|error| ConsensusError::InvalidGenesisOwned(error.to_string()))?,
        )?;
        output.extend_from_slice(domain.block_hash.as_slice());
        output.extend_from_slice(domain.state.state_root().as_slice());
        let descriptor = state
            .domain_descriptor(domain_id)
            .map(encode_domain_descriptor)
            .transpose()
            .map_err(|error| ConsensusError::InvalidGenesisOwned(error.to_string()))?
            .unwrap_or_default();
        put_bytes(&mut output, &descriptor)?;
    }
    if output.len() > MAX_CHECKPOINT_METADATA_BYTES {
        return Err(ConsensusError::InconsistentStore(
            "checkpoint metadata exceeds limit",
        ));
    }
    Ok(output)
}

fn decode_checkpoint(
    database: &arbor_storage::Database,
    input: &[u8],
) -> Result<DevelopmentCheckpoint, ConsensusError> {
    if input.len() > MAX_CHECKPOINT_METADATA_BYTES {
        return Err(ConsensusError::InconsistentStore(
            "checkpoint metadata exceeds limit",
        ));
    }
    let mut cursor = Cursor::new(input);
    cursor.expect(CHECKPOINT_TAG)?;
    if cursor.u8()? != CHECKPOINT_VERSION {
        return Err(ConsensusError::InconsistentStore(
            "unsupported checkpoint metadata version",
        ));
    }
    let network_id = NetworkId(cursor.hash()?);
    let height = ConsensusHeight(cursor.u64()?);
    let consensus_hash = cursor.hash()?;
    let timestamp = cursor.u64()?;
    let validator_set_hash = cursor.hash()?;
    let next_validator_set_hash = cursor.hash()?;
    let root_domain_id = DomainId(cursor.hash()?);
    let governance_address = Address::from_slice(cursor.take(20)?);
    let domain_heads_root = cursor.hash()?;
    let validator_set = decode_validator_set(cursor.bytes()?)
        .map_err(|error| ConsensusError::InvalidGenesisOwned(error.to_string()))?;
    let finality_proof = cursor.bytes()?.to_vec();
    let count = cursor.u32()? as usize;
    if count == 0 || count > MAX_CHECKPOINT_DOMAINS {
        return Err(ConsensusError::InconsistentStore(
            "invalid checkpoint domain count",
        ));
    }
    let mut domains = Vec::with_capacity(count);
    for _ in 0..count {
        let domain_id = DomainId(cursor.hash()?);
        let config = DomainConfig {
            chain_id: cursor.u64()?,
            protocol_revision: cursor.u32()?,
            gas_limit: cursor.u64()?,
            initial_base_fee_per_gas: cursor.u128()?,
        };
        let header = decode_domain_header(cursor.bytes()?)
            .map_err(|error| ConsensusError::InvalidGenesisOwned(error.to_string()))?;
        let block_hash = cursor.hash()?;
        let state_root = cursor.hash()?;
        let descriptor_bytes = cursor.bytes()?;
        let descriptor = (!descriptor_bytes.is_empty())
            .then(|| decode_domain_descriptor(descriptor_bytes))
            .transpose()
            .map_err(|error| ConsensusError::InvalidGenesisOwned(error.to_string()))?;
        let state = ExecutionState::from_persisted(state_root, database, |hash| {
            database
                .contract_code(hash)
                .map_err(|error| EvmError::State(error.to_string()))
        })
        .map_err(|error| ConsensusError::InvalidGenesisOwned(error.to_string()))?;
        domains.push(FinalizedDomainCheckpoint {
            domain_id,
            config,
            header,
            block_hash,
            state,
            descriptor,
        });
    }
    cursor.finish()?;
    let state = FinalizedChainState::from_checkpoint(FinalizedChainCheckpoint {
        network_id,
        height,
        consensus_hash,
        timestamp,
        validator_set_hash,
        next_validator_set_hash,
        root_domain_id,
        governance_address,
        domain_heads_root,
        domains,
    })?;
    Ok(DevelopmentCheckpoint {
        state,
        validator_set,
        finality_proof,
    })
}

fn put_bytes(output: &mut Vec<u8>, bytes: &[u8]) -> Result<(), ConsensusError> {
    let length = u32::try_from(bytes.len())
        .map_err(|_| ConsensusError::InconsistentStore("checkpoint field length overflow"))?;
    output.extend_from_slice(&length.to_be_bytes());
    output.extend_from_slice(bytes);
    Ok(())
}

struct Cursor<'a> {
    input: &'a [u8],
    position: usize,
}

impl<'a> Cursor<'a> {
    const fn new(input: &'a [u8]) -> Self {
        Self { input, position: 0 }
    }

    fn expect(&mut self, expected: &[u8]) -> Result<(), ConsensusError> {
        if self.take(expected.len())? != expected {
            return Err(ConsensusError::InconsistentStore(
                "checkpoint metadata tag mismatch",
            ));
        }
        Ok(())
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], ConsensusError> {
        let end = self
            .position
            .checked_add(length)
            .ok_or(ConsensusError::InconsistentStore(
                "checkpoint cursor overflow",
            ))?;
        let bytes = self
            .input
            .get(self.position..end)
            .ok_or(ConsensusError::InconsistentStore(
                "truncated checkpoint metadata",
            ))?;
        self.position = end;
        Ok(bytes)
    }

    fn u8(&mut self) -> Result<u8, ConsensusError> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, ConsensusError> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().map_err(
            |_| ConsensusError::InconsistentStore("checkpoint u32"),
        )?))
    }

    fn u64(&mut self) -> Result<u64, ConsensusError> {
        Ok(u64::from_be_bytes(self.take(8)?.try_into().map_err(
            |_| ConsensusError::InconsistentStore("checkpoint u64"),
        )?))
    }

    fn u128(&mut self) -> Result<u128, ConsensusError> {
        Ok(u128::from_be_bytes(self.take(16)?.try_into().map_err(
            |_| ConsensusError::InconsistentStore("checkpoint u128"),
        )?))
    }

    fn hash(&mut self) -> Result<B256, ConsensusError> {
        Ok(B256::from_slice(self.take(32)?))
    }

    fn bytes(&mut self) -> Result<&'a [u8], ConsensusError> {
        let length = self.u32()? as usize;
        self.take(length)
    }

    fn finish(self) -> Result<(), ConsensusError> {
        if self.position != self.input.len() {
            return Err(ConsensusError::InconsistentStore(
                "trailing checkpoint metadata",
            ));
        }
        Ok(())
    }
}
