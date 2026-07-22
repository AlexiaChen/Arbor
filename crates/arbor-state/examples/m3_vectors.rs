//! Prints the committed M3 state-root vectors for reproducibility.

use std::collections::BTreeMap;

use alloy_primitives::{B256, U256, address, keccak256};
use arbor_primitives::DomainId;
use arbor_state::{
    Account, DomainHead, DomainHeadsCommitment, EthereumStateCommitment, encode_account,
    secure_account_key, secure_storage_key, storage_trie_value,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    for (name, root) in vectors()? {
        println!("{name}={root}");
    }
    Ok(())
}

fn vectors() -> Result<Vec<(&'static str, B256)>, arbor_state::StateError> {
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

    let empty = EthereumStateCommitment::build(&BTreeMap::new())?.root();
    let one = EthereumStateCommitment::build(&BTreeMap::from([(
        secure_account_key(alice),
        encode_account(&alice_account),
    )]))?
    .root();
    let two = EthereumStateCommitment::build(&BTreeMap::from([
        (secure_account_key(alice), encode_account(&alice_account)),
        (secure_account_key(bob), encode_account(&bob_account)),
    ]))?
    .root();
    let updated = EthereumStateCommitment::build(&BTreeMap::from([(
        secure_account_key(alice),
        encode_account(&Account {
            nonce: 2,
            balance: U256::from(999_999_u64),
            ..Account::default()
        }),
    )]))?
    .root();
    let storage = EthereumStateCommitment::build(&BTreeMap::from([
        (
            secure_storage_key(U256::ZERO),
            storage_trie_value(U256::from(1)).expect("one is non-zero"),
        ),
        (
            secure_storage_key(U256::from(255)),
            storage_trie_value(U256::MAX).expect("max is non-zero"),
        ),
    ]))?
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

    Ok(vec![
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
