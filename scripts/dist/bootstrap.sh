#!/bin/bash
# bootstrap.sh —— 从发布包/URL 安装 bifrost MCP（一句话版）
#
# 用法:
#   ./bootstrap.sh --from /data/liuxin/dist/bifrost-0.1.0-x86_64-linux-musl.tar.gz \
#                  --storage /data/liuxin/bifrost
#   ./bootstrap.sh --from https://github.com/<you>/bifrost/releases/download/v0.1.0/<pkg>.tar.gz \
#                  --sha256 <hex> --storage /data/liuxin/bifrost
#
# 它做四件事：取包 → 校验 → 调 install.sh → 装 skill
set -uo pipefail

SRC=""          # 本地文件 / 目录 / http(s) URL
SHA=""          # 期望的 sha256（URL 来源建议必填）
STORAGE=""
NAME="bifrost"
SETTINGS=""
NO_SKILL=0
NO_HERMES=0
ROLE=""
EXTRA=()

while [ $# -gt 0 ]; do
    case "$1" in
        --from)      SRC="$2"; shift 2 ;;
        --sha256)    SHA="$2"; shift 2 ;;
        --storage)   STORAGE="$2"; shift 2 ;;
        --role)      ROLE="$2"; shift 2 ;;
        --name)      NAME="$2"; shift 2 ;;
        --settings)  SETTINGS="$2"; shift 2 ;;
        --no-skill)  NO_SKILL=1; shift ;;
        --no-hermes) NO_HERMES=1; shift ;;
        -h|--help)   sed -n '2,14p' "$0" | sed 's/^# \{0,1\}//'; exit 1 ;;
        *) echo "未知参数: $1" >&2; exit 1 ;;
    esac
done

[ -n "$SRC" ] || { echo "FAIL: 需要 --from <tarball|目录|URL>" >&2; exit 1; }

WORK="$(mktemp -d)"; trap 'rm -rf "$WORK"' EXIT

# ---- 1. 取包 ----
case "$SRC" in
    http://*|https://*)
        OUT="$WORK/$(basename "${SRC%%\?*}")"
        echo "→ 下载 $SRC"
        if command -v curl >/dev/null; then curl -fsSL --retry 3 -o "$OUT" "$SRC" || { echo "FAIL: 下载失败" >&2; exit 1; }
        else wget -qO "$OUT" "$SRC" || { echo "FAIL: 下载失败（无 curl/wget）" >&2; exit 1; }; fi
        ;;
    *)  OUT="$SRC" ;;
esac

# ---- 2. 校验 ----
if [ -n "$SHA" ]; then
    GOT="$(sha256sum "$OUT" 2>/dev/null | cut -d' ' -f1)"
    [ "$GOT" = "$SHA" ] || { echo "FAIL: sha256 不匹配" >&2
        echo "  期望: $SHA" >&2; echo "  实际: $GOT" >&2; exit 1; }
    echo "✓ sha256 校验通过"
elif [ -f "$OUT.sha256" ]; then
    EXP="$(cut -d' ' -f1 "$OUT.sha256")"
    GOT="$(sha256sum "$OUT" | cut -d' ' -f1)"
    [ "$EXP" = "$GOT" ] && echo "✓ sha256 校验通过（来自 .sha256 文件）" \
        || { echo "FAIL: sha256 与同目录 .sha256 不符" >&2; exit 1; }
else
    echo "! 未提供 --sha256，跳过校验（生产环境请补上）"
fi

# ---- 3. 解包 ----
if [ -d "$OUT" ]; then
    PKG="$OUT"
else
    tar xzf "$OUT" -C "$WORK" || { echo "FAIL: 解包失败" >&2; exit 1; }
    PKG="$(find "$WORK" -maxdepth 2 -name install.sh -printf '%h\n' | head -1)"
    [ -n "$PKG" ] || { echo "FAIL: 包里找不到 install.sh" >&2; exit 1; }
fi

# ---- 4. 安装 ----
ARGS=(--name "$NAME")
[ -n "$SETTINGS" ] && ARGS+=(--settings "$SETTINGS")
[ -n "$STORAGE" ] && ARGS+=(--storage "$STORAGE")
[ -n "$ROLE" ] && ARGS+=(--role "$ROLE")
[ "$NO_HERMES" = "1" ] && ARGS+=(--no-hermes)
echo "→ 运行 $PKG/install.sh ${ARGS[*]}"
bash "$PKG/install.sh" "${ARGS[@]}" "${EXTRA[@]}" || exit 1

# ---- 5. 装 skill（让 agent 自己知道怎么用/排障）----
if [ "$NO_SKILL" = "0" ] && [ -f "$PKG/SKILL.md" ]; then
    HOMES=()
    [ -n "${HERMES_HOME:-}" ] && HOMES+=("$HERMES_HOME")
    HOMES+=("/opt/data" "$HOME/.hermes")
    for h in "${HOMES[@]}"; do
        if [ -d "$h/skills" ]; then
            mkdir -p "$h/skills/setup-bifrost"
            install -m 0644 "$PKG/SKILL.md" "$h/skills/setup-bifrost/SKILL.md"
            echo "✓ skill: $h/skills/setup-bifrost/SKILL.md"
            break
        fi
    done
fi

echo
echo "完成。验证:"
echo "  hermes mcp list | grep $NAME"
echo "  hermes mcp test $NAME"
