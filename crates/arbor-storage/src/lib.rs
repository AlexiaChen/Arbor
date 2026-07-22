//! Versioned parity-db schema, durable atomic state commits, recovery, and pruning.

#![forbid(unsafe_code)]

use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use alloy_primitives::B256;
use arbor_primitives::{DomainId, NetworkId};
use arbor_state::{EthereumStateCommitment, NodeStore, SnapshotManifest, StateError, TrieSnapshot};
use parity_db::{Db, Options};
use thiserror::Error;

/// Current Arbor application schema stored inside parity-db.
pub const SCHEMA_VERSION: u32 = 1;

const COLUMN_META: u8 = 0;
const COLUMN_TRIE_NODES: u8 = 1;
const COLUMN_FLAT_STATE: u8 = 2;
const COLUMN_CONTRACT_CODE: u8 = 3;
const COLUMN_RECEIPTS: u8 = 4;
const COLUMN_INDEXES: u8 = 5;
const COLUMN_DOMAIN_REGISTRY: u8 = 6;
const COLUMN_SNAPSHOTS: u8 = 7;
const COLUMN_COUNT: u8 = 8;

const KEY_SCHEMA: &[u8] = b"schema-version";
const KEY_NETWORK: &[u8] = b"network-id";
const KEY_GENESIS: &[u8] = b"genesis-hash";
const KEY_MARKER: &[u8] = b"finalized-marker";
const PREFIX_ROOT: &[u8] = b"root:";
const PREFIX_MANIFEST: &[u8] = b"manifest:";
const PREFIX_HEAD: &[u8] = b"head:";

type DbTransaction = Vec<(u8, Vec<u8>, Option<Vec<u8>>)>;

/// An application-schema migration edge.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Migration {
    /// Existing schema version.
    pub from: u32,
    /// Resulting schema version.
    pub to: u32,
}

/// Registry of executable application migrations.
///
/// Schema 1 is the first production schema, so no migration is currently safe or necessary.
pub const MIGRATIONS: &[Migration] = &[];

/// Database identity that prevents opening one network with another network's configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DatabaseIdentity {
    /// Genesis-bound Arbor network identifier.
    pub network_id: NetworkId,
    /// Final genesis block or genesis-spec hash.
    pub genesis_hash: B256,
}

/// Historical-state retention behavior.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetentionPolicy {
    /// Retain every historical state root and all reachable nodes.
    Archive,
    /// Retain the newest `history` finalized roots per domain.
    Full {
        /// Number of finalized roots retained per domain; must be non-zero.
        history: u64,
    },
}

impl RetentionPolicy {
    fn validate(self) -> Result<(), StorageError> {
        if matches!(self, Self::Full { history: 0 }) {
            return Err(StorageError::InvalidRetention);
        }
        Ok(())
    }
}

/// Atomically visible root-consensus commit marker.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FinalizedMarker {
    /// Finalized root-consensus height.
    pub height: u64,
    /// Finalized consensus-block hash.
    pub consensus_hash: B256,
    /// Sparse commitment to all finalized domain heads.
    pub domain_heads_root: B256,
}

/// One domain state included in a finalized commit.
#[derive(Clone, Debug)]
pub struct DomainStateCommit {
    /// Domain whose state advances.
    pub domain_id: DomainId,
    /// Fully materialized authenticated state snapshot.
    pub snapshot: TrieSnapshot,
}

/// Opaque durable value written with the finalized marker.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexedValue {
    /// Stable schema-owned lookup key.
    pub key: Vec<u8>,
    /// Canonically encoded value.
    pub value: Vec<u8>,
}

/// Complete application payload submitted in one parity-db transaction.
#[derive(Clone, Debug)]
pub struct CommitBatch {
    /// New finalized marker.
    pub marker: FinalizedMarker,
    /// Domain roots and trie nodes finalized at this height.
    pub states: Vec<DomainStateCommit>,
    /// Contract code keyed by code hash.
    pub contract_code: BTreeMap<B256, Vec<u8>>,
    /// Receipt records and their schema-owned keys.
    pub receipts: Vec<IndexedValue>,
    /// Transaction/block/log indexes.
    pub indexes: Vec<IndexedValue>,
}

impl CommitBatch {
    /// Creates an otherwise-empty finalized commit.
    #[must_use]
    pub fn new(marker: FinalizedMarker) -> Self {
        Self {
            marker,
            states: Vec::new(),
            contract_code: BTreeMap::new(),
            receipts: Vec::new(),
            indexes: Vec::new(),
        }
    }
}

/// Result of one durable commit.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CommitStats {
    /// Content-addressed trie nodes absent before this commit.
    pub new_nodes: usize,
    /// Bytes in newly inserted trie nodes.
    pub new_node_bytes: u64,
    /// Historical roots pruned by full-node retention.
    pub pruned_roots: usize,
    /// Deferred full-retention failure after the protocol commit was already durable.
    ///
    /// A caller must never retry the finalized commit because this field is populated.
    pub pruning_error: Option<String>,
}

/// Root reachability result reported by database inspection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RootHealth {
    /// Domain owning the finalized head.
    pub domain_id: DomainId,
    /// Finalized state root.
    pub state_root: B256,
    /// Number of secure leaves reached when healthy.
    pub leaves: Option<usize>,
    /// Corruption description when reachability failed.
    pub error: Option<String>,
}

/// Version and finalized-state information returned to operators.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DatabaseInspection {
    /// Arbor application schema version.
    pub schema_version: u32,
    /// Bound database identity.
    pub identity: DatabaseIdentity,
    /// Latest atomically visible finalized marker.
    pub finalized: Option<FinalizedMarker>,
    /// Reachability result for every latest domain root.
    pub roots: Vec<RootHealth>,
}

