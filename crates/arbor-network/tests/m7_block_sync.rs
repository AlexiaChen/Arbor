//! M7 end-to-end finalized block sync over two real rust-libp2p listeners.

use std::time::Duration;

use alloy_primitives::{Address, B256, Bytes, U256, address, keccak256};
use arbor_chain::{ConsensusBlock, encode_consensus_block};
use arbor_codec::{decode_domain_header, encode_eip1559, encode_eip1559_signing_payload};
use arbor_consensus::{DevGenesis, DevelopmentCheckpoint, EngineMode, SingleValidatorEngine};
use arbor_crypto::{derive_domain_id, eip1559_transaction_hash};
use arbor_mempool::MempoolConfig;
use arbor_network::{
    BlockImport, BlockSync, DevelopmentCheckpointVerifier, DirectRequest, DirectResponse,
    FinalizedBlock, InboundRequest, NetworkEvent, NetworkService, NetworkServiceConfig,
    SnapshotBundle, SnapshotStaging, SyncStatus,
};
use arbor_primitives::{Eip1559Transaction, NetworkId};
use arbor_storage::{Database, DatabaseIdentity, RetentionPolicy};
use arbor_system::{
    CHAIN_REGISTRY_ADDRESS, CreateChainRequest, MIN_CREATION_DEPOSIT, encode_create_chain_call,
};
use k256::ecdsa::SigningKey;
use libp2p::Multiaddr;
use tempfile::TempDir;

fn identity() -> DatabaseIdentity {
    DatabaseIdentity {
        network_id: NetworkId(B256::repeat_byte(0x71)),
        genesis_hash: B256::repeat_byte(0x72),
    }
}

fn open_engine(directory: &TempDir) -> SingleValidatorEngine {
    let identity = identity();
    let database = Database::open(directory.path(), identity, RetentionPolicy::Archive).unwrap();
    SingleValidatorEngine::open(
        EngineMode::DevValidator,
        database,
        DevGenesis::local_default(identity.network_id, identity.genesis_hash).unwrap(),
        MempoolConfig::default(),
    )
    .unwrap()
}

fn status(engine: &SingleValidatorEngine) -> SyncStatus {
    let state = engine.finalized_state();
    SyncStatus {
        height: state.height.0,
        consensus_hash: state.consensus_hash.into(),
        domain_heads_root: state.domain_heads_root().into(),
        checkpoint_height: state.height.0,
    }
}

fn network(address: Multiaddr) -> NetworkService {
    let identity = identity();
    NetworkService::new(NetworkServiceConfig::development(
        identity.network_id,
        identity.genesis_hash,
        address,
    ))
    .unwrap()
}

const SENDER: Address = address!("4a62316623ad457f02cdc5d997ded67a383ec569");

fn sign_transaction(mut transaction: Eip1559Transaction) -> Bytes {
    let digest = keccak256(encode_eip1559_signing_payload(&transaction).unwrap());
    let key = SigningKey::from_bytes((&[7_u8; 32]).into()).unwrap();
    let (signature, recovery_id) = key.sign_prehash_recoverable(digest.as_slice()).unwrap();
    let bytes = signature.to_bytes();
    transaction.r = U256::from_be_slice(&bytes[..32]);
    transaction.s = U256::from_be_slice(&bytes[32..]);
    transaction.y_parity = recovery_id.is_y_odd();
    encode_eip1559(&transaction).unwrap().into()
}

fn signed_transfer(chain_id: u64, nonce: u64) -> Bytes {
    sign_transaction(Eip1559Transaction {
        chain_id,
        nonce,
        max_priority_fee_per_gas: 2,
        max_fee_per_gas: 2_000_000_000,
        gas_limit: 21_000,
        to: Some(Address::repeat_byte(0xaa)),
        value: U256::from(123),
        input: Bytes::new(),
        access_list: Vec::new(),
        y_parity: false,
        r: U256::ZERO,
        s: U256::ZERO,
    })
}

