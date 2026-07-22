//! Versioned native system contracts and the root `ChainRegistry` ABI/state rules.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;

use alloy_primitives::{Address, B256, Bytes, U256, address, keccak256};
use arbor_codec::encode_domain_descriptor;
use arbor_crypto::{derive_domain_id, domain_genesis_hash};
use arbor_primitives::{DomainDescriptor, DomainGenesis, DomainId, DomainStatus, NetworkId};
use thiserror::Error;

/// Version of the system-address registry interpreted by protocol revision one.
pub const SYSTEM_ADDRESS_REGISTRY_VERSION: u32 = 1;
/// Version of the protocol-info native implementation.
pub const PROTOCOL_INFO_IMPLEMENTATION_VERSION: u32 = 1;
/// Version of the first `ChainRegistry` native implementation.
pub const CHAIN_REGISTRY_IMPLEMENTATION_VERSION: u32 = 1;
/// Fixed gas charged by the protocol-info native call after ordinary call intrinsic costs.
pub const PROTOCOL_INFO_GAS: u64 = 500;
/// Fixed native gas charged by one `createChain` call.
pub const CHAIN_REGISTRY_CREATE_GAS: u64 = 180_000;
/// Fixed native gas charged by one deposit lifecycle call.
pub const CHAIN_REGISTRY_DEPOSIT_GAS: u64 = 45_000;
/// Root-governed protocol-revision-one anti-spam deposit.
pub const MIN_CREATION_DEPOSIT: U256 = U256::from_limbs([1_000_000_000_000_000_000, 0, 0, 0]);
/// Consensus heights for which a successful creation deposit remains locked.
pub const CREATION_DEPOSIT_LOCK_BLOCKS: u64 = 100;
/// Reserved address of the read-only protocol-info native system contract.
pub const PROTOCOL_INFO_ADDRESS: Address = address!("0000000000000000000000000000000000000800");
/// Reserved root-only address of the native `ChainRegistry` system contract.
pub const CHAIN_REGISTRY_ADDRESS: Address = address!("0000000000000000000000000000000000000801");
/// Canonical Solidity ABI signature exposed by the protocol-info contract.
pub const PROTOCOL_INFO_ABI_SIGNATURE: &[u8] = b"protocolInfo()";
/// Canonical Solidity ABI signature exposed by the registry.
pub const CREATE_CHAIN_ABI_SIGNATURE: &[u8] =
    b"createChain(bytes32,string,string,uint64,address,uint64,uint128,uint256,uint32)";
/// Canonical owner-call signature that releases an unlocked creation deposit.
pub const REFUND_DEPOSIT_ABI_SIGNATURE: &[u8] = b"refundDeposit(bytes32)";
/// Canonical root-governance signature that irreversibly burns a creation deposit.
pub const BURN_DEPOSIT_ABI_SIGNATURE: &[u8] = b"burnDeposit(bytes32)";

const REGISTRY_DESCRIPTOR_TAG: &[u8] = b"ARBOR_CHAIN_REGISTRY_DESCRIPTOR_V1";
const REGISTRY_CHAIN_ID_TAG: &[u8] = b"ARBOR_CHAIN_REGISTRY_CHAIN_ID_V1";
const REGISTRY_CREATION_HEIGHT_TAG: &[u8] = b"ARBOR_CHAIN_REGISTRY_CREATION_HEIGHT_V1";
const REGISTRY_DEPOSIT_TAG: &[u8] = b"ARBOR_CHAIN_REGISTRY_DEPOSIT_V1";
const REGISTRY_DEPOSIT_UNLOCK_TAG: &[u8] = b"ARBOR_CHAIN_REGISTRY_DEPOSIT_UNLOCK_V1";
const REGISTRY_OWNER_TAG: &[u8] = b"ARBOR_CHAIN_REGISTRY_OWNER_V1";
const REGISTRY_DEPOSIT_STATUS_TAG: &[u8] = b"ARBOR_CHAIN_REGISTRY_DEPOSIT_STATUS_V1";
const ROOT_DOMAIN_MARKER_TAG: &[u8] = b"ARBOR_ROOT_DOMAIN_MARKER_V1";

/// A versioned native entry that cannot be replaced by user bytecode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SystemContract {
    /// Reserved EVM address.
    pub address: Address,
    /// Keccak hash of the canonical ABI signature set.
    pub abi_hash: B256,
    /// Version of the native implementation.
    pub implementation_version: u32,
    /// Maximum fixed native execution gas among this entry's ABI methods.
    pub max_gas_cost: u64,
}

