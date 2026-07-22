# Arbor

Arbor is an early Rust implementation of an account-based, EVM-compatible
blockchain in which one root PoS/BFT consensus finalizes batches for a tree of
logical domains. M0 protocol decisions, the M1 workspace baseline, M2 protocol
types/codecs/cryptography, M3 authenticated state/storage, M4 single-domain
EVM execution, and M5 deterministic block production/development finality are
complete. A local development chain now runs continuously; M6 tree-domain
creation is next.
Production BFT work remains blocked by ADR-004's durable-signing gate.

Read [the architecture](doc/architecture.md), [implementation plan](doc/plan.md),
and [ADRs](doc/adr/README.md) before changing protocol boundaries. M2-M4
consensus-, state-, execution-, and block-sensitive crates are recorded in
[protocol dependencies](doc/protocol/dependencies.md); fixed M4 rules are in
[the execution protocol](doc/protocol/execution.md), and fixed M5 rules are in
[the block protocol](doc/protocol/blocks.md).

## Workspace checks

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
bash scripts/check-m1-smoke.sh
bash scripts/check-m5-smoke.sh
```

CI additionally runs `cargo nextest`, `cargo deny`, documentation-link checks,
forbidden dependency checks, and canonical-vector tests on Linux aarch64.

The single operator entry point is built as `arbor`:

```bash
cargo run -p arbor-cli -- --help
```

The ordinary node mode remains a supervised assembly placeholder. M5 adds an
explicit local-only validator path:

```bash
cargo run -p arbor-cli -- node init --dev --data-dir ./tmp/node1
cargo run -p arbor-cli -- node run --dev-validator --data-dir ./tmp/node1
```

It continuously constructs, validates, durably commits, and announces local
finalized blocks, and shuts down cleanly on SIGTERM/Ctrl-C. Raw transaction and
receipt behavior is currently exposed through the in-process engine API and
integration tests; HTTP/WebSocket JSON-RPC remains M9. This dev engine is not
PoS/BFT and cannot be enabled in production mode.
P2P uses rust-libp2p when network implementation starts in M7.
