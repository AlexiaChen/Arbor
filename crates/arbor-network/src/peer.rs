use std::collections::BTreeMap;

use libp2p::PeerId;

/// Score at or below which a peer is locally banned.
const BAN_THRESHOLD: i32 = -100;
/// Score at or below which requests are temporarily ignored.
const THROTTLE_THRESHOLD: i32 = -40;
/// Saturation bound preventing unbounded score growth.
const SCORE_LIMIT: i32 = 200;

/// Local-only peer score. It never changes protocol validity.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PeerScore(i32);

impl PeerScore {
    /// Returns the signed score for metrics and operator diagnostics.
    #[must_use]
    pub const fn value(self) -> i32 {
        self.0
    }

    fn reward(&mut self, amount: i32) {
        self.0 = self.0.saturating_add(amount).min(SCORE_LIMIT);
    }

    fn penalize(&mut self, amount: i32) {
        self.0 = self.0.saturating_sub(amount).max(-SCORE_LIMIT);
    }

    /// Returns the local connection/request policy derived from this score.
    #[must_use]
    pub const fn disposition(self) -> PeerDisposition {
        if self.0 <= BAN_THRESHOLD {
            PeerDisposition::Banned
        } else if self.0 <= THROTTLE_THRESHOLD {
            PeerDisposition::Throttled
        } else {
            PeerDisposition::Accepted
        }
    }
}

/// Local-only peer handling policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PeerDisposition {
    /// Normal request and gossip handling.
    Accepted,
    /// Keep the connection but do not accept expensive new work.
    Throttled,
    /// Close/ignore the peer until local policy is reset.
    Banned,
}

/// Bounded local peer-policy registry.
#[derive(Clone, Debug, Default)]
pub struct PeerBook {
    scores: BTreeMap<PeerId, PeerScore>,
}

impl PeerBook {
    /// Returns a peer's current score.
    #[must_use]
    pub fn score(&self, peer: &PeerId) -> PeerScore {
        self.scores.get(peer).copied().unwrap_or_default()
    }

    /// Rewards a valid completed exchange.
    pub fn reward(&mut self, peer: PeerId) {
        self.scores.entry(peer).or_default().reward(2);
    }

    /// Penalizes a timeout or disconnect during an exchange.
    pub fn timeout(&mut self, peer: PeerId) {
        self.scores.entry(peer).or_default().penalize(10);
    }

    /// Penalizes malformed but bounded input.
    pub fn malformed(&mut self, peer: PeerId) {
        self.scores.entry(peer).or_default().penalize(25);
    }

    /// Immediately reaches the local ban threshold for a genesis/identity mismatch.
    pub fn identity_mismatch(&mut self, peer: PeerId) {
        self.scores
            .entry(peer)
            .or_default()
            .penalize(-BAN_THRESHOLD);
    }

    /// Returns the handling policy for a peer.
    #[must_use]
    pub fn disposition(&self, peer: &PeerId) -> PeerDisposition {
        self.score(peer).disposition()
    }
}

#[cfg(test)]
mod tests {
    use libp2p::identity::Keypair;

    use super::*;

    #[test]
    fn penalties_throttle_then_ban_without_unbounded_scores() {
        let peer = Keypair::generate_ed25519().public().to_peer_id();
        let mut book = PeerBook::default();
        for _ in 0..4 {
            book.timeout(peer);
        }
        assert_eq!(book.disposition(&peer), PeerDisposition::Throttled);
        book.identity_mismatch(peer);
        assert_eq!(book.disposition(&peer), PeerDisposition::Banned);
        for _ in 0..1_000 {
            book.reward(peer);
        }
        assert_eq!(book.score(&peer).value(), SCORE_LIMIT);
    }
}
