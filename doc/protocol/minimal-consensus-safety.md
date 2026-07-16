# Minimal consensus safety boundary

This document is the M0 fallback required by ADR-004. It is a safety
specification and model-evaluation result, not authorization to implement a
third consensus engine.

## State and transition boundary

A validator safety record is the tuple `(height, round, phase, block_hash)`.
Before any signing provider can release a vote signature, the exact tuple must
be written to a temporary record, `fsync`ed, atomically renamed over the active
record, and followed by an `fsync` of the parent directory. A storage, flush,
rename, or actor error aborts signing. Restart loads this record before the
validator participates.

For the same `(height, round, phase)`, a validator may only repeat the same
`block_hash`. Tuples may not regress. Validator-set changes become effective
only from a finalized transition; an unfinalized proposal cannot change the
power used to validate a certificate.

## Quorum rule

Certificate power is strictly greater than two thirds of the voting power in
the finalized validator set for that height. Safety assumes Byzantine voting
power is strictly less than one third. Under those bounds, any two certificates
intersect in honest voting power, and an honest validator's durable signing rule
prevents certificates for conflicting blocks in the same safety slot.

Network partitions may stop progress. They must never lower the certificate
threshold, activate an unfinalized validator set, erase the durable record, or
permit a local majority shortcut.

## Executable model result

`spikes/consensus` exhaustively enumerates every certificate-sized subset of
four equal-power validators and each possible single Byzantine validator. Every
pair of certificates intersects in at least one honest validator. It also
injects the crash boundary before and after durable persistence, reopens the
safety record, and proves a conflicting vote is rejected after restart.

Run:

```bash
cargo run --manifest-path spikes/consensus/Cargo.toml
```

The model checks the safety boundary, but it is not a live BFT engine proof.
M8 stays blocked until an implementation makes persistence failure fatal and a
real four-process suite covers every vote boundary, catch-up, validator-set
transition, partition, and restart.
