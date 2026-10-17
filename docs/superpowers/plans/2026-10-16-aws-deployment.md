# AWS Deployment Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Deploy the SellerCanopy fork of Buzz to one EC2 server with Terraform, a Makefile, and shell scripts. Add a command that merges `block/buzz` updates into the fork. Run always-on Claude Code and Codex agents in containers on the same server.

**Architecture:** Terraform (`infra/`) builds the AWS resources. Shell scripts (`scripts/aws/`) handle secrets, image rollout over SSM, backups, upstream sync, and agent identities. The root `Makefile` is the only entry point. The server runs the existing `deploy/compose/` stack unchanged, with `BUZZ_IMAGE` pointed at the fork's ECR image. Agents run as a second Compose project (`buzz-agents`, files in `deploy/aws/`) that connects to the relay at its public `wss://` address.

**Tech Stack:** Terraform 1.10+ (AWS provider), Bash, AWS CLI, Docker buildx, Docker Compose 2.24.4+, shellcheck, Node 22 (agent image only).

**Spec:** `docs/superpowers/specs/2026-10-16-aws-deployment-design.md`

## Global Constraints

- Terraform 1.10 or newer. S3 backend with `use_lockfile = true`, state key `buzz/terraform.tfstate`, bucket from `TF_STATE_BUCKET`. No DynamoDB table.
- Region default `us-east-1`. Instance default `t4g.large`, Amazon Linux 2023, IMDSv2 required.
- Security group: inbound 80 and 443 only. No inbound 22.
- EBS data volume with daily snapshots through Data Lifecycle Manager, 7 kept.
- Two ECR repositories, `relay` and `agents`, each keeping the 10 newest images. Backup bucket: private, versioned, 30-day expiry, `force_destroy = false`.
- Images build for `linux/arm64` from the root `Dockerfile` and are tagged with the short git SHA. Never `:main`.
- Secrets live in SSM Parameter Store as SecureString under `/buzz/`: `BUZZ_RELAY_PRIVATE_KEY`, `BUZZ_GIT_HOOK_HMAC_SECRET`, `POSTGRES_PASSWORD`, `REDIS_PASSWORD`, `BUZZ_S3_ACCESS_KEY`, `BUZZ_S3_SECRET_KEY`. Never overwrite an existing one.
- Last passing image tag is recorded in SSM parameter `/buzz/last_good_image`.
- Compose 2.24.4 or newer on the server. Do not modify any file under `deploy/compose/`.
- Every script starts with `#!/usr/bin/env bash` and `set -euo pipefail`, and passes `shellcheck`.
- Repo rules: commit with `git commit -s` after `. ./bin/activate-hermit`. No em-dashes, no emojis, and no AI attribution lines in commits, code, or docs.
- `.aws.env` is git-ignored. Required settings: `AWS_PROFILE`, `TF_STATE_BUCKET`, `ROUTE53_ZONE`, `BUZZ_DOMAIN`, `RELAY_OWNER_PUBKEY`. Optional: `AWS_REGION`, `INSTANCE_TYPE`, `AGENTS` (default `claude,codex`, valid names `claude` and `codex`, empty disables agents), `AGENT_RESPOND_TO` (default `owner-only`; also `allowlist`, `anyone`, `nobody`), `AGENT_RESPOND_TO_ALLOWLIST`.
- Agents: separate Compose project `buzz-agents`, own network, no Docker socket, non-root user, `restart: on-failure` (never `always`), `stop_grace_period: 60s`, entrypoint ends in `exec buzz-acp`. Never set `BUZZ_ACP_NO_PRESENCE`. Leave `BUZZ_ACP_EXIT_AFTER_INACTIVITY` unset.
- Agents connect to `wss://$BUZZ_DOMAIN` with `extra_hosts: ["$BUZZ_DOMAIN:host-gateway"]`. They use API keys (`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`) read from the shell environment, never from `.aws.env`. SSM paths: `/buzz/agents/<name>/BUZZ_PRIVATE_KEY`, `/buzz/agents/ANTHROPIC_API_KEY`, `/buzz/agents/OPENAI_API_KEY`.
- Do not modify `Dockerfile.sprig`, `scripts/sprig-entrypoint.sh`, or anything under `deploy/compose/`.

## Review Focus

