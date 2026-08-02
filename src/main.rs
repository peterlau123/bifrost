// CLI entry point with client/server mode selection
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

    /// Server mode - execute tasks from shared storage
    Server {
        /// Initialize settings then exit
        #[arg(long)]
        init: bool,

        /// Configuration file path
        #[arg(short, long, value_name = "FILE")]
        config: Option<PathBuf>,

        /// Run as systemd service
        #[arg(long)]
        systemd: bool,
    },

    /// MCP server mode - expose bifrost as MCP tools over stdio
    McpServe {
        /// Configuration file path
        #[arg(short, long, value_name = "FILE")]
        config: Option<PathBuf>,
    },
}

/// Client commands
#[derive(Subcommand, Debug)]
enum ClientCommand {
    /// Submit a task or job for execution
    Submit {
        /// Command to execute (shell command)
        #[arg(long, conflicts_with = "job")]
        command: Option<String>,

        /// Job definition file (YAML, multi-task workflow)
        #[arg(long, conflicts_with = "command")]
        job: Option<PathBuf>,

        /// Timeout in seconds
        #[arg(long, default_value = "300")]
        timeout: u64,

        /// Task priority (0-255, lower is higher)
        #[arg(short = 'p', long, default_value = "0")]
        priority: u8,

        /// Working directory
        #[arg(short = 'w', long)]
        working_dir: Option<PathBuf>,
    },

    /// Check task or job status
    Status {
        /// Task or job ID
        id: String,
    },

    /// Cancel a running task or job
    Cancel {
        /// Task or job ID
        id: String,
    },

    /// Clean up files of finished tasks older than the age threshold
    Clean {
        /// Only remove files older than N days (default 7)
        #[arg(long, default_value = "7")]
        older_than: u64,

        /// Preview what would be removed without deleting anything
        #[arg(long)]
        dry_run: bool,

        /// Storage directory to clean (default: settings.json's shared_storage)
        #[arg(long, value_name = "PATH")]
        storage: Option<PathBuf>,
    },
}

fn main() {
    let cli = Cli::parse();

    match cli.mode {
        Mode::Client { command } => handle_client_mode(command),
        Mode::Server {
            init,
            config,
            systemd,
        } => {
            if init {
                match bifrost::core::settings::init() {
                    Ok(p) => println!("Settings saved to {}", p.display()),
                    Err(e) => eprintln!("{}", e),
                }
                return;
            }
            handle_server_mode(config, systemd);
        }
        Mode::McpServe { config } => handle_mcp_serve(config),
    }
}

/// Handle MCP server mode: expose bifrost tools over stdio
/// Config resolution order: BIFROST_CONFIG env > -c flag > ~/.bifrost/settings.json > defaults
fn handle_mcp_serve(config: Option<PathBuf>) {
    use bifrost::core::settings;

    // BIFROST_CONFIG env lets any MCP client (OpenCode, Claude Code, etc.)
    // point the server at a specific storage without a shared home dir.
    let config = config.or_else(|| std::env::var("BIFROST_CONFIG").ok().map(PathBuf::from));

    let settings = if let Some(ref config_path) = config {
        match std::fs::read_to_string(config_path) {
            Ok(content) => match serde_json::from_str::<settings::BifrostSettings>(&content) {
                Ok(s) => s,
                Err(_) => {
                    eprintln!("Bad config, using defaults");
                    settings::BifrostSettings::defaults()
                }
            },
            Err(_) => {
                eprintln!("Cannot read config, using defaults");
                settings::BifrostSettings::defaults()
            }
        }
    } else {
        settings::load()
    };
    let rt = tokio::runtime::Runtime::new().expect("failed to start tokio runtime");
    match rt.block_on(bifrost::mcp_server::run(settings)) {
        Ok(()) => {}
        Err(e) => {
            eprintln!("MCP server error: {}", e);
            std::process::exit(1);
        }
    }
}

