use alloy_primitives::{Address, B256, Bloom, Bytes, U256};
use alloy_rlp::{Decodable, Encodable, Header};
use arbor_primitives::{AccessListItem, Eip1559Transaction, Log, Receipt};

use crate::{
    CodecError, MAX_ACCESS_LIST_ADDRESSES, MAX_ACCESS_LIST_STORAGE_KEYS, MAX_CALLDATA_BYTES,
    MAX_CANONICAL_OBJECT_BYTES, MAX_INITCODE_BYTES, MAX_TRANSACTION_ENVELOPE_BYTES,
};

/// EIP-2718 transaction type assigned to EIP-1559.
pub const EIP_1559_TX_TYPE: u8 = 0x02;

fn validate(transaction: &Eip1559Transaction, require_signature: bool) -> Result<(), CodecError> {
    if transaction.chain_id == 0 {
        return Err(CodecError::InvalidValue("EIP-1559 chain id"));
    }
    if transaction.max_priority_fee_per_gas > transaction.max_fee_per_gas {
        return Err(CodecError::InvalidValue("EIP-1559 priority fee"));
    }
    let input_limit = if transaction.to.is_none() {
        MAX_INITCODE_BYTES
    } else {
        MAX_CALLDATA_BYTES
    };
    if transaction.input.len() > input_limit {
        return Err(CodecError::LimitExceeded {
            field: "EIP-1559 input",
            limit: input_limit,
            actual: transaction.input.len(),
        });
    }
    if transaction.access_list.len() > MAX_ACCESS_LIST_ADDRESSES {
        return Err(CodecError::LimitExceeded {
            field: "EIP-1559 access list",
            limit: MAX_ACCESS_LIST_ADDRESSES,
            actual: transaction.access_list.len(),
        });
    }
    for item in &transaction.access_list {
        if item.storage_keys.len() > MAX_ACCESS_LIST_STORAGE_KEYS {
            return Err(CodecError::LimitExceeded {
                field: "EIP-1559 storage keys",
                limit: MAX_ACCESS_LIST_STORAGE_KEYS,
                actual: item.storage_keys.len(),
            });
        }
    }
    if require_signature && (transaction.r.is_zero() || transaction.s.is_zero()) {
        return Err(CodecError::InvalidValue("EIP-1559 signature scalar"));
    }
    Ok(())
}

fn encode_access_list(access_list: &[AccessListItem], out: &mut Vec<u8>) {
    let mut payload = Vec::new();
    for item in access_list {
        let mut item_payload = Vec::new();
        item.address.encode(&mut item_payload);

        let mut keys_payload = Vec::new();
        for key in &item.storage_keys {
            key.encode(&mut keys_payload);
        }
        Header {
            list: true,
            payload_length: keys_payload.len(),
        }
        .encode(&mut item_payload);
        item_payload.extend_from_slice(&keys_payload);

        Header {
            list: true,
            payload_length: item_payload.len(),
        }
        .encode(&mut payload);
        payload.extend_from_slice(&item_payload);
    }
    Header {
        list: true,
        payload_length: payload.len(),
    }
    .encode(out);
    out.extend_from_slice(&payload);
}

fn encode_fields(transaction: &Eip1559Transaction, signed: bool) -> Vec<u8> {
    let mut payload = Vec::new();
    transaction.chain_id.encode(&mut payload);
    transaction.nonce.encode(&mut payload);
    transaction.max_priority_fee_per_gas.encode(&mut payload);
    transaction.max_fee_per_gas.encode(&mut payload);
    transaction.gas_limit.encode(&mut payload);
    match transaction.to {
        Some(address) => address.encode(&mut payload),
        None => (&[] as &[u8]).encode(&mut payload),
    }
    transaction.value.encode(&mut payload);
    transaction.input.as_ref().encode(&mut payload);
    encode_access_list(&transaction.access_list, &mut payload);
    if signed {
        u8::from(transaction.y_parity).encode(&mut payload);
        transaction.r.encode(&mut payload);
        transaction.s.encode(&mut payload);
    }
    payload
}

