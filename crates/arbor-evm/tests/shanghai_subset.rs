//! Frozen Shanghai execution-spec subset for the Arbor `revm` environment adapter.

use std::collections::BTreeMap;

use alloy_primitives::{Address, B256, Bytes, U256, address};
use arbor_evm::{
    DomainEnv, EvmError, ExecutionState, GenesisAccount, ProtocolSpec, execute_transaction,
};
use arbor_primitives::Eip1559Transaction;

const PUSH0_CONTRACT: Address = address!("0000000000000000000000000000000000003855");

fn state() -> ExecutionState {
    let mut allocations = BTreeMap::new();
    allocations.insert(
        address!("0000000000000000000000000000000000000001"),
        GenesisAccount {
            balance: U256::from(1_000_000_000_u64),
            ..GenesisAccount::default()
        },
    );
    // PUSH0 PUSH0 MSTORE PUSH1 0x20 PUSH0 RETURN
    allocations.insert(
        PUSH0_CONTRACT,
        GenesisAccount {
            code: Bytes::from_static(&[0x5f, 0x5f, 0x52, 0x60, 0x20, 0x5f, 0xf3]),
            ..GenesisAccount::default()
        },
    );
    ExecutionState::from_genesis(&allocations).unwrap()
}

fn env() -> DomainEnv {
    DomainEnv {
        chain_id: 1,
        block_number: 17_034_871,
        timestamp: 1_681_338_456,
        beneficiary: Address::ZERO,
        gas_limit: 30_000_000,
        base_fee_per_gas: 1,
        prevrandao: B256::repeat_byte(1),
    }
}

fn transaction(to: Option<Address>, input: Bytes, gas_limit: u64) -> Eip1559Transaction {
    Eip1559Transaction {
        chain_id: 1,
        nonce: 0,
        max_priority_fee_per_gas: 0,
        max_fee_per_gas: 1,
        gas_limit,
        to,
        value: U256::ZERO,
        input,
        access_list: Vec::new(),
        y_parity: false,
        r: U256::ZERO,
        s: U256::ZERO,
    }
}

#[test]
fn eip3855_push0_is_active_at_protocol_revision_one() {
    let mut state = state();
    let result = execute_transaction(
        &mut state,
        ProtocolSpec::V1,
        env(),
        &transaction(Some(PUSH0_CONTRACT), Bytes::new(), 50_000),
        address!("0000000000000000000000000000000000000001"),
    )
    .unwrap();
    assert!(result.success);
    assert_eq!(result.output, Bytes::from(vec![0_u8; 32]));
}

#[test]
fn eip3860_initcode_and_intrinsic_gas_limits_are_enforced() {
    let mut state = state();
    let initial_root = state.state_root();
    assert!(matches!(
        execute_transaction(
            &mut state,
            ProtocolSpec::V1,
            env(),
            &transaction(None, Bytes::from(vec![0_u8; 49_153]), 1_000_000),
            address!("0000000000000000000000000000000000000001"),
        ),
        Err(EvmError::InvalidTransaction(_))
    ));
    assert_eq!(state.state_root(), initial_root);

    assert!(matches!(
        execute_transaction(
            &mut state,
            ProtocolSpec::V1,
            env(),
            &transaction(Some(Address::ZERO), Bytes::from_static(&[1]), 21_000),
            address!("0000000000000000000000000000000000000001"),
        ),
        Err(EvmError::InvalidTransaction(_))
    ));
    assert_eq!(state.state_root(), initial_root);
}

#[test]
fn frozen_fixture_declares_the_same_revision_and_limits() {
    let fixture = include_str!("../../../testdata/ethereum-tests/shanghai/arbor-subset.txt");
    assert!(fixture.contains("evm_revision=Shanghai"));
    assert!(fixture.contains("max_initcode_bytes=49152"));
    assert!(fixture.contains("push0_runtime=0x5f5f5260205ff3"));
}
