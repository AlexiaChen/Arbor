//! Stable account, storage, and domain-head commitment vectors.

use std::{collections::BTreeMap, str::FromStr};

use alloy_primitives::{B256, U256, address, keccak256};
use arbor_primitives::DomainId;
use arbor_state::{
    Account, DomainHead, DomainHeadsCommitment, EthereumStateCommitment, encode_account,
    secure_account_key, secure_storage_key, storage_trie_value,
};

#[test]
fn committed_state_roots_match() {
    let expected: BTreeMap<_, _> =
        include_str!("../../../testdata/vectors/arbor-v1/state-roots.txt")
            .lines()
            .filter(|line| !line.starts_with('#') && !line.is_empty())
            .map(|line| {
                let (name, hash) = line.split_once('=').unwrap();
                (name, B256::from_str(hash).unwrap())
            })
            .collect();
    assert_eq!(expected, vectors());
}

fn vectors() -> BTreeMap<&'static str, B256> {
    let alice = address!("0000000000000000000000000000000000000001");
    let bob = address!("0000000000000000000000000000000000000002");
    let alice_account = Account {
        nonce: 1,
        balance: U256::from(1_000_000_u64),
        ..Account::default()
    };
    let bob_account = Account {
        balance: U256::from(2_000_000_u64),
        code_hash: keccak256(b"bob-code"),
        ..Account::default()
    };
    let empty = EthereumStateCommitment::build(&BTreeMap::new())
        .unwrap()
        .root();
    let one = EthereumStateCommitment::build(&BTreeMap::from([(
        secure_account_key(alice),
        encode_account(&alice_account),
    )]))
    .unwrap()
    .root();
    let two = EthereumStateCommitment::build(&BTreeMap::from([
        (secure_account_key(alice), encode_account(&alice_account)),
        (secure_account_key(bob), encode_account(&bob_account)),
    ]))
    .unwrap()
    .root();
    let updated = EthereumStateCommitment::build(&BTreeMap::from([(
        secure_account_key(alice),
        encode_account(&Account {
            nonce: 2,
            balance: U256::from(999_999_u64),
            ..Account::default()
        }),
    )]))
    .unwrap()
    .root();
    let storage = EthereumStateCommitment::build(&BTreeMap::from([
        (
            secure_storage_key(U256::ZERO),
            storage_trie_value(U256::from(1)).unwrap(),
        ),
        (
            secure_storage_key(U256::from(255)),
            storage_trie_value(U256::MAX).unwrap(),
        ),
    ]))
    .unwrap()
    .root();
    let heads_empty = DomainHeadsCommitment::new().root();
    let mut heads = DomainHeadsCommitment::new();
    heads.insert(
        DomainId(B256::repeat_byte(1)),
        DomainHead {
            domain_block_hash: B256::repeat_byte(2),
            state_root: one,
        },
    );
    let heads_one = heads.root();
    heads.insert(
        DomainId(B256::repeat_byte(3)),
        DomainHead {
            domain_block_hash: B256::repeat_byte(4),
            state_root: two,
        },
    );
    let heads_two = heads.root();
    BTreeMap::from([
        ("empty_state", empty),
        ("one_account", one),
        ("two_accounts", two),
        ("updated_and_deleted", updated),
        ("two_storage_slots", storage),
        ("empty_domain_heads", heads_empty),
        ("one_domain_head", heads_one),
        ("two_domain_heads", heads_two),
    ])
}
