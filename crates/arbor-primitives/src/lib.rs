//! Consensus-facing primitive types.
//!
//! This crate deliberately contains data and invariant-bearing newtypes only.
//! Canonical bytes live in `arbor-codec`; hashing and signatures live in
//! `arbor-crypto`.

#![forbid(unsafe_code)]

pub use alloy_primitives::{Address, B256, Bloom, Bytes, U256};

/// Version of the Arbor wire and canonical encoding rules.
pub const PROTOCOL_VERSION: u32 = 1;
/// Version byte following every Arbor-native canonical domain tag.
pub const CANONICAL_CODEC_VERSION: u8 = 1;

macro_rules! hash_identifier {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(pub B256);

        impl $name {
            /// Creates an identifier from its canonical 32 bytes.
            #[must_use]
            pub const fn new(value: B256) -> Self {
                Self(value)
            }

            /// Returns the canonical 32-byte value.
            #[must_use]
            pub const fn as_b256(self) -> B256 {
                self.0
            }
        }

        impl From<B256> for $name {
            fn from(value: B256) -> Self {
                Self(value)
            }
        }

        impl From<$name> for B256 {
            fn from(value: $name) -> Self {
                value.0
            }
        }
    };
}

hash_identifier!(
    NetworkId,
    "Hash identifying a genesis and protocol network."
);
hash_identifier!(DomainId, "Globally unique logical-domain identifier.");
hash_identifier!(
    ValidatorId,
    "Stable identifier derived from a validator key."
);

macro_rules! number_identifier {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(pub u64);

        impl $name {
            /// Creates a typed protocol number.
            #[must_use]
            pub const fn new(value: u64) -> Self {
                Self(value)
            }

            /// Returns the underlying number.
            #[must_use]
            pub const fn get(self) -> u64 {
                self.0
            }
        }
    };
}

number_identifier!(ConsensusHeight, "Root consensus-chain height.");
number_identifier!(DomainNumber, "Logical block number within one domain.");
number_identifier!(ConsensusRound, "BFT round or view within a height.");

/// Consensus header finalized by the root validator set.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConsensusBlockHeader {
    /// Protocol rules used to interpret the block.
    pub protocol_version: u32,
    /// Genesis-bound network identifier.
    pub network_id: NetworkId,
    /// Root consensus height.
    pub height: ConsensusHeight,
    /// Previous consensus-block hash.
    pub parent_hash: B256,
    /// Consensus timestamp in seconds since the Unix epoch.
    pub timestamp: u64,
    /// Commitment to canonically ordered domain batches.
    pub batches_root: B256,
    /// Commitment to domain execution results produced at this height.
    pub domain_results_root: B256,
    /// Sparse commitment to all active domain heads.
    pub domain_heads_root: B256,
    /// Active validator-set commitment.
    pub validator_set_hash: B256,
    /// Next validator-set commitment.
    pub next_validator_set_hash: B256,
    /// Root-domain reward address of the proposer.
    pub proposer: Address,
}

/// Ordered transaction batch for one logical domain.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DomainBatch {
    /// Target domain.
    pub domain_id: DomainId,
    /// Finalized parent domain-block hash.
    pub parent_domain_block_hash: B256,
    /// EIP-2718 envelopes in execution order.
    pub transactions: Vec<Bytes>,
}

/// Header of a logical domain block derived from one consensus block.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DomainBlockHeader {
    /// Protocol rules used to interpret the block.
    pub protocol_version: u32,
    /// Logical domain identifier.
    pub domain_id: DomainId,
    /// Domain-local block number.
    pub number: DomainNumber,
    /// Previous domain-block hash.
    pub parent_hash: B256,
    /// Root height that finalized this block.
    pub consensus_height: ConsensusHeight,
    /// Ethereum transaction-trie root.
    pub transactions_root: B256,
    /// Ethereum account-state root.
    pub state_root: B256,
    /// Ethereum receipt-trie root.
    pub receipts_root: B256,
    /// Aggregate Ethereum logs bloom.
    pub logs_bloom: Bloom,
    /// Domain gas limit.
    pub gas_limit: u64,
    /// Gas consumed by this block.
    pub gas_used: u64,
    /// EIP-1559 base fee.
    pub base_fee_per_gas: u128,
}

/// One EVM log committed by a receipt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Log {
    /// Contract that emitted the log.
    pub address: Address,
    /// Indexed log topics.
    pub topics: Vec<B256>,
    /// Unindexed log data.
    pub data: Bytes,
}

/// Consensus fields of an Ethereum-compatible receipt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Receipt {
    /// Transaction execution status.
    pub status: bool,
    /// Cumulative gas used within the domain block.
    pub cumulative_gas_used: u64,
    /// Bloom covering all receipt logs.
    pub logs_bloom: Bloom,
    /// Ordered EVM logs.
    pub logs: Vec<Log>,
}

/// EIP-2930 access-list entry used by EIP-1559 transactions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AccessListItem {
    /// Warmed account address.
    pub address: Address,
    /// Warmed storage slots.
    pub storage_keys: Vec<B256>,
}