- `RELAY_OWNER_PUBKEY` given as an `npub1...` string instead of 64 hex characters: reject with a message naming the expected format. (Task 1)
- `BUZZ_DOMAIN` pasted as a URL (`https://buzz.example.com/`): reject, since Caddy and `RELAY_URL` need a bare host name. (Task 1)
- `sync-upstream` when already up to date, or on a detached HEAD: report and exit as specified, never create an empty merge commit. (Task 2)
- `make rollback` with no `TAG` and no recorded `/buzz/last_good_image`: fail with a clear message instead of deploying an empty tag. (Task 5)
- First deploy, when no Postgres container exists yet: skip the pre-deploy backup instead of failing. (Task 6, in `backup.sh`)
- `AGENTS=claude` only: no `codex` service, key, or API key requirement appears anywhere. (Tasks 7 and 8)
- `AGENT_RESPOND_TO=allowlist` with no allowlist, or with a non-hex entry: reject before anything is sent to AWS. (Task 1)

---

## File Structure

| Path | Responsibility |
|---|---|
| `Makefile` | Entry point. Thin: each target calls a script or Terraform. |
| `.aws.env.example` | Template for local settings. |
| `scripts/aws/lib.sh` | Shared shell functions: settings load and validation, tool checks, Terraform outputs. |
| `scripts/aws/sync-upstream.sh` | Merge `upstream/main` into the current branch. |
| `scripts/aws/secrets.sh` | Create missing secrets in SSM. |
| `scripts/aws/render-env.sh` | Runs on the server. Builds `.env` from the template and SSM. |
| `scripts/aws/remote-deploy.sh` | Runs on the server. Unpacks a release, logs in to ECR, upgrades Compose. |
| `scripts/aws/deploy.sh` | Runs on the laptop. Uploads a release, sends the SSM command, checks health. |
| `scripts/aws/backup.sh` | Dumps Postgres to the backup bucket. |
| `infra/*.tf`, `infra/user-data.sh.tftpl` | All AWS resources. |
| `tests/aws/run.sh`, `tests/aws/test-*.sh`, `tests/aws/stubs/aws` | Bash tests and an `aws` command stub. |
| `docs/aws-deployment.md` | Operator runbook. |
| `deploy/aws/Dockerfile.agents` | Node 22 image: Buzz tools from the sprig image, plus Claude and Codex adapters. |
| `deploy/aws/compose.agents.yml` | One service per agent, selected with Compose profiles. |
| `scripts/aws/agents-secrets.sh`, `agents-register.sh`, `render-agents-env.sh` | Agent API keys, identities, and server-side env files. |

---

### Task 1: Makefile skeleton, settings, and validation

**Files:**
- Create: `Makefile`, `.aws.env.example`, `scripts/aws/lib.sh`, `tests/aws/run.sh`, `tests/aws/test-lib.sh`
- Modify: `.gitignore`

**Interfaces:**
- Produces in `lib.sh`:
  - `load_settings` : sources `.aws.env` from the repo root (it derives `REPO_ROOT` itself when unset; an `AGENTS` that is unset takes the default, while `AGENTS=` set to empty stays empty and disables agents, so use `${AGENTS-claude,codex}`), applies defaults (`AWS_REGION=us-east-1`, `INSTANCE_TYPE=t4g.large`, `AGENTS=claude,codex`, `AGENT_RESPOND_TO=owner-only`), validates, exports all settings. Exits 1 with a message on any invalid value.
  - `require_tool <name>` : exits 1 with `missing tool: <name>` if not on PATH.
  - `tf_output <name>` : prints `terraform -chdir=infra output -raw <name>`.
- Produces: `tests/aws/run.sh` runs every `tests/aws/test-*.sh` and exits non-zero if any fails. Each test file exits non-zero on failure and uses `$REPO_ROOT`.
- Produces Makefile target `help` and variable `SCRIPTS := scripts/aws`.

