# ADR-003: Ethereum MPT state and sparse domain-head commitment

- Status: Proposed
- Date: 2026-07-15

## Context

RocksDB persistence and authenticated state are separate concerns. Domain state
should retain Ethereum proof semantics, while the global domain-head map needs
fixed-depth membership and non-membership proofs.

## Proposed decision

- Domain accounts and contract storage use Ethereum's Keccak/RLP Merkle Patricia
  Trie with secure hashed keys.
- Immutable encoded trie nodes are stored by node hash in RocksDB. Historical
  roots are retained according to archive/full policy.
- `domain_heads_root` uses a 256-level binary sparse Merkle map keyed by
  `keccak256("ARBOR_DOMAIN_HEAD_KEY_V1" || domain_id)`. A leaf commits to
  `keccak256("ARBOR_DOMAIN_HEAD_VALUE_V1" || domain_block_hash || state_root)`.
- Empty hashes and leaf/branch domain separation are fixed in
  [protocol constants](../protocol/constants.md).

## Spike evidence

`spikes/state-commitment` uses `alloy-trie` 0.9.5 and RocksDB 0.24.0. It creates
two roots from an updated leaf set, verifies inclusion proofs, writes immutable
proof nodes and historical root markers atomically, reopens RocksDB, reads both
roots, and prunes the first root marker without damaging the second.

The experiment rebuilds from sorted leaves. It therefore passes root/proof and
restart semantics but does not yet prove production incremental-update cost,
complete node reachability, or safe trie-node garbage collection.

## Promotion gate

Accept only after the M3-oriented extension demonstrates branch-level
incremental updates, historical proofs after restart, reference-count or
mark-and-sweep pruning, crash injection around the commit marker, and a recorded
100,000-account write-amplification benchmark. Until then, no trie crate becomes
a production dependency.

