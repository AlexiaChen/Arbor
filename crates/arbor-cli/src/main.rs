//! Arbor command-line entry point.

#![forbid(unsafe_code)]

use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use alloy_primitives::{Address, Bytes, U256, address, keccak256};
use arbor_codec::{encode_eip1559, encode_eip1559_signing_payload};
use arbor_crypto::{derive_domain_id, eip1559_transaction_hash};
use arbor_node::{
    Config, Supervisor, init_tracing, initialize_dev_chain, open_database, open_dev_engine,
    run_dev_validator,
};
use arbor_primitives::{DomainId, Eip1559Transaction};
use arbor_system::{
    CHAIN_REGISTRY_ADDRESS, CreateChainRequest, MIN_CREATION_DEPOSIT, encode_create_chain_call,
};
use clap::{Parser, Subcommand};
use k256::ecdsa::SigningKey;
use thiserror::Error;

const DEV_FIXTURE_SECRET: [u8; 32] = [7_u8; 32];
const DEV_FIXTURE_SENDER: Address = address!("4a62316623ad457f02cdc5d997ded67a383ec569");
const M6_CHILD_CHAIN_ID: u64 = 2_049;
const M6_GRANDCHILD_CHAIN_ID: u64 = 2_050;

#[derive(Debug, Parser)]
#[command(name = "arbor", version, about = "Arbor node and operator tools")]
struct Arguments {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Initialize or run an Arbor node.
    Node {
        #[command(subcommand)]
        command: NodeCommand,
    },
    /// Inspect local database metadata.
    Db {
        #[command(subcommand)]
        command: DbCommand,
    },
    /// Development-only chain protocol acceptance tools.
    Chain {
        #[command(subcommand)]
        command: ChainCommand,
    },
}

#[derive(Debug, Subcommand)]
enum NodeCommand {
    /// Write a versioned default configuration.
    Init {
        /// Directory that will contain config.toml and node data.
        #[arg(long)]
        data_dir: PathBuf,
        /// Initialize the deterministic M5 development genesis.
        #[arg(long)]
        dev: bool,
    },
    /// Run the node assembly baseline until interrupted.
    Run {
        /// Directory initialized by `arbor node init`.
        #[arg(long)]
        data_dir: PathBuf,
        /// Run immediate-finality single-validator development consensus.
        #[arg(long)]
        dev_validator: bool,
    },
}

