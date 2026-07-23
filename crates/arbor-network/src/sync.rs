use alloy_primitives::B256;
use arbor_chain::{ConsensusBlock, decode_consensus_block};
use arbor_codec::decode_consensus_header;
use arbor_crypto::consensus_header_hash;
use arbor_primitives::ConsensusBlockHeader;
use thiserror::Error;

use crate::{
    BlockBody, FinalizedBlock, FinalizedHeader, MAX_BLOCKS_PER_RESPONSE, MAX_HEADERS_PER_RESPONSE,
    SyncStatus,
};

/// Consensus-adapter boundary used before any block body is requested.
pub trait HeaderFinalityVerifier {
    /// Verifies one canonical header/proof pair under the selected consensus mode.
    ///
    /// # Errors
    ///
    /// Returns without scheduling body download when finality is not established.
    fn verify_header(
        &self,
        finalized: &FinalizedHeader,
        header: &ConsensusBlockHeader,
    ) -> Result<(), String>;
}

/// Explicit empty-proof verifier for development-only single-validator headers.
#[derive(Clone, Copy, Debug, Default)]
pub struct DevelopmentHeaderVerifier;

impl HeaderFinalityVerifier for DevelopmentHeaderVerifier {
    fn verify_header(
        &self,
        finalized: &FinalizedHeader,
        _header: &ConsensusBlockHeader,
    ) -> Result<(), String> {
        if finalized.finality_proof.is_empty() {
            Ok(())
        } else {
            Err("development header finality proof must be empty".to_owned())
        }
    }
}

/// Header accepted for an exact subsequent body request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedHeader {
    /// Canonically decoded header.
    pub header: ConsensusBlockHeader,
    /// Proof already accepted by the selected finality verifier.
    pub finality_proof: Vec<u8>,
}

/// Deterministic header/finality synchronization.
#[derive(Clone, Copy, Debug, Default)]
pub struct HeaderSync;

impl HeaderSync {
    /// Verifies sequential ancestry, canonical header bytes, finality, and advertised remote tip.
    ///
    /// # Errors
    ///
    /// Rejects dropped/reordered/duplicate headers or any invalid proof before body download.
    pub fn verify_response<V: HeaderFinalityVerifier>(
        &self,
        local: SyncStatus,
        remote: SyncStatus,
        headers: Vec<FinalizedHeader>,
        verifier: &V,
    ) -> Result<Vec<VerifiedHeader>, HeaderSyncError> {
        if headers.len() > usize::from(MAX_HEADERS_PER_RESPONSE) {
            return Err(HeaderSyncError::TooManyHeaders);
        }
        if remote.height <= local.height {
            return if headers.is_empty() {
                Ok(Vec::new())
            } else {
                Err(HeaderSyncError::UnsolicitedHeaders)
            };
        }
        if headers.is_empty() {
            return Err(HeaderSyncError::EmptyResponse);
        }
        let mut expected_height = local.height.saturating_add(1);
        let mut expected_parent = B256::from(local.consensus_hash);
        let mut accepted = Vec::with_capacity(headers.len());
        for finalized in headers {
            if finalized.height != expected_height || finalized.height > remote.height {
                return Err(HeaderSyncError::NonSequential {
                    expected: expected_height,
                    actual: finalized.height,
                });
            }
            let header = decode_consensus_header(&finalized.header)
                .map_err(|error| HeaderSyncError::Decode(error.to_string()))?;
            if header.height.0 != finalized.height {
                return Err(HeaderSyncError::ClaimedHeight);
            }
            if header.parent_hash != expected_parent {
                return Err(HeaderSyncError::Parent);
            }
            verifier
                .verify_header(&finalized, &header)
                .map_err(HeaderSyncError::Finality)?;
            expected_parent = consensus_header_hash(&header)
                .map_err(|error| HeaderSyncError::Decode(error.to_string()))?;
            expected_height = expected_height.saturating_add(1);
            accepted.push(VerifiedHeader {
                header,
                finality_proof: finalized.finality_proof,
            });
        }
        if accepted
            .last()
            .is_some_and(|header| header.header.height.0 == remote.height)
            && (expected_parent != B256::from(remote.consensus_hash)
                || accepted.last().is_none_or(|header| {
                    header.header.domain_heads_root != B256::from(remote.domain_heads_root)
                }))
        {
            return Err(HeaderSyncError::RemoteTip);
        }
        Ok(accepted)
    }
}

