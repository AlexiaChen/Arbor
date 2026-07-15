# Arbor

Arbor is an early Rust implementation of an account-based, EVM-compatible
blockchain in which one root PoS/BFT consensus finalizes batches for a tree of
logical domains. The project is currently at the M0 protocol-spike and M1
workspace-baseline stage; it is not a runnable blockchain yet.

Read [the architecture](doc/architecture.md), [implementation plan](doc/plan.md),
and [ADRs](doc/adr/README.md) before changing protocol boundaries.

## Workspace checks

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

CI additionally runs `cargo nextest`, `cargo deny`, documentation-link checks,
forbidden dependency checks, and canonical-vector tests on Linux aarch64.

The single operator entry point is built as `arbor`:

```bash
cargo run -p arbor-cli -- --help
```

Node execution is only a supervised lifecycle placeholder in M1. Storage,
execution, networking, and consensus behavior arrive in later milestones.