fn encode_envelope(transaction: &Eip1559Transaction, signed: bool) -> Result<Vec<u8>, CodecError> {
    validate(transaction, signed)?;
    let payload = encode_fields(transaction, signed);
    let mut encoded = Vec::with_capacity(payload.len() + 8);
    encoded.push(EIP_1559_TX_TYPE);
    Header {
        list: true,
        payload_length: payload.len(),
    }
    .encode(&mut encoded);
    encoded.extend_from_slice(&payload);
    if encoded.len() > MAX_TRANSACTION_ENVELOPE_BYTES {
        return Err(CodecError::InputTooLarge {
            kind: "EIP-1559 envelope",
            limit: MAX_TRANSACTION_ENVELOPE_BYTES,
            actual: encoded.len(),
        });
    }
    Ok(encoded)
}

/// Encodes the exact EIP-1559 signing payload: `0x02 || rlp(unsigned_fields)`.
///
/// # Errors
///
/// Returns [`CodecError`] when fields violate Arbor's EIP-1559 resource limits.
pub fn encode_eip1559_signing_payload(
    transaction: &Eip1559Transaction,
) -> Result<Vec<u8>, CodecError> {
    encode_envelope(transaction, false)
}

/// Encodes a signed EIP-1559 type-2 envelope.
///
/// # Errors
///
/// Returns [`CodecError`] for invalid signature scalars, fees, or resource limits.
pub fn encode_eip1559(transaction: &Eip1559Transaction) -> Result<Vec<u8>, CodecError> {
    encode_envelope(transaction, true)
}

fn list_payload<'a>(input: &mut &'a [u8], field: &'static str) -> Result<&'a [u8], CodecError> {
    let header = Header::decode(input)?;
    if !header.list {
        return Err(CodecError::InvalidValue(field));
    }
    if input.len() < header.payload_length {
        return Err(CodecError::UnexpectedEof);
    }
    let (payload, rest) = input.split_at(header.payload_length);
    *input = rest;
    Ok(payload)
}

fn decode_access_list(input: &mut &[u8]) -> Result<Vec<AccessListItem>, CodecError> {
    let mut list = list_payload(input, "EIP-1559 access list")?;
    let mut access_list = Vec::new();
    while !list.is_empty() {
        if access_list.len() == MAX_ACCESS_LIST_ADDRESSES {
            return Err(CodecError::LimitExceeded {
                field: "EIP-1559 access list",
                limit: MAX_ACCESS_LIST_ADDRESSES,
                actual: access_list.len() + 1,
            });
        }
        let mut item = list_payload(&mut list, "EIP-1559 access-list item")?;
        let address = Address::decode(&mut item)?;
        let mut keys = list_payload(&mut item, "EIP-1559 storage-key list")?;
        if !item.is_empty() {
            return Err(CodecError::TrailingBytes);
        }
        let mut storage_keys = Vec::new();
        while !keys.is_empty() {
            if storage_keys.len() == MAX_ACCESS_LIST_STORAGE_KEYS {
                return Err(CodecError::LimitExceeded {
                    field: "EIP-1559 storage keys",
                    limit: MAX_ACCESS_LIST_STORAGE_KEYS,
                    actual: storage_keys.len() + 1,
                });
            }
            storage_keys.push(B256::decode(&mut keys)?);
        }
        access_list.push(AccessListItem {
            address,
            storage_keys,
        });
    }
    Ok(access_list)
}

