use std::collections::BTreeMap;

use alloy_primitives::{B256, keccak256};
use arbor_primitives::DomainId;

use crate::StateError;

const KEY_TAG: &[u8] = b"ARBOR_DOMAIN_HEAD_KEY_V1";
const VALUE_TAG: &[u8] = b"ARBOR_DOMAIN_HEAD_VALUE_V1";
const LEAF_TAG: &[u8] = b"ARBOR_SMT_LEAF_V1";
const BRANCH_TAG: &[u8] = b"ARBOR_SMT_BRANCH_V1";
const SMT_DEPTH: usize = 256;

/// Finalized domain head committed by the global sparse Merkle map.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DomainHead {
    /// Finalized domain-block hash.
    pub domain_block_hash: B256,
    /// Corresponding Ethereum account-state root.
    pub state_root: B256,
}

/// Fixed-depth domain-head membership or non-membership proof.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DomainHeadProof {
    /// Sparse-map root.
    pub root: B256,
    /// Domain queried by this proof.
    pub domain_id: DomainId,
    /// Head value for membership; `None` proves non-membership.
    pub value: Option<DomainHead>,
    /// Sibling hashes ordered from root depth zero to leaf depth 255.
    pub siblings: Vec<B256>,
}

impl DomainHeadProof {
    /// Verifies the fixed-depth path and value binding.
    ///
    /// # Errors
    ///
    /// Returns [`StateError`] if the proof length or reconstructed root is invalid.
    pub fn verify(&self) -> Result<(), StateError> {
        if self.siblings.len() != SMT_DEPTH {
            return Err(StateError::InvalidPath("domain-head proof depth"));
        }
        let key = domain_head_key(self.domain_id);
        let mut hash = match self.value {
            Some(value) => leaf_hash(key, domain_head_value(value)),
            None => empty_hashes()[SMT_DEPTH],
        };
        for depth in (0..SMT_DEPTH).rev() {
            let sibling = self.siblings[depth];
            hash = if bit(key, depth) == 0 {
                branch_hash(depth, hash, sibling)
            } else {
                branch_hash(depth, sibling, hash)
            };
        }
        if hash != self.root {
            return Err(StateError::Proof("domain-head root mismatch".to_owned()));
        }
        Ok(())
    }
}

/// Deterministic 256-level sparse Merkle commitment for all active domain heads.
#[derive(Clone, Debug, Default)]
pub struct DomainHeadsCommitment {
    heads: BTreeMap<B256, (DomainId, DomainHead)>,
}

impl DomainHeadsCommitment {
    /// Constructs an empty commitment.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts or replaces one domain head.
    pub fn insert(&mut self, domain_id: DomainId, head: DomainHead) {
        self.heads
            .insert(domain_head_key(domain_id), (domain_id, head));
    }

    /// Removes a domain head.
    pub fn remove(&mut self, domain_id: DomainId) -> Option<DomainHead> {
        self.heads
            .remove(&domain_head_key(domain_id))
            .map(|(_, head)| head)
    }

    /// Returns the sparse-map root.
    #[must_use]
    pub fn root(&self) -> B256 {
        subtree_root(&self.entries(), 0, &empty_hashes())
    }

    /// Produces membership or non-membership proof for a domain.
    #[must_use]
    pub fn proof(&self, domain_id: DomainId) -> DomainHeadProof {
        let target = domain_head_key(domain_id);
        let entries = self.entries();
        let empties = empty_hashes();
        let mut siblings = Vec::with_capacity(SMT_DEPTH);
        let mut range = entries.as_slice();
        for depth in 0..SMT_DEPTH {
            let split = range.partition_point(|entry| bit(entry.0, depth) == 0);
            let (left, right) = range.split_at(split);
            if bit(target, depth) == 0 {
                siblings.push(subtree_root(right, depth + 1, &empties));
                range = left;
            } else {
                siblings.push(subtree_root(left, depth + 1, &empties));
                range = right;
            }
        }
        let value = self.heads.get(&target).map(|(_, head)| *head);
        DomainHeadProof {
            root: self.root(),
            domain_id,
            value,
            siblings,
        }
    }