- [ ] **Step 1: Write failing tests in `tests/aws/test-lib.sh`.** Each case writes a temp `.aws.env`, runs `load_settings` in a subshell with `AWS_ENV_FILE` pointing at it, and asserts exit code and message:
  - missing `BUZZ_DOMAIN` fails and names it.
  - `BUZZ_DOMAIN=https://buzz.example.com/` fails with `bare host name`.
  - `RELAY_OWNER_PUBKEY=npub1abc...` fails with `64 hex`.
  - `AGENTS=claude,gemini` fails and names `gemini`.
  - `AGENT_RESPOND_TO=allowlist` with an empty or non-hex `AGENT_RESPOND_TO_ALLOWLIST` fails.
  - a valid file succeeds and exports `AWS_REGION=us-east-1`, `INSTANCE_TYPE=t4g.large`, and `AGENTS=claude,codex`.
- [ ] **Step 2:** Run `bash tests/aws/run.sh`. Expected: FAIL (`lib.sh` not found).
- [ ] **Step 3: Implement `lib.sh`** per the Interfaces. `load_settings` reads `${AWS_ENV_FILE:-$REPO_ROOT/.aws.env}`. Domain check: reject `://`, `/`, spaces, trailing `.`. Pubkey check: `^[0-9a-f]{64}$`.
- [ ] **Step 4:** Create `tests/aws/run.sh`, `.aws.env.example` (every setting, commented, with example values), `Makefile` (`.DEFAULT_GOAL := help`, `help` prints targets from `## ` comments), and add `.aws.env`, `infra/.terraform/`, `infra/*.tfstate*` to `.gitignore`.
- [ ] **Step 5:** Run `bash tests/aws/run.sh` and `make help`. Expected: tests PASS, help lists targets.
- [ ] **Step 6: Commit.** `git add -A && git commit -s -m "feat(aws): add Makefile skeleton and settings validation"`

### Task 2: `sync-upstream`

**Files:**
- Create: `scripts/aws/sync-upstream.sh`, `tests/aws/test-sync-upstream.sh`
- Modify: `Makefile` (target `sync-upstream`)

**Interfaces:**
- Consumes: `lib.sh` is not needed. The script is standalone.
- Produces: `sync-upstream.sh` reads env `UPSTREAM_URL` (default `https://github.com/block/buzz.git`) so tests can point at a local repo. Exit 0 on merged or already up to date. Exit 1 on dirty tree, remote URL mismatch, detached HEAD, or conflict.

- [ ] **Step 1: Write failing tests.** Build two temp repos (`upstream` bare, `fork` clone) and assert:
  - dirty tree: exits 1, message contains `uncommitted`.
  - existing `upstream` remote with a different URL: exits 1, prints both URLs.
  - clean merge: new upstream commit appears in the fork, exit 0, no push happened.
  - already up to date: exit 0, message contains `already up to date`, `git rev-list --count` unchanged.
  - detached HEAD: exits 1, message contains `detached`.
  - conflict: exits 1, lists the conflicting file, and `.git/MERGE_HEAD` still exists (merge left in place).
- [ ] **Step 2:** Run `bash tests/aws/test-sync-upstream.sh`. Expected: FAIL.
- [ ] **Step 3: Implement `sync-upstream.sh`** following the spec's "Upstream sync" steps 1 to 6. Use `git merge --no-edit upstream/main`. Final message prints the merged commit count and `git push origin <branch>` as text only.
- [ ] **Step 4:** Add `make sync-upstream` calling the script. Run the test file again. Expected: PASS. Run `shellcheck scripts/aws/sync-upstream.sh`. Expected: no output.
- [ ] **Step 5: Commit.** `git add -A && git commit -s -m "feat(aws): add upstream sync command"`

### Task 3: Terraform network, IAM, storage, DNS

**Files:**
- Create: `infra/versions.tf`, `infra/backend.tf`, `infra/variables.tf`, `infra/network.tf`, `infra/iam.tf`, `infra/storage.tf`, `infra/outputs.tf`
- Modify: `Makefile` (targets `init`, `plan`, `apply`, `destroy`)

**Interfaces:**
- Produces variables: `aws_region`, `instance_type`, `domain`, `route53_zone`.
- Produces resources other files rely on: `aws_security_group.relay`, `aws_iam_instance_profile.server`, `aws_ecr_repository.relay`, `aws_ecr_repository.agents`, `aws_s3_bucket.backups`.
- Produces outputs: `ecr_repository_url`, `agents_ecr_repository_url`, `backup_bucket`. (`instance_id`, `elastic_ip`, and the Route53 record belong to Task 4, because they reference the server and Elastic IP defined there. Task 3 must validate on its own.)
- Makefile: `init` runs `require_tool terraform`, checks version is 1.10 or newer, then `terraform -chdir=infra init -backend-config="bucket=$TF_STATE_BUCKET" -backend-config="key=buzz/terraform.tfstate" -backend-config="region=$AWS_REGION" -backend-config="use_lockfile=true"`. `destroy` requires typing `BUZZ_DOMAIN` to confirm.

