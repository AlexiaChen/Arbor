# Arbor

Arbor is an early Rust implementation of an account-based, EVM-compatible
blockchain in which one root PoS/BFT consensus finalizes batches for a tree of
logical domains. M0 protocol decisions, the M1 workspace baseline, M2 protocol
types/codecs/cryptography, M3 authenticated state/storage, and M4 single-domain
EVM execution are complete; M5 block production is next, and this is not a
runnable blockchain yet.
Production BFT work remains blocked by ADR-004's durable-signing gate.

Read [the architecture](doc/architecture.md), [implementation plan](doc/plan.md),
and [ADRs](doc/adr/README.md) before changing protocol boundaries. M2-M4
consensus-, state-, and execution-sensitive crates are recorded in
[protocol dependencies](doc/protocol/dependencies.md); fixed M4 rules are in
[the execution protocol](doc/protocol/execution.md).

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

Node execution is still a supervised lifecycle placeholder. The smoke gate
checks configuration/database initialization, schema/marker/root inspection,
and graceful SIGTERM shutdown. Authenticated state, durable storage, and the
in-process single-domain EVM service exist; block production, networking, and
consensus behavior arrive in later milestones.
P2P uses rust-libp2p when network implementation starts in M7.
