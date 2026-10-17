#!/usr/bin/env bash
# Tests for scripts/aws/ops.sh (make status, logs, shell) with stub aws,
# terraform, and timeout. The logs scenarios also run the logged server command
# here, with a fake /opt/buzz, to check how it treats the end of a follow.
set -uo pipefail
# shellcheck source=tests/aws/helpers.sh
. "$(dirname "${BASH_SOURCE[0]}")/helpers.sh"

OUT="$WORK/out"
ERR="$WORK/err"
OPT="$WORK/opt"

write_aws_env

stubs_ok() {
  reset_stubs
  printf '%s' "i-0abc123" > "$STUB_DIR/tf-instance_id.out"
  echo "cmd-1" > "$STUB_DIR/ssm-send-command.out"
  echo "Success" > "$STUB_DIR/ssm-get-command-invocation.out"
  echo "relay   running" > "$STUB_DIR/ssm-get-command-invocation.StandardOutputContent.out"
}

# run_ops <scripts-dir> <action> [VAR=value ...]: sets RC.
run_ops() {
  local dir="$1" action="$2"
  shift 2
  RC=0
  env POLL_INTERVAL=0 POLL_ATTEMPTS=3 ${@+"$@"} bash "$dir/ops.sh" "$action" > "$OUT" 2> "$ERR" || RC=$?
}

sent_line() { grep -F 'aws ssm send-command' "$STUB_LOG" | head -n 1; }

# decode_command <file>: the logged SSM command as a script under the sandbox.
decode_command() {
  sent_line \
    | sed -e 's/^.*{"commands":\["//' -e 's/"\]}.*$//' -e 's/","/\
/g' -e 's/\\"/"/g' -e 's/\\\\/\\/g' \
    | sed "s#/opt/buzz#$OPT#g" > "$1"
}

scenario_status() {
  stubs_ok
  run_ops "$1" status
  [ "$RC" -eq 0 ] || return 1
  local line
  line="$(sent_line)"
  case "$line" in *"--instance-ids i-0abc123 --document-name AWS-RunShellScript "*) ;; *) return 1 ;; esac
  case "$line" in *"cd /opt/buzz/deploy/compose"*) ;; *) return 1 ;; esac
  case "$line" in *"BUZZ_COMPOSE_TLS=true ./run.sh status"*) ;; *) return 1 ;; esac
  grep -qF "relay   running" "$OUT"
}

scenario_status_failure() {
  stubs_ok
  echo "Failed" > "$STUB_DIR/ssm-get-command-invocation.out"
  run_ops "$1" status
  [ "$RC" -ne 0 ] && grep -qF "status Failed" "$ERR" && ! grep -qF "relay   running" "$OUT"
}

scenario_logs_default_service() {
  stubs_ok
  echo "relay log line" > "$STUB_DIR/ssm-get-command-invocation.StandardOutputContent.out"
  run_ops "$1" logs
  [ "$RC" -eq 0 ] || return 1
  local line
  line="$(sent_line)"
  case "$line" in *"timeout 10 ./run.sh logs --no-color --tail 100 'relay'"*) ;; *) return 1 ;; esac
  grep -qF "relay log line" "$OUT"
}

scenario_logs_other_service() {
  stubs_ok
  run_ops "$1" logs SERVICE=caddy
  [ "$RC" -eq 0 ] && case "$(sent_line)" in *"--tail 100 'caddy'"*) true ;; *) false ;; esac
}

scenario_logs_bad_service() {
  stubs_ok
  run_ops "$1" logs "SERVICE=relay';touch /tmp/pwned;'"
  [ "$RC" -ne 0 ] && grep -qF "SERVICE must be" "$ERR" && [ "$(log_count 'send-command')" -eq 0 ]
}

# run_logs_command <scripts-dir> <timeout-rc>: runs the logged logs command
# against the fake timeout.
run_logs_command() {
  stubs_ok
  run_ops "$1" logs
  [ "$RC" -eq 0 ] || return 1
  decode_command "$WORK/server.sh"
  : > "$STUB_LOG"
  echo "$2" > "$STUB_DIR/timeout.rc"
  rm -rf "$OPT"
  mkdir -p "$OPT/deploy/compose"
  RC=0
  bash "$WORK/server.sh" > "$WORK/srv.out" 2> "$WORK/srv.err" || RC=$?
}

