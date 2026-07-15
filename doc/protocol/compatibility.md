# Compatibility and support policy

- The supported production target is Linux x86_64 on the current pinned stable
  Rust toolchain (`rust-version = 1.97` at M1 completion).
- Linux aarch64 is a canonical-vector verification target, not yet a supported
  production platform.
- Arbor v1 offers no compatibility with FnFnCoreWallet network messages,
  addresses, blocks, wallet files, UTXO set, or database format.
- Consensus compatibility is defined by protocol version, canonical vectors,
  `ProtocolSpec`, genesis/network ID, and activated upgrade schedule.
- Unknown protocol or codec versions are rejected. Nodes do not guess execution
  rules from their installed dependency versions.
- Database schemas are versioned separately. Opening a newer schema with an
  older binary fails closed; migrations are explicit and restartable.
- Public RPC compatibility is versioned independently from consensus. Derived
  RPC fields cannot enter consensus roots.
- Dependency upgrades affecting EVM, trie, crypto, or BFT behavior require the
  same fixed fixtures before and after the update and an ADR when bytes change.

