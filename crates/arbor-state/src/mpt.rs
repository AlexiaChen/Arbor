use std::collections::{BTreeMap, BTreeSet};

use alloy_primitives::{Address, B256, Bytes, U256, keccak256};
use alloy_rlp::{Encodable, Header};
use alloy_trie::{EMPTY_ROOT_HASH, Nibbles, proof::verify_proof};
use thiserror::Error;

/// Maximum secure-trie depth in nibbles.
const SECURE_KEY_NIBBLES: usize = 64;

/// Authenticated-state construction, proof, or node-store failure.
#[derive(Debug, Error)]
pub enum StateError {
    /// A stored node referenced by a trie edge is absent.
    #[error("missing trie node {0}")]
    MissingNode(B256),
    /// Stored node bytes do not match their content-addressed key.
    #[error("trie node hash mismatch: expected {expected}, got {actual}")]
    NodeHashMismatch {
        /// Hash requested by the parent node.
        expected: B256,
        /// Hash calculated from the stored value.
        actual: B256,
    },
    /// Trie RLP is malformed or non-canonical.
    #[error("malformed trie RLP: {0}")]
    MalformedRlp(&'static str),
    /// A compact path or trie topology violates secure-key rules.
    #[error("invalid trie path: {0}")]
    InvalidPath(&'static str),
    /// A node-store backend failed.
    #[error("node store failure: {0}")]
    Store(String),
    /// An independently checked Ethereum proof failed.
    #[error("proof verification failed: {0}")]
    Proof(String),
    /// A collection exceeds the protocol resource budget.
    #[error("state limit exceeded: {0}")]
    LimitExceeded(&'static str),
}

/// Read boundary for immutable content-addressed MPT nodes.
pub trait NodeStore {
    /// Returns exact RLP node bytes for a Keccak content hash.
    ///
    /// # Errors
    ///
    /// Returns [`StateError`] if the backend cannot complete the read.
    fn get_node(&self, hash: B256) -> Result<Option<Vec<u8>>, StateError>;
}

/// In-memory node store used by overlays and deterministic tests.
#[derive(Clone, Debug, Default)]
pub struct MemoryNodeStore {
    nodes: BTreeMap<B256, Vec<u8>>,
}

impl MemoryNodeStore {
    /// Constructs a store from content-addressed nodes.
    #[must_use]
    pub fn new(nodes: BTreeMap<B256, Vec<u8>>) -> Self {
        Self { nodes }
    }

    /// Returns the stored node map.
    #[must_use]
    pub const fn nodes(&self) -> &BTreeMap<B256, Vec<u8>> {
        &self.nodes
    }

    /// Removes a node to support missing-node recovery tests.
    pub fn remove(&mut self, hash: B256) -> Option<Vec<u8>> {
        self.nodes.remove(&hash)
    }
}

impl NodeStore for MemoryNodeStore {
    fn get_node(&self, hash: B256) -> Result<Option<Vec<u8>>, StateError> {
        Ok(self.nodes.get(&hash).cloned())
    }
}

/// A fully materialized immutable secure MPT snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrieSnapshot {
    root: B256,
    nodes: BTreeMap<B256, Vec<u8>>,
    leaves: BTreeMap<B256, Vec<u8>>,
}

impl TrieSnapshot {
    /// Returns the Ethereum MPT root.
    #[must_use]
    pub const fn root(&self) -> B256 {
        self.root
    }

    /// Returns every content-addressed RLP node needed to traverse the root.
    #[must_use]
    pub const fn nodes(&self) -> &BTreeMap<B256, Vec<u8>> {
        &self.nodes
    }

    /// Returns secure-key/value leaves, suitable for a rebuildable flat cache.
    #[must_use]
    pub const fn leaves(&self) -> &BTreeMap<B256, Vec<u8>> {
        &self.leaves
    }

    /// Adds independently rooted content-addressed nodes required by values in this trie.
    ///
    /// Account snapshots use this to carry contract-storage trie nodes through the same
    /// durable commit and retention manifest. The account root and flat account leaves do
    /// not change.
    ///
    /// # Errors
    ///
    /// Returns [`StateError::NodeHashMismatch`] if a node is not keyed by its Keccak hash,
    /// or [`StateError::Proof`] if the same hash is paired with different bytes.
    pub fn extend_nodes(
        &mut self,
        nodes: impl IntoIterator<Item = (B256, Vec<u8>)>,
    ) -> Result<(), StateError> {
        for (hash, bytes) in nodes {
            if keccak256(&bytes) != hash {
                return Err(StateError::NodeHashMismatch {
                    expected: hash,
                    actual: keccak256(&bytes),
                });
            }
            if self
                .nodes
                .get(&hash)
                .is_some_and(|existing| *existing != bytes)
            {
                return Err(StateError::Proof("content hash collision".to_owned()));
            }
            self.nodes.insert(hash, bytes);
        }
        Ok(())
    }

    /// Creates an independently verifiable inclusion or exclusion proof.
    ///
    /// # Errors
    ///
    /// Returns [`StateError`] if a snapshot node is missing or malformed.
    pub fn proof(&self, key: B256) -> Result<StateProof, StateError> {
        EthereumStateCommitment::proof(self.root, key, &MemoryNodeStore::new(self.nodes.clone()))
    }
}

/// Ethereum MPT inclusion or exclusion proof.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StateProof {
    /// Root against which the proof is checked.
    pub root: B256,
    /// Secure 32-byte trie key.
    pub key: B256,
    /// Exact RLP leaf value, or `None` for exclusion.
    pub value: Option<Vec<u8>>,
    /// Root-to-terminal standard Ethereum MPT nodes.
    pub nodes: Vec<Bytes>,
}

impl StateProof {
    /// Verifies the proof with alloy-trie's independent Ethereum proof verifier.
    ///
    /// # Errors
    ///
    /// Returns [`StateError::Proof`] when the proof does not match its root/key/value.
    pub fn verify(&self) -> Result<(), StateError> {
        verify_proof(
            self.root,
            Nibbles::unpack(self.key),
            self.value.clone(),
            &self.nodes,
        )
        .map_err(|error| StateError::Proof(error.to_string()))
    }
}

/// Deterministic Ethereum secure Merkle-Patricia Trie commitment implementation.
#[derive(Clone, Copy, Debug, Default)]
pub struct EthereumStateCommitment;

/// Authenticated secure-state commitment boundary.
pub trait StateCommitment {
    /// Builds an immutable snapshot from canonical secure leaves.
    ///
    /// # Errors
    ///
    /// Returns [`StateError`] when commitment construction violates resource or trie rules.
    fn commit(&self, leaves: &BTreeMap<B256, Vec<u8>>) -> Result<TrieSnapshot, StateError>;
}

impl StateCommitment for EthereumStateCommitment {
    fn commit(&self, leaves: &BTreeMap<B256, Vec<u8>>) -> Result<TrieSnapshot, StateError> {
        Self::build(leaves)
    }
}

impl EthereumStateCommitment {
    /// Builds a snapshot from already-secure, uniquely keyed leaf values.
    ///
    /// The `BTreeMap` gives a canonical order; caller insertion order cannot affect the root.
    ///
    /// # Errors
    ///
    /// Returns [`StateError`] if the collection exceeds supported sizes.
    pub fn build(leaves: &BTreeMap<B256, Vec<u8>>) -> Result<TrieSnapshot, StateError> {
        if leaves.is_empty() {
            return Ok(TrieSnapshot {
                root: EMPTY_ROOT_HASH,
                nodes: BTreeMap::new(),
                leaves: BTreeMap::new(),
            });
        }
        if leaves.len() > u32::MAX as usize {
            return Err(StateError::LimitExceeded("secure trie leaves"));
        }
        let entries: Vec<_> = leaves
            .iter()
            .map(|(key, value)| (unpack_nibbles(*key), value.as_slice()))
            .collect();
        let mut nodes = BTreeMap::new();
        let root = build_node(&entries, 0, &mut nodes)?;
        debug_assert_eq!(root.hash, keccak256(&root.encoded));
        Ok(TrieSnapshot {
            root: root.hash,
            nodes,
            leaves: leaves.clone(),
        })
    }

