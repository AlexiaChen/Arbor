use std::collections::{BTreeMap, BTreeSet};

use alloy_primitives::{Address, B256, keccak256};
use arbor_chain::{
    DomainConfig, FinalizedChainCheckpoint, FinalizedChainState, FinalizedDomainCheckpoint,
};
use arbor_codec::{
    decode_domain_descriptor, decode_domain_header, decode_validator_set, encode_domain_descriptor,
    encode_domain_header, encode_validator_set,
};
use arbor_crypto::validator_set_hash;
use arbor_evm::{EvmError, ExecutionState};
use arbor_primitives::{ConsensusHeight, DomainDescriptor, DomainId, NetworkId, ValidatorSet};
use arbor_state::{DomainHead, DomainHeadProof, MemoryNodeStore, SnapshotChunk, SnapshotManifest};
use thiserror::Error;

use crate::protocol::MAX_CONSENSUS_MESSAGE_BYTES;
use crate::{MAX_DIRECT_REQUEST_BYTES, MAX_SNAPSHOT_CHUNK_BYTES};

const CHECKPOINT_TAG: &[u8] = b"ARBOR_NETWORK_CHECKPOINT_V1";
const CHECKPOINT_VERSION: u8 = 1;
const STATE_CHUNK_TAG: &[u8] = b"ARBOR_STATE_CHUNK_V1";
const STATE_CHUNK_VERSION: u8 = 1;
const CHUNK_NODE: u8 = 1;
const CHUNK_CODE: u8 = 2;
const MAX_CHECKPOINT_DOMAINS: usize = 256;
const MAX_CHUNKS_PER_DOMAIN: usize = 65_536;
const MAX_SNAPSHOT_TOTAL_BYTES: usize = 512 * 1024 * 1024;
const CHUNK_FIXED_BYTES: usize = 22 + 1 + 32 + 4 + 4;
const CHUNK_RECORD_FIXED_BYTES: usize = 1 + 32 + 4;

/// Finality verification boundary for one fully decoded checkpoint manifest.
pub trait CheckpointFinalityVerifier {
    /// Verifies the checkpoint hash, validator set, and opaque proof before chunks may be imported.
    ///
    /// # Errors
    ///
    /// Returns a stable diagnostic without changing application storage.
    fn verify(&self, manifest: &CheckpointManifest) -> Result<(), String>;
}

/// Explicit finality rule for the non-BFT single-validator development engine.
#[derive(Clone, Copy, Debug, Default)]
pub struct DevelopmentCheckpointVerifier;

impl CheckpointFinalityVerifier for DevelopmentCheckpointVerifier {
    fn verify(&self, manifest: &CheckpointManifest) -> Result<(), String> {
        if manifest.finality_proof.is_empty() {
            Ok(())
        } else {
            Err("development checkpoint proof must be empty".to_owned())
        }
    }
}

/// One domain's authenticated checkpoint metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckpointDomain {
    /// Domain identifier.
    pub domain_id: DomainId,
    /// Immutable execution parameters.
    pub config: DomainConfig,
    /// Latest finalized domain header.
    pub header: arbor_primitives::DomainBlockHeader,
    /// Canonical hash of `header`.
    pub block_hash: B256,
    /// Runtime-created descriptor; absent only for the root domain.
    pub descriptor: Option<DomainDescriptor>,
    /// Content-addressed state chunks.
    pub state: SnapshotManifest,
    /// Membership proof into `domain_heads_root`.
    pub head_proof: DomainHeadProof,
}

