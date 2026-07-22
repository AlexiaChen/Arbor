use alloy_primitives::{Bytes, TxKind, U256};
use arbor_primitives::{AccessListItem as ArborAccessListItem, Eip1559Transaction, Log};
use arbor_system::{
    PROTOCOL_INFO_ADDRESS, PROTOCOL_INFO_GAS, encode_protocol_info, protocol_info_selector,
};
use revm::{
    Context, ExecuteEvm, MainBuilder, MainContext,
    context::{BlockEnv, Cfg, TxEnv},
    context_interface::{ContextTr, transaction::AccessList},
    handler::{EthPrecompiles, PrecompileProvider, precompile_output_to_interpreter_result},
    interpreter::{CallInputs, InterpreterResult},
    precompile::PrecompileOutput,
    primitives::{AddressSet, hardfork::SpecId},
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
    pub logs: Vec<Log>,
    /// Return or revert bytes; empty for a halt.
    pub output: Bytes,
    /// Address produced by successful contract creation.
    pub created_address: Option<alloy_primitives::Address>,
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
    let mut evm = evm.with_precompiles(ArborPrecompiles::new(evm_spec, spec, env.chain_id));
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
        .map(|log| Log {
            address: log.address,
            topics: log.data.topics().to_vec(),
            data: log.data.data.clone(),
        })
        .collect();
    state.apply_changes(outcome.state)?;
    Ok(EvmExecution {
        success,
        gas_used,
        logs,
        output,
        created_address,
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
}

impl ArborPrecompiles {
    fn new(evm_spec: SpecId, spec: ProtocolSpec, chain_id: u64) -> Self {
        let ethereum = EthPrecompiles::new(evm_spec);
        let mut warm = ethereum.warm_addresses().clone();
        warm.insert(PROTOCOL_INFO_ADDRESS);
        Self {
            ethereum,
            warm,
            spec,
            chain_id,
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
        <EthPrecompiles as PrecompileProvider<CTX>>::run(&mut self.ethereum, context, inputs)
    }

    fn warm_addresses(&self) -> &AddressSet {
        &self.warm
    }
}