/// Combines bodies only with the exact headers that were already verified.
///
/// # Errors
///
/// Rejects a different body count, height, canonical encoding, or embedded header.
pub fn finalized_blocks_from_bodies(
    headers: &[VerifiedHeader],
    bodies: Vec<BlockBody>,
) -> Result<Vec<FinalizedBlock>, HeaderSyncError> {
    if bodies.len() != headers.len() {
        return Err(HeaderSyncError::BodyCount);
    }
    headers
        .iter()
        .zip(bodies)
        .map(|(verified, body)| {
            if body.height != verified.header.height.0 {
                return Err(HeaderSyncError::BodyHeight);
            }
            let block = decode_consensus_block(&body.block)
                .map_err(|error| HeaderSyncError::Decode(error.to_string()))?;
            if block.header != verified.header {
                return Err(HeaderSyncError::BodyHeader);
            }
            Ok(FinalizedBlock {
                height: body.height,
                block: body.block,
                finality_proof: verified.finality_proof.clone(),
            })
        })
        .collect()
}

/// Header/finality/body synchronization failure.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum HeaderSyncError {
    /// Peer exceeded the bounded response count.
    #[error("too many finalized headers")]
    TooManyHeaders,
    /// Peer sent headers while not ahead.
    #[error("peer sent unsolicited headers")]
    UnsolicitedHeaders,
    /// Ahead peer returned no header progress.
    #[error("peer dropped the requested header range")]
    EmptyResponse,
    /// Header heights were reordered or duplicated.
    #[error("non-sequential header: expected {expected}, got {actual}")]
    NonSequential {
        /// Required next height.
        expected: u64,
        /// Received height.
        actual: u64,
    },
    /// Wrapper and canonical header heights differ.
    #[error("finalized header claimed height mismatch")]
    ClaimedHeight,
    /// Header does not extend the previous verified hash.
    #[error("finalized header parent mismatch")]
    Parent,
    /// Canonical decoding or hashing failed.
    #[error("invalid finalized header/body encoding: {0}")]
    Decode(String),
    /// Consensus adapter rejected finality.
    #[error("invalid header finality proof: {0}")]
    Finality(String),
    /// Complete response disagrees with advertised remote status.
    #[error("verified header tip differs from advertised remote status")]
    RemoteTip,
    /// Body count differs from the verified header request.
    #[error("block body count differs from verified headers")]
    BodyCount,
    /// Body wrapper height differs from its verified header.
    #[error("block body height differs from verified header")]
    BodyHeight,
    /// Decoded body embeds a different header.
    #[error("block body does not match its verified header")]
    BodyHeader,
}

/// Application boundary required by the block-sync state machine.
///
/// Implementations must verify finality before import and make each successful import durable
/// before returning. M7's development implementation may explicitly accept an empty proof; M8
/// must replace that verifier with the accepted QC/finality proof path.
pub trait BlockImport {
    /// Returns the local durable finalized status.
    ///
    /// # Errors
    ///
    /// Returns an application/storage error without changing state.
    fn status(&self) -> Result<SyncStatus, String>;

    /// Verifies that a decoded block is finalized under the selected consensus mode.
    ///
    /// This is called before [`Self::import_block`], so invalid proof bytes cannot reach the
    /// durable application commit boundary.
    ///
    /// # Errors
    ///
    /// Returns a proof error without changing state.
    fn verify_finality(
        &self,
        finalized: &FinalizedBlock,
        block: &ConsensusBlock,
    ) -> Result<(), String>;

    /// Replays, validates, and durably commits exactly one next-height block.
    ///
    /// # Errors
    ///
    /// Returns an application/storage error. A failed call must not publish a new finalized head.
    fn import_block(&mut self, block: ConsensusBlock) -> Result<SyncStatus, String>;
}

/// Deterministic bounded block-sync state machine.
#[derive(Clone, Copy, Debug)]
pub struct BlockSync {
    max_blocks_per_response: u16,
}

impl Default for BlockSync {
    fn default() -> Self {
        Self {
            max_blocks_per_response: MAX_BLOCKS_PER_RESPONSE,
        }
    }
}

impl BlockSync {
    /// Constructs a stricter local request budget.
    ///
    /// # Errors
    ///
    /// Rejects zero or protocol-exceeding limits.
    pub fn new(max_blocks_per_response: u16) -> Result<Self, BlockSyncError> {
        if max_blocks_per_response == 0 || max_blocks_per_response > MAX_BLOCKS_PER_RESPONSE {
            return Err(BlockSyncError::InvalidLimit);
        }
        Ok(Self {
            max_blocks_per_response,
        })
    }

    /// Returns the first height and bounded count for the next request.
    ///
    /// `None` means the local durable head is already caught up.
    #[must_use]
    pub fn next_request(&self, local: SyncStatus, remote: SyncStatus) -> Option<(u64, u16)> {
        if remote.height <= local.height {
            return None;
        }
        let remaining = remote.height - local.height;
        Some((
            local.height.saturating_add(1),
            u16::try_from(remaining)
                .unwrap_or(u16::MAX)
                .min(self.max_blocks_per_response),
        ))
    }

