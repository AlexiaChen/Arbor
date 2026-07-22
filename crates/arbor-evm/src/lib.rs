//! Versioned `revm` adapter and authenticated execution state.
//!
//! Protocol revision one fixes Shanghai EVM rules. Dependency upgrades cannot
//! change that mapping without changing this crate and the committed vectors.

#![forbid(unsafe_code)]

mod execute;
mod spec;
mod state;

pub use execute::{EvmExecution, execute_transaction, execute_transaction_with_system};
pub use spec::{DomainEnv, EvmRevision, ProtocolSpec};
pub use state::{ExecutionState, GenesisAccount};

use thiserror::Error;

/// Deterministic EVM adapter failure.
#[derive(Debug, Error)]
pub enum EvmError {
    /// The protocol revision is unknown to this binary.
    #[error("unsupported protocol revision {0}")]
    UnsupportedProtocolRevision(u32),
    /// Explicit block/domain input violates protocol limits.
    #[error("invalid domain environment: {0}")]
    InvalidEnvironment(&'static str),
    /// Authenticated account or storage state is malformed or incomplete.
    #[error("authenticated execution state failure: {0}")]
    State(String),
    /// A signed transaction is invalid at the current state/block.
    #[error("invalid transaction: {0}")]
    InvalidTransaction(String),
    /// The EVM rejected a block header field.
    #[error("invalid EVM block environment: {0}")]
    InvalidHeader(String),
    /// A native system-contract failure aborted execution.
    #[error("native system-contract failure: {0}")]
    System(String),
}

impl From<arbor_state::StateError> for EvmError {
    fn from(error: arbor_state::StateError) -> Self {
        Self::State(error.to_string())
    }
}
