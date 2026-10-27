#!/bin/bash
# release.sh —— 发布入口（Gitee 流水线和本机都用它）
#
# 两条通道（按分支自动判定，也可以用参数强制）:
#   main            → 快照通道：只出 bifrost-latest-x86_64-linux-musl.tar.gz（+ .sha256）
#                     不打 tag、不发 Release，作为流水线制品留存
#   release/x.y.z   → 正式通道：Cargo.toml 版本必须等于 x.y.z → 打 tag v<x.y.z> → 发 Gitee Release
#   其它分支        → 直接拒绝（防止从 feature 分支误发版；确需发版加 --force-branch）
#
# 用法:
#   # CI（Gitee Go）: 什么都不用传，分支由 GITEE_BRANCH 注入
#   # 本机:
#   bash scripts/ci/release.sh --dry-run                    # 只看会做什么
#   bash scripts/ci/release.sh --snapshot --no-publish      # 只出 latest 快照包
#   GITEE_TOKEN=xxx bash scripts/ci/release.sh --release    # 正式发版（需 release/x.y.z 分支）
#   bash scripts/ci/release.sh --release --force-branch --tag v0.2.0
#
# 规则:
#   - 版本号唯一来源是 Cargo.toml；正式版的 tag = v<版本>，且必须与分支名一致
#   - 同版本重复发版 → 直接失败（要求先改 Cargo.toml 版本），不再自动出 -b<构建号>
#   - musl 静态构建失败 → 默认失败；确需退回 gnu 加 --allow-fallback
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT" || exit 1

CHANNEL=""
TAG_OVERRIDE=""
PUBLISH=1
DRY=0
FORCE_BRANCH=0
ALLOW_FALLBACK=0
while [ $# -gt 0 ]; do
    case "$1" in
        --snapshot)       CHANNEL="snapshot"; shift ;;
        --release)        CHANNEL="release"; shift ;;
        --no-publish)     PUBLISH=0; shift ;;
        --dry-run)        DRY=1; shift ;;
        --force-branch)   FORCE_BRANCH=1; shift ;;
        --allow-fallback) ALLOW_FALLBACK=1; shift ;;
        --tag)            TAG_OVERRIDE="${2:-}"; shift 2 ;;
        -h|--help) sed -n '2,24p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "未知参数: $1" >&2; exit 1 ;;
    esac
done

VERSION="${VERSION:-$(awk -F'"' '/^version[[:space:]]*=/{print $2; exit}' Cargo.toml)}"
OUT="${OUT:-$ROOT/dist}"
BUILD_NUM="${GITEE_PIPELINE_BUILD_NUMBER:-0}"
REPO="${GITEE_REPO:-peterlau/bifrost}"
REF="${GITEE_COMMIT:-$(git rev-parse HEAD 2>/dev/null || echo main)}"
API="${GITEE_API:-https://gitee.com/api/v5}"
TOKEN="${GITEE_TOKEN:-}"
BRANCH="${GITEE_BRANCH:-$(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo unknown)}"

say() { echo "[release] $*"; }
fail() { echo "FAIL: $*" >&2; exit 1; }

[ -n "$VERSION" ] || fail "取不到版本号（Cargo.toml）"

# ── 0. 通道判定：分支是唯一权威，除非显式 --snapshot/--release ──
if [ -z "$CHANNEL" ]; then
    case "$BRANCH" in
        main) CHANNEL="snapshot" ;;
        release/[0-9]*.[0-9]*.[0-9]*) CHANNEL="release" ;;
        *) fail "当前分支 '$BRANCH' 不允许发版。main 出快照包、release/x.y.z 出正式版（确需强推加 --force-branch）" ;;
    esac
    say "按分支判定通道: $BRANCH → $CHANNEL"
fi

