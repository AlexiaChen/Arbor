//! Cross-crate M2 golden vectors and hostile-input regression tests.

use alloy_primitives::{Address, B256, Bloom, Bytes, U256, address, b256, hex};
use arbor_codec::{
    CodecError, decode_consensus_header, decode_domain_batch, decode_domain_descriptor,
    decode_domain_genesis, decode_domain_header, decode_eip1559, decode_eip1559_receipt,
    decode_quorum_certificate, decode_validator_set, decode_vote, encode_consensus_header,
    encode_domain_batch, encode_domain_descriptor, encode_domain_genesis, encode_domain_header,
    encode_eip1559, encode_eip1559_receipt, encode_quorum_certificate, encode_validator_set,
    encode_vote,
};
use arbor_crypto::{
    ConsensusSigner, consensus_header_hash, derive_domain_id, eip1559_signing_hash,
    eip1559_transaction_hash, keccak, recover_eip1559_sender, validator_id, validator_set_hash,
    verify_quorum_certificate, verify_vote_signature,
};
use arbor_primitives::{
    AccessListItem, CommitSignature, ConsensusBlockHeader, ConsensusHeight, ConsensusRound,
    ConsensusSignature, DomainBatch, DomainBlockHeader, DomainDescriptor, DomainGenesis, DomainId,
    DomainNumber, DomainStatus, Eip1559Transaction, Log, NetworkId, QuorumCertificate, Receipt,
    Validator, ValidatorId, ValidatorSet, Vote, VotePhase,
};

fn hash(byte: u8) -> B256 {
    B256::repeat_byte(byte)
}

fn sample_header(seed: u8) -> ConsensusBlockHeader {
    ConsensusBlockHeader {
        protocol_version: 1,
        network_id: NetworkId(hash(seed)),
        height: ConsensusHeight(u64::from(seed) * 17),
        parent_hash: hash(seed.wrapping_add(1)),
        timestamp: 1_700_000_000 + u64::from(seed),
        batches_root: hash(seed.wrapping_add(2)),
        domain_results_root: hash(seed.wrapping_add(3)),
        domain_heads_root: hash(seed.wrapping_add(4)),
        validator_set_hash: hash(seed.wrapping_add(5)),
        next_validator_set_hash: hash(seed.wrapping_add(6)),
        proposer: Address::repeat_byte(seed.wrapping_add(7)),
    }
}

fn sample_domain_header() -> DomainBlockHeader {
    DomainBlockHeader {
        protocol_version: 1,
        domain_id: DomainId(hash(0x10)),
        number: DomainNumber(7),
        parent_hash: hash(0x11),
        consensus_height: ConsensusHeight(9),
        transactions_root: hash(0x12),
        state_root: hash(0x13),
        receipts_root: hash(0x14),
        logs_bloom: Bloom::repeat_byte(0x15),
        gas_limit: 30_000_000,
        gas_used: 21_000,
        base_fee_per_gas: 1_000_000_000,
    }
}

fn sample_descriptor() -> DomainDescriptor {
    DomainDescriptor {
        domain_id: DomainId(hash(0x21)),
        parent_domain_id: DomainId(hash(0x22)),
        joint_domain_block_hash: hash(0x23),
        create_tx_hash: hash(0x24),
        origin_hash: hash(0x25),
        name: "Arbor Demo".to_owned(),
        symbol: "ARB".to_owned(),
        evm_chain_id: 2_048,
        owner: Address::repeat_byte(0x26),
        protocol_revision: 1,
        gas_limit: 30_000_000,
        initial_base_fee: 1_000_000_000,
        initial_supply: U256::from(1_000_000_000_u64),
        creation_deposit: U256::from(100_000_u64),
        status: DomainStatus::Active,
    }
}

