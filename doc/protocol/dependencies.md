# M2 protocol dependency record

Consensus-sensitive dependencies are exact-pinned in the workspace manifest and
locked in `Cargo.lock`.

| Crate | Version | Source | License | Enabled role |
| --- | --- | --- | --- | --- |
| `alloy-primitives` | 1.6.1 | crates.io / alloy-rs/core | MIT OR Apache-2.0 | `Address`, `B256`, `Bloom`, `Bytes`, `U256`, Keccak, EIP-1559 recovery; only `std`, `k256`, and `rlp` features |
| `alloy-rlp` | 0.3.16 | crates.io / alloy-rs/core | MIT OR Apache-2.0 | Explicit Ethereum EIP-1559 transaction and receipt RLP |
| `k256` | 0.13.4 | crates.io / RustCrypto/elliptic-curves | MIT OR Apache-2.0 | Deterministic low-s secp256k1 validator signatures; version aligned with `alloy-primitives` |

M2 deliberately does not depend on `alloy-consensus`: Arbor owns its native
header/vote/QC types and limits, and importing that crate would also introduce
unneeded Ethereum consensus and trie dependencies. BFT candidate crates remain
outside the production workspace under ADR-004. rust-libp2p remains an M7
dependency.

Any upgrade must rerun the committed EIP-1559 cross-check, 30 canonical hash
vectors, debug/release tests, Linux aarch64 vector job, Clippy, and `cargo deny`.

## Tracked advisory exception

`alloy-primitives` 1.6.1 transitively depends on `paste` 1.0.15. RUSTSEC-2024-0436
marks `paste` unmaintained but reports no vulnerability and offers no safe
upgrade. `deny.toml` therefore ignores that exact advisory ID only. The ignore
must be removed as soon as an exact-pinned `alloy-primitives` release removes
the dependency; unmaintained advisories remain enabled globally.
