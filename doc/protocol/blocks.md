# M5 consensus block and development-finality protocol

This document fixes the protocol revision 1 rules implemented by
`arbor-chain` and the local-only finality behavior implemented by
`arbor-consensus`. It complements the [execution protocol](execution.md).

## Block construction and ancestry

A `ConsensusBlock` contains one `ConsensusBlockHeader`, canonically sorted
`DomainBatch` values, and one `DomainBlockHeader` result aligned with every
batch. Batches are strictly increasing by raw 32-byte `domain_id`; duplicates
are invalid. A builder may accept input in another order only by sorting it
before execution. Validation never repairs network or stored input.

The consensus header must use protocol version 1, the configured network ID,
`parent.height + 1`, and the exact finalized parent consensus hash. Its
timestamp is in `parent.timestamp + 1 ..= parent.timestamp + 30`. A batch must
name an active domain and its exact finalized domain-block hash. Its result has
`number = parent_domain.number + 1`, the carrying consensus height, and the
same domain ID and parent hash.

Zero-batch consensus blocks are valid. They advance root consensus height and
timestamp but do not create a vacant domain block, change domain number/base
fee, or write another copy of unchanged domain state.

## Collection roots

`batches_root` hashes canonical `DomainBatch` bytes with leaf tag
`ARBOR_BATCH_LEAF_V1`. `domain_results_root` hashes canonical
`DomainBlockHeader` bytes with leaf tag `ARBOR_RESULT_LEAF_V1`. For collection
count `n`, leaf index `i`, and canonical value `v`:

```text
leaf = keccak256(leaf_tag || codec_version || u32be(n) || u32be(i)
                 || u32be(len(v)) || v)
empty(depth) = keccak256("ARBOR_MERKLE_EMPTY_V1" || codec_version
                         || leaf_tag || u16be(depth))
branch(depth, left, right) =
  keccak256("ARBOR_MERKLE_BRANCH_V1" || codec_version || leaf_tag
            || u16be(depth) || left || right)
root = keccak256("ARBOR_MERKLE_ROOT_V1" || codec_version || leaf_tag
                 || u32be(n) || tree_root)
```

At each level an unpaired left node uses `empty(depth)` as its right sibling;
nodes are never duplicated. An empty collection uses `empty(0)` as
`tree_root`. `domain_heads_root` remains the separate 256-level sparse map from
ADR-003 and commits every current `(domain block hash, state root)`, including
unchanged idle domains.

Consensus and domain block hashes are Keccak-256 of their existing canonical
header encodings. The versioned full-body codec is used for durable replay and
future networking, but the body is not a second block-hash preimage.

## Deterministic execution environment

Each domain batch executes from its finalized parent state. Protocol revision
1 derives `PREVRANDAO` without host randomness:

```text
keccak256("ARBOR_PREVRANDAO_V1" || codec_version
          || parent_consensus_hash || u64be(consensus_height))
```

The proposer header address is the EVM beneficiary, so the M4 fee transition
credits per-domain priority fees there. M5 adds no inflationary block reward;
the root development genesis contains one validator/reward address and its
reward is exactly priority fees. Staking issuance and production rewards
remain an M8 economic/protocol decision.

The next base fee is computed only when the domain has a batch. It uses the
Ethereum EIP-1559 target of half the parent gas limit and denominator 8; an
upward change is at least one unit and downward subtraction saturates at zero.

Limits are 256 batches, 30,000,000 gas per domain, 120,000,000 actual gas in
the consensus block, 10,000 transactions per batch, and 16 MiB for the encoded
full block. The transaction/envelope and EVM limits from the execution protocol
also apply. Validators re-execute and independently compare transaction,
state, receipt, batch, domain-result, and domain-head roots.

## Proposal and durable commit boundary

Proposal execution produces a private `ValidatedProposal` overlay. Selected
mempool entries are reserved, not finalized. Finalized state, receipt lookup,
the storage marker, and commit subscribers continue to expose the old head.
Explicit abandonment restores reserved entries unless a newer same-nonce
replacement already exists.

Commit writes trie/storage nodes, code, receipts, transaction positions, the
complete block body, development commit WAL, domain heads, and finalized marker
in one synchronous parity-db transaction. Only after that transaction returns
success does the engine replace its finalized in-memory view and publish a
`CommitEvent`. Recovery checks the genesis fingerprint, replays every stored
block from height one, and requires the replayed head, marker, per-domain state
head, and development WAL to agree.

## Single-validator scope

`SingleValidatorEngine` can be opened only with explicit `DevValidator` mode;
the CLI additionally requires a directory initialized by `node init --dev`
before accepting `node run --dev-validator`. It immediately commits a locally
built and validated proposal and intentionally has no vote, QC, peer, or
Byzantine-fault semantics. The fixed development consensus key is public test
material. This path cannot be selected as production consensus and does not
satisfy or bypass ADR-004.

The stable M6/M7 consumption boundary is the finalized `CommitEvent` carrying
height, consensus hash, and `domain_heads_root`. Fixed height-one roots are in
`testdata/vectors/arbor-v1/m5-block-roots.txt`.
