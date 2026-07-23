use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};

use alloy_primitives::keccak256;
use futures::StreamExt;
use libp2p::{
    Multiaddr, PeerId, StreamProtocol, Swarm, SwarmBuilder,
    gossipsub::{self, IdentTopic, MessageAuthenticity, ValidationMode},
    identify, identity,
    kad::{self, store::MemoryStore},
    noise, ping,
    request_response::{self, OutboundRequestId, ProtocolSupport, ResponseChannel},
    swarm::{NetworkBehaviour, SwarmEvent, dial_opts::DialOpts},
    tcp, yamux,
};
use thiserror::Error;

use crate::{
    Capability, DirectRequest, DirectResponse, FinalizedAnnouncement, GossipMessage, Handshake,
    MAX_DIRECT_REQUEST_BYTES, MAX_DIRECT_RESPONSE_BYTES, NodeRole, PeerBook, PeerDisposition,
    RequestEnvelope, ResponseEnvelope,
};

const IDENTIFY_PROTOCOL: &str = "/arbor/identify/1";
const DIRECT_PROTOCOL: &str = "/arbor/direct/1";
const KADEMLIA_PROTOCOL: &str = "/arbor/kad/1";
const GOSSIP_VERSION: u8 = 1;
const GOSSIP_TRANSACTION: u8 = 1;
const GOSSIP_FINALIZED: u8 = 2;
const MAX_GOSSIP_BYTES: usize = 300 * 1024;

/// Runtime configuration for one rust-libp2p service.
#[derive(Clone)]
pub struct NetworkServiceConfig {
    /// Persistent libp2p transport identity, separate from the validator key.
    pub keypair: identity::Keypair,
    /// Genesis-bound Arbor application handshake.
    pub handshake: Handshake,
    /// TCP multiaddress to listen on.
    pub listen_address: Multiaddr,
    /// Direct exchange deadline.
    pub request_timeout: Duration,
    /// Maximum concurrent inbound plus outbound direct streams.
    pub max_concurrent_streams: usize,
    /// Enables RFC 6762 local discovery through the safe `mdns-sd` adapter.
    pub enable_mdns: bool,
}

impl NetworkServiceConfig {
    /// Builds a full-node configuration with a fresh in-memory peer identity.
    ///
    /// Production node assembly should persist the returned keypair separately from consensus
    /// keys instead of regenerating it on every restart.
    #[must_use]
    pub fn development(
        network_id: arbor_primitives::NetworkId,
        genesis_hash: alloy_primitives::B256,
        listen_address: Multiaddr,
    ) -> Self {
        Self {
            keypair: identity::Keypair::generate_ed25519(),
            handshake: Handshake::new(
                network_id,
                genesis_hash,
                NodeRole::Full,
                vec![
                    Capability::TransactionGossip,
                    Capability::FinalizedGossip,
                    Capability::BlockSync,
                    Capability::StateSync,
                    Capability::DomainHistory,
                    Capability::ConsensusDirect,
                ],
            ),
            listen_address,
            request_timeout: Duration::from_secs(10),
            max_concurrent_streams: 32,
            enable_mdns: false,
        }
    }
}

/// Monotonic transport counters suitable for metrics export by `arbor-node`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct NetworkMetrics {
    /// Currently established transport connections.
    pub connected_peers: usize,
    /// Peers that passed the genesis-bound Arbor handshake.
    pub authenticated_peers: usize,
    /// Valid direct requests handed to the application.
    pub inbound_requests: u64,
    /// Valid direct responses handed to the application.
    pub inbound_responses: u64,
    /// Valid gossip messages handed to the application.
    pub gossip_messages: u64,
    /// Malformed, oversized, or identity-mismatched messages rejected.
    pub rejected_messages: u64,
    /// Direct exchanges that timed out or lost their connection.
    pub request_failures: u64,
    /// Peers discovered through mDNS.
    pub discovered_peers: u64,
    /// mDNS advertisement failures after transport startup.
    pub discovery_failures: u64,
}

/// One validated inbound direct request with its one-shot response channel.
pub struct InboundRequest {
    /// Authenticated libp2p peer identity.
    pub peer: PeerId,
    /// Arbor correlation identifier.
    pub request_id: u64,
    /// Validated bounded request.
    pub request: DirectRequest,
    channel: ResponseChannel<ResponseEnvelope>,
}

