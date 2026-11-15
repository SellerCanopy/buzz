# Per-agent settings

Each agent has a folder, `deploy/aws/agents/<name>/`, where `<name>` is the agent
name from `AGENTS` (the part before the colon, or the whole entry for a bare
`claude` or `codex`). The folder holds up to four things:

| File | Sets | Applied by |
|------|------|------------|
| `agent.json` | Name, profile, model, effort, who it answers, limits, instructions | see the table below |
| `avatar.png` (or `.jpg`, `.jpeg`, `.webp`, `.gif`) | Profile picture | `make agents-attest` |
| `mcp.json` | MCP tool servers | `make deploy` (see below) |
| `instructions.md` (any name) | Standing instructions, named by `instructions_file` | `make deploy` |

Every file is optional.

## agent.json

Every field is optional. This is the full list:

| Field | Type | Applied by | Meaning |
|-------|------|------------|---------|
| `display_name` | text | `make deploy` and `make agents-attest` | Name people see. Defaults to the AGENTS name. |
| `about` | text | `make agents-attest` | Profile text. |
| `model` | token | `make deploy` | Model for every session: default, sonnet, fable, opus, haiku. |
| `effort` | token | `make deploy` | How hard the model thinks: low, medium, high. |
| `permission_mode` | `default` or `auto` or `accept-edits` or `bypass-permissions` or `dont-ask` or `plan` | `make deploy` | Tool permission mode. Default bypass-permissions. |
| `respond_to` | `owner-only` or `allowlist` or `anyone` or `nobody` | `make deploy` and `make agents-attest` | Whose messages it answers. Defaults to AGENT_RESPOND_TO. |
| `respond_to_allowlist` | list of public keys | `make deploy` and `make agents-attest` | Public keys it answers when respond_to is allowlist (the owner is always included). |
| `parallelism` | whole number 1 to 32 | `make deploy` and `make agents-attest` | Conversations it works on at once. |
| `session_policy` | `channel` or `thread` | `make deploy` | One session per channel or per thread. |
| `multiple_event_handling` | `queue` or `steer` or `interrupt` or `owner-interrupt` | `make deploy` | What a new mention does while it is mid-turn. |
| `context_message_limit` | whole number 0 to 200 | `make deploy` | Earlier messages it reads for a thread or DM reply. |
| `max_turns_per_session` | whole number 0 to 100000 | `make deploy` | Turns before a session restarts. 0 never restarts. |
| `idle_timeout` | whole number 1 to 86400 | `make deploy` | Seconds of silence before a turn is stopped. |
| `max_turn_duration` | whole number 1 to 86400 | `make deploy` | Seconds one turn may last at most. |
| `heartbeat_interval` | whole number 0 to 86400 | `make deploy` | Seconds between self-prompts. 0 turns them off. |
| `heartbeat_prompt` | text | `make deploy` | Text of the self-prompt. |
| `memory` | bool | `make deploy` | Whether the agent keeps notes between conversations. Default true. |
| `instructions_file` | file name | `make deploy` | File in the agent folder with its standing instructions (up to 64 KB). |

Example:

```json
{
  "display_name": "Coder",
  "about": "Writes and edits code.",
  "model": "sonnet",
  "effort": "high",
  "respond_to": "anyone",
  "parallelism": 2,
  "instructions_file": "instructions.md"
}
```

Notes:

- The `AGENTS` name (in `.aws.env`) is the agent's identity: its key, folder, and
  container. Pick it before the first deploy. `display_name` can change freely.
- `respond_to` and `respond_to_allowlist` fall back to `AGENT_RESPOND_TO` and
  `AGENT_RESPOND_TO_ALLOWLIST` in `.aws.env`, so those act as the default for every
  agent that does not set its own.
- `instructions_file` names a file in the agent folder, for example
  `instructions.md`. The deploy copies it next to `mcp.json` and the agent reads it
  at start.
