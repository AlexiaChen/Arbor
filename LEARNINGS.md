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

### L-003: [architecture] State storage is parity-db, not FnFnCoreWallet LevelDB (updated 2026-07-16)
- **Issue**: Storage planning initially selected RocksDB before comparing a blockchain-specific Rust store.
- **Trigger**: storage, database, LevelDB, RocksDB, parity-db, state, contract storage
- **Pattern**: Arbor uses authenticated account/storage tries and a domain-head commitment, with trie nodes, code, receipts, indexes, safety state, and caches persisted in parity-db columns. The old LevelDB wrappers and file layout are reference material only.
- **Evidence**: `doc/architecture.md` section 6; `doc/plan.md` M3.
- **Confidence**: 10/10
- **Action**: Use parity-db abstractions and temporary directories in tests. Keep `sync_wal` and `sync_data` enabled for protocol commits; do not reuse FnFnCoreWallet database wrappers or on-disk layout.

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

### L-008: [workflow] Keep M0 candidate dependencies outside the production workspace (2026-07-15)
- **Issue**: M0 state and BFT risk spikes.
- **Trigger**: alloy-trie, parity-db, Malachite, hotstuff_rs, spike, Cargo dependency
- **Pattern**: Candidate crates can compile and run without becoming an architectural commitment. Standalone Cargo workspaces under `spikes/` prevent their feature graphs and types from leaking into production crates before ADR hard gates pass.
- **Evidence**: `spikes/state-commitment/Cargo.toml`, `spikes/consensus/Cargo.toml`, ADR-003, ADR-004.
- **Confidence**: 10/10
- **Action**: Do not add candidate trie or BFT crates to root `[workspace.dependencies]` until the corresponding ADR is Accepted.

### L-009: [consensus] A shared quorum harness does not validate a third-party BFT engine (2026-07-15)
- **Issue**: M0 durable signer and four-validator fixture.
- **Trigger**: safety gate, four validators, WAL restart, double-sign, candidate passed
- **Pattern**: The shared harness validates Arbor's intended adapter contract: fsync before signing, restart recovery, weighted quorum, validator update, and conflicting-vote refusal. It does not exercise Malachite or HotStuff message/round/WAL behavior by itself.
- **Evidence**: `spikes/consensus`, ADR-004.
- **Confidence**: 10/10
- **Action**: A candidate may be rejected immediately on a conclusive safety-ordering failure. Never report feature compilation or the shared model as candidate safety proof.

### L-010: [toolchain] RocksDB bindgen needs a complete libclang include setup (superseded 2026-07-16)
- **Issue**: Building the M0 state-commitment spike.
- **Trigger**: librocksdb-sys, clang-sys, llvm-config, libclang, stdbool.h
- **Pattern**: Having only a versioned `libclang-15.so.15` is insufficient for bindgen discovery, and without clang resource headers it may also miss `stdbool.h`. A development environment needs a normal libclang/clang installation, or explicit `LIBCLANG_PATH` plus the compiler include path in `BINDGEN_EXTRA_CLANG_ARGS`.
- **Evidence**: Historical M0 RocksDB build attempt; ADR-003 now selects parity-db.
- **Confidence**: 9/10
- **Action**: Historical note only. Do not reintroduce the bindgen/libclang burden unless a future ADR reverses ADR-003.

### L-011: [testing] Restricted sandboxes can reject loopback port allocation (2026-07-15)
- **Issue**: `arbor-testkit` random port validation.
- **Trigger**: TcpListener, random port, PermissionDenied, sandbox
- **Pattern**: A sandbox may reject `127.0.0.1:0` with `Operation not permitted` even though the helper is correct. Skipping on permission denial would hide actual platform failures.
- **Evidence**: `crates/arbor-testkit`; workspace test passes outside the restricted socket sandbox.
- **Confidence**: 10/10
- **Action**: Preserve the bind assertion and rerun network/process tests with local-socket permission.

### L-012: [storage] Compare blockchain-native Rust stores before accepting a generic C++ engine (2026-07-16)
- **Issue**: The RocksDB spike spent build time on C++ bindings even though parity-db directly targets fixed-size uniformly distributed trie keys, small values, batch block imports, columns, atomic transactions, and crash recovery.
- **Trigger**: embedded KV, blockchain database, parity-db, RocksDB, bindgen, libclang
- **Pattern**: Storage choice includes build reproducibility and operational surface, not only runtime throughput. A parity-db hash column maps to content-addressed trie nodes and a narrowly scoped B-tree column covers ordered metadata.
- **Evidence**: The migrated M0 spike passed historical proof reopen, atomic transaction boundary, pruning, and a 100,000-account benchmark without libclang.
- **Confidence**: 9/10
- **Action**: Keep node hashes in a uniform hash-indexed column, ordered metadata in narrowly scoped B-tree columns, and measure durable recovery as well as submission latency.

