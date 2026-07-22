use alloy_primitives::{Address, B256};
use revm::primitives::hardfork::SpecId;

use crate::EvmError;

/// EVM hardfork fixed by an Arbor protocol revision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum EvmRevision {
    /// Ethereum Shanghai rules, including EIP-3855 and EIP-3860.
    Shanghai = 1,
}

impl EvmRevision {
    pub(crate) const fn spec_id(self) -> SpecId {
        match self {
            Self::Shanghai => SpecId::SHANGHAI,
        }
    }
}

/// Consensus-sensitive execution configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProtocolSpec {
    /// Arbor protocol revision selected by the domain descriptor.
    pub protocol_revision: u32,
    /// Fixed Ethereum hardfork rules.
    pub evm_revision: EvmRevision,
    /// Maximum deployed bytecode length.
    pub max_code_bytes: usize,
    /// Maximum initcode length.
    pub max_initcode_bytes: usize,
    /// Maximum logs emitted by one transaction.
    pub max_logs_per_transaction: usize,
}

impl ProtocolSpec {
    /// First production execution rule set.
    pub const V1: Self = Self {
        protocol_revision: 1,
        evm_revision: EvmRevision::Shanghai,
        max_code_bytes: 24_576,
        max_initcode_bytes: 49_152,
        max_logs_per_transaction: 1_024,
    };

    /// Resolves an exact supported revision; unknown revisions stop execution.
    ///
    /// # Errors
    ///
    /// Returns [`EvmError::UnsupportedProtocolRevision`] for unknown revisions.
    pub const fn resolve(protocol_revision: u32) -> Result<Self, EvmError> {
        match protocol_revision {
            1 => Ok(Self::V1),
            other => Err(EvmError::UnsupportedProtocolRevision(other)),
        }
    }
}

/// Explicit domain block environment; no host clock or local setting is consulted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DomainEnv {
    /// Unique target EVM chain ID.
    pub chain_id: u64,
    /// Domain-local logical block number.
    pub block_number: u64,
    /// Root-consensus timestamp.
    pub timestamp: u64,
    /// Domain proposer reward address.
    pub beneficiary: Address,
    /// Domain gas limit.
    pub gas_limit: u64,
    /// EIP-1559 base fee.
    pub base_fee_per_gas: u128,
    /// Consensus-provided randomness commitment used by `PREVRANDAO`.
    pub prevrandao: B256,
}

impl DomainEnv {
    pub(crate) fn validate(self) -> Result<(), EvmError> {
        if self.chain_id == 0 {
            return Err(EvmError::InvalidEnvironment("chain ID must be non-zero"));
        }
        if self.gas_limit == 0 || self.gas_limit > 30_000_000 {
            return Err(EvmError::InvalidEnvironment(
                "domain gas limit must be in 1..=30,000,000",
            ));
        }
        if self.base_fee_per_gas > u128::from(u64::MAX) {
            return Err(EvmError::InvalidEnvironment(
                "base fee exceeds the fixed revm block field",
            ));
        }
        Ok(())
    }
}