/// High-level events emitted after transport and handshake validation.
pub enum NetworkEvent {
    /// The OS accepted a listen address.
    Listening(Multiaddr),
    /// A same-network peer was discovered through mDNS and scheduled for dialing.
    PeerDiscovered {
        /// Discovered transport identity.
        peer: PeerId,
        /// Discovered TCP address.
        address: Multiaddr,
    },
    /// The last transport connection to a peer closed.
    PeerDisconnected {
        /// Disconnected transport identity.
        peer: PeerId,
    },
    /// A peer completed the genesis-bound Arbor handshake.
    PeerAuthenticated {
        /// libp2p transport identity.
        peer: PeerId,
        /// Validated remote capabilities and role.
        handshake: Handshake,
    },
    /// A peer was rejected locally for an application identity mismatch.
    PeerRejected {
        /// libp2p transport identity.
        peer: PeerId,
        /// Stable rejection reason.
        reason: String,
    },
    /// Valid signed gossip from an authenticated peer.
    Gossip {
        /// Message source.
        peer: PeerId,
        /// Bounded decoded payload.
        message: GossipMessage,
    },
    /// Valid authenticated direct request awaiting an application response.
    Request(InboundRequest),
    /// Valid correlated direct response.
    Response {
        /// Responding peer.
        peer: PeerId,
        /// Caller-generated request identifier.
        request_id: u64,
        /// Bounded response.
        response: DirectResponse,
    },
    /// An outbound direct exchange failed or timed out.
    RequestFailed {
        /// Target peer.
        peer: PeerId,
        /// Caller-generated identifier, when still tracked.
        request_id: Option<u64>,
        /// Stable diagnostic text.
        reason: String,
    },
}

#[derive(NetworkBehaviour)]
#[behaviour(to_swarm = "BehaviourEvent")]
struct Behaviour {
    identify: identify::Behaviour,
    ping: ping::Behaviour,
    kademlia: kad::Behaviour<MemoryStore>,
    gossipsub: gossipsub::Behaviour,
    direct: request_response::cbor::Behaviour<RequestEnvelope, ResponseEnvelope>,
}

enum BehaviourEvent {
    Identify(Box<identify::Event>),
    Ping(ping::Event),
    Kademlia(Box<kad::Event>),
    Gossipsub(gossipsub::Event),
    Direct(request_response::Event<RequestEnvelope, ResponseEnvelope>),
}

impl From<identify::Event> for BehaviourEvent {
    fn from(value: identify::Event) -> Self {
        Self::Identify(Box::new(value))
    }
}

impl From<ping::Event> for BehaviourEvent {
    fn from(value: ping::Event) -> Self {
        Self::Ping(value)
    }
}

impl From<kad::Event> for BehaviourEvent {
    fn from(value: kad::Event) -> Self {
        Self::Kademlia(Box::new(value))
    }
}

impl From<gossipsub::Event> for BehaviourEvent {
    fn from(value: gossipsub::Event) -> Self {
        Self::Gossipsub(value)
    }
}

impl From<request_response::Event<RequestEnvelope, ResponseEnvelope>> for BehaviourEvent {
    fn from(value: request_response::Event<RequestEnvelope, ResponseEnvelope>) -> Self {
        Self::Direct(value)
    }
}

/// Poll-driven rust-libp2p service.
///
/// The caller must continuously poll [`Self::next_event`]. Expensive execution/storage work
/// belongs behind a bounded application queue so the swarm remains responsive to slow peers.
pub struct NetworkService {
    swarm: Swarm<Behaviour>,
    handshake: Handshake,
    finalized_topic: IdentTopic,
    transaction_topics: BTreeMap<[u8; 32], IdentTopic>,
    authenticated: BTreeMap<PeerId, Handshake>,
    connected: BTreeSet<PeerId>,
    pending: BTreeMap<OutboundRequestId, u64>,
    next_request_id: u64,
    peer_book: PeerBook,
    metrics: NetworkMetrics,
    mdns: Option<crate::mdns::MdnsDiscovery>,
    mdns_tick: tokio::time::Interval,
}

impl NetworkService {
    /// Builds all M7 discovery, gossip, and direct behaviours and starts listening.
    ///
    /// # Errors
    ///
    /// Returns a typed setup error for invalid protocol or listen configuration.
    pub fn new(config: NetworkServiceConfig) -> Result<Self, NetworkError> {
        if config.max_concurrent_streams == 0 {
            return Err(NetworkError::Configuration(
                "max concurrent streams must be non-zero".to_owned(),
            ));
        }
        let peer_id = config.keypair.public().to_peer_id();
        let handshake = config.handshake;
        handshake.validate_against(&handshake)?;
        let behaviour_handshake = handshake.clone();
        let request_timeout = config.request_timeout;
        let max_concurrent_streams = config.max_concurrent_streams;
        let enable_mdns = config.enable_mdns;
        let mut swarm = SwarmBuilder::with_existing_identity(config.keypair)
            .with_tokio()
            .with_tcp(
                tcp::Config::default().nodelay(true),
                noise::Config::new,
                yamux::Config::default,
            )
            .map_err(|error| NetworkError::Configuration(error.to_string()))?
            .with_behaviour(move |keypair| {
                build_behaviour(
                    keypair,
                    peer_id,
                    &behaviour_handshake,
                    request_timeout,
                    max_concurrent_streams,
                )
                .map_err(|error| Box::new(error) as Box<dyn std::error::Error + Send + Sync>)
            })
            .map_err(|error| NetworkError::Configuration(error.to_string()))?
            .with_swarm_config(|config| config.with_idle_connection_timeout(Duration::from_mins(1)))
            .build();
        swarm
            .listen_on(config.listen_address)
            .map_err(|error| NetworkError::Listen(error.to_string()))?;
        let finalized_topic = IdentTopic::new(finalized_topic_name(&handshake));
        swarm
            .behaviour_mut()
            .gossipsub
            .subscribe(&finalized_topic)
            .map_err(|error| NetworkError::Gossip(error.to_string()))?;
        let mdns = enable_mdns
            .then(|| crate::mdns::MdnsDiscovery::new(peer_id, &handshake))
            .transpose()
            .map_err(NetworkError::Discovery)?;
        let mut mdns_tick = tokio::time::interval(Duration::from_millis(250));
        mdns_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        Ok(Self {
            swarm,
            handshake,
            finalized_topic,
            transaction_topics: BTreeMap::new(),
            authenticated: BTreeMap::new(),
            connected: BTreeSet::new(),
            pending: BTreeMap::new(),
            next_request_id: 1,
            peer_book: PeerBook::default(),
            metrics: NetworkMetrics::default(),
            mdns,
            mdns_tick,
        })
    }