# Real timeout exits 124 when it ends the follow; that is the normal end.
scenario_logs_command_follow_end_is_ok() {
  run_logs_command "$1" 124 || return 1
  [ "$RC" -eq 0 ] && [ "$(log_count '^timeout 10 ./run.sh logs --no-color --tail 100 relay$')" -eq 1 ]
}

scenario_logs_command_real_failure_fails() {
  run_logs_command "$1" 2 || return 1
  [ "$RC" -eq 2 ]
}

scenario_shell() {
  stubs_ok
  run_ops "$1" shell
  [ "$RC" -eq 0 ] && [ "$(log_count '^aws ssm start-session --target i-0abc123$')" -eq 1 ] \
    && [ "$(log_count 'send-command')" -eq 0 ]
}

# PATH holds only the stubs for aws and terraform plus the plain tools the
# scripts call, so session-manager-plugin is not found even if it is installed.
scenario_shell_without_plugin() {
  stubs_ok
  local farm="$WORK/farm" t
  rm -rf "$farm"
  mkdir -p "$farm"
  for t in bash env dirname cat cp mktemp rm grep sed tr tail head sleep date; do
    ln -s "$(command -v "$t")" "$farm/$t"
  done
  ln -s "$REPO_ROOT/tests/aws/stubs/aws" "$farm/aws"
  ln -s "$REPO_ROOT/tests/aws/stubs/terraform" "$farm/terraform"
  RC=0
  PATH="$farm" "$farm/bash" "$1/ops.sh" shell > "$OUT" 2> "$ERR" || RC=$?
  [ "$RC" -ne 0 ] && grep -qF "missing tool: session-manager-plugin" "$ERR" \
    && [ "$(log_count 'start-session')" -eq 0 ]
}

scenario_usage() {
  stubs_ok
  run_ops "$1" reboot
  [ "$RC" -ne 0 ] && grep -qF "usage: ops.sh" "$ERR" && [ "$(log_count 'send-command')" -eq 0 ]
}

scenario_missing_instance_output() {
  stubs_ok
  rm -f "$STUB_DIR/tf-instance_id.out"
  run_ops "$1" status
  [ "$RC" -ne 0 ] && [ "$(log_count 'send-command')" -eq 0 ]
}

D="$REPO_ROOT/scripts/aws"
expect_ok "status runs run.sh status from the compose directory and prints the output" scenario_status "$D"
expect_ok "status exits nonzero when the server command fails" scenario_status_failure "$D"
expect_ok "logs is bounded by timeout and --tail and defaults to the relay" scenario_logs_default_service "$D"
expect_ok "logs takes SERVICE for another service" scenario_logs_other_service "$D"
expect_ok "logs rejects a SERVICE with shell characters before sending" scenario_logs_bad_service "$D"
expect_ok "logs command treats the timeout end of a follow as success" scenario_logs_command_follow_end_is_ok "$D"
expect_ok "logs command passes a real failure through" scenario_logs_command_real_failure_fails "$D"
expect_ok "shell opens an SSM session on the instance" scenario_shell "$D"
expect_ok "shell stops when session-manager-plugin is missing" scenario_shell_without_plugin "$D"
expect_ok "an unknown action prints usage" scenario_usage "$D"
expect_ok "missing instance_id output sends nothing" scenario_missing_instance_output "$D"

expect_caught "plugin check removed" scenario_shell_without_plugin ops.sh \
  's/^\[ "\$action" != "shell" \] \|\| require_tool session-manager-plugin$/true/'
expect_caught "SERVICE not validated" scenario_logs_bad_service ops.sh \
  's/^\[\[ "\$service" =~ .*$/true/'
expect_caught "timeout end treated as failure" scenario_logs_command_follow_end_is_ok ops.sh \
  's/ \|\| \[ \\"\\\$rc\\" -eq 124 \]//'
expect_caught "every nonzero code treated as the end of the follow" scenario_logs_command_real_failure_fails ops.sh \
  's/\[ \\"\\\$rc\\" -eq 0 \] \|\| \[ \\"\\\$rc\\" -eq 124 \]/true/'
expect_caught "logs unbounded" scenario_logs_default_service ops.sh \
  's/timeout \$LOG_SECONDS //'
expect_caught "logs not tailed" scenario_logs_default_service ops.sh \
  's/ --tail \$LOG_LINES//'
expect_caught "status runs the wrong command" scenario_status ops.sh \
  's/\.\/run\.sh status/.\/run.sh config/'
expect_caught "usage check removed" scenario_usage ops.sh \
  's/^  \*\) die "usage: ops.sh status\|logs\|shell" ;;$/  *) true ;;/'

exit "$fail"
