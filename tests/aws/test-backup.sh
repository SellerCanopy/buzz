#!/usr/bin/env bash
# Tests for scripts/aws/backup.sh with stub aws, docker, and terraform.
# Two layers. The local scenarios run backup.sh and check what it sends and how
# it reacts to the server's answer. The server scenarios pull the command out of
# the logged send-command call and run it here against a fake /opt/buzz, a fake
# docker, and a fake aws, so the server-side logic is tested too.
set -uo pipefail
# shellcheck source=tests/aws/helpers.sh
. "$(dirname "${BASH_SOURCE[0]}")/helpers.sh"

BUCKET="buzz-backups-test"
OUT="$WORK/out"
ERR="$WORK/err"
OPT="$WORK/opt"
DUMP_TEXT="CREATE TABLE events (id int);"

write_aws_env

stubs_ok() {
  reset_stubs
  printf '%s' "$BUCKET" > "$STUB_DIR/tf-backup_bucket.out"
  printf '%s' "i-0abc123" > "$STUB_DIR/tf-instance_id.out"
  echo "cmd-1" > "$STUB_DIR/ssm-send-command.out"
  echo "Success" > "$STUB_DIR/ssm-get-command-invocation.out"
  echo "backup uploaded: s3://$BUCKET/db/20260101T000000Z.sql.gz" \
    > "$STUB_DIR/ssm-get-command-invocation.StandardOutputContent.out"
}

# run_backup <scripts-dir> [VAR=value ...]: sets RC.
run_backup() {
  local dir="$1"
  shift
  RC=0
  env POLL_INTERVAL=0 POLL_ATTEMPTS=3 ${@+"$@"} bash "$dir/backup.sh" > "$OUT" 2> "$ERR" || RC=$?
}

# sent_line: the logged send-command call.
sent_line() { grep -F 'aws ssm send-command' "$STUB_LOG" | head -n 1; }

# decode_command <file>: writes the logged SSM command as a shell script, one
# command per line, with /opt/buzz moved under the test sandbox.
decode_command() {
  sent_line \
    | sed -e 's/^.*{"commands":\["//' -e 's/"\]}.*$//' -e 's/","/\
/g' -e 's/\\"/"/g' -e 's/\\\\/\\/g' \
    | sed "s#/opt/buzz#$OPT#g" > "$1"
}

# ---- local scenarios --------------------------------------------------------

scenario_sends_dump_command() {
  stubs_ok
  run_backup "$1"
  [ "$RC" -eq 0 ] || return 1
  [ "$(log_count 'send-command')" -eq 1 ] || return 1
  local line
  line="$(sent_line)"
  case "$line" in
    *"--instance-ids i-0abc123 --document-name AWS-RunShellScript "*) ;;
    *) return 1 ;;
  esac
  case "$line" in *pg_dump*) ;; *) return 1 ;; esac
  case "$line" in *pipefail*) ;; *) return 1 ;; esac
  case "$line" in *"s3://$BUCKET/db/"*) ;; *) return 1 ;; esac
  case "$line" in *"%Y%m%dT%H%M%SZ"*) ;; *) return 1 ;; esac
  case "$line" in *"export AWS_DEFAULT_REGION='us-east-1'"*) ;; *) return 1 ;; esac
  case "$line" in *PGPASSWORD*) return 1 ;; esac
  # Order inside the command: strict mode, then the dump, then the upload.
  local pre dump_at upload_at strict_at
  pre="${line%%pipefail*}"
  strict_at="${#pre}"
  pre="${line%%pg_dump -U*}"
  dump_at="${#pre}"
  pre="${line%%aws s3 cp*}"
  upload_at="${#pre}"
  [ "$strict_at" -lt "$dump_at" ] && [ "$dump_at" -lt "$upload_at" ] || return 1
  grep -qF "backup uploaded: s3://$BUCKET/db/" "$OUT"
}

scenario_no_postgres_reported() {
  stubs_ok
  echo "no postgres container" > "$STUB_DIR/ssm-get-command-invocation.StandardOutputContent.out"
  run_backup "$1"
  [ "$RC" -eq 0 ] && grep -qF "no postgres container" "$OUT"
}

scenario_send_failure() {
  stubs_ok
  echo 1 > "$STUB_DIR/ssm-send-command.rc"
  run_backup "$1"
  [ "$RC" -ne 0 ] && [ "$(log_count 'get-command-invocation')" -eq 0 ]
}

scenario_command_failed() {
  stubs_ok
  echo "Failed" > "$STUB_DIR/ssm-get-command-invocation.out"
  run_backup "$1"
  [ "$RC" -ne 0 ] && grep -qF "status Failed" "$ERR" \
    && [ "$(log_count 'StandardOutputContent')" -eq 0 ]
}

