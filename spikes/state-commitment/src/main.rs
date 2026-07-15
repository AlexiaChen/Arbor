//! Disposable M0 state-commitment experiment.

#![forbid(unsafe_code)]

use std::{collections::BTreeMap, path::Path};

use alloy_primitives::{B256, Bytes, keccak256};
use alloy_trie::{HashBuilder, Nibbles, proof::ProofRetainer, proof::verify_proof};
use rocksdb::{DB, Options, WriteBatch};
use tempfile::tempdir;
use thiserror::Error;

type Leaves = BTreeMap<B256, Vec<u8>>;

#[derive(Debug)]
struct Snapshot {
    root: B256,
    proofs: BTreeMap<Nibbles, Vec<Bytes>>,
}

fn build_snapshot(leaves: &Leaves) -> Snapshot {
    let targets = leaves.keys().copied().map(Nibbles::unpack);
    let retainer = ProofRetainer::from_iter(targets);
    let mut builder = HashBuilder::default().with_proof_retainer(retainer);
    for (key, value) in leaves {
        builder.add_leaf(Nibbles::unpack(*key), value);
    }
    let root = builder.root();
    let nodes = builder.take_proof_nodes();
    let proofs = leaves
        .keys()
        .copied()
        .map(Nibbles::unpack)
        .map(|key| {
            let proof = nodes
                .matching_nodes_sorted(&key)
                .into_iter()
                .map(|(_, node)| node)
                .collect();
            (key, proof)
        })
        .collect();
    Snapshot { root, proofs }
}

fn snapshot_key(height: u64) -> [u8; 16] {
    let mut key = [0_u8; 16];
    key[..8].copy_from_slice(b"snapshot");
    key[8..].copy_from_slice(&height.to_be_bytes());
    key
}

fn persist_snapshot(db: &DB, height: u64, snapshot: &Snapshot) -> Result<(), SpikeError> {
    let mut batch = WriteBatch::default();
    batch.put(snapshot_key(height), snapshot.root);
    for proof in snapshot.proofs.values() {
        for node in proof {
            let hash = keccak256(node);
            batch.put(hash, node);
        }
    }
    db.write(batch)?;
    db.flush_wal(true)?;
    Ok(())
}

fn load_root(db: &DB, height: u64) -> Result<B256, SpikeError> {
    let value = db.get(snapshot_key(height))?.ok_or(SpikeError::MissingRoot(height))?;
    B256::try_from(value.as_slice()).map_err(|_| SpikeError::MalformedRoot(height))
}

fn open(path: &Path) -> Result<DB, rocksdb::Error> {
    let mut options = Options::default();
    options.create_if_missing(true);
    DB::open(&options, path)
}

fn main() -> Result<(), SpikeError> {
    let directory = tempdir()?;
    let first_key = keccak256(b"account:alice");
    let second_key = keccak256(b"account:bob");
    let mut leaves = Leaves::from([(first_key, b"nonce=0,balance=10".to_vec())]);
    let first = build_snapshot(&leaves);

    leaves.insert(second_key, b"nonce=0,balance=20".to_vec());
    leaves.insert(first_key, b"nonce=1,balance=9".to_vec());
    let second = build_snapshot(&leaves);
    assert_ne!(first.root, second.root);

    for (key, value) in &leaves {
        let nibbles = Nibbles::unpack(*key);
        let proof = second.proofs.get(&nibbles).expect("proof target exists");
        verify_proof(second.root, nibbles, Some(value.clone()), proof)?;
    }

    {
        let db = open(directory.path())?;
        persist_snapshot(&db, 1, &first)?;
        persist_snapshot(&db, 2, &second)?;
    }

    let db = open(directory.path())?;
    assert_eq!(load_root(&db, 1)?, first.root);
    assert_eq!(load_root(&db, 2)?, second.root);

    db.delete(snapshot_key(1))?;
    assert!(db.get(snapshot_key(1))?.is_none());
    assert_eq!(load_root(&db, 2)?, second.root);
    println!("state spike passed: historical roots, proof verification, reopen, root pruning");
    Ok(())
}

#[derive(Debug, Error)]
enum SpikeError {
    #[error(transparent)]
    Database(#[from] rocksdb::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Proof(#[from] alloy_trie::proof::ProofVerificationError),
    #[error("missing root at height {0}")]
    MissingRoot(u64),
    #[error("malformed root at height {0}")]
    MalformedRoot(u64),
}

