use alloy_primitives::{B256, U256};
use alloy_rlp::{Decodable, Encodable, Header};
use alloy_trie::{EMPTY_ROOT_HASH, KECCAK_EMPTY};

use crate::mpt::{RlpKind, StateError, decode_rlp_list};

/// Ethereum-compatible account leaf committed by a domain state trie.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Account {
    /// Transaction nonce.
    pub nonce: u64,
    /// Native-asset balance.
    pub balance: U256,
    /// Root of this account's secure storage trie.
    pub storage_root: B256,
    /// Keccak-256 hash of deployed bytecode.
    pub code_hash: B256,
}

impl Default for Account {
    fn default() -> Self {
        Self {
            nonce: 0,
            balance: U256::ZERO,
            storage_root: EMPTY_ROOT_HASH,
            code_hash: KECCAK_EMPTY,
        }
    }
}

impl Account {
    /// Returns whether EIP-161 state clearing removes this account.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nonce == 0 && self.balance.is_zero() && self.code_hash == KECCAK_EMPTY
    }
}

/// Encodes an account using the Ethereum state-trie RLP tuple.
#[must_use]
pub fn encode_account(account: &Account) -> Vec<u8> {
    let mut payload = Vec::with_capacity(72);
    account.nonce.encode(&mut payload);
    account.balance.encode(&mut payload);
    account.storage_root.encode(&mut payload);
    account.code_hash.encode(&mut payload);
    let mut out = Vec::with_capacity(payload.len() + 2);
    Header {
        list: true,
        payload_length: payload.len(),
    }
    .encode(&mut out);
    out.extend_from_slice(&payload);
    out
}

/// Decodes one exact Ethereum account RLP value.
///
/// # Errors
///
/// Returns [`StateError::MalformedRlp`] for non-canonical values or wrong fields.
pub fn decode_account(input: &[u8]) -> Result<Account, StateError> {
    let fields = decode_rlp_list(input)?;
    if fields.len() != 4 || fields.iter().any(|field| field.kind != RlpKind::Bytes) {
        return Err(StateError::MalformedRlp("account field layout"));
    }
    let nonce = decode_u64(fields[0].payload)?;
    let balance = decode_u256(fields[1].payload)?;
    let storage_root = B256::try_from(fields[2].payload)
        .map_err(|_| StateError::MalformedRlp("account storage root"))?;
    let code_hash = B256::try_from(fields[3].payload)
        .map_err(|_| StateError::MalformedRlp("account code hash"))?;
    Ok(Account {
        nonce,
        balance,
        storage_root,
        code_hash,
    })
}

/// Encodes a non-zero EVM storage word for insertion into a storage trie.
///
/// A zero word is represented by deleting the secure storage leaf.
#[must_use]
pub fn storage_trie_value(value: U256) -> Option<Vec<u8>> {
    if value.is_zero() {
        None
    } else {
        let mut out = Vec::new();
        value.encode(&mut out);
        Some(out)
    }
}

/// Decodes one non-zero EVM storage word from its canonical trie RLP value.
///
/// # Errors
///
/// Returns [`StateError::MalformedRlp`] for a list, zero, non-minimal, or oversized value.
pub fn decode_storage_trie_value(input: &[u8]) -> Result<U256, StateError> {
    let mut remaining = input;
    let value = U256::decode(&mut remaining)
        .map_err(|_| StateError::MalformedRlp("storage value layout"))?;
    if !remaining.is_empty() {
        return Err(StateError::MalformedRlp("storage value trailing bytes"));
    }
    if value.is_zero() {
        return Err(StateError::MalformedRlp("zero storage leaf"));
    }
    Ok(value)
}

fn reject_leading_zero(bytes: &[u8]) -> Result<(), StateError> {
    if bytes.first() == Some(&0) {
        return Err(StateError::MalformedRlp("non-minimal integer"));
    }
    Ok(())
}

fn decode_u64(bytes: &[u8]) -> Result<u64, StateError> {
    reject_leading_zero(bytes)?;
    if bytes.len() > 8 {
        return Err(StateError::MalformedRlp("u64 overflow"));
    }
    let mut word = [0_u8; 8];
    word[8 - bytes.len()..].copy_from_slice(bytes);
    Ok(u64::from_be_bytes(word))
}

fn decode_u256(bytes: &[u8]) -> Result<U256, StateError> {
    reject_leading_zero(bytes)?;
    if bytes.len() > 32 {
        return Err(StateError::MalformedRlp("U256 overflow"));
    }
    Ok(U256::from_be_slice(bytes))
}

#[cfg(test)]
mod tests {
    use alloy_primitives::keccak256;

    use super::*;

    #[test]
    fn account_rlp_matches_alloy_trie() {
        let account = Account {
            nonce: 7,
            balance: U256::from(1_000_000_u64),
            storage_root: keccak256(b"storage"),
            code_hash: keccak256(b"code"),
        };
        let upstream = alloy_trie::TrieAccount {
            nonce: account.nonce,
            balance: account.balance,
            storage_root: account.storage_root,
            code_hash: account.code_hash,
        };
        assert_eq!(encode_account(&account), alloy_rlp::encode(upstream));
        assert_eq!(decode_account(&encode_account(&account)).unwrap(), account);
    }

    #[test]
    fn empty_and_storage_rules_are_explicit() {
        assert!(Account::default().is_empty());
        assert_eq!(storage_trie_value(U256::ZERO), None);
        assert_eq!(storage_trie_value(U256::from(1)), Some(vec![1]));
        assert_eq!(decode_storage_trie_value(&[1]).unwrap(), U256::from(1));
        assert!(decode_storage_trie_value(&[0x80]).is_err());
    }
}
