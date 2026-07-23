//! M4 deterministic EVM execution, failure semantics, and durable restart acceptance tests.

use std::collections::BTreeMap;

use alloy_primitives::{Address, B256, Bytes, U256, address, keccak256};
use alloy_rlp::{Encodable, Header};
use arbor_codec::{encode_eip1559, encode_eip1559_signing_payload};
use arbor_evm::{DomainEnv, EvmError, ExecutionState, GenesisAccount};
use arbor_executor::{ExecutorError, ExecutorService, receipts_root};
use arbor_primitives::{DomainId, Eip1559Transaction, NetworkId};
use arbor_storage::{
    CommitBatch, Database, DatabaseIdentity, DomainStateCommit, FinalizedMarker, IndexedValue,
    RetentionPolicy,
};
use arbor_system::{PROTOCOL_INFO_ADDRESS, protocol_info_selector};
use k256::ecdsa::SigningKey;

const CHAIN_ID: u64 = 2_048;
const INITIAL_BALANCE: u128 = 10_000_000_000_000_000_000;
const STORAGE_RUNTIME: &[u8] = &[
    0x60, 0x00, 0x35, 0x80, 0x60, 0x00, 0x55, 0x60, 0x00, 0x52, 0x60, 0x01, 0x60, 0x20, 0x60, 0x00,
    0xa1, 0x60, 0x20, 0x60, 0x00, 0xf3,
];
const REVERT_RUNTIME: &[u8] = &[
    0x60, 0x01, 0x60, 0x00, 0x55, 0x60, 0x01, 0x60, 0x00, 0x52, 0x60, 0x01, 0x60, 0x20, 0x60, 0x00,
    0xa1, 0x60, 0x00, 0x60, 0x00, 0xfd,
];

fn domain() -> DomainId {
    DomainId(B256::repeat_byte(0x44))
}

fn signing_key() -> SigningKey {
    SigningKey::from_bytes((&[7_u8; 32]).into()).unwrap()
}

fn sign(mut transaction: Eip1559Transaction) -> Bytes {
    let payload = encode_eip1559_signing_payload(&transaction).unwrap();
    let digest = keccak256(payload);
    let (signature, recovery_id) = signing_key()
        .sign_prehash_recoverable(digest.as_slice())
        .unwrap();
    let bytes = signature.to_bytes();
    transaction.r = U256::from_be_slice(&bytes[..32]);
    transaction.s = U256::from_be_slice(&bytes[32..]);
    transaction.y_parity = recovery_id.is_y_odd();
    encode_eip1559(&transaction).unwrap().into()
}

fn tx(nonce: u64, to: Option<Address>, input: Bytes, value: U256, gas_limit: u64) -> Bytes {
    sign(Eip1559Transaction {
        chain_id: CHAIN_ID,
        nonce,
        max_priority_fee_per_gas: 2,
        max_fee_per_gas: 20,
        gas_limit,
        to,
        value,
        input,
        access_list: Vec::new(),
        y_parity: false,
        r: U256::ZERO,
        s: U256::ZERO,
    })
}

fn sender() -> Address {
    arbor_crypto::recover_eip1559_sender(
        &arbor_codec::decode_eip1559(&tx(
            0,
            Some(Address::ZERO),
            Bytes::new(),
            U256::ZERO,
            21_000,
        ))
        .unwrap(),
    )
    .unwrap()
}

fn create_address(sender: Address, nonce: u64) -> Address {
    let mut payload = Vec::new();
    sender.encode(&mut payload);
    nonce.encode(&mut payload);
    let mut encoded = Vec::new();
    Header {
        list: true,
        payload_length: payload.len(),
    }
    .encode(&mut encoded);
    encoded.extend_from_slice(&payload);
    Address::from_slice(&keccak256(encoded)[12..])
}

