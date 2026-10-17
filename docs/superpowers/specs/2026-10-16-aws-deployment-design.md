# AWS deployment for the SellerCanopy fork of Buzz

## Goal

Deploy the SellerCanopy fork of Buzz to AWS for a small team. One command builds the fork's relay image and rolls it out. Another command merges updates from the original project (`block/buzz`) into the fork.

Phase 2 runs always-on coding agents (Claude Code and Codex) in containers on the same server, so agents no longer depend on a laptop being awake. See "Phase 2: cloud agents".

## Decisions

| Topic | Decision |
|---|---|
| Hosting | One EC2 server running the existing Docker Compose stack (`deploy/compose/`) with Caddy for HTTPS |
| Image source | Built from the fork, stored in AWS ECR, pinned by git commit tag |
| Domain | Route53 record in an existing hosted zone |
| Server access | AWS SSM (Systems Manager). No SSH, no open port 22 |
| Terraform state | The existing S3 bucket in the SellerCanopy AWS account. This project does not create it |
| Region | `us-east-1` |
| Agents | Claude Code and Codex in separate containers on the same server, API-key billing, own Compose project |
| Upstream sync | Merge `upstream/main` into the current branch |

## Out of scope

- High availability, autoscaling, and managed databases (RDS, ElastiCache).
- Desktop and mobile app builds. Clients point at the deployed relay's URL.
- The Kubernetes agent provider (`buzz-backend-kubernetes`), which needs a cluster and the desktop app.
- Agent subscription logins. Agents use API keys.
- CI automation. Deploys run from the developer's laptop through `make`.

## Layout

```
Makefile                  Entry point for every operation
.aws.env.example          Template for local settings (copied to .aws.env, git-ignored)
infra/                    Terraform root module
  main.tf, variables.tf, outputs.tf, versions.tf, backend.tf
scripts/aws/
  deploy.sh               Tells the server to run a new image tag
  secrets.sh              Creates secrets in SSM Parameter Store if absent
  sync-upstream.sh        Merges block/buzz into the fork
  backup.sh               Dumps Postgres to S3
  agents-secrets.sh       Stores agent API keys in SSM (phase 2)
  agents-register.sh      Creates and registers agent identities (phase 2)
  render-agents-env.sh    Runs on the server. Builds agents.env from SSM (phase 2)
  user-data.sh.tftpl      First-boot setup (Docker, Compose, repo files)
deploy/aws/
  Dockerfile.agents       Node 22 image with Buzz tools, Claude and Codex adapters (phase 2)
  compose.agents.yml      Separate Compose project for the agents (phase 2)
```

## Settings (`.aws.env`)

Required values, with no defaults for the first five:

- `AWS_PROFILE`: profile for the SellerCanopy account.
- `TF_STATE_BUCKET`: name of the existing state bucket.
- `ROUTE53_ZONE`: hosted zone name, for example `example.com`.
- `BUZZ_DOMAIN`: full host name, for example `buzz.example.com`.
- `RELAY_OWNER_PUBKEY`: 64-character hex Nostr public key of the owner.
- `AWS_REGION`: defaults to `us-east-1`.
- `INSTANCE_TYPE`: defaults to `t4g.large` (8 GB). Agents run git checkouts and builds next to Postgres, Redis, and MinIO, so `t4g.medium` is too small.
- `AGENTS`: comma-separated agent names, defaults to `claude,codex`. Valid names are `claude` and `codex`. Empty disables phase 2.
- `AGENT_RESPOND_TO`: who may talk to the agents. One of `owner-only` (default), `allowlist`, `anyone`, `nobody`.
- `AGENT_RESPOND_TO_ALLOWLIST`: comma-separated 64-hex pubkeys. Required when `AGENT_RESPOND_TO=allowlist`.

`TF_STATE_BUCKET` feeds `terraform init -backend-config`. The state key is `buzz/terraform.tfstate`. Terraform's S3 backend uses `use_lockfile = true` for locking, so no DynamoDB table is needed. This needs Terraform 1.10 or newer; `versions.tf` pins that minimum. The installed binary must meet it, and `make init` checks it.

## What Terraform builds

