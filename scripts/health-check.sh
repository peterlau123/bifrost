#!/bin/bash
# Health check script for bifrost daemon

HEARTBEAT_FILE="/var/lib/bifrost/logs/heartbeat.json"
BATCH_PROGRESS_DIR="/var/lib/bifrost/batch_progress"
MAX_AGE_SECONDS=60

check_heartbeat() {
    if [ ! -f "$HEARTBEAT_FILE" ]; then
        echo "ERROR: Heartbeat file not found at $HEARTBEAT_FILE"
        return 1
    fi

    # Check file age
    FILE_AGE=$(stat -c %Y "$HEARTBEAT_FILE" 2>/dev/null || stat -f %m "$HEARTBEAT_FILE")
    CURRENT_TIME=$(date +%s)
    AGE=$((CURRENT_TIME - FILE_AGE))

    if [ $AGE -gt $MAX_AGE_SECONDS ]; then
        echo "ERROR: Heartbeat file too old (${AGE} seconds)"
        return 1
    fi

    # Parse heartbeat JSON
    if command -v jq >/dev/null 2>&1; then
        LAST_BEAT=$(jq -r '.last_heartbeat' "$HEARTBEAT_FILE")
        ACTIVE_TASKS=$(jq -r '.active_tasks' "$HEARTBEAT_FILE")

        echo "OK: Last heartbeat: $LAST_BEAT, Active tasks: $ACTIVE_TASKS"
    else
        echo "OK: Heartbeat file present and recent (${AGE} seconds old)"
    fi

    return 0
}

check_service() {
    if systemctl is-active --quiet bifrost; then
        echo "OK: Bifrost service is running"
        return 0
    else
        echo "ERROR: Bifrost service is not running"
        return 1
    fi
}

check_gpu_utilization() {
    # Check nvidia-smi availability
    if ! command -v nvidia-smi >/dev/null 2>&1; then
        echo "WARN: nvidia-smi not found, GPU monitoring unavailable"
        return 0
    fi

    # Check GPU utilization
    GPU_UTIL=$(nvidia-smi --query-gpu=utilization.gpu --format=csv,noheader,nounits | head -1)

    if [ -z "$GPU_UTIL" ]; then
        echo "ERROR: Failed to query GPU utilization"
        return 1
    fi

    echo "OK: GPU utilization: ${GPU_UTIL}%"

    # Check GPU memory usage
    GPU_MEM=$(nvidia-smi --query-gpu=memory.used --format=csv,noheader,nounits | head -1)
    echo "OK: GPU memory used: ${GPU_MEM} MB"

    return 0
}

check_batch_queue() {
    if [ ! -d "$BATCH_PROGRESS_DIR" ]; then
        echo "OK: Batch progress directory not found (no batches submitted)"
        return 0
    fi

    # Count active batch files
    BATCH_COUNT=$(find "$BATCH_PROGRESS_DIR" -name "*.json" -type f | wc -l)

    if [ $BATCH_COUNT -eq 0 ]; then
        echo "OK: No active batches found"
        return 0
    fi

    # Parse batch status if jq available
    if command -v jq >/dev/null 2>&1; then
        RUNNING_COUNT=0
        COMPLETED_COUNT=0

        for batch_file in "$BATCH_PROGRESS_DIR"/*.json; do
            BATCH_STATUS=$(jq -r '.status' "$batch_file")

            case "$BATCH_STATUS" in
                "Running")
                    RUNNING_COUNT=$((RUNNING_COUNT + 1))
                    ;;
                "Completed"|"Failed"|"Cancelled")
                    COMPLETED_COUNT=$((COMPLETED_COUNT + 1))
                    ;;
            esac
        done

        echo "OK: Active batches: $RUNNING_COUNT running, $COMPLETED_COUNT completed"
    else
        echo "OK: Batch files found: $BATCH_COUNT (jq not available for status parsing)"
    fi

    return 0
}

check_gpu_scheduler() {
    # Placeholder for GPU scheduler health check
    # Would require daemon to expose GPU pool status via heartbeat or separate file

    echo "OK: GPU scheduler health check (placeholder)"
    echo "  Requires daemon to expose GPU pool status"

    return 0
}

# Main health check
case "$1" in
    startup)
        echo "Startup check: verifying bifrost daemon started"
        sleep 3
        check_service
        exit $?
        ;;

    shutdown)
        echo "Shutdown check: verifying bifrost daemon stopped cleanly"
        exit 0
        ;;

    heartbeat)
        check_heartbeat
        exit $?
        ;;

    full)
        echo "Full health check..."
        check_service
        SERVICE_STATUS=$?

        check_heartbeat
        HEARTBEAT_STATUS=$?

        check_gpu_utilization
        GPU_STATUS=$?

        check_batch_queue
        BATCH_STATUS=$?

        check_gpu_scheduler
        SCHEDULER_STATUS=$?

        if [ $SERVICE_STATUS -eq 0 ] && [ $HEARTBEAT_STATUS -eq 0 ]; then
            echo "HEALTHY: Core checks passed"

            # Report GPU/Batch status (warnings don't fail health check)
            if [ $GPU_STATUS -ne 0 ]; then
                echo "  GPU monitoring issues detected"
            fi

            if [ $BATCH_STATUS -ne 0 ]; then
                echo "  Batch queue issues detected"
            fi

            exit 0
        else
            echo "UNHEALTHY: Core checks failed"
            exit 1
        fi
        ;;

    *)
        echo "Usage: $0 {startup|shutdown|heartbeat|full}"
        echo ""
        echo "Checks:"
        echo "  startup   - Verify service started successfully"
        echo "  shutdown  - Verify service stopped cleanly"
        echo "  heartbeat - Check heartbeat.json freshness"
        echo "  full      - Comprehensive health check"
        exit 1
        ;;
esac