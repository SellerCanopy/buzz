#!/usr/bin/env bash
# Operations on the running server: service status, recent relay logs, and an
# interactive shell. status and logs run over SSM and print the output here.
# Usage: ops.sh status|logs|shell
# Environment: SERVICE (logs only; Compose service name, default relay),
# POLL_INTERVAL and POLL_ATTEMPTS (seconds, count; SSM command wait).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/aws/lib.sh
. "$SCRIPT_DIR/lib.sh"
# shellcheck source=scripts/aws/ssm.sh
. "$SCRIPT_DIR/ssm.sh"

LOG_LINES=100
LOG_SECONDS=10

# remote_script <status|logs> <service>: prints the commands the server runs.
remote_script() {
  echo "#!/bin/bash"
  echo "set -euo pipefail"
  echo "export HOME=/root"
  echo "cd /opt/buzz/deploy/compose"
  case "$1" in
    status) echo "BUZZ_COMPOSE_TLS=true ./run.sh status" ;;
    logs)
      # run.sh logs always follows, so timeout ends it. Exit 124 is the
      # expected end of a follow, not a failure.
      echo "rc=0"
      echo "BUZZ_COMPOSE_TLS=true timeout $LOG_SECONDS ./run.sh logs --no-color --tail $LOG_LINES '$2' || rc=\$?"
      echo "[ \"\$rc\" -eq 0 ] || [ \"\$rc\" -eq 124 ] || exit \"\$rc\""
      ;;
  esac
}

# run_remote <status|logs> <service>: runs the command on the server and prints
# its output.
run_remote() {
  local command_id
  command_id="$(remote_script "$1" "$2" | ssm_send "$instance" "buzz $1")"
  [ -n "$command_id" ] || die "send-command returned no command id"
  ssm_wait "$command_id" "$instance" || exit 1
  ssm_output "$command_id" "$instance"
}

[ $# -eq 1 ] || die "usage: ops.sh status|logs|shell"
action="$1"
case "$action" in
  status | logs | shell) ;;
  *) die "usage: ops.sh status|logs|shell" ;;
esac

service="${SERVICE:-relay}"
[[ "$service" =~ ^[a-z0-9][a-z0-9_-]*$ ]] || die "SERVICE must be a Compose service name (lowercase letters, digits, '_' and '-'): $service"

require_tool aws
require_tool terraform
[ "$action" != "shell" ] || require_tool session-manager-plugin
load_settings
cd "$REPO_ROOT"

instance="$(tf_output instance_id)"
[ -n "$instance" ] || die "terraform output instance_id is empty (run make apply first)"

case "$action" in
  shell) exec aws ssm start-session --target "$instance" ;;
  *) run_remote "$action" "$service" ;;
esac
