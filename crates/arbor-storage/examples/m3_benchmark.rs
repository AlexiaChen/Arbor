//! Reproducible M3 release benchmark for 100,000 secure account leaves.

use std::{collections::BTreeMap, fs, path::Path, time::Instant};

use alloy_primitives::keccak256;
use arbor_primitives::{DomainId, NetworkId};
use arbor_state::EthereumStateCommitment;
use arbor_storage::{
    CommitBatch, Database, DatabaseIdentity, DomainStateCommit, FinalizedMarker, RetentionPolicy,
};

const ACCOUNTS: u64 = 100_000;
const UPDATES: u64 = 1_000;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let identity = DatabaseIdentity {
        network_id: NetworkId(keccak256(b"ARBOR_M3_BENCH_NETWORK")),
        genesis_hash: keccak256(b"ARBOR_M3_BENCH_GENESIS"),
    };
    let domain_id = DomainId(keccak256(b"ARBOR_M3_BENCH_DOMAIN"));
    let mut leaves = BTreeMap::new();
    for index in 0..ACCOUNTS {
        leaves.insert(
            keccak256(index.to_be_bytes()),
            index.to_be_bytes().repeat(4),
        );
    }

    let started = Instant::now();
    let first = EthereumStateCommitment::build(&leaves)?;
    let first_build = started.elapsed();
    let first_root = first.root();
    let db = Database::open(directory.path(), identity, RetentionPolicy::Archive)?;
    let started = Instant::now();
    let first_stats = db.commit(batch(1, domain_id, first))?;
    let first_write = started.elapsed();

    for index in 0..UPDATES {
        leaves.insert(
            keccak256(index.to_be_bytes()),
            (index + ACCOUNTS).to_be_bytes().repeat(4),
        );
    }
    let started = Instant::now();
    let second = EthereumStateCommitment::build(&leaves)?;
    let second_build = started.elapsed();
    let second_root = second.root();
    let started = Instant::now();
    let second_stats = db.commit(batch(2, domain_id, second))?;
    let second_write = started.elapsed();
    drop(db);
    let disk_bytes = directory_size(directory.path())?;
    let logical_update_bytes = UPDATES * 32;
    let amplification_hundredths = second_stats
        .new_node_bytes
        .saturating_mul(100)
        .checked_div(logical_update_bytes)
        .expect("logical update bytes are non-zero");

    println!(
        "accounts={ACCOUNTS} updates={UPDATES} root1={first_root} root2={second_root} \
         initial_build_ms={} initial_write_ms={} initial_nodes={} initial_node_bytes={} \
         update_build_ms={} update_write_ms={} incremental_nodes={} incremental_node_bytes={} \
         logical_update_bytes={logical_update_bytes} write_amplification={}.{:02} disk_bytes={disk_bytes}",
        first_build.as_millis(),
        first_write.as_millis(),
        first_stats.new_nodes,
        first_stats.new_node_bytes,
        second_build.as_millis(),
        second_write.as_millis(),
        second_stats.new_nodes,
        second_stats.new_node_bytes,
        amplification_hundredths / 100,
        amplification_hundredths % 100,
    );
    Ok(())
}

fn batch(height: u64, domain_id: DomainId, snapshot: arbor_state::TrieSnapshot) -> CommitBatch {
    let mut batch = CommitBatch::new(FinalizedMarker {
        height,
        consensus_hash: keccak256(height.to_be_bytes()),
        domain_heads_root: keccak256([height.to_be_bytes().as_slice(), b"heads"].concat()),
    });
    batch.states.push(DomainStateCommit {
        domain_id,
        consensus_height: height,
        snapshot,
    });
    batch
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