- **Network:** A small VPC with one public subnet, an internet gateway, and a route table.
- **Firewall:** A security group allowing inbound 80 and 443 only. No inbound 22.
- **Server:** One `t4g.large` (ARM) instance, Amazon Linux 2023, with the SSM agent. IMDSv2 required.
- **Fixed IP:** An Elastic IP attached to the server.
- **Data disk:** A separate EBS volume mounted for Docker volumes (Postgres, git, media, Caddy). Daily snapshots through Data Lifecycle Manager, 7 kept.
- **Image store:** Two ECR repositories, `relay` and `agents`, each with a lifecycle rule keeping the 10 newest images.
- **DNS:** A Route53 `A` record for `BUZZ_DOMAIN` pointing at the Elastic IP.
- **IAM role for the server:** SSM core access, read-only ECR pull, read of `/buzz/*` SSM parameters, write to the backup bucket prefix.
- **Backup bucket:** A private, versioned S3 bucket for `pg_dump` output, with a 30-day expiry rule.
- **First boot:** Installs Docker and the Compose plugin, mounts the data disk, and starts nothing until the first deploy. The first deploy delivers the Compose files.

The Compose version must be 2.24.4 or newer (see `deploy/compose/README.md`). First boot installs a pinned release.

## Secrets

`make secrets` generates these once and stores them as SecureString parameters under `/buzz/`:

- `BUZZ_RELAY_PRIVATE_KEY` (64 hex)
- `BUZZ_GIT_HOOK_HMAC_SECRET` (64 hex)
- `POSTGRES_PASSWORD`, `REDIS_PASSWORD`
- `BUZZ_S3_ACCESS_KEY`, `BUZZ_S3_SECRET_KEY`

If a parameter exists, the script leaves it alone. Rotating a secret is a manual, deliberate act because the relay key and database password must stay stable across restarts.

At deploy time, the server renders `deploy/compose/.env` from the `.env.example` template, these parameters, and non-secret values (`BUZZ_DOMAIN`, `RELAY_URL=wss://<domain>`, `RELAY_OWNER_PUBKEY`, `BUZZ_IMAGE`). It refuses to start if any `CHANGE_ME` value remains, matching `run.sh`.

## Make targets

| Target | Behavior |
|---|---|
| `make help` | Lists targets. |
| `make init` | Checks the tool versions. Runs `terraform init` with the state bucket. |
| `make plan` | Runs `terraform plan`. |
| `make apply` | Runs `terraform apply` after showing the plan. |
| `make destroy` | Runs `terraform destroy`. Requires typing the domain name to confirm. |
| `make secrets` | Runs `scripts/aws/secrets.sh`. |
| `make build` | `docker buildx build` for `linux/arm64` from the root `Dockerfile`, tagged `<ecr-repo>:<short-sha>`. |
| `make push` | Logs in to ECR and pushes the tag. |
| `make deploy` | Runs `build`, `push`, then `scripts/aws/deploy.sh <tag>`. |
| `make rollback TAG=<sha>` | Runs `deploy.sh` with an earlier tag. Skips build and push. |
| `make status` | Runs `run.sh status` on the server over SSM. |
| `make logs` | Runs `run.sh logs` on the server over SSM. |
| `make shell` | Opens an interactive SSM session. |
| `make backup` | Runs `scripts/aws/backup.sh`. |
| `make sync-upstream` | Runs `scripts/aws/sync-upstream.sh`. |

Every target reads `.aws.env` and fails with a clear message if a required value is missing.

## Deploy flow

1. `make deploy` checks that the working tree is clean. It fails otherwise, so every image maps to a real commit.
2. `make backup` runs first, unless this is the first deploy and no database exists yet.
3. `docker buildx build --platform linux/arm64` builds the root `Dockerfile`. The Dockerfile is platform-agnostic, and upstream CI already builds `linux/arm64` natively.
4. The image is pushed to ECR as `<short-sha>`.
5. `deploy.sh` packs `deploy/compose/` (compose files, `Caddyfile`, `run.sh`, `.env.example`) into a tarball and uploads it to the backup bucket under `releases/<short-sha>.tgz`.
6. `deploy.sh` calls `aws ssm send-command` on the server with a script that:
   1. Downloads and unpacks that tarball.
   2. Logs in to ECR with the instance role.
   3. Re-renders `.env` with `BUZZ_IMAGE=<ecr-repo>:<short-sha>`.
   4. Runs `BUZZ_COMPOSE_TLS=true ./run.sh upgrade`.
7. `deploy.sh` polls the command result, then calls `https://<domain>/_liveness`. It exits non-zero if either fails.
8. SSM parameter `/buzz/last_good_image` records the last tag that passed the health check. `deploy.sh` updates it only after a pass. `make rollback` uses it when `TAG` is omitted.

The relay's `BUZZ_AUTO_MIGRATE=true` setting runs database migrations on start, as in `.env.example`. A rollback to an older image cannot undo a migration. The pre-deploy backup in step 2 covers this.