/// ABI request accepted by root `ChainRegistry.createChain`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreateChainRequest {
    /// Existing active parent domain.
    pub parent_domain_id: DomainId,
    /// Display name after deterministic normalization.
    pub name: String,
    /// Display symbol after deterministic normalization.
    pub symbol: String,
    /// Globally unique EVM chain ID.
    pub evm_chain_id: u64,
    /// Recipient of the new domain's initial supply.
    pub owner: Address,
    /// Per-domain EVM gas limit.
    pub gas_limit: u64,
    /// Genesis EIP-1559 base fee.
    pub initial_base_fee: u128,
    /// Genesis native supply.
    pub initial_supply: U256,
    /// Versioned execution rules.
    pub protocol_revision: u32,
}

/// Deterministic creation data supplied to the journaled native precompile.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedChainCreation {
    /// Canonically normalized request.
    pub request: CreateChainRequest,
    /// Descriptor published after the containing consensus block finalizes.
    pub descriptor: DomainDescriptor,
    /// Hash committed in the registry's authenticated storage.
    pub descriptor_hash: B256,
    /// Consensus height at which the containing block is proposed.
    pub creation_height: u64,
    /// First height at which the locked deposit may be refunded by a future lifecycle call.
    pub deposit_unlock_height: u64,
}

/// Consensus inputs available to every root `ChainRegistry` lifecycle call.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChainRegistryRuntime {
    /// The only domain allowed to execute the registry.
    pub root_domain_id: DomainId,
    /// Domain whose transaction is currently executing.
    pub executing_domain_id: DomainId,
    /// Height of the consensus proposal containing the call.
    pub consensus_height: u64,
    /// Root-governance executor authorized to burn locked deposits.
    pub governance_address: Address,
    /// Deterministic creation inputs, present only for a valid `createChain` candidate.
    pub prepared_creation: Option<PreparedChainCreation>,
}

/// Consensus-visible lifecycle state of one creation deposit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum CreationDepositStatus {
    /// Deposit exists and remains refundable or burnable under the protocol rules.
    Locked = 1,
    /// Owner reclaimed the deposit at or after its unlock height.
    Refunded = 2,
    /// Root governance irreversibly removed the deposit from supply.
    Burned = 3,
}

/// ABI, parameter, or deterministic creation failure. Calls map these to EVM revert.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ChainRegistryError {
    /// Calldata is not the exact canonical v1 ABI encoding.
    #[error("invalid createChain ABI calldata")]
    InvalidCalldata,
    /// Display metadata is empty, too long, or outside the stable ASCII profile.
    #[error("invalid domain display metadata")]
    InvalidMetadata,
    /// A protocol field is outside the v1 range.
    #[error("invalid domain creation parameter: {0}")]
    InvalidParameter(&'static str),
    /// The supplied value is below the root-governed minimum.
    #[error("creation deposit {actual} is below minimum {minimum}")]
    InsufficientDeposit {
        /// Required minimum.
        minimum: U256,
        /// Transaction value.
        actual: U256,
    },
    /// Arithmetic for a consensus height overflowed.
    #[error("creation deposit unlock height overflow")]
    HeightOverflow,
    /// Canonical descriptor/genesis encoding failed unexpectedly.
    #[error("domain creation encoding failed: {0}")]
    Encoding(String),
}

/// Returns the protocol-revision-one native protocol-info entry.
#[must_use]
pub fn protocol_info_contract() -> SystemContract {
    SystemContract {
        address: PROTOCOL_INFO_ADDRESS,
        abi_hash: keccak256(PROTOCOL_INFO_ABI_SIGNATURE),
        implementation_version: PROTOCOL_INFO_IMPLEMENTATION_VERSION,
        max_gas_cost: PROTOCOL_INFO_GAS,
    }
}

/// Returns the protocol-revision-one native chain-registry entry.
#[must_use]
pub fn chain_registry_contract() -> SystemContract {
    SystemContract {
        address: CHAIN_REGISTRY_ADDRESS,
        abi_hash: keccak256(
            [
                CREATE_CHAIN_ABI_SIGNATURE,
                REFUND_DEPOSIT_ABI_SIGNATURE,
                BURN_DEPOSIT_ABI_SIGNATURE,
            ]
            .concat(),
        ),
        implementation_version: CHAIN_REGISTRY_IMPLEMENTATION_VERSION,
        max_gas_cost: CHAIN_REGISTRY_CREATE_GAS,
    }
}

