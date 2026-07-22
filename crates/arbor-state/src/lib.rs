//! Authenticated Ethereum state, overlays, domain-head commitments, and snapshots.

#![forbid(unsafe_code)]

mod account;
mod domain_heads;
mod mpt;
mod overlay;
mod snapshot;

pub use account::{Account, decode_account, encode_account, storage_trie_value};
pub use alloy_trie::{EMPTY_ROOT_HASH, KECCAK_EMPTY};
pub use domain_heads::{DomainHead, DomainHeadProof, DomainHeadsCommitment};
pub use mpt::{
    EthereumStateCommitment, MemoryNodeStore, NodeStore, StateCommitment, StateError, StateProof,
    TrieSnapshot, secure_account_key, secure_storage_key,
};
pub use overlay::{StateOverlay, StateView};
pub use snapshot::{
    MAX_SNAPSHOT_CHUNK_BYTES, MAX_SNAPSHOT_CHUNKS, SnapshotChunk, SnapshotManifest,
};
