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
    /// Initialize ~/.bifrost/ with default settings
    Init,

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
        #[arg(long, default_value = "300")]
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
        #[arg(long, default_value = "600")]
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

    /// Query task history from SQLite
    History {
        /// Filter by status (Pending, Running, Completed, Failed, Cancelled, Timeout)
        #[arg(long)]
        status: Option<String>,

        /// Filter by task type (shell, pytest, custom)
        #[arg(long)]
        task_type: Option<String>,

        /// Maximum results (default 20)
        #[arg(long, default_value = "20")]
        limit: i64,

        /// Result offset for pagination
        #[arg(long, default_value = "0")]
        offset: i64,

        /// Show detail for a specific task ID
        #[arg(long)]
        task_id: Option<String>,

        /// Output format (json or text)
        #[arg(long, default_value = "text")]
        format: String,
    },

    /// Batch operations
    Batch {
        #[command(subcommand)]
        command: BatchCommand,
    },
}

/// Batch commands
#[derive(Subcommand, Debug)]
enum BatchCommand {
    /// Submit a batch manifest for execution
    SubmitManifest {
        /// Path to manifest JSON file
        #[arg(short, long)]
        manifest: PathBuf,

        /// Batch progress directory
        #[arg(short = 'b', long)]
        batch_dir: Option<PathBuf>,
    },

    /// Check batch execution status
    BatchStatus {
        /// Batch ID to check
        #[arg(short, long)]
        batch_id: String,
    },

    /// Cancel a running batch
    CancelBatch {
        /// Batch ID to cancel
        #[arg(short, long)]
        batch_id: String,
    },

