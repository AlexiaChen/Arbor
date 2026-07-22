# Protocol revision 1 execution

This document fixes the consensus-sensitive M4 execution boundary. Local node
configuration, installed crate defaults, wall clock, and RPC policy cannot alter
these rules.

## Version mapping

- Arbor `protocol_revision = 1` maps exactly to Shanghai
  `revm::primitives::hardfork::SpecId::SHANGHAI` under exact-pinned `revm`
  41.0.0.
- The adapter never selects `SpecId::default()`, `LATEST`, or `NEXT`.
- Contract code is limited to 24,576 bytes and initcode to 49,152 bytes.
- v1 accepts only canonical EIP-1559 type-2 envelopes. Sender recovery and
  transaction hashes happen before `revm`; `TxEnv` still enables chain ID,
  nonce, balance, intrinsic gas, base fee, priority fee and block gas checks.

The explicit block input is:

```text
DomainEnv {
  chain_id,
  block_number,
  timestamp,
  beneficiary,
  gas_limit,
  base_fee_per_gas,
  prevrandao,
}
```

Timestamp and prevrandao come from consensus data. Difficulty is zero, blob
fields are absent, and v1 domain gas is limited to 30,000,000.

## State and failure boundary

`ExecutionState` reads accounts using `keccak(address)` and storage using
`keccak(slot_be_32)` from M3 Ethereum MPTs. A finalized snapshot carries the
account trie nodes plus every currently referenced storage trie node; code is
stored by Keccak hash.

A batch executes on a clone of its parent state. Malformed signature/envelope,
wrong chain ID, nonce mismatch, insufficient funds, intrinsic-gas failure, or
aggregate block-gas overflow invalidates the candidate and discards that clone.
`REVERT` and EVM halt/out-of-gas are valid transaction outcomes with receipt
status zero: value/code/storage/log changes revert, while `revm`'s journal keeps
the sender nonce and actual gas charge. Base fee is burned and the effective
priority fee is credited to `beneficiary`.

## Transactions, receipts and roots

Transactions root and receipts root are standard Ethereum ordered MPT roots:

```text
key   = RLP(zero_based_index)
tx    = exact EIP-2718 envelope bytes
value = 0x02 || RLP([status, cumulative_gas_used, logs_bloom, logs])
```

Each receipt bloom accrues the emitting address and every topic using Ethereum's
2048-bit bloom. Block bloom is the bitwise OR of receipt blooms. Return/revert
bytes, contract address, sender, transaction gas used and other RPC/index fields
do not enter the receipt root.

The committed fixed block in
`testdata/vectors/arbor-v1/execution-roots.txt` executes transfer, CREATE,
storage plus LOG1, revert, native protocol info and out-of-gas in that order.
The integration gate durably commits its account/storage nodes, code and typed
receipts to parity-db, reopens the database, reconstructs executable state, and
checks identical state and receipt roots.

## Native system registry v1

The first native system contract is read-only:

- address: `0x0000000000000000000000000000000000000800`
- ABI: `protocolInfo()`; selector `0x93420cf4`
- native execution gas: 500, in addition to normal transaction/call gas
- output: ABI words `(uint32 protocol_revision, uint8 evm_revision,
  uint32 registry_version, uint64 chain_id)`

It is implemented through `revm`'s precompile provider. Wrong calldata or
non-zero transferred value returns EVM revert, so the ordinary journal controls
nonce, fee and transfer rollback.

## Frozen Shanghai subset

`testdata/ethereum-tests/shanghai/arbor-subset.txt` freezes the M4 adapter subset
covering EIP-3855 `PUSH0`, EIP-3860 initcode size, and intrinsic calldata gas.
It records its source links to the Ethereum execution specs and EIPs. This is a
small deterministic compatibility gate, not a claim that Arbor already runs the
full upstream execution-spec-test corpus.
