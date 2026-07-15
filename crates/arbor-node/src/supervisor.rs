//! Fail-fast task supervision.

use std::{future::Future, time::Duration};

use thiserror::Error;
use tokio::task::{JoinError, JoinSet};

use crate::{Shutdown, ShutdownTrigger};

type TaskResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

/// Supervises long-running node services and coordinates shutdown.
pub struct Supervisor {
    trigger: ShutdownTrigger,
    shutdown: Shutdown,
    tasks: JoinSet<(&'static str, TaskResult)>,
}

impl Supervisor {
    /// Creates an empty supervisor and its shutdown channel.
    #[must_use]
    pub fn new() -> Self {
        let (trigger, shutdown) = Shutdown::channel();
        Self {
            trigger,
            shutdown,
            tasks: JoinSet::new(),
        }
    }

    /// Returns a receiver for a service spawned elsewhere.
    #[must_use]
    pub fn shutdown_signal(&self) -> Shutdown {
        self.shutdown.clone()
    }

    /// Spawns a named critical service. Returning, even successfully, shuts
    /// down the process because critical services are expected to be durable.
    pub fn spawn<F, E>(&mut self, name: &'static str, task: F)
    where
        F: Future<Output = Result<(), E>> + Send + 'static,
        E: std::error::Error + Send + Sync + 'static,
    {
        self.tasks
            .spawn(async move { (name, task.await.map_err(Into::into)) });
    }

    /// Waits for Ctrl-C or the first critical task exit, then asks remaining
    /// tasks to stop and enforces the supplied grace period.
    ///
    /// # Errors
    ///
    /// Returns [`SupervisorError`] when signal handling fails, a critical task
    /// exits or panics, or shutdown exceeds the grace period.
    pub async fn run(mut self, grace: Duration) -> Result<(), SupervisorError> {
        let outcome = tokio::select! {
            signal = tokio::signal::ctrl_c() => {
                signal.map_err(SupervisorError::Signal)?;
                Ok(())
            }
            task = self.tasks.join_next(), if !self.tasks.is_empty() => {
                match task {
                    Some(Ok((name, Ok(())))) => Err(SupervisorError::UnexpectedExit { name }),
                    Some(Ok((name, Err(source)))) => Err(SupervisorError::Task { name, source }),
                    Some(Err(source)) => Err(SupervisorError::Join(source)),
                    None => Ok(()),
                }
            }
        };

        self.trigger.shutdown();
        if tokio::time::timeout(grace, async {
            while self.tasks.join_next().await.is_some() {}
        })
        .await
        .is_err()
        {
            self.tasks.abort_all();
            return Err(SupervisorError::ShutdownTimeout(grace));
        }
        outcome
    }
}

impl Default for Supervisor {
    fn default() -> Self {
        Self::new()
    }
}

/// Task-supervision failures.
#[derive(Debug, Error)]
pub enum SupervisorError {
    /// Installing or receiving the OS signal failed.
    #[error("failed to receive shutdown signal: {0}")]
    Signal(std::io::Error),
    /// A critical service returned successfully when it should remain alive.
    #[error("critical task {name} exited unexpectedly")]
    UnexpectedExit {
        /// Static service name.
        name: &'static str,
    },
    /// A critical service returned an error.
    #[error("critical task {name} failed: {source}")]
    Task {
        /// Static service name.
        name: &'static str,
        /// Service-specific typed error erased only at the assembly boundary.
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// A task panicked or was cancelled.
    #[error("critical task join failed: {0}")]
    Join(JoinError),
    /// Services did not stop before the grace period elapsed.
    #[error("tasks did not stop within {0:?}")]
    ShutdownTimeout(Duration),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, thiserror::Error)]
    #[error("fixture failure")]
    struct FixtureError;

    #[tokio::test]
    async fn task_failure_cancels_siblings() {
        let mut supervisor = Supervisor::new();
        let mut shutdown = supervisor.shutdown_signal();
        supervisor.spawn("waiter", async move {
            shutdown.cancelled().await;
            Ok::<_, FixtureError>(())
        });
        supervisor.spawn("failure", async { Err::<(), _>(FixtureError) });

        let error = supervisor.run(Duration::from_secs(1)).await.unwrap_err();
        assert!(matches!(
            error,
            SupervisorError::Task {
                name: "failure",
                ..
            }
        ));
    }
}
