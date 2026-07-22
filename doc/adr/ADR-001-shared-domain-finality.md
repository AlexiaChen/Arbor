# ADR-001: Shared root finality for all domains

- Status: Accepted
- Date: 2026-07-15
- Updated: 2026-07-22 (M5 deterministic block validation and dev finality)

## Context

Starting a validator set and BFT instance for every child domain leaves stake
origin, weak domains, cross-domain finality, and unbounded node work undefined.

## Decision

Arbor v1 has one root PoS/BFT consensus chain. A finalized `ConsensusBlock`
orders zero or one batch per active domain. Validators execute every included
batch and validate `domain_results_root` plus the full sparse
`domain_heads_root`. A domain result inherits finality only through the carrying
consensus block, its QC, and the correctly typed inclusion proof.

The root EVM domain is the protocol control plane; it is not a second consensus
engine. Child domains have independent account state, EVM chain ID, gas market,
native asset, and logical block number. An idle domain produces no vacant block.

## Safety invariants

- Local history subscriptions never affect proposal validity.
- A validator retains executable head state for every active domain.
- Domain-result proofs and domain-head proofs are distinct proof types.
- Batch order is canonical by `domain_id`; duplicate domain batches are invalid.
- Per-domain and aggregate resource limits are consensus parameters.

## Consequences

All domains share root security and finality. v1 validators also share the total
execution cost, so this design does not claim unbounded execution scalability.
Independent committees, sharding, or parent checkpointing require a future ADR.

M5 implements this application boundary and fixed roots without selecting a
production BFT engine. `SingleValidatorEngine` is explicit local development
mode only and does not change the production safety assumptions above.