- [ ] **Step 1: Write `versions.tf` and `backend.tf`** (empty `backend "s3" {}` block, configured at init). Pin `required_version = ">= 1.10"`.
- [ ] **Step 2: Write the resources** listed in the spec under "What Terraform builds": VPC with one public subnet, internet gateway, route table, security group (80, 443 inbound only), IAM role and instance profile (SSM core, ECR read, `ssm:GetParameter*` on `/buzz/*`, `ssm:PutParameter` on `/buzz/agents/*` so the server can store agent keys, S3 read/write on the backup bucket), two ECR repositories (`relay`, `agents`) each with a keep-10 lifecycle policy, backup bucket (private, versioned, 30-day expiry, `force_destroy = false`).
- [ ] **Step 3: Write the Makefile targets.** `plan` runs `terraform fmt -check`, `validate`, then `plan` with `-var` flags built from the settings.
- [ ] **Step 4: Verify offline.** Run `terraform -chdir=infra init -backend=false && terraform -chdir=infra fmt -check && terraform -chdir=infra validate`. Expected: `Success! The configuration is valid.`
- [ ] **Step 5: Verify the guard.** Run `make destroy` with wrong input at the prompt. Expected: aborts without calling Terraform.
- [ ] **Step 6: Commit.** `git add -A && git commit -s -m "feat(aws): add network, IAM, storage, and DNS Terraform"`

### Task 4: Terraform server and first-boot script

**Files:**
- Create: `infra/compute.tf`, `infra/dns.tf`, `infra/user-data.sh.tftpl`
- Modify: `infra/outputs.tf`

**Interfaces:**
- Consumes from Task 3: `aws_security_group.relay`, `aws_iam_instance_profile.server`, the subnet, variables `instance_type`, `domain`, `route53_zone`.
- Produces: `aws_instance.server`, `aws_eip.relay`, `aws_ebs_volume.data`, `aws_dlm_lifecycle_policy.daily`, and in `dns.tf` the Route53 `A` record for `var.domain` pointing at `aws_eip.relay.public_ip`. New outputs: `instance_id` is `aws_instance.server.id`; `elastic_ip` is `aws_eip.relay.public_ip`.

- [ ] **Step 1: Write `compute.tf`:** ARM Amazon Linux 2023 AMI via the SSM public parameter, `metadata_options { http_tokens = "required" }`, the instance, Elastic IP and association, data volume attachment, DLM policy (daily, retain 7).
- [ ] **Step 2: Write `user-data.sh.tftpl`:** installs Docker, installs the pinned Compose plugin release (2.24.4 or newer), formats the data volume only if it has no filesystem, mounts it at `/var/lib/docker`, enables Docker, creates `/opt/buzz`. Starts nothing.
- [ ] **Step 3: Verify.** `terraform -chdir=infra fmt -check && terraform -chdir=infra validate`. Expected: valid. Also run `shellcheck` on the rendered template with the template variables replaced by placeholders. Expected: no output.
- [ ] **Step 4: Commit.** `git add -A && git commit -s -m "feat(aws): add server, data disk, and first-boot script"`

### Task 5: Secrets, deploy, and rollback

**Files:**
- Create: `scripts/aws/secrets.sh`, `scripts/aws/render-env.sh`, `scripts/aws/remote-deploy.sh`, `scripts/aws/deploy.sh`, `scripts/aws/build.sh`, `tests/aws/stubs/aws`, `tests/aws/stubs/docker`, `tests/aws/stubs/terraform`, `tests/aws/test-secrets.sh`, `tests/aws/test-render-env.sh`, `tests/aws/test-deploy.sh`
- Modify: `Makefile` (targets `secrets`, `build`, `push`, `deploy`, `rollback`)

