#!/usr/bin/env bash
# Operations on the running server: service status, recent relay logs, the agent
# containers, one agent's logs, and an interactive shell. Every action except
# shell runs over SSM and prints the output here.
# Usage: ops.sh status|logs|agents-status|agents-logs|shell
# Environment: SERVICE (logs only; Compose service name, default relay),
# AGENT (agents-logs only; a name from AGENTS), POLL_INTERVAL and POLL_ATTEMPTS (seconds, count; SSM command wait).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/aws/lib.sh
. "$SCRIPT_DIR/lib.sh"
# shellcheck source=scripts/aws/ssm.sh
. "$SCRIPT_DIR/ssm.sh"

LOG_LINES=100
LOG_SECONDS=10

# remote_script <action> <service-or-agent>: prints the commands the server runs.
remote_script() {
  echo "#!/bin/bash"
  echo "set -euo pipefail"
  echo "export HOME=/root"
  echo "cd /opt/buzz/deploy/compose"
  case "$1" in
    status) echo "BUZZ_COMPOSE_TLS=true ./run.sh status" ;;
    agents-status)
      # Stopped agents stay in the list: a clean exit after !shutdown is expected.
      # shellcheck disable=SC2028 # docker needs the literal backslash-t
      echo "docker ps -a --filter label=buzz.agent --format 'table {{.Label \"buzz.agent\"}}\t{{.Status}}\t{{.Image}}'"
      ;;
    agents-logs)
      echo "rc=0"
      echo "timeout $LOG_SECONDS docker compose -p 'buzz-agent-$2' -f /opt/buzz/deploy/aws/compose.agents.yml --env-file '/opt/buzz/agents/$2.compose.env' logs --follow --no-color --tail $LOG_LINES agent || rc=\$?"
      echo "[ \"\$rc\" -eq 0 ] || [ \"\$rc\" -eq 124 ] || exit \"\$rc\""
      ;;
    logs)
      # run.sh logs always follows, so timeout ends it. Exit 124 is the
      # expected end of a follow, not a failure.
      echo "rc=0"
      echo "BUZZ_COMPOSE_TLS=true timeout $LOG_SECONDS ./run.sh logs --no-color --tail $LOG_LINES '$2' || rc=\$?"
      echo "[ \"\$rc\" -eq 0 ] || [ \"\$rc\" -eq 124 ] || exit \"\$rc\""
      ;;
  esac
}

# run_remote <action> <service-or-agent>: runs the command on the server and prints
# its output.
run_remote() {
  local command_id
  command_id="$(remote_script "$1" "$2" | ssm_send "$instance" "buzz $1")"
  [ -n "$command_id" ] || die "send-command returned no command id"
  ssm_wait "$command_id" "$instance" || exit 1
  ssm_output "$command_id" "$instance"
}

usage="usage: ops.sh status|logs|agents-status|agents-logs|shell"
[ $# -eq 1 ] || die "$usage"
action="$1"
case "$action" in
  status | logs | agents-status | agents-logs | shell) ;;
  *) die "$usage" ;;
esac

service="${SERVICE:-relay}"
[[ "$service" =~ ^[a-z0-9][a-z0-9_-]*$ ]] || die "SERVICE must be a Compose service name (lowercase letters, digits, '_' and '-'): $service"

require_tool aws
require_tool terraform
[ "$action" != "shell" ] || require_tool session-manager-plugin
load_settings
cd "$REPO_ROOT"

agent="${AGENT:-}"
if [ "$action" = "agents-logs" ]; then
  [ -n "$agent" ] || die "AGENT is required (make agents-logs AGENT=<name>)"
  match=""
  while read -r name _; do
    [ "$name" != "$agent" ] || match="$name"
  done < <(agent_list)
  [ -n "$match" ] || die "$agent is not an agent in AGENTS (${AGENTS:-empty}). Known agents: $(agent_list | cut -d' ' -f1 | paste -sd, -)"
fi

instance="$(tf_output instance_id)"
[ -n "$instance" ] || die "terraform output instance_id is empty (run make apply first)"

case "$action" in
  shell) exec aws ssm start-session --target "$instance" ;;
  agents-logs) run_remote "$action" "$agent" ;;
  *) run_remote "$action" "$service" ;;
esac