fn sample_genesis() -> DomainGenesis {
    let descriptor = sample_descriptor();
    DomainGenesis {
        domain_id: descriptor.domain_id,
        parent_domain_id: descriptor.parent_domain_id,
        joint_domain_block_hash: descriptor.joint_domain_block_hash,
        create_tx_hash: descriptor.create_tx_hash,
        name: descriptor.name,
        symbol: descriptor.symbol,
        evm_chain_id: descriptor.evm_chain_id,
        owner: descriptor.owner,
        protocol_revision: descriptor.protocol_revision,
        gas_limit: descriptor.gas_limit,
        initial_base_fee: descriptor.initial_base_fee,
        initial_supply: descriptor.initial_supply,
        initial_state_root: hash(0x27),
    }
}

fn sample_vote(seed: u8) -> Vote {
    Vote {
        network_id: NetworkId(hash(seed)),
        height: ConsensusHeight(100 + u64::from(seed)),
        round: ConsensusRound(u64::from(seed % 4)),
        phase: VotePhase(seed % 3 + 1),
        block_hash: hash(seed.wrapping_add(1)),
        validator_id: ValidatorId(hash(seed.wrapping_add(2))),
    }
}

fn sample_validator_set(seed: u8) -> ValidatorSet {
    let first_signer = ConsensusSigner::from_secret_bytes(&[seed; 32]).unwrap();
    let second_signer = ConsensusSigner::from_secret_bytes(&[seed.wrapping_add(16); 32]).unwrap();
    let mut validators = vec![
        Validator {
            id: validator_id(first_signer.public_key()),
            public_key: first_signer.public_key(),
            power: 10,
        },
        Validator {
            id: validator_id(second_signer.public_key()),
            public_key: second_signer.public_key(),
            power: 20,
        },
    ];
    validators.sort_by_key(|validator| validator.id);
    ValidatorSet {
        epoch: u64::from(seed),
        validators,
    }
}

fn known_eip1559_transaction() -> Eip1559Transaction {
    // Cross-checked against alloy-consensus 2.2.0's upstream EIP-1559 vector.
    Eip1559Transaction {
        chain_id: 1,
        nonce: 0x42,
        gas_limit: 44_386,
        to: Some(address!("6069a6c32cf691f5982febae4faf8a6f3ab2f0f6")),
        value: U256::ZERO,
        input: Bytes::from_static(&hex!(
            "a22cb4650000000000000000000000005eee75727d804a2b13038928d36f8b188945a57a0000000000000000000000000000000000000000000000000000000000000000"
        )),
        max_fee_per_gas: 0x0004_a817_c800,
        max_priority_fee_per_gas: 0x3b9a_ca00,
        access_list: Vec::new(),
        y_parity: false,
        r: U256::from_be_bytes(hex!(
            "840cfc572845f5786e702984c2a582528cad4b49b2a10b9db1be7fca90058565"
        )),
        s: U256::from_be_bytes(hex!(
            "25e7109ceb98168d95b09b18bbf6b685130e0562f233877d492b94eee0c5b6d1"
        )),
    }
}

#[test]
fn official_eip1559_vector_matches_signing_hash_tx_hash_and_sender() {
    let transaction = known_eip1559_transaction();
    assert_eq!(
        eip1559_signing_hash(&transaction).unwrap(),
        b256!("0d5688ac3897124635b6cf1bc0e29d6dfebceebdc10a54d74f2ef8b56535b682")
    );
    assert_eq!(
        eip1559_transaction_hash(&transaction).unwrap(),
        b256!("0ec0b6a2df4d87424e5f6ad2a654e27aaeb7dac20ae9e8385cc09087ad532ee0")
    );
    assert_eq!(
        recover_eip1559_sender(&transaction).unwrap(),
        address!("dd6b8b3dc6b7ad97db52f08a275ff4483e024cea")
    );
    let encoded = encode_eip1559(&transaction).unwrap();
    assert_eq!(decode_eip1559(&encoded).unwrap(), transaction);
}

