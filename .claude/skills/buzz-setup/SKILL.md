---
name: buzz-setup
description: Use when someone wants to deploy their own Buzz relay and cloud-hosted agents (separate Claude or Codex containers, each with its own MCP servers) to AWS from this fork, using the Makefile, Terraform, and scripts under scripts/aws and infra.
---

# Buzz AWS setup

Deploys the Buzz relay and named agent containers to one AWS server. Every value below is collected from the person you are helping. Never reuse values from another person's setup.

Status markers: **[verified]** was run end to end. **[unverified]** was written from the design and tests but has not run against real AWS yet. Update this file when a step is proven or a gotcha is found.

## How to run this skill

You guide the person and run the commands. The person only does what no agent can reach: choose settings, approve cost, and click through the desktop or phone app.

- Work through sections 1 to 4b in order. Say which step you are on and what it does in plain words.
- Ask every question with AskUserQuestion: 2 to 4 options, the recommended one first. Never end a message with a question the person has to type an answer to.
- Get explicit approval before `make apply` and before the first `make deploy`. Both cost money or change a live server.
- Run the commands yourself. Do not ask the person to run them, except for app steps.
- Never print a private key, token, or password unless the person asks for that one key. Load tokens from the shell or a git-ignored file.
- A step marked **[unverified]** has not run against real AWS. Say so before you run it, and change its marker when it works.
- Long commands (`make deploy` takes about 20 minutes on a cold build) belong in the background. Tell the person you are waiting.
- If a command fails, read the full output, then fix the cause. AWS cuts remote error text at about 8000 characters, so read it with `aws ssm get-command-invocation`.

## 1. Collect values

Ask for these with AskUserQuestion, one decision at a time. Write the answers to the git-ignored `.aws.env` (copy `.aws.env.example`).

| Setting | Meaning |
|---|---|
| `AWS_PROFILE` | AWS CLI profile for the account that will host Buzz. |
| `TF_STATE_BUCKET` | S3 bucket for Terraform state. Must already exist with versioning on. |
| `ROUTE53_ZONE` | Route 53 hosted zone that will contain the relay host name. |
| `BUZZ_DOMAIN` | Bare host name, for example `buzz.<zone>`. No scheme, slash, or trailing dot. |
| `RELAY_OWNER_PUBKEY` | 64 lowercase hex characters. Not an `npub`. |
| `AGENTS` | Comma list of `name:runtime`. Runtime is `claude` or `codex`. Names match `^[a-z][a-z0-9-]{0,23}$`. |
| `AGENT_RESPOND_TO` | `owner-only` (default), `allowlist`, `anyone`, or `nobody`. |

Optional: `AWS_REGION` (default `us-east-1`), `INSTANCE_TYPE` (default `t4g.large`).

Owner key: if the person has no Buzz key, generate one outside the repo (file mode 600) that holds the `nsec`, `npub`, and hex public key, and use the hex public key as `RELAY_OWNER_PUBKEY`. A generated key imported into the desktop app without problems. Never print the private key in chat or commit it. **[verified]**

## 2. Prerequisites to check before anything else

- **Terraform 1.10 or newer.** Older versions fail `make init` because the state backend uses S3 native locking. If the installed version is older, download a 1.10+ binary into a user directory and put it on PATH for these commands only. Do not replace the person's install. **[verified]**
- **State bucket exists and is versioned.** The scripts do not create it. **[verified]**
- **Docker running** (the image builds for linux/arm64 with buildx). **[verified]**
- **AWS profile works:** `aws sts get-caller-identity --profile <profile>`.
- **Claude agents need a token in the shell.** Export `CLAUDE_CODE_OAUTH_TOKEN` (or `ANTHROPIC_API_KEY`) before `make agents-secrets`. A `.env` that names it `ANTHROPIC_OAUTH_TOKEN` must be mapped to `CLAUDE_CODE_OAUTH_TOKEN` first. Load it without printing it. **[verified for the variable names; the adapter honoring the token is unverified]**
- Codex agents need `OPENAI_API_KEY`.

## 3. Per-agent MCP servers

Each agent gets its own MCP config from a committed file `deploy/aws/agents/<name>/mcp.json` (same shape as a `.mcp.json`: `{"mcpServers": {...}}`). No file means the agent has no extra MCP servers.

- Do not put secrets in `mcp.json`. It is committed to git.
- The staged directory must be mode 0755 and the file 0644, because the container runs as uid 1001. The render script sets this. **[verified in tests]**
- Example: one agent can use the filesystem server (`@modelcontextprotocol/server-filesystem` on `/workspace`) and another the memory server (`@modelcontextprotocol/server-memory`). Pin the package versions in `args`.
- Different agents must use different MCP servers by design. Prove it with the repo test `tests/aws/test-agents-mcp.sh`. **[verified locally, no model call]**

## 4. Deploy

Run from the repo root with `.aws.env` in place. Show the plan to the person and get approval before `make apply`, the first step that costs money.