    /// Verifies and imports one sequential response.
    ///
    /// Transport shape, decoded height/hash, finality, deterministic execution, and durable
    /// application commit are checked in that order. No unverified block reaches `import_block`.
    ///
    /// # Errors
    ///
    /// Returns a typed error for malformed order, proof, decoding, or application commit.
    pub fn import_response<I: BlockImport>(
        &self,
        importer: &mut I,
        remote: SyncStatus,
        blocks: Vec<FinalizedBlock>,
    ) -> Result<BlockSyncReport, BlockSyncError> {
        if blocks.len() > usize::from(self.max_blocks_per_response) {
            return Err(BlockSyncError::TooManyBlocks {
                limit: self.max_blocks_per_response,
                actual: blocks.len(),
            });
        }
        let before = importer.status().map_err(BlockSyncError::Application)?;
        if remote.height <= before.height {
            if blocks.is_empty() {
                return Ok(BlockSyncReport {
                    before,
                    after: before,
                    imported: 0,
                });
            }
            return Err(BlockSyncError::UnsolicitedBlocks);
        }
        if blocks.is_empty() {
            return Err(BlockSyncError::EmptyResponse {
                local: before.height,
                remote: remote.height,
            });
        }

        let mut expected_height = before.height.saturating_add(1);
        let mut current = before;
        let mut imported_count = 0_usize;
        for finalized in blocks {
            finalized
                .validate_transport()
                .map_err(BlockSyncError::Transport)?;
            if finalized.height != expected_height || finalized.height > remote.height {
                return Err(BlockSyncError::NonSequentialHeight {
                    expected: expected_height,
                    actual: finalized.height,
                });
            }
            let block = decode_consensus_block(&finalized.block)
                .map_err(|error| BlockSyncError::Decode(error.to_string()))?;
            if block.header.height.0 != finalized.height {
                return Err(BlockSyncError::ClaimedHeight {
                    claimed: finalized.height,
                    decoded: block.header.height.0,
                });
            }
            let block_hash = block
                .hash()
                .map_err(|error| BlockSyncError::Decode(error.to_string()))?;
            importer
                .verify_finality(&finalized, &block)
                .map_err(BlockSyncError::Finality)?;
            current = importer
                .import_block(block)
                .map_err(BlockSyncError::Application)?;
            if current.height != expected_height || B256::from(current.consensus_hash) != block_hash
            {
                return Err(BlockSyncError::Application(
                    "importer returned a status inconsistent with the committed block".to_owned(),
                ));
            }
            imported_count += 1;
            expected_height = expected_height.saturating_add(1);
        }
        if current.height == remote.height
            && (current.consensus_hash != remote.consensus_hash
                || current.domain_heads_root != remote.domain_heads_root)
        {
            return Err(BlockSyncError::RemoteTip);
        }

        Ok(BlockSyncReport {
            before,
            after: current,
            imported: imported_count,
        })
    }
}

/// Successful bounded sync progress.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BlockSyncReport {
    /// Durable status before this response.
    pub before: SyncStatus,
    /// Durable status after importing all accepted blocks.
    pub after: SyncStatus,
    /// Number of newly committed blocks.
    pub imported: usize,
}

