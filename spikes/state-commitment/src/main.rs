//! Disposable M0 state-commitment experiment.

#![forbid(unsafe_code)]

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    env, fs,
    path::Path,
    time::Instant,
};

use alloy_primitives::{B256, Bytes, keccak256};
use alloy_trie::{
    HashBuilder, Nibbles,
    nodes::BranchNodeCompact,
    proof::{ProofRetainer, verify_proof},
};
use parity_db::{Db, Options};
use tempfile::tempdir;
use thiserror::Error;

type Leaves = BTreeMap<B256, Vec<u8>>;
type Transaction = Vec<(u8, Vec<u8>, Option<Vec<u8>>)>;

const NODE_COLUMN: u8 = 0;
const META_COLUMN: u8 = 1;

#[derive(Debug)]
struct Snapshot {
    root: B256,
    branches: HashMap<Nibbles, BranchNodeCompact>,
    proofs: BTreeMap<Nibbles, Vec<Bytes>>,
}

#[derive(Debug, Default)]
struct WriteStats {
    new_nodes: usize,
    new_node_bytes: u64,
}

fn build_snapshot(leaves: &Leaves, proof_targets: impl IntoIterator<Item = B256>) -> Snapshot {
    let targets: Vec<_> = proof_targets.into_iter().map(Nibbles::unpack).collect();
    let retainer = ProofRetainer::from_iter(targets.iter().cloned());
    let mut builder = HashBuilder::default()
        .with_updates(true)
        .with_proof_retainer(retainer);
    for (key, value) in leaves {
        builder.add_leaf(Nibbles::unpack(*key), value);
    }
    let root = builder.root();
    let proof_nodes = builder.take_proof_nodes();
    let (_builder, branches) = builder.split();
    let branches = branches.into_iter().collect();
    let proofs = targets
        .into_iter()
        .map(|key| {
            let proof = proof_nodes
                .matching_nodes_sorted(&key)
                .into_iter()
                .map(|(_, node)| node)
                .collect();
            (key, proof)
        })
        .collect();
    Snapshot {
        root,
        branches,
        proofs,
    }
}

fn prefixed(prefix: &[u8], suffix: impl AsRef<[u8]>) -> Vec<u8> {
    let mut key = Vec::with_capacity(prefix.len() + suffix.as_ref().len());
    key.extend_from_slice(prefix);
    key.extend_from_slice(suffix.as_ref());
    key
}

fn height_key(prefix: &[u8], height: u64) -> Vec<u8> {
    prefixed(prefix, height.to_be_bytes())
}

fn node_key(hash: B256) -> Vec<u8> {
    hash.to_vec()
}

fn proof_key(height: u64, key: B256) -> Vec<u8> {
    let mut suffix = Vec::with_capacity(40);
    suffix.extend_from_slice(&height.to_be_bytes());
    suffix.extend_from_slice(key.as_slice());
    prefixed(b"proof:", suffix)
}

fn encode_hashes(hashes: &[B256]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(4 + hashes.len() * 32);
    bytes.extend_from_slice(&(hashes.len() as u32).to_be_bytes());
    for hash in hashes {
        bytes.extend_from_slice(hash.as_slice());
    }
    bytes
}

fn decode_hashes(bytes: &[u8]) -> Result<Vec<B256>, SpikeError> {
    let count_bytes: [u8; 4] = bytes
        .get(..4)
        .ok_or(SpikeError::MalformedManifest)?
        .try_into()?;
    let count = u32::from_be_bytes(count_bytes) as usize;
    if bytes.len() != 4 + count * 32 {
        return Err(SpikeError::MalformedManifest);
    }
    bytes[4..]
        .chunks_exact(32)
        .map(|chunk| B256::try_from(chunk).map_err(|_| SpikeError::MalformedManifest))
        .collect()
}

fn encode_branch(path: &Nibbles, node: &BranchNodeCompact) -> Vec<u8> {
    let packed = path.pack();
    let mut bytes = Vec::with_capacity(16 + packed.len() + node.hashes.len() * 32);
    bytes.extend_from_slice(&(path.len() as u16).to_be_bytes());
    bytes.extend_from_slice(&packed);
    bytes.extend_from_slice(&node.state_mask.get().to_be_bytes());
    bytes.extend_from_slice(&node.tree_mask.get().to_be_bytes());
    bytes.extend_from_slice(&node.hash_mask.get().to_be_bytes());
    bytes.extend_from_slice(&(node.hashes.len() as u16).to_be_bytes());
    for hash in node.hashes.iter() {
        bytes.extend_from_slice(hash.as_slice());
    }
    bytes.push(u8::from(node.root_hash.is_some()));
    if let Some(root) = node.root_hash {
        bytes.extend_from_slice(root.as_slice());
    }
    bytes
}

