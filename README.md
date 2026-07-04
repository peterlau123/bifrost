# Bifrost - Offline Machine Command Execution Framework

Bifrost is a Rust-based framework for executing machine commands in offline environments with air-gapped separation between client and daemon machines.

## Overview

Bifrost enables task submission on a client machine (connected network) and execution on a daemon machine (offline/disconnected). Tasks and results are synchronized via portable storage (USB drives, network shares, or synchronized directories).

### Key Features

- **Air-gapped operation**: Complete separation between client and daemon
- **Multiple task types**: Shell commands, pytest tests, custom adapters
- **Priority scheduling**: Tasks sorted by priority (0-255)
- **Timeout management**: Configurable timeouts per task
- **Retry support**: Automatic retry with configurable count
- **Comprehensive logging**: stdout, stderr, and execution metadata
- **systemd integration**: Production-ready daemon deployment
- **Health monitoring**: Heartbeat-based health checks

## Architecture

```
┌─────────────┐         ┌──────────────┐         ┌─────────────┐
│   Client    │  USB/   │ Shared       │  USB/   │   Daemon    │
│   Machine   │───────>│ Storage      │<───────│   Machine   │
│  (Online)   │  Sync  │ (Portable)   │  Sync  │  (Offline)  │
└─────────────┘         └──────────────┘         └─────────────┘
      │                        │                        │
      │                        │                        │
  ┌───▼───┐              ┌────▼────┐              ┌───▼───┐
  │Submit │              │ Pending │              │Watcher│
  │Tasks  │              │ Results │              │Execute│
  └───────┘              │Completed│              │Logs   │
                         └─────────┘              └───────┘
```

### Workflow

1. **Client** submits tasks to `pending/` directory
2. **Daemon** watches for new tasks in `pending/`
3. **Executor** runs tasks with timeout/retry management
4. **Results** written to `results/` directory
5. **Completed** tasks moved to `completed/`
6. **Client** retrieves results from `results/`

## Quick Start

### Installation

```bash
# Clone repository
git clone https://github.com/your-org/bifrost.git
cd bifrost

# Build release binary
cargo build --release

# Install (requires root)
sudo ./scripts/systemd-setup.sh
```

### Client Usage

Submit a task:
```bash
# Shell command
bifrost client submit --command "python script.py" --task-type shell --timeout 300

# Pytest test
bifrost client pytest --path tests/ --timeout 600
```

Check task status:
```bash
bifrost client status --task-id <TASK_ID>
```

Retrieve results:
```bash
bifrost client results --task-id <TASK_ID> --format json
```

### Daemon Usage

Start daemon manually:
```bash
bifrost daemon --config /etc/bifrost/daemon.yaml
```

Start as systemd service:
```bash
sudo systemctl start bifrost
sudo systemctl status bifrost
```

## Configuration

### Daemon Configuration (`/etc/bifrost/daemon.yaml`)

```yaml
# Bifrost daemon configuration
shared_storage: "/var/lib/bifrost"
log_level: "info"
poll_interval: 5          # seconds
max_concurrent_tasks: 4
default_timeout: 3600     # seconds
```

### Task Priority

Priority ranges from 0 to 255:
- **0-10**: High priority (critical tasks)
- **11-50**: Normal priority (default tasks)
- **51-100**: Low priority (background tasks)
- **101-255**: Lowest priority (maintenance tasks)

## Task Types

### Shell

Execute any shell command:
```bash
bifrost client submit --command "rsync -av src/ dest/" --task-type shell
```

### Pytest

Run pytest tests with automatic JSON report:
```bash
bifrost client pytest --path tests/unit/ --timeout 600
```

Generated artifacts:
- `report.json` - pytest JSON report
- `stdout.log` - test output
- `stderr.log` - test errors

### Custom

Adapter-based custom execution (see [Adapter Guide](docs/ADAPTER_GUIDE.md)).

## Directory Structure

```
/var/lib/bifrost/
├── pending/         # Tasks waiting for execution
│   └── <task_id>.json
├── results/         # Task execution results
│   └── <task_id>.json
├── completed/       # Completed task metadata
│   └── <task_id>.json
└── logs/            # Execution logs
    └── <task_id>/
        ├── stdout.log
        ├── stderr.log
        ├── metadata.json
        └── heartbeat.json
```

## Logs and Monitoring

### Execution Logs

Each task generates detailed logs:
- `stdout.log` - Command output
- `stderr.log` - Command errors
- `metadata.json` - Execution metadata (timing, exit code)

### systemd Logs

View daemon logs:
```bash
journalctl -u bifrost -f
```

### Health Checks

Check daemon health:
```bash
/usr/local/bin/bifrost-health-check.sh full
```

## Development

### Running Tests

```bash
# All tests
cargo test --all

# Unit tests only
cargo test --lib

# Integration tests
cargo test --test full_workflow_test

# Coverage (requires tarpaulin)
cargo tarpaulin --out Html
```

### Project Structure

```
bifrost/
├── src/
│   ├── core/           # Core models and protocol
│   ├── client/         # Client submission and query
│   ├── daemon/         # Daemon executor and watcher
│   └── main.rs         # CLI entry point
├── tests/
│   ├── unit/           # Unit tests
│   └── integration/    # Integration tests
├── adapters/           # Task adapters
├── config/             # Default configurations
├── scripts/            # Deployment scripts
└── docs/               # Documentation
```

## Documentation

- [Adapter Configuration Guide](docs/ADAPTER_GUIDE.md) - Custom task adapters
- [Deployment Guide](docs/DEPLOYMENT.md) - Production deployment
- [Architecture Details](docs/ARCHITECTURE.md) - Technical architecture

## Contributing

Contributions welcome! See [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines.

## License

MIT License - see [LICENSE](LICENSE) file.

## Status

Bifrost is in early development (v0.1.0). Core functionality is stable and tested, but APIs may evolve.

## Support

- GitHub Issues: https://github.com/your-org/bifrost/issues
- Documentation: https://github.com/your-org/bifrost/docs