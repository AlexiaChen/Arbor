# ADR-005: Canonical encoding, hashes, transactions, and roots

- Status: Accepted
- Date: 2026-07-15
- Updated: 2026-07-22 (M2 codecs/signatures and M5 block collection roots)

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

Validator consensus keys are separate secp256k1 keys encoded as 33-byte
compressed SEC1 points. Consensus signatures are deterministic canonical low-s
ECDSA `(r,s)` values encoded as exactly 64 bytes; validator IDs hash the
compressed key with `ARBOR_VALIDATOR_ID_V1` and the canonical version byte.
The recoverable parity used by Ethereum account transactions is not part of a
consensus signature.

Ethereum transaction and receipt tries use RLP(index) keys and Ethereum MPT
root rules. The empty trie root is
`56e81f171bcc55a6ff8345e692c0f86e5b48e01b996cadc001622fb5e363b421`.
Receipt consensus fields are status, cumulative gas used, logs bloom, and logs;
RPC-only fields do not enter the receipt root.

Consensus batches are sorted by `domain_id`. Merkle collections use a
domain-separated binary tree with explicit leaf count; odd nodes are paired
with the level-specific empty hash rather than duplicated. Exact tags and
limits are fixed in [protocol constants](../protocol/constants.md); the M5
leaf/empty/branch/root preimages are fixed in the
[block protocol](../protocol/blocks.md).

## Consequences

M2 added canonical object and Ethereum transaction/receipt vectors; M5 added
block collection and full-replay root vectors before those bodies enter future
networking. Generic Rust serialization formats are not consensus codecs.
