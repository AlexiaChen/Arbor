//! Deterministic single-domain batch execution, Ethereum roots, receipts, and service API.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;

use alloy_primitives::{Address, B256, Bloom, Bytes};
use alloy_trie::root::ordered_trie_root_encoded;
use arbor_codec::{decode_eip1559, encode_eip1559_receipt};
use arbor_crypto::{eip1559_transaction_hash, recover_eip1559_sender};
use arbor_evm::{
    DomainEnv, EvmError, ExecutionState, GenesisAccount, ProtocolSpec,
    execute_transaction_with_system,
};
use arbor_primitives::{DomainDescriptor, DomainId, Eip1559Transaction, Log, NetworkId, Receipt};
use arbor_system::{
    CHAIN_REGISTRY_ADDRESS, ChainRegistryRuntime, PreparedChainCreation, decode_create_chain_call,
    prepare_chain_creation,
};
use thiserror::Error;

/// Maximum type-2 transactions in one domain batch.
pub const MAX_TRANSACTIONS_PER_BATCH: usize = 10_000;

/// Proposal-start inputs available only while executing the root registry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChainRegistryExecutionContext {
    /// Network domain separation used by deterministic domain IDs.
    pub network_id: NetworkId,
    /// The only domain allowed to mutate the registry.
    pub root_domain_id: DomainId,
    /// Domain whose batch is currently executing.
    pub executing_domain_id: DomainId,
    /// Consensus height being proposed.
    pub creation_height: u64,
    /// Root-governance executor authorized for registry lifecycle calls.
    pub governance_address: Address,
    /// Finalized heads captured before any batch in this proposal executes.
    pub finalized_heads: BTreeMap<DomainId, B256>,
}

/// New domain state produced by one successful root registry transaction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreatedDomain {
    /// Root-registry descriptor committed by the transaction.
    pub descriptor: DomainDescriptor,
    /// Empty-state-plus-owner genesis authenticated state.
    pub genesis_state: ExecutionState,
}

/// RPC/index-friendly fields derived while executing one transaction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutedTransaction {
    /// Standard Ethereum typed transaction hash.
    pub transaction_hash: B256,
    /// Recovered transaction sender.
    pub sender: Address,
    /// Receipt status.
    pub success: bool,
    /// Gas used by this transaction after refunds.
    pub gas_used: u64,
    /// Successful create address, if any.
    pub contract_address: Option<Address>,
    /// Return/revert bytes; not part of the receipt root.
    pub output: Bytes,
}

/// Complete deterministic result of one domain batch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DomainExecutionResult {
    /// Authenticated state after all transactions.
    pub state: ExecutionState,
    /// Standard Ethereum transaction trie root over original envelopes.
    pub transactions_root: B256,
    /// Standard Ethereum typed receipt trie root.
    pub receipts_root: B256,
    /// OR of every receipt bloom.
    pub logs_bloom: Bloom,
    /// Cumulative gas used by the domain block.
    pub gas_used: u64,
    /// Consensus receipt fields in transaction order.
    pub receipts: Vec<Receipt>,
    /// Exact typed-RLP receipts suitable for durable storage.
    pub encoded_receipts: Vec<Vec<u8>>,
    /// Non-consensus derived execution fields.
    pub transactions: Vec<ExecutedTransaction>,
    /// Successful registry creations in exact transaction order.
    pub created_domains: Vec<CreatedDomain>,
}

/// Block-invalid execution failure. EVM revert/halt is not an error here and yields status zero.
#[derive(Debug, Error)]
pub enum ExecutorError {
    /// The batch exceeds its transaction-count budget.
    #[error("domain batch has {actual} transactions; limit is {limit}")]
    TooManyTransactions {
        /// Protocol maximum.
        limit: usize,
        /// Observed count.
        actual: usize,
    },
    /// A typed transaction envelope or signature is malformed.
    #[error("transaction {index} envelope/signature is invalid: {reason}")]
    InvalidEnvelope {
        /// Transaction position.
        index: usize,
        /// Stable error description.
        reason: String,
    },
    /// A transaction targets another domain chain ID.
    #[error("transaction {index} chain ID {actual} does not match domain chain ID {expected}")]
    WrongChainId {
        /// Transaction position.
        index: usize,
        /// Expected domain chain ID.
        expected: u64,
        /// Encoded transaction chain ID.
        actual: u64,
    },
    /// Revm rejected transaction validity or authenticated state.
    #[error("transaction {index} execution is block-invalid: {source}")]
    Transaction {
        /// Transaction position.
        index: usize,
        /// EVM adapter failure.
        #[source]
        source: EvmError,
    },
    /// Actual cumulative gas exceeds the explicit domain block limit.
    #[error("domain block gas overflow at transaction {index}: used {used}, limit {limit}")]
    BlockGasOverflow {
        /// Transaction position.
        index: usize,
        /// Cumulative gas used.
        used: u64,
        /// Domain block limit.
        limit: u64,
    },
    /// Receipt encoding exceeded a protocol limit.
    #[error("receipt {index} encoding failed: {reason}")]
    ReceiptEncoding {
        /// Transaction position.
        index: usize,
        /// Codec error.
        reason: String,
    },
    /// Proposal-derived native system input could not be materialized.
    #[error("transaction {index} native system preparation failed: {reason}")]
    SystemPreparation {
        /// Transaction position.
        index: usize,
        /// Stable failure context.
        reason: String,
    },
}