**Interfaces:**
- Consumes: `load_settings`, `require_tool`, `tf_output` from Task 1; Terraform outputs from Tasks 3 and 4.
- Produces:
  - `tests/aws/stubs/aws`, `tests/aws/stubs/docker`, `tests/aws/stubs/terraform`: fakes that append their arguments to `$STUB_LOG` and print the contents of `$STUB_DIR/<command>-<subcommand>.out` when present (for `terraform output -raw <name>`, the file is `$STUB_DIR/tf-<name>.out`). Tests put `tests/aws/stubs` first on `PATH`.
  - `build.sh <tag> [--push]` : builds the relay image for `linux/arm64` from the root `Dockerfile`, tagged `<ecr-repo>:<tag>`. With `--push`, logs in to ECR and pushes. Makefile `build` and `push` and `deploy.sh` all call it, so the build command lives in one place. Task 9 extends it for agents.
  - `secrets.sh` : no arguments. For each of the six names, `aws ssm get-parameter`; if absent, `aws ssm put-parameter --type SecureString` with `openssl rand -hex 32` for keys and `openssl rand -hex 24` for passwords (hex only, because these values are embedded in `DATABASE_URL` and `REDIS_URL`, where `/`, `+`, and `=` break URL parsing). Never calls put on an existing name.
  - `render-env.sh <template> <image>` : prints a complete `.env` to stdout. Takes secrets from `aws ssm get-parameters-by-path --path /buzz/ --with-decryption`, takes `BUZZ_DOMAIN` and `RELAY_OWNER_PUBKEY` from env, sets `RELAY_URL=wss://$BUZZ_DOMAIN`, `BUZZ_CORS_ORIGINS`, `BUZZ_MEDIA_*` from the domain, and `BUZZ_IMAGE=<image>`. Exits 1 if any `CHANGE_ME` remains.
  - `remote-deploy.sh <tag>` : on the server, unpacks `/opt/buzz/release.tgz`, runs the ECR login, runs `render-env.sh`, then `BUZZ_COMPOSE_TLS=true ./run.sh upgrade`.
  - `deploy.sh [tag]` : with no tag, resolves `git rev-parse --short HEAD` and runs the full flow (clean tree check, backup via `${BACKUP_CMD:-scripts/aws/backup.sh}` which Task 6 creates, `build.sh <tag> --push`, upload release, `aws ssm send-command`, poll, health check, record `/buzz/last_good_image`). The SSM command exports `BUZZ_DOMAIN` and `RELAY_OWNER_PUBKEY` (and, in Task 9, `AGENTS` and the `AGENT_RESPOND_TO*` settings) so the server-side scripts can read them. A failing backup aborts the deploy before the send-command call. With `ROLLBACK=1` and no tag, reads `/buzz/last_good_image` and exits 1 with `no recorded good image` if empty. With `DRY_RUN=1` prints the SSM command instead of sending it.

- [ ] **Step 1: Write failing tests.**
  - `test-secrets.sh`: with the stub reporting all six absent, the log shows six `put-parameter` calls. Run twice with all present: zero `put-parameter` calls.
  - `test-render-env.sh`: given a template with `CHANGE_ME` values and a stub returning the six secrets, output has no `CHANGE_ME`, contains `BUZZ_IMAGE=repo:abc1234` and `RELAY_URL=wss://buzz.example.com`. With one secret missing from the stub, exit 1.
  - `test-deploy.sh`: dirty tree exits 1 and builds nothing. `ROLLBACK=1` with empty `last_good_image` exits 1 with `no recorded good image`. With `DRY_RUN=1` and `BACKUP_CMD` set to a script that logs a line, the backup line precedes the printed SSM command. With `BACKUP_CMD` set to a failing script, deploy exits non-zero and prints no SSM command.
