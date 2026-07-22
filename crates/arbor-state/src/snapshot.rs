use alloy_primitives::{B256, keccak256};
use arbor_primitives::DomainId;

use crate::StateError;

const SNAPSHOT_TAG: &[u8] = b"ARBOR_SNAPSHOT_MANIFEST_V1";
const SNAPSHOT_VERSION: u8 = 1;

/// Maximum number of chunks in one snapshot manifest.
pub const MAX_SNAPSHOT_CHUNKS: usize = 65_536;
/// Maximum bytes described by one snapshot chunk.
pub const MAX_SNAPSHOT_CHUNK_BYTES: u32 = 8 * 1024 * 1024;

/// Content-addressed snapshot chunk descriptor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SnapshotChunk {
    /// Contiguous zero-based chunk number.
    pub index: u32,
    /// Exact encoded chunk length.
    pub bytes: u32,
    /// Keccak-256 of exact chunk bytes.
    pub hash: B256,
}

/// Minimal authenticated-state snapshot manifest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SnapshotManifest {
    /// Finalized root-consensus height.
    pub consensus_height: u64,
    /// Domain carried by this snapshot.
    pub domain_id: DomainId,
    /// Ethereum state root reconstructed by its chunks.
    pub state_root: B256,
    /// Ordered content-addressed chunks.
    pub chunks: Vec<SnapshotChunk>,
}

impl SnapshotManifest {
    /// Validates chunk limits, order, and non-zero sizes.
    ///
    /// # Errors
    ///
    /// Returns [`StateError`] for malformed or oversized descriptors.
    pub fn validate(&self) -> Result<(), StateError> {
        if self.chunks.len() > MAX_SNAPSHOT_CHUNKS {
            return Err(StateError::LimitExceeded("snapshot chunk count"));
        }
        for (expected, chunk) in self.chunks.iter().enumerate() {
            if chunk.index as usize != expected {
                return Err(StateError::InvalidPath("snapshot chunk ordering"));
            }
            if chunk.bytes == 0 || chunk.bytes > MAX_SNAPSHOT_CHUNK_BYTES {
                return Err(StateError::LimitExceeded("snapshot chunk bytes"));
            }
        }
        Ok(())
    }

    /// Encodes the fixed-width versioned manifest.
    ///
    /// # Errors
    ///
    /// Returns [`StateError`] if validation fails.
    pub fn encode(&self) -> Result<Vec<u8>, StateError> {
        self.validate()?;
        let count = u32::try_from(self.chunks.len())
            .map_err(|_| StateError::LimitExceeded("snapshot chunk count"))?;
        let mut out = Vec::with_capacity(SNAPSHOT_TAG.len() + 77 + self.chunks.len() * 40);
        out.extend_from_slice(SNAPSHOT_TAG);
        out.push(SNAPSHOT_VERSION);
        out.extend_from_slice(&self.consensus_height.to_be_bytes());
        out.extend_from_slice(self.domain_id.0.as_slice());
        out.extend_from_slice(self.state_root.as_slice());
        out.extend_from_slice(&count.to_be_bytes());
        for chunk in &self.chunks {
            out.extend_from_slice(&chunk.index.to_be_bytes());
            out.extend_from_slice(&chunk.bytes.to_be_bytes());
            out.extend_from_slice(chunk.hash.as_slice());
        }
        Ok(out)
    }