fn initcode(runtime: &[u8]) -> Bytes {
    let offset = 12_u8;
    let length = u8::try_from(runtime.len()).unwrap();
    let mut code = vec![
        0x60, length, 0x60, offset, 0x60, 0x00, 0x39, 0x60, length, 0x60, 0x00, 0xf3,
    ];
    code.extend_from_slice(runtime);
    code.into()
}

fn genesis() -> ExecutionState {
    let mut allocations = BTreeMap::new();
    allocations.insert(
        sender(),
        GenesisAccount {
            balance: U256::from(INITIAL_BALANCE),
            ..GenesisAccount::default()
        },
    );
    allocations.insert(
        address!("0000000000000000000000000000000000000bad"),
        GenesisAccount {
            code: Bytes::from_static(REVERT_RUNTIME),
            ..GenesisAccount::default()
        },
    );
    ExecutionState::from_genesis(&allocations).unwrap()
}

fn env(gas_limit: u64) -> DomainEnv {
    DomainEnv {
        chain_id: CHAIN_ID,
        block_number: 1,
        timestamp: 1_700_000_001,
        beneficiary: address!("0000000000000000000000000000000000000fee"),
        gas_limit,
        base_fee_per_gas: 10,
        prevrandao: B256::repeat_byte(0x77),
    }
}

fn fixed_block() -> Vec<Bytes> {
    let contract = create_address(sender(), 1);
    let mut word = [0_u8; 32];
    word[31] = 42;
    let mut out_of_gas_word = word;
    out_of_gas_word[31] = 43;
    vec![
        tx(
            0,
            Some(address!("00000000000000000000000000000000000000aa")),
            Bytes::new(),
            U256::from(123),
            21_000,
        ),
        tx(1, None, initcode(STORAGE_RUNTIME), U256::ZERO, 250_000),
        tx(
            2,
            Some(contract),
            Bytes::copy_from_slice(&word),
            U256::ZERO,
            150_000,
        ),
        tx(
            3,
            Some(address!("0000000000000000000000000000000000000bad")),
            Bytes::new(),
            U256::ZERO,
            100_000,
        ),
        tx(
            4,
            Some(PROTOCOL_INFO_ADDRESS),
            Bytes::copy_from_slice(&protocol_info_selector()),
            U256::ZERO,
            50_000,
        ),
        tx(
            5,
            Some(contract),
            Bytes::copy_from_slice(&out_of_gas_word),
            U256::ZERO,
            25_000,
        ),
    ]
}

#[test]
fn transfer_deploy_call_log_revert_oog_and_protocol_info() {
    let parent = genesis();
    let result = ExecutorService::new(1)
        .unwrap()
        .execute_batch(env(30_000_000), &parent, &fixed_block())
        .unwrap();
    assert_eq!(
        result
            .receipts
            .iter()
            .map(|receipt| receipt.status)
            .collect::<Vec<_>>(),
        vec![true, true, true, false, true, false]
    );
    assert_eq!(result.receipts[2].logs.len(), 1);
    assert!(result.receipts[3].logs.is_empty());
    assert!(result.receipts[5].logs.is_empty());
    assert_eq!(result.transactions[4].output.len(), 128);
    assert_eq!(&result.transactions[4].output[28..32], &1_u32.to_be_bytes());
    assert_eq!(result.transactions[4].output[63], 1);
    assert_eq!(
        &result.transactions[4].output[120..128],
        &CHAIN_ID.to_be_bytes()
    );

    let contract = result.transactions[1].contract_address.unwrap();
    assert_eq!(contract, create_address(sender(), 1));
    assert_eq!(
        result.state.storage(contract, U256::ZERO).unwrap(),
        U256::from(42)
    );
    assert_eq!(
        result
            .state
            .storage(
                address!("0000000000000000000000000000000000000bad"),
                U256::ZERO
            )
            .unwrap(),
        U256::ZERO
    );
    assert_eq!(result.state.account(sender()).unwrap().unwrap().nonce, 6);
    assert_eq!(
        result
            .state
            .account(address!("00000000000000000000000000000000000000aa"))
            .unwrap()
            .unwrap()
            .balance,
        U256::from(123)
    );
    assert_eq!(
        result.receipts_root,
        receipts_root(&result.encoded_receipts)
    );
    let vectors = include_str!("../../../testdata/vectors/arbor-v1/execution-roots.txt");
    let expected = |name: &str| {
        vectors
            .lines()
            .filter_map(|line| line.split_once('='))
            .find_map(|(key, value)| (key == name).then_some(value))
            .unwrap()
    };
    assert_eq!(
        result.state.state_root().to_string(),
        expected("state_root")
    );
    assert_eq!(
        result.transactions_root.to_string(),
        expected("transactions_root")
    );
    assert_eq!(result.receipts_root.to_string(), expected("receipts_root"));
    assert_eq!(result.logs_bloom.to_string(), expected("logs_bloom"));
    assert_eq!(result.gas_used.to_string(), expected("gas_used"));
    assert_eq!(sender().to_string().to_lowercase(), expected("sender"));
    assert_eq!(
        contract.to_string().to_lowercase(),
        expected("created_contract")
    );
}

