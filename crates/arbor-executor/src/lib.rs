//! Deterministic single-domain batch execution, Ethereum roots, receipts, and service API.

#![forbid(unsafe_code)]

use alloy_primitives::{Address, B256, Bloom, Bytes};
use alloy_trie::root::ordered_trie_root_encoded;
use arbor_codec::{decode_eip1559, encode_eip1559_receipt};
use arbor_crypto::{eip1559_transaction_hash, recover_eip1559_sender};
use arbor_evm::{DomainEnv, EvmError, ExecutionState, ProtocolSpec, execute_transaction};
use arbor_primitives::{Eip1559Transaction, Log, Receipt};
use thiserror::Error;

/// Maximum type-2 transactions in one domain batch.
pub const MAX_TRANSACTIONS_PER_BATCH: usize = 10_000;

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
        execute_batch(self.spec, env, parent, envelopes)
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
        let result = execute_transaction(&mut state, spec, env, &transaction, sender)
            .map_err(|source| ExecutorError::Transaction { index, source })?;
        cumulative_gas =
            cumulative_gas
                .checked_add(result.gas_used)
                .ok_or(ExecutorError::BlockGasOverflow {
                    index,
                    used: u64::MAX,
                    limit: env.gas_limit,
                })?;
        if cumulative_gas > env.gas_limit {
            return Err(ExecutorError::BlockGasOverflow {
                index,
                used: cumulative_gas,
                limit: env.gas_limit,
            });
        }
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
    })
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