fn signed_create_domain(root: arbor_primitives::DomainId) -> (Bytes, arbor_primitives::DomainId) {
    let envelope = sign_transaction(Eip1559Transaction {
        chain_id: 2_048,
        nonce: 0,
        max_priority_fee_per_gas: 1,
        max_fee_per_gas: 2_000_000_000,
        gas_limit: 500_000,
        to: Some(CHAIN_REGISTRY_ADDRESS),
        value: MIN_CREATION_DEPOSIT,
        input: encode_create_chain_call(&CreateChainRequest {
            parent_domain_id: root,
            name: "M7 Snapshot Child".to_owned(),
            symbol: "M7C".to_owned(),
            evm_chain_id: 2_049,
            owner: SENDER,
            gas_limit: 20_000_000,
            initial_base_fee: 1_000_000_000,
            initial_supply: U256::from(10_u128.pow(18)),
            protocol_revision: 1,
        })
        .unwrap(),
        access_list: Vec::new(),
        y_parity: false,
        r: U256::ZERO,
        s: U256::ZERO,
    });
    let transaction = arbor_codec::decode_eip1559(&envelope).unwrap();
    let transaction_hash = eip1559_transaction_hash(&transaction).unwrap();
    (
        envelope,
        derive_domain_id(identity().network_id, root, transaction_hash),
    )
}

struct DevelopmentImporter<'a> {
    engine: &'a mut SingleValidatorEngine,
}

impl BlockImport for DevelopmentImporter<'_> {
    fn status(&self) -> Result<SyncStatus, String> {
        Ok(status(self.engine))
    }

    fn verify_finality(
        &self,
        finalized: &FinalizedBlock,
        _block: &ConsensusBlock,
    ) -> Result<(), String> {
        if finalized.finality_proof.is_empty() {
            Ok(())
        } else {
            Err("development sync accepts only its explicit empty proof marker".to_owned())
        }
    }

    fn import_block(&mut self, block: ConsensusBlock) -> Result<SyncStatus, String> {
        self.engine
            .import_development_finalized_block(&block)
            .map_err(|error| error.to_string())?;
        Ok(status(self.engine))
    }
}

fn serve_sync_request(
    network: &mut NetworkService,
    engine: &SingleValidatorEngine,
    source_status: SyncStatus,
    inbound: InboundRequest,
) {
    match inbound.request.clone() {
        DirectRequest::Status => {
            network
                .respond(inbound, DirectResponse::Status(source_status))
                .unwrap();
        }
        DirectRequest::Blocks {
            start_height,
            limit,
        } => {
            let end = start_height
                .saturating_add(u64::from(limit))
                .min(source_status.height.saturating_add(1));
            let blocks = (start_height..end)
                .map(|height| {
                    let block = engine.finalized_block(height).unwrap().unwrap();
                    FinalizedBlock {
                        height,
                        block: encode_consensus_block(&block).unwrap(),
                        // SingleValidatorEngine deliberately has no QC. M8 replaces this
                        // explicit development proof mode.
                        finality_proof: Vec::new(),
                    }
                })
                .collect();
            network
                .respond(inbound, DirectResponse::Blocks(blocks))
                .unwrap();
        }
        _ => panic!("unexpected sync request"),
    }
}

async fn wait_for_listener(network: &mut NetworkService) -> Multiaddr {
    loop {
        if let NetworkEvent::Listening(address) = network.next_event().await {
            return address;
        }
    }
}

fn assert_bad_finality_does_not_import(
    source: &SingleValidatorEngine,
    target: &mut SingleValidatorEngine,
    source_status: SyncStatus,
) {
    let block = source.finalized_block(1).unwrap().unwrap();
    let response = FinalizedBlock {
        height: 1,
        block: encode_consensus_block(&block).unwrap(),
        finality_proof: vec![1],
    };
    let error = BlockSync::default()
        .import_response(
            &mut DevelopmentImporter { engine: target },
            source_status,
            vec![response],
        )
        .unwrap_err();
    assert!(error.to_string().contains("invalid finality proof"));
    assert_eq!(target.finalized_state().height.0, 0);
}

fn assert_sync_result(
    source: &SingleValidatorEngine,
    target: SingleValidatorEngine,
    target_dir: &TempDir,
    root_domain: arbor_primitives::DomainId,
    child_domain: arbor_primitives::DomainId,
) {
    assert_eq!(target.finalized_state(), source.finalized_state());
    assert_eq!(target.finalized_state().domains().count(), 2);
    assert_eq!(
        target
            .finalized_state()
            .domain(child_domain)
            .unwrap()
            .state
            .state_root(),
        source
            .finalized_state()
            .domain(child_domain)
            .unwrap()
            .state
            .state_root()
    );
    assert_eq!(
        target
            .finalized_state()
            .domain(root_domain)
            .unwrap()
            .state
            .account(Address::repeat_byte(0xaa))
            .unwrap()
            .unwrap()
            .balance,
        U256::from(123)
    );
    drop(target);
    let reopened = open_engine(target_dir);
    assert_eq!(reopened.finalized_state(), source.finalized_state());
}