1. `make init`, then `make plan`. A clean first plan adds 32 resources and changes nothing. **[verified]**
2. `make apply` (after approval). Creates the server, disk, IP, DNS record, image repositories, backup bucket, and IAM. It piped `yes` to the prompt when run by an agent. About two minutes. **[verified]**
   First boot takes about 100 seconds after the server registers with SSM. Confirm with `cloud-init status` over SSM: Docker 25 active, Compose v5.6.0, an ext4 data disk mounted at `/var/lib/docker`, storage driver `overlay2`, and an empty `/opt/buzz`. **[verified]**
3. `make secrets` creates the relay secrets in SSM Parameter Store. It never overwrites. **[verified]**
4. `make deploy` backs up, builds, pushes, and releases. The first cold build took about 20 minutes. Then check `https://<BUZZ_DOMAIN>/_liveness`. The relay starts first. If agents are listed in `AGENTS` but have no secrets or registration yet, the agent phase fails, the relay stays up, and the command exits non-zero on purpose. Continue with step 5. The error text on screen is cut off by AWS after about 8000 characters, so read the full output with `aws ssm get-command-invocation`. **[verified: relay answered 200 on the first deploy]**
5. Export the Claude token (see prerequisites), then run `make agents-secrets` and `make agents-register`. Registration needs the relay running, so it comes after the first `make deploy`. It is safe to rerun: each agent keeps a key, a public key, and a `REGISTERED` marker in Parameter Store, and a failed registration resumes on the next run. **[verified: both agents registered against the real relay]**
6. Run `make deploy` again. The agents start only after the relay is healthy. A stale agent project (one removed from `AGENTS`) is taken down without deleting its volume. Both agent containers started, logged `connected to relay` and `presence set to online`, and reached the relay through the public address. **[verified]**
7. Make the agents appear, with names, in the owner's Desktop agent list. Desktop lists a relay agent only when the owner has signed a record for it and the agent's profile carries the owner's proof. Neither the relay deploy nor the agent containers publish these. Export the owner's secret key in your shell (`export BUZZ_OWNER_KEY=<hex or nsec>`; it is used only on your machine, never sent to the server), then run `make agents-attest`. It publishes the owner record, then runs `buzz users set-profile` inside each container with the owner's proof, so the profile gets the agent's name, about text, and picture. It needs `cargo` and is safe to rerun. Rerun it after adding an agent or changing its profile.

   **Choose names and settings before the first deploy.** The `AGENTS` name (lowercase, for example `coder`) is the agent's identity and cannot be renamed later without creating a new agent. Everything else is a file in `deploy/aws/agents/<name>/`. `agent.json` is the agent's full spec: name, about text, model, effort, who it answers, how many conversations at once, timeouts, memory, and an `instructions_file`. `avatar.png` is the picture and `mcp.json` the tools. Print the field list with `python3 scripts/aws/agent_spec.py fields` and check a file with `python3 scripts/aws/agent_spec.py check <file>`. Ask the person what each agent should be called, what it does, which model it should use, and whom it should answer, then write those files. Runtime fields apply on `make deploy`. Profile fields and the owner record apply on `make agents-attest`. Details are in `deploy/aws/agents/README.md`. **[verified: the relay holds a named, owner-signed profile and an owner record for both agents. Desktop display not yet confirmed.]**
8. `make status`, `make agents-status`, and `make logs` (or `make agents-logs AGENT=<name>`) to confirm.

To stop an agent for good, send `!shutdown` in its channel. `make deploy` starts it again.

## 4b. Connect the desktop app as owner

The person does these steps in the app. You run the server commands. Tell them each step, wait for them to finish it, and ask with AskUserQuestion when you need a choice or a value from them.

1. Open the Buzz desktop app. On a fresh install it offers to create a new identity key or use an existing one. Choose **use an existing key** and give it the owner key's `nsec` (the line starting `nsec1` in the owner key file). Never print the key unless the person asks; if they do, print only that line and say it is now in the session transcript. **[verified]**
2. On the community screen choose **Join a community**, then paste `wss://<BUZZ_DOMAIN>`. Do not choose "Create a community" or "I own the community": those start Buzz's hosted-service flow, not a self-hosted relay. **[verified]**
3. The owner key was registered as `owner` when the relay was deployed, so it joins with no approval step. **[verified]**
4. Confirm: the profile public ID must equal the owner key's `npub`. On the server, `select pubkey, role from relay_members` in the `postgres` container (over SSM, from `/opt/buzz/deploy/compose`, `docker compose -p buzz-prod exec -T postgres ...`) shows exactly one `owner` row, and it must equal `RELAY_OWNER_PUBKEY`. **[verified]**

### The app shows "membership is required"

The relay admits only keys it knows. This happens when the app has a different key from the owner key, for example after choosing "create new key".

1. Ask the person to open the Join page and click **Copy** next to their public ID (`npub1...`).
2. Convert it to 64-character hex. This Python snippet decodes the bech32 text (it does not check the checksum):
   ```python
   CH="qpzry9x8gf2tvdw0s3jn54khce6mua7l"
   s="<npub>"; d=[CH.index(c) for c in s[5:]][:-6]; acc=bits=0; out=[]
   for v in d:
       acc=(acc<<5)|v; bits+=5
       while bits>=8: bits-=8; out.append((acc>>bits)&255)
   print(bytes(out).hex())
   ```