/// Genesis-bound checkpoint manifest authenticated before state chunks reach storage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckpointManifest {
    /// Network identifier from the local database identity.
    pub network_id: NetworkId,
    /// Genesis hash from the local database identity.
    pub genesis_hash: B256,
    /// Finalized checkpoint height.
    pub height: ConsensusHeight,
    /// Finalized consensus hash.
    pub consensus_hash: B256,
    /// Finalized consensus timestamp.
    pub timestamp: u64,
    /// Current validator-set commitment.
    pub validator_set_hash: B256,
    /// Next validator-set commitment.
    pub next_validator_set_hash: B256,
    /// Validator set required to verify finality.
    pub validator_set: ValidatorSet,
    /// Sparse commitment to every active domain head.
    pub domain_heads_root: B256,
    /// Immutable root domain.
    pub root_domain_id: DomainId,
    /// Genesis-bound governance address.
    pub governance_address: Address,
    /// Opaque finality bytes interpreted only by the selected consensus adapter.
    pub finality_proof: Vec<u8>,
    /// Strictly sorted complete active-domain set.
    pub domains: Vec<CheckpointDomain>,
}

impl CheckpointManifest {
    /// Encodes the exact versioned checkpoint manifest.
    ///
    /// # Errors
    ///
    /// Returns a limit or canonical encoding error.
    pub fn encode(&self) -> Result<Vec<u8>, SnapshotError> {
        self.validate_shape()?;
        let validator_set = encode_validator_set(&self.validator_set)
            .map_err(|error| SnapshotError::Codec(error.to_string()))?;
        let mut output = Vec::new();
        output.extend_from_slice(CHECKPOINT_TAG);
        output.push(CHECKPOINT_VERSION);
        output.extend_from_slice(self.network_id.0.as_slice());
        output.extend_from_slice(self.genesis_hash.as_slice());
        output.extend_from_slice(&self.height.0.to_be_bytes());
        output.extend_from_slice(self.consensus_hash.as_slice());
        output.extend_from_slice(&self.timestamp.to_be_bytes());
        output.extend_from_slice(self.validator_set_hash.as_slice());
        output.extend_from_slice(self.next_validator_set_hash.as_slice());
        output.extend_from_slice(self.domain_heads_root.as_slice());
        output.extend_from_slice(self.root_domain_id.0.as_slice());
        output.extend_from_slice(self.governance_address.as_slice());
        put_bytes(&mut output, &validator_set)?;
        put_bytes(&mut output, &self.finality_proof)?;
        output.extend_from_slice(
            &u32::try_from(self.domains.len())
                .map_err(|_| SnapshotError::Limit("checkpoint domain count"))?
                .to_be_bytes(),
        );
        for domain in &self.domains {
            output.extend_from_slice(domain.domain_id.0.as_slice());
            output.extend_from_slice(&domain.config.chain_id.to_be_bytes());
            output.extend_from_slice(&domain.config.protocol_revision.to_be_bytes());
            output.extend_from_slice(&domain.config.gas_limit.to_be_bytes());
            output.extend_from_slice(&domain.config.initial_base_fee_per_gas.to_be_bytes());
            put_bytes(
                &mut output,
                &encode_domain_header(&domain.header)
                    .map_err(|error| SnapshotError::Codec(error.to_string()))?,
            )?;
            output.extend_from_slice(domain.block_hash.as_slice());
            let descriptor = domain
                .descriptor
                .as_ref()
                .map(encode_domain_descriptor)
                .transpose()
                .map_err(|error| SnapshotError::Codec(error.to_string()))?
                .unwrap_or_default();
            put_bytes(&mut output, &descriptor)?;
            put_bytes(
                &mut output,
                &domain
                    .state
                    .encode()
                    .map_err(|error| SnapshotError::State(error.to_string()))?,
            )?;
            encode_head_proof(&mut output, &domain.head_proof)?;
        }
        if u64::try_from(output.len()).map_or(true, |length| length > MAX_DIRECT_REQUEST_BYTES) {
            return Err(SnapshotError::Limit("checkpoint manifest bytes"));
        }
        Ok(output)
    }

