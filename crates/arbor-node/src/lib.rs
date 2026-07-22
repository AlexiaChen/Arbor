//! Node lifecycle, configuration, task supervision, and graceful shutdown.
//!
//! This crate assembles services. Consensus and execution rules belong in
//! their dedicated crates and must not be implemented here.

#![forbid(unsafe_code)]

mod config;
mod error;
mod shutdown;
mod supervisor;

pub use config::{Config, ConfigError, NetworkConfig, NodeConfig};
pub use error::{ErrorClass, NodeError};
pub use shutdown::{Shutdown, ShutdownTrigger};
pub use supervisor::{Supervisor, SupervisorError};

use std::{path::Path, time::Duration};

use alloy_primitives::keccak256;
use arbor_consensus::{ConsensusError, DevGenesis, EngineMode, SingleValidatorEngine};
use arbor_mempool::MempoolConfig;
use arbor_primitives::NetworkId;
use arbor_storage::{Database, DatabaseIdentity, RetentionPolicy, StorageError};
use thiserror::Error;
use tracing_subscriber::EnvFilter;

/// Genesis-bound identity of the local development network.
#[must_use]
pub fn dev_database_identity() -> DatabaseIdentity {
    DatabaseIdentity {
        network_id: NetworkId(keccak256(b"ARBOR_DEV_NETWORK_V1")),
        genesis_hash: keccak256(b"ARBOR_DEV_GENESIS_V1"),
    }
}

/// Opens the node database with protocol-required synchronous parity-db durability.
///
/// # Errors
///
/// Returns [`StorageError`] for schema, identity, corruption, or filesystem failures.
pub fn open_database(data_dir: &Path) -> Result<Database, StorageError> {
    Database::open(
        data_dir.join("db"),
        dev_database_identity(),
        RetentionPolicy::Archive,
    )
}

/// Initializes deterministic height-zero development application state.
///
/// Reopening an already initialized matching database is idempotent.
///
/// # Errors
///
/// Returns [`DevNodeError`] for genesis, storage, or replay inconsistency.
pub fn initialize_dev_chain(data_dir: &Path) -> Result<(), DevNodeError> {
    let identity = dev_database_identity();
    let genesis = DevGenesis::local_default(identity.network_id, identity.genesis_hash)?;
    let database = open_database(data_dir)?;
    SingleValidatorEngine::open(
        EngineMode::DevValidator,
        database,
        genesis,
        MempoolConfig::default(),
    )?;
    Ok(())
}

/// Runs the M5 immediate-finality development engine until graceful shutdown.
///
/// Proposal timestamps advance from finalized protocol state; the interval controls production
/// cadence only and is not an execution input.
///
/// # Errors
///
/// Returns [`DevNodeError`] when database open/replay or block production fails.
pub async fn run_dev_validator(
    data_dir: &Path,
    mut shutdown: Shutdown,
) -> Result<(), DevNodeError> {
    let identity = dev_database_identity();
    let genesis = DevGenesis::local_default(identity.network_id, identity.genesis_hash)?;
    let database = open_database(data_dir)?;
    let mut engine = SingleValidatorEngine::open(
        EngineMode::DevValidator,
        database,
        genesis,
        MempoolConfig::default(),
    )?;
    let mut interval = tokio::time::interval(Duration::from_millis(250));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            () = shutdown.cancelled() => return Ok(()),
            _instant = interval.tick() => {
                let timestamp = engine.finalized_state().timestamp.checked_add(1)
                    .ok_or(DevNodeError::TimestampOverflow)?;
                let event = engine.produce_block(timestamp)?;
                tracing::info!(
                    height = event.height,
                    consensus_hash = %event.consensus_hash,
                    domain_heads_root = %event.domain_heads_root,
                    "finalized development block"
                );
            }
        }
    }
}

/// Development node assembly failure.
#[derive(Debug, Error)]
pub enum DevNodeError {
    /// Consensus application or recovery failed.
    #[error(transparent)]
    Consensus(#[from] ConsensusError),
    /// Database open failed before consensus assembly.
    #[error(transparent)]
    Storage(#[from] StorageError),
    /// Consensus timestamp space is exhausted.
    #[error("development consensus timestamp overflow")]
    TimestampOverflow,
}

/// Installs process-wide structured logging.
///
/// Calling this more than once is harmless, which keeps tests and embedded
/// callers from racing over the global subscriber.
pub fn init_tracing(default_filter: &str) {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_filter));
    let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
}
