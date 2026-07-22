//! Versioned native system-address registry and read-only protocol information ABI.

#![forbid(unsafe_code)]

use alloy_primitives::{Address, B256, Bytes, address, keccak256};

/// Version of the system-address registry interpreted by protocol revision one.
pub const SYSTEM_ADDRESS_REGISTRY_VERSION: u32 = 1;
/// Version of the protocol-info native implementation.
pub const PROTOCOL_INFO_IMPLEMENTATION_VERSION: u32 = 1;
/// Fixed gas charged by the protocol-info native call after ordinary call intrinsic costs.
pub const PROTOCOL_INFO_GAS: u64 = 500;
/// Reserved address of the read-only protocol-info native system contract.
pub const PROTOCOL_INFO_ADDRESS: Address = address!("0000000000000000000000000000000000000800");
/// Canonical Solidity ABI signature exposed by the native contract.
pub const PROTOCOL_INFO_ABI_SIGNATURE: &[u8] = b"protocolInfo()";

/// A versioned native entry that cannot be replaced by user bytecode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SystemContract {
    /// Reserved EVM address.
    pub address: Address,
    /// Keccak hash of the canonical ABI signature set.
    pub abi_hash: B256,
    /// Version of the native implementation.
    pub implementation_version: u32,
    /// Fixed native execution gas, excluding ordinary transaction/call gas.
    pub gas_cost: u64,
}

/// Returns the protocol-revision-one native system registry.
#[must_use]
pub fn protocol_info_contract() -> SystemContract {
    SystemContract {
        address: PROTOCOL_INFO_ADDRESS,
        abi_hash: keccak256(PROTOCOL_INFO_ABI_SIGNATURE),
        implementation_version: PROTOCOL_INFO_IMPLEMENTATION_VERSION,
        gas_cost: PROTOCOL_INFO_GAS,
    }
}

/// Returns the four-byte selector for `protocolInfo()`.
#[must_use]
pub fn protocol_info_selector() -> [u8; 4] {
    let hash = keccak256(PROTOCOL_INFO_ABI_SIGNATURE);
    [hash[0], hash[1], hash[2], hash[3]]
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