/// parity-db application schema, commit, or recovery failure.
#[derive(Debug, Error)]
pub enum StorageError {
    /// parity-db rejected an operation or detected physical corruption.
    #[error("parity-db failure: {0}")]
    Database(#[from] parity_db::Error),
    /// Authenticated trie validation failed.
    #[error(transparent)]
    State(#[from] StateError),
    /// Existing application schema is unsupported.
    #[error("unsupported Arbor database schema {actual}; expected {expected}")]
    UnsupportedSchema {
        /// Supported schema.
        expected: u32,
        /// Schema found on disk.
        actual: u32,
    },
    /// Database identity differs from node configuration.
    #[error("database network/genesis identity mismatch")]
    IdentityMismatch,
    /// Required metadata is absent or malformed.
    #[error("corrupt Arbor database metadata: {0}")]
    CorruptMetadata(&'static str),
    /// Finalized heights must strictly increase.
    #[error("non-monotonic finalized height: current {current}, proposed {proposed}")]
    NonMonotonicCommit {
        /// Existing height.
        current: u64,
        /// Proposed height.
        proposed: u64,
    },
    /// One commit contains duplicate domains or duplicate index keys.
    #[error("duplicate key in finalized commit: {0}")]
    DuplicateCommitKey(&'static str),
    /// A zero-history full-node policy would delete the current root.
    #[error("full retention history must be non-zero")]
    InvalidRetention,
    /// The requested historical root does not exist.
    #[error("missing state root for domain {domain_id:?} at height {height}")]
    MissingRoot {
        /// Requested domain.
        domain_id: DomainId,
        /// Requested consensus height.
        height: u64,
    },
    /// Current finalized head cannot be pruned.
    #[error("cannot prune the latest finalized domain head")]
    PruneCurrentHead,
    /// Integer conversion or encoded manifest length failed.
    #[error("storage encoding limit exceeded: {0}")]
    LimitExceeded(&'static str),
}

/// Durable Arbor database backed by parity-db.
pub struct Database {
    db: Db,
    identity: DatabaseIdentity,
    retention: RetentionPolicy,
}

impl Database {
    /// Opens or initializes a database with synchronous WAL and data durability enabled.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] for physical corruption, schema mismatch, or identity mismatch.
    pub fn open(
        path: impl AsRef<Path>,
        identity: DatabaseIdentity,
        retention: RetentionPolicy,
    ) -> Result<Self, StorageError> {
        retention.validate()?;
        let db = Db::open_or_create(&database_options(path.as_ref()))?;
        initialize_or_validate(&db, identity)?;
        Ok(Self {
            db,
            identity,
            retention,
        })
    }

    /// Returns the identity recorded by this database.
    #[must_use]
    pub const fn identity(&self) -> DatabaseIdentity {
        self.identity
    }

    /// Returns the latest finalized marker.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if marker bytes are malformed.
    pub fn finalized_marker(&self) -> Result<Option<FinalizedMarker>, StorageError> {
        self.db
            .get(COLUMN_META, KEY_MARKER)?
            .map(|bytes| decode_marker(&bytes))
            .transpose()
    }

    /// Durably submits nodes, roots, receipts, indexes, heads, and marker atomically.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] before submission when roots are invalid or heights regress, or
    /// propagates a parity-db commit failure without reporting durable completion.
    pub fn commit(&self, batch: CommitBatch) -> Result<CommitStats, StorageError> {
        if let Some(current) = self.finalized_marker()?
            && batch.marker.height <= current.height
        {
            return Err(StorageError::NonMonotonicCommit {
                current: current.height,
                proposed: batch.marker.height,
            });
        }
        validate_unique(&batch)?;
        let mut transaction = DbTransaction::new();
        let mut stats = CommitStats::default();
        let mut pending_node_hashes = BTreeSet::new();

        for state in &batch.states {
            self.append_state_changes(
                state,
                batch.marker.height,
                &mut transaction,
                &mut pending_node_hashes,
                &mut stats,
            )?;
        }

        for (hash, code) in batch.contract_code {
            if alloy_primitives::keccak256(&code) != hash {
                return Err(StorageError::CorruptMetadata("contract code hash"));
            }
            transaction.push((COLUMN_CONTRACT_CODE, hash.to_vec(), Some(code)));
        }
        transaction.extend(
            batch
                .receipts
                .into_iter()
                .map(|record| (COLUMN_RECEIPTS, record.key, Some(record.value))),
        );
        transaction.extend(
            batch
                .indexes
                .into_iter()
                .map(|record| (COLUMN_INDEXES, record.key, Some(record.value))),
        );
        transaction.push((
            COLUMN_META,
            KEY_MARKER.to_vec(),
            Some(encode_marker(batch.marker)),
        ));

        self.db.commit(transaction)?;
        match self.apply_retention(batch.marker.height, &batch.states) {
            Ok(pruned) => stats.pruned_roots = pruned,
            Err(error) => stats.pruning_error = Some(error.to_string()),
        }
        Ok(stats)
    }

    fn append_state_changes(
        &self,
        state: &DomainStateCommit,
        height: u64,
        transaction: &mut DbTransaction,
        pending_node_hashes: &mut BTreeSet<B256>,
        write_stats: &mut CommitStats,
    ) -> Result<(), StorageError> {
        let rebuilt = EthereumStateCommitment::collect_leaves(
            state.snapshot.root(),
            &arbor_state::MemoryNodeStore::new(state.snapshot.nodes().clone()),
        )?;
        if rebuilt != *state.snapshot.leaves() {
            return Err(StorageError::CorruptMetadata(
                "snapshot leaves differ from trie",
            ));
        }
        for (hash, bytes) in state.snapshot.nodes() {
            match self.db.get(COLUMN_TRIE_NODES, hash.as_slice())? {
                Some(existing) if existing != *bytes => {
                    return Err(StorageError::CorruptMetadata("content hash collision"));
                }
                None if pending_node_hashes.insert(*hash) => {
                    transaction.push((COLUMN_TRIE_NODES, hash.to_vec(), Some(bytes.clone())));
                    write_stats.new_nodes += 1;
                    write_stats.new_node_bytes += u64::try_from(bytes.len())
                        .map_err(|_| StorageError::LimitExceeded("trie node bytes"))?;
                }
                Some(_) | None => {}
            }
        }
        let old_leaf_keys = if let Some((old_height, _)) = self.latest_head(state.domain_id)? {
            self.load_manifest(state.domain_id, old_height)?
                .ok_or(StorageError::CorruptMetadata("missing latest manifest"))?
                .leaf_keys
                .into_iter()
                .collect()
        } else {
            BTreeSet::new()
        };
        let new_leaf_keys: BTreeSet<_> = state.snapshot.leaves().keys().copied().collect();
        for key in old_leaf_keys.difference(&new_leaf_keys) {
            transaction.push((COLUMN_FLAT_STATE, flat_key(state.domain_id, *key), None));
        }
        for (key, value) in state.snapshot.leaves() {
            transaction.push((
                COLUMN_FLAT_STATE,
                flat_key(state.domain_id, *key),
                Some(value.clone()),
            ));
        }
        let manifest = ReachabilityManifest {
            node_hashes: state.snapshot.nodes().keys().copied().collect(),
            leaf_keys: new_leaf_keys.into_iter().collect(),
        };
        transaction.extend([
            (
                COLUMN_META,
                root_key(state.domain_id, height),
                Some(state.snapshot.root().to_vec()),
            ),
            (
                COLUMN_META,
                manifest_key(state.domain_id, height),
                Some(manifest.encode()?),
            ),
            (
                COLUMN_META,
                head_key(state.domain_id),
                Some(encode_head(height, state.snapshot.root())),
            ),
        ]);
        Ok(())
    }

    /// Loads a historical domain state root.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] for absence or malformed root bytes.
    pub fn state_root(&self, domain_id: DomainId, height: u64) -> Result<B256, StorageError> {
        let bytes = self
            .db
            .get(COLUMN_META, &root_key(domain_id, height))?
            .ok_or(StorageError::MissingRoot { domain_id, height })?;
        decode_hash(&bytes, "state root")
    }

    /// Builds and independently verifies a historical state proof.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] for a missing root or corrupt/missing node.
    pub fn state_proof(
        &self,
        domain_id: DomainId,
        height: u64,
        secure_key: B256,
    ) -> Result<arbor_state::StateProof, StorageError> {
        Ok(EthereumStateCommitment::proof(
            self.state_root(domain_id, height)?,
            secure_key,
            self,
        )?)
    }

    /// Returns one rebuildable flat-cache value.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if parity-db cannot complete the read.
    pub fn flat_state(
        &self,
        domain_id: DomainId,
        secure_key: B256,
    ) -> Result<Option<Vec<u8>>, StorageError> {
        Ok(self
            .db
            .get(COLUMN_FLAT_STATE, &flat_key(domain_id, secure_key))?)
    }

    /// Loads contract bytecode by its Keccak hash.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if parity-db cannot complete the read or stored bytes are corrupt.
    pub fn contract_code(&self, code_hash: B256) -> Result<Option<Vec<u8>>, StorageError> {
        let code = self.db.get(COLUMN_CONTRACT_CODE, code_hash.as_slice())?;
        if code
            .as_ref()
            .is_some_and(|bytes| alloy_primitives::keccak256(bytes) != code_hash)
        {
            return Err(StorageError::CorruptMetadata("contract code hash"));
        }
        Ok(code)
    }

    /// Loads a canonically encoded receipt by its schema-owned key.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if parity-db cannot complete the read.
    pub fn receipt(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StorageError> {
        Ok(self.db.get(COLUMN_RECEIPTS, key)?)
    }

    /// Loads a transaction/block/log index value by its schema-owned key.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if parity-db cannot complete the read.
    pub fn index(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StorageError> {
        Ok(self.db.get(COLUMN_INDEXES, key)?)
    }

    /// Deletes a domain's flat cache without changing its authenticated root.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if iteration or atomic deletion fails.
    pub fn clear_flat_state(&self, domain_id: DomainId) -> Result<usize, StorageError> {
        let prefix = domain_id.0.as_slice();
        let keys = prefixed_keys(&self.db, COLUMN_FLAT_STATE, prefix)?;
        let count = keys.len();
        self.db
            .commit(keys.into_iter().map(|key| (COLUMN_FLAT_STATE, key, None)))?;
        Ok(count)
    }

    /// Rebuilds a domain flat cache solely by traversing content-addressed trie nodes.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] for a missing root/node, corruption, or database failure.
    pub fn rebuild_flat_state(
        &self,
        domain_id: DomainId,
        root: B256,
    ) -> Result<usize, StorageError> {
        let leaves = EthereumStateCommitment::collect_leaves(root, self)?;
        let count = leaves.len();
        self.db.commit(
            leaves
                .into_iter()
                .map(|(key, value)| (COLUMN_FLAT_STATE, flat_key(domain_id, key), Some(value))),
        )?;
        Ok(count)
    }

    /// Stores a validated snapshot manifest keyed by its domain and height.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] for invalid manifests or database failure.
    pub fn put_snapshot_manifest(&self, manifest: &SnapshotManifest) -> Result<(), StorageError> {
        self.db.commit([(
            COLUMN_SNAPSHOTS,
            root_key(manifest.domain_id, manifest.consensus_height),
            Some(manifest.encode()?),
        )])?;
        Ok(())
    }

    /// Loads a snapshot manifest.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] for corrupt bytes or database failure.
    pub fn snapshot_manifest(
        &self,
        domain_id: DomainId,
        height: u64,
    ) -> Result<Option<SnapshotManifest>, StorageError> {
        self.db
            .get(COLUMN_SNAPSHOTS, &root_key(domain_id, height))?
            .map(|bytes| SnapshotManifest::decode(&bytes).map_err(StorageError::from))
            .transpose()
    }

    /// Atomically replaces the rebuildable `ChainRegistry` query projection.
    ///
    /// The caller must derive `entries` from a verified root-domain state view.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] for duplicate domains or database failure.
    pub fn replace_domain_registry(
        &self,
        entries: impl IntoIterator<Item = (DomainId, Vec<u8>)>,
    ) -> Result<(), StorageError> {
        let existing = all_keys(&self.db, COLUMN_DOMAIN_REGISTRY)?;
        let mut transaction: Vec<_> = existing
            .into_iter()
            .map(|key| (COLUMN_DOMAIN_REGISTRY, key, None))
            .collect();
        let mut seen = BTreeSet::new();
        for (domain_id, value) in entries {
            if !seen.insert(domain_id) {
                return Err(StorageError::DuplicateCommitKey("domain registry"));
            }
            transaction.push((COLUMN_DOMAIN_REGISTRY, domain_id.0.to_vec(), Some(value)));
        }
        self.db.commit(transaction)?;
        Ok(())
    }

    /// Loads one rebuildable `ChainRegistry` projection record.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if parity-db cannot complete the read.
    pub fn domain_registry(&self, domain_id: DomainId) -> Result<Option<Vec<u8>>, StorageError> {
        Ok(self
            .db
            .get(COLUMN_DOMAIN_REGISTRY, domain_id.0.as_slice())?)
    }

    /// Checks application schema, finalized marker, and latest-root reachability.
    ///
    /// Physical parity-db failures still return an error; individual root corruption is reported
    /// inside [`RootHealth`] so operators can see every affected domain in one invocation.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when metadata or iteration cannot be read.
    pub fn inspect(&self) -> Result<DatabaseInspection, StorageError> {
        let schema_version = read_schema(&self.db)?;
        let mut roots = Vec::new();
        let mut iterator = self.db.iter(COLUMN_META)?;
        iterator.seek(PREFIX_HEAD)?;
        while let Some((key, value)) = iterator.next()? {
            if !key.starts_with(PREFIX_HEAD) {
                break;
            }
            let domain_id = decode_domain_suffix(&key, PREFIX_HEAD)?;
            let (_, state_root) = decode_head(&value)?;
            match EthereumStateCommitment::collect_leaves(state_root, self) {
                Ok(leaves) => roots.push(RootHealth {
                    domain_id,
                    state_root,
                    leaves: Some(leaves.len()),
                    error: None,
                }),
                Err(error) => roots.push(RootHealth {
                    domain_id,
                    state_root,
                    leaves: None,
                    error: Some(error.to_string()),
                }),
            }
        }
        Ok(DatabaseInspection {
            schema_version,
            identity: self.identity,
            finalized: self.finalized_marker()?,
            roots,
        })
    }

    /// Prunes one non-current historical root and content nodes unreachable from all manifests.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the root is current/missing, a manifest is corrupt, or the
    /// atomic parity-db transaction fails.
    pub fn prune_root(&self, domain_id: DomainId, height: u64) -> Result<usize, StorageError> {
        if self
            .latest_head(domain_id)?
            .is_some_and(|(head, _)| head == height)
        {
            return Err(StorageError::PruneCurrentHead);
        }
        let removed = self
            .load_manifest(domain_id, height)?
            .ok_or(StorageError::MissingRoot { domain_id, height })?;
        let live = self.live_node_hashes(Some((domain_id, height)))?;
        let mut transaction = vec![
            (COLUMN_META, root_key(domain_id, height), None),
            (COLUMN_META, manifest_key(domain_id, height), None),
        ];
        let mut deleted = 0;
        for hash in removed.node_hashes {
            if !live.contains(&hash) {
                transaction.push((COLUMN_TRIE_NODES, hash.to_vec(), None));
                deleted += 1;
            }
        }
        self.db.commit(transaction)?;
        Ok(deleted)
    }

    fn latest_head(&self, domain_id: DomainId) -> Result<Option<(u64, B256)>, StorageError> {
        self.db
            .get(COLUMN_META, &head_key(domain_id))?
            .map(|bytes| decode_head(&bytes))
            .transpose()
    }

    fn load_manifest(
        &self,
        domain_id: DomainId,
        height: u64,
    ) -> Result<Option<ReachabilityManifest>, StorageError> {
        self.db
            .get(COLUMN_META, &manifest_key(domain_id, height))?
            .map(|bytes| ReachabilityManifest::decode(&bytes))
            .transpose()
    }

    fn live_node_hashes(
        &self,
        excluding: Option<(DomainId, u64)>,
    ) -> Result<BTreeSet<B256>, StorageError> {
        let mut live = BTreeSet::new();
        let mut iterator = self.db.iter(COLUMN_META)?;
        iterator.seek(PREFIX_MANIFEST)?;
        while let Some((key, value)) = iterator.next()? {
            if !key.starts_with(PREFIX_MANIFEST) {
                break;
            }
            if excluding.is_some_and(|pair| key == manifest_key(pair.0, pair.1)) {
                continue;
            }
            live.extend(ReachabilityManifest::decode(&value)?.node_hashes);
        }
        Ok(live)
    }

    fn apply_retention(
        &self,
        height: u64,
        states: &[DomainStateCommit],
    ) -> Result<usize, StorageError> {
        let RetentionPolicy::Full { history } = self.retention else {
            return Ok(0);
        };
        let cutoff = height.saturating_sub(history.saturating_sub(1));
        let mut pruned = 0;
        for state in states {
            for old_height in self.root_heights(state.domain_id)? {
                if old_height < cutoff {
                    self.prune_root(state.domain_id, old_height)?;
                    pruned += 1;
                }
            }
        }
        Ok(pruned)
    }

    fn root_heights(&self, domain_id: DomainId) -> Result<Vec<u64>, StorageError> {
        let prefix = root_prefix(domain_id);
        let mut heights = Vec::new();
        let mut iterator = self.db.iter(COLUMN_META)?;
        iterator.seek(&prefix)?;
        while let Some((key, _)) = iterator.next()? {
            if !key.starts_with(&prefix) {
                break;
            }
            let suffix = key
                .get(prefix.len()..)
                .ok_or(StorageError::CorruptMetadata("root height key"))?;
            let bytes: [u8; 8] = suffix
                .try_into()
                .map_err(|_| StorageError::CorruptMetadata("root height key"))?;
            heights.push(u64::from_be_bytes(bytes));
        }
        Ok(heights)
    }
}

impl NodeStore for Database {
    fn get_node(&self, hash: B256) -> Result<Option<Vec<u8>>, StateError> {
        self.db
            .get(COLUMN_TRIE_NODES, hash.as_slice())
            .map_err(|error| StateError::Store(error.to_string()))
    }
}

fn database_options(path: &Path) -> Options {
    let mut options = Options::with_columns(path, COLUMN_COUNT);
    options.columns[COLUMN_META as usize].btree_index = true;
    options.columns[COLUMN_TRIE_NODES as usize].uniform = true;
    options.columns[COLUMN_FLAT_STATE as usize].btree_index = true;
    options.columns[COLUMN_CONTRACT_CODE as usize].uniform = true;
    options.columns[COLUMN_RECEIPTS as usize].btree_index = true;
    options.columns[COLUMN_INDEXES as usize].btree_index = true;
    options.columns[COLUMN_DOMAIN_REGISTRY as usize].btree_index = true;
    options.columns[COLUMN_SNAPSHOTS as usize].btree_index = true;
    options.sync_wal = true;
    options.sync_data = true;
    options
}

fn initialize_or_validate(db: &Db, identity: DatabaseIdentity) -> Result<(), StorageError> {
    if db.get(COLUMN_META, KEY_SCHEMA)?.is_none() {
        if db.get(COLUMN_META, KEY_NETWORK)?.is_some()
            || db.get(COLUMN_META, KEY_GENESIS)?.is_some()
            || db.get(COLUMN_META, KEY_MARKER)?.is_some()
        {
            return Err(StorageError::CorruptMetadata("partial schema identity"));
        }
        db.commit([
            (
                COLUMN_META,
                KEY_SCHEMA.to_vec(),
                Some(SCHEMA_VERSION.to_be_bytes().to_vec()),
            ),
            (
                COLUMN_META,
                KEY_NETWORK.to_vec(),
                Some(identity.network_id.0.to_vec()),
            ),
            (
                COLUMN_META,
                KEY_GENESIS.to_vec(),
                Some(identity.genesis_hash.to_vec()),
            ),
        ])?;
    } else {
        let actual = read_schema(db)?;
        if actual != SCHEMA_VERSION {
            return Err(StorageError::UnsupportedSchema {
                expected: SCHEMA_VERSION,
                actual,
            });
        }
        let network = db
            .get(COLUMN_META, KEY_NETWORK)?
            .ok_or(StorageError::CorruptMetadata("missing network id"))?;
        let genesis = db
            .get(COLUMN_META, KEY_GENESIS)?
            .ok_or(StorageError::CorruptMetadata("missing genesis hash"))?;
        if decode_hash(&network, "network id")? != identity.network_id.0
            || decode_hash(&genesis, "genesis hash")? != identity.genesis_hash
        {
            return Err(StorageError::IdentityMismatch);
        }
    }
    Ok(())
}

fn read_schema(db: &Db) -> Result<u32, StorageError> {
    let bytes = db
        .get(COLUMN_META, KEY_SCHEMA)?
        .ok_or(StorageError::CorruptMetadata("missing schema version"))?;
    let bytes: [u8; 4] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| StorageError::CorruptMetadata("schema version"))?;
    Ok(u32::from_be_bytes(bytes))
}

fn validate_unique(batch: &CommitBatch) -> Result<(), StorageError> {
    let mut domains = BTreeSet::new();
    if batch
        .states
        .iter()
        .any(|state| !domains.insert(state.domain_id))
    {
        return Err(StorageError::DuplicateCommitKey("domain state"));
    }
    for (name, values) in [("receipt", &batch.receipts), ("index", &batch.indexes)] {
        let mut keys = BTreeSet::new();
        if values.iter().any(|entry| !keys.insert(&entry.key)) {
            return Err(StorageError::DuplicateCommitKey(name));
        }
    }
    Ok(())
}

#[derive(Debug)]
struct ReachabilityManifest {
    node_hashes: Vec<B256>,
    leaf_keys: Vec<B256>,
}

impl ReachabilityManifest {
    fn encode(&self) -> Result<Vec<u8>, StorageError> {
        let node_count = u32::try_from(self.node_hashes.len())
            .map_err(|_| StorageError::LimitExceeded("manifest nodes"))?;
        let leaf_count = u32::try_from(self.leaf_keys.len())
            .map_err(|_| StorageError::LimitExceeded("manifest leaves"))?;
        let mut out = Vec::with_capacity(8 + (self.node_hashes.len() + self.leaf_keys.len()) * 32);
        out.extend_from_slice(&node_count.to_be_bytes());
        for hash in &self.node_hashes {
            out.extend_from_slice(hash.as_slice());
        }
        out.extend_from_slice(&leaf_count.to_be_bytes());
        for key in &self.leaf_keys {
            out.extend_from_slice(key.as_slice());
        }
        Ok(out)
    }

    fn decode(bytes: &[u8]) -> Result<Self, StorageError> {
        let mut cursor = 0;
        let node_count = take_u32(bytes, &mut cursor)? as usize;
        let node_bytes = node_count
            .checked_mul(32)
            .ok_or(StorageError::LimitExceeded("manifest nodes"))?;
        if bytes.len().saturating_sub(cursor) < node_bytes + 4 {
            return Err(StorageError::CorruptMetadata("truncated node manifest"));
        }
        let mut node_hashes = Vec::with_capacity(node_count);
        for _ in 0..node_count {
            node_hashes.push(take_hash(bytes, &mut cursor)?);
        }
        let leaf_count = take_u32(bytes, &mut cursor)? as usize;
        let expected = cursor
            .checked_add(leaf_count * 32)
            .ok_or(StorageError::LimitExceeded("manifest leaves"))?;
        if expected != bytes.len() {
            return Err(StorageError::CorruptMetadata("manifest length"));
        }
        let mut leaf_keys = Vec::with_capacity(leaf_count);
        for _ in 0..leaf_count {
            leaf_keys.push(take_hash(bytes, &mut cursor)?);
        }
        if !strictly_sorted(&node_hashes) || !strictly_sorted(&leaf_keys) {
            return Err(StorageError::CorruptMetadata(
                "non-canonical manifest order",
            ));
        }
        Ok(Self {
            node_hashes,
            leaf_keys,
        })
    }
}

fn strictly_sorted(values: &[B256]) -> bool {
    values.windows(2).all(|window| window[0] < window[1])
}

fn encode_marker(marker: FinalizedMarker) -> Vec<u8> {
    let mut out = Vec::with_capacity(72);
    out.extend_from_slice(&marker.height.to_be_bytes());
    out.extend_from_slice(marker.consensus_hash.as_slice());
    out.extend_from_slice(marker.domain_heads_root.as_slice());
    out
}

fn decode_marker(bytes: &[u8]) -> Result<FinalizedMarker, StorageError> {
    if bytes.len() != 72 {
        return Err(StorageError::CorruptMetadata("finalized marker"));
    }
    let mut cursor = 0;
    Ok(FinalizedMarker {
        height: take_u64(bytes, &mut cursor)?,
        consensus_hash: take_hash(bytes, &mut cursor)?,
        domain_heads_root: take_hash(bytes, &mut cursor)?,
    })
}

fn encode_head(height: u64, root: B256) -> Vec<u8> {
    let mut out = Vec::with_capacity(40);
    out.extend_from_slice(&height.to_be_bytes());
    out.extend_from_slice(root.as_slice());
    out
}

fn decode_head(bytes: &[u8]) -> Result<(u64, B256), StorageError> {
    if bytes.len() != 40 {
        return Err(StorageError::CorruptMetadata("domain head"));
    }
    let mut cursor = 0;
    Ok((
        take_u64(bytes, &mut cursor)?,
        take_hash(bytes, &mut cursor)?,
    ))
}

fn root_prefix(domain_id: DomainId) -> Vec<u8> {
    prefixed(PREFIX_ROOT, domain_id.0.as_slice())
}

fn root_key(domain_id: DomainId, height: u64) -> Vec<u8> {
    let mut key = root_prefix(domain_id);
    key.extend_from_slice(&height.to_be_bytes());
    key
}

fn manifest_key(domain_id: DomainId, height: u64) -> Vec<u8> {
    let mut key = prefixed(PREFIX_MANIFEST, domain_id.0.as_slice());
    key.extend_from_slice(&height.to_be_bytes());
    key
}

fn head_key(domain_id: DomainId) -> Vec<u8> {
    prefixed(PREFIX_HEAD, domain_id.0.as_slice())
}

fn flat_key(domain_id: DomainId, secure_key: B256) -> Vec<u8> {
    let mut key = Vec::with_capacity(64);
    key.extend_from_slice(domain_id.0.as_slice());
    key.extend_from_slice(secure_key.as_slice());
    key
}

fn prefixed(prefix: &[u8], suffix: &[u8]) -> Vec<u8> {
    let mut key = Vec::with_capacity(prefix.len() + suffix.len());
    key.extend_from_slice(prefix);
    key.extend_from_slice(suffix);
    key
}

fn decode_domain_suffix(key: &[u8], prefix: &[u8]) -> Result<DomainId, StorageError> {
    let suffix = key
        .strip_prefix(prefix)
        .ok_or(StorageError::CorruptMetadata("domain key prefix"))?;
    Ok(DomainId(decode_hash(suffix, "domain key")?))
}

fn decode_hash(bytes: &[u8], name: &'static str) -> Result<B256, StorageError> {
    B256::try_from(bytes).map_err(|_| StorageError::CorruptMetadata(name))
}

fn take_u32(bytes: &[u8], cursor: &mut usize) -> Result<u32, StorageError> {
    let value: [u8; 4] = bytes
        .get(*cursor..*cursor + 4)
        .ok_or(StorageError::CorruptMetadata("manifest u32"))?
        .try_into()
        .map_err(|_| StorageError::CorruptMetadata("manifest u32"))?;
    *cursor += 4;
    Ok(u32::from_be_bytes(value))
}

fn take_u64(bytes: &[u8], cursor: &mut usize) -> Result<u64, StorageError> {
    let value: [u8; 8] = bytes
        .get(*cursor..*cursor + 8)
        .ok_or(StorageError::CorruptMetadata("metadata u64"))?
        .try_into()
        .map_err(|_| StorageError::CorruptMetadata("metadata u64"))?;
    *cursor += 8;
    Ok(u64::from_be_bytes(value))
}

fn take_hash(bytes: &[u8], cursor: &mut usize) -> Result<B256, StorageError> {
    let hash = decode_hash(
        bytes
            .get(*cursor..*cursor + 32)
            .ok_or(StorageError::CorruptMetadata("metadata hash"))?,
        "metadata hash",
    )?;
    *cursor += 32;
    Ok(hash)
}

fn prefixed_keys(db: &Db, column: u8, prefix: &[u8]) -> Result<Vec<Vec<u8>>, StorageError> {
    let mut keys = Vec::new();
    let mut iterator = db.iter(column)?;
    iterator.seek(prefix)?;
    while let Some((key, _)) = iterator.next()? {
        if !key.starts_with(prefix) {
            break;
        }
        keys.push(key);
    }
    Ok(keys)
}

fn all_keys(db: &Db, column: u8) -> Result<Vec<Vec<u8>>, StorageError> {
    let mut keys = Vec::new();
    let mut iterator = db.iter(column)?;
    iterator.seek(&[])?;
    while let Some((key, _)) = iterator.next()? {
        keys.push(key);
    }
    Ok(keys)
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        fs,
        process::Command,
        thread,
        time::{Duration, Instant},
    };

    use alloy_primitives::keccak256;
    use arbor_state::EthereumStateCommitment;
    use tempfile::tempdir;

    use super::*;

    fn identity() -> DatabaseIdentity {
        DatabaseIdentity {
            network_id: NetworkId(keccak256(b"network")),
            genesis_hash: keccak256(b"genesis"),
        }
    }

    fn domain() -> DomainId {
        DomainId(keccak256(b"root-domain"))
    }

    fn snapshot(values: &[(u8, u8)]) -> TrieSnapshot {
        let leaves = values
            .iter()
            .map(|(key, value)| (keccak256([*key]), vec![*value]))
            .collect::<BTreeMap<_, _>>();
        EthereumStateCommitment::build(&leaves).unwrap()
    }

    fn large_snapshot(seed: u64) -> TrieSnapshot {
        let leaves = (0_u64..5_000)
            .map(|index| {
                (
                    keccak256(index.to_be_bytes()),
                    (index.wrapping_add(seed)).to_be_bytes().repeat(8),
                )
            })
            .collect::<BTreeMap<_, _>>();
        EthereumStateCommitment::build(&leaves).unwrap()
    }

    fn marker(height: u64) -> FinalizedMarker {
        FinalizedMarker {
            height,
            consensus_hash: keccak256(height.to_be_bytes()),
            domain_heads_root: keccak256(height.to_be_bytes()),
        }
    }

    fn commit(height: u64, state: TrieSnapshot) -> CommitBatch {
        let mut batch = CommitBatch::new(marker(height));
        batch.states.push(DomainStateCommit {
            domain_id: domain(),
            snapshot: state,
        });
        batch
    }

    #[test]
    fn commit_reopen_historical_proof_and_flat_rebuild() {
        let dir = tempdir().unwrap();
        let first = snapshot(&[(1, 10), (2, 20)]);
        let second = snapshot(&[(1, 11), (3, 30)]);
        let key = keccak256([1]);
        {
            let db = Database::open(dir.path(), identity(), RetentionPolicy::Archive).unwrap();
            db.commit(commit(1, first.clone())).unwrap();
            db.commit(commit(2, second.clone())).unwrap();
            assert_eq!(db.finalized_marker().unwrap(), Some(marker(2)));
        }
        let db = Database::open(dir.path(), identity(), RetentionPolicy::Archive).unwrap();
        assert_eq!(db.state_root(domain(), 1).unwrap(), first.root());
        assert_eq!(db.state_root(domain(), 2).unwrap(), second.root());
        db.state_proof(domain(), 1, key).unwrap().verify().unwrap();
        db.state_proof(domain(), 2, key).unwrap().verify().unwrap();
        assert_eq!(db.clear_flat_state(domain()).unwrap(), 2);
        assert_eq!(db.flat_state(domain(), key).unwrap(), None);
        assert_eq!(db.rebuild_flat_state(domain(), second.root()).unwrap(), 2);
        assert_eq!(db.flat_state(domain(), key).unwrap(), Some(vec![11]));
        assert!(db.inspect().unwrap().roots[0].error.is_none());
    }

    #[test]
    fn schema_identity_and_monotonic_marker_are_enforced() {
        let dir = tempdir().unwrap();
        let db = Database::open(dir.path(), identity(), RetentionPolicy::Archive).unwrap();
        db.commit(commit(1, snapshot(&[(1, 1)]))).unwrap();
        assert!(matches!(
            db.commit(commit(1, snapshot(&[(1, 2)]))),
            Err(StorageError::NonMonotonicCommit { .. })
        ));
        drop(db);
        assert!(matches!(
            Database::open(
                dir.path(),
                DatabaseIdentity {
                    network_id: NetworkId(B256::ZERO),
                    genesis_hash: identity().genesis_hash,
                },
                RetentionPolicy::Archive
            ),
            Err(StorageError::IdentityMismatch)
        ));
    }

    #[test]
    fn full_retention_prunes_only_unreachable_historical_nodes() {
        let dir = tempdir().unwrap();
        let db =
            Database::open(dir.path(), identity(), RetentionPolicy::Full { history: 2 }).unwrap();
        let first = snapshot(&[(1, 1), (2, 2)]);
        let second = snapshot(&[(1, 3), (2, 2)]);
        let third = snapshot(&[(1, 4), (2, 2)]);
        db.commit(commit(1, first)).unwrap();
        db.commit(commit(2, second.clone())).unwrap();
        let stats = db.commit(commit(3, third.clone())).unwrap();
        assert_eq!(stats.pruned_roots, 1);
        assert!(matches!(
            db.state_root(domain(), 1),
            Err(StorageError::MissingRoot { .. })
        ));
        db.state_proof(domain(), 2, keccak256([2]))
            .unwrap()
            .verify()
            .unwrap();
        db.state_proof(domain(), 3, keccak256([1]))
            .unwrap()
            .verify()
            .unwrap();
    }

    #[test]
    fn missing_and_corrupt_nodes_are_reported_without_panicking() {
        let dir = tempdir().unwrap();
        let state = snapshot(&[(1, 1), (2, 2), (3, 3)]);
        let root = state.root();
        let db = Database::open(dir.path(), identity(), RetentionPolicy::Archive).unwrap();
        db.commit(commit(1, state)).unwrap();
        db.db
            .commit([(COLUMN_TRIE_NODES, root.to_vec(), None)])
            .unwrap();
        let inspection = db.inspect().unwrap();
        assert!(inspection.roots[0].error.is_some());
        assert!(matches!(
            db.state_proof(domain(), 1, keccak256([1])),
            Err(StorageError::State(StateError::MissingNode(_)))
        ));
    }

    #[test]
    fn missing_latest_manifest_blocks_next_commit() {
        let dir = tempdir().unwrap();
        let db = Database::open(dir.path(), identity(), RetentionPolicy::Archive).unwrap();
        db.commit(commit(1, snapshot(&[(1, 1)]))).unwrap();
        db.db
            .commit([(COLUMN_META, manifest_key(domain(), 1), None)])
            .unwrap();
        assert!(matches!(
            db.commit(commit(2, snapshot(&[(1, 2)]))),
            Err(StorageError::CorruptMetadata("missing latest manifest"))
        ));
        assert_eq!(db.finalized_marker().unwrap(), Some(marker(1)));
    }

    #[test]
    fn snapshot_manifest_roundtrips_through_database() {
        let dir = tempdir().unwrap();
        let db = Database::open(dir.path(), identity(), RetentionPolicy::Archive).unwrap();
        let manifest = SnapshotManifest {
            consensus_height: 7,
            domain_id: domain(),
            state_root: keccak256(b"state"),
            chunks: vec![arbor_state::SnapshotChunk {
                index: 0,
                bytes: 3,
                hash: keccak256(b"abc"),
            }],
        };
        db.put_snapshot_manifest(&manifest).unwrap();
        assert_eq!(db.snapshot_manifest(domain(), 7).unwrap(), Some(manifest));
    }

    #[test]
    fn code_receipt_indexes_and_registry_projection_roundtrip() {
        let dir = tempdir().unwrap();
        let db = Database::open(dir.path(), identity(), RetentionPolicy::Archive).unwrap();
        let code = b"contract bytecode".to_vec();
        let code_hash = keccak256(&code);
        let mut batch = commit(1, snapshot(&[(1, 1)]));
        batch.contract_code.insert(code_hash, code.clone());
        batch.receipts.push(IndexedValue {
            key: b"receipt:1".to_vec(),
            value: b"receipt bytes".to_vec(),
        });
        batch.indexes.push(IndexedValue {
            key: b"tx:1".to_vec(),
            value: b"position".to_vec(),
        });
        db.commit(batch).unwrap();
        assert_eq!(db.contract_code(code_hash).unwrap(), Some(code));
        assert_eq!(
            db.receipt(b"receipt:1").unwrap(),
            Some(b"receipt bytes".to_vec())
        );
        assert_eq!(db.index(b"tx:1").unwrap(), Some(b"position".to_vec()));

        db.replace_domain_registry([(domain(), b"descriptor".to_vec())])
            .unwrap();
        assert_eq!(
            db.domain_registry(domain()).unwrap(),
            Some(b"descriptor".to_vec())
        );
        db.replace_domain_registry([]).unwrap();
        assert_eq!(db.domain_registry(domain()).unwrap(), None);
    }

    #[test]
    fn schema_mismatch_is_rejected_without_implicit_migration() {
        let dir = tempdir().unwrap();
        let db = Database::open(dir.path(), identity(), RetentionPolicy::Archive).unwrap();
        db.db
            .commit([(
                COLUMN_META,
                KEY_SCHEMA.to_vec(),
                Some(99_u32.to_be_bytes().to_vec()),
            )])
            .unwrap();
        drop(db);
        assert!(matches!(
            Database::open(dir.path(), identity(), RetentionPolicy::Archive),
            Err(StorageError::UnsupportedSchema { actual: 99, .. })
        ));
    }

    #[test]
    fn corrupt_uncommitted_log_cannot_change_application_state() {
        let dir = tempdir().unwrap();
        let db = Database::open(dir.path(), identity(), RetentionPolicy::Archive).unwrap();
        drop(db);
        fs::write(dir.path().join("log99"), b"not-a-valid-parity-db-log").unwrap();
        match Database::open(dir.path(), identity(), RetentionPolicy::Archive) {
            Ok(db) => {
                assert_eq!(db.finalized_marker().unwrap(), None);
                assert!(db.inspect().unwrap().roots.is_empty());
            }
            Err(StorageError::Database(_)) => {}
            Err(other) => panic!("unexpected corrupt-log result: {other}"),
        }
    }

    #[test]
    fn subprocess_kill_exposes_only_old_or_complete_commit() {
        if std::env::var_os("ARBOR_STORAGE_CRASH_WORKER").is_some() {
            return;
        }
        for delay_ms in [0_u64, 1, 3, 10] {
            let dir = tempdir().unwrap();
            let first = large_snapshot(1);
            let second = large_snapshot(2);
            {
                let db = Database::open(dir.path(), identity(), RetentionPolicy::Archive).unwrap();
                db.commit(commit(1, first.clone())).unwrap();
            }
            let ready = dir.path().join("commit-ready");
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "tests::crash_commit_worker", "--nocapture"])
                .env("ARBOR_STORAGE_CRASH_WORKER", "1")
                .env("ARBOR_STORAGE_CRASH_DB", dir.path())
                .env("ARBOR_STORAGE_CRASH_READY", &ready)
                .spawn()
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(15);
            while !ready.exists() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(2));
            }
            assert!(ready.exists(), "crash worker did not reach commit boundary");
            thread::sleep(Duration::from_millis(delay_ms));
            let _ = child.kill();
            let _ = child.wait();

            let db = Database::open(dir.path(), identity(), RetentionPolicy::Archive).unwrap();
            let marker = db.finalized_marker().unwrap().unwrap();
            match marker.height {
                1 => {
                    assert_eq!(db.state_root(domain(), 1).unwrap(), first.root());
                    assert!(matches!(
                        db.state_root(domain(), 2),
                        Err(StorageError::MissingRoot { .. })
                    ));
                }
                2 => {
                    assert_eq!(db.state_root(domain(), 2).unwrap(), second.root());
                    assert!(db.inspect().unwrap().roots[0].error.is_none());
                }
                other => panic!("unexpected partial marker height {other}"),
            }
        }
    }

    #[test]
    fn crash_commit_worker() {
        if std::env::var_os("ARBOR_STORAGE_CRASH_WORKER").is_none() {
            return;
        }
        let path = std::env::var_os("ARBOR_STORAGE_CRASH_DB").unwrap();
        let ready = std::env::var_os("ARBOR_STORAGE_CRASH_READY").unwrap();
        let second = large_snapshot(2);
        let db = Database::open(path, identity(), RetentionPolicy::Archive).unwrap();
        fs::write(ready, b"ready").unwrap();
        db.commit(commit(2, second)).unwrap();
    }
}
