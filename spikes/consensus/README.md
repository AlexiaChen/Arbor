# Consensus candidate spike

This M0 directory holds the shared dummy application and durable signer harness
used to evaluate both candidates. The default executable verifies the invariant
that vote state reaches durable storage before signing, survives restart, and
rejects a conflicting vote. It also checks the quorum boundary for four equal
validators, one offline validator, and a validator-power update.

```bash
cargo run --manifest-path spikes/consensus/Cargo.toml
cargo check --manifest-path spikes/consensus/Cargo.toml --features malachite
cargo check --manifest-path spikes/consensus/Cargo.toml --features hotstuff
```

The feature checks intentionally keep candidate dependencies outside the
production workspace. They prove current dependency/toolchain compatibility,
not a live engine run. ADR-004 remains `Proposed` until each adapter runs the
same four-process fault/restart scenarios; the shared harness must not be cited
as proof that either third-party engine passed that gate.

