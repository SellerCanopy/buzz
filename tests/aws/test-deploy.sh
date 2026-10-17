#!/usr/bin/env bash
# Tests for scripts/aws/deploy.sh and build.sh with stub aws, docker, terraform,
# and curl. Each scenario runs in a throwaway git repo, never the real one.
set -uo pipefail
# shellcheck source=tests/aws/helpers.sh
. "$(dirname "${BASH_SOURCE[0]}")/helpers.sh"

REPO_URL="123456789012.dkr.ecr.us-east-1.amazonaws.com/buzz/relay"
REGISTRY="123456789012.dkr.ecr.us-east-1.amazonaws.com"
BUCKET="buzz-backups-test"
OUT="$WORK/out"
ERR="$WORK/err"
REPO="$WORK/repo"

write_aws_env

# new_repo: a committed throwaway repo with the files the release tarball needs.
# Sets SHA to its short HEAD.
new_repo() {
  rm -rf "$REPO"
  mkdir -p "$REPO/deploy/compose" "$REPO/scripts/aws"
  echo "run" > "$REPO/deploy/compose/run.sh"
  echo "lib" > "$REPO/scripts/aws/lib.sh"
  echo "render" > "$REPO/scripts/aws/render-env.sh"
  echo "remote" > "$REPO/scripts/aws/remote-deploy.sh"
  echo "example" > "$REPO/deploy/compose/.env.example"
  printf 'ignored.txt\ndeploy/compose/.env\ndeploy/compose/.env.*\n!deploy/compose/.env.example\n' > "$REPO/.gitignore"
  (
    cd "$REPO" || exit 1
    git init -q .
    git add -A
    git -c user.name=t -c user.email=t@t -c commit.gpgsign=false commit -q -m init
  )
  SHA="$(git -C "$REPO" rev-parse --short HEAD)"
}

# stubs_ok: terraform outputs and happy-path answers for a full deploy.
stubs_ok() {
  reset_stubs
  printf '%s' "$REPO_URL" > "$STUB_DIR/tf-ecr_repository_url.out"
  printf '%s' "$BUCKET" > "$STUB_DIR/tf-backup_bucket.out"
  printf '%s' "i-0abc123" > "$STUB_DIR/tf-instance_id.out"
  echo "cmd-1" > "$STUB_DIR/ssm-send-command.out"
  echo "Success" > "$STUB_DIR/ssm-get-command-invocation.out"
}

# backup_ok / backup_fail: BACKUP_CMD scripts. Each echoes a marker to stdout and
# creates $WORK/backup.ran so tests can tell whether the backup started.
mk_backups() {
  rm -f "$WORK/backup.ran"
  printf '#!/usr/bin/env bash\necho BACKUP-RAN\ntouch "%s/backup.ran"\n' "$WORK" > "$WORK/backup-ok.sh"
  printf '#!/usr/bin/env bash\necho BACKUP-RAN\ntouch "%s/backup.ran"\necho "disk full" >&2\nexit 7\n' "$WORK" > "$WORK/backup-fail.sh"
}

