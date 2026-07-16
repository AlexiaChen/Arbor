# Consensus candidate spike

This M0 standalone workspace contains the shared dummy application, durable
signer boundary, and executable fallback safety model. It checks four-validator
weighted quorum behavior, one offline validator, a validator-power update,
crash/restart safety-state recovery, conflicting-vote rejection, and exhaustive
certificate intersection with one possible Byzantine validator.

```bash
cargo run --manifest-path spikes/consensus/Cargo.toml
cargo check --manifest-path spikes/consensus/Cargo.toml --all-features
```

The feature check proves that Malachite 0.7.0-pre and `hotstuff_rs` 0.4.0 compile
on the pinned stable toolchain. ADR-004 rejects both unmodified engines because
their persistence/signing ordering fails Arbor's hard gate. The executable is
the required fallback model, not evidence that either engine passed a live
four-process test. Production consensus remains blocked until ADR-004 is
reopened under its stated conditions.
