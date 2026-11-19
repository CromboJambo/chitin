//! chitin-container — containerized deployment for chitin compositor.
//!
//! CLI for building and managing chitin containers with GPU passthrough,
//! virtual display, and remote access support.

use chitin_container::{ChitinContainerConfig, ChitinRuntime};
use clap::{Parser, Subcommand};
use std::process;

/// Containerized deployment for chitin compositor (spec 06).
#[derive(Parser)]
#[command(name = "chitin-container", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Build the chitin container image.
    Build {
        /// Directory containing Dockerfile.chitin (defaults to current dir).
        #[arg(short, long, default_value = ".")]
        dir: String,
    },
    /// Start a chitin container.
    Run {
        /// Container name (defaults to "chitin-desktop").
        #[arg(short, long, default_value = "chitin-desktop")]
        name: String,
        /// Virtual display width (defaults to 1920).
        #[arg(long, default_value_t = 1920)]
        width: u32,
        /// Virtual display height (defaults to 1080).
        #[arg(long, default_value_t = 1080)]
        height: u32,
        /// Enable RustDesk remote access.
        #[arg(long)]
        remote: bool,
    },
    /// Stop a running chitin container.
    Stop {
        /// Container name.
        #[arg(short, long, default_value = "chitin-desktop")]
        name: String,
    },
    /// Remove a chitin container (must be stopped first).
    Remove {
        /// Container name.
        #[arg(short, long, default_value = "chitin-desktop")]
        name: String,
    },
    /// Show status of a chitin container.
    Status {
        /// Container name.
        #[arg(short, long, default_value = "chitin-desktop")]
        name: String,
    },
    /// Show logs from a chitin container.
    Logs {
        /// Container name.
        #[arg(short, long, default_value = "chitin-desktop")]
        name: String,
        /// Number of tail lines to show.
        #[arg(short, long, default_value_t = 50)]
        tail: usize,
    },
}

fn main() {
    // Initialize tracing.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init();

    let cli = Cli::parse();

    if let Err(e) = run(cli) {
        eprintln!("error: {}", e);
        process::exit(1);
    }
}

fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    match cli.command {
        Command::Build { dir } => {
            let runtime = ChitinRuntime::new()?;
            runtime.build_image(&dir)?;
            println!("Container image built successfully.");
        }
        Command::Run { name, width, height, remote } => {
            let mut config = ChitinContainerConfig::new(&name);
            config.virtual_resolution = chitin_container::config::Resolution { width, height };

            if !remote {
                config = config.without_remote();
            }

            let runtime = ChitinRuntime::new()?;
            let container_id = runtime.start(&config)?;
            println!("Container started: {} (id: {})", name, container_id);

            if let Some(remote_config) = &config.remote {
                match remote_config.protocol {
                    chitin_container::config::RemoteProtocol::RustDesk => {
                        println!(
                            "Remote access available via RustDesk on port {}",
                            remote_config.port
                        );
                    }
                    chitin_container::config::RemoteProtocol::NoVnc => {
                        println!(
                            "Remote access available via NoVNC on port {}",
                            remote_config.port
                        );
                    }
                }
            }
        }
        Command::Stop { name } => {
            let runtime = ChitinRuntime::new()?;
            runtime.stop(&name)?;
            println!("Container stopped: {}", name);
        }
        Command::Remove { name } => {
            let runtime = ChitinRuntime::new()?;
            runtime.remove(&name)?;
            println!("Container removed: {}", name);
        }
        Command::Status { name } => {
            let runtime = ChitinRuntime::new()?;
            if runtime.is_running(&name)? {
                println!("Container {} is running.", name);
            } else {
                println!("Container {} is not running.", name);
            }
        }
        Command::Logs { name, tail } => {
            let runtime = ChitinRuntime::new()?;
            let logs = runtime.logs(&name, tail)?;
            print!("{}", logs);
        }
    }

    Ok(())
}