    /// Loads and verifies a proof from any immutable node store.
    ///
    /// # Errors
    ///
    /// Returns [`StateError`] for missing, corrupt, or malformed nodes.
    pub fn proof<S: NodeStore>(root: B256, key: B256, store: &S) -> Result<StateProof, StateError> {
        if root == EMPTY_ROOT_HASH {
            return Ok(StateProof {
                root,
                key,
                value: None,
                nodes: Vec::new(),
            });
        }
        let target = unpack_nibbles(key);
        let mut encoded = load_hashed(store, root)?;
        let mut depth = 0;
        let mut proof = Vec::new();
        let value = loop {
            proof.push(Bytes::copy_from_slice(&encoded));
            match decode_node(&encoded)? {
                DecodedNode::Leaf { path, value } => {
                    break (target[depth..] == path).then_some(value);
                }
                DecodedNode::Extension { path, child } => {
                    if !target[depth..].starts_with(&path) {
                        break None;
                    }
                    depth += path.len();
                    encoded = resolve_child(store, child)?;
                }
                DecodedNode::Branch { children } => {
                    if depth >= SECURE_KEY_NIBBLES {
                        return Err(StateError::InvalidPath("branch beyond secure key"));
                    }
                    let Some(child) = children[target[depth] as usize].clone() else {
                        break None;
                    };
                    depth += 1;
                    encoded = resolve_child(store, child)?;
                }
            }
        };
        let proof = StateProof {
            root,
            key,
            value,
            nodes: proof,
        };
        proof.verify()?;
        Ok(proof)
    }

