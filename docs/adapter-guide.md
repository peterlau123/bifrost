# Adapter Configuration Guide

Bifrost supports custom task execution through adapters. This guide explains how to configure and create adapters for specialized task types.

## Overview

Adapters extend Bifrost to support specialized execution patterns beyond standard shell commands and pytest tests.

## Built-in Adapters

### Shell Adapter

Default adapter for shell command execution:

```yaml
adapter: shell
command: "your-command-here"
timeout: 300
env_vars:
  VAR1: "value1"
  VAR2: "value2"
```

### Pytest Adapter

Specialized adapter for pytest tests:

```yaml
adapter: pytest
path: "tests/"
timeout: 600
artifacts:
  - report.json
  - coverage.xml
env_vars:
  PYTEST_ADDOPTS: "--verbose"
```

Generated artifacts:
- `report.json` - pytest JSON report
- `coverage.xml` - coverage report (if enabled)
- `stdout.log` - test output
- `stderr.log` - test errors

## Custom Adapters

### Creating a Custom Adapter

1. Create adapter configuration file in `adapters/<name>.yaml`:

```yaml
# adapters/custom_test.yaml
name: custom_test
description: Custom test execution adapter
command_template: "custom-runner --input {input} --output {output}"
timeout: 600
artifacts:
  - results.json
  - logs.txt
env_vars:
  CUSTOM_CONFIG: "/etc/custom/config.yaml"
```

2. Use adapter in task submission:

```bash
bifrost client submit \
  --command "custom_test --input data.csv" \
  --task-type custom \
  --adapter custom_test
```

### Adapter Template Variables

Available template variables:

- `{command}` - Original command from task
- `{input}` - Input file path
- `{output}` - Output directory path
- `{task_id}` - Task UUID
- `{working_dir}` - Task working directory
- `{timeout}` - Task timeout

Example:

```yaml
command_template: "python {working_dir}/run.py --task {task_id} --timeout {timeout}"
```

### Adapter Artifacts

Specify expected artifacts:

```yaml
artifacts:
  - report.json        # JSON report
  - output.txt         # Text output
  - metrics.csv        # Metrics data
  - logs/*.log         # Log files (glob pattern)
```

Bifrost will:
1. Verify artifacts exist after execution
2. Include artifacts in task result
3. Store artifacts in logs directory

### Adapter Environment Variables

Set environment variables for execution:

```yaml
env_vars:
  PATH: "/custom/bin:$PATH"
  PYTHONPATH: "/custom/lib/python"
  DEBUG: "1"
  LOG_LEVEL: "info"
```

## Adapter Examples

### pytest with Coverage

```yaml
name: pytest_coverage
description: pytest test with coverage report
command_template: "pytest {command} --cov=src --cov-report=xml --json-report"
timeout: 600
artifacts:
  - report.json
  - coverage.xml
env_vars:
  PYTEST_JSON_REPORT_FILE: "report.json"
```

### Shell Script Adapter

```yaml
name: shell_script
description: Execute shell scripts with logging
command_template: "bash {command} | tee logs.txt"
timeout: 300
artifacts:
  - logs.txt
env_vars:
  SCRIPT_DIR: "/opt/scripts"
```

### Docker Adapter

```yaml
name: docker_run
description: Execute command in Docker container
command_template: "docker run --rm -v {working_dir}:/work {image} {command}"
timeout: 1200
parameters:
  image: "python:3.9"
artifacts:
  - output/
env_vars:
  DOCKER_OPTS: "--memory=2g"
```

### Custom Test Runner

```yaml
name: custom_runner
description: Custom test execution framework
command_template: "custom-tester run --suite {suite} --output results.json"
timeout: 900
parameters:
  suite: "default"
artifacts:
  - results.json
  - performance_metrics.json
env_vars:
  TESTER_CONFIG: "/etc/custom-tester/config.yaml"
```

## Adapter Configuration

### Global Adapter Configuration

Store adapters in `~/.bifrost/adapters/`:

```
~/.bifrost/adapters/
├── pytest.yaml
├── shell.yaml
├── docker.yaml
└── custom.yaml
```

### Project Adapter Configuration

Project-specific adapters in `<project>/adapters/`:

```
project/
├── adapters/
│   ├── unit_test.yaml
│   └── integration_test.yaml
└── config/
    └── settings.json
```

### Loading Priority

Bifrost loads adapters in order:
1. Built-in adapters (shell, pytest)
2. Project adapters (`./adapters/`)
3. Global adapters (`~/.bifrost/adapters/`)
4. Custom adapters specified in task

## Best Practices

### 1. Timeout Management

Set appropriate timeouts:

```yaml
# Short tests
timeout: 60

# Integration tests
timeout: 600

# Long-running processes
timeout: 3600
```

### 2. Artifact Naming

Use consistent artifact names:

```yaml
artifacts:
  - report.json       # Standard report
  - output.txt        # Standard output
  - errors.log        # Standard error log
```

### 3. Environment Variables

Use clear variable names:

```yaml
env_vars:
  APP_MODE: "testing"
  LOG_FORMAT: "json"
  TIMEOUT_FACTOR: "1.5"
```

### 4. Error Handling

Configure retry behavior:

```yaml
retry_count: 3
retry_delay: 10      # seconds
retry_on_errors:
  - "timeout"
  - "connection_refused"
```

### 5. Resource Limits

Set resource constraints:

```yaml
resources:
  memory_limit: "2G"
  cpu_limit: "80%"
  disk_limit: "10G"
```

## Troubleshooting

### Adapter Not Found

```
Error: Adapter 'custom_test' not found
```

Solution:
1. Verify adapter file exists: `ls adapters/custom_test.yaml`
2. Check file permissions: `chmod 644 adapters/custom_test.yaml`
3. Validate YAML syntax: `cat adapters/custom_test.yaml`

### Artifact Missing

```
Error: Artifact 'report.json' not found
```

Solution:
1. Check command generates artifact
2. Verify working directory: `pwd`
3. Check file permissions
4. Increase timeout if generation is slow

### Timeout Issues

```
Error: Task timed out after 300 seconds
```

Solution:
1. Increase timeout: `timeout: 600`
2. Optimize command execution
3. Check system resources
4. Profile command performance

## Advanced Topics

### Adapter Inheritance

Extend existing adapters:

```yaml
name: pytest_extended
extends: pytest
timeout: 900
artifacts:
  - report.json
  - coverage.xml
  - performance.json
env_vars:
  PYTEST_PLUGINS: "pytest-timeout,pytest-cov"
```

### Conditional Execution

Execute based on conditions:

```yaml
conditions:
  file_exists: "input.txt"
  env_set: "RUN_TESTS"
  min_disk_space: "5G"
```

### Parallel Execution

Execute multiple tasks:

```yaml
parallel_tasks:
  - command: "test unit/"
    timeout: 300
  - command: "test integration/"
    timeout: 600
```

## See Also

- [Deployment Guide](deployment.md) - Production deployment
- [README](../README.md) - Project overview