    /// Decodes an exact bounded checkpoint manifest.
    ///
    /// # Errors
    ///
    /// Rejects unknown versions, malformed fields, limits, and trailing bytes.
    pub fn decode(input: &[u8]) -> Result<Self, SnapshotError> {
        if u64::try_from(input.len()).map_or(true, |length| length > MAX_DIRECT_REQUEST_BYTES) {
            return Err(SnapshotError::Limit("checkpoint manifest bytes"));
        }
        let mut cursor = Cursor::new(input);
        cursor.expect(CHECKPOINT_TAG)?;
        if cursor.u8()? != CHECKPOINT_VERSION {
            return Err(SnapshotError::Malformed(
                "unsupported checkpoint manifest version",
            ));
        }
        let network_id = NetworkId(cursor.hash()?);
        let genesis_hash = cursor.hash()?;
        let height = ConsensusHeight(cursor.u64()?);
        let consensus_hash = cursor.hash()?;
        let timestamp = cursor.u64()?;
        let validator_set_hash = cursor.hash()?;
        let next_validator_set_hash = cursor.hash()?;
        let domain_heads_root = cursor.hash()?;
        let root_domain_id = DomainId(cursor.hash()?);
        let governance_address = Address::from_slice(cursor.take(20)?);
        let validator_set = decode_validator_set(cursor.bytes()?)
            .map_err(|error| SnapshotError::Codec(error.to_string()))?;
        let finality_proof = cursor.bytes()?.to_vec();
        let count = cursor.u32()? as usize;
        if count == 0 || count > MAX_CHECKPOINT_DOMAINS {
            return Err(SnapshotError::Limit("checkpoint domain count"));
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
                .map_err(|error| SnapshotError::Codec(error.to_string()))?;
            let block_hash = cursor.hash()?;
            let descriptor_bytes = cursor.bytes()?;
            let descriptor = (!descriptor_bytes.is_empty())
                .then(|| decode_domain_descriptor(descriptor_bytes))
                .transpose()
                .map_err(|error| SnapshotError::Codec(error.to_string()))?;
            let state = SnapshotManifest::decode(cursor.bytes()?)
                .map_err(|error| SnapshotError::State(error.to_string()))?;
            let head_proof = decode_head_proof(&mut cursor)?;
            domains.push(CheckpointDomain {
                domain_id,
                config,
                header,
                block_hash,
                descriptor,
                state,
                head_proof,
            });
        }
        cursor.finish()?;
        let manifest = Self {
            network_id,
            genesis_hash,
            height,
            consensus_hash,
            timestamp,
            validator_set_hash,
            next_validator_set_hash,
            validator_set,
            domain_heads_root,
            root_domain_id,
            governance_address,
            finality_proof,
            domains,
        };
        manifest.validate_shape()?;
        Ok(manifest)
    }

    fn validate_shape(&self) -> Result<(), SnapshotError> {
        if self.height.0 == 0 {
            return Err(SnapshotError::Malformed(
                "checkpoint height must be non-zero",
            ));
        }
        if self.finality_proof.len() > MAX_CONSENSUS_MESSAGE_BYTES {
            return Err(SnapshotError::Limit("checkpoint finality proof"));
        }
        if self.domains.is_empty() || self.domains.len() > MAX_CHECKPOINT_DOMAINS {
            return Err(SnapshotError::Limit("checkpoint domain count"));
        }
        if self
            .domains
            .windows(2)
            .any(|pair| pair[0].domain_id >= pair[1].domain_id)
        {
            return Err(SnapshotError::Malformed(
                "checkpoint domains are not strictly sorted",
            ));
        }
        if validator_set_hash(&self.validator_set)
            .map_err(|error| SnapshotError::Codec(error.to_string()))?
            != self.validator_set_hash
        {
            return Err(SnapshotError::Invalid("validator-set hash mismatch"));
        }
        for domain in &self.domains {
            domain
                .state
                .validate()
                .map_err(|error| SnapshotError::State(error.to_string()))?;
            if domain.state.consensus_height != self.height.0
                || domain.state.domain_id != domain.domain_id
                || domain.state.state_root != domain.header.state_root
                || domain.header.domain_id != domain.domain_id
                || domain.head_proof.root != self.domain_heads_root
                || domain.head_proof.domain_id != domain.domain_id
                || domain.head_proof.value
                    != Some(DomainHead {
                        domain_block_hash: domain.block_hash,
                        state_root: domain.header.state_root,
                    })
            {
                return Err(SnapshotError::Invalid(
                    "domain checkpoint metadata mismatch",
                ));
            }
            domain
                .head_proof
                .verify()
                .map_err(|error| SnapshotError::State(error.to_string()))?;
        }
        Ok(())
    }
}