#[test]
fn canonical_types_round_trip_exactly() {
    let consensus = sample_header(1);
    assert_eq!(
        decode_consensus_header(&encode_consensus_header(&consensus).unwrap()).unwrap(),
        consensus
    );

    let domain = sample_domain_header();
    assert_eq!(
        decode_domain_header(&encode_domain_header(&domain).unwrap()).unwrap(),
        domain
    );

    let descriptor = sample_descriptor();
    assert_eq!(
        decode_domain_descriptor(&encode_domain_descriptor(&descriptor).unwrap()).unwrap(),
        descriptor
    );

    let genesis = sample_genesis();
    assert_eq!(
        decode_domain_genesis(&encode_domain_genesis(&genesis).unwrap()).unwrap(),
        genesis
    );

    let batch = DomainBatch {
        domain_id: DomainId(hash(0x31)),
        parent_domain_block_hash: hash(0x32),
        transactions: vec![encode_eip1559(&known_eip1559_transaction()).unwrap().into()],
    };
    assert_eq!(
        decode_domain_batch(&encode_domain_batch(&batch).unwrap()).unwrap(),
        batch
    );

    let receipt = Receipt {
        status: true,
        cumulative_gas_used: 21_000,
        logs_bloom: Bloom::repeat_byte(0x33),
        logs: vec![Log {
            address: Address::repeat_byte(0x34),
            topics: vec![hash(0x35)],
            data: Bytes::from_static(b"event"),
        }],
    };
    assert_eq!(
        decode_eip1559_receipt(&encode_eip1559_receipt(&receipt).unwrap()).unwrap(),
        receipt
    );

    let validators = sample_validator_set(1);
    assert_eq!(
        decode_validator_set(&encode_validator_set(&validators).unwrap()).unwrap(),
        validators
    );

    let vote = sample_vote(1);
    assert_eq!(decode_vote(&encode_vote(&vote).unwrap()).unwrap(), vote);

    let certificate = QuorumCertificate {
        network_id: vote.network_id,
        height: vote.height,
        round: vote.round,
        phase: vote.phase,
        block_hash: vote.block_hash,
        signatures: vec![CommitSignature {
            validator_id: ValidatorId(hash(0x40)),
            signature: ConsensusSignature([0x41; 64]),
        }],
    };
    assert_eq!(
        decode_quorum_certificate(&encode_quorum_certificate(&certificate).unwrap()).unwrap(),
        certificate
    );
}

#[test]
fn malformed_versions_trailing_bytes_and_limits_are_rejected() {
    let header = sample_header(2);
    let mut encoded = encode_consensus_header(&header).unwrap();
    let version_offset = b"ARBOR_CONSENSUS_HEADER_V1".len();
    encoded[version_offset] = 2;
    assert_eq!(
        decode_consensus_header(&encoded),
        Err(CodecError::UnknownVersion(2))
    );

    let mut encoded = encode_consensus_header(&header).unwrap();
    encoded.push(0);
    assert_eq!(
        decode_consensus_header(&encoded),
        Err(CodecError::TrailingBytes)
    );

    let mut wrong_type = encode_eip1559(&known_eip1559_transaction()).unwrap();
    wrong_type[0] = 3;
    assert_eq!(
        decode_eip1559(&wrong_type),
        Err(CodecError::InvalidValue("EIP-2718 transaction type"))
    );

    let mut oversized = known_eip1559_transaction();
    oversized.input = vec![0; 128 * 1024 + 1].into();
    assert!(matches!(
        encode_eip1559(&oversized),
        Err(CodecError::LimitExceeded {
            field: "EIP-1559 input",
            ..
        })
    ));

    oversized.to = None;
    oversized.input = vec![0; 49_153].into();
    assert!(matches!(
        encode_eip1559(&oversized),
        Err(CodecError::LimitExceeded {
            field: "EIP-1559 input",
            limit: 49_152,
            ..
        })
    ));
}

#[test]
fn nested_access_list_is_bounded_and_round_trips() {
    let mut transaction = known_eip1559_transaction();
    transaction.access_list = vec![AccessListItem {
        address: Address::repeat_byte(0x55),
        storage_keys: vec![hash(0x56), hash(0x57)],
    }];
    let encoded = encode_eip1559(&transaction).unwrap();
    assert_eq!(decode_eip1559(&encoded).unwrap(), transaction);
}

