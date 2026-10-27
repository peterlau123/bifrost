#!/bin/bash
# pack.sh —— 打 Linux 安装包（client 与 server 同一个包，装的时候用 --role 选）
#
# 产物:
#   <out>/bifrost-<version>-x86_64-linux-musl.tar.gz
#   <out>/bifrost-<version>-x86_64-linux-musl.tar.gz.sha256
#
# 用法:
#   scripts/dist/pack.sh                       # 编 musl 静态版 → 打包到 ../dist
#   OUT=/tmp/rel VERSION=0.2.0 scripts/dist/pack.sh
#   scripts/dist/pack.sh --binary target/x86_64-unknown-linux-musl/release/bifrost --skip-build
#   scripts/dist/pack.sh --target x86_64-unknown-linux-gnu    # 没有 musl 工具链时的退路
#   scripts/dist/pack.sh --label latest              # main 分支快照包（bifrost-latest-... tar.gz + .deb）
#
# 默认产物是静态 musl（ldd 显示 "statically linked"），目标机不需要 glibc/工具链。
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
OUT="${OUT:-$ROOT/dist}"
VERSION=""
TARGET="x86_64-unknown-linux-musl"
SKIP_BUILD=0
BINARY=""
WITH_DEB=1
LABEL=""

while [ $# -gt 0 ]; do
    case "$1" in
        --version)    VERSION="$2"; shift 2 ;;
        --out)        OUT="$2"; shift 2 ;;
        --target)     TARGET="$2"; shift 2 ;;
        --binary)     BINARY="$2"; SKIP_BUILD=1; shift 2 ;;
        --skip-build) SKIP_BUILD=1; shift ;;
        --no-deb)     WITH_DEB=0; shift ;;
        --label)      LABEL="$2"; shift 2 ;;
        -h|--help)    sed -n '2,19p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "未知参数: $1" >&2; exit 1 ;;
    esac
done

# 版本以 Cargo.toml 为准（CI 的 tag 也从这里来），可用 --version 覆盖
if [ -z "$VERSION" ]; then
    VERSION="$(awk -F'"' '/^version[[:space:]]*=/{print $2; exit}' "$ROOT/Cargo.toml")"
fi
[ -n "$VERSION" ] || { echo "FAIL: 取不到版本号（Cargo.toml）" >&2; exit 1; }

# 构建环境：只有路径存在时才注入，避免覆盖 CI/宿主自己的工具链
[ -d /opt/data/.rustup ] && export RUSTUP_HOME="${RUSTUP_HOME:-/opt/data/.rustup}"
[ -d /opt/data/.cargo ]  && export CARGO_HOME="${CARGO_HOME:-/opt/data/.cargo}"
case ":$PATH:" in *":${CARGO_HOME:-/nonexistent}/bin:"*) ;; *) [ -d "${CARGO_HOME:-}" ] && export PATH="$CARGO_HOME/bin:$PATH" ;; esac

if [ "$SKIP_BUILD" = "0" ]; then
    echo "→ 构建 $TARGET（$VERSION）"
    if [ "$TARGET" = "x86_64-unknown-linux-musl" ]; then
        rustup target add x86_64-unknown-linux-musl >/dev/null 2>&1 || true
    fi
    BINARY="$(cd "$ROOT" && bash scripts/build/build.sh --target "$TARGET" 2>/dev/null | tail -1)"
fi
[ -n "$BINARY" ] && [ -x "$BINARY" ] || { echo "FAIL: 找不到可执行产物（$BINARY）" >&2; exit 1; }

# 交付门槛：必须是静态链接（不是就看得到 glibc 依赖，换机器会踩 ABI）
if command -v ldd >/dev/null; then
    if ldd "$BINARY" 2>&1 | grep -q "statically linked"; then
        LINK_KIND="static"
    else
        LINK_KIND="dynamic（$(ldd "$BINARY" 2>&1 | awk '/libc\.so/{print $1; exit}')）"
        echo "! 警告: 产物不是静态链接 → $LINK_KIND，目标机 glibc 旧了会在长跑里偶发失败" >&2
    fi
else
    LINK_KIND="unknown"
fi

PKG="bifrost-${LABEL:-$VERSION}-x86_64-linux-musl"
[ "$TARGET" = "x86_64-unknown-linux-gnu" ] && PKG="bifrost-${LABEL:-$VERSION}-x86_64-linux-gnu"

STAGE_ROOT="$(mktemp -d)"
trap 'rm -rf "$STAGE_ROOT"' EXIT
STAGE="$STAGE_ROOT/$PKG"
mkdir -p "$STAGE"

cp "$BINARY" "$STAGE/bifrost"
cp "$HERE/install.sh" "$HERE/bootstrap.sh" "$HERE/README.md" "$HERE/settings.example.json" "$STAGE/"
[ -f "$ROOT/skills/setup-bifrost/SKILL.md" ] && cp "$ROOT/skills/setup-bifrost/SKILL.md" "$STAGE/SKILL.md"
[ -f "$ROOT/LICENSE" ] && cp "$ROOT/LICENSE" "$STAGE/"
chmod 0755 "$STAGE/bifrost" "$STAGE/install.sh" "$STAGE/bootstrap.sh"

# 包内清单：出问题时能一眼看出这份包是哪个 commit、用什么工具链编的
{
    echo "package:  $PKG"
    echo "version:  $VERSION"
    echo "label:    ${LABEL:-（无，正式版本）}"
    echo "commit:   $(cd "$ROOT" && git rev-parse --short HEAD 2>/dev/null || echo unknown)"
    echo "branch:   ${BIFROST_BRANCH:-$(cd "$ROOT" && git rev-parse --abbrev-ref HEAD 2>/dev/null || echo unknown)}"
    echo "target:   $TARGET"
    echo "link:     $LINK_KIND"
    echo "rustc:    $(rustc --version 2>/dev/null || echo unknown)"
    echo "built_at: $(date -u +%FT%TZ)"
    echo "binary_sha256: $(sha256sum "$BINARY" | cut -d' ' -f1)"
} > "$STAGE/BUILD_INFO.txt"

mkdir -p "$OUT"
tar czf "$OUT/$PKG.tar.gz" -C "$STAGE_ROOT" "$PKG"
( cd "$OUT" && sha256sum "$PKG.tar.gz" > "$PKG.tar.gz.sha256" )

if [ "$WITH_DEB" = "1" ] && command -v dpkg-deb >/dev/null; then
    bash "$HERE/build-deb.sh" --binary "$BINARY" --version "$VERSION" --out "$OUT" || \
        echo "! .deb 构建失败（tar.gz 仍然可用）" >&2
fi

echo "✓ $OUT/$PKG.tar.gz  ($(du -h "$OUT/$PKG.tar.gz" | cut -f1), link=$LINK_KIND)"
echo "  sha256: $(cut -d' ' -f1 "$OUT/$PKG.tar.gz.sha256")"
echo
echo "目标机安装（二选一）:"
echo "  client: tar xzf $PKG.tar.gz && cd $PKG && ./install.sh --role client --storage <共享盘>"
echo "  server: tar xzf $PKG.tar.gz && cd $PKG && sudo ./install.sh --role server --storage <共享盘>"
