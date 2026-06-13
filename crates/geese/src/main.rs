use std::{env, path::PathBuf, process};

use clap::{Parser, Subcommand};
use geese_client::{ClientError, GeesedClient, ensure_running};
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
    /// Show geesed daemon status (does not autospawn)
    Status,
    /// List profiles (* prefix on locked)
    List,
    /// Create a new profile
    New { name: String },
    /// Delete a profile
    Delete { name: String },
    /// Lock a profile
    Lock { name: String },
    /// Unlock a profile
    Unlock { name: String },
    /// Copy a profile
    Copy { src: String, dest: String },
    /// Print the path of a profile
    Path { name: String },
    /// Start a goose acp process for a profile
    Start { name: String },
    /// Stop (SIGTERM) a goose acp process for a profile
    Stop { name: String },
    /// Kill (SIGKILL) a goose acp process for a profile
    Kill { name: String },
    /// List running goose acp processes
    Ps,
    /// Print the resolved working directory for a profile
    Cwd { name: String },
    /// Set or clear the per-profile working directory
    SetCwd {
        /// Profile name
        name: String,
        /// Working directory path to set (omit to use --unset)
        path: Option<String>,
        /// Clear the per-profile cwd instead of setting it
        #[arg(long)]
        unset: bool,
    },
    /// Get or set global geesed configuration
    #[command(subcommand)]
    Config(ConfigCommands),
}

#[derive(Debug, Subcommand)]
enum ConfigCommands {
    /// Print global config (or a single key if provided)
    Get {
        /// Optional key name (currently: "cwd")
        key: Option<String>,
    },
    /// Set a global config value
    Set {
        /// Config key name (currently: "cwd")
        key: String,
        /// Config value
        value: String,
    },
    /// Clear a global config value
    Unset {
        /// Config key name (currently: "cwd")
        key: String,
    },
}

#[derive(Debug, Error)]
enum CliError {
    #[error("geesed: not running (no socket at {0})")]
    NotRunning(PathBuf),
    #[error(transparent)]
    Client(#[from] ClientError),
    #[error("{0}")]
    User(String),
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
        Commands::List => {
            let mut client = ensure_running().await?;
            let profiles = client.list_profiles().await?;
            for entry in profiles {
                if entry.locked {
                    println!("*{}", entry.name);
                } else {
                    println!("{}", entry.name);
                }
            }
        }
        Commands::New { name } => {
            let mut client = ensure_running().await?;
            client.create_profile(&name).await?;
        }
        Commands::Delete { name } => {
            let mut client = ensure_running().await?;
            client.delete_profile(&name).await?;
        }
        Commands::Lock { name } => {
            let mut client = ensure_running().await?;
            client.lock_profile(&name).await?;
        }
        Commands::Unlock { name } => {
            let mut client = ensure_running().await?;
            client.unlock_profile(&name).await?;
        }
        Commands::Copy { src, dest } => {
            let mut client = ensure_running().await?;
            client.copy_profile(&src, &dest).await?;
        }
        Commands::Path { name } => {
            let mut client = ensure_running().await?;
            let entry = client.get_profile(&name).await?;
            println!("{}", entry.path);
        }
        Commands::Start { name } => {
            let mut client = ensure_running().await?;
            client.start_goosed(&name).await?;
        }
        Commands::Stop { name } => {
            let mut client = ensure_running().await?;
            client.stop_goosed(&name).await?;
        }
        Commands::Kill { name } => {
            let mut client = ensure_running().await?;
            client.kill_goosed(&name).await?;
        }
        Commands::Ps => {
            let mut client = ensure_running().await?;
            let running = client.list_running_goosed().await?;
            if !running.is_empty() {
                println!("{:<20} {:<10} STARTED", "NAME", "PID");
                for entry in running {
                    println!("{:<20} {:<10} {}", entry.name, entry.pid, entry.started_at);
                }
            }
        }
        Commands::Cwd { name } => {
            let mut client = ensure_running().await?;
            let resolved = client.resolve_cwd(&name).await?;
            println!("{}", resolved.display());
        }
        Commands::SetCwd { name, path, unset } => {
            let mut client = ensure_running().await?;
            if unset {
                client.unset_profile_cwd(&name).await?;
            } else {
                let p =
                    path.ok_or_else(|| CliError::User("provide a path or use --unset".to_owned()))?;
                client.set_profile_cwd(&name, &p).await?;
            }
        }
        Commands::Config(sub) => match sub {
            ConfigCommands::Get { key } => {
                let mut client = ensure_running().await?;
                let config = client.get_global_config().await?;
                match key.as_deref() {
                    None | Some("") => {
                        let cwd_display = config.cwd.as_deref().unwrap_or("<not set>");
                        println!("cwd = {cwd_display}");
                    }
                    Some("cwd") => {
                        let cwd_display = config.cwd.as_deref().unwrap_or("<not set>");
                        println!("{cwd_display}");
                    }
                    Some(k) => {
                        return Err(CliError::User(format!("unknown config key: {k}")));
                    }
                }
            }
            ConfigCommands::Set { key, value } => {
                let mut client = ensure_running().await?;
                match key.as_str() {
                    "cwd" => {
                        client.set_global_config(Some(&value)).await?;
                    }
                    k => {
                        return Err(CliError::User(format!("unknown config key: {k}")));
                    }
                }
            }
            ConfigCommands::Unset { key } => {
                let mut client = ensure_running().await?;
                match key.as_str() {
                    "cwd" => {
                        client.set_global_config(None).await?;
                    }
                    k => {
                        return Err(CliError::User(format!("unknown config key: {k}")));
                    }
                }
            }
        },
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
