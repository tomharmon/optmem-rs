use std::{path::PathBuf, process::ExitCode, sync::Arc, time::Duration};

use clap::Parser;
use optmem::{Error, OptMem, settings::AppSettings, storage::FileStorage};
use optmem_agent::{MemoryTools, ToolPolicy};
use optmem_mcp::OptMemServer;
use rmcp::{ServiceExt, transport::stdio};

#[derive(Parser)]
#[command(
    version,
    about = "OptMem MCP server over stdio; stdout is reserved for the protocol"
)]
struct Cli {
    /// User config file; defaults to $XDG_CONFIG_HOME/optmem/config.toml
    #[arg(long = "config", env = "OPTMEM_CONFIG")]
    config_file: Option<PathBuf>,
    /// Override storage_dir from the user config
    #[arg(long, env = "MEMORY_DIR")]
    memory_dir: Option<PathBuf>,
    /// Explicitly initialize the memory before serving (idempotent)
    #[arg(long)]
    init: bool,
    /// Allow reads only; refuse note appends and summary submissions
    #[arg(long, conflicts_with = "allow_forget")]
    read_only: bool,
    /// Expose the summary-repair tool (never deletes original memories)
    #[arg(long)]
    allow_forget: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("optmem-mcp: {error}");
            return ExitCode::FAILURE;
        }
    };
    let result = runtime.block_on(run(cli));
    // Tokio's blocking stdin reader cannot be cancelled while the client keeps
    // its pipe open. Bound shutdown so Ctrl-C can still terminate the process.
    runtime.shutdown_timeout(Duration::from_secs(1));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("optmem-mcp: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    let settings = AppSettings::load(cli.config_file)?;
    let path = settings.storage_dir(cli.memory_dir.as_deref())?;
    let storage = if cli.init {
        FileStorage::initialize(&path)?
    } else {
        FileStorage::open(&path).map_err(|error| match error {
            Error::MissingStore(_) => Error::Invalid(format!(
                "No memory at {}. Initialize it with memo init, or start this server with --init.",
                path.display()
            )),
            other => other,
        })?
    };
    let config = settings.memory_config(&path)?;
    if cli.init {
        settings.initialize(&path, &config)?;
    }
    let memory = Arc::new(OptMem::with_config(storage, config)?);
    let policy = if cli.read_only {
        ToolPolicy::read_only()
    } else {
        ToolPolicy {
            allow_notes: true,
            allow_forget: cli.allow_forget,
        }
    };
    let server = OptMemServer::new(MemoryTools::new(memory).with_policy(policy));
    let service = server.serve(stdio()).await?;
    let cancellation = service.cancellation_token();
    tokio::select! {
        result = service.waiting() => { result?; },
        result = tokio::signal::ctrl_c() => {
            result?;
            cancellation.cancel();
        },
    }
    Ok(())
}
