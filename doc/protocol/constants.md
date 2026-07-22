# Initial protocol constants

These M0 values define v1 compatibility boundaries. Governance may schedule a
future `ProtocolSpec`; local configuration cannot change them.

| Constant | v1 value |
| --- | --- |
| Protocol version | `1` |
| Canonical codec version | `1` |
| Hash function | Keccak-256 |
| Max transaction envelope | 256 KiB |
| Max calldata | 128 KiB |
| Max deployed code | 24,576 bytes |
| Max initcode | 49,152 bytes |
| Max access-list addresses | 1,024 |
| Max storage keys per access-list address | 1,024 |
| Max logs per transaction | 1,024 |
| Max log data per transaction | 256 KiB |
| Max batches per consensus block | 256 |
| Max transactions per domain batch | 10,000 |
| Max domain gas per block | 30,000,000 |
| Max aggregate gas per consensus block | 120,000,000 |
| Max encoded consensus block | 16 MiB |
| Max P2P frame | 16 MiB |
| Max RPC request body | 2 MiB |
| Max canonical nesting depth | 32 |
| Max canonical collection items | 65,536 |
| Snapshot chunk target/max | 4 MiB / 8 MiB |
| Max future timestamp step | 30 seconds |
| Max active validators | 100 |

Domain-separation tags:

- `ARBOR_DOMAIN_V1`
- `ARBOR_DOMAIN_GENESIS_V1`
- `ARBOR_DOMAIN_DESCRIPTOR_V1`
- `ARBOR_DOMAIN_HEADER_V1`
- `ARBOR_DOMAIN_BATCH_V1`
- `ARBOR_DOMAIN_HEAD_KEY_V1`
- `ARBOR_DOMAIN_HEAD_VALUE_V1`
- `ARBOR_SMT_LEAF_V1`
- `ARBOR_SMT_BRANCH_V1`
- `ARBOR_CONSENSUS_HEADER_V1`
- `ARBOR_VALIDATOR_ID_V1`
- `ARBOR_VALIDATOR_SET_V1`
- `ARBOR_BATCH_LEAF_V1`
- `ARBOR_RESULT_LEAF_V1`
- `ARBOR_VOTE_V1`
- `ARBOR_QC_V1`

Economic constants such as creation deposit and staking/slashing values remain
unset until their state machines and economic review exist. Implementations
must reject an absent production spec rather than silently choose local values.
