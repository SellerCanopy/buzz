#!/bin/bash
set -euo pipefail

# Installs this agent's MCP server config, then hands over to the shared sprig
# entrypoint, which ends in exec buzz-acp so shutdown signals reach the runner.
#
# /etc/buzz-agent/mcp.json (mounted read-only by compose.agents.yml) uses Claude
# Code's .mcp.json format. The Claude adapter starts the agent in $HOME and reads
# project servers from $HOME/.mcp.json, so each agent sees only its own file. An
# agent without a file gets an empty server list. Codex agents ignore the file.

src=/etc/buzz-agent/mcp.json
dst="$HOME/.mcp.json"
tmp="$dst.tmp"

if [ -f "$src" ]; then
  if [ ! -r "$src" ]; then
    echo "agent-entrypoint: cannot read $src (check that the staged file is mode 0644 and its directory 0755)" >&2
    exit 1
  fi
  node -e '
    const cfg = JSON.parse(require("fs").readFileSync(process.argv[1], "utf8"));
    const s = cfg && cfg.mcpServers;
    if (!s || typeof s !== "object" || Array.isArray(s)) {
      throw new Error("top-level \"mcpServers\" object is missing");
    }
  ' "$src" || {
    echo "agent-entrypoint: $src is not a valid .mcp.json file" >&2
    exit 1
  }
  cp "$src" "$tmp"
else
  printf '%s\n' '{"mcpServers": {}}' > "$tmp"
fi
mv "$tmp" "$dst"

exec /usr/local/bin/sprig-entrypoint "$@"