fn prepare_multidomain_source(
    directory: &TempDir,
) -> (
    SingleValidatorEngine,
    arbor_primitives::DomainId,
    arbor_primitives::DomainId,
) {
    let mut source = open_engine(directory);
    let root = source.finalized_state().root_domain_id();
    let (create_domain, child) = signed_create_domain(root);
    source.submit_raw(root, create_domain).unwrap();
    source.submit_raw(root, signed_transfer(2_048, 1)).unwrap();
    source.produce_block(1_700_000_001).unwrap();
    source.submit_raw(child, signed_transfer(2_049, 0)).unwrap();
    for timestamp in 1_700_000_002..=1_700_000_003 {
        source.produce_block(timestamp).unwrap();
    }
    (source, root, child)
}

fn assert_child_history(engine: &SingleValidatorEngine, child: arbor_primitives::DomainId) {
    let history = engine.domain_history(child, 0, 8).unwrap();
    assert!(!history.is_empty());
    assert!(
        history
            .iter()
            .map(|bytes| decode_domain_header(bytes).unwrap())
            .all(|header| header.domain_id == child)
    );
}

async fn sync_three_blocks(
    source_network: &mut NetworkService,
    target_network: &mut NetworkService,
    source_engine: &SingleValidatorEngine,
    target_engine: &mut SingleValidatorEngine,
    source_peer: libp2p::PeerId,
    source_status: SyncStatus,
) {
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut requested_status = false;
        let mut requested_blocks = false;
        let mut imported = 0;
        loop {
            tokio::select! {
                event = source_network.next_event() => {
                    if let NetworkEvent::Request(inbound) = event {
                        serve_sync_request(
                            source_network,
                            source_engine,
                            source_status,
                            inbound,
                        );
                    }
                }
                event = target_network.next_event() => {
                    match event {
                        NetworkEvent::PeerAuthenticated { peer, .. }
                            if peer == source_peer && !requested_status =>
                        {
                            target_network.request(source_peer, DirectRequest::Status).unwrap();
                            requested_status = true;
                        }
                        NetworkEvent::Response {
                            peer,
                            response: DirectResponse::Status(remote),
                            ..
                        } if peer == source_peer && !requested_blocks => {
                            request_next_legacy_block(
                                target_network,
                                target_engine,
                                source_peer,
                                remote,
                            );
                            requested_blocks = true;
                        }
                        NetworkEvent::Response {
                            peer,
                            response: DirectResponse::Blocks(blocks),
                            ..
                        } if peer == source_peer => {
                            imported += BlockSync::default()
                                .import_response(
                                    &mut DevelopmentImporter {
                                        engine: target_engine,
                                    },
                                    source_status,
                                    blocks,
                                )
                                .unwrap()
                                .imported;
                            if imported == 3 {
                                break;
                            }
                            request_next_legacy_block(
                                target_network,
                                target_engine,
                                source_peer,
                                source_status,
                            );
                        }
                        _ => {}
                    }
                }
            }
        }
    })
    .await
    .unwrap();
}

fn request_next_legacy_block(
    network: &mut NetworkService,
    engine: &SingleValidatorEngine,
    source_peer: libp2p::PeerId,
    remote: SyncStatus,
) {
    let (start_height, limit) = BlockSync::default()
        .next_request(status(engine), remote)
        .expect("source still has finalized blocks");
    network
        .request(
            source_peer,
            DirectRequest::Blocks {
                start_height,
                limit,
            },
        )
        .unwrap();
}

fn produce_snapshot_bundle(
    source: &SingleValidatorEngine,
    child: arbor_primitives::DomainId,
) -> SnapshotBundle {
    let checkpoint = source.development_checkpoint();
    assert_eq!(checkpoint.state.domains().count(), 2);
    assert!(checkpoint.state.domain(child).is_some());
    SnapshotBundle::produce(
        &checkpoint.state,
        identity().genesis_hash,
        checkpoint.validator_set,
        checkpoint.finality_proof,
    )
    .unwrap()
}

