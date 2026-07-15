# Project Learnings

> Append-only knowledge base maintained during issue processing.
> The agent reads this before starting each issue to avoid repeating mistakes.
> Human edits welcome — add, annotate, or mark as [OBSOLETE].

---

### L-001: [architecture] Arbor v1 is account-based, not UTXO (2026-07-15)
- **Issue**: Initial architecture planning.
- **Trigger**: UTXO, transaction model, account, EVM, state transition
- **Pattern**: Arbor deliberately diverges from FnFnCoreWallet's UTXO model. The first version uses an Ethereum-like account/state model with `nonce`, `balance`, `code_hash`, contract storage, receipts, logs, and state roots so EVM support remains viable.
- **Evidence**: `doc/architecture.md`, `doc/plan.md`
- **Confidence**: 10/10
- **Action**: Do not design new features around UTXO inputs/outputs or spend scripts. Model value movement as account state transitions.

### L-002: [architecture] Do not reintroduce MPVSS/PVSS, PoW, or old delegate consensus (2026-07-15)
- **Issue**: Consensus redesign.
- **Trigger**: consensus, PoW, delegate, PVSS, MPVSS, mpvss-rs
- **Pattern**: The old C++ consensus path is intentionally not being migrated. Arbor v1 targets PoS plus BFT finality, with single-validator finality only for local development.
- **Evidence**: `doc/architecture.md` section 7; `doc/plan.md` M5 and M8.
- **Confidence**: 10/10
- **Action**: Do not add `mpvss-rs`, mining RPCs such as `getwork`/`submitwork`, or delegate/PVSS milestones. Put consensus work behind `arbor-consensus`.

### L-003: [architecture] State storage is RocksDB, not FnFnCoreWallet LevelDB (2026-07-15)
- **Issue**: Storage planning.
- **Trigger**: storage, database, LevelDB, RocksDB, state, contract storage
- **Pattern**: Arbor uses authenticated account/storage tries and a domain-head commitment, with trie nodes, code, receipts, indexes, WAL, and caches persisted in RocksDB column families. The old LevelDB wrappers and file layout are reference material only.
- **Evidence**: `doc/architecture.md` section 6; `doc/plan.md` M3.
- **Confidence**: 10/10
- **Action**: Use RocksDB abstractions and temporary directories in tests. Do not reuse FnFnCoreWallet database wrappers or on-disk layout.

### L-004: [convention] Architecture and plan must stay in sync (2026-07-15)
- **Issue**: Documentation-first phase.
- **Trigger**: architecture.md, plan.md, milestone, dependency, protocol decision
- **Pattern**: This repository currently has design docs before implementation. A protocol decision is incomplete if it updates only one document; stale milestones can mislead future agents into rebuilding rejected paths.
- **Evidence**: `doc/architecture.md`, `doc/plan.md`
- **Confidence**: 9/10
- **Action**: When changing core direction, update both architecture and implementation plan, then search for stale terms before finishing.

### L-005: [convention] Do not claim current build commands until Cargo exists (2026-07-15)
- **Issue**: Repository bootstrap.
- **Trigger**: cargo, workspace, CI, build, test, clippy
- **Pattern**: The repo is currently documentation-first. Cargo commands in docs are planned gates, not executable current-state checks until `Cargo.toml` and crates are added.
- **Evidence**: Current root contains `README.md`, `AGENTS.md`, `LEARNINGS.md`, and `doc/`.
- **Confidence**: 10/10
- **Action**: Before running or documenting implementation commands as current behavior, check for `Cargo.toml`. Mark commands as planned when the workspace does not exist yet.

### L-006: [architecture] Do not carry over FnFnCoreWallet Template design (2026-07-15)
- **Issue**: Fork creation and authorization redesign.
- **Trigger**: template, makeorigin, fork create, multisig, weighted, validator template
- **Pattern**: FnFnCoreWallet used special Template objects for fork creation, delegated consensus, proof ownership, multisig, and weighted authorization. Arbor intentionally does not migrate this layer; it is an old pre-contract abstraction.
- **Evidence**: `doc/architecture.md` sections 4-5; `doc/plan.md` M6.
- **Confidence**: 10/10
- **Action**: Do not add `arbor-template`, `maketemplate`, `addnewtemplate`, or Template-backed fork creation. Use ordinary EIP-1559 transactions calling root-domain `ChainRegistry`, `Staking`, or future EVM contracts.

### L-007: [architecture] Child domains inherit root BFT finality (2026-07-15)
- **Issue**: Tree-chain security model review.
- **Trigger**: child chain, domain, validator set, consensus instance, extended, piggyback, vacant
- **Pattern**: Arbor v1 runs one root PoS/BFT consensus. A consensus block orders batches for multiple domains, and each domain result inherits finality through an inclusion proof. Domains do not start independent validator sets or BFT engines.
- **Evidence**: `doc/architecture.md` section 3; `doc/plan.md` M5-M8.
- **Confidence**: 10/10
- **Action**: Do not port `Extended`, `Piggyback`, or `Vacant` blocks. Keep domain-local state and block numbering, but route creation, staking, validator sets, and governance through the root domain.