/// Minimal in-process execution service used before M9 RPC wiring.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExecutorService {
    spec: ProtocolSpec,
}

impl ExecutorService {
    /// Creates a service for one exact registered protocol revision.
    ///
    /// # Errors
    ///
    /// Returns [`EvmError`] if the revision is unknown.
    pub fn new(protocol_revision: u32) -> Result<Self, EvmError> {
        Ok(Self {
            spec: ProtocolSpec::resolve(protocol_revision)?,
        })
    }

    /// Executes one domain block without mutating the parent state on failure.
    ///
    /// # Errors
    ///
    /// Returns [`ExecutorError`] for a block-invalid envelope, transaction, or gas overflow.
    pub fn execute_batch(
        &self,
        env: DomainEnv,
        parent: &ExecutionState,
        envelopes: &[Bytes],
    ) -> Result<DomainExecutionResult, ExecutorError> {
        execute_batch_with_registry(self.spec, env, parent, envelopes, None)
    }

    /// Executes a root batch with proposal-start `ChainRegistry` inputs.
    ///
    /// # Errors
    ///
    /// Returns [`ExecutorError`] under the same conditions as [`Self::execute_batch`].
    pub fn execute_batch_with_registry(
        &self,
        env: DomainEnv,
        parent: &ExecutionState,
        envelopes: &[Bytes],
        registry: &ChainRegistryExecutionContext,
    ) -> Result<DomainExecutionResult, ExecutorError> {
        execute_batch_with_registry(self.spec, env, parent, envelopes, Some(registry))
    }
}

/// Executes a fixed ordered batch against a cloned parent state.
///
/// # Errors
///
/// Returns [`ExecutorError`] for block-invalid input. Reverts and out-of-gas halts produce
/// status-zero receipts and continue to the next transaction.
pub fn execute_batch(
    spec: ProtocolSpec,
    env: DomainEnv,
    parent: &ExecutionState,
    envelopes: &[Bytes],
) -> Result<DomainExecutionResult, ExecutorError> {
    execute_batch_with_registry(spec, env, parent, envelopes, None)
}

fn execute_batch_with_registry(
    spec: ProtocolSpec,
    env: DomainEnv,
    parent: &ExecutionState,
    envelopes: &[Bytes],
    registry: Option<&ChainRegistryExecutionContext>,
) -> Result<DomainExecutionResult, ExecutorError> {
    if envelopes.len() > MAX_TRANSACTIONS_PER_BATCH {
        return Err(ExecutorError::TooManyTransactions {
            limit: MAX_TRANSACTIONS_PER_BATCH,
            actual: envelopes.len(),
        });
    }
    let transactions_root = ordered_trie_root_encoded(envelopes);
    let mut state = parent.clone();
    let mut cumulative_gas = 0_u64;
    let mut block_bloom = Bloom::ZERO;
    let mut receipts = Vec::with_capacity(envelopes.len());
    let mut encoded_receipts = Vec::with_capacity(envelopes.len());
    let mut executed = Vec::with_capacity(envelopes.len());
    let mut created_domains = Vec::new();

    for (index, envelope) in envelopes.iter().enumerate() {
        let transaction = decode_transaction(index, envelope)?;
        if transaction.chain_id != env.chain_id {
            return Err(ExecutorError::WrongChainId {
                index,
                expected: env.chain_id,
                actual: transaction.chain_id,
            });
        }
        let sender = recover_eip1559_sender(&transaction).map_err(|error| {
            ExecutorError::InvalidEnvelope {
                index,
                reason: error.to_string(),
            }
        })?;
        let transaction_hash = eip1559_transaction_hash(&transaction).map_err(|error| {
            ExecutorError::InvalidEnvelope {
                index,
                reason: error.to_string(),
            }
        })?;
        let (prepared_creation, genesis_state) =
            prepare_registry_creation(index, registry, &transaction, transaction_hash)?;
        let registry_runtime = registry_runtime(registry, prepared_creation.clone());
        let result = execute_transaction_with_system(
            &mut state,
            spec,
            env,
            &transaction,
            sender,
            registry_runtime,
        )
        .map_err(|source| ExecutorError::Transaction { index, source })?;
        cumulative_gas =
            checked_cumulative_gas(index, cumulative_gas, result.gas_used, env.gas_limit)?;
        let receipt_bloom = logs_bloom(&result.logs);
        block_bloom.accrue_bloom(&receipt_bloom);
        let receipt = Receipt {
            status: result.success,
            cumulative_gas_used: cumulative_gas,
            logs_bloom: receipt_bloom,
            logs: result.logs,
        };
        let encoded =
            encode_eip1559_receipt(&receipt).map_err(|error| ExecutorError::ReceiptEncoding {
                index,
                reason: error.to_string(),
            })?;
        executed.push(ExecutedTransaction {
            transaction_hash,
            sender,
            success: receipt.status,
            gas_used: result.gas_used,
            contract_address: result.created_address,
            output: result.output,
        });
        if let Some(descriptor) = result.created_domain {
            let genesis_state = genesis_state.ok_or_else(|| ExecutorError::SystemPreparation {
                index,
                reason: "successful registry call has no prepared genesis state".to_owned(),
            })?;
            created_domains.push(CreatedDomain {
                descriptor,
                genesis_state,
            });
        }
        receipts.push(receipt);
        encoded_receipts.push(encoded);
    }
    let receipts_root = ordered_trie_root_encoded(&encoded_receipts);
    Ok(DomainExecutionResult {
        state,
        transactions_root,
        receipts_root,
        logs_bloom: block_bloom,
        gas_used: cumulative_gas,
        receipts,
        encoded_receipts,
        transactions: executed,
        created_domains,
    })
}

