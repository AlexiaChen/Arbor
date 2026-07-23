//! Bounded rust-libp2p transport, Arbor handshakes, gossip, direct protocols, and sync policy.
//!
//! Peer identities authenticate transport connections but are intentionally separate from
//! validator consensus identities. Production consensus messages still require the M8 validator
//! signature checks at the consensus adapter boundary.

#![forbid(unsafe_code)]

mod consensus;
mod identity;
mod mdns;
mod peer;
mod protocol;
mod service;
mod snapshot;
mod sync;

pub use consensus::{
    ConsensusDirectMessage, ConsensusDirectReceiver, ConsensusDirectSender, ConsensusMailboxError,
    consensus_direct_mailbox,
};
pub use identity::{
    PeerIdentityError, PersistentPeer, PersistentPeerError, load_or_create_peer_identity,
};
pub use peer::{PeerBook, PeerDisposition, PeerScore};
pub use protocol::{
    BlockBody, Capability, DirectRequest, DirectResponse, FinalizedAnnouncement, FinalizedBlock,
    FinalizedHeader, GossipMessage, Handshake, HandshakeError, MAX_BLOCKS_PER_RESPONSE,
    MAX_DIRECT_REQUEST_BYTES, MAX_DIRECT_RESPONSE_BYTES, MAX_HEADERS_PER_RESPONSE,
    MAX_HISTORY_ITEMS, MAX_SNAPSHOT_CHUNK_BYTES, NodeRole, RequestEnvelope, ResponseEnvelope,
    SyncStatus, WireHash,
};
pub use service::{
    InboundRequest, NetworkError, NetworkEvent, NetworkMetrics, NetworkService,
    NetworkServiceConfig,
};
pub use snapshot::{
    CheckpointFinalityVerifier, CheckpointManifest, DevelopmentCheckpointVerifier, SnapshotBundle,
    SnapshotError, SnapshotImport, SnapshotStaging,
};
pub use sync::{
    BlockImport, BlockSync, BlockSyncError, BlockSyncReport, DevelopmentHeaderVerifier,
    HeaderFinalityVerifier, HeaderSync, HeaderSyncError, VerifiedHeader,
    finalized_blocks_from_bodies,
};