#[test]
fn invalid_transactions_and_aggregate_block_gas_are_rejected() {
    let parent = genesis();
    let service = ExecutorService::new(1).unwrap();
    let wrong_chain = sign(Eip1559Transaction {
        chain_id: CHAIN_ID + 1,
        nonce: 0,
        max_priority_fee_per_gas: 2,
        max_fee_per_gas: 20,
        gas_limit: 21_000,
        to: Some(Address::ZERO),
        value: U256::ZERO,
        input: Bytes::new(),
        access_list: Vec::new(),
        y_parity: false,
        r: U256::ZERO,
        s: U256::ZERO,
    });
    assert!(matches!(
        service.execute_batch(env(30_000_000), &parent, &[wrong_chain]),
        Err(ExecutorError::WrongChainId { .. })
    ));
    assert!(matches!(
        service.execute_batch(
            env(30_000_000),
            &parent,
            &[tx(1, Some(Address::ZERO), Bytes::new(), U256::ZERO, 21_000)]
        ),
        Err(ExecutorError::Transaction { .. })
    ));
    assert!(matches!(
        service.execute_batch(
            env(30_000),
            &parent,
            &[
                tx(0, Some(Address::ZERO), Bytes::new(), U256::ZERO, 30_000),
                tx(1, Some(Address::ZERO), Bytes::new(), U256::ZERO, 30_000),
            ]
        ),
        Err(ExecutorError::BlockGasOverflow { .. })
    ));
    assert_eq!(parent.account(sender()).unwrap().unwrap().nonce, 0);
}

#[test]
fn storage_refund_and_eip1559_burn_reward_accounting_are_applied() {
    let service = ExecutorService::new(1).unwrap();
    let first = service
        .execute_batch(env(30_000_000), &genesis(), &fixed_block())
        .unwrap();
    let contract = first.transactions[1].contract_address.unwrap();
    let sender_before = first.state.account(sender()).unwrap().unwrap().balance;
    let beneficiary = env(30_000_000).beneficiary;
    let reward_before = first
        .state
        .account(beneficiary)
        .unwrap()
        .unwrap_or_default()
        .balance;
    let mut zero = [0_u8; 32];
    let second = service
        .execute_batch(
            DomainEnv {
                block_number: 2,
                timestamp: 1_700_000_002,
                ..env(30_000_000)
            },
            &first.state,
            &[tx(
                6,
                Some(contract),
                Bytes::copy_from_slice(&zero),
                U256::ZERO,
                100_000,
            )],
        )
        .unwrap();
    zero.fill(0);
    assert!(second.transactions[0].gas_used < 30_000);
    assert_eq!(
        second.state.storage(contract, U256::ZERO).unwrap(),
        U256::ZERO
    );
    let sender_after = second.state.account(sender()).unwrap().unwrap().balance;
    let reward_after = second.state.account(beneficiary).unwrap().unwrap().balance;
    let gas = U256::from(second.gas_used);
    assert_eq!(sender_before - sender_after, gas * U256::from(12));
    assert_eq!(reward_after - reward_before, gas * U256::from(2));
    assert_eq!(
        (sender_before + reward_before) - (sender_after + reward_after),
        gas * U256::from(10)
    );
}