    /// List all active batches
    ListBatches {
        /// Batch progress directory
        #[arg(short = 'b', long)]
        batch_dir: Option<PathBuf>,
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
    use bifrost::client::{pytest, results, status, submit};
    use bifrost::core::db::Database;
    use bifrost::core::models::{TaskStatus, TaskType};
    use bifrost::core::protocol::Protocol;
    use bifrost::core::settings;
    use uuid::Uuid;

    let settings = settings::load();
    let shared_storage = settings.shared_storage.clone();
    let db_path = settings.db_path();
    let db = Database::open(db_path, Some(shared_storage.clone())).ok();

    match command {
        ClientCommand::Init => {
            match settings::init() {
                Ok(path) => println!("Settings saved to {}", path.display()),
                Err(e) => eprintln!("{}", e),
            }
        }

        ClientCommand::Submit {
            command: cmd,
            task_type,
            priority,
            timeout,
            working_dir,
        } => {
            let protocol =
                Protocol::new(shared_storage.clone()).expect("Failed to create protocol");

            let parsed_type = match task_type.as_str() {
                "pytest" => TaskType::Pytest,
                "shell" => TaskType::Shell,
                "custom" => TaskType::Custom,
                _ => TaskType::Shell,
            };

            match submit::submit_task(
                &protocol,
                db.as_ref(),
                cmd,
                parsed_type,
                priority,
                timeout,
                working_dir,
            ) {
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
            let protocol =
                Protocol::new(shared_storage.clone()).expect("Failed to create protocol");

            match submit::submit_pytest_task(
                &protocol,
                db.as_ref(),
                path.clone(),
                priority,
                timeout,
                working_dir,
            ) {
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
            let protocol =
                Protocol::new(shared_storage.clone()).expect("Failed to create protocol");

            let parsed_id = Uuid::parse_str(&task_id).expect("Invalid task ID format");

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
            let protocol =
                Protocol::new(shared_storage.clone()).expect("Failed to create protocol");

            let parsed_id = Uuid::parse_str(&task_id).expect("Invalid task ID format");

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

        ClientCommand::History {
            status,
            task_type,
            limit,
            offset,
            task_id,
            format,
        } => {
            // Use the existing db (may be None if SQLite open failed)
            if let Some(ref db) = db {
                if let Some(tid) = task_id {
                    match Uuid::parse_str(&tid) {
                        Ok(id) => match db.get_task_by_id(id) {
                            Ok(Some(detail)) => {
                                println!("Task: {}", detail.task_id);
                                println!("  Command:    {}", detail.command);
                                println!("  Type:       {}", detail.task_type);
                                println!("  Status:     {}", detail.status);
                                if let Some(ec) = detail.exit_code {
                                    println!("  Exit Code:  {}", ec);
                                }
                                if let Some(msg) = detail.error_message {
                                    println!("  Error:      {}", msg);
                                }
                                if let Some(dur) = detail.duration_ms {
                                    println!("  Duration:   {}ms", dur);
                                }
                                if let Some(grp) = detail.batch_id {
                                    println!("  Batch ID:   {}", grp);
                                }
                                if let Some(ref p) = detail.pytest_result {
                                    println!(
                                        "  Pytest:     {}/{}/{} (p/f/s)",
                                        p.passed, p.failed, p.skipped
                                    );
                                    if let Some(d) = p.duration_ms {
                                        println!("  PyDur:      {}ms", d);
                                    }
                                }
                            }
                            Ok(None) => eprintln!("Task not found: {}", tid),
                            Err(e) => eprintln!("Failed to query task: {}", e),
                        },
                        Err(e) => eprintln!("Invalid task ID format: {}", e),
                    }
                } else {
                    let status_filter =
                        status
                            .as_ref()
                            .and_then(|s| match s.to_lowercase().as_str() {
                                "pending" => Some(TaskStatus::Pending),
                                "running" => Some(TaskStatus::Running),
                                "completed" => Some(TaskStatus::Completed),
                                "failed" => Some(TaskStatus::Failed),
                                "cancelled" => Some(TaskStatus::Cancelled),
                                "timeout" => Some(TaskStatus::Timeout),
                                _ => None,
                            });
                    let type_filter =
                        task_type
                            .as_ref()
                            .and_then(|s| match s.to_lowercase().as_str() {
                                "shell" => Some(TaskType::Shell),
                                "pytest" => Some(TaskType::Pytest),
                                "custom" => Some(TaskType::Custom),
                                _ => None,
                            });

                    match db.query_tasks(status_filter, type_filter, limit, offset) {
                        Ok(records) => {
                            if records.is_empty() {
                                println!("No tasks found");
                            } else {
                                let is_json = format == "json";
                                if is_json {
                                    println!("{}", serde_json::to_string_pretty(&records).unwrap());
                                } else {
                                    for r in &records {
                                        println!(
                                            "{}  {:<10}  {}  {}",
                                            &r.task_id[..8],
                                            r.status,
                                            r.task_type,
                                            r.command
                                        );
                                    }
                                    println!("---");
                                    println!("{} task(s) shown", records.len());
                                }
                            }
                        }
                        Err(e) => eprintln!("Failed to query history: {}", e),
                    }
                }
            } else {
                eprintln!("History unavailable: database not configured");
            }
        }

        ClientCommand::Batch { command } => {
            use bifrost::core::batch_tracker::BatchTracker;

            // Default batch progress directory
            let batch_dir = PathBuf::from("/tmp/bifrost/batch_progress");

            match command {
                BatchCommand::SubmitManifest {
                    manifest,
                    batch_dir: custom_batch_dir,
                } => {
                    let protocol =
                        Protocol::new(shared_storage.clone()).expect("Failed to create protocol");

                    let tracker = BatchTracker::new(custom_batch_dir.unwrap_or(batch_dir));

                    match submit::submit_batch_manifest(&protocol, db.as_ref(), &tracker, &manifest)
                    {
                        Ok(batch_id) => {
                            println!("Batch manifest submitted successfully");
                            println!("  Batch ID: {}", batch_id);
                            println!("  Manifest: {}", manifest.display());
                            println!("  Status: Submitting");
                        }
                        Err(e) => {
                            eprintln!("Failed to submit batch manifest: {}", e);
                        }
                    }
                }

                BatchCommand::BatchStatus { batch_id } => {
                    let tracker = BatchTracker::new(batch_dir);

                    let parsed_id = Uuid::parse_str(&batch_id).expect("Invalid batch ID format");

                    match tracker.load_progress(parsed_id) {
                        Ok(progress) => {
                            println!("Batch status for: {}", batch_id);
                            println!("  Status: {}", progress.status);
                            println!("  Total tasks: {}", progress.total_tasks);
                            println!("  Completed: {}", progress.completed_tasks.len());
                            println!("  Created: {}", progress.created_at);
                            println!("  Updated: {}", progress.updated_at);
                        }
                        Err(e) => {
                            eprintln!("Failed to load batch progress: {}", e);
                        }
                    }
                }

                BatchCommand::CancelBatch { batch_id } => {
                    println!("Cancel batch: {}", batch_id);
                    println!("  Note: Batch cancellation not yet implemented");
                    println!("  Requires daemon to support cancel signal");
                }

                BatchCommand::ListBatches {
                    batch_dir: custom_batch_dir,
                } => {
                    let tracker = BatchTracker::new(custom_batch_dir.unwrap_or(batch_dir));

                    match tracker.list_active_batches() {
                        Ok(batches) => {
                            println!("Active batches:");
                            if batches.is_empty() {
                                println!("  No active batches found");
                            } else {
                                for batch in batches {
                                    println!("  - Batch ID: {}", batch.batch_id);
                                    println!("    Status: {}", batch.status);
                                    println!(
                                        "    Tasks: {} of {} completed",
                                        batch.completed_tasks.len(),
                                        batch.total_tasks
                                    );
                                }
                            }
                        }
                        Err(e) => {
                            eprintln!("Failed to list batches: {}", e);
                        }
                    }
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
            "--command",
            "pytest tests/",
            "--task-type",
            "pytest",
            "--priority",
            "10",
            "--timeout",
            "600",
        ]);

        assert!(cli.is_ok());
        let cli = cli.unwrap();
        match cli.mode {
            Mode::Client { command } => match command {
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
            },
            _ => panic!("Expected Client mode"),
        }
    }

    #[test]
    fn test_cli_parse_client_status() {
        let cli = Cli::try_parse_from(["bifrost", "client", "status", "--task-id", "12345"]);

        assert!(cli.is_ok());
        let cli = cli.unwrap();
        match cli.mode {
            Mode::Client { command } => match command {
                ClientCommand::Status { task_id } => {
                    assert_eq!(task_id, "12345");
                }
                _ => panic!("Expected Status command"),
            },
            _ => panic!("Expected Client mode"),
        }
    }

    #[test]
    fn test_cli_parse_daemon_with_config() {
        let cli = Cli::try_parse_from([
            "bifrost",
            "daemon",
            "--config",
            "/etc/bifrost/daemon.yaml",
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
        let cli = Cli::try_parse_from(["bifrost", "daemon"]);

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
