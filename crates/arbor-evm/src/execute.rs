use alloy_primitives::{Address, Bytes, TxKind, U256};
use arbor_primitives::{
    AccessListItem as ArborAccessListItem, DomainDescriptor, Eip1559Transaction, Log as ArborLog,
};
use arbor_system::{
    CHAIN_REGISTRY_ADDRESS, CHAIN_REGISTRY_CREATE_GAS, CHAIN_REGISTRY_DEPOSIT_GAS,
    ChainRegistryRuntime, CreationDepositStatus, PROTOCOL_INFO_ADDRESS, PROTOCOL_INFO_GAS,
    chain_id_slot, create_chain_selector, creation_storage_writes, decode_burn_deposit_call,
    decode_refund_deposit_call, deposit_slot, deposit_status_slot, deposit_unlock_slot,
    descriptor_slot, encode_create_chain_call, encode_protocol_info, owner_slot,
    protocol_info_selector,
};
use revm::{
    Context, ExecuteEvm, MainBuilder, MainContext,
    context::{BlockEnv, Cfg, TxEnv},
    context_interface::{ContextTr, JournalTr, transaction::AccessList},
    handler::{EthPrecompiles, PrecompileProvider, precompile_output_to_interpreter_result},
    interpreter::{CallInputs, InterpreterResult},
    precompile::PrecompileOutput,
    primitives::{AddressSet, Log, LogData, hardfork::SpecId},
};

use crate::{DomainEnv, EvmError, ExecutionState, ProtocolSpec, state::ExecutionDatabase};

/// Consensus-relevant result of one valid transaction execution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EvmExecution {
    /// `true` only for successful EVM completion; revert/halt are receipt status zero.
    pub success: bool,
    /// Gas consumed after refunds.
    pub gas_used: u64,
    /// Logs surviving EVM journaling.
    pub logs: Vec<ArborLog>,
    /// Return or revert bytes; empty for a halt.
    pub output: Bytes,
    /// Address produced by successful contract creation.
    pub created_address: Option<alloy_primitives::Address>,
    /// Successful root `ChainRegistry` creation, if this was that native call.
    pub created_domain: Option<DomainDescriptor>,
}

/// Executes one already decoded and sender-recovered type-2 transaction.
///
/// Invalid transactions leave `state` unchanged. Successful, reverted, and halted
/// executions all apply `revm`'s journal result so nonce and actual gas accounting persist.
///
/// # Errors
///
/// Returns [`EvmError`] for unsupported specs, invalid block/transaction inputs, or
/// malformed authenticated state.
pub fn execute_transaction(
    state: &mut ExecutionState,
    spec: ProtocolSpec,
    env: DomainEnv,
    transaction: &Eip1559Transaction,
    sender: alloy_primitives::Address,
) -> Result<EvmExecution, EvmError> {
    execute_transaction_with_system(state, spec, env, transaction, sender, None)
}

