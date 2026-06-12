use std::{env, path::PathBuf, process};

use clap::{Parser, Subcommand};
use geese_client::{ClientError, GeesedClient};
use thiserror::Error;

#[derive(Debug, Parser)]
#[command(name = "geese")]
#[command(about = "Talk to a local geesed daemon")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    Status,
}

#[derive(Debug, Error)]
enum CliError {
    #[error("geesed: not running (no socket at {0})")]
    NotRunning(PathBuf),
    #[error(transparent)]
    Client(#[from] ClientError),
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("{error}");
        process::exit(1);
    }
}

async fn run() -> Result<(), CliError> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Status => {
            let socket = socket_override();
            let mut client = match socket.as_deref() {
                Some(path) => GeesedClient::connect_at(path).await,
                None => GeesedClient::connect().await,
            }
            .map_err(map_not_running)?;

            let status = client.status().await.map_err(map_not_running)?;
            let socket_path = client.socket_path().to_path_buf();

            println!("geesed: running");
            println!("  pid:        {}", status.pid);
            println!("  version:    {}", status.version);
            println!("  uptime:     {}", format_uptime(status.uptime_ms));
            println!("  socket:     {}", socket_path.display());
            println!("  started:    {}", status.started_at);
        }
    }

    Ok(())
}

fn socket_override() -> Option<PathBuf> {
    env::var_os("GEESED_SOCKET").map(PathBuf::from)
}

fn map_not_running(error: ClientError) -> CliError {
    match error {
        ClientError::NotRunning(path) => CliError::NotRunning(path),
        other => CliError::Client(other),
    }
}

fn format_uptime(uptime_ms: u64) -> String {
    let total_seconds = uptime_ms / 1_000;
    let hours = total_seconds / 3_600;
    let minutes = (total_seconds % 3_600) / 60;
    let seconds = total_seconds % 60;

    if hours > 0 {
        format!("{hours}h {minutes}m {seconds}s")
    } else if minutes > 0 {
        format!("{minutes}m {seconds}s")
    } else {
        format!("{seconds}s")
    }
}