/// Handle client mode operations
fn handle_client_mode(command: ClientCommand) {
    use bifrost::core::bridge::Bridge;
    use bifrost::core::job::load_job;
    use bifrost::core::models::TaskType;
    use bifrost::core::protocol::Protocol;
    use bifrost::core::settings;

    let settings = settings::load();
    let shared_storage = settings.shared_storage.clone();

    match command {
        ClientCommand::Submit {
            command: cmd,
            job,
            timeout,
            priority,
            working_dir,
        } => {
            let protocol = Protocol::new(shared_storage).expect("Failed to create bridge");
            let bridge: &dyn Bridge = &protocol;

            if let Some(job_path) = job {
                // Submit job from YAML
                match load_job(&job_path) {
                    Ok(job_def) => match bifrost::client::launcher::launch_job(bridge, job_def) {
                        Ok(job_result) => {
                            println!();
                            println!("{}", serde_json::to_string_pretty(&job_result).unwrap());
                        }
                        Err(e) => eprintln!("Job failed: {}", e),
                    },
                    Err(e) => eprintln!("Bad job file: {}", e),
                }
            } else if let Some(cmd_str) = cmd {
                // Submit single command
                let task_type = if cmd_str.trim_start().starts_with("pytest") {
                    TaskType::Pytest
                } else {
                    TaskType::Shell
                };

                let submit_start = std::time::Instant::now();
                match bifrost::client::submit::submit_task(
                    bridge,
                    cmd_str,
                    task_type,
                    priority,
                    timeout,
                    working_dir,
                ) {
                    Ok(task_id) => {
                        let submit_elapsed = submit_start.elapsed();
                        println!("Task submitted successfully");
                        println!("  Task ID: {}", task_id);
                        println!("  Status: Pending");
                        println!(
                            "  Submit time: {:.2}ms",
                            submit_elapsed.as_secs_f64() * 1000.0
                        );
                    }
                    Err(e) => eprintln!("Failed to submit task: {}", e),
                }
            } else {
                eprintln!("Error: must provide either --command or --job");
                eprintln!("  Usage: bifrost client submit --command <shell>");
                eprintln!("         bifrost client submit --job <job.yaml>");
            }
        }

        ClientCommand::Status { id } => {
            use uuid::Uuid;

            let protocol = Protocol::new(shared_storage).expect("Failed to create bridge");
            let bridge: &dyn Bridge = &protocol;

            match Uuid::parse_str(&id) {
                Ok(parsed_id) => match bifrost::client::status::query_status(bridge, parsed_id) {
                    Ok(status_resp) => {
                        println!("Task status for: {}", id);
                        println!("  Status: {}", status_resp.status);
                        if let Some(msg) = status_resp.message {
                            println!("  Message: {}", msg);
                        }
                    }
                    Err(e) => eprintln!("Failed to query status: {}", e),
                },
                Err(e) => eprintln!("Invalid task ID format: {}", e),
            }
        }

        ClientCommand::Cancel { id } => {
            println!("Cancel: {}", id);
            println!("  Note: Task cancellation not yet implemented");
            println!("  Requires server to support cancel signal");
        }

        ClientCommand::Clean {
            older_than,
            dry_run,
            storage,
        } => {
            use std::time::SystemTime;
            let storage = storage.unwrap_or_else(|| PathBuf::from(&shared_storage));
            let cands = match bifrost::client::clean::scan_finished(
                &storage,
                older_than,
                SystemTime::now(),
            ) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("Clean failed: {}", e);
                    return;
                }
            };

            if cands.is_empty() {
                println!(
                    "No finished tasks older than {} days found. Storage is clean.",
                    older_than
                );
                return;
            }

            let mut file_count = 0usize;
            for c in &cands {
                let mut n = 1; // result file
                n += c.status.iter().count()
                    + c.commands.len()
                    + c.artifacts.len()
                    + c.logs.iter().count();
                file_count += n;
                if dry_run {
                    println!("  would remove {} ({} files)", c.task_id, n);
                }
            }

            if dry_run {
                println!(
                    "[dry-run] {} finished tasks, {} files would be removed (older than {} days)",
                    cands.len(),
                    file_count,
                    older_than
                );
                return;
            }

            match bifrost::client::clean::purge(&cands) {
                Ok(removed) => println!(
                    "Removed {} files from {} finished tasks (older than {} days)",
                    removed,
                    cands.len(),
                    older_than
                ),
                Err(e) => eprintln!("Clean failed: {}", e),
            }
        }
    }
}

