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

use tracing_subscriber::EnvFilter;

/// Installs process-wide structured logging.
///
/// Calling this more than once is harmless, which keeps tests and embedded
/// callers from racing over the global subscriber.
pub fn init_tracing(default_filter: &str) {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_filter));
    let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
}