    /// Returns the local libp2p peer identity.
    #[must_use]
    pub fn local_peer_id(&self) -> PeerId {
        *self.swarm.local_peer_id()
    }

    /// Returns current monotonic network counters.
    #[must_use]
    pub const fn metrics(&self) -> NetworkMetrics {
        self.metrics
    }

    /// Returns local-only peer policy diagnostics.
    #[must_use]
    pub const fn peer_book(&self) -> &PeerBook {
        &self.peer_book
    }

    /// Returns whether a transport connection is currently established.
    #[must_use]
    pub fn is_connected(&self, peer: &PeerId) -> bool {
        self.connected.contains(peer)
    }

    /// Returns whether a peer passed the Arbor application handshake.
    #[must_use]
    pub fn is_authenticated(&self, peer: &PeerId) -> bool {
        self.authenticated.contains_key(peer)
    }

    /// Dials one explicit peer/address pair.
    ///
    /// # Errors
    ///
    /// Returns a transport error if the dial cannot be scheduled.
    pub fn dial(&mut self, peer: PeerId, address: Multiaddr) -> Result<(), NetworkError> {
        self.swarm
            .behaviour_mut()
            .kademlia
            .add_address(&peer, address.clone());
        self.swarm
            .dial(DialOpts::peer_id(peer).addresses(vec![address]).build())
            .map_err(|error| NetworkError::Dial(error.to_string()))
    }

    /// Starts or refreshes Kademlia bootstrap.
    ///
    /// # Errors
    ///
    /// Returns an error when no bootstrap peer is known.
    pub fn bootstrap(&mut self) -> Result<(), NetworkError> {
        self.swarm
            .behaviour_mut()
            .kademlia
            .bootstrap()
            .map(|_| ())
            .map_err(|error| NetworkError::Dial(error.to_string()))
    }

    /// Subscribes to raw transaction gossip for one domain.
    ///
    /// # Errors
    ///
    /// Returns a gossipsub subscription error.
    pub fn subscribe_domain(
        &mut self,
        domain_id: arbor_primitives::DomainId,
    ) -> Result<(), NetworkError> {
        let raw: [u8; 32] = domain_id.0.into();
        if self.transaction_topics.contains_key(&raw) {
            return Ok(());
        }
        let topic = IdentTopic::new(transaction_topic_name(&self.handshake, raw));
        self.swarm
            .behaviour_mut()
            .gossipsub
            .subscribe(&topic)
            .map_err(|error| NetworkError::Gossip(error.to_string()))?;
        self.transaction_topics.insert(raw, topic);
        Ok(())
    }

    /// Publishes one bounded raw transaction on its domain topic.
    ///
    /// # Errors
    ///
    /// Returns a validation or gossipsub publication error.
    pub fn publish_transaction(
        &mut self,
        domain_id: arbor_primitives::DomainId,
        envelope: Vec<u8>,
    ) -> Result<(), NetworkError> {
        let raw: [u8; 32] = domain_id.0.into();
        let message = GossipMessage::Transaction {
            domain_id: raw,
            envelope,
        };
        message.validate().map_err(NetworkError::Protocol)?;
        let topic = self
            .transaction_topics
            .get(&raw)
            .cloned()
            .ok_or(NetworkError::Protocol(
                "transaction topic is not subscribed locally",
            ))?;
        self.swarm
            .behaviour_mut()
            .gossipsub
            .publish(topic, encode_gossip(&message)?)
            .map(|_| ())
            .map_err(|error| NetworkError::Gossip(error.to_string()))
    }

    /// Publishes a small finalized announcement; peers fetch exact bodies over direct sync.
    ///
    /// # Errors
    ///
    /// Returns a validation or gossipsub publication error.
    pub fn publish_finalized(
        &mut self,
        announcement: FinalizedAnnouncement,
    ) -> Result<(), NetworkError> {
        let message = GossipMessage::Finalized(announcement);
        message.validate().map_err(NetworkError::Protocol)?;
        self.swarm
            .behaviour_mut()
            .gossipsub
            .publish(self.finalized_topic.clone(), encode_gossip(&message)?)
            .map(|_| ())
            .map_err(|error| NetworkError::Gossip(error.to_string()))
    }