The server pulls the ECR image, so the Compose file's `image:` default (`ghcr.io/block/buzz:main`) is overridden by `BUZZ_IMAGE`. The Compose files themselves are not modified.

## Upstream sync

`scripts/aws/sync-upstream.sh`:

1. Fails if the working tree has uncommitted changes.
2. Adds the remote `upstream` as `https://github.com/block/buzz.git` if it does not exist. If it exists with a different URL, it fails and prints both URLs.
3. Runs `git fetch upstream`.
4. Runs `git merge --no-edit upstream/main` into the current branch.
5. If the merge conflicts, it stops and prints the conflicting files. It never aborts the merge and never resolves conflicts.
6. It never pushes. The final message shows the commit count merged and the command to push.

## Backups

`make backup` runs `pg_dump` inside the Postgres container on the server and uploads the compressed dump to the backup bucket. EBS snapshots cover the data disk (Postgres, git, media volumes). The dump gives a portable copy and runs before every deploy.

`deploy/compose/.env` secrets are recoverable from SSM Parameter Store. The owner private key is the owner's responsibility and is not stored by this project.

## Phase 2: cloud agents

### How agents attach to the relay

An agent is a separate program, the agent runner `buzz-acp`, that connects to the relay over WebSocket. `docs/remote-agents.md` (section Launchers) says any launcher that sets the right environment variables and runs `buzz-acp` is valid. The relay needs no code change. This phase adds containers that do exactly that.

### Components

- **Agent image** (`deploy/aws/Dockerfile.agents`): Debian-based `node:22-bookworm-slim`. Node 22 is the floor for the Claude adapter. It installs two adapters, which are wrappers that let Buzz talk to a coding agent:
  - `@agentclientprotocol/claude-agent-acp`. Its Claude SDK dependency ships a prebuilt `linux-arm64` Claude Code binary, so no separate Claude Code install is needed.
  - `@agentclientprotocol/codex-acp`. It depends on `@openai/codex`, so no separate Codex install is needed.
- **Buzz tools in the image:** The `sprig` binary comes from the existing `Dockerfile.sprig` build (`COPY --from=$SPRIG_IMAGE`). The image adds the same multicall links `Dockerfile.sprig` creates (`buzz-acp`, `buzz-agent`, `buzz-dev-mcp`, `rg`, `tree`, `buzz`, `git-credential-nostr`, `git-sign-nostr`) and reuses `scripts/sprig-entrypoint.sh`, which ends in `exec buzz-acp`. `Dockerfile.sprig` is not modified. `git` and `bash` are installed for agent work.
- **Compose file** (`deploy/aws/compose.agents.yml`): a separate Compose project named `buzz-agents`, so `deploy/compose/` and `run.sh` stay untouched. One service per name in `AGENTS`. The services sit on their own network, so agents cannot reach Postgres, Redis, or MinIO.
- **Relay address:** Agents connect to `wss://$BUZZ_DOMAIN`. Each service sets `extra_hosts: ["$BUZZ_DOMAIN:host-gateway"]`, which sends that name to the host's port 443, where Caddy answers with the real certificate. The relay derives the community from the `Host` header, so agents must use the public name.

### Per-agent environment

| Variable | Value |
|---|---|
| `BUZZ_PRIVATE_KEY` | Agent key from SSM `/buzz/agents/<name>/BUZZ_PRIVATE_KEY` |
| `BUZZ_RELAY_URL` | `wss://$BUZZ_DOMAIN` |
| `BUZZ_ACP_AGENT_OWNER` | `RELAY_OWNER_PUBKEY` |
| `BUZZ_ACP_AGENT_COMMAND` | `claude-agent-acp` or `codex-acp` |
| `BUZZ_ACP_MCP_COMMAND` | `buzz-dev-mcp` |
| `BUZZ_ACP_DISPLAY_NAME` | The agent name |
| `BUZZ_ACP_RESPOND_TO`, `BUZZ_ACP_RESPOND_TO_ALLOWLIST` | From `AGENT_RESPOND_TO` and `AGENT_RESPOND_TO_ALLOWLIST` |
| `ANTHROPIC_API_KEY` | Claude agent only, from SSM `/buzz/agents/ANTHROPIC_API_KEY` |
| `OPENAI_API_KEY` | Codex agent only, from SSM `/buzz/agents/OPENAI_API_KEY` |

`BUZZ_ACP_NO_PRESENCE` is never set, and `BUZZ_ACP_EXIT_AFTER_INACTIVITY` is left at its default of 0 (always on).

