# bifrost-agent（Hermes profile distribution）

装上即带 bifrost MCP 的工具定义；**每台机器还需要把二进制放上 PATH**（一条命令，由 `setup-bifrost` skill 完成）。

## 安装

```bash
hermes profile install git@gitee.com:<owner>/bifrost-agent.git
hermes profile use bifrost-agent          # 或 hermes -p bifrost-agent chat
```

## 装完后的一次性步骤

在 Hermes 里说一句「按 setup-bifrost skill 装一下」，或手工：

```bash
mkdir -p ~/.local/bin
install -m 0755 \
  "${HERMES_HOME:-$HOME/.hermes}/profiles/bifrost-agent/skills/setup-bifrost/scripts/bifrost" \
  ~/.local/bin/bifrost
~/.local/bin/bifrost --version
```

然后确认 PATH 包含 `~/.local/bin`，重启会话，`hermes mcp test bifrost` 应发现 5 个工具。

## 为什么二进制放在 skill 里

`distribution_owned` 不复制仓库根下的任意目录（实测 `bin/` 会被忽略），但 `skills/` 是完整复制的。
把二进制放在 `skills/setup-bifrost/scripts/bifrost` 就能随 distribution 一起分发，无需另建 Release。
