# Running Buzz on AWS

This runbook covers one Buzz relay on one AWS server, managed from your laptop with `make`. The design is in `docs/superpowers/specs/2026-10-16-aws-deployment-design.md`.

The first full run (`make apply`, `make secrets`, `make deploy`) against a real AWS account is done by the owner. The scripts were tested against stand-ins for AWS only.

## Before you start

You need these on your laptop:

- The AWS CLI, with a profile that can create the resources in `infra/`.
- Terraform 1.10 or newer.
- Docker with `buildx`, to build the `linux/arm64` image.
- `openssl` and `curl`.
- `session-manager-plugin`, only for `make shell`.

You also need two things in AWS that no `make` target creates:

- A public Route 53 hosted zone that contains your domain.
- An S3 bucket for the Terraform state. Create it by hand and turn on versioning.

## Setup order

1. Copy `.aws.env.example` to `.aws.env` (git ignores it) and fill in the five required settings:
   - `AWS_PROFILE`: the AWS CLI profile to use.
   - `TF_STATE_BUCKET`: the state bucket from above.
   - `ROUTE53_ZONE`: the hosted zone name, such as `example.com`.
   - `BUZZ_DOMAIN`: the relay host name, such as `buzz.example.com`. No scheme, no slash.
   - `RELAY_OWNER_PUBKEY`: the owner's public key as 64 lowercase hex characters, not an `npub`.

   `AWS_REGION` (default `us-east-1`) and `INSTANCE_TYPE` (default `t4g.large`) are optional.
2. `make init` checks the Terraform version and connects to the state bucket.
3. `make plan` shows what Terraform will create. This step changes nothing.
4. `make apply` shows the plan again and waits. Type `yes` to create the server, data disk, network, IAM role, image repository, backup bucket, and DNS record.
5. `make secrets` creates the six relay secrets in SSM Parameter Store under `/buzz/`. It skips any that already exist, so running it again is safe.
6. `make deploy` backs up, builds the image, pushes it, and starts the relay on the server. The working tree must be clean. `DRY_RUN=1 make deploy` runs the checks and the backup, then prints the server command without sending it.

On the first `make deploy` the backup prints `no postgres container` and continues, because no database exists yet. `AGENTS` defaults to `claude,codex`, so the first `make deploy` also ends with an error that says the relay is up and the agents need secrets and registration. Either follow the steps under Agents, or set `AGENTS=` in `.aws.env` to run without agents. If the deploy fails right after `make apply` because the server has not registered with SSM yet, wait a few minutes and run it again.

## First-deploy check

`make deploy` already calls the health URL until it answers. To check by hand:

```
curl https://<your domain>/_liveness
```

A healthy relay answers with a success status. The first HTTPS request may take a minute while the server gets its certificate.

## Everyday use

| Command | What it does |
|---|---|
| `make deploy` | Back up, build, push, and deploy the current commit. |
| `make status` | Show the Compose services on the server. |
| `make logs` | Show the last 100 relay log lines. `make logs SERVICE=caddy` shows another service. |
| `make shell` | Open an interactive session on the server. Needs `session-manager-plugin`. |
| `make backup` | Dump the database to S3 now. |
| `make rollback` | Redeploy an earlier image. See below. |
| `make help` | List every target. |

`make logs` ends after about 10 seconds, so it cannot hang. SSM caps the output it returns at about 24000 characters, so a long tail is cut. For a live follow, use `make shell` and run `cd /opt/buzz/deploy/compose && BUZZ_COMPOSE_TLS=true ./run.sh logs`.

## Rollback

`make rollback TAG=<tag>` redeploys the release with that tag. Tags are short git SHAs. List the ones you can return to:

```
aws s3 ls s3://<backup bucket>/releases/
```

`make rollback` with no `TAG` uses the value in the SSM parameter `/buzz/last_good_image`. After a healthy deploy, that value is the tag now running. So a bare `make rollback` redeploys what is already running. To leave a bad but healthy release, name the older tag: `make rollback TAG=<older tag>`.

A rollback skips the clean-tree check, the build, and the push. It checks that `releases/<tag>.tgz` exists in the backup bucket before it sends anything. It runs a backup first. If that backup fails, the rollback prints a warning and goes on, because a broken backup should not block you from leaving a bad release. The relay runs database migrations on start and a rollback cannot undo them. If the older image cannot read the migrated database, restore a dump (see Restore).

## Backups

- **Before every deploy.** `make deploy` runs the backup first. If the backup fails, the deploy stops before it changes anything.
- **On demand.** `make backup` runs `pg_dump` inside the Postgres container on the server, compresses it, and uploads it to `s3://<backup bucket>/db/<UTC time>.sql.gz`, for example `db/20261016T031500Z.sql.gz`. A partial or empty dump is never uploaded.
- **Retention.** The bucket deletes every object after 30 days. That covers the release files under `releases/` too, so a rollback can only reach tags deployed in the last 30 days.
- **Staging space.** The dump is written to `/var/tmp` on the server's root disk before it is compressed and uploaded. A very large database can fill that disk.
- **Disk snapshots.** AWS takes a daily snapshot of the data disk (Postgres, git, and media volumes) and keeps 7.
- **Secrets.** The relay secrets live in SSM Parameter Store. The owner's private key is not stored by this project. Keep it safe yourself.

