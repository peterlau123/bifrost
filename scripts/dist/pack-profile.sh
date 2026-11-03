#!/bin/bash
# pack-profile.sh —— 组装一份 Hermes profile distribution（可 push 到任意 git 仓库）
#
# 产物是一个目录（不是 tar）：
#   dist/profile-bifrost-agent/
#     distribution.yaml
#     config.yaml              mcp_servers.bifrost（裸命令，走 PATH）
#     README.md
#     skills/setup-bifrost/SKILL.md
#     skills/setup-bifrost/scripts/bifrost     ← 二进制藏在 skill 里随包走
#
# 把它 push 到任意 git 仓库后，目标机一条命令：
#   hermes profile install <git-url>
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
OUT="${OUT:-$ROOT/../dist/profile-bifrost-agent}"
export RUSTUP_HOME="${RUSTUP_HOME:-/opt/data/.rustup}"
export CARGO_HOME="${CARGO_HOME:-/opt/data/.cargo}"
export PATH="$CARGO_HOME/bin:$PATH"

echo "→ 构建 musl 二进制"
cargo build --release --target x86_64-unknown-linux-musl --bin bifrost --manifest-path "$ROOT/Cargo.toml"
BIN="$ROOT/target/x86_64-unknown-linux-musl/release/bifrost"
[ -x "$BIN" ] || { echo "FAIL: 缺少构建产物" >&2; exit 1; }

rm -rf "$OUT"
mkdir -p "$OUT/skills/setup-bifrost/scripts"
cp "$HERE/profile/distribution.yaml" "$OUT/"
cp "$HERE/profile/config.yaml"        "$OUT/"
cp "$HERE/profile/README.md"          "$OUT/"
cp "$HERE/profile/skills/setup-bifrost/SKILL.md" "$OUT/skills/setup-bifrost/"
install -m 0755 "$BIN" "$OUT/skills/setup-bifrost/scripts/bifrost"

cat > "$OUT/.gitignore" <<'EOF'
# 用户数据不发布
.env
.env.*
sessions/
memories/
logs/
home/
workspace/
state.db*
EOF

echo "✓ $OUT"
echo
echo "发布（新建空 git 仓库后，URL 按网络选 github / 内网）："
echo "  cd $OUT && git init && git add -A && git commit -m 'bifrost-agent v0.1.0'"
echo "  git remote add origin <git-url> && git push -u origin main"
echo
echo "目标机安装:"
echo "  hermes profile install <git-url>"
