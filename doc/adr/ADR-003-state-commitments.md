# ADR-003: Ethereum MPT state, parity-db, and sparse domain-head commitment

- Status: Accepted
- Date: 2026-07-16
- Updated: 2026-07-22 (M3 production implementation)

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

The fixed sparse-map encoding is:

- present leaf: `keccak256("ARBOR_SMT_LEAF_V1" || 0x01 || key_hash || value_hash)`;
- empty leaf: `keccak256("ARBOR_SMT_LEAF_V1" || 0x00)`;
- branch at depth `d`: `keccak256("ARBOR_SMT_BRANCH_V1" || d_be_u16 || left || right)`.

Empty hashes recurse from depth 256 to zero. Proofs contain exactly 256 sibling
hashes ordered root-to-leaf.

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

## M3 implementation evidence

Production `arbor-state` now materializes every standard Ethereum RLP MPT node,
persists nodes by Keccak content hash, traverses persisted roots without a flat
cache, and verifies generated inclusion/non-membership proofs through
`alloy-trie`'s independent verifier. Fixed state/storage/domain-head vectors
cross-check the commitment in debug, release, and the aarch64 CI job.

Production `arbor-storage` uses application schema version 1, binds a database
to network/genesis identity, keeps `sync_wal` and `sync_data` enabled, and
submits trie nodes, reachability manifests, flat-cache changes, receipts,
indexes, heads, and the finalized marker in one transaction. Tests cover reopen,
schema mismatch, missing/corrupt nodes, malformed trailing logs, historical
proofs, full retention, flat-cache reconstruction, and process kills at four
commit timings. A durable commit is never reported as failed solely because
best-effort post-commit pruning needs retry.

The [2026-07-22 M3 production benchmark](../benchmarks/2026-07-22-m3-state-storage.md)
records the current implementation without setting a performance promise.

## Consequences

M3 promotes exact-pinned `alloy-trie` and parity-db behind Arbor-owned state and
storage boundaries. It removes RocksDB and its libclang build requirement from
the architecture. Future schema changes require an explicit migration edge and
old-database reopen/rollback exercise; a dependency upgrade must preserve the
fixed vectors and crash behavior.
