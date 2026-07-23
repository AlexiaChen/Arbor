use alloy_primitives::B256;
use arbor_chain::MAX_CONSENSUS_BLOCK_BYTES;
use arbor_codec::MAX_TRANSACTION_ENVELOPE_BYTES;
use arbor_primitives::{CANONICAL_CODEC_VERSION, DomainId, NetworkId, PROTOCOL_VERSION};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Arbor direct protocol version negotiated over libp2p.
pub const DIRECT_PROTOCOL_VERSION: u16 = 1;
/// Hard cap for a direct request before decoding.
pub const MAX_DIRECT_REQUEST_BYTES: u64 = 2 * 1024 * 1024;
/// Hard cap for a direct response before decoding.
pub const MAX_DIRECT_RESPONSE_BYTES: u64 = (MAX_CONSENSUS_BLOCK_BYTES as u64) + 2 * 1024 * 1024;
/// Hard cap for one state snapshot chunk.
pub const MAX_SNAPSHOT_CHUNK_BYTES: usize = 8 * 1024 * 1024;
/// Maximum complete block body requested in one bounded exchange.
///
/// A canonical block may be 16 MiB, so body batching would exceed the 18 MiB direct-frame cap
/// and let a valid request force large pre-encoding allocations. Header sync remains iterative.
pub const MAX_BLOCKS_PER_RESPONSE: u16 = 1;
/// Maximum canonical header/finality pairs in one response.
pub const MAX_HEADERS_PER_RESPONSE: u16 = 64;
/// Maximum history records requested in one bounded exchange.
pub const MAX_HISTORY_ITEMS: u16 = 1024;
/// Maximum direct consensus payload accepted by the transport adapter.
pub const MAX_CONSENSUS_MESSAGE_BYTES: usize = 1024 * 1024;

/// Fixed wire representation of a protocol hash.
pub type WireHash = [u8; 32];

/// Node role advertised during the Arbor application handshake.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum NodeRole {
    /// Executes every active domain and may propose in explicit development mode.
    Validator,
    /// Executes and verifies every active domain without signing consensus votes.
    Full,
    /// Verifies checkpoint/finality data without claiming execution validation.
    Light,
}

/// Independently negotiable Arbor network capability.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum Capability {
    /// Transaction gossip.
    TransactionGossip,
    /// Finalized-height announcements.
    FinalizedGossip,
    /// Header and finalized block synchronization.
    BlockSync,
    /// Authenticated state snapshot synchronization.
    StateSync,
    /// Node-local domain history synchronization.
    DomainHistory,
    /// Targeted consensus adapter messages.
    ConsensusDirect,
}

/// Genesis-bound Arbor application handshake carried by every direct exchange.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Handshake {
    /// Network identifier from the local database identity.
    pub network_id: WireHash,
    /// Genesis hash from the local database identity.
    pub genesis_hash: WireHash,
    /// Arbor protocol version.
    pub protocol_version: u32,
    /// Canonical codec version.
    pub codec_version: u8,
    /// Direct transport protocol version.
    pub direct_protocol_version: u16,
    /// Node role; it is descriptive and grants no consensus authority.
    pub role: NodeRole,
    /// Strictly sorted unique capabilities.
    pub capabilities: Vec<Capability>,
    /// Largest direct frame this peer is willing to receive.
    pub max_frame_bytes: u32,
}

impl Handshake {
    /// Builds a canonical local handshake.
    #[must_use]
    pub fn new(
        network_id: NetworkId,
        genesis_hash: B256,
        role: NodeRole,
        mut capabilities: Vec<Capability>,
    ) -> Self {
        capabilities.sort_unstable();
        capabilities.dedup();
        Self {
            network_id: network_id.0.into(),
            genesis_hash: genesis_hash.into(),
            protocol_version: PROTOCOL_VERSION,
            codec_version: CANONICAL_CODEC_VERSION,
            direct_protocol_version: DIRECT_PROTOCOL_VERSION,
            role,
            capabilities,
            max_frame_bytes: u32::try_from(MAX_DIRECT_RESPONSE_BYTES).unwrap_or(u32::MAX),
        }
    }