- [ ] **Step 2:** Run `bash tests/aws/run.sh`. Expected: FAIL for the three new files.
- [ ] **Step 3: Implement the five scripts** per the Interfaces. `deploy.sh` uses `docker buildx build --platform linux/arm64 --load` replaced by `--push` to the ECR URL from `tf_output ecr_repository_url`, and uploads `tar czf` of `deploy/compose` plus `scripts/aws/render-env.sh` and `scripts/aws/remote-deploy.sh` to `s3://<backup bucket>/releases/<tag>.tgz`.
- [ ] **Step 4: Add Makefile targets.** `build` calls `build.sh $(TAG)`. `push` calls `build.sh $(TAG) --push`. `TAG` defaults to the short git SHA. `deploy` calls `deploy.sh`. `rollback` calls `ROLLBACK=1 deploy.sh $(TAG)`.
- [ ] **Step 5:** Run `bash tests/aws/run.sh` and `shellcheck scripts/aws/*.sh`. Expected: all PASS, no shellcheck output.
- [ ] **Step 6: Commit.** `git add -A && git commit -s -m "feat(aws): add secrets, deploy, and rollback"`

### Task 6: Backup, operations targets, and runbook

**Files:**
- Create: `scripts/aws/backup.sh`, `tests/aws/test-backup.sh`, `docs/aws-deployment.md`
- Modify: `Makefile` (targets `backup`, `status`, `logs`, `shell`)

**Interfaces:**
- Consumes: `tf_output instance_id`, `tf_output backup_bucket`, the `aws` stub from Task 5.
- Produces: `backup.sh` : sends an SSM command that runs `docker compose exec -T postgres pg_dump -U buzz buzz | gzip` and uploads to `s3://<bucket>/db/<UTC timestamp>.sql.gz`. Prints `no postgres container` and exits 0 when none is running. `status`/`logs` send `./run.sh status|logs` over SSM. `shell` runs `aws ssm start-session`.

- [ ] **Step 1: Write the failing test** `test-backup.sh`: with the stub reporting a running container, the logged command contains `pg_dump` and `s3://<bucket>/db/`. With none running: output contains `no postgres container`, exit 0.
- [ ] **Step 2:** Run it. Expected: FAIL.
- [ ] **Step 3: Implement `backup.sh`** and the three Makefile targets. `require_tool session-manager-plugin` runs before `shell`.
- [ ] **Step 4: Write `docs/aws-deployment.md`:** setup order (`.aws.env`, `make init`, `make apply`, `make secrets`, `make deploy`), the first-deploy check (`curl https://<domain>/_liveness`), rollback, backup and restore, and `sync-upstream` followed by a manual `git push`. State that the first end-to-end run is done by the owner.
- [ ] **Step 5:** Run `bash tests/aws/run.sh`, `shellcheck scripts/aws/*.sh`, and `make help`. Expected: all PASS, help lists every target in the spec's table.
- [ ] **Step 6: Commit.** `git add -A && git commit -s -m "feat(aws): add backup, ops targets, and runbook"`

### Task 7: Agent image and Compose file

**Files:**
- Create: `deploy/aws/Dockerfile.agents`, `deploy/aws/compose.agents.yml`, `tests/aws/test-agents-compose.sh`, `tests/aws/test-agents-image.sh`

**Interfaces:**
- Produces `Dockerfile.agents`: build arg `SPRIG_IMAGE` (required), base `node:22-bookworm-slim`. Installs `git`, `bash`, `ca-certificates`, `curl`, then `npm install -g @agentclientprotocol/claude-agent-acp @agentclientprotocol/codex-acp`. Copies `/usr/local/bin/sprig` from `$SPRIG_IMAGE` and `scripts/sprig-entrypoint.sh` (build context is the repo root). Creates the same eight multicall links as `Dockerfile.sprig`. Runs as non-root user `agent` with `HOME=/home/agent`.
- Produces `compose.agents.yml`: project name `buzz-agents`, services `agent-claude` (profile `claude`) and `agent-codex` (profile `codex`), one network, `env_file` set to `./claude.env` and `./codex.env` for the matching service, image `${AGENTS_IMAGE}`, volume `agent-<name>-home:/home/agent`, and the run rules from Global Constraints.
- Consumes: env files named `claude.env` and `codex.env` plus a `.env` with `AGENTS_IMAGE`, `BUZZ_DOMAIN`, and `COMPOSE_PROFILES` (written by Task 8).

