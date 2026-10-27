#!/bin/bash
# verify-release.sh —— 检查"最新一次发布是不是当前 HEAD 的"
#
# 用途：main 推上去之后，怎么确认流水线真的跑过、latest 快照包是不是这次提交打的？
#   latest 是滚动标签，看包内 BUILD_INFO 才能知道身份；这个脚本用 Release 正文里记录的
#   commit（由 publish-latest.sh 写入）和本地 HEAD 比对，一条命令给结论。
#
# 用法:
#   bash scripts/ci/verify-release.sh                # 查 latest（main 快照）
#   bash scripts/ci/verify-release.sh v0.2.0         # 查某个正式 tag
#   GITEE_TOKEN=xxx bash scripts/ci/verify-release.sh # 私有仓库时需要令牌
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT" || exit 1

TAG="${1:-latest}"
REPO="${GITEE_REPO:-peterlau/bifrost}"
API="${GITEE_API:-https://gitee.com/api/v5}"
TOKEN="${GITEE_TOKEN:-}"
HEAD_SHA="$(git rev-parse HEAD)"
HEAD_SHORT="$(git rev-parse --short HEAD)"

say() { echo "[verify] $*"; }

AUTH=()
[ -n "$TOKEN" ] && AUTH=(-H "Authorization: token $TOKEN")

resp="$(curl -sS "${AUTH[@]}" "$API/repos/$REPO/releases/tags/$TAG" -w $'\n%{http_code}' 2>/dev/null)"
code="$(printf '%s' "$resp" | tail -1)"
body="$(printf '%s' "$resp" | sed '$d')"

if [ "$code" != "200" ] || ! printf '%s' "$body" | tr -d ' \n' | grep -q '"id":[0-9]'; then
    say "❌ 没有 tag=$TAG 的 Release（HTTP $code）"
    say "   说明：main 的流水线没跑起来，或没配 GITEE_TOKEN（快照发布失败只告警、不拖红）"
    say "   去 Gitee → 仓库 → 流水线，看有没有 HEAD ($HEAD_SHORT) 的运行记录"
    exit 1
fi

info="$(printf '%s' "$body" | python3 -c '
import sys, json, re
d = json.load(sys.stdin)
print(d.get("created_at", ""))
print(len(d.get("assets", [])))
m = re.search(r"commit:\s*([0-9a-f]{7,40})", d.get("body") or "")
print(m.group(1) if m else "")
' 2>/dev/null)"
created="$(printf '%s' "$info" | sed -n 1p)"
nassets="$(printf '%s' "$info" | sed -n 2p)"
rel_sha="$(printf '%s' "$info" | sed -n 3p)"

say "tag=$TAG  发布时间=$created  附件=$nassets  正文记录 commit=${rel_sha:-<无>}"
say "本地 HEAD = $HEAD_SHA"

if [ -n "$rel_sha" ] && [ "$rel_sha" = "$HEAD_SHA" ]; then
    say "✅ 该发布就是这个 HEAD 的产物"
    exit 0
fi
if [ -n "$rel_sha" ]; then
    say "⚠️ 发布记录的是 ${rel_sha:0:8}，本地 HEAD 是 $HEAD_SHORT —— 发布落后于当前提交"
    say "   （latest 是滚动标签，正文 commit 只在创建时写入；以包内 BUILD_INFO.txt 为准）"
    exit 2
fi
say "⚠️ Release 正文里没有 commit 记录（老版本或手工建的），只能靠包内 BUILD_INFO.txt"
exit 2
