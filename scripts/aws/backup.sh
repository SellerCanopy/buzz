#!/usr/bin/env bash
# Dumps the Postgres database on the server and uploads the compressed dump to
# s3://<backup bucket>/db/<UTC timestamp>.sql.gz. The dump runs on the server
# over SSM, so the database password never leaves the Postgres container.
# Prints "no postgres container" and exits 0 when the server has no running
# Postgres container (the first deploy). Exits nonzero on any failure.
# Usage: backup.sh
# Environment: POLL_INTERVAL and POLL_ATTEMPTS (seconds, count; SSM command wait).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/aws/lib.sh
. "$SCRIPT_DIR/lib.sh"
# shellcheck source=scripts/aws/ssm.sh
. "$SCRIPT_DIR/ssm.sh"

NO_POSTGRES_MARKER="no postgres container"
UPLOADED_MARKER="backup uploaded:"

# remote_script <bucket>: prints the commands the server runs, one per line.
# The dump goes to a temp file and is checked before anything is uploaded, and
# pipefail plus set -e stop the script on any failure, so a partial dump is
# never stored as a backup.
remote_script() {
  echo "#!/bin/bash"
  echo "set -euo pipefail"
  echo "export HOME=/root"
  echo "export AWS_DEFAULT_REGION='$AWS_REGION'"
  echo "dir=/opt/buzz/deploy/compose"
  # No .env means no release was ever unpacked here, so there is no database.
  echo "if [ ! -f \"\$dir/.env\" ]; then echo '$NO_POSTGRES_MARKER'; exit 0; fi"
  echo "cd \"\$dir\""
  # Same Compose files as run.sh uses with BUZZ_COMPOSE_TLS=true.
  echo "compose() { docker compose --env-file .env -f compose.yml -f compose.caddy.yml \"\$@\"; }"
  # Assigned on its own line so a docker failure stops the script instead of
  # reading as "no container".
  echo "running=\"\$(compose ps --status running -q postgres)\""
  echo "if [ -z \"\$running\" ]; then echo '$NO_POSTGRES_MARKER'; exit 0; fi"
  echo "ts=\"\$(date -u +%Y%m%dT%H%M%SZ)\""
  echo "tmp=\"\$(mktemp /var/tmp/buzz-dump.XXXXXX)\""
  echo "trap 'rm -f \"\$tmp\" \"\$tmp.gz\"' EXIT"
  # The user and database come from the container's own environment, which is
  # what Compose sets from POSTGRES_USER and POSTGRES_DB. The local socket
  # needs no password.
  echo "compose exec -T postgres sh -c 'pg_dump -U \"\$POSTGRES_USER\" -d \"\$POSTGRES_DB\"' > \"\$tmp\""
  echo "[ -s \"\$tmp\" ] || { echo 'pg_dump wrote nothing, nothing uploaded' >&2; exit 1; }"
  echo "gzip \"\$tmp\""
  echo "aws s3 cp \"\$tmp.gz\" \"s3://$1/db/\$ts.sql.gz\""
  echo "echo \"$UPLOADED_MARKER s3://$1/db/\$ts.sql.gz\""
}

require_tool aws
require_tool terraform
load_settings
cd "$REPO_ROOT"

bucket="$(tf_output backup_bucket)"
instance="$(tf_output instance_id)"
[ -n "$bucket" ] && [ -n "$instance" ] || die "terraform outputs are empty (run make apply first)"
[[ "$bucket" =~ ^[a-z0-9][a-z0-9.-]*$ ]] || die "backup bucket name looks wrong: $bucket"

command_id="$(remote_script "$bucket" | ssm_send "$instance" "buzz backup")"
[ -n "$command_id" ] || die "send-command returned no command id"
echo "sent SSM command $command_id, waiting"

ssm_wait "$command_id" "$instance" || exit 1
output="$(ssm_output "$command_id" "$instance")"
echo "$output"

# A command that succeeds without reporting either result is not trusted: a
# deploy would go ahead believing the data was saved.
case "$output" in
  *"$UPLOADED_MARKER"* | *"$NO_POSTGRES_MARKER"*) ;;
  *) die "the server finished without reporting an upload or 'no postgres container'" ;;
esac