/// Manifest plus exact content-addressed chunks served by a snapshot producer.
#[derive(Clone, Debug)]
pub struct SnapshotBundle {
    manifest: CheckpointManifest,
    chunks: BTreeMap<(DomainId, u32), Vec<u8>>,
}

impl SnapshotBundle {
    /// Produces a deterministic checkpoint and chunks from one finalized application view.
    ///
    /// # Errors
    ///
    /// Returns a typed error for limits or inconsistent source state.
    pub fn produce(
        state: &FinalizedChainState,
        genesis_hash: B256,
        validator_set: ValidatorSet,
        finality_proof: Vec<u8>,
    ) -> Result<Self, SnapshotError> {
        let mut chunks = BTreeMap::new();
        let mut domains = Vec::new();
        for (domain_id, domain) in state.domains() {
            let encoded_chunks = encode_state_chunks(
                domain_id,
                domain.state.snapshot().nodes(),
                domain.state.contract_code(),
            )?;
            let descriptors = encoded_chunks
                .iter()
                .enumerate()
                .map(|(index, bytes)| {
                    Ok(SnapshotChunk {
                        index: u32::try_from(index)
                            .map_err(|_| SnapshotError::Limit("snapshot chunk index"))?,
                        bytes: u32::try_from(bytes.len())
                            .map_err(|_| SnapshotError::Limit("snapshot chunk bytes"))?,
                        hash: keccak256(bytes),
                    })
                })
                .collect::<Result<Vec<_>, SnapshotError>>()?;
            for (descriptor, bytes) in descriptors.iter().zip(encoded_chunks) {
                chunks.insert((domain_id, descriptor.index), bytes);
            }
            domains.push(CheckpointDomain {
                domain_id,
                config: domain.config,
                header: domain.header.clone(),
                block_hash: domain.block_hash,
                descriptor: state.domain_descriptor(domain_id).cloned(),
                state: SnapshotManifest {
                    consensus_height: state.height.0,
                    domain_id,
                    state_root: domain.state.state_root(),
                    chunks: descriptors,
                },
                head_proof: state
                    .domain_head_proof(domain_id)
                    .ok_or(SnapshotError::Invalid("missing domain-head proof"))?,
            });
        }
        let manifest = CheckpointManifest {
            network_id: state.network_id,
            genesis_hash,
            height: state.height,
            consensus_hash: state.consensus_hash,
            timestamp: state.timestamp,
            validator_set_hash: state.validator_set_hash,
            next_validator_set_hash: state.next_validator_set_hash,
            validator_set,
            domain_heads_root: state.domain_heads_root(),
            root_domain_id: state.root_domain_id(),
            governance_address: state.governance_address(),
            finality_proof,
            domains,
        };
        manifest.encode()?;
        Ok(Self { manifest, chunks })
    }

    /// Returns the decoded manifest.
    #[must_use]
    pub const fn manifest(&self) -> &CheckpointManifest {
        &self.manifest
    }

    /// Returns exact encoded manifest bytes.
    ///
    /// # Errors
    ///
    /// Returns a manifest encoding error.
    pub fn manifest_bytes(&self) -> Result<Vec<u8>, SnapshotError> {
        self.manifest.encode()
    }