/// Returns the four-byte selector for `protocolInfo()`.
#[must_use]
pub fn protocol_info_selector() -> [u8; 4] {
    selector(PROTOCOL_INFO_ABI_SIGNATURE)
}

/// Returns the four-byte selector for `createChain(...)`.
#[must_use]
pub fn create_chain_selector() -> [u8; 4] {
    selector(CREATE_CHAIN_ABI_SIGNATURE)
}

/// Returns the four-byte selector for `refundDeposit(bytes32)`.
#[must_use]
pub fn refund_deposit_selector() -> [u8; 4] {
    selector(REFUND_DEPOSIT_ABI_SIGNATURE)
}

/// Returns the four-byte selector for `burnDeposit(bytes32)`.
#[must_use]
pub fn burn_deposit_selector() -> [u8; 4] {
    selector(BURN_DEPOSIT_ABI_SIGNATURE)
}

/// Encodes one canonical creation-deposit lifecycle call.
#[must_use]
pub fn encode_refund_deposit_call(domain_id: DomainId) -> Bytes {
    encode_domain_id_call(refund_deposit_selector(), domain_id)
}

/// Encodes one canonical governance deposit-burn call.
#[must_use]
pub fn encode_burn_deposit_call(domain_id: DomainId) -> Bytes {
    encode_domain_id_call(burn_deposit_selector(), domain_id)
}

/// Decodes an exact `refundDeposit(bytes32)` call.
#[must_use]
pub fn decode_refund_deposit_call(input: &[u8]) -> Option<DomainId> {
    decode_domain_id_call(input, refund_deposit_selector())
}

/// Decodes an exact `burnDeposit(bytes32)` call.
#[must_use]
pub fn decode_burn_deposit_call(input: &[u8]) -> Option<DomainId> {
    decode_domain_id_call(input, burn_deposit_selector())
}

/// Encodes `protocolInfo()` output as ABI words:
/// `(uint32 protocolRevision, uint8 evmRevision, uint32 registryVersion, uint64 chainId)`.
#[must_use]
pub fn encode_protocol_info(protocol_revision: u32, evm_revision: u8, chain_id: u64) -> Bytes {
    let mut output = vec![0_u8; 32 * 4];
    output[28..32].copy_from_slice(&protocol_revision.to_be_bytes());
    output[63] = evm_revision;
    output[92..96].copy_from_slice(&SYSTEM_ADDRESS_REGISTRY_VERSION.to_be_bytes());
    output[120..128].copy_from_slice(&chain_id.to_be_bytes());
    output.into()
}

/// Canonically normalizes and encodes one `createChain` ABI call.
///
/// # Errors
///
/// Returns [`ChainRegistryError`] for invalid metadata or protocol fields.
pub fn encode_create_chain_call(request: &CreateChainRequest) -> Result<Bytes, ChainRegistryError> {
    const HEAD_WORDS: usize = 9;
    let mut request = request.clone();
    request.name = normalize_name(request.name.as_bytes())?;
    request.symbol = normalize_symbol(request.symbol.as_bytes())?;
    validate_request(&request)?;
    let name = request.name.as_bytes();
    let symbol = request.symbol.as_bytes();
    let name_tail = abi_bytes_tail(name);
    let symbol_tail = abi_bytes_tail(symbol);
    let name_offset = HEAD_WORDS * 32;
    let symbol_offset = name_offset + name_tail.len();
    let mut out = Vec::with_capacity(4 + symbol_offset + symbol_tail.len());
    out.extend_from_slice(&create_chain_selector());
    out.extend_from_slice(request.parent_domain_id.0.as_slice());
    put_word_usize(&mut out, name_offset);
    put_word_usize(&mut out, symbol_offset);
    put_word_u64(&mut out, request.evm_chain_id);
    out.extend_from_slice(&[0_u8; 12]);
    out.extend_from_slice(request.owner.as_slice());
    put_word_u64(&mut out, request.gas_limit);
    put_word_u128(&mut out, request.initial_base_fee);
    out.extend_from_slice(&request.initial_supply.to_be_bytes::<32>());
    put_word_u32(&mut out, request.protocol_revision);
    out.extend_from_slice(&name_tail);
    out.extend_from_slice(&symbol_tail);
    Ok(out.into())
}