    /// Validates a remote handshake against the local genesis and supported versions.
    ///
    /// # Errors
    ///
    /// Returns a typed mismatch or malformed-capability error.
    pub fn validate_against(&self, local: &Self) -> Result<(), HandshakeError> {
        if self.network_id != local.network_id {
            return Err(HandshakeError::Network);
        }
        if self.genesis_hash != local.genesis_hash {
            return Err(HandshakeError::Genesis);
        }
        if self.protocol_version != local.protocol_version {
            return Err(HandshakeError::ProtocolVersion);
        }
        if self.codec_version != local.codec_version {
            return Err(HandshakeError::CodecVersion);
        }
        if self.direct_protocol_version != local.direct_protocol_version {
            return Err(HandshakeError::DirectProtocolVersion);
        }
        if self.max_frame_bytes == 0 || u64::from(self.max_frame_bytes) > MAX_DIRECT_RESPONSE_BYTES
        {
            return Err(HandshakeError::FrameLimit);
        }
        if self.capabilities.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(HandshakeError::Capabilities);
        }
        Ok(())
    }

    /// Returns whether the peer advertised one capability.
    #[must_use]
    pub fn supports(&self, capability: Capability) -> bool {
        self.capabilities.binary_search(&capability).is_ok()
    }
}

/// Application-handshake rejection.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum HandshakeError {
    /// Network ID differs.
    #[error("network id mismatch")]
    Network,
    /// Genesis hash differs.
    #[error("genesis hash mismatch")]
    Genesis,
    /// Arbor protocol version differs.
    #[error("protocol version mismatch")]
    ProtocolVersion,
    /// Canonical codec version differs.
    #[error("codec version mismatch")]
    CodecVersion,
    /// Direct protocol version differs.
    #[error("direct protocol version mismatch")]
    DirectProtocolVersion,
    /// Advertised frame limit is zero or exceeds the accepted transport budget.
    #[error("invalid direct frame limit")]
    FrameLimit,
    /// Capabilities are not strictly sorted and unique.
    #[error("capabilities are not canonical")]
    Capabilities,
}

/// Current finalized status exchanged before selecting a sync path.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SyncStatus {
    /// Latest finalized root-consensus height.
    pub height: u64,
    /// Latest finalized root-consensus hash.
    pub consensus_hash: WireHash,
    /// Sparse commitment to every active domain head.
    pub domain_heads_root: WireHash,
    /// Latest checkpoint snapshot currently available from this peer, or zero.
    pub checkpoint_height: u64,
}

/// One finalized block body plus an M8-reserved finality proof.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FinalizedBlock {
    /// Claimed finalized height, checked against the decoded header.
    pub height: u64,
    /// Canonically encoded complete consensus block.
    pub block: Vec<u8>,
    /// Opaque proof bytes. Empty is accepted only by an explicit development verifier.
    pub finality_proof: Vec<u8>,
}

/// One finalized consensus header plus the consensus-adapter proof that authenticates it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FinalizedHeader {
    /// Claimed consensus height.
    pub height: u64,
    /// Canonically encoded `ConsensusBlockHeader`.
    pub header: Vec<u8>,
    /// Opaque bounded proof bytes.
    pub finality_proof: Vec<u8>,
}

/// Complete canonical block body fetched only after its header was verified.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BlockBody {
    /// Exact verified consensus height.
    pub height: u64,
    /// Canonically encoded complete `ConsensusBlock`.
    pub block: Vec<u8>,
}

impl FinalizedBlock {
    /// Applies transport-level limits without claiming finality or execution validity.
    ///
    /// # Errors
    ///
    /// Returns a static reason for oversized or empty input.
    pub fn validate_transport(&self) -> Result<(), &'static str> {
        if self.block.is_empty() || self.block.len() > MAX_CONSENSUS_BLOCK_BYTES {
            return Err("invalid finalized block bytes");
        }
        if self.finality_proof.len() > MAX_CONSENSUS_MESSAGE_BYTES {
            return Err("finality proof exceeds limit");
        }
        Ok(())
    }
}