### L-013: [consensus] Logging a WAL failure is not a durable signing boundary (2026-07-16)
- **Issue**: Malachite 0.7.0-pre flushes before publication but converts WAL/actor failures into logs and returns success; hotstuff_rs 0.4.0 sends a vote before updating persisted vote state.
- **Trigger**: vote, signer, WAL, fsync, Malachite, hotstuff_rs, durable safety state
- **Pattern**: Ordering diagrams are insufficient unless every persistence failure aborts signature release. Compile success and liveness tests cannot override a failed safety-ordering audit.
- **Evidence**: ADR-004 rejects both unmodified candidates; the fallback model checks four-validator quorum intersection plus restart conflict refusal.
- **Confidence**: 10/10
- **Action**: Persist the exact vote intent before delegating to a signer, propagate every storage error, and keep M8 blocked until a real four-process suite passes.

### L-014: [workflow] The Rust workspace exists; verify it instead of treating Cargo as planned (2026-07-22)
- **Issue**: M0/M1 completion review after the workspace baseline landed.
- **Trigger**: workspace, Cargo.toml, M1, CI, current build commands
- **Pattern**: L-005 describes the repository before M1 and is now obsolete. The root Cargo workspace, architecture crates, lockfile, CI, and executable quality gates exist; completion claims must be based on running them and the M1 CLI smoke path.
- **Evidence**: `Cargo.toml`, `.github/workflows/ci.yml`, `scripts/check-m1-smoke.sh`.
- **Confidence**: 10/10
- **Action**: Run fmt, Clippy, nextest, deny, documentation/dependency checks, and the M1 smoke gate. Do not describe the repository as docs-only.

### L-015: [network] Use rust-libp2p at the M7 network boundary (2026-07-22)
- **Issue**: P2P implementation direction was reconfirmed while reviewing M0/M1.
- **Trigger**: P2P, network, libp2p, temporary TCP, M1, M7
- **Pattern**: Arbor should implement its first real P2P transport with rust-libp2p, keeping peer identity distinct from validator consensus identity. The dependency and protocol implementation belong to M7; M1 only reserves the `arbor-network` crate and local configuration boundary.
- **Evidence**: `doc/architecture.md` section 8; `doc/plan.md` M7.
- **Confidence**: 10/10
- **Action**: Do not build a temporary custom TCP protocol or pull libp2p behavior into protocol primitives, state, or consensus crates.

### L-016: [protocol] Ethereum receipts stay typed RLP; Arbor-native objects use explicit codecs (2026-07-22)
- **Issue**: M2 initially risked treating receipt fields like an Arbor-native tagged object.
- **Trigger**: receipt, EIP-2718, EIP-1559, RLP, canonical codec, receipt root
- **Pattern**: EIP-1559 transactions and receipts must retain their standard `0x02 || rlp(payload)` bytes because those bytes feed Ethereum transaction/receipt tries. Arbor headers, domain descriptors, validator sets, votes, and QCs use separate tagged fixed-width canonical encodings.
- **Evidence**: `crates/arbor-codec/src/ethereum.rs`, ADR-005, M2 cross-vector tests.
- **Confidence**: 10/10
- **Action**: Never wrap Ethereum envelopes or receipts in an Arbor tag before hashing or trie insertion.

### L-017: [protocol] Hash immutable DomainGenesis, and bind every consensus signature to validator identity (2026-07-22)
- **Issue**: M2 origin/signature boundary review.
- **Trigger**: origin_hash, DomainGenesis, DomainDescriptor, ValidatorId, vote, QC, signature
- **Pattern**: `origin_hash` hashes a separate immutable `DomainGenesis`; hashing a descriptor containing its own `origin_hash` would be circular. Consensus signing and verification must also require `vote.validator_id == hash(consensus_public_key)` and QC verification must check membership, every signature, and strictly greater than two-thirds power.
- **Evidence**: `arbor-primitives::DomainGenesis`, `arbor-crypto`, `testdata/vectors/arbor-v1`.
- **Confidence**: 10/10
- **Action**: Keep mutable descriptor/status data out of the genesis preimage, and reject signer-ID mismatches before releasing or accepting votes.

### L-018: [supply-chain] Scope maintenance advisory exceptions to one exact transitive crate (2026-07-22)
- **Issue**: M2 `cargo deny` found RUSTSEC-2024-0436 through exact-pinned `alloy-primitives` 1.6.1 -> `paste` 1.0.15.
- **Trigger**: cargo deny, advisory, unmaintained, paste, alloy-primitives
- **Pattern**: This advisory reports that `paste` is archived, not a known vulnerability, and no safe upstream upgrade exists. A global relaxation would hide unrelated maintenance failures, so the repository records one exact advisory ID plus its removal condition.
- **Evidence**: `deny.toml`, `doc/protocol/dependencies.md`.
- **Confidence**: 9/10
- **Action**: Recheck every `alloy-primitives` upgrade and delete the exception immediately when the transitive `paste` dependency disappears.