scenario_command_timeout() {
  stubs_ok
  echo "InProgress" > "$STUB_DIR/ssm-get-command-invocation.out"
  run_backup "$1"
  [ "$RC" -ne 0 ] && grep -qF "timed out" "$ERR"
}

# Success with no recognizable answer is not a backup.
scenario_silent_success_rejected() {
  stubs_ok
  : > "$STUB_DIR/ssm-get-command-invocation.StandardOutputContent.out"
  run_backup "$1"
  [ "$RC" -ne 0 ] && grep -qF "without reporting" "$ERR"
}

scenario_missing_outputs() {
  stubs_ok
  rm -f "$STUB_DIR/tf-instance_id.out"
  run_backup "$1"
  [ "$RC" -ne 0 ] && [ "$(log_count 'send-command')" -eq 0 ]
}

scenario_bad_bucket_name() {
  stubs_ok
  printf '%s' "bad bucket;x" > "$STUB_DIR/tf-backup_bucket.out"
  run_backup "$1"
  [ "$RC" -ne 0 ] && grep -qF "bucket name looks wrong" "$ERR" && [ "$(log_count 'send-command')" -eq 0 ]
}

# ---- server scenarios -------------------------------------------------------

# server_setup <with-env: 1|0>: sandbox for the decoded command and its stubs.
server_setup() {
  stubs_ok
  rm -rf "$OPT"
  mkdir -p "$OPT/deploy/compose"
  [ "$1" -eq 0 ] || echo "POSTGRES_PASSWORD=x" > "$OPT/deploy/compose/.env"
  echo "abc123" > "$STUB_DIR/docker-compose-ps.out"
  printf '%s\n' "$DUMP_TEXT" > "$STUB_DIR/docker-compose-exec.out"
}

# run_server <scripts-dir>: sends the command through backup.sh, decodes it from
# the log, then runs it. Sets RC, SRV_OUT, SRV_ERR.
run_server() {
  run_backup "$1"
  [ "$RC" -eq 0 ] || return 1
  decode_command "$WORK/server.sh"
  : > "$STUB_LOG"
  RC=0
  bash "$WORK/server.sh" > "$WORK/srv.out" 2> "$WORK/srv.err" || RC=$?
}

leftover_dumps() { find /var/tmp -maxdepth 1 -name 'buzz-dump.*' 2>/dev/null | wc -l | tr -d ' '; }

scenario_server_uploads_dump() {
  server_setup 1
  local before
  before="$(leftover_dumps)"
  run_server "$1" || return 1
  [ "$RC" -eq 0 ] || return 1
  grep -qE "^backup uploaded: s3://$BUCKET/db/[0-9]{8}T[0-9]{6}Z\.sql\.gz$" "$WORK/srv.out" || return 1
  [ "$(log_count '^aws s3 cp ')" -eq 1 ] || return 1
  [ "$(log_count '^aws s3 cp .*\.gz s3://'"$BUCKET"'/db/[0-9]{8}T[0-9]{6}Z\.sql\.gz$')" -eq 1 ] || return 1
  [ "$(log_count '^docker compose --env-file .env -f compose.yml -f compose.caddy.yml ps --status running -q postgres$')" -eq 1 ] || return 1
  [ "$(log_count '^docker compose .* exec -T postgres sh -c pg_dump ')" -eq 1 ] || return 1
  # What was uploaded is the gzipped dump, and the temp files are gone.
  [ "$(gzip -dc "$STUB_DIR/s3-cp.upload")" = "$DUMP_TEXT" ] || return 1
  [ "$(leftover_dumps)" -eq "$before" ]
}

scenario_server_no_container() {
  server_setup 1
  : > "$STUB_DIR/docker-compose-ps.out"
  run_server "$1" || return 1
  [ "$RC" -eq 0 ] && grep -qx "no postgres container" "$WORK/srv.out" \
    && [ "$(log_count '^aws s3 cp ')" -eq 0 ] && [ "$(log_count ' exec ')" -eq 0 ]
}

# First deploy: no .env yet, so Docker is never asked.
scenario_server_no_release_yet() {
  server_setup 0
  run_server "$1" || return 1
  [ "$RC" -eq 0 ] && grep -qx "no postgres container" "$WORK/srv.out" \
    && [ "$(log_count '^docker ')" -eq 0 ] && [ "$(log_count '^aws s3 cp ')" -eq 0 ]
}