/// Block-sync validation or durable import failure.
#[derive(Debug, Error)]
pub enum BlockSyncError {
    /// Local batch size configuration exceeds the wire protocol.
    #[error("invalid block sync request limit")]
    InvalidLimit,
    /// Peer sent more blocks than requested.
    #[error("block response has {actual} blocks; limit is {limit}")]
    TooManyBlocks {
        /// Configured local bound.
        limit: u16,
        /// Actual peer response count.
        actual: usize,
    },
    /// Peer sent blocks although the remote status is not ahead.
    #[error("peer sent unsolicited blocks")]
    UnsolicitedBlocks,
    /// Peer claimed to be ahead but returned no progress.
    #[error("empty block response while local height {local} trails remote height {remote}")]
    EmptyResponse {
        /// Local durable height.
        local: u64,
        /// Claimed remote height.
        remote: u64,
    },
    /// Claimed response height is not the next sequential height.
    #[error("non-sequential block height: expected {expected}, got {actual}")]
    NonSequentialHeight {
        /// Next required height.
        expected: u64,
        /// Claimed response height.
        actual: u64,
    },
    /// Encoded header height differs from its response wrapper.
    #[error("claimed block height {claimed} differs from decoded height {decoded}")]
    ClaimedHeight {
        /// Wrapper height.
        claimed: u64,
        /// Header height.
        decoded: u64,
    },
    /// Transport-level size/shape validation failed.
    #[error("invalid block transport: {0}")]
    Transport(&'static str),
    /// Canonical block decoding or hashing failed.
    #[error("invalid block encoding: {0}")]
    Decode(String),
    /// Consensus finality verification failed before application import.
    #[error("invalid finality proof: {0}")]
    Finality(String),
    /// Replay, storage, or returned-status validation failed.
    #[error("block import failed: {0}")]
    Application(String),
    /// Imported complete range disagrees with the peer's advertised finalized status.
    #[error("imported tip differs from advertised remote status")]
    RemoteTip,
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Importer {
        status: SyncStatus,
        imports: usize,
    }

    impl BlockImport for Importer {
        fn status(&self) -> Result<SyncStatus, String> {
            Ok(self.status)
        }

        fn verify_finality(
            &self,
            _finalized: &FinalizedBlock,
            _block: &ConsensusBlock,
        ) -> Result<(), String> {
            Err("fixture proof rejected".to_owned())
        }

        fn import_block(&mut self, _block: ConsensusBlock) -> Result<SyncStatus, String> {
            self.imports += 1;
            Ok(self.status)
        }
    }

    #[test]
    fn empty_or_malformed_responses_never_reach_import() {
        let status = SyncStatus {
            height: 0,
            consensus_hash: [0; 32],
            domain_heads_root: [0; 32],
            checkpoint_height: 0,
        };
        let remote = SyncStatus {
            height: 1,
            consensus_hash: [1; 32],
            domain_heads_root: [1; 32],
            checkpoint_height: 1,
        };
        let mut importer = Importer { status, imports: 0 };
        assert!(matches!(
            BlockSync::default().import_response(&mut importer, remote, Vec::new()),
            Err(BlockSyncError::EmptyResponse { .. })
        ));
        assert_eq!(importer.imports, 0);

        let malformed = FinalizedBlock {
            height: 1,
            block: vec![1, 2, 3],
            finality_proof: Vec::new(),
        };
        assert!(matches!(
            BlockSync::default().import_response(&mut importer, remote, vec![malformed]),
            Err(BlockSyncError::Decode(_))
        ));
        assert_eq!(importer.imports, 0);
    }

    #[test]
    fn dropped_reordered_and_duplicate_headers_are_rejected_before_body_download() {
        let local = SyncStatus {
            height: 0,
            consensus_hash: [0; 32],
            domain_heads_root: [0; 32],
            checkpoint_height: 0,
        };
        let remote = SyncStatus {
            height: 2,
            consensus_hash: [2; 32],
            domain_heads_root: [2; 32],
            checkpoint_height: 2,
        };
        assert_eq!(
            HeaderSync.verify_response(local, remote, Vec::new(), &DevelopmentHeaderVerifier),
            Err(HeaderSyncError::EmptyResponse)
        );
        let reordered = FinalizedHeader {
            height: 2,
            header: vec![1],
            finality_proof: Vec::new(),
        };
        assert_eq!(
            HeaderSync.verify_response(
                local,
                remote,
                vec![reordered.clone()],
                &DevelopmentHeaderVerifier,
            ),
            Err(HeaderSyncError::NonSequential {
                expected: 1,
                actual: 2,
            })
        );
        let header = arbor_primitives::ConsensusBlockHeader {
            protocol_version: arbor_primitives::PROTOCOL_VERSION,
            network_id: arbor_primitives::NetworkId(B256::repeat_byte(1)),
            height: arbor_primitives::ConsensusHeight(1),
            parent_hash: B256::ZERO,
            timestamp: 1,
            batches_root: B256::repeat_byte(2),
            domain_results_root: B256::repeat_byte(3),
            domain_heads_root: B256::repeat_byte(4),
            validator_set_hash: B256::repeat_byte(5),
            next_validator_set_hash: B256::repeat_byte(5),
            proposer: alloy_primitives::Address::ZERO,
        };
        let encoded = arbor_codec::encode_consensus_header(&header).unwrap();
        let duplicate = vec![
            FinalizedHeader {
                height: 1,
                header: encoded.clone(),
                finality_proof: Vec::new(),
            },
            FinalizedHeader {
                height: 1,
                header: encoded,
                finality_proof: Vec::new(),
            },
        ];
        assert_eq!(
            HeaderSync.verify_response(local, remote, duplicate, &DevelopmentHeaderVerifier),
            Err(HeaderSyncError::NonSequential {
                expected: 2,
                actual: 1,
            })
        );
    }
}