/// Handle server mode operations
fn handle_server_mode(config: Option<PathBuf>, systemd: bool) {
    use bifrost::core::settings;
    use bifrost::daemon::runner::run_server;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    println!("Starting bifrost server...");

    let settings = if let Some(ref config_path) = config {
        match std::fs::read_to_string(config_path) {
            Ok(content) => {
                match serde_json::from_str::<bifrost::core::settings::BifrostSettings>(&content) {
                    Ok(s) => s,
                    Err(_) => {
                        eprintln!("Bad config, using defaults");
                        bifrost::core::settings::BifrostSettings::defaults()
                    }
                }
            }
            Err(_) => {
                eprintln!("Cannot read config, using defaults");
                bifrost::core::settings::BifrostSettings::defaults()
            }
        }
    } else {
        settings::load()
    };

    if systemd {
        println!("  Mode: systemd service");
    }

    println!("  Shared storage: {}", settings.shared_storage.display());

    let shutdown = Arc::new(AtomicBool::new(false));

    let rt = tokio::runtime::Runtime::new().expect("Failed to create runtime");
    rt.block_on(async {
        // tokio::signal::ctrl_c() integrates with tokio's select! for clean shutdown
        let sd = shutdown.clone();
        tokio::spawn(async move {
            tokio::signal::ctrl_c().await.ok();
            eprintln!("\nShutdown requested...");
            sd.store(true, Ordering::SeqCst);
        });

        if let Err(e) = run_server(settings, shutdown).await {
            eprintln!("Server error: {}", e);
        }
    });

    println!("Server exited.");
    // Force exit: spawned heartbeat/task futures may still be alive
    // and block runtime drop. We're done — just leave.
    std::process::exit(0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cli_parse_submit_command() {
        let cli = Cli::try_parse_from([
            "bifrost",
            "client",
            "submit",
            "--command",
            "pytest tests/",
            "--timeout",
            "600",
        ]);

        assert!(cli.is_ok());
        let cli = cli.unwrap();
        match cli.mode {
            Mode::Client { command } => match command {
                ClientCommand::Submit {
                    command: cmd,
                    job,
                    timeout,
                    priority,
                    working_dir: _,
                } => {
                    assert_eq!(cmd.unwrap(), "pytest tests/");
                    assert!(job.is_none());
                    assert_eq!(timeout, 600);
                    assert_eq!(priority, 0);
                }
                _ => panic!("Expected Submit command"),
            },
            _ => panic!("Expected Client mode"),
        }
    }

    #[test]
    fn test_cli_parse_submit_job() {
        let cli = Cli::try_parse_from([
            "bifrost",
            "client",
            "submit",
            "--job",
            "examples/smoke.yaml",
        ]);

        assert!(cli.is_ok());
        let cli = cli.unwrap();
        match cli.mode {
            Mode::Client { command } => match command {
                ClientCommand::Submit {
                    command: cmd, job, ..
                } => {
                    assert!(cmd.is_none());
                    assert_eq!(job.unwrap(), PathBuf::from("examples/smoke.yaml"));
                }
                _ => panic!("Expected Submit command"),
            },
            _ => panic!("Expected Client mode"),
        }
    }

    #[test]
    fn test_cli_parse_status() {
        let cli = Cli::try_parse_from([
            "bifrost",
            "client",
            "status",
            "550e8400-e29b-41d4-a716-446655440000",
        ]);

        assert!(cli.is_ok());
        let cli = cli.unwrap();
        match cli.mode {
            Mode::Client { command } => match command {
                ClientCommand::Status { id } => {
                    assert_eq!(id, "550e8400-e29b-41d4-a716-446655440000");
                }
                _ => panic!("Expected Status command"),
            },
            _ => panic!("Expected Client mode"),
        }
    }

    #[test]
    fn test_cli_parse_cancel() {
        let cli = Cli::try_parse_from([
            "bifrost",
            "client",
            "cancel",
            "550e8400-e29b-41d4-a716-446655440000",
        ]);

        assert!(cli.is_ok());
        let cli = cli.unwrap();
        match cli.mode {
            Mode::Client { command } => match command {
                ClientCommand::Cancel { id } => {
                    assert_eq!(id, "550e8400-e29b-41d4-a716-446655440000");
                }
                _ => panic!("Expected Cancel command"),
            },
            _ => panic!("Expected Client mode"),
        }
    }

    #[test]
    fn test_cli_parse_server() {
        let cli = Cli::try_parse_from(["bifrost", "server"]);

        assert!(cli.is_ok());
        let cli = cli.unwrap();
        match cli.mode {
            Mode::Server {
                init,
                config,
                systemd,
            } => {
                assert!(!init);
                assert!(config.is_none());
                assert!(!systemd);
            }
            _ => panic!("Expected Server mode"),
        }
    }

    #[test]
    fn test_cli_parse_server_with_config() {
        let cli = Cli::try_parse_from([
            "bifrost",
            "server",
            "--config",
            "/etc/bifrost/server.json",
            "--systemd",
        ]);

        assert!(cli.is_ok());
        let cli = cli.unwrap();
        match cli.mode {
            Mode::Server {
                init: _,
                config,
                systemd,
            } => {
                assert_eq!(config.unwrap(), PathBuf::from("/etc/bifrost/server.json"));
                assert!(systemd);
            }
            _ => panic!("Expected Server mode"),
        }
    }

    #[test]
    fn test_cli_parse_submit_command_and_job_conflict() {
        let cli = Cli::try_parse_from([
            "bifrost",
            "client",
            "submit",
            "--command",
            "echo hi",
            "--job",
            "job.yaml",
        ]);
        assert!(cli.is_err());
    }
}