/// Executes one transaction with an optional, proposal-derived root `ChainRegistry` transition.
///
/// The prepared transition contains only consensus inputs captured before proposal execution. Its
/// storage writes and event are journaled by `revm`, so a revert discards the deposit and registry
/// mutation together.
///
/// # Errors
///
/// Returns [`EvmError`] under the same block-invalid conditions as [`execute_transaction`].
pub fn execute_transaction_with_system(
    state: &mut ExecutionState,
    spec: ProtocolSpec,
    env: DomainEnv,
    transaction: &Eip1559Transaction,
    sender: alloy_primitives::Address,
    registry: Option<ChainRegistryRuntime>,
) -> Result<EvmExecution, EvmError> {
    env.validate()?;
    if ProtocolSpec::resolve(spec.protocol_revision)? != spec {
        return Err(EvmError::InvalidEnvironment(
            "ProtocolSpec fields do not match the registered revision",
        ));
    }
    if transaction.input.len() > spec.max_initcode_bytes && transaction.to.is_none() {
        return Err(EvmError::InvalidTransaction(
            "contract initcode exceeds protocol limit".to_owned(),
        ));
    }

    let db = ExecutionDatabase::new(state);
    let block = BlockEnv {
        number: U256::from(env.block_number),
        beneficiary: env.beneficiary,
        timestamp: U256::from(env.timestamp),
        gas_limit: env.gas_limit,
        basefee: u64::try_from(env.base_fee_per_gas)
            .map_err(|_| EvmError::InvalidEnvironment("base fee does not fit u64"))?,
        difficulty: U256::ZERO,
        prevrandao: Some(env.prevrandao),
        blob_excess_gas_and_price: None,
        slot_num: 0,
    };
    let context = Context::mainnet()
        .with_db(db)
        .modify_cfg_chained(|cfg| {
            cfg.set_spec_and_mainnet_gas_params(spec.evm_revision.spec_id());
            cfg.chain_id = env.chain_id;
            cfg.limit_contract_code_size = Some(spec.max_code_bytes);
            cfg.limit_contract_initcode_size = Some(spec.max_initcode_bytes);
        })
        .with_block(block);
    let evm = context.build_mainnet();
    let evm_spec = spec.evm_revision.spec_id();
    let mut evm = evm.with_precompiles(ArborPrecompiles::new(
        evm_spec,
        spec,
        env.chain_id,
        registry.clone(),
    ));
    let outcome = evm
        .transact(to_tx_env(transaction, sender))
        .map_err(map_revm_error)?;
    if outcome.result.logs().len() > spec.max_logs_per_transaction {
        return Err(EvmError::InvalidTransaction(
            "transaction log count exceeds protocol limit".to_owned(),
        ));
    }

    let success = outcome.result.is_success();
    let gas_used = outcome.result.tx_gas_used();
    let created_address = outcome.result.created_address();
    let output = outcome.result.output().cloned().unwrap_or_default();
    let logs = outcome
        .result
        .logs()
        .iter()
        .map(|log| ArborLog {
            address: log.address,
            topics: log.data.topics().to_vec(),
            data: log.data.data.clone(),
        })
        .collect();
    state.apply_changes(outcome.state)?;
    let created_domain = (success
        && transaction.to == Some(CHAIN_REGISTRY_ADDRESS)
        && transaction.input.starts_with(&create_chain_selector()))
    .then(|| {
        registry.and_then(|context| {
            context
                .prepared_creation
                .map(|creation| creation.descriptor)
        })
    })
    .flatten();
    Ok(EvmExecution {
        success,
        gas_used,
        logs,
        output,
        created_address,
        created_domain,
    })
}

fn to_tx_env(transaction: &Eip1559Transaction, sender: alloy_primitives::Address) -> TxEnv {
    let access_list = transaction
        .access_list
        .iter()
        .map(
            |ArborAccessListItem {
                 address,
                 storage_keys,
             }| {
                revm::context_interface::transaction::AccessListItem {
                    address: *address,
                    storage_keys: storage_keys.clone(),
                }
            },
        )
        .collect();
    TxEnv {
        tx_type: 2,
        caller: sender,
        gas_limit: transaction.gas_limit,
        gas_price: transaction.max_fee_per_gas,
        kind: transaction.to.map_or(TxKind::Create, TxKind::Call),
        value: transaction.value,
        data: transaction.input.clone(),
        nonce: transaction.nonce,
        chain_id: Some(transaction.chain_id),
        access_list: AccessList(access_list),
        gas_priority_fee: Some(transaction.max_priority_fee_per_gas),
        blob_hashes: Vec::new(),
        max_fee_per_blob_gas: 0,
        authorization_list: Vec::new(),
    }
}

fn map_revm_error(
    error: revm::context::result::EVMError<
        crate::state::ExecutionDatabaseError,
        revm::context::result::InvalidTransaction,
    >,
) -> EvmError {
    use revm::context::result::EVMError;
    match error {
        EVMError::Transaction(error) => EvmError::InvalidTransaction(error.to_string()),
        EVMError::Header(error) => EvmError::InvalidHeader(error.to_string()),
        EVMError::Database(error) => EvmError::State(error.to_string()),
        EVMError::Custom(error) => EvmError::System(error),
        EVMError::CustomAny(error) => EvmError::System(error.to_string()),
    }
}

#[derive(Debug)]
struct ArborPrecompiles {
    ethereum: EthPrecompiles,
    warm: AddressSet,
    spec: ProtocolSpec,
    chain_id: u64,
    registry: Option<ChainRegistryRuntime>,
}