    /// Returns one exact content-addressed chunk.
    #[must_use]
    pub fn chunk(&self, domain_id: DomainId, index: u32) -> Option<&[u8]> {
        self.chunks.get(&(domain_id, index)).map(Vec::as_slice)
    }
}

/// In-memory bounded staging area that cannot publish partial snapshot state.
pub struct SnapshotStaging {
    manifest: CheckpointManifest,
    chunks: BTreeMap<(DomainId, u32), Vec<u8>>,
    expected: BTreeMap<(DomainId, u32), SnapshotChunk>,
    staged_bytes: usize,
}

impl SnapshotStaging {
    /// Authenticates manifest identity/finality before accepting chunks.
    ///
    /// # Errors
    ///
    /// Returns before allocating chunk storage on identity, proof, or manifest failure.
    pub fn new<V: CheckpointFinalityVerifier>(
        manifest_bytes: &[u8],
        expected_network: NetworkId,
        expected_genesis: B256,
        verifier: &V,
    ) -> Result<Self, SnapshotError> {
        let manifest = CheckpointManifest::decode(manifest_bytes)?;
        if manifest.network_id != expected_network || manifest.genesis_hash != expected_genesis {
            return Err(SnapshotError::Identity);
        }
        verifier
            .verify(&manifest)
            .map_err(SnapshotError::Finality)?;
        let expected = manifest
            .domains
            .iter()
            .flat_map(|domain| {
                domain
                    .state
                    .chunks
                    .iter()
                    .map(move |chunk| ((domain.domain_id, chunk.index), *chunk))
            })
            .collect();
        Ok(Self {
            manifest,
            chunks: BTreeMap::new(),
            expected,
            staged_bytes: 0,
        })
    }

    /// Stages one exact chunk after checking its declared length and Keccak hash.
    ///
    /// Duplicate identical chunks are idempotent; conflicting duplicates are rejected.
    ///
    /// # Errors
    ///
    /// Returns a typed error without modifying already staged data.
    pub fn stage_chunk(
        &mut self,
        domain_id: DomainId,
        index: u32,
        bytes: Vec<u8>,
    ) -> Result<(), SnapshotError> {
        let key = (domain_id, index);
        let descriptor = self
            .expected
            .get(&key)
            .ok_or(SnapshotError::UnexpectedChunk)?;
        if !SnapshotManifest::verify_chunk(*descriptor, &bytes) {
            return Err(SnapshotError::ChunkHash);
        }
        if let Some(existing) = self.chunks.get(&key) {
            if existing == &bytes {
                return Ok(());
            }
            return Err(SnapshotError::ConflictingChunk);
        }
        let total = self
            .staged_bytes
            .checked_add(bytes.len())
            .ok_or(SnapshotError::Limit("snapshot staged bytes"))?;
        if total > MAX_SNAPSHOT_TOTAL_BYTES {
            return Err(SnapshotError::Limit("snapshot staged bytes"));
        }
        self.chunks.insert(key, bytes);
        self.staged_bytes = total;
        Ok(())
    }

