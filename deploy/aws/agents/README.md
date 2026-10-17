# Per-agent MCP config

Each cloud agent can have its own MCP servers (tool servers the agent can call).
Put the config in `deploy/aws/agents/<name>/mcp.json`, where `<name>` is the agent
name from `AGENTS` (the part before the colon, or the whole entry for a bare
`claude` or `codex`).

The file uses Claude Code's `.mcp.json` format:

```json
{
  "mcpServers": {
    "docs-search": {
      "command": "npx",
      "args": ["-y", "some-mcp-server"]
    }
  }
}
```

An agent without a file gets `{"mcpServers": {}}`. Codex agents ignore the file.

## How it is loaded

The deploy copies the file into a per-agent directory on the server. Compose mounts
that directory read-only at `/etc/buzz-agent` in that agent's container only. At
start, `deploy/aws/agent-entrypoint.sh` copies `/etc/buzz-agent/mcp.json` to
`$HOME/.mcp.json`, or writes an empty server list when there is no file. It exits
with an error if the file is not a JSON object with a `mcpServers` object.

The agent process runs in `/home/agent`, so Claude Code reads that file as the
project config. Servers start when a session starts, with no approval step.
Because each agent has its own container, home volume, and mount, an agent never
sees another agent's file.

Proven by `tests/aws/test-agents-mcp.sh` against adapter
`@agentclientprotocol/claude-agent-acp` 0.93.0: two agents with different files
each start only their own probe server, and an agent with no file starts none.
The message sequence that starts the servers is `initialize`, then `session/new`.
Other install locations were not needed.

## Limits

- **No secrets.** `mcp.json` is committed to git, so it must hold no tokens,
  keys, or passwords. A later change will add per-agent secret injection.
- **`${VAR}` reads the container environment, not the host.** The file is copied as
  written. Claude Code expands `${VAR}` in it at run time from the agent container's
  environment (checked with a probe server). That environment holds only what the
  deploy renders into the agent's env file, so a variable set on your laptop or in
  `.aws.env` is not there.
- **Commands must exist in the image.** The image has Node 22, `git`, and `bash`.
  A server that needs another program must install it at run time (for example
  with `npx`) or wait for an image change.
- **Changes need a deploy.** The file is read when the container starts.