Find the bucket name with `terraform -chdir=infra output -raw backup_bucket`.

## Restore

Restoring replaces the whole database with the dump. Run this from `make shell`. There is no restore script, by design.

1. Pick a dump: `aws s3 ls s3://<backup bucket>/db/`.
2. On the server, set up the shell and download it:

   ```
   sudo -i
   cd /opt/buzz/deploy/compose
   export AWS_DEFAULT_REGION=<your region>
   C="docker compose --env-file .env -f compose.yml -f compose.caddy.yml"
   aws s3 cp s3://<backup bucket>/db/<file>.sql.gz /var/tmp/restore.sql.gz
   ```
3. Stop the relay so nothing writes during the restore:

   ```
   $C stop relay
   ```
4. Drop and recreate the database, then load the dump. `ON_ERROR_STOP` makes the load stop at the first error:

   ```
   $C exec -T postgres sh -c 'psql -U "$POSTGRES_USER" -d postgres -v ON_ERROR_STOP=1 -c "DROP DATABASE IF EXISTS \"$POSTGRES_DB\" WITH (FORCE)" -c "CREATE DATABASE \"$POSTGRES_DB\""'
   gunzip -c /var/tmp/restore.sql.gz | $C exec -T postgres sh -c 'psql -U "$POSTGRES_USER" -d "$POSTGRES_DB" -v ON_ERROR_STOP=1 -q'
   ```
5. Start the relay and clean up:

   ```
   $C start relay
   rm /var/tmp/restore.sql.gz
   ```
6. Check `curl https://<your domain>/_liveness` from your laptop.

A dump holds the database only. Media and git data live on the data disk, which the daily snapshots cover.

## Syncing with upstream Buzz

`make sync-upstream` fetches `block/buzz` and merges `upstream/main` into your current branch. It stops on conflicts and never resolves them. It never pushes. After a clean merge, run `git push` yourself, then `make deploy`.

## Changes to the first-boot script

The server runs `infra/user-data.sh.tftpl` once, on its first boot. Terraform ignores later edits to it, so an edit does not reach the running server. To apply one, either open `make shell` and run the new script by hand (it is safe to run again), or replace the server on purpose with `terraform -chdir=infra apply -replace=aws_instance.server`, passing the same `-var` options that `scripts/aws/tf.sh` uses. Replacing the server needs the owner's go-ahead, and the new server needs a `make deploy`.

## Tearing down

`make destroy` asks you to type your `BUZZ_DOMAIN`. The backup bucket is created with `force_destroy = false`, so Terraform refuses to delete it while it holds objects.

## Agents

Cloud agents are AI assistants that run on the same server as the relay. Each one is its own container with its own identity. You talk to an agent by mentioning it in a channel.

By default `AGENTS=claude,codex` runs one Claude agent and one Codex agent. Set `AGENTS=` (empty) to run none.

### Setup order

1. `make apply`, then `make deploy` once so the relay is up. Agents need a running relay to register, so this first deploy has no agent keys or credentials. It brings the relay up, then stops with an error that says the relay is up and to run `make agents-secrets` and `make agents-register`. That error is expected. Do not roll back.
2. Export the credentials in your shell. They never go in `.aws.env`. For example, `set -a; . ./.env; set +a` loads them from a `.env` file.
   - Claude agents: `CLAUDE_CODE_OAUTH_TOKEN`, or `ANTHROPIC_API_KEY` if no token is set.
   - Codex agents: `OPENAI_API_KEY`.
3. `make agents-secrets` stores them in SSM under `/buzz/agents/`. It skips any that already exist.
4. `make agents-register` creates one identity per agent and adds it to the relay. It is safe to run again.
5. `make deploy` again. After the relay is healthy, it renders each agent's environment on the server and starts the agent containers. A failure in this agent phase leaves the relay running, fails the deploy, and records no last good image.

### Defining several agents

`AGENTS` is a comma-separated list. An entry is `claude`, `codex`, or `<name>:<runtime>`. Names must be unique.

```
AGENTS=reviewer:claude,docs:claude,codex
```

Each agent gets its own container, home volume, and Compose project (`buzz-agent-<name>`). Run `make agents-register` and `make deploy` after you change the list. An agent you remove is stopped by the next deploy. If the list becomes empty, the deploy also deletes `/opt/buzz/agents` on the server.

`AGENT_RESPOND_TO` (`owner-only` by default, or `allowlist`, `anyone`, `nobody`) sets who an agent answers. `allowlist` also needs `AGENT_RESPOND_TO_ALLOWLIST`.

### Per-agent tools

Put an agent's MCP servers (tools it can call) in `deploy/aws/agents/<name>/mcp.json`. The file is committed, so it holds no secrets. Details and limits are in `deploy/aws/agents/README.md`.

### Using an agent

