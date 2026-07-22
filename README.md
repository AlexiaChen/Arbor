# Arbor

Arbor is an early Rust implementation of an account-based, EVM-compatible
blockchain in which one root PoS/BFT consensus finalizes batches for a tree of
logical domains. M0 protocol decisions, the M1 workspace baseline, and M2
protocol types/codecs/cryptography are complete; M3 state/storage is next, and
this is not a runnable blockchain yet. Production BFT work remains blocked by
ADR-004's durable-signing gate.

Read [the architecture](doc/architecture.md), [implementation plan](doc/plan.md),
and [ADRs](doc/adr/README.md) before changing protocol boundaries. M2's
consensus-sensitive crates are recorded in
[protocol dependencies](doc/protocol/dependencies.md).

## Workspace checks

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
bash scripts/check-m1-smoke.sh
```

CI additionally runs `cargo nextest`, `cargo deny`, documentation-link checks,
forbidden dependency checks, and canonical-vector tests on Linux aarch64.

The single operator entry point is built as `arbor`:

```bash
cargo run -p arbor-cli -- --help
```

Node execution is only a supervised lifecycle placeholder in M1. The smoke gate
checks configuration initialization, database inspection, and graceful SIGTERM
shutdown. Storage, execution, networking, and consensus behavior arrive in
later milestones. P2P uses rust-libp2p when network implementation starts in M7.
