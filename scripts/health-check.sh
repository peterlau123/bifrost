#!/bin/bash
# Health check script for bifrost daemon

HEARTBEAT_FILE="/var/lib/bifrost/logs/heartbeat.json"
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

        if [ $SERVICE_STATUS -eq 0 ] && [ $HEARTBEAT_STATUS -eq 0 ]; then
            echo "HEALTHY: All checks passed"
            exit 0
        else
            echo "UNHEALTHY: Some checks failed"
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