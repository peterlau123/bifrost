#!/bin/bash
# publish-github.sh —— 把安装包发到 GitHub Releases（gh CLI，幂等）
#
# 两条通道（与 Gitee 侧 publish.sh / publish-latest.sh 的语义一致）：
#   --latest       滚动快照：tag `latest` 的 Release（prerelease），每次覆盖附件
#   --tag vX.Y.Z   正式版本：Release 已存在则直接失败（正式版本不可覆盖）
#
# 用法:
#   GH_TOKEN=xxx scripts/dist/publish-github.sh --dist dist --latest --ref <sha>
#   GH_TOKEN=xxx scripts/dist/publish-github.sh --dist dist --tag v0.2.0 --ref <sha>
#   scripts/dist/publish-github.sh --dist dist --tag v0.2.0 --repo owner/repo --dry-run
#
# 参数:
#   --dist DIR        装包所在目录（默认 dist）
#   --latest          快照通道（tag=latest，prerelease，覆盖附件）
#   --tag TAG         正式通道的 tag（如 v0.2.0）
#   --ref REF         tag 指向的提交（默认 GITHUB_SHA / 当前 HEAD）
#   --repo OWNER/REPO 默认取 GITHUB_REPOSITORY，其次 `gh repo view`
#   --notes FILE      Release 说明文件（默认取最近 10 条提交）
#   --name NAME       Release 标题（默认 = tag）
#   --prerelease      标记为预发布
#   --dry-run         只打印将执行的 gh 命令
#
# 认证：gh 自己读 GH_TOKEN / GITHUB_TOKEN；Actions 里传 secrets.GITHUB_TOKEN 即可。
set -uo pipefail

DIST="${DIST:-dist}"
REPO="${GITHUB_REPOSITORY:-}"
REF=""
TAG=""
LATEST=0
NOTES=""
REL_NAME=""
PRERELEASE=0
DRY=0

while [ $# -gt 0 ]; do
    case "$1" in
        --dist)       DIST="$2"; shift 2 ;;
        --latest)     LATEST=1; shift ;;
        --tag)        TAG="$2"; shift 2 ;;
        --ref)        REF="$2"; shift 2 ;;
        --repo)       REPO="$2"; shift 2 ;;
        --notes)      NOTES="$2"; shift 2 ;;
        --name)       REL_NAME="$2"; shift 2 ;;
        --prerelease) PRERELEASE=1; shift ;;
        --dry-run)    DRY=1; shift ;;
        -h|--help)    sed -n '2,26p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "未知参数: $1" >&2; exit 1 ;;
    esac
done

say() { echo "[publish-github] $*"; }
fail() { echo "FAIL: $*" >&2; exit 1; }

command -v gh >/dev/null 2>&1 || fail "需要 GitHub CLI（gh）。Actions 的 ubuntu-latest 自带；本机见 https://cli.github.com"

if [ -z "$REPO" ]; then
    REPO="$(gh repo view --json nameWithOwner -q .nameWithOwner 2>/dev/null || true)"
