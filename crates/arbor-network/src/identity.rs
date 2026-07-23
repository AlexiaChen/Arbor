use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    str::FromStr,
};

use libp2p::{Multiaddr, PeerId, identity::Keypair};
use thiserror::Error;

const MAX_PEER_KEY_BYTES: u64 = 4 * 1024;

/// One explicit reconnecting peer in `<peer-id>@<multiaddr>` operator syntax.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PersistentPeer {
    /// Expected libp2p transport identity.
    pub peer_id: PeerId,
    /// Dial address without an embedded `/p2p` suffix.
    pub address: Multiaddr,
}

impl FromStr for PersistentPeer {
    type Err = PersistentPeerError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (peer, address) = value.split_once('@').ok_or(PersistentPeerError::Syntax)?;
        if peer.is_empty() || address.is_empty() || address.contains('@') {
            return Err(PersistentPeerError::Syntax);
        }
        let peer_id = PeerId::from_str(peer).map_err(|_| PersistentPeerError::PeerId)?;
        let address = Multiaddr::from_str(address).map_err(|_| PersistentPeerError::Multiaddr)?;
        if address
            .iter()
            .any(|protocol| matches!(protocol, libp2p::multiaddr::Protocol::P2p(_)))
        {
            return Err(PersistentPeerError::EmbeddedPeerId);
        }
        Ok(Self { peer_id, address })
    }
}

/// Persistent peer configuration failure.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum PersistentPeerError {
    /// Entry is not exactly `<peer-id>@<multiaddr>`.
    #[error("persistent peer must use `<peer-id>@<multiaddr>`")]
    Syntax,
    /// Peer ID is malformed.
    #[error("persistent peer contains an invalid peer ID")]
    PeerId,
    /// Multiaddress is malformed.
    #[error("persistent peer contains an invalid multiaddress")]
    Multiaddr,
    /// Peer identity must not be duplicated inside the address.
    #[error("persistent peer address must not contain a `/p2p` suffix")]
    EmbeddedPeerId,
}

/// Loads a stable libp2p peer identity or creates it exactly once with private permissions.
///
/// The peer key authenticates transport identity only. It must never be reused as an Arbor
/// validator consensus key.
///
/// # Errors
///
/// Returns a typed filesystem or protobuf-decoding failure. A corrupt/truncated existing key is
/// never silently replaced because doing so would unexpectedly change the node's Peer ID.
pub fn load_or_create_peer_identity(path: impl AsRef<Path>) -> Result<Keypair, PeerIdentityError> {
    let path = path.as_ref();
    match load(path) {
        Ok(keypair) => return Ok(keypair),
        Err(PeerIdentityError::Read { source, .. })
            if source.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| PeerIdentityError::CreateDirectory {
            path: parent.to_owned(),
            source,
        })?;
    }
    let keypair = Keypair::generate_ed25519();
    let encoded = keypair
        .to_protobuf_encoding()
        .map_err(|error| PeerIdentityError::Encode(error.to_string()))?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(path) {
        Ok(mut file) => {
            file.write_all(&encoded)
                .and_then(|()| file.sync_all())
                .map_err(|source| PeerIdentityError::Write {
                    path: path.to_owned(),
                    source,
                })?;
            Ok(keypair)
        }
        Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => load(path),
        Err(source) => Err(PeerIdentityError::Write {
            path: path.to_owned(),
            source,
        }),
    }
}

fn load(path: &Path) -> Result<Keypair, PeerIdentityError> {
    let mut file =
        OpenOptions::new()
            .read(true)
            .open(path)
            .map_err(|source| PeerIdentityError::Read {
                path: path.to_owned(),
                source,
            })?;
    let length = file
        .metadata()
        .map_err(|source| PeerIdentityError::Read {
            path: path.to_owned(),
            source,
        })?
        .len();
    if length == 0 || length > MAX_PEER_KEY_BYTES {
        return Err(PeerIdentityError::InvalidLength {
            path: path.to_owned(),
            length,
        });
    }
    let capacity = usize::try_from(length).map_err(|_| PeerIdentityError::InvalidLength {
        path: path.to_owned(),
        length,
    })?;
    let mut bytes = Vec::with_capacity(capacity);
    file.read_to_end(&mut bytes)
        .map_err(|source| PeerIdentityError::Read {
            path: path.to_owned(),
            source,
        })?;
    Keypair::from_protobuf_encoding(&bytes).map_err(|error| PeerIdentityError::Decode {
        path: path.to_owned(),
        reason: error.to_string(),
    })
}

/// Persistent peer-identity failure.
#[derive(Debug, Error)]
pub enum PeerIdentityError {
    /// Parent directory could not be created.
    #[error("failed to create peer identity directory {path}: {source}")]
    CreateDirectory {
        /// Directory path.
        path: PathBuf,
        /// Filesystem failure.
        source: std::io::Error,
    },
    /// Existing identity could not be read.
    #[error("failed to read peer identity {path}: {source}")]
    Read {
        /// Identity path.
        path: PathBuf,
        /// Filesystem failure.
        source: std::io::Error,
    },
    /// New identity could not be durably written.
    #[error("failed to write peer identity {path}: {source}")]
    Write {
        /// Identity path.
        path: PathBuf,
        /// Filesystem failure.
        source: std::io::Error,
    },
    /// Existing file is empty or exceeds the fixed key budget.
    #[error("peer identity {path} has invalid length {length}")]
    InvalidLength {
        /// Identity path.
        path: PathBuf,
        /// Actual length.
        length: u64,
    },
    /// Generated key could not be encoded.
    #[error("failed to encode generated peer identity: {0}")]
    Encode(String),
    /// Existing key bytes are malformed.
    #[error("failed to decode peer identity {path}: {reason}")]
    Decode {
        /// Identity path.
        path: PathBuf,
        /// Decoder diagnostic.
        reason: String,
    },
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn identity_is_stable_and_corruption_is_not_replaced() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("network/peer.key");
        let first = load_or_create_peer_identity(&path).unwrap();
        let second = load_or_create_peer_identity(&path).unwrap();
        assert_eq!(first.public().to_peer_id(), second.public().to_peer_id());
        fs::write(&path, b"bad").unwrap();
        assert!(matches!(
            load_or_create_peer_identity(&path),
            Err(PeerIdentityError::Decode { .. })
        ));
    }
}