- An unknown field, a wrong type, a value out of range, or an unsafe character
  stops the deploy with an error naming the file. Run
  `python3 scripts/aws/agent_spec.py check deploy/aws/agents/<name>/agent.json` to
  check a file before deploying.
- The list above comes from `scripts/aws/agent_spec.py`. Print it with
  `python3 scripts/aws/agent_spec.py fields`.

To change a setting, edit the file, then run `make deploy` (runtime settings
restart the agent) and `make agents-attest` (profile and owner record). The attest
command needs `BUZZ_OWNER_KEY` in your shell because the owner signs the record and
the picture upload.

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

- **Secrets go in AWS, not in the file.** `mcp.json` is committed to git, so it must
  hold no tokens, keys, or passwords. Write `${NAME}` where the server needs one,
  for example `"env": {"GITHUB_TOKEN": "${GITHUB_TOKEN}"}`. Then store the value:

  ```bash
  export GITHUB_TOKEN=...            # in your shell only
  make agents-env AGENT=coder VAR=GITHUB_TOKEN
  make deploy
  ```

  `make agents-env` saves the value encrypted in AWS Parameter Store at
  `/buzz/agents/<name>/env/<NAME>`. `make agents-env AGENT=coder` with no `VAR`
  lists the secrets the file needs and which are stored. The deploy finds every
  `${NAME}` in the file (one with a `:-default`, or `HOME`, `PATH`, `PWD`, `USER`,
  `SHELL`, needs none), writes those values into that agent's private env file on
  the server, and recreates the container. A missing secret stops the deploy and
  names the command to run. To rotate a secret, run `make agents-env` again, then
  `make deploy`.

  Rules: names use capital letters, digits, and `_`. Names starting with `BUZZ_`,
  `CLAUDE`, `ANTHROPIC_`, `OPENAI_`, or `CODEX_` are reserved. Values may hold only
  `A-Z a-z 0-9 . _ ~ + / = : @ -` because the env file stores them unquoted. An agent
  gets only its own secrets, and only those its `mcp.json` names.

  The agent's tools can read its environment, so a secret is visible to anything the
  agent runs. Give each server a key with the least access it needs.
- **`${VAR}` reads the container environment, not the host.** The file is copied as
  written. Claude Code expands `${VAR}` in it at run time from the agent container's
  environment (proven for the `env` block by `tests/aws/test-agents-mcp.sh`). A
  variable set on your laptop or in `.aws.env` is not there unless you store it with
  `make agents-env`.
- **File modes matter.** The container user is uid 1001 and the mount is read-only,
  so the staged config directory must be mode 0755 and `mcp.json` mode 0644. If the
  file is unreadable, the container exits with a message naming the path.
- **Commands must exist in the image.** The image has Node 22, `git`, `bash`, and
  `buzz-hive-mcp`. A server that needs another program must install it at run time
  (for example with `npx`) or wait for an image change.
- **`buzz-hive` is the Beehive tool server.** The `buzz-hive` entry in each agent's
  file runs `buzz-hive-mcp`, which gives the agent the Beehive tools (create and
  start a card, report a stage result). It signs in with the agent's own key from
  `BUZZ_PRIVATE_KEY` and finds Beehive at `BUZZ_RELAY_URL`'s host, so it needs no
  entry in `env`. Set `BUZZ_HIVE_URL` only where Beehive is on another host. It
  acts at the agent's Beehive level, which an admin sets and which is never above
  the owner's. Remove the entry for an agent that should not see Beehive.
- **Changes need a deploy.** The file is read when the container starts. The
  deploy recreates every agent container each time, so the entrypoint copies the
  new file in and the env file's secrets are read again. The deploy refuses a
  dirty git tree, so commit a file change first. A secret change alone needs no
  commit: run `make agents-env`, then `make deploy`.

To pick settings and MCP servers for a job, use the `buzz-agent-config` skill
(`.claude/skills/buzz-agent-config/SKILL.md`).