    /// Traverses a stored root and returns all secure leaves after checking every node hash.
    ///
    /// # Errors
    ///
    /// Returns [`StateError`] for missing/corrupt nodes, cycles, or malformed topology.
    pub fn collect_leaves<S: NodeStore>(
        root: B256,
        store: &S,
    ) -> Result<BTreeMap<B256, Vec<u8>>, StateError> {
        if root == EMPTY_ROOT_HASH {
            return Ok(BTreeMap::new());
        }
        let encoded = load_hashed(store, root)?;
        let mut leaves = BTreeMap::new();
        let mut visited = BTreeSet::from([root]);
        walk_node(
            store,
            &encoded,
            &mut Vec::with_capacity(SECURE_KEY_NIBBLES),
            &mut leaves,
            &mut visited,
        )?;
        Ok(leaves)
    }
}

/// Returns the secure account-trie key `keccak256(address)`.
#[must_use]
pub fn secure_account_key(address: Address) -> B256 {
    keccak256(address)
}

/// Returns the secure contract-storage key `keccak256(slot_be_32)`.
#[must_use]
pub fn secure_storage_key(slot: U256) -> B256 {
    keccak256(slot.to_be_bytes::<32>())
}

#[derive(Debug)]
struct BuiltNode {
    encoded: Vec<u8>,
    hash: B256,
}

fn build_node(
    entries: &[(Vec<u8>, &[u8])],
    depth: usize,
    nodes: &mut BTreeMap<B256, Vec<u8>>,
) -> Result<BuiltNode, StateError> {
    if entries.is_empty() || depth > SECURE_KEY_NIBBLES {
        return Err(StateError::InvalidPath("invalid recursive trie range"));
    }
    let encoded = if entries.len() == 1 {
        let path = entries[0]
            .0
            .get(depth..)
            .ok_or(StateError::InvalidPath("leaf depth"))?;
        encode_list(&[
            EncodedItem::Bytes(compact_encode(path, true)),
            EncodedItem::Bytes(entries[0].1.to_vec()),
        ])
    } else {
        let common = common_prefix(entries, depth);
        if common > 0 {
            let child = build_node(entries, depth + common, nodes)?;
            encode_list(&[
                EncodedItem::Bytes(compact_encode(&entries[0].0[depth..depth + common], false)),
                child_item(&child),
            ])
        } else {
            if depth >= SECURE_KEY_NIBBLES {
                return Err(StateError::InvalidPath("duplicate secure key"));
            }
            let mut items = Vec::with_capacity(17);
            for nibble in 0_u8..16 {
                let start = entries.partition_point(|entry| entry.0[depth] < nibble);
                let end = entries.partition_point(|entry| entry.0[depth] <= nibble);
                if start == end {
                    items.push(EncodedItem::Bytes(Vec::new()));
                } else {
                    let child = build_node(&entries[start..end], depth + 1, nodes)?;
                    items.push(child_item(&child));
                }
            }
            items.push(EncodedItem::Bytes(Vec::new()));
            encode_list(&items)
        }
    };
    let hash = keccak256(&encoded);
    nodes.insert(hash, encoded.clone());
    Ok(BuiltNode { encoded, hash })
}

fn common_prefix(entries: &[(Vec<u8>, &[u8])], depth: usize) -> usize {
    let first = &entries[0].0;
    let last = &entries[entries.len() - 1].0;
    first[depth..]
        .iter()
        .zip(&last[depth..])
        .take_while(|(left, right)| left == right)
        .count()
}

fn child_item(child: &BuiltNode) -> EncodedItem {
    if child.encoded.len() < 32 {
        EncodedItem::Raw(child.encoded.clone())
    } else {
        EncodedItem::Bytes(child.hash.to_vec())
    }
}

enum EncodedItem {
    Bytes(Vec<u8>),
    Raw(Vec<u8>),
}

fn encode_list(items: &[EncodedItem]) -> Vec<u8> {
    let mut payload = Vec::new();
    for item in items {
        match item {
            EncodedItem::Bytes(bytes) => bytes.as_slice().encode(&mut payload),
            EncodedItem::Raw(raw) => payload.extend_from_slice(raw),
        }
    }
    let mut out = Vec::with_capacity(payload.len() + 3);
    Header {
        list: true,
        payload_length: payload.len(),
    }
    .encode(&mut out);
    out.extend_from_slice(&payload);
    out
}

fn compact_encode(path: &[u8], leaf: bool) -> Vec<u8> {
    let odd = path.len() % 2 == 1;
    let flag = if leaf { 2_u8 } else { 0_u8 };
    let mut out = Vec::with_capacity(path.len().div_ceil(2) + 1);
    let mut index = 0;
    if odd {
        out.push(((flag | 1) << 4) | path[0]);
        index = 1;
    } else {
        out.push(flag << 4);
    }
    while index < path.len() {
        out.push((path[index] << 4) | path[index + 1]);
        index += 2;
    }
    out
}

fn compact_decode(bytes: &[u8]) -> Result<(Vec<u8>, bool), StateError> {
    let first = *bytes
        .first()
        .ok_or(StateError::InvalidPath("empty compact path"))?;
    let flag = first >> 4;
    if flag > 3 {
        return Err(StateError::InvalidPath("compact flag"));
    }
    let odd = flag & 1 == 1;
    if !odd && first & 0x0f != 0 {
        return Err(StateError::InvalidPath("non-canonical compact padding"));
    }
    let mut path = Vec::with_capacity(bytes.len() * 2);
    if odd {
        path.push(first & 0x0f);
    }
    for byte in &bytes[1..] {
        path.push(byte >> 4);
        path.push(byte & 0x0f);
    }
    Ok((path, flag & 2 == 2))
}

fn unpack_nibbles(key: B256) -> Vec<u8> {
    let mut out = Vec::with_capacity(SECURE_KEY_NIBBLES);
    for byte in key {
        out.push(byte >> 4);
        out.push(byte & 0x0f);
    }
    out
}

fn pack_nibbles(path: &[u8]) -> Result<B256, StateError> {
    if path.len() != SECURE_KEY_NIBBLES {
        return Err(StateError::InvalidPath("leaf is not a full secure key"));
    }
    let mut out = [0_u8; 32];
    for (index, pair) in path.chunks_exact(2).enumerate() {
        out[index] = (pair[0] << 4) | pair[1];
    }
    Ok(B256::from(out))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RlpKind {
    Bytes,
    List,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct RlpItem<'a> {
    pub(crate) raw: &'a [u8],
    pub(crate) payload: &'a [u8],
    pub(crate) kind: RlpKind,
}

pub(crate) fn decode_rlp_list(input: &[u8]) -> Result<Vec<RlpItem<'_>>, StateError> {
    let top = decode_rlp_item(input)?;
    if top.kind != RlpKind::List || top.raw.len() != input.len() {
        return Err(StateError::MalformedRlp("expected one exact list"));
    }
    let mut fields = Vec::new();
    let mut remaining = top.payload;
    while !remaining.is_empty() {
        let item = decode_rlp_item(remaining)?;
        fields.push(item);
        remaining = &remaining[item.raw.len()..];
    }
    Ok(fields)
}

fn decode_rlp_item(input: &[u8]) -> Result<RlpItem<'_>, StateError> {
    let first = *input
        .first()
        .ok_or(StateError::MalformedRlp("truncated item"))?;
    let (kind, offset, length) = match first {
        0x00..=0x7f => (RlpKind::Bytes, 0, 1),
        0x80..=0xb7 => (RlpKind::Bytes, 1, usize::from(first - 0x80)),
        0xb8..=0xbf => {
            let length_bytes = usize::from(first - 0xb7);
            let length = decode_rlp_length(input, length_bytes)?;
            if length < 56 {
                return Err(StateError::MalformedRlp("non-minimal long string"));
            }
            (RlpKind::Bytes, 1 + length_bytes, length)
        }
        0xc0..=0xf7 => (RlpKind::List, 1, usize::from(first - 0xc0)),
        0xf8..=0xff => {
            let length_bytes = usize::from(first - 0xf7);
            let length = decode_rlp_length(input, length_bytes)?;
            if length < 56 {
                return Err(StateError::MalformedRlp("non-minimal long list"));
            }
            (RlpKind::List, 1 + length_bytes, length)
        }
    };
    let end = offset
        .checked_add(length)
        .ok_or(StateError::MalformedRlp("length overflow"))?;
    let raw = input
        .get(..end)
        .ok_or(StateError::MalformedRlp("truncated payload"))?;
    let payload = &raw[offset..];
    if first == 0x81 && payload.first().is_some_and(|byte| *byte < 0x80) {
        return Err(StateError::MalformedRlp("non-minimal single byte"));
    }
    Ok(RlpItem { raw, payload, kind })
}

