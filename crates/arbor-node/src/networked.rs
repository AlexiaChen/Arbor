use std::{
    collections::{BTreeMap, VecDeque},
    net::{IpAddr, SocketAddr},
    str::FromStr,
    time::Duration,
};

use alloy_primitives::{B256, Bytes};
use arbor_chain::{ConsensusBlock, encode_consensus_block};
use arbor_consensus::{DevelopmentCheckpoint, SingleValidatorEngine};
use arbor_network::{
    BlockBody, BlockImport, BlockSync, Capability, CheckpointManifest, ConsensusDirectMessage,
    DevelopmentCheckpointVerifier, DevelopmentHeaderVerifier, DirectRequest, DirectResponse,
    FinalizedAnnouncement, FinalizedBlock, FinalizedHeader, GossipMessage, Handshake, HeaderSync,
    InboundRequest, NetworkError, NetworkEvent, NetworkService, NetworkServiceConfig, NodeRole,
    PersistentPeer, SnapshotBundle, SnapshotStaging, SyncStatus, VerifiedHeader,
    consensus_direct_mailbox, finalized_blocks_from_bodies, load_or_create_peer_identity,
};
use arbor_primitives::{DomainId, NetworkId};
use libp2p::{Multiaddr, PeerId};

use crate::{
    DevNodeError, HistorySubscription, NetworkConfig, Shutdown, dev_database_identity,
    open_dev_engine,
};

const SNAPSHOT_CACHE_COUNT: usize = 4;
const CONSENSUS_DIRECT_QUEUE: usize = 64;
const RECONNECT_INTERVAL: Duration = Duration::from_secs(1);
const METRICS_INTERVAL: Duration = Duration::from_secs(10);
const PRODUCTION_INTERVAL: Duration = Duration::from_millis(250);
const SHUTDOWN_NETWORK_DRAIN: Duration = Duration::from_secs(5);

enum PendingAction {
    Status,
    Manifest {
        remote: SyncStatus,
    },
    Chunk {
        domain_id: DomainId,
        index: u32,
    },
    Headers {
        remote: SyncStatus,
    },
    Bodies {
        remote: SyncStatus,
        headers: Vec<VerifiedHeader>,
    },
}

struct SnapshotDownload {
    remote: SyncStatus,
    staging: SnapshotStaging,
    remaining: VecDeque<(DomainId, u32)>,
}

struct InitializedNode {
    engine: SingleValidatorEngine,
    network: NetworkService,
    persistent: Vec<PersistentPeer>,
}

struct DevelopmentImporter<'a> {
    engine: &'a mut SingleValidatorEngine,
}

impl BlockImport for DevelopmentImporter<'_> {
    fn status(&self) -> Result<SyncStatus, String> {
        Ok(engine_status(
            self.engine,
            self.engine.finalized_state().height.0,
        ))
    }

    fn verify_finality(
        &self,
        finalized: &FinalizedBlock,
        _block: &ConsensusBlock,
    ) -> Result<(), String> {
        if finalized.finality_proof.is_empty() {
            Ok(())
        } else {
            Err("development block finality proof must be empty".to_owned())
        }
    }

    fn import_block(&mut self, block: ConsensusBlock) -> Result<SyncStatus, String> {
        self.engine
            .import_development_finalized_block(&block)
            .map_err(|error| error.to_string())?;
        Ok(engine_status(
            self.engine,
            self.engine.finalized_state().height.0,
        ))
    }
}

