// CLI entry point with client/daemon mode selection
use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// Bifrost - Offline machine command execution framework
#[derive(Parser, Debug)]
#[command(name = "bifrost")]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    mode: Mode,
}

/// Bifrost operation modes
#[derive(Subcommand, Debug)]
enum Mode {
    /// Client mode - submit tasks and check results
    Client {
        #[command(subcommand)]
        command: ClientCommand,
    },

    /// Daemon mode - execute tasks from shared storage
    Daemon {
        /// Configuration file path
        #[arg(short, long, value_name = "FILE")]
        config: Option<PathBuf>,

        /// Run as systemd service
        #[arg(long)]
        systemd: bool,
    },
}

/// Client commands
#[derive(Subcommand, Debug)]
enum ClientCommand {
    /// Submit a new task for execution
    Submit {
        /// Command to execute
        #[arg(short, long)]
        command: String,

        /// Task type (pytest, shell, custom)
        #[arg(short = 't', long, default_value = "shell")]
        task_type: String,

        /// Task priority (0-255)
        #[arg(short = 'p', long, default_value = "0")]
        priority: u8,

        /// Timeout in seconds
        #[arg(short, long, default_value = "300")]
        timeout: u64,

        /// Working directory
        #[arg(short = 'w', long)]
        working_dir: Option<PathBuf>,
    },

    /// Check task status
    Status {
        /// Task ID to check
        #[arg(short, long)]
        task_id: String,
    },

    /// Retrieve task results
    Results {
        /// Task ID to retrieve results for
        #[arg(short, long)]
        task_id: String,

        /// Output format (json, yaml, text)
        #[arg(short = 'f', long, default_value = "json")]
        format: String,
    },
}

fn main() {
    let cli = Cli::parse();

    match cli.mode {
        Mode::Client { command } => handle_client_mode(command),
        Mode::Daemon { config, systemd } => handle_daemon_mode(config, systemd),
    }
}

/// Handle client mode operations
fn handle_client_mode(command: ClientCommand) {
    match command {
        ClientCommand::Submit {
            command: cmd,
            task_type,
            priority,
            timeout,
            working_dir,
        } => {
            // Placeholder: Client submit functionality
            println!("Submitting task:");
            println!("  Command: {}", cmd);
            println!("  Type: {}", task_type);
            println!("  Priority: {}", priority);
            println!("  Timeout: {}s", timeout);
            if let Some(wd) = working_dir {
                println!("  Working dir: {}", wd.display());
            }
            println!("  Status: Not implemented yet (placeholder)");
        }

        ClientCommand::Status { task_id } => {
            // Placeholder: Client status check
            println!("Checking status for task: {}", task_id);
            println!("  Status: Not implemented yet (placeholder)");
        }

        ClientCommand::Results { task_id, format } => {
            // Placeholder: Client results retrieval
            println!("Retrieving results for task: {}", task_id);
            println!("  Format: {}", format);
            println!("  Status: Not implemented yet (placeholder)");
        }
    }
}

/// Handle daemon mode operations
fn handle_daemon_mode(config: Option<PathBuf>, systemd: bool) {
    println!("Starting bifrost daemon...");

    if let Some(config_path) = config {
        println!("  Config: {}", config_path.display());
    }

    if systemd {
        println!("  Mode: systemd service");
    }

    // Placeholder: Daemon functionality to be implemented
    println!("  Status: Daemon not implemented yet (placeholder)");
    println!("  Use --config to specify configuration file");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cli_parse_client_submit() {
        let cli = Cli::try_parse_from([
            "bifrost",
            "client",
            "submit",
            "--command", "pytest tests/",
            "--task-type", "pytest",
            "--priority", "10",
            "--timeout", "600",
        ]);

        assert!(cli.is_ok());
        let cli = cli.unwrap();
        match cli.mode {
            Mode::Client { command } => {
                match command {
                    ClientCommand::Submit {
                        command: cmd,
                        task_type,
                        priority,
                        timeout,
                        working_dir,
                    } => {
                        assert_eq!(cmd, "pytest tests/");
                        assert_eq!(task_type, "pytest");
                        assert_eq!(priority, 10);
                        assert_eq!(timeout, 600);
                        assert!(working_dir.is_none());
                    }
                    _ => panic!("Expected Submit command"),
                }
            }
            _ => panic!("Expected Client mode"),
        }
    }

    #[test]
    fn test_cli_parse_client_status() {
        let cli = Cli::try_parse_from([
            "bifrost",
            "client",
            "status",
            "--task-id", "12345",
        ]);

        assert!(cli.is_ok());
        let cli = cli.unwrap();
        match cli.mode {
            Mode::Client { command } => {
                match command {
                    ClientCommand::Status { task_id } => {
                        assert_eq!(task_id, "12345");
                    }
                    _ => panic!("Expected Status command"),
                }
            }
            _ => panic!("Expected Client mode"),
        }
    }

    #[test]
    fn test_cli_parse_daemon_with_config() {
        let cli = Cli::try_parse_from([
            "bifrost",
            "daemon",
            "--config", "/etc/bifrost/daemon.yaml",
            "--systemd",
        ]);

        assert!(cli.is_ok());
        let cli = cli.unwrap();
        match cli.mode {
            Mode::Daemon { config, systemd } => {
                assert_eq!(config.unwrap(), PathBuf::from("/etc/bifrost/daemon.yaml"));
                assert!(systemd);
            }
            _ => panic!("Expected Daemon mode"),
        }
    }

    #[test]
    fn test_cli_parse_daemon_default() {
        let cli = Cli::try_parse_from([
            "bifrost",
            "daemon",
        ]);

        assert!(cli.is_ok());
        let cli = cli.unwrap();
        match cli.mode {
            Mode::Daemon { config, systemd } => {
                assert!(config.is_none());
                assert!(!systemd);
            }
            _ => panic!("Expected Daemon mode"),
        }
    }
}