#[tokio::test]
async fn listener_catches_up_over_libp2p_and_reopens_the_same_roots() {
    let source_dir = tempfile::tempdir().unwrap();
    let target_dir = tempfile::tempdir().unwrap();
    let (source_engine, root_domain, child_domain) = prepare_multidomain_source(&source_dir);
    let source_status = status(&source_engine);

    let address: Multiaddr = "/ip4/127.0.0.1/tcp/0".parse().unwrap();
    let mut source_network = network(address.clone());
    let mut target_network = network(address);
    let source_peer = source_network.local_peer_id();
    let source_address = tokio::time::timeout(
        Duration::from_secs(5),
        wait_for_listener(&mut source_network),
    )
    .await
    .unwrap();
    target_network.dial(source_peer, source_address).unwrap();

    let mut target_engine = open_engine(&target_dir);
    assert_bad_finality_does_not_import(&source_engine, &mut target_engine, source_status);
    sync_three_blocks(
        &mut source_network,
        &mut target_network,
        &source_engine,
        &mut target_engine,
        source_peer,
        source_status,
    )
    .await;

    assert_child_history(&source_engine, child_domain);
    assert_sync_result(
        &source_engine,
        target_engine,
        &target_dir,
        root_domain,
        child_domain,
    );
}

#[test]
fn checkpoint_snapshot_then_incremental_blocks_reopen_identically() {
    let source_dir = tempfile::tempdir().unwrap();
    let target_dir = tempfile::tempdir().unwrap();
    let (mut source, _root, child) = prepare_multidomain_source(&source_dir);

    let identity = identity();
    let bundle = produce_snapshot_bundle(&source, child);
    let manifest_bytes = bundle.manifest_bytes().unwrap();

    let mut target = open_engine(&target_dir);
    let mut invalid_manifest = bundle.manifest().clone();
    invalid_manifest.finality_proof = vec![1];
    assert!(
        SnapshotStaging::new(
            &invalid_manifest.encode().unwrap(),
            identity.network_id,
            identity.genesis_hash,
            &DevelopmentCheckpointVerifier,
        )
        .is_err()
    );
    assert_eq!(target.finalized_state().height.0, 0);
    let mut staging = SnapshotStaging::new(
        &manifest_bytes,
        identity.network_id,
        identity.genesis_hash,
        &DevelopmentCheckpointVerifier,
    )
    .unwrap();
    let first_domain = &bundle.manifest().domains[0];
    let first_descriptor = first_domain.state.chunks[0];
    let mut tampered = bundle
        .chunk(first_domain.domain_id, first_descriptor.index)
        .unwrap()
        .to_vec();
    tampered[0] ^= 1;
    assert!(
        staging
            .stage_chunk(first_domain.domain_id, first_descriptor.index, tampered)
            .is_err()
    );
    assert_eq!(target.finalized_state().height.0, 0);

    for domain in &bundle.manifest().domains {
        for chunk in &domain.state.chunks {
            let bytes = bundle
                .chunk(domain.domain_id, chunk.index)
                .unwrap()
                .to_vec();
            staging
                .stage_chunk(domain.domain_id, chunk.index, bytes.clone())
                .unwrap();
            // Reordered/duplicate delivery is idempotent.
            staging
                .stage_chunk(domain.domain_id, chunk.index, bytes)
                .unwrap();
        }
    }
    let imported = staging.finish().unwrap();
    target
        .import_development_checkpoint(DevelopmentCheckpoint {
            state: imported.state,
            validator_set: imported.validator_set,
            finality_proof: imported.finality_proof,
        })
        .unwrap();
    assert_eq!(target.finalized_state(), source.finalized_state());

    for timestamp in 1_700_000_004..=1_700_000_005 {
        source.produce_block(timestamp).unwrap();
    }
    let remote = status(&source);
    let mut imported = 0;
    for height in 4..=5 {
        let block = FinalizedBlock {
            height,
            block: encode_consensus_block(&source.finalized_block(height).unwrap().unwrap())
                .unwrap(),
            finality_proof: Vec::new(),
        };
        imported += BlockSync::default()
            .import_response(
                &mut DevelopmentImporter {
                    engine: &mut target,
                },
                remote,
                vec![block],
            )
            .unwrap()
            .imported;
    }
    assert_eq!(imported, 2);
    assert_eq!(target.finalized_state(), source.finalized_state());
    drop(target);
    assert_eq!(
        open_engine(&target_dir).finalized_state(),
        source.finalized_state()
    );
}