- [ ] **Step 1: Write failing tests.**
  - `test-agents-compose.sh`: with dummy env files, `docker compose -f deploy/aws/compose.agents.yml --profile claude config` renders `agent-claude` only. `COMPOSE_PROFILES=claude,codex` renders both. Every rendered service has `restart: on-failure`, `stop_grace_period: 1m0s`, an `extra_hosts` entry for the domain, and no `/var/run/docker.sock` volume.
  - `test-agents-image.sh` (skip with `SKIP_IMAGE=1`): build the sprig image from `Dockerfile.sprig`, then `Dockerfile.agents` with `SPRIG_IMAGE` set to it. Assert `command -v buzz-acp claude-agent-acp codex-acp` succeeds, `id -u` is not 0, and `buzz-acp --help` exits 0 (this proves the musl-built binary runs on Debian).
- [ ] **Step 2:** Run both. Expected: FAIL (files missing).
- [ ] **Step 3: Write `Dockerfile.agents` and `compose.agents.yml`** per the Interfaces. Confirm the Codex adapter command name against `crates/buzz-acp/README.md` and the package's `bin` entry, and record it in a comment-free way by using it in the compose `BUZZ_ACP_AGENT_COMMAND`.
- [ ] **Step 4:** Run both tests. Expected: PASS. The image test is slow on the first Rust build. If the binary fails on Debian, switch the copy to build from `Dockerfile.sprig`'s builder with a static target and update the test, not the assertion.
- [ ] **Step 5: Commit.** `git add -A && git commit -s -m "feat(aws): add agent image and Compose project"`

### Task 8: Agent keys, identities, and env rendering

**Files:**
- Create: `scripts/aws/agents-secrets.sh`, `scripts/aws/agents-register.sh`, `scripts/aws/render-agents-env.sh`, `tests/aws/test-agents-secrets.sh`, `tests/aws/test-agents-register.sh`, `tests/aws/test-render-agents-env.sh`
- Modify: `Makefile` (targets `agents-secrets`, `agents-register`)

**Interfaces:**
- Consumes: `load_settings`, `require_tool`, `tf_output instance_id` (Task 1), the `aws` stub (Task 5).
- Produces:
  - `agents-secrets.sh` : no arguments. Requires `ANTHROPIC_API_KEY` in the shell when `AGENTS` contains `claude`, and `OPENAI_API_KEY` when it contains `codex`. Exits 1 naming the missing variable. Puts each as a SecureString under `/buzz/agents/` only if absent.
  - `agents-register.sh` : no arguments. Sends one SSM command to the server. For each name in `AGENTS`: skip if `/buzz/agents/<name>/BUZZ_PRIVATE_KEY` exists. Otherwise run `buzz-admin generate-key` in the relay container, store the secret key in SSM, then run `./run.sh add-member <pubkey>`.
  - `render-agents-env.sh <outdir> <agents-image>` : runs on the server. Writes `<outdir>/.env` (`AGENTS_IMAGE`, `BUZZ_DOMAIN`, `COMPOSE_PROFILES` as the comma-joined `AGENTS`) and one `<outdir>/<name>.env` per agent with the variables in the spec's per-agent table. Exits 1 if any value is empty.

- [ ] **Step 1: Write failing tests.**
  - `test-agents-secrets.sh`: missing `ANTHROPIC_API_KEY` with `AGENTS=claude` exits 1 and names it. `AGENTS=claude` with only that key set never requires `OPENAI_API_KEY` and makes one `put-parameter` call. Second run makes none.
  - `test-agents-register.sh`: the logged SSM command contains `generate-key` and `add-member` for each agent without a key. With keys present in the stub, it contains neither.
  - `test-render-agents-env.sh`: `AGENTS=claude` writes `claude.env` containing `BUZZ_ACP_AGENT_COMMAND=claude-agent-acp`, `BUZZ_RELAY_URL=wss://buzz.example.com`, `ANTHROPIC_API_KEY=...`, `BUZZ_ACP_AGENT_OWNER=<owner pubkey>`, and no `codex.env`. Neither file contains `BUZZ_ACP_NO_PRESENCE` or `BUZZ_ACP_EXIT_AFTER_INACTIVITY`. An empty stub value exits 1.
