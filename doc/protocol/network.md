# M7 network and block-sync protocol

This document records the completed M7 network and synchronization boundary.
M7 is complete only for the explicit single-validator development-finality
adapter. It is not a claim of production finality and does not unblock M8 or
ADR-004.

## Identity and handshake

Arbor persists an Ed25519 libp2p peer key at
`<data-dir>/network/peer.key` with owner-only permissions and uses it for Noise
transport authentication. Corrupt existing key material is a hard startup error
and is never silently replaced. The peer ID is node-local network identity only
and is never a validator ID or authorization proof. Validator consensus keys
remain the separate secp256k1 keys committed by the finalized validator set.

Every direct request carries an Arbor application handshake binding:

- `network_id` and `genesis_hash`;
- protocol, canonical codec, and direct-protocol versions;
- descriptive node role and strictly sorted capabilities;
- the peer's maximum accepted direct frame.

Wrong network, genesis, codec, protocol, malformed capability ordering, or
invalid frame budget is rejected before a request reaches block, state, or
consensus code. A connection also performs an eager handshake, but carrying the
same identity in each direct envelope prevents an ordering race from turning a
new transport connection into an authorized application peer.

## rust-libp2p behaviours

`arbor-network` exact-pins rust-libp2p 0.56.0 and composes:

- Noise over TCP with Yamux and signed peer IDs;
- `identify`, `ping`, and Kademlia discovery;
- signed gossipsub for raw transactions and finalized-head announcements;
- CBOR request-response for authenticated direct traffic.

LAN discovery uses exact-pinned `mdns-sd` 0.20.2. The libp2p `mdns` feature is
not enabled because its current dependency graph contains vulnerable
`hickory-proto` 0.25.2. mDNS results add addresses to Kademlia and initiate a
dial, but grant no authority: the full Arbor application handshake still
authenticates network, genesis, versions, and capabilities.

Transaction topics include network and domain identity. Finalized gossip carries
only `(height, consensus_hash, domain_heads_root)`; receivers fetch the exact
body through the direct protocol. Proposal, vote, round-change, block sync, and
state sync are direct messages and never broadcast as consensus gossip.

CBOR is only a transport envelope. Consensus hashes continue to use the
explicit Arbor block codec and Ethereum transaction/receipt encodings.

## Limits, backpressure, and peer policy

Direct CBOR decoding has fixed request and response byte budgets. Requests also
bound header/body count, history count, consensus payload size, snapshot chunk
size, total snapshot size, and request ID. Gossiped transaction envelopes
retain the M2 256 KiB limit. The request-response behaviour has a deadline and a
maximum concurrent stream count, so a slow peer cannot consume an unbounded
number of swarm streams. Consensus-direct delivery uses a bounded non-blocking
mailbox; a full application worker returns a rejection rather than blocking the
swarm or execution task.

Local peer scores penalize timeouts, malformed input, and application identity
mismatches. Scores may throttle or disconnect a peer, but never change block or
transaction validity.

Counters expose connected/authenticated peers, inbound requests/responses,
gossip, rejections, request failures, discovery successes, and discovery
failures. The node emits these as structured tracing fields; Prometheus export
remains M9.

## Four-level synchronization

The current executable path is:

1. authenticate the peer against network/genesis/version;
2. request its `SyncStatus`;
3. for incremental sync, request bounded headers and finality proofs;
4. check canonical encoding, claimed height, strict ancestry, selected finality
   adapter, and advertised remote tip before requesting bodies;
5. request bodies only for verified heights and require every decoded
   `ConsensusBlock` to embed the exact verified header;
6. replay the full block with `ChainMachine::validate_proposal`;
7. atomically commit block, state/code, receipts/indexes, WAL, domain heads, and
   the finalized marker through parity-db;
8. publish the in-memory finalized view and commit event only after commit
   success.

`SingleValidatorEngine::import_development_finalized_block` implements the last
three steps. Its M7 adapters accept only an empty development proof because the
single-validator engine deliberately emits no vote or QC. This proves neither
Byzantine finality nor M8 readiness. An M8 adapter must verify the accepted
validator-set transition and QC/finality chain before calling the same
application import boundary.

For a height-zero node, `SyncStatus.checkpoint_height` selects snapshot/state
sync before incremental headers. The exact versioned checkpoint manifest binds:

- network/genesis, finalized height/hash/timestamp, current and next validator
  set hashes, the complete validator set, and the adapter-owned finality proof;
- the global `domain_heads_root`, root domain, governance address, and complete
  strictly sorted active-domain set;
- each domain's immutable config, latest header/hash, optional runtime
  descriptor, sparse-head membership proof, state manifest, and chunk hashes.

The producer deterministically chunks all reachable authenticated trie nodes
and referenced contract code. Staging accepts reordered chunks and identical
duplicates, rejects conflicting, undeclared, oversized, or hash-invalid chunks,
then reconstructs every state and verifies every state/code/head/descriptor
root. `SingleValidatorEngine::import_development_checkpoint` revalidates the
complete chain state and writes all domains plus the global marker in one
synchronous parity-db transaction. Each idle domain retains its own domain
block height even though the global marker advances to the checkpoint consensus
height. No in-memory finalized view changes before the transaction succeeds.

Domain-history requests serve bounded canonical domain headers from the local
derived history projection. History retention never changes execution,
proposal validation, latest authenticated state, or global roots.

## Node assembly and acceptance

`arbor node run --dev-validator` runs the producing development node. Running
the initialized development config without the flag starts a sync-only full
listener. Configuration supports listen address, mDNS, and explicit
`<peer-id>@<multiaddr>` persistent peers. The listener redials, requests status,
selects snapshot or incremental sync, subscribes newly created domains, feeds
transaction gossip into the mempool, and reacts to finalized announcements.
Recent locally verified checkpoints are retained in a bounded snapshot cache,
including checkpoints imported by a listener so it can serve later peers.

The M7 executable gates prove:

- a height-zero real listener imports a root plus `ChainRegistry` child domain
  and reopens with the same finalized state and all domain roots;
- a multi-domain checkpoint snapshot followed by incremental blocks reopens
  identically, while invalid finality or a tampered chunk leaves the target
  finalized marker unchanged;
- a persistent listener disconnects, restarts with the same peer identity,
  reconnects, and catches the producer's final announced head;
- dropped, reordered, duplicate, timed-out, wrong-handshake, oversized, and
  slow-consumer cases fail within explicit bounds.

Socket tests retain explicit timeouts. A restricted agent sandbox may reject
loopback binding; those tests must be rerun in an environment that permits
local sockets rather than skipped or weakened.
