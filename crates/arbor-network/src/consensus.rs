use libp2p::PeerId;
use thiserror::Error;
use tokio::sync::mpsc;

use crate::protocol::MAX_CONSENSUS_MESSAGE_BYTES;

/// One authenticated targeted consensus-adapter message.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConsensusDirectMessage {
    /// Authenticated transport peer.
    pub peer: PeerId,
    /// Adapter-owned non-zero discriminator.
    pub kind: u8,
    /// Opaque bounded bytes; M8 must additionally authenticate validator authority/signatures.
    pub payload: Vec<u8>,
}

/// Bounded non-blocking ingress used by the network task to isolate consensus work.
#[derive(Clone)]
pub struct ConsensusDirectSender {
    sender: mpsc::Sender<ConsensusDirectMessage>,
}

/// Receiver owned by the selected consensus adapter.
pub struct ConsensusDirectReceiver {
    receiver: mpsc::Receiver<ConsensusDirectMessage>,
}

/// Creates one bounded consensus direct-protocol mailbox.
///
/// # Errors
///
/// Rejects a zero queue capacity.
pub fn consensus_direct_mailbox(
    capacity: usize,
) -> Result<(ConsensusDirectSender, ConsensusDirectReceiver), ConsensusMailboxError> {
    if capacity == 0 {
        return Err(ConsensusMailboxError::Capacity);
    }
    let (sender, receiver) = mpsc::channel(capacity);
    Ok((
        ConsensusDirectSender { sender },
        ConsensusDirectReceiver { receiver },
    ))
}

impl ConsensusDirectSender {
    /// Delivers one message without waiting on a slow consensus consumer.
    ///
    /// # Errors
    ///
    /// Returns `Full` for backpressure and `Closed` after adapter shutdown.
    pub fn try_send(&self, message: ConsensusDirectMessage) -> Result<(), ConsensusMailboxError> {
        if message.kind == 0 || message.payload.len() > MAX_CONSENSUS_MESSAGE_BYTES {
            return Err(ConsensusMailboxError::Message);
        }
        self.sender.try_send(message).map_err(|error| match error {
            mpsc::error::TrySendError::Full(_) => ConsensusMailboxError::Full,
            mpsc::error::TrySendError::Closed(_) => ConsensusMailboxError::Closed,
        })
    }
}

impl ConsensusDirectReceiver {
    /// Waits for the next bounded consensus message.
    pub async fn recv(&mut self) -> Option<ConsensusDirectMessage> {
        self.receiver.recv().await
    }

    /// Receives immediately when a message is already queued.
    ///
    /// # Errors
    ///
    /// Returns `Empty` when no message is ready or `Disconnected` after sender shutdown.
    pub fn try_recv(&mut self) -> Result<ConsensusDirectMessage, mpsc::error::TryRecvError> {
        self.receiver.try_recv()
    }
}

/// Consensus direct-mailbox failure.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum ConsensusMailboxError {
    /// Queue capacity must be non-zero.
    #[error("consensus direct queue capacity must be non-zero")]
    Capacity,
    /// Message kind or payload violates the protocol bound.
    #[error("invalid consensus direct message")]
    Message,
    /// Bounded queue is full; caller must apply peer backpressure.
    #[error("consensus direct queue is full")]
    Full,
    /// Consensus adapter has shut down.
    #[error("consensus direct adapter is closed")]
    Closed,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bounded_mailbox_never_waits_for_a_slow_consumer() {
        let (sender, mut receiver) = consensus_direct_mailbox(1).unwrap();
        let peer = PeerId::random();
        let message = ConsensusDirectMessage {
            peer,
            kind: 1,
            payload: vec![1],
        };
        sender.try_send(message.clone()).unwrap();
        assert_eq!(
            sender.try_send(message.clone()),
            Err(ConsensusMailboxError::Full)
        );
        assert_eq!(receiver.recv().await, Some(message));
    }
}