/// Signed EIP-1559 transaction fields carried by a type-2 envelope.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Eip1559Transaction {
    /// Target EVM chain ID.
    pub chain_id: u64,
    /// Sender account nonce.
    pub nonce: u64,
    /// Maximum proposer tip per gas.
    pub max_priority_fee_per_gas: u128,
    /// Maximum total fee per gas.
    pub max_fee_per_gas: u128,
    /// Transaction gas limit.
    pub gas_limit: u64,
    /// Recipient, or `None` for contract creation.
    pub to: Option<Address>,
    /// Native value transferred.
    pub value: U256,
    /// Call data or initcode.
    pub input: Bytes,
    /// EIP-2930 access list.
    pub access_list: Vec<AccessListItem>,
    /// secp256k1 recovery parity.
    pub y_parity: bool,
    /// secp256k1 signature scalar r.
    pub r: U256,
    /// secp256k1 signature scalar s.
    pub s: U256,
}

/// Domain lifecycle state recorded by the root `ChainRegistry`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum DomainStatus {
    /// Creation transaction finalized; activation height not reached.
    Pending = 0,
    /// Domain accepts batches.
    Active = 1,
    /// Root governance temporarily rejects new batches.
    Frozen = 2,
}

/// Root-domain description of one logical domain.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DomainDescriptor {
    /// Domain identifier.
    pub domain_id: DomainId,
    /// Parent domain identifier.
    pub parent_domain_id: DomainId,
    /// Parent finalized head captured before creation execution.
    pub joint_domain_block_hash: B256,
    /// Root-domain transaction that requested creation.
    pub create_tx_hash: B256,
    /// Hash of canonical domain genesis data.
    pub origin_hash: B256,
    /// Normalized display name.
    pub name: String,
    /// Normalized display symbol.
    pub symbol: String,
    /// Globally unique EVM chain ID.
    pub evm_chain_id: u64,
    /// Current owner of mutable metadata.
    pub owner: Address,
    /// Versioned execution rules.
    pub protocol_revision: u32,
    /// Domain gas limit.
    pub gas_limit: u64,
    /// Initial EIP-1559 base fee.
    pub initial_base_fee: u128,
    /// Genesis native supply.
    pub initial_supply: U256,
    /// Root-domain anti-spam deposit.
    pub creation_deposit: U256,
    /// Lifecycle status.
    pub status: DomainStatus,
}

/// Immutable genesis preimage committed by `origin_hash`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DomainGenesis {
    /// Derived domain identifier.
    pub domain_id: DomainId,
    /// Parent domain identifier.
    pub parent_domain_id: DomainId,
    /// Parent finalized head captured before proposal execution.
    pub joint_domain_block_hash: B256,
    /// Root-domain creation transaction.
    pub create_tx_hash: B256,
    /// Normalized display name from the creation request.
    pub name: String,
    /// Normalized display symbol from the creation request.
    pub symbol: String,
    /// Globally unique EVM chain ID.
    pub evm_chain_id: u64,
    /// Initial owner.
    pub owner: Address,
    /// Initial protocol revision.
    pub protocol_revision: u32,
    /// Initial domain gas limit.
    pub gas_limit: u64,
    /// Initial EIP-1559 base fee.
    pub initial_base_fee: u128,
    /// Genesis native supply.
    pub initial_supply: U256,
    /// Authenticated state root after genesis allocation and system contracts.
    pub initial_state_root: B256,
}

/// Compressed SEC1 secp256k1 validator public key.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ConsensusPublicKey(pub [u8; 33]);

/// Canonical 64-byte secp256k1 ECDSA signature `(r, s)`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ConsensusSignature(pub [u8; 64]);

/// Validator and its power at a finalized root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Validator {
    /// Stable hash identifier.
    pub id: ValidatorId,
    /// Consensus-only public key.
    pub public_key: ConsensusPublicKey,
    /// Integer voting power.
    pub power: u64,
}

/// Finalized validator set for an epoch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatorSet {
    /// Epoch number.
    pub epoch: u64,
    /// Validators sorted by identifier.
    pub validators: Vec<Validator>,
}

/// Candidate-independent BFT phase discriminator.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct VotePhase(pub u8);

/// Vote intent that must be durable before its signature is released.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Vote {
    /// Network domain separation.
    pub network_id: NetworkId,
    /// Consensus height.
    pub height: ConsensusHeight,
    /// Round or view.
    pub round: ConsensusRound,
    /// Candidate adapter phase.
    pub phase: VotePhase,
    /// Proposed consensus-block hash.
    pub block_hash: B256,
    /// Signing validator.
    pub validator_id: ValidatorId,
}

/// Validator signature attached to a certificate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommitSignature {
    /// Signing validator.
    pub validator_id: ValidatorId,
    /// Signature over the exact vote preimage.
    pub signature: ConsensusSignature,
}

/// Weighted quorum certificate for one vote slot and block.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QuorumCertificate {
    /// Network domain separation.
    pub network_id: NetworkId,
    /// Consensus height.
    pub height: ConsensusHeight,
    /// Round or view.
    pub round: ConsensusRound,
    /// Certified phase.
    pub phase: VotePhase,
    /// Certified block hash.
    pub block_hash: B256,
    /// Signatures sorted by validator identifier.
    pub signatures: Vec<CommitSignature>,
}