    fn entries(&self) -> Vec<(B256, B256)> {
        self.heads
            .iter()
            .map(|(key, (_, head))| (*key, leaf_hash(*key, domain_head_value(*head))))
            .collect()
    }
}

fn domain_head_key(domain_id: DomainId) -> B256 {
    let mut preimage = Vec::with_capacity(KEY_TAG.len() + 32);
    preimage.extend_from_slice(KEY_TAG);
    preimage.extend_from_slice(domain_id.0.as_slice());
    keccak256(preimage)
}

fn domain_head_value(head: DomainHead) -> B256 {
    let mut preimage = Vec::with_capacity(VALUE_TAG.len() + 64);
    preimage.extend_from_slice(VALUE_TAG);
    preimage.extend_from_slice(head.domain_block_hash.as_slice());
    preimage.extend_from_slice(head.state_root.as_slice());
    keccak256(preimage)
}

fn leaf_hash(key: B256, value: B256) -> B256 {
    let mut preimage = Vec::with_capacity(LEAF_TAG.len() + 65);
    preimage.extend_from_slice(LEAF_TAG);
    preimage.push(1);
    preimage.extend_from_slice(key.as_slice());
    preimage.extend_from_slice(value.as_slice());
    keccak256(preimage)
}

fn branch_hash(depth: usize, left: B256, right: B256) -> B256 {
    let mut preimage = Vec::with_capacity(BRANCH_TAG.len() + 66);
    preimage.extend_from_slice(BRANCH_TAG);
    preimage.extend_from_slice(
        &u16::try_from(depth)
            .expect("SMT depth never exceeds 256")
            .to_be_bytes(),
    );
    preimage.extend_from_slice(left.as_slice());
    preimage.extend_from_slice(right.as_slice());
    keccak256(preimage)
}

fn empty_hashes() -> [B256; SMT_DEPTH + 1] {
    let mut hashes = [B256::ZERO; SMT_DEPTH + 1];
    let mut preimage = Vec::with_capacity(LEAF_TAG.len() + 1);
    preimage.extend_from_slice(LEAF_TAG);
    preimage.push(0);
    hashes[SMT_DEPTH] = keccak256(preimage);
    for depth in (0..SMT_DEPTH).rev() {
        hashes[depth] = branch_hash(depth, hashes[depth + 1], hashes[depth + 1]);
    }
    hashes
}

fn subtree_root(entries: &[(B256, B256)], depth: usize, empties: &[B256; 257]) -> B256 {
    if entries.is_empty() {
        return empties[depth];
    }
    if depth == SMT_DEPTH {
        return entries[0].1;
    }
    let split = entries.partition_point(|entry| bit(entry.0, depth) == 0);
    branch_hash(
        depth,
        subtree_root(&entries[..split], depth + 1, empties),
        subtree_root(&entries[split..], depth + 1, empties),
    )
}

fn bit(key: B256, depth: usize) -> u8 {
    (key[depth / 8] >> (7 - depth % 8)) & 1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(byte: u8) -> DomainId {
        DomainId(B256::repeat_byte(byte))
    }

    fn head(byte: u8) -> DomainHead {
        DomainHead {
            domain_block_hash: B256::repeat_byte(byte),
            state_root: B256::repeat_byte(byte.wrapping_add(1)),
        }
    }

    #[test]
    fn membership_nonmembership_and_order_are_stable() {
        let mut first = DomainHeadsCommitment::new();
        first.insert(id(1), head(1));
        first.insert(id(2), head(2));
        let mut second = DomainHeadsCommitment::new();
        second.insert(id(2), head(2));
        second.insert(id(1), head(1));
        assert_eq!(first.root(), second.root());
        first.proof(id(1)).verify().unwrap();
        first.proof(id(3)).verify().unwrap();

        let mut tampered = first.proof(id(1));
        tampered.value = Some(head(9));
        assert!(tampered.verify().is_err());
    }
}
