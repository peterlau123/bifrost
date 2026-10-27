#!/bin/bash
# toolchain.sh —— 保证构建机上的 Rust 足够新（流水线用）
#
# 为什么需要：Gitee Go 的 build@rust 插件版本偏旧（生成模板里是 1.69），而
#   - bifrost 的依赖（rmcp 3 / schemars 1.x）需要更新的 stable
#   - Cargo.lock 是 version 4（cargo >= 1.78 才认识），1.69 连 lock 都读不了，
#     会直接 "failed to parse lock file ... perhaps Cargo needs to be updated?"
# 这里先探测，不够就用 rustup 装一份 stable；装完**硬校验**版本，不达标就红。
#
# 用法: bash scripts/ci/toolchain.sh [--min 1.85]
#       RUST_MIN_VERSION=1.85 bash scripts/ci/toolchain.sh
set -uo pipefail

MIN="1.80"
if [ "${1:-}" = "--min" ]; then MIN="${2:-1.80}"; fi
MIN="${RUST_MIN_VERSION:-$MIN}"

NEED_MAJOR="${MIN%%.*}"
NEED_REST="${MIN#*.}"
NEED_MINOR="${NEED_REST%%.*}"

say() { echo "[toolchain] $*"; }

# 注意：不能写成 local a=".." b="${a}.." —— bash 会先把整条命令里的变量展开完再赋值，
# set -u 下会报 "b: unbound variable"。必须拆成多条赋值（2026-10-27 CI 就是崩在这里）。
version_ok() {
    local v="${1:-}"
    [ -n "$v" ] || return 1
    local maj
    local rest
    local min
    maj="${v%%.*}"
    rest="${v#*.}"
    min="${rest%%.*}"
    # 非数字（比如 "unknown"）直接判不合格
    case "${maj}${min}" in *[!0-9]*) return 1 ;; esac
    [ "$maj" -gt "$NEED_MAJOR" ] && return 0
    [ "$maj" -eq "$NEED_MAJOR" ] && [ "$min" -ge "$NEED_MINOR" ] && return 0
    return 1
}

# rustup 装完在 ~/.cargo/bin；流水线每条 command 是独立 shell，所以这里也要显式加 PATH
export PATH="$HOME/.cargo/bin:$PATH"

cur="$(cargo --version 2>/dev/null | awk '{print $2}')"
say "当前 cargo: ${cur:-缺失}（要求 >= $MIN）"

if version_ok "$cur"; then
    say "rust $cur 够用，跳过安装"
else
    say "rust ${cur:-缺失} < $MIN → 安装 stable"
    if command -v rustup >/dev/null 2>&1; then
        rustup toolchain install stable --profile minimal 2>&1 | tail -3 || true
        rustup default stable 2>&1 | tail -1 || true
    else
        curl -sSf --max-time 180 https://sh.rustup.rs \
            | sh -s -- -y --profile minimal --default-toolchain stable 2>&1 | tail -3 \
            || { echo "FAIL: rustup 安装失败（构建机出网受限？）" >&2; exit 1; }
        # shellcheck disable=SC1091
        . "$HOME/.cargo/env" 2>/dev/null || true
        export PATH="$HOME/.cargo/bin:$PATH"
    fi
    hash -r 2>/dev/null || true
fi

# ── 硬校验：装完还是旧的就直接失败，不让后面的 clippy/test 用错工具链 ──
cur="$(cargo --version 2>/dev/null | awk '{print $2}')"
if ! version_ok "$cur"; then
    echo "FAIL: 工具链仍是 cargo ${cur:-缺失}（要求 >= $MIN）。" >&2
    echo "      Cargo.lock 是 version 4，旧 cargo 会报 'failed to parse lock file'。" >&2
    echo "      排查：构建机能否访问 sh.rustup.rs；或把流水线插件的 rustVersion 调高。" >&2
    exit 1
fi
say "工具链就绪: cargo $cur / $(rustc --version 2>&1)"

rustup component add rustfmt clippy >/dev/null 2>&1 || say "! rustfmt/clippy 组件安装失败（后面 fmt/clippy 会报错）"

# musl 静态目标：rust-lld 自带，不需要 musl-gcc
rustup target add x86_64-unknown-linux-musl >/dev/null 2>&1 \
    || say "! musl 目标安装失败，pack.sh 会退回 gnu 构建"

# 可选：国内构建机走镜像（流水线参数里设 CARGO_REGISTRY_MIRROR=rsproxy 即可）
if [ -n "${CARGO_REGISTRY_MIRROR:-}" ] && [ ! -f "${CARGO_HOME:-$HOME/.cargo}/config.toml" ]; then
    mkdir -p "${CARGO_HOME:-$HOME/.cargo}"
    cat > "${CARGO_HOME:-$HOME/.cargo}/config.toml" <<'EOF'
[source.crates-io]
replace-with = "mirror"
[source.mirror]
registry = "sparse+https://rsproxy.cn/index/"
EOF
    say "cargo 已切到镜像 $CARGO_REGISTRY_MIRROR"
fi

cargo --version
rustc --version
