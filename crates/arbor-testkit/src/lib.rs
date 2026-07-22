//! Temporary filesystem, networking, process, and timeout helpers for tests.

#![forbid(unsafe_code)]

use std::{
    future::Future,
    net::{SocketAddr, TcpListener},
    process::Child,
    time::Duration,
};

use tempfile::TempDir;
use thiserror::Error;

/// Owns a temporary node data directory and removes it on drop.
pub struct TestDir(TempDir);

impl TestDir {
    /// Creates an isolated temporary data directory.
    ///
    /// # Errors
    ///
    /// Returns the underlying IO error when the directory cannot be created.
    pub fn new() -> Result<Self, std::io::Error> {
        tempfile::tempdir().map(Self)
    }

    /// Returns the directory path.
    #[must_use]
    pub fn path(&self) -> &std::path::Path {
        self.0.path()
    }
}

/// Asks the OS to reserve an available loopback port.
///
/// The listener stays open so parallel tests cannot claim the same port. Drop
/// the reservation immediately before handing its address to the service.
///
/// # Errors
///
/// Returns the underlying IO error when loopback bind or address lookup fails.
pub fn reserve_loopback_port() -> Result<(SocketAddr, TcpListener), std::io::Error> {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
    let address = listener.local_addr()?;
    Ok((address, listener))
}

/// Child process that is terminated and reaped when a test exits early.
pub struct ProcessGuard {
    child: Option<Child>,
}

impl ProcessGuard {
    /// Takes ownership of a spawned child.
    #[must_use]
    pub const fn new(child: Child) -> Self {
        Self { child: Some(child) }
    }

    /// Waits for normal completion and disarms drop cleanup.
    ///
    /// # Errors
    ///
    /// Returns an IO error if waiting for the process fails or this guard was
    /// already internally disarmed.
    pub fn wait(mut self) -> Result<std::process::ExitStatus, std::io::Error> {
        let Some(mut child) = self.child.take() else {
            return Err(std::io::Error::other("process guard was already disarmed"));
        };
        child.wait()
    }

    /// Terminates a still-running child, waits for it, and disarms drop cleanup.
    ///
    /// If the child already exited, this only reaps and returns its status.
    ///
    /// # Errors
    ///
    /// Returns an IO error if status inspection, termination, or waiting fails.
    pub fn kill_and_wait(mut self) -> Result<std::process::ExitStatus, std::io::Error> {
        let Some(mut child) = self.child.take() else {
            return Err(std::io::Error::other("process guard was already disarmed"));
        };
        if child.try_wait()?.is_none() {
            child.kill()?;
        }
        child.wait()
    }
}

impl Drop for ProcessGuard {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Runs a future with a mandatory deadline.
///
/// # Errors
///
/// Returns [`TestkitError::Timeout`] when the future does not finish before the
/// deadline.
pub async fn with_timeout<F>(duration: Duration, future: F) -> Result<F::Output, TestkitError>
where
    F: Future,
{
    tokio::time::timeout(duration, future)
        .await
        .map_err(|_| TestkitError::Timeout(duration))
}

/// Test helper failures.
#[derive(Debug, Error)]
pub enum TestkitError {
    /// The fixture exceeded its explicit deadline.
    #[error("fixture exceeded timeout of {0:?}")]
    Timeout(Duration),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temporary_directory_exists() {
        let directory = TestDir::new().unwrap();
        assert!(directory.path().is_dir());
    }

    #[test]
    fn port_reservation_is_loopback() {
        let (address, _reservation) = reserve_loopback_port().unwrap();
        assert!(address.ip().is_loopback());
        assert_ne!(address.port(), 0);
    }

    #[tokio::test]
    async fn timeout_is_typed() {
        let error = with_timeout(Duration::from_millis(1), std::future::pending::<()>())
            .await
            .unwrap_err();
        assert!(matches!(error, TestkitError::Timeout(_)));
    }
}