    /// Sends one bounded direct request to an authenticated peer.
    ///
    /// # Errors
    ///
    /// Returns an authorization or request-shape error before touching the swarm.
    pub fn request(&mut self, peer: PeerId, request: DirectRequest) -> Result<u64, NetworkError> {
        if !matches!(request, DirectRequest::Handshake) && !self.authenticated.contains_key(&peer) {
            return Err(NetworkError::UnauthenticatedPeer(peer));
        }
        if let Some(capability) = required_capability(&request)
            && !self
                .authenticated
                .get(&peer)
                .is_some_and(|handshake| handshake.supports(capability))
        {
            return Err(NetworkError::UnsupportedCapability { peer, capability });
        }
        if self.peer_book.disposition(&peer) != PeerDisposition::Accepted {
            return Err(NetworkError::PeerPolicy(peer));
        }
        request.validate().map_err(NetworkError::Protocol)?;
        let request_id = self.allocate_request_id();
        let outbound = self.swarm.behaviour_mut().direct.send_request(
            &peer,
            RequestEnvelope {
                request_id,
                handshake: self.handshake.clone(),
                request,
            },
        );
        self.pending.insert(outbound, request_id);
        Ok(request_id)
    }

    /// Sends one validated response through the inbound request's one-shot channel.
    ///
    /// # Errors
    ///
    /// Returns an error for an oversized response or a closed connection.
    pub fn respond(
        &mut self,
        inbound: InboundRequest,
        response: DirectResponse,
    ) -> Result<(), NetworkError> {
        response
            .validate_transport()
            .map_err(NetworkError::Protocol)?;
        self.swarm
            .behaviour_mut()
            .direct
            .send_response(
                inbound.channel,
                ResponseEnvelope {
                    request_id: inbound.request_id,
                    response,
                },
            )
            .map_err(|_| NetworkError::ResponseChannel)
    }

    /// Waits for the next validated high-level event while continuing to drive the swarm.
    pub async fn next_event(&mut self) -> NetworkEvent {
        loop {
            tokio::select! {
                event = self.swarm.select_next_some() => {
                    if let Some(event) = self.handle_swarm_event(event) {
                        return event;
                    }
                }
                _ = self.mdns_tick.tick(), if self.mdns.is_some() => {
                    if let Some(event) = self.poll_mdns() {
                        return event;
                    }
                }
            }
        }
    }

