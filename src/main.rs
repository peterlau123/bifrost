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

    /// Run pytest tests with automatic JSON report
    Pytest {
        /// Test path to run
        #[arg(short, long)]
        path: String,

        /// Task priority (0-255)
        #[arg(short = 'p', long, default_value = "5")]
        priority: u8,

        /// Timeout in seconds
        #[arg(short, long, default_value = "600")]
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
    use bifrost::core::protocol::Protocol;
    use bifrost::core::models::TaskType;
    use bifrost::client::{submit, status, results, pytest};
    use uuid::Uuid;

    // Default shared storage path
    let shared_storage = PathBuf::from("/tmp/bifrost");

    match command {
        ClientCommand::Submit {
            command: cmd,
            task_type,
            priority,
            timeout,
            working_dir,
        } => {
            let protocol = Protocol::new(shared_storage.clone())
                .expect("Failed to create protocol");

            let parsed_type = match task_type.as_str() {
                "pytest" => TaskType::Pytest,
                "shell" => TaskType::Shell,
                "custom" => TaskType::Custom,
                _ => TaskType::Shell,
            };

            match submit::submit_task(&protocol, cmd, parsed_type, priority, timeout, working_dir) {
                Ok(task_id) => {
                    println!("Task submitted successfully");
                    println!("  Task ID: {}", task_id);
                    println!("  Status: Pending");
                }
                Err(e) => {
                    eprintln!("Failed to submit task: {}", e);
                }
            }
        }

        ClientCommand::Pytest {
            path,
            priority,
            timeout,
            working_dir,
        } => {
            let protocol = Protocol::new(shared_storage.clone())
                .expect("Failed to create protocol");

            match submit::submit_pytest_task(&protocol, path, priority, timeout, working_dir) {
                Ok(task_id) => {
                    println!("Pytest task submitted successfully");
                    println!("  Task ID: {}", task_id);
                    println!("  Command: {}", pytest::build_pytest_command(&path));
                    println!("  Artifact: report.json");
                }
                Err(e) => {
                    eprintln!("Failed to submit pytest task: {}", e);
                }
            }
        }

        ClientCommand::Status { task_id } => {
            let protocol = Protocol::new(shared_storage.clone())
                .expect("Failed to create protocol");

            let parsed_id = Uuid::parse_str(&task_id)
                .expect("Invalid task ID format");

            match status::query_status(&protocol, parsed_id) {
                Ok(status_resp) => {
                    println!("Task status for: {}", task_id);
                    println!("  Status: {}", status_resp.status);
                    if let Some(msg) = status_resp.message {
                        println!("  Message: {}", msg);
                    }
                }
                Err(e) => {
                    eprintln!("Failed to query status: {}", e);
                }
            }
        }

        ClientCommand::Results { task_id, format } => {
            let protocol = Protocol::new(shared_storage.clone())
                .expect("Failed to create protocol");

            let parsed_id = Uuid::parse_str(&task_id)
                .expect("Invalid task ID format");

            let result_format = match format.as_str() {
                "json" => results::ResultFormat::Json,
                "yaml" => results::ResultFormat::Yaml,
                "text" => results::ResultFormat::Text,
                _ => results::ResultFormat::Json,
            };

            match results::get_result_formatted(&protocol, parsed_id, result_format) {
                Ok(result_text) => {
                    println!("Results for task: {}", task_id);
                    println!("{}", result_text);
                }
                Err(e) => {
                    eprintln!("Failed to retrieve results: {}", e);
                }
            }
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