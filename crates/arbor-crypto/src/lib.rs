//! Keccak hashing, EIP-1559 sender recovery, and consensus-signature boundaries.

#![forbid(unsafe_code)]

use alloy_primitives::{Signature, keccak256};
use arbor_codec::{
    CodecError, encode_consensus_header, encode_domain_genesis, encode_eip1559,
    encode_eip1559_signing_payload, encode_quorum_certificate, encode_validator_set, encode_vote,
};
use arbor_primitives::{
    Address, B256, CANONICAL_CODEC_VERSION, ConsensusBlockHeader, ConsensusPublicKey,
    ConsensusSignature, DomainGenesis, DomainId, Eip1559Transaction, NetworkId, QuorumCertificate,
    ValidatorId, ValidatorSet, Vote,
};
use k256::ecdsa::{
    Signature as K256Signature, SigningKey, VerifyingKey,
    signature::hazmat::{PrehashSigner, PrehashVerifier},
};
use thiserror::Error;

const DOMAIN_ID_TAG: &[u8] = b"ARBOR_DOMAIN_V1";
const VALIDATOR_ID_TAG: &[u8] = b"ARBOR_VALIDATOR_ID_V1";

/// Cryptographic validation or key failure.
#[derive(Debug, Error)]
pub enum CryptoError {
    /// Canonical bytes could not be produced.
    #[error(transparent)]
    Codec(#[from] CodecError),
    /// A secp256k1 secret key is invalid.
    #[error("invalid secp256k1 secret key")]
    InvalidSecretKey,
    /// A compressed validator public key is invalid.
    #[error("invalid secp256k1 consensus public key")]
    InvalidPublicKey,
    /// A signature is malformed, high-s, or fails verification/recovery.
    #[error("invalid or non-canonical secp256k1 signature")]
    InvalidSignature,
    /// The public key does not match the validator ID inside the vote.
    #[error("consensus key does not match vote validator id")]
    ValidatorMismatch,
    /// A certificate references a validator outside the finalized set.
    #[error("certificate references an unknown validator")]
    UnknownValidator,
    /// The attached voting power is not strictly greater than two thirds.
    #[error("certificate does not have strictly greater than two-thirds voting power")]
    InsufficientQuorum,
    /// Voting power arithmetic overflowed.
    #[error("validator voting power overflow")]
    VotingPowerOverflow,
}

/// Returns Keccak-256 of arbitrary bytes.
#[must_use]
pub fn keccak(input: impl AsRef<[u8]>) -> B256 {
    keccak256(input)
}

/// Derives the ADR-002 domain identifier from fixed-width canonical fields.
#[must_use]
pub fn derive_domain_id(
    network_id: NetworkId,
    parent_domain_id: DomainId,
    create_tx_hash: B256,
) -> DomainId {
    let mut preimage = Vec::with_capacity(DOMAIN_ID_TAG.len() + 1 + 96);
    preimage.extend_from_slice(DOMAIN_ID_TAG);
    preimage.push(CANONICAL_CODEC_VERSION);
    preimage.extend_from_slice(network_id.0.as_slice());
    preimage.extend_from_slice(parent_domain_id.0.as_slice());
    preimage.extend_from_slice(create_tx_hash.as_slice());
    DomainId(keccak256(preimage))
}

/// Hashes a canonical consensus header.
///
/// # Errors
///
/// Returns [`CryptoError::Codec`] if canonical encoding fails.
pub fn consensus_header_hash(value: &ConsensusBlockHeader) -> Result<B256, CryptoError> {
    Ok(keccak256(encode_consensus_header(value)?))
}

/// Hashes canonical immutable domain-genesis bytes.
///
/// # Errors
///
/// Returns [`CryptoError::Codec`] if genesis validation or encoding fails.
pub fn domain_genesis_hash(value: &DomainGenesis) -> Result<B256, CryptoError> {
    Ok(keccak256(encode_domain_genesis(value)?))
}

/// Hashes the canonical validator set.
///
/// # Errors
///
/// Returns [`CryptoError::Codec`] if the validator set is non-canonical.
pub fn validator_set_hash(value: &ValidatorSet) -> Result<B256, CryptoError> {
    validate_validator_set(value)?;
    Ok(keccak256(encode_validator_set(value)?))
}

fn validate_validator_set(value: &ValidatorSet) -> Result<(), CryptoError> {
    let mut total = 0_u128;
    for validator in &value.validators {
        VerifyingKey::from_sec1_bytes(&validator.public_key.0)
            .map_err(|_| CryptoError::InvalidPublicKey)?;
        if validator.id != validator_id(validator.public_key) {
            return Err(CryptoError::ValidatorMismatch);
        }
        total = total
            .checked_add(u128::from(validator.power))
            .ok_or(CryptoError::VotingPowerOverflow)?;
    }
    if total == 0 {
        return Err(CryptoError::InsufficientQuorum);
    }
    Ok(())
}

/// Hashes the canonical quorum certificate.
///
/// # Errors
///
/// Returns [`CryptoError::Codec`] if the certificate is non-canonical.
pub fn quorum_certificate_hash(value: &QuorumCertificate) -> Result<B256, CryptoError> {
    Ok(keccak256(encode_quorum_certificate(value)?))
}

/// Returns the standard EIP-1559 signing hash.
///
/// # Errors
///
/// Returns [`CryptoError::Codec`] when transaction fields violate protocol limits.
pub fn eip1559_signing_hash(value: &Eip1559Transaction) -> Result<B256, CryptoError> {
    Ok(keccak256(encode_eip1559_signing_payload(value)?))
}

/// Returns the standard EIP-2718 transaction hash of a signed type-2 envelope.
///
/// # Errors
///
/// Returns [`CryptoError::Codec`] when transaction fields or signature scalars are invalid.
pub fn eip1559_transaction_hash(value: &Eip1559Transaction) -> Result<B256, CryptoError> {
    Ok(keccak256(encode_eip1559(value)?))
}

/// Recovers and validates the EIP-1559 sender, rejecting high-s signatures.
///
/// # Errors
///
/// Returns [`CryptoError`] for invalid fields, malformed scalars, high-s, or failed recovery.
pub fn recover_eip1559_sender(value: &Eip1559Transaction) -> Result<Address, CryptoError> {
    let signature = Signature::from_scalars_and_parity(
        B256::from(value.r.to_be_bytes::<32>()),
        B256::from(value.s.to_be_bytes::<32>()),
        value.y_parity,
    );
    if signature.normalize_s().is_some() {
        return Err(CryptoError::InvalidSignature);
    }
    signature
        .recover_address_from_prehash(&eip1559_signing_hash(value)?)
        .map_err(|_| CryptoError::InvalidSignature)
}

/// In-memory consensus signer. Validator safety storage must authorize a vote
/// before this object is called; this type does not weaken ADR-004's ordering.
pub struct ConsensusSigner(SigningKey);

impl ConsensusSigner {
    /// Creates a signer from a 32-byte secp256k1 secret scalar.
    ///
    /// # Errors
    ///
    /// Returns [`CryptoError::InvalidSecretKey`] for zero or out-of-range keys.
    pub fn from_secret_bytes(secret: &[u8; 32]) -> Result<Self, CryptoError> {
        SigningKey::from_bytes(secret.into())
            .map(Self)
            .map_err(|_| CryptoError::InvalidSecretKey)
    }