- [ ] **Step 2:** Run `bash tests/aws/run.sh`. Expected: FAIL for the three new files.
- [ ] **Step 3: Confirm key format locally.** Run `buzz-admin generate-key` from a local build (`cargo run -p buzz-admin -- generate-key`) and check what `BUZZ_PRIVATE_KEY` accepts (`buzz-acp --help` and `crates/buzz-acp/src/config.rs`). Store and render the accepted format. Do not store the throwaway key.
- [ ] **Step 4: Implement the three scripts** per the Interfaces, and the two Makefile targets.
- [ ] **Step 5:** Run `bash tests/aws/run.sh` and `shellcheck scripts/aws/*.sh`. Expected: all PASS, no shellcheck output.
- [ ] **Step 6: Commit.** `git add -A && git commit -s -m "feat(aws): add agent keys, registration, and env rendering"`

### Task 9: Agent rollout, operations, and local connection check

**Files:**
- Modify: `scripts/aws/deploy.sh`, `scripts/aws/remote-deploy.sh`, `Makefile` (`build`, `push`, `agents-status`, `agents-logs`), `tests/aws/test-deploy.sh`, `docs/aws-deployment.md`

**Interfaces:**
- Consumes: Tasks 5, 7, and 8.
- Produces: with `AGENTS` non-empty, `build` first builds the sprig image from `Dockerfile.sprig`, then `Dockerfile.agents` with `SPRIG_IMAGE`, both `--platform linux/arm64`. `push` pushes the agents image as `<short-sha>` to `agents_ecr_repository_url`. `remote-deploy.sh <tag>` additionally runs `render-agents-env.sh /opt/buzz/agents <agents-image>` and `docker compose -p buzz-agents -f compose.agents.yml up -d` after `run.sh upgrade` reports healthy. `agents-logs` takes `AGENT=<name>` and fails if it is not in `AGENTS`.

- [ ] **Step 1: Write failing tests** in `test-deploy.sh`. With `DRY_RUN=1` and `AGENTS=claude,codex`, the printed SSM command contains `-p buzz-agents` and the agents image tag. With `AGENTS=` it contains neither. `make agents-logs AGENT=codex` with `AGENTS=claude` exits 1.
- [ ] **Step 2:** Run `bash tests/aws/test-deploy.sh`. Expected: FAIL.
- [ ] **Step 3: Implement the changes** per the Interfaces. The agent rollout starts only after the relay health check passes, and a failed agent start fails the deploy with the container logs printed. Rollback restores both tags.
- [ ] **Step 4: Update `docs/aws-deployment.md`:** agent setup order (`make apply`, `make deploy` once so the relay is up, `export ANTHROPIC_API_KEY=... OPENAI_API_KEY=...`, `make agents-secrets`, `make agents-register`, `make deploy`), how to mention an agent in a channel, setting API spend limits, `!shutdown` stopping an agent for good, and restarting it with `make deploy`.
- [ ] **Step 5: Local connection check (agent-run, local stack only).** Start the local relay as documented in `AGENTS.md` (`just setup`, `just relay`). Generate a throwaway key with `buzz-admin`, add it as a member, run the agents image against `ws://host.docker.internal:3000` with `AGENTS` set to `claude` and a dummy `ANTHROPIC_API_KEY`. Expected: the container stays up and the relay shows the agent online. Record the exact commands in `docs/aws-deployment.md` under a troubleshooting heading. If the relay's host-based community lookup rejects `host.docker.internal`, use the host name the local relay accepts and note it.
- [ ] **Step 6:** Run `bash tests/aws/run.sh`, `shellcheck scripts/aws/*.sh`, and `make help`. Expected: all PASS; help lists the agent targets.
- [ ] **Step 7: Commit.** `git add -A && git commit -s -m "feat(aws): roll out agents with deploy and add agent ops targets"`

---

## Self-review notes

- Spec coverage: settings (Task 1), sync (2), Terraform resources (3, 4), secrets and `.env` rendering (5), deploy flow and rollback (5), backups (6), runbook and testing (all tasks), phase 2 image and Compose (7), agent identities and secrets (8), agent rollout and ops (9).
- The spec's `bootstrap` bucket was removed at the owner's request. No task creates a state bucket.
- Not run by agents: `make apply`, `make deploy`, `make agents-register`, and any command that touches the real AWS account. Tasks 3 to 6 verify with `-backend=false`, `validate`, shellcheck, and the `aws` stub only. Task 7 and Task 9 step 5 run Docker and the relay locally.
- Open items carried from the spec: confirm the Codex command name and the accepted private key format (Tasks 7 and 8), and confirm the musl-built `sprig` binary runs on Debian (Task 7).