#[derive(Debug, Subcommand)]
enum DbCommand {
    /// Report whether the data directory is initialized.
    Inspect {
        /// Node data directory.
        #[arg(long)]
        data_dir: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
enum ChainCommand {
    /// Run the M6 two-level domain and same-block inclusion-proof acceptance path.
    M6Smoke {
        /// Fresh data directory initialized by `arbor node init --dev`.
        #[arg(long)]
        data_dir: PathBuf,
    },
}

#[derive(Debug, Error)]
enum CliError {
    #[error("{0}")]
    Config(#[from] arbor_node::ConfigError),
    #[error("failed to create {path}: {source}")]
    Create {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to write {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{0}")]
    Supervisor(#[from] arbor_node::SupervisorError),
    #[error("{0}")]
    Storage(#[from] arbor_storage::StorageError),
    #[error("{0}")]
    DevNode(#[from] arbor_node::DevNodeError),
    #[error("{0}")]
    Consensus(#[from] arbor_consensus::ConsensusError),
    #[error("--dev-validator requires a data directory initialized with node init --dev")]
    DevValidatorRequiresDevConfig,
    #[error("M6 smoke requires a data directory initialized with node init --dev")]
    M6RequiresDevConfig,
    #[error("M6 smoke requires a fresh height-zero development chain")]
    M6RequiresFreshChain,
    #[error("M6 acceptance failed: {0}")]
    M6Acceptance(String),
}

#[tokio::main]
async fn main() -> Result<(), CliError> {
    let arguments = Arguments::parse();
    match arguments.command {
        Command::Node { command } => match command {
            NodeCommand::Init { data_dir, dev } => init(&data_dir, dev),
            NodeCommand::Run {
                data_dir,
                dev_validator,
            } => run(data_dir, dev_validator).await,
        },
        Command::Db { command } => match command {
            DbCommand::Inspect { data_dir } => inspect(&data_dir),
        },
        Command::Chain { command } => match command {
            ChainCommand::M6Smoke { data_dir } => m6_smoke(&data_dir),
        },
    }
}

fn init(data_dir: &Path, dev: bool) -> Result<(), CliError> {
    fs::create_dir_all(data_dir).map_err(|source| CliError::Create {
        path: data_dir.to_owned(),
        source,
    })?;
    let path = data_dir.join("config.toml");
    let mut config = Config::default();
    config.node.dev = dev;
    let config = config.to_toml()?;
    fs::write(&path, config).map_err(|source| CliError::Write {
        path: path.clone(),
        source,
    })?;
    open_database(data_dir)?;
    if dev {
        initialize_dev_chain(data_dir)?;
    }
    println!("initialized {}", path.display());
    Ok(())
}

async fn run(data_dir: PathBuf, dev_validator: bool) -> Result<(), CliError> {
    init_tracing("info");
    let config = Config::load(data_dir.join("config.toml"))?;
    open_database(&data_dir)?;
    if dev_validator && !config.node.dev {
        return Err(CliError::DevValidatorRequiresDevConfig);
    }
    tracing::info!(
        moniker = %config.node.moniker,
        dev_validator,
        "starting Arbor workspace baseline"
    );

    let mut supervisor = Supervisor::new();
    let mut shutdown = supervisor.shutdown_signal();
    if dev_validator {
        let history = config.node.domains.clone();
        supervisor.spawn("dev-validator", async move {
            run_dev_validator(&data_dir, history, shutdown).await
        });
    } else {
        supervisor.spawn("node-placeholder", async move {
            shutdown.cancelled().await;
            Ok::<_, std::convert::Infallible>(())
        });
    }
    supervisor.run(Duration::from_secs(10)).await?;
    Ok(())
}

fn inspect(data_dir: &Path) -> Result<(), CliError> {
    let config = Config::load(data_dir.join("config.toml"))?;
    println!(
        "config_version={} moniker={}",
        config.version, config.node.moniker
    );
    let inspection = open_database(data_dir)?.inspect()?;
    println!("database_schema={}", inspection.schema_version);
    match inspection.finalized {
        Some(marker) => println!(
            "finalized_height={} consensus_hash={} domain_heads_root={}",
            marker.height, marker.consensus_hash, marker.domain_heads_root
        ),
        None => println!("finalized_marker=none"),
    }
    let unhealthy = inspection
        .roots
        .iter()
        .filter(|root| root.error.is_some())
        .count();
    println!(
        "root_reachability={} roots={} unhealthy={}",
        if unhealthy == 0 { "ok" } else { "corrupt" },
        inspection.roots.len(),
        unhealthy
    );
    Ok(())
}

fn m6_smoke(data_dir: &Path) -> Result<(), CliError> {
    let config = Config::load(data_dir.join("config.toml"))?;
    if !config.node.dev {
        return Err(CliError::M6RequiresDevConfig);
    }
    let mut engine = open_dev_engine(data_dir, &config.node.domains)?;
    if engine.finalized_state().height.0 != 0 {
        return Err(CliError::M6RequiresFreshChain);
    }
    let root = engine.finalized_state().root_domain_id();
    let root_chain_id = engine
        .finalized_state()
        .domain(root)
        .ok_or_else(|| CliError::M6Acceptance("root domain is absent".to_owned()))?
        .config
        .chain_id;

    let child_envelope =
        create_domain_transaction(root_chain_id, 0, root, M6_CHILD_CHAIN_ID, "M6 Child", "M6C")?;
    let child_id = created_domain_id(&child_envelope, root)?;
    engine.submit_raw(root, child_envelope)?;
    produce_next(&mut engine)?;
    require_domain(&engine, child_id, "child")?;

    let grandchild_envelope = create_domain_transaction(
        root_chain_id,
        1,
        child_id,
        M6_GRANDCHILD_CHAIN_ID,
        "M6 Grandchild",
        "M6G",
    )?;
    let grandchild_id = created_domain_id(&grandchild_envelope, child_id)?;
    engine.submit_raw(root, grandchild_envelope)?;
    produce_next(&mut engine)?;
    require_domain(&engine, grandchild_id, "grandchild")?;

    let child_deploy = deploy_transaction(M6_CHILD_CHAIN_ID)?;
    let grandchild_deploy = deploy_transaction(M6_GRANDCHILD_CHAIN_ID)?;
    engine.submit_raw(child_id, child_deploy)?;
    engine.submit_raw(grandchild_id, grandchild_deploy)?;
    let timestamp = engine
        .finalized_state()
        .timestamp
        .checked_add(1)
        .ok_or_else(|| CliError::M6Acceptance("timestamp overflow".to_owned()))?;
    let proposal = engine.build_proposal(timestamp)?;
    let block = engine
        .pending_block(proposal)
        .ok_or_else(|| CliError::M6Acceptance("pending M6 proposal disappeared".to_owned()))?
        .clone();
    let child_proof = block
        .domain_result_proof(child_id)
        .map_err(|error| CliError::M6Acceptance(error.to_string()))?
        .ok_or_else(|| CliError::M6Acceptance("child result proof is absent".to_owned()))?;
    let grandchild_proof = block
        .domain_result_proof(grandchild_id)
        .map_err(|error| CliError::M6Acceptance(error.to_string()))?
        .ok_or_else(|| CliError::M6Acceptance("grandchild result proof is absent".to_owned()))?;
    child_proof
        .verify()
        .map_err(|error| CliError::M6Acceptance(error.to_string()))?;
    grandchild_proof
        .verify()
        .map_err(|error| CliError::M6Acceptance(error.to_string()))?;
    if child_proof.root != grandchild_proof.root
        || child_proof.root != block.header.domain_results_root
    {
        return Err(CliError::M6Acceptance(
            "domain proofs do not bind the same consensus block root".to_owned(),
        ));
    }
    engine.commit_proposal(proposal)?;

    let contract = DEV_FIXTURE_SENDER.create(0);
    require_contract(&engine, child_id, contract, "child")?;
    require_contract(&engine, grandchild_id, contract, "grandchild")?;
    if engine.finalized_state().consensus_hash != proposal.hash() {
        return Err(CliError::M6Acceptance(
            "committed hash differs from verified proposal".to_owned(),
        ));
    }

    println!("m6_acceptance=ok");
    println!("child_domain={}", child_id.0);
    println!("grandchild_domain={}", grandchild_id.0);
    println!("child_contract={contract}");
    println!("grandchild_contract={contract}");
    println!(
        "finalized_height={} consensus_hash={} domain_results_root={}",
        engine.finalized_state().height.0,
        engine.finalized_state().consensus_hash,
        block.header.domain_results_root
    );
    println!("child_proof=ok root={}", child_proof.root);
    println!("grandchild_proof=ok root={}", grandchild_proof.root);
    Ok(())
}

fn create_domain_transaction(
    root_chain_id: u64,
    nonce: u64,
    parent_domain_id: DomainId,
    evm_chain_id: u64,
    name: &str,
    symbol: &str,
) -> Result<Bytes, CliError> {
    let input = encode_create_chain_call(&CreateChainRequest {
        parent_domain_id,
        name: name.to_owned(),
        symbol: symbol.to_owned(),
        evm_chain_id,
        owner: DEV_FIXTURE_SENDER,
        gas_limit: 20_000_000,
        initial_base_fee: 1_000_000_000,
        initial_supply: U256::from(10_u128.pow(18)),
        protocol_revision: 1,
    })
    .map_err(|error| CliError::M6Acceptance(error.to_string()))?;
    sign_dev_transaction(Eip1559Transaction {
        chain_id: root_chain_id,
        nonce,
        max_priority_fee_per_gas: 1,
        max_fee_per_gas: 2_000_000_000,
        gas_limit: 500_000,
        to: Some(CHAIN_REGISTRY_ADDRESS),
        value: MIN_CREATION_DEPOSIT,
        input,
        access_list: Vec::new(),
        y_parity: false,
        r: U256::ZERO,
        s: U256::ZERO,
    })
}

fn deploy_transaction(chain_id: u64) -> Result<Bytes, CliError> {
    let runtime = [0x60, 0x00, 0x60, 0x00, 0xf3];
    let length = u8::try_from(runtime.len()).expect("fixed runtime length fits u8");
    let mut initcode = vec![
        0x60, length, 0x60, 0x0c, 0x60, 0x00, 0x39, 0x60, length, 0x60, 0x00, 0xf3,
    ];
    initcode.extend_from_slice(&runtime);
    sign_dev_transaction(Eip1559Transaction {
        chain_id,
        nonce: 0,
        max_priority_fee_per_gas: 1,
        max_fee_per_gas: 2_000_000_000,
        gas_limit: 250_000,
        to: None,
        value: U256::ZERO,
        input: initcode.into(),
        access_list: Vec::new(),
        y_parity: false,
        r: U256::ZERO,
        s: U256::ZERO,
    })
}

fn sign_dev_transaction(mut transaction: Eip1559Transaction) -> Result<Bytes, CliError> {
    let payload = encode_eip1559_signing_payload(&transaction)
        .map_err(|error| CliError::M6Acceptance(error.to_string()))?;
    let digest = keccak256(payload);
    let key = SigningKey::from_bytes((&DEV_FIXTURE_SECRET).into())
        .map_err(|error| CliError::M6Acceptance(error.to_string()))?;
    let (signature, recovery_id) = key
        .sign_prehash_recoverable(digest.as_slice())
        .map_err(|error| CliError::M6Acceptance(error.to_string()))?;
    let bytes = signature.to_bytes();
    transaction.r = U256::from_be_slice(&bytes[..32]);
    transaction.s = U256::from_be_slice(&bytes[32..]);
    transaction.y_parity = recovery_id.is_y_odd();
    encode_eip1559(&transaction)
        .map(Bytes::from)
        .map_err(|error| CliError::M6Acceptance(error.to_string()))
}

fn created_domain_id(envelope: &[u8], parent: DomainId) -> Result<DomainId, CliError> {
    let transaction = arbor_codec::decode_eip1559(envelope)
        .map_err(|error| CliError::M6Acceptance(error.to_string()))?;
    let hash = eip1559_transaction_hash(&transaction)
        .map_err(|error| CliError::M6Acceptance(error.to_string()))?;
    Ok(derive_domain_id(
        arbor_node::dev_database_identity().network_id,
        parent,
        hash,
    ))
}

fn produce_next(engine: &mut arbor_consensus::SingleValidatorEngine) -> Result<(), CliError> {
    let timestamp = engine
        .finalized_state()
        .timestamp
        .checked_add(1)
        .ok_or_else(|| CliError::M6Acceptance("timestamp overflow".to_owned()))?;
    engine.produce_block(timestamp)?;
    Ok(())
}

fn require_domain(
    engine: &arbor_consensus::SingleValidatorEngine,
    domain_id: DomainId,
    label: &str,
) -> Result<(), CliError> {
    engine
        .finalized_state()
        .domain(domain_id)
        .ok_or_else(|| CliError::M6Acceptance(format!("{label} domain was not finalized")))?;
    Ok(())
}

fn require_contract(
    engine: &arbor_consensus::SingleValidatorEngine,
    domain_id: DomainId,
    contract: Address,
    label: &str,
) -> Result<(), CliError> {
    let state = &engine
        .finalized_state()
        .domain(domain_id)
        .ok_or_else(|| CliError::M6Acceptance(format!("{label} domain is absent")))?
        .state;
    let account = state
        .account(contract)
        .map_err(|error| CliError::M6Acceptance(error.to_string()))?
        .ok_or_else(|| CliError::M6Acceptance(format!("{label} contract account is absent")))?;
    if !state.contract_code().contains_key(&account.code_hash) {
        return Err(CliError::M6Acceptance(format!(
            "{label} contract code is absent"
        )));
    }
    Ok(())
}