/// Decodes and normalizes exact v1 `createChain` calldata.
///
/// # Errors
///
/// Returns [`ChainRegistryError`] for malformed offsets, padding, integers, UTF-8, or metadata.
pub fn decode_create_chain_call(input: &[u8]) -> Result<CreateChainRequest, ChainRegistryError> {
    const HEAD_BYTES: usize = 9 * 32;
    if input.len() < 4 + HEAD_BYTES || input[..4] != create_chain_selector() {
        return Err(ChainRegistryError::InvalidCalldata);
    }
    let args = &input[4..];
    let word = |index: usize| -> Result<&[u8], ChainRegistryError> {
        args.get(index * 32..(index + 1) * 32)
            .ok_or(ChainRegistryError::InvalidCalldata)
    };
    let parent_domain_id = DomainId(B256::from_slice(word(0)?));
    let name_offset = decode_usize_word(word(1)?)?;
    let symbol_offset = decode_usize_word(word(2)?)?;
    if name_offset != HEAD_BYTES || symbol_offset < name_offset + 32 {
        return Err(ChainRegistryError::InvalidCalldata);
    }
    let (name_bytes, name_end) = decode_dynamic_bytes(args, name_offset, 64)?;
    if symbol_offset != name_end {
        return Err(ChainRegistryError::InvalidCalldata);
    }
    let (symbol_bytes, symbol_end) = decode_dynamic_bytes(args, symbol_offset, 16)?;
    if symbol_end != args.len() {
        return Err(ChainRegistryError::InvalidCalldata);
    }
    let evm_chain_id = decode_u64_word(word(3)?)?;
    let owner_word = word(4)?;
    if owner_word[..12] != [0_u8; 12] {
        return Err(ChainRegistryError::InvalidCalldata);
    }
    let owner = Address::from_slice(&owner_word[12..]);
    let gas_limit = decode_u64_word(word(5)?)?;
    let initial_base_fee = decode_u128_word(word(6)?)?;
    let initial_supply = U256::from_be_slice(word(7)?);
    let protocol_revision = decode_u32_word(word(8)?)?;
    let name = normalize_name(name_bytes)?;
    let symbol = normalize_symbol(symbol_bytes)?;
    let request = CreateChainRequest {
        parent_domain_id,
        name,
        symbol,
        evm_chain_id,
        owner,
        gas_limit,
        initial_base_fee,
        initial_supply,
        protocol_revision,
    };
    validate_request(&request)?;
    Ok(request)
}

/// Derives the immutable genesis and registry descriptor for a valid request.
///
/// # Errors
///
/// Returns [`ChainRegistryError`] for invalid fields, an insufficient deposit, height overflow,
/// or canonical encoding failure.
pub fn prepare_chain_creation(
    mut request: CreateChainRequest,
    network_id: NetworkId,
    create_tx_hash: B256,
    joint_domain_block_hash: B256,
    initial_state_root: B256,
    creation_deposit: U256,
    creation_height: u64,
) -> Result<PreparedChainCreation, ChainRegistryError> {
    request.name = normalize_name(request.name.as_bytes())?;
    request.symbol = normalize_symbol(request.symbol.as_bytes())?;
    validate_request(&request)?;
    if creation_deposit < MIN_CREATION_DEPOSIT {
        return Err(ChainRegistryError::InsufficientDeposit {
            minimum: MIN_CREATION_DEPOSIT,
            actual: creation_deposit,
        });
    }
    let domain_id = derive_domain_id(network_id, request.parent_domain_id, create_tx_hash);
    let genesis = DomainGenesis {
        domain_id,
        parent_domain_id: request.parent_domain_id,
        joint_domain_block_hash,
        create_tx_hash,
        name: request.name.clone(),
        symbol: request.symbol.clone(),
        evm_chain_id: request.evm_chain_id,
        owner: request.owner,
        protocol_revision: request.protocol_revision,
        gas_limit: request.gas_limit,
        initial_base_fee: request.initial_base_fee,
        initial_supply: request.initial_supply,
        initial_state_root,
    };
    let origin_hash = domain_genesis_hash(&genesis)
        .map_err(|error| ChainRegistryError::Encoding(error.to_string()))?;
    let descriptor = DomainDescriptor {
        domain_id,
        parent_domain_id: request.parent_domain_id,
        joint_domain_block_hash,
        create_tx_hash,
        origin_hash,
        name: request.name.clone(),
        symbol: request.symbol.clone(),
        evm_chain_id: request.evm_chain_id,
        owner: request.owner,
        protocol_revision: request.protocol_revision,
        gas_limit: request.gas_limit,
        initial_base_fee: request.initial_base_fee,
        initial_supply: request.initial_supply,
        creation_deposit,
        status: DomainStatus::Active,
    };
    let descriptor_hash = keccak256(
        encode_domain_descriptor(&descriptor)
            .map_err(|error| ChainRegistryError::Encoding(error.to_string()))?,
    );
    let deposit_unlock_height = creation_height
        .checked_add(CREATION_DEPOSIT_LOCK_BLOCKS)
        .ok_or(ChainRegistryError::HeightOverflow)?;
    Ok(PreparedChainCreation {
        request,
        descriptor,
        descriptor_hash,
        creation_height,
        deposit_unlock_height,
    })
}

