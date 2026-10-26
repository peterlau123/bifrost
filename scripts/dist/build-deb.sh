#!/bin/bash
# build-deb.sh —— 把已编译好的静态二进制打成 Debian 包（apt install / dpkg -i 均可）
#
# 用法:
#   scripts/dist/build-deb.sh --binary target/x86_64-unknown-linux-musl/release/bifrost
#   scripts/dist/build-deb.sh --binary dist/bifrost-linux-x86_64 --version 0.2.0 --out dist
#
# 产物: <out>/bifrost_<version>-<revision>_<arch>.deb (+ .sha256)
# 包内布局（Debian 惯例）:
#   /usr/bin/bifrost                     静态二进制
#   /usr/bin/bifrost-setup               client/server 角色配置
#   /lib/systemd/system/bifrost.service  服务端 unit（默认不 enable）
#   /etc/default/bifrost                 conffile；BIFROST_SERVER=1 时 postinst 自动起服务
#   /usr/share/bifrost/settings.example.json
#   /usr/share/doc/bifrost/{README.md,copyright}
# 配置策略: /etc/bifrost/settings.json 由 postinst 生成（不打包），避免升级时 conffile 冲突，
#          因为共享盘路径每台机器不同，实际由 `bifrost-setup server --storage ...` 写入。
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
VERSION=""
REVISION="1"
ARCH="amd64"
OUT="${OUT:-$ROOT/dist}"
BINARY=""

while [ $# -gt 0 ]; do
    case "$1" in
        --binary)   BINARY="$2"; shift 2 ;;
        --version)  VERSION="$2"; shift 2 ;;
        --revision) REVISION="$2"; shift 2 ;;
        --arch)     ARCH="$2"; shift 2 ;;
        --out)      OUT="$2"; shift 2 ;;
        -h|--help)  sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "未知参数: $1" >&2; exit 1 ;;
    esac
done

[ -n "$BINARY" ] && [ -x "$BINARY" ] || { echo "FAIL: 需要 --binary <可执行文件>" >&2; exit 1; }
[ -n "$VERSION" ] || VERSION="$(awk -F'"' '/^version[[:space:]]*=/{print $2; exit}' "$ROOT/Cargo.toml")"
command -v dpkg-deb >/dev/null || { echo "FAIL: 需要 dpkg-deb（Debian/Ubuntu 机器自带）" >&2; exit 1; }

# 静态链接是跨机器可用的前提，动态产物也允许，但在包里记一笔
LINK="static"
if command -v ldd >/dev/null && ! ldd "$BINARY" 2>&1 | grep -q "statically linked"; then
    LINK="dynamic"
fi

STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
P="$STAGE/pkg"
mkdir -p "$P/usr/bin" "$P/lib/systemd/system" "$P/etc/default" \
         "$P/usr/share/bifrost" "$P/usr/share/doc/bifrost" "$P/DEBIAN"

install -m 0755 "$BINARY" "$P/usr/bin/bifrost"
install -m 0644 "$HERE/deb/bifrost.service" "$P/lib/systemd/system/bifrost.service"
install -m 0755 "$HERE/deb/bifrost-setup" "$P/usr/bin/bifrost-setup"
install -m 0644 "$HERE/settings.example.json" "$P/usr/share/bifrost/settings.example.json"
install -m 0644 "$HERE/README.md" "$P/usr/share/doc/bifrost/README.md"
install -m 0644 "$ROOT/LICENSE" "$P/usr/share/doc/bifrost/copyright" 2>/dev/null || true

cat > "$P/etc/default/bifrost" <<'EOF'
# bifrost 服务端开关（客户端场景保持 0）
# 改成 1 之后: sudo systemctl enable --now bifrost
BIFROST_SERVER=0
# 共享盘目录（bifrost-setup server 会写进 /etc/bifrost/settings.json）
BIFROST_STORAGE=
EOF

for f in postinst prerm postrm; do
    install -m 0755 "$HERE/deb/$f" "$P/DEBIAN/$f"
done
echo "/etc/default/bifrost" > "$P/DEBIAN/conffiles"

SIZE="$(du -sk "$P" | cut -f1)"
sed -e "s/@VERSION@/${VERSION}-${REVISION}/" \
    -e "s/@ARCH@/$ARCH/" \
    -e "s/@SIZE@/$SIZE/" \
    "$HERE/deb/control.in" > "$P/DEBIAN/control"

mkdir -p "$OUT"
DEB="$OUT/bifrost_${VERSION}-${REVISION}_${ARCH}.deb"
dpkg-deb --build --root-owner-group "$P" "$DEB" >/dev/null
( cd "$OUT" && sha256sum "$(basename "$DEB")" > "$(basename "$DEB").sha256" )

echo "✓ $DEB  ($(du -h "$DEB" | cut -f1), link=$LINK)"
echo "  sha256: $(cut -d' ' -f1 "$DEB.sha256")"
echo
echo "目标机:"
echo "  sudo dpkg -i $(basename "$DEB")     # 或 sudo apt install ./$(basename "$DEB")"
echo "  bifrost-setup client --storage <共享盘>    # 客户端"
echo "  sudo bifrost-setup server --storage <共享盘>   # 服务端"
