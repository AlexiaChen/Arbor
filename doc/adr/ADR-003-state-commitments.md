# ADR-003: Ethereum MPT state, parity-db, and sparse domain-head commitment

- Status: Accepted
- Date: 2026-07-16

## Context

Persistent storage and authenticated state are separate concerns. Domain state
must retain Ethereum proof semantics, while the global domain-head map needs
fixed-depth membership and non-membership proofs. The original RocksDB proposal
also imposed a C++/bindgen/libclang build chain despite a Rust database designed
for blockchain trie workloads being available.

## Decision

- Domain accounts and contract storage use Ethereum's Keccak/RLP Merkle Patricia
  Trie with secure hashed keys, initially through `alloy-trie` 0.9.5.
- parity-db 0.5.5 is the persistence engine. Immutable encoded trie nodes use a
  uniform hash-indexed column keyed by 32-byte content hash. Roots, reachability
  manifests, proofs, block metadata, and indexes use separately configured
  columns; ordered scans use B-tree columns only where needed.
- `sync_wal` and `sync_data` stay enabled for protocol commits. Trie nodes,
  manifests, block/receipt/index changes, and the finalized commit marker share
  one atomic parity-db transaction.
- Historical roots are retained according to archive/full policy. Full pruning
  uses reachability manifests and mark-and-sweep; a root marker is never deleted
  without accounting for its reachable nodes.
- `domain_heads_root` uses a 256-level binary sparse Merkle map keyed by
  `keccak256("ARBOR_DOMAIN_HEAD_KEY_V1" || domain_id)`. A leaf commits to
  `keccak256("ARBOR_DOMAIN_HEAD_VALUE_V1" || domain_block_hash || state_root)`.
  Empty hashes and leaf/branch separation are fixed in
  [protocol constants](../protocol/constants.md).

## Spike evidence

`spikes/state-commitment` produces stable MPT roots, persists content-addressed
branch/proof nodes and two historical roots atomically, closes and reopens the
database, reconstructs and verifies historical proofs, injects a crash before
transaction submission, and prunes one manifest without damaging the retained
root. The second snapshot writes only content hashes absent from parity-db.

The [2026-07-16 release benchmark](../../spikes/state-commitment/results/2026-07-16-parity-db.md) on the development host used 100,000 accounts
and updated 1,000: initial root build 116 ms, initial database submission 23 ms,
7,487 new nodes / 1,251,836 node bytes; update root build 115 ms, update database
submission 12 ms, 1,012 new nodes / 315,610 node bytes. Against 32,000 logical
value bytes, measured node-byte write amplification was 9.86x; temporary
database disk use was 40,126,235 bytes. The fixed roots are recorded in the
spike output and README command is reproducible.

## Consequences and M3 boundary

The spike accepts parity-db and the MPT commitment without introducing either
dependency into the production workspace before M3. It removes RocksDB and its
libclang build requirement from the architecture. M3 must add schema/version
checks, subprocess kill injection during parity-db's commit pipeline, corrupt
log/node tests, archive/full retention policy, and a traversal optimization;
those are production hardening tasks, not reasons to reopen this M0 choice.
