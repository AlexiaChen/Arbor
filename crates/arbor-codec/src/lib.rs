//! Canonical Arbor codecs and bounded EIP-2718 transaction envelopes.

#![forbid(unsafe_code)]

mod canonical;
mod ethereum;

pub use canonical::{
    decode_consensus_header, decode_domain_batch, decode_domain_descriptor, decode_domain_genesis,
    decode_domain_header, decode_quorum_certificate, decode_validator_set, decode_vote,
    encode_consensus_header, encode_domain_batch, encode_domain_descriptor, encode_domain_genesis,
    encode_domain_header, encode_quorum_certificate, encode_validator_set, encode_vote,
};
pub use ethereum::{
    EIP_1559_TX_TYPE, decode_eip1559, decode_eip1559_receipt, encode_eip1559,
    encode_eip1559_receipt, encode_eip1559_signing_payload,
};

use thiserror::Error;

/// Maximum accepted EIP-2718 transaction envelope.
pub const MAX_TRANSACTION_ENVELOPE_BYTES: usize = 256 * 1024;
/// Maximum accepted EIP-1559 calldata.
pub const MAX_CALLDATA_BYTES: usize = 128 * 1024;
/// Maximum EIP-3860 contract-creation initcode.
pub const MAX_INITCODE_BYTES: usize = 49_152;
/// Maximum access-list addresses.
pub const MAX_ACCESS_LIST_ADDRESSES: usize = 1_024;
/// Maximum storage keys in one access-list entry.
pub const MAX_ACCESS_LIST_STORAGE_KEYS: usize = 1_024;
/// Maximum batches in one consensus block.
pub const MAX_BATCHES_PER_BLOCK: usize = 256;
/// Maximum transactions in one domain batch.
pub const MAX_TRANSACTIONS_PER_BATCH: usize = 10_000;
/// Maximum canonical collection length.
pub const MAX_CANONICAL_COLLECTION_ITEMS: usize = 65_536;
/// Maximum validators and certificate signatures.
pub const MAX_ACTIVE_VALIDATORS: usize = 100;
/// Maximum encoded consensus object accepted by this codec.
pub const MAX_CANONICAL_OBJECT_BYTES: usize = 16 * 1024 * 1024;

/// Canonical or Ethereum envelope failure.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum CodecError {
    /// Input exceeds its outer byte budget.
    #[error("{kind} is {actual} bytes; limit is {limit}")]
    InputTooLarge {
        /// Input category.
        kind: &'static str,
        /// Configured limit.
        limit: usize,
        /// Observed length.
        actual: usize,
    },
    /// Input ended in the middle of a field.
    #[error("unexpected end of canonical input")]
    UnexpectedEof,
    /// An Arbor-native domain tag did not match the requested type.
    #[error("invalid canonical domain tag")]
    InvalidTag,
    /// The canonical codec version is unknown.
    #[error("unsupported canonical codec version {0}")]
    UnknownVersion(u8),
    /// Bytes remain after a complete value.
    #[error("trailing bytes after canonical value")]
    TrailingBytes,
    /// A collection or variable byte field exceeds its protocol limit.
    #[error("{field} contains {actual} items or bytes; limit is {limit}")]
    LimitExceeded {
        /// Field name.
        field: &'static str,
        /// Configured limit.
        limit: usize,
        /// Observed length.
        actual: usize,
    },
    /// A string field is not UTF-8.
    #[error("{0} is not valid UTF-8")]
    InvalidUtf8(&'static str),
    /// A scalar or enum value is not valid for the protocol type.
    #[error("invalid value for {0}")]
    InvalidValue(&'static str),
    /// A consensus collection is not strictly sorted or contains duplicates.
    #[error("{0} must be strictly sorted and unique")]
    NonCanonicalOrder(&'static str),
    /// Ethereum RLP rejected malformed or non-minimal input.
    #[error("invalid Ethereum RLP: {0}")]
    Rlp(String),
}

impl From<alloy_rlp::Error> for CodecError {
    fn from(error: alloy_rlp::Error) -> Self {
        Self::Rlp(error.to_string())
    }
}
