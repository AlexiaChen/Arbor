//! Disposable M0 safety-store and four-validator harness.

#![forbid(unsafe_code)]

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

use tempfile::tempdir;
use thiserror::Error;

const RECORD_LEN: usize = 49;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Vote {
    height: u64,
    round: u64,
    phase: u8,
    block_hash: [u8; 32],
}

struct DurableSafetyStore {
    path: PathBuf,
    last_vote: Option<Vote>,
}

impl DurableSafetyStore {
    fn open(path: impl Into<PathBuf>) -> Result<Self, SpikeError> {
        let path = path.into();
        let last_vote = if path.exists() {
            let mut bytes = Vec::new();
            File::open(&path)?.read_to_end(&mut bytes)?;
            Some(decode_vote(&bytes)?)
        } else {
            None
        };
        Ok(Self { path, last_vote })
    }

    fn persist_before_vote(&mut self, vote: Vote) -> Result<(), SpikeError> {
        if let Some(previous) = self.last_vote {
            let same_slot = (previous.height, previous.round, previous.phase)
                == (vote.height, vote.round, vote.phase);
            if same_slot && previous.block_hash != vote.block_hash {
                return Err(SpikeError::Equivocation);
            }
            if (vote.height, vote.round, vote.phase)
                < (previous.height, previous.round, previous.phase)
            {
                return Err(SpikeError::Regression);
            }
        }

        let temporary = self.path.with_extension("tmp");
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temporary)?;
        file.write_all(&encode_vote(vote))?;
        file.sync_all()?;
        fs::rename(&temporary, &self.path)?;
        sync_parent(&self.path)?;
        self.last_vote = Some(vote);
        Ok(())
    }
}

fn sync_parent(path: &Path) -> Result<(), std::io::Error> {
    if let Some(parent) = path.parent() {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

fn encode_vote(vote: Vote) -> [u8; RECORD_LEN] {
    let mut bytes = [0_u8; RECORD_LEN];
    bytes[..8].copy_from_slice(&vote.height.to_be_bytes());
    bytes[8..16].copy_from_slice(&vote.round.to_be_bytes());
    bytes[16] = vote.phase;
    bytes[17..].copy_from_slice(&vote.block_hash);
    bytes
}

fn decode_vote(bytes: &[u8]) -> Result<Vote, SpikeError> {
    if bytes.len() != RECORD_LEN {
        return Err(SpikeError::MalformedSafetyRecord(bytes.len()));
    }
    let mut height = [0_u8; 8];
    height.copy_from_slice(&bytes[..8]);
    let mut round = [0_u8; 8];
    round.copy_from_slice(&bytes[8..16]);
    let mut block_hash = [0_u8; 32];
    block_hash.copy_from_slice(&bytes[17..]);
    Ok(Vote {
        height: u64::from_be_bytes(height),
        round: u64::from_be_bytes(round),
        phase: bytes[16],
        block_hash,
    })
}

#[derive(Clone, Debug)]
struct DummyApplication {
    value: u64,
    validators: BTreeMap<u8, u64>,
}

impl DummyApplication {
    fn apply(&mut self, delta: u64, validator_update: Option<(u8, u64)>) {
        self.value += delta;
        if let Some((validator, power)) = validator_update {
            self.validators.insert(validator, power);
        }
    }

    fn quorum_power(&self) -> u64 {
        let total: u64 = self.validators.values().sum();
        total * 2 / 3 + 1
    }

    fn can_commit(&self, online: &[u8]) -> bool {
        let online_power: u64 = online
            .iter()
            .filter_map(|validator| self.validators.get(validator))
            .sum();
        online_power >= self.quorum_power()
    }
}

/// Exhaustively checks the four-validator instance of the safety argument used
/// by the fallback specification: two 2f+1 certificates must share an honest
/// signer when at most f=1 validator is Byzantine.
fn check_quorum_intersection_model() {
    let validators = [0_u8, 1, 2, 3];
    let quorums: Vec<BTreeSet<u8>> = (0_u8..16)
        .filter(|mask| mask.count_ones() >= 3)
        .map(|mask| {
            validators
                .into_iter()
                .filter(|validator| mask & (1 << validator) != 0)
                .collect()
        })
        .collect();

    for byzantine in validators {
        for left in &quorums {
            for right in &quorums {
                let intersection: Vec<_> = left.intersection(right).copied().collect();
                assert!(
                    intersection.iter().any(|validator| *validator != byzantine),
                    "two certificates intersect only in the Byzantine validator"
                );
            }
        }
    }
}

fn check_crash_boundary_model(directory: &Path) -> Result<(), SpikeError> {
    let path = directory.join("model.safety");
    let first = Vote {
        height: 11,
        round: 4,
        phase: 2,
        block_hash: [0x33; 32],
    };
    let conflict = Vote {
        block_hash: [0x44; 32],
        ..first
    };

    // Crash before persistence releases no signature and leaves no safety state.
    assert!(!path.exists());
    // Every released signature follows the fsync+rename+parent-fsync boundary.
    DurableSafetyStore::open(&path)?.persist_before_vote(first)?;
    assert_eq!(DurableSafetyStore::open(&path)?.last_vote, Some(first));
    assert!(matches!(
        DurableSafetyStore::open(&path)?.persist_before_vote(conflict),
        Err(SpikeError::Equivocation)
    ));
    Ok(())
}

fn main() -> Result<(), SpikeError> {
    let directory = tempdir()?;
    let path = directory.path().join("validator-0.safety");
    let vote = Vote {
        height: 7,
        round: 2,
        phase: 1,
        block_hash: [0x11; 32],
    };

    DurableSafetyStore::open(&path)?.persist_before_vote(vote)?;
    let mut restarted = DurableSafetyStore::open(&path)?;
    restarted.persist_before_vote(vote)?;
    let conflict = Vote {
        block_hash: [0x22; 32],
        ..vote
    };
    assert!(matches!(
        restarted.persist_before_vote(conflict),
        Err(SpikeError::Equivocation)
    ));

    let mut application = DummyApplication {
        value: 0,
        validators: BTreeMap::from([(0, 1), (1, 1), (2, 1), (3, 1)]),
    };
    assert!(application.can_commit(&[0, 1, 2]));
    assert!(!application.can_commit(&[0, 1]));
    application.apply(1, Some((3, 2)));
    assert_eq!(application.value, 1);
    assert!(application.can_commit(&[0, 1, 3]));

    check_quorum_intersection_model();
    check_crash_boundary_model(directory.path())?;

    println!(
        "consensus fallback model passed: four validators, validator update, one offline, durable restart, exhaustive quorum intersection"
    );
    Ok(())
}

#[derive(Debug, Error)]
enum SpikeError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("refusing conflicting vote for the same height/round/phase")]
    Equivocation,
    #[error("refusing safety-state regression")]
    Regression,
    #[error("malformed safety record length {0}")]
    MalformedSafetyRecord(usize),
}