fn add_content_node(
    db: &Db,
    batch: &mut Transaction,
    bytes: &[u8],
    reachable: &mut BTreeSet<B256>,
    stats: &mut WriteStats,
) -> Result<(), SpikeError> {
    let hash = keccak256(bytes);
    reachable.insert(hash);
    if db.get(NODE_COLUMN, &node_key(hash))?.is_none() {
        batch.push((NODE_COLUMN, node_key(hash), Some(bytes.to_vec())));
        stats.new_nodes += 1;
        stats.new_node_bytes += bytes.len() as u64;
    }
    Ok(())
}

fn prepare_snapshot_batch(
    db: &Db,
    height: u64,
    snapshot: &Snapshot,
) -> Result<(Transaction, WriteStats), SpikeError> {
    let mut batch = Transaction::new();
    let mut stats = WriteStats::default();
    let mut reachable = BTreeSet::new();
    for (path, branch) in &snapshot.branches {
        add_content_node(
            db,
            &mut batch,
            &encode_branch(path, branch),
            &mut reachable,
            &mut stats,
        )?;
    }
    for (key, proof) in &snapshot.proofs {
        let mut proof_hashes = Vec::with_capacity(proof.len());
        for node in proof {
            let hash = keccak256(node);
            add_content_node(db, &mut batch, node, &mut reachable, &mut stats)?;
            proof_hashes.push(hash);
        }
        batch.push((
            META_COLUMN,
            proof_key(height, B256::from_slice(&key.pack())),
            Some(encode_hashes(&proof_hashes)),
        ));
    }
    let hashes: Vec<_> = reachable.into_iter().collect();
    batch.push((
        META_COLUMN,
        height_key(b"manifest:", height),
        Some(encode_hashes(&hashes)),
    ));
    batch.push((
        META_COLUMN,
        height_key(b"root:", height),
        Some(snapshot.root.to_vec()),
    ));
    Ok((batch, stats))
}

fn persist_snapshot(db: &Db, height: u64, snapshot: &Snapshot) -> Result<WriteStats, SpikeError> {
    let (batch, stats) = prepare_snapshot_batch(db, height, snapshot)?;
    db.commit(batch)?;
    Ok(stats)
}

fn load_root(db: &Db, height: u64) -> Result<B256, SpikeError> {
    let value = db
        .get(META_COLUMN, &height_key(b"root:", height))?
        .ok_or(SpikeError::MissingRoot(height))?;
    B256::try_from(value.as_slice()).map_err(|_| SpikeError::MalformedRoot(height))
}

fn load_proof(db: &Db, height: u64, key: B256) -> Result<Vec<Bytes>, SpikeError> {
    let manifest = db
        .get(META_COLUMN, &proof_key(height, key))?
        .ok_or(SpikeError::MissingProof(height))?;
    decode_hashes(&manifest)?
        .into_iter()
        .map(|hash| {
            db.get(NODE_COLUMN, &node_key(hash))?
                .map(Bytes::from)
                .ok_or(SpikeError::MissingNode(hash))
        })
        .collect()
}

fn prune_snapshot(db: &Db, height: u64) -> Result<usize, SpikeError> {
    let removed_manifest = db
        .get(META_COLUMN, &height_key(b"manifest:", height))?
        .ok_or(SpikeError::MalformedManifest)
        .and_then(|value| decode_hashes(&value))?;
    let mut batch = Transaction::new();
    batch.push((META_COLUMN, height_key(b"root:", height), None));
    batch.push((META_COLUMN, height_key(b"manifest:", height), None));
    let proof_prefix = prefixed(b"proof:", height.to_be_bytes());
    let mut iterator = db.iter(META_COLUMN)?;
    iterator.seek(&proof_prefix)?;
    while let Some((key, _)) = iterator.next()? {
        if !key.starts_with(&proof_prefix) {
            break;
        }
        batch.push((META_COLUMN, key, None));
    }
    db.commit(batch)?;

    let mut live = BTreeSet::new();
    let mut iterator = db.iter(META_COLUMN)?;
    iterator.seek(b"manifest:")?;
    while let Some((key, value)) = iterator.next()? {
        if !key.starts_with(b"manifest:") {
            break;
        }
        live.extend(decode_hashes(&value)?);
    }
    let mut gc = Transaction::new();
    let mut removed = 0;
    for hash in removed_manifest {
        if !live.contains(&hash) {
            gc.push((NODE_COLUMN, node_key(hash), None));
            removed += 1;
        }
    }
    db.commit(gc)?;
    Ok(removed)
}

fn options(path: &Path) -> Options {
    let mut options = Options::with_columns(path, 2);
    options.columns[NODE_COLUMN as usize].uniform = true;
    options.columns[META_COLUMN as usize].btree_index = true;
    options.sync_wal = true;
    options.sync_data = true;
    options
}

fn open(path: &Path) -> Result<Db, parity_db::Error> {
    Db::open_or_create(&options(path))
}

fn directory_size(path: &Path) -> Result<u64, std::io::Error> {
    let mut total = 0;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        total += if metadata.is_dir() {
            directory_size(&entry.path())?
        } else {
            metadata.len()
        };
    }
    Ok(total)
}

