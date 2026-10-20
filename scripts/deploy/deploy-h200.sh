#!/bin/bash
# deploy-h200.sh —— H200 预设：等于 deploy-server.sh 带上本机的默认路径。
#
# 保留这个入口是因为历史命令和文档里都在用它；真正的实现只有一份，
# 在 scripts/deploy/deploy-server.sh。多传的参数会覆盖这里的默认值。
#
#   scripts/deploy/deploy-h200.sh                       # 用仓库里的 musl 产物部署 + 重启
#   scripts/deploy/deploy-h200.sh --binary /tmp/b       # 换二进制
#   scripts/deploy/deploy-h200.sh --no-systemd          # 不起 systemd（调试）
set -uo pipefail
REPO="$(cd "$(dirname "$0")/../.." && pwd)"
exec bash "$REPO/scripts/deploy/deploy-server.sh" \
    --storage /data/liuxin/bifrost \
    --binary "$REPO/target/x86_64-unknown-linux-musl/release/bifrost" \
    "$@"
