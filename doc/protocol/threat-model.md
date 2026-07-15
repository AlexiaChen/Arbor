# Initial threat model

Arbor assumes Byzantine voting power is strictly below one third per epoch and
eventual network synchrony for liveness. During a long partition the protocol
must stop finalizing before it risks conflicting finality.

| Threat | Required control |
| --- | --- |
| Equivocation or vote replay | Network/height/round/phase domain separation; durable state before signing; evidence |
| Long-range history | Finalized validator transitions and operator weak-subjectivity checkpoints |
| Validator key compromise | Encrypted/remote signer path, rotation, tombstone rules, no RPC key custody |
| Domain creation spam | Root-governed deposit, block limits, globally unique chain ID |
| EVM/system-contract reentrancy | One journal, explicit gas, protocol-versioned native code, revert tests |
| Resource exhaustion | Fixed decode, transaction, block, RPC, P2P, and snapshot budgets |
| RocksDB crash/corruption | WAL sync, atomic commit marker, immutable nodes, reachability checks |
| Snapshot poisoning | Verify finality, manifest/chunk hashes, domain-head proofs, and state roots before activation |
| Index/state disagreement | Trie is truth; indexes are rebuildable and never used for consensus validity |
| Eclipse/slow peer | Authenticated peers, timeouts, backpressure, scoring; no local score affects validity |
| RPC/keystore exposure | Node RPC has no signing/unlock/stop by default; keystore uses a separate directory |

Out of scope for v1 security claims: private-domain confidentiality, trustless
bridges, cross-domain atomicity, sharded data availability, and per-domain
validator sets.

