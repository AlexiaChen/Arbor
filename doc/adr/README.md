# Architecture decision records

| ADR | Decision | Status |
| --- | --- | --- |
| [ADR-001](ADR-001-shared-domain-finality.md) | Shared root finality | Accepted |
| [ADR-002](ADR-002-domain-identity-and-creation.md) | Domain identity and creation | Accepted |
| [ADR-003](ADR-003-state-commitments.md) | State commitments | Proposed |
| [ADR-004](ADR-004-bft-candidate-selection.md) | BFT candidate selection | Proposed |
| [ADR-005](ADR-005-canonical-encoding-and-roots.md) | Canonical encoding and roots | Accepted |

`Proposed` means production implementation depending on that decision is still
blocked by the recorded M0 hard gate. A passing shared harness is not sufficient
to promote an ADR; the named third-party candidate must pass its own scenario.