# A dump that dies partway must fail the script and upload nothing.
scenario_server_failed_dump_not_uploaded() {
  server_setup 1
  echo "partial output" > "$STUB_DIR/docker-compose-exec.out"
  echo 1 > "$STUB_DIR/docker-compose-exec.rc"
  local before
  before="$(leftover_dumps)"
  run_server "$1" || return 1
  [ "$RC" -ne 0 ] && [ "$(log_count '^aws s3 cp ')" -eq 0 ] \
    && ! grep -q "backup uploaded" "$WORK/srv.out" && [ "$(leftover_dumps)" -eq "$before" ]
}

scenario_server_empty_dump_not_uploaded() {
  server_setup 1
  : > "$STUB_DIR/docker-compose-exec.out"
  run_server "$1" || return 1
  [ "$RC" -ne 0 ] && [ "$(log_count '^aws s3 cp ')" -eq 0 ]
}

# Docker failing is not the same as no container.
scenario_server_docker_error_is_failure() {
  server_setup 1
  echo 1 > "$STUB_DIR/docker-compose-ps.rc"
  run_server "$1" || return 1
  [ "$RC" -ne 0 ] && ! grep -q "no postgres container" "$WORK/srv.out" \
    && [ "$(log_count '^aws s3 cp ')" -eq 0 ]
}

scenario_server_upload_failure_fails() {
  server_setup 1
  echo 1 > "$STUB_DIR/s3-cp.rc"
  run_server "$1" || return 1
  [ "$RC" -ne 0 ] && ! grep -q "backup uploaded" "$WORK/srv.out"
}

D="$REPO_ROOT/scripts/aws"
expect_ok "sends a pipefail dump command that uploads to s3://<bucket>/db/ after the dump" scenario_sends_dump_command "$D"
expect_ok "server reports no postgres container: printed locally, exit 0" scenario_no_postgres_reported "$D"
expect_ok "send-command failure exits nonzero" scenario_send_failure "$D"
expect_ok "failed server command exits nonzero" scenario_command_failed "$D"
expect_ok "server command that never finishes times out" scenario_command_timeout "$D"
expect_ok "success without an upload or no-container report is rejected" scenario_silent_success_rejected "$D"
expect_ok "missing terraform outputs send nothing" scenario_missing_outputs "$D"
expect_ok "odd bucket name is rejected before sending" scenario_bad_bucket_name "$D"
expect_ok "server: running container gives an upload of the gzipped dump" scenario_server_uploads_dump "$D"
expect_ok "server: no running container prints the marker and uploads nothing" scenario_server_no_container "$D"
expect_ok "server: no .env yet (first deploy) prints the marker without calling docker" scenario_server_no_release_yet "$D"
expect_ok "server: a failing pg_dump fails the script and uploads nothing" scenario_server_failed_dump_not_uploaded "$D"
expect_ok "server: an empty dump fails the script and uploads nothing" scenario_server_empty_dump_not_uploaded "$D"
expect_ok "server: a docker error is a failure, not 'no postgres container'" scenario_server_docker_error_is_failure "$D"
expect_ok "server: a failed upload fails the script" scenario_server_upload_failure_fails "$D"

expect_caught "silent success accepted" scenario_silent_success_rejected backup.sh \
  's/^  \*\) die "the server finished without reporting.*$/  *) true ;;/'
expect_caught "server answer not printed" scenario_no_postgres_reported backup.sh \
  's/^echo "\$output"$/true/'
expect_caught "bucket name not checked" scenario_bad_bucket_name backup.sh \
  's/^\[\[ "\$bucket" =~ .*$/true/'
expect_caught "wrong upload prefix" scenario_sends_dump_command backup.sh \
  's#/db/#/dumps/#g'
expect_caught "pipefail dropped from the server command" scenario_sends_dump_command backup.sh \
  's/set -euo pipefail"$/set -eu"/'
expect_caught "strict mode dropped on the server" scenario_server_failed_dump_not_uploaded backup.sh \
  's/^  echo "set -euo pipefail"$/  echo "set +e"/'
expect_caught "empty-dump check removed" scenario_server_empty_dump_not_uploaded backup.sh \
  's/^  echo "\[ -s .*$/  true/'
expect_caught "docker failure read as no container" scenario_server_docker_error_is_failure backup.sh \
  's/compose ps --status running -q postgres\)/compose ps --status running -q postgres || true)/'
expect_caught ".env guard removed" scenario_server_no_release_yet backup.sh \
  's/^  echo "if \[ ! -f .*$/  true/'
expect_caught "running-container check removed" scenario_server_no_container backup.sh \
  's/^  echo "if \[ -z .*running.*$/  true/'
expect_caught "upload not checked" scenario_server_upload_failure_fails backup.sh \
  's/^  echo "aws s3 cp (.*)"$/  echo "aws s3 cp \1 || true"/'

exit "$fail"
