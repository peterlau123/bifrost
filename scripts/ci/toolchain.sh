#!/bin/bash
# toolchain.sh —— 保证构建机上的 Rust 足够新（流水线用）
#
# 为什么需要：Gitee Go 的 build@rust 插件版本偏旧（生成模板里是 1.69），而 bifrost 的
# 依赖（rmcp 3 / schemars 1.x）需要更新的 stable。这里先探测，不够就用 rustup 装一份；
# 已经够用就什么都不做（不浪费构建时间）。
#
# 用法: bash scripts/ci/toolchain.sh [--min 1.85]
set -uo pipefail

MIN="1.80"
[[ "${1:-}" == "--min" ]] && MIN="${2:-1.80}"
NEED_MAJOR="${MIN%%.*}"
NEED_MINOR="${MIN#*.}"

version_ok() {   # $1=x.y.z
    local v="$1"
    [ -n "$v" ] || return 1
    local maj="${v%%.*}" rest="${v#*.}" min="${rest%%.*}"
    [ "$maj" -gt "$NEED_MAJOR" ] && return 0
    [ "$maj" -eq "$NEED_MAJOR" ] && [ "$min" -ge "$NEED_MINOR" ] && return 0
    return 1
}

cur="$(cargo --version 2>/dev/null | awk '{print $2}')"
if version_ok "$cur"; then
    echo "[toolchain] rust $cur 够用（>= $MIN）"
else
    echo "[toolchain] rust ${cur:-缺失} < $MIN → 安装 stable"
    curl -sSf --max-time 120 https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable >/dev/null 2>&1 \
        || { echo "FAIL: rustup 安装失败（构建机出网受限？）" >&2; exit 1; }
    # shellcheck disable=SC1091
    . "$HOME/.cargo/env"
    echo "[toolchain] 现在: $(cargo --version)"
fi

rustup component add rustfmt clippy >/dev/null 2>&1 || true
# musl 静态目标：rust-lld 自带，不需要 musl-gcc
rustup target add x86_64-unknown-linux-musl >/dev/null 2>&1 || \
    echo "[toolchain] ! musl 目标安装失败，pack.sh 会退回 gnu 构建"

# 可选：国内构建机走镜像（流水线参数里设 CARGO_REGISTRY_MIRROR=rsproxy 即可）
if [ -n "${CARGO_REGISTRY_MIRROR:-}" ] && [ ! -f "${CARGO_HOME:-$HOME/.cargo}/config.toml" ]; then
    mkdir -p "${CARGO_HOME:-$HOME/.cargo}"
    cat > "${CARGO_HOME:-$HOME/.cargo}/config.toml" <<'EOF'
[source.crates-io]
replace-with = "mirror"
[source.mirror]
registry = "sparse+https://rsproxy.cn/index/"
EOF
    echo "[toolchain] cargo 已切到镜像 $CARGO_REGISTRY_MIRROR"
fi

cargo --version; rustc --version
