#!/bin/bash
# publish-latest.sh —— 把 main 分支的快照包发到 tag=latest 的滚动 Release（覆盖旧附件）
#
# 用途：main 分支不产正式版本，但需要一个稳定的下载地址给安装脚本/气隙机器用：
#   https://gitee.com/<repo>/releases/download/latest/bifrost-latest-x86_64-linux-musl.tar.gz
#
# 行为：
#   - tag `latest` 不存在则创建（指向 --ref），存在就复用（滚动标签，不指向固定 commit，
#     真实版本看包内 BUILD_INFO.txt）
#   - Release 标为 prerelease=true，名字 "latest snapshot"
#   - 每次先删掉旧的 bifrost-latest-* 附件，再上传 dist 里新的
#
# 用法:
#   GITEE_TOKEN=xxx scripts/dist/publish-latest.sh --dist dist
#   scripts/dist/publish-latest.sh --dist dist --repo peterlau/bifrost --ref <sha> --dry-run
set -uo pipefail

REPO="${GITEE_REPO:-}"
DIST="${DIST:-dist}"
REF=""
TOKEN="${GITEE_TOKEN:-}"
DRY=0
API="${GITEE_API:-https://gitee.com/api/v5}"
TAG="latest"
REL_NAME="latest snapshot"

while [ $# -gt 0 ]; do
    case "$1" in
        --dist)    DIST="$2"; shift 2 ;;
        --repo)    REPO="$2"; shift 2 ;;
        --ref)     REF="$2"; shift 2 ;;
        --token)   TOKEN="$2"; shift 2 ;;
        --dry-run) DRY=1; shift ;;
        -h|--help) sed -n '2,18p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "未知参数: $1" >&2; exit 1 ;;
    esac
done

say() { echo "[latest] $*"; }
fail() { echo "FAIL: $*" >&2; exit 1; }

if [ -z "$REPO" ]; then
    url="$(git remote get-url origin 2>/dev/null || echo)"
    REPO="$(printf '%s' "$url" | sed -E 's#(\.git)?/?$##' | sed -E 's#.*[:/]([^/]+/[^/]+)$#\1#')"