    /// Returns whether every manifest chunk is present.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.chunks.len() == self.expected.len()
    }

    /// Reconstructs every trie/code store and validates the full finalized chain checkpoint.
    ///
    /// # Errors
    ///
    /// Missing, duplicate, corrupt, or root-inconsistent data is rejected before storage import.
    pub fn finish(self) -> Result<SnapshotImport, SnapshotError> {
        if !self.is_complete() {
            return Err(SnapshotError::Incomplete);
        }
        let mut domains = Vec::with_capacity(self.manifest.domains.len());
        for domain in &self.manifest.domains {
            let mut nodes = BTreeMap::new();
            let mut code = BTreeMap::new();
            for descriptor in &domain.state.chunks {
                let bytes = self
                    .chunks
                    .get(&(domain.domain_id, descriptor.index))
                    .ok_or(SnapshotError::Incomplete)?;
                decode_state_chunk(
                    domain.domain_id,
                    descriptor.index,
                    bytes,
                    &mut nodes,
                    &mut code,
                )?;
            }
            let store = MemoryNodeStore::new(nodes);
            let state = ExecutionState::from_persisted(domain.state.state_root, &store, |hash| {
                Ok(code.get(&hash).cloned())
            })
            .map_err(|error| SnapshotError::State(error.to_string()))?;
            let referenced = state
                .contract_code()
                .keys()
                .copied()
                .collect::<BTreeSet<_>>();
            if referenced != code.keys().copied().collect() {
                return Err(SnapshotError::Invalid(
                    "snapshot contains unreferenced contract code",
                ));
            }
            domains.push(FinalizedDomainCheckpoint {
                domain_id: domain.domain_id,
                config: domain.config,
                header: domain.header.clone(),
                block_hash: domain.block_hash,
                state,
                descriptor: domain.descriptor.clone(),
            });
        }
        let state = FinalizedChainState::from_checkpoint(FinalizedChainCheckpoint {
            network_id: self.manifest.network_id,
            height: self.manifest.height,
            consensus_hash: self.manifest.consensus_hash,
            timestamp: self.manifest.timestamp,
            validator_set_hash: self.manifest.validator_set_hash,
            next_validator_set_hash: self.manifest.next_validator_set_hash,
            root_domain_id: self.manifest.root_domain_id,
            governance_address: self.manifest.governance_address,
            domain_heads_root: self.manifest.domain_heads_root,
            domains,
        })
        .map_err(|error| SnapshotError::State(error.to_string()))?;
        Ok(SnapshotImport {
            state,
            validator_set: self.manifest.validator_set,
            finality_proof: self.manifest.finality_proof,
        })
    }
}

/// Fully verified snapshot result ready for one atomic consensus import.
pub struct SnapshotImport {
    /// Reconstructed finalized application state.
    pub state: FinalizedChainState,
    /// Manifest validator set.
    pub validator_set: ValidatorSet,
    /// Manifest finality proof.
    pub finality_proof: Vec<u8>,
}

fn encode_state_chunks(
    domain_id: DomainId,
    nodes: &BTreeMap<B256, Vec<u8>>,
    code: &BTreeMap<B256, Vec<u8>>,
) -> Result<Vec<Vec<u8>>, SnapshotError> {
    let records = nodes
        .iter()
        .map(|(hash, bytes)| (CHUNK_NODE, *hash, bytes.as_slice()))
        .chain(
            code.iter()
                .map(|(hash, bytes)| (CHUNK_CODE, *hash, bytes.as_slice())),
        )
        .collect::<Vec<_>>();
    let mut chunks = Vec::new();
    let mut cursor = 0;
    while cursor < records.len() || chunks.is_empty() {
        let index =
            u32::try_from(chunks.len()).map_err(|_| SnapshotError::Limit("snapshot chunks"))?;
        let mut size = CHUNK_FIXED_BYTES;
        let start = cursor;
        while let Some((_, _, bytes)) = records.get(cursor) {
            let record_size = CHUNK_RECORD_FIXED_BYTES
                .checked_add(bytes.len())
                .ok_or(SnapshotError::Limit("snapshot record bytes"))?;
            if size + record_size > MAX_SNAPSHOT_CHUNK_BYTES {
                if cursor == start {
                    return Err(SnapshotError::Limit("snapshot record bytes"));
                }
                break;
            }
            size += record_size;
            cursor += 1;
        }
        let selected = &records[start..cursor];
        let mut output = Vec::with_capacity(size);
        output.extend_from_slice(STATE_CHUNK_TAG);
        output.push(STATE_CHUNK_VERSION);
        output.extend_from_slice(domain_id.0.as_slice());
        output.extend_from_slice(&index.to_be_bytes());
        output.extend_from_slice(
            &u32::try_from(selected.len())
                .map_err(|_| SnapshotError::Limit("snapshot records"))?
                .to_be_bytes(),
        );
        for (kind, hash, bytes) in selected {
            output.push(*kind);
            output.extend_from_slice(hash.as_slice());
            output.extend_from_slice(
                &u32::try_from(bytes.len())
                    .map_err(|_| SnapshotError::Limit("snapshot record bytes"))?
                    .to_be_bytes(),
            );
            output.extend_from_slice(bytes);
        }
        chunks.push(output);
        if chunks.len() > MAX_CHUNKS_PER_DOMAIN {
            return Err(SnapshotError::Limit("snapshot chunks"));
        }
    }
    Ok(chunks)
}