    fn handle_swarm_event(&mut self, event: SwarmEvent<BehaviourEvent>) -> Option<NetworkEvent> {
        match event {
            SwarmEvent::NewListenAddr { address, .. } => {
                if let Some(mdns) = &mut self.mdns
                    && let Err(error) = mdns.register(&address)
                {
                    self.metrics.discovery_failures =
                        self.metrics.discovery_failures.saturating_add(1);
                    tracing::warn!(%error, "mDNS advertisement failed");
                }
                Some(NetworkEvent::Listening(address))
            }
            SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                self.connected.insert(peer_id);
                self.metrics.connected_peers = self.connected.len();
                if self.peer_book.disposition(&peer_id) == PeerDisposition::Banned {
                    let _ = self.swarm.disconnect_peer_id(peer_id);
                    return None;
                }
                let _ = self.request(peer_id, DirectRequest::Handshake);
                None
            }
            SwarmEvent::ConnectionClosed {
                peer_id,
                num_established,
                ..
            } => {
                if num_established > 0 {
                    return None;
                }
                self.connected.remove(&peer_id);
                self.authenticated.remove(&peer_id);
                self.metrics.connected_peers = self.connected.len();
                self.metrics.authenticated_peers = self.authenticated.len();
                Some(NetworkEvent::PeerDisconnected { peer: peer_id })
            }
            SwarmEvent::Behaviour(BehaviourEvent::Direct(event)) => self.handle_direct_event(event),
            SwarmEvent::Behaviour(BehaviourEvent::Gossipsub(gossipsub::Event::Message {
                propagation_source,
                message,
                ..
            })) => self.handle_gossip(propagation_source, &message.data),
            SwarmEvent::Behaviour(BehaviourEvent::Identify(event)) => {
                if let identify::Event::Received { peer_id, info, .. } = *event {
                    for address in info.listen_addrs {
                        self.swarm
                            .behaviour_mut()
                            .kademlia
                            .add_address(&peer_id, address);
                    }
                }
                None
            }
            SwarmEvent::Behaviour(BehaviourEvent::Ping(event)) => {
                if event.result.is_err() {
                    self.peer_book.timeout(event.peer);
                }
                None
            }
            SwarmEvent::Behaviour(BehaviourEvent::Kademlia(event)) => {
                tracing::trace!(?event, "kademlia event");
                None
            }
            SwarmEvent::OutgoingConnectionError {
                peer_id: Some(peer),
                error,
                ..
            } => {
                self.peer_book.timeout(peer);
                self.metrics.request_failures = self.metrics.request_failures.saturating_add(1);
                Some(NetworkEvent::RequestFailed {
                    peer,
                    request_id: None,
                    reason: error.to_string(),
                })
            }
            _ => None,
        }
    }

    fn handle_direct_event(
        &mut self,
        event: request_response::Event<RequestEnvelope, ResponseEnvelope>,
    ) -> Option<NetworkEvent> {
        match event {
            request_response::Event::Message {
                peer,
                message:
                    request_response::Message::Request {
                        request, channel, ..
                    },
                ..
            } => self.handle_inbound_request(peer, request, channel),
            request_response::Event::Message {
                peer,
                message:
                    request_response::Message::Response {
                        request_id,
                        response,
                    },
                ..
            } => self.handle_inbound_response(peer, request_id, response),
            request_response::Event::OutboundFailure {
                peer,
                request_id,
                error,
                ..
            } => {
                let application_id = self.pending.remove(&request_id);
                self.peer_book.timeout(peer);
                self.metrics.request_failures = self.metrics.request_failures.saturating_add(1);
                Some(NetworkEvent::RequestFailed {
                    peer,
                    request_id: application_id,
                    reason: error.to_string(),
                })
            }
            request_response::Event::InboundFailure { peer, .. } => {
                self.peer_book.timeout(peer);
                self.metrics.request_failures = self.metrics.request_failures.saturating_add(1);
                None
            }
            request_response::Event::ResponseSent { peer, .. } => {
                self.peer_book.reward(peer);
                None
            }
        }
    }

    fn handle_inbound_request(
        &mut self,
        peer: PeerId,
        request: RequestEnvelope,
        channel: ResponseChannel<ResponseEnvelope>,
    ) -> Option<NetworkEvent> {
        if let Err(error) = request.validate(&self.handshake) {
            let reason = error.to_string();
            self.reject_peer(peer, &reason);
            return Some(NetworkEvent::PeerRejected { peer, reason });
        }
        if self.peer_book.disposition(&peer) != PeerDisposition::Accepted {
            let _ = self.swarm.behaviour_mut().direct.send_response(
                channel,
                ResponseEnvelope {
                    request_id: request.request_id,
                    response: DirectResponse::Rejected { code: 429 },
                },
            );
            return None;
        }
        let newly_authenticated = self.authenticate(peer, request.handshake.clone());
        if request.request == DirectRequest::Handshake {
            let _ = self.swarm.behaviour_mut().direct.send_response(
                channel,
                ResponseEnvelope {
                    request_id: request.request_id,
                    response: DirectResponse::HandshakeAccepted(self.handshake.clone()),
                },
            );
            return newly_authenticated.then_some(NetworkEvent::PeerAuthenticated {
                peer,
                handshake: request.handshake,
            });
        }
        self.metrics.inbound_requests = self.metrics.inbound_requests.saturating_add(1);
        Some(NetworkEvent::Request(InboundRequest {
            peer,
            request_id: request.request_id,
            request: request.request,
            channel,
        }))
    }

    fn handle_inbound_response(
        &mut self,
        peer: PeerId,
        request_id: OutboundRequestId,
        response: ResponseEnvelope,
    ) -> Option<NetworkEvent> {
        let expected = self.pending.remove(&request_id);
        if expected != Some(response.request_id) {
            self.peer_book.malformed(peer);
            self.metrics.rejected_messages = self.metrics.rejected_messages.saturating_add(1);
            return None;
        }
        if let Err(reason) = response.response.validate_transport() {
            self.peer_book.malformed(peer);
            self.metrics.rejected_messages = self.metrics.rejected_messages.saturating_add(1);
            return Some(NetworkEvent::PeerRejected {
                peer,
                reason: reason.to_owned(),
            });
        }
        if let DirectResponse::HandshakeAccepted(remote) = &response.response {
            if let Err(error) = remote.validate_against(&self.handshake) {
                let reason = error.to_string();
                self.reject_peer(peer, &reason);
                return Some(NetworkEvent::PeerRejected { peer, reason });
            }
            let newly_authenticated = self.authenticate(peer, remote.clone());
            return newly_authenticated.then_some(NetworkEvent::PeerAuthenticated {
                peer,
                handshake: remote.clone(),
            });
        }
        self.peer_book.reward(peer);
        self.metrics.inbound_responses = self.metrics.inbound_responses.saturating_add(1);
        Some(NetworkEvent::Response {
            peer,
            request_id: response.request_id,
            response: response.response,
        })
    }

    fn handle_gossip(&mut self, peer: PeerId, bytes: &[u8]) -> Option<NetworkEvent> {
        if !self.authenticated.contains_key(&peer)
            || self.peer_book.disposition(&peer) != PeerDisposition::Accepted
        {
            return None;
        }
        match decode_gossip(bytes).and_then(|message| {
            message.validate().map_err(NetworkError::Protocol)?;
            Ok(message)
        }) {
            Ok(message) => {
                self.metrics.gossip_messages = self.metrics.gossip_messages.saturating_add(1);
                Some(NetworkEvent::Gossip { peer, message })
            }
            Err(error) => {
                self.peer_book.malformed(peer);
                self.metrics.rejected_messages = self.metrics.rejected_messages.saturating_add(1);
                tracing::debug!(%peer, %error, "rejected gossip");
                None
            }
        }
    }

    fn authenticate(&mut self, peer: PeerId, handshake: Handshake) -> bool {
        let newly_authenticated = self.authenticated.insert(peer, handshake).is_none();
        self.metrics.authenticated_peers = self.authenticated.len();
        self.peer_book.reward(peer);
        newly_authenticated
    }

    fn reject_peer(&mut self, peer: PeerId, reason: &str) {
        self.peer_book.identity_mismatch(peer);
        self.metrics.rejected_messages = self.metrics.rejected_messages.saturating_add(1);
        self.authenticated.remove(&peer);
        self.metrics.authenticated_peers = self.authenticated.len();
        tracing::debug!(%peer, %reason, "rejected peer identity");
        let _ = self.swarm.disconnect_peer_id(peer);
    }

    fn allocate_request_id(&mut self) -> u64 {
        let id = self.next_request_id.max(1);
        self.next_request_id = id.saturating_add(1).max(1);
        id
    }

    fn poll_mdns(&mut self) -> Option<NetworkEvent> {
        let (peer, address) = self.mdns.as_ref()?.try_next()?;
        self.swarm
            .behaviour_mut()
            .kademlia
            .add_address(&peer, address.clone());
        if !self.connected.contains(&peer) {
            let _ = self.dial(peer, address.clone());
        }
        self.metrics.discovered_peers = self.metrics.discovered_peers.saturating_add(1);
        Some(NetworkEvent::PeerDiscovered { peer, address })
    }
}