/// Request carried over the authenticated direct request-response protocol.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum DirectRequest {
    /// Establish the genesis-bound application handshake.
    Handshake,
    /// Query the latest finalized status.
    Status,
    /// Request sequential finalized headers and finality proofs.
    Headers {
        /// First requested height.
        start_height: u64,
        /// Maximum header count.
        limit: u16,
    },
    /// Request complete bodies for strictly increasing verified heights.
    BlockBodies {
        /// Heights whose headers were already verified.
        heights: Vec<u64>,
    },
    /// Request sequential finalized blocks starting at `start_height`.
    Blocks {
        /// First requested height.
        start_height: u64,
        /// Maximum block count.
        limit: u16,
    },
    /// Request the checkpoint snapshot manifest.
    SnapshotManifest {
        /// Finalized checkpoint height.
        checkpoint_height: u64,
    },
    /// Request one content-addressed snapshot chunk.
    SnapshotChunk {
        /// Finalized checkpoint height.
        checkpoint_height: u64,
        /// Domain whose authenticated state is requested.
        domain_id: WireHash,
        /// Contiguous chunk index.
        index: u32,
    },
    /// Request node-local historical bodies/log projections.
    DomainHistory {
        /// Selected domain.
        domain_id: WireHash,
        /// First domain-local block number.
        start_number: u64,
        /// Maximum history record count.
        limit: u16,
    },
    /// Targeted consensus-adapter payload; never sent through gossip.
    Consensus {
        /// Adapter-owned message discriminator.
        kind: u8,
        /// Opaque bounded adapter bytes.
        payload: Vec<u8>,
    },
}

impl DirectRequest {
    /// Validates request-specific collection and payload limits.
    ///
    /// # Errors
    ///
    /// Returns a static rejection reason.
    pub fn validate(&self) -> Result<(), &'static str> {
        match self {
            Self::Blocks { limit, .. } if *limit == 0 || *limit > MAX_BLOCKS_PER_RESPONSE => {
                Err("invalid block request limit")
            }
            Self::Headers { limit, .. } if *limit == 0 || *limit > MAX_HEADERS_PER_RESPONSE => {
                Err("invalid header request limit")
            }
            Self::BlockBodies { heights }
                if heights.is_empty()
                    || heights.len() > usize::from(MAX_BLOCKS_PER_RESPONSE)
                    || heights.windows(2).any(|pair| pair[0] >= pair[1]) =>
            {
                Err("invalid block body heights")
            }
            Self::DomainHistory { limit, .. } if *limit == 0 || *limit > MAX_HISTORY_ITEMS => {
                Err("invalid history request limit")
            }
            Self::Consensus { kind, payload }
                if *kind == 0 || payload.len() > MAX_CONSENSUS_MESSAGE_BYTES =>
            {
                Err("invalid consensus direct payload")
            }
            _ => Ok(()),
        }
    }
}

/// Response carried over the authenticated direct request-response protocol.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum DirectResponse {
    /// Remote identity accepted the handshake and returns its own identity.
    HandshakeAccepted(Handshake),
    /// Remote finalized status.
    Status(SyncStatus),
    /// Sequential finalized headers and adapter proofs.
    Headers(Vec<FinalizedHeader>),
    /// Complete block bodies matching a prior verified-header request.
    BlockBodies(Vec<BlockBody>),
    /// Sequential finalized block bodies.
    Blocks(Vec<FinalizedBlock>),
    /// Encoded checkpoint snapshot manifest.
    SnapshotManifest(Vec<u8>),
    /// Exact snapshot chunk bytes.
    SnapshotChunk(Vec<u8>),
    /// Bounded node-local history records.
    DomainHistory(Vec<Vec<u8>>),
    /// A targeted consensus message was delivered to the adapter boundary.
    ConsensusAccepted,
    /// Typed remote rejection without internal error disclosure.
    Rejected {
        /// Stable transport error code.
        code: u16,
    },
}