fn decode_state_chunk(
    expected_domain: DomainId,
    expected_index: u32,
    input: &[u8],
    nodes: &mut BTreeMap<B256, Vec<u8>>,
    code: &mut BTreeMap<B256, Vec<u8>>,
) -> Result<(), SnapshotError> {
    if input.is_empty() || input.len() > MAX_SNAPSHOT_CHUNK_BYTES {
        return Err(SnapshotError::Limit("snapshot chunk bytes"));
    }
    let mut cursor = Cursor::new(input);
    cursor.expect(STATE_CHUNK_TAG)?;
    if cursor.u8()? != STATE_CHUNK_VERSION
        || DomainId(cursor.hash()?) != expected_domain
        || cursor.u32()? != expected_index
    {
        return Err(SnapshotError::Malformed("snapshot chunk identity"));
    }
    let count = cursor.u32()? as usize;
    for _ in 0..count {
        let kind = cursor.u8()?;
        let hash = cursor.hash()?;
        let bytes = cursor.bytes()?.to_vec();
        if keccak256(&bytes) != hash {
            return Err(SnapshotError::ChunkHash);
        }
        let target = match kind {
            CHUNK_NODE => &mut *nodes,
            CHUNK_CODE => &mut *code,
            _ => return Err(SnapshotError::Malformed("snapshot record kind")),
        };
        if target.insert(hash, bytes).is_some() {
            return Err(SnapshotError::Invalid("duplicate snapshot record"));
        }
    }
    cursor.finish()
}

fn encode_head_proof(output: &mut Vec<u8>, proof: &DomainHeadProof) -> Result<(), SnapshotError> {
    output.extend_from_slice(proof.root.as_slice());
    output.extend_from_slice(proof.domain_id.0.as_slice());
    match proof.value {
        Some(value) => {
            output.push(1);
            output.extend_from_slice(value.domain_block_hash.as_slice());
            output.extend_from_slice(value.state_root.as_slice());
        }
        None => output.push(0),
    }
    output.extend_from_slice(
        &u16::try_from(proof.siblings.len())
            .map_err(|_| SnapshotError::Limit("domain-head proof siblings"))?
            .to_be_bytes(),
    );
    for sibling in &proof.siblings {
        output.extend_from_slice(sibling.as_slice());
    }
    Ok(())
}

fn decode_head_proof(cursor: &mut Cursor<'_>) -> Result<DomainHeadProof, SnapshotError> {
    let root = cursor.hash()?;
    let domain_id = DomainId(cursor.hash()?);
    let value = match cursor.u8()? {
        0 => None,
        1 => Some(DomainHead {
            domain_block_hash: cursor.hash()?,
            state_root: cursor.hash()?,
        }),
        _ => return Err(SnapshotError::Malformed("domain-head proof value")),
    };
    let count = cursor.u16()? as usize;
    if count != 256 {
        return Err(SnapshotError::Malformed("domain-head proof depth"));
    }
    let mut siblings = Vec::with_capacity(count);
    for _ in 0..count {
        siblings.push(cursor.hash()?);
    }
    Ok(DomainHeadProof {
        root,
        domain_id,
        value,
        siblings,
    })
}