fn build_behaviour(
    keypair: &identity::Keypair,
    peer_id: PeerId,
    handshake: &Handshake,
    request_timeout: Duration,
    max_concurrent_streams: usize,
) -> Result<Behaviour, NetworkError> {
    let identify = identify::Behaviour::new(
        identify::Config::new(IDENTIFY_PROTOCOL.to_owned(), keypair.public())
            .with_agent_version(format!("arbor/{}", handshake.protocol_version)),
    );
    let ping = ping::Behaviour::new(
        ping::Config::new()
            .with_interval(Duration::from_secs(10))
            .with_timeout(Duration::from_secs(5)),
    );
    let kad_protocol = StreamProtocol::try_from_owned(KADEMLIA_PROTOCOL.to_owned())
        .map_err(|error| NetworkError::Configuration(error.to_string()))?;
    let mut kad_config = kad::Config::new(kad_protocol);
    kad_config.set_query_timeout(request_timeout);
    let mut kademlia = kad::Behaviour::with_config(peer_id, MemoryStore::new(peer_id), kad_config);
    kademlia.set_mode(Some(kad::Mode::Server));
    let gossip_config = gossipsub::ConfigBuilder::default()
        .validation_mode(ValidationMode::Strict)
        .max_transmit_size(MAX_GOSSIP_BYTES)
        .heartbeat_interval(Duration::from_secs(1))
        .message_id_fn(|message| gossipsub::MessageId::from(keccak256(&message.data).to_vec()))
        .build()
        .map_err(|error| NetworkError::Configuration(error.to_string()))?;
    let gossipsub =
        gossipsub::Behaviour::new(MessageAuthenticity::Signed(keypair.clone()), gossip_config)
            .map_err(|error| NetworkError::Configuration(error.to_string()))?;

    let codec =
        request_response::cbor::codec::Codec::<RequestEnvelope, ResponseEnvelope>::default()
            .set_request_size_maximum(MAX_DIRECT_REQUEST_BYTES)
            .set_response_size_maximum(MAX_DIRECT_RESPONSE_BYTES);
    let direct_config = request_response::Config::default()
        .with_request_timeout(request_timeout)
        .with_max_concurrent_streams(max_concurrent_streams);
    let direct_protocol = StreamProtocol::try_from_owned(DIRECT_PROTOCOL.to_owned())
        .map_err(|error| NetworkError::Configuration(error.to_string()))?;
    let direct = request_response::Behaviour::with_codec(
        codec,
        [(direct_protocol, ProtocolSupport::Full)],
        direct_config,
    );
    Ok(Behaviour {
        identify,
        ping,
        kademlia,
        gossipsub,
        direct,
    })
}

fn required_capability(request: &DirectRequest) -> Option<Capability> {
    match request {
        DirectRequest::Handshake | DirectRequest::Status => None,
        DirectRequest::Headers { .. }
        | DirectRequest::BlockBodies { .. }
        | DirectRequest::Blocks { .. } => Some(Capability::BlockSync),
        DirectRequest::SnapshotManifest { .. } | DirectRequest::SnapshotChunk { .. } => {
            Some(Capability::StateSync)
        }
        DirectRequest::DomainHistory { .. } => Some(Capability::DomainHistory),
        DirectRequest::Consensus { .. } => Some(Capability::ConsensusDirect),
    }
}

fn finalized_topic_name(handshake: &Handshake) -> String {
    format!("arbor/{}/finalized/1", hex_hash(handshake.network_id))
}

