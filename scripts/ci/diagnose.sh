#!/bin/bash
# diagnose.sh —— 门禁第一步：把构建环境打印出来
#
# 为什么：构建镜像内容不透明（有没有 python3？rustc 到底几版？能装 musl target 吗？），
# 失败时日志里没有这些信息，只能靠猜。让每个构建日志的头 20 行就带上环境指纹，
# 出问题一眼定位。
set -uo pipefail

echo "===================== 构建环境诊断 ====================="
echo "时间:      $(date -u +%FT%TZ)"
echo "系统:      $(uname -srm)"
echo "发行版:    $( (cat /etc/os-release 2>/dev/null | sed -n 's/^PRETTY_NAME=//p') || echo unknown)"
echo "用户:      $(id -un 2>/dev/null) (uid=$(id -u 2>/dev/null))"
echo "工作目录:  $(pwd)"
# CI 通用 + GitHub Actions 注入的变量；本机跑时就空
echo "分支:      ${GITHUB_REF_NAME:-${BIFROST_BRANCH:-$(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo unknown)}}"
echo "提交:      ${GITHUB_SHA:-$(git rev-parse HEAD 2>/dev/null || echo unknown)}"
echo "构建号:    ${GITHUB_RUN_NUMBER:-<本机>}"
echo "工作流:    ${GITHUB_WORKFLOW:-<本机>}"
echo "GH Token:  $([ -n "${GH_TOKEN:-${GITHUB_TOKEN:-}}" ] && echo '已配置' || echo '缺失（发布到 Release 会失败）')"

echo "---------- 工具链 ----------"
for c in cargo rustc rustup git python3 python cc gcc dpkg-deb ldd tr head gh; do
    p="$(command -v "$c" 2>/dev/null)"
    echo "$(printf '%-9s' "$c") ${p:-<缺失>}"
done
cargo --version 2>/dev/null || true
rustc --version 2>/dev/null || true

echo "---------- 系统工具自检（测试里用到）----------"
if head -c 4 /dev/zero 2>/dev/null | tr '\000' X 2>/dev/null | grep -q XXXX; then
    echo "head/tr 可用（测试用它造大输出，替代 python）"
else
    echo "!! head/tr 不可用 —— 部分输出相关测试会失败"
fi

echo "---------- Rust targets ----------"
rustup target list --installed 2>/dev/null || echo "<无法列出>"

echo "---------- 磁盘 ----------"
df -h . 2>/dev/null | tail -2

echo "---------- 缓存 ----------"
[ -d target ] && du -sh target 2>/dev/null || echo "target/ 不存在（全量编译）"
echo "===================== 诊断结束 ====================="