### Run rules (from `docs/remote-agents.md`, section L1)

- The entrypoint runs `exec buzz-acp`, so shutdown signals reach the runner.
- `stop_grace_period: 60s`, so the runner can finish and go offline cleanly.
- `restart: on-failure`. Never `always`, because a clean exit means the owner sent `!shutdown`, and it must stay stopped.
- A named volume holds `/home/agent`, the agents' workspace. Containers run as a non-root user. The Docker socket is not mounted.

### Identity and secrets

- `make agents-secrets` reads `ANTHROPIC_API_KEY` and `OPENAI_API_KEY` from the shell environment (never from `.aws.env`) and stores them as SecureString parameters. It skips any that exist.
- `make agents-register` creates one key per agent name, stores the key in SSM, and runs `buzz-admin add-member --pubkey <pubkey>` inside the relay container so the closed relay admits the agent. It skips agents whose key already exists. The key is generated by `buzz-admin generate-key` inside the relay container.

### Deploy flow additions

- `make build` also builds the sprig image from `Dockerfile.sprig`, then builds `Dockerfile.agents` on top of it. Both are `linux/arm64`. The agents image is tagged `<short-sha>` in the `agents` ECR repository.
- `remote-deploy.sh` renders `agents.env` from SSM, then runs `docker compose -p buzz-agents -f compose.agents.yml up -d`, only after the relay reports healthy.
- Deploys with `AGENTS` empty skip all of this. A rollback restores both image tags.

### Phase 2 make targets

| Target | Behavior |
|---|---|
| `make agents-secrets` | Stores the two API keys in SSM. |
| `make agents-register` | Creates and registers one identity per agent. |
| `make agents-status` | Shows the `buzz-agents` containers over SSM. |
| `make agents-logs AGENT=<name>` | Follows one agent's logs over SSM. |

### Trade-offs the owner accepts

- Each agent's private key sits in SSM and in its container environment. Anyone who can read SSM parameters in this account, or run commands on the server, can read it.
- Agents run shell commands and write code with network access. Set a monthly spend limit on both API keys.
- Workspaces live on the server's data disk and are covered by the daily snapshots.
- Claude Code through a subscription login is not supported. The docs say nothing on headless subscription login, and Codex needs an API key.

## Error handling

- Scripts use `set -euo pipefail`.
- Missing tools (`terraform`, `aws`, `docker`, `session-manager-plugin`) produce a message naming the tool.
- `deploy.sh` prints the remote command output when a step fails.
- `destroy` never deletes the backup bucket contents silently: the bucket has `force_destroy = false`.

## Testing

- `terraform fmt -check` and `terraform validate` run in `make plan`.
- `shellcheck` on every script under `scripts/aws/`.
- `sync-upstream.sh` is tested against two throwaway local git repositories: clean merge, conflict, dirty tree, and wrong remote URL.
- Phase 2 image smoke test, run locally: build the agents image and check that `buzz-acp`, `claude-agent-acp`, and `codex-acp` are on `PATH` and the process runs as a non-root user.
- Phase 2 Compose test: `docker compose -f deploy/aws/compose.agents.yml config` renders for `claude,codex`, `claude` alone, and `codex` alone.
- Phase 2 local connection check against a local relay, with a throwaway key: the agent container connects and publishes presence. No real API key is used.
- A fresh `make apply`, `make secrets`, `make deploy`, and `curl https://<domain>/_liveness` against the real account is the end-to-end check. For phase 2, the owner then mentions an agent in a channel and gets a reply. The author runs it. Nothing in this project is run against production by an agent without the owner's go-ahead.

## Risks and open items

- Compose uses MinIO-compatible local storage on the data disk. A disk loss without a snapshot loses media and git objects. Daily snapshots limit this to one day.
- A single server means downtime during deploys (a few seconds while the relay container restarts) and during instance failure.
- The state bucket's region and any bucket-level access policy are assumed to allow the chosen AWS profile. `make init` surfaces a failure here immediately.
- The Route53 hosted zone must already exist in the same account.
- The `sprig` binary comes from a musl (Alpine) build and runs in a Debian image. It must be statically linked for this to work. The image smoke test runs it to confirm.
- `buzz-admin generate-key` prints hex keys, while the harness docs show `nsec1...` for `BUZZ_PRIVATE_KEY`. The implementer confirms which format the harness accepts and stores that format.
- The adapter command for Codex is assumed to be `codex-acp`, from the package's `bin` entry. The implementer confirms it against `crates/buzz-acp/README.md`.