fn decode_rlp_length(input: &[u8], length_bytes: usize) -> Result<usize, StateError> {
    let bytes = input
        .get(1..1 + length_bytes)
        .ok_or(StateError::MalformedRlp("truncated length"))?;
    if bytes.first() == Some(&0) || length_bytes > std::mem::size_of::<usize>() {
        return Err(StateError::MalformedRlp("non-canonical length"));
    }
    let mut length = 0_usize;
    for byte in bytes {
        length = length
            .checked_mul(256)
            .and_then(|value| value.checked_add(usize::from(*byte)))
            .ok_or(StateError::MalformedRlp("length overflow"))?;
    }
    Ok(length)
}

#[derive(Clone, Debug)]
enum ChildReference {
    Hash(B256),
    Embedded(Vec<u8>),
}

enum DecodedNode {
    Leaf {
        path: Vec<u8>,
        value: Vec<u8>,
    },
    Extension {
        path: Vec<u8>,
        child: ChildReference,
    },
    Branch {
        children: Box<[Option<ChildReference>; 16]>,
    },
}

fn decode_node(encoded: &[u8]) -> Result<DecodedNode, StateError> {
    let fields = decode_rlp_list(encoded)?;
    if fields.len() == 2 {
        if fields[0].kind != RlpKind::Bytes {
            return Err(StateError::MalformedRlp("compact path type"));
        }
        let (path, leaf) = compact_decode(fields[0].payload)?;
        if path.is_empty() {
            return Err(StateError::InvalidPath("empty leaf/extension path"));
        }
        if leaf {
            if fields[1].kind != RlpKind::Bytes {
                return Err(StateError::MalformedRlp("leaf value type"));
            }
            Ok(DecodedNode::Leaf {
                path,
                value: fields[1].payload.to_vec(),
            })
        } else {
            Ok(DecodedNode::Extension {
                path,
                child: decode_child(fields[1])?
                    .ok_or(StateError::InvalidPath("empty extension"))?,
            })
        }
    } else if fields.len() == 17 {
        if fields[16].kind != RlpKind::Bytes || !fields[16].payload.is_empty() {
            return Err(StateError::InvalidPath("secure branch value must be empty"));
        }
        let mut children: [Option<ChildReference>; 16] = std::array::from_fn(|_| None);
        for (index, field) in fields[..16].iter().copied().enumerate() {
            children[index] = decode_child(field)?;
        }
        Ok(DecodedNode::Branch {
            children: Box::new(children),
        })
    } else {
        Err(StateError::MalformedRlp("node arity"))
    }
}

