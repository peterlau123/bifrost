#!/bin/bash
# publish.sh —— 在 Gitee 上打 tag、建 Release、上传安装包（幂等，可重复执行）
#
# 用法:
#   GITEE_TOKEN=xxxx scripts/dist/publish.sh --dist dist --tag v0.1.0
#   scripts/dist/publish.sh --dist dist --tag v0.1.0 --repo peterlau/bifrost --dry-run
#
# 参数:
#   --dist DIR        装包所在目录（默认 dist）
#   --tag TAG         必填，如 v0.1.0
#   --repo OWNER/REPO 默认从 git remote 推断（gitee.com:peterlau/bifrost.git → peterlau/bifrost）
#   --ref REF         tag 指向的提交/分支（默认 HEAD 的 sha）
#   --token TOKEN     Gitee 私人令牌（默认读环境变量 GITEE_TOKEN；需要 projects 权限）
#   --notes FILE      Release 说明文件（默认用最近提交生成）
#   --name NAME       Release 标题（默认 = tag）
#   --prerelease      标记为预发布
#   --dry-run         只打印将执行的 API 调用
#
# 幂等：tag 已存在则复用；该 tag 的 Release 已存在则复用并只补传缺失附件。
set -uo pipefail

REPO=""
DIST="${DIST:-dist}"
TAG=""
REF=""
TOKEN="${GITEE_TOKEN:-}"
NOTES=""
REL_NAME=""
PRERELEASE=false
DRY=0
API="${GITEE_API:-https://gitee.com/api/v5}"

while [ $# -gt 0 ]; do
    case "$1" in
        --dist)       DIST="$2"; shift 2 ;;
        --tag)        TAG="$2"; shift 2 ;;
        --repo)       REPO="$2"; shift 2 ;;
        --ref)        REF="$2"; shift 2 ;;
        --token)      TOKEN="$2"; shift 2 ;;
        --notes)      NOTES="$2"; shift 2 ;;
        --name)       REL_NAME="$2"; shift 2 ;;
        --prerelease) PRERELEASE=true; shift ;;
        --dry-run)    DRY=1; shift ;;
        -h|--help)    sed -n '2,23p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "未知参数: $1" >&2; exit 1 ;;
    esac
done

[ -n "$TAG" ] || { echo "FAIL: 需要 --tag（如 v0.1.0）" >&2; exit 1; }
[ -d "$DIST" ] || { echo "FAIL: --dist 目录不存在: $DIST" >&2; exit 1; }

# 仓库：优先 --repo / GITEE_REPO，其次从 remote 推断
if [ -z "$REPO" ]; then
    REPO="${GITEE_REPO:-}"
fi
if [ -z "$REPO" ]; then
    url="$(git remote get-url origin 2>/dev/null || echo)"
    REPO="$(printf '%s' "$url" | sed -E 's#.*[:/]([^/]+/[^/]+?)(\.git)?$#\1#')"