if [ "$FORCE_BRANCH" = "0" ]; then
    if [ "$CHANNEL" = "snapshot" ] && [ "$BRANCH" != "main" ]; then
        fail "快照通道只在 main 上跑（当前 $BRANCH）；确需覆盖加 --force-branch"
    fi
    if [ "$CHANNEL" = "release" ] && [[ "$BRANCH" != release/* ]]; then
        fail "正式通道只在 release/x.y.z 上跑（当前 $BRANCH）；确需覆盖加 --force-branch"
    fi
fi

case "$CHANNEL" in
    snapshot)
        LABEL="latest"
        TAG=""
        say "快照通道：出 bifrost-latest-* 包；有令牌时覆盖更新 tag=latest 的滚动 Release（不产正式版本）"
        ;;
    release)
        # 分支名与 Cargo.toml 版本必须一致，防止 release/0.3.0 里打出 0.2.0 的包
        if [ "$FORCE_BRANCH" = "0" ] && [ "$BRANCH" != "release/$VERSION" ]; then
            fail "分支 '$BRANCH' 与 Cargo.toml 版本 '$VERSION' 不一致（应为 release/$VERSION），先对齐版本号"
        fi
        LABEL=""
        TAG="${TAG_OVERRIDE:-v$VERSION}"
        say "正式通道：tag=$TAG（版本来自 Cargo.toml）"
        ;;
    *) fail "未知通道: $CHANNEL" ;;
esac

say "版本: $VERSION   分支: $BRANCH   通道: $CHANNEL   构建号: $BUILD_NUM   仓库: $REPO"

# ── 1. 构建 + 打包（musl 静态优先；失败默认中止）──
if [ "$DRY" = "1" ]; then
    dry_args="--version $VERSION --out $OUT"
    [ -n "$LABEL" ] && dry_args="$dry_args --label $LABEL"
    [ "$CHANNEL" = "snapshot" ] && dry_args="$dry_args --no-deb"
    say "DRY: pack.sh $dry_args"
else
    mkdir -p "$OUT"
    PACK_ARGS=(--version "$VERSION" --out "$OUT")
    [ -n "$LABEL" ] && PACK_ARGS+=(--label "$LABEL")
    [ "$CHANNEL" = "snapshot" ] && PACK_ARGS+=(--no-deb)   # 快照包不带 .deb（apt 里版本号会打架）

    if ! bash scripts/dist/pack.sh "${PACK_ARGS[@]}"; then
        [ "$ALLOW_FALLBACK" = "1" ] || fail "musl 静态构建失败（要退回 gnu 包请加 --allow-fallback，允许目标机依赖 glibc）"
        say "musl 构建失败 → 按 --allow-fallback 退回 gnu"
        bash scripts/dist/pack.sh "${PACK_ARGS[@]}" --target x86_64-unknown-linux-gnu \
            || fail "打包失败"
    fi
fi

if [ "$CHANNEL" = "snapshot" ]; then
    say "快照包完成: $OUT/bifrost-latest-x86_64-linux-musl.tar.gz"
    [ -n "${GITEE_PARAMS:-}" ] && echo "SNAPSHOT_TAG=latest" >> "$GITEE_PARAMS"
    if [ "$PUBLISH" = "1" ] && [ "$DRY" = "0" ]; then
        if [ -n "$TOKEN" ]; then
            # 覆盖更新 tag=latest 的滚动 Release；失败只告警，不能拖红 main
            bash scripts/dist/publish-latest.sh --dist "$OUT" --repo "$REPO" --ref "$REF" --token "$TOKEN" \
                || say "! latest 快照发布失败（不影响 main 构建）—— 制品仍在 $OUT"
        else
            say "没有 GITEE_TOKEN：只留制品，不更新 latest Release"
        fi
    fi
    [ "$DRY" = "1" ] && say "DRY: 有令牌时会跑 publish-latest.sh，覆盖更新 tag=latest 的滚动 Release"
    say "（main 分支不产正式版本；要发版请从 release/x.y.z 打）"
    exit 0
fi

[ "$PUBLISH" = "1" ] || { say "已跳过发布（--no-publish），产物在 $OUT"; exit 0; }

if [ "$DRY" = "1" ]; then
    say "DRY: 检查 tag $TAG 是否已发布 → POST /tags（refs=$REF）→ POST /releases + attach_files"
    exit 0
fi

if [ -z "$TOKEN" ]; then
    if [ -n "${GITEE_PIPELINE_BUILD_NUMBER:-}" ]; then
        fail "CI 里缺 GITEE_TOKEN —— 在流水线「参数设置」里加加密变量 GITEE_TOKEN（勾 projects 权限）"
    fi
    say "没有 GITEE_TOKEN，只打包不发版。发版命令："
    echo "  GITEE_TOKEN=<你的令牌> bash scripts/dist/publish.sh --dist $OUT --tag $TAG"
    exit 0
fi

# ── 2. 同版本重复发版 = 硬失败（不再偷偷出 -b<构建号>）──
# 注意：Gitee 对不存在的 tag 返回 HTTP 200 + "null"，必须看 body
if [ "$DRY" = "0" ]; then
    body="$(curl -sS -H "Authorization: token $TOKEN" "$API/repos/$REPO/releases/tags/$TAG" 2>/dev/null)"
    if printf '%s' "$body" | tr -d ' \n' | grep -q '"id":[0-9]'; then
        fail "$TAG 已经发布过。正式版本不可覆盖 —— 请先把 Cargo.toml 版本号升到下一个版本（并新建 release/x.y.z 分支）"
    fi
fi

# ── 3. 发版 ────────────────────────────────────────────────────
bash scripts/dist/publish.sh --dist "$OUT" --tag "$TAG" --repo "$REPO" --ref "$REF" --token "$TOKEN" \
    || fail "发布失败"

# 把 tag 回传给流水线后续任务（Gitee Go 的参数文件）
[ -n "${GITEE_PARAMS:-}" ] && echo "RELEASE_TAG=$TAG" >> "$GITEE_PARAMS"
say "发布完成: https://gitee.com/$REPO/releases/tag/$TAG"
