use arbor_primitives::{
    Address, B256, Bloom, CANONICAL_CODEC_VERSION, CommitSignature, ConsensusBlockHeader,
    ConsensusHeight, ConsensusPublicKey, ConsensusRound, ConsensusSignature, DomainBatch,
    DomainBlockHeader, DomainDescriptor, DomainGenesis, DomainId, DomainNumber, DomainStatus,
    NetworkId, QuorumCertificate, U256, Validator, ValidatorId, ValidatorSet, Vote, VotePhase,
};

use crate::{
    CodecError, MAX_ACTIVE_VALIDATORS, MAX_CANONICAL_OBJECT_BYTES, MAX_TRANSACTION_ENVELOPE_BYTES,
    MAX_TRANSACTIONS_PER_BATCH,
};

const CONSENSUS_HEADER_TAG: &[u8] = b"ARBOR_CONSENSUS_HEADER_V1";
const DOMAIN_HEADER_TAG: &[u8] = b"ARBOR_DOMAIN_HEADER_V1";
const DOMAIN_BATCH_TAG: &[u8] = b"ARBOR_DOMAIN_BATCH_V1";
const DOMAIN_DESCRIPTOR_TAG: &[u8] = b"ARBOR_DOMAIN_DESCRIPTOR_V1";
const DOMAIN_GENESIS_TAG: &[u8] = b"ARBOR_DOMAIN_GENESIS_V1";
const VALIDATOR_SET_TAG: &[u8] = b"ARBOR_VALIDATOR_SET_V1";
const VOTE_TAG: &[u8] = b"ARBOR_VOTE_V1";
const QC_TAG: &[u8] = b"ARBOR_QC_V1";

struct Encoder {
    bytes: Vec<u8>,
}

impl Encoder {
    fn tagged(tag: &[u8]) -> Self {
        let mut bytes = Vec::with_capacity(tag.len() + 1 + 256);
        bytes.extend_from_slice(tag);
        bytes.push(CANONICAL_CODEC_VERSION);
        Self { bytes }
    }

    fn u8(&mut self, value: u8) {
        self.bytes.push(value);
    }

    fn u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    fn u64(&mut self, value: u64) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    fn u128(&mut self, value: u128) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    fn b256(&mut self, value: B256) {
        self.bytes.extend_from_slice(value.as_slice());
    }

    fn address(&mut self, value: Address) {
        self.bytes.extend_from_slice(value.as_slice());
    }

    fn bloom(&mut self, value: &Bloom) {
        self.bytes.extend_from_slice(value.as_slice());
    }

    fn u256(&mut self, value: U256) {
        self.bytes.extend_from_slice(&value.to_be_bytes::<32>());
    }

    fn bytes(&mut self, value: &[u8]) {
        self.u32(u32::try_from(value.len()).expect("protocol limits fit u32"));
        self.bytes.extend_from_slice(value);
    }

    fn count(&mut self, value: usize) {
        self.u32(u32::try_from(value).expect("protocol collection limits fit u32"));
    }

    fn finish(self) -> Result<Vec<u8>, CodecError> {
        check_limit(
            "canonical object",
            self.bytes.len(),
            MAX_CANONICAL_OBJECT_BYTES,
        )?;
        Ok(self.bytes)
    }
}

struct Decoder<'a> {
    input: &'a [u8],
    position: usize,
}

impl<'a> Decoder<'a> {
    fn tagged(input: &'a [u8], tag: &[u8]) -> Result<Self, CodecError> {
        check_limit("canonical object", input.len(), MAX_CANONICAL_OBJECT_BYTES)?;
        let mut decoder = Self { input, position: 0 };
        if decoder.take(tag.len())? != tag {
            return Err(CodecError::InvalidTag);
        }
        let version = decoder.u8()?;
        if version != CANONICAL_CODEC_VERSION {
            return Err(CodecError::UnknownVersion(version));
        }
        Ok(decoder)
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], CodecError> {
        let end = self
            .position
            .checked_add(length)
            .ok_or(CodecError::UnexpectedEof)?;
        let value = self
            .input
            .get(self.position..end)
            .ok_or(CodecError::UnexpectedEof)?;
        self.position = end;
        Ok(value)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], CodecError> {
        self.take(N)?
            .try_into()
            .map_err(|_| CodecError::UnexpectedEof)
    }