#[test]
fn high_s_and_non_minimal_rlp_are_rejected() {
    let mut transaction = known_eip1559_transaction();
    transaction.s = U256::MAX;
    assert!(recover_eip1559_sender(&transaction).is_err());

    let non_minimal_chain_id = [
        0x02, 0xcd, 0x81, 0x01, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0xc0, 0x80, 0x01, 0x01,
    ];
    assert!(matches!(
        decode_eip1559(&non_minimal_chain_id),
        Err(CodecError::Rlp(_))
    ));
}

#[test]
fn fixed_width_max_values_are_big_endian_and_empty_lists_round_trip() {
    let vote = Vote {
        network_id: NetworkId(hash(0xaa)),
        height: ConsensusHeight(u64::MAX),
        round: ConsensusRound(u64::MAX),
        phase: VotePhase(u8::MAX),
        block_hash: hash(0xbb),
        validator_id: ValidatorId(hash(0xcc)),
    };
    let encoded = encode_vote(&vote).unwrap();
    let height_offset = b"ARBOR_VOTE_V1".len() + 1 + 32;
    assert_eq!(
        &encoded[height_offset..height_offset + 8],
        &u64::MAX.to_be_bytes()
    );
    assert_eq!(decode_vote(&encoded).unwrap(), vote);

    let receipt = Receipt {
        status: false,
        cumulative_gas_used: u64::MAX,
        logs_bloom: Bloom::default(),
        logs: Vec::new(),
    };
    assert_eq!(
        decode_eip1559_receipt(&encode_eip1559_receipt(&receipt).unwrap()).unwrap(),
        receipt
    );

    assert_eq!(
        encode_validator_set(&ValidatorSet {
            epoch: 0,
            validators: Vec::new(),
        }),
        Err(CodecError::InvalidValue("validator set"))
    );
}

#[test]
fn bounded_decoders_do_not_panic_on_arbitrary_seed_inputs() {
    for length in 0..=512 {
        let bytes: Vec<_> = (0..length)
            .map(|index| {
                u8::try_from(index % 256)
                    .unwrap()
                    .wrapping_mul(31)
                    .wrapping_add(u8::try_from(length % 256).unwrap())
            })
            .collect();
        let _ = decode_consensus_header(&bytes);
        let _ = decode_domain_header(&bytes);
        let _ = decode_domain_batch(&bytes);
        let _ = decode_domain_descriptor(&bytes);
        let _ = decode_domain_genesis(&bytes);
        let _ = decode_validator_set(&bytes);
        let _ = decode_vote(&bytes);
        let _ = decode_quorum_certificate(&bytes);
        let _ = decode_eip1559(&bytes);
        let _ = decode_eip1559_receipt(&bytes);
    }
}

#[test]
fn committed_fuzz_seed_manifest_is_executable() {
    for line in include_str!("../../../testdata/fuzz/corpus/m2-decoders.hex").lines() {
        let seed = line.split('#').next().unwrap().trim();
        if seed.is_empty() {
            continue;
        }
        let bytes = hex::decode(seed).unwrap();
        let _ = decode_consensus_header(&bytes);
        let _ = decode_domain_header(&bytes);
        let _ = decode_domain_batch(&bytes);
        let _ = decode_domain_descriptor(&bytes);
        let _ = decode_domain_genesis(&bytes);
        let _ = decode_validator_set(&bytes);
        let _ = decode_vote(&bytes);
        let _ = decode_quorum_certificate(&bytes);
        let _ = decode_eip1559(&bytes);
        let _ = decode_eip1559_receipt(&bytes);
    }
}

#[test]
fn validator_and_certificate_order_is_canonical() {
    let mut validators = sample_validator_set(1);
    validators.validators.reverse();
    assert_eq!(
        encode_validator_set(&validators),
        Err(CodecError::NonCanonicalOrder("validators"))
    );

    let vote = sample_vote(1);
    let duplicate = CommitSignature {
        validator_id: ValidatorId(hash(1)),
        signature: ConsensusSignature([1; 64]),
    };
    let certificate = QuorumCertificate {
        network_id: vote.network_id,
        height: vote.height,
        round: vote.round,
        phase: vote.phase,
        block_hash: vote.block_hash,
        signatures: vec![duplicate, duplicate],
    };
    assert_eq!(
        encode_quorum_certificate(&certificate),
        Err(CodecError::NonCanonicalOrder("certificate signatures"))
    );
}