impl ArborPrecompiles {
    fn new(
        evm_spec: SpecId,
        spec: ProtocolSpec,
        chain_id: u64,
        registry: Option<ChainRegistryRuntime>,
    ) -> Self {
        let ethereum = EthPrecompiles::new(evm_spec);
        let mut warm = ethereum.warm_addresses().clone();
        warm.insert(PROTOCOL_INFO_ADDRESS);
        warm.insert(CHAIN_REGISTRY_ADDRESS);
        Self {
            ethereum,
            warm,
            spec,
            chain_id,
            registry,
        }
    }
}

impl<CTX> PrecompileProvider<CTX> for ArborPrecompiles
where
    CTX: ContextTr<Cfg: Cfg<Spec = SpecId>>,
{
    type Output = InterpreterResult;

    fn set_spec(&mut self, spec: <CTX::Cfg as Cfg>::Spec) -> bool {
        let changed =
            <EthPrecompiles as PrecompileProvider<CTX>>::set_spec(&mut self.ethereum, spec);
        self.warm.clone_from(self.ethereum.warm_addresses());
        self.warm.insert(PROTOCOL_INFO_ADDRESS);
        self.warm.insert(CHAIN_REGISTRY_ADDRESS);
        changed
    }

    fn run(
        &mut self,
        context: &mut CTX,
        inputs: &CallInputs,
    ) -> Result<Option<Self::Output>, String> {
        if inputs.bytecode_address == PROTOCOL_INFO_ADDRESS {
            let call_data = inputs.input.as_bytes(context);
            let valid = call_data.as_ref() == protocol_info_selector() && !inputs.transfers_value();
            let output = if valid {
                PrecompileOutput::new(
                    PROTOCOL_INFO_GAS,
                    encode_protocol_info(
                        self.spec.protocol_revision,
                        self.spec.evm_revision as u8,
                        self.chain_id,
                    ),
                    inputs.reservoir,
                )
            } else {
                PrecompileOutput::revert(
                    PROTOCOL_INFO_GAS,
                    Bytes::from_static(b"invalid protocolInfo call"),
                    inputs.reservoir,
                )
            };
            return Ok(Some(precompile_output_to_interpreter_result(
                output,
                inputs.gas_limit,
            )));
        }
        if inputs.bytecode_address == CHAIN_REGISTRY_ADDRESS {
            return self.run_chain_registry(context, inputs).map(Some);
        }
        <EthPrecompiles as PrecompileProvider<CTX>>::run(&mut self.ethereum, context, inputs)
    }

    fn warm_addresses(&self) -> &AddressSet {
        &self.warm
    }
}

impl ArborPrecompiles {
    fn run_chain_registry<CTX>(
        &self,
        context: &mut CTX,
        inputs: &CallInputs,
    ) -> Result<InterpreterResult, String>
    where
        CTX: ContextTr<Cfg: Cfg<Spec = SpecId>>,
    {
        let Some(runtime) = self.registry.as_ref() else {
            return Ok(registry_revert(inputs, b"ChainRegistry unavailable"));
        };
        if runtime.executing_domain_id != runtime.root_domain_id {
            return Ok(registry_revert(inputs, b"ChainRegistry is root-only"));
        }
        let calldata = inputs.input.bytes(context);
        if calldata.starts_with(&create_chain_selector()) {
            return Self::run_create_chain(context, inputs, runtime, calldata.as_ref());
        }
        if let Some(domain_id) = decode_refund_deposit_call(calldata.as_ref()) {
            return Self::run_deposit_lifecycle(context, inputs, runtime, domain_id, false);
        }
        if let Some(domain_id) = decode_burn_deposit_call(calldata.as_ref()) {
            return Self::run_deposit_lifecycle(context, inputs, runtime, domain_id, true);
        }
        Ok(registry_revert(inputs, b"unknown ChainRegistry call"))
    }

