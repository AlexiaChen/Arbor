//! Cloneable graceful-shutdown signal.

use tokio::sync::watch;

/// Receiver held by supervised tasks.
#[derive(Clone, Debug)]
pub struct Shutdown {
    receiver: watch::Receiver<bool>,
}

/// Sender held by process supervision.
#[derive(Clone, Debug)]
pub struct ShutdownTrigger {
    sender: watch::Sender<bool>,
}

impl Shutdown {
    /// Creates a new graceful-shutdown signal pair.
    #[must_use]
    pub fn channel() -> (ShutdownTrigger, Self) {
        let (sender, receiver) = watch::channel(false);
        (ShutdownTrigger { sender }, Self { receiver })
    }

    /// Resolves once shutdown is requested or all triggers are dropped.
    pub async fn cancelled(&mut self) {
        if *self.receiver.borrow() {
            return;
        }
        let _ = self.receiver.changed().await;
    }

    /// Returns whether shutdown was already requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        *self.receiver.borrow()
    }
}

impl ShutdownTrigger {
    /// Requests orderly termination. Repeated calls are idempotent.
    pub fn shutdown(&self) {
        self.sender.send_replace(true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn signal_is_observed() {
        let (trigger, mut shutdown) = Shutdown::channel();
        trigger.shutdown();
        shutdown.cancelled().await;
        assert!(shutdown.is_cancelled());
    }
}