3. Add it over SSM from `/opt/buzz/deploy/compose`: `BUZZ_COMPOSE_TLS=true ./run.sh add-member <hex> < /dev/null`. It is safe to rerun. **[verified]**
4. The person reconnects. They are now a `member`, not the owner.

### Switch from a member key to the owner key

In the app: **Settings**, then the **Sign out** section, then **Delete my data**. This wipes the identity key, agent settings, and cached data on that device. Warn the person first, because it also removes any local agents they set up. The dialog shows the current `nsec`, so they can save it first. After the restart, choose **use an existing key** and import the owner `nsec`, then join again. **[verified]**

### Put the agents in a channel

A new agent is a relay member but sits in no channel, so its log says `discovered 0 channel(s)` and `agent will sit idle`, and the person sees nothing to talk to. Join each agent to a channel with the `buzz` command inside its container, over SSM:

`docker exec buzz-agent-<name>-agent-1 buzz --format compact channels join --channel <channel-uuid>`

List channel ids first with `... buzz --format compact channels list`. A fresh relay has `general` and `welcome-everyone`. The running agent notices the membership and logs `subscribing to new channel` with no restart. **[verified]** A reply to a mention in the channel has not been tested. **[unverified]**

### Other clients

The iOS app also takes a relay address as a community. It has to be built or installed separately (`just mobile-dev`). **[unverified]**

### The agents in the app

Before `make agents-attest`, the cloud agents are plain channel members with no name, so the agent list hides them and the app shows a raw key. After it, they appear as owned agents named after `AGENTS` (for example `coder`, `researcher`). If the list is still empty, quit and reopen the app, because it reads the relay agents at startup. Agents the person already had in the app, with other names, are local and not on the relay. Check with the `relay_members` query: only the owner, the cloud agents, and keys you added appear. **[relay side verified; Desktop display unverified]**

Other targets: `make backup`, `make rollback TAG=<tag>`, `make shell`, `make sync-upstream` (merges the upstream project into the fork, never pushes), `make destroy`.

Full detail lives in `docs/aws-deployment.md`.

## 5. Test locally before deploying

Do this before `make apply`. It costs nothing and found real problems. **[verified]**

- `bash tests/aws/run.sh` runs every script test against fake AWS commands. Add `SKIP_IMAGE=1` to skip the slow image tests.
- Terraform: run `terraform fmt -check`, `init -backend=false`, and `validate` in a `hashicorp/terraform:1.10.5` container if the local version is older.
- Connection check: start a throwaway relay with Docker (the stock `ghcr.io/block/buzz:main` image, membership required), register a throwaway key, and run the agent image against it with a dummy API key. The agent log should show `connected to relay` and `presence set to online`. A reply to a mention needs a channel and a real token, so it stays a manual check.
- The commands used are in `docs/aws-deployment.md` under troubleshooting.

## 6. Gotchas learned

- `AWS_REGION` must be the region of the Terraform state bucket. The S3 backend uses it, and a mismatch makes `make init` fail with a `301` error naming the real region. Find it with `aws s3api get-bucket-location --bucket <bucket>`. A bucket in `us-east-1` returns `None`. **[verified]**

- Ports 5432, 6379, and 3000 are often taken on a developer laptop. Use an isolated Compose project with its own ports.
- `deploy/compose/compose.yml` is named `buzz-prod`. Always pass `-p <name>` when running it locally, or it collides with a real deployment.
- Docker Desktop on macOS does not enforce host file modes on bind mounts. Test permission failures with a root-owned file in a named volume.
- A rollback needs an agents image for the target tag. If the tag predates agents, the rollback fails on purpose with a message.

- The agent image pins Claude Code and both adapters (`ARG`s in `deploy/aws/Dockerfile.agents`). To use newer versions, run `npm view <package> version`, update the ARGs and `want_cc` in `tests/aws/test-agents-image.sh`, then rerun the image and MCP tests. The adapter picks up the pinned Claude Code through `CLAUDE_CODE_EXECUTABLE`. **[verified in tests]**
- Terraform below 1.10 is refused on purpose.
- First-boot user data changes do not re-run on an existing server (`ignore_changes`). Re-run the script over SSM or replace the instance.
- `make rollback` without `TAG` redeploys the last good tag, which equals the running tag after a healthy deploy. To leave a bad but healthy release, pass `TAG=`.
- Access is through SSM only. There is no SSH port.
- A cold build of the agent image takes 15 to 20 minutes. The full local test run takes about 10 minutes; `SKIP_IMAGE=1` skips the image tests.
- shellcheck is often missing locally. Use `docker run --rm -v "$PWD:/mnt" koalaman/shellcheck:stable <files>`.

## 7. Keep this file current

When a step is proven on a real deploy, change its marker to **[verified]**. Add each new failure and its fix to Gotchas. Keep personal values (domains, buckets, keys, tokens) out of this file.