/// Storage slots that make one descriptor, chain ID, deposit, and owner consensus-visible.
#[must_use]
pub fn creation_storage_writes(prepared: &PreparedChainCreation) -> [(U256, U256); 7] {
    let domain_id = prepared.descriptor.domain_id;
    [
        (
            descriptor_slot(domain_id),
            U256::from_be_slice(prepared.descriptor_hash.as_slice()),
        ),
        (
            chain_id_slot(prepared.descriptor.evm_chain_id),
            U256::from_be_slice(domain_id.0.as_slice()),
        ),
        (
            creation_height_slot(domain_id),
            U256::from(prepared.creation_height),
        ),
        (
            deposit_slot(domain_id),
            prepared.descriptor.creation_deposit,
        ),
        (
            deposit_unlock_slot(domain_id),
            U256::from(prepared.deposit_unlock_height),
        ),
        (
            owner_slot(domain_id),
            U256::from_be_slice(prepared.descriptor.owner.as_slice()),
        ),
        (
            deposit_status_slot(domain_id),
            U256::from(CreationDepositStatus::Locked as u8),
        ),
    ]
}

/// Initial root-domain registry storage binding the root ID and root EVM chain ID.
#[must_use]
pub fn root_registry_genesis_storage(
    root_domain_id: DomainId,
    root_chain_id: u64,
) -> BTreeMap<U256, U256> {
    BTreeMap::from([
        (
            descriptor_slot(root_domain_id),
            U256::from_be_slice(
                keccak256([ROOT_DOMAIN_MARKER_TAG, root_domain_id.0.as_slice()].concat())
                    .as_slice(),
            ),
        ),
        (
            chain_id_slot(root_chain_id),
            U256::from_be_slice(root_domain_id.0.as_slice()),
        ),
    ])
}

/// Registry slot containing a domain descriptor hash, or the root-domain marker.
#[must_use]
pub fn descriptor_slot(domain_id: DomainId) -> U256 {
    tagged_slot(REGISTRY_DESCRIPTOR_TAG, domain_id.0.as_slice())
}

/// Registry slot mapping a globally unique EVM chain ID to its domain ID.
#[must_use]
pub fn chain_id_slot(chain_id: u64) -> U256 {
    tagged_slot(REGISTRY_CHAIN_ID_TAG, &chain_id.to_be_bytes())
}

fn creation_height_slot(domain_id: DomainId) -> U256 {
    tagged_slot(REGISTRY_CREATION_HEIGHT_TAG, domain_id.0.as_slice())
}

/// Registry slot holding the outstanding creation-deposit amount.
#[must_use]
pub fn deposit_slot(domain_id: DomainId) -> U256 {
    tagged_slot(REGISTRY_DEPOSIT_TAG, domain_id.0.as_slice())
}

/// Registry slot holding the first refundable consensus height.
#[must_use]
pub fn deposit_unlock_slot(domain_id: DomainId) -> U256 {
    tagged_slot(REGISTRY_DEPOSIT_UNLOCK_TAG, domain_id.0.as_slice())
}

/// Registry slot holding the domain owner authorized to reclaim the deposit.
#[must_use]
pub fn owner_slot(domain_id: DomainId) -> U256 {
    tagged_slot(REGISTRY_OWNER_TAG, domain_id.0.as_slice())
}

/// Registry slot holding [`CreationDepositStatus`] for one domain.
#[must_use]
pub fn deposit_status_slot(domain_id: DomainId) -> U256 {
    tagged_slot(REGISTRY_DEPOSIT_STATUS_TAG, domain_id.0.as_slice())
}

fn encode_domain_id_call(selector: [u8; 4], domain_id: DomainId) -> Bytes {
    let mut output = Vec::with_capacity(36);
    output.extend_from_slice(&selector);
    output.extend_from_slice(domain_id.0.as_slice());
    output.into()
}

fn decode_domain_id_call(input: &[u8], selector: [u8; 4]) -> Option<DomainId> {
    (input.len() == 36 && input[..4] == selector).then(|| DomainId(B256::from_slice(&input[4..])))
}