fn transaction_topic_name(handshake: &Handshake, domain_id: [u8; 32]) -> String {
    format!(
        "arbor/{}/tx/{}/1",
        hex_hash(handshake.network_id),
        hex_hash(domain_id)
    )
}

fn hex_hash(value: [u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(64);
    for byte in value {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

fn encode_gossip(message: &GossipMessage) -> Result<Vec<u8>, NetworkError> {
    message.validate().map_err(NetworkError::Protocol)?;
    match message {
        GossipMessage::Transaction {
            domain_id,
            envelope,
        } => {
            let length = u32::try_from(envelope.len())
                .map_err(|_| NetworkError::Protocol("transaction length overflow"))?;
            let mut output = Vec::with_capacity(38 + envelope.len());
            output.extend_from_slice(&[GOSSIP_VERSION, GOSSIP_TRANSACTION]);
            output.extend_from_slice(domain_id);
            output.extend_from_slice(&length.to_be_bytes());
            output.extend_from_slice(envelope);
            Ok(output)
        }
        GossipMessage::Finalized(announcement) => {
            let mut output = Vec::with_capacity(106);
            output.extend_from_slice(&[GOSSIP_VERSION, GOSSIP_FINALIZED]);
            output.extend_from_slice(&announcement.height.to_be_bytes());
            output.extend_from_slice(&announcement.consensus_hash);
            output.extend_from_slice(&announcement.domain_heads_root);
            Ok(output)
        }
    }
}

fn decode_gossip(input: &[u8]) -> Result<GossipMessage, NetworkError> {
    let prefix = input
        .get(..2)
        .ok_or(NetworkError::Protocol("truncated gossip"))?;
    if prefix[0] != GOSSIP_VERSION {
        return Err(NetworkError::Protocol("unsupported gossip version"));
    }
    match prefix[1] {
        GOSSIP_TRANSACTION => {
            if input.len() < 38 {
                return Err(NetworkError::Protocol("truncated transaction gossip"));
            }
            let domain_id: [u8; 32] = input[2..34]
                .try_into()
                .map_err(|_| NetworkError::Protocol("transaction domain id"))?;
            let length = u32::from_be_bytes(
                input[34..38]
                    .try_into()
                    .map_err(|_| NetworkError::Protocol("transaction length"))?,
            ) as usize;
            if input.len() != 38_usize.saturating_add(length) {
                return Err(NetworkError::Protocol("transaction gossip length mismatch"));
            }
            Ok(GossipMessage::Transaction {
                domain_id,
                envelope: input[38..].to_vec(),
            })
        }
        GOSSIP_FINALIZED => {
            if input.len() != 74 {
                return Err(NetworkError::Protocol("finalized gossip length mismatch"));
            }
            let height = u64::from_be_bytes(
                input[2..10]
                    .try_into()
                    .map_err(|_| NetworkError::Protocol("finalized height"))?,
            );
            let consensus_hash = input[10..42]
                .try_into()
                .map_err(|_| NetworkError::Protocol("finalized hash"))?;
            let domain_heads_root = input[42..74]
                .try_into()
                .map_err(|_| NetworkError::Protocol("domain heads root"))?;
            Ok(GossipMessage::Finalized(FinalizedAnnouncement {
                height,
                consensus_hash,
                domain_heads_root,
            }))
        }
        _ => Err(NetworkError::Protocol("unknown gossip message")),
    }
}

/// rust-libp2p setup, protocol, or exchange error.
#[derive(Debug, Error)]
pub enum NetworkError {
    /// Local behaviour configuration is invalid.
    #[error("invalid network configuration: {0}")]
    Configuration(String),
    /// Listen setup failed.
    #[error("failed to listen: {0}")]
    Listen(String),
    /// Dial or discovery bootstrap failed.
    #[error("failed to dial/bootstrap: {0}")]
    Dial(String),
    /// Gossip subscription/publication failed.
    #[error("gossip failure: {0}")]
    Gossip(String),
    /// Application handshake rejected the peer.
    #[error(transparent)]
    Handshake(#[from] crate::HandshakeError),
    /// Message violated a fixed transport/protocol limit.
    #[error("network protocol rejection: {0}")]
    Protocol(&'static str),
    /// Expensive requests require a completed application handshake.
    #[error("peer {0} has not completed the Arbor handshake")]
    UnauthenticatedPeer(PeerId),
    /// Local peer score currently throttles or bans the peer.
    #[error("peer {0} is blocked by local peer policy")]
    PeerPolicy(PeerId),
    /// Peer did not negotiate the capability required by this request.
    #[error("peer {peer} did not negotiate capability {capability:?}")]
    UnsupportedCapability {
        /// Target peer.
        peer: PeerId,
        /// Required capability.
        capability: Capability,
    },
    /// Safe mDNS adapter could not be initialized.
    #[error("mDNS discovery failure: {0}")]
    Discovery(String),
    /// The inbound connection closed before a response could be sent.
    #[error("direct response channel is closed")]
    ResponseChannel,
}

#[cfg(test)]
mod tests {
    use alloy_primitives::B256;
    use arbor_primitives::NetworkId;

    use super::*;

    #[test]
    fn gossip_codec_is_exact_and_bounded() {
        let message = GossipMessage::Transaction {
            domain_id: [3; 32],
            envelope: vec![2, 0xc0],
        };
        let encoded = encode_gossip(&message).unwrap();
        assert_eq!(decode_gossip(&encoded).unwrap(), message);
        let mut trailing = encoded;
        trailing.push(0);
        assert!(decode_gossip(&trailing).is_err());
    }

    #[tokio::test]
    async fn service_builds_with_all_required_behaviours() {
        let address: Multiaddr = "/ip4/127.0.0.1/tcp/0".parse().unwrap();
        let service = NetworkService::new(NetworkServiceConfig::development(
            NetworkId(B256::repeat_byte(1)),
            B256::repeat_byte(2),
            address,
        ))
        .unwrap();
        assert_ne!(service.local_peer_id(), PeerId::random());
        assert_eq!(service.metrics(), NetworkMetrics::default());
    }

    #[tokio::test]
    async fn two_listeners_authenticate_and_exchange_bounded_status() {
        let network_id = NetworkId(B256::repeat_byte(1));
        let genesis_hash = B256::repeat_byte(2);
        let address: Multiaddr = "/ip4/127.0.0.1/tcp/0".parse().unwrap();
        let mut dialer = NetworkService::new(NetworkServiceConfig::development(
            network_id,
            genesis_hash,
            address.clone(),
        ))
        .unwrap();
        let mut listener = NetworkService::new(NetworkServiceConfig::development(
            network_id,
            genesis_hash,
            address,
        ))
        .unwrap();
        let listener_peer = listener.local_peer_id();
        let listen_address = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let NetworkEvent::Listening(address) = listener.next_event().await {
                    break address;
                }
            }
        })
        .await
        .unwrap();
        dialer.dial(listener_peer, listen_address).unwrap();

        let expected = crate::SyncStatus {
            height: 7,
            consensus_hash: [7; 32],
            domain_heads_root: [8; 32],
            checkpoint_height: 7,
        };
        tokio::time::timeout(Duration::from_secs(10), async {
            let mut requested = false;
            loop {
                tokio::select! {
                    event = dialer.next_event() => {
                        match event {
                            NetworkEvent::PeerAuthenticated { peer, .. }
                                if peer == listener_peer && !requested =>
                            {
                                dialer.request(listener_peer, DirectRequest::Status).unwrap();
                                requested = true;
                            }
                            NetworkEvent::Response {
                                peer,
                                response: DirectResponse::Status(actual),
                                ..
                            } if peer == listener_peer => {
                                assert_eq!(actual, expected);
                                break;
                            }
                            _ => {}
                        }
                    }
                    event = listener.next_event() => {
                        if let NetworkEvent::Request(inbound) = event {
                            assert_eq!(inbound.request, DirectRequest::Status);
                            listener
                                .respond(inbound, DirectResponse::Status(expected))
                                .unwrap();
                        }
                    }
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(dialer.metrics().authenticated_peers, 1);
        assert_eq!(listener.metrics().authenticated_peers, 1);
    }

    #[tokio::test]
    async fn unresponsive_peer_times_out_without_stalling_the_swarm() {
        let network_id = NetworkId(B256::repeat_byte(1));
        let genesis_hash = B256::repeat_byte(2);
        let address: Multiaddr = "/ip4/127.0.0.1/tcp/0".parse().unwrap();
        let mut dialer_config =
            NetworkServiceConfig::development(network_id, genesis_hash, address.clone());
        dialer_config.request_timeout = Duration::from_millis(150);
        let mut dialer = NetworkService::new(dialer_config).unwrap();
        let mut listener = NetworkService::new(NetworkServiceConfig::development(
            network_id,
            genesis_hash,
            address,
        ))
        .unwrap();
        let listener_peer = listener.local_peer_id();
        let listen_address = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let NetworkEvent::Listening(address) = listener.next_event().await {
                    break address;
                }
            }
        })
        .await
        .unwrap();
        dialer.dial(listener_peer, listen_address).unwrap();

        tokio::time::timeout(Duration::from_secs(5), async {
            let mut requested = false;
            let mut held_requests = Vec::new();
            loop {
                tokio::select! {
                    event = dialer.next_event() => match event {
                        NetworkEvent::PeerAuthenticated { peer, .. }
                            if peer == listener_peer && !requested =>
                        {
                            dialer.request(listener_peer, DirectRequest::Status).unwrap();
                            requested = true;
                        }
                        NetworkEvent::RequestFailed { peer, request_id: Some(_), .. }
                            if peer == listener_peer =>
                        {
                            assert_eq!(held_requests.len(), 1);
                            break;
                        }
                        _ => {}
                    },
                    event = listener.next_event() => {
                        if let NetworkEvent::Request(inbound) = event {
                            // Keep the response channel open but deliberately never answer.
                            held_requests.push(inbound);
                        }
                    }
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(dialer.metrics().request_failures, 1);
    }
}
