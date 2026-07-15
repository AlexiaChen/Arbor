# ADR-004: BFT candidate selection

- Status: Proposed
- Date: 2026-07-15

## Candidates

Malachite is the preferred evaluation target because it separates engine, WAL,
network, sync, and application concerns and has a formal Tendermint model. It is
still explicitly alpha and unaudited. `hotstuff_rs` 0.4.0 is the comparison
target because its `App`, `KVStore`, network, block sync, and dynamic validator
set boundaries are compact, but its release and maintenance cadence need review.

Neither dependency is present in the production workspace. Optional feature
checks live only in `spikes/consensus`.

## Shared application boundary

Both adapters must drive the same deterministic dummy application and expose
Arbor semantics: build, validate, commit, validator-set-at-finalized-root, and a
safety store that durably persists before returning a signed vote. Candidate
block/vote types must not escape `arbor-consensus`.

## Current evidence

The shared executable verifies a four-validator weighted quorum, progress with
one equal-power validator offline, refusal to commit with only half the power,
a validator power update, atomic/fsynced vote state, restart recovery, and
conflicting-vote rejection. This validates Arbor's adapter contract and fault
harness only; it is not evidence that either engine itself executed the run.

## Hard gate

Each candidate must independently pass a four-process test with validator set
update, one node offline and catching up, WAL restart at every pre-vote boundary,
and an assertion that fewer than one third faulty voting power cannot finalize
conflicting blocks. Logs must identify durable state completion before signature
release. Network partition tests must show loss of liveness, not safety.

If neither passes, M8 remains blocked while a minimal consensus specification
and formal model are evaluated. A temporary majority-vote engine is forbidden.