#[test]
fn protocol_info_rejects_value_and_wrong_selector_inside_evm_journal() {
    let result = ExecutorService::new(1)
        .unwrap()
        .execute_batch(
            env(30_000_000),
            &genesis(),
            &[
                tx(
                    0,
                    Some(PROTOCOL_INFO_ADDRESS),
                    Bytes::copy_from_slice(&protocol_info_selector()),
                    U256::from(1),
                    50_000,
                ),
                tx(
                    1,
                    Some(PROTOCOL_INFO_ADDRESS),
                    Bytes::from_static(&[0, 0, 0, 0]),
                    U256::ZERO,
                    50_000,
                ),
            ],
        )
        .unwrap();
    assert_eq!(
        result
            .receipts
            .iter()
            .map(|receipt| receipt.status)
            .collect::<Vec<_>>(),
        vec![false, false]
    );
    assert_eq!(result.state.account(sender()).unwrap().unwrap().nonce, 2);
    assert_eq!(
        result
            .state
            .account(PROTOCOL_INFO_ADDRESS)
            .unwrap()
            .unwrap_or_default()
            .balance,
        U256::ZERO
    );
}

#[test]
fn fixed_block_survives_durable_restart_with_identical_roots() {
    let result = ExecutorService::new(1)
        .unwrap()
        .execute_batch(env(30_000_000), &genesis(), &fixed_block())
        .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let identity = DatabaseIdentity {
        network_id: NetworkId(B256::repeat_byte(0x11)),
        genesis_hash: B256::repeat_byte(0x22),
    };
    let mut batch = CommitBatch::new(FinalizedMarker {
        height: 1,
        consensus_hash: B256::repeat_byte(0x33),
        domain_heads_root: B256::repeat_byte(0x55),
    });
    batch.states.push(DomainStateCommit {
        domain_id: domain(),
        consensus_height: 1,
        snapshot: result.state.snapshot().clone(),
    });
    batch.contract_code = result.state.contract_code().clone();
    batch.receipts = result
        .encoded_receipts
        .iter()
        .enumerate()
        .map(|(index, receipt)| IndexedValue {
            key: (index as u64).to_be_bytes().to_vec(),
            value: receipt.clone(),
        })
        .collect();
    let state_root = result.state.state_root();
    let receipt_root = result.receipts_root;
    let contract = result.transactions[1].contract_address.unwrap();
    {
        let database =
            Database::open(directory.path(), identity, RetentionPolicy::Archive).unwrap();
        database.commit(batch).unwrap();
    }
    let database = Database::open(directory.path(), identity, RetentionPolicy::Archive).unwrap();
    assert_eq!(database.state_root(domain(), 1).unwrap(), state_root);
    assert!(database.inspect().unwrap().roots[0].error.is_none());
    let restored = ExecutionState::from_persisted(state_root, &database, |hash| {
        database
            .contract_code(hash)
            .map_err(|error| EvmError::State(error.to_string()))
    })
    .unwrap();
    assert_eq!(restored.state_root(), state_root);
    assert_eq!(
        restored.storage(contract, U256::ZERO).unwrap(),
        U256::from(42)
    );
    let encoded = (0..fixed_block().len())
        .map(|index| {
            database
                .receipt(&(index as u64).to_be_bytes())
                .unwrap()
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(receipts_root(&encoded), receipt_root);
}
