---
name: buzz-setup
description: Use when someone wants to deploy their own Buzz relay and cloud-hosted agents (separate Claude or Codex containers, each with its own MCP servers) to AWS from this fork, using the Makefile, Terraform, and scripts under scripts/aws and infra.
---

# Buzz AWS setup

Deploys the Buzz relay and named agent containers to one AWS server. Every value below is collected from the person you are helping. Never reuse values from another person's setup.

Status markers: **[verified]** was run end to end. **[unverified]** was written from the design and tests but has not run against real AWS yet. Update this file when a step is proven or a gotcha is found.

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

Owner key: if the person has no Buzz key, generate one outside the repo (mode 600) and give them the hex public key. Never print the private key in chat or commit it.

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
3. `make secrets` creates the relay secrets in SSM Parameter Store. It never overwrites. **[unverified]**
4. `make deploy` backs up, builds, pushes, and releases. Then check `https://<BUZZ_DOMAIN>/_liveness`. The relay starts first. If agents are listed in `AGENTS` but have no secrets or registration yet, the agent phase fails, the relay stays up, and the command exits non-zero on purpose. Continue with step 5. **[unverified]**
5. Export the Claude token (see prerequisites), then run `make agents-secrets` and `make agents-register`. Registration needs the relay running, so it comes after the first `make deploy`. It is safe to rerun: each agent keeps a key, a public key, and a `REGISTERED` marker in Parameter Store, and a failed registration resumes on the next run. **[unverified against real AWS; the logic is tested with stubs]**
6. Run `make deploy` again. The agents start only after the relay is healthy. A stale agent project (one removed from `AGENTS`) is taken down without deleting its volume. **[unverified against real AWS]**
7. `make status`, `make agents-status`, and `make logs` (or `make agents-logs AGENT=<name>`) to confirm.

To stop an agent for good, send `!shutdown` in its channel. `make deploy` starts it again.

Connect a client: the Buzz desktop app and the iOS app both take a relay address as a "community". Add `wss://<BUZZ_DOMAIN>` and sign in with the owner key. The iOS app has to be built or installed separately (`just mobile-dev`). **[unverified: the add-community screens were not checked]**

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