fi
case "$REPO" in
    */*) ;;
    *) echo "FAIL: 推不出仓库名，用 --repo owner/repo 指定" >&2; exit 1 ;;
esac

# 待上传文件：tar.gz + .sha256（可选 BUILD_INFO 之类不单独传）
mapfile -t ASSETS < <(ls -1 "$DIST"/*.tar.gz "$DIST"/*.deb "$DIST"/*.sha256 2>/dev/null)
[ ${#ASSETS[@]} -gt 0 ] || { echo "FAIL: $DIST 里没有 .tar.gz / .sha256" >&2; exit 1; }

REF="${REF:-${GITEE_COMMIT:-$(git rev-parse HEAD 2>/dev/null || echo main)}}"
REL_NAME="${REL_NAME:-$TAG}"
if [ -z "$NOTES" ] || [ ! -f "$NOTES" ]; then
    NOTES_TEXT="$(git log --oneline -10 2>/dev/null || echo "release $TAG")"
else
    NOTES_TEXT="$(cat "$NOTES")"
fi

say() { echo "[publish] $*"; }

if [ "$DRY" = "0" ] && [ -z "$TOKEN" ]; then
    echo "FAIL: 需要 Gitee 令牌（环境变量 GITEE_TOKEN 或 --token）" >&2
    echo "  生成: https://gitee.com/profile/personal_access_tokens （勾 projects）" >&2
    exit 1
fi

api() {   # $1=method $2=path  $3...=curl 参数
    local method="$1" path="$2"; shift 2
    curl -sS -X "$method" "$API$path" "$@" \
        -H "Authorization: token $TOKEN" -w $'\n%{http_code}' 2>/dev/null
}

json_id() { printf '%s' "$1" | tr ',' '\n' | grep -m1 '"id"' | grep -oE '[0-9]+'; }

# ── 1. tag ─────────────────────────────────────────────────────
say "仓库: $REPO    tag: $TAG    ref: ${REF:0:12}"
if [ "$DRY" = "1" ]; then
    echo "DRY: POST $API/repos/$REPO/tags  (refs=$REF, tag_name=$TAG)"
else
    resp="$(api POST "/repos/$REPO/tags" -F "refs=$REF" -F "tag_name=$TAG" -F "tag_message=release $TAG")"
    code="$(printf '%s' "$resp" | tail -1)"
    body="$(printf '%s' "$resp" | sed '$d')"
    case "$code" in
        2*) say "✓ tag 已创建" ;;
        *)  say "tag 未创建（HTTP $code），假定已存在继续：$(printf '%s' "$body" | head -c 200)" ;;
    esac
fi

# ── 2. release ─────────────────────────────────────────────────
REL_ID=""
if [ "$DRY" = "0" ]; then
    resp="$(api GET "/repos/$REPO/releases/tags/$TAG")"
    code="$(printf '%s' "$resp" | tail -1)"
    body="$(printf '%s' "$resp" | sed '$d')"
    if [ "$code" = "200" ]; then
        REL_ID="$(json_id "$body")"
        say "Release 已存在（id=$REL_ID），只补传附件"
    fi
fi

if [ -z "$REL_ID" ]; then
    if [ "$DRY" = "1" ]; then
        echo "DRY: POST $API/repos/$REPO/releases  (tag_name=$TAG, name=$REL_NAME, prerelease=$PRERELEASE)"
        REL_ID="<new>"
    else
        resp="$(api POST "/repos/$REPO/releases" \
            -F "tag_name=$TAG" -F "name=$REL_NAME" \
            -F "body=$NOTES_TEXT" -F "target_commitish=$REF" \
            -F "prerelease=$PRERELEASE")"
        code="$(printf '%s' "$resp" | tail -1)"
        body="$(printf '%s' "$resp" | sed '$d')"
        case "$code" in
            2*) REL_ID="$(json_id "$body")"; say "✓ Release 已创建（id=$REL_ID）" ;;
            *)  echo "FAIL: 创建 Release 失败（HTTP $code）: $(printf '%s' "$body" | head -c 400)" >&2; exit 1 ;;
        esac
        [ -n "$REL_ID" ] || { echo "FAIL: 返回里没有 release id: $(printf '%s' "$body" | head -c 300)" >&2; exit 1; }
    fi
fi

# ── 3. 附件 ────────────────────────────────────────────────────
for f in "${ASSETS[@]}"; do
    base="$(basename "$f")"
    if [ "$DRY" = "1" ]; then
        echo "DRY: POST $API/repos/$REPO/releases/$REL_ID/attach_files  file=@$f"
        continue
    fi
    resp="$(api POST "/repos/$REPO/releases/$REL_ID/attach_files" -F "file=@$f")"
    code="$(printf '%s' "$resp" | tail -1)"
    case "$code" in
        2*) say "✓ 上传 $base（$(du -h "$f" | cut -f1)）" ;;
        *)  echo "FAIL: 上传 $base 失败（HTTP $code）: $(printf '%s' "$resp" | sed '$d' | head -c 300)" >&2; exit 1 ;;
    esac
done

say "完成"
if [ "$DRY" = "0" ]; then
    echo "  Release 页面: https://gitee.com/$REPO/releases/tag/$TAG"
    echo "  安装地址:     https://gitee.com/$REPO/releases/download/$TAG/$(basename "${ASSETS[0]}")"
fi
