#!/bin/bash
# verify-release.sh —— 检查"最新一次发布是不是当前 HEAD 的"
#
# 用途：main 推上去之后，怎么确认流水线真的跑过、latest 快照包是不是这次提交打的？
#   latest 是滚动标签，看包内 BUILD_INFO 才能知道身份；这个脚本用 Release 的 target_commitish
#   和本地 HEAD 比对，一条命令给结论。
#
# 用法:
#   bash scripts/ci/verify-release.sh                # 查 latest（main 快照）
#   bash scripts/ci/verify-release.sh v0.2.0         # 查某个正式 tag
#   GH_TOKEN=xxx bash scripts/ci/verify-release.sh   # 私有仓库时需要令牌
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT" || exit 1

TAG="${1:-latest}"
REPO="${GITHUB_REPOSITORY:-}"
if [ -z "$REPO" ]; then
    # 从 origin remote 反推 owner/repo
    url="$(git remote get-url origin 2>/dev/null || true)"
    case "$url" in
        git@github.com:*)     REPO="${url#git@github.com:}" ;;
        https://github.com/*) REPO="${url#https://github.com/}" ;;
    esac
    REPO="${REPO%.git}"
fi
case "$REPO" in
    */*) ;;
    *) echo "FAIL: 推不出仓库名，请设置 GITHUB_REPOSITORY=owner/repo" >&2; exit 1 ;;
esac

HEAD_SHA="$(git rev-parse HEAD)"
HEAD_SHORT="$(git rev-parse --short HEAD)"

say() { echo "[verify] $*"; }

command -v gh >/dev/null 2>&1 || { echo "FAIL: 需要 GitHub CLI (gh)" >&2; exit 1; }

if ! info="$(gh release view "$TAG" --repo "$REPO" --json tagName,targetCommitish,createdAt,isPrerelease,assets 2>/dev/null)"; then
    say "❌ 没有 tag=$TAG 的 Release"
    say "   说明：main 的流水线没跑起来，或 GH_TOKEN 未配置"
    say "   查看: gh run list --repo $REPO --branch main --limit 3（看 HEAD=$HEAD_SHORT 的运行记录）"
    exit 1
fi

read -r created target is_pre nassets <<EOF
$(printf '%s' "$info" | python3 -c '
import sys, json
d = json.load(sys.stdin)
print(d.get("createdAt", ""))
print(d.get("targetCommitish", ""))
print("prerelease" if d.get("isPrerelease") else "stable")
print(len(d.get("assets", [])))
')
EOF

say "tag=$TAG  type=$is_pre  发布时间=$created  附件=$nassets"
say "Release target_commitish = $target"
say "本地 HEAD               = $HEAD_SHA"

# targetCommitish 可能是 branch 名（"main"）也可能是 sha
if [ "$target" = "$HEAD_SHA" ]; then
    say "✅ 该发布就是这个 HEAD 的产物"
    exit 0
fi

# 如果 target 是分支名，拿到分支当前指向的 sha 再比
if resolved="$(git ls-remote origin "refs/heads/$target" 2>/dev/null | awk '{print $1}')" && [ -n "$resolved" ]; then
    say "（target 是分支名，其当前指向 $resolved）"
    if [ "$resolved" = "$HEAD_SHA" ]; then
        say "✅ 发布基于的分支正好指向当前 HEAD"
        exit 0
    fi
fi

say "⚠️ 该发布对应的 ref 与本地 HEAD 不一致（latest 是滚动标签，以包内 BUILD_INFO.txt 为准）"
exit 2
