#!/bin/bash
# install-supervisor-cron.sh - 安装 crontab @reboot 自启 (H20 上执行一次)
#
# 作用: 系统重启后自动拉起 bifrost-supervisor, 保证 H20 server 长期在线。
# 用法: ./install-supervisor-cron.sh
set -u

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
SUPERVISOR="${SCRIPT_DIR}/bifrost-supervisor.sh"

if [[ ! -x "$SUPERVISOR" ]]; then
    echo "✗ supervisor 脚本不存在或不可执行: $SUPERVISOR"
    exit 1
fi

# 构造 crontab 行
CRON_LINE="@reboot ${SUPERVISOR} start >/dev/null 2>&1"

# 检查是否已存在
if crontab -l 2>/dev/null | grep -q "bifrost-supervisor"; then
    echo "✓ crontab 已配置 bifrost-supervisor 自启"
    crontab -l | grep bifrost-supervisor
else
    (crontab -l 2>/dev/null; echo "$CRON_LINE") | crontab -
    echo "✓ 已安装 @reboot 自启:"
    echo "  $CRON_LINE"
fi

echo ""
echo "验证: crontab -l | grep bifrost"
