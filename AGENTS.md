# Repository Guidelines

## Project Structure & Module Organization

This repository is a Rust 2024 Cargo workspace. `README.md` is the root overview, `doc/architecture.md` is the normative architecture, `doc/plan.md` tracks milestones, and `doc/adr/` records protocol decisions. Production crates live under `crates/`; disposable M0 experiments live in excluded standalone workspaces under `spikes/` and must not leak candidate dependencies into production crates. Keep fixtures under `testdata/` and shared process/fault helpers in `crates/arbor-testkit/`.

## Build, Test, and Development Commands

Use the stable toolchain pinned by `rust-toolchain.toml`. Required workspace gates are:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo nextest run --workspace --all-features
cargo deny check
```

Use `cargo fmt` before submitting changes and `cargo test -p <crate>` for focused runs. Run `scripts/check-doc-links.sh` and `scripts/check-forbidden-deps.sh` when changing documentation or dependencies. Candidate spikes have their own lockfiles and commands documented in each `spikes/*/README.md`.

## Coding Style & Naming Conventions

Use Rust 2024 idioms, `rustfmt`, and explicit typed errors with `thiserror` where appropriate. Crate names should use the `arbor-*` pattern. Rust modules, functions, and files use `snake_case`; types and traits use `PascalCase`; constants use `SCREAMING_SNAKE_CASE`. Keep consensus, state, network, keystore, and RPC boundaries separate. Do not introduce Substrate/Cosmos-style frameworks.

## Testing Guidelines

Core protocol code needs unit tests plus golden vectors for hashes, codec output, signatures, account state, block roots, validator sets, votes, QC, and finality proofs. Storage tests must use temporary directories. E2E tests should use `arbor-testkit` and cover single-validator dev chains, multi-node PoS/BFT finality, restarts, transaction propagation, domain creation, and EVM contract execution.

Do not weaken socket/process tests because a restricted agent sandbox rejects loopback binds; rerun those checks in an environment that permits local sockets. Every async or multi-process fixture must have an explicit timeout and process guard.

## Architecture Constraints

Follow the current design direction: EIP-2718/EVM account model, authenticated state persisted in parity-db, and one root PoS/BFT consensus that finalizes all logical child-chain domains. Domain creation is a root-domain `ChainRegistry` call. Do not add MPVSS/PVSS, PoW/DPoS, FnFnCoreWallet Template types, per-domain consensus, or legacy block types. C++ behavior is semantic reference material, not a compatibility target. Keep parity-db's synchronous WAL and data durability enabled for protocol commits; do not treat an in-memory commit overlay as durable completion.

ADR status is a hard implementation boundary. `Proposed` ADR-003/004 spike results may guide experiments but cannot justify production trie or BFT dependencies. A shared safety harness is not evidence that a third-party engine passed live four-node recovery tests.

## Commit & Pull Request Guidelines

The current history only contains `first commit`, so use concise imperative commit messages, for example `add consensus plan` or `implement account state codec`. PRs should describe scope, affected modules, tests run, and any protocol or storage compatibility impact. Link issues when available and include screenshots only for UI-facing changes.
