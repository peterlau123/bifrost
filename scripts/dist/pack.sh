#!/bin/bash
# 打可搬运的 bifrost MCP 便携包（静态 musl，免编译安装到其它机器）
#
# 用法:
#   scripts/dist/pack.sh                    # 输出到 ../dist/
#   OUT=/data/liuxin/dist VERSION=0.1.1 scripts/dist/pack.sh
#
# 产物: <OUT>/bifrost-mcp-<ver>-x86_64-linux-musl.tar.gz (+ .sha256)
set -euo pipefail

VERSION="${VERSION:-0.1.0}"
ARCH="x86_64-linux-musl"
HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
OUT="${OUT:-$ROOT/../dist}"

export RUSTUP_HOME="${RUSTUP_HOME:-/opt/data/.rustup}"
export CARGO_HOME="${CARGO_HOME:-/opt/data/.cargo}"
export PATH="$CARGO_HOME/bin:$PATH"

echo "→ 构建 musl 静态二进制"
cargo build --release --target "x86_64-unknown-linux-musl" --bin bifrost --manifest-path "$ROOT/Cargo.toml"
BIN="$ROOT/target/x86_64-unknown-linux-musl/release/bifrost"
[ -x "$BIN" ] || { echo "FAIL: 没找到构建产物 $BIN" >&2; exit 1; }

PKG="bifrost-mcp-$VERSION-$ARCH"
STAGE="$(mktemp -d)/$PKG"
mkdir -p "$STAGE"
cp "$BIN" "$STAGE/bifrost"
cp "$HERE/install.sh" "$HERE/bootstrap.sh" "$HERE/README.md" "$HERE/settings.example.json" "$STAGE/"
SKILL="$ROOT/skills/setup-bifrost/SKILL.md"
[ -f "$SKILL" ] && cp "$SKILL" "$STAGE/SKILL.md"
chmod 0755 "$STAGE/bifrost" "$STAGE/install.sh" "$STAGE/bootstrap.sh"

mkdir -p "$OUT"
tar czf "$OUT/$PKG.tar.gz" -C "$(dirname "$STAGE")" "$PKG"
( cd "$OUT" && sha256sum "$PKG.tar.gz" > "$PKG.tar.gz.sha256" )
rm -rf "$(dirname "$STAGE")"

echo "✓ $OUT/$PKG.tar.gz  ($(du -h "$OUT/$PKG.tar.gz" | cut -f1))"
echo "  sha256: $(cut -d' ' -f1 "$OUT/$PKG.tar.gz.sha256" | cut -c1-32)…"
echo
echo "目标机安装:"
echo "  tar xzf $PKG.tar.gz && cd $PKG && ./install.sh --storage <共享盘路径>"
