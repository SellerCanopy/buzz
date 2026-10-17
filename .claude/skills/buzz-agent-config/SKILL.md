---
name: buzz-agent-config
description: Use when someone wants to add a cloud Buzz agent, or change an existing one, based on the job it should do (coding, research, support triage, monitoring). Picks the model, effort, permissions, who it answers, instructions, and MCP servers, then writes deploy/aws/agents/<name>/ and ships it.
---

# Configure a Buzz agent for a job

Turns "I want an agent that does X" into the files under `deploy/aws/agents/<name>/`, then deploys them. The relay and server must already exist (see the `buzz-setup` skill).

## How to run this skill

- Ask every question with AskUserQuestion: 2 to 4 options, the recommended one first, with what each choice does in its description.
- Show the files you drafted before you write them. Write only after the person approves.
- Run the commands yourself. The person only checks the desktop app.
- Never put a token, key, or password in any file in `deploy/aws/agents/`. Those files are committed to git.
- Write no em-dashes and no emojis. Commit with `git commit -s` and no attribution line.

## Agent structure

One entry in `AGENTS` (in `.aws.env`, as `name:runtime`) gives one agent. The name ties every piece together.

```
deploy/aws/agents/<name>/        in git: what you edit
  agent.json                     settings (see step 2)
  mcp.json                       tool servers
  instructions.md                standing instructions (any file name)
  avatar.png                     profile picture

AWS SSM Parameter Store          created by make agents-secrets / agents-register
  /buzz/agents/<name>/BUZZ_PRIVATE_KEY, BUZZ_PUBLIC_KEY, REGISTERED
  /buzz/agents/<name>/env/<NAME>   MCP secrets, from make agents-env

server, under /opt/buzz/agents   written by make deploy
  <name>.env                     BUZZ_ACP_* settings from agent.json, plus its MCP secrets
  <name>/config/                 mcp.json and the instructions file, mounted at /etc/buzz-agent
  agents.list                    the names currently deployed

Docker                           one Compose project per agent
  project buzz-agent-<name>      container buzz-agent-<name>-agent-1, own network and home volume
```

Inside the container the agent runs as user 1001 in `/home/agent`. Its `.mcp.json` is copied there at start. The relay's databases are not reachable from it.

The `<name>` must match in all places. Removing a name from `AGENTS` stops its container on the next deploy but keeps its home volume and keys.

## 1. Learn the job

Ask what the agent is responsible for. Offer these, plus Other:

- **Writes code.** Edits files and runs commands.
- **Researches.** Looks things up and keeps notes.
- **Triages for a team.** Answers many people in a channel.
- **Watches something.** Wakes up on a timer and reports.

Then ask in one more question who may talk to it: only the owner, a list of people, or anyone in the channel. Ask for the agent's name last. The name in `AGENTS` is its identity and a rename creates a new agent, so confirm it before the first deploy.

## 2. Pick starting settings

These are starting points, not tested recommendations. Show the person the row and let them change it.

| Job | model | effort | respond_to | parallelism | Other fields | MCP servers |
|-----|-------|--------|------------|-------------|--------------|-------------|
| Writes code | `sonnet` (`opus` for hard work) | `high` | `owner-only` | 2 | `memory: true` | filesystem |
| Researches | `opus` | `medium` | `owner-only` | 1 | `memory: true` | memory, plus a web fetch server if asked |
| Triages for a team | `sonnet` | `low` | `allowlist` or `anyone` | 4 | `session_policy: thread`, `multiple_event_handling: queue`, `context_message_limit: 30` | only what the answers need |
| Watches something | `haiku` | `low` | `owner-only` | 1 | `heartbeat_interval: 3600`, a `heartbeat_prompt` | the server for the thing it watches |

Notes that change the choice:

- `permission_mode` defaults to `bypass-permissions`, so the agent runs tools without asking. If the person wants an agent that proposes and does not act, set `plan`.
- `respond_to: anyone` lets every channel member spend the owner's Claude quota. Say so before you pick it.
- `allowlist` needs `respond_to_allowlist`, a list of 64-character hex public keys. The owner is always included.
- `instructions_file` holds standing instructions (up to 64 KB). Write one for any job beyond "general helper": what to do, what never to do, how to report.
- The full field list comes from `python3 scripts/aws/agent_spec.py fields`. Use that output, not this table, when they disagree.

## 3. Pick MCP servers

An MCP server is a program the agent calls as a tool. Each agent has its own `mcp.json`, in Claude Code's `.mcp.json` format.

1. Choose the fewest servers that cover the job.
2. Pin each package to an exact version: `"@scope/pkg@1.2.3"`. Look up the current one with `npm view <pkg> version` and say which you chose.
3. Check the server starts: run its command once on your machine with `npx -y <pkg>@<version>` and confirm it waits for input instead of exiting with an error.
4. The image has only Node 22, `git`, and `bash`. A server that needs Python or another program will not start.
5. If a server needs a secret, such as an API key, never write the value in `mcp.json`. Write `${NAME}` in its `env` block, for example `"env": {"GITHUB_TOKEN": "${GITHUB_TOKEN}"}`, and store the value in step 5 below. Ask the person for a key with the least access the job needs, because the agent's tools can read its environment.