/// Runs a complete development network node.
///
/// A validator produces immediate-finality development blocks; a listener executes the exact
/// same application but only imports authenticated snapshot/block sync. Neither path is production
/// BFT.
pub(crate) async fn run_dev_node(
    data_dir: &std::path::Path,
    history: HistorySubscription,
    config: NetworkConfig,
    produce: bool,
    shutdown: &mut Shutdown,
) -> Result<(), DevNodeError> {
    let identity = dev_database_identity();
    let InitializedNode {
        mut engine,
        mut network,
        persistent,
    } = initialize_network_node(
        data_dir,
        &history,
        &config,
        produce,
        identity.network_id,
        identity.genesis_hash,
    )?;
    subscribe_domains(&mut network, &engine)?;

    let (consensus_sender, mut consensus_receiver) =
        consensus_direct_mailbox(CONSENSUS_DIRECT_QUEUE).map_err(network_error)?;
    let mut actions = BTreeMap::new();
    let mut downloads = BTreeMap::<PeerId, SnapshotDownload>::new();
    let mut snapshots = BTreeMap::<u64, SnapshotBundle>::new();
    cache_snapshot(&engine, identity.genesis_hash, &mut snapshots)?;
    let mut production = tokio::time::interval(PRODUCTION_INTERVAL);
    production.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut reconnect = tokio::time::interval(RECONNECT_INTERVAL);
    reconnect.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut metrics = tokio::time::interval(METRICS_INTERVAL);
    metrics.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            () = shutdown.cancelled() => {
                if produce {
                    drain_network_before_shutdown(
                        &mut network,
                        &mut engine,
                        identity.network_id,
                        identity.genesis_hash,
                        &mut snapshots,
                        &consensus_sender,
                        &mut actions,
                        &mut downloads,
                    ).await?;
                }
                return Ok(());
            },
            _ = production.tick(), if produce => {
                produce_and_announce(&mut engine, &mut network, identity.genesis_hash, &mut snapshots)?;
            }
            _ = reconnect.tick() => {
                for peer in &persistent {
                    if !network.is_connected(&peer.peer_id) {
                        let _ = network.dial(peer.peer_id, peer.address.clone());
                    } else if network.is_authenticated(&peer.peer_id)
                        && actions.is_empty()
                        && downloads.is_empty()
                    {
                        send_request(
                            &mut network,
                            &mut actions,
                            peer.peer_id,
                            DirectRequest::Status,
                            PendingAction::Status,
                        )?;
                    }
                }
            }
            _ = metrics.tick() => {
                log_network_metrics(&network);
            }
            message = consensus_receiver.recv() => {
                if let Some(message) = message {
                    tracing::trace!(
                        peer = %message.peer,
                        kind = message.kind,
                        bytes = message.payload.len(),
                        "development consensus-direct loopback consumed message"
                    );
                }
            }
            event = network.next_event() => {
                handle_network_event(
                    event,
                    &mut network,
                    &mut engine,
                    identity.network_id,
                    identity.genesis_hash,
                    &mut snapshots,
                    &consensus_sender,
                    &mut actions,
                    &mut downloads,
                )?;
                subscribe_domains(&mut network, &engine)?;
            }
        }
    }
}

fn initialize_network_node(
    data_dir: &std::path::Path,
    history: &HistorySubscription,
    config: &NetworkConfig,
    produce: bool,
    network_id: NetworkId,
    genesis_hash: B256,
) -> Result<InitializedNode, DevNodeError> {
    let engine = open_dev_engine(data_dir, history)?;
    let keypair =
        load_or_create_peer_identity(data_dir.join("network/peer.key")).map_err(network_error)?;
    let mut service_config = NetworkServiceConfig::development(
        network_id,
        genesis_hash,
        socket_multiaddr(config.listen_addr)?,
    );
    service_config.keypair = keypair;
    service_config.enable_mdns = config.mdns;
    service_config.handshake = Handshake::new(
        network_id,
        genesis_hash,
        if produce {
            NodeRole::Validator
        } else {
            NodeRole::Full
        },
        vec![
            Capability::TransactionGossip,
            Capability::FinalizedGossip,
            Capability::BlockSync,
            Capability::StateSync,
            Capability::DomainHistory,
            Capability::ConsensusDirect,
        ],
    );
    let network = NetworkService::new(service_config).map_err(network_error)?;
    let persistent = config
        .persistent_peers
        .iter()
        .map(|value| PersistentPeer::from_str(value).map_err(network_error))
        .collect::<Result<Vec<_>, DevNodeError>>()?;
    Ok(InitializedNode {
        engine,
        network,
        persistent,
    })
}

fn produce_and_announce(
    engine: &mut SingleValidatorEngine,
    network: &mut NetworkService,
    genesis_hash: B256,
    snapshots: &mut BTreeMap<u64, SnapshotBundle>,
) -> Result<(), DevNodeError> {
    let timestamp = engine
        .finalized_state()
        .timestamp
        .checked_add(1)
        .ok_or(DevNodeError::TimestampOverflow)?;
    let event = engine.produce_block(timestamp)?;
    subscribe_domains(network, engine)?;
    cache_snapshot(engine, genesis_hash, snapshots)?;
    let _ = network.publish_finalized(FinalizedAnnouncement {
        height: event.height,
        consensus_hash: event.consensus_hash.into(),
        domain_heads_root: event.domain_heads_root.into(),
    });
    tracing::info!(
        height = event.height,
        consensus_hash = %event.consensus_hash,
        domain_heads_root = %event.domain_heads_root,
        "finalized development block"
    );
    Ok(())
}

