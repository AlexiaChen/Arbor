# M6 ChainRegistry and tree-domain protocol

This document fixes the completed M6 creation, lifecycle, multi-domain
execution, local-history, and proof rules. It supplements ADR-002 and the block
rules in [blocks.md](blocks.md). Polished public RPC, production keystores, and
general operator chain commands remain M9 work; M6's executable acceptance path
is deliberately restricted to a fresh development chain and its public fixture
signer.

## Native ABI and root-only authority

Protocol revision 1 reserves
`0x0000000000000000000000000000000000000801` for the root-only native
`ChainRegistry`. Its first ABI method is:

```solidity
createChain(
    bytes32 parentDomainId,
    string name,
    string symbol,
    uint64 evmChainId,
    address owner,
    uint64 gasLimit,
    uint128 initialBaseFee,
    uint256 initialSupply,
    uint32 protocolRevision
) returns (bytes32 domainId)
```

The selector is the first four bytes of Keccak-256 of that exact signature.
The decoder requires canonical Solidity head/tail offsets, zero integer/address
padding, zero dynamic padding, and no trailing bytes. Names are limited to 64
ASCII alphanumeric/space/`-_.` bytes after collapsing ASCII whitespace;
symbols are 1-16 ASCII alphanumeric bytes normalized to uppercase. Chain ID,
owner, gas limit, base fee, and protocol revision are range checked.

The v1 creation call charges 180,000 native execution gas and requires at least
`1e18` root-domain native units as `msg.value`. The deposit, owner, `Locked`
status, and unlock height (`creation_height + 100`) are committed in registry
storage. Protocol revision one fixes those minimum and lock-duration values;
changing them requires a later protocol revision rather than a local CLI flag.

The same root-only native implementation exposes two exact 36-byte calls:

```solidity
refundDeposit(bytes32 domainId) returns (bytes32 domainId)
burnDeposit(bytes32 domainId) returns (bytes32 domainId)
```

Each lifecycle call charges 45,000 native execution gas on success and accepts
no value. `refundDeposit` succeeds only for the recorded owner at or after the
unlock consensus height. It transfers the outstanding amount from the registry
to the owner, zeros the amount, and changes status from `Locked` to `Refunded`.
`burnDeposit` succeeds only for the genesis-bound root-governance executor; it
may act while the deposit is locked, transfers the amount to the zero-address
burn sink, zeros the amount, and changes status to `Burned`. A terminal deposit
cannot be refunded or burned again. In the deterministic development genesis,
the governance executor is a distinct fixture address from the proposer reward
address; this authorization hook is not yet a production governance design.

The native call runs inside the same `revm` journal as its value transfer,
storage writes, and `ChainCreated` log. Invalid parameters, an unknown parent,
a used chain ID, a duplicate derived domain ID, static execution, or insufficient
deposit produce an ordinary EVM revert and status-zero receipt; storage and value
transfer are rolled back together.

An early or non-owner refund, a non-governance burn, an unknown/terminal
deposit, a value-bearing lifecycle call, or any registry call from a child
domain likewise produces a status-zero receipt. Balance transfer, storage
status, amount, and lifecycle event share one `revm` journal checkpoint.

## Deterministic creation

Before any batch executes, proposal validation captures every finalized domain
head. A root creation transaction uses that immutable map for `joint`, even if
the parent also has a batch in the same consensus block. A domain created by an
earlier transaction in the same proposal is therefore not a valid parent.

The implementation derives `domain_id` exactly as ADR-002 specifies and builds
the new genesis from an otherwise empty authenticated state plus the owner's
`initial_supply`. Native system addresses are versioned execution behavior and
do not require copied parent bytecode. `origin_hash` commits the canonical
`DomainGenesis`, including this initial state root.

At root genesis, the registry account has nonce 1 plus authenticated root-domain
and root-chain-ID slots. The fixed nonce is required because EIP-161 emptiness
does not preserve an otherwise balance/code/nonce-empty account merely because
it has a non-empty storage root.

On successful finalization, the child genesis header (domain number zero) is
inserted into the global domain-head commitment at the creation consensus
height. It cannot have a batch in that proposal because block validation only
accepts domains present in the proposal's finalized parent. It becomes eligible
for mempool admission and a first batch at the following consensus height.

## Persistence and queries

The descriptor hash, chain-ID mapping, creation height, owner, deposit
amount/status, and unlock height are consensus state in the root-domain MPT.
The canonically encoded descriptor in parity-db's `domain_registry` column is an atomic,
rebuildable query projection written with the finalized marker; it is not a
second consensus truth. Restart recovery replays stored consensus bodies from
genesis and requires all reconstructed domain heads and state roots to match the
durable marker.

`FinalizedChainState` exposes descriptor lookup, root-to-target genealogy, and
a 256-sibling current domain-head proof. `ConsensusBlock` separately exposes a
binary Merkle domain-result proof. The two proof types are not interchangeable:
the former proves a checkpoint head, while the latter proves execution in one
particular consensus block.

## Honest development scheduler

The development engine maintains one chain-ID-isolated mempool per finalized
domain. It computes a per-active-domain fair gas share from the total block gas
budget, caps one domain at 1,024 selected transactions per turn, rotates the
first domain after each commit, and finally sorts batches by `domain_id` for
canonical block construction. Scheduling is proposer policy only. Full proposal
replay, resource limits, roots, and transaction validity remain independent of
local history subscription or scheduler configuration.

## Node-local history projection

`node.domains` accepts `all`, `root`, or `root,<domain-id>...`. All validators
still decode and execute every proposed batch, persist every domain's latest
authenticated state and contract code, and reconstruct every domain head during
restart. The selection only controls derived receipt and transaction-location
indexes for newly committed batches. A two-database integration test feeds the
same root and child transactions to `all` and `root` nodes: both finalize the
same state, consensus hash, and `domain_heads_root`, while only the `all` node
serves the child receipt. This setting is therefore not a registry allowlist or
a block-validity input.

## Executable M6 acceptance

`arbor chain m6-smoke --data-dir <fresh-dev-dir>` uses only the documented
development fixture key. It creates root -> child at height one, child ->
grandchild at height two, and at height three deploys bytecode in both child
and grandchild. Before commit it constructs and verifies both binary
domain-result proofs against the same candidate `domain_results_root`; after
the synchronous commit it checks both contract accounts/code, the finalized
consensus hash, and durable database reopen. `scripts/check-m6-smoke.sh` runs
this complete path and requires three healthy persisted domain roots.

Fixed root/child creation values are in
`testdata/vectors/arbor-v1/m6-domain-roots.txt`. The integration test covers
root -> child -> grandchild creation, same-block parent execution with a frozen
joint, chain-ID conflict rollback, isolated balances/nonces, multi-domain
scheduling, head/result proofs, governance-only burn, height-gated owner refund,
local-history independence, atomic descriptor projection, and restart replay.