Add the agent to a channel and mention it, such as `@reviewer please look at this thread`. `!shutdown` stops an agent for good, and the container does not restart by itself. The next `make deploy` starts it again.

### Spend limits

Agents bill your own Anthropic and OpenAI accounts, and nothing in this project caps that. Set a monthly spend limit in each provider's console before you start an agent.

### Checking agents

| Command | What it does |
|---|---|
| `make agents-status` | List the agent containers on the server and their state. |
| `make agents-logs AGENT=<name>` | Show that agent's recent logs for about 10 seconds. `<name>` must be in `AGENTS`. |

### Rollback and agents

A rollback restores the relay and the agent image to the same tag. Both images are built and pushed under one tag, and `/buzz/last_good_image` holds it. Before it sends anything, the rollback checks that the agents image with that tag exists in ECR. If it does not, it stops with `no agents image tagged <tag>`. That happens for a release that was deployed with `AGENTS` empty. To roll back the relay alone, set `AGENTS=` in `.aws.env` for that run. That stops all agents. Any other ECR error, such as a missing permission or an expired login, is shown as the AWS error text. A release built before agents existed also has no agent files, so a deploy with `AGENTS` set fails early with `release has no deploy/aws agent files`.

### Troubleshooting agents

**An agent starts and exits.** Run `make agents-logs AGENT=<name>`. A missing credential, a bad `mcp.json`, or a relay that rejects the agent shows up there. If the cause is the identity, run `make agents-register` again.

**Building the agent image on your laptop.** The agent image is built on top of the relay's agent runner image (`buzz-sprig:<tag>`). The build needs Docker's default builder, either the `docker` driver or the containerd image store, so the first image is visible to the second. A `docker-container` builder cannot see it.

**Checking an agent against a local relay.** `just setup` and `just relay` use ports 5432, 6379, and 3000, so they fail when other containers already hold those ports. This runs an isolated relay and one agent instead, with throwaway keys. It never touches the AWS account.

```
# 1. Relay stack. Copy deploy/compose/compose.yml and write a .env beside it with
#    throwaway secrets, BUZZ_DOMAIN=buzz-t9.test, BUZZ_HTTP_PORT=53000,
#    BUZZ_REQUIRE_RELAY_MEMBERSHIP=true, BUZZ_GIT_CONFORMANCE_PROBE=false.
#    Always pass -p: compose.yml is named buzz-prod, the same as a real deployment.
docker compose -p buzz-t9 --env-file .env up -d

# 2. Make an identity and add it as a member. No -T or TTY is needed.
docker compose -p buzz-t9 exec relay /usr/local/bin/buzz-admin generate-key < /dev/null
docker compose -p buzz-t9 exec relay /usr/local/bin/buzz-admin add-member --pubkey <public key> < /dev/null

# 3. Build the images (a cold sprig build takes several minutes).
docker build -f Dockerfile.sprig -t buzz-t9-sprig:local .
docker build -f deploy/aws/Dockerfile.agents --build-arg SPRIG_IMAGE=buzz-t9-sprig:local -t buzz-t9-agent:local .

# 4. Render the agent files with the test stand-in for aws. Put one line per
#    parameter, "<name><TAB><value>", in $STUB_DIR/ssm-get-parameters-by-path.out:
#    /buzz/agents/t9/BUZZ_PRIVATE_KEY and /buzz/agents/ANTHROPIC_API_KEY (any dummy text).
STUB_DIR=<dir> STUB_LOG=<file> PATH="$PWD/tests/aws/stubs:$PATH" \
  BUZZ_DOMAIN=buzz-t9.test RELAY_OWNER_PUBKEY=<64 hex> AGENTS=t9:claude \
  bash scripts/aws/render-agents-env.sh <outdir> buzz-t9-agent:local deploy/aws/agents

# 5. Point the agent at the local relay, then start it.
sed 's#^BUZZ_RELAY_URL=.*#BUZZ_RELAY_URL=ws://buzz-t9.test:53000#' <outdir>/t9.env > <outdir>/t9.env.new && mv <outdir>/t9.env.new <outdir>/t9.env
docker compose -p buzz-agent-t9 -f deploy/aws/compose.agents.yml --env-file <outdir>/t9.compose.env up -d
docker logs buzz-agent-t9-agent-1

# 6. Clean up.
docker compose -p buzz-agent-t9 -f deploy/aws/compose.agents.yml --env-file <outdir>/t9.compose.env down -v
docker compose -p buzz-t9 --env-file .env down -v
```

A working agent logs `connected to relay`, and the relay logs `NIP-42 auth successful` for the agent's key. Inside the container, `/etc/buzz-agent/mcp.json` and `~/.mcp.json` both read `{"mcpServers": {}}` for an agent with no config. The staged config is readable even though the render directory is mode 0700, because the container sees only the mounted `config` directory. On Docker Compose v5, `exec` without `-T` works with no terminal, so `run.sh add-member` runs fine over SSM.

## Running the script tests

`make test-aws` runs the tests for every script. They use stand-ins for `aws`, `docker`, `terraform`, and `curl`, so they never touch a real account.