    /// Decodes one exact manifest with bounded allocation.
    ///
    /// # Errors
    ///
    /// Returns [`StateError`] for unknown version, truncation, trailing bytes, or bad limits.
    pub fn decode(input: &[u8]) -> Result<Self, StateError> {
        let fixed = SNAPSHOT_TAG.len() + 1 + 8 + 32 + 32 + 4;
        if input.len() < fixed || !input.starts_with(SNAPSHOT_TAG) {
            return Err(StateError::MalformedRlp("snapshot manifest tag/length"));
        }
        let mut cursor = SNAPSHOT_TAG.len();
        if input[cursor] != SNAPSHOT_VERSION {
            return Err(StateError::MalformedRlp("snapshot manifest version"));
        }
        cursor += 1;
        let consensus_height = take_u64(input, &mut cursor)?;
        let domain_id = DomainId(take_hash(input, &mut cursor)?);
        let state_root = take_hash(input, &mut cursor)?;
        let count = take_u32(input, &mut cursor)? as usize;
        if count > MAX_SNAPSHOT_CHUNKS {
            return Err(StateError::LimitExceeded("snapshot chunk count"));
        }
        let expected = cursor
            .checked_add(count * 40)
            .ok_or(StateError::LimitExceeded("snapshot encoded size"))?;
        if input.len() != expected {
            return Err(StateError::MalformedRlp(
                "snapshot manifest trailing/truncated",
            ));
        }
        let mut chunks = Vec::with_capacity(count);
        for _ in 0..count {
            chunks.push(SnapshotChunk {
                index: take_u32(input, &mut cursor)?,
                bytes: take_u32(input, &mut cursor)?,
                hash: take_hash(input, &mut cursor)?,
            });
        }
        let manifest = Self {
            consensus_height,
            domain_id,
            state_root,
            chunks,
        };
        manifest.validate()?;
        Ok(manifest)
    }

    /// Returns the domain-separated manifest hash.
    ///
    /// # Errors
    ///
    /// Returns [`StateError`] if the manifest is invalid.
    pub fn hash(&self) -> Result<B256, StateError> {
        Ok(keccak256(self.encode()?))
    }

    /// Verifies exact bytes against a declared chunk descriptor.
    #[must_use]
    pub fn verify_chunk(chunk: SnapshotChunk, bytes: &[u8]) -> bool {
        usize::try_from(chunk.bytes) == Ok(bytes.len()) && keccak256(bytes) == chunk.hash
    }
}

fn take_u32(input: &[u8], cursor: &mut usize) -> Result<u32, StateError> {
    let bytes: [u8; 4] = input
        .get(*cursor..*cursor + 4)
        .ok_or(StateError::MalformedRlp("snapshot u32"))?
        .try_into()
        .map_err(|_| StateError::MalformedRlp("snapshot u32"))?;
    *cursor += 4;
    Ok(u32::from_be_bytes(bytes))
}

fn take_u64(input: &[u8], cursor: &mut usize) -> Result<u64, StateError> {
    let bytes: [u8; 8] = input
        .get(*cursor..*cursor + 8)
        .ok_or(StateError::MalformedRlp("snapshot u64"))?
        .try_into()
        .map_err(|_| StateError::MalformedRlp("snapshot u64"))?;
    *cursor += 8;
    Ok(u64::from_be_bytes(bytes))
}

fn take_hash(input: &[u8], cursor: &mut usize) -> Result<B256, StateError> {
    let hash = B256::try_from(
        input
            .get(*cursor..*cursor + 32)
            .ok_or(StateError::MalformedRlp("snapshot hash"))?,
    )
    .map_err(|_| StateError::MalformedRlp("snapshot hash"))?;
    *cursor += 32;
    Ok(hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_roundtrip_and_chunk_binding() {
        let bytes = b"snapshot chunk";
        let chunk = SnapshotChunk {
            index: 0,
            bytes: u32::try_from(bytes.len()).unwrap(),
            hash: keccak256(bytes),
        };
        let manifest = SnapshotManifest {
            consensus_height: 9,
            domain_id: DomainId(B256::repeat_byte(1)),
            state_root: B256::repeat_byte(2),
            chunks: vec![chunk],
        };
        assert_eq!(
            SnapshotManifest::decode(&manifest.encode().unwrap()).unwrap(),
            manifest
        );
        assert!(SnapshotManifest::verify_chunk(chunk, bytes));
        assert!(!SnapshotManifest::verify_chunk(chunk, b"tampered"));
    }
}