fn selector(signature: &[u8]) -> [u8; 4] {
    let hash = keccak256(signature);
    [hash[0], hash[1], hash[2], hash[3]]
}

fn validate_request(request: &CreateChainRequest) -> Result<(), ChainRegistryError> {
    if request.parent_domain_id.0 == B256::ZERO {
        return Err(ChainRegistryError::InvalidParameter("zero parent domain"));
    }
    if request.evm_chain_id == 0 {
        return Err(ChainRegistryError::InvalidParameter("zero EVM chain ID"));
    }
    if request.owner == Address::ZERO {
        return Err(ChainRegistryError::InvalidParameter("zero owner"));
    }
    if request.gas_limit == 0 || request.gas_limit > 30_000_000 {
        return Err(ChainRegistryError::InvalidParameter("domain gas limit"));
    }
    if request.initial_base_fee > u128::from(u64::MAX) {
        return Err(ChainRegistryError::InvalidParameter("initial base fee"));
    }
    if request.protocol_revision != 1 {
        return Err(ChainRegistryError::InvalidParameter("protocol revision"));
    }
    Ok(())
}

fn normalize_name(bytes: &[u8]) -> Result<String, ChainRegistryError> {
    let value = std::str::from_utf8(bytes).map_err(|_| ChainRegistryError::InvalidMetadata)?;
    let normalized = value.split_ascii_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty()
        || normalized.len() > 64
        || !normalized
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b' ' | b'-' | b'_' | b'.'))
    {
        return Err(ChainRegistryError::InvalidMetadata);
    }
    Ok(normalized)
}

fn normalize_symbol(bytes: &[u8]) -> Result<String, ChainRegistryError> {
    let value = std::str::from_utf8(bytes).map_err(|_| ChainRegistryError::InvalidMetadata)?;
    let normalized = value.trim().to_ascii_uppercase();
    if normalized.is_empty()
        || normalized.len() > 16
        || !normalized.bytes().all(|byte| byte.is_ascii_alphanumeric())
    {
        return Err(ChainRegistryError::InvalidMetadata);
    }
    Ok(normalized)
}

fn tagged_slot(tag: &[u8], key: &[u8]) -> U256 {
    U256::from_be_slice(keccak256([tag, key].concat()).as_slice())
}

fn abi_bytes_tail(value: &[u8]) -> Vec<u8> {
    let padded = value.len().div_ceil(32) * 32;
    let mut out = Vec::with_capacity(32 + padded);
    put_word_usize(&mut out, value.len());
    out.extend_from_slice(value);
    out.resize(32 + padded, 0);
    out
}

fn decode_dynamic_bytes(
    args: &[u8],
    offset: usize,
    limit: usize,
) -> Result<(&[u8], usize), ChainRegistryError> {
    if !offset.is_multiple_of(32) {
        return Err(ChainRegistryError::InvalidCalldata);
    }
    let length_word = args
        .get(offset..offset + 32)
        .ok_or(ChainRegistryError::InvalidCalldata)?;
    let length = decode_usize_word(length_word)?;
    if length > limit {
        return Err(ChainRegistryError::InvalidMetadata);
    }
    let start = offset
        .checked_add(32)
        .ok_or(ChainRegistryError::InvalidCalldata)?;
    let padded = length
        .checked_add(31)
        .ok_or(ChainRegistryError::InvalidCalldata)?
        / 32
        * 32;
    let end = start
        .checked_add(padded)
        .ok_or(ChainRegistryError::InvalidCalldata)?;
    let data = args
        .get(start..start + length)
        .ok_or(ChainRegistryError::InvalidCalldata)?;
    let padding = args
        .get(start + length..end)
        .ok_or(ChainRegistryError::InvalidCalldata)?;
    if padding.iter().any(|byte| *byte != 0) {
        return Err(ChainRegistryError::InvalidCalldata);
    }
    Ok((data, end))
}

fn decode_usize_word(word: &[u8]) -> Result<usize, ChainRegistryError> {
    if word.len() != 32 || word[..24] != [0_u8; 24] {
        return Err(ChainRegistryError::InvalidCalldata);
    }
    let bytes: [u8; 8] = word[24..]
        .try_into()
        .map_err(|_| ChainRegistryError::InvalidCalldata)?;
    usize::try_from(u64::from_be_bytes(bytes)).map_err(|_| ChainRegistryError::InvalidCalldata)
}

