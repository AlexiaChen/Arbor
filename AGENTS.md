# Repository Guidelines

## Project Structure & Module Organization

This repository is currently documentation-first. `README.md` is the root overview, while `doc/architecture.md` captures the target Rust architecture and `doc/plan.md` captures implementation milestones. Planned source code should live under `crates/` as a Cargo workspace, with modules such as `arbor-primitives`, `arbor-state`, `arbor-storage`, `arbor-evm`, `arbor-consensus`, `arbor-network`, and `arbor-node`. Keep test fixtures under `testdata/` and multi-node helpers under `crates/arbor-testkit/` once implementation begins.

## Build, Test, and Development Commands

No Cargo workspace exists yet. After `Cargo.toml` is added, use the planned standard commands:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Use `cargo fmt` before submitting changes. Use `cargo test -p <crate>` for focused test runs once crates exist. For documentation-only edits, review Markdown rendering and keep examples consistent with `doc/plan.md`.

## Coding Style & Naming Conventions

Use Rust 2024 idioms, `rustfmt`, and explicit typed errors with `thiserror` where appropriate. Crate names should use the `arbor-*` pattern. Rust modules, functions, and files use `snake_case`; types and traits use `PascalCase`; constants use `SCREAMING_SNAKE_CASE`. Keep consensus, state, network, keystore, and RPC boundaries separate. Do not introduce Substrate/Cosmos-style frameworks.

## Testing Guidelines

Core protocol code needs unit tests plus golden vectors for hashes, codec output, signatures, account state, block roots, validator sets, votes, QC, and finality proofs. Storage tests must use temporary directories. E2E tests should use `arbor-testkit` and cover single-validator dev chains, multi-node PoS/BFT finality, restarts, transaction propagation, domain creation, and EVM contract execution.

## Architecture Constraints

Follow the current design direction: EIP-2718/EVM account model, authenticated state persisted in RocksDB, and one root PoS/BFT consensus that finalizes all logical child-chain domains. Domain creation is a root-domain `ChainRegistry` call. Do not add MPVSS/PVSS, PoW/DPoS, FnFnCoreWallet Template types, per-domain consensus, or legacy block types. C++ behavior is semantic reference material, not a compatibility target.

## Commit & Pull Request Guidelines

The current history only contains `first commit`, so use concise imperative commit messages, for example `add consensus plan` or `implement account state codec`. PRs should describe scope, affected modules, tests run, and any protocol or storage compatibility impact. Link issues when available and include screenshots only for UI-facing changes.