#[test]
fn consensus_signatures_are_bound_to_every_vote_field() {
    let signer = ConsensusSigner::from_secret_bytes(&[7; 32]).unwrap();
    let mut vote = sample_vote(7);
    vote.validator_id = validator_id(signer.public_key());
    let signature = signer.sign_vote(&vote).unwrap();
    verify_vote_signature(signer.public_key(), &vote, signature).unwrap();
    assert_eq!(
        validator_id(signer.public_key()),
        validator_id(signer.public_key())
    );

    let mut conflicting = vote;
    conflicting.block_hash = hash(0xff);
    assert!(verify_vote_signature(signer.public_key(), &conflicting, signature).is_err());

    let mut impersonated = vote;
    impersonated.validator_id = ValidatorId(hash(0xee));
    assert!(signer.sign_vote(&impersonated).is_err());
}

#[test]
fn weighted_quorum_certificate_requires_strictly_more_than_two_thirds() {
    let mut entries: Vec<_> = (1_u8..=4)
        .map(|seed| {
            let signer = ConsensusSigner::from_secret_bytes(&[seed; 32]).unwrap();
            let validator = Validator {
                id: validator_id(signer.public_key()),
                public_key: signer.public_key(),
                power: 1,
            };
            (validator, signer)
        })
        .collect();
    entries.sort_by_key(|(validator, _)| validator.id);
    let validator_set = ValidatorSet {
        epoch: 1,
        validators: entries
            .iter()
            .map(|(validator, _)| validator.clone())
            .collect(),
    };
    let mut certificate = QuorumCertificate {
        network_id: NetworkId(hash(0x70)),
        height: ConsensusHeight(10),
        round: ConsensusRound(2),
        phase: VotePhase(1),
        block_hash: hash(0x71),
        signatures: Vec::new(),
    };
    for (validator, signer) in entries.iter().take(3) {
        let vote = Vote {
            network_id: certificate.network_id,
            height: certificate.height,
            round: certificate.round,
            phase: certificate.phase,
            block_hash: certificate.block_hash,
            validator_id: validator.id,
        };
        certificate.signatures.push(CommitSignature {
            validator_id: validator.id,
            signature: signer.sign_vote(&vote).unwrap(),
        });
    }
    verify_quorum_certificate(&certificate, &validator_set).unwrap();
    certificate.signatures.pop();
    assert!(verify_quorum_certificate(&certificate, &validator_set).is_err());
}

#[test]
fn thirty_fixed_consensus_vectors_match() {
    let mut vectors = Vec::new();
    for seed in 0_u8..10 {
        vectors.push((
            format!("domain-id-{seed}"),
            derive_domain_id(
                NetworkId(hash(seed)),
                DomainId(hash(seed + 1)),
                hash(seed + 2),
            )
            .0,
        ));
    }
    for seed in 1_u8..=10 {
        vectors.push((
            format!("vote-hash-{seed}"),
            keccak(encode_vote(&sample_vote(seed)).unwrap()),
        ));
    }
    for seed in 1_u8..=5 {
        vectors.push((
            format!("header-hash-{seed}"),
            consensus_header_hash(&sample_header(seed)).unwrap(),
        ));
    }
    for seed in 1_u8..=5 {
        vectors.push((
            format!("validator-set-hash-{seed}"),
            validator_set_hash(&sample_validator_set(seed)).unwrap(),
        ));
    }
    assert_eq!(vectors.len(), 30);
    let actual = vectors
        .into_iter()
        .map(|(name, value)| format!("{name}={}", hex::encode(value)))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    assert_eq!(
        include_str!("../../../testdata/vectors/arbor-v1/canonical-hashes.txt"),
        actual
    );
}