fn decode_u64_word(word: &[u8]) -> Result<u64, ChainRegistryError> {
    if word.len() != 32 || word[..24] != [0_u8; 24] {
        return Err(ChainRegistryError::InvalidCalldata);
    }
    Ok(u64::from_be_bytes(
        word[24..]
            .try_into()
            .map_err(|_| ChainRegistryError::InvalidCalldata)?,
    ))
}

fn decode_u128_word(word: &[u8]) -> Result<u128, ChainRegistryError> {
    if word.len() != 32 || word[..16] != [0_u8; 16] {
        return Err(ChainRegistryError::InvalidCalldata);
    }
    Ok(u128::from_be_bytes(
        word[16..]
            .try_into()
            .map_err(|_| ChainRegistryError::InvalidCalldata)?,
    ))
}

fn decode_u32_word(word: &[u8]) -> Result<u32, ChainRegistryError> {
    if word.len() != 32 || word[..28] != [0_u8; 28] {
        return Err(ChainRegistryError::InvalidCalldata);
    }
    Ok(u32::from_be_bytes(
        word[28..]
            .try_into()
            .map_err(|_| ChainRegistryError::InvalidCalldata)?,
    ))
}

fn put_word_usize(out: &mut Vec<u8>, value: usize) {
    put_word_u64(
        out,
        u64::try_from(value).expect("bounded ABI length fits u64"),
    );
}

fn put_word_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&[0_u8; 24]);
    out.extend_from_slice(&value.to_be_bytes());
}

fn put_word_u128(out: &mut Vec<u8>, value: u128) {
    out.extend_from_slice(&[0_u8; 16]);
    out.extend_from_slice(&value.to_be_bytes());
}

