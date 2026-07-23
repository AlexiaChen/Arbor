use std::{
    net::{IpAddr, Ipv4Addr},
    str::FromStr,
};

use libp2p::{Multiaddr, PeerId, multiaddr::Protocol};
use mdns_sd::{Receiver, ServiceDaemon, ServiceEvent, ServiceInfo};

use crate::Handshake;

const PEER_PROPERTY: &str = "peer";
const NETWORK_PROPERTY: &str = "network";

pub(crate) struct MdnsDiscovery {
    daemon: ServiceDaemon,
    receiver: Receiver<ServiceEvent>,
    service_type: String,
    network: String,
    peer: PeerId,
    registered: bool,
}

impl MdnsDiscovery {
    pub(crate) fn new(peer: PeerId, handshake: &Handshake) -> Result<Self, String> {
        let network = hex_hash(handshake.network_id);
        let service_type = format!("_arbor-{}._tcp.local.", &network[..8]);
        let daemon = ServiceDaemon::new().map_err(|error| error.to_string())?;
        let receiver = daemon
            .browse(&service_type)
            .map_err(|error| error.to_string())?;
        Ok(Self {
            daemon,
            receiver,
            service_type,
            network,
            peer,
            registered: false,
        })
    }

    pub(crate) fn register(&mut self, address: &Multiaddr) -> Result<(), String> {
        if self.registered {
            return Ok(());
        }
        let Some((ip, port)) = ip4_tcp(address) else {
            return Ok(());
        };
        let peer = self.peer.to_string();
        let hostname = format!("{peer}.local.");
        let properties = [
            (PEER_PROPERTY, peer.as_str()),
            (NETWORK_PROPERTY, self.network.as_str()),
        ];
        let service = ServiceInfo::new(
            &self.service_type,
            &peer,
            &hostname,
            IpAddr::V4(ip),
            port,
            &properties[..],
        )
        .map_err(|error| error.to_string())?;
        self.daemon
            .register(service)
            .map_err(|error| error.to_string())?;
        self.registered = true;
        Ok(())
    }

    pub(crate) fn try_next(&self) -> Option<(PeerId, Multiaddr)> {
        while let Ok(event) = self.receiver.try_recv() {
            let ServiceEvent::ServiceResolved(service) = event else {
                continue;
            };
            if service.get_property_val_str(NETWORK_PROPERTY) != Some(self.network.as_str()) {
                continue;
            }
            let peer = service
                .get_property_val_str(PEER_PROPERTY)
                .and_then(|value| PeerId::from_str(value).ok())?;
            if peer == self.peer {
                continue;
            }
            let ip = service.get_addresses_v4().into_iter().next()?;
            let address = format!("/ip4/{ip}/tcp/{}", service.get_port())
                .parse()
                .ok()?;
            return Some((peer, address));
        }
        None
    }
}

impl Drop for MdnsDiscovery {
    fn drop(&mut self) {
        let _ = self.daemon.shutdown();
    }
}

fn ip4_tcp(address: &Multiaddr) -> Option<(Ipv4Addr, u16)> {
    let mut protocols = address.iter();
    let Protocol::Ip4(ip) = protocols.next()? else {
        return None;
    };
    let Protocol::Tcp(port) = protocols.next()? else {
        return None;
    };
    Some((ip, port))
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
