# ADR-002: Domain identity and creation anchors

- Status: Accepted
- Date: 2026-07-15
- Updated: 2026-07-22 (M2 canonical version byte made explicit)

## Decision

Domains are created only by an EIP-1559 transaction calling the root-domain
`ChainRegistry`. No Template or embedded origin block is introduced.

```text
domain_id = keccak256(
  "ARBOR_DOMAIN_V1" || canonical_codec_version ||
  network_id || parent_domain_id || create_tx_hash
)
```

All fixed-width fields use their ADR-005 canonical bytes. `joint` is the parent
domain finalized head captured before proposal execution begins. A parent batch
in the same consensus block cannot change it. `origin_hash` is Keccak-256 of the
canonical `DomainGenesis` containing the request, identifiers, joint, and
initial state root.

`evm_chain_id` is non-zero, globally unique in the Arbor network, immutable,
and checked by the root registry. `(parent_domain_id, create_tx_hash)` is an
idempotency key. Names and symbols are normalized display metadata, not IDs.

Creation locks a governance-set root-domain deposit. Minimum amount, lock
duration, refund, and burn rules live in a versioned `ProtocolSpec`; CLI code
must not invent them. A new domain becomes active only after its creation block
finalizes and may receive transactions from the following consensus height.

## Consequences

Every domain begins from empty state plus versioned system contracts and the
owner allocation. Parent state is never copied. Cross-domain assets or calls
require a separately reviewed protocol.