# run_deploy <scripts-dir> [VAR=value ...] [-- args]: sets RC.
run_deploy() {
  local dir="$1"
  shift
  local -a envs=()
  while [ $# -gt 0 ] && [ "$1" != "--" ]; do envs+=("$1"); shift; done
  [ $# -gt 0 ] && shift
  RC=0
  env REPO_ROOT="$REPO" BACKUP_CMD="$WORK/backup-ok.sh" \
    POLL_INTERVAL=0 POLL_ATTEMPTS=3 HEALTH_INTERVAL=0 HEALTH_ATTEMPTS=3 \
    ${envs[@]+"${envs[@]}"} bash "$dir/deploy.sh" "$@" > "$OUT" 2> "$ERR" || RC=$?
}

# nothing_shipped: no build, login, upload, or server command in the call log.
nothing_shipped() {
  [ "$(log_count '^docker ')" -eq 0 ] \
    && [ "$(log_count '^aws s3 cp')" -eq 0 ] \
    && [ "$(log_count 'send-command')" -eq 0 ] \
    && [ "$(log_count 'put-parameter')" -eq 0 ]
}

scenario_dirty_untracked() {
  new_repo; stubs_ok; mk_backups
  echo x > "$REPO/new-file.txt"
  run_deploy "$1"
  [ "$RC" -eq 1 ] && grep -qF "working tree is not clean" "$ERR" && grep -qF "new-file.txt" "$ERR" \
    && nothing_shipped && [ ! -e "$WORK/backup.ran" ]
}

scenario_dirty_modified() {
  new_repo; stubs_ok; mk_backups
  echo more >> "$REPO/deploy/compose/run.sh"
  run_deploy "$1"
  [ "$RC" -eq 1 ] && grep -qF "working tree is not clean" "$ERR" && nothing_shipped
}

scenario_ignored_file_is_clean() {
  new_repo; stubs_ok; mk_backups
  echo x > "$REPO/ignored.txt"
  run_deploy "$1" DRY_RUN=1
  [ "$RC" -eq 0 ] && grep -qF "SSM command (dry run)" "$OUT"
}

scenario_no_recorded_image_empty() {
  new_repo; stubs_ok; mk_backups
  run_deploy "$1" ROLLBACK=1
  [ "$RC" -eq 1 ] && grep -qF "no recorded good image" "$ERR" && nothing_shipped && [ ! -e "$WORK/backup.ran" ]
}

scenario_no_recorded_image_not_found() {
  new_repo; stubs_ok; mk_backups
  echo 254 > "$STUB_DIR/ssm-get-parameter.rc"
  echo "An error occurred (ParameterNotFound) when calling the GetParameter operation" > "$STUB_DIR/ssm-get-parameter.err"
  run_deploy "$1" ROLLBACK=1
  [ "$RC" -eq 1 ] && grep -qF "no recorded good image" "$ERR" && nothing_shipped
}

scenario_recorded_image_read_error() {
  new_repo; stubs_ok; mk_backups
  echo 254 > "$STUB_DIR/ssm-get-parameter.rc"
  echo "An error occurred (AccessDeniedException) when calling the GetParameter operation" > "$STUB_DIR/ssm-get-parameter.err"
  run_deploy "$1" ROLLBACK=1
  [ "$RC" -eq 1 ] && grep -qF "cannot read /buzz/last_good_image" "$ERR" \
    && ! grep -qF "no recorded good image" "$ERR" && nothing_shipped
}

scenario_dry_run_backup_first() {
  new_repo; stubs_ok; mk_backups
  run_deploy "$1" DRY_RUN=1
  [ "$RC" -eq 0 ] || return 1
  local b s
  b="$(grep -n 'BACKUP-RAN' "$OUT" | head -n 1 | cut -d: -f1)"
  s="$(grep -n 'SSM command (dry run)' "$OUT" | head -n 1 | cut -d: -f1)"
  [ -n "$b" ] && [ -n "$s" ] && [ "$b" -lt "$s" ] || return 1
  grep -qF "AWS-RunShellScript" "$OUT" || return 1
  grep -qF -- "--instance-ids i-0abc123" "$OUT" || return 1
  grep -qF "export BUZZ_DOMAIN='$DOMAIN'" "$OUT" || return 1
  grep -qF "export RELAY_OWNER_PUBKEY='$PUBKEY'" "$OUT" || return 1
  grep -qF "export ECR_REPO='$REPO_URL'" "$OUT" || return 1
  grep -qF "aws s3 cp 's3://$BUCKET/releases/$SHA.tgz' /opt/buzz/release.tgz" "$OUT" || return 1
  grep -qF "bash /opt/buzz/scripts/aws/remote-deploy.sh '$SHA'" "$OUT" || return 1
  grep -qF "tar xzf /opt/buzz/release.tgz --no-same-owner -C /opt/buzz" "$OUT" || return 1
  nothing_shipped
}

scenario_backup_failure_aborts() {
  new_repo; stubs_ok; mk_backups
  run_deploy "$1" BACKUP_CMD="$WORK/backup-fail.sh"
  [ "$RC" -ne 0 ] || return 1
  grep -qF "backup failed" "$ERR" || return 1
  ! grep -q "SSM command" "$OUT" || return 1
  nothing_shipped
}

# A failing backup warns loudly but does not stop a rollback.
scenario_rollback_continues_past_backup_failure() {
  new_repo; stubs_ok; mk_backups
  run_deploy "$1" BACKUP_CMD="$WORK/backup-fail.sh" ROLLBACK=1 -- oldtag4
  [ "$RC" -eq 0 ] || return 1
  grep -qF "WARNING: backup failed" "$ERR" || return 1
  [ -e "$WORK/backup.ran" ] || return 1
  [ "$(log_count 'send-command')" -eq 1 ] && [ "$(log_count '^docker ')" -eq 0 ]
}

scenario_rollback_continues_past_backup_failure_dry_run() {
  new_repo; stubs_ok; mk_backups
  run_deploy "$1" DRY_RUN=1 BACKUP_CMD="$WORK/backup-fail.sh" ROLLBACK=1 -- oldtag4
  [ "$RC" -eq 0 ] && grep -qF "WARNING: backup failed" "$ERR" && grep -qF "SSM command (dry run)" "$OUT"
}

scenario_backup_failure_aborts_dry_run() {
  new_repo; stubs_ok; mk_backups
  run_deploy "$1" DRY_RUN=1 BACKUP_CMD="$WORK/backup-fail.sh"
  [ "$RC" -ne 0 ] && ! grep -q "SSM command" "$OUT" && nothing_shipped
}

scenario_missing_backup_script_aborts() {
  new_repo; stubs_ok; mk_backups
  run_deploy "$1" BACKUP_CMD="$WORK/does-not-exist.sh"
  [ "$RC" -ne 0 ] && nothing_shipped
}

# Full happy path: order of the calls and the contents of the tarball.
scenario_full_deploy() {
  new_repo; stubs_ok; mk_backups
  echo "SECRET=1" > "$REPO/deploy/compose/.env"
  echo "SECRET=1" > "$REPO/deploy/compose/.env.bak"
  echo "SECRET=1" > "$REPO/deploy/compose/.env.local"
  run_deploy "$1"
  [ "$RC" -eq 0 ] || return 1
  local login build up send poll health put prev=0 n
  login="$(log_line '^docker login --username AWS --password-stdin '"$REGISTRY"'$')"
  build="$(log_line '^docker buildx build --platform linux/arm64 -t '"$REPO_URL:$SHA"' --push ')"
  up="$(log_line '^aws s3 cp .*release.tgz s3://'"$BUCKET/releases/$SHA"'.tgz$')"
  send="$(log_line '^aws ssm send-command --instance-ids i-0abc123 --document-name AWS-RunShellScript ')"
  poll="$(log_line '^aws ssm get-command-invocation --command-id cmd-1 --instance-id i-0abc123 ')"
  health="$(log_line '^curl .*https://'"$DOMAIN"'/_liveness$')"
  put="$(log_line '^aws ssm put-parameter --name /buzz/last_good_image --type String --value '"$SHA"' --overwrite$')"
  for n in "$login" "$build" "$up" "$send" "$poll" "$health" "$put"; do
    [ -n "$n" ] && [ "$n" -gt "$prev" ] || return 1
    prev="$n"
  done
  [ -e "$WORK/backup.ran" ] || return 1
  # Tarball: compose files and server scripts in, the local .env out.
  local list
  list="$(tar tzf "$STUB_DIR/s3-cp.upload")"
  printf '%s\n' "$list" | grep -qx 'deploy/compose/run.sh' || return 1
  printf '%s\n' "$list" | grep -qx 'scripts/aws/lib.sh' || return 1
  printf '%s\n' "$list" | grep -qx 'scripts/aws/render-env.sh' || return 1
  printf '%s\n' "$list" | grep -qx 'scripts/aws/remote-deploy.sh' || return 1
  printf '%s\n' "$list" | grep -qx 'deploy/compose/.env.example' || return 1
  ! printf '%s\n' "$list" | grep -qE 'deploy/compose/\.env(\.bak|\.local)?$'
}

scenario_explicit_tag() {
  new_repo; stubs_ok; mk_backups
  run_deploy "$1" -- v1.2.3
  [ "$RC" -eq 0 ] || return 1
  [ "$(log_count 'docker buildx build .* -t '"$REPO_URL"':v1.2.3 --push ')" -eq 1 ] \
    && [ "$(log_count 'put-parameter --name /buzz/last_good_image --type String --value v1.2.3 ')" -eq 1 ]
}

scenario_bad_tag_rejected() {
  new_repo; stubs_ok; mk_backups
  run_deploy "$1" DRY_RUN=1 -- 'x;touch /tmp/pwned'
  [ "$RC" -ne 0 ] && grep -qF "tag may contain only" "$ERR" && nothing_shipped
}

scenario_command_failed() {
  new_repo; stubs_ok; mk_backups
  echo "Failed" > "$STUB_DIR/ssm-get-command-invocation.out"
  run_deploy "$1"
  [ "$RC" -eq 1 ] && grep -qF "status Failed" "$ERR" \
    && [ "$(log_count '^curl ')" -eq 0 ] && [ "$(log_count 'put-parameter')" -eq 0 ]
}

scenario_command_timeout() {
  new_repo; stubs_ok; mk_backups
  echo "InProgress" > "$STUB_DIR/ssm-get-command-invocation.out"
  run_deploy "$1"
  [ "$RC" -eq 1 ] && grep -qF "timed out" "$ERR" \
    && [ "$(log_count 'get-command-invocation .*--query Status')" -eq 3 ] \
    && [ "$(log_count '^curl ')" -eq 0 ] && [ "$(log_count 'put-parameter')" -eq 0 ]
}

# Seven reads in a row fail: the error is shown once, polling still runs to the timeout.
scenario_poll_error_surfaced() {
  new_repo; stubs_ok; mk_backups
  echo 254 > "$STUB_DIR/ssm-get-command-invocation.rc"
  echo "An error occurred (AccessDeniedException) when calling the GetCommandInvocation operation" > "$STUB_DIR/ssm-get-command-invocation.err"
  : > "$STUB_DIR/ssm-get-command-invocation.out"
  run_deploy "$1" POLL_ATTEMPTS=7
  [ "$RC" -eq 1 ] && grep -qF "timed out" "$ERR" || return 1
  [ "$(grep -c 'AccessDeniedException' "$ERR")" -eq 1 ] || return 1
  [ "$(log_count 'get-command-invocation .*--query Status')" -eq 7 ]
}

scenario_poll_few_empty_reads_quiet() {
  new_repo; stubs_ok; mk_backups
  echo 254 > "$STUB_DIR/ssm-get-command-invocation.rc"
  echo "An error occurred (AccessDeniedException) when calling the GetCommandInvocation operation" > "$STUB_DIR/ssm-get-command-invocation.err"
  : > "$STUB_DIR/ssm-get-command-invocation.out"
  run_deploy "$1" POLL_ATTEMPTS=4
  [ "$RC" -eq 1 ] && grep -qF "timed out" "$ERR" && ! grep -q 'AccessDeniedException' "$ERR"
}

scenario_unhealthy_not_recorded() {
  new_repo; stubs_ok; mk_backups
  echo 22 > "$STUB_DIR/curl.rc"
  run_deploy "$1"
  [ "$RC" -eq 1 ] && grep -qF "health check failed" "$ERR" \
    && [ "$(log_count '^curl ')" -eq 3 ] && [ "$(log_count 'put-parameter')" -eq 0 ]
}

scenario_send_failure() {
  new_repo; stubs_ok; mk_backups
  echo 1 > "$STUB_DIR/ssm-send-command.rc"
  run_deploy "$1"
  [ "$RC" -ne 0 ] && [ "$(log_count 'put-parameter')" -eq 0 ]
}

scenario_rollback_deploys_recorded() {
  new_repo; stubs_ok; mk_backups
  echo "oldtag1" > "$STUB_DIR/ssm-get-parameter.out"
  run_deploy "$1" ROLLBACK=1
  [ "$RC" -eq 0 ] || return 1
  [ "$(log_count '^docker ')" -eq 0 ] && [ "$(log_count '^aws s3 cp')" -eq 0 ] || return 1
  [ "$(log_count 'head-object --bucket '"$BUCKET"' --key releases/oldtag1.tgz')" -eq 1 ] || return 1
  [ "$(log_count 'send-command')" -eq 1 ] || return 1
  [ "$(log_count 'put-parameter --name /buzz/last_good_image --type String --value oldtag1 ')" -eq 1 ]
}

scenario_rollback_explicit_tag_dirty_tree() {
  new_repo; stubs_ok; mk_backups
  echo x > "$REPO/dirty.txt"
  run_deploy "$1" ROLLBACK=1 -- oldtag2
  [ "$RC" -eq 0 ] && [ "$(log_count 'send-command')" -eq 1 ] && [ "$(log_count '^docker ')" -eq 0 ] \
    && [ "$(log_count 'get-parameter')" -eq 0 ]
}

scenario_rollback_missing_release() {
  new_repo; stubs_ok; mk_backups
  echo 254 > "$STUB_DIR/s3api-head-object.rc"
  run_deploy "$1" ROLLBACK=1 -- oldtag3
  [ "$RC" -ne 0 ] && grep -qF "no release tarball" "$ERR" && [ "$(log_count 'send-command')" -eq 0 ]
}

# build.sh: push logs in first; no push loads locally.
scenario_build_load() {
  new_repo; stubs_ok
  RC=0
  bash "$1/build.sh" abc1234 > "$OUT" 2> "$ERR" || RC=$?
  [ "$RC" -eq 0 ] || return 1
  [ "$(log_count '^docker buildx build --platform linux/arm64 -t '"$REPO_URL"':abc1234 --load ')" -eq 1 ] \
    && [ "$(log_count 'docker login')" -eq 0 ] && [ "$(log_count 'ecr get-login-password')" -eq 0 ]
}

scenario_build_push() {
  new_repo; stubs_ok
  RC=0
  bash "$1/build.sh" abc1234 --push > "$OUT" 2> "$ERR" || RC=$?
  [ "$RC" -eq 0 ] || return 1
  local login build
  login="$(log_line '^docker login --username AWS --password-stdin '"$REGISTRY"'$')"
  build="$(log_line '^docker buildx build --platform linux/arm64 -t '"$REPO_URL"':abc1234 --push ')"
  [ -n "$login" ] && [ -n "$build" ] && [ "$login" -lt "$build" ] \
    && [ "$(log_count 'ecr get-login-password --region us-east-1')" -eq 1 ]
}

scenario_build_login_failure_stops() {
  new_repo; stubs_ok
  echo 1 > "$STUB_DIR/docker-login.rc"
  RC=0
  bash "$1/build.sh" abc1234 --push > "$OUT" 2> "$ERR" || RC=$?
  [ "$RC" -ne 0 ] && [ "$(log_count 'buildx')" -eq 0 ]
}

R="$REPO_ROOT/scripts/aws"
expect_ok "dirty tree (untracked file) exits 1 and ships nothing" scenario_dirty_untracked "$R"
expect_ok "dirty tree (modified file) exits 1 and ships nothing" scenario_dirty_modified "$R"
expect_ok "git-ignored files do not make the tree dirty" scenario_ignored_file_is_clean "$R"
expect_ok "rollback with empty last_good_image: no recorded good image" scenario_no_recorded_image_empty "$R"
expect_ok "rollback with absent last_good_image: no recorded good image" scenario_no_recorded_image_not_found "$R"
expect_ok "rollback with an SSM read error reports it, not 'no recorded'" scenario_recorded_image_read_error "$R"
expect_ok "dry run: backup runs before the printed SSM command, nothing sent" scenario_dry_run_backup_first "$R"
expect_ok "failing backup aborts before build, upload, and send-command" scenario_backup_failure_aborts "$R"
expect_ok "rollback with a failing backup warns and still sends the command" scenario_rollback_continues_past_backup_failure "$R"
expect_ok "rollback with a failing backup warns and reaches the dry-run command" scenario_rollback_continues_past_backup_failure_dry_run "$R"
expect_ok "failing backup aborts a dry run too" scenario_backup_failure_aborts_dry_run "$R"
expect_ok "missing backup script aborts" scenario_missing_backup_script_aborts "$R"
expect_ok "full deploy: login, build, upload, send, poll, health, record in order" scenario_full_deploy "$R"
expect_ok "explicit tag is built and recorded" scenario_explicit_tag "$R"
expect_ok "a tag with shell characters is rejected before the SSM command is built" scenario_bad_tag_rejected "$R"
expect_ok "failed server command: no health check, no record" scenario_command_failed "$R"
expect_ok "server command that never finishes times out" scenario_command_timeout "$R"
expect_ok "seven failed status reads show the aws error once and still time out" scenario_poll_error_surfaced "$R"
expect_ok "four failed status reads stay quiet" scenario_poll_few_empty_reads_quiet "$R"
expect_ok "unhealthy relay: last_good_image is not updated" scenario_unhealthy_not_recorded "$R"
expect_ok "send-command failure records nothing" scenario_send_failure "$R"
expect_ok "rollback redeploys last_good_image without build or upload" scenario_rollback_deploys_recorded "$R"
expect_ok "rollback with a tag works on a dirty tree and skips the SSM read" scenario_rollback_explicit_tag_dirty_tree "$R"
expect_ok "rollback to a tag with no release tarball fails before sending" scenario_rollback_missing_release "$R"
expect_ok "build.sh without --push loads locally and does not log in" scenario_build_load "$R"
expect_ok "build.sh --push logs in to ECR before building" scenario_build_push "$R"
expect_ok "build.sh stops when the ECR login fails" scenario_build_login_failure_stops "$R"

expect_caught "dirty-tree guard removed" scenario_dirty_untracked deploy.sh \
  's/^  \[ -z "\$dirty" \] \|\| die .*/  true; : "$dirty"/'
expect_caught "dirty-tree guard removed (modified file)" scenario_dirty_modified deploy.sh \
  's/^  \[ -z "\$dirty" \] \|\| die .*/  true; : "$dirty"/'
expect_caught "backup failure ignored" scenario_backup_failure_aborts deploy.sh \
  's/^    die "backup failed, deploy aborted before any change"$/    echo ignored >\&2/'
expect_caught "backup step removed" scenario_dry_run_backup_first deploy.sh \
  's/^if ! bash "\$backup_cmd"; then$/if false; then/'
expect_caught "rollback aborts on backup failure" scenario_rollback_continues_past_backup_failure deploy.sh \
  's/^    echo "WARNING: backup failed\..*$/    die "backup failed"/'
expect_caught "rollback backup warning removed" scenario_rollback_continues_past_backup_failure_dry_run deploy.sh \
  's/^    echo "WARNING: backup failed\..*$/    true/'
expect_caught "no-recorded-image guard removed" scenario_no_recorded_image_empty deploy.sh \
  's/case "\$tag" in "" \| None\) die/case "$tag" in NOMATCH) die/'
expect_caught "ssm read errors treated as not found" scenario_recorded_image_read_error deploy.sh \
  's/^    if \[ "\$rc" -ne 0 \] && ! grep -q ParameterNotFound "\$ERR_FILE"; then/    if false; then/'
expect_caught "health check skipped" scenario_unhealthy_not_recorded deploy.sh \
  's/^wait_for_health$/true/'
expect_caught "failed command treated as success" scenario_command_failed ssm.sh \
  's/Success\) return 0 ;;/*) return 0 ;;/'
expect_caught "poll attempts cut to one" scenario_command_timeout ssm.sh \
  's/for \(\(i = 0; i < POLL_ATTEMPTS; i\+\+\)\); do/for ((i = 0; i < POLL_ATTEMPTS; i = 0)); do break; done; for ((i = 0; i < 1; i++)); do/'
expect_caught "local .env files shipped in the tarball" scenario_full_deploy deploy.sh \
  "s/^  find .* -delete\$/  true/"
expect_caught "tar keeps the build owner" scenario_dry_run_backup_first deploy.sh \
  's/ --no-same-owner//'
expect_caught "poll error never shown" scenario_poll_error_surfaced ssm.sh \
  's/^        if \[ "\$empty_reads" -ge 5 \] && \[ "\$warned" -eq 0 \]; then$/        if false; then/'
expect_caught "poll error shown on the first empty read" scenario_poll_few_empty_reads_quiet ssm.sh \
  's/-ge 5 \]/-ge 1 ]/'
expect_caught "tag not validated" scenario_bad_tag_rejected deploy.sh \
  's/^\[\[ "\$tag" =~ .*$/true/'
expect_caught "rollback builds again" scenario_rollback_deploys_recorded deploy.sh \
  's/^if \[ "\$\{ROLLBACK:-\}" != "1" \]; then$/if true; then/'
expect_caught "login after build" scenario_build_push build.sh \
  's/^if \[ "\$mode" = "--push" \]; then$/if false; then/'
expect_caught "push not conditional on flag" scenario_build_load build.sh \
  's/^mode="--load"$/mode="--push"/'

exit "$fail"
