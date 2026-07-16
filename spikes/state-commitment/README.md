# State commitment spike

Disposable M0 experiment for ADR-003. It uses `alloy-trie` branch updates and
parity-db 0.5.5 content-addressed nodes. The verification run covers historical
roots, proof reconstruction after database reopen, an uncommitted crash
boundary, an atomic committed snapshot, and manifest-based mark-and-sweep
pruning.

```bash
cargo run --manifest-path spikes/state-commitment/Cargo.toml -- verify
cargo run --release --manifest-path spikes/state-commitment/Cargo.toml -- benchmark
```

The database uses a uniform hash-indexed column for 32-byte node hashes and a
B-tree column for roots, manifests, and proof indexes. `sync_wal` and
`sync_data` remain enabled. The benchmark builds 100,000 deterministic accounts,
changes 1,000, and reports roots, time, new content-node bytes, write
amplification, and disk use.

The in-memory hash-builder currently traverses the sorted leaf set for each
root, while persistence writes only branch encodings whose content hashes are
new. M3 must preserve that storage boundary and can optimize traversal without
changing the commitment.

This standalone workspace is deliberately excluded from production. parity-db
removes the RocksDB bindgen/libclang requirement; its ordinary Rust dependency
build still requires the platform C compiler used by compression dependencies.
