# Architecture decision records

| ADR | Decision | Status |
| --- | --- | --- |
| [ADR-001](ADR-001-shared-domain-finality.md) | Shared root finality | Accepted |
| [ADR-002](ADR-002-domain-identity-and-creation.md) | Domain identity and creation | Accepted |
| [ADR-003](ADR-003-state-commitments.md) | Ethereum MPT and parity-db | Accepted |
| [ADR-004](ADR-004-bft-candidate-selection.md) | Reject unsafe BFT candidates; retain hard gate | Accepted |
| [ADR-005](ADR-005-canonical-encoding-and-roots.md) | Canonical encoding and roots | Accepted |

`Accepted` records an architectural decision, including a decision to reject all
evaluated candidates. It does not imply a dependency is production-ready. A
passing shared harness is not sufficient candidate evidence; ADR-004 keeps M8
blocked until its reopening conditions pass.