fi
case "$REPO" in */*) ;; *) fail "推不出仓库名，用 --repo owner/repo" ;; esac

REF="${REF:-${GITEE_COMMIT:-$(git rev-parse HEAD 2>/dev/null || echo main)}}"
[ -d "$DIST" ] || fail "--dist 目录不存在: $DIST"

mapfile -t ASSETS < <(ls -1 "$DIST"/bifrost-latest-* 2>/dev/null)
[ ${#ASSETS[@]} -gt 0 ] || fail "$DIST 里没有 bifrost-latest-* 快照包"

[ "$DRY" = "1" ] || [ -n "$TOKEN" ] || fail "需要 Gitee 令牌（GITEE_TOKEN 或 --token，勾 projects 权限）"

api() {   # $1=method $2=path  $3...=curl 参数
    local method="$1" path="$2"; shift 2
    curl -sS -X "$method" "$API$path" "$@" \
        -H "Authorization: token $TOKEN" -w $'\n%{http_code}' 2>/dev/null
}
json_field() {   # $1=json $2=key  → 取顶层字段
    printf '%s' "$1" | python3 -c 'import sys,json;d=json.load(sys.stdin);print(d.get(sys.argv[1],""))' "$2" 2>/dev/null
}
has_json_id() { printf '%s' "$1" | tr -d ' \n' | grep -q '"id":[0-9]'; }

say "仓库: $REPO    tag: $TAG    ref: ${REF:0:12}"

# ── 1. tag latest（不存在才建；已存在说明是滚动标签，直接复用）──
if [ "$DRY" = "1" ]; then
    echo "DRY: POST $API/repos/$REPO/tags  (refs=$REF, tag_name=$TAG)  [已存在则忽略]"
else
    resp="$(api POST "/repos/$REPO/tags" -F "refs=$REF" -F "tag_name=$TAG" -F "tag_message=rolling snapshot")"
    code="$(printf '%s' "$resp" | tail -1)"
    [ "${code:0:1}" = "2" ] && say "✓ tag $TAG 已创建" || say "· tag $TAG 已存在（复用，滚动标签）"
fi

# ── 2. 找到/建立 latest Release ──
REL_ID=""
if [ "$DRY" = "1" ]; then
    echo "DRY: GET $API/repos/$REPO/releases/tags/$TAG  → 没有则 POST $API/repos/$REPO/releases"
else
    resp="$(api GET "/repos/$REPO/releases/tags/$TAG")"
    body="$(printf '%s' "$resp" | sed '$d')"
    if has_json_id "$body"; then
        REL_ID="$(json_field "$body" id)"
        say "· Release 已存在（id=$REL_ID），先清旧快照附件"
    else
        now="$(date -u +%FT%TZ)"
        body_text="滚动快照：main 分支每次推送覆盖更新。版本号见包内 BUILD_INFO.txt
commit: $REF
built_at: $now
注意：这是 main 的构建产物，不是正式版本；正式版本请从 release/x.y.z 打 tag。"
        resp="$(api POST "/repos/$REPO/releases" \
            -F "tag_name=$TAG" -F "name=$REL_NAME" -F "body=$body_text" \
            -F "target_commitish=$REF" -F "prerelease=true")"
        code="$(printf '%s' "$resp" | tail -1)"
        body="$(printf '%s' "$resp" | sed '$d')"
        [ "${code:0:1}" = "2" ] || fail "创建 latest Release 失败（HTTP $code）: $(printf '%s' "$body" | head -c 300)"
        REL_ID="$(json_field "$body" id)"
        [ -n "$REL_ID" ] || fail "返回里没有 release id"
        say "✓ latest Release 已创建（id=$REL_ID）"
    fi
fi

# ── 3. 删掉旧的 bifrost-latest-* 附件 ──
if [ "$DRY" = "1" ]; then
    echo "DRY: GET $API/repos/$REPO/releases/<id> → DELETE 每个 bifrost-latest-* 附件的 attach_files/<asset_id>"
else
    resp="$(api GET "/repos/$REPO/releases/$REL_ID")"
    body="$(printf '%s' "$resp" | sed '$d')"
    old_assets="$(printf '%s' "$body" | python3 -c '
import sys, json
try:
    d = json.load(sys.stdin)
except Exception:
    sys.exit(0)
for a in d.get("assets", []):
    if str(a.get("name", "")).startswith("bifrost-latest"):
        print(a.get("id"), a.get("name"), sep="\t")
' 2>/dev/null)"
    if [ -n "$old_assets" ]; then
        while IFS=$'\t' read -r aid aname; do
            [ -n "$aid" ] || continue
            r="$(api DELETE "/repos/$REPO/releases/$REL_ID/attach_files/$aid")"
            c="$(printf '%s' "$r" | tail -1)"
            [ "${c:0:1}" = "2" ] && say "· 已删除旧附件 $aname" || say "! 删不掉旧附件 $aname（HTTP $c），继续"
        done <<< "$old_assets"
    fi
fi

# ── 4. 上传新快照附件 ──
for f in "${ASSETS[@]}"; do
    base="$(basename "$f")"
    if [ "$DRY" = "1" ]; then
        echo "DRY: POST $API/repos/$REPO/releases/<id>/attach_files  file=@$f"
        continue
    fi
    resp="$(api POST "/repos/$REPO/releases/$REL_ID/attach_files" -F "file=@$f")"
    code="$(printf '%s' "$resp" | tail -1)"
    case "$code" in
        2*) say "✓ 上传 $base（$(du -h "$f" | cut -f1)）" ;;
        *)  fail "上传 $base 失败（HTTP $code）: $(printf '%s' "$resp" | sed '$d' | head -c 300)" ;;
    esac
done

say "完成: https://gitee.com/$REPO/releases/tag/$TAG"
echo "  直连地址: https://gitee.com/$REPO/releases/download/$TAG/$(basename "${ASSETS[0]}")"
