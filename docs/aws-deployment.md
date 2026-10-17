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

On the first `make deploy` the backup prints `no postgres container` and continues, because no database exists yet. If the deploy fails right after `make apply` because the server has not registered with SSM yet, wait a few minutes and run it again.

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

`make logs` ends after about 10 seconds, so it cannot hang. For a live follow, use `make shell` and run `cd /opt/buzz/deploy/compose && BUZZ_COMPOSE_TLS=true ./run.sh logs`.

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

Cloud agents are covered later in this document once they ship.

## Running the script tests

`make test-aws` runs the tests for every script. They use stand-ins for `aws`, `docker`, `terraform`, and `curl`, so they never touch a real account.