/// Decodes exactly one signed EIP-1559 envelope with Arbor resource limits.
///
/// # Errors
///
/// Returns [`CodecError`] for malformed RLP, non-minimal fields, invalid values, or limits.
pub fn decode_eip1559(input: &[u8]) -> Result<Eip1559Transaction, CodecError> {
    if input.len() > MAX_TRANSACTION_ENVELOPE_BYTES {
        return Err(CodecError::InputTooLarge {
            kind: "EIP-1559 envelope",
            limit: MAX_TRANSACTION_ENVELOPE_BYTES,
            actual: input.len(),
        });
    }
    let Some((&transaction_type, body)) = input.split_first() else {
        return Err(CodecError::UnexpectedEof);
    };
    if transaction_type != EIP_1559_TX_TYPE {
        return Err(CodecError::InvalidValue("EIP-2718 transaction type"));
    }
    let mut outer = body;
    let mut fields = list_payload(&mut outer, "EIP-1559 payload")?;
    if !outer.is_empty() {
        return Err(CodecError::TrailingBytes);
    }

    let chain_id = u64::decode(&mut fields)?;
    let nonce = u64::decode(&mut fields)?;
    let max_priority_fee_per_gas = u128::decode(&mut fields)?;
    let max_fee_per_gas = u128::decode(&mut fields)?;
    let gas_limit = u64::decode(&mut fields)?;
    let to_bytes = Bytes::decode(&mut fields)?;
    let to = match to_bytes.len() {
        0 => None,
        20 => Some(Address::from_slice(&to_bytes)),
        _ => return Err(CodecError::InvalidValue("EIP-1559 destination")),
    };
    let value = U256::decode(&mut fields)?;
    let input = Bytes::decode(&mut fields)?;
    let access_list = decode_access_list(&mut fields)?;
    let y_parity = match u8::decode(&mut fields)? {
        0 => false,
        1 => true,
        _ => return Err(CodecError::InvalidValue("EIP-1559 y parity")),
    };
    let r = U256::decode(&mut fields)?;
    let s = U256::decode(&mut fields)?;
    if !fields.is_empty() {
        return Err(CodecError::TrailingBytes);
    }
    let transaction = Eip1559Transaction {
        chain_id,
        nonce,
        max_priority_fee_per_gas,
        max_fee_per_gas,
        gas_limit,
        to,
        value,
        input,
        access_list,
        y_parity,
        r,
        s,
    };
    validate(&transaction, true)?;
    Ok(transaction)
}

fn encode_logs(logs: &[Log], out: &mut Vec<u8>) -> Result<(), CodecError> {
    if logs.len() > 1_024 {
        return Err(CodecError::LimitExceeded {
            field: "receipt logs",
            limit: 1_024,
            actual: logs.len(),
        });
    }
    let mut encoded_logs = Vec::new();
    for log in logs {
        if log.topics.len() > 4 {
            return Err(CodecError::LimitExceeded {
                field: "log topics",
                limit: 4,
                actual: log.topics.len(),
            });
        }
        if log.data.len() > MAX_CALLDATA_BYTES * 2 {
            return Err(CodecError::LimitExceeded {
                field: "log data",
                limit: MAX_CALLDATA_BYTES * 2,
                actual: log.data.len(),
            });
        }
        let mut encoded_log = Vec::new();
        log.address.encode(&mut encoded_log);
        let mut topics_payload = Vec::new();
        for topic in &log.topics {
            topic.encode(&mut topics_payload);
        }
        Header {
            list: true,
            payload_length: topics_payload.len(),
        }
        .encode(&mut encoded_log);
        encoded_log.extend_from_slice(&topics_payload);
        log.data.as_ref().encode(&mut encoded_log);
        Header {
            list: true,
            payload_length: encoded_log.len(),
        }
        .encode(&mut encoded_logs);
        encoded_logs.extend_from_slice(&encoded_log);
    }
    Header {
        list: true,
        payload_length: encoded_logs.len(),
    }
    .encode(out);
    out.extend_from_slice(&encoded_logs);
    Ok(())
}

