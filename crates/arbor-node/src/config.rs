//! Versioned node configuration.

use std::{collections::BTreeSet, fs, net::SocketAddr, path::Path, str::FromStr};

use alloy_primitives::B256;
use arbor_primitives::DomainId;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use thiserror::Error;

/// Current on-disk configuration schema.
pub const CONFIG_VERSION: u32 = 1;

/// Full operator configuration.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Configuration schema version.
    pub version: u32,
    /// Node process settings.
    pub node: NodeConfig,
    /// Network listener settings.
    pub network: NetworkConfig,
}

/// Node-local settings that cannot alter protocol validity.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NodeConfig {
    /// Human-readable instance name.
    pub moniker: String,
    /// Enables deterministic development genesis and permits `--dev-validator`.
    #[serde(default)]
    pub dev: bool,
    /// Node-local transaction history: `all`, `root`, or `root,<domain-id>...`.
    #[serde(default)]
    pub domains: HistorySubscription,
}

/// Node-local domain-history selection; it cannot alter proposal execution or validity.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum HistorySubscription {
    /// Retain derived transaction history for every known domain.
    #[default]
    All,
    /// Retain root history plus the explicitly listed child domains.
    RootAnd(BTreeSet<DomainId>),
}

impl HistorySubscription {
    /// Resolves the symbolic `root` entry into a concrete set for the consensus persistence edge.
    #[must_use]
    pub fn selected_domains(&self, root_domain_id: DomainId) -> Option<BTreeSet<DomainId>> {
        match self {
            Self::All => None,
            Self::RootAnd(domains) => {
                let mut domains = domains.clone();
                domains.insert(root_domain_id);
                Some(domains)
            }
        }
    }
}

impl Serialize for HistorySubscription {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let value = match self {
            Self::All => "all".to_owned(),
            Self::RootAnd(domains) => std::iter::once("root".to_owned())
                .chain(domains.iter().map(|domain_id| domain_id.0.to_string()))
                .collect::<Vec<_>>()
                .join(","),
        };
        serializer.serialize_str(&value)
    }
}

impl<'de> Deserialize<'de> for HistorySubscription {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        parse_history_subscription(&value).map_err(de::Error::custom)
    }
}

fn parse_history_subscription(value: &str) -> Result<HistorySubscription, &'static str> {
    if value == "all" {
        return Ok(HistorySubscription::All);
    }
    let mut parts = value.split(',');
    if parts.next() != Some("root") {
        return Err("node.domains must be `all`, `root`, or `root,<domain-id>...`");
    }
    let mut domains = BTreeSet::new();
    for part in parts {
        if part.is_empty() || part == "root" || part == "all" {
            return Err("node.domains contains an invalid entry");
        }
        let hash =
            B256::from_str(part).map_err(|_| "node.domains contains an invalid domain ID")?;
        domains.insert(DomainId(hash));
    }
    Ok(HistorySubscription::RootAnd(domains))
}

/// Local network settings.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkConfig {
    /// P2P listen address. Port zero asks the OS for an ephemeral port.
    pub listen_addr: SocketAddr,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            node: NodeConfig {
                moniker: "arbor-node".to_owned(),
                dev: false,
                domains: HistorySubscription::All,
            },
            network: NetworkConfig {
                listen_addr: "127.0.0.1:0".parse().expect("literal address is valid"),
            },
        }
    }
}

impl Config {
    /// Parses and validates TOML configuration.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] when decoding fails or a value violates the
    /// supported configuration schema.
    pub fn from_toml(input: &str) -> Result<Self, ConfigError> {
        let config: Self = toml::from_str(input)?;
        config.validate()?;
        Ok(config)
    }

    /// Loads and validates configuration from disk.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] when the file cannot be read, decoded, or
    /// validated.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        let input = fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_owned(),
            source,
        })?;
        Self::from_toml(&input)
    }

    /// Serializes configuration using the stable TOML schema.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] if serialization fails.
    pub fn to_toml(&self) -> Result<String, ConfigError> {
        Ok(toml::to_string_pretty(self)?)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        if self.version != CONFIG_VERSION {
            return Err(ConfigError::UnsupportedVersion {
                expected: CONFIG_VERSION,
                actual: self.version,
            });
        }
        if self.node.moniker.trim().is_empty() {
            return Err(ConfigError::EmptyMoniker);
        }
        Ok(())
    }
}

/// Configuration loading and validation failures.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// The file could not be read.
    #[error("failed to read configuration {path}: {source}")]
    Read {
        /// Attempted path.
        path: std::path::PathBuf,
        /// Underlying IO failure.
        source: std::io::Error,
    },
    /// TOML decoding failed.
    #[error("invalid configuration: {0}")]
    Decode(#[from] toml::de::Error),
    /// TOML encoding failed.
    #[error("failed to encode configuration: {0}")]
    Encode(#[from] toml::ser::Error),
    /// The schema version is not understood.
    #[error("unsupported configuration version {actual}; expected {expected}")]
    UnsupportedVersion {
        /// Supported version.
        expected: u32,
        /// Version in the file.
        actual: u32,
    },
    /// Moniker must contain a non-whitespace character.
    #[error("node.moniker must not be empty")]
    EmptyMoniker,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_round_trips() {
        let config = Config::default();
        let encoded = config.to_toml().unwrap();
        assert_eq!(Config::from_toml(&encoded).unwrap(), config);
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let input = Config::default().to_toml().unwrap() + "unknown = true\n";
        assert!(matches!(
            Config::from_toml(&input),
            Err(ConfigError::Decode(_))
        ));
    }

    #[test]
    fn unsupported_version_is_rejected() {
        let input = Config::default()
            .to_toml()
            .unwrap()
            .replace("version = 1", "version = 2");
        assert!(matches!(
            Config::from_toml(&input),
            Err(ConfigError::UnsupportedVersion { actual: 2, .. })
        ));
    }

    #[test]
    fn history_subscription_round_trips_and_rejects_ambiguous_forms() {
        let domain = DomainId(B256::repeat_byte(0x42));
        let mut config = Config::default();
        config.node.domains = HistorySubscription::RootAnd(BTreeSet::from([domain]));
        let encoded = config.to_toml().unwrap();
        assert!(encoded.contains(&format!("domains = \"root,{}\"", domain.0)));
        assert_eq!(Config::from_toml(&encoded).unwrap(), config);
        assert_eq!(
            config
                .node
                .domains
                .selected_domains(DomainId(B256::repeat_byte(0x11)))
                .unwrap()
                .len(),
            2
        );

        for invalid in ["child", "all,root", "root,all", "root,"] {
            let input = Config::default()
                .to_toml()
                .unwrap()
                .replace("domains = \"all\"", &format!("domains = \"{invalid}\""));
            assert!(matches!(
                Config::from_toml(&input),
                Err(ConfigError::Decode(_))
            ));
        }
    }
}
