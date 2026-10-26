#!/bin/bash
# release.sh —— 发布入口（Gitee 流水线和本机都用它）：构建 → 打包 → 打 tag → 发 Release
#
# 用法:
#   # CI（Gitee Go）: 什么都不用传，变量由流水线注入
#   # 本机:
#   GITEE_TOKEN=xxxx bash scripts/ci/release.sh
#   GITEE_TOKEN=xxxx bash scripts/ci/release.sh --tag v0.2.0
#   bash scripts/ci/release.sh --no-publish        # 只构建打包，不发版
#   VERSION=0.2.0 OUT=/tmp/rel bash scripts/ci/release.sh
#
# 规则:
#   - 版本号以 Cargo.toml 为准，tag = v<版本>
#   - 该 tag 已经有 Release 时，自动改用 v<版本>-b<构建号>，不覆盖已发布的版本
#   - 没有 GITEE_TOKEN：在 CI 里直接失败（main 必须产出 Release）；本机只打包并给出后续命令
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT" || exit 1

TAG_OVERRIDE=""
PUBLISH=1
for a in "$@"; do
    case "$a" in
        --tag) TAG_OVERRIDE="${2:-}"; shift 2 2>/dev/null || true ;;
        --no-publish) PUBLISH=0 ;;
        -h|--help) sed -n '2,16p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    esac
done

VERSION="${VERSION:-$(awk -F'"' '/^version[[:space:]]*=/{print $2; exit}' Cargo.toml)}"
OUT="${OUT:-$ROOT/dist}"
BUILD_NUM="${GITEE_PIPELINE_BUILD_NUMBER:-0}"
REPO="${GITEE_REPO:-peterlau/bifrost}"
REF="${GITEE_COMMIT:-$(git rev-parse HEAD 2>/dev/null || echo main)}"
API="${GITEE_API:-https://gitee.com/api/v5}"
TOKEN="${GITEE_TOKEN:-}"
TAG="${TAG_OVERRIDE:-v$VERSION}"

say() { echo "[release] $*"; }
say "版本: $VERSION   构建号: $BUILD_NUM   仓库: $REPO"

# ── 1. 构建 + 打包（musl 优先，没有 musl 工具链时退回 gnu 并在包名里标明）──
mkdir -p "$OUT"
if ! bash scripts/dist/pack.sh --version "$VERSION" --out "$OUT"; then
    say "musl 构建失败，退回 gnu（目标机需要 glibc）"
    bash scripts/dist/pack.sh --version "$VERSION" --out "$OUT" --target x86_64-unknown-linux-gnu \
        || { echo "FAIL: 打包失败" >&2; exit 1; }
fi

if [ "$PUBLISH" = "0" ]; then
    say "已跳过发布（--no-publish），产物在 $OUT"
    exit 0
fi

if [ -z "$TOKEN" ]; then
    if [ -n "${GITEE_PIPELINE_BUILD_NUMBER:-}" ]; then
        echo "FAIL: CI 里缺 GITEE_TOKEN —— 在流水线「参数设置」里加加密变量 GITEE_TOKEN（勾 projects 权限）" >&2
        exit 1
    fi
    say "没有 GITEE_TOKEN，只打包不发版。发版命令："
    echo "  GITEE_TOKEN=<你的令牌> bash scripts/dist/publish.sh --dist $OUT --tag $TAG"
    exit 0
fi

# ── 2. tag 冲突就让位给构建号，绝不覆盖已发布版本 ────────────────
# 注意：Gitee 对不存在的 tag 返回 HTTP 200 + "null"，必须看 body
body="$(curl -sS -H "Authorization: token $TOKEN" "$API/repos/$REPO/releases/tags/$TAG" 2>/dev/null)"
if printf '%s' "$body" | tr -d ' \n' | grep -q '"id":[0-9]' && [ -z "$TAG_OVERRIDE" ]; then
    TAG="v$VERSION-b$BUILD_NUM"
    say "v$VERSION 已发布过，本次用 $TAG"
fi

# ── 3. 发版 ────────────────────────────────────────────────────
bash scripts/dist/publish.sh --dist "$OUT" --tag "$TAG" --repo "$REPO" --ref "$REF" --token "$TOKEN" \
    || { echo "FAIL: 发布失败" >&2; exit 1; }

# 把 tag 回传给流水线后续任务（Gitee Go 的参数文件）
[ -n "${GITEE_PARAMS:-}" ] && echo "RELEASE_TAG=$TAG" >> "$GITEE_PARAMS"
say "发布完成: https://gitee.com/$REPO/releases/tag/$TAG"