fn put_word_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&[0_u8; 28]);
    out.extend_from_slice(&value.to_be_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> CreateChainRequest {
        CreateChainRequest {
            parent_domain_id: DomainId(B256::repeat_byte(0x11)),
            name: " Demo   Child ".to_owned(),
            symbol: "dmo".to_owned(),
            evm_chain_id: 2_049,
            owner: address!("00000000000000000000000000000000000000aa"),
            gas_limit: 20_000_000,
            initial_base_fee: 10,
            initial_supply: U256::from(1_000_000_u64),
            protocol_revision: 1,
        }
    }

    #[test]
    fn protocol_info_registry_and_abi_are_stable() {
        let contract = protocol_info_contract();
        assert_eq!(contract.address, PROTOCOL_INFO_ADDRESS);
        assert_eq!(contract.implementation_version, 1);
        assert_eq!(protocol_info_selector(), [0x93, 0x42, 0x0c, 0xf4]);

        let output = encode_protocol_info(1, 12, 2_048);
        assert_eq!(output.len(), 128);
        assert_eq!(&output[28..32], &1_u32.to_be_bytes());
        assert_eq!(output[63], 12);
        assert_eq!(&output[92..96], &1_u32.to_be_bytes());
        assert_eq!(&output[120..128], &2_048_u64.to_be_bytes());
    }

    #[test]
    fn create_chain_abi_is_canonical_and_normalizes_metadata() {
        let encoded = encode_create_chain_call(&request()).unwrap();
        let decoded = decode_create_chain_call(&encoded).unwrap();
        assert_eq!(decoded.name, "Demo Child");
        assert_eq!(decoded.symbol, "DMO");
        assert_eq!(decoded.evm_chain_id, 2_049);
        assert_eq!(create_chain_selector(), [0xf7, 0x34, 0x54, 0x86]);

        let mut trailing = encoded.to_vec();
        trailing.push(0);
        assert_eq!(
            decode_create_chain_call(&trailing),
            Err(ChainRegistryError::InvalidCalldata)
        );
    }

    #[test]
    fn deposit_lifecycle_abi_is_exact() {
        let domain_id = DomainId(B256::repeat_byte(0x77));
        let refund = encode_refund_deposit_call(domain_id);
        let burn = encode_burn_deposit_call(domain_id);
        assert_eq!(refund.len(), 36);
        assert_eq!(burn.len(), 36);
        assert_ne!(refund_deposit_selector(), burn_deposit_selector());
        assert_eq!(refund_deposit_selector(), [0x2f, 0x34, 0x13, 0x57]);
        assert_eq!(burn_deposit_selector(), [0xd6, 0x16, 0x07, 0xfb]);
        assert_eq!(decode_refund_deposit_call(&refund), Some(domain_id));
        assert_eq!(decode_burn_deposit_call(&burn), Some(domain_id));
        assert_eq!(decode_burn_deposit_call(&refund), None);

        let mut trailing = refund.to_vec();
        trailing.push(0);
        assert_eq!(decode_refund_deposit_call(&trailing), None);
    }

    #[test]
    fn deterministic_creation_binds_joint_genesis_and_deposit() {
        let request =
            decode_create_chain_call(&encode_create_chain_call(&request()).unwrap()).unwrap();
        let prepared = prepare_chain_creation(
            request,
            NetworkId(B256::repeat_byte(0x22)),
            B256::repeat_byte(0x33),
            B256::repeat_byte(0x44),
            B256::repeat_byte(0x55),
            MIN_CREATION_DEPOSIT,
            7,
        )
        .unwrap();
        assert_eq!(
            prepared.descriptor.joint_domain_block_hash,
            B256::repeat_byte(0x44)
        );
        assert_eq!(prepared.descriptor.creation_deposit, MIN_CREATION_DEPOSIT);
        assert_eq!(prepared.deposit_unlock_height, 107);
        assert_eq!(creation_storage_writes(&prepared).len(), 7);
    }

    #[test]
    fn creation_is_idempotent_and_duplicate_display_names_do_not_alias_ids() {
        let request =
            decode_create_chain_call(&encode_create_chain_call(&request()).unwrap()).unwrap();
        let prepare = |create_tx_hash| {
            prepare_chain_creation(
                request.clone(),
                NetworkId(B256::repeat_byte(0x22)),
                create_tx_hash,
                B256::repeat_byte(0x44),
                B256::repeat_byte(0x55),
                MIN_CREATION_DEPOSIT,
                7,
            )
            .unwrap()
        };

        let first = prepare(B256::repeat_byte(0x33));
        let replay = prepare(B256::repeat_byte(0x33));
        let same_name_other_transaction = prepare(B256::repeat_byte(0x34));

        assert_eq!(first, replay);
        assert_eq!(
            first.descriptor.name,
            same_name_other_transaction.descriptor.name
        );
        assert_ne!(
            first.descriptor.domain_id,
            same_name_other_transaction.descriptor.domain_id
        );
        assert_ne!(
            first.descriptor_hash,
            same_name_other_transaction.descriptor_hash
        );
    }

    #[test]
    fn creation_rejects_invalid_parameters_metadata_deposit_and_height() {
        let valid = request();
        let invalid_requests = [
            CreateChainRequest {
                parent_domain_id: DomainId(B256::ZERO),
                ..valid.clone()
            },
            CreateChainRequest {
                evm_chain_id: 0,
                ..valid.clone()
            },
            CreateChainRequest {
                owner: Address::ZERO,
                ..valid.clone()
            },
            CreateChainRequest {
                gas_limit: 0,
                ..valid.clone()
            },
            CreateChainRequest {
                gas_limit: 30_000_001,
                ..valid.clone()
            },
            CreateChainRequest {
                initial_base_fee: u128::from(u64::MAX) + 1,
                ..valid.clone()
            },
            CreateChainRequest {
                protocol_revision: 2,
                ..valid.clone()
            },
        ];
        for invalid in invalid_requests {
            assert!(matches!(
                encode_create_chain_call(&invalid),
                Err(ChainRegistryError::InvalidParameter(_))
            ));
        }
        for (name, symbol) in [("", "DMO"), ("bad/name", "DMO"), ("Demo", "D-MO")] {
            assert_eq!(
                encode_create_chain_call(&CreateChainRequest {
                    name: name.to_owned(),
                    symbol: symbol.to_owned(),
                    ..valid.clone()
                }),
                Err(ChainRegistryError::InvalidMetadata)
            );
        }

        let canonical =
            decode_create_chain_call(&encode_create_chain_call(&valid).unwrap()).unwrap();
        assert_eq!(
            prepare_chain_creation(
                canonical.clone(),
                NetworkId(B256::repeat_byte(0x22)),
                B256::repeat_byte(0x33),
                B256::repeat_byte(0x44),
                B256::repeat_byte(0x55),
                MIN_CREATION_DEPOSIT - U256::from(1),
                7,
            ),
            Err(ChainRegistryError::InsufficientDeposit {
                minimum: MIN_CREATION_DEPOSIT,
                actual: MIN_CREATION_DEPOSIT - U256::from(1),
            })
        );
        assert_eq!(
            prepare_chain_creation(
                canonical,
                NetworkId(B256::repeat_byte(0x22)),
                B256::repeat_byte(0x33),
                B256::repeat_byte(0x44),
                B256::repeat_byte(0x55),
                MIN_CREATION_DEPOSIT,
                u64::MAX,
            ),
            Err(ChainRegistryError::HeightOverflow)
        );
    }
}
