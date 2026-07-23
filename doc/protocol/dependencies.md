# M2-M7 protocol dependency record

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
| `libp2p` | 0.56.0 | crates.io / libp2p/rust-libp2p | MIT | M7 transport identity, Noise/TCP/Yamux, identify, ping, Kademlia, signed gossipsub, and bounded CBOR request-response; enabled features are exactly `cbor`, `ed25519`, `gossipsub`, `identify`, `kad`, `macros`, `noise`, `ping`, `request-response`, `tcp`, `tokio`, and `yamux` |
| `mdns-sd` | 0.20.2 | crates.io / keepsimple1/mdns-sd | Apache-2.0 OR MIT | M7 LAN discovery adapter; default features disabled, network/genesis application handshake remains authoritative |

The enabled `revm` precompile graph includes CC0-1.0 licensed
`aurora-engine-modexp`, `secp256k1`, and `secp256k1-sys`; `deny.toml` allows
that permissive license explicitly. No blanket crate or source exception is
used.

Arbor deliberately does not depend on `alloy-consensus`: Arbor owns its native
header/vote/QC types and limits, and importing that crate would also introduce
unneeded Ethereum consensus and trie dependencies. BFT candidate crates remain
outside the production workspace under ADR-004. M7 adds rust-libp2p directly
without importing either rejected BFT candidate or its network facade.

M5 adds no third-party production dependency. `arbor-chain` composes the
existing canonical codec, Keccak, executor, authenticated heads, and fixed EVM
adapter; `arbor-consensus` is an internal development-only engine over the
existing synchronous parity-db commit boundary. No rejected BFT candidate was
added to the workspace.

M7's `arbor-network` wire DTOs carry fixed hash arrays and already canonical
block bytes. CBOR frames are transport envelopes only: they do not determine
block, transaction, state, vote, or QC hashes. The codec enforces explicit
request/response byte limits before handing payloads to application validation.
The 0.56.0 libp2p `mdns` feature remains disabled because it pulls
`hickory-proto` 0.25.2, which fails `cargo deny` on RUSTSEC-2026-0118 and
RUSTSEC-2026-0119. Arbor does not waive those DoS advisories. M7 instead uses
the exact-pinned `mdns-sd` adapter, then authenticates every discovered peer
through the same full network/genesis/version handshake as Kademlia or explicit
peer dialing.

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