/// Encodes `0x02 || rlp([status, cumulative_gas_used, logs_bloom, logs])`.
///
/// # Errors
///
/// Returns [`CodecError`] for excessive logs, topics, data, or total bytes.
pub fn encode_eip1559_receipt(receipt: &Receipt) -> Result<Vec<u8>, CodecError> {
    let mut payload = Vec::new();
    u8::from(receipt.status).encode(&mut payload);
    receipt.cumulative_gas_used.encode(&mut payload);
    receipt.logs_bloom.as_slice().encode(&mut payload);
    encode_logs(&receipt.logs, &mut payload)?;

    let mut encoded = Vec::with_capacity(payload.len() + 8);
    encoded.push(EIP_1559_TX_TYPE);
    Header {
        list: true,
        payload_length: payload.len(),
    }
    .encode(&mut encoded);
    encoded.extend_from_slice(&payload);
    if encoded.len() > MAX_CANONICAL_OBJECT_BYTES {
        return Err(CodecError::InputTooLarge {
            kind: "EIP-1559 receipt",
            limit: MAX_CANONICAL_OBJECT_BYTES,
            actual: encoded.len(),
        });
    }
    Ok(encoded)
}

fn decode_logs(input: &mut &[u8]) -> Result<Vec<Log>, CodecError> {
    let mut logs_payload = list_payload(input, "receipt logs")?;
    let mut logs = Vec::new();
    while !logs_payload.is_empty() {
        if logs.len() == 1_024 {
            return Err(CodecError::LimitExceeded {
                field: "receipt logs",
                limit: 1_024,
                actual: logs.len() + 1,
            });
        }
        let mut log = list_payload(&mut logs_payload, "receipt log")?;
        let address = Address::decode(&mut log)?;
        let mut topics_payload = list_payload(&mut log, "log topics")?;
        let mut topics = Vec::new();
        while !topics_payload.is_empty() {
            if topics.len() == 4 {
                return Err(CodecError::LimitExceeded {
                    field: "log topics",
                    limit: 4,
                    actual: topics.len() + 1,
                });
            }
            topics.push(B256::decode(&mut topics_payload)?);
        }
        let data = Bytes::decode(&mut log)?;
        if data.len() > MAX_CALLDATA_BYTES * 2 {
            return Err(CodecError::LimitExceeded {
                field: "log data",
                limit: MAX_CALLDATA_BYTES * 2,
                actual: data.len(),
            });
        }
        if !log.is_empty() {
            return Err(CodecError::TrailingBytes);
        }
        logs.push(Log {
            address,
            topics,
            data,
        });
    }
    Ok(logs)
}

/// Decodes exactly one EIP-1559 typed receipt with bounded logs.
///
/// # Errors
///
/// Returns [`CodecError`] for malformed RLP, invalid fields, limits, or trailing bytes.
pub fn decode_eip1559_receipt(input: &[u8]) -> Result<Receipt, CodecError> {
    if input.len() > MAX_CANONICAL_OBJECT_BYTES {
        return Err(CodecError::InputTooLarge {
            kind: "EIP-1559 receipt",
            limit: MAX_CANONICAL_OBJECT_BYTES,
            actual: input.len(),
        });
    }
    let Some((&receipt_type, body)) = input.split_first() else {
        return Err(CodecError::UnexpectedEof);
    };
    if receipt_type != EIP_1559_TX_TYPE {
        return Err(CodecError::InvalidValue("EIP-2718 receipt type"));
    }
    let mut outer = body;
    let mut fields = list_payload(&mut outer, "EIP-1559 receipt payload")?;
    if !outer.is_empty() {
        return Err(CodecError::TrailingBytes);
    }
    let status = match u8::decode(&mut fields)? {
        0 => false,
        1 => true,
        _ => return Err(CodecError::InvalidValue("receipt status")),
    };
    let cumulative_gas_used = u64::decode(&mut fields)?;
    let bloom_bytes = Bytes::decode(&mut fields)?;
    if bloom_bytes.len() != 256 {
        return Err(CodecError::InvalidValue("receipt logs bloom"));
    }
    let logs_bloom = Bloom::from_slice(&bloom_bytes);
    let logs = decode_logs(&mut fields)?;
    if !fields.is_empty() {
        return Err(CodecError::TrailingBytes);
    }
    Ok(Receipt {
        status,
        cumulative_gas_used,
        logs_bloom,
        logs,
    })
}
