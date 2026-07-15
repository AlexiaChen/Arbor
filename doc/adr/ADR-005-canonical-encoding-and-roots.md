# ADR-005: Canonical encoding, hashes, transactions, and roots

- Status: Accepted
- Date: 2026-07-15

## Decision

Protocol hashes use Keccak-256. Every Arbor-native signing or hash preimage
starts with a unique ASCII domain tag followed by a canonical version byte.
Fixed integers are unsigned big-endian with their specified width. Fixed hashes
and addresses are raw bytes. Variable bytes and collections use an unsigned
32-bit big-endian length followed by exactly that many bytes/items. Decoders
reject unknown versions, non-minimal alternatives, trailing bytes, and inputs
over the resource budget.

User transactions are standard EIP-2718 envelopes; v1 accepts EIP-1559 type 2.
Signing payload, sender recovery, and transaction hash follow the Ethereum
specification without an Arbor wrapper or second transaction ID.

Ethereum transaction and receipt tries use RLP(index) keys and Ethereum MPT
root rules. The empty trie root is
`56e81f171bcc55a6ff8345e692c0f86e5b48e01b996cadc001622fb5e363b421`.
Receipt consensus fields are status, cumulative gas used, logs bloom, and logs;
RPC-only fields do not enter the receipt root.

Consensus batches are sorted by `domain_id`. Merkle collections use a
domain-separated binary tree with explicit leaf count; odd nodes are paired
with the level-specific empty hash rather than duplicated. Exact tags and
limits are fixed in [protocol constants](../protocol/constants.md).

## Consequences

M2 must add golden vectors and independent Ethereum cross-checks before these
algorithms enter networking or storage. Generic Rust serialization formats are
not consensus codecs.

