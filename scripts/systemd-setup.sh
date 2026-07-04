#!/bin/bash
# systemd deployment script for bifrost daemon

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

echo -e "${GREEN}Bifrost systemd Deployment Script${NC}"
echo "========================================"

# Check if running as root
if [ "$EUID" -ne 0 ]; then
    echo -e "${RED}Error: This script must be run as root${NC}"
    echo "Usage: sudo ./systemd-setup.sh"
    exit 1
fi

# Configuration
BINARY_NAME="bifrost"
INSTALL_DIR="/usr/local/bin"
CONFIG_DIR="/etc/bifrost"
DATA_DIR="/var/lib/bifrost"
SERVICE_NAME="bifrost.service"

# Step 1: Build release binary
echo -e "${YELLOW}[1/6] Building release binary...${NC}"
cargo build --release
BINARY_PATH="target/release/${BINARY_NAME}"

if [ ! -f "$BINARY_PATH" ]; then
    echo -e "${RED}Error: Binary not found at ${BINARY_PATH}${NC}"
    exit 1
fi

# Step 2: Install binary
echo -e "${YELLOW}[2/6] Installing binary to ${INSTALL_DIR}...${NC}"
cp "$BINARY_PATH" "${INSTALL_DIR}/${BINARY_NAME}"
chmod +x "${INSTALL_DIR}/${BINARY_NAME}"
echo -e "${GREEN}✓ Binary installed${NC}"

# Step 3: Create configuration directory
echo -e "${YELLOW}[3/6] Creating configuration directory...${NC}"
mkdir -p "$CONFIG_DIR"

# Copy default config if exists
if [ -f "config/daemon.yaml" ]; then
    cp config/daemon.yaml "${CONFIG_DIR}/daemon.yaml"
    echo -e "${GREEN}✓ Configuration installed${NC}"
else
    echo -e "${YELLOW}⚠ No default config found, creating minimal config${NC}"
    cat > "${CONFIG_DIR}/daemon.yaml" <<EOF
# Bifrost daemon configuration
shared_storage: "${DATA_DIR}"
log_level: "info"
poll_interval: 5
max_concurrent_tasks: 4
default_timeout: 3600
EOF
fi

# Step 4: Create data directory
echo -e "${YELLOW}[4/6] Creating data directory...${NC}"
mkdir -p "$DATA_DIR/pending"
mkdir -p "$DATA_DIR/results"
mkdir -p "$DATA_DIR/completed"
mkdir -p "$DATA_DIR/logs"

# Set permissions
chown -R root:root "$DATA_DIR"
chmod -R 755 "$DATA_DIR"
echo -e "${GREEN}✓ Data directories created${NC}"

# Step 5: Install systemd service
echo -e "${YELLOW}[5/6] Installing systemd service...${NC}"

if [ ! -f "$SERVICE_NAME" ]; then
    echo -e "${RED}Error: Service file ${SERVICE_NAME} not found${NC}"
    exit 1
fi

cp "$SERVICE_NAME" "/etc/systemd/system/${SERVICE_NAME}"
systemctl daemon-reload
echo -e "${GREEN}✓ systemd service installed${NC}"

# Step 6: Enable and start service
echo -e "${YELLOW}[6/6] Enabling and starting service...${NC}"
systemctl enable bifrost
systemctl start bifrost

# Check status
sleep 2
if systemctl is-active --quiet bifrost; then
    echo -e "${GREEN}✓ Bifrost service started successfully${NC}"
    systemctl status bifrost --no-pager
else
    echo -e "${RED}Error: Bifrost service failed to start${NC}"
    systemctl status bifrost --no-pager
    exit 1
fi

echo ""
echo -e "${GREEN}========================================${NC}"
echo -e "${GREEN}Deployment Complete!${NC}"
echo ""
echo "Installation Summary:"
echo "  Binary:    ${INSTALL_DIR}/${BINARY_NAME}"
echo "  Config:    ${CONFIG_DIR}/daemon.yaml"
echo "  Data:      ${DATA_DIR}"
echo "  Service:   ${SERVICE_NAME}"
echo ""
echo "Usage:"
echo "  Status:    systemctl status bifrost"
echo "  Stop:      systemctl stop bifrost"
echo "  Restart:   systemctl restart bifrost"
echo "  Logs:      journalctl -u bifrost -f"
echo ""
echo -e "${YELLOW}Next steps:${NC}"
echo "  1. Edit ${CONFIG_DIR}/daemon.yaml to customize configuration"
echo "  2. Run 'bifrost client submit --command <cmd>' to submit tasks"
echo "  3. Check logs with 'journalctl -u bifrost'"