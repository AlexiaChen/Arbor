# M2-M5 protocol dependency record

Consensus-sensitive dependencies are exact-pinned in the workspace manifest and
locked in `Cargo.lock`.

| Crate | Version | Source | License | Enabled role |
| --- | --- | --- | --- | --- |
| `alloy-primitives` | 1.6.1 | crates.io / alloy-rs/core | MIT OR Apache-2.0 | `Address`, `B256`, `Bloom`, `Bytes`, `U256`, Keccak, EIP-1559 recovery; only `std`, `k256`, and `rlp` features |
| `alloy-rlp` | 0.3.16 | crates.io / alloy-rs/core | MIT OR Apache-2.0 | Explicit Ethereum EIP-1559 transaction and receipt RLP |
| `k256` | 0.13.4 | crates.io / RustCrypto/elliptic-curves | MIT OR Apache-2.0 | Deterministic low-s secp256k1 validator signatures; version aligned with `alloy-primitives` |
| `alloy-trie` | 0.9.5 | crates.io / alloy-rs/trie | MIT OR Apache-2.0 | Independent Ethereum MPT root/proof verification and account semantics; only `ethereum` and `std` features |
| `parity-db` | 0.5.5 | crates.io / paritytech/parity-db | MIT OR Apache-2.0 | Synchronous-WAL/data durable atomic storage for immutable trie nodes, manifests, roots, receipts, and indexes |
| `revm` | 41.0.0 | crates.io / bluealloy/revm | MIT | Protocol revision 1 EVM interpreter fixed to Shanghai; default features disabled, only `std`, portable crypto, and secp256k1 precompiles enabled |

The enabled `revm` precompile graph includes CC0-1.0 licensed
`aurora-engine-modexp`, `secp256k1`, and `secp256k1-sys`; `deny.toml` allows
that permissive license explicitly. No blanket crate or source exception is
used.

Arbor deliberately does not depend on `alloy-consensus`: Arbor owns its native
header/vote/QC types and limits, and importing that crate would also introduce
unneeded Ethereum consensus and trie dependencies. BFT candidate crates remain
outside the production workspace under ADR-004. rust-libp2p remains an M7
dependency.

M5 adds no third-party production dependency. `arbor-chain` composes the
existing canonical codec, Keccak, executor, authenticated heads, and fixed EVM
adapter; `arbor-consensus` is an internal development-only engine over the
existing synchronous parity-db commit boundary. No rejected BFT candidate was
added to the workspace.

Any upgrade must rerun the committed EIP-1559 cross-check,
canonical/state/execution root vectors, fixed Shanghai subset, proof
cross-checks, parity-db reopen/kill/pruning tests, debug/release tests, Linux
aarch64 vector job, Clippy, and `cargo deny`. A `revm` crate upgrade does not
change `ProtocolSpec::V1` away from Shanghai; that requires a future scheduled
protocol revision and new vectors.

## Tracked advisory exception

`alloy-primitives` 1.6.1 transitively depends on `paste` 1.0.15. RUSTSEC-2024-0436
marks `paste` unmaintained but reports no vulnerability and offers no safe
upgrade. `deny.toml` therefore ignores that exact advisory ID only. The ignore
must be removed as soon as an exact-pinned `alloy-primitives` release removes
the dependency; unmaintained advisories remain enabled globally.