fn decode_child(item: RlpItem<'_>) -> Result<Option<ChildReference>, StateError> {
    match item.kind {
        RlpKind::List => {
            if item.raw.len() >= 32 {
                return Err(StateError::InvalidPath("oversized embedded node"));
            }
            Ok(Some(ChildReference::Embedded(item.raw.to_vec())))
        }
        RlpKind::Bytes if item.payload.is_empty() => Ok(None),
        RlpKind::Bytes if item.payload.len() == 32 => {
            Ok(Some(ChildReference::Hash(B256::from_slice(item.payload))))
        }
        RlpKind::Bytes => Err(StateError::InvalidPath("invalid child reference")),
    }
}

fn load_hashed<S: NodeStore>(store: &S, expected: B256) -> Result<Vec<u8>, StateError> {
    let encoded = store
        .get_node(expected)?
        .ok_or(StateError::MissingNode(expected))?;
    let actual = keccak256(&encoded);
    if actual != expected {
        return Err(StateError::NodeHashMismatch { expected, actual });
    }
    Ok(encoded)
}

fn resolve_child<S: NodeStore>(store: &S, child: ChildReference) -> Result<Vec<u8>, StateError> {
    match child {
        ChildReference::Hash(hash) => load_hashed(store, hash),
        ChildReference::Embedded(encoded) => Ok(encoded),
    }
}