impl DirectResponse {
    /// Applies response collection and per-item limits before handing bytes to protocol code.
    ///
    /// # Errors
    ///
    /// Returns a static rejection reason.
    pub fn validate_transport(&self) -> Result<(), &'static str> {
        match self {
            Self::Status(status) if status.checkpoint_height > status.height => {
                Err("checkpoint height exceeds finalized height")
            }
            Self::Headers(headers) => {
                if headers.len() > usize::from(MAX_HEADERS_PER_RESPONSE) {
                    return Err("too many finalized headers");
                }
                let mut total = 0_usize;
                for header in headers {
                    if header.header.is_empty()
                        || header.header.len() > MAX_CONSENSUS_MESSAGE_BYTES
                        || header.finality_proof.len() > MAX_CONSENSUS_MESSAGE_BYTES
                    {
                        return Err("invalid finalized header bytes");
                    }
                    total = total
                        .checked_add(header.header.len())
                        .and_then(|value| value.checked_add(header.finality_proof.len()))
                        .ok_or("finalized header response size overflow")?;
                }
                if u64::try_from(total).map_or(true, |total| total > MAX_DIRECT_RESPONSE_BYTES) {
                    return Err("finalized header response exceeds limit");
                }
                Ok(())
            }
            Self::BlockBodies(bodies) => {
                if bodies.len() > usize::from(MAX_BLOCKS_PER_RESPONSE) {
                    return Err("too many block bodies");
                }
                let mut total = 0_usize;
                for body in bodies {
                    if body.block.is_empty() || body.block.len() > MAX_CONSENSUS_BLOCK_BYTES {
                        return Err("invalid block body bytes");
                    }
                    total = total
                        .checked_add(body.block.len())
                        .ok_or("block body response size overflow")?;
                }
                if u64::try_from(total).map_or(true, |total| total > MAX_DIRECT_RESPONSE_BYTES) {
                    return Err("block body response exceeds limit");
                }
                Ok(())
            }
            Self::Blocks(blocks) => {
                if blocks.len() > usize::from(MAX_BLOCKS_PER_RESPONSE) {
                    return Err("too many finalized blocks");
                }
                let mut total = 0_usize;
                for block in blocks {
                    block.validate_transport()?;
                    total = total
                        .checked_add(block.block.len())
                        .and_then(|value| value.checked_add(block.finality_proof.len()))
                        .ok_or("finalized response size overflow")?;
                }
                if u64::try_from(total).map_or(true, |total| total > MAX_DIRECT_RESPONSE_BYTES) {
                    return Err("finalized response exceeds limit");
                }
                Ok(())
            }
            Self::SnapshotManifest(bytes)
                if u64::try_from(bytes.len())
                    .map_or(true, |length| length > MAX_DIRECT_REQUEST_BYTES) =>
            {
                Err("snapshot manifest exceeds limit")
            }
            Self::SnapshotChunk(bytes)
                if bytes.is_empty() || bytes.len() > MAX_SNAPSHOT_CHUNK_BYTES =>
            {
                Err("snapshot chunk exceeds limit")
            }
            Self::DomainHistory(items) => {
                if items.len() > usize::from(MAX_HISTORY_ITEMS) {
                    return Err("too many history items");
                }
                let total = items.iter().try_fold(0_usize, |total, item| {
                    total.checked_add(item.len()).ok_or(())
                });
                if total.is_err()
                    || u64::try_from(total.unwrap_or(usize::MAX))
                        .map_or(true, |total| total > MAX_DIRECT_RESPONSE_BYTES)
                {
                    return Err("history response exceeds limit");
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

/// Correlation and handshake wrapper for one direct request.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RequestEnvelope {
    /// Caller-generated non-zero request identifier.
    pub request_id: u64,
    /// Genesis-bound sender identity.
    pub handshake: Handshake,
    /// Bounded request payload.
    pub request: DirectRequest,
}

impl RequestEnvelope {
    /// Validates the envelope against local identity.
    ///
    /// # Errors
    ///
    /// Returns a handshake error or request limit reason.
    pub fn validate(&self, local: &Handshake) -> Result<(), EnvelopeError> {
        if self.request_id == 0 {
            return Err(EnvelopeError::Request("request id is zero"));
        }
        self.handshake.validate_against(local)?;
        self.request.validate().map_err(EnvelopeError::Request)
    }
}

/// Correlation wrapper for one direct response.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ResponseEnvelope {
    /// Exact identifier from the request.
    pub request_id: u64,
    /// Bounded response payload.
    pub response: DirectResponse,
}

/// Direct-envelope validation error.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum EnvelopeError {
    /// Genesis-bound handshake failed.
    #[error(transparent)]
    Handshake(#[from] HandshakeError),
    /// Request fields exceeded their protocol limits.
    #[error("invalid direct request: {0}")]
    Request(&'static str),
}

/// Finalized-height gossip payload. Peers still fetch and validate exact blocks directly.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FinalizedAnnouncement {
    /// Finalized root-consensus height.
    pub height: u64,
    /// Finalized consensus-header hash.
    pub consensus_hash: WireHash,
    /// Sparse commitment to all current domain heads.
    pub domain_heads_root: WireHash,
}

/// Signed gossipsub payload. Gossip never carries consensus votes.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum GossipMessage {
    /// Raw EIP-2718 transaction routed by domain topic.
    Transaction {
        /// Target domain.
        domain_id: WireHash,
        /// Exact raw typed envelope.
        envelope: Vec<u8>,
    },
    /// Small finalized-head announcement.
    Finalized(FinalizedAnnouncement),
}

impl GossipMessage {
    /// Applies gossip payload limits before publication or event delivery.
    ///
    /// # Errors
    ///
    /// Returns a static reason for malformed or oversized data.
    pub fn validate(&self) -> Result<(), &'static str> {
        match self {
            Self::Transaction { envelope, .. }
                if envelope.is_empty() || envelope.len() > MAX_TRANSACTION_ENVELOPE_BYTES =>
            {
                Err("transaction envelope exceeds limit")
            }
            Self::Finalized(announcement) if announcement.height == 0 => {
                Err("zero-height finalized announcement")
            }
            _ => Ok(()),
        }
    }