    fn u8(&mut self) -> Result<u8, CodecError> {
        Ok(self.array::<1>()?[0])
    }

    fn u32(&mut self) -> Result<u32, CodecError> {
        Ok(u32::from_be_bytes(self.array()?))
    }

    fn u64(&mut self) -> Result<u64, CodecError> {
        Ok(u64::from_be_bytes(self.array()?))
    }

    fn u128(&mut self) -> Result<u128, CodecError> {
        Ok(u128::from_be_bytes(self.array()?))
    }

    fn b256(&mut self) -> Result<B256, CodecError> {
        Ok(B256::from(self.array::<32>()?))
    }

    fn address(&mut self) -> Result<Address, CodecError> {
        Ok(Address::from(self.array::<20>()?))
    }

    fn bloom(&mut self) -> Result<Bloom, CodecError> {
        Ok(Bloom::from(self.array::<256>()?))
    }

    fn u256(&mut self) -> Result<U256, CodecError> {
        Ok(U256::from_be_bytes(self.array::<32>()?))
    }

    fn count(&mut self, field: &'static str, limit: usize) -> Result<usize, CodecError> {
        let count = self.u32()? as usize;
        check_limit(field, count, limit)?;
        Ok(count)
    }

    fn bytes(&mut self, field: &'static str, limit: usize) -> Result<&'a [u8], CodecError> {
        let length = self.u32()? as usize;
        check_limit(field, length, limit)?;
        self.take(length)
    }

    fn string(&mut self, field: &'static str, limit: usize) -> Result<String, CodecError> {
        let value = self.bytes(field, limit)?;
        std::str::from_utf8(value)
            .map(str::to_owned)
            .map_err(|_| CodecError::InvalidUtf8(field))
    }

    fn finish(self) -> Result<(), CodecError> {
        if self.position == self.input.len() {
            Ok(())
        } else {
            Err(CodecError::TrailingBytes)
        }
    }
}

fn check_limit(field: &'static str, actual: usize, limit: usize) -> Result<(), CodecError> {
    if actual > limit {
        Err(CodecError::LimitExceeded {
            field,
            limit,
            actual,
        })
    } else {
        Ok(())
    }
}

fn check_strictly_sorted<T: Ord>(values: &[T], field: &'static str) -> Result<(), CodecError> {
    if values.len() < 2 || values.windows(2).all(|pair| pair[0] < pair[1]) {
        Ok(())
    } else {
        Err(CodecError::NonCanonicalOrder(field))
    }
}

/// Encodes a consensus header hash preimage.
///
/// # Errors
///
/// Returns [`CodecError`] if the encoded object exceeds its protocol budget.
pub fn encode_consensus_header(value: &ConsensusBlockHeader) -> Result<Vec<u8>, CodecError> {
    let mut out = Encoder::tagged(CONSENSUS_HEADER_TAG);
    out.u32(value.protocol_version);
    out.b256(value.network_id.0);
    out.u64(value.height.0);
    out.b256(value.parent_hash);
    out.u64(value.timestamp);
    out.b256(value.batches_root);
    out.b256(value.domain_results_root);
    out.b256(value.domain_heads_root);
    out.b256(value.validator_set_hash);
    out.b256(value.next_validator_set_hash);
    out.address(value.proposer);
    out.finish()
}

/// Decodes one exact consensus header.
///
/// # Errors
///
/// Returns [`CodecError`] for malformed, unsupported, oversized, or trailing input.
pub fn decode_consensus_header(input: &[u8]) -> Result<ConsensusBlockHeader, CodecError> {
    let mut decoder = Decoder::tagged(input, CONSENSUS_HEADER_TAG)?;
    let value = ConsensusBlockHeader {
        protocol_version: decoder.u32()?,
        network_id: NetworkId(decoder.b256()?),
        height: ConsensusHeight(decoder.u64()?),
        parent_hash: decoder.b256()?,
        timestamp: decoder.u64()?,
        batches_root: decoder.b256()?,
        domain_results_root: decoder.b256()?,
        domain_heads_root: decoder.b256()?,
        validator_set_hash: decoder.b256()?,
        next_validator_set_hash: decoder.b256()?,
        proposer: decoder.address()?,
    };
    decoder.finish()?;
    Ok(value)
}

/// Encodes a logical domain header hash preimage.
///
/// # Errors
///
/// Returns [`CodecError`] if the encoded object exceeds its protocol budget.
pub fn encode_domain_header(value: &DomainBlockHeader) -> Result<Vec<u8>, CodecError> {
    let mut out = Encoder::tagged(DOMAIN_HEADER_TAG);
    out.u32(value.protocol_version);
    out.b256(value.domain_id.0);
    out.u64(value.number.0);
    out.b256(value.parent_hash);
    out.u64(value.consensus_height.0);
    out.b256(value.transactions_root);
    out.b256(value.state_root);
    out.b256(value.receipts_root);
    out.bloom(&value.logs_bloom);
    out.u64(value.gas_limit);
    out.u64(value.gas_used);
    out.u128(value.base_fee_per_gas);
    out.finish()
}

/// Decodes one exact logical domain header.
///
/// # Errors
///
/// Returns [`CodecError`] for malformed, unsupported, oversized, or trailing input.
pub fn decode_domain_header(input: &[u8]) -> Result<DomainBlockHeader, CodecError> {
    let mut decoder = Decoder::tagged(input, DOMAIN_HEADER_TAG)?;
    let value = DomainBlockHeader {
        protocol_version: decoder.u32()?,
        domain_id: DomainId(decoder.b256()?),
        number: DomainNumber(decoder.u64()?),
        parent_hash: decoder.b256()?,
        consensus_height: ConsensusHeight(decoder.u64()?),
        transactions_root: decoder.b256()?,
        state_root: decoder.b256()?,
        receipts_root: decoder.b256()?,
        logs_bloom: decoder.bloom()?,
        gas_limit: decoder.u64()?,
        gas_used: decoder.u64()?,
        base_fee_per_gas: decoder.u128()?,
    };
    decoder.finish()?;
    Ok(value)
}

/// Encodes a bounded domain batch.
///
/// # Errors
///
/// Returns [`CodecError`] for excessive transaction counts, envelope sizes, or total bytes.
pub fn encode_domain_batch(value: &DomainBatch) -> Result<Vec<u8>, CodecError> {
    check_limit(
        "domain batch transactions",
        value.transactions.len(),
        MAX_TRANSACTIONS_PER_BATCH,
    )?;
    let mut out = Encoder::tagged(DOMAIN_BATCH_TAG);
    out.b256(value.domain_id.0);
    out.b256(value.parent_domain_block_hash);
    out.count(value.transactions.len());
    for transaction in &value.transactions {
        check_limit(
            "transaction envelope",
            transaction.len(),
            MAX_TRANSACTION_ENVELOPE_BYTES,
        )?;
        out.bytes(transaction);
    }
    out.finish()
}

/// Decodes one bounded domain batch.
///
/// # Errors
///
/// Returns [`CodecError`] for malformed, oversized, unsupported, or trailing input.
pub fn decode_domain_batch(input: &[u8]) -> Result<DomainBatch, CodecError> {
    let mut decoder = Decoder::tagged(input, DOMAIN_BATCH_TAG)?;
    let domain_id = DomainId(decoder.b256()?);
    let parent_domain_block_hash = decoder.b256()?;
    let count = decoder.count("domain batch transactions", MAX_TRANSACTIONS_PER_BATCH)?;
    let mut transactions = Vec::with_capacity(count);
    for _ in 0..count {
        transactions.push(
            decoder
                .bytes("transaction envelope", MAX_TRANSACTION_ENVELOPE_BYTES)?
                .to_vec()
                .into(),
        );
    }
    decoder.finish()?;
    Ok(DomainBatch {
        domain_id,
        parent_domain_block_hash,
        transactions,
    })
}

/// Encodes a root-domain descriptor.
///
/// # Errors
///
/// Returns [`CodecError`] for invalid identifiers, metadata limits, or total size.
pub fn encode_domain_descriptor(value: &DomainDescriptor) -> Result<Vec<u8>, CodecError> {
    check_limit("domain name", value.name.len(), 64)?;
    check_limit("domain symbol", value.symbol.len(), 16)?;
    if value.evm_chain_id == 0 {
        return Err(CodecError::InvalidValue("evm chain id"));
    }
    let mut out = Encoder::tagged(DOMAIN_DESCRIPTOR_TAG);
    out.b256(value.domain_id.0);
    out.b256(value.parent_domain_id.0);
    out.b256(value.joint_domain_block_hash);
    out.b256(value.create_tx_hash);
    out.b256(value.origin_hash);
    out.bytes(value.name.as_bytes());
    out.bytes(value.symbol.as_bytes());
    out.u64(value.evm_chain_id);
    out.address(value.owner);
    out.u32(value.protocol_revision);
    out.u64(value.gas_limit);
    out.u128(value.initial_base_fee);
    out.u256(value.initial_supply);
    out.u256(value.creation_deposit);
    out.u8(value.status as u8);
    out.finish()
}

/// Decodes a root-domain descriptor.
///
/// # Errors
///
/// Returns [`CodecError`] for malformed, invalid, oversized, or trailing input.
pub fn decode_domain_descriptor(input: &[u8]) -> Result<DomainDescriptor, CodecError> {
    let mut decoder = Decoder::tagged(input, DOMAIN_DESCRIPTOR_TAG)?;
    let domain_id = DomainId(decoder.b256()?);
    let parent_domain_id = DomainId(decoder.b256()?);
    let joint_domain_block_hash = decoder.b256()?;
    let create_tx_hash = decoder.b256()?;
    let origin_hash = decoder.b256()?;
    let name = decoder.string("domain name", 64)?;
    let symbol = decoder.string("domain symbol", 16)?;
    let evm_chain_id = decoder.u64()?;
    if evm_chain_id == 0 {
        return Err(CodecError::InvalidValue("evm chain id"));
    }
    let owner = decoder.address()?;
    let protocol_revision = decoder.u32()?;
    let gas_limit = decoder.u64()?;
    let initial_base_fee = decoder.u128()?;
    let initial_supply = decoder.u256()?;
    let creation_deposit = decoder.u256()?;
    let status = match decoder.u8()? {
        0 => DomainStatus::Pending,
        1 => DomainStatus::Active,
        2 => DomainStatus::Frozen,
        _ => return Err(CodecError::InvalidValue("domain status")),
    };
    decoder.finish()?;
    Ok(DomainDescriptor {
        domain_id,
        parent_domain_id,
        joint_domain_block_hash,
        create_tx_hash,
        origin_hash,
        name,
        symbol,
        evm_chain_id,
        owner,
        protocol_revision,
        gas_limit,
        initial_base_fee,
        initial_supply,
        creation_deposit,
        status,
    })
}

/// Encodes the immutable domain genesis hash preimage.
///
/// # Errors
///
/// Returns [`CodecError`] for invalid metadata, a zero chain ID, or excessive total size.
pub fn encode_domain_genesis(value: &DomainGenesis) -> Result<Vec<u8>, CodecError> {
    check_limit("domain name", value.name.len(), 64)?;
    check_limit("domain symbol", value.symbol.len(), 16)?;
    if value.evm_chain_id == 0 {
        return Err(CodecError::InvalidValue("evm chain id"));
    }
    let mut out = Encoder::tagged(DOMAIN_GENESIS_TAG);
    out.b256(value.domain_id.0);
    out.b256(value.parent_domain_id.0);
    out.b256(value.joint_domain_block_hash);
    out.b256(value.create_tx_hash);
    out.bytes(value.name.as_bytes());
    out.bytes(value.symbol.as_bytes());
    out.u64(value.evm_chain_id);
    out.address(value.owner);
    out.u32(value.protocol_revision);
    out.u64(value.gas_limit);
    out.u128(value.initial_base_fee);
    out.u256(value.initial_supply);
    out.b256(value.initial_state_root);
    out.finish()
}

/// Decodes one immutable domain genesis preimage.
///
/// # Errors
///
/// Returns [`CodecError`] for malformed, invalid, oversized, or trailing input.
pub fn decode_domain_genesis(input: &[u8]) -> Result<DomainGenesis, CodecError> {
    let mut decoder = Decoder::tagged(input, DOMAIN_GENESIS_TAG)?;
    let domain_id = DomainId(decoder.b256()?);
    let parent_domain_id = DomainId(decoder.b256()?);
    let joint_domain_block_hash = decoder.b256()?;
    let create_tx_hash = decoder.b256()?;
    let name = decoder.string("domain name", 64)?;
    let symbol = decoder.string("domain symbol", 16)?;
    let evm_chain_id = decoder.u64()?;
    if evm_chain_id == 0 {
        return Err(CodecError::InvalidValue("evm chain id"));
    }
    let owner = decoder.address()?;
    let protocol_revision = decoder.u32()?;
    let gas_limit = decoder.u64()?;
    let initial_base_fee = decoder.u128()?;
    let initial_supply = decoder.u256()?;
    let initial_state_root = decoder.b256()?;
    decoder.finish()?;
    Ok(DomainGenesis {
        domain_id,
        parent_domain_id,
        joint_domain_block_hash,
        create_tx_hash,
        name,
        symbol,
        evm_chain_id,
        owner,
        protocol_revision,
        gas_limit,
        initial_base_fee,
        initial_supply,
        initial_state_root,
    })
}

/// Encodes a validator set after enforcing sorted, unique identifiers.
///
/// # Errors
///
/// Returns [`CodecError`] for zero power, excessive validators, or non-canonical order.
pub fn encode_validator_set(value: &ValidatorSet) -> Result<Vec<u8>, CodecError> {
    check_limit("validators", value.validators.len(), MAX_ACTIVE_VALIDATORS)?;
    if value.validators.is_empty() {
        return Err(CodecError::InvalidValue("validator set"));
    }
    let ids: Vec<_> = value
        .validators
        .iter()
        .map(|validator| validator.id)
        .collect();
    check_strictly_sorted(&ids, "validators")?;
    let mut out = Encoder::tagged(VALIDATOR_SET_TAG);
    out.u64(value.epoch);
    out.count(value.validators.len());
    for validator in &value.validators {
        if validator.power == 0 {
            return Err(CodecError::InvalidValue("validator power"));
        }
        out.b256(validator.id.0);
        out.bytes.extend_from_slice(&validator.public_key.0);
        out.u64(validator.power);
    }
    out.finish()
}

/// Decodes a validator set and rejects unsorted or duplicate identifiers.
///
/// # Errors
///
/// Returns [`CodecError`] for malformed, oversized, zero-power, or unordered input.
pub fn decode_validator_set(input: &[u8]) -> Result<ValidatorSet, CodecError> {
    let mut decoder = Decoder::tagged(input, VALIDATOR_SET_TAG)?;
    let epoch = decoder.u64()?;
    let count = decoder.count("validators", MAX_ACTIVE_VALIDATORS)?;
    if count == 0 {
        return Err(CodecError::InvalidValue("validator set"));
    }
    let mut validators = Vec::with_capacity(count);
    for _ in 0..count {
        let id = ValidatorId(decoder.b256()?);
        let public_key = ConsensusPublicKey(decoder.array::<33>()?);
        let power = decoder.u64()?;
        if power == 0 {
            return Err(CodecError::InvalidValue("validator power"));
        }
        validators.push(Validator {
            id,
            public_key,
            power,
        });
    }
    let ids: Vec<_> = validators.iter().map(|validator| validator.id).collect();
    check_strictly_sorted(&ids, "validators")?;
    decoder.finish()?;
    Ok(ValidatorSet { epoch, validators })
}

/// Encodes the exact pre-sign durable vote intent.
///
/// # Errors
///
/// Returns [`CodecError`] for a zero phase or excessive total size.
pub fn encode_vote(value: &Vote) -> Result<Vec<u8>, CodecError> {
    if value.phase.0 == 0 {
        return Err(CodecError::InvalidValue("vote phase"));
    }
    let mut out = Encoder::tagged(VOTE_TAG);
    out.b256(value.network_id.0);
    out.u64(value.height.0);
    out.u64(value.round.0);
    out.u8(value.phase.0);
    out.b256(value.block_hash);
    out.b256(value.validator_id.0);
    out.finish()
}

/// Decodes one exact durable vote intent.
///
/// # Errors
///
/// Returns [`CodecError`] for malformed, zero-phase, unsupported, or trailing input.
pub fn decode_vote(input: &[u8]) -> Result<Vote, CodecError> {
    let mut decoder = Decoder::tagged(input, VOTE_TAG)?;
    let network_id = NetworkId(decoder.b256()?);
    let height = ConsensusHeight(decoder.u64()?);
    let round = ConsensusRound(decoder.u64()?);
    let phase = VotePhase(decoder.u8()?);
    if phase.0 == 0 {
        return Err(CodecError::InvalidValue("vote phase"));
    }
    let block_hash = decoder.b256()?;
    let validator_id = ValidatorId(decoder.b256()?);
    decoder.finish()?;
    Ok(Vote {
        network_id,
        height,
        round,
        phase,
        block_hash,
        validator_id,
    })
}

/// Encodes a quorum certificate with strictly ordered signer identifiers.
///
/// # Errors
///
/// Returns [`CodecError`] for invalid phase, excessive signatures, or signer ordering.
pub fn encode_quorum_certificate(value: &QuorumCertificate) -> Result<Vec<u8>, CodecError> {
    if value.phase.0 == 0 {
        return Err(CodecError::InvalidValue("certificate phase"));
    }
    check_limit(
        "certificate signatures",
        value.signatures.len(),
        MAX_ACTIVE_VALIDATORS,
    )?;
    if value.signatures.is_empty() {
        return Err(CodecError::InvalidValue("certificate signatures"));
    }
    let ids: Vec<_> = value
        .signatures
        .iter()
        .map(|signature| signature.validator_id)
        .collect();
    check_strictly_sorted(&ids, "certificate signatures")?;
    let mut out = Encoder::tagged(QC_TAG);
    out.b256(value.network_id.0);
    out.u64(value.height.0);
    out.u64(value.round.0);
    out.u8(value.phase.0);
    out.b256(value.block_hash);
    out.count(value.signatures.len());
    for signature in &value.signatures {
        out.b256(signature.validator_id.0);
        out.bytes.extend_from_slice(&signature.signature.0);
    }
    out.finish()
}

/// Decodes a quorum certificate and rejects duplicate or unsorted signers.
///
/// # Errors
///
/// Returns [`CodecError`] for malformed, oversized, zero-phase, or unordered input.
pub fn decode_quorum_certificate(input: &[u8]) -> Result<QuorumCertificate, CodecError> {
    let mut decoder = Decoder::tagged(input, QC_TAG)?;
    let network_id = NetworkId(decoder.b256()?);
    let height = ConsensusHeight(decoder.u64()?);
    let round = ConsensusRound(decoder.u64()?);
    let phase = VotePhase(decoder.u8()?);
    if phase.0 == 0 {
        return Err(CodecError::InvalidValue("certificate phase"));
    }
    let block_hash = decoder.b256()?;
    let count = decoder.count("certificate signatures", MAX_ACTIVE_VALIDATORS)?;
    if count == 0 {
        return Err(CodecError::InvalidValue("certificate signatures"));
    }
    let mut signatures = Vec::with_capacity(count);
    for _ in 0..count {
        signatures.push(CommitSignature {
            validator_id: ValidatorId(decoder.b256()?),
            signature: ConsensusSignature(decoder.array::<64>()?),
        });
    }
    let ids: Vec<_> = signatures
        .iter()
        .map(|signature| signature.validator_id)
        .collect();
    check_strictly_sorted(&ids, "certificate signatures")?;
    decoder.finish()?;
    Ok(QuorumCertificate {
        network_id,
        height,
        round,
        phase,
        block_hash,
        signatures,
    })
}
