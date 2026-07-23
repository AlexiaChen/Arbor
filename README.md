# Arbor

Arbor is an early Rust implementation of an account-based, EVM-compatible
blockchain in which one root PoS/BFT consensus finalizes batches for a tree of
logical domains. M0 protocol decisions, the M1 workspace baseline, M2 protocol
types/codecs/cryptography, M3 authenticated state/storage, M4 single-domain
EVM execution, M5 deterministic block production/development finality, and M6
tree domains are complete. M6 includes the journaled root `ChainRegistry`,
deposit refund/burn authorization, deterministic tree-domain genesis,
multi-domain scheduling/proofs, node-local history projection, atomic
persistence/replay, and a dev-only two-level CLI acceptance path. General public
RPC/CLI and production keystores remain M9 work.
Production BFT work remains blocked by ADR-004's durable-signing gate.
M7 is complete for the explicitly non-BFT development mode: rust-libp2p
transport/discovery, persisted peer identity, genesis-bound handshake, four-level
sync, authenticated multi-domain checkpoints, node/CLI assembly, reconnect, and
bounded fault handling are covered by real-listener tests. This does not unblock
the production BFT path or satisfy ADR-004.

Read [the architecture](doc/architecture.md), [implementation plan](doc/plan.md),
and [ADRs](doc/adr/README.md) before changing protocol boundaries. M2-M4
consensus-, state-, execution-, and block-sensitive crates are recorded in
[protocol dependencies](doc/protocol/dependencies.md); fixed M4 rules are in
[the execution protocol](doc/protocol/execution.md), and fixed M5 rules are in
[the block protocol](doc/protocol/blocks.md). M6 rules are fixed in [the domain
protocol](doc/protocol/domains.md). The current M7 wire and block-sync boundary
is documented in [the network protocol](doc/protocol/network.md).

## Workspace checks

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
bash scripts/check-m1-smoke.sh
bash scripts/check-m5-smoke.sh
bash scripts/check-m6-smoke.sh
bash scripts/check-m7-smoke.sh
```

CI additionally runs `cargo nextest`, `cargo deny`, documentation-link checks,
forbidden dependency checks, and canonical-vector tests on Linux aarch64.

The single operator entry point is built as `arbor`:

```bash
cargo run -p arbor-cli -- --help
```

M5 adds an explicit local-only validator path:

```bash
cargo run -p arbor-cli -- node init --dev --data-dir ./tmp/node1
cargo run -p arbor-cli -- node run --dev-validator --data-dir ./tmp/node1
```

It continuously constructs, validates, durably commits, and announces local
finalized blocks, and shuts down cleanly on SIGTERM/Ctrl-C. Running the same
development config without `--dev-validator` starts a sync-only full listener.
`network.listen_addr`, `network.mdns`, and `network.persistent_peers` configure
discovery and reconnect; the node keeps its libp2p key at
`<data-dir>/network/peer.key`. Raw transaction and receipt behavior is currently
exposed through the in-process engine API and integration tests;
HTTP/WebSocket JSON-RPC remains M9. Neither development node mode is PoS/BFT.