fn verify_semantics() -> Result<(), SpikeError> {
    let directory = tempdir()?;
    let alice = keccak256(b"account:alice");
    let bob = keccak256(b"account:bob");
    let mut leaves = Leaves::from([(alice, b"nonce=0,balance=10".to_vec())]);
    let first = build_snapshot(&leaves, [alice]);
    leaves.insert(bob, b"nonce=0,balance=20".to_vec());
    leaves.insert(alice, b"nonce=1,balance=9".to_vec());
    let second = build_snapshot(&leaves, [alice, bob]);
    assert_ne!(first.root, second.root);

    {
        let db = open(directory.path())?;
        persist_snapshot(&db, 1, &first)?;
        let (uncommitted, _) = prepare_snapshot_batch(&db, 2, &second)?;
        drop(uncommitted); // crash before the atomic transaction reaches parity-db
    }
    {
        let db = open(directory.path())?;
        assert_eq!(load_root(&db, 1)?, first.root);
        assert!(matches!(load_root(&db, 2), Err(SpikeError::MissingRoot(2))));
        persist_snapshot(&db, 2, &second)?;
    }
    let db = open(directory.path())?;
    for (key, value) in &leaves {
        let proof = load_proof(&db, 2, *key)?;
        verify_proof(
            second.root,
            Nibbles::unpack(*key),
            Some(value.clone()),
            &proof,
        )?;
    }
    let removed = prune_snapshot(&db, 1)?;
    assert!(matches!(load_root(&db, 1), Err(SpikeError::MissingRoot(1))));
    assert_eq!(load_root(&db, 2)?, second.root);
    let proof = load_proof(&db, 2, alice)?;
    verify_proof(
        second.root,
        Nibbles::unpack(alice),
        Some(leaves[&alice].clone()),
        &proof,
    )?;
    println!(
        "state semantics passed: historical proof reopen, atomic crash boundary, mark/sweep ({removed} nodes removed)"
    );
    Ok(())
}

fn benchmark() -> Result<(), SpikeError> {
    const ACCOUNTS: u64 = 100_000;
    const UPDATES: u64 = 1_000;
    let directory = tempdir()?;
    let mut leaves = Leaves::new();
    for index in 0..ACCOUNTS {
        leaves.insert(
            keccak256(index.to_be_bytes()),
            index.to_be_bytes().repeat(4),
        );
    }
    let started = Instant::now();
    let first = build_snapshot(&leaves, []);
    let build_first = started.elapsed();
    let db = open(directory.path())?;
    let started = Instant::now();
    let first_stats = persist_snapshot(&db, 1, &first)?;
    let write_first = started.elapsed();

    for index in 0..UPDATES {
        leaves.insert(
            keccak256(index.to_be_bytes()),
            (index + ACCOUNTS).to_be_bytes().repeat(4),
        );
    }
    let started = Instant::now();
    let second = build_snapshot(&leaves, []);
    let build_second = started.elapsed();
    let started = Instant::now();
    let second_stats = persist_snapshot(&db, 2, &second)?;
    let write_second = started.elapsed();
    drop(db);
    let disk_bytes = directory_size(directory.path())?;
    println!(
        "benchmark accounts={ACCOUNTS} updates={UPDATES} root1={} root2={} initial_build_ms={} initial_write_ms={} initial_nodes={} initial_node_bytes={} update_build_ms={} update_write_ms={} incremental_nodes={} incremental_node_bytes={} logical_update_bytes={} write_amplification={:.2} disk_bytes={disk_bytes}",
        first.root,
        second.root,
        build_first.as_millis(),
        write_first.as_millis(),
        first_stats.new_nodes,
        first_stats.new_node_bytes,
        build_second.as_millis(),
        write_second.as_millis(),
        second_stats.new_nodes,
        second_stats.new_node_bytes,
        UPDATES * 32,
        second_stats.new_node_bytes as f64 / (UPDATES * 32) as f64,
    );
    Ok(())
}

fn main() -> Result<(), SpikeError> {
    match env::args().nth(1).as_deref() {
        None | Some("verify") => verify_semantics(),
        Some("benchmark") => benchmark(),
        Some(other) => Err(SpikeError::UnknownCommand(other.to_owned())),
    }
}

#[derive(Debug, Error)]
enum SpikeError {
    #[error(transparent)]
    Database(#[from] parity_db::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Proof(#[from] alloy_trie::proof::ProofVerificationError),
    #[error(transparent)]
    Slice(#[from] std::array::TryFromSliceError),
    #[error("missing root at height {0}")]
    MissingRoot(u64),
    #[error("malformed root at height {0}")]
    MalformedRoot(u64),
    #[error("missing proof at height {0}")]
    MissingProof(u64),
    #[error("missing content-addressed node {0}")]
    MissingNode(B256),
    #[error("malformed node manifest")]
    MalformedManifest,
    #[error("unknown command {0}; expected verify or benchmark")]
    UnknownCommand(String),
}