    /// Returns the compressed, consensus-only public key.
    #[must_use]
    pub fn public_key(&self) -> ConsensusPublicKey {
        let encoded = self.0.verifying_key().to_encoded_point(true);
        let mut bytes = [0_u8; 33];
        bytes.copy_from_slice(encoded.as_bytes());
        ConsensusPublicKey(bytes)
    }

    /// Signs an already durability-authorized vote.
    ///
    /// # Errors
    ///
    /// Returns a codec or signing error. The caller must not publish on error.
    pub fn sign_vote(&self, vote: &Vote) -> Result<ConsensusSignature, CryptoError> {
        if vote.validator_id != validator_id(self.public_key()) {
            return Err(CryptoError::ValidatorMismatch);
        }
        let digest = keccak256(encode_vote(vote)?);
        let signature: K256Signature = self
            .0
            .sign_prehash(digest.as_slice())
            .map_err(|_| CryptoError::InvalidSignature)?;
        let mut bytes = [0_u8; 64];
        bytes.copy_from_slice(signature.to_bytes().as_ref());
        Ok(ConsensusSignature(bytes))
    }
}

/// Derives the stable validator identifier from its compressed public key.
#[must_use]
pub fn validator_id(public_key: ConsensusPublicKey) -> ValidatorId {
    let mut preimage = Vec::with_capacity(VALIDATOR_ID_TAG.len() + 1 + 33);
    preimage.extend_from_slice(VALIDATOR_ID_TAG);
    preimage.push(CANONICAL_CODEC_VERSION);
    preimage.extend_from_slice(&public_key.0);
    ValidatorId(keccak256(preimage))
}

/// Verifies a consensus signature against the exact vote preimage.
///
/// # Errors
///
/// Returns [`CryptoError`] for invalid keys, signatures, vote bytes, or verification failure.
pub fn verify_vote_signature(
    public_key: ConsensusPublicKey,
    vote: &Vote,
    signature: ConsensusSignature,
) -> Result<(), CryptoError> {
    if vote.validator_id != validator_id(public_key) {
        return Err(CryptoError::ValidatorMismatch);
    }
    let key =
        VerifyingKey::from_sec1_bytes(&public_key.0).map_err(|_| CryptoError::InvalidPublicKey)?;
    let signature =
        K256Signature::from_slice(&signature.0).map_err(|_| CryptoError::InvalidSignature)?;
    if signature.normalize_s().is_some() {
        return Err(CryptoError::InvalidSignature);
    }
    let digest = keccak256(encode_vote(vote)?);
    key.verify_prehash(digest.as_slice(), &signature)
        .map_err(|_| CryptoError::InvalidSignature)
}

/// Verifies signer membership, every signature, and weighted quorum power.
///
/// # Errors
///
/// Returns [`CryptoError`] for a malformed set/certificate, unknown signer,
/// invalid signature, or voting power not strictly greater than two thirds.
pub fn verify_quorum_certificate(
    certificate: &QuorumCertificate,
    validator_set: &ValidatorSet,
) -> Result<(), CryptoError> {
    validate_validator_set(validator_set)?;
    encode_quorum_certificate(certificate)?;

    let total_power = validator_set
        .validators
        .iter()
        .try_fold(0_u128, |total, validator| {
            total
                .checked_add(u128::from(validator.power))
                .ok_or(CryptoError::VotingPowerOverflow)
        })?;
    let mut signed_power = 0_u128;
    for commit in &certificate.signatures {
        let index = validator_set
            .validators
            .binary_search_by_key(&commit.validator_id, |validator| validator.id)
            .map_err(|_| CryptoError::UnknownValidator)?;
        let validator = &validator_set.validators[index];
        let vote = Vote {
            network_id: certificate.network_id,
            height: certificate.height,
            round: certificate.round,
            phase: certificate.phase,
            block_hash: certificate.block_hash,
            validator_id: commit.validator_id,
        };
        verify_vote_signature(validator.public_key, &vote, commit.signature)?;
        signed_power = signed_power
            .checked_add(u128::from(validator.power))
            .ok_or(CryptoError::VotingPowerOverflow)?;
    }
    if signed_power * 3 <= total_power * 2 {
        return Err(CryptoError::InsufficientQuorum);
    }
    Ok(())
}