fi
case "$REPO" in
    */*) ;;
    *) fail "推不出仓库名，用 --repo owner/repo 或设置 GITHUB_REPOSITORY" ;;
esac

REF="${REF:-${GITHUB_SHA:-$(git rev-parse HEAD 2>/dev/null || echo main)}}"
[ -d "$DIST" ] || fail "--dist 目录不存在: $DIST"

if [ "$LATEST" = "1" ]; then
    [ -z "$TAG" ] || fail "--latest 与 --tag 只能二选一"
    TAG="latest"
    mapfile -t ASSETS < <(ls -1 "$DIST"/bifrost-latest-* "$DIST"/*.deb* 2>/dev/null)
    [ ${#ASSETS[@]} -gt 0 ] || fail "$DIST 里没有 bifrost-latest-* 快照包"
    REL_NAME="${REL_NAME:-latest snapshot}"
    PRERELEASE=1
else
    [ -n "$TAG" ] || fail "需要 --tag（如 v0.1.0）或 --latest"
    mapfile -t ASSETS < <(ls -1 "$DIST"/*.tar.gz "$DIST"/*.deb "$DIST"/*.sha256 2>/dev/null)
    [ ${#ASSETS[@]} -gt 0 ] || fail "$DIST 里没有 .tar.gz / .deb / .sha256"
    REL_NAME="${REL_NAME:-$TAG}"
fi

if [ -n "$NOTES" ] && [ -f "$NOTES" ]; then
    NOTES_TEXT="$(cat "$NOTES")"
else
    NOTES_TEXT="$(git log --oneline -10 2>/dev/null || echo "release $TAG")"
fi

if [ "$DRY" = "0" ]; then
    gh auth status >/dev/null 2>&1 \
        || fail "gh 未登录。Actions 里设置 GH_TOKEN=\${{ secrets.GITHUB_TOKEN }}；本机跑 gh auth login"
fi

run() {
    if [ "$DRY" = "1" ]; then
        printf 'DRY:'; printf ' %q' "$@"; echo
    else
        "$@"
    fi
}

say "仓库: $REPO    tag: $TAG    ref: ${REF:0:12}    assets: ${#ASSETS[@]}"

# ── 1. Rolling snapshot: latest ────────────────────────────────
if [ "$LATEST" = "1" ]; then
    if [ "$DRY" = "0" ] && gh release view "$TAG" --repo "$REPO" >/dev/null 2>&1; then
        say "· latest Release 已存在（滚动快照，覆盖附件）"
    else
        # tag 不存在时 --target 决定它指向哪个 commit；已存在时 GitHub 忽略 --target
        run gh release create "$TAG" --repo "$REPO" --target "$REF" \
            --title "$REL_NAME" --notes "$NOTES_TEXT" --prerelease \
            || fail "创建 latest Release 失败"
    fi

    # 让 latest 标签跟随本次构建，GitHub 自动生成的 Source code 包才不会停在旧提交
    if [ "$DRY" = "1" ]; then
        echo "DRY: PATCH .../git/refs/tags/latest sha=$REF force=true"
    else
        gh api -X PATCH "repos/$REPO/git/refs/tags/latest" -f sha="$REF" -F force=true >/dev/null 2>&1 \
            && say "· tag latest → ${REF:0:12}" \
            || say "! 移动 tag latest 失败（Source code 包可能停留在旧提交）"
    fi

    # 版本号变化后旧 deb/tar 会留在 latest 页面：只保留本次上传的附件
    if [ "$DRY" = "0" ]; then
        while IFS= read -r name; do
            [ -n "$name" ] || continue
            keep=0
            for f in "${ASSETS[@]}"; do
                [ "$(basename "$f")" = "$name" ] && { keep=1; break; }
            done
            if [ "$keep" = "0" ]; then
                case "$name" in
                    bifrost*) gh release delete-asset "$TAG" "$name" --repo "$REPO" --yes >/dev/null 2>&1 \
                                  && say "· 清理旧附件 $name" ;;
                esac
            fi
        done < <(gh release view "$TAG" --repo "$REPO" --json assets --jq '.assets[].name' 2>/dev/null)
    fi

    run gh release upload "$TAG" --repo "$REPO" --clobber "${ASSETS[@]}" \
        || fail "上传快照附件失败"
    say "完成: https://github.com/$REPO/releases/tag/$TAG"
    echo "  直连地址: https://github.com/$REPO/releases/download/$TAG/$(basename "${ASSETS[0]}")"
    exit 0
fi

# ── 2. 正式版本：不可覆盖 ──────────────────────────────────────
if [ "$DRY" = "0" ] && gh release view "$TAG" --repo "$REPO" >/dev/null 2>&1; then
    fail "$TAG 已经发布过。正式版本不可覆盖 —— 请先把 Cargo.toml 版本号升到下一个版本（并新建 release/x.y.z 分支）"
fi

CREATE_ARGS=(gh release create "$TAG" --repo "$REPO" --target "$REF" \
    --title "$REL_NAME" --notes "$NOTES_TEXT")
[ "$PRERELEASE" = "1" ] && CREATE_ARGS+=(--prerelease)
run "${CREATE_ARGS[@]}" "${ASSETS[@]}" || fail "创建 Release 失败"

say "完成: https://github.com/$REPO/releases/tag/$TAG"
echo "  直连地址: https://github.com/$REPO/releases/download/$TAG/$(basename "${ASSETS[0]}")"