fn walk_node<S: NodeStore>(
    store: &S,
    encoded: &[u8],
    path: &mut Vec<u8>,
    leaves: &mut BTreeMap<B256, Vec<u8>>,
    visited: &mut BTreeSet<B256>,
) -> Result<(), StateError> {
    match decode_node(encoded)? {
        DecodedNode::Leaf {
            path: suffix,
            value,
        } => {
            path.extend_from_slice(&suffix);
            let key = pack_nibbles(path)?;
            if leaves.insert(key, value).is_some() {
                return Err(StateError::InvalidPath("duplicate leaf"));
            }
            path.truncate(path.len() - suffix.len());
        }
        DecodedNode::Extension {
            path: extension,
            child,
        } => {
            path.extend_from_slice(&extension);
            walk_child(store, child, path, leaves, visited)?;
            path.truncate(path.len() - extension.len());
        }
        DecodedNode::Branch { children } => {
            if path.len() >= SECURE_KEY_NIBBLES {
                return Err(StateError::InvalidPath("branch depth"));
            }
            for (nibble, child) in children.into_iter().enumerate() {
                if let Some(child) = child {
                    path.push(u8::try_from(nibble).expect("branch nibble is below 16"));
                    walk_child(store, child, path, leaves, visited)?;
                    path.pop();
                }
            }
        }
    }
    Ok(())
}

fn walk_child<S: NodeStore>(
    store: &S,
    child: ChildReference,
    path: &mut Vec<u8>,
    leaves: &mut BTreeMap<B256, Vec<u8>>,
    visited: &mut BTreeSet<B256>,
) -> Result<(), StateError> {
    match child {
        ChildReference::Embedded(encoded) => walk_node(store, &encoded, path, leaves, visited),
        ChildReference::Hash(hash) => {
            if !visited.insert(hash) {
                return Err(StateError::InvalidPath("trie node cycle"));
            }
            let encoded = load_hashed(store, hash)?;
            walk_node(store, &encoded, path, leaves, visited)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaves(count: u8) -> BTreeMap<B256, Vec<u8>> {
        (0..count)
            .map(|index| (keccak256([index]), vec![index + 1; usize::from(index) + 1]))
            .collect()
    }

    #[test]
    fn roots_match_alloy_and_proofs_cover_presence_and_absence() {
        for count in 0..20 {
            let leaves = leaves(count);
            let snapshot = EthereumStateCommitment::build(&leaves).unwrap();
            let upstream = {
                let mut builder = alloy_trie::HashBuilder::default();
                for (key, value) in &leaves {
                    builder.add_leaf(Nibbles::unpack(*key), value);
                }
                builder.root()
            };
            assert_eq!(snapshot.root(), upstream);
            for key in leaves.keys().copied().chain([keccak256(b"missing")]) {
                snapshot.proof(key).unwrap().verify().unwrap();
            }
            assert_eq!(
                EthereumStateCommitment::collect_leaves(
                    snapshot.root(),
                    &MemoryNodeStore::new(snapshot.nodes().clone())
                )
                .unwrap(),
                leaves
            );
        }
    }

    #[test]
    fn corrupt_and_missing_nodes_are_rejected() {
        let snapshot = EthereumStateCommitment::build(&leaves(8)).unwrap();
        let mut missing = MemoryNodeStore::new(snapshot.nodes().clone());
        missing.remove(snapshot.root());
        assert!(matches!(
            EthereumStateCommitment::collect_leaves(snapshot.root(), &missing),
            Err(StateError::MissingNode(_))
        ));

        let mut corrupt = snapshot.nodes().clone();
        corrupt.get_mut(&snapshot.root()).unwrap()[0] ^= 1;
        assert!(matches!(
            EthereumStateCommitment::collect_leaves(
                snapshot.root(),
                &MemoryNodeStore::new(corrupt)
            ),
            Err(StateError::NodeHashMismatch { .. })
        ));
    }
}
