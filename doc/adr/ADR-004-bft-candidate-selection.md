# ADR-004: BFT candidate selection

- Status: Accepted
- Date: 2026-07-16

## Candidates

Malachite 0.7.0-pre was evaluated because it separates engine, WAL, network,
sync, and application concerns. `hotstuff_rs` 0.4.0 was the comparison target
because its application, storage, network, sync, and validator-set boundaries
are compact. Neither dependency is present in the production workspace.

## Shared application boundary

Both adapters must drive the same deterministic dummy application and expose
Arbor semantics: build, validate, commit, validator-set-at-finalized-root, and a
safety store that durably persists before returning a signed vote. Candidate
block/vote types must not escape `arbor-consensus`.

The shared executable verifies four-validator weighted quorum behavior, one
equal-power validator offline, refusal to commit with only half the power, a
validator-power update, atomic/fsynced vote state, restart recovery,
conflicting-vote rejection, and exhaustive certificate intersection. This
validates Arbor's boundary and fallback model only; it is not candidate-engine
evidence.

## Candidate evidence

The source audit is conclusive at the mandatory persistence gate:

- `hotstuff_rs` 0.4.0 constructs and signs `PhaseVote`, sends it through the
  network handle, and only then calls `set_highest_view_phase_voted`. Both the
  proposal and nudge voting paths have this order. A crash after send and before
  the store update can therefore release a conflicting signature after restart.
- `arc-malachitebft-engine` 0.7.0-pre signs in `Effect::SignVote`. Its later
  publish effect requests a WAL flush, but `wal_append` and `wal_flush` convert
  both WAL errors and actor-call errors into log messages and return `Ok(())`.
  Publication can continue without proven durable state; the durable boundary
  is also later than Arbor's required pre-sign boundary.

Both versions compile on Rust stable 1.97.0, so this is not a compatibility
failure. It is a safety-ordering failure. Functional four-node tests cannot
override it, and fail-fast rejection avoids presenting liveness evidence as a
hard-gate pass.

## Decision

Neither candidate is accepted as an unmodified production engine. No BFT crate
enters the production workspace, and M8 remains blocked. The required fallback
[minimal consensus safety boundary](../protocol/minimal-consensus-safety.md) is
specified and evaluated by the executable four-validator exhaustive model in
`spikes/consensus`.

Reopening this ADR requires either an adapter that persists and fsyncs the exact
vote intent before delegating to a signer, with every persistence error aborting
the signature, or a candidate release with equivalent semantics. That path must
then pass a real four-process validator update, offline catch-up, WAL restart at
every vote boundary, partition, and conflicting-finalization suite. A temporary
majority-vote engine remains forbidden.