fn registry_runtime(
    context: Option<&ChainRegistryExecutionContext>,
    prepared_creation: Option<PreparedChainCreation>,
) -> Option<ChainRegistryRuntime> {
    context.map(|context| ChainRegistryRuntime {
        root_domain_id: context.root_domain_id,
        executing_domain_id: context.executing_domain_id,
        consensus_height: context.creation_height,
        governance_address: context.governance_address,
        prepared_creation,
    })
}

fn prepare_registry_creation(
    index: usize,
    registry: Option<&ChainRegistryExecutionContext>,
    transaction: &Eip1559Transaction,
    transaction_hash: B256,
) -> Result<(Option<PreparedChainCreation>, Option<ExecutionState>), ExecutorError> {
    let Some(registry) = registry else {
        return Ok((None, None));
    };
    if registry.executing_domain_id != registry.root_domain_id
        || transaction.to != Some(CHAIN_REGISTRY_ADDRESS)
    {
        return Ok((None, None));
    }
    let Ok(request) = decode_create_chain_call(&transaction.input) else {
        return Ok((None, None));
    };
    let Some(&joint) = registry.finalized_heads.get(&request.parent_domain_id) else {
        return Ok((None, None));
    };
    let genesis_state = ExecutionState::from_genesis(&BTreeMap::from([(
        request.owner,
        GenesisAccount {
            balance: request.initial_supply,
            ..GenesisAccount::default()
        },
    )]))
    .map_err(|error| ExecutorError::SystemPreparation {
        index,
        reason: error.to_string(),
    })?;
    let Ok(prepared) = prepare_chain_creation(
        request,
        registry.network_id,
        transaction_hash,
        joint,
        genesis_state.state_root(),
        transaction.value,
        registry.creation_height,
    ) else {
        return Ok((None, None));
    };
    Ok((Some(prepared), Some(genesis_state)))
}

fn checked_cumulative_gas(
    index: usize,
    current: u64,
    transaction: u64,
    limit: u64,
) -> Result<u64, ExecutorError> {
    let used = current
        .checked_add(transaction)
        .ok_or(ExecutorError::BlockGasOverflow {
            index,
            used: u64::MAX,
            limit,
        })?;
    if used > limit {
        return Err(ExecutorError::BlockGasOverflow { index, used, limit });
    }
    Ok(used)
}

fn decode_transaction(index: usize, envelope: &[u8]) -> Result<Eip1559Transaction, ExecutorError> {
    decode_eip1559(envelope).map_err(|error| ExecutorError::InvalidEnvelope {
        index,
        reason: error.to_string(),
    })
}

fn logs_bloom(logs: &[Log]) -> Bloom {
    let mut bloom = Bloom::ZERO;
    for log in logs {
        bloom.accrue_raw_log(log.address, &log.topics);
    }
    bloom
}

/// Computes the exact Ethereum ordered trie root of typed transaction envelopes.
#[must_use]
pub fn transactions_root(envelopes: &[Bytes]) -> B256 {
    ordered_trie_root_encoded(envelopes)
}

/// Computes the exact Ethereum ordered trie root of typed receipt encodings.
#[must_use]
pub fn receipts_root(encoded_receipts: &[Vec<u8>]) -> B256 {
    ordered_trie_root_encoded(encoded_receipts)
}