fn log_network_metrics(network: &NetworkService) {
    let current = network.metrics();
    tracing::info!(
        connected_peers = current.connected_peers,
        authenticated_peers = current.authenticated_peers,
        inbound_requests = current.inbound_requests,
        inbound_responses = current.inbound_responses,
        gossip_messages = current.gossip_messages,
        rejected_messages = current.rejected_messages,
        request_failures = current.request_failures,
        discovered_peers = current.discovered_peers,
        discovery_failures = current.discovery_failures,
        "network metrics"
    );
}

#[allow(clippy::too_many_arguments)]
async fn drain_network_before_shutdown(
    network: &mut NetworkService,
    engine: &mut SingleValidatorEngine,
    network_id: NetworkId,
    genesis_hash: B256,
    snapshots: &mut BTreeMap<u64, SnapshotBundle>,
    consensus_sender: &arbor_network::ConsensusDirectSender,
    actions: &mut BTreeMap<u64, PendingAction>,
    downloads: &mut BTreeMap<PeerId, SnapshotDownload>,
) -> Result<(), DevNodeError> {
    let deadline = tokio::time::sleep(SHUTDOWN_NETWORK_DRAIN);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            () = &mut deadline => return Ok(()),
            event = network.next_event() => {
                handle_network_event(
                    event,
                    network,
                    engine,
                    network_id,
                    genesis_hash,
                    snapshots,
                    consensus_sender,
                    actions,
                    downloads,
                )?;
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_network_event(
    event: NetworkEvent,
    network: &mut NetworkService,
    engine: &mut SingleValidatorEngine,
    network_id: NetworkId,
    genesis_hash: B256,
    snapshots: &mut BTreeMap<u64, SnapshotBundle>,
    consensus_sender: &arbor_network::ConsensusDirectSender,
    actions: &mut BTreeMap<u64, PendingAction>,
    downloads: &mut BTreeMap<PeerId, SnapshotDownload>,
) -> Result<(), DevNodeError> {
    let previous_height = engine.finalized_state().height.0;
    match event {
        NetworkEvent::Listening(address) => {
            tracing::info!(peer_id = %network.local_peer_id(), %address, "P2P listener ready");
        }
        NetworkEvent::PeerDiscovered { peer, address } => {
            tracing::debug!(%peer, %address, "discovered P2P peer");
        }
        NetworkEvent::PeerDisconnected { peer } => {
            downloads.remove(&peer);
            tracing::debug!(%peer, "P2P peer disconnected");
        }
        NetworkEvent::PeerAuthenticated { peer, .. } => {
            send_request(
                network,
                actions,
                peer,
                DirectRequest::Status,
                PendingAction::Status,
            )?;
        }
        NetworkEvent::PeerRejected { peer, reason } => {
            tracing::warn!(%peer, %reason, "P2P peer rejected");
        }
        NetworkEvent::Request(inbound) => {
            serve_request(network, engine, snapshots, consensus_sender, inbound)?;
        }
        NetworkEvent::Gossip { peer, message } => match message {
            GossipMessage::Transaction {
                domain_id,
                envelope,
            } => {
                if let Err(error) =
                    engine.submit_raw(DomainId(B256::from(domain_id)), Bytes::from(envelope))
                {
                    tracing::debug!(%peer, %error, "rejected gossiped transaction");
                }
            }
            GossipMessage::Finalized(announcement)
                if announcement.height > engine.finalized_state().height.0
                    && actions.is_empty()
                    && downloads.is_empty() =>
            {
                send_request(
                    network,
                    actions,
                    peer,
                    DirectRequest::Status,
                    PendingAction::Status,
                )?;
            }
            GossipMessage::Finalized(_) => {}
        },
        NetworkEvent::Response {
            peer,
            request_id,
            response,
        } => {
            let action = actions.remove(&request_id);
            handle_response(
                peer,
                action,
                response,
                network,
                engine,
                network_id,
                genesis_hash,
                actions,
                downloads,
            )?;
        }
        NetworkEvent::RequestFailed {
            peer,
            request_id,
            reason,
        } => {
            if let Some(id) = request_id {
                actions.remove(&id);
            }
            downloads.remove(&peer);
            tracing::debug!(%peer, %reason, "P2P request failed");
        }
    }
    if engine.finalized_state().height.0 != previous_height {
        cache_snapshot(engine, genesis_hash, snapshots)?;
    }
    Ok(())
}

fn serve_request(
    network: &mut NetworkService,
    engine: &SingleValidatorEngine,
    snapshots: &BTreeMap<u64, SnapshotBundle>,
    consensus_sender: &arbor_network::ConsensusDirectSender,
    inbound: InboundRequest,
) -> Result<(), DevNodeError> {
    let response = match inbound.request.clone() {
        DirectRequest::Handshake => DirectResponse::Rejected { code: 400 },
        DirectRequest::Status => DirectResponse::Status(engine_status(
            engine,
            snapshots.keys().next_back().copied().unwrap_or(0),
        )),
        DirectRequest::Headers {
            start_height,
            limit,
        } => serve_headers(engine, start_height, limit)?,
        DirectRequest::BlockBodies { heights } => serve_bodies(engine, heights)?,
        DirectRequest::Blocks {
            start_height,
            limit,
        } => serve_blocks(engine, start_height, limit)?,
        DirectRequest::SnapshotManifest { checkpoint_height } => snapshots
            .get(&checkpoint_height)
            .map(SnapshotBundle::manifest_bytes)
            .transpose()
            .map_err(network_error)?
            .map_or(
                DirectResponse::Rejected { code: 404 },
                DirectResponse::SnapshotManifest,
            ),
        DirectRequest::SnapshotChunk {
            checkpoint_height,
            domain_id,
            index,
        } => snapshots
            .get(&checkpoint_height)
            .and_then(|bundle| bundle.chunk(DomainId(B256::from(domain_id)), index))
            .map_or(DirectResponse::Rejected { code: 404 }, |bytes| {
                DirectResponse::SnapshotChunk(bytes.to_vec())
            }),
        DirectRequest::DomainHistory {
            domain_id,
            start_number,
            limit,
        } => DirectResponse::DomainHistory(engine.domain_history(
            DomainId(B256::from(domain_id)),
            start_number,
            usize::from(limit),
        )?),
        DirectRequest::Consensus { kind, payload } => {
            match consensus_sender.try_send(ConsensusDirectMessage {
                peer: inbound.peer,
                kind,
                payload,
            }) {
                Ok(()) => DirectResponse::ConsensusAccepted,
                Err(_) => DirectResponse::Rejected { code: 429 },
            }
        }
    };
    network.respond(inbound, response).map_err(network_error)
}

fn serve_headers(
    engine: &SingleValidatorEngine,
    start_height: u64,
    limit: u16,
) -> Result<DirectResponse, DevNodeError> {
    let mut headers = Vec::new();
    for height in requested_heights(engine, start_height, limit) {
        let Some(block) = engine.finalized_block(height)? else {
            return Ok(DirectResponse::Rejected { code: 404 });
        };
        headers.push(FinalizedHeader {
            height,
            header: arbor_codec::encode_consensus_header(&block.header).map_err(network_error)?,
            finality_proof: Vec::new(),
        });
    }
    Ok(DirectResponse::Headers(headers))
}

fn serve_bodies(
    engine: &SingleValidatorEngine,
    heights: Vec<u64>,
) -> Result<DirectResponse, DevNodeError> {
    let mut bodies = Vec::with_capacity(heights.len());
    for height in heights {
        let Some(block) = engine.finalized_block(height)? else {
            return Ok(DirectResponse::Rejected { code: 404 });
        };
        bodies.push(BlockBody {
            height,
            block: encode_consensus_block(&block).map_err(network_error)?,
        });
    }
    Ok(DirectResponse::BlockBodies(bodies))
}

fn serve_blocks(
    engine: &SingleValidatorEngine,
    start_height: u64,
    limit: u16,
) -> Result<DirectResponse, DevNodeError> {
    let mut blocks = Vec::new();
    for height in requested_heights(engine, start_height, limit) {
        let Some(block) = engine.finalized_block(height)? else {
            return Ok(DirectResponse::Rejected { code: 404 });
        };
        blocks.push(FinalizedBlock {
            height,
            block: encode_consensus_block(&block).map_err(network_error)?,
            finality_proof: Vec::new(),
        });
    }
    Ok(DirectResponse::Blocks(blocks))
}

fn requested_heights(
    engine: &SingleValidatorEngine,
    start_height: u64,
    limit: u16,
) -> std::ops::Range<u64> {
    start_height
        ..start_height
            .saturating_add(u64::from(limit))
            .min(engine.finalized_state().height.0.saturating_add(1))
}

#[allow(clippy::too_many_arguments)]
fn handle_response(
    peer: PeerId,
    action: Option<PendingAction>,
    response: DirectResponse,
    network: &mut NetworkService,
    engine: &mut SingleValidatorEngine,
    network_id: NetworkId,
    genesis_hash: B256,
    actions: &mut BTreeMap<u64, PendingAction>,
    downloads: &mut BTreeMap<PeerId, SnapshotDownload>,
) -> Result<(), DevNodeError> {
    match (action, response) {
        (Some(PendingAction::Status), DirectResponse::Status(remote)) => {
            request_sync_path(peer, remote, network, engine, actions)?;
        }
        (Some(PendingAction::Manifest { remote }), DirectResponse::SnapshotManifest(bytes)) => {
            begin_snapshot_download(
                peer,
                remote,
                &bytes,
                network_id,
                genesis_hash,
                network,
                actions,
                downloads,
            )?;
        }
        (Some(PendingAction::Chunk { domain_id, index }), DirectResponse::SnapshotChunk(bytes)) => {
            accept_snapshot_chunk(
                peer, domain_id, index, bytes, network, engine, actions, downloads,
            )?;
        }
        (Some(PendingAction::Headers { remote }), DirectResponse::Headers(headers)) => {
            let local = engine_status(engine, 0);
            let verified = HeaderSync
                .verify_response(local, remote, headers, &DevelopmentHeaderVerifier)
                .map_err(network_error)?;
            let heights = verified
                .iter()
                .map(|header| header.header.height.0)
                .collect();
            send_request(
                network,
                actions,
                peer,
                DirectRequest::BlockBodies { heights },
                PendingAction::Bodies {
                    remote,
                    headers: verified,
                },
            )?;
        }
        (Some(PendingAction::Bodies { remote, headers }), DirectResponse::BlockBodies(bodies)) => {
            let blocks = finalized_blocks_from_bodies(&headers, bodies).map_err(network_error)?;
            BlockSync::default()
                .import_response(&mut DevelopmentImporter { engine }, remote, blocks)
                .map_err(network_error)?;
            request_sync_path(peer, remote, network, engine, actions)?;
        }
        (_, DirectResponse::Rejected { code }) => {
            tracing::debug!(%peer, code, "peer rejected sync request");
        }
        (None, _) => {}
        _ => {
            return Err(DevNodeError::Network(
                "direct response does not match pending request".to_owned(),
            ));
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn begin_snapshot_download(
    peer: PeerId,
    remote: SyncStatus,
    manifest_bytes: &[u8],
    network_id: NetworkId,
    genesis_hash: B256,
    network: &mut NetworkService,
    actions: &mut BTreeMap<u64, PendingAction>,
    downloads: &mut BTreeMap<PeerId, SnapshotDownload>,
) -> Result<(), DevNodeError> {
    let manifest = CheckpointManifest::decode(manifest_bytes).map_err(network_error)?;
    if manifest.height.0 != remote.checkpoint_height {
        return Err(DevNodeError::Network(
            "snapshot manifest height differs from advertised checkpoint".to_owned(),
        ));
    }
    let staging = SnapshotStaging::new(
        manifest_bytes,
        network_id,
        genesis_hash,
        &DevelopmentCheckpointVerifier,
    )
    .map_err(network_error)?;
    let remaining = manifest
        .domains
        .iter()
        .flat_map(|domain| {
            domain
                .state
                .chunks
                .iter()
                .map(move |chunk| (domain.domain_id, chunk.index))
        })
        .collect();
    downloads.insert(
        peer,
        SnapshotDownload {
            remote,
            staging,
            remaining,
        },
    );
    request_next_chunk(peer, network, actions, downloads)
}

#[allow(clippy::too_many_arguments)]
fn accept_snapshot_chunk(
    peer: PeerId,
    domain_id: DomainId,
    index: u32,
    bytes: Vec<u8>,
    network: &mut NetworkService,
    engine: &mut SingleValidatorEngine,
    actions: &mut BTreeMap<u64, PendingAction>,
    downloads: &mut BTreeMap<PeerId, SnapshotDownload>,
) -> Result<(), DevNodeError> {
    let download = downloads
        .get_mut(&peer)
        .ok_or_else(|| DevNodeError::Network("snapshot session disappeared".to_owned()))?;
    download
        .staging
        .stage_chunk(domain_id, index, bytes)
        .map_err(network_error)?;
    request_next_chunk(peer, network, actions, downloads)?;
    if !downloads
        .get(&peer)
        .is_some_and(|download| download.staging.is_complete())
    {
        return Ok(());
    }
    let download = downloads
        .remove(&peer)
        .ok_or_else(|| DevNodeError::Network("snapshot session disappeared".to_owned()))?;
    let remote = download.remote;
    let imported = download.staging.finish().map_err(network_error)?;
    engine.import_development_checkpoint(DevelopmentCheckpoint {
        state: imported.state,
        validator_set: imported.validator_set,
        finality_proof: imported.finality_proof,
    })?;
    request_sync_path(peer, remote, network, engine, actions)
}

fn request_sync_path(
    peer: PeerId,
    remote: SyncStatus,
    network: &mut NetworkService,
    engine: &SingleValidatorEngine,
    actions: &mut BTreeMap<u64, PendingAction>,
) -> Result<(), DevNodeError> {
    let local = engine_status(engine, 0);
    if remote.height <= local.height {
        return Ok(());
    }
    if local.height == 0 && remote.checkpoint_height > 0 {
        send_request(
            network,
            actions,
            peer,
            DirectRequest::SnapshotManifest {
                checkpoint_height: remote.checkpoint_height,
            },
            PendingAction::Manifest { remote },
        )
    } else if let Some((start_height, limit)) = BlockSync::default().next_request(local, remote) {
        send_request(
            network,
            actions,
            peer,
            DirectRequest::Headers {
                start_height,
                limit,
            },
            PendingAction::Headers { remote },
        )
    } else {
        Ok(())
    }
}

fn request_next_chunk(
    peer: PeerId,
    network: &mut NetworkService,
    actions: &mut BTreeMap<u64, PendingAction>,
    downloads: &mut BTreeMap<PeerId, SnapshotDownload>,
) -> Result<(), DevNodeError> {
    let Some(download) = downloads.get_mut(&peer) else {
        return Ok(());
    };
    let Some((domain_id, index)) = download.remaining.pop_front() else {
        return Ok(());
    };
    send_request(
        network,
        actions,
        peer,
        DirectRequest::SnapshotChunk {
            checkpoint_height: download.remote.checkpoint_height,
            domain_id: domain_id.0.into(),
            index,
        },
        PendingAction::Chunk { domain_id, index },
    )
}

fn send_request(
    network: &mut NetworkService,
    actions: &mut BTreeMap<u64, PendingAction>,
    peer: PeerId,
    request: DirectRequest,
    action: PendingAction,
) -> Result<(), DevNodeError> {
    let request_id = network.request(peer, request).map_err(network_error)?;
    actions.insert(request_id, action);
    Ok(())
}

fn subscribe_domains(
    network: &mut NetworkService,
    engine: &SingleValidatorEngine,
) -> Result<(), DevNodeError> {
    for (domain_id, _) in engine.finalized_state().domains() {
        network.subscribe_domain(domain_id).map_err(network_error)?;
    }
    Ok(())
}

fn cache_snapshot(
    engine: &SingleValidatorEngine,
    genesis_hash: B256,
    snapshots: &mut BTreeMap<u64, SnapshotBundle>,
) -> Result<(), DevNodeError> {
    if engine.finalized_state().height.0 == 0 {
        return Ok(());
    }
    let checkpoint = engine.development_checkpoint();
    let height = checkpoint.state.height.0;
    snapshots.insert(
        height,
        SnapshotBundle::produce(
            &checkpoint.state,
            genesis_hash,
            checkpoint.validator_set,
            checkpoint.finality_proof,
        )
        .map_err(network_error)?,
    );
    while snapshots.len() > SNAPSHOT_CACHE_COUNT {
        let Some(oldest) = snapshots.keys().next().copied() else {
            break;
        };
        snapshots.remove(&oldest);
    }
    Ok(())
}

fn engine_status(engine: &SingleValidatorEngine, checkpoint_height: u64) -> SyncStatus {
    let state = engine.finalized_state();
    SyncStatus {
        height: state.height.0,
        consensus_hash: state.consensus_hash.into(),
        domain_heads_root: state.domain_heads_root().into(),
        checkpoint_height,
    }
}

fn socket_multiaddr(address: SocketAddr) -> Result<Multiaddr, DevNodeError> {
    let value = match address.ip() {
        IpAddr::V4(ip) => format!("/ip4/{ip}/tcp/{}", address.port()),
        IpAddr::V6(ip) => format!("/ip6/{ip}/tcp/{}", address.port()),
    };
    value
        .parse()
        .map_err(|error| DevNodeError::Network(format!("invalid listen multiaddress: {error}")))
}

fn network_error(error: impl std::fmt::Display) -> DevNodeError {
    DevNodeError::Network(error.to_string())
}

impl From<NetworkError> for DevNodeError {
    fn from(error: NetworkError) -> Self {
        network_error(error)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use arbor_testkit::{reserve_loopback_port, with_timeout};

    use super::*;
    use crate::{Shutdown, initialize_dev_chain};

    fn spawn_node(
        data_dir: PathBuf,
        config: NetworkConfig,
        produce: bool,
    ) -> (
        crate::ShutdownTrigger,
        tokio::task::JoinHandle<Result<(), DevNodeError>>,
    ) {
        let (trigger, mut shutdown) = Shutdown::channel();
        let task = tokio::spawn(async move {
            run_dev_node(
                &data_dir,
                HistorySubscription::All,
                config,
                produce,
                &mut shutdown,
            )
            .await
        });
        (trigger, task)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn persistent_listener_snapshot_syncs_restarts_and_catches_up() {
        with_timeout(Duration::from_secs(20), async {
            let source = tempfile::tempdir().unwrap();
            let target = tempfile::tempdir().unwrap();
            initialize_dev_chain(source.path()).unwrap();
            initialize_dev_chain(target.path()).unwrap();
            let source_peer = load_or_create_peer_identity(source.path().join("network/peer.key"))
                .unwrap()
                .public()
                .to_peer_id();
            let (listen_addr, reservation) = reserve_loopback_port().unwrap();
            drop(reservation);
            let source_config = NetworkConfig {
                listen_addr,
                mdns: false,
                persistent_peers: Vec::new(),
            };
            let target_config = NetworkConfig {
                listen_addr: "127.0.0.1:0".parse().unwrap(),
                mdns: false,
                persistent_peers: vec![format!(
                    "{source_peer}@/ip4/127.0.0.1/tcp/{}",
                    listen_addr.port()
                )],
            };

            let (source_trigger, source_task) =
                spawn_node(source.path().to_owned(), source_config, true);
            tokio::time::sleep(Duration::from_secs(1)).await;
            let (first_trigger, first_task) =
                spawn_node(target.path().to_owned(), target_config.clone(), false);
            tokio::time::sleep(Duration::from_millis(1_500)).await;
            first_trigger.shutdown();
            first_task.await.unwrap().unwrap();

            // Source advances while the listener is disconnected.
            tokio::time::sleep(Duration::from_secs(1)).await;
            let (second_trigger, second_task) =
                spawn_node(target.path().to_owned(), target_config, false);
            tokio::time::sleep(Duration::from_millis(1_500)).await;
            source_trigger.shutdown();
            source_task.await.unwrap().unwrap();
            // Leave the listener running long enough to receive the source's final announcement.
            tokio::time::sleep(Duration::from_secs(1)).await;
            second_trigger.shutdown();
            second_task.await.unwrap().unwrap();

            let source_engine = open_dev_engine(source.path(), &HistorySubscription::All).unwrap();
            let target_engine = open_dev_engine(target.path(), &HistorySubscription::All).unwrap();
            assert!(source_engine.finalized_state().height.0 >= 8);
            assert_eq!(
                target_engine.finalized_state(),
                source_engine.finalized_state()
            );
        })
        .await
        .unwrap();
    }
}