    fn run_create_chain<CTX>(
        context: &mut CTX,
        inputs: &CallInputs,
        runtime: &ChainRegistryRuntime,
        calldata: &[u8],
    ) -> Result<InterpreterResult, String>
    where
        CTX: ContextTr<Cfg: Cfg<Spec = SpecId>>,
    {
        let Some(prepared) = runtime.prepared_creation.as_ref() else {
            return Ok(registry_revert(inputs, b"invalid createChain call"));
        };
        if inputs.is_static
            || calldata
                != encode_create_chain_call(&prepared.request)
                    .expect("prepared ChainRegistry request is valid")
                    .as_ref()
            || inputs.transfer_value() != Some(prepared.descriptor.creation_deposit)
        {
            return Ok(registry_revert(inputs, b"invalid createChain call"));
        }

        let parent_exists = context
            .journal_mut()
            .sload(
                CHAIN_REGISTRY_ADDRESS,
                descriptor_slot(prepared.descriptor.parent_domain_id),
            )
            .map_err(|_| "ChainRegistry parent lookup failed".to_owned())?
            .data
            != alloy_primitives::U256::ZERO;
        if !parent_exists {
            return Ok(registry_revert(inputs, b"unknown parent domain"));
        }
        let domain_exists = context
            .journal_mut()
            .sload(
                CHAIN_REGISTRY_ADDRESS,
                descriptor_slot(prepared.descriptor.domain_id),
            )
            .map_err(|_| "ChainRegistry domain lookup failed".to_owned())?
            .data
            != alloy_primitives::U256::ZERO;
        if domain_exists {
            return Ok(registry_revert(inputs, b"domain already exists"));
        }
        let chain_id_used = context
            .journal_mut()
            .sload(
                CHAIN_REGISTRY_ADDRESS,
                chain_id_slot(prepared.descriptor.evm_chain_id),
            )
            .map_err(|_| "ChainRegistry chain ID lookup failed".to_owned())?
            .data
            != alloy_primitives::U256::ZERO;
        if chain_id_used {
            return Ok(registry_revert(inputs, b"EVM chain ID already exists"));
        }

        for (slot, value) in creation_storage_writes(prepared) {
            context
                .journal_mut()
                .sstore(CHAIN_REGISTRY_ADDRESS, slot, value)
                .map_err(|_| "ChainRegistry storage update failed".to_owned())?;
        }
        let event = alloy_primitives::keccak256(
            b"ChainCreated(bytes32,bytes32,uint64,address,uint256,uint64)",
        );
        let mut data = Vec::with_capacity(32 * 5);
        data.extend_from_slice(prepared.descriptor.parent_domain_id.0.as_slice());
        let mut chain_id = [0_u8; 32];
        chain_id[24..].copy_from_slice(&prepared.descriptor.evm_chain_id.to_be_bytes());
        data.extend_from_slice(&chain_id);
        let mut owner = [0_u8; 32];
        owner[12..].copy_from_slice(prepared.descriptor.owner.as_slice());
        data.extend_from_slice(&owner);
        data.extend_from_slice(&prepared.descriptor.creation_deposit.to_be_bytes::<32>());
        let mut unlock_height = [0_u8; 32];
        unlock_height[24..].copy_from_slice(&prepared.deposit_unlock_height.to_be_bytes());
        data.extend_from_slice(&unlock_height);
        context.journal_mut().log(Log {
            address: CHAIN_REGISTRY_ADDRESS,
            data: LogData::new(
                vec![event, prepared.descriptor.domain_id.0],
                Bytes::from(data),
            )
            .expect("two ChainCreated topics are valid"),
        });
        Ok(precompile_output_to_interpreter_result(
            PrecompileOutput::new(
                CHAIN_REGISTRY_CREATE_GAS,
                Bytes::copy_from_slice(prepared.descriptor.domain_id.0.as_slice()),
                inputs.reservoir,
            ),
            inputs.gas_limit,
        ))
    }