Codex agents ignore `mcp.json`.

## 4. Write and check the files

Create or edit, in `deploy/aws/agents/<name>/`:

- `agent.json` with only the fields that differ from the defaults.
- `mcp.json`. An agent with no file gets an empty server list.
- `instructions.md` and an `instructions_file` entry, when step 2 calls for it.
- `avatar.png` (or `.jpg`, `.jpeg`, `.webp`, `.gif`) if the person has a picture. Use a square image. The committed pictures are 512 by 512 PNG placeholders, so ask whether the person wants their own. If the folder has no picture, the agent gets no profile picture.

`make agents-attest` uploads the picture to the relay's media store with the owner key and sets it as the agent's profile picture. It stops with an error if the upload fails.

For a new agent, also add `name:claude` to `AGENTS` in `.aws.env`.

Check before you deploy:

```bash
python3 scripts/aws/agent_spec.py check deploy/aws/agents/<name>/agent.json
python3 -c 'import json,sys; c=json.load(open(sys.argv[1])); assert isinstance(c["mcpServers"], dict)' deploy/aws/agents/<name>/mcp.json
```

## 5. Ship it

0. For each `${NAME}` in `mcp.json`, ask the person to put the value in their shell with `export NAME=...`. Do not read it from a file, and never print it. Then run `make agents-env AGENT=<name> VAR=NAME`. Run `make agents-env AGENT=<name>` with no `VAR` to list what is stored and what is missing. Names must be capital letters, digits, and `_`. Values may hold only `A-Z a-z 0-9 . _ ~ + / = : @ -`, so tell the person if a key has other characters.
1. Commit the files. `make deploy` refuses a dirty git tree.
2. Run `make deploy`. It recreates every agent container, so file changes and secret changes both apply.
3. If this is a new agent, run `make agents-secrets` and `make agents-register` once, then `make deploy` again. Registration needs the relay running.
4. Run `make agents-attest` with the owner key in the shell, then unset it. This publishes the name, about text, picture, and owner record. Run it whenever `display_name`, `about`, the picture, `respond_to`, `respond_to_allowlist`, or `parallelism` changed.
5. A new agent sits in no channel. Join it to one, as described in the `buzz-setup` skill.

## 6. Verify

Run `make agents-logs AGENT=<name>`. The startup line must show the `model`, `respond_to`, and `agents=` values you set. The log prints no effort value, so effort stays unconfirmed.

To confirm an MCP server loaded, send the agent a message that needs its tool, for example "list the files in your home folder" for the filesystem server. Tell the person to reopen the desktop app and check the name and picture.

## Changing, removing, or undoing an agent

**Change a setting.** Edit the file, then follow step 5 from "commit". Skip `agents-secrets` and `agents-register`. Use this table to know what to run:

| You changed | Run |
|-------------|-----|
| `model`, `effort`, `permission_mode`, limits, `memory`, `heartbeat_*`, `session_policy`, `multiple_event_handling`, `instructions_file` or its file, `mcp.json` | `make deploy` |
| A secret value for `mcp.json` | `make agents-env AGENT=<name> VAR=NAME`, then `make deploy` (no commit needed) |
| `about`, the picture | `make agents-attest` |
| `display_name`, `respond_to`, `respond_to_allowlist`, `parallelism` | both |

**Rename.** The `AGENTS` name cannot change. A new name creates a new agent with a new key, and the old one stops. `display_name` changes freely.

**Remove.** Delete the name from `AGENTS`, commit, and run `make deploy`. The container stops. Its home volume, keys, and relay membership stay. Deleting the folder under `deploy/aws/agents/` removes only the settings.

**Undo a bad release.** `make rollback TAG=<earlier commit>` redeploys the earlier image. The agents restart on that commit's files.

If an agent's container keeps restarting, run `make agents-logs AGENT=<name>`. A bad `mcp.json` or `agent.json` is named in the first lines.

## How an MCP change reaches an agent

1. `make deploy` copies `deploy/aws/agents/<name>/mcp.json` into that agent's own folder on the server, mode 0644.
2. Compose mounts that folder read-only at `/etc/buzz-agent` in that agent's container only.
3. The container starts through `deploy/aws/agent-entrypoint.sh`. It checks the file is JSON with an `mcpServers` object, then overwrites `/home/agent/.mcp.json` with it. Claude Code reads that file when a session starts.

The agent never re-reads the file while it runs. The change takes effect because the deploy starts a new container. The deploy recreates every agent container each time, whether or not the commit changed. An invalid file stops the container with an error in its log.

Secrets follow the same path. `make deploy` writes each `${NAME}` the file uses into that agent's env file, and Claude Code fills it into the server's `env` block when a session starts.

Editing the file on the server does nothing until the container restarts. Do not do that: the next deploy overwrites it.
