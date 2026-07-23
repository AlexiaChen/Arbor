//! Node lifecycle, configuration, task supervision, and graceful shutdown.
//!
//! This crate assembles services. Consensus and execution rules belong in
//! their dedicated crates and must not be implemented here.

#![forbid(unsafe_code)]

mod config;
mod error;
mod networked;
mod shutdown;
mod supervisor;

pub use config::{Config, ConfigError, HistorySubscription, NetworkConfig, NodeConfig};
pub use error::{ErrorClass, NodeError};
pub use shutdown::{Shutdown, ShutdownTrigger};
pub use supervisor::{Supervisor, SupervisorError};

use std::path::Path;

use alloy_primitives::keccak256;
use arbor_consensus::{
    ConsensusError, DevGenesis, DomainHistoryRetention, EngineMode, SingleValidatorEngine,
};
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
    open_dev_engine(data_dir, &HistorySubscription::All)?;
    arbor_network::load_or_create_peer_identity(data_dir.join("network/peer.key"))
        .map_err(|error| DevNodeError::Network(error.to_string()))?;
    Ok(())
}

/// Opens or replays the deterministic development engine with local history settings.
///
/// The setting controls only derived receipt and transaction-location persistence. Every domain
/// is executed and its latest authenticated state remains available for consensus validation.
///
/// # Errors
///
/// Returns [`DevNodeError`] for genesis, storage, or replay inconsistency.
pub fn open_dev_engine(
    data_dir: &Path,
    history: &HistorySubscription,
) -> Result<SingleValidatorEngine, DevNodeError> {
    let identity = dev_database_identity();
    let genesis = DevGenesis::local_default(identity.network_id, identity.genesis_hash)?;
    let history_retention = history.selected_domains(genesis.root_domain_id).map_or(
        DomainHistoryRetention::All,
        DomainHistoryRetention::Selected,
    );
    let database = open_database(data_dir)?;
    Ok(SingleValidatorEngine::open_with_history(
        EngineMode::DevValidator,
        database,
        genesis,
        MempoolConfig::default(),
        history_retention,
    )?)
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
    history: HistorySubscription,
    network: NetworkConfig,
    mut shutdown: Shutdown,
) -> Result<(), DevNodeError> {
    networked::run_dev_node(data_dir, history, network, true, &mut shutdown).await
}

/// Runs a development full listener that only imports finalized snapshot/block sync.
///
/// # Errors
///
/// Returns [`DevNodeError`] for database, network, snapshot, or block-import failure.
pub async fn run_dev_listener(
    data_dir: &Path,
    history: HistorySubscription,
    network: NetworkConfig,
    mut shutdown: Shutdown,
) -> Result<(), DevNodeError> {
    networked::run_dev_node(data_dir, history, network, false, &mut shutdown).await
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
    /// P2P assembly, identity, synchronization, or protocol handling failed.
    #[error("development network failure: {0}")]
    Network(String),
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