    fn run_deposit_lifecycle<CTX>(
        context: &mut CTX,
        inputs: &CallInputs,
        runtime: &ChainRegistryRuntime,
        domain_id: arbor_primitives::DomainId,
        burn: bool,
    ) -> Result<InterpreterResult, String>
    where
        CTX: ContextTr<Cfg: Cfg<Spec = SpecId>>,
    {
        if inputs.is_static || inputs.transfers_value() {
            return Ok(registry_revert(inputs, b"invalid deposit lifecycle call"));
        }
        let status = context
            .journal_mut()
            .sload(CHAIN_REGISTRY_ADDRESS, deposit_status_slot(domain_id))
            .map_err(|_| "ChainRegistry deposit status lookup failed".to_owned())?
            .data;
        if status != U256::from(CreationDepositStatus::Locked as u8) {
            return Ok(registry_revert(inputs, b"deposit is not locked"));
        }
        let deposit = context
            .journal_mut()
            .sload(CHAIN_REGISTRY_ADDRESS, deposit_slot(domain_id))
            .map_err(|_| "ChainRegistry deposit lookup failed".to_owned())?
            .data;
        if deposit == U256::ZERO {
            return Ok(registry_revert(inputs, b"deposit does not exist"));
        }

        let (recipient, next_status, event) = if burn {
            if inputs.caller != runtime.governance_address {
                return Ok(registry_revert(inputs, b"unauthorized deposit burn"));
            }
            (
                Address::ZERO,
                CreationDepositStatus::Burned,
                alloy_primitives::keccak256(b"CreationDepositBurned(bytes32,uint256,address)"),
            )
        } else {
            let unlock_height = context
                .journal_mut()
                .sload(CHAIN_REGISTRY_ADDRESS, deposit_unlock_slot(domain_id))
                .map_err(|_| "ChainRegistry unlock-height lookup failed".to_owned())?
                .data;
            if U256::from(runtime.consensus_height) < unlock_height {
                return Ok(registry_revert(inputs, b"deposit remains locked"));
            }
            let owner_word = context
                .journal_mut()
                .sload(CHAIN_REGISTRY_ADDRESS, owner_slot(domain_id))
                .map_err(|_| "ChainRegistry owner lookup failed".to_owned())?
                .data
                .to_be_bytes::<32>();
            let owner = Address::from_slice(&owner_word[12..]);
            if inputs.caller != owner {
                return Ok(registry_revert(inputs, b"deposit refund requires owner"));
            }
            (
                owner,
                CreationDepositStatus::Refunded,
                alloy_primitives::keccak256(b"CreationDepositRefunded(bytes32,uint256,address)"),
            )
        };

        let transfer_error = context
            .journal_mut()
            .transfer(CHAIN_REGISTRY_ADDRESS, recipient, deposit)
            .map_err(|_| "ChainRegistry deposit transfer failed".to_owned())?;
        if transfer_error.is_some() {
            return Ok(registry_revert(
                inputs,
                b"registry deposit balance unavailable",
            ));
        }
        context
            .journal_mut()
            .sstore(CHAIN_REGISTRY_ADDRESS, deposit_slot(domain_id), U256::ZERO)
            .map_err(|_| "ChainRegistry deposit update failed".to_owned())?;
        context
            .journal_mut()
            .sstore(
                CHAIN_REGISTRY_ADDRESS,
                deposit_status_slot(domain_id),
                U256::from(next_status as u8),
            )
            .map_err(|_| "ChainRegistry deposit status update failed".to_owned())?;

        let mut data = Vec::with_capacity(64);
        data.extend_from_slice(&deposit.to_be_bytes::<32>());
        let mut actor = [0_u8; 32];
        actor[12..].copy_from_slice(inputs.caller.as_slice());
        data.extend_from_slice(&actor);
        context.journal_mut().log(Log {
            address: CHAIN_REGISTRY_ADDRESS,
            data: LogData::new(vec![event, domain_id.0], Bytes::from(data))
                .expect("two deposit lifecycle topics are valid"),
        });
        Ok(precompile_output_to_interpreter_result(
            PrecompileOutput::new(
                CHAIN_REGISTRY_DEPOSIT_GAS,
                Bytes::copy_from_slice(domain_id.0.as_slice()),
                inputs.reservoir,
            ),
            inputs.gas_limit,
        ))
    }
}

fn registry_revert(inputs: &CallInputs, message: &'static [u8]) -> InterpreterResult {
    precompile_output_to_interpreter_result(
        PrecompileOutput::revert(
            CHAIN_REGISTRY_CREATE_GAS,
            Bytes::from_static(message),
            inputs.reservoir,
        ),
        inputs.gas_limit,
    )
}