    /// Returns the routed domain for a transaction payload.
    #[must_use]
    pub fn domain_id(&self) -> Option<DomainId> {
        match self {
            Self::Transaction { domain_id, .. } => Some(DomainId(B256::from(*domain_id))),
            Self::Finalized(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handshake() -> Handshake {
        Handshake::new(
            NetworkId(B256::repeat_byte(1)),
            B256::repeat_byte(2),
            NodeRole::Full,
            vec![Capability::StateSync, Capability::BlockSync],
        )
    }

    #[test]
    fn handshake_rejects_identity_versions_and_noncanonical_capabilities() {
        let local = handshake();
        let mut remote = local.clone();
        remote.network_id[0] ^= 1;
        assert_eq!(
            remote.validate_against(&local),
            Err(HandshakeError::Network)
        );

        let mut remote = local.clone();
        remote.genesis_hash[0] ^= 1;
        assert_eq!(
            remote.validate_against(&local),
            Err(HandshakeError::Genesis)
        );

        let mut remote = local.clone();
        remote.protocol_version = remote.protocol_version.saturating_add(1);
        assert_eq!(
            remote.validate_against(&local),
            Err(HandshakeError::ProtocolVersion)
        );

        let mut remote = local.clone();
        remote.codec_version = remote.codec_version.saturating_add(1);
        assert_eq!(
            remote.validate_against(&local),
            Err(HandshakeError::CodecVersion)
        );

        let mut remote = local.clone();
        remote.capabilities = vec![Capability::StateSync, Capability::BlockSync];
        assert_eq!(
            remote.validate_against(&local),
            Err(HandshakeError::Capabilities)
        );
    }

    #[test]
    fn direct_and_gossip_limits_are_explicit() {
        assert!(
            DirectRequest::Blocks {
                start_height: 1,
                limit: MAX_BLOCKS_PER_RESPONSE
            }
            .validate()
            .is_ok()
        );
        assert!(
            DirectRequest::Blocks {
                start_height: 1,
                limit: MAX_BLOCKS_PER_RESPONSE + 1
            }
            .validate()
            .is_err()
        );
        assert!(
            DirectRequest::Headers {
                start_height: 1,
                limit: MAX_HEADERS_PER_RESPONSE
            }
            .validate()
            .is_ok()
        );
        assert!(
            DirectRequest::Headers {
                start_height: 1,
                limit: MAX_HEADERS_PER_RESPONSE + 1
            }
            .validate()
            .is_err()
        );
        assert!(
            GossipMessage::Transaction {
                domain_id: [0; 32],
                envelope: vec![0; MAX_TRANSACTION_ENVELOPE_BYTES + 1],
            }
            .validate()
            .is_err()
        );
    }
}