fn put_bytes(output: &mut Vec<u8>, bytes: &[u8]) -> Result<(), SnapshotError> {
    output.extend_from_slice(
        &u32::try_from(bytes.len())
            .map_err(|_| SnapshotError::Limit("encoded field bytes"))?
            .to_be_bytes(),
    );
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

    fn expect(&mut self, expected: &[u8]) -> Result<(), SnapshotError> {
        if self.take(expected.len())? != expected {
            return Err(SnapshotError::Malformed("encoded tag mismatch"));
        }
        Ok(())
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], SnapshotError> {
        let end = self
            .position
            .checked_add(length)
            .ok_or(SnapshotError::Malformed("encoded cursor overflow"))?;
        let bytes = self
            .input
            .get(self.position..end)
            .ok_or(SnapshotError::Malformed("truncated encoded input"))?;
        self.position = end;
        Ok(bytes)
    }

    fn u8(&mut self) -> Result<u8, SnapshotError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, SnapshotError> {
        Ok(u16::from_be_bytes(
            self.take(2)?
                .try_into()
                .map_err(|_| SnapshotError::Malformed("encoded u16"))?,
        ))
    }

    fn u32(&mut self) -> Result<u32, SnapshotError> {
        Ok(u32::from_be_bytes(
            self.take(4)?
                .try_into()
                .map_err(|_| SnapshotError::Malformed("encoded u32"))?,
        ))
    }

    fn u64(&mut self) -> Result<u64, SnapshotError> {
        Ok(u64::from_be_bytes(
            self.take(8)?
                .try_into()
                .map_err(|_| SnapshotError::Malformed("encoded u64"))?,
        ))
    }

    fn u128(&mut self) -> Result<u128, SnapshotError> {
        Ok(u128::from_be_bytes(
            self.take(16)?
                .try_into()
                .map_err(|_| SnapshotError::Malformed("encoded u128"))?,
        ))
    }

    fn hash(&mut self) -> Result<B256, SnapshotError> {
        Ok(B256::from_slice(self.take(32)?))
    }

    fn bytes(&mut self) -> Result<&'a [u8], SnapshotError> {
        let length = self.u32()? as usize;
        self.take(length)
    }

    fn finish(self) -> Result<(), SnapshotError> {
        if self.position != self.input.len() {
            return Err(SnapshotError::Malformed("trailing encoded bytes"));
        }
        Ok(())
    }
}

/// Snapshot production, transport, verification, or reconstruction failure.
#[derive(Debug, Error)]
pub enum SnapshotError {
    /// Network/genesis identity differs from the local database.
    #[error("checkpoint network/genesis identity mismatch")]
    Identity,
    /// Consensus adapter rejected checkpoint finality.
    #[error("checkpoint finality verification failed: {0}")]
    Finality(String),
    /// A fixed protocol resource budget was exceeded.
    #[error("snapshot limit exceeded: {0}")]
    Limit(&'static str),
    /// Exact binary framing is malformed.
    #[error("malformed snapshot: {0}")]
    Malformed(&'static str),
    /// Canonical Arbor codec failed.
    #[error("snapshot codec failure: {0}")]
    Codec(String),
    /// Authenticated state reconstruction failed.
    #[error("snapshot state failure: {0}")]
    State(String),
    /// Cross-field or authenticated commitment mismatch.
    #[error("invalid snapshot: {0}")]
    Invalid(&'static str),
    /// Chunk was not declared by the manifest.
    #[error("snapshot chunk was not declared")]
    UnexpectedChunk,
    /// Chunk bytes do not match their descriptor.
    #[error("snapshot chunk hash/length mismatch")]
    ChunkHash,
    /// Same chunk identity was supplied with different bytes.
    #[error("conflicting duplicate snapshot chunk")]
    ConflictingChunk,
    /// Import attempted before every declared chunk arrived.
    #[error("snapshot staging is incomplete")]
    Incomplete,
}

impl From<EvmError> for SnapshotError {
    fn from(error: EvmError) -> Self {
        Self::State(error.to_string())
    }
}
