//! Stable top-level error classification.

use thiserror::Error;

/// Coarse class suitable for CLI exit handling, metrics, and RPC translation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorClass {
    /// Invalid operator or user input.
    Configuration,
    /// A local disk or persistence failure.
    Storage,
    /// A supervised service failed.
    Service,
    /// An orderly shutdown was requested.
    Shutdown,
}

/// Node assembly errors. Protocol crates keep their typed domain errors and
/// map them only at this outer boundary.
#[derive(Debug, Error)]
pub enum NodeError {
    /// Configuration could not be loaded or validated.
    #[error(transparent)]
    Configuration(#[from] crate::ConfigError),
    /// A supervised task failed.
    #[error(transparent)]
    Supervisor(#[from] crate::SupervisorError),
}

impl NodeError {
    /// Returns the stable operational class for this error.
    #[must_use]
    pub const fn class(&self) -> ErrorClass {
        match self {
            Self::Configuration(_) => ErrorClass::Configuration,
            Self::Supervisor(_) => ErrorClass::Service,
        }
    }
}
