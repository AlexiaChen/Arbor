# State commitment spike

Disposable M0 experiment for ADR-003. It builds Ethereum-compatible MPT roots
and inclusion proofs with `alloy-trie`, persists immutable proof nodes and two
historical roots to RocksDB, closes/reopens the database, and prunes the first
root marker without invalidating the second root.

Run:

```bash
cargo run --manifest-path spikes/state-commitment/Cargo.toml
```

The RocksDB binding uses bindgen. Linux development images need a complete
clang/libclang installation. On a minimal image containing only a versioned
shared object, both `LIBCLANG_PATH` and the compiler resource include path may
need to be supplied explicitly.

This is deliberately excluded from the production workspace. Rebuilding a
snapshot currently replays sorted leaves, so this PoC validates semantics but
does not yet prove the write-amplification target for M3.
