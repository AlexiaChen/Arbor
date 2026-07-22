//! Arbor command-line entry point.

#![forbid(unsafe_code)]

use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use arbor_node::{Config, Supervisor, init_tracing};
use arbor_primitives::NetworkId;
use arbor_storage::{Database, DatabaseIdentity, RetentionPolicy};
use clap::{Parser, Subcommand};
use thiserror::Error;

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
}

#[derive(Debug, Subcommand)]
enum NodeCommand {
    /// Write a versioned default configuration.
    Init {
        /// Directory that will contain config.toml and node data.
        #[arg(long)]
        data_dir: PathBuf,
    },
    /// Run the node assembly baseline until interrupted.
    Run {
        /// Directory initialized by `arbor node init`.
        #[arg(long)]
        data_dir: PathBuf,
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
}

#[tokio::main]
async fn main() -> Result<(), CliError> {
    let arguments = Arguments::parse();
    match arguments.command {
        Command::Node { command } => match command {
            NodeCommand::Init { data_dir } => init(&data_dir),
            NodeCommand::Run { data_dir } => run(data_dir).await,
        },
        Command::Db { command } => match command {
            DbCommand::Inspect { data_dir } => inspect(&data_dir),
        },
    }
}

fn init(data_dir: &Path) -> Result<(), CliError> {
    fs::create_dir_all(data_dir).map_err(|source| CliError::Create {
        path: data_dir.to_owned(),
        source,
    })?;
    let path = data_dir.join("config.toml");
    let config = Config::default().to_toml()?;
    fs::write(&path, config).map_err(|source| CliError::Write {
        path: path.clone(),
        source,
    })?;
    open_database(data_dir)?;
    println!("initialized {}", path.display());
    Ok(())
}

async fn run(data_dir: PathBuf) -> Result<(), CliError> {
    init_tracing("info");
    let config = Config::load(data_dir.join("config.toml"))?;
    open_database(&data_dir)?;
    tracing::info!(moniker = %config.node.moniker, "starting Arbor workspace baseline");

    let mut supervisor = Supervisor::new();
    let mut shutdown = supervisor.shutdown_signal();
    supervisor.spawn("node-placeholder", async move {
        shutdown.cancelled().await;
        Ok::<_, std::convert::Infallible>(())
    });
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

fn open_database(data_dir: &Path) -> Result<Database, arbor_storage::StorageError> {
    Database::open(
        data_dir.join("db"),
        DatabaseIdentity {
            network_id: NetworkId(alloy_primitives::keccak256(b"ARBOR_DEV_NETWORK_V1")),
            genesis_hash: alloy_primitives::keccak256(b"ARBOR_DEV_GENESIS_V1"),
        },
        RetentionPolicy::Archive,
    )
}